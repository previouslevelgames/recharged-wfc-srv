//! The admin's tool for a person who is to go (docs/profiles.md, "Deleting a
//! player"): their Recharged player with everything theirs, and whatever
//! their WFC profiles have on the server, every board's rows first. The WFC
//! profiles themselves stay: the game logs in only to the profile number its
//! save holds (DWC refuses any other), so deleting one would lock that Mii
//! out of online play instead of giving it a fresh start. Their consoles
//! and the player's sign-ins are banned instead (`crate::bans`), so the Mii
//! that comes back with each login doesn't.

use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use tracing::warn;

use super::merge::Side;
use crate::{accounts, bans, gpcm::Gpcm, reports};

/// What a delete removes, as its first ask shows it and its second does it.
#[derive(Debug, PartialEq, Serialize)]
pub struct Deletion {
    /// The Recharged player who goes, when the profile plays for one.
    pub player: Option<Side>,
    /// The WFC profiles whose rows go: all the player's, or the one profile
    /// without a player. The profiles stay.
    pub profiles: Vec<Profile>,
    /// Their rows on the boards (`scores`) and of ended seasons and days
    /// (`past_scores`).
    pub scores: i64,
    pub past_scores: i64,
    /// The online matches they reported (docs/reports.md): both sides go.
    pub online_matches: i64,
    /// The player's sets, which go for their opponents too.
    pub sets: i64,
    /// The player's friends, both ways.
    pub friends: i64,
    /// Mii friends: the player's own, and other players' of these profiles.
    pub mii_friends: i64,
    /// Of the profiles, those logged in to GPCM now. The delete ends their
    /// logins with the rest of their consoles'.
    pub online: usize,
    /// The consoles of the profiles, which the delete bans: NAS and GPCM
    /// refuse every Mii on them. Never the CPU buddy's account.
    pub consoles: i64,
    /// The player's sign-ins (Discord's) the delete bans.
    pub identities: i64,
    /// The ban, once the delete is done.
    pub ban_id: Option<i64>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Profile {
    pub profile_id: i32,
    pub mii_name: Option<String>,
    /// The game code its game plays.
    pub release: String,
}

/// What deleting the player of `profile_id` would remove, or why it can't
/// be: no such profile, the CPU buddy, a set still to be played or an
/// online match in progress.
pub async fn plan(
    pool: &PgPool,
    gpcm: &Gpcm,
    profile_id: i32,
) -> Result<Result<Deletion, &'static str>, sqlx::Error> {
    reports::settle_idle(pool).await?;
    let mut conn = pool.acquire().await?;
    plan_in(&mut conn, gpcm, profile_id).await
}

async fn plan_in(
    conn: &mut PgConnection,
    gpcm: &Gpcm,
    profile_id: i32,
) -> Result<Result<Deletion, &'static str>, sqlx::Error> {
    if profile_id == accounts::CPU_BUDDY {
        return Ok(Err("the CPU buddy is the server's own"));
    }
    let Some(bound) = sqlx::query_scalar!(
        r#"SELECT player_wfc_profiles.player_id AS "player_id?"
           FROM profiles LEFT JOIN player_wfc_profiles USING (profile_id)
           WHERE profiles.profile_id = $1"#,
        profile_id
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(Err("no such WFC profile"));
    };
    let player = match bound {
        Some(player_id) => side(&mut *conn, player_id).await?,
        None => None,
    };
    let player_id = player.as_ref().map(|player| player.player_id);
    let profiles: Vec<Profile> = sqlx::query!(
        "SELECT profile_id, ingamesn, gsbrcd FROM profiles
         WHERE profile_id = $1
            OR profile_id IN (SELECT profile_id FROM player_wfc_profiles WHERE player_id = $2)
         ORDER BY profile_id",
        profile_id,
        player_id,
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|row| Profile {
        profile_id: row.profile_id,
        mii_name: row.ingamesn,
        release: row.gsbrcd.chars().take(4).collect(),
    })
    .collect();
    let ids: Vec<i32> = profiles.iter().map(|profile| profile.profile_id).collect();

    // The launchers and games of a set count on it until it is over.
    let open = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM matches WHERE state IN ('challenged', 'accepted')
                          AND (home = $1 OR away = $1)) AS "open!""#,
        player_id,
    )
    .fetch_one(&mut *conn)
    .await?;
    if open {
        return Ok(Err("the player has a set to play: wait until it is over"));
    }
    // A report after the delete would open the match again, without its
    // start.
    let playing = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM online_match_sides JOIN online_matches USING (match_id)
                          WHERE online_match_sides.profile_id = ANY($1)
                            AND online_matches.state = 'in_progress') AS "playing!""#,
        &ids,
    )
    .fetch_one(&mut *conn)
    .await?;
    if playing {
        return Ok(Err("an online match is in progress: wait until it is over"));
    }

    let counts = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM scores WHERE profile_id = ANY($1)) AS "scores!",
             (SELECT count(*) FROM past_scores WHERE profile_id = ANY($1)) AS "past_scores!",
             (SELECT count(DISTINCT match_id) FROM online_match_sides
              WHERE profile_id = ANY($1)) AS "online_matches!",
             (SELECT count(*) FROM matches WHERE home = $2 OR away = $2) AS "sets!",
             (SELECT count(*) FROM friends WHERE player_id = $2 OR friend_id = $2) AS "friends!",
             (SELECT count(*) FROM friend_profiles
              WHERE profile_id = ANY($1) OR player_id = $2) AS "mii_friends!",
             (SELECT count(DISTINCT user_id) FROM profiles
              WHERE profile_id = ANY($1) AND user_id <> $3) AS "consoles!",
             (SELECT count(*) FROM player_identities
              WHERE player_id = $2 AND provider <> 'wfc') AS "identities!""#,
        &ids,
        player_id,
        accounts::CPU_BUDDY_ACCOUNT,
    )
    .fetch_one(&mut *conn)
    .await?;
    let presence = gpcm.presence().await?;
    let online = ids.iter().filter(|id| presence.contains_key(id)).count();
    Ok(Ok(Deletion {
        player,
        profiles,
        scores: counts.scores,
        past_scores: counts.past_scores,
        online_matches: counts.online_matches,
        sets: counts.sets,
        friends: counts.friends,
        mii_friends: counts.mii_friends,
        online,
        consoles: counts.consoles,
        identities: counts.identities,
        ban_id: None,
    }))
}

/// The player `player_id`, held until the delete is done.
async fn side(conn: &mut PgConnection, player_id: i64) -> Result<Option<Side>, sqlx::Error> {
    let Some(name) = sqlx::query_scalar!(
        "SELECT display_name FROM players WHERE player_id = $1 FOR UPDATE",
        player_id
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let signs_in_with = sqlx::query_scalar!(
        "SELECT provider FROM player_identities WHERE player_id = $1 ORDER BY provider",
        player_id
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(Some(Side {
        player_id,
        name,
        signs_in_with,
    }))
}

/// Deletes the player of `profile_id` and what [`plan`] lists, all at once:
/// the profiles' rows on every board and of past seasons, their online
/// matches and the Mii friends of them; the player with their sign-ins,
/// sessions (their launchers are signed out), friends, chats, search and
/// sets. Bans, by `banned_by`, the profiles' consoles and the player's
/// sign-ins, and ends the consoles' logins to this server. Err: why not (as
/// [`plan`]).
pub async fn delete(
    pool: &PgPool,
    gpcm: &Gpcm,
    profile_id: i32,
    banned_by: &str,
) -> Result<Result<Deletion, &'static str>, sqlx::Error> {
    reports::settle_idle(pool).await?;
    let mut tx = pool.begin().await?;
    let mut deletion = match plan_in(&mut tx, gpcm, profile_id).await? {
        Ok(deletion) => deletion,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let ids: Vec<i32> = deletion
        .profiles
        .iter()
        .map(|profile| profile.profile_id)
        .collect();
    // A game puts its whole running total: once its rows are gone, its next
    // match would put the old one back.
    let flagged = gpcm.keep_off_boards(&ids).await?;
    let player_id = deletion.player.as_ref().map(|player| player.player_id);
    let name = match &deletion.player {
        Some(player) => player.name.clone(),
        None => deletion
            .profiles
            .iter()
            .find(|profile| profile.profile_id == profile_id)
            .and_then(|profile| profile.mii_name.clone())
            .unwrap_or_else(|| format!("WFC profile {profile_id}")),
    };
    // The ban first: the player's sign-ins go with them.
    let removed = match bans::ban(&mut tx, &name, banned_by, &ids, player_id).await {
        Ok(ban_id) => match remove(&mut tx, &ids, player_id).await {
            Ok(()) => tx.commit().await.map(|()| ban_id),
            Err(failure) => Err(failure),
        },
        Err(failure) => Err(failure),
    };
    let ban_id = match removed {
        Ok(ban_id) => ban_id,
        Err(failure) => {
            if let Err(error) = gpcm.back_on_boards(&flagged).await {
                warn!(target: "admin", %error, "logins left off the boards");
            }
            return Err(failure);
        }
    };
    deletion.ban_id = Some(ban_id);
    // Every Mii of the consoles leaves: the game would send its name again.
    gpcm.disconnect(&bans::profiles(pool, ban_id).await?);
    Ok(Ok(deletion))
}

async fn remove(
    conn: &mut PgConnection,
    profiles: &[i32],
    player_id: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM scores WHERE profile_id = ANY($1)", profiles)
        .execute(&mut *conn)
        .await?;
    sqlx::query!(
        "DELETE FROM past_scores WHERE profile_id = ANY($1)",
        profiles
    )
    .execute(&mut *conn)
    .await?;
    // Their sides, games and timelines go with them.
    sqlx::query!(
        "DELETE FROM online_matches WHERE match_id IN
             (SELECT match_id FROM online_match_sides WHERE profile_id = ANY($1))",
        profiles
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "DELETE FROM friend_profiles WHERE profile_id = ANY($1)",
        profiles
    )
    .execute(&mut *conn)
    .await?;
    // The rest is the player's: sign-ins, sessions, the WFC profiles' links,
    // friends, chats, the search, the sets and their reports.
    if let Some(player_id) = player_id {
        sqlx::query!("DELETE FROM players WHERE player_id = $1", player_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::*;

    /// A WFC profile of a console `user_id` (made now), as its login makes it.
    async fn profile(pool: &PgPool, user_id: i64, gsbrcd: &str, mii: &str) -> i32 {
        crate::accounts::log_in(pool, user_id).await.unwrap();
        crate::accounts::profile_login(pool, user_id, gsbrcd, Some(mii), "10.0.0.1")
            .await
            .unwrap()
            .profile_id
    }

    const ADMIN: &str = "admin@example.com";

    async fn run(pool: &PgPool, sql: &str) {
        let mut conn = pool.acquire().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned()))
            .execute(&mut *conn)
            .await
            .unwrap();
    }

    async fn count(pool: &PgPool, sql: &str) -> i64 {
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
            .fetch_one(&mut *conn)
            .await
            .unwrap()
    }

    /// An online match `profile` reported as home, with an away side of
    /// `opponent` when its game reported too. Each side's series is the
    /// match's number.
    fn online_match(state: &str, profile: i32, opponent: Option<i32>) -> String {
        let side = |side: &str, profile: i32| {
            format!(
                "INSERT INTO online_match_sides (match_id, side, profile_id, series, mii,
                                                 opponent_mii, home_captain, away_captain)
                 SELECT max(match_id), '{side}', {profile}, max(match_id), 'A', 'B', 'mario', 'wario'
                 FROM online_matches;"
            )
        };
        format!(
            "INSERT INTO online_matches (release, mode, best_of, state, home_mii, away_mii,
                                         home_captain, away_captain)
             VALUES ('R4QE', 'ranked', 3, '{state}', 'A', 'B', 'mario', 'wario');
             {}
             {}",
            side("home", profile),
            opponent.map_or(String::new(), |opponent| side("away", opponent)),
        )
    }

    #[sqlx::test]
    async fn a_player_goes_with_every_row_of_their_profiles(pool: PgPool) {
        let gpcm = Gpcm::new(
            pool.clone(),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        // Birnen (1) plays the US and the European game; Kiki (2) has a
        // profile of her own; a retail Mii (no player) played Birnen.
        run(
            &pool,
            "INSERT INTO players (display_name) VALUES ('Birnen'), ('Kiki')",
        )
        .await;
        let us = profile(&pool, 11, "R4QEaaaaaaa", "Birnen").await;
        let eu = profile(&pool, 12, "R4QPbbbbbbb", "Birnen").await;
        let kikis = profile(&pool, 13, "R4QEccccccc", "Kiki").await;
        let retail = profile(&pool, 14, "R4QEddddddd", "Retail").await;
        run(&pool, &format!(
            "INSERT INTO player_identities (provider, external_id, player_id, username) VALUES
                 ('discord', 'd1', 1, 'birnen'), ('discord', 'd2', 2, 'kiki');
             INSERT INTO player_wfc_profiles (profile_id, player_id) VALUES
                 ({us}, 1), ({eu}, 1), ({kikis}, 2);
             INSERT INTO scores (profile_id, category, region, score, userdata) VALUES
                 ({us}, 0, 2, 50, ''), ({us}, 1, 2, 5, ''), ({eu}, 0, 4, 30, ''),
                 ({kikis}, 0, 2, 40, ''), ({retail}, 0, 2, 20, '');
             INSERT INTO past_scores (profile_id, category, first_day, region, score, userdata, updated_at)
                 VALUES ({eu}, 0, '2026-10-15', 4, 70, '', now()), ({kikis}, 0, '2026-10-15', 2, 60, '', now());
             INSERT INTO matches (kind, state, home, away, rule_set, rules, rules_hash, winner) VALUES
                 ('test', 'finished', 1, 2, 'x', '{{}}', 'h', 1);
             INSERT INTO friends (player_id, friend_id) VALUES (1, 2), (2, 1);
             INSERT INTO friend_profiles (player_id, profile_id) VALUES (1, {retail}), (2, {us}), (2, {retail});
             INSERT INTO chat_messages (sender, recipient, body) VALUES (1, 2, 'hi');
             INSERT INTO player_sessions (token_hash, player_id, expires_at)
                 VALUES (decode(repeat('ab', 32), 'hex'), 1, now() + interval '1 day');"
        )).await;
        run(&pool, &online_match("finished", us, Some(retail))).await;
        run(&pool, &online_match("finished", kikis, Some(retail))).await;
        gpcm.fake_login(eu, IpAddr::from([10, 0, 0, 1]));

        assert_eq!(
            plan(&pool, &gpcm, 7).await.unwrap(),
            Err("no such WFC profile")
        );
        assert_eq!(
            plan(&pool, &gpcm, accounts::CPU_BUDDY).await.unwrap(),
            Err("the CPU buddy is the server's own")
        );
        // A set to play, and an online match in progress, wait.
        run(
            &pool,
            "INSERT INTO matches (kind, state, home, away, rule_set, rules, rules_hash)
                    VALUES ('test', 'challenged', 2, 1, 'x', '{}', 'h')",
        )
        .await;
        assert_eq!(
            delete(&pool, &gpcm, us, ADMIN).await.unwrap(),
            Err("the player has a set to play: wait until it is over")
        );
        run(&pool, "DELETE FROM matches WHERE state = 'challenged'").await;
        run(&pool, &online_match("in_progress", eu, None)).await;
        assert_eq!(
            delete(&pool, &gpcm, us, ADMIN).await.unwrap(),
            Err("an online match is in progress: wait until it is over")
        );
        run(
            &pool,
            "UPDATE online_matches SET state = 'abandoned' WHERE state = 'in_progress'",
        )
        .await;

        // Asked from either profile, the same player goes.
        let planned = plan(&pool, &gpcm, eu).await.unwrap().unwrap();
        let mut deleted = delete(&pool, &gpcm, us, ADMIN).await.unwrap().unwrap();
        let ban_id = deleted.ban_id.take().expect("a ban");
        assert_eq!(planned, deleted);
        let player = deleted.player.as_ref().unwrap();
        assert_eq!((player.player_id, player.name.as_str()), (1, "Birnen"));
        assert_eq!(
            deleted
                .profiles
                .iter()
                .map(|p| (p.profile_id, p.release.as_str()))
                .collect::<Vec<_>>(),
            [(us, "R4QE"), (eu, "R4QP")]
        );
        assert_eq!(
            (
                deleted.scores,
                deleted.past_scores,
                deleted.online_matches,
                deleted.sets,
                deleted.friends,
                deleted.mii_friends,
                deleted.online,
                deleted.consoles,
                deleted.identities,
            ),
            (3, 1, 2, 1, 2, 2, 1, 2, 1)
        );

        // Birnen is gone, and so is every row of his profiles; they stay.
        assert_eq!(count(&pool, "SELECT count(*) FROM players").await, 1);
        assert_eq!(
            count(
                &pool,
                &format!("SELECT count(*) FROM profiles WHERE profile_id IN ({us}, {eu})")
            )
            .await,
            2
        );
        let left: Vec<i32> = sqlx::query_scalar("SELECT profile_id FROM scores ORDER BY 1")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(left, [kikis, retail]);
        let left: Vec<i32> = sqlx::query_scalar("SELECT profile_id FROM past_scores")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(left, [kikis]);
        // Kiki's match with the retail Mii stays.
        assert_eq!(count(&pool, "SELECT count(*) FROM online_matches").await, 1);
        assert_eq!(
            count(&pool, "SELECT count(*) FROM online_match_sides").await,
            2
        );
        for table in [
            "matches",
            "friends",
            "chat_messages",
            "player_sessions",
            "player_wfc_profiles WHERE player_id = 1",
            "player_identities WHERE player_id = 1",
        ] {
            assert_eq!(
                count(&pool, &format!("SELECT count(*) FROM {table}")).await,
                0,
                "{table}"
            );
        }
        let kept: Vec<i32> = sqlx::query_scalar("SELECT profile_id FROM friend_profiles")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(kept, [retail], "Kiki keeps the retail Mii");
        // The European game, logged in now, puts nothing until it logs in
        // again.
        assert!(gpcm.off_boards(eu).await.unwrap());
        assert!(!gpcm.off_boards(kikis).await.unwrap());

        // Both of Birnen's consoles and his Discord account are banned, as
        // Birnen; Kiki's are not.
        for (user_id, banned) in [(11, true), (12, true), (13, false), (14, false)] {
            assert_eq!(
                bans::console_banned(&pool, user_id).await.unwrap(),
                banned,
                "console {user_id}"
            );
        }
        assert!(bans::identity_banned(&pool, "discord", "d1").await.unwrap());
        assert!(!bans::identity_banned(&pool, "discord", "d2").await.unwrap());
        assert_eq!(bans::profiles(&pool, ban_id).await.unwrap(), [us, eu]);
        let mut conn = pool.acquire().await.unwrap();
        let ban = bans::of_console(&mut conn, 12).await.unwrap().unwrap();
        assert_eq!(
            (ban.ban_id, ban.name.as_str(), ban.banned_by.as_str()),
            (ban_id, "Birnen", ADMIN)
        );
        drop(conn);
        // Lifted, the consoles log in again and the account makes a player.
        assert_eq!(bans::lift(&pool, ban_id).await.unwrap(), Some(ban));
        assert!(!bans::console_banned(&pool, 11).await.unwrap());
        assert!(!bans::identity_banned(&pool, "discord", "d1").await.unwrap());
        assert_eq!(bans::lift(&pool, ban_id).await.unwrap(), None);
    }

    #[sqlx::test]
    async fn a_ban_ends_the_logins_of_its_profiles(pool: PgPool) {
        let gpcm = Gpcm::new(
            pool,
            Default::default(),
            Default::default(),
            Default::default(),
        );
        gpcm.fake_login(5, IpAddr::from([10, 0, 0, 1]));
        assert_eq!(gpcm.disconnect(&[5, 6]), [5]);
    }

    #[sqlx::test]
    async fn a_profile_without_a_player_loses_only_its_rows(pool: PgPool) {
        let gpcm = Gpcm::new(
            pool.clone(),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let retail = profile(&pool, 11, "R4QEaaaaaaa", "Retail").await;
        let other = profile(&pool, 12, "R4QEbbbbbbb", "Other").await;
        run(
            &pool,
            &format!(
                "INSERT INTO players (display_name) VALUES ('Kiki');
             INSERT INTO scores (profile_id, category, region, score, userdata) VALUES
                 ({retail}, 0, 2, 20, ''), ({other}, 0, 2, 10, '');
             INSERT INTO friend_profiles (player_id, profile_id) VALUES (1, {retail});"
            ),
        )
        .await;
        run(&pool, &online_match("finished", other, Some(retail))).await;

        let deleted = delete(&pool, &gpcm, retail, ADMIN).await.unwrap().unwrap();
        assert_eq!(deleted.player, None);
        assert_eq!((deleted.consoles, deleted.identities), (1, 0));
        // Its console is banned under its Mii name; the other is not.
        assert!(bans::console_banned(&pool, 11).await.unwrap());
        assert!(!bans::console_banned(&pool, 12).await.unwrap());
        let mut conn = pool.acquire().await.unwrap();
        let ban = bans::of_console(&mut conn, 11).await.unwrap().unwrap();
        assert_eq!(ban.name, "Retail");
        drop(conn);
        assert_eq!(
            (
                deleted.scores,
                deleted.online_matches,
                deleted.sets,
                deleted.mii_friends
            ),
            (1, 1, 0, 1)
        );
        assert_eq!(count(&pool, "SELECT count(*) FROM players").await, 1);
        let left: Vec<i32> = sqlx::query_scalar("SELECT profile_id FROM scores")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(left, [other]);
        assert_eq!(count(&pool, "SELECT count(*) FROM online_matches").await, 0);
        // Not logged in: its next login finds no row and starts from 0.
        assert!(!gpcm.off_boards(retail).await.unwrap());

        // A console that claimed the CPU buddy's account loses its rows, but
        // the server's own account is never banned.
        let claimed = profile(&pool, accounts::CPU_BUDDY_ACCOUNT, "R4QEccccccc", "Claimed").await;
        let deleted = delete(&pool, &gpcm, claimed, ADMIN).await.unwrap().unwrap();
        assert_eq!(deleted.consoles, 0);
        assert!(
            !bans::console_banned(&pool, accounts::CPU_BUDDY_ACCOUNT)
                .await
                .unwrap()
        );
    }
}

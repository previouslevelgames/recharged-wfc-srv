//! The admin's tools for a person with more than one Recharged player or
//! WFC profile (docs/profiles.md, "One player, two profiles"): a WFC profile
//! linked to a player by hand, and a player merged into another with
//! everything theirs.

use serde::Serialize;
use sqlx::PgPool;

/// What a merge moves, as its first ask shows it and its second does it.
#[derive(Debug, PartialEq, Serialize)]
pub struct Merge {
    pub from: Side,
    pub into: Side,
    /// The WFC profiles that move to `into`.
    pub profiles: i64,
    /// The sets of `from` that become `into`'s.
    pub matches: i64,
    /// The sets between the two, which go (a player can't play themselves).
    pub between: i64,
    /// The friends (both ways) and Mii friends that move.
    pub friends: i64,
    /// The accounts `from` signs in with that move (`into` has none of that
    /// kind); the others go with `from`.
    pub identities: Vec<String>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Side {
    pub player_id: i64,
    pub name: String,
    /// The accounts they sign in with: discord, test, wfc.
    pub signs_in_with: Vec<String>,
}

async fn side(pool: &PgPool, player_id: i64) -> Result<Option<Side>, sqlx::Error> {
    let Some(name) = sqlx::query_scalar!(
        "SELECT display_name FROM players WHERE player_id = $1",
        player_id
    )
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let signs_in_with = sqlx::query_scalar!(
        "SELECT provider FROM player_identities WHERE player_id = $1 ORDER BY provider",
        player_id
    )
    .fetch_all(pool)
    .await?;
    Ok(Some(Side {
        player_id,
        name,
        signs_in_with,
    }))
}

/// The WFC profile `profile_id` plays for `player_id` from now on, or for
/// nobody (`None`). Err: why not (no such profile or player).
pub async fn link(
    pool: &PgPool,
    profile_id: i32,
    player_id: Option<i64>,
) -> Result<Result<(), &'static str>, sqlx::Error> {
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM profiles WHERE profile_id = $1) AS "exists!""#,
        profile_id
    )
    .fetch_one(pool)
    .await?;
    if !exists {
        return Ok(Err("no such WFC profile"));
    }
    let Some(player_id) = player_id else {
        sqlx::query!(
            "DELETE FROM player_wfc_profiles WHERE profile_id = $1",
            profile_id
        )
        .execute(pool)
        .await?;
        return Ok(Ok(()));
    };
    if side(pool, player_id).await?.is_none() {
        return Ok(Err("no such player"));
    }
    sqlx::query!(
        "INSERT INTO player_wfc_profiles (profile_id, player_id) VALUES ($1, $2)
         ON CONFLICT (profile_id) DO UPDATE SET player_id = $2, bound_at = now()",
        profile_id,
        player_id,
    )
    .execute(pool)
    .await?;
    Ok(Ok(()))
}

/// What merging `from` into `into` would move, or why it can't be: the same
/// player, one that isn't there, two Discord accounts (two people, as far as
/// the server can tell), or a set still to be played.
pub async fn plan(
    pool: &PgPool,
    from: i64,
    into: i64,
) -> Result<Result<Merge, &'static str>, sqlx::Error> {
    if from == into {
        return Ok(Err("a player can't be merged into themselves"));
    }
    let (Some(from_side), Some(into_side)) = (side(pool, from).await?, side(pool, into).await?)
    else {
        return Ok(Err("no such player"));
    };
    let discord = |side: &Side| side.signs_in_with.iter().any(|kind| kind == "discord");
    if discord(&from_side) && discord(&into_side) {
        return Ok(Err(
            "both sign in with Discord: two accounts, likely two people",
        ));
    }
    let open = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM matches WHERE state IN ('challenged', 'accepted')
                          AND (home IN ($1, $2) OR away IN ($1, $2))) AS "open!""#,
        from,
        into,
    )
    .fetch_one(pool)
    .await?;
    if open {
        return Ok(Err("one of them has a set to play: wait until it is over"));
    }
    let counts = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM player_wfc_profiles WHERE player_id = $1) AS "profiles!",
             (SELECT count(*) FROM matches
              WHERE (home = $1 OR away = $1) AND NOT (home = $2 OR away = $2)) AS "matches!",
             (SELECT count(*) FROM matches
              WHERE (home = $1 AND away = $2) OR (home = $2 AND away = $1)) AS "between!",
             (SELECT count(*) FROM friends WHERE (player_id = $1 AND friend_id <> $2)
                                              OR (friend_id = $1 AND player_id <> $2))
               + (SELECT count(*) FROM friend_profiles WHERE player_id = $1) AS "friends!""#,
        from,
        into,
    )
    .fetch_one(pool)
    .await?;
    let identities = from_side
        .signs_in_with
        .iter()
        .filter(|kind| !into_side.signs_in_with.contains(kind))
        .cloned()
        .collect();
    Ok(Ok(Merge {
        from: from_side,
        into: into_side,
        profiles: counts.profiles,
        matches: counts.matches,
        between: counts.between,
        friends: counts.friends,
        identities,
    }))
}

/// Merges `from` into `into`, all at once: their WFC profiles, sets and
/// reports, chats, friends and Mii friends, sessions (their launchers stay
/// signed in, as `into`), and the accounts `into` has none of; the sets
/// between the two go, `from`'s search ends, and `from` is gone. Err: why not
/// (as [`plan`]).
pub async fn merge(
    pool: &PgPool,
    from: i64,
    into: i64,
) -> Result<Result<Merge, &'static str>, sqlx::Error> {
    let merged = match plan(pool, from, into).await? {
        Ok(merged) => merged,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "DELETE FROM matches WHERE (home = $1 AND away = $2) OR (home = $2 AND away = $1)",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE player_wfc_profiles SET player_id = $2 WHERE player_id = $1",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("UPDATE matches SET home = $2 WHERE home = $1", from, into)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("UPDATE matches SET away = $2 WHERE away = $1", from, into)
        .execute(&mut *tx)
        .await?;
    sqlx::query!(
        "UPDATE matches SET winner = $2 WHERE winner = $1",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE match_reports SET player_id = $2 WHERE player_id = $1",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM chat_messages WHERE (sender = $1 AND recipient = $2) OR (sender = $2 AND recipient = $1)",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE chat_messages SET sender = $2 WHERE sender = $1",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE chat_messages SET recipient = $2 WHERE recipient = $1",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO friends (player_id, friend_id)
         SELECT $2, friend_id FROM friends WHERE player_id = $1 AND friend_id <> $2
         ON CONFLICT DO NOTHING",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO friends (player_id, friend_id)
         SELECT player_id, $2 FROM friends WHERE friend_id = $1 AND player_id <> $2
         ON CONFLICT DO NOTHING",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO friend_profiles (player_id, profile_id)
         SELECT $2, profile_id FROM friend_profiles WHERE player_id = $1
         ON CONFLICT DO NOTHING",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE player_identities SET player_id = $2
         WHERE player_id = $1
           AND provider NOT IN (SELECT provider FROM player_identities WHERE player_id = $2)",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE player_sessions SET player_id = $2 WHERE player_id = $1",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE players SET
             heartbeat_at = GREATEST(players.heartbeat_at, old.heartbeat_at),
             last_seen_at = GREATEST(players.last_seen_at, old.last_seen_at)
         FROM players old
         WHERE players.player_id = $2 AND old.player_id = $1",
        from,
        into
    )
    .execute(&mut *tx)
    .await?;
    // What is left of `from` (its other accounts, its search) goes with it.
    sqlx::query!("DELETE FROM players WHERE player_id = $1", from)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Ok(merged))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WFC profile of a console `user_id` (made now), as its login makes it.
    async fn profile(pool: &PgPool, user_id: i64, gsbrcd: &str) -> i32 {
        crate::accounts::log_in(pool, user_id).await.unwrap();
        crate::accounts::profile_login(pool, user_id, gsbrcd, Some("Mii"), "10.0.0.1")
            .await
            .unwrap()
            .profile_id
    }

    async fn run(pool: &PgPool, sql: &str) {
        let mut conn = pool.acquire().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned()))
            .execute(&mut *conn)
            .await
            .unwrap();
    }

    async fn pairs(pool: &PgPool, sql: &str) -> Vec<(i64, i64)> {
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query_as(sqlx::AssertSqlSafe(sql.to_owned()))
            .fetch_all(&mut *conn)
            .await
            .unwrap()
    }

    #[sqlx::test]
    async fn a_player_merges_into_another_with_all_theirs(pool: PgPool) {
        // Birnen (1, Discord) and Birnen's Mii player (2), and Kiki (3).
        run(
            &pool,
            "INSERT INTO players (display_name) VALUES ('Birnen'), ('Birnen Mii'), ('Kiki')",
        )
        .await;
        let us = profile(&pool, 1, "R4QEaaaaaaa").await;
        let eu = profile(&pool, 2, "R4QPbbbbbbb").await;
        run(&pool, &format!(
            "INSERT INTO player_identities (provider, external_id, player_id, username) VALUES
                 ('discord', 'd1', 1, 'birnen'), ('wfc', '{eu}', 2, 'fc');
             INSERT INTO player_wfc_profiles (profile_id, player_id) VALUES ({us}, 1), ({eu}, 2);
             INSERT INTO matches (kind, state, home, away, rule_set, rules, rules_hash, winner) VALUES
                 ('test', 'finished', 2, 3, 'x', '{{}}', 'h', 2),
                 ('test', 'finished', 1, 2, 'x', '{{}}', 'h', 1);
             INSERT INTO friends (player_id, friend_id) VALUES (2, 3), (3, 2), (2, 1);
             INSERT INTO chat_messages (sender, recipient, body) VALUES (2, 3, 'hi'), (1, 2, 'me');
             INSERT INTO player_sessions (token_hash, player_id, expires_at)
                 VALUES (decode(repeat('ab', 32), 'hex'), 2, now() + interval '1 day');"
        )).await;

        assert!(plan(&pool, 1, 1).await.unwrap().is_err());
        assert!(plan(&pool, 2, 99).await.unwrap().is_err());
        // A set to play waits.
        run(
            &pool,
            "INSERT INTO matches (kind, state, home, away, rule_set, rules, rules_hash)
                    VALUES ('test', 'challenged', 3, 2, 'x', '{}', 'h')",
        )
        .await;
        assert!(merge(&pool, 2, 1).await.unwrap().is_err());
        run(&pool, "DELETE FROM matches WHERE state = 'challenged'").await;

        let merged = merge(&pool, 2, 1).await.unwrap().unwrap();
        assert_eq!(
            (
                merged.profiles,
                merged.matches,
                merged.between,
                merged.friends
            ),
            (1, 1, 1, 2)
        );
        assert_eq!(merged.identities, ["wfc"]);
        assert_eq!(merged.into.name, "Birnen");

        // Player 2 is gone; everything theirs is Birnen's.
        let left: Vec<i64> = sqlx::query_scalar("SELECT player_id FROM players ORDER BY 1")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(left, [1, 3]);
        assert_eq!(
            pairs(
                &pool,
                "SELECT profile_id::bigint, player_id FROM player_wfc_profiles ORDER BY 1"
            )
            .await,
            [(i64::from(us), 1), (i64::from(eu), 1)]
        );
        assert_eq!(
            pairs(&pool, "SELECT home, away FROM matches ORDER BY 1").await,
            [(1, 3)]
        );
        let winner: Option<i64> = sqlx::query_scalar("SELECT winner FROM matches")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(winner, Some(1));
        assert_eq!(
            pairs(
                &pool,
                "SELECT player_id, friend_id FROM friends ORDER BY 1, 2"
            )
            .await,
            [(1, 3), (3, 1)]
        );
        assert_eq!(
            pairs(&pool, "SELECT sender, recipient FROM chat_messages").await,
            [(1, 3)]
        );
        assert_eq!(
            pairs(
                &pool,
                "SELECT external_id::bigint, player_id FROM player_identities
                          WHERE provider = 'wfc'"
            )
            .await,
            [(i64::from(eu), 1)]
        );
        let session: i64 = sqlx::query_scalar("SELECT player_id FROM player_sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(session, 1);

        // Two Discord accounts are two people.
        run(
            &pool,
            "INSERT INTO player_identities (provider, external_id, player_id, username)
                    VALUES ('discord', 'd3', 3, 'kiki')",
        )
        .await;
        assert_eq!(
            plan(&pool, 3, 1).await.unwrap(),
            Err("both sign in with Discord: two accounts, likely two people")
        );
    }

    #[sqlx::test]
    async fn a_profile_is_linked_by_hand(pool: PgPool) {
        run(
            &pool,
            "INSERT INTO players (display_name) VALUES ('A'), ('B')",
        )
        .await;
        let mine = profile(&pool, 1, "R4QEaaaaaaa").await;
        let bound = |pool: PgPool| async move {
            sqlx::query_scalar::<_, i64>("SELECT player_id FROM player_wfc_profiles")
                .fetch_optional(&pool)
                .await
                .unwrap()
        };
        assert_eq!(link(&pool, mine, Some(1)).await.unwrap(), Ok(()));
        assert_eq!(bound(pool.clone()).await, Some(1));
        assert_eq!(link(&pool, mine, Some(2)).await.unwrap(), Ok(()));
        assert_eq!(bound(pool.clone()).await, Some(2));
        assert_eq!(
            link(&pool, mine, Some(99)).await.unwrap(),
            Err("no such player")
        );
        assert_eq!(
            link(&pool, 7, Some(1)).await.unwrap(),
            Err("no such WFC profile")
        );
        assert_eq!(link(&pool, mine, None).await.unwrap(), Ok(()));
        assert_eq!(bound(pool.clone()).await, None);
    }
}

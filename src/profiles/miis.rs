//! The game's own online players as Mii profiles (docs/profiles.md, "Mii
//! profiles"): the WFC profiles logged in to the game without a Recharged
//! launcher, shown by their Mii name, and the ones a player keeps as friends.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::PgPool;

use super::friends::MAX_FRIENDS;
use crate::gpcm::Presence;

/// The game's online players shown at most.
const MAX_ONLINE: usize = 100;

/// A WFC profile, as the launcher shows it: no friend code, only the Mii.
#[derive(Debug, PartialEq, Serialize)]
pub struct MiiProfile {
    pub profile_id: i32,
    /// The Mii name of its last login.
    pub mii: Option<String>,
    /// The release: R4QE, R4QP, R4QJ or R4QK.
    pub game: String,
    pub online: bool,
    /// While logged in, what its game does: `menu`, `matchmaking` or
    /// `match`, from DWC's status.
    pub status: Option<&'static str>,
    /// Its last login, in Unix seconds.
    pub last_online: i64,
    /// The player keeps it as a friend.
    pub friend: bool,
}

fn status(presence: &Presence) -> &'static str {
    match presence.status {
        Some(2) => "match",
        Some(3..=6) => "matchmaking",
        _ => "menu",
    }
}

/// The profiles `ids`, in that order, with their presence.
async fn profiles(
    pool: &PgPool,
    ids: &[i32],
    presence: &HashMap<i32, Presence>,
    friend: bool,
) -> Result<Vec<MiiProfile>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT profile_id, gsbrcd, ingamesn, last_login_at FROM profiles WHERE profile_id = ANY($1)",
        ids
    )
    .fetch_all(pool)
    .await?;
    Ok(ids
        .iter()
        .filter_map(|id| rows.iter().find(|row| row.profile_id == *id))
        .map(|row| {
            let logged_in = presence.get(&row.profile_id);
            MiiProfile {
                profile_id: row.profile_id,
                mii: row.ingamesn.clone().filter(|name| !name.is_empty()),
                game: row.gsbrcd.chars().take(4).collect(),
                online: logged_in.is_some(),
                status: logged_in.map(status),
                last_online: logged_in
                    .map_or(row.last_login_at, |presence| presence.logged_in_at)
                    .unix_timestamp(),
                friend,
            }
        })
        .collect())
}

/// Adds the Mii profile `profile_id` to `player_id`'s friends. `None` for
/// no such profile; else whether it was added (false for one already there,
/// or with the list full).
pub async fn add(
    pool: &PgPool,
    player_id: i64,
    profile_id: i32,
) -> Result<Option<bool>, sqlx::Error> {
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM profiles WHERE profile_id = $1) AS "exists!""#,
        profile_id
    )
    .fetch_one(pool)
    .await?;
    if !exists {
        return Ok(None);
    }
    let added = sqlx::query!(
        "INSERT INTO friend_profiles (player_id, profile_id)
         SELECT $1, $2
         WHERE (SELECT count(*) FROM friend_profiles WHERE player_id = $1) < $3
         ON CONFLICT DO NOTHING",
        player_id,
        profile_id,
        MAX_FRIENDS,
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(Some(added == 1))
}

/// Whether `profile_id` was among `player_id`'s Mii friends.
pub async fn remove(pool: &PgPool, player_id: i64, profile_id: i32) -> Result<bool, sqlx::Error> {
    let removed = sqlx::query!(
        "DELETE FROM friend_profiles WHERE player_id = $1 AND profile_id = $2",
        player_id,
        profile_id,
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(removed == 1)
}

/// `player_id`'s Mii friends: those logged in to the game first, by name,
/// then the others, the last online first.
pub async fn friends(
    pool: &PgPool,
    player_id: i64,
    presence: &HashMap<i32, Presence>,
) -> Result<Vec<MiiProfile>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        "SELECT profile_id FROM friend_profiles WHERE player_id = $1",
        player_id
    )
    .fetch_all(pool)
    .await?;
    let mut friends = profiles(pool, &ids, presence, true).await?;
    friends.sort_by(|a, b| {
        b.online
            .cmp(&a.online)
            .then_with(|| match a.online {
                true => a.mii.cmp(&b.mii),
                false => b.last_online.cmp(&a.last_online),
            })
            .then(a.profile_id.cmp(&b.profile_id))
    });
    Ok(friends)
}

/// The profiles logged in to the game whose player isn't in Recharged's
/// lists already, as `player_id` sees them, by Mii name: not their own, not a
/// player's whose launcher is open, not their Mii friends.
pub async fn online(
    pool: &PgPool,
    player_id: i64,
    presence: &HashMap<i32, Presence>,
) -> Result<Vec<MiiProfile>, sqlx::Error> {
    let logged_in: Vec<i32> = presence.keys().copied().collect();
    let ids = sqlx::query_scalar!(
        r#"SELECT profiles.profile_id AS "profile_id!" FROM profiles
           WHERE profiles.profile_id = ANY($1)
             AND NOT EXISTS (
                 SELECT 1 FROM player_wfc_profiles JOIN players USING (player_id)
                 WHERE player_wfc_profiles.profile_id = profiles.profile_id
                   AND (players.player_id = $2
                        OR players.heartbeat_at > now() - make_interval(secs => $3)))
             AND NOT EXISTS (
                 SELECT 1 FROM friend_profiles
                 WHERE friend_profiles.player_id = $2
                   AND friend_profiles.profile_id = profiles.profile_id)
           ORDER BY lower(profiles.ingamesn), profiles.profile_id"#,
        &logged_in,
        player_id,
        f64::from(super::matches::HEARTBEAT_TIMEOUT_SECS),
    )
    .fetch_all(pool)
    .await?;
    let ids = &ids[..ids.len().min(MAX_ONLINE)];
    profiles(pool, ids, presence, false).await
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::*;
    use crate::profiles::players::{self, Identity};

    fn logged_in(status: Option<u8>) -> Presence {
        Presence {
            logged_in_at: OffsetDateTime::now_utc(),
            status,
            status_since: None,
            friend_requests: 0,
            mutual_friends: 0,
        }
    }

    /// A WFC profile of the game, as its login makes it: its id.
    async fn profile(pool: &PgPool, user_id: i64, gsbrcd: &str, mii: &str) -> i32 {
        crate::accounts::log_in(pool, user_id).await.unwrap();
        crate::accounts::profile_login(pool, user_id, gsbrcd, Some(mii), "10.0.0.1")
            .await
            .unwrap()
            .profile_id
    }

    async fn player(pool: &PgPool, id: &str, name: &str) -> i64 {
        players::sign_in(
            pool,
            &Identity {
                provider: "discord",
                external_id: id,
                username: name,
                display_name: name,
                avatar_url: None,
            },
        )
        .await
        .unwrap()
    }

    #[sqlx::test]
    async fn the_games_players_are_those_not_in_rechargeds_lists(pool: PgPool) {
        let mine = profile(&pool, 1, "R4QEaaaaaaa", "Nelly").await;
        let launcher = profile(&pool, 2, "R4QEbbbbbbb", "Sam").await;
        let kiki = profile(&pool, 3, "R4QPccccccc", "Kiki").await;
        let bo = profile(&pool, 4, "R4QEddddddd", "Bo").await;
        let offline = profile(&pool, 5, "R4QEeeeeeee", "Zed").await;
        let nelly = player(&pool, "1", "nelly").await;
        let sam = player(&pool, "2", "sam").await;
        players::bind_wfc(&pool, nelly, mine).await.unwrap();
        // Sam's launcher is open (the sign-in is a heartbeat): Sam shows as a player.
        players::bind_wfc(&pool, sam, launcher).await.unwrap();
        let presence: HashMap<i32, Presence> = [
            (mine, logged_in(Some(1))),
            (launcher, logged_in(Some(1))),
            (kiki, logged_in(Some(2))),
            (bo, logged_in(Some(4))),
        ]
        .into_iter()
        .collect();

        let names = |list: &[MiiProfile]| {
            list.iter()
                .map(|profile| (profile.mii.clone().unwrap(), profile.status))
                .collect::<Vec<_>>()
        };
        let game = online(&pool, nelly, &presence).await.unwrap();
        assert_eq!(
            names(&game),
            [
                ("Bo".to_owned(), Some("matchmaking")),
                ("Kiki".to_owned(), Some("match"))
            ]
        );
        assert_eq!(
            (game[1].game.as_str(), game[1].online, game[1].friend),
            ("R4QP", true, false)
        );

        // A Mii friend leaves that list for the friends'.
        assert_eq!(add(&pool, nelly, kiki).await.unwrap(), Some(true));
        assert_eq!(add(&pool, nelly, kiki).await.unwrap(), Some(false));
        assert_eq!(add(&pool, nelly, offline).await.unwrap(), Some(true));
        assert_eq!(add(&pool, nelly, 999).await.unwrap(), None);
        assert_eq!(
            names(&online(&pool, nelly, &presence).await.unwrap()),
            [("Bo".to_owned(), Some("matchmaking"))]
        );
        let mii_friends = friends(&pool, nelly, &presence).await.unwrap();
        assert_eq!(
            names(&mii_friends),
            [("Kiki".to_owned(), Some("match")), ("Zed".to_owned(), None)]
        );
        assert!(mii_friends.iter().all(|friend| friend.friend));
        assert!(!mii_friends[1].online);

        assert!(remove(&pool, nelly, kiki).await.unwrap());
        assert!(!remove(&pool, nelly, kiki).await.unwrap());
        assert_eq!(friends(&pool, nelly, &presence).await.unwrap().len(), 1);
    }
}

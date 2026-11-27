//! The game's own online players as Mii profiles (docs/profiles.md, "Mii
//! profiles"): the WFC profiles logged in to the game without a Recharged
//! launcher, shown by their Mii name and friend code, and the ones a player
//! keeps as friends.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::PgPool;

use super::friends::MAX_FRIENDS;
use super::players::Activity;
use crate::gpcm::{Matchmaking, Presence};

/// A WFC profile, as the launcher shows it: its Mii and friend code.
#[derive(Debug, PartialEq, Serialize)]
pub struct MiiProfile {
    pub profile_id: i32,
    /// The Mii name of its last login.
    pub mii: Option<String>,
    /// Its Mii in Base64, when its player's launcher sent it
    /// (docs/profiles.md, "Miis").
    pub mii_data: Option<String>,
    /// The release: R4QE, R4QP, R4QJ or R4QK.
    pub game: String,
    /// As the game shows it, for that release.
    pub friend_code: String,
    pub online: bool,
    /// While logged in, what its game does: `menu`, `matchmaking` or
    /// `match`, from DWC's status.
    pub status: Option<&'static str>,
    /// While matchmaking or in a match: `ranked` (matched with anybody) or
    /// `friends`.
    #[serde(rename = "match")]
    pub match_kind: Option<&'static str>,
    /// Its last login, in Unix seconds.
    pub last_online: i64,
    /// The player keeps it as a friend.
    pub friend: bool,
    /// The Recharged player whose profile it is, when a launcher proved it
    /// (a Mii or Discord player): they are challenged as that player.
    pub player_id: Option<i64>,
    /// While that player has Recharged open: what it does, `launcher`, `game`
    /// or `match` (see players.rs, `Activity`).
    pub player_activity: Option<&'static str>,
}

fn status(presence: &Presence) -> &'static str {
    match presence.status {
        Some(2) => "match",
        // A friend room's host plays once a friend has joined.
        Some(6) if presence.room_players.is_some_and(|players| players >= 2) => "match",
        Some(3..=6) => "matchmaking",
        _ => "menu",
    }
}

fn match_kind(presence: &Presence) -> Option<&'static str> {
    match presence.matchmaking? {
        Matchmaking::Anybody => Some("ranked"),
        Matchmaking::Friends => Some("friends"),
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
        r#"SELECT profiles.profile_id, profiles.gsbrcd, profiles.ingamesn, profiles.last_login_at,
                  profiles.mii_data, bound.player_id AS "player_id?",
                  players.heartbeat_at > now() - make_interval(secs => $2) AS "open?",
                  players.activity AS "activity?"
           FROM profiles
           LEFT JOIN player_wfc_profiles bound ON bound.profile_id = profiles.profile_id
           LEFT JOIN players ON players.player_id = bound.player_id
           WHERE profiles.profile_id = ANY($1)"#,
        ids,
        f64::from(super::matches::HEARTBEAT_TIMEOUT_SECS),
    )
    .fetch_all(pool)
    .await?;
    Ok(ids
        .iter()
        .filter_map(|id| rows.iter().find(|row| row.profile_id == *id))
        .map(|row| {
            let logged_in = presence.get(&row.profile_id);
            let game: String = row.gsbrcd.chars().take(4).collect();
            MiiProfile {
                profile_id: row.profile_id,
                mii: row.ingamesn.clone().filter(|name| !name.is_empty()),
                mii_data: row.mii_data.as_deref().map(super::mii_data::encode),
                friend_code: crate::accounts::friend_code(row.profile_id, &game),
                game,
                online: logged_in.is_some(),
                status: logged_in.map(status),
                match_kind: logged_in.and_then(match_kind),
                last_online: logged_in
                    .map_or(row.last_login_at, |presence| presence.logged_in_at)
                    .unix_timestamp(),
                friend,
                player_id: row.player_id,
                player_activity: (row.player_id.is_some() && row.open == Some(true))
                    .then(|| Activity::from_header(row.activity.as_deref()).name()),
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
            matchmaking: None,
            room_players: None,
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
    async fn mii_friends_are_kept_online_first(pool: PgPool) {
        let kiki = profile(&pool, 3, "R4QPccccccc", "Kiki").await;
        let offline = profile(&pool, 5, "R4QEeeeeeee", "Zed").await;
        let nelly = player(&pool, "1", "nelly").await;
        let presence: HashMap<i32, Presence> = [(kiki, logged_in(Some(2)))].into_iter().collect();

        let names = |list: &[MiiProfile]| {
            list.iter()
                .map(|profile| (profile.mii.clone().unwrap(), profile.status))
                .collect::<Vec<_>>()
        };
        assert_eq!(add(&pool, nelly, kiki).await.unwrap(), Some(true));
        assert_eq!(add(&pool, nelly, kiki).await.unwrap(), Some(false));
        assert_eq!(add(&pool, nelly, offline).await.unwrap(), Some(true));
        assert_eq!(add(&pool, nelly, 999).await.unwrap(), None);
        let mii_friends = friends(&pool, nelly, &presence).await.unwrap();
        assert_eq!(
            names(&mii_friends),
            [("Kiki".to_owned(), Some("match")), ("Zed".to_owned(), None)]
        );
        assert!(mii_friends.iter().all(|friend| friend.friend));
        assert_eq!(
            (mii_friends[0].game.as_str(), mii_friends[0].online),
            ("R4QP", true)
        );
        assert_eq!(
            mii_friends[0].friend_code,
            crate::accounts::friend_code(kiki, "R4QP")
        );
        assert!(!mii_friends[1].online);

        assert!(remove(&pool, nelly, kiki).await.unwrap());
        assert!(!remove(&pool, nelly, kiki).await.unwrap());
        assert_eq!(friends(&pool, nelly, &presence).await.unwrap().len(), 1);
    }

    #[sqlx::test]
    async fn a_mii_friend_of_a_recharged_player_is_that_player(pool: PgPool) {
        let nelly = player(&pool, "1", "nelly").await;
        let sam = player(&pool, "2", "sam").await;
        let sams = profile(&pool, 2, "R4QEbbbbbbb", "Sam").await;
        let wii = profile(&pool, 3, "R4QEccccccc", "Kiki").await;
        players::bind_wfc(&pool, sam, sams).await.unwrap();
        add(&pool, nelly, sams).await.unwrap();
        add(&pool, nelly, wii).await.unwrap();
        let presence = HashMap::new();
        let shown = |list: &[MiiProfile]| {
            list.iter()
                .map(|friend| {
                    (
                        friend.mii.clone().unwrap(),
                        friend.player_id,
                        friend.player_activity,
                    )
                })
                .collect::<Vec<_>>()
        };

        // Sam's Recharged is open (the sign-in is a heartbeat): challenged as
        // Sam; a Wii's profile has no player.
        let mut seen = shown(&friends(&pool, nelly, &presence).await.unwrap());
        seen.sort();
        assert_eq!(
            seen,
            [
                ("Kiki".to_owned(), None, None),
                ("Sam".to_owned(), Some(sam), Some("launcher"))
            ]
        );
        sqlx::query!(
            "UPDATE players SET activity = 'match' WHERE player_id = $1",
            sam
        )
        .execute(&pool)
        .await
        .unwrap();
        let in_match = friends(&pool, nelly, &presence).await.unwrap();
        let sam_row = in_match
            .iter()
            .find(|friend| friend.player_id == Some(sam))
            .unwrap();
        assert_eq!(sam_row.player_activity, Some("match"));
        // Closed: still Sam, without an activity.
        sqlx::query!(
            "UPDATE players SET heartbeat_at = now() - interval '1 hour' WHERE player_id = $1",
            sam
        )
        .execute(&pool)
        .await
        .unwrap();
        let closed = friends(&pool, nelly, &presence).await.unwrap();
        let sam_row = closed
            .iter()
            .find(|friend| friend.player_id == Some(sam))
            .unwrap();
        assert_eq!(sam_row.player_activity, None);
    }

    #[test]
    fn a_mii_shows_whom_its_match_is_with() {
        let shown = |status, matchmaking, room_players| {
            let presence = Presence {
                matchmaking,
                room_players,
                ..logged_in(Some(status))
            };
            (super::status(&presence), match_kind(&presence))
        };
        assert_eq!(shown(1, None, None), ("menu", None));
        assert_eq!(
            shown(3, Some(Matchmaking::Anybody), None),
            ("matchmaking", Some("ranked"))
        );
        assert_eq!(
            shown(2, Some(Matchmaking::Anybody), None),
            ("match", Some("ranked"))
        );
        assert_eq!(
            shown(2, Some(Matchmaking::Friends), None),
            ("match", Some("friends"))
        );
        // A game that logged in playing (the server restarted) doesn't say.
        assert_eq!(shown(2, None, None), ("match", None));
        // A friend room's host waits alone, then plays with whoever joined.
        assert_eq!(
            shown(6, Some(Matchmaking::Friends), Some(1)),
            ("matchmaking", Some("friends"))
        );
        assert_eq!(
            shown(6, Some(Matchmaking::Friends), Some(2)),
            ("match", Some("friends"))
        );
    }
}

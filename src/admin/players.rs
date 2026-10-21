//! The Players page: the accounts consoles registered, their profiles (one per
//! Mii slot) with their friend codes, and who is logged in to GPCM.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;

use super::page::snapshot;
use crate::{
    accounts,
    gpcm::{Gpcm, Presence},
};

/// Profiles shown, the last logins first.
const SHOWN: i64 = 200;

#[derive(Serialize)]
pub struct Players {
    totals: Totals,
    /// The [`SHOWN`] profiles that logged in last, online ones first.
    profiles: Vec<Player>,
}

#[derive(Serialize)]
pub struct Totals {
    accounts: i64,
    profiles: i64,
    /// Logged in to GPCM now; the CPU buddy does not count.
    online: usize,
    #[serde(with = "time::serde::rfc3339::option")]
    last_login: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct Player {
    profile_id: i32,
    user_id: i64,
    gsbrcd: String,
    /// The code a player of the profile's own release enters.
    friend_code: String,
    mii_name: Option<String>,
    /// Answered by the server itself (docs/login.md, "The CPU buddy").
    cpu_buddy: bool,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    last_login_at: OffsetDateTime,
    last_ip: Option<String>,
    /// Rows in `scores`.
    scores: i64,
    online: Option<Online>,
}

/// A login to GPCM.
#[derive(Serialize)]
struct Online {
    #[serde(with = "time::serde::rfc3339")]
    since: OffsetDateTime,
    status: Option<u8>,
    friend_requests: usize,
    mutual_friends: usize,
}

impl From<&Presence> for Online {
    fn from(presence: &Presence) -> Self {
        Self {
            since: presence.logged_in_at,
            status: presence.status,
            friend_requests: presence.friend_requests,
            mutual_friends: presence.mutual_friends,
        }
    }
}

pub async fn read(pool: &PgPool, gpcm: &Gpcm) -> Result<Players, sqlx::Error> {
    let presence = gpcm.presence();
    let mut tx = snapshot(pool).await?;
    let players = Players {
        totals: totals(&mut tx, &presence).await?,
        profiles: profiles(&mut tx, &presence).await?,
    };
    tx.commit().await?;
    Ok(players)
}

pub async fn totals(
    conn: &mut PgConnection,
    presence: &HashMap<i32, Presence>,
) -> Result<Totals, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT (SELECT count(*) FROM accounts WHERE user_id <> $1) AS "accounts!",
                  (SELECT count(*) FROM profiles WHERE profile_id <> $2) AS "profiles!",
                  (SELECT max(last_login_at) FROM profiles WHERE profile_id <> $2) AS last_login"#,
        accounts::CPU_BUDDY_ACCOUNT,
        accounts::CPU_BUDDY,
    )
    .fetch_one(conn)
    .await?;
    Ok(Totals {
        accounts: row.accounts,
        profiles: row.profiles,
        online: presence.len(),
        last_login: row.last_login,
    })
}

async fn profiles(
    conn: &mut PgConnection,
    presence: &HashMap<i32, Presence>,
) -> Result<Vec<Player>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT p.profile_id, p.user_id, p.gsbrcd, p.ingamesn, p.created_at,
                  p.last_login_at, p.last_ip,
                  (SELECT count(*) FROM scores s WHERE s.profile_id = p.profile_id) AS "scores!"
           FROM profiles p
           ORDER BY p.last_login_at DESC, p.profile_id
           LIMIT $1"#,
        SHOWN,
    )
    .fetch_all(conn)
    .await?;
    let mut players: Vec<Player> = rows
        .into_iter()
        .map(|row| Player {
            friend_code: accounts::friend_code(
                row.profile_id,
                row.gsbrcd.get(..4).unwrap_or_default(),
            ),
            cpu_buddy: row.profile_id == accounts::CPU_BUDDY,
            online: presence.get(&row.profile_id).map(Online::from),
            profile_id: row.profile_id,
            user_id: row.user_id,
            gsbrcd: row.gsbrcd,
            mii_name: row.ingamesn,
            created_at: row.created_at,
            last_login_at: row.last_login_at,
            last_ip: row.last_ip,
            scores: row.scores,
        })
        .collect();
    // The CPU buddy, then who is online; each in the order of their logins.
    players.sort_by_key(|player| (!player.cpu_buddy, player.online.is_none()));
    Ok(players)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nas::Tickets;
    use std::sync::Arc;

    #[sqlx::test]
    async fn players_list_profiles_with_friend_codes_and_the_cpu_buddy_first(pool: PgPool) {
        accounts::log_in(&pool, 42).await.unwrap();
        let profile = accounts::profile_login(&pool, 42, "R4QEaaaaaaa", Some("Mario"), "10.0.0.1")
            .await
            .unwrap();
        let gpcm = Gpcm::new(pool.clone(), Arc::new(Tickets::default()));

        let players = read(&pool, &gpcm).await.unwrap();
        assert_eq!((players.totals.accounts, players.totals.profiles), (1, 1));
        assert_eq!(players.totals.online, 0);
        let [buddy, mario] = &players.profiles[..] else {
            panic!("two profiles");
        };
        assert!(buddy.cpu_buddy);
        assert_eq!(buddy.friend_code, "1770-9365-9135");
        assert_eq!(mario.profile_id, profile.profile_id);
        assert_eq!(mario.friend_code, "0868-9934-5920");
        assert_eq!(mario.mii_name.as_deref(), Some("Mario"));
        assert!(mario.online.is_none());
    }
}

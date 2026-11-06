//! Bans (docs/profiles.md, "Deleting a player"): an admin who deletes a player
//! bans their consoles and sign-ins with them. NAS and GPCM refuse the
//! consoles' logins, every Mii's on them, and a banned Discord account makes
//! no new player. A profile's page lifts a ban.

use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;

use crate::accounts;

#[derive(Debug, PartialEq, Serialize)]
pub struct Ban {
    pub ban_id: i64,
    /// Whom it was for: the Recharged player's name, else the Mii name.
    pub name: String,
    pub banned_by: String,
    #[serde(with = "time::serde::rfc3339")]
    pub banned_at: OffsetDateTime,
}

/// Whether the console `user_id` is banned.
pub async fn console_banned(pool: &PgPool, user_id: i64) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM banned_accounts WHERE user_id = $1) AS "banned!""#,
        user_id
    )
    .fetch_one(pool)
    .await
}

/// Whether the sign-in `provider`/`external_id` (a Discord account) is banned.
pub async fn identity_banned(
    pool: &PgPool,
    provider: &str,
    external_id: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM banned_identities
                          WHERE provider = $1 AND external_id = $2) AS "banned!""#,
        provider,
        external_id
    )
    .fetch_one(pool)
    .await
}

/// Bans, as `name`, the consoles of `profiles` and the sign-ins of
/// `player_id` but its WFC profiles (the consoles cover those). Consoles
/// banned already keep their ban, and the CPU buddy's account, which a
/// console may claim too (NAS registers any userid), is never banned. Run before the player goes, whose sign-ins
/// go with them. The new ban.
pub async fn ban(
    conn: &mut PgConnection,
    name: &str,
    banned_by: &str,
    profiles: &[i32],
    player_id: Option<i64>,
) -> Result<i64, sqlx::Error> {
    let ban_id = sqlx::query_scalar!(
        "INSERT INTO bans (name, banned_by) VALUES ($1, $2) RETURNING ban_id",
        name,
        banned_by
    )
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query!(
        "INSERT INTO banned_accounts (user_id, ban_id)
         SELECT DISTINCT user_id, $2::BIGINT FROM profiles
         WHERE profile_id = ANY($1) AND user_id <> $3
         ON CONFLICT DO NOTHING",
        profiles,
        ban_id,
        accounts::CPU_BUDDY_ACCOUNT,
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "INSERT INTO banned_identities (provider, external_id, ban_id)
         SELECT provider, external_id, $2::BIGINT FROM player_identities
         WHERE player_id = $1 AND provider <> 'wfc'
         ON CONFLICT DO NOTHING",
        player_id,
        ban_id
    )
    .execute(&mut *conn)
    .await?;
    Ok(ban_id)
}

/// Every WFC profile of the consoles `ban_id` covers: their games are to
/// leave GPCM.
pub async fn profiles(pool: &PgPool, ban_id: i64) -> Result<Vec<i32>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT profile_id FROM profiles
         WHERE user_id IN (SELECT user_id FROM banned_accounts WHERE ban_id = $1)
         ORDER BY profile_id",
        ban_id
    )
    .fetch_all(pool)
    .await
}

/// The ban of the console `user_id`, if it has one.
pub async fn of_console(conn: &mut PgConnection, user_id: i64) -> Result<Option<Ban>, sqlx::Error> {
    sqlx::query_as!(
        Ban,
        "SELECT bans.ban_id, name, banned_by, banned_at
         FROM banned_accounts JOIN bans USING (ban_id)
         WHERE banned_accounts.user_id = $1",
        user_id
    )
    .fetch_optional(conn)
    .await
}

/// Lifts `ban_id`: its consoles log in again, its sign-ins make a new player.
/// The ban, if there was one.
pub async fn lift(pool: &PgPool, ban_id: i64) -> Result<Option<Ban>, sqlx::Error> {
    sqlx::query_as!(
        Ban,
        "DELETE FROM bans WHERE ban_id = $1 RETURNING ban_id, name, banned_by, banned_at",
        ban_id
    )
    .fetch_optional(pool)
    .await
}

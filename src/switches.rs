//! Settings an owner switches on the admin dashboard's Maintenance page. They
//! live in the database, so they survive restarts and updates; until an owner
//! switches one, the environment's value stands.

use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;

/// Sign-in with a name alone, without Discord (docs/profiles.md).
pub const TEST_SIGN_IN: &str = "test_sign_in";

/// How an owner last set a switch.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Switch {
    pub enabled: bool,
    pub changed_by: String,
    #[serde(with = "time::serde::rfc3339")]
    pub changed_at: OffsetDateTime,
}

/// The switch `name`, if an owner ever set it.
pub async fn get(pool: &PgPool, name: &str) -> Result<Option<Switch>, sqlx::Error> {
    sqlx::query_as!(
        Switch,
        "SELECT enabled, changed_by, changed_at FROM switches WHERE name = $1",
        name
    )
    .fetch_optional(pool)
    .await
}

/// Whether `name` is on: the owner's switch, else `default`.
pub async fn enabled(pool: &PgPool, name: &str, default: bool) -> Result<bool, sqlx::Error> {
    Ok(get(pool, name)
        .await?
        .map_or(default, |switch| switch.enabled))
}

/// Sets `name` for the owner `by`.
pub async fn set(
    pool: &PgPool,
    name: &str,
    enabled: bool,
    by: &str,
) -> Result<Switch, sqlx::Error> {
    sqlx::query_as!(
        Switch,
        "INSERT INTO switches (name, enabled, changed_by) VALUES ($1, $2, $3)
         ON CONFLICT (name) DO UPDATE
         SET enabled = excluded.enabled, changed_by = excluded.changed_by, changed_at = now()
         RETURNING enabled, changed_by, changed_at",
        name,
        enabled,
        by
    )
    .fetch_one(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test]
    async fn the_last_switch_wins_over_the_default(pool: PgPool) {
        assert_eq!(get(&pool, TEST_SIGN_IN).await.unwrap(), None);
        assert!(enabled(&pool, TEST_SIGN_IN, true).await.unwrap());

        let off = set(&pool, TEST_SIGN_IN, false, "owner@example.com")
            .await
            .unwrap();
        assert!(!off.enabled);
        assert_eq!(off.changed_by, "owner@example.com");
        assert!(!enabled(&pool, TEST_SIGN_IN, true).await.unwrap());

        set(&pool, TEST_SIGN_IN, true, "other@example.com")
            .await
            .unwrap();
        let switch = get(&pool, TEST_SIGN_IN).await.unwrap().unwrap();
        assert!(switch.enabled);
        assert_eq!(switch.changed_by, "other@example.com");
        assert!(enabled(&pool, TEST_SIGN_IN, false).await.unwrap());
    }
}

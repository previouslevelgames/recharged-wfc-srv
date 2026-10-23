//! Players, the accounts they sign in with, and the launchers' sessions
//! (docs/profiles.md).

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

/// How long a launcher stays signed in.
pub const SESSION_TTL: Duration = Duration::days(90);
/// `last_used_at` is written at most this often.
const USE_RESOLUTION: Duration = Duration::hours(1);
/// Session tokens: 32 random bytes in Base64url.
const TOKEN_LEN: usize = 43;

/// An account to sign in with: Discord's, or a test name.
pub struct Identity<'a> {
    /// `discord` or `test`.
    pub provider: &'static str,
    pub external_id: &'a str,
    pub username: &'a str,
    pub display_name: &'a str,
    pub avatar_url: Option<&'a str>,
}

/// A player as the launcher and other players see them.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Player {
    pub id: i64,
    pub name: String,
    pub avatar_url: Option<String>,
    /// The Discord username they sign in with.
    pub discord: Option<String>,
    /// A test player (PROFILES_TEST_SIGN_IN).
    pub test: bool,
}

/// Signs in with `identity`: its player, made at the first sign-in. The
/// player's name and picture follow the account's.
pub async fn sign_in(pool: &PgPool, identity: &Identity<'_>) -> Result<i64, sqlx::Error> {
    loop {
        let mut transaction = pool.begin().await?;
        if let Some(player_id) = sqlx::query_scalar!(
            "UPDATE player_identities SET username = $3, avatar_url = $4
             WHERE provider = $1 AND external_id = $2
             RETURNING player_id",
            identity.provider,
            identity.external_id,
            identity.username,
            identity.avatar_url,
        )
        .fetch_optional(&mut *transaction)
        .await?
        {
            sqlx::query!(
                "UPDATE players SET display_name = $2, last_seen_at = now() WHERE player_id = $1",
                player_id,
                identity.display_name,
            )
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
            return Ok(player_id);
        }

        let player_id = sqlx::query_scalar!(
            "INSERT INTO players (display_name) VALUES ($1) RETURNING player_id",
            identity.display_name,
        )
        .fetch_one(&mut *transaction)
        .await?;
        // A first sign-in of the same account elsewhere may come first: then
        // start again and take its player.
        if sqlx::query_scalar!(
            "INSERT INTO player_identities (provider, external_id, player_id, username, avatar_url)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT DO NOTHING
             RETURNING player_id",
            identity.provider,
            identity.external_id,
            player_id,
            identity.username,
            identity.avatar_url,
        )
        .fetch_optional(&mut *transaction)
        .await?
        .is_some()
        {
            transaction.commit().await?;
            return Ok(player_id);
        }
    }
}

pub async fn player(pool: &PgPool, player_id: i64) -> Result<Option<Player>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT players.player_id, players.display_name,
                  discord.username AS "discord?", discord.avatar_url AS "avatar_url?",
                  test.username AS "test?"
           FROM players
           LEFT JOIN player_identities discord
                  ON discord.player_id = players.player_id AND discord.provider = 'discord'
           LEFT JOIN player_identities test
                  ON test.player_id = players.player_id AND test.provider = 'test'
           WHERE players.player_id = $1"#,
        player_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| Player {
        id: row.player_id,
        name: row.display_name,
        avatar_url: row.avatar_url,
        discord: row.discord,
        test: row.test.is_some(),
    }))
}

/// Opens a launcher session for `player_id`. Returns its token, which only
/// the launcher keeps.
pub async fn open_session(pool: &PgPool, player_id: i64) -> Result<String, sqlx::Error> {
    let token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let token_hash = hash(&token);
    sqlx::query!(
        "INSERT INTO player_sessions (token_hash, player_id, expires_at) VALUES ($1, $2, $3)",
        token_hash.as_slice(),
        player_id,
        OffsetDateTime::now_utc() + SESSION_TTL,
    )
    .execute(pool)
    .await?;
    Ok(token)
}

/// The player signed in with `token`, unless the session expired or was
/// closed.
pub async fn session(pool: &PgPool, token: &str) -> Result<Option<i64>, sqlx::Error> {
    if token.len() != TOKEN_LEN {
        return Ok(None);
    }
    let token_hash = hash(token);
    let Some(row) = sqlx::query!(
        "SELECT player_id, last_used_at FROM player_sessions
         WHERE token_hash = $1 AND expires_at > now()",
        token_hash.as_slice(),
    )
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    if OffsetDateTime::now_utc() - row.last_used_at > USE_RESOLUTION {
        sqlx::query!(
            "UPDATE player_sessions SET last_used_at = now() WHERE token_hash = $1",
            token_hash.as_slice(),
        )
        .execute(pool)
        .await?;
        sqlx::query!(
            "UPDATE players SET last_seen_at = now() WHERE player_id = $1",
            row.player_id,
        )
        .execute(pool)
        .await?;
    }
    Ok(Some(row.player_id))
}

/// Closes the session of `token`. Whether there was one.
pub async fn close_session(pool: &PgPool, token: &str) -> Result<bool, sqlx::Error> {
    let token_hash = hash(token);
    let closed = sqlx::query!(
        "DELETE FROM player_sessions WHERE token_hash = $1",
        token_hash.as_slice(),
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(closed == 1)
}

fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discord<'a>(id: &'a str, username: &'a str, name: &'a str) -> Identity<'a> {
        Identity {
            provider: "discord",
            external_id: id,
            username,
            display_name: name,
            avatar_url: Some("https://cdn.discordapp.com/embed/avatars/0.png"),
        }
    }

    #[sqlx::test]
    async fn an_account_keeps_its_player(pool: PgPool) {
        let first = sign_in(&pool, &discord("1", "nelly", "Nelly"))
            .await
            .unwrap();
        let again = sign_in(&pool, &discord("1", "nelly2", "Nelly 2"))
            .await
            .unwrap();
        let other = sign_in(&pool, &discord("2", "sam", "Sam")).await.unwrap();
        assert_eq!(first, again);
        assert_ne!(first, other);

        let player = player(&pool, first).await.unwrap().unwrap();
        assert_eq!(player.name, "Nelly 2");
        assert_eq!(player.discord.as_deref(), Some("nelly2"));
        assert!(!player.test);
    }

    #[sqlx::test]
    async fn a_session_lasts_until_it_is_closed(pool: PgPool) {
        let player_id = sign_in(&pool, &discord("1", "nelly", "Nelly"))
            .await
            .unwrap();
        let token = open_session(&pool, player_id).await.unwrap();
        assert_eq!(token.len(), TOKEN_LEN);
        assert_eq!(session(&pool, &token).await.unwrap(), Some(player_id));
        assert_eq!(session(&pool, "another token").await.unwrap(), None);
        assert!(close_session(&pool, &token).await.unwrap());
        assert_eq!(session(&pool, &token).await.unwrap(), None);
        assert!(!close_session(&pool, &token).await.unwrap());
    }
}

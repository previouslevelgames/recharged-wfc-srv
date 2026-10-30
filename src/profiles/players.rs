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
/// player's name and picture follow the account's, and the sign-in is a
/// heartbeat (the launcher is open).
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
                "UPDATE players SET display_name = $2, last_seen_at = now(), heartbeat_at = now(), activity = NULL
                 WHERE player_id = $1",
                player_id,
                identity.display_name,
            )
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
            return Ok(player_id);
        }

        let player_id = sqlx::query_scalar!(
            "INSERT INTO players (display_name, heartbeat_at) VALUES ($1, now()) RETURNING player_id",
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

/// The player's Discord account id (the long number), which never changes;
/// none for test players. The launcher makes the player's Mii from it.
pub async fn discord_id(pool: &PgPool, player_id: i64) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT external_id FROM player_identities WHERE player_id = $1 AND provider = 'discord'",
        player_id,
    )
    .fetch_optional(pool)
    .await
}

/// What a player's Recharged is doing (docs/profiles.md, "Activity").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Launcher,
    /// The game, outside a match.
    Game,
    /// A match in the game: offline, classic online or a set's.
    Match,
}

impl Activity {
    /// The `Recharged-Activity` header's value; anything else (or none) is
    /// the launcher, which never sends one.
    pub fn from_header(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("game") => Self::Game,
            Some("match") => Self::Match,
            _ => Self::Launcher,
        }
    }

    /// As stored in `players.activity`.
    fn column(self) -> Option<&'static str> {
        match self {
            Self::Launcher => None,
            Self::Game => Some("game"),
            Self::Match => Some("match"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Launcher => "launcher",
            Self::Game => "game",
            Self::Match => "match",
        }
    }
}

/// `player_id`'s launcher or game made a request: they have Recharged open,
/// doing `activity` (written at most every few seconds, and at once when the
/// activity changes).
pub async fn heartbeat(
    pool: &PgPool,
    player_id: i64,
    activity: Activity,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE players SET heartbeat_at = now(), activity = $2
         WHERE player_id = $1
           AND (heartbeat_at IS NULL OR heartbeat_at < now() - interval '5 seconds'
                OR activity IS DISTINCT FROM $2)",
        player_id,
        activity.column(),
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether `player_id` has Recharged open.
pub async fn is_online(pool: &PgPool, player_id: i64) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar!(
        "SELECT 1 AS \"online!\" FROM players
         WHERE player_id = $1 AND heartbeat_at > now() - make_interval(secs => $2)",
        player_id,
        f64::from(super::matches::HEARTBEAT_TIMEOUT_SECS),
    )
    .fetch_optional(pool)
    .await?
    .is_some())
}

/// What `player_id`'s Recharged is doing; none while it is closed.
pub async fn activity(pool: &PgPool, player_id: i64) -> Result<Option<Activity>, sqlx::Error> {
    Ok(sqlx::query!(
        "SELECT activity FROM players
         WHERE player_id = $1 AND heartbeat_at > now() - make_interval(secs => $2)",
        player_id,
        f64::from(super::matches::HEARTBEAT_TIMEOUT_SECS),
    )
    .fetch_optional(pool)
    .await?
    .map(|row| Activity::from_header(row.activity.as_deref())))
}

/// A player in the launcher's lists: whether they have Recharged open and a
/// set to play, when they last had it open, and whether the player who asks
/// keeps them as a friend.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Listed {
    #[serde(flatten)]
    pub player: Player,
    pub online: bool,
    pub busy: bool,
    /// While online: "launcher", "game" or "match" (see [`Activity`]).
    pub activity: Option<&'static str>,
    /// Their last heartbeat in Unix seconds; none before the first.
    pub last_online: Option<i64>,
    pub friend: bool,
    /// The two keep each other as friends: they can write each other
    /// (docs/profiles.md, "Chat").
    pub chat: bool,
    /// Their messages the player has not read.
    pub unread: i64,
    /// Their Discord account id, for a friend the player can chat with.
    pub discord_id: Option<String>,
}

/// Who has Recharged open, but `player_id`, by name.
pub async fn online(pool: &PgPool, player_id: i64) -> Result<Vec<Listed>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        "SELECT player_id FROM players
         WHERE heartbeat_at > now() - make_interval(secs => $2) AND player_id <> $1
         ORDER BY lower(display_name), player_id
         LIMIT 100",
        player_id,
        f64::from(super::matches::HEARTBEAT_TIMEOUT_SECS),
    )
    .fetch_all(pool)
    .await?;
    listed(pool, player_id, &ids).await
}

/// The last 10 players `player_id` played a set with, the latest first.
pub async fn recent_opponents(pool: &PgPool, player_id: i64) -> Result<Vec<Listed>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        r#"SELECT opponent AS "opponent!" FROM (
               SELECT CASE WHEN home = $1 THEN away ELSE home END AS opponent, updated_at
               FROM matches
               WHERE (home = $1 OR away = $1) AND state IN ('finished', 'disputed')
           ) AS played
           GROUP BY opponent
           ORDER BY max(updated_at) DESC
           LIMIT 10"#,
        player_id,
    )
    .fetch_all(pool)
    .await?;
    listed(pool, player_id, &ids).await
}

/// The players `ids`, in that order, as `viewer` sees them.
pub(super) async fn listed(
    pool: &PgPool,
    viewer: i64,
    ids: &[i64],
) -> Result<Vec<Listed>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT players.player_id, players.display_name,
                  discord.username AS "discord?", discord.avatar_url AS "avatar_url?",
                  test.username AS "test?",
                  coalesce(players.heartbeat_at > now() - make_interval(secs => $2), false)
                      AS "online!",
                  players.activity,
                  EXISTS (SELECT 1 FROM matches
                          WHERE state IN ('challenged', 'accepted')
                            AND (home = players.player_id OR away = players.player_id))
                      AS "busy!",
                  extract(epoch FROM players.heartbeat_at)::bigint AS last_online,
                  EXISTS (SELECT 1 FROM friends
                          WHERE friends.player_id = $3 AND friends.friend_id = players.player_id)
                      AS "friend!",
                  EXISTS (SELECT 1 FROM friends
                          WHERE friends.player_id = players.player_id AND friends.friend_id = $3)
                      AS "keeps_you!",
                  (SELECT count(*) FROM chat_messages
                   WHERE chat_messages.sender = players.player_id AND chat_messages.recipient = $3
                     AND chat_messages.read_at IS NULL) AS "unread!",
                  discord.external_id AS "discord_id?"
           FROM players
           LEFT JOIN player_identities discord
                  ON discord.player_id = players.player_id AND discord.provider = 'discord'
           LEFT JOIN player_identities test
                  ON test.player_id = players.player_id AND test.provider = 'test'
           WHERE players.player_id = ANY($1)"#,
        ids,
        f64::from(super::matches::HEARTBEAT_TIMEOUT_SECS),
        viewer,
    )
    .fetch_all(pool)
    .await?;
    let mut listed: Vec<Listed> = rows
        .into_iter()
        .map(|row| Listed {
            player: Player {
                id: row.player_id,
                name: row.display_name,
                avatar_url: row.avatar_url,
                discord: row.discord,
                test: row.test.is_some(),
            },
            online: row.online,
            busy: row.busy,
            activity: row
                .online
                .then(|| Activity::from_header(row.activity.as_deref()).name()),
            last_online: row.last_online,
            friend: row.friend,
            chat: row.friend && row.keeps_you,
            unread: row.unread,
            discord_id: (row.friend && row.keeps_you)
                .then_some(row.discord_id)
                .flatten(),
        })
        .collect();
    listed.sort_by_key(|entry| ids.iter().position(|id| *id == entry.player.id));
    Ok(listed)
}

/// The player who signs in with `name`: a Discord username, or on a test
/// server a test name.
pub async fn find(pool: &PgPool, name: &str) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT player_id FROM player_identities
         WHERE lower(username) = lower($1)
         ORDER BY provider = 'discord' DESC
         LIMIT 1",
        name.trim(),
    )
    .fetch_optional(pool)
    .await
}

/// A WFC profile of a player's (docs/profiles.md, "Friend codes").
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WfcProfile {
    pub profile_id: i32,
    /// The release: `R4QE` or `R4QP`, from the profile's `gsbrcd`.
    pub game: String,
    pub friend_code: String,
    /// The Mii name of its last login.
    pub mii: Option<String>,
}

/// Makes `profile_id` `player_id`'s, whoever's it was.
pub async fn bind_wfc(pool: &PgPool, player_id: i64, profile_id: i32) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO player_wfc_profiles (profile_id, player_id) VALUES ($1, $2)
         ON CONFLICT (profile_id) DO UPDATE SET player_id = $2, bound_at = now()",
        profile_id,
        player_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// `player_id`'s WFC profiles, the latest bound first.
pub async fn wfc_profiles(pool: &PgPool, player_id: i64) -> Result<Vec<WfcProfile>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT profiles.profile_id, profiles.gsbrcd, profiles.ingamesn
         FROM player_wfc_profiles
         JOIN profiles USING (profile_id)
         WHERE player_wfc_profiles.player_id = $1
         ORDER BY player_wfc_profiles.bound_at DESC",
        player_id,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let game: String = row.gsbrcd.chars().take(4).collect();
            WfcProfile {
                friend_code: crate::accounts::friend_code(row.profile_id, &game),
                profile_id: row.profile_id,
                game,
                mii: row.ingamesn,
            }
        })
        .collect())
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
    async fn players_are_found_by_their_username(pool: PgPool) {
        let nelly = sign_in(&pool, &discord("1", "nelly", "Nelly"))
            .await
            .unwrap();
        assert_eq!(find(&pool, " Nelly ").await.unwrap(), Some(nelly));
        assert_eq!(find(&pool, "nobody").await.unwrap(), None);
    }

    #[sqlx::test]
    async fn a_wfc_profile_is_its_last_players(pool: PgPool) {
        crate::accounts::log_in(&pool, 42).await.unwrap();
        let profile =
            crate::accounts::profile_login(&pool, 42, "R4QEabcdefg", Some("Nelly"), "10.0.0.1")
                .await
                .unwrap();
        let nelly = sign_in(&pool, &discord("1", "nelly", "Nelly"))
            .await
            .unwrap();
        let sam = sign_in(&pool, &discord("2", "sam", "Sam")).await.unwrap();

        bind_wfc(&pool, nelly, profile.profile_id).await.unwrap();
        let bound = wfc_profiles(&pool, nelly).await.unwrap();
        assert_eq!(
            bound,
            [WfcProfile {
                profile_id: profile.profile_id,
                game: "R4QE".into(),
                friend_code: "0868-9934-5920".into(),
                mii: Some("Nelly".into()),
            }]
        );
        bind_wfc(&pool, sam, profile.profile_id).await.unwrap();
        assert!(wfc_profiles(&pool, nelly).await.unwrap().is_empty());
        assert_eq!(wfc_profiles(&pool, sam).await.unwrap().len(), 1);
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

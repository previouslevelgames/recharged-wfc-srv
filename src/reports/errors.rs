//! Error codes Recharged builds report (docs/reports.md, "Errors"): the
//! number an error popup showed the player, and what the game was doing.
//! Most come before or without a GPCM login (a failed login, a matchmaking
//! that could not connect), so anyone may send one: an address sends at most
//! [`PER_WINDOW`] per [`WINDOW`], a profile is kept only when it is logged
//! in from that address, and the server keeps [`KEPT_DAYS`] days of them.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;

use super::report::RELEASES;

/// An address's errors at most per [`WINDOW`]: a game shows a few.
const PER_WINDOW: u32 = 30;
const WINDOW: Duration = Duration::from_secs(10 * 60);
/// Days of errors kept.
const KEPT_DAYS: i32 = 30;
/// The game's session states (`NetworkSession::mSessionState`).
pub const PHASES: [&str; 7] = [
    "none",
    "login",
    "matchmaking",
    "prestart",
    "loading",
    "in_game",
    "game_end",
];
/// The codes popups show.
const MAX_CODE: i64 = 999_999;
/// A build's name at most, in characters, as in the reports.
const MAX_BUILD: usize = 64;

/// An error, checked.
#[derive(Debug, PartialEq)]
pub struct ClientError {
    pub release: &'static str,
    pub code: i32,
    /// One of [`PHASES`], when the game said.
    pub phase: Option<&'static str>,
    pub build: Option<String>,
    /// The profile the game named, if any: not checked yet.
    pub profile_id: Option<i32>,
}

#[derive(Deserialize)]
struct RawError {
    release: String,
    code: i64,
    #[serde(default)]
    phase: Option<Value>,
    #[serde(default)]
    build: Option<Value>,
    #[serde(default)]
    profile_id: Option<Value>,
}

impl ClientError {
    /// The error in `body`, or what is wrong with it. The code and the
    /// release are needed; the rest is dropped when the server cannot read
    /// it.
    pub fn parse(body: &[u8]) -> Result<Self, String> {
        let raw: RawError = serde_json::from_slice(body).map_err(|error| error.to_string())?;
        let release = RELEASES
            .into_iter()
            .find(|release| *release == raw.release)
            .ok_or_else(|| format!("release: one of {}", RELEASES.join(", ")))?;
        let code = (1..=MAX_CODE)
            .contains(&raw.code)
            .then_some(raw.code as i32)
            .ok_or_else(|| format!("code: 1 to {MAX_CODE}"))?;
        let phase = raw
            .phase
            .as_ref()
            .and_then(Value::as_str)
            .and_then(|phase| PHASES.into_iter().find(|known| *known == phase));
        let build = raw
            .build
            .as_ref()
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|build| {
                !build.is_empty()
                    && build.chars().count() <= MAX_BUILD
                    && !build.chars().any(char::is_control)
            })
            .map(str::to_owned);
        let profile_id = raw
            .profile_id
            .as_ref()
            .and_then(Value::as_i64)
            .and_then(|id| i32::try_from(id).ok())
            .filter(|&id| id > 0);
        Ok(Self {
            release,
            code,
            phase,
            build,
            profile_id,
        })
    }
}

/// How many errors each address sent lately. A plain `std` mutex: it is
/// never held across an `.await`.
#[derive(Default)]
pub struct Limits {
    windows: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl Limits {
    /// Whether `address` may send one more now.
    pub fn allow(&self, address: IpAddr) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock().unwrap();
        windows.retain(|_, (since, _)| now.duration_since(*since) < WINDOW);
        let (_, sent) = windows.entry(address).or_insert((now, 0));
        *sent += 1;
        *sent <= PER_WINDOW
    }
}

/// Keeps `error` from `address`, `profile_id` only once checked, and lets
/// the oldest go.
pub async fn store(
    pool: &PgPool,
    error: &ClientError,
    profile_id: Option<i32>,
    address: &str,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "DELETE FROM client_errors WHERE received_at < now() - make_interval(days => $1)",
        KEPT_DAYS,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO client_errors (code, phase, release, build, profile_id, address)
         VALUES ($1, $2, $3, $4, $5, $6)",
        error.code,
        error.phase,
        error.release,
        error.build,
        profile_id,
        address,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(body: Value) -> Result<ClientError, String> {
        ClientError::parse(body.to_string().as_bytes())
    }

    #[test]
    fn an_error_needs_a_release_and_a_code_only() {
        let error = parse(json!({
            "release": "R4QE", "build": "1.1.0+g0123456789ab", "code": 86420,
            "phase": "matchmaking", "profile_id": 1_000_000_000,
        }))
        .unwrap();
        assert_eq!(
            error,
            ClientError {
                release: "R4QE",
                code: 86420,
                phase: Some("matchmaking"),
                build: Some("1.1.0+g0123456789ab".into()),
                profile_id: Some(1_000_000_000),
            }
        );

        let bare = parse(
            json!({"release": "R4QP", "code": 20110, "phase": "elsewhere", "profile_id": -1}),
        )
        .unwrap();
        assert_eq!(
            (bare.phase, bare.build, bare.profile_id),
            (None, None, None)
        );

        for body in [
            json!({"release": "R4QX", "code": 86420}),
            json!({"release": "R4QE", "code": 0}),
            json!({"release": "R4QE", "code": 1_000_000}),
            json!({"release": "R4QE"}),
        ] {
            assert!(parse(body.clone()).is_err(), "{body}");
        }
    }

    #[test]
    fn an_address_sends_a_few_per_window() {
        let limits = Limits::default();
        let (game, other) = ([10, 0, 0, 1].into(), [10, 0, 0, 2].into());
        assert!((0..PER_WINDOW).all(|_| limits.allow(game)));
        assert!(!limits.allow(game));
        assert!(limits.allow(other));
    }

    #[sqlx::test]
    async fn errors_older_than_the_kept_days_go(pool: PgPool) {
        let error = parse(json!({"release": "R4QE", "code": 86420})).unwrap();
        store(&pool, &error, None, "10.0.0.1").await.unwrap();
        sqlx::query!("UPDATE client_errors SET received_at = now() - interval '31 days'")
            .execute(&pool)
            .await
            .unwrap();
        store(&pool, &error, None, "10.0.0.1").await.unwrap();
        let kept = sqlx::query_scalar!(r#"SELECT count(*) AS "kept!" FROM client_errors"#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kept, 1);
    }
}

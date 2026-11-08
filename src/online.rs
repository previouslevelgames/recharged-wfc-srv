//! How many players are online, once a minute, for the Monitoring page's
//! chart (docs/install.md, "Admin dashboard"): logged in to WFC (GPCM, the
//! CPU buddy left out; any game, Recharged or not), of them in an online
//! match, and with Recharged open (a launcher or game of a version with
//! profiles asked in the last minute). Samples older than [`KEPT_DAYS`] go.
//!
//! Each server knows only the players logged in to it, so servers that share
//! the database each write their own row per minute, by [`server`], and the
//! Monitoring page adds them up.
//!
//! `GET /api/v1/online` ([`router`]) tells anyone the numbers of now, for the
//! launcher and the game (docs/install.md, "Players online").

use std::{
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;
use sqlx::PgPool;
use tokio::time::MissedTickBehavior;
use tracing::warn;

use crate::gpcm::Gpcm;

/// The days samples are kept.
pub const KEPT_DAYS: i32 = 30;
/// A sample every minute.
const EVERY: Duration = Duration::from_secs(60);
/// Recharged is open while its launcher or game asks at least this often
/// (the profile service's heartbeat).
const RECHARGED_SECS: f64 = 60.0;
/// `GET /api/v1/online` counts again after this long at the most: however
/// often it is asked, the database sees one query in this time.
const ANSWER_FOR: Duration = Duration::from_secs(10);

/// Players online: logged in to WFC, in an online match, with Recharged open.
type Counts = (i32, i32, i32);

/// This server among those that share the database: its host name, which
/// Docker makes the container's (or that of the container whose network it
/// runs in).
pub fn server() -> &'static str {
    static SERVER: LazyLock<String> = LazyLock::new(|| {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|name| name.trim().to_owned())
            .unwrap_or_default()
    });
    &SERVER
}

/// Players online now: logged in to WFC, of them in an online match (DWC's
/// status 2), and with Recharged open. The first two count this server's
/// players only; the last, from the database, every server's.
pub async fn now(pool: &PgPool, gpcm: &Gpcm) -> Result<Counts, sqlx::Error> {
    let presence = gpcm.presence();
    let wfc = i32::try_from(presence.len()).unwrap_or(i32::MAX);
    let playing = i32::try_from(
        presence
            .values()
            .filter(|presence| presence.status == Some(2))
            .count(),
    )
    .unwrap_or(i32::MAX);
    let recharged = sqlx::query_scalar!(
        r#"SELECT count(*)::INTEGER AS "count!" FROM players
           WHERE heartbeat_at > now() - make_interval(secs => $1)"#,
        RECHARGED_SECS,
    )
    .fetch_one(pool)
    .await?;
    Ok((wfc, playing, recharged))
}

/// Writes this server's sample of this minute, and drops those older than
/// [`KEPT_DAYS`].
pub async fn sample(pool: &PgPool, gpcm: &Gpcm) -> Result<(), sqlx::Error> {
    let (wfc, playing, recharged) = now(pool, gpcm).await?;
    sqlx::query!(
        "INSERT INTO online_samples (at, server, wfc, playing, recharged)
         VALUES (date_trunc('minute', now()), $1, $2, $3, $4)
         ON CONFLICT (at, server) DO UPDATE
         SET wfc = EXCLUDED.wfc, playing = EXCLUDED.playing, recharged = EXCLUDED.recharged",
        server(),
        wfc,
        playing,
        recharged,
    )
    .execute(pool)
    .await?;
    sqlx::query!(
        "DELETE FROM online_samples WHERE at < now() - make_interval(days => $1)",
        KEPT_DAYS,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Samples once a minute, for as long as the server runs; a sample that
/// fails is skipped.
pub async fn run(pool: PgPool, gpcm: Arc<Gpcm>) -> std::io::Result<()> {
    let mut every = tokio::time::interval(EVERY);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        if let Err(error) = sample(&pool, &gpcm).await {
            warn!(target: "online", %error, "players online not sampled");
        }
    }
}

#[derive(Clone)]
struct OnlineState {
    pool: PgPool,
    gpcm: Arc<Gpcm>,
    /// The last count and when it was made.
    last: Arc<Mutex<Option<(Instant, Counts)>>>,
}

/// `GET /api/v1/online`, for any host name and without a session: the
/// players online now, as [`now`] counts them.
pub fn router(pool: PgPool, gpcm: Arc<Gpcm>) -> Router {
    Router::new()
        .route("/api/v1/online", get(online))
        .with_state(OnlineState {
            pool,
            gpcm,
            last: Arc::default(),
        })
}

async fn online(State(state): State<OnlineState>) -> Response {
    let kept = *state.last.lock().unwrap();
    let counts = match kept {
        Some((at, counts)) if at.elapsed() < ANSWER_FOR => counts,
        _ => match now(&state.pool, &state.gpcm).await {
            Ok(counts) => {
                *state.last.lock().unwrap() = Some((Instant::now(), counts));
                counts
            }
            Err(error) => {
                warn!(target: "online", %error, "players online not counted");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "the server's database failed" })),
                )
                    .into_response();
            }
        },
    };
    let (wfc, playing, recharged) = counts;
    (
        [(header::CACHE_CONTROL, "public, max-age=10")],
        Json(json!({
            "wfc": wfc,
            "playing": playing,
            "recharged": recharged,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use axum::{body::Body, http::Request};
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;
    use crate::nas::Tickets;

    async fn get_online(app: Router) -> (StatusCode, Option<String>, Value) {
        let request = Request::get("/api/v1/online")
            .header(header::HOST, "2.31.48.136")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let cache = response
            .headers()
            .get(header::CACHE_CONTROL)
            .map(|value| value.to_str().unwrap().to_owned());
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .unwrap();
        (status, cache, serde_json::from_slice(&bytes).unwrap())
    }

    #[sqlx::test]
    async fn anyone_sees_how_many_players_are_online(pool: PgPool) {
        let gpcm = Arc::new(Gpcm::new(
            pool.clone(),
            Arc::new(Tickets::default()),
            Arc::default(),
        ));
        // Two logged in to WFC, one of them in an online match.
        gpcm.fake_login(1_000_000_001, IpAddr::from([203, 0, 113, 4]));
        gpcm.fake_login(1_000_000_002, IpAddr::from([198, 51, 100, 7]));
        gpcm.fake_status(1_000_000_002, 2);
        let app = router(pool.clone(), gpcm.clone());
        let (status, cache, body) = get_online(app.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cache.as_deref(), Some("public, max-age=10"));
        assert_eq!(body, json!({ "wfc": 2, "playing": 1, "recharged": 0 }));
        // Asked again at once, the server answers the count it made.
        gpcm.fake_login(1_000_000_003, IpAddr::from([192, 0, 2, 9]));
        let (_, _, again) = get_online(app).await;
        assert_eq!(again["wfc"], 2);
        // A new count sees the third.
        let (_, _, fresh) = get_online(router(pool, gpcm)).await;
        assert_eq!(fresh["wfc"], 3);
    }

    #[sqlx::test]
    async fn each_server_keeps_one_sample_a_minute(pool: PgPool) {
        // Another server's sample of this minute stays beside this one's.
        sqlx::query(
            "INSERT INTO online_samples (at, server, wfc, playing, recharged)
             VALUES (date_trunc('minute', now()), 'other', 4, 1, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let gpcm = Gpcm::new(pool.clone(), Arc::new(Tickets::default()), Arc::default());
        sample(&pool, &gpcm).await.unwrap();
        sample(&pool, &gpcm).await.unwrap();
        let rows: Vec<(String, i32)> = sqlx::query_as(
            "SELECT server, wfc FROM online_samples
             WHERE at = date_trunc('minute', now()) ORDER BY server = 'other'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(!server().is_empty());
        assert_eq!(rows, [(server().to_owned(), 0), ("other".to_owned(), 4)]);
    }
}

//! How many players are online, once a minute, for the Monitoring page's
//! chart (docs/install.md, "Admin dashboard"): logged in to WFC (GPCM, the
//! CPU buddy left out; any game, Recharged or not), of them in an online
//! match, and with Recharged open (a launcher or game of a version with
//! profiles asked in the last minute). Samples older than [`KEPT_DAYS`] go.

use std::{sync::Arc, time::Duration};

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

/// Players online now: logged in to WFC, of them in an online match (DWC's
/// status 2), and with Recharged open.
pub async fn now(pool: &PgPool, gpcm: &Gpcm) -> Result<(i32, i32, i32), sqlx::Error> {
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

/// Writes this minute's sample, and drops those older than [`KEPT_DAYS`].
pub async fn sample(pool: &PgPool, gpcm: &Gpcm) -> Result<(), sqlx::Error> {
    let (wfc, playing, recharged) = now(pool, gpcm).await?;
    sqlx::query!(
        "INSERT INTO online_samples (at, wfc, playing, recharged)
         VALUES (date_trunc('minute', now()), $1, $2, $3)
         ON CONFLICT (at) DO UPDATE
         SET wfc = EXCLUDED.wfc, playing = EXCLUDED.playing, recharged = EXCLUDED.recharged",
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

//! How many players are online, once a minute, for the Monitoring page's
//! chart (docs/install.md, "Admin dashboard"): logged in to WFC (GPCM, the
//! CPU buddy left out; any game, Recharged or not), of them in an online
//! match, and with Recharged open (a launcher or game of a version with
//! profiles asked in the last minute). Samples older than [`KEPT_DAYS`] go.
//!
//! Each server knows only the players logged in to it, so servers that share
//! the database each write their own row per minute, by [`server`], and the
//! Monitoring page adds them up.

use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nas::Tickets;

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

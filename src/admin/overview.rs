//! The Overview page: each part of the server in a few figures.

use serde::Serialize;
use sqlx::PgPool;

use super::{database, leaderboards, matches, matchmaking, page::snapshot, players};
use crate::{gpcm::Gpcm, master::Master, reports};

#[derive(Serialize)]
pub struct Overview {
    database: database::Summary,
    players: players::Totals,
    matchmaking: matchmaking::Totals,
    matches: matches::Totals,
    leaderboards: leaderboards::Totals,
}

pub async fn read(pool: &PgPool, gpcm: &Gpcm, master: &Master) -> Result<Overview, sqlx::Error> {
    let presence = gpcm.presence();
    reports::settle_idle(pool).await?;
    let mut tx = snapshot(pool).await?;
    let overview = Overview {
        database: database::summary(&mut tx).await?,
        players: players::totals(&mut tx, &presence).await?,
        matchmaking: matchmaking::totals(master),
        matches: matches::totals(&mut tx).await?,
        leaderboards: leaderboards::totals(&mut tx).await?,
    };
    tx.commit().await?;
    Ok(overview)
}

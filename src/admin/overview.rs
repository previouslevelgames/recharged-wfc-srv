//! The Overview page: each part of the server in a few figures.

use serde::Serialize;
use sqlx::PgPool;

use super::{database, leaderboards, page::snapshot, players};
use crate::gpcm::Gpcm;

#[derive(Serialize)]
pub struct Overview {
    database: database::Summary,
    players: players::Totals,
    leaderboards: leaderboards::Totals,
}

pub async fn read(pool: &PgPool, gpcm: &Gpcm) -> Result<Overview, sqlx::Error> {
    let presence = gpcm.presence();
    let mut tx = snapshot(pool).await?;
    let overview = Overview {
        database: database::summary(&mut tx).await?,
        players: players::totals(&mut tx, &presence).await?,
        leaderboards: leaderboards::totals(&mut tx).await?,
    };
    tx.commit().await?;
    Ok(overview)
}

//! The Overview page: each part of the server in a few figures.

use serde::Serialize;
use sqlx::PgPool;

use super::{database, leaderboards, page::snapshot};

#[derive(Serialize)]
pub struct Overview {
    database: database::Summary,
    leaderboards: leaderboards::Totals,
}

pub async fn read(pool: &PgPool) -> Result<Overview, sqlx::Error> {
    let mut tx = snapshot(pool).await?;
    let overview = Overview {
        database: database::summary(&mut tx).await?,
        leaderboards: leaderboards::totals(&mut tx).await?,
    };
    tx.commit().await?;
    Ok(overview)
}

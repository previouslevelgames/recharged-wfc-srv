//! What every dashboard page's API answers: the page's content, with who is
//! looking and the server they look at.

use std::{future::Future, time::Duration};

use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use tracing::warn;

use super::google::Account;

/// A database that answers slower than this counts as down.
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize)]
pub struct PageData<T> {
    viewer: Account,
    /// The server's clock, for the page's "ago"s.
    #[serde(with = "time::serde::rfc3339")]
    now: OffsetDateTime,
    server: Server,
    /// What the page shows. `None` when the database failed to answer;
    /// `error` says why.
    content: Option<T>,
    error: Option<String>,
}

#[derive(Serialize)]
struct Server {
    version: &'static str,
    revision: &'static str,
    #[serde(with = "time::serde::rfc3339")]
    started_at: OffsetDateTime,
    /// The connection pool, shared with the game's endpoints.
    pool: Pool,
}

#[derive(Serialize)]
struct Pool {
    size: u32,
    idle: usize,
    max: u32,
}

impl<T> PageData<T> {
    /// Reads the page's content with `read`, or why the database could not
    /// tell.
    pub async fn read(
        pool: &PgPool,
        viewer: Account,
        started_at: OffsetDateTime,
        read: impl Future<Output = Result<T, sqlx::Error>>,
    ) -> Self {
        let (content, error) = match tokio::time::timeout(TIMEOUT, read).await {
            Ok(Ok(content)) => (Some(content), None),
            Ok(Err(error)) => {
                warn!(target: "admin", %error, "database read failed");
                (None, Some(error.to_string()))
            }
            Err(_) => (
                None,
                Some(format!("no answer within {} seconds", TIMEOUT.as_secs())),
            ),
        };
        Self {
            viewer,
            now: OffsetDateTime::now_utc(),
            server: Server {
                version: crate::VERSION,
                revision: crate::REVISION,
                started_at,
                pool: Pool {
                    size: pool.size(),
                    idle: pool.num_idle(),
                    max: pool.options().get_max_connections(),
                },
            },
            content,
            error,
        }
    }
}

/// A read-only transaction for a page's queries: one consistent snapshot,
/// and one commit in PostgreSQL's counters per poll rather than one per
/// query.
pub async fn snapshot(pool: &PgPool) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query!("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unreachable_database_is_reported() {
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

        // Nothing listens on port 1.
        let pool = PgPoolOptions::new()
            .acquire_timeout(Duration::from_secs(1))
            .connect_lazy_with(PgConnectOptions::new().host("127.0.0.1").port(1));
        let viewer = Account {
            email: "admin@example.com".into(),
            name: None,
            picture: None,
        };
        let page = PageData::read(&pool, viewer, OffsetDateTime::now_utc(), async {
            snapshot(&pool).await.map(|_| ())
        })
        .await;
        assert!(page.content.is_none());
        assert!(page.error.is_some());
    }
}

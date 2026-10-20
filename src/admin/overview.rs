//! What the dashboard shows: this server, its PostgreSQL, and the data in it.
//!
//! The dashboard polls every few seconds, so these are reads of PostgreSQL's
//! statistics views and of the small `scores` table, in one read-only
//! transaction.

use std::time::Duration;

use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use tracing::warn;

use super::google::Account;

/// A database that answers slower than this counts as down.
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize)]
pub struct Overview {
    /// Who is looking.
    viewer: Account,
    /// The server's clock, for the dashboard's "ago"s.
    #[serde(with = "time::serde::rfc3339")]
    now: OffsetDateTime,
    server: Server,
    /// `None` when the database failed to answer; `database_error` says why.
    database: Option<Database>,
    database_error: Option<String>,
}

#[derive(Serialize)]
struct Server {
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

#[derive(Serialize)]
struct Database {
    #[serde(flatten)]
    summary: Summary,
    tables: Vec<Table>,
    connections: Vec<Connection>,
    migrations: Vec<Migration>,
    leaderboards: Vec<Leaderboard>,
}

/// The server and this database's counters since the last statistics reset.
#[derive(Serialize)]
struct Summary {
    name: String,
    version: String,
    #[serde(with = "time::serde::rfc3339")]
    started_at: OffsetDateTime,
    size_bytes: i64,
    max_connections: i32,
    /// Client connections to the whole server, this dashboard's included.
    open_connections: i64,
    /// Of those, the ones running a query, not counting this dashboard.
    active_connections: i64,
    commits: i64,
    rollbacks: i64,
    blocks_hit: i64,
    blocks_read: i64,
    deadlocks: i64,
}

/// A table in this database (`pg_stat_user_tables`).
#[derive(Serialize)]
struct Table {
    /// Qualified outside the `public` schema.
    name: String,
    /// An estimate, as of the last write or `ANALYZE`.
    rows: i64,
    dead_rows: i64,
    /// Table, indexes and TOAST.
    total_bytes: i64,
    index_bytes: i64,
    seq_scans: i64,
    index_scans: Option<i64>,
    #[serde(with = "time::serde::rfc3339::option")]
    last_vacuum: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    last_analyze: Option<OffsetDateTime>,
}

/// A client connection to the server (`pg_stat_activity`), other than this
/// dashboard's.
#[derive(Serialize)]
struct Connection {
    pid: i32,
    user: Option<String>,
    database: Option<String>,
    application: String,
    client: Option<String>,
    state: Option<String>,
    /// What it waits for, e.g. `Client: ClientRead`.
    wait: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    connected_at: Option<OffsetDateTime>,
    /// When `state` began.
    #[serde(with = "time::serde::rfc3339::option")]
    state_since: Option<OffsetDateTime>,
    /// The current or last query, cut to 500 characters.
    query: Option<String>,
}

/// An applied migration (`_sqlx_migrations`).
#[derive(Serialize)]
struct Migration {
    version: i64,
    description: String,
    #[serde(with = "time::serde::rfc3339")]
    installed_on: OffsetDateTime,
    success: bool,
    execution_ms: i64,
}

/// The `scores` of one category.
#[derive(Serialize)]
struct Leaderboard {
    category: i32,
    players: i64,
    jp: i64,
    us: i64,
    eu: i64,
    kr: i64,
    top_score: i32,
    #[serde(with = "time::serde::rfc3339")]
    last_put: OffsetDateTime,
}

/// Reads everything, or why the database could not tell.
pub async fn collect(pool: &PgPool, viewer: Account, started_at: OffsetDateTime) -> Overview {
    let (database, database_error) = match tokio::time::timeout(TIMEOUT, read(pool)).await {
        Ok(Ok(database)) => (Some(database), None),
        Ok(Err(error)) => {
            warn!(target: "admin", %error, "database overview failed");
            (None, Some(error.to_string()))
        }
        Err(_) => (
            None,
            Some(format!("no answer within {} seconds", TIMEOUT.as_secs())),
        ),
    };
    Overview {
        viewer,
        now: OffsetDateTime::now_utc(),
        server: Server {
            revision: crate::REVISION,
            started_at,
            pool: Pool {
                size: pool.size(),
                idle: pool.num_idle(),
                max: pool.options().get_max_connections(),
            },
        },
        database,
        database_error,
    }
}

async fn read(pool: &PgPool) -> Result<Database, sqlx::Error> {
    // One transaction: one consistent snapshot, and one commit in the
    // counters per poll rather than one per query.
    let mut tx = pool.begin().await?;
    sqlx::query!("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await?;
    let database = Database {
        summary: summary(&mut tx).await?,
        tables: tables(&mut tx).await?,
        connections: connections(&mut tx).await?,
        migrations: migrations(&mut tx).await?,
        leaderboards: leaderboards(&mut tx).await?,
    };
    tx.commit().await?;
    Ok(database)
}

async fn summary(conn: &mut PgConnection) -> Result<Summary, sqlx::Error> {
    sqlx::query_as!(
        Summary,
        r#"SELECT current_database() AS "name!",
                  current_setting('server_version') AS "version!",
                  pg_postmaster_start_time() AS "started_at!",
                  pg_database_size(current_database()) AS "size_bytes!",
                  current_setting('max_connections')::integer AS "max_connections!",
                  (SELECT count(*) FROM pg_stat_activity
                   WHERE backend_type = 'client backend') AS "open_connections!",
                  (SELECT count(*) FROM pg_stat_activity
                   WHERE backend_type = 'client backend' AND state = 'active'
                     AND pid <> pg_backend_pid()) AS "active_connections!",
                  xact_commit AS "commits!",
                  xact_rollback AS "rollbacks!",
                  blks_hit AS "blocks_hit!",
                  blks_read AS "blocks_read!",
                  deadlocks AS "deadlocks!"
           FROM pg_stat_database
           WHERE datname = current_database()"#
    )
    .fetch_one(conn)
    .await
}

async fn tables(conn: &mut PgConnection) -> Result<Vec<Table>, sqlx::Error> {
    sqlx::query_as!(
        Table,
        r#"SELECT CASE WHEN schemaname = 'public' THEN relname::text
                       ELSE schemaname || '.' || relname END AS "name!",
                  n_live_tup AS "rows!",
                  n_dead_tup AS "dead_rows!",
                  pg_total_relation_size(relid) AS "total_bytes!",
                  pg_indexes_size(relid) AS "index_bytes!",
                  seq_scan AS "seq_scans!",
                  idx_scan AS index_scans,
                  greatest(last_vacuum, last_autovacuum) AS last_vacuum,
                  greatest(last_analyze, last_autoanalyze) AS last_analyze
           FROM pg_stat_user_tables
           ORDER BY pg_total_relation_size(relid) DESC, relname"#
    )
    .fetch_all(conn)
    .await
}

async fn connections(conn: &mut PgConnection) -> Result<Vec<Connection>, sqlx::Error> {
    sqlx::query_as!(
        Connection,
        r#"SELECT pid AS "pid!",
                  usename::text AS "user",
                  datname::text AS "database",
                  application_name AS "application!",
                  host(client_addr) AS client,
                  state,
                  wait_event_type || ': ' || wait_event AS wait,
                  backend_start AS connected_at,
                  state_change AS state_since,
                  left(query, 500) AS query
           FROM pg_stat_activity
           WHERE backend_type = 'client backend' AND pid <> pg_backend_pid()
           ORDER BY state = 'active' DESC NULLS LAST, backend_start"#
    )
    .fetch_all(conn)
    .await
}

async fn migrations(conn: &mut PgConnection) -> Result<Vec<Migration>, sqlx::Error> {
    sqlx::query_as!(
        Migration,
        r#"SELECT version,
                  description,
                  installed_on,
                  success,
                  execution_time / 1000000 AS "execution_ms!"
           FROM _sqlx_migrations
           ORDER BY version"#
    )
    .fetch_all(conn)
    .await
}

async fn leaderboards(conn: &mut PgConnection) -> Result<Vec<Leaderboard>, sqlx::Error> {
    sqlx::query_as!(
        Leaderboard,
        r#"SELECT category,
                  count(*) AS "players!",
                  count(*) FILTER (WHERE region = 1) AS "jp!",
                  count(*) FILTER (WHERE region = 2) AS "us!",
                  count(*) FILTER (WHERE region = 4) AS "eu!",
                  count(*) FILTER (WHERE region = 8) AS "kr!",
                  max(score) AS "top_score!",
                  max(updated_at) AS "last_put!"
           FROM scores
           GROUP BY category
           ORDER BY category"#
    )
    .fetch_all(conn)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test]
    async fn reads_the_server_and_its_data(pool: PgPool) {
        sqlx::query!(
            "INSERT INTO scores (profile_id, category, region, score, userdata)
             VALUES (1, 0, 4, 10, ''), (2, 0, 2, 30, ''), (2, 1, 2, 5, '')"
        )
        .execute(&pool)
        .await
        .unwrap();

        let database = read(&pool).await.unwrap();

        assert!(database.summary.version.starts_with(char::is_numeric));
        assert!(database.summary.size_bytes > 0);
        assert!(database.summary.open_connections >= 1);
        assert!(database.tables.iter().any(|table| table.name == "scores"));
        assert_eq!(
            database.migrations.len(),
            sqlx::migrate!()
                .iter()
                .filter(|migration| migration.migration_type.is_up_migration())
                .count(),
            "every migration is listed"
        );
        assert!(
            database
                .migrations
                .iter()
                .all(|migration| migration.success)
        );

        let boards: Vec<_> = database
            .leaderboards
            .iter()
            .map(|board| {
                (
                    board.category,
                    board.players,
                    board.us,
                    board.eu,
                    board.top_score,
                )
            })
            .collect();
        assert_eq!(boards, [(0, 2, 1, 1, 30), (1, 1, 1, 0, 5)]);
    }

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
        let overview = collect(&pool, viewer, OffsetDateTime::now_utc()).await;
        assert!(overview.database.is_none());
        assert!(overview.database_error.is_some());
    }
}

//! This server as a node of the pool (docs/nodes.md): its name, its lease in
//! `nodes`, and the keys every node shares.
//!
//! A node draws a new lease each time it starts: a row of its own, whose id is
//! its incarnation. It renews it every [`HEARTBEAT`] with what it carries, and
//! removes it when it stops. A lease not renewed for [`DEAD_AFTER`] is a dead
//! node's, and the node that renews next removes it. A node whose own lease
//! was taken for dead draws a new one.

use std::{
    sync::atomic::{AtomicI64, Ordering},
    time::Duration,
};

use sqlx::PgPool;
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

/// The variable that names a node.
pub const VARIABLE: &str = "NODE_NAME";
/// A name's longest length, in characters.
const MAX_NAME: usize = 32;
/// How often a node renews its lease.
pub const HEARTBEAT: Duration = Duration::from_secs(5);
/// How long a lease lasts without being renewed.
pub const DEAD_AFTER: Duration = Duration::from_secs(15);
/// How long a stopping node tries to remove its lease. The database may be
/// stopping too (the bundled one stops with the server), and the lease runs
/// out by itself.
pub const LEAVE_WITHIN: Duration = Duration::from_secs(2);

/// The key NAS signs its login tokens with, which GPCM checks: any node's
/// token logs in to any node.
pub const NAS_TICKETS: &str = "nas-tickets";

/// Reads [`VARIABLE`] with `var`: the node's name, else `host_name`. Docker
/// makes the host name the container's, or that of the container whose
/// network it runs in.
pub fn name(
    var: impl Fn(&str) -> Option<String>,
    host_name: impl FnOnce() -> String,
) -> Result<String, String> {
    let Some(name) = var(VARIABLE)
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
    else {
        return Ok(host_name());
    };
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    if name.chars().count() > MAX_NAME || !name.chars().all(allowed) {
        return Err(format!(
            "{VARIABLE}: at most {MAX_NAME} letters, digits, - _ or ."
        ));
    }
    Ok(name)
}

/// The machine's host name, `""` if it can't be read.
pub fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .unwrap_or_default()
}

/// What a node carries, which its lease tells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Load {
    /// Logged in to GPCM.
    pub players: i32,
    /// QR2 sessions heard from lately.
    pub games: i32,
}

/// This node: its name, and its lease once it has one.
pub struct Node {
    name: String,
    /// Its lease's id; 0 without one.
    id: AtomicI64,
}

/// Named by the host, without a lease: a server on its own.
impl Default for Node {
    fn default() -> Self {
        Self::new(host_name())
    }
}

impl Node {
    pub fn new(name: String) -> Self {
        Self {
            name,
            id: AtomicI64::new(0),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its lease's id, `None` without one.
    pub fn lease(&self) -> Option<i64> {
        Some(self.id.load(Ordering::Relaxed)).filter(|&id| id != 0)
    }

    /// Draws a new lease.
    pub async fn register(&self, pool: &PgPool, load: Load) -> Result<i64, sqlx::Error> {
        let id = sqlx::query_scalar!(
            "INSERT INTO nodes (name, revision, version, players, games)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
            self.name,
            crate::REVISION,
            crate::VERSION,
            load.players,
            load.games,
        )
        .fetch_one(pool)
        .await?;
        self.id.store(id, Ordering::Relaxed);
        Ok(id)
    }

    /// Renews the lease with `load`, or draws a new one if it has none or the
    /// others took it for dead, and removes the leases of dead nodes.
    pub async fn renew(&self, pool: &PgPool, load: Load) -> Result<(), sqlx::Error> {
        let renewed = match self.lease() {
            Some(id) => {
                sqlx::query!(
                    "UPDATE nodes SET heartbeat_at = now(), players = $2, games = $3
                     WHERE id = $1",
                    id,
                    load.players,
                    load.games,
                )
                .execute(pool)
                .await?
                .rows_affected()
                    == 1
            }
            None => false,
        };
        if !renewed {
            let id = self.register(pool, load).await?;
            warn!(target: "node", node = %self.name, id, "lease lost: drew a new one");
        }
        sqlx::query!(
            "DELETE FROM nodes WHERE heartbeat_at < now() - make_interval(secs => $1)",
            DEAD_AFTER.as_secs_f64(),
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Removes the lease, as the node stops.
    pub async fn leave(&self, pool: &PgPool) -> Result<(), sqlx::Error> {
        if let Some(id) = self.lease() {
            sqlx::query!("DELETE FROM nodes WHERE id = $1", id)
                .execute(pool)
                .await?;
            self.id.store(0, Ordering::Relaxed);
            info!(target: "node", node = %self.name, id, "lease removed");
        }
        Ok(())
    }

    /// Renews the lease every [`HEARTBEAT`] with what `load` says, for as long
    /// as the server runs; a renewal that fails is tried at the next.
    pub async fn keep(&self, pool: &PgPool, load: impl Fn() -> Load) -> std::io::Result<()> {
        let mut every = tokio::time::interval(HEARTBEAT);
        every.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            if let Err(error) = self.renew(pool, load()).await {
                warn!(target: "node", %error, "lease not renewed");
            }
        }
    }
}

/// The pool's key `name`: made by the first node that asks for it, and the
/// same for every node after.
pub async fn shared_key(pool: &PgPool, name: &str) -> Result<[u8; 32], sqlx::Error> {
    let made: [u8; 32] = rand::random();
    sqlx::query!(
        "INSERT INTO shared_keys (name, key) VALUES ($1, $2) ON CONFLICT (name) DO NOTHING",
        name,
        &made[..],
    )
    .execute(pool)
    .await?;
    let key = sqlx::query_scalar!("SELECT key FROM shared_keys WHERE name = $1", name)
        .fetch_one(pool)
        .await?;
    key.try_into()
        .map_err(|_| sqlx::Error::Decode(format!("shared key {name}: not 32 bytes").into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_from(value: Option<&str>) -> Result<String, String> {
        name(
            |variable| {
                assert_eq!(variable, VARIABLE);
                value.map(str::to_owned)
            },
            || "0a90c4f2e1b3".to_owned(),
        )
    }

    #[test]
    fn a_node_is_named_by_its_variable_else_its_host() {
        assert_eq!(name_from(Some(" srv-1 ")).unwrap(), "srv-1");
        assert_eq!(name_from(Some("node_2.nebula")).unwrap(), "node_2.nebula");
        assert_eq!(name_from(None).unwrap(), "0a90c4f2e1b3");
        assert_eq!(name_from(Some("  ")).unwrap(), "0a90c4f2e1b3");
        assert!(name_from(Some("node 2")).is_err());
        assert!(name_from(Some("nöde")).is_err());
        assert!(name_from(Some(&"n".repeat(MAX_NAME + 1))).is_err());
    }

    async fn leases(pool: &PgPool) -> Vec<(i64, String, i32, i32)> {
        sqlx::query_as("SELECT id, name, players, games FROM nodes ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[sqlx::test]
    async fn a_lease_is_renewed_with_the_load_and_removed_at_the_end(pool: PgPool) {
        let node = Node::new("srv-1".to_owned());
        assert_eq!(node.lease(), None);
        let id = node.register(&pool, Load::default()).await.unwrap();
        assert_eq!(node.lease(), Some(id));

        let load = Load {
            players: 12,
            games: 3,
        };
        node.renew(&pool, load).await.unwrap();
        assert_eq!(leases(&pool).await, [(id, "srv-1".to_owned(), 12, 3)]);
        let revision: String = sqlx::query_scalar("SELECT revision FROM nodes")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(revision, crate::REVISION);

        node.leave(&pool).await.unwrap();
        assert_eq!(node.lease(), None);
        assert!(leases(&pool).await.is_empty());
    }

    #[sqlx::test]
    async fn a_dead_lease_goes_and_a_lost_one_is_drawn_again(pool: PgPool) {
        let node = Node::new("srv-1".to_owned());
        let id = node.register(&pool, Load::default()).await.unwrap();
        // Another node, last heard from a minute ago.
        sqlx::query(
            "INSERT INTO nodes (name, revision, version, heartbeat_at)
             VALUES ('srv-2', 'old', '1.2.14', now() - interval '1 minute')",
        )
        .execute(&pool)
        .await
        .unwrap();
        node.renew(&pool, Load::default()).await.unwrap();
        assert_eq!(leases(&pool).await, [(id, "srv-1".to_owned(), 0, 0)]);

        // Taken for dead by another node while it still runs.
        sqlx::query("DELETE FROM nodes")
            .execute(&pool)
            .await
            .unwrap();
        node.renew(&pool, Load::default()).await.unwrap();
        let again = node.lease().unwrap();
        assert_ne!(again, id);
        assert_eq!(leases(&pool).await, [(again, "srv-1".to_owned(), 0, 0)]);
    }

    #[sqlx::test]
    async fn every_node_gets_the_same_shared_key(pool: PgPool) {
        let first = shared_key(&pool, NAS_TICKETS).await.unwrap();
        assert_eq!(shared_key(&pool, NAS_TICKETS).await.unwrap(), first);
        assert_ne!(shared_key(&pool, "another").await.unwrap(), first);
    }
}

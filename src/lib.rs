use std::sync::Arc;

use axum::{Json, Router, http::header, routing::get};
use serde::Serialize;
use tokio::net::{TcpListener, UdpSocket};
use tower_http::trace::TraceLayer;

mod accounts;
mod activity;
pub mod admin;
mod bans;
mod gamestats;
mod gpcm;
mod html;
mod master;
mod nas;
mod natneg;
pub mod node;
mod online;
mod pairing;
pub mod profiles;
pub mod relay;
mod reports;
mod serverbrowser;
mod switches;

pub const REVISION: &str = env!("BUILD_REVISION");
/// The release, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The daily log files in `LOG_DIR` are `recharged-wfc-srv.2026-10-24.log`;
/// `/admin` reads them.
pub const LOG_FILE_PREFIX: &str = "recharged-wfc-srv";
pub const LOG_FILE_SUFFIX: &str = "log";

/// The one game this server serves, as GameSpy names it.
const GAME_NAME: &str = "mschargedwii";

/// The server's services and what they share: the database, NAS's login
/// tokens, which GPCM checks, GPCM's logins, which gamestats2 checks and
/// `/admin` shows (with the other nodes'), and the master server's sessions.
#[derive(Clone)]
pub struct App {
    pool: sqlx::PgPool,
    tickets: Arc<nas::Tickets>,
    gpcm: Arc<gpcm::Gpcm>,
    master: Arc<master::Master>,
    /// What happened in matchmaking lately, which `/admin` shows.
    activity: Arc<activity::Activity>,
    /// Matches' games kept for RANKED: the profile service sets them, the
    /// server browser lists by them.
    pairings: Arc<pairing::Pairings>,
    /// This server as a node of the pool (docs/nodes.md).
    node: Arc<node::Node>,
}

impl App {
    /// A server on its own: named by its host, with keys of its own and no
    /// lease, as in tests. A server that serves runs [`App::join`].
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self::with(pool, nas::Tickets::default(), node::host_name())
    }

    /// The server as the node `name` of the pool (docs/nodes.md): with the
    /// keys the nodes share, and its lease drawn.
    pub async fn join(pool: sqlx::PgPool, name: String) -> Result<Self, sqlx::Error> {
        let key = node::shared_key(&pool, node::NAS_TICKETS).await?;
        let app = Self::with(pool, nas::Tickets::with_key(key), name);
        app.node.register(&app.pool, app.load()).await?;
        Ok(app)
    }

    fn with(pool: sqlx::PgPool, tickets: nas::Tickets, name: String) -> Self {
        let tickets = Arc::new(tickets);
        let activity = Arc::<activity::Activity>::default();
        let node = Arc::new(node::Node::new(name));
        Self {
            gpcm: Arc::new(gpcm::Gpcm::new(
                pool.clone(),
                tickets.clone(),
                activity.clone(),
                node.clone(),
            )),
            master: Arc::default(),
            activity,
            pairings: Arc::default(),
            node,
            pool,
            tickets,
        }
    }

    /// The node's name.
    pub fn node_name(&self) -> &str {
        self.node.name()
    }

    /// What the node carries now.
    fn load(&self) -> node::Load {
        let count = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
        node::Load {
            players: count(self.gpcm.local_presence().len()),
            games: count(self.master.sessions().len()),
        }
    }

    /// Renews the node's lease every few seconds, for as long as it runs.
    pub async fn keep_lease(&self) -> std::io::Result<()> {
        self.node.keep(&self.pool, || self.load()).await
    }

    /// Removes the node's lease, as it stops, within [`node::LEAVE_WITHIN`].
    /// A failure is logged: the lease runs out by itself.
    pub async fn leave(&self) {
        match tokio::time::timeout(node::LEAVE_WITHIN, self.node.leave(&self.pool)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(target: "node", %error, "lease not removed"),
            Err(_) => tracing::warn!(target: "node", "lease not removed: no database"),
        }
    }

    /// The HTTP routes: status, NAS, gamestats2, match reports, and `/admin`
    /// and the profile service with their configurations. NAS, gamestats2 and
    /// the reports need the caller's address: serve them with
    /// `into_make_service_with_connect_info::<SocketAddr>()`.
    pub fn router(
        &self,
        admin: Option<admin::Config>,
        profiles: Option<profiles::Config>,
    ) -> Router {
        let node = self.node.clone();
        let mut router = Router::new()
            .route("/status", get(move || status(node.clone())))
            .merge(nas::router(self.pool.clone(), self.tickets.clone()))
            .merge(gamestats::router(
                self.pool.clone(),
                self.gpcm.clone(),
                self.pairings.clone(),
            ))
            .merge(reports::router(self.pool.clone(), self.gpcm.clone()));
        // The Maintenance page switches test sign-in while profiles run.
        let test_sign_in = profiles.as_ref().map(|config| config.test_sign_in);
        if let Some(config) = admin {
            router = router.merge(admin::router(
                self.pool.clone(),
                self.gpcm.clone(),
                self.master.clone(),
                self.activity.clone(),
                self.pairings.clone(),
                config,
                test_sign_in,
            ));
        }
        if let Some(config) = profiles {
            router = router.merge(profiles::router(
                self.pool.clone(),
                self.gpcm.clone(),
                self.pairings.clone(),
                config,
            ));
        }
        // One span per request; its events are at `debug` (`RUST_LOG=tower_http=debug`).
        router.layer(TraceLayer::new_for_http())
    }

    /// GPCM, the GameSpy login, on `listener` (TCP 29900).
    pub async fn serve_gpcm(&self, listener: TcpListener) -> std::io::Result<()> {
        self.gpcm.clone().serve(listener).await
    }

    /// The server browser, which lists the master server's sessions, on
    /// `listener` (TCP 28910).
    pub async fn serve_server_browser(&self, listener: TcpListener) -> std::io::Result<()> {
        serverbrowser::serve(
            self.master.clone(),
            self.activity.clone(),
            self.pairings.clone(),
            listener,
        )
        .await
    }

    /// NAT negotiation on `socket` (UDP 27901), with the relay for games
    /// that cannot reach each other if `relay` has its settings.
    pub async fn serve_natneg(
        &self,
        socket: UdpSocket,
        relay: Option<relay::Config>,
    ) -> std::io::Result<()> {
        Arc::new(natneg::Natneg::new(
            self.activity.clone(),
            self.master.clone(),
            relay,
        ))
        .serve(socket)
        .await
    }

    /// The QR2 master server on `socket` (UDP 27900).
    pub async fn serve_master(&self, socket: UdpSocket) -> std::io::Result<()> {
        self.master.clone().serve(socket).await
    }

    /// Samples the players online once a minute, for `/admin`'s Monitoring.
    pub async fn sample_online(&self) -> std::io::Result<()> {
        online::run(self.pool.clone(), self.gpcm.clone(), self.node.clone()).await
    }
}

#[derive(Serialize)]
struct Status {
    status: &'static str,
    revision: &'static str,
    /// Which node answered.
    node: String,
}

async fn status(node: Arc<node::Node>) -> impl axum::response::IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(Status {
            status: "ok",
            revision: REVISION,
            node: node.name().to_owned(),
        }),
    )
}

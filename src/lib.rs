use std::sync::Arc;

use axum::{Json, Router, http::header, routing::get};
use serde::Serialize;
use tokio::net::{TcpListener, UdpSocket};
use tower_http::trace::TraceLayer;

mod accounts;
mod activity;
pub mod admin;
mod gamestats;
mod gpcm;
mod html;
mod master;
mod nas;
mod natneg;
mod pairing;
pub mod profiles;
mod reports;
mod serverbrowser;

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
/// `/admin` shows, and the master server's sessions.
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
}

impl App {
    pub fn new(pool: sqlx::PgPool) -> Self {
        let tickets = Arc::<nas::Tickets>::default();
        let activity = Arc::<activity::Activity>::default();
        Self {
            gpcm: Arc::new(gpcm::Gpcm::new(
                pool.clone(),
                tickets.clone(),
                activity.clone(),
            )),
            master: Arc::default(),
            activity,
            pairings: Arc::default(),
            pool,
            tickets,
        }
    }

    /// The HTTP routes: status, NAS, gamestats2, match reports, and `/admin`
    /// and the profile service with their configurations. gamestats2 and the
    /// reports need the caller's address: serve them with
    /// `into_make_service_with_connect_info::<SocketAddr>()`.
    pub fn router(
        &self,
        admin: Option<admin::Config>,
        profiles: Option<profiles::Config>,
    ) -> Router {
        let mut router = Router::new()
            .route("/status", get(status))
            .merge(nas::router(self.pool.clone(), self.tickets.clone()))
            .merge(gamestats::router(self.pool.clone(), self.gpcm.clone()))
            .merge(reports::router(self.pool.clone(), self.gpcm.clone()));
        if let Some(config) = admin {
            router = router.merge(admin::router(
                self.pool.clone(),
                self.gpcm.clone(),
                self.master.clone(),
                self.activity.clone(),
                config,
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

    /// NAT negotiation on `socket` (UDP 27901).
    pub async fn serve_natneg(&self, socket: UdpSocket) -> std::io::Result<()> {
        Arc::new(natneg::Natneg::new(self.activity.clone()))
            .serve(socket)
            .await
    }

    /// The QR2 master server on `socket` (UDP 27900).
    pub async fn serve_master(&self, socket: UdpSocket) -> std::io::Result<()> {
        self.master.clone().serve(socket).await
    }
}

#[derive(Serialize)]
struct Status {
    status: &'static str,
    revision: &'static str,
}

async fn status() -> impl axum::response::IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(Status {
            status: "ok",
            revision: REVISION,
        }),
    )
}

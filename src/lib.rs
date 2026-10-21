use std::sync::Arc;

use axum::{Json, Router, http::header, routing::get};
use serde::Serialize;
use tokio::net::{TcpListener, UdpSocket};
use tower_http::trace::TraceLayer;

mod accounts;
pub mod admin;
mod gamestats;
mod gpcm;
mod master;
mod nas;

pub const REVISION: &str = env!("BUILD_REVISION");

/// The one game this server serves, as GameSpy names it.
const GAME_NAME: &str = "mschargedwii";

/// The server's services and what they share: the database, NAS's login
/// tokens, which GPCM checks, GPCM's logins, which `/admin` shows, and the
/// master server's sessions.
#[derive(Clone)]
pub struct App {
    pool: sqlx::PgPool,
    tickets: Arc<nas::Tickets>,
    gpcm: Arc<gpcm::Gpcm>,
    master: Arc<master::Master>,
}

impl App {
    pub fn new(pool: sqlx::PgPool) -> Self {
        let tickets = Arc::<nas::Tickets>::default();
        Self {
            gpcm: Arc::new(gpcm::Gpcm::new(pool.clone(), tickets.clone())),
            master: Arc::default(),
            pool,
            tickets,
        }
    }

    /// The HTTP routes: status, NAS, gamestats2, and `/admin` with its
    /// configuration.
    pub fn router(&self, admin: Option<admin::Config>) -> Router {
        let mut router = Router::new()
            .route("/status", get(status))
            .merge(nas::router(self.pool.clone(), self.tickets.clone()))
            .merge(gamestats::router(self.pool.clone()));
        if let Some(config) = admin {
            router = router.merge(admin::router(self.pool.clone(), self.gpcm.clone(), config));
        }
        // One span per request; its events are at `debug` (`RUST_LOG=tower_http=debug`).
        router.layer(TraceLayer::new_for_http())
    }

    /// GPCM, the GameSpy login, on `listener` (TCP 29900).
    pub async fn serve_gpcm(&self, listener: TcpListener) -> std::io::Result<()> {
        self.gpcm.clone().serve(listener).await
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

use axum::{Json, Router, http::header, routing::get};
use serde::Serialize;
use tower_http::trace::TraceLayer;

pub mod admin;
mod gamestats;

pub const REVISION: &str = env!("BUILD_REVISION");

/// The server's routes. `/admin` is served only with its configuration.
pub fn router(pool: sqlx::Pool<sqlx::Postgres>, admin: Option<admin::Config>) -> Router {
    let mut router = Router::new()
        .route("/status", get(status))
        .merge(gamestats::router(pool.clone()));
    if let Some(config) = admin {
        router = router.merge(admin::router(pool, config));
    }
    // One span per request; its events are at `debug` (`RUST_LOG=tower_http=debug`).
    router.layer(TraceLayer::new_for_http())
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

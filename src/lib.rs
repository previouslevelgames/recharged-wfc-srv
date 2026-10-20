use axum::{Json, Router, http::header, routing::get};
use serde::Serialize;
use tower_http::trace::TraceLayer;

mod gamestats;

pub const REVISION: &str = env!("BUILD_REVISION");

pub fn router(pool: sqlx::Pool<sqlx::Postgres>) -> Router {
    Router::new()
        .route("/status", get(status))
        .merge(gamestats::router(pool))
        // One span per request; its events are at `debug` (`RUST_LOG=tower_http=debug`).
        .layer(TraceLayer::new_for_http())
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

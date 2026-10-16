use axum::{Json, Router, http::header, routing::get};
use serde::Serialize;

pub const REVISION: &str = env!("BUILD_REVISION");

pub fn router() -> Router {
    Router::new().route("/status", get(status))
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

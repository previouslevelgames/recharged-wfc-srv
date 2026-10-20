use axum::{body::Body, http::Request};
use tower::ServiceExt;

#[tokio::test]
async fn status_returns_the_embedded_revision_as_uncached_json() {
    let response = recharged_wfc_srv::router(lazy_pool(), None)
        .oneshot(Request::get("/status").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(response.headers()["cache-control"], "no-store");

    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        status,
        serde_json::json!({"status": "ok", "revision": recharged_wfc_srv::REVISION})
    );
    assert!(!status["revision"].as_str().unwrap().is_empty());
}

/// A pool that connects on its first query: these requests never make one.
fn lazy_pool() -> sqlx::PgPool {
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    PgPoolOptions::new().connect_lazy_with(PgConnectOptions::new())
}

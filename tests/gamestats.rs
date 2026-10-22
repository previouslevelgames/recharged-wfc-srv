use std::net::SocketAddr;

use axum::{body::Body, extract::connect_info::MockConnectInfo, http::Request};
use tower::ServiceExt;

const GET2: &str = "/mschargedwii/web/client/get2.asp";
const PUT2: &str = "/mschargedwii/web/client/put2.asp";

async fn get(uri: &str) -> (u16, Vec<u8>) {
    let response = recharged_wfc_srv::App::new(lazy_pool())
        .router(None)
        .layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 4], 50000))))
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    (status, body.to_vec())
}

#[tokio::test]
async fn the_first_request_gets_a_32_byte_token() {
    for path in [GET2, PUT2] {
        let (status, body) = get(&format!("{path}?pid=1000000003")).await;
        assert_eq!(status, 200);
        assert_eq!(body.len(), 32, "{path}");
    }
}

#[tokio::test]
async fn other_games_and_endpoints_are_not_found() {
    for uri in [
        "/othergame/web/client/get2.asp?pid=1",
        "/mschargedwii/web/client/other.asp?pid=1",
    ] {
        assert_eq!(get(uri).await.0, 404, "{uri}");
    }
}

/// The client turns any 4xx/5xx into a global DWC error: refusals are 200.
#[tokio::test]
async fn refusals_are_200_with_an_error_body() {
    for query in [
        "",
        "?pid=abc",
        "?pid=1&hash=0000000000000000000000000000000000000000",
        "?pid=1&hash=0000000000000000000000000000000000000000&data=AAAA",
    ] {
        let (status, body) = get(&format!("{GET2}{query}")).await;
        assert_eq!(status, 200, "{query}");
        assert!(body.starts_with(b"error:"), "{query}");
    }
}

/// A pool that connects on its first query: these requests never make one.
fn lazy_pool() -> sqlx::PgPool {
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    PgPoolOptions::new().connect_lazy_with(PgConnectOptions::new())
}

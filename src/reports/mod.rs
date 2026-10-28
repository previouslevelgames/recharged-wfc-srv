//! WFC match reports (docs/reports.md): Recharged builds report every series
//! they play online, ranked or with friends, which retail games never tell
//! the server. Both games of a match report, each from the address it is
//! logged in to GPCM from; the server pairs their sides into one match and
//! works its result out from the games both saw the same ([`store`],
//! [`outcome`]). They also tell what went wrong; with the error codes the
//! games showed ([`errors`]), the Monitoring page of `/admin` shows it.

mod errors;
pub(crate) mod outcome;
pub(crate) mod report;
mod store;

use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, State, rejection::BytesRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::json;
use sqlx::PgPool;
use tracing::{debug, info, trace, warn};

use crate::gpcm::Gpcm;
use report::Report;
pub(crate) use store::{left, settle_idle, sides};

/// A report is a few hundred bytes.
const MAX_BODY: usize = 16 * 1024;

#[derive(Clone)]
struct ReportsState {
    pool: PgPool,
    /// Who is logged in where: the reports they may send.
    gpcm: Arc<Gpcm>,
    /// The errors each address sent lately.
    limits: Arc<errors::Limits>,
}

/// `POST /api/v1/wfc/reports` and `POST /api/v1/wfc/errors`, for any host
/// name: the game sends them to its server's address. They need the
/// caller's address: serve them with
/// `into_make_service_with_connect_info::<SocketAddr>()`.
pub fn router(pool: PgPool, gpcm: Arc<Gpcm>) -> Router {
    Router::new()
        .route("/api/v1/wfc/reports", post(receive))
        .route("/api/v1/wfc/errors", post(receive_error))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(ReportsState {
            pool,
            gpcm,
            limits: Arc::default(),
        })
}

async fn receive(
    State(state): State<ReportsState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(rejection) => return refuse(peer, &rejection.body_text()),
    };
    let text = String::from_utf8_lossy(&body);
    trace!(target: "raw::reports", %peer, body = %text, "report");
    let report = match Report::parse(&body) {
        Ok(report) => report,
        Err(problem) => {
            // Counted for a game logged in from here only: a stranger could
            // name any profile.
            if let Some(profile_id) = profile_id(&body)
                && state.gpcm.logged_in_from(profile_id) == Some(peer.ip())
            {
                state
                    .count_refusal(profile_id, "invalid", &problem, peer)
                    .await;
            }
            return refuse(peer, &problem);
        }
    };

    // As for gamestats2's puts (docs/gamestats2.md, "Trust"): a profile id
    // is easy to learn, the login's address is not.
    let logged_in_from = state.gpcm.logged_in_from(report.profile_id);
    if logged_in_from != Some(peer.ip()) {
        warn!(target: "reports", profile_id = report.profile_id, %peer, ?logged_in_from, "report from elsewhere");
        let problem = match logged_in_from {
            Some(address) => format!("logged in from {address}"),
            None => "not logged in".to_owned(),
        };
        state
            .count_refusal(report.profile_id, "elsewhere", &problem, peer)
            .await;
        return json_error(
            StatusCode::FORBIDDEN,
            "profile not logged in from this address",
        );
    }

    match store::record(&state.pool, &report, &text).await {
        Ok(recorded) => {
            let (match_id, side) = (recorded.match_id, recorded.side.as_str());
            let (event, game) = (report.event.name(), report.event.game());
            if recorded.repeated {
                debug!(target: "reports", match_id, profile_id = report.profile_id, series = report.series, event, game, "report repeated");
            } else {
                info!(target: "reports", match_id, profile_id = report.profile_id, series = report.series, side, event, game, state = recorded.state, "report stored");
            }
            Json(json!({ "match_id": match_id })).into_response()
        }
        Err(error) => {
            warn!(target: "reports", %error, "database error");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the server's database failed",
            )
        }
    }
}

/// An error code a game showed its player.
async fn receive_error(
    State(state): State<ReportsState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(rejection) => return refuse(peer, &rejection.body_text()),
    };
    trace!(target: "raw::reports", %peer, body = %String::from_utf8_lossy(&body), "error");
    let error = match errors::ClientError::parse(&body) {
        Ok(error) => error,
        Err(problem) => return refuse(peer, &problem),
    };
    if !state.limits.allow(peer.ip()) {
        debug!(target: "reports", %peer, code = error.code, "error dropped: too many");
        return json_error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many errors from this address",
        );
    }
    // Only a profile logged in from where the error came is kept.
    let profile_id = error
        .profile_id
        .filter(|&id| state.gpcm.logged_in_from(id) == Some(peer.ip()));
    let address = peer.ip().to_string();
    match errors::store(&state.pool, &error, profile_id, &address).await {
        Ok(()) => {
            info!(target: "reports", code = error.code, phase = error.phase, ?profile_id, %peer, build = error.build, "error code reported");
            Json(json!({})).into_response()
        }
        Err(failure) => {
            warn!(target: "reports", error = %failure, "database error");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the server's database failed",
            )
        }
    }
}

impl ReportsState {
    /// Keeps a refusal for the Monitoring page; the log has it anyway.
    async fn count_refusal(&self, profile_id: i32, kind: &str, problem: &str, peer: SocketAddr) {
        let address = peer.ip().to_string();
        if let Err(error) = store::refused(&self.pool, profile_id, kind, problem, &address).await {
            warn!(target: "reports", %error, "database error");
        }
    }
}

/// The profile a body that is not a report names, if it names one.
fn profile_id(body: &[u8]) -> Option<i32> {
    let body: serde_json::Value = serde_json::from_slice(body).ok()?;
    i32::try_from(body.get("profile_id")?.as_i64()?).ok()
}

fn refuse(peer: SocketAddr, problem: &str) -> Response {
    warn!(target: "reports", %peer, problem, "report refused");
    json_error(StatusCode::BAD_REQUEST, problem)
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use axum::{
        body::Body,
        extract::connect_info::MockConnectInfo,
        http::{Request, header},
    };
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;
    use crate::{accounts, reports::report::tests::start};

    const ANOSCH_HOME: [u8; 4] = [203, 0, 113, 4];
    const SAM_HOME: [u8; 4] = [198, 51, 100, 7];

    struct Server {
        pool: PgPool,
        gpcm: Arc<Gpcm>,
    }

    impl Server {
        fn new(pool: PgPool) -> Self {
            let gpcm = Gpcm::new(pool.clone(), Arc::default(), Arc::default());
            Self {
                pool,
                gpcm: Arc::new(gpcm),
            }
        }

        /// A profile of a console of its own, logged in to GPCM from `address`.
        async fn log_in(&self, user_id: i64, mii: &str, address: [u8; 4]) -> i32 {
            accounts::log_in(&self.pool, user_id).await.unwrap();
            let profile =
                accounts::profile_login(&self.pool, user_id, "R4QEaaaaaaa", Some(mii), "x")
                    .await
                    .unwrap();
            self.gpcm
                .fake_login(profile.profile_id, IpAddr::from(address));
            profile.profile_id
        }

        /// POSTs `body` to the reports from `address`, to the server's bare
        /// IP as the game does. The status and the JSON answered.
        async fn post(&self, address: [u8; 4], body: impl Into<Body>) -> (StatusCode, Value) {
            self.post_to("/api/v1/wfc/reports", address, body).await
        }

        async fn post_to(
            &self,
            path: &str,
            address: [u8; 4],
            body: impl Into<Body>,
        ) -> (StatusCode, Value) {
            let app = router(self.pool.clone(), self.gpcm.clone())
                .layer(MockConnectInfo(SocketAddr::from((address, 51000))));
            let request = Request::post(path)
                .header(header::HOST, "2.31.48.136")
                .header(header::CONTENT_TYPE, "application/json")
                .body(body.into())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap();
            (status, serde_json::from_slice(&bytes).unwrap())
        }

        async fn report(&self, address: [u8; 4], body: &Value) -> (StatusCode, Value) {
            self.post(address, body.to_string()).await
        }
    }

    fn as_sam(profile_id: i32) -> Value {
        let mut body = start();
        body["profile_id"] = json!(profile_id);
        body["series"] = json!(1_792_000_002);
        body["side"] = json!("away");
        body["mii"] = json!("Sam");
        body["opponent_mii"] = json!("Anosch");
        body
    }

    fn game(mut body: Value, number: i32, home: i32, away: i32) -> Value {
        body["event"] = json!("game");
        body["game"] = json!({
            "number": number, "score": {"home": home, "away": away},
            "captains": {"home": "mario", "away": "bowser"},
            "stadium": "unknown", "ending": "normal", "seconds": 180,
        });
        body
    }

    #[sqlx::test]
    async fn refusals_are_kept_for_the_monitoring(pool: PgPool) {
        let server = Server::new(pool.clone());
        let anosch = server.log_in(1, "Anosch", ANOSCH_HOME).await;
        let mut body = start();
        body["profile_id"] = json!(anosch);
        assert_eq!(
            server.report(SAM_HOME, &body).await.0,
            StatusCode::FORBIDDEN
        );
        body["mode"] = json!("casual");
        assert_eq!(
            server.report(ANOSCH_HOME, &body).await.0,
            StatusCode::BAD_REQUEST
        );

        let rows = sqlx::query!(
            "SELECT kind, count, problem, address FROM report_refusals WHERE profile_id = $1
             ORDER BY kind",
            anosch,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let rows: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.kind.as_str(),
                    row.count,
                    row.problem.as_str(),
                    row.address.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("elsewhere", 1, "logged in from 203.0.113.4", "198.51.100.7"),
                ("invalid", 1, "mode: one of ranked, friends", "203.0.113.4"),
            ]
        );
    }

    #[sqlx::test]
    async fn an_error_keeps_its_profile_only_from_its_login(pool: PgPool) {
        let server = Server::new(pool.clone());
        let anosch = server.log_in(1, "Anosch", ANOSCH_HOME).await;
        let error = json!({
            "release": "R4QE", "build": "1.1.0+g0123456789ab", "code": 86420,
            "phase": "matchmaking", "profile_id": anosch,
        });
        for address in [ANOSCH_HOME, SAM_HOME] {
            let answer = server
                .post_to("/api/v1/wfc/errors", address, error.to_string())
                .await;
            assert_eq!(answer, (StatusCode::OK, json!({})));
        }
        let (status, _) = server
            .post_to(
                "/api/v1/wfc/errors",
                ANOSCH_HOME,
                r#"{"release":"R4QE","code":0}"#,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let rows = sqlx::query!(
            "SELECT code, phase, build, profile_id, address FROM client_errors ORDER BY error_id"
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let rows: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.code,
                    row.phase.as_deref(),
                    row.profile_id,
                    row.address.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (86420, Some("matchmaking"), Some(anosch), "203.0.113.4"),
                (86420, Some("matchmaking"), None, "198.51.100.7"),
            ]
        );
    }

    #[sqlx::test]
    async fn a_report_comes_from_the_profiles_login(pool: PgPool) {
        let server = Server::new(pool);
        let anosch = server.log_in(1, "Anosch", ANOSCH_HOME).await;
        let mut body = start();
        body["profile_id"] = json!(anosch);

        // From another address, or for a profile not logged in.
        let refused = json!({"error": "profile not logged in from this address"});
        assert_eq!(
            server.report(SAM_HOME, &body).await,
            (StatusCode::FORBIDDEN, refused.clone())
        );
        let mut stranger = body.clone();
        stranger["profile_id"] = json!(1_000_000_050);
        assert_eq!(
            server.report(ANOSCH_HOME, &stranger).await,
            (StatusCode::FORBIDDEN, refused)
        );
        let none = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM online_matches"#)
            .fetch_one(&server.pool)
            .await
            .unwrap();
        assert_eq!(none, 0);

        let (status, answer) = server.report(ANOSCH_HOME, &body).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert!(answer["match_id"].as_i64().unwrap() > 0);
        // The same report again: the same match.
        assert_eq!(server.report(ANOSCH_HOME, &body).await, (status, answer));
    }

    #[sqlx::test]
    async fn malformed_reports_say_what_is_wrong(pool: PgPool) {
        let server = Server::new(pool);
        let anosch = server.log_in(1, "Anosch", ANOSCH_HOME).await;
        let mut body = start();
        body["profile_id"] = json!(anosch);

        let mut wrong = body.clone();
        wrong["mode"] = json!("cup");
        let (status, answer) = server.report(ANOSCH_HOME, &wrong).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(answer["error"], "mode: one of ranked, friends");

        for raw in ["{", "null", ""] {
            let (status, answer) = server.post(ANOSCH_HOME, raw).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{raw}");
            assert!(answer["error"].as_str().is_some(), "{raw}: {answer}");
        }
        // Checked before the login: a broken report is 400 from anywhere.
        assert_eq!(server.post(SAM_HOME, "{").await.0, StatusCode::BAD_REQUEST);

        let mut padded = body.clone();
        padded["padding"] = json!("x".repeat(MAX_BODY));
        let (status, answer) = server.report(ANOSCH_HOME, &padded).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(answer["error"].as_str().is_some());
    }

    #[sqlx::test]
    async fn both_games_report_a_series_into_one_match(pool: PgPool) {
        let server = Server::new(pool);
        let anosch = server.log_in(1, "Anosch", ANOSCH_HOME).await;
        let sam = server.log_in(2, "Sam", SAM_HOME).await;
        let mut home = start();
        home["profile_id"] = json!(anosch);
        let away = as_sam(sam);

        let (_, opened) = server.report(ANOSCH_HOME, &home).await;
        let (_, joined) = server.report(SAM_HOME, &away).await;
        assert_eq!(joined, opened);
        // Sam's game can't report Anosch's side: it is not logged in there.
        assert_eq!(
            server
                .report(SAM_HOME, &game(home.clone(), 1, 3, 0))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        for number in [1, 2] {
            assert_eq!(
                server
                    .report(ANOSCH_HOME, &game(home.clone(), number, 3, 0))
                    .await
                    .1,
                opened
            );
            assert_eq!(
                server
                    .report(SAM_HOME, &game(away.clone(), number, 3, 0))
                    .await
                    .1,
                opened
            );
        }

        let match_id = opened["match_id"].as_i64().unwrap();
        let row = sqlx::query!(
            "SELECT state, winner, home_wins, away_wins FROM online_matches WHERE match_id = $1",
            match_id
        )
        .fetch_one(&server.pool)
        .await
        .unwrap();
        assert_eq!(
            (
                row.state.as_str(),
                row.winner.as_deref(),
                row.home_wins,
                row.away_wins
            ),
            ("finished", Some("home"), 2, 0)
        );
    }
}

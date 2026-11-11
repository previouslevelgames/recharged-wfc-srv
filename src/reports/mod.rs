//! WFC match reports (docs/reports.md): Recharged builds report every series
//! they play online, ranked or with friends, which retail games never tell
//! the server. Both games of a match report, each from the address it is
//! logged in to GPCM from; the server pairs their sides into one match and
//! works its result out from the games both saw the same ([`store`],
//! [`outcome`]). They also tell what went wrong; with the error codes the
//! games showed ([`errors`]), the Monitoring page of `/admin` shows it.

pub(crate) mod dossiers;
pub(crate) mod errors;
pub(crate) mod outcome;
pub(crate) mod record;
pub(crate) mod report;
mod store;

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        ConnectInfo, DefaultBodyLimit, Query, Request, State,
        rejection::{BytesRejection, QueryRejection},
    },
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use time::format_description::well_known::Rfc3339;
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

/// `POST /api/v1/wfc/reports`, `POST /api/v1/wfc/errors`,
/// `POST /api/v1/wfc/desyncs` and `GET /api/v1/wfc/forfeit`, for any host
/// name: the game sends them to its server's address. They need the caller's address: serve them with
/// `into_make_service_with_connect_info::<SocketAddr>()`.
pub fn router(pool: PgPool, gpcm: Arc<Gpcm>) -> Router {
    let state = ReportsState {
        pool,
        gpcm,
        limits: Arc::default(),
    };
    // A dossier is bigger than a report: its body is read only from where
    // some profile is logged in.
    let desyncs = Router::new()
        .route("/api/v1/wfc/desyncs", post(receive_dossier))
        .layer(DefaultBodyLimit::max(dossiers::MAX_BYTES))
        .route_layer(middleware::from_fn_with_state(state.clone(), from_a_login));
    Router::new()
        .route("/api/v1/wfc/reports", post(receive))
        .route("/api/v1/wfc/errors", post(receive_error))
        .route("/api/v1/wfc/forfeit", get(forfeit))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .merge(desyncs)
        .with_state(state)
}

/// Before a dossier's body is read: a request from no login's address is
/// refused as one from another profile's would be, without reading its
/// bytes. The handler then checks the dossier's own profile.
async fn from_a_login(
    State(state): State<ReportsState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if !state.gpcm.any_http_login_at(peer.ip()) {
        warn!(target: "reports", %peer, "dossier from no login");
        return json_error(
            StatusCode::FORBIDDEN,
            "profile not logged in from this address",
        );
    }
    next.run(request).await
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
                && logged_in_at(&state.gpcm, profile_id, peer.ip())
            {
                state
                    .count_refusal(profile_id, "invalid", &problem, peer)
                    .await;
            }
            return refuse(peer, &problem);
        }
    };

    // As for gamestats2's puts (docs/gamestats2.md, "Trust"): a profile id
    // is easy to learn, the login's addresses are not.
    let login = state.gpcm.login_addresses(report.profile_id);
    if !login.is_some_and(|login| login.contains(peer.ip())) {
        let logged_in_from = login.map(|login| login.to_string());
        warn!(target: "reports", profile_id = report.profile_id, %peer, ?logged_in_from, "report from elsewhere");
        let problem = match login {
            Some(login) => format!("logged in from {login}"),
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
        .filter(|&id| logged_in_at(&state.gpcm, id, peer.ip()));
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

/// A game's desync dossier (docs/reports.md, "Desync dossiers"). The game
/// deletes it on 200, 400, 413 and 422, and sends it again on 403, 429 and
/// 5xx: never 404, which tells it the server takes none.
async fn receive_dossier(
    State(state): State<ReportsState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        // 413 for one too big: the game gives it up.
        Err(rejection) => {
            warn!(target: "reports", %peer, problem = rejection.body_text(), "dossier refused");
            return json_error(rejection.status(), &rejection.body_text());
        }
    };
    trace!(target: "raw::reports", %peer, bytes = body.len(), "dossier");
    let dossier = match dossiers::Dossier::parse(&body) {
        Ok(dossier) => dossier,
        Err(problem) => return refuse(peer, &problem),
    };
    let profile_id = dossier.profile_id;
    if !logged_in_at(&state.gpcm, profile_id, peer.ip()) {
        warn!(target: "reports", profile_id, %peer, "dossier from elsewhere");
        return json_error(
            StatusCode::FORBIDDEN,
            "profile not logged in from this address",
        );
    }

    let (series, game, frame) = (dossier.series, dossier.game, dossier.frame);
    match dossiers::store(&state.pool, &dossier, &body).await {
        Ok(dossiers::Stored::Kept { match_id, diverged }) => {
            let diverged = diverged.map(dossiers::Kind::as_str);
            info!(target: "reports", match_id, profile_id, series, game, frame, side = dossier.side.as_str(), build = dossier.build, ?diverged, "desync dossier stored");
            Json(json!({ "match_id": match_id })).into_response()
        }
        Ok(dossiers::Stored::NoSeries) => {
            debug!(target: "reports", profile_id, series, "dossier of no series");
            json_error(StatusCode::UNPROCESSABLE_ENTITY, "no such series")
        }
        Ok(dossiers::Stored::WrongSide) => {
            debug!(target: "reports", profile_id, series, "dossier of the other side");
            json_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "the series was played on the other side",
            )
        }
        Ok(dossiers::Stored::TooMany) => {
            debug!(target: "reports", profile_id, "dossier dropped: too many");
            json_error(
                StatusCode::TOO_MANY_REQUESTS,
                "too many dossiers from this profile",
            )
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

#[derive(Deserialize)]
struct ForfeitQuery {
    profile_id: i32,
}

/// The game whose match broke off asks whether its opponent forfeited it
/// (docs/reports.md, "Forfeit"): the latest forfeit against `profile_id`
/// in the last 3 minutes, `{"forfeit": {"by": "<Mii>", "at": "<RFC 3339>"}}`,
/// else `{"forfeit": null}`. Only from the profile's login, as a report.
async fn forfeit(
    State(state): State<ReportsState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    query: Result<Query<ForfeitQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(ForfeitQuery { profile_id })) = query else {
        debug!(target: "reports", %peer, "forfeit asked without a profile");
        return json_error(StatusCode::BAD_REQUEST, "profile_id: a WFC profile");
    };
    if !logged_in_at(&state.gpcm, profile_id, peer.ip()) {
        debug!(target: "reports", profile_id, %peer, "forfeit asked from elsewhere");
        return json_error(
            StatusCode::FORBIDDEN,
            "profile not logged in from this address",
        );
    }
    match store::forfeit_against(&state.pool, profile_id).await {
        Ok(found) => {
            let forfeit = found.map(|(by, at)| {
                json!({
                    "by": by,
                    "at": at.format(&Rfc3339).expect("a date formats"),
                })
            });
            debug!(target: "reports", profile_id, ?forfeit, "forfeit asked");
            Json(json!({ "forfeit": forfeit })).into_response()
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

impl ReportsState {
    /// Keeps a refusal for the Monitoring page; the log has it anyway.
    async fn count_refusal(&self, profile_id: i32, kind: &str, problem: &str, peer: SocketAddr) {
        let address = peer.ip().to_string();
        if let Err(error) = store::refused(&self.pool, profile_id, kind, problem, &address).await {
            warn!(target: "reports", %error, "database error");
        }
    }
}

/// Whether `profile_id` is logged in and `address` is one of its login's.
fn logged_in_at(gpcm: &Gpcm, profile_id: i32, address: IpAddr) -> bool {
    gpcm.login_addresses(profile_id)
        .is_some_and(|login| login.contains(address))
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
    use crate::{
        accounts,
        reports::{
            dossiers::tests::{dossier, machine_input, step},
            report::{Side, tests::start},
        },
    };

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

        /// GETs `path` from `address`, as the game asks.
        async fn get(&self, path: &str, address: [u8; 4]) -> (StatusCode, Value) {
            let app = router(self.pool.clone(), self.gpcm.clone())
                .layer(MockConnectInfo(SocketAddr::from((address, 51000))));
            let request = Request::get(path)
                .header(header::HOST, "2.31.48.136")
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap();
            (status, serde_json::from_slice(&bytes).unwrap())
        }

        /// The latest forfeit against `profile_id`, as its game asks.
        async fn forfeit(&self, address: [u8; 4], profile_id: i32) -> (StatusCode, Value) {
            self.get(
                &format!("/api/v1/wfc/forfeit?profile_id={profile_id}"),
                address,
            )
            .await
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

    /// A network that sends the game's HTTP out by another address than its
    /// GPCM connection: reports and errors come from where its NAS login did.
    #[sqlx::test]
    async fn a_report_counts_from_where_its_nas_login_came_from(pool: PgPool) {
        const GPCM: [u8; 4] = [12, 75, 123, 129];
        const NAS: [u8; 4] = [12, 75, 115, 108];
        let server = Server::new(pool.clone());
        let anosch = server.log_in(1, "Anosch", GPCM).await;
        server
            .gpcm
            .fake_login_via(anosch, IpAddr::from(GPCM), IpAddr::from(NAS));
        let mut body = start();
        body["profile_id"] = json!(anosch);

        assert_eq!(
            server.report(SAM_HOME, &body).await.0,
            StatusCode::FORBIDDEN
        );
        let (status, answer) = server.report(NAS, &body).await;
        assert_eq!(status, StatusCode::OK, "{answer}");

        let error = json!({"release": "R4QE", "code": 86420, "profile_id": anosch});
        let answer = server
            .post_to("/api/v1/wfc/errors", NAS, error.to_string())
            .await;
        assert_eq!(answer, (StatusCode::OK, json!({})));
        let kept: Option<i32> = sqlx::query_scalar("SELECT profile_id FROM client_errors")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kept, Some(anosch));

        let refusal: String =
            sqlx::query_scalar("SELECT problem FROM report_refusals WHERE profile_id = $1")
                .bind(anosch)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(refusal, "logged in from 12.75.123.129 (NAS 12.75.115.108)");
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

    fn end(mut body: Value, reason: &str, cause: &str) -> Value {
        body["event"] = json!("end");
        body["reason"] = json!(reason);
        body["cause"] = json!(cause);
        body
    }

    #[sqlx::test]
    async fn a_forfeit_gives_the_match_to_the_opponent_who_asks(pool: PgPool) {
        let server = Server::new(pool);
        let anosch = server.log_in(1, "Anosch", ANOSCH_HOME).await;
        let sam = server.log_in(2, "Sam", SAM_HOME).await;
        let mut home = start();
        home["profile_id"] = json!(anosch);
        let away = as_sam(sam);
        let (_, opened) = server.report(ANOSCH_HOME, &home).await;
        server.report(SAM_HOME, &away).await;
        for (address, body) in [(ANOSCH_HOME, &home), (SAM_HOME, &away)] {
            server.report(address, &game(body.clone(), 1, 3, 0)).await;
        }
        let none = (StatusCode::OK, json!({"forfeit": null}));
        assert_eq!(server.forfeit(SAM_HOME, sam).await, none);

        // Anosch won game 1 and leaves game 2 on purpose, leading 1-0: Sam's
        // game sees it broken off.
        let (status, answer) = server
            .report(ANOSCH_HOME, &end(home.clone(), "abandoned", "forfeit"))
            .await;
        assert_eq!((status, &answer), (StatusCode::OK, &opened));
        let mut broke_off = game(away.clone(), 2, 1, 0);
        broke_off["game"]["ending"] = json!("disconnect");
        server.report(SAM_HOME, &broke_off).await;
        server
            .report(SAM_HOME, &end(away.clone(), "abandoned", "disconnect"))
            .await;
        let match_id = opened["match_id"].as_i64().unwrap();
        let row = sqlx::query!(
            "SELECT state, winner, home_wins, away_wins, forfeited_by FROM online_matches
             WHERE match_id = $1",
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
                row.away_wins,
                row.forfeited_by
            ),
            ("finished", Some("away"), 2, 0, Some(anosch))
        );

        // Sam's game asks, from its login: Anosch forfeited, just now.
        let (status, answer) = server.forfeit(SAM_HOME, sam).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(answer["forfeit"]["by"], "Anosch");
        let at = answer["forfeit"]["at"].as_str().unwrap();
        let at = time::OffsetDateTime::parse(at, &Rfc3339).unwrap();
        assert!((time::OffsetDateTime::now_utc() - at).whole_seconds() < 10);
        // Not against Anosch; not from elsewhere; not without a profile.
        assert_eq!(server.forfeit(ANOSCH_HOME, anosch).await, none);
        assert_eq!(
            server.forfeit(ANOSCH_HOME, sam).await,
            (
                StatusCode::FORBIDDEN,
                json!({"error": "profile not logged in from this address"})
            )
        );
        assert_eq!(
            server.get("/api/v1/wfc/forfeit", SAM_HOME).await.0,
            StatusCode::BAD_REQUEST
        );
        // Three minutes later it is no news.
        sqlx::query!(
            "UPDATE online_matches SET forfeited_at = now() - interval '181 seconds'
             WHERE match_id = $1",
            match_id
        )
        .execute(&server.pool)
        .await
        .unwrap();
        assert_eq!(server.forfeit(SAM_HOME, sam).await, none);
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
        // Nobody forfeited: Sam's game, which lost, asks for nothing.
        assert_eq!(
            server.forfeit(SAM_HOME, sam).await,
            (StatusCode::OK, json!({"forfeit": null}))
        );
    }

    /// Nor more than so many bytes a day, however few dossiers they make.
    #[sqlx::test]
    async fn a_profile_sends_so_many_bytes_a_day(pool: PgPool) {
        let server = Server::new(pool.clone());
        let anosch = server.log_in(1, "Anosch", ANOSCH_HOME).await;
        let mut body = start();
        body["profile_id"] = json!(anosch);
        server.report(ANOSCH_HOME, &body).await;
        // Nine large dossiers of another match, 2.1 MiB together.
        sqlx::query!(
            "WITH m AS (
                 INSERT INTO online_matches (release, mode, best_of, home_mii, away_mii,
                                             home_captain, away_captain)
                 VALUES ('R4QE', 'ranked', 3, 'Anosch', 'Sam', 'mario', 'luigi')
                 RETURNING match_id)
             INSERT INTO desync_dossiers
                 (match_id, side, game, profile_id, release, build, platform, frame,
                  written_at, body)
             SELECT m.match_id, 'home', game, $1, 'R4QE', '1.1.6', '', 1, now(),
                    convert_to(repeat('x', 250000), 'UTF8')
             FROM m, generate_series(1, 9) AS game",
            anosch,
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut home = dossier(Side::Home);
        home.profile_id = anosch;
        home.series = 1_792_000_000;
        let (status, _) = server
            .post_to("/api/v1/wfc/desyncs", ANOSCH_HOME, home.encode())
            .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

        // With two of them gone, it fits.
        sqlx::query!("DELETE FROM desync_dossiers WHERE game > 7")
            .execute(&pool)
            .await
            .unwrap();
        let (status, answer) = server
            .post_to("/api/v1/wfc/desyncs", ANOSCH_HOME, home.encode())
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
    }

    /// A dossier from where no profile is logged in is refused before its
    /// body is read: one too big gets the 403, not the 413 its reading
    /// would. From a login's NAS address it is read.
    #[sqlx::test]
    async fn a_dossier_from_no_login_is_not_read(pool: PgPool) {
        const DESYNCS: &str = "/api/v1/wfc/desyncs";
        const GPCM: [u8; 4] = [12, 75, 123, 129];
        const NAS: [u8; 4] = [12, 75, 115, 108];
        let server = Server::new(pool);
        let anosch = server.log_in(1, "Anosch", GPCM).await;
        let too_big = vec![0u8; dossiers::MAX_BYTES + 1];
        let (status, answer) = server.post_to(DESYNCS, SAM_HOME, too_big.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
        assert_eq!(answer["error"], "profile not logged in from this address");
        let (status, _) = server.post_to(DESYNCS, NAS, too_big.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        server
            .gpcm
            .fake_login_via(anosch, IpAddr::from(GPCM), IpAddr::from(NAS));
        let (status, _) = server.post_to(DESYNCS, NAS, too_big).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        let mut d = dossier(Side::Home);
        d.profile_id = anosch;
        let (status, answer) = server.post_to(DESYNCS, NAS, d.encode()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
    }

    /// Both sides' dossiers of a game, and where they diverge.
    #[sqlx::test]
    async fn the_two_dossiers_of_a_desync_meet(pool: PgPool) {
        const DESYNCS: &str = "/api/v1/wfc/desyncs";
        let server = Server::new(pool.clone());
        let anosch = server.log_in(1, "Anosch", ANOSCH_HOME).await;
        let sam = server.log_in(2, "Sam", SAM_HOME).await;
        let mut body = start();
        body["profile_id"] = json!(anosch);
        server.report(ANOSCH_HOME, &body).await;
        server.report(SAM_HOME, &as_sam(sam)).await;

        let mut home = dossier(Side::Home);
        home.profile_id = anosch;
        home.series = 1_792_000_000;
        let mut away = dossier(Side::Away);
        away.profile_id = sam;
        away.series = 1_792_000_002;
        // The largest dossiers: every field, every machine's inputs.
        let values: Vec<u32> = (1..=512).collect();
        let mut nine = values.clone();
        nine[1] = 9;
        for (d, last) in [(&mut home, &values), (&mut away, &nine)] {
            d.steps = (4797..4812).map(|frame| step(frame, &values)).collect();
            d.steps.push(step(4812, last));
            let netplay = d.netplay.as_mut().unwrap();
            netplay.machines = 4;
            for step in &mut netplay.inputs {
                step.machines.resize(4, machine_input(0));
            }
        }
        assert_eq!(away.encode().len(), dossiers::MAX_BYTES);

        // Only from its login, of a series its profile played, on its side.
        let (status, _) = server.post_to(DESYNCS, SAM_HOME, home.encode()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let mut unknown = home.clone();
        unknown.series += 1;
        let (status, _) = server.post_to(DESYNCS, ANOSCH_HOME, unknown.encode()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let mut other = home.clone();
        other.side = Side::Away;
        let (status, _) = server.post_to(DESYNCS, ANOSCH_HOME, other.encode()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = server
            .post_to(DESYNCS, ANOSCH_HOME, vec![0u8; dossiers::MAX_BYTES + 1])
            .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        let (status, _) = server.post_to(DESYNCS, ANOSCH_HOME, vec![0u8; 200]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Twice: its answer was lost.
        for _ in 0..2 {
            let (status, answer) = server.post_to(DESYNCS, ANOSCH_HOME, home.encode()).await;
            assert_eq!(status, StatusCode::OK);
            assert!(answer["match_id"].is_i64());
        }
        let diverged = sqlx::query_scalar!("SELECT count(*) AS \"count!\" FROM desync_divergences")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(diverged, 0);
        let (status, _) = server.post_to(DESYNCS, SAM_HOME, away.encode()).await;
        assert_eq!(status, StatusCode::OK);

        let kept = sqlx::query_scalar!("SELECT count(*) AS \"count!\" FROM desync_dossiers")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kept, 2);
        let row = sqlx::query!(
            "SELECT kind, frame, field_index, home_caller, away_bytes FROM desync_divergences"
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (
                row.kind.as_str(),
                row.frame,
                row.field_index,
                row.home_caller
            ),
            ("value", Some(4812), Some(1), Some(0x0012_3404))
        );
        assert_eq!(row.away_bytes.unwrap()[..4], 9u32.to_le_bytes());
    }

    /// A profile's dossiers of the last day: the next is refused, and kept
    /// by the game for later.
    #[sqlx::test]
    async fn a_profile_sends_so_many_dossiers_a_day(pool: PgPool) {
        let server = Server::new(pool.clone());
        let anosch = server.log_in(1, "Anosch", ANOSCH_HOME).await;
        let mut body = start();
        body["profile_id"] = json!(anosch);
        server.report(ANOSCH_HOME, &body).await;
        // Two other matches, each with ten dossiers of the profile.
        for _ in 0..2 {
            sqlx::query!(
                "WITH m AS (
                     INSERT INTO online_matches (release, mode, best_of, home_mii, away_mii,
                                                 home_captain, away_captain)
                     VALUES ('R4QE', 'ranked', 3, 'Anosch', 'Sam', 'mario', 'luigi')
                     RETURNING match_id)
                 INSERT INTO desync_dossiers
                     (match_id, side, game, profile_id, release, build, platform, frame,
                      written_at, body)
                 SELECT m.match_id, side, game, $1, 'R4QE', '1.1.6', '', 1, now(), ''
                 FROM m, unnest(ARRAY['home', 'away']) AS side, generate_series(1, 5) AS game",
                anosch,
            )
            .execute(&pool)
            .await
            .unwrap();
        }

        let mut home = dossier(Side::Home);
        home.profile_id = anosch;
        home.series = 1_792_000_000;
        let (status, _) = server
            .post_to("/api/v1/wfc/desyncs", ANOSCH_HOME, home.encode())
            .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        let kept = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM desync_dossiers"#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kept, 20);

        // A day later, it is taken.
        sqlx::query!("UPDATE desync_dossiers SET received_at = now() - interval '25 hours'")
            .execute(&pool)
            .await
            .unwrap();
        let (status, _) = server
            .post_to("/api/v1/wfc/desyncs", ANOSCH_HOME, home.encode())
            .await;
        assert_eq!(status, StatusCode::OK);
    }
}

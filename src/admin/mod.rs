//! `/admin`: the dashboard of the WFC server, behind Google sign-in.
//!
//! Only the Google accounts in `ADMIN_GOOGLE_EMAILS` or `OWNER_GOOGLE_EMAILS`
//! get in ([`google`]).
//! Sessions live in memory ([`session`]). Every page in [`PAGES`] is the same
//! static page; its script shows the section for its path and polls that
//! section's JSON under `/admin/api/` ([`page`]): [`overview`], [`players`],
//! [`matchmaking`], [`matches`], [`monitoring`], [`leaderboards`],
//! [`competitive`], [`database`], [`logs`] and [`maintenance`]. The other sections are
//! planned with the services they need. A section's rows may open a page of
//! their own, its path and an id ([`DETAILS`]). The Competitive page also
//! edits the rule sets matches are played by.
//!
//! Owners, in `OWNER_GOOGLE_EMAILS`, are above admins: they alone see the
//! pages in [`OWNER_PAGES`], whose operations cannot be undone. Those ask
//! twice ([`maintenance::Confirmations`]).

mod competitive;
mod config;
mod database;
mod deletion;
mod google;
mod leaderboards;
mod logs;
mod maintenance;
mod matches;
mod matchmaking;
mod matchup;
mod merge;
mod monitoring;
mod overview;
mod page;
mod players;
#[cfg(test)]
mod preview;
mod session;
mod sets;

/// Reported matches for tests elsewhere, e.g. a player's stats.
#[cfg(test)]
pub(crate) use matches::tests::Games;

use std::{fmt::Display, future::Future, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{delete, get, post, put},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use tracing::{info, warn};

use crate::{
    activity::Activity,
    bans,
    gpcm::Gpcm,
    html::{escape, label_page},
    master::Master,
    pairing::Pairings,
    profiles::rules::{self, Rules, SaveError},
};
pub use config::{Config, GoogleEndpoints, VARIABLES};
use google::{Account, Login, SignInError};
use maintenance::{Confirmations, Operation, Refusal};
use page::PageData;
use session::{SESSION_TTL, Sessions};

/// The dashboard's pages, Overview first. The script in `assets/admin.js`
/// knows the same list.
const PAGES: [&str; 11] = [
    "/admin",
    "/admin/players",
    "/admin/matchmaking",
    "/admin/matches",
    "/admin/monitoring",
    "/admin/leaderboards",
    "/admin/competitive",
    "/admin/moderation",
    "/admin/database",
    "/admin/logs",
    "/admin/maintenance",
];

/// The pages for owners only: anyone else gets a 404.
const OWNER_PAGES: [&str; 1] = ["/admin/maintenance"];

/// The sections whose rows open a page of their own: `/admin/matches/12`.
const DETAILS: [&str; 2] = ["/admin/players", "/admin/matches"];

const SESSION_COOKIE: &str = "wfc_admin";
/// The viewer the pages name for the diagnostics token.
const DIAGNOSTICS_READER: &str = "diagnostics";
const LOGIN_COOKIE: &str = "wfc_admin_login";
/// Time to choose an account on Google's page.
const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);

const LOGIN_PAGE: &str = include_str!("assets/login.html");
const DASHBOARD_PAGE: &str = include_str!("assets/dashboard.html");
const STYLESHEET: &str = include_str!("assets/admin.css");
const SCRIPT: &str = include_str!("assets/admin.js");

/// Scripts and styles only from `/admin`; Google's profile photos.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; \
    style-src 'self'; img-src 'self' data: https://*.googleusercontent.com; \
    connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

struct AdminState {
    config: Config,
    pool: PgPool,
    /// Who is logged in, for the Players page.
    gpcm: Arc<Gpcm>,
    /// The games' sessions and what they did lately, for the Matchmaking
    /// page.
    master: Arc<Master>,
    activity: Arc<Activity>,
    /// The games the profile service keeps for sets, for a set's timeline.
    pairings: Arc<Pairings>,
    /// For Google's token endpoint.
    http: reqwest::Client,
    sessions: Sessions,
    /// The Maintenance page's operations waiting for their second ask.
    confirmations: Confirmations,
    /// `PROFILES_TEST_SIGN_IN` while profiles run, for the Maintenance
    /// page's switch; none without profiles.
    test_sign_in: Option<bool>,
    started_at: OffsetDateTime,
    /// [`LOGIN_PAGE`] and [`DASHBOARD_PAGE`] with the server's label.
    login_page: String,
    dashboard_page: String,
}

pub(crate) fn router(
    pool: PgPool,
    gpcm: Arc<Gpcm>,
    master: Arc<Master>,
    activity: Arc<Activity>,
    pairings: Arc<Pairings>,
    config: Config,
    test_sign_in: Option<bool>,
) -> Router {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("the TLS backend initializes");
    let label = config.label.as_deref();
    let login_page = label_page(LOGIN_PAGE, label);
    let dashboard_page = label_page(DASHBOARD_PAGE, label);
    let state = Arc::new(AdminState {
        config,
        pool,
        gpcm,
        master,
        activity,
        pairings,
        http,
        sessions: Sessions::default(),
        confirmations: Confirmations::default(),
        test_sign_in,
        started_at: OffsetDateTime::now_utc(),
        login_page,
        dashboard_page,
    });
    let mut router = Router::new();
    for path in PAGES {
        router = router.route(path, get(home));
    }
    for path in DETAILS {
        router = router.route(&format!("{path}/{{id}}"), get(home));
    }
    router
        .route("/admin/", get(|| async { Redirect::permanent("/admin") }))
        .route("/admin/login", get(login))
        .route("/admin/callback", get(callback))
        .route("/admin/logout", post(logout))
        .route("/admin/api/overview", get(api_overview))
        .route("/admin/api/players", get(api_players))
        .route("/admin/api/players/{id}", get(api_player))
        .route("/admin/api/matchmaking", get(api_matchmaking))
        .route("/admin/api/matchmaking/profiles/{id}", get(api_matchup))
        .route("/admin/api/matches", get(api_matches))
        .route("/admin/api/matches/{id}", get(api_match))
        .route("/admin/api/monitoring", get(api_monitoring))
        .route("/admin/api/database", get(api_database))
        .route("/admin/api/leaderboards", get(api_leaderboards))
        .route("/admin/api/competitive", get(api_competitive))
        .route("/admin/api/logs", get(api_logs))
        .route("/admin/api/logs/{day}", get(log_file))
        .route("/admin/api/desyncs/{match_id}/{game}", get(dossier_text))
        .route(
            "/admin/api/desyncs/{match_id}/{game}/{side}",
            get(dossier_file),
        )
        .route("/admin/api/sets/{id}", get(api_set))
        .route("/admin/api/maintenance", get(api_maintenance))
        .route(
            "/admin/api/maintenance/wipe-leaderboards",
            post(arm_wipe_leaderboards),
        )
        .route(
            "/admin/api/maintenance/wipe-leaderboards/confirm",
            post(wipe_leaderboards),
        )
        .route(
            "/admin/api/maintenance/test-sign-in",
            put(switch_test_sign_in),
        )
        .route("/admin/api/wfc-profiles/{id}/player", put(link_profile))
        .route("/admin/api/players/merge", post(arm_merge_players))
        .route("/admin/api/players/merge/confirm", post(merge_players))
        .route("/admin/api/players/delete", post(arm_delete_player))
        .route("/admin/api/players/delete/confirm", post(delete_player))
        .route("/admin/api/bans/{id}", delete(lift_ban))
        .route("/admin/api/rules", post(create_rule_set))
        .route(
            "/admin/api/rules/{id}",
            put(update_rule_set).delete(delete_rule_set),
        )
        .route("/admin/admin.css", get(stylesheet))
        .route("/admin/admin.js", get(script))
        .layer(middleware::map_response(security_headers))
        .layer(middleware::from_fn(note_bearer))
        .with_state(state)
}

impl AdminState {
    fn signed_in(&self, jar: &CookieJar) -> Option<Account> {
        self.sessions.get(jar.get(SESSION_COOKIE)?.value())
    }

    /// Who reads a page's data: the signed-in admin, or the diagnostics
    /// token (`Authorization: Bearer`), which reads every page but the
    /// owners' and changes nothing.
    fn reader(&self, jar: &CookieJar, headers: &HeaderMap) -> Option<Account> {
        self.signed_in(jar).or_else(|| {
            let token = headers
                .get(header::AUTHORIZATION)?
                .to_str()
                .ok()?
                .strip_prefix("Bearer ")?;
            self.config
                .is_diagnostics_token(token.trim())
                .then(|| Account {
                    email: DIAGNOSTICS_READER.to_owned(),
                    name: Some("Diagnostics token".to_owned()),
                    picture: None,
                    owner: false,
                })
        })
    }

    /// A cookie for `/admin` only, out of reach of scripts.
    fn cookie(&self, name: &'static str, value: String, max_age: Duration) -> Cookie<'static> {
        Cookie::build((name, value))
            .path("/admin")
            .http_only(true)
            .secure(self.config.secure())
            // Lax: sent on Google's redirect back to the callback.
            .same_site(SameSite::Lax)
            .max_age(time::Duration::seconds(max_age.as_secs() as i64))
            .build()
    }

    /// The sign-in page, for coming back to `return_to`, a [`page`].
    fn login_page(&self, status: StatusCode, message: Option<&str>, return_to: &str) -> Response {
        let message = message
            .map(|message| format!(r#"<p class="notice" role="alert">{}</p>"#, escape(message)))
            .unwrap_or_default();
        // On PUBLIC_URL, where Google returns to, so the sign-in cookie is
        // there too, whichever address shows this page.
        let mut login_url = format!("{}/admin/login", self.config.public_url);
        if return_to != PAGES[0] {
            login_url = format!("{login_url}?next={return_to}");
        }
        let page = self
            .login_page
            .replace("{{message}}", &message)
            .replace("{{login_url}}", &escape(&login_url));
        (status, Html(page)).into_response()
    }

    async fn sign_in(
        &self,
        login: Option<Login>,
        params: CallbackParams,
    ) -> Result<Account, SignInError> {
        if let Some(error) = params.error {
            return Err(SignInError::Google(error));
        }
        let login = login.ok_or(SignInError::NoLogin)?;
        let code = params
            .code
            .filter(|_| params.state.as_deref() == Some(login.state.as_str()))
            .ok_or(SignInError::BadCallback)?;
        let mut account = google::exchange(&self.http, &self.config, &login, &code).await?;
        if !self.config.may_sign_in(&account.email) {
            return Err(SignInError::NotAdmin(account.email));
        }
        account.owner = self.config.is_owner(&account.email);
        Ok(account)
    }

    /// A page's JSON, with its content from `read`, or 401 when signed out.
    async fn page_data<T: Serialize, E: Display>(
        &self,
        jar: &CookieJar,
        headers: &HeaderMap,
        read: impl Future<Output = Result<T, E>>,
    ) -> Response {
        let Some(viewer) = self.reader(jar, headers) else {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "signed out"})),
            )
                .into_response();
        };
        Json(PageData::read(&self.pool, viewer, self.started_at, read).await).into_response()
    }
}

/// `path` if it is one of [`PAGES`] or a page of one of [`DETAILS`]' rows,
/// else the Overview. Redirects only ever go to these.
fn page(path: &str) -> String {
    let detail = DETAILS.into_iter().any(|section| {
        path.strip_prefix(section)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|id| {
                (1..=18).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
            })
    });
    if detail || PAGES.contains(&path) {
        path.to_owned()
    } else {
        PAGES[0].to_owned()
    }
}

/// The dashboard, or the sign-in page. A row's page is there for a number
/// only.
async fn home(State(state): State<Arc<AdminState>>, jar: CookieJar, uri: Uri) -> Response {
    let path = page(uri.path());
    if path != uri.path() {
        return StatusCode::NOT_FOUND.into_response();
    }
    match state.signed_in(&jar) {
        Some(viewer) if !viewer.owner && OWNER_PAGES.contains(&path.as_str()) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Some(_) => Html(state.dashboard_page.clone()).into_response(),
        None => state.login_page(StatusCode::OK, None, &path),
    }
}

/// Where to go once signed in.
#[derive(Deserialize)]
struct LoginParams {
    next: Option<String>,
}

/// Sends the browser to Google, with the sign-in's secrets in a cookie.
async fn login(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    Query(params): Query<LoginParams>,
) -> (CookieJar, Redirect) {
    let login = Login::new(&page(params.next.as_deref().unwrap_or_default()));
    let cookie = state.cookie(LOGIN_COOKIE, login.to_cookie(), LOGIN_TTL);
    (
        jar.add(cookie),
        Redirect::to(login.authorize_url(&state.config).as_str()),
    )
}

/// What Google adds to the callback: `code` and `state`, or `error`.
#[derive(Deserialize)]
struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    Query(params): Query<CallbackParams>,
) -> Response {
    let login = jar
        .get(LOGIN_COOKIE)
        .and_then(|cookie| Login::from_cookie(cookie.value()));
    let return_to = page(login.as_ref().map_or("", |login| login.return_to.as_str()));
    let jar = jar.remove(Cookie::build(LOGIN_COOKIE).path("/admin"));
    match state.sign_in(login, params).await {
        Ok(account) => {
            info!(target: "admin", email = %account.email, owner = account.owner, "signed in");
            let session = state.sessions.open(account);
            let jar = jar.add(state.cookie(SESSION_COOKIE, session, SESSION_TTL));
            (jar, Redirect::to(&return_to)).into_response()
        }
        Err(error) => {
            warn!(target: "admin", %error, "sign-in refused");
            let page = state.login_page(error.status(), Some(&error.message()), &return_to);
            (jar, page).into_response()
        }
    }
}

async fn logout(
    State(state): State<Arc<AdminState>>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Response {
    // SameSite already keeps the cookie off other sites' forms.
    let origin = headers.get(header::ORIGIN);
    if origin.is_some_and(|origin| origin != state.config.public_url.as_str()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(account) = jar
        .get(SESSION_COOKIE)
        .and_then(|cookie| state.sessions.close(cookie.value()))
    {
        info!(target: "admin", email = %account.email, "signed out");
    }
    let jar = jar.remove(Cookie::build(SESSION_COOKIE).path("/admin"));
    (jar, Redirect::to("/admin")).into_response()
}

async fn api_overview(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    state
        .page_data(
            &jar,
            &headers,
            overview::read(&state.pool, &state.gpcm, &state.master),
        )
        .await
}

async fn api_players(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Query(search): Query<players::Search>,
) -> Response {
    state
        .page_data(
            &jar,
            &headers,
            players::read(&state.pool, &state.gpcm, &search),
        )
        .await
}

/// A profile's detail; `null` for none.
async fn api_player(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<i32>,
) -> Response {
    state
        .page_data(
            &jar,
            &headers,
            players::detail(
                &state.pool,
                &state.gpcm,
                &players::Now {
                    master: &state.master,
                    activity: &state.activity,
                    pairings: &state.pairings,
                },
                id,
            ),
        )
        .await
}

async fn api_matchmaking(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    state
        .page_data(
            &jar,
            &headers,
            matchmaking::read(&state.pool, &state.master, &state.activity),
        )
        .await
}

/// A profile's matchmaking now, and with `?with=<profile>` whether each
/// one's ranked search lists the other's game, for diagnosing.
async fn api_matchup(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<i32>,
    Query(with): Query<matchup::With>,
) -> Response {
    state
        .page_data(
            &jar,
            &headers,
            matchup::read(
                &state.pool,
                &state.gpcm,
                &state.master,
                &state.activity,
                &state.pairings,
                id,
                with,
            ),
        )
        .await
}

async fn api_matches(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    state
        .page_data(&jar, &headers, matches::read(&state.pool))
        .await
}

/// A match's detail; `null` for none.
async fn api_match(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    state
        .page_data(&jar, &headers, matches::detail(&state.pool, id))
        .await
}

async fn api_monitoring(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    state
        .page_data(&jar, &headers, monitoring::read(&state.pool, &state.gpcm))
        .await
}

async fn api_database(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    state
        .page_data(&jar, &headers, database::read(&state.pool))
        .await
}

async fn api_leaderboards(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Query(search): Query<players::Search>,
    Query(shown): Query<leaderboards::Shown>,
) -> Response {
    state
        .page_data(
            &jar,
            &headers,
            leaderboards::read(&state.pool, &search, &shown),
        )
        .await
}

async fn api_competitive(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    state
        .page_data(&jar, &headers, competitive::read(&state.pool))
        .await
}

async fn api_logs(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Query(filter): Query<logs::Filter>,
) -> Response {
    let dir = state.config.log_dir.clone();
    state
        .page_data(&jar, &headers, logs::read(dir, filter))
        .await
}

/// A set's live state, for diagnosing; `null` for none.
async fn api_set(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    state
        .page_data(
            &jar,
            &headers,
            sets::timeline(
                &state.pool,
                &state.gpcm,
                &state.master,
                &state.activity,
                &state.pairings,
                id,
            ),
        )
        .await
}

/// A day's whole log file, to save.
async fn log_file(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(day): Path<String>,
) -> Response {
    if state.reader(&jar, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match logs::file(state.config.log_dir.clone(), day).await {
        Ok(Some((name, bytes))) => (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8".to_owned()),
                (
                    header::CONTENT_DISPOSITION,
                    format!(r#"attachment; filename="{name}""#),
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            warn!(target: "admin", %error, "log file unreadable");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// A side's desync dossier as the game sent it, to save.
async fn dossier_file(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((match_id, game, side)): Path<(i64, i32, String)>,
) -> Response {
    if state.reader(&jar, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match monitoring::dossier(&state.pool, match_id, game, &side).await {
        Ok(Some((name, bytes))) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
                (
                    header::CONTENT_DISPOSITION,
                    format!(r#"attachment; filename="{name}""#),
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            warn!(target: "admin", %error, "database error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// A game's desync dossiers as text, to save or hand to a debugger.
async fn dossier_text(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((match_id, game)): Path<(i64, i32)>,
) -> Response {
    if state.reader(&jar, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match monitoring::dossier_text(&state.pool, match_id, game).await {
        Ok(Some((name, text))) => (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8".to_owned()),
                (
                    header::CONTENT_DISPOSITION,
                    format!(r#"attachment; filename="{name}""#),
                ),
            ],
            text,
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            warn!(target: "admin", %error, "database error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// A rule set as the Competitive page sends it.
#[derive(Deserialize)]
struct RuleSetForm {
    name: String,
    rules: Rules,
}

impl AdminState {
    /// Who may change things: a signed-in admin, from this server's own
    /// pages. Otherwise 403, or 401 when signed out.
    fn editor(&self, jar: &CookieJar, headers: &HeaderMap) -> Result<Account, StatusCode> {
        let origin = headers.get(header::ORIGIN);
        if origin.is_none_or(|origin| origin != self.config.public_url.as_str()) {
            return Err(StatusCode::FORBIDDEN);
        }
        self.signed_in(jar).ok_or(StatusCode::UNAUTHORIZED)
    }

    /// Who may run an owner's operation: an [`AdminState::editor`] who is an
    /// owner. Otherwise 403, or 401 when signed out.
    fn owner(&self, jar: &CookieJar, headers: &HeaderMap) -> Result<Account, StatusCode> {
        Some(self.editor(jar, headers)?)
            .filter(|account| account.owner)
            .ok_or(StatusCode::FORBIDDEN)
    }

    async fn save_rule_set(
        &self,
        jar: &CookieJar,
        headers: &HeaderMap,
        id: Option<i64>,
        form: RuleSetForm,
    ) -> Response {
        let editor = match self.editor(jar, headers) {
            Ok(editor) => editor,
            Err(StatusCode::UNAUTHORIZED) => return error(StatusCode::UNAUTHORIZED, "signed out"),
            Err(status) => return status.into_response(),
        };
        match rules::save(&self.pool, id, &form.name, form.rules).await {
            Ok(Ok(saved)) => {
                info!(target: "admin", email = %editor.email, rule_set = saved.name, "rule set saved");
                Json(saved).into_response()
            }
            Ok(Err(SaveError::Invalid(problem))) => error(StatusCode::BAD_REQUEST, &problem),
            Ok(Err(SaveError::NameTaken)) => {
                error(StatusCode::CONFLICT, "another rule set has this name")
            }
            Ok(Err(SaveError::NotFound)) => error(StatusCode::NOT_FOUND, "no such rule set"),
            Err(failure) => {
                warn!(target: "admin", error = %failure, "database error");
                error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed")
            }
        }
    }
}

async fn create_rule_set(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(form): Json<RuleSetForm>,
) -> Response {
    state.save_rule_set(&jar, &headers, None, form).await
}

async fn update_rule_set(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(form): Json<RuleSetForm>,
) -> Response {
    state.save_rule_set(&jar, &headers, Some(id), form).await
}

/// Matches made with the rule set keep their copy of it.
async fn delete_rule_set(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let editor = match state.editor(&jar, &headers) {
        Ok(editor) => editor,
        Err(status) => return status.into_response(),
    };
    match rules::delete(&state.pool, id).await {
        Ok(true) => {
            info!(target: "admin", email = %editor.email, id, "rule set deleted");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            warn!(target: "admin", %error, "database error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// The Maintenance page, for owners only.
async fn api_maintenance(State(state): State<Arc<AdminState>>, jar: CookieJar) -> Response {
    if state.signed_in(&jar).is_some_and(|viewer| !viewer.owner) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // An owner's sign-in only: not the diagnostics token.
    state
        .page_data(
            &jar,
            &HeaderMap::new(),
            maintenance::read(&state.pool, &state.gpcm, state.test_sign_in),
        )
        .await
}

/// The first ask: arms the wipe and answers with what it would delete, the
/// phrase to type and the token for the second ask.
async fn arm_wipe_leaderboards(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    let owner = match state.owner(&jar, &headers) {
        Ok(owner) => owner,
        Err(status) => return status.into_response(),
    };
    let leaderboards = match maintenance::leaderboards(&state.pool, &state.gpcm).await {
        Ok(leaderboards) => leaderboards,
        Err(failure) => {
            warn!(target: "admin", error = %failure, "database error");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed");
        }
    };
    let asked = state
        .confirmations
        .arm(Operation::WipeLeaderboards, &owner.email, leaderboards);
    info!(target: "admin", email = %owner.email, "leaderboard wipe asked");
    Json(asked).into_response()
}

/// The second ask: the first one's token and the phrase typed out.
#[derive(Deserialize)]
struct Confirmation {
    token: String,
    phrase: String,
}

async fn wipe_leaderboards(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(confirmation): Json<Confirmation>,
) -> Response {
    let owner = match state.owner(&jar, &headers) {
        Ok(owner) => owner,
        Err(status) => return status.into_response(),
    };
    let operation = Operation::WipeLeaderboards;
    let confirmed = state.confirmations.confirm(
        operation,
        &owner.email,
        &confirmation.token,
        &confirmation.phrase,
    );
    if let Err(refusal) = confirmed {
        warn!(target: "admin", email = %owner.email, ?refusal, "leaderboard wipe refused");
        return refused(operation, refusal);
    }
    match maintenance::wipe_leaderboards(&state.pool).await {
        Ok(wiped) => {
            warn!(target: "admin", email = %owner.email, ?wiped, "leaderboards wiped");
            Json(wiped).into_response()
        }
        Err(failure) => {
            warn!(target: "admin", error = %failure, "database error");
            error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed")
        }
    }
}

/// A WFC profile's player, set by hand: a player id, or `null` for none; or
/// another WFC profile (`profile_id`) to play for one player with.
#[derive(Deserialize)]
struct LinkForm {
    #[serde(default)]
    player_id: Option<i64>,
    #[serde(default)]
    profile_id: Option<i32>,
}

async fn link_profile(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(profile_id): Path<i32>,
    Json(form): Json<LinkForm>,
) -> Response {
    let owner = match state.owner(&jar, &headers) {
        Ok(owner) => owner,
        Err(status) => return status.into_response(),
    };
    if let Some(other) = form.profile_id {
        return match merge::pair(&state.pool, profile_id, other).await {
            Ok(Ok(paired)) => {
                info!(target: "admin", email = %owner.email, profile_id, other, player_id = paired.player_id,
                      created = paired.created, "WFC profiles linked by hand");
                Json(paired).into_response()
            }
            Ok(Err(refusal)) => error(StatusCode::CONFLICT, refusal),
            Err(failure) => {
                warn!(target: "admin", error = %failure, "database error");
                error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed")
            }
        };
    }
    match merge::link(&state.pool, profile_id, form.player_id).await {
        Ok(Ok(())) => {
            info!(target: "admin", email = %owner.email, profile_id, player_id = ?form.player_id,
                  "WFC profile linked by hand");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(refusal)) => error(StatusCode::NOT_FOUND, refusal),
        Err(failure) => {
            warn!(target: "admin", error = %failure, "database error");
            error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed")
        }
    }
}

/// A merge's players: `from` goes into `into`.
#[derive(Deserialize)]
struct MergeForm {
    from: i64,
    into: i64,
}

/// The first ask: what the merge would move, the phrase to type and the
/// token for the second ask; or why it can't be.
async fn arm_merge_players(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(form): Json<MergeForm>,
) -> Response {
    let owner = match state.owner(&jar, &headers) {
        Ok(owner) => owner,
        Err(status) => return status.into_response(),
    };
    match merge::plan(&state.pool, form.from, form.into).await {
        Ok(Ok(plan)) => {
            let operation = Operation::MergePlayers {
                from: form.from,
                into: form.into,
            };
            Json(state.confirmations.arm(operation, &owner.email, plan)).into_response()
        }
        Ok(Err(refusal)) => error(StatusCode::CONFLICT, refusal),
        Err(failure) => {
            warn!(target: "admin", error = %failure, "database error");
            error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed")
        }
    }
}

/// The second ask: the players again, the first ask's token and the phrase.
#[derive(Deserialize)]
struct MergeConfirmation {
    from: i64,
    into: i64,
    token: String,
    phrase: String,
}

async fn merge_players(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(confirmation): Json<MergeConfirmation>,
) -> Response {
    let owner = match state.owner(&jar, &headers) {
        Ok(owner) => owner,
        Err(status) => return status.into_response(),
    };
    let operation = Operation::MergePlayers {
        from: confirmation.from,
        into: confirmation.into,
    };
    if let Err(refusal) = state.confirmations.confirm(
        operation,
        &owner.email,
        &confirmation.token,
        &confirmation.phrase,
    ) {
        return refused(operation, refusal);
    }
    match merge::merge(&state.pool, confirmation.from, confirmation.into).await {
        Ok(Ok(merged)) => {
            warn!(target: "admin", email = %owner.email, from = confirmation.from,
                  into = confirmation.into, "players merged");
            Json(merged).into_response()
        }
        Ok(Err(refusal)) => error(StatusCode::CONFLICT, refusal),
        Err(failure) => {
            warn!(target: "admin", error = %failure, "database error");
            error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed")
        }
    }
}

/// A delete's WFC profile: the player it plays for goes.
#[derive(Deserialize)]
struct DeleteForm {
    profile_id: i32,
}

/// The first ask: what the delete would remove, the phrase to type and the
/// token for the second ask; or why it can't be.
async fn arm_delete_player(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(form): Json<DeleteForm>,
) -> Response {
    // Any admin, not only an owner: a player on the boards who must go
    // shouldn't wait for one.
    let admin = match state.editor(&jar, &headers) {
        Ok(admin) => admin,
        Err(status) => return status.into_response(),
    };
    match deletion::plan(&state.pool, &state.gpcm, form.profile_id).await {
        Ok(Ok(plan)) => {
            let operation = Operation::DeletePlayer {
                profile_id: form.profile_id,
            };
            info!(target: "admin", email = %admin.email, profile_id = form.profile_id,
                  "player deletion asked");
            Json(state.confirmations.arm(operation, &admin.email, plan)).into_response()
        }
        Ok(Err(refusal)) => error(StatusCode::CONFLICT, refusal),
        Err(failure) => {
            warn!(target: "admin", error = %failure, "database error");
            error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed")
        }
    }
}

/// The second ask: the profile again, the first ask's token and the phrase.
#[derive(Deserialize)]
struct DeleteConfirmation {
    profile_id: i32,
    token: String,
    phrase: String,
}

async fn delete_player(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(confirmation): Json<DeleteConfirmation>,
) -> Response {
    // Any admin, not only an owner: a player on the boards who must go
    // shouldn't wait for one.
    let admin = match state.editor(&jar, &headers) {
        Ok(admin) => admin,
        Err(status) => return status.into_response(),
    };
    let operation = Operation::DeletePlayer {
        profile_id: confirmation.profile_id,
    };
    if let Err(refusal) = state.confirmations.confirm(
        operation,
        &admin.email,
        &confirmation.token,
        &confirmation.phrase,
    ) {
        return refused(operation, refusal);
    }
    match deletion::delete(
        &state.pool,
        &state.gpcm,
        confirmation.profile_id,
        &admin.email,
    )
    .await
    {
        Ok(Ok(deleted)) => {
            warn!(target: "admin", email = %admin.email, profile_id = confirmation.profile_id,
                  player_id = ?deleted.player.as_ref().map(|player| player.player_id),
                  profiles = deleted.profiles.len(), scores = deleted.scores,
                  past_scores = deleted.past_scores, online_matches = deleted.online_matches,
                  sets = deleted.sets, ban_id = ?deleted.ban_id, consoles = deleted.consoles,
                  identities = deleted.identities, "player deleted and banned");
            Json(deleted).into_response()
        }
        Ok(Err(refusal)) => error(StatusCode::CONFLICT, refusal),
        Err(failure) => {
            warn!(target: "admin", error = %failure, "database error");
            error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed")
        }
    }
}

/// Lifts a ban (any admin, as a delete bans): its consoles log in again, and
/// its Discord accounts make a new player.
async fn lift_ban(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(ban_id): Path<i64>,
) -> Response {
    let admin = match state.editor(&jar, &headers) {
        Ok(admin) => admin,
        Err(status) => return status.into_response(),
    };
    match bans::lift(&state.pool, ban_id).await {
        Ok(Some(ban)) => {
            warn!(target: "admin", email = %admin.email, ban_id, name = %ban.name,
                  banned_by = %ban.banned_by, "ban lifted");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "no such ban"),
        Err(failure) => {
            warn!(target: "admin", error = %failure, "database error");
            error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed")
        }
    }
}

/// The answer to a second ask that was refused.
fn refused(operation: Operation, refusal: Refusal) -> Response {
    match refusal {
        Refusal::Expired => error(
            StatusCode::CONFLICT,
            "this confirmation expired: start again",
        ),
        Refusal::Phrase => error(
            StatusCode::BAD_REQUEST,
            &format!("type \"{}\" to confirm: start again", operation.phrase()),
        ),
    }
}

/// The Maintenance page's switch for test sign-in.
#[derive(Deserialize)]
struct SwitchForm {
    enabled: bool,
}

/// Switches test sign-in on or off: it stays so across restarts.
async fn switch_test_sign_in(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(form): Json<SwitchForm>,
) -> Response {
    let owner = match state.owner(&jar, &headers) {
        Ok(owner) => owner,
        Err(status) => return status.into_response(),
    };
    let Some(default) = state.test_sign_in else {
        return error(StatusCode::NOT_FOUND, "this server has no profiles");
    };
    let switched = crate::switches::set(
        &state.pool,
        crate::switches::TEST_SIGN_IN,
        form.enabled,
        &owner.email,
    )
    .await;
    if let Err(failure) = switched {
        warn!(target: "admin", error = %failure, "database error");
        return error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed");
    }
    if form.enabled {
        warn!(target: "admin", email = %owner.email, "test sign-in switched on");
    } else {
        info!(target: "admin", email = %owner.email, "test sign-in switched off");
    }
    match maintenance::test_sign_in(&state.pool, default).await {
        Ok(test_sign_in) => Json(test_sign_in).into_response(),
        Err(failure) => {
            warn!(target: "admin", error = %failure, "database error");
            error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed")
        }
    }
}

/// A JSON error the dashboard's script shows.
fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLESHEET,
    )
}

async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        SCRIPT,
    )
}

/// Every `/admin` response: never cached, never framed, and only this
/// server's own scripts.
/// A request with a bearer token (the diagnostics token, or a wrong one)
/// goes to the log with its path.
async fn note_bearer(request: Request, next: Next) -> Response {
    if request.headers().contains_key(header::AUTHORIZATION) {
        info!(target: "admin", method = %request.method(), path = %request.uri().path(), "bearer token");
    }
    next.run(request).await
}

async fn security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    for (name, value) in [
        (header::CACHE_CONTROL, "no-store"),
        (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

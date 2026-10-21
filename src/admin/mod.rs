//! `/admin`: the dashboard of the WFC server, behind Google sign-in.
//!
//! Only the Google accounts in `ADMIN_GOOGLE_EMAILS` get in ([`google`]).
//! Sessions live in memory ([`session`]). Every page in [`PAGES`] is the same
//! static page; its script shows the section for its path and polls that
//! section's JSON under `/admin/api/` ([`page`]): [`overview`], [`players`],
//! [`matchmaking`], [`leaderboards`] and [`database`]. The other sections are
//! planned with the services they need.

mod config;
mod database;
mod google;
mod leaderboards;
mod matchmaking;
mod overview;
mod page;
mod players;
mod session;

use std::{future::Future, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use tracing::{info, warn};

use crate::{activity::Activity, gpcm::Gpcm, master::Master};
pub use config::{Config, GoogleEndpoints, VARIABLES};
use google::{Account, Login, SignInError};
use page::PageData;
use session::{SESSION_TTL, Sessions};

/// The dashboard's pages, Overview first. The script in `assets/admin.js`
/// knows the same list.
const PAGES: [&str; 6] = [
    "/admin",
    "/admin/players",
    "/admin/matchmaking",
    "/admin/leaderboards",
    "/admin/moderation",
    "/admin/database",
];

const SESSION_COOKIE: &str = "wfc_admin";
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
    /// For Google's token endpoint.
    http: reqwest::Client,
    sessions: Sessions,
    started_at: OffsetDateTime,
}

pub(crate) fn router(
    pool: PgPool,
    gpcm: Arc<Gpcm>,
    master: Arc<Master>,
    activity: Arc<Activity>,
    config: Config,
) -> Router {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("the TLS backend initializes");
    let state = Arc::new(AdminState {
        config,
        pool,
        gpcm,
        master,
        activity,
        http,
        sessions: Sessions::default(),
        started_at: OffsetDateTime::now_utc(),
    });
    let mut router = Router::new();
    for path in PAGES {
        router = router.route(path, get(home));
    }
    router
        .route("/admin/", get(|| async { Redirect::permanent("/admin") }))
        .route("/admin/login", get(login))
        .route("/admin/callback", get(callback))
        .route("/admin/logout", post(logout))
        .route("/admin/api/overview", get(api_overview))
        .route("/admin/api/players", get(api_players))
        .route("/admin/api/matchmaking", get(api_matchmaking))
        .route("/admin/api/database", get(api_database))
        .route("/admin/api/leaderboards", get(api_leaderboards))
        .route("/admin/admin.css", get(stylesheet))
        .route("/admin/admin.js", get(script))
        .layer(middleware::map_response(security_headers))
        .with_state(state)
}

impl AdminState {
    fn signed_in(&self, jar: &CookieJar) -> Option<Account> {
        self.sessions.get(jar.get(SESSION_COOKIE)?.value())
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

    /// The sign-in page, for coming back to `return_to`, one of [`PAGES`].
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
        let page = LOGIN_PAGE
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
        let account = google::exchange(&self.http, &self.config, &login, &code).await?;
        if !self.config.admins.contains(&account.email) {
            return Err(SignInError::NotAdmin(account.email));
        }
        Ok(account)
    }

    /// A page's JSON, with its content from `read`, or 401 when signed out.
    async fn page_data<T: Serialize>(
        &self,
        jar: &CookieJar,
        read: impl Future<Output = Result<T, sqlx::Error>>,
    ) -> Response {
        let Some(viewer) = self.signed_in(jar) else {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "signed out"})),
            )
                .into_response();
        };
        Json(PageData::read(&self.pool, viewer, self.started_at, read).await).into_response()
    }
}

/// `path` if it is one of [`PAGES`], else the Overview. Redirects only ever
/// go to these.
fn page(path: &str) -> &'static str {
    PAGES
        .into_iter()
        .find(|page| *page == path)
        .unwrap_or(PAGES[0])
}

/// The dashboard, or the sign-in page.
async fn home(State(state): State<Arc<AdminState>>, jar: CookieJar, uri: Uri) -> Response {
    match state.signed_in(&jar) {
        Some(_) => Html(DASHBOARD_PAGE).into_response(),
        None => state.login_page(StatusCode::OK, None, page(uri.path())),
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
    let login = Login::new(page(params.next.as_deref().unwrap_or_default()));
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
            info!(target: "admin", email = %account.email, "signed in");
            let session = state.sessions.open(account);
            let jar = jar.add(state.cookie(SESSION_COOKIE, session, SESSION_TTL));
            (jar, Redirect::to(return_to)).into_response()
        }
        Err(error) => {
            warn!(target: "admin", %error, "sign-in refused");
            let page = state.login_page(error.status(), Some(&error.message()), return_to);
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

async fn api_overview(State(state): State<Arc<AdminState>>, jar: CookieJar) -> Response {
    state
        .page_data(
            &jar,
            overview::read(&state.pool, &state.gpcm, &state.master),
        )
        .await
}

async fn api_players(State(state): State<Arc<AdminState>>, jar: CookieJar) -> Response {
    state
        .page_data(&jar, players::read(&state.pool, &state.gpcm))
        .await
}

async fn api_matchmaking(State(state): State<Arc<AdminState>>, jar: CookieJar) -> Response {
    state
        .page_data(
            &jar,
            matchmaking::read(&state.pool, &state.master, &state.activity),
        )
        .await
}

async fn api_database(State(state): State<Arc<AdminState>>, jar: CookieJar) -> Response {
    state.page_data(&jar, database::read(&state.pool)).await
}

async fn api_leaderboards(State(state): State<Arc<AdminState>>, jar: CookieJar) -> Response {
    state.page_data(&jar, leaderboards::read(&state.pool)).await
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

/// Escapes text for HTML content and quoted attributes.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_neutralizes_markup() {
        assert_eq!(
            escape(r#"<a href="x">'&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;"
        );
    }
}

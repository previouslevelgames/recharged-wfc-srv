//! `/admin`: a dashboard of the server and its database, behind Google
//! sign-in.
//!
//! Only the Google accounts in `ADMIN_GOOGLE_EMAILS` get in ([`google`]).
//! Sessions live in memory ([`session`]). The dashboard is a static page
//! that polls `/admin/api/overview` ([`overview`]) as JSON.

mod config;
mod google;
mod overview;
mod session;

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::Deserialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use tracing::{info, warn};

pub use config::{Config, GoogleEndpoints, VARIABLES};
use google::{Account, Login, SignInError};
use session::{SESSION_TTL, Sessions};

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
    /// For Google's token endpoint.
    http: reqwest::Client,
    sessions: Sessions,
    started_at: OffsetDateTime,
}

pub fn router(pool: PgPool, config: Config) -> Router {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("the TLS backend initializes");
    let state = Arc::new(AdminState {
        config,
        pool,
        http,
        sessions: Sessions::default(),
        started_at: OffsetDateTime::now_utc(),
    });
    Router::new()
        .route("/admin", get(home))
        .route("/admin/", get(|| async { Redirect::permanent("/admin") }))
        .route("/admin/login", get(login))
        .route("/admin/callback", get(callback))
        .route("/admin/logout", post(logout))
        .route("/admin/api/overview", get(api_overview))
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

    fn login_page(&self, status: StatusCode, message: Option<&str>) -> Response {
        let message = message
            .map(|message| format!(r#"<p class="notice" role="alert">{}</p>"#, escape(message)))
            .unwrap_or_default();
        // On PUBLIC_URL, where Google returns to, so the sign-in cookie is
        // there too, whichever address shows this page.
        let login_url = format!("{}/admin/login", self.config.public_url);
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
}

/// The dashboard, or the sign-in page.
async fn home(State(state): State<Arc<AdminState>>, jar: CookieJar) -> Response {
    match state.signed_in(&jar) {
        Some(_) => Html(DASHBOARD_PAGE).into_response(),
        None => state.login_page(StatusCode::OK, None),
    }
}

/// Sends the browser to Google, with the sign-in's secrets in a cookie.
async fn login(State(state): State<Arc<AdminState>>, jar: CookieJar) -> (CookieJar, Redirect) {
    let login = Login::new();
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
    let jar = jar.remove(Cookie::build(LOGIN_COOKIE).path("/admin"));
    match state.sign_in(login, params).await {
        Ok(account) => {
            info!(target: "admin", email = %account.email, "signed in");
            let session = state.sessions.open(account);
            let jar = jar.add(state.cookie(SESSION_COOKIE, session, SESSION_TTL));
            (jar, Redirect::to("/admin")).into_response()
        }
        Err(error) => {
            warn!(target: "admin", %error, "sign-in refused");
            let page = state.login_page(error.status(), Some(&error.message()));
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
    let Some(viewer) = state.signed_in(&jar) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "signed out"})),
        )
            .into_response();
    };
    Json(overview::collect(&state.pool, viewer, state.started_at).await).into_response()
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

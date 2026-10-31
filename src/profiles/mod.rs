//! Recharged profiles: players who sign in from the launcher, for competitive
//! play (docs/profiles.md). It is no part of WFC: the game's own logins stay
//! NAS and GPCM's.
//!
//! The launcher asks for a sign-in ([`sign_in`]), opens its page in the
//! browser and asks again until the session arrives. The page shows the
//! launcher's code and goes on to Discord ([`discord`]), or, on test servers,
//! takes a name. Players, their accounts and sessions are in the database
//! ([`players`]); the launcher sends its session as a bearer token.
//!
//! Signed in, a player challenges another to a test match ([`matches`]) by
//! a rule set the server's admins keep ([`rules`]). The port proves which
//! WFC profile its game is logged in as, says when the game is ready, and
//! reports the games it played ([`play`]); two ready games find only each
//! other in RANKED ([`crate::pairing`]).

mod chat;
mod config;
mod discord;
#[cfg(test)]
mod flow_tests;
mod friends;
mod matches;
mod play;
mod players;
pub(crate) mod rules;
mod sign_in;
mod social;

use std::{sync::Arc, time::Duration};

use axum::{
    Form, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use tracing::{info, warn};

use crate::{
    gpcm::Gpcm,
    html::{escape, label_page},
    pairing::Pairings,
};

pub use config::{Config, DiscordClient, DiscordEndpoints, VARIABLES};
use discord::SignInError;
use players::{Identity, Player};
use sign_in::{Outcome, Poll, SIGN_IN_TTL, SignIns};

/// How often the launcher should ask whether a sign-in has ended, in seconds.
const POLL_INTERVAL: u64 = 2;
/// Test names: short, and safe to show anywhere.
const MAX_TEST_NAME: usize = 16;

const PAGE: &str = include_str!("assets/page.html");
const STYLESHEET: &str = include_str!("assets/sign-in.css");

/// The pages' scripts and styles only from here.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'self'; \
    img-src 'self' data: https://cdn.discordapp.com; form-action 'self'; \
    frame-ancestors 'none'; base-uri 'none'";

struct ProfilesState {
    config: Config,
    pool: PgPool,
    /// For Discord's token and user endpoints.
    http: reqwest::Client,
    sign_ins: SignIns,
    /// Who is logged in to WFC with which login ticket.
    gpcm: Arc<Gpcm>,
    /// Matches' games kept for RANKED, which the server browser reads.
    pairings: Arc<Pairings>,
    /// [`PAGE`] with the server's label.
    page: String,
}

pub(crate) fn router(
    pool: PgPool,
    gpcm: Arc<Gpcm>,
    pairings: Arc<Pairings>,
    config: Config,
) -> Router {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("the TLS backend initializes");
    let discord = config.discord.is_some();
    let page = label_page(PAGE, config.label.as_deref());
    let state = Arc::new(ProfilesState {
        config,
        pool,
        http,
        sign_ins: SignIns::default(),
        gpcm,
        pairings,
        page,
    });
    let mut router = Router::new()
        .route("/api/v1/server", get(server))
        .route("/api/v1/sign-in", post(start_sign_in))
        .route("/api/v1/sign-in/test", post(api_test_sign_in))
        .route("/api/v1/sign-in/{id}", get(poll_sign_in))
        .route("/api/v1/sign-out", post(sign_out))
        .route("/api/v1/me", get(me))
        .route("/api/v1/rules", get(play::rule_sets))
        .route("/api/v1/wfc", post(play::bind))
        .route("/api/v1/matches", post(play::challenge))
        .route("/api/v1/matches/current", get(play::current))
        .route("/api/v1/matches/{id}/accept", post(play::accept))
        .route("/api/v1/matches/{id}/decline", post(play::decline))
        .route("/api/v1/matches/{id}/cancel", post(play::cancel))
        .route("/api/v1/matches/{id}/ready", post(play::ready))
        .route(
            "/api/v1/matches/{id}/ready-to-play",
            post(play::ready_to_play),
        )
        .route("/api/v1/matches/{id}/rematch", post(play::rematch))
        .route("/api/v1/players", get(play::players))
        .route("/api/v1/players/discord", post(social::players_by_discord))
        .route("/api/v1/discord/token", post(social::discord_token))
        .route("/api/v1/friends", post(play::add_friend))
        .route("/api/v1/friends/{id}", delete(play::remove_friend))
        .route(
            "/api/v1/chat/{id}",
            get(play::chat_messages).post(play::send_chat),
        )
        .route("/api/v1/matches/{id}/report", post(play::report))
        .route("/sign-in", get(sign_in_page))
        .route("/sign-in/test", post(page_test_sign_in))
        .route("/sign-in/sign-in.css", get(stylesheet));
    if discord {
        router = router
            .route("/sign-in/discord", get(to_discord))
            .route("/sign-in/discord/callback", get(discord_callback));
    }
    router
        .layer(middleware::map_response(security_headers))
        .with_state(state)
}

/// A JSON error: `{"error": message}`.
fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn database_error(error: sqlx::Error) -> Response {
    warn!(target: "profiles", %error, "database error");
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "the server's database failed",
    )
}

impl ProfilesState {
    /// The player of the request's bearer token, or the 401 to answer. The
    /// request is the player's heartbeat: their Recharged is open, doing what
    /// its `Recharged-Activity` header says.
    async fn signed_in(&self, headers: &HeaderMap) -> Result<Player, Response> {
        let unauthorized = || json_error(StatusCode::UNAUTHORIZED, "signed out");
        let token = bearer(headers).ok_or_else(unauthorized)?;
        let player_id = players::session(&self.pool, token)
            .await
            .map_err(database_error)?
            .ok_or_else(unauthorized)?;
        let activity = players::Activity::from_header(
            headers
                .get("recharged-activity")
                .and_then(|value| value.to_str().ok()),
        );
        players::heartbeat(&self.pool, player_id, activity)
            .await
            .map_err(database_error)?;
        players::player(&self.pool, player_id)
            .await
            .map_err(database_error)?
            .ok_or_else(unauthorized)
    }

    /// Signs in with `identity` and opens a session. The player and its
    /// token.
    async fn open(&self, identity: &Identity<'_>) -> Result<(i64, String), sqlx::Error> {
        let player_id = players::sign_in(&self.pool, identity).await?;
        let token = players::open_session(&self.pool, player_id).await?;
        info!(target: "profiles", player_id, provider = identity.provider, "signed in");
        Ok((player_id, token))
    }

    /// Ends the sign-in `id` with a session for `identity`.
    async fn finish(&self, id: &str, identity: &Identity<'_>) -> Result<bool, sqlx::Error> {
        let (player_id, token) = self.open(identity).await?;
        if self.sign_ins.finish(
            id,
            Outcome::SignedIn {
                token: token.clone(),
                player_id,
            },
        ) {
            return Ok(true);
        }
        // It expired meanwhile: nobody will pick the session up.
        players::close_session(&self.pool, &token).await?;
        Ok(false)
    }

    /// The Discord callback: the sign-in it ends and the account, or why
    /// not. A sign-in found but refused is ended with the reason, which the
    /// launcher then shows too.
    async fn discord_account(
        &self,
        params: CallbackParams,
    ) -> Result<(String, discord::Account), SignInError> {
        let (id, authorization) = params
            .state
            .as_deref()
            .and_then(|state| self.sign_ins.take_authorization(state))
            .ok_or(SignInError::Expired)?;
        let refuse = |error: SignInError| {
            self.sign_ins.finish(&id, Outcome::Refused(error.message()));
            error
        };
        if let Some(error) = params.error {
            return Err(refuse(SignInError::Discord(error)));
        }
        let code = params
            .code
            .ok_or(SignInError::BadCallback)
            .map_err(refuse)?;
        let discord = self
            .config
            .discord
            .as_ref()
            .expect("the route needs Discord");
        match discord::exchange(&self.http, &self.config, discord, &authorization, &code).await {
            Ok(account) => Ok((id, account)),
            Err(error) => Err(refuse(error)),
        }
    }

    /// A page on the sign-in's design, with `content` (HTML) under `title`.
    fn page(&self, status: StatusCode, title: &str, content: &str) -> Response {
        let page = self
            .page
            .replace("{{title}}", &escape(title))
            .replace("{{content}}", content);
        (status, Html(page)).into_response()
    }

    fn notice_page(&self, status: StatusCode, message: &str) -> Response {
        let content = format!(r#"<p class="notice" role="alert">{}</p>"#, escape(message));
        self.page(status, "Signing in", &content)
    }

    fn signed_in_page(&self, name: &str) -> Response {
        let content = format!(
            "<p>Signed in as <b>{}</b>.</p><p>You can close this page and go back to Recharged.</p>",
            escape(name)
        );
        self.page(StatusCode::OK, "Signed in", &content)
    }
}

/// The token of an `Authorization: Bearer` header.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// What the launcher asks first: whether this server has profiles, and how
/// players sign in to it.
async fn server(State(state): State<Arc<ProfilesState>>) -> Response {
    Json(json!({
        "version": crate::VERSION,
        "revision": crate::REVISION,
        "sign_in": {
            "discord": state.config.discord.is_some(),
            "test": state.config.test_sign_in,
        },
        // The launcher's Discord Social SDK signs in to this application
        // (docs/profiles.md, "Discord friends").
        "discord_application": state.config.discord.as_ref().map(|discord| &discord.client_id),
    }))
    .into_response()
}

/// A new sign-in: the page for the browser and the code it shows.
async fn start_sign_in(State(state): State<Arc<ProfilesState>>) -> Response {
    let Some((id, code)) = state.sign_ins.start() else {
        warn!(target: "profiles", "too many sign-ins waiting");
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "too many sign-ins at once; try again in a minute",
        );
    };
    Json(json!({
        "id": id,
        "code": sign_in::display_code(&code),
        "url": format!("{}/sign-in?id={id}", state.config.public_url),
        "expires_in": SIGN_IN_TTL.as_secs(),
        "poll_interval": POLL_INTERVAL,
    }))
    .into_response()
}

/// Whether the sign-in `id` has ended: the session, once.
async fn poll_sign_in(State(state): State<Arc<ProfilesState>>, Path(id): Path<String>) -> Response {
    match state.sign_ins.poll(&id) {
        Poll::Waiting => Json(json!({ "status": "waiting" })).into_response(),
        Poll::Ended(Outcome::SignedIn { token, player_id }) => {
            match players::player(&state.pool, player_id).await {
                Ok(Some(player)) => signed_in(&token, &player),
                Ok(None) => json_error(StatusCode::NOT_FOUND, "unknown sign-in"),
                Err(error) => database_error(error),
            }
        }
        Poll::Ended(Outcome::Refused(message)) => {
            Json(json!({ "status": "refused", "message": message })).into_response()
        }
        Poll::Unknown => json_error(StatusCode::NOT_FOUND, "unknown sign-in"),
    }
}

fn signed_in(token: &str, player: &Player) -> Response {
    Json(json!({ "status": "signed_in", "token": token, "player": player })).into_response()
}

#[derive(Deserialize)]
struct TestSignIn {
    name: String,
}

/// Test sign-in without a browser, for scripts and a second game on one PC.
async fn api_test_sign_in(
    State(state): State<Arc<ProfilesState>>,
    Json(request): Json<TestSignIn>,
) -> Response {
    if !state.config.test_sign_in {
        return json_error(StatusCode::FORBIDDEN, "this server has no test sign-in");
    }
    let Some(name) = test_name(&request.name) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "a test name has 1 to 16 letters, digits, - or _",
        );
    };
    let external_id = name.to_lowercase();
    match state.open(&test_identity(&name, &external_id)).await {
        Ok((player_id, token)) => match players::player(&state.pool, player_id).await {
            Ok(Some(player)) => signed_in(&token, &player),
            Ok(None) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "the player went away"),
            Err(error) => database_error(error),
        },
        Err(error) => database_error(error),
    }
}

async fn sign_out(State(state): State<Arc<ProfilesState>>, headers: HeaderMap) -> Response {
    let Some(token) = bearer(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "signed out");
    };
    match players::close_session(&state.pool, token).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => json_error(StatusCode::UNAUTHORIZED, "signed out"),
        Err(error) => database_error(error),
    }
}

/// The player, with the WFC profiles proven theirs and their Discord
/// account id.
async fn me(State(state): State<Arc<ProfilesState>>, headers: HeaderMap) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    let discord_id = match players::discord_id(&state.pool, player.id).await {
        Ok(discord_id) => discord_id,
        Err(error) => return database_error(error),
    };
    match players::wfc_profiles(&state.pool, player.id).await {
        Ok(wfc) => {
            let mut me = serde_json::to_value(&player).expect("a player serializes");
            me["wfc"] = json!(wfc);
            me["discord_id"] = json!(discord_id);
            Json(me).into_response()
        }
        Err(error) => database_error(error),
    }
}

#[derive(Deserialize)]
struct SignInParams {
    id: Option<String>,
}

/// The page the launcher opens: its code, then Discord or a test name.
async fn sign_in_page(
    State(state): State<Arc<ProfilesState>>,
    Query(params): Query<SignInParams>,
) -> Response {
    let id = params.id.unwrap_or_default();
    let Some(code) = state.sign_ins.code(&id) else {
        return state.notice_page(
            StatusCode::NOT_FOUND,
            "This sign-in expired or has ended. Start again in Recharged's launcher.",
        );
    };
    let mut content = format!(
        r#"<p>Signing in to Recharged's launcher. Check that the launcher shows this code:</p>
<p class="code">{}</p>
<p class="hint">If it doesn't, close this page: someone else started this sign-in.</p>"#,
        sign_in::display_code(&code)
    );
    if state.config.discord.is_some() {
        content += &format!(
            r#"<p><a class="button discord" href="/sign-in/discord?id={}">Continue with Discord</a></p>"#,
            escape(&id)
        );
    }
    if state.config.test_sign_in {
        content += &format!(
            r#"<form method="post" action="/sign-in/test">
<input type="hidden" name="id" value="{}">
<label>Test player <input name="name" maxlength="{MAX_TEST_NAME}" required autocomplete="off"></label>
<button type="submit">Sign in as test player</button>
</form>"#,
            escape(&id)
        );
    }
    state.page(StatusCode::OK, "Sign in", &content)
}

/// Sends the browser to Discord for the sign-in `id`.
async fn to_discord(
    State(state): State<Arc<ProfilesState>>,
    Query(params): Query<SignInParams>,
) -> Response {
    let id = params.id.unwrap_or_default();
    let discord = state
        .config
        .discord
        .as_ref()
        .expect("the route needs Discord");
    match state.sign_ins.authorize(&id) {
        Some(authorization) => {
            Redirect::to(authorization.url(&state.config, discord).as_str()).into_response()
        }
        None => state.notice_page(StatusCode::NOT_FOUND, &SignInError::Expired.message()),
    }
}

/// What Discord adds to the callback: `code` and `state`, or `error`.
#[derive(Deserialize)]
struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn discord_callback(
    State(state): State<Arc<ProfilesState>>,
    Query(params): Query<CallbackParams>,
) -> Response {
    let (id, account) = match state.discord_account(params).await {
        Ok(found) => found,
        Err(error) => {
            warn!(target: "profiles", %error, "Discord sign-in refused");
            return state.notice_page(error.status(), &error.message());
        }
    };
    let identity = Identity {
        provider: "discord",
        external_id: &account.id,
        username: &account.username,
        display_name: &account.display_name,
        avatar_url: Some(&account.avatar_url),
    };
    match state.finish(&id, &identity).await {
        Ok(true) => state.signed_in_page(&account.display_name),
        Ok(false) => state.notice_page(StatusCode::NOT_FOUND, &SignInError::Expired.message()),
        Err(error) => {
            warn!(target: "profiles", %error, "database error");
            state.notice_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The server could not finish signing you in. Please try again.",
            )
        }
    }
}

#[derive(Deserialize)]
struct TestSignInForm {
    id: String,
    name: String,
}

/// The sign-in page's test form.
async fn page_test_sign_in(
    State(state): State<Arc<ProfilesState>>,
    Form(form): Form<TestSignInForm>,
) -> Response {
    if !state.config.test_sign_in {
        return state.notice_page(StatusCode::FORBIDDEN, "This server has no test sign-in.");
    }
    let Some(name) = test_name(&form.name) else {
        return state.notice_page(
            StatusCode::BAD_REQUEST,
            "A test name has 1 to 16 letters, digits, - or _.",
        );
    };
    if state.sign_ins.code(&form.id).is_none() {
        return state.notice_page(StatusCode::NOT_FOUND, &SignInError::Expired.message());
    }
    let external_id = name.to_lowercase();
    match state
        .finish(&form.id, &test_identity(&name, &external_id))
        .await
    {
        Ok(true) => state.signed_in_page(&name),
        Ok(false) => state.notice_page(StatusCode::NOT_FOUND, &SignInError::Expired.message()),
        Err(error) => {
            warn!(target: "profiles", %error, "database error");
            state.notice_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The server could not finish signing you in. Please try again.",
            )
        }
    }
}

/// `name` as a test name, if it is one.
fn test_name(name: &str) -> Option<String> {
    let name = name.trim();
    let valid = (1..=MAX_TEST_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    valid.then(|| name.to_owned())
}

fn test_identity<'a>(name: &'a str, external_id: &'a str) -> Identity<'a> {
    Identity {
        provider: "test",
        external_id,
        username: name,
        display_name: name,
        avatar_url: None,
    }
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLESHEET,
    )
}

/// Every response of the service: never cached, never framed.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_names_are_short_and_plain() {
        assert_eq!(test_name(" Mario_2 ").as_deref(), Some("Mario_2"));
        for name in ["", " ", "seventeen-letters", "a b", "<b>", "Mário"] {
            assert!(test_name(name).is_none(), "{name}");
        }
    }

    #[test]
    fn a_bearer_token_is_read_from_its_header() {
        let mut headers = HeaderMap::new();
        assert!(bearer(&headers).is_none());
        headers.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert_eq!(bearer(&headers), Some("abc"));
        headers.insert(header::AUTHORIZATION, "Basic abc".parse().unwrap());
        assert!(bearer(&headers).is_none());
    }
}

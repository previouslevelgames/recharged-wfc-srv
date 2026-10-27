//! `/admin`: Google sign-in against a fake Google, and what it guards.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Form, Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode, header, request},
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use recharged_wfc_srv::admin::Config;
use reqwest::Url;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;

const PUBLIC_URL: &str = "http://127.0.0.1:8080";
const CLIENT_ID: &str = "client.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "client-secret";
const CODE: &str = "the-code";

/// The pages every admin sees: all but the owners' Maintenance page.
const PAGES: [&str; 9] = [
    "/admin",
    "/admin/players",
    "/admin/matchmaking",
    "/admin/matches",
    "/admin/leaderboards",
    "/admin/competitive",
    "/admin/moderation",
    "/admin/database",
    "/admin/logs",
];

#[sqlx::test]
async fn an_admin_signs_in_sees_every_section_and_signs_out(pool: PgPool) {
    let (config, google) = fake_google().await;
    let mut browser = Browser::new(recharged_wfc_srv::App::new(pool).router(Some(config), None));

    let (status, _, page) = browser.get("/admin").await;
    assert_eq!(status, 200);
    assert!(page.contains(r#"href="http://127.0.0.1:8080/admin/login""#));
    assert_eq!(browser.get("/admin/api/overview").await.0, 401);

    let params = start_sign_in(&mut browser, "/admin/login").await;
    google.approve(&params, "Admin@Example.com");
    let (status, headers, _) = browser.callback(CODE, &params["state"]).await;
    assert_eq!(status, 303);
    assert_eq!(headers[header::LOCATION], "/admin");
    let session = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap())
        .find(|value| value.starts_with("wfc_admin="))
        .unwrap();
    for attribute in ["HttpOnly", "SameSite=Lax", "Path=/admin"] {
        assert!(session.contains(attribute), "{session}");
    }
    assert!(!browser.cookies.contains_key("wfc_admin_login"));

    for path in PAGES {
        let (status, _, page) = browser.get(path).await;
        assert_eq!(status, 200, "{path}");
        assert!(page.contains("/admin/admin.js"), "{path}");
    }
    assert_eq!(browser.get("/admin/elsewhere").await.0, 404);
    // A profile's or a match's own page, for a number only.
    assert_eq!(browser.get("/admin/players/999999999").await.0, 200);
    assert_eq!(browser.get("/admin/matches/12").await.0, 200);
    for path in [
        "/admin/matches/twelve",
        "/admin/matches/",
        "/admin/leaderboards/1",
    ] {
        assert_eq!(browser.get(path).await.0, 404, "{path}");
    }

    for (api, check) in [
        ("overview", "/players/accounts"),
        ("overview", "/matchmaking/sessions"),
        ("overview", "/matches/in_progress"),
        ("players", "/profiles/0/friend_code"),
        ("matchmaking", "/totals/searching"),
        ("matches", "/totals/one_sided"),
        ("database", "/tables/0/name"),
        ("leaderboards", "/totals/entries"),
        ("competitive", "/rule_sets/0/rules/tweaks"),
        ("logs", "/files"),
    ] {
        let (status, _, body) = browser.get(&format!("/admin/api/{api}")).await;
        assert_eq!(status, 200, "{api}");
        let data: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(data["viewer"]["email"], "admin@example.com", "{api}");
        assert_eq!(data["server"]["version"], recharged_wfc_srv::VERSION);
        assert_eq!(data["server"]["revision"], recharged_wfc_srv::REVISION);
        assert!(data["error"].is_null(), "{body}");
        assert!(data["content"].pointer(check).is_some(), "{body}");
    }

    let (status, _, body) = browser.get("/admin/api/players/999999999").await;
    assert_eq!(status, 200);
    let data: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        data["content"]["profile"]["friend_code"], "1770-9365-9135",
        "{body}"
    );
    assert_eq!(data["content"]["stats"][0]["mode"], "ranked", "{body}");

    // No match 12 yet: no content, and no error either.
    let (status, _, body) = browser.get("/admin/api/matches/12").await;
    assert_eq!(status, 200);
    let data: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        data["content"].is_null() && data["error"].is_null(),
        "{body}"
    );

    let (status, _, _) = browser
        .send(Request::post("/admin/logout").header(header::ORIGIN, PUBLIC_URL))
        .await;
    assert_eq!(status, 303);
    assert!(!browser.cookies.contains_key("wfc_admin"));
    assert_eq!(browser.get("/admin/api/overview").await.0, 401);
    assert_eq!(browser.get("/admin/api/matches/12").await.0, 401);
}

#[sqlx::test]
async fn only_an_owner_wipes_the_leaderboards_and_only_when_asked_twice(pool: PgPool) {
    const ASK: &str = "/admin/api/maintenance/wipe-leaderboards";
    const CONFIRM: &str = "/admin/api/maintenance/wipe-leaderboards/confirm";
    for (profile_id, category) in [(1, 0), (2, 1)] {
        sqlx::query(
            "INSERT INTO scores (profile_id, category, region, score, userdata)
             VALUES ($1, $2, 4, 10, '')",
        )
        .bind(profile_id)
        .bind(category)
        .execute(&pool)
        .await
        .unwrap();
    }
    let scores = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM scores")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let (config, google) = fake_google().await;
    let app = recharged_wfc_srv::App::new(pool.clone()).router(Some(config), None);

    // An admin, not an owner: no page, no nav item, no wipe.
    let mut admin = Browser::new(app.clone());
    sign_in(&mut admin, &google, "second@example.com").await;
    assert_eq!(admin.get("/admin/maintenance").await.0, 404);
    assert_eq!(admin.get("/admin/api/maintenance").await.0, 403);
    assert_eq!(admin.post_json(ASK, None).await.0, 403);
    let (_, _, body) = admin.get("/admin/api/overview").await;
    let data: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(data["viewer"]["owner"], false, "{body}");

    // The owner, who is in OWNER_GOOGLE_EMAILS only.
    let mut owner = Browser::new(app);
    sign_in(&mut owner, &google, "owner@example.com").await;
    assert_eq!(owner.get("/admin/maintenance").await.0, 200);
    let (status, _, body) = owner.get("/admin/api/maintenance").await;
    assert_eq!(status, 200);
    let data: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(data["viewer"]["owner"], true, "{body}");
    assert_eq!(data["content"]["leaderboards"]["entries"], 2, "{body}");

    // Never from another site, nor without the first ask.
    let (status, _, _) = owner
        .send(Request::post(ASK).header(header::ORIGIN, "https://elsewhere.example"))
        .await;
    assert_eq!(status, 403);
    let confirm =
        |token: &str, phrase: &str| Some(serde_json::json!({ "token": token, "phrase": phrase }));
    let (status, answer) = owner
        .post_json(CONFIRM, confirm("made-up", "wipe all leaderboards"))
        .await;
    assert_eq!(status, 409, "{answer}");

    // The first ask tells what goes; a wrong phrase uses its token up.
    let (status, asked) = owner.post_json(ASK, None).await;
    assert_eq!(status, 200, "{asked}");
    assert_eq!(asked["phrase"], "wipe all leaderboards");
    assert_eq!(asked["impact"]["entries"], 2, "{asked}");
    let token = asked["token"].as_str().unwrap();
    let (status, answer) = owner.post_json(CONFIRM, confirm(token, "wipe")).await;
    assert_eq!(status, 400, "{answer}");
    let (status, _) = owner
        .post_json(CONFIRM, confirm(token, "wipe all leaderboards"))
        .await;
    assert_eq!(status, 409);
    assert_eq!(scores().await, 2, "nothing wiped yet");

    let (_, asked) = owner.post_json(ASK, None).await;
    let token = asked["token"].as_str().unwrap();
    let (status, wiped) = owner
        .post_json(CONFIRM, confirm(token, "wipe all leaderboards"))
        .await;
    assert_eq!(status, 200, "{wiped}");
    assert_eq!(wiped, serde_json::json!({ "scores": 2, "past_scores": 0 }));
    assert_eq!(scores().await, 0);
}

#[tokio::test]
async fn an_admin_reads_and_downloads_the_log_files() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("admin-logs");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let day = "2026-10-24T09:00:00.000000Z  INFO gpcm: logged in profile_id=7\n\
               2026-10-24T09:01:00.000000Z  WARN admin: sign-in refused\n";
    std::fs::write(dir.join("recharged-wfc-srv.2026-10-24.log"), day).unwrap();
    std::fs::write(dir.join("secret.txt"), "not a log").unwrap();

    let (mut config, google) = fake_google().await;
    config.log_dir = Some(dir.clone());
    let mut browser =
        Browser::new(recharged_wfc_srv::App::new(lazy_pool()).router(Some(config), None));
    assert_eq!(browser.get("/admin/api/logs").await.0, 401);
    assert_eq!(browser.get("/admin/api/logs/2026-10-24").await.0, 401);

    let params = start_sign_in(&mut browser, "/admin/login").await;
    google.approve(&params, "admin@example.com");
    browser.callback(CODE, &params["state"]).await;

    let (status, _, body) = browser.get("/admin/api/logs?level=warn").await;
    assert_eq!(status, 200);
    let data: serde_json::Value = serde_json::from_str(&body).unwrap();
    let logs = &data["content"];
    assert_eq!(logs["day"], "2026-10-24", "{body}");
    assert_eq!(logs["files"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(logs["matched"], 1, "{body}");
    assert_eq!(logs["lines"][0]["level"], "WARN", "{body}");
    assert_eq!(logs["lines"][0]["target"], "admin", "{body}");

    let (status, headers, body) = browser.get("/admin/api/logs/2026-10-24").await;
    assert_eq!(status, 200);
    assert_eq!(body, day);
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        r#"attachment; filename="recharged-wfc-srv.2026-10-24.log""#
    );
    for uri in [
        "/admin/api/logs/2026-10-23",
        "/admin/api/logs/..%2Fsecret.txt",
        "/admin/api/logs/secret.txt",
    ] {
        assert_eq!(browser.get(uri).await.0, 404, "{uri}");
    }

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn other_google_accounts_are_refused() {
    let (config, google) = fake_google().await;
    let mut browser =
        Browser::new(recharged_wfc_srv::App::new(lazy_pool()).router(Some(config), None));

    let params = start_sign_in(&mut browser, "/admin/login").await;
    google.approve(&params, "someone@example.com");
    let (status, _, page) = browser.callback(CODE, &params["state"]).await;
    assert_eq!(status, 403);
    assert!(page.contains("someone@example.com is not an admin"));
    assert!(!browser.cookies.contains_key("wfc_admin"));
    assert_eq!(browser.get("/admin/api/overview").await.0, 401);
}

#[tokio::test]
async fn a_callback_must_belong_to_a_sign_in_in_this_browser() {
    let (config, google) = fake_google().await;
    let mut browser =
        Browser::new(recharged_wfc_srv::App::new(lazy_pool()).router(Some(config), None));

    // No sign-in cookie.
    let (status, _, page) = browser.callback(CODE, "state").await;
    assert_eq!(status, 400);
    assert!(page.contains("expired"));

    // Another sign-in's state.
    let params = start_sign_in(&mut browser, "/admin/login").await;
    google.approve(&params, "admin@example.com");
    let (status, _, page) = browser.callback(CODE, "another-state").await;
    assert_eq!(status, 400);
    assert!(page.contains("did not start in this browser"));

    // Cancelled on Google's page.
    start_sign_in(&mut browser, "/admin/login").await;
    let (status, _, page) = browser.get("/admin/callback?error=access_denied").await;
    assert_eq!(status, 400);
    assert!(page.contains("cancelled"));

    assert!(!browser.cookies.contains_key("wfc_admin"));
}

#[tokio::test]
async fn sign_in_returns_to_the_page_it_began_on() {
    let (config, google) = fake_google().await;
    let mut browser =
        Browser::new(recharged_wfc_srv::App::new(lazy_pool()).router(Some(config), None));

    let (status, _, page) = browser.get("/admin/database").await;
    assert_eq!(status, 200);
    assert!(page.contains(r#"href="http://127.0.0.1:8080/admin/login?next=/admin/database""#));

    let (_, _, page) = browser.get("/admin/matches/12").await;
    assert!(page.contains(r#"href="http://127.0.0.1:8080/admin/login?next=/admin/matches/12""#));

    for (login, page) in [
        ("/admin/login?next=/admin/database", "/admin/database"),
        ("/admin/login?next=/admin/matches/12", "/admin/matches/12"),
        ("/admin/login?next=/admin/matches/../logs", "/admin"),
        ("/admin/login?next=https://elsewhere.example", "/admin"),
    ] {
        let params = start_sign_in(&mut browser, login).await;
        google.approve(&params, "admin@example.com");
        let (status, headers, _) = browser.callback(CODE, &params["state"]).await;
        assert_eq!(status, 303, "{login}");
        assert_eq!(headers[header::LOCATION], page, "{login}");
    }
}

#[tokio::test]
async fn admin_pages_are_locked_down() {
    let (config, _) = fake_google().await;
    let mut browser =
        Browser::new(recharged_wfc_srv::App::new(lazy_pool()).router(Some(config), None));

    for uri in ["/admin", "/admin/admin.js", "/admin/api/database"] {
        let (_, headers, _) = browser.get(uri).await;
        assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{uri}");
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY", "{uri}");
        let policy = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(policy.contains("script-src 'self'"), "{uri}");
    }

    let (status, _, _) = browser
        .send(Request::post("/admin/logout").header(header::ORIGIN, "https://elsewhere.example"))
        .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn admin_is_not_served_without_its_configuration() {
    let mut browser = Browser::new(recharged_wfc_srv::App::new(lazy_pool()).router(None, None));
    for uri in ["/admin", "/admin/login", "/admin/api/overview"] {
        assert_eq!(browser.get(uri).await.0, 404, "{uri}");
    }
}

/// Signs `email` in, through [`FakeGoogle`].
async fn sign_in(browser: &mut Browser, google: &FakeGoogle, email: &str) {
    let params = start_sign_in(browser, "/admin/login").await;
    google.approve(&params, email);
    let (status, _, _) = browser.callback(CODE, &params["state"]).await;
    assert_eq!(status, 303, "{email}");
}

/// Follows `login`, a `/admin/login` URI, to Google. Returns the query it
/// sends there.
async fn start_sign_in(browser: &mut Browser, login: &str) -> HashMap<String, String> {
    let (status, headers, _) = browser.get(login).await;
    assert_eq!(status, 303);
    assert!(browser.cookies.contains_key("wfc_admin_login"));
    let location = Url::parse(headers[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(
        location.as_str().split('?').next(),
        Some("https://accounts.example.com/authorize")
    );
    let params: HashMap<String, String> = location.query_pairs().into_owned().collect();
    assert_eq!(params["client_id"], CLIENT_ID);
    assert_eq!(
        params["redirect_uri"],
        format!("{PUBLIC_URL}/admin/callback")
    );
    assert_eq!(params["code_challenge_method"], "S256");
    params
}

/// Sends requests to the router like a browser: keeps the cookies it gets.
struct Browser {
    app: Router,
    cookies: HashMap<String, String>,
}

impl Browser {
    fn new(app: Router) -> Self {
        Self {
            app,
            cookies: HashMap::new(),
        }
    }

    async fn get(&mut self, uri: &str) -> (StatusCode, HeaderMap, String) {
        self.send(Request::get(uri)).await
    }

    async fn callback(&mut self, code: &str, state: &str) -> (StatusCode, HeaderMap, String) {
        self.get(&format!("/admin/callback?code={code}&state={state}"))
            .await
    }

    /// A change from this server's own pages: `json`, or no body. Returns
    /// the JSON answer, or `null`.
    async fn post_json(
        &mut self,
        uri: &str,
        json: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::post(uri)
            .header(header::ORIGIN, PUBLIC_URL)
            .header(header::CONTENT_TYPE, "application/json");
        let body = json.map_or_else(Body::empty, |json| Body::from(json.to_string()));
        let (status, _, answer) = self.send_body(request, body).await;
        (status, serde_json::from_str(&answer).unwrap_or_default())
    }

    async fn send(&mut self, request: request::Builder) -> (StatusCode, HeaderMap, String) {
        self.send_body(request, Body::empty()).await
    }

    async fn send_body(
        &mut self,
        mut request: request::Builder,
        body: Body,
    ) -> (StatusCode, HeaderMap, String) {
        if !self.cookies.is_empty() {
            let cookies: Vec<String> = self
                .cookies
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            request = request.header(header::COOKIE, cookies.join("; "));
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        for cookie in response.headers().get_all(header::SET_COOKIE) {
            let cookie = cookie.to_str().unwrap();
            let (pair, attributes) = cookie.split_once(';').unwrap_or((cookie, ""));
            let (name, value) = pair.split_once('=').unwrap();
            if attributes.contains("Max-Age=0") {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.to_owned(), value.to_owned());
            }
        }
        let (parts, body) = response.into_parts();
        let body = axum::body::to_bytes(body, 1 << 20).await.unwrap();
        (
            parts.status,
            parts.headers,
            String::from_utf8(body.to_vec()).unwrap(),
        )
    }
}

/// Google's token endpoint, on a local port. It exchanges [`CODE`] once
/// for each [`FakeGoogle::approve`].
#[derive(Clone, Default)]
struct FakeGoogle {
    grant: Arc<Mutex<Option<Grant>>>,
}

struct Grant {
    challenge: String,
    nonce: String,
    email: String,
}

impl FakeGoogle {
    /// As if `email` signed in on Google's page, which got `params`.
    fn approve(&self, params: &HashMap<String, String>, email: &str) {
        *self.grant.lock().unwrap() = Some(Grant {
            challenge: params["code_challenge"].clone(),
            nonce: params["nonce"].clone(),
            email: email.to_owned(),
        });
    }
}

/// An admin configuration that signs in with a new [`FakeGoogle`].
async fn fake_google() -> (Config, FakeGoogle) {
    let google = FakeGoogle::default();
    let app = Router::new()
        .route("/token", post(token))
        .with_state(google.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut config = Config::new(
        PUBLIC_URL,
        CLIENT_ID,
        CLIENT_SECRET,
        "admin@example.com, second@example.com",
    )
    .unwrap();
    config.owners = vec!["owner@example.com".into()];
    config.google.authorize = Url::parse("https://accounts.example.com/authorize").unwrap();
    config.google.token = Url::parse(&format!("http://{address}/token")).unwrap();
    (config, google)
}

async fn token(
    State(google): State<FakeGoogle>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let field = |name: &str| form.get(name).map(String::as_str).unwrap_or_default();
    let grant = google
        .grant
        .lock()
        .unwrap()
        .take()
        .ok_or(StatusCode::BAD_REQUEST)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(field("code_verifier")).as_slice());
    if field("grant_type") != "authorization_code"
        || field("code") != CODE
        || field("client_id") != CLIENT_ID
        || field("client_secret") != CLIENT_SECRET
        || field("redirect_uri") != format!("{PUBLIC_URL}/admin/callback")
        || challenge != grant.challenge
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let encode = |value: serde_json::Value| URL_SAFE_NO_PAD.encode(value.to_string());
    let claims = serde_json::json!({
        "iss": "https://accounts.google.com",
        "aud": CLIENT_ID,
        "sub": "1234567890",
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        "nonce": grant.nonce,
        "email": grant.email,
        "email_verified": true,
        "name": "Test Admin",
    });
    Ok(Json(serde_json::json!({
        "access_token": "access-token",
        "expires_in": 3600,
        "token_type": "Bearer",
        "id_token": format!(
            "{}.{}.signature",
            encode(serde_json::json!({"alg": "RS256"})),
            encode(claims)
        ),
    })))
}

/// A pool that connects on its first query: these requests never make one.
fn lazy_pool() -> PgPool {
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    PgPoolOptions::new().connect_lazy_with(PgConnectOptions::new())
}

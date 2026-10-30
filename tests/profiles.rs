//! Recharged profiles (docs/profiles.md): the launcher's sign-in through the
//! browser against a fake Discord, test sign-in, and sessions.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Form, Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode, header},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use recharged_wfc_srv::profiles::{Config, DiscordClient, DiscordEndpoints};
use reqwest::Url;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;

const PUBLIC_URL: &str = "http://127.0.0.1:8080";
const CLIENT_ID: &str = "1561321462327943259";
const CLIENT_SECRET: &str = "client-secret";
const CODE: &str = "the-code";
const ACCESS_TOKEN: &str = "access-token";
const SDK_CODE: &str = "sdk-code";
const SDK_VERIFIER: &str = "sdk-verifier";
const SDK_REDIRECT: &str = "http://127.0.0.1:52411/callback";
const SDK_REFRESH: &str = "sdk-refresh-token";

#[sqlx::test]
async fn the_launcher_signs_in_with_discord(pool: PgPool) {
    let (config, discord) = fake_discord(false).await;
    let app = recharged_wfc_srv::App::new(pool).router(None, Some(config));

    let (status, _, body) = send(&app, Request::post("/api/v1/sign-in"), None).await;
    assert_eq!(status, 200);
    let sign_in: Value = serde_json::from_str(&body).unwrap();
    let id = sign_in["id"].as_str().unwrap();
    let code = sign_in["code"].as_str().unwrap();
    assert_eq!(
        sign_in["url"].as_str().unwrap(),
        format!("{PUBLIC_URL}/sign-in?id={id}")
    );
    assert_eq!(code.len(), 9);
    assert_eq!(
        poll(&app, id).await,
        (StatusCode::OK, json!({"status": "waiting"}))
    );

    // The page the launcher opens shows its code and goes on to Discord.
    let (status, headers, page) = get_page(&app, &format!("/sign-in?id={id}")).await;
    assert_eq!(status, 200);
    assert!(page.contains(code), "{page}");
    assert!(
        page.contains(&format!(r#"href="/sign-in/discord?id={id}""#)),
        "{page}"
    );
    assert!(!page.contains("Sign in as test player"));
    let policy = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(policy.contains("frame-ancestors 'none'"));

    let params = to_discord(&app, id).await;
    discord.approve(&params, "80351110224678912", "nelly", Some("Nelly"));
    let (status, _, page) = get_page(
        &app,
        &format!(
            "/sign-in/discord/callback?code={CODE}&state={}",
            params["state"]
        ),
    )
    .await;
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("Signed in as <b>Nelly</b>"), "{page}");

    // The session, once.
    let (status, signed_in) = poll(&app, id).await;
    assert_eq!(status, 200);
    assert_eq!(signed_in["status"], "signed_in");
    assert_eq!(signed_in["player"]["name"], "Nelly");
    assert_eq!(signed_in["player"]["discord"], "nelly");
    assert_eq!(signed_in["player"]["test"], false);
    assert_eq!(poll(&app, id).await.0, 404);

    let token = signed_in["token"].as_str().unwrap();
    let (status, _, body) = send(&app, Request::get("/api/v1/me"), Some(token)).await;
    assert_eq!(status, 200);
    let mut me: Value = serde_json::from_str(&body).unwrap();
    // With the WFC profiles proven the player's (none yet) and the Discord
    // account id, which the launcher makes the player's Mii from.
    assert_eq!(me["wfc"], json!([]));
    assert_eq!(me["discord_id"], "80351110224678912");
    me.as_object_mut().unwrap().remove("wfc");
    me.as_object_mut().unwrap().remove("discord_id");
    assert_eq!(me, signed_in["player"]);

    // Signing in again with the same account: the same player.
    let (_, _, body) = send(&app, Request::post("/api/v1/sign-in"), None).await;
    let again: Value = serde_json::from_str(&body).unwrap();
    let id = again["id"].as_str().unwrap();
    let params = to_discord(&app, id).await;
    discord.approve(&params, "80351110224678912", "nelly", None);
    get_page(
        &app,
        &format!(
            "/sign-in/discord/callback?code={CODE}&state={}",
            params["state"]
        ),
    )
    .await;
    let (_, signed_in_again) = poll(&app, id).await;
    assert_eq!(signed_in_again["player"]["id"], me["id"]);
    assert_eq!(signed_in_again["player"]["name"], "nelly");
    assert_ne!(signed_in_again["token"], signed_in["token"]);
}

#[sqlx::test]
async fn the_discord_sdk_gets_tokens_for_the_signed_in_account(pool: PgPool) {
    let (config, discord) = fake_discord(false).await;
    let app = recharged_wfc_srv::App::new(pool).router(None, Some(config));
    let token = discord_sign_in(&app, &discord, "80351110224678912", "nelly").await;
    let exchange = |body: Value| {
        let app = app.clone();
        let token = token.clone();
        async move {
            let (status, headers, body) = send(
                &app,
                Request::post("/api/v1/discord/token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .with_body(&body.to_string()),
                Some(&token),
            )
            .await;
            (
                status,
                headers,
                serde_json::from_str::<Value>(&body).unwrap(),
            )
        }
    };
    let code = json!({
        "code": SDK_CODE,
        "code_verifier": SDK_VERIFIER,
        "redirect_uri": SDK_REDIRECT,
    });

    // The SDK's authorization signed in with the same account: its tokens,
    // never the OpenID token, and nothing for caches.
    *discord.sdk_account.lock().unwrap() = "80351110224678912".into();
    let (status, headers, tokens) = exchange(code.clone()).await;
    assert_eq!(status, 200, "{tokens}");
    assert_eq!(tokens["access_token"], "sdk-access-token");
    assert_eq!(tokens["refresh_token"], SDK_REFRESH);
    assert_eq!(tokens["expires_in"], 604800);
    assert_eq!(tokens["scope"], "openid sdk.social_layer_presence");
    assert!(tokens.get("id_token").is_none(), "{tokens}");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let (status, _, refreshed) = exchange(json!({ "refresh_token": SDK_REFRESH })).await;
    assert_eq!(status, 200, "{refreshed}");
    assert_eq!(refreshed["access_token"], "sdk-access-token");

    // Another Discord account than the player's.
    *discord.sdk_account.lock().unwrap() = "41771983423143937".into();
    let (status, _, refused) = exchange(code.clone()).await;
    assert_eq!(status, 403);
    assert!(
        refused["error"].as_str().unwrap().contains("connect nelly"),
        "{refused}"
    );

    // Discord refuses: a used code, an ended link.
    let (status, _, _) = exchange(json!({
        "code": "used",
        "code_verifier": SDK_VERIFIER,
        "redirect_uri": SDK_REDIRECT,
    }))
    .await;
    assert_eq!(status, 400);
    let (status, _, refused) = exchange(json!({ "refresh_token": "revoked" })).await;
    assert_eq!(status, 400);
    assert_eq!(refused["error"], "Discord refused: connect Discord again");

    // Only the SDK's own redirect, and one grant at a time.
    for body in [
        json!({"code": SDK_CODE, "code_verifier": SDK_VERIFIER,
               "redirect_uri": format!("{PUBLIC_URL}/sign-in/discord/callback")}),
        json!({"code": SDK_CODE, "code_verifier": SDK_VERIFIER,
               "redirect_uri": "http://127.0.0.1.example.com/callback"}),
        json!({"code": SDK_CODE, "code_verifier": SDK_VERIFIER}),
        json!({"code": SDK_CODE, "code_verifier": SDK_VERIFIER, "redirect_uri": SDK_REDIRECT,
               "refresh_token": SDK_REFRESH}),
        json!({}),
    ] {
        assert_eq!(exchange(body.clone()).await.0, 400, "{body}");
    }

    // Signed in only.
    let (status, _, _) = send(
        &app,
        Request::post("/api/v1/discord/token")
            .header(header::CONTENT_TYPE, "application/json")
            .with_body(&code.to_string()),
        None,
    )
    .await;
    assert_eq!(status, 401);
}

#[sqlx::test]
async fn discord_friends_who_play_recharged_are_found(pool: PgPool) {
    let (config, discord) = fake_discord(false).await;
    let app = recharged_wfc_srv::App::new(pool).router(None, Some(config));
    let nelly = discord_sign_in(&app, &discord, "80351110224678912", "nelly").await;
    let bob = discord_sign_in(&app, &discord, "41771983423143937", "bob").await;
    let lookup = |ids: Value| {
        let app = app.clone();
        let nelly = nelly.clone();
        async move {
            let (status, _, body) = send(
                &app,
                Request::post("/api/v1/players/discord")
                    .header(header::CONTENT_TYPE, "application/json")
                    .with_body(&json!({ "ids": ids }).to_string()),
                Some(&nelly),
            )
            .await;
            (status, serde_json::from_str::<Value>(&body).unwrap())
        }
    };

    // Of three Discord friends, one plays Recharged; the player who asks is
    // never among them.
    let (status, found) = lookup(json!([
        "41771983423143937",
        "123456789",
        "80351110224678912"
    ]))
    .await;
    assert_eq!(status, 200, "{found}");
    let players = found["players"].as_array().unwrap();
    assert_eq!(players.len(), 1, "{found}");
    assert_eq!(players[0]["name"], "bob");
    assert_eq!(players[0]["discord_id"], "41771983423143937");
    assert_eq!(players[0]["online"], true);
    assert_eq!(players[0]["friend"], false);
    assert_eq!(
        lookup(json!([])).await,
        (StatusCode::OK, json!({"players": []}))
    );

    let too_many: Vec<String> = (0..201).map(|n| n.to_string()).collect();
    assert_eq!(lookup(json!(too_many)).await.0, 400);
    assert_eq!(lookup(json!(["bob"])).await.0, 400);
    assert_eq!(lookup(json!([""])).await.0, 400);
    drop(bob);
}

#[sqlx::test]
async fn a_cancelled_sign_in_tells_the_launcher(pool: PgPool) {
    let (config, _) = fake_discord(false).await;
    let app = recharged_wfc_srv::App::new(pool).router(None, Some(config));

    let (_, _, body) = send(&app, Request::post("/api/v1/sign-in"), None).await;
    let sign_in: Value = serde_json::from_str(&body).unwrap();
    let id = sign_in["id"].as_str().unwrap();
    let params = to_discord(&app, id).await;
    let (status, _, page) = get_page(
        &app,
        &format!(
            "/sign-in/discord/callback?error=access_denied&state={}",
            params["state"]
        ),
    )
    .await;
    assert_eq!(status, 400);
    assert!(page.contains("cancelled"), "{page}");
    assert_eq!(
        poll(&app, id).await,
        (
            StatusCode::OK,
            json!({"status": "refused", "message": "Sign-in was cancelled."})
        )
    );

    // A state no sign-in has.
    let (status, _, page) = get_page(
        &app,
        &format!("/sign-in/discord/callback?code={CODE}&state=made-up"),
    )
    .await;
    assert_eq!(status, 400);
    assert!(page.contains("expired"), "{page}");
}

#[sqlx::test]
async fn a_test_player_signs_in_and_out(pool: PgPool) {
    let (config, _) = fake_discord(true).await;
    let app = recharged_wfc_srv::App::new(pool).router(None, Some(config));

    let (status, _, body) = send(&app, Request::get("/api/v1/server"), None).await;
    assert_eq!(status, 200);
    let server: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(server["sign_in"], json!({"discord": true, "test": true}));
    assert_eq!(server["discord_application"], CLIENT_ID);
    assert_eq!(server["version"], recharged_wfc_srv::VERSION);

    let signed_in = test_sign_in(&app, "Mario").await;
    assert_eq!(signed_in["player"]["name"], "Mario");
    assert_eq!(signed_in["player"]["test"], true);
    let token = signed_in["token"].as_str().unwrap();
    assert_eq!(
        send(&app, Request::get("/api/v1/me"), Some(token)).await.0,
        200
    );
    // The same name in other letters: the same player.
    let again = test_sign_in(&app, "mario").await;
    assert_eq!(again["player"]["id"], signed_in["player"]["id"]);

    let (status, _, _) = send(&app, Request::post("/api/v1/sign-out"), Some(token)).await;
    assert_eq!(status, 204);
    for (request, token) in [
        (Request::get("/api/v1/me"), Some(token)),
        (Request::get("/api/v1/me"), None),
        (Request::post("/api/v1/sign-out"), Some(token)),
    ] {
        assert_eq!(send(&app, request, token).await.0, 401);
    }

    let (status, _, _) = send(
        &app,
        Request::post("/api/v1/sign-in/test")
            .header(header::CONTENT_TYPE, "application/json")
            .with_body(r#"{"name": "no spaces allowed"}"#),
        None,
    )
    .await;
    assert_eq!(status, 400);
}

#[sqlx::test]
async fn the_sign_in_page_takes_a_test_name(pool: PgPool) {
    let (config, _) = fake_discord(true).await;
    let app = recharged_wfc_srv::App::new(pool).router(None, Some(config));

    let (_, _, body) = send(&app, Request::post("/api/v1/sign-in"), None).await;
    let sign_in: Value = serde_json::from_str(&body).unwrap();
    let id = sign_in["id"].as_str().unwrap();
    let (_, _, page) = get_page(&app, &format!("/sign-in?id={id}")).await;
    assert!(page.contains("Sign in as test player"), "{page}");

    let (status, _, page) = send(
        &app,
        Request::post("/sign-in/test")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .with_body(&format!("id={id}&name=Luigi")),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(page.contains("Signed in as <b>Luigi</b>"), "{page}");
    let (_, signed_in) = poll(&app, id).await;
    assert_eq!(signed_in["player"]["name"], "Luigi");
}

#[tokio::test]
async fn test_sign_in_needs_its_setting() {
    let (config, _) = fake_discord(false).await;
    let app = recharged_wfc_srv::App::new(lazy_pool()).router(None, Some(config));
    let (status, _, _) = send(
        &app,
        Request::post("/api/v1/sign-in/test")
            .header(header::CONTENT_TYPE, "application/json")
            .with_body(r#"{"name": "Mario"}"#),
        None,
    )
    .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn profiles_are_not_served_without_their_configuration() {
    let app = recharged_wfc_srv::App::new(lazy_pool()).router(None, None);
    for uri in ["/api/v1/server", "/api/v1/me", "/sign-in"] {
        assert_eq!(send(&app, Request::get(uri), None).await.0, 404, "{uri}");
    }
}

#[tokio::test]
async fn expired_sign_ins_have_no_page() {
    let (config, _) = fake_discord(true).await;
    let app = recharged_wfc_srv::App::new(lazy_pool()).router(None, Some(config));
    for uri in ["/sign-in?id=made-up", "/sign-in/discord?id=made-up"] {
        let (status, _, page) = get_page(&app, uri).await;
        assert_eq!(status, 404, "{uri}");
        assert!(page.contains("Start again"), "{page}");
    }
    assert_eq!(poll(&app, "made-up").await.0, 404);
}

/// Follows `/sign-in/discord?id=<id>` to Discord. Returns the query it sends
/// there.
async fn to_discord(app: &Router, id: &str) -> HashMap<String, String> {
    let (status, headers, _) = get_page(app, &format!("/sign-in/discord?id={id}")).await;
    assert_eq!(status, 303);
    let location = Url::parse(headers[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(
        location.as_str().split('?').next(),
        Some("https://discord.example.com/oauth2/authorize")
    );
    let params: HashMap<String, String> = location.query_pairs().into_owned().collect();
    assert_eq!(params["client_id"], CLIENT_ID);
    assert_eq!(params["scope"], "identify");
    assert_eq!(
        params["redirect_uri"],
        format!("{PUBLIC_URL}/sign-in/discord/callback")
    );
    assert_eq!(params["code_challenge_method"], "S256");
    params
}

/// Signs in with the Discord account `id` through the browser; the session.
async fn discord_sign_in(app: &Router, discord: &FakeDiscord, id: &str, username: &str) -> String {
    let (_, _, body) = send(app, Request::post("/api/v1/sign-in"), None).await;
    let sign_in: Value = serde_json::from_str(&body).unwrap();
    let sign_in_id = sign_in["id"].as_str().unwrap();
    let params = to_discord(app, sign_in_id).await;
    discord.approve(&params, id, username, None);
    let (status, _, page) = get_page(
        app,
        &format!(
            "/sign-in/discord/callback?code={CODE}&state={}",
            params["state"]
        ),
    )
    .await;
    assert_eq!(status, 200, "{page}");
    let (_, signed_in) = poll(app, sign_in_id).await;
    signed_in["token"].as_str().unwrap().to_owned()
}

async fn poll(app: &Router, id: &str) -> (StatusCode, Value) {
    let (status, _, body) = send(app, Request::get(format!("/api/v1/sign-in/{id}")), None).await;
    (status, serde_json::from_str(&body).unwrap())
}

async fn test_sign_in(app: &Router, name: &str) -> Value {
    let (status, _, body) = send(
        app,
        Request::post("/api/v1/sign-in/test")
            .header(header::CONTENT_TYPE, "application/json")
            .with_body(&json!({ "name": name }).to_string()),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}

async fn get_page(app: &Router, uri: &str) -> (StatusCode, HeaderMap, String) {
    send(app, Request::get(uri), None).await
}

/// A request and the body it carries.
struct WithBody {
    request: axum::http::request::Builder,
    body: String,
}

trait RequestBody {
    fn with_body(self, body: &str) -> WithBody;
}

impl RequestBody for axum::http::request::Builder {
    fn with_body(self, body: &str) -> WithBody {
        WithBody {
            request: self,
            body: body.to_owned(),
        }
    }
}

impl From<axum::http::request::Builder> for WithBody {
    fn from(request: axum::http::request::Builder) -> Self {
        Self {
            request,
            body: String::new(),
        }
    }
}

/// Sends `request` to the router, with `token` as its bearer.
async fn send(
    app: &Router,
    request: impl Into<WithBody>,
    token: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
    let WithBody { mut request, body } = request.into();
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let body = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    (
        parts.status,
        parts.headers,
        String::from_utf8(body.to_vec()).unwrap(),
    )
}

/// Discord's token and user endpoints, on a local port. It exchanges
/// [`CODE`] once for each [`FakeDiscord::approve`].
#[derive(Clone, Default)]
struct FakeDiscord {
    grant: Arc<Mutex<Option<Grant>>>,
    user: Arc<Mutex<Option<Value>>>,
    /// The account the Discord SDK's authorization signed in with: its
    /// [`SDK_CODE`] and [`SDK_REFRESH`] give tokens for it.
    sdk_account: Arc<Mutex<String>>,
}

struct Grant {
    challenge: String,
    user: Value,
}

impl FakeDiscord {
    /// As if the user signed in on Discord's page, which got `params`.
    fn approve(
        &self,
        params: &HashMap<String, String>,
        id: &str,
        username: &str,
        global_name: Option<&str>,
    ) {
        *self.grant.lock().unwrap() = Some(Grant {
            challenge: params["code_challenge"].clone(),
            user: json!({
                "id": id,
                "username": username,
                "global_name": global_name,
                "avatar": null,
                "discriminator": "0",
            }),
        });
    }
}

/// A profile configuration that signs in with a new [`FakeDiscord`].
async fn fake_discord(test_sign_in: bool) -> (Config, FakeDiscord) {
    let discord = FakeDiscord::default();
    let app = Router::new()
        .route("/token", post(token))
        .route("/users/@me", get(user))
        .with_state(discord.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = DiscordClient {
        client_id: CLIENT_ID.into(),
        client_secret: CLIENT_SECRET.into(),
        endpoints: DiscordEndpoints {
            authorize: Url::parse("https://discord.example.com/oauth2/authorize").unwrap(),
            token: Url::parse(&format!("http://{address}/token")).unwrap(),
            user: Url::parse(&format!("http://{address}/users/@me")).unwrap(),
        },
    };
    let config = Config::new(PUBLIC_URL, Some(client), test_sign_in).unwrap();
    (config, discord)
}

async fn token(
    State(discord): State<FakeDiscord>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let field = |name: &str| form.get(name).map(String::as_str).unwrap_or_default();
    let refused = || {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant"})),
        )
    };
    if field("client_id") != CLIENT_ID || field("client_secret") != CLIENT_SECRET {
        return Err(refused());
    }
    // The Discord SDK's grants.
    let sdk_grant = match field("grant_type") {
        "refresh_token" => Some(field("refresh_token") == SDK_REFRESH),
        _ if field("redirect_uri") != format!("{PUBLIC_URL}/sign-in/discord/callback") => Some(
            field("code") == SDK_CODE
                && field("code_verifier") == SDK_VERIFIER
                && field("redirect_uri") == SDK_REDIRECT,
        ),
        _ => None,
    };
    if let Some(valid) = sdk_grant {
        if !valid {
            return Err(refused());
        }
        let claims =
            json!({"iss": "https://discord.com", "sub": *discord.sdk_account.lock().unwrap()});
        let id_token = format!(
            "{}.{}.signature",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        return Ok(Json(json!({
            "access_token": "sdk-access-token",
            "token_type": "Bearer",
            "expires_in": 604800,
            "refresh_token": SDK_REFRESH,
            "scope": "openid sdk.social_layer_presence",
            "id_token": id_token,
        })));
    }
    let grant = discord.grant.lock().unwrap().take().ok_or_else(refused)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(field("code_verifier")).as_slice());
    if field("grant_type") != "authorization_code"
        || field("code") != CODE
        || field("redirect_uri") != format!("{PUBLIC_URL}/sign-in/discord/callback")
        || challenge != grant.challenge
    {
        return Err(refused());
    }
    *discord.user.lock().unwrap() = Some(grant.user);
    Ok(Json(json!({
        "access_token": ACCESS_TOKEN,
        "token_type": "Bearer",
        "expires_in": 604800,
        "refresh_token": "refresh-token",
        "scope": "identify",
    })))
}

async fn user(
    State(discord): State<FakeDiscord>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some(&format!("Bearer {ACCESS_TOKEN}"))
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let user = discord.user.lock().unwrap().take();
    user.map(Json).ok_or(StatusCode::UNAUTHORIZED)
}

/// A pool that connects on its first query: these requests never make one.
fn lazy_pool() -> PgPool {
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    PgPoolOptions::new().connect_lazy_with(PgConnectOptions::new())
}

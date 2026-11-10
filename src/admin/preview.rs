//! `just preview`: the dashboard on 127.0.0.1 with made-up matches and desync
//! dossiers, to look at a page without Google or a game. A fake Google signs
//! anyone in who clicks Sign in; the data is a test database's, gone when it
//! stops. A test, so it reaches the server's own code: the reports and the
//! dossiers go in as the games' would.
//!
//! `just preview-dev`: the same dashboard on the development database
//! (DATABASE_URL), as it is: the matches games played against `just run`.

use std::{collections::HashMap, net::SocketAddr, sync::Arc, sync::Mutex};

use axum::{
    Form, Json, Router,
    extract::{Query, State},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Url;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use super::{Config, matches::tests::Games};
use crate::reports::{
    dossiers::{
        self, Dossier,
        tests::{dossier, field, step},
    },
    report::Side,
};

/// Where the browser opens it: PUBLIC_URL, which the sign-in returns to.
const ADDRESS: &str = "127.0.0.1:8080";
const EMAIL: &str = "preview@example.com";

#[sqlx::test]
#[ignore = "serves the dashboard until stopped: just preview"]
async fn preview_the_dashboard(pool: PgPool) {
    seed(&pool).await;
    serve(pool).await;
}

#[tokio::test]
#[ignore = "serves the dashboard until stopped: just preview-dev"]
async fn preview_the_dev_database() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL, the development database's");
    serve(PgPool::connect(&url).await.unwrap()).await;
}

/// The dashboard on [`ADDRESS`], signed in by a fake Google, until stopped.
async fn serve(pool: PgPool) {
    let google = fake_google().await;
    let mut config =
        Config::new(&format!("http://{ADDRESS}"), "preview", "preview", EMAIL).unwrap();
    config.google = google;
    let router = crate::App::new(pool).router(Some(config), None);
    let listener = tokio::net::TcpListener::bind(ADDRESS).await.unwrap();
    println!(
        "\nThe dashboard: http://{ADDRESS}/admin/monitoring (Sign in signs you in). Ctrl-C stops it.\n"
    );
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}

const BUILD: &str = "1.1.6+g0123456789ab";
/// Systems, as the reports' `os` and the dossiers' `platform`.
type System = (&'static str, &'static str);
const LINUX: System = ("Linux (SteamOS), x86_64", "linux x86_64");
const WINDOWS: System = ("Windows 11 (26100), x86_64", "windows x86_64");
const WINE: System = (
    "Windows 11 (26100), x86_64, Wine 9.0 on Linux",
    "windows x86_64",
);

/// One match for each thing the Desync dossiers card tells apart. Each goes
/// as a Recharged build's would: the start, the game that went out of sync,
/// the end it stopped (`abandoned`, by a `desync`), then the dossiers.
async fn seed(pool: &PgPool) {
    let games = Games::new(pool.clone()).await;

    // The same binary, a value: field 1 of the desync's step.
    let (home, mut away) = series(&games, 1_792_000_000, [LINUX, LINUX]).await;
    away.steps[1] = step(4812, &[1, 9, 3]);
    send(pool, &[home, away]).await;

    // The same binary, another place: the games ran different code. A
    // DebugPrintf branch one side took and the other did not.
    let (mut home, away) = series(&games, 1_792_100_000, [LINUX, LINUX]).await;
    let mut branch = field(0xB012_3456, 0);
    branch.bytes[..12].copy_from_slice(b"shoot power ");
    branch.size = 20;
    home.steps[0].fields[2] = branch;
    send(pool, &[home, away]).await;

    // Linux against Windows: the same code at other offsets, a pose apart.
    let (mut home, mut away) = series(&games, 1_792_200_000, [LINUX, WINDOWS]).await;
    for field in away.steps.iter_mut().flat_map(|step| &mut step.fields) {
        field.caller += 0x4_0000;
    }
    let pose = |x: f32| {
        let mut pose = field(0xF000_1203, 0);
        pose.size = 64;
        pose.hash = x.to_bits();
        pose.bytes[..4].copy_from_slice(&x.to_le_bytes());
        pose
    };
    home.steps[1].fields.push(pose(1.25));
    away.steps[1].fields.push(pose(1.250_000_1));
    send(pool, &[home, away]).await;

    // Every field the same: the seed, or state outside the checksum.
    let (home, away) = series(&games, 1_792_300_000, [LINUX, WINE]).await;
    send(pool, &[home, away]).await;

    // One side's alone: the other's dossier did not come (yet).
    let (home, _) = series(&games, 1_792_400_000, [LINUX, WINDOWS]).await;
    send(pool, &[home]).await;
}

/// A new series between Anosch (home, on `systems[0]`) and Sam (away, on
/// `systems[1]`) whose first game went out of sync at frame 4812, as both
/// games report it; the dossiers they would send.
async fn series(games: &Games, series: i64, systems: [System; 2]) -> (Dossier, Dossier) {
    let sides = [
        (games.anosch, series, "home", systems[0]),
        (games.sam, series + 1, "away", systems[1]),
    ];
    let report = |(profile_id, series, side, (os, _)): (i32, i64, &str, System), event: &str| {
        let mut report = games.report(profile_id, series, side, event);
        report["sidekicks"] =
            json!({"home": ["toad", "toad", "boo"], "away": ["koopa", "shy_guy", "birdo"]});
        report["build"] = json!(BUILD);
        report["os"] = json!(os);
        report
    };
    // Event by event, both sides' each time: the second game joins the
    // first's match while it is still open.
    for side in sides {
        games.send(&report(side, "start")).await;
    }
    for side in sides {
        let mut desync = Games::game(report(side, "game"), 1, 1, 0);
        desync["game"]["ending"] = json!("desync");
        desync["game"]["seconds"] = json!(97);
        desync["game"]["sync"] = json!({"check": "checksum", "frame": 4812});
        games.send(&desync).await;
    }
    for side in sides {
        let mut end = report(side, "end");
        end["reason"] = json!("abandoned");
        end["cause"] = json!("desync");
        games.send(&end).await;
    }
    let dossier = |side, (profile_id, series, _, (_, platform)): (i32, i64, &str, System)| {
        let mut dossier = dossier(side);
        (dossier.profile_id, dossier.series, dossier.game) = (profile_id, series, 1);
        dossier.build = BUILD.to_owned();
        dossier.platform = platform.to_owned();
        dossier
    };
    (dossier(Side::Home, sides[0]), dossier(Side::Away, sides[1]))
}

async fn send(pool: &PgPool, dossiers: &[Dossier]) {
    for dossier in dossiers {
        let body = dossier.encode();
        dossiers::store(pool, dossier, &body).await.unwrap();
    }
}

/// A Google whose sign-in page lets anybody in as [`EMAIL`] at once.
#[derive(Clone, Default)]
struct FakeGoogle {
    /// By code: the sign-in's PKCE challenge and nonce.
    grants: Arc<Mutex<HashMap<String, (String, String)>>>,
}

async fn fake_google() -> super::GoogleEndpoints {
    let google = FakeGoogle::default();
    let app = Router::new()
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .with_state(google);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    super::GoogleEndpoints {
        authorize: Url::parse(&format!("http://{address}/authorize")).unwrap(),
        token: Url::parse(&format!("http://{address}/token")).unwrap(),
    }
}

async fn authorize(
    State(google): State<FakeGoogle>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let code = format!("code-{}", google.grants.lock().unwrap().len());
    google.grants.lock().unwrap().insert(
        code.clone(),
        (params["code_challenge"].clone(), params["nonce"].clone()),
    );
    let mut back = Url::parse(&params["redirect_uri"]).unwrap();
    back.query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &params["state"]);
    Redirect::to(back.as_str()).into_response()
}

async fn token(
    State(google): State<FakeGoogle>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Json<Value>, axum::http::StatusCode> {
    let field = |name: &str| form.get(name).map(String::as_str).unwrap_or_default();
    let (challenge, nonce) = google
        .grants
        .lock()
        .unwrap()
        .remove(field("code"))
        .ok_or(axum::http::StatusCode::BAD_REQUEST)?;
    if URL_SAFE_NO_PAD.encode(Sha256::digest(field("code_verifier")).as_slice()) != challenge {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }
    let encode = |value: Value| URL_SAFE_NO_PAD.encode(value.to_string());
    let claims = json!({
        "iss": "https://accounts.google.com", "aud": "preview", "sub": "preview",
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 3600, "nonce": nonce,
        "email": EMAIL, "email_verified": true, "name": "Preview",
    });
    Ok(Json(json!({
        "access_token": "preview", "expires_in": 3600, "token_type": "Bearer",
        "id_token": format!("{}.{}.signature", encode(json!({"alg": "RS256"})), encode(claims)),
    })))
}

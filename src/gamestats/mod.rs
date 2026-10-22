//! gamestats2: the leaderboard service (`get2.asp` / `put2.asp`).
//!
//! Every operation is two GETs on the same URL (see `docs/gamestats2.md`): the
//! first asks for a token, the second carries the hash of that token and the
//! encoded request. This module owns that transport, shared by both
//! endpoints; `leaderboard` owns what a put stores and what a get returns.
//!
//! The client treats any 4xx/5xx as a global DWC error that drops the player
//! offline, so every request for this game is answered with 200, failures
//! included: an `error:<reason>` body fails only the ranking request.
//!
//! A put counts only from the address its pid is logged in to GPCM from
//! (`docs/gamestats2.md`, "Trust"): the pid alone is anyone's to send.

mod codec;
mod leaderboard;
pub(crate) mod submission;
mod token;

#[cfg(test)]
mod fixtures;

use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use axum::{
    Router,
    extract::{ConnectInfo, Path, Query, RawQuery, State, rejection::QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use tracing::{trace, warn};

use crate::{GAME_NAME, gpcm::Gpcm};
use leaderboard::Leaderboard;
use token::TokenStore;

/// Signs tokens and responses: the first 20 characters of the init data the
/// game passes to `DWC_RnkInitialize`.
const GAMESTATS_KEY: &str = "DbfBialvJznkQWYuOrRa";

/// State shared by every gamestats2 request. Cloned per request, so it holds
/// `Arc`s (the database pool has its own inside).
#[derive(Clone)]
pub struct GamestatsState {
    tokens: Arc<TokenStore>,
    leaderboard: Leaderboard,
    /// Who is logged in where: the puts they may send.
    gpcm: Arc<Gpcm>,
}

impl GamestatsState {
    fn new(pool: sqlx::PgPool, gpcm: Arc<Gpcm>) -> Self {
        Self {
            tokens: Arc::new(TokenStore::default()),
            leaderboard: Leaderboard::new(pool),
            gpcm,
        }
    }
}

#[cfg(test)]
impl GamestatsState {
    /// With a GPCM of its own, which nobody is logged in to yet.
    fn with_pool(pool: sqlx::PgPool) -> Self {
        let gpcm = Gpcm::new(pool.clone(), Arc::default(), Arc::default());
        Self::new(pool, Arc::new(gpcm))
    }

    /// For tests that never touch the database: the pool connects on the first
    /// query, so none is needed until then.
    fn without_database() -> Self {
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

        Self::with_pool(PgPoolOptions::new().connect_lazy_with(PgConnectOptions::new()))
    }
}

/// The routes. They need the caller's address: serve them with
/// `into_make_service_with_connect_info::<SocketAddr>()`.
pub fn router(pool: sqlx::Pool<sqlx::Postgres>, gpcm: Arc<Gpcm>) -> Router {
    Router::new()
        .route("/{game}/web/client/{endpoint}", get(handle))
        .with_state(GamestatsState::new(pool, gpcm))
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Get,
    Put,
}

impl Op {
    fn from_endpoint(endpoint: &str) -> Option<Self> {
        match endpoint {
            "get2.asp" => Some(Self::Get),
            "put2.asp" => Some(Self::Put),
            _ => None,
        }
    }
}

/// The query string. Every field is optional, so deserializing fails only on
/// a malformed query; missing or invalid values become [`GamestatsError`]s.
#[derive(Debug, Deserialize)]
struct Params {
    pid: Option<String>,
    hash: Option<String>,
    data: Option<String>,
}

async fn handle(
    State(state): State<GamestatsState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((game, endpoint)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
    params: Result<Query<Params>, QueryRejection>,
) -> Response {
    trace!(target: "raw::gamestats", %game, %endpoint, query = raw.as_deref().unwrap_or(""), "request");

    let Some(op) = Op::from_endpoint(&endpoint).filter(|_| game == GAME_NAME) else {
        // Another game or endpoint: no client of this game asks for it.
        return StatusCode::NOT_FOUND.into_response();
    };

    match process(&state, op, peer.ip(), params).await {
        Ok(body) => {
            trace!(target: "raw::gamestats", ?op, reply = %hex(&body), "reply");
            body.into_response()
        }
        Err(error) => error.into_response(),
    }
}

async fn process(
    state: &GamestatsState,
    op: Op,
    peer: IpAddr,
    params: Result<Query<Params>, QueryRejection>,
) -> Result<Vec<u8>, GamestatsError> {
    let Query(params) =
        params.map_err(|rejection| GamestatsError::BadQuery(rejection.body_text()))?;
    let pid: i32 = params
        .pid
        .ok_or_else(|| GamestatsError::BadQuery("missing pid".into()))?
        .parse()
        .map_err(|_| GamestatsError::BadQuery("invalid pid".into()))?;

    let (hash, data) = match (params.hash, params.data) {
        // Request 1: only the pid. Answer with a token.
        (None, None) => return Ok(state.tokens.issue(pid).into_bytes()),
        (Some(hash), Some(data)) => (hash, data),
        _ => return Err(GamestatsError::BadQuery("hash and data go together".into())),
    };

    // Request 2: the hash proves the client got our token for this pid.
    state.tokens.redeem(pid, &hash)?;

    let (data_pid, body) = codec::decode_data(&data)?;
    if data_pid != pid {
        return Err(GamestatsError::PidMismatch);
    }

    let payload = match op {
        Op::Put => {
            let logged_in_from = state.gpcm.logged_in_from(pid);
            if logged_in_from != Some(peer) {
                warn!(target: "gamestats", pid, %peer, ?logged_in_from, "put from elsewhere");
                return Err(GamestatsError::NotLoggedIn);
            }
            leaderboard::put(state, pid, &body).await?
        }
        Op::Get => leaderboard::get(state, pid, &body).await?,
    };
    Ok(codec::sign(payload))
}

/// Why a request was refused. Sent to the client as `error:<reason>` with
/// status 200.
#[derive(Debug)]
pub enum GamestatsError {
    /// The query string is malformed, or misses or garbles a field.
    BadQuery(String),
    /// No token was issued to this pid, or it expired.
    NoToken,
    /// The hash does not match the token issued to this pid.
    BadHash,
    /// The pid inside `data` differs from the one in the query.
    PidMismatch,
    /// A put for a pid that is not logged in to GPCM from the same address.
    NotLoggedIn,
    /// `data` Base64 is malformed or invalid,
    BadData,
    /// The database failed. The details go to the log, not to the client.
    Storage,
}

impl fmt::Display for GamestatsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadQuery(reason) => write!(f, "bad query: {reason}"),
            Self::NoToken => f.write_str("no token issued to this pid, or it expired"),
            Self::BadHash => f.write_str("bad hash"),
            Self::PidMismatch => f.write_str("pid in data differs from the query"),
            Self::NotLoggedIn => f.write_str("pid not logged in from this address"),
            Self::BadData => f.write_str("bad data"),
            Self::Storage => f.write_str("storage"),
        }
    }
}

impl std::error::Error for GamestatsError {}

impl From<sqlx::Error> for GamestatsError {
    fn from(error: sqlx::Error) -> Self {
        warn!(target: "gamestats", %error, "database error");
        Self::Storage
    }
}

impl IntoResponse for GamestatsError {
    fn into_response(self) -> Response {
        warn!(target: "gamestats", error = %self, "request refused");
        format!("error:{self}").into_response()
    }
}

/// Lowercase hex, as the client prints its hashes.
fn hex(bytes: &[u8]) -> String {
    use fmt::Write;

    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

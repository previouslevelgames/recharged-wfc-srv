//! Players' names (docs/profiles.md, "Names"): one name, one player, in any
//! letter case. The launcher asks whether a name is free while it is typed,
//! and a player can rename themselves; the old name is free at once.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use super::mii_sign_in::{ClientAddress, MAX_NAME, NAME_TAKEN, PerAddress, profile_name};
use super::{ProfilesState, database_error, json_error, players};

/// Name asks one address can make in [`ASK_WINDOW`]: the launcher asks once a
/// typed name rests for a moment.
pub const ASKS_PER_WINDOW: u32 = 60;
const ASK_WINDOW: Duration = Duration::from_secs(60);

/// [`free`]'s asks: [`ASKS_PER_WINDOW`] a minute.
pub fn asks() -> PerAddress {
    PerAddress::new(ASKS_PER_WINDOW, ASK_WINDOW)
}

fn invalid() -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        &format!("a name has 1 to {MAX_NAME} characters, without control characters"),
    )
}

#[derive(Deserialize)]
pub(super) struct NameQuery {
    name: String,
}

/// `GET /api/v1/names?name=`: whether a player could take that name now, as
/// the launcher shows it while it is typed. Only a hint: making or renaming
/// a player takes the name, and says so if it was taken meanwhile.
pub(super) async fn free(
    State(state): State<Arc<ProfilesState>>,
    ClientAddress(address): ClientAddress,
    Query(query): Query<NameQuery>,
) -> Response {
    if !state.name_asks.allow(address) {
        return json_error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many asks from this address; try again in a minute",
        );
    }
    let Some(name) = profile_name(&query.name) else {
        return invalid();
    };
    match players::name_taken(&state.pool, &name).await {
        Ok(taken) => Json(json!({ "name": name, "free": !taken })).into_response(),
        Err(error) => database_error(error),
    }
}

#[derive(Deserialize)]
pub(super) struct Rename {
    name: String,
}

/// `PUT /api/v1/me/name`: the player is called `name` from now on. 409 when
/// another player is.
pub(super) async fn rename(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Json(rename): Json<Rename>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    let Some(name) = profile_name(&rename.name) else {
        return invalid();
    };
    match players::rename(&state.pool, player.id, &name).await {
        Ok(true) => {}
        Ok(false) => return json_error(StatusCode::CONFLICT, NAME_TAKEN),
        Err(error) => return database_error(error),
    }
    info!(target: "profiles", player_id = player.id, "renamed");
    match players::player(&state.pool, player.id).await {
        Ok(Some(player)) => Json(player).into_response(),
        Ok(None) => json_error(StatusCode::UNAUTHORIZED, "signed out"),
        Err(error) => database_error(error),
    }
}

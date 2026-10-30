//! The launcher's Discord friends (docs/profiles.md, "Discord friends"). The
//! launcher's Discord Social SDK reads the friends list from Discord itself;
//! the server trades the SDK's codes for tokens, as the confidential client
//! the SDK can't be, and tells which of those friends play Recharged. It
//! keeps neither tokens nor friends lists.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

use super::{
    ProfilesState, database_error,
    discord::{self, SdkGrant, TokenError},
    json_error,
    players::{self, MAX_DISCORD_LOOKUP},
};

/// The longest code, verifier, redirect or refresh token passed on.
const MAX_FIELD: usize = 512;

#[derive(Deserialize)]
pub(super) struct TokenRequest {
    /// The SDK authorization's code, its PKCE verifier and its redirect...
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    code_verifier: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    /// ...or the refresh token of an earlier answer.
    #[serde(default)]
    refresh_token: Option<String>,
}

/// Trades a code or refresh token for the SDK's tokens, for the Discord
/// account the player signed in with.
pub(super) async fn discord_token(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Json(request): Json<TokenRequest>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    let Some(client) = state.config.discord.as_ref() else {
        return json_error(StatusCode::NOT_FOUND, "this server has no Discord sign-in");
    };
    let fields = [
        &request.code,
        &request.code_verifier,
        &request.redirect_uri,
        &request.refresh_token,
    ];
    if fields
        .iter()
        .any(|field| field.as_ref().is_some_and(|value| value.len() > MAX_FIELD))
    {
        return json_error(StatusCode::BAD_REQUEST, "a field is too long");
    }
    let grant = match fields {
        [Some(code), Some(verifier), Some(redirect_uri), None] => {
            if !discord::is_sdk_redirect(redirect_uri) {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "the redirect is not the Discord SDK's",
                );
            }
            SdkGrant::Code {
                code,
                verifier,
                redirect_uri,
            }
        }
        [None, None, None, Some(refresh_token)] => SdkGrant::Refresh { refresh_token },
        _ => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "send a code with its verifier and redirect, or a refresh token",
            );
        }
    };
    let grant_name = match grant {
        SdkGrant::Code { .. } => "code",
        SdkGrant::Refresh { .. } => "refresh",
    };
    let tokens = match discord::sdk_tokens(&state.http, client, grant).await {
        Ok(tokens) => tokens,
        Err(error @ TokenError::Refused(_)) => {
            info!(target: "profiles", player_id = player.id, grant = grant_name, %error, "Discord SDK tokens refused");
            return json_error(
                StatusCode::BAD_REQUEST,
                "Discord refused: connect Discord again",
            );
        }
        Err(error @ TokenError::Failed(_)) => {
            warn!(target: "profiles", player_id = player.id, grant = grant_name, %error, "Discord SDK tokens failed");
            return json_error(StatusCode::BAD_GATEWAY, "Discord could not be reached");
        }
    };
    let signed_in_with = match players::discord_id(&state.pool, player.id).await {
        Ok(id) => id,
        Err(error) => return database_error(error),
    };
    if let (Some(account), Some(signed_in_with)) = (tokens.account_id(), signed_in_with)
        && account != signed_in_with
    {
        info!(target: "profiles", player_id = player.id, "Discord SDK tokens for another account");
        let name = player.discord.as_deref().unwrap_or(&player.name);
        return json_error(
            StatusCode::FORBIDDEN,
            &format!("that is another Discord account: connect {name}, whom you signed in with"),
        );
    }
    info!(target: "profiles", player_id = player.id, grant = grant_name, "Discord SDK tokens");
    Json(tokens).into_response()
}

#[derive(Deserialize)]
pub(super) struct DiscordAccounts {
    ids: Vec<String>,
}

/// Which of the Discord accounts `ids` (the player's Discord friends) play
/// Recharged, as the player's lists show players.
pub(super) async fn players_by_discord(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Json(request): Json<DiscordAccounts>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if request.ids.len() > MAX_DISCORD_LOOKUP {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("at most {MAX_DISCORD_LOOKUP} Discord accounts at once"),
        );
    }
    if !request
        .ids
        .iter()
        .all(|id| (1..=20).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return json_error(StatusCode::BAD_REQUEST, "Discord account ids are numbers");
    }
    match players::by_discord(&state.pool, player.id, &request.ids).await {
        Ok(found) => Json(json!({ "players": found })).into_response(),
        Err(error) => database_error(error),
    }
}

//! The launcher's and the port's requests for playing (docs/profiles.md):
//! the rule sets, proving a WFC login, who is online, and sets from the
//! challenge through Ready to play! to the result.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tracing::info;

use super::{
    ProfilesState, chat, database_error, friends, json_error,
    matches::{self, Game, Match, Refusal},
    mii_sign_in::ClientAddress,
    miis,
    players::{self, Player},
    rules::{self, RuleSet, Rules},
    search,
};
use crate::pairing::Pairing;

/// The rule sets a challenge can pick.
pub(super) async fn rule_sets(State(state): State<Arc<ProfilesState>>) -> Response {
    match rules::list(&state.pool).await {
        Ok(sets) => Json(json!({ "rule_sets": sets })).into_response(),
        Err(error) => database_error(error),
    }
}

#[derive(Deserialize)]
pub(super) struct WfcLogin {
    pub(super) profile_id: i32,
    pub(super) login_ticket: String,
}

/// The port proves that its game is logged in to WFC as `profile_id`: that
/// WFC profile is the player's from now on. A Mii player's first one also
/// identifies it (docs/profiles.md, "Playing without Discord").
pub(super) async fn bind(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Json(login): Json<WfcLogin>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if !state
        .gpcm
        .logged_in_with(login.profile_id, &login.login_ticket)
    {
        return json_error(
            StatusCode::FORBIDDEN,
            "that profile is not logged in to WFC with this login ticket",
        );
    }
    if let Err(error) = players::bind_wfc(&state.pool, player.id, login.profile_id).await {
        return database_error(error);
    }
    info!(target: "profiles", player_id = player.id, profile_id = login.profile_id, "WFC profile bound");
    let profile = match players::wfc_profiles(&state.pool, player.id).await {
        Ok(profiles) => profiles
            .into_iter()
            .find(|profile| profile.profile_id == login.profile_id),
        Err(error) => return database_error(error),
    };
    let Some(profile) = profile else {
        return json_error(StatusCode::NOT_FOUND, "the profile went away");
    };
    if player.account == "mii" && player.friend_code.is_none() {
        match players::bind_identity(
            &state.pool,
            player.id,
            profile.profile_id,
            &profile.friend_code,
        )
        .await
        {
            Ok(true) => {
                info!(target: "profiles", player_id = player.id, profile_id = profile.profile_id, "WFC profile identifies the player")
            }
            Ok(false) => {}
            Err(error) => return database_error(error),
        }
    }
    Json(profile).into_response()
}

#[derive(Deserialize)]
pub(super) struct Challenge {
    /// The Discord username (or test name) of the player challenged...
    #[serde(default)]
    opponent: Option<String>,
    /// ...or their player id, from the lists of `GET /api/v1/players`.
    #[serde(default)]
    opponent_id: Option<i64>,
    /// One of the server's rule sets...
    #[serde(default)]
    rule_set: Option<i64>,
    /// ...or rules of the challenger's own, under `name` (the preset they
    /// started from; `Custom` without one).
    #[serde(default)]
    rules: Option<Rules>,
    #[serde(default)]
    name: Option<String>,
}

/// A challenge's or a search's rules: one of the server's rule sets, or rules
/// of the player's own under `name` (`Custom` without one).
async fn rules_of(
    state: &ProfilesState,
    own: Option<Rules>,
    rule_set: Option<i64>,
    name: Option<String>,
) -> Result<RuleSet, Response> {
    match (own, rule_set) {
        (Some(own), _) => {
            let name = name.as_deref().map(str::trim).unwrap_or("");
            let name = if name.is_empty() { "Custom" } else { name };
            if name.chars().count() > 48 {
                return Err(json_error(
                    StatusCode::BAD_REQUEST,
                    "name: up to 48 characters",
                ));
            }
            match own.check() {
                Ok(rules) => Ok(RuleSet {
                    id: 0,
                    name: name.to_owned(),
                    hash: rules.hash(),
                    rules,
                }),
                Err(problem) => Err(json_error(StatusCode::BAD_REQUEST, &problem)),
            }
        }
        (None, Some(id)) => match rules::get(&state.pool, id).await {
            Ok(Some(rule_set)) => Ok(rule_set),
            Ok(None) => Err(json_error(StatusCode::NOT_FOUND, "no such rule set")),
            Err(error) => Err(database_error(error)),
        },
        (None, None) => Err(json_error(StatusCode::BAD_REQUEST, "which rules?")),
    }
}

#[derive(Deserialize)]
pub(super) struct SearchRequest {
    /// The rules, as a challenge's.
    #[serde(default)]
    rule_set: Option<i64>,
    #[serde(default)]
    rules: Option<Rules>,
    #[serde(default)]
    name: Option<String>,
    /// The searcher's game: the first four characters of its id (R4QE).
    game: String,
}

/// The player searches for a random opponent by their rules (docs/profiles.md,
/// "Random opponents"); again: starts over with these rules.
pub(super) async fn search_start(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Json(request): Json<SearchRequest>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if let Err(response) = state.sweep().await {
        return response;
    }
    if request.game.len() != 4
        || !request
            .game
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
    {
        return json_error(
            StatusCode::BAD_REQUEST,
            "game: the first four characters of the game's id",
        );
    }
    let rule_set = match rules_of(&state, request.rules, request.rule_set, request.name).await {
        Ok(rule_set) => rule_set,
        Err(response) => return response,
    };
    match matches::current(&state.pool, player.id).await {
        Ok(Some(found)) if found.is_open() => {
            return json_error(StatusCode::CONFLICT, "you have a set to play");
        }
        Ok(_) => {}
        Err(error) => return database_error(error),
    }
    if let Err(error) = search::start(
        &state.pool,
        player.id,
        &rule_set,
        &request.game,
        &state.busy_profiles(),
    )
    .await
    {
        return database_error(error);
    }
    info!(target: "profiles", player = player.id, rule_set = rule_set.name, "search");
    match search::get(&state.pool, player.id).await {
        Ok(found) => Json(json!({ "search": found })).into_response(),
        Err(error) => database_error(error),
    }
}

/// The player stops searching; a random challenge still waiting for an answer
/// is withdrawn.
pub(super) async fn search_stop(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    match search::stop(&state.pool, player.id).await {
        Ok(withdrawn) => {
            if let Some(match_id) = withdrawn {
                state.pairings.end(match_id);
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => database_error(error),
    }
}

pub(super) async fn challenge(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Json(request): Json<Challenge>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if let Err(response) = state.sweep().await {
        return response;
    }
    let opponent = match (&request.opponent_id, &request.opponent) {
        (Some(id), _) => match players::player(&state.pool, *id).await {
            Ok(Some(found)) => found.id,
            Ok(None) => return json_error(StatusCode::NOT_FOUND, "no such player"),
            Err(error) => return database_error(error),
        },
        (None, Some(name)) => match players::find(&state.pool, name).await {
            Ok(Some(opponent)) => opponent,
            Ok(None) => {
                return json_error(
                    StatusCode::NOT_FOUND,
                    &format!("nobody called {} has signed in to Recharged", name.trim()),
                );
            }
            Err(error) => return database_error(error),
        },
        (None, None) => return json_error(StatusCode::BAD_REQUEST, "whom to challenge?"),
    };
    if opponent == player.id {
        return json_error(StatusCode::BAD_REQUEST, "you can't challenge yourself");
    }
    // A set lasts while both have Recharged open; a match in the game is
    // played to its end first.
    match players::activity(&state.pool, opponent).await {
        Ok(Some(players::Activity::Match)) => {
            return json_error(
                StatusCode::CONFLICT,
                "that player is playing a match right now",
            );
        }
        Ok(Some(_)) => {}
        Ok(None) => {
            return json_error(
                StatusCode::CONFLICT,
                "that player doesn't have Recharged open right now",
            );
        }
        Err(error) => return database_error(error),
    }
    let rule_set = match rules_of(&state, request.rules, request.rule_set, request.name).await {
        Ok(rule_set) => rule_set,
        Err(response) => return response,
    };
    match matches::challenge(&state.pool, player.id, opponent, &rule_set, None, false).await {
        Ok(Ok(found)) => {
            info!(target: "profiles", match_id = found.id, home = player.id, away = opponent, rule_set = found.rule_set, "challenge");
            state.respond(StatusCode::CREATED, &found, &player).await
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => database_error(error),
    }
}

/// The player's match still to be played, or the last one that ended, with
/// how soon to ask again (seconds) and the server's clock.
pub(super) async fn current(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if let Err(response) = state.sweep().await {
        return response;
    }
    let server_time = OffsetDateTime::now_utc().unix_timestamp();
    // The player's search for a random opponent, if they search.
    let searching = match search::get(&state.pool, player.id).await {
        Ok(found) => found,
        Err(error) => return database_error(error),
    };
    match matches::current(&state.pool, player.id).await {
        Ok(Some(found)) => {
            // The lobby follows a set waiting for its players closely.
            let poll_interval = match found.stage() {
                "invited" | "open" | "go" => 2,
                "playing" => 10,
                _ => 5,
            };
            match state.view(&found, &player).await {
                Ok(view) => Json(json!({
                    "match": view,
                    "search": searching,
                    "poll_interval": if searching.is_some() { 2 } else { poll_interval },
                    "server_time": server_time,
                }))
                .into_response(),
                Err(error) => database_error(error),
            }
        }
        Ok(None) => Json(json!({
            "match": null,
            "search": searching,
            "poll_interval": if searching.is_some() { 2 } else { 5 },
            "server_time": server_time,
        }))
        .into_response(),
        Err(error) => database_error(error),
    }
}

/// Who has Recharged open, the players last played with, and the player's
/// friends.
pub(super) async fn players(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    ClientAddress(address): ClientAddress,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if let Err(response) = state.observe_country(player.id, address).await {
        return response;
    }
    if let Err(response) = state.sweep().await {
        return response;
    }
    let online = match players::online(&state.pool, player.id).await {
        Ok(online) => online,
        Err(error) => return database_error(error),
    };
    let recent = match players::recent_opponents(&state.pool, player.id).await {
        Ok(recent) => recent,
        Err(error) => return database_error(error),
    };
    // The players available for random games, and the game's own online
    // players, only for one who is available too.
    let presence = state.gpcm.presence();
    let (available, game_online) = match players::available(&state.pool, player.id).await {
        Ok(true) => {
            let available = match players::available_now(&state.pool, player.id).await {
                Ok(available) => available,
                Err(error) => return database_error(error),
            };
            match miis::online(&state.pool, player.id, &presence).await {
                Ok(game_online) => (available, game_online),
                Err(error) => return database_error(error),
            }
        }
        Ok(false) => (Vec::new(), Vec::new()),
        Err(error) => return database_error(error),
    };
    let mii_friends = match miis::friends(&state.pool, player.id, &presence).await {
        Ok(mii_friends) => mii_friends,
        Err(error) => return database_error(error),
    };
    match friends::list(&state.pool, player.id).await {
        Ok(friends) => Json(json!({
            "online": online,
            "recent": recent,
            "friends": friends,
            "available": available,
            "mii_friends": mii_friends,
            "game_online": game_online,
        }))
        .into_response(),
        Err(error) => database_error(error),
    }
}

#[derive(Deserialize)]
pub(super) struct MiiFriend {
    profile_id: i32,
}

/// Adds a Mii profile to the friends of the one who asks; the answer is the
/// Mii friends.
pub(super) async fn add_mii_friend(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Json(request): Json<MiiFriend>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    match miis::add(&state.pool, player.id, request.profile_id).await {
        Ok(None) => return json_error(StatusCode::NOT_FOUND, "no such profile"),
        Ok(Some(false)) => {
            // Already there, or the list is full.
            let count = sqlx::query_scalar!(
                r#"SELECT count(*) AS "count!" FROM friend_profiles WHERE player_id = $1"#,
                player.id
            )
            .fetch_one(&state.pool)
            .await;
            if matches!(count, Ok(count) if count >= friends::MAX_FRIENDS) {
                return json_error(StatusCode::CONFLICT, "the friends list is full");
            }
        }
        Ok(Some(true)) => {}
        Err(error) => return database_error(error),
    }
    mii_friends(&state, player.id).await
}

/// Takes a Mii profile off the friends of the one who asks.
pub(super) async fn remove_mii_friend(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(profile_id): Path<i32>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if let Err(error) = miis::remove(&state.pool, player.id, profile_id).await {
        return database_error(error);
    }
    mii_friends(&state, player.id).await
}

async fn mii_friends(state: &ProfilesState, player_id: i64) -> Response {
    match miis::friends(&state.pool, player_id, &state.gpcm.presence()).await {
        Ok(mii_friends) => Json(json!({ "mii_friends": mii_friends })).into_response(),
        Err(error) => database_error(error),
    }
}

#[derive(Deserialize)]
pub(super) struct Available {
    available: bool,
}

/// Switches the player's availability for random games.
pub(super) async fn set_available(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Json(request): Json<Available>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    match players::set_available(&state.pool, player.id, request.available).await {
        Ok(()) => Json(json!({ "available": request.available })).into_response(),
        Err(error) => database_error(error),
    }
}

#[derive(Deserialize)]
pub(super) struct Friend {
    /// A player id from the lists of `GET /api/v1/players`...
    #[serde(default)]
    player_id: Option<i64>,
    /// ...or a Discord username (or test name).
    #[serde(default)]
    name: Option<String>,
}

/// Adds a player to the friends of the one who asks; the answer is the list.
pub(super) async fn add_friend(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Json(request): Json<Friend>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    let friend = match (request.player_id, &request.name) {
        (Some(id), _) => match players::player(&state.pool, id).await {
            Ok(Some(found)) => found.id,
            Ok(None) => return json_error(StatusCode::NOT_FOUND, "no such player"),
            Err(error) => return database_error(error),
        },
        (None, Some(name)) => match players::find(&state.pool, name).await {
            Ok(Some(found)) => found,
            Ok(None) => {
                return json_error(
                    StatusCode::NOT_FOUND,
                    &format!("nobody called {} has signed in to Recharged", name.trim()),
                );
            }
            Err(error) => return database_error(error),
        },
        (None, None) => return json_error(StatusCode::BAD_REQUEST, "whom to add?"),
    };
    if friend == player.id {
        return json_error(StatusCode::BAD_REQUEST, "you can't add yourself");
    }
    match friends::add(&state.pool, player.id, friend).await {
        Ok(true) => {
            info!(target: "profiles", player_id = player.id, friend_id = friend, "friend added")
        }
        Ok(false) => {
            // Already a friend, or the list is full.
            match friends::list(&state.pool, player.id).await {
                Ok(list) if !list.iter().any(|listed| listed.player.id == friend) => {
                    return json_error(
                        StatusCode::CONFLICT,
                        &format!(
                            "you have {} friends already: remove one first",
                            friends::MAX_FRIENDS
                        ),
                    );
                }
                Ok(_) => {}
                Err(error) => return database_error(error),
            }
        }
        Err(error) => return database_error(error),
    }
    friends_answer(&state, player.id).await
}

/// Takes a player off the friends of the one who asks; the answer is the list.
pub(super) async fn remove_friend(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    match friends::remove(&state.pool, player.id, id).await {
        Ok(true) => {
            info!(target: "profiles", player_id = player.id, friend_id = id, "friend removed")
        }
        Ok(false) => {}
        Err(error) => return database_error(error),
    }
    friends_answer(&state, player.id).await
}

async fn friends_answer(state: &ProfilesState, player_id: i64) -> Response {
    match friends::list(&state.pool, player_id).await {
        Ok(list) => Json(json!({ "friends": list })).into_response(),
        Err(error) => database_error(error),
    }
}

#[derive(Deserialize)]
pub(super) struct ChatQuery {
    /// The last message the launcher has: only later ones.
    #[serde(default)]
    after: Option<i64>,
}

/// The messages with player `id` (docs/profiles.md, "Chat"), and whether
/// the two may write (they keep each other as friends).
pub(super) async fn chat_messages(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<ChatQuery>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    let may = match chat::may_chat(&state.pool, player.id, id).await {
        Ok(may) => may,
        Err(error) => return database_error(error),
    };
    match chat::messages(&state.pool, player.id, id, query.after).await {
        Ok(messages) => Json(json!({ "messages": messages, "can_send": may })).into_response(),
        Err(error) => database_error(error),
    }
}

#[derive(Deserialize)]
pub(super) struct ChatMessage {
    body: String,
}

/// A message to player `id`, who must keep the player as a friend, as the
/// player keeps them.
pub(super) async fn send_chat(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<ChatMessage>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    let Some(body) = chat::clean(&request.body) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("a message has 1 to {} characters", chat::MAX_LENGTH),
        );
    };
    match chat::may_chat(&state.pool, player.id, id).await {
        Ok(true) => {}
        Ok(false) => {
            return json_error(
                StatusCode::FORBIDDEN,
                "you can write to friends who have you as a friend too",
            );
        }
        Err(error) => return database_error(error),
    }
    match chat::sent_last_minute(&state.pool, player.id).await {
        Ok(sent) if sent >= chat::PER_MINUTE => {
            return json_error(
                StatusCode::TOO_MANY_REQUESTS,
                "that's a lot of messages: wait a minute",
            );
        }
        Ok(_) => {}
        Err(error) => return database_error(error),
    }
    match chat::send(&state.pool, player.id, id, &body).await {
        Ok(message) => (StatusCode::CREATED, Json(message)).into_response(),
        Err(error) => database_error(error),
    }
}

pub(super) async fn accept(
    state: State<Arc<ProfilesState>>,
    headers: HeaderMap,
    id: Path<i64>,
) -> Response {
    answer(state, headers, id, true).await
}

pub(super) async fn decline(
    state: State<Arc<ProfilesState>>,
    headers: HeaderMap,
    id: Path<i64>,
) -> Response {
    answer(state, headers, id, false).await
}

async fn answer(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    accept: bool,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if let Err(response) = state.sweep().await {
        return response;
    }
    match matches::answer(&state.pool, id, player.id, accept).await {
        Ok(Ok(found)) => {
            info!(target: "profiles", match_id = id, state = found.state, "answered");
            if !accept {
                state.pairings.end(id);
            }
            state.respond(StatusCode::OK, &found, &player).await
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => database_error(error),
    }
}

pub(super) async fn cancel(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    match matches::cancel(&state.pool, id, player.id).await {
        Ok(Ok(found)) => {
            info!(target: "profiles", match_id = id, player_id = player.id, "cancelled");
            state.pairings.end(id);
            state.respond(StatusCode::OK, &found, &player).await
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => database_error(error),
    }
}

#[derive(Deserialize)]
pub(super) struct ReadyToPlay {
    ready: bool,
}

/// The player presses (or takes back) Ready to play! in the launcher. Once
/// both are, the set's stage is `go` and both launchers start their games.
pub(super) async fn ready_to_play(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<ReadyToPlay>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if let Err(response) = state.sweep().await {
        return response;
    }
    match matches::ready_to_play(&state.pool, id, player.id, request.ready).await {
        Ok(Ok(found)) => {
            info!(target: "profiles", match_id = id, player_id = player.id, ready = request.ready, stage = found.stage(), "ready to play");
            // Taken back: the player's game, which may have said it is ready
            // a moment ago, must not pair the set with the opponent's.
            if !request.ready
                && let Some(side) = found.side(player.id)
            {
                state.pairings.unready(id, side);
            }
            state.respond(StatusCode::OK, &found, &player).await
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => database_error(error),
    }
}

/// The player plays the ended set again with the same opponent and rules.
pub(super) async fn rematch(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if let Err(response) = state.sweep().await {
        return response;
    }
    match matches::rematch(&state.pool, id, player.id).await {
        Ok(Ok(found)) => {
            info!(target: "profiles", match_id = found.id, rematch_of = id, player_id = player.id, stage = found.stage(), "rematch");
            state.respond(StatusCode::OK, &found, &player).await
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => database_error(error),
    }
}

#[derive(Deserialize)]
pub(super) struct Ready {
    /// The WFC profile the game is logged in as, proven with
    /// `POST /api/v1/wfc`.
    profile_id: i32,
    /// The hash of the rules the game applies.
    rules_hash: String,
}

/// The player's game is online and plays by the match's rules: keep it for
/// the match in RANKED. The port says it again every few seconds.
pub(super) async fn ready(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<Ready>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    let found = match matches::get(&state.pool, id, player.id).await {
        Ok(Some(found)) if found.state == "accepted" => found,
        Ok(Some(found)) => return refused(Refusal::State(found.state)),
        Ok(None) => return refused(Refusal::NotFound),
        Err(error) => return database_error(error),
    };
    let side = found.side(player.id).expect("the match is the player's");
    // The game still plays the set, even if the checks below refuse it (a
    // GPCM login lost after a restart, while the match goes on): the sweep
    // ends a set only once its game stopped calling.
    if let Err(error) = matches::game_called(&state.pool, found.id, side).await {
        return database_error(error);
    }
    if request.rules_hash != found.rules_hash {
        return json_error(
            StatusCode::CONFLICT,
            "the game applies other rules than the match's",
        );
    }
    let profile = match players::wfc_profiles(&state.pool, player.id).await {
        Ok(profiles) => profiles
            .into_iter()
            .find(|profile| profile.profile_id == request.profile_id),
        Err(error) => return database_error(error),
    };
    let Some(profile) = profile else {
        return json_error(
            StatusCode::CONFLICT,
            "that WFC profile is not yours: prove its login first",
        );
    };
    if state.gpcm.logged_in_from(profile.profile_id).is_none() {
        return json_error(StatusCode::CONFLICT, "that WFC profile is not online");
    }
    let pairing = state
        .pairings
        .ready(found.id, side, profile.profile_id, &profile.game);
    // Another game of the player's took the set: this one leaves it.
    if pairing != Pairing::Replaced
        && let Err(error) =
            matches::game_ready(&state.pool, found.id, side, pairing == Pairing::Paired).await
    {
        return database_error(error);
    }
    Json(json!({
        "paired": pairing == Pairing::Paired,
        "replaced": pairing == Pairing::Replaced,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct Report {
    games: Vec<Game>,
}

/// The player's game reports the series it played.
pub(super) async fn report(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<Report>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    match matches::report(&state.pool, id, player.id, &request.games).await {
        Ok(Ok(found)) => {
            info!(target: "profiles", match_id = id, player_id = player.id, state = found.state, "reported");
            if !found.is_open() {
                state.pairings.end(id);
            }
            state.respond(StatusCode::OK, &found, &player).await
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => database_error(error),
    }
}

impl ProfilesState {
    /// Ends the sets that waited too long or lost a player, and their
    /// pairings.
    async fn sweep(&self) -> Result<(), Response> {
        for match_id in matches::sweep(&self.pool).await.map_err(database_error)? {
            self.pairings.end(match_id);
        }
        // Searches ask their next player.
        search::advance(&self.pool, &self.busy_profiles())
            .await
            .map_err(database_error)?;
        Ok(())
    }

    /// The WFC profiles in the game's own online matchmaking or match now
    /// (DWC's status 2 to 6): their players are busy for searches.
    fn busy_profiles(&self) -> Vec<i32> {
        self.gpcm
            .presence()
            .into_iter()
            .filter(|(_, presence)| matches!(presence.status, Some(2..=6)))
            .map(|(profile_id, _)| profile_id)
            .collect()
    }

    async fn respond(&self, status: StatusCode, found: &Match, player: &Player) -> Response {
        match self.view(found, player).await {
            Ok(view) => (status, Json(view)).into_response(),
            Err(error) => database_error(error),
        }
    }

    /// `found` as `player` sees it (docs/profiles.md, "Matches").
    async fn view(&self, found: &Match, player: &Player) -> Result<Value, sqlx::Error> {
        let opponent_id = found.opponent(player.id);
        let opponent = players::player(&self.pool, opponent_id).await?;
        let opponent_wfc = players::wfc_profiles(&self.pool, opponent_id).await?;
        let opponent_online = players::is_online(&self.pool, opponent_id).await?;
        let reported = matches::reported(&self.pool, found.id, player.id).await?;
        let ended = matches!(found.state.as_str(), "finished" | "disputed");
        let games = if ended {
            matches::games(&self.pool, found.id, player.id).await?
        } else {
            Vec::new()
        };
        let winner = match (found.state.as_str(), found.winner) {
            ("finished", Some(winner)) if winner == player.id => json!("you"),
            ("finished", Some(_)) => json!("opponent"),
            ("finished", None) => json!("draw"),
            _ => Value::Null,
        };
        let side = found.side(player.id).unwrap_or(0);
        let unix = |time: Option<OffsetDateTime>| time.map(OffsetDateTime::unix_timestamp);
        let stage = found.stage();
        Ok(json!({
            "id": found.id,
            "state": found.state,
            "stage": stage,
            "reason": (stage == "dissolved").then_some(found.state.as_str()),
            "side": if found.home == player.id { "challenger" } else { "challenged" },
            "opponent": opponent.map(|opponent| json!({
                "id": opponent.id,
                "name": opponent.name,
                "avatar_url": opponent.avatar_url,
                "discord": opponent.discord,
                "account": opponent.account,
                "friend_code": opponent.friend_code,
                "wfc": opponent_wfc,
            })),
            "rule_set": found.rule_set,
            "rules": found.rules,
            "rules_hash": found.rules_hash,
            "paired": self.pairings.pairing(found.id) == Pairing::Paired,
            "reported": reported,
            "winner": winner,
            "games": games,
            "created_at": found.created_at.unix_timestamp(),
            "you_ready": found.ready_at[side].is_some(),
            "opponent_ready": found.ready_at[1 - side].is_some(),
            "opponent_online": opponent_online,
            "go_at": unix(found.go_at),
            "playing_at": unix(found.playing_at),
            "rematch_of": found.rematch_of,
            "random": found.random,
        }))
    }
}

fn refused(refusal: Refusal) -> Response {
    match refusal {
        Refusal::NotFound => json_error(StatusCode::NOT_FOUND, "no such match of yours"),
        Refusal::State(state) => json_error(StatusCode::CONFLICT, &format!("the match is {state}")),
        Refusal::NotYours => json_error(
            StatusCode::FORBIDDEN,
            "only the challenged player answers a challenge",
        ),
        Refusal::Busy => json_error(
            StatusCode::CONFLICT,
            "you or your opponent already have a match to play",
        ),
        Refusal::BadReport(problem) => json_error(StatusCode::BAD_REQUEST, &problem),
        Refusal::Reported => json_error(StatusCode::CONFLICT, "your game has reported this match"),
    }
}

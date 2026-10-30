//! The launcher's and the port's requests for playing (docs/profiles.md):
//! the rule sets, proving a WFC login, who is online, and sets from the
//! challenge through Ready to play! to the result.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tracing::{info, warn};

use super::{
    ProfilesState, database_error, friends, json_error,
    matches::{self, Game, Match, Refusal},
    players::{self, Player},
    rules,
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
    profile_id: i32,
    login_ticket: String,
}

/// The port proves that its game is logged in to WFC as `profile_id`: that
/// WFC profile is the player's from now on.
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
    match players::wfc_profiles(&state.pool, player.id).await {
        Ok(profiles) => match profiles
            .into_iter()
            .find(|profile| profile.profile_id == login.profile_id)
        {
            Some(profile) => Json(profile).into_response(),
            None => json_error(StatusCode::NOT_FOUND, "the profile went away"),
        },
        Err(error) => database_error(error),
    }
}

#[derive(Deserialize)]
pub(super) struct Challenge {
    /// The Discord username (or test name) of the player challenged...
    #[serde(default)]
    opponent: Option<String>,
    /// ...or their player id, from the lists of `GET /api/v1/players`.
    #[serde(default)]
    opponent_id: Option<i64>,
    rule_set: i64,
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
    // A set lasts while both have Recharged open.
    match players::is_online(&state.pool, opponent).await {
        Ok(true) => {}
        Ok(false) => {
            return json_error(
                StatusCode::CONFLICT,
                "that player doesn't have Recharged open right now",
            );
        }
        Err(error) => return database_error(error),
    }
    let rule_set = match rules::get(&state.pool, request.rule_set).await {
        Ok(Some(rule_set)) => rule_set,
        Ok(None) => return json_error(StatusCode::NOT_FOUND, "no such rule set"),
        Err(error) => return database_error(error),
    };
    match matches::challenge(&state.pool, player.id, opponent, &rule_set, None).await {
        Ok(Ok(found)) => {
            info!(target: "profiles", match_id = found.id, home = player.id, away = opponent, rule_set = found.rule_set, "challenge");
            befriend(&state, player.id, opponent).await;
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
                    "poll_interval": poll_interval,
                    "server_time": server_time,
                }))
                .into_response(),
                Err(error) => database_error(error),
            }
        }
        Ok(None) => Json(json!({ "match": null, "poll_interval": 5, "server_time": server_time }))
            .into_response(),
        Err(error) => database_error(error),
    }
}

/// Who has Recharged open, the players last played with, and the player's
/// friends.
pub(super) async fn players(
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
    let online = match players::online(&state.pool, player.id).await {
        Ok(online) => online,
        Err(error) => return database_error(error),
    };
    let recent = match players::recent_opponents(&state.pool, player.id).await {
        Ok(recent) => recent,
        Err(error) => return database_error(error),
    };
    match friends::list(&state.pool, player.id).await {
        Ok(friends) => {
            Json(json!({ "online": online, "recent": recent, "friends": friends })).into_response()
        }
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

/// A challenge or an accepted one makes the two players friends of the one
/// who acted; a failure here costs only that.
async fn befriend(state: &ProfilesState, player_id: i64, friend_id: i64) {
    if let Err(error) = friends::add(&state.pool, player_id, friend_id).await {
        warn!(target: "profiles", player_id, friend_id, %error, "keeping a friend failed");
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
            if accept {
                befriend(&state, player.id, found.home).await;
            } else {
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
    let side = found.side(player.id).expect("the match is the player's");
    let pairing = state
        .pairings
        .ready(found.id, side, profile.profile_id, &profile.game);
    if let Err(error) =
        matches::game_ready(&state.pool, found.id, side, pairing == Pairing::Paired).await
    {
        return database_error(error);
    }
    Json(json!({
        "paired": pairing == Pairing::Paired,
        "other_release": pairing == Pairing::OtherRelease,
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
        Ok(())
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

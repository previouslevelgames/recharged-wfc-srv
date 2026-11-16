//! The launcher's and the game's requests about tournaments
//! (docs/tournaments.md, "Routes"): the list, one tournament with its
//! bracket, joining and leaving, and READY for a set. Only a player who
//! signed in with Discord plays one (a test player too, on a test server).

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

use super::{
    bracket::{self, Format},
    store::{self, Refusal, Set, Tournament},
};
use crate::profiles::{
    ProfilesState, bearer, database_error, json_error,
    players::{self, Player},
};

/// How soon the page should ask again while it is open.
const POLL_INTERVAL: u32 = 5;

/// Why `player` can't play a tournament, or none when they can: they need a
/// Discord account (a test player is let in on a test server).
async fn ineligible(state: &ProfilesState, player: &Player) -> Option<&'static str> {
    match player.account {
        "discord" => None,
        "test" if state.test_sign_in().await => None,
        _ => Some("discord_required"),
    }
}

fn discord_required() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "discord_required",
            "message": "tournaments are played with a Discord account: connect yours first",
        })),
    )
        .into_response()
}

fn refused(refusal: Refusal) -> Response {
    match refusal {
        Refusal::NotFound => json_error(StatusCode::NOT_FOUND, "no such tournament or set"),
        Refusal::State(state) => json_error(StatusCode::CONFLICT, &format!("it is {state}")),
        Refusal::Full => json_error(StatusCode::CONFLICT, "the tournament is full"),
        Refusal::Joined => json_error(StatusCode::CONFLICT, "you are in it already"),
        Refusal::NotIn => json_error(StatusCode::NOT_FOUND, "you are not in it"),
        Refusal::TooFew => json_error(StatusCode::CONFLICT, "it needs at least two entrants"),
        Refusal::BadSeeding => json_error(
            StatusCode::BAD_REQUEST,
            "the seeding names each entrant once",
        ),
        Refusal::Settled => json_error(
            StatusCode::CONFLICT,
            "a set after this one has a result already",
        ),
        Refusal::Busy => json_error(
            StatusCode::CONFLICT,
            "you or your opponent already have a match to play",
        ),
    }
}

/// The player of a request that may come without a session.
async fn maybe_signed_in(
    state: &ProfilesState,
    headers: &HeaderMap,
) -> Result<Option<Player>, Response> {
    if bearer(headers).is_none() {
        return Ok(None);
    }
    state.signed_in(headers).await.map(Some)
}

/// A tournament as the list and its page show it.
async fn summary(state: &ProfilesState, tournament: &Tournament) -> Result<Value, sqlx::Error> {
    let organizer = match tournament.organizer {
        Some(id) => players::player(&state.pool, id)
            .await?
            .map(|player| json!({ "id": player.id, "name": player.name })),
        None => None,
    };
    Ok(json!({
        "id": tournament.id,
        "name": tournament.name,
        "organizer": organizer,
        "format": tournament.format,
        "state": tournament.state,
        "rule_set": tournament.rule_set,
        "rules": tournament.rules,
        "best_of": tournament.best_of,
        "finals_best_of": tournament.finals_best_of,
        "max_entrants": tournament.max_entrants,
        "entrants": tournament.entrants,
        "starts_at": tournament.starts_at.map(|time| time.unix_timestamp()),
        "created_at": tournament.created_at.unix_timestamp(),
        "started_at": tournament.started_at.map(|time| time.unix_timestamp()),
        "ended_at": tournament.ended_at.map(|time| time.unix_timestamp()),
    }))
}

/// `GET /api/v1/tournaments`: open for entries or running, then the last
/// that ended; with a session, whether the player is in each.
pub(in crate::profiles) async fn list(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
) -> Response {
    let player = match maybe_signed_in(&state, &headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if let Err(response) = state.sweep().await {
        return response;
    }
    let tournaments = match store::list(&state.pool).await {
        Ok(tournaments) => tournaments,
        Err(error) => return database_error(error),
    };
    let mut listed = Vec::new();
    for tournament in &tournaments {
        let mut view = match summary(&state, tournament).await {
            Ok(view) => view,
            Err(error) => return database_error(error),
        };
        if let Some(player) = &player {
            let entrants = match store::entrants(&state.pool, tournament.id).await {
                Ok(entrants) => entrants,
                Err(error) => return database_error(error),
            };
            view["joined"] = json!(entrants.iter().any(|e| e.player_id == player.id));
        }
        listed.push(view);
    }
    Json(json!({ "tournaments": listed })).into_response()
}

/// `GET /api/v1/tournaments/<id>`: the tournament, its entrants, its
/// bracket's sets, and with a session the player's own place in it (`me`).
pub(in crate::profiles) async fn get(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let player = match maybe_signed_in(&state, &headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if let Err(response) = state.sweep().await {
        return response;
    }
    match page(&state, id, player.as_ref()).await {
        Ok(Some(view)) => Json(view).into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "no such tournament"),
        Err(error) => database_error(error),
    }
}

/// Where a set's two players come from, by the bracket: `{"seed": n}` (from
/// 0; a bye past the entrants), or `{"winner": <set id>}` / `{"loser": …}`.
fn sources(tournament: &Tournament, entrants: usize, sets: &[Set]) -> Vec<Value> {
    let Some(format) = Format::parse(tournament.format) else {
        return Vec::new();
    };
    if sets.is_empty() || !(2..=bracket::MAX_ENTRANTS).contains(&entrants) {
        return Vec::new();
    }
    let plan = bracket::plan(format, entrants);
    if plan.len() != sets.len() {
        return Vec::new();
    }
    let id = |index: usize| sets[index].id;
    plan.iter()
        .map(|set| {
            Value::Array(
                set.from
                    .iter()
                    .map(|from| match *from {
                        bracket::From::Seed(seed) => json!({ "seed": seed }),
                        bracket::From::Winner(at) => json!({ "winner": id(at) }),
                        bracket::From::Loser(at) => json!({ "loser": id(at) }),
                    })
                    .collect(),
            )
        })
        .collect()
}

fn set_view(set: &Set, from: Option<&Value>, seed_of: impl Fn(i64) -> Option<i32>) -> Value {
    json!({
        "from": from,
        "id": set.id,
        "index": set.index,
        "side": set.side,
        "round": set.round,
        "position": set.position,
        "games": set.games,
        "state": set.state,
        "home": set.home,
        "away": set.away,
        "winner": set.winner,
        "decided_by": set.decided_by,
        "home_wins": set.home_wins,
        "away_wins": set.away_wins,
        "match": set.match_id,
        "game_home": matches!(set.state.as_str(), "ready" | "playing").then(|| set.game_home(seed_of)).flatten(),
    })
}

async fn page(
    state: &ProfilesState,
    id: i64,
    player: Option<&Player>,
) -> Result<Option<Value>, sqlx::Error> {
    let Some(tournament) = store::get(&state.pool, id).await? else {
        return Ok(None);
    };
    let entrants = store::entrants(&state.pool, id).await?;
    let sets = store::sets(&state.pool, id).await?;
    let mut people: HashMap<i64, Player> = HashMap::new();
    for entrant in &entrants {
        if let Some(found) = players::player(&state.pool, entrant.player_id).await? {
            people.insert(found.id, found);
        }
    }
    let entrant_views: Vec<Value> = entrants
        .iter()
        .map(|entrant| {
            let found = people.get(&entrant.player_id);
            json!({
                "player": {
                    "id": entrant.player_id,
                    "name": found.map(|p| p.name.as_str()),
                    "avatar_url": found.and_then(|p| p.avatar_url.as_deref()),
                    "mii_data": found.and_then(|p| p.mii_data.as_deref()),
                    "discord": found.and_then(|p| p.discord.as_deref()),
                    "account": found.map(|p| p.account),
                    "country": found.and_then(|p| p.country.as_deref()),
                },
                "seed": entrant.seed,
                "dropped": entrant.dropped,
                "place": entrant.place,
            })
        })
        .collect();
    let seeded = entrants.iter().filter(|e| e.seed.is_some()).count();
    let from = sources(&tournament, seeded, &sets);
    let me = match player {
        Some(player) => {
            let entrant = entrants.iter().find(|e| e.player_id == player.id);
            let reason = ineligible(state, player).await;
            let set = sets
                .iter()
                .find(|set| set.has(player.id) && matches!(set.state.as_str(), "ready" | "playing"))
                .or_else(|| {
                    sets.iter().find(|set| {
                        set.has(player.id) && matches!(set.state.as_str(), "pending" | "disputed")
                    })
                });
            json!({
                "player": player.id,
                "entrant": entrant.is_some(),
                "eligible": reason.is_none(),
                "reason": reason,
                "dropped": entrant.is_some_and(|e| e.dropped),
                "place": entrant.and_then(|e| e.place),
                "set": set.map(|set| set.id),
            })
        }
        None => Value::Null,
    };
    let seed_of = |player: i64| {
        entrants
            .iter()
            .find(|e| e.player_id == player)
            .and_then(|e| e.seed)
    };
    Ok(Some(json!({
        "tournament": summary(state, &tournament).await?,
        "entrants": entrant_views,
        "sets": sets
            .iter()
            .enumerate()
            .map(|(index, set)| set_view(set, from.get(index), seed_of))
            .collect::<Vec<_>>(),
        "me": me,
        "poll_interval": POLL_INTERVAL,
    })))
}

/// `POST /api/v1/tournaments/<id>/join`.
pub(in crate::profiles) async fn join(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if ineligible(&state, &player).await.is_some() {
        return discord_required();
    }
    match store::join(&state.pool, id, player.id).await {
        Ok(Ok(())) => {
            info!(target: "profiles", tournament_id = id, player_id = player.id, "joined a tournament");
            respond_page(&state, id, &player).await
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => database_error(error),
    }
}

/// `DELETE /api/v1/tournaments/<id>/join`: out before it starts; once it
/// runs, the player gives up their open sets.
pub(in crate::profiles) async fn leave(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    match store::leave(&state.pool, id, player.id).await {
        Ok(Ok(called_off)) => {
            for match_id in called_off {
                state.pairings.end(match_id);
            }
            info!(target: "profiles", tournament_id = id, player_id = player.id, "left a tournament");
            respond_page(&state, id, &player).await
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => database_error(error),
    }
}

async fn respond_page(state: &ProfilesState, id: i64, player: &Player) -> Response {
    match page(state, id, Some(player)).await {
        Ok(Some(view)) => Json(view).into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "no such tournament"),
        Err(error) => database_error(error),
    }
}

#[derive(Deserialize)]
pub(in crate::profiles) struct Ready {
    ready: bool,
}

/// `POST /api/v1/tournaments/<id>/sets/<set>/ready`: `{"ready": true}` makes
/// the set's match (or says ready for it), `false` takes it back. The match
/// as `GET /api/v1/matches/current` shows it, or `null`.
pub(in crate::profiles) async fn ready(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Path((id, set_id)): Path<(i64, i64)>,
    Json(request): Json<Ready>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    if ineligible(&state, &player).await.is_some() {
        return discord_required();
    }
    if let Err(response) = state.sweep().await {
        return response;
    }
    match store::ready(&state.pool, id, set_id, player.id, request.ready).await {
        Ok(Ok(found)) => {
            info!(target: "profiles", tournament_id = id, set_id, player_id = player.id, ready = request.ready, "tournament READY");
            let Some(found) = found else {
                return Json(json!({ "match": null })).into_response();
            };
            if !request.ready
                && let Some(side) = found.side(player.id)
            {
                state.pairings.unready(found.id, side);
            }
            match state.view(&found, &player).await {
                Ok(view) => Json(json!({ "match": view })).into_response(),
                Err(error) => database_error(error),
            }
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => database_error(error),
    }
}

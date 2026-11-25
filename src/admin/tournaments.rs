//! The Tournaments page (docs/tournaments.md, "The admin page"): the
//! tournaments the server runs, made, started, called off and decided here.

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{info, warn};

use super::{AdminState, error};
use crate::profiles::{
    rules::{self, RuleSet},
    tournaments::{
        bracket::{Format, MAX_ENTRANTS},
        store::{self, Entrant, NewTournament, Refusal, Seeding, Set, Tournament},
    },
};

#[derive(Serialize)]
pub struct Tournaments {
    tournaments: Vec<Shown>,
    rule_sets: Vec<RuleSet>,
    series: &'static [u8],
}

#[derive(Serialize)]
struct Shown {
    #[serde(flatten)]
    tournament: Tournament,
    entrants: Vec<Named<Entrant>>,
    sets: Vec<Set>,
}

/// An entrant and the player's name.
#[derive(Serialize)]
struct Named<T> {
    #[serde(flatten)]
    item: T,
    name: String,
}

pub async fn read(pool: &PgPool) -> Result<Tournaments, sqlx::Error> {
    let mut tournaments = Vec::new();
    for tournament in store::list(pool).await? {
        let listed = store::entrants(pool, tournament.id).await?;
        let ids: Vec<i64> = listed.iter().map(|entrant| entrant.player_id).collect();
        let mut names: HashMap<i64, String> = sqlx::query!(
            "SELECT player_id, display_name FROM players WHERE player_id = ANY($1)",
            &ids,
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|row| (row.player_id, row.display_name))
        .collect();
        let entrants = listed
            .into_iter()
            .map(|entrant| Named {
                name: names.remove(&entrant.player_id).unwrap_or_default(),
                item: entrant,
            })
            .collect();
        let sets = store::sets(pool, tournament.id).await?;
        tournaments.push(Shown {
            tournament,
            entrants,
            sets,
        });
    }
    Ok(Tournaments {
        tournaments,
        rule_sets: rules::list(pool).await?,
        series: &rules::SERIES,
    })
}

fn refused(refusal: Refusal) -> Response {
    let (status, message) = match refusal {
        Refusal::NotFound => (
            StatusCode::NOT_FOUND,
            "no such tournament or set".to_owned(),
        ),
        Refusal::State(state) => (StatusCode::CONFLICT, format!("it is {state}")),
        Refusal::Full => (StatusCode::CONFLICT, "it is full".into()),
        Refusal::Joined => (StatusCode::CONFLICT, "already in it".into()),
        Refusal::NotIn => (StatusCode::NOT_FOUND, "not in it".into()),
        Refusal::TooFew => (
            StatusCode::CONFLICT,
            "it needs at least two entrants".into(),
        ),
        Refusal::BadSeeding => (
            StatusCode::BAD_REQUEST,
            "the seeding names each entrant once".into(),
        ),
        Refusal::Settled => (
            StatusCode::CONFLICT,
            "a set after this one has a result already".into(),
        ),
        Refusal::Busy => (StatusCode::CONFLICT, "a player has a match to play".into()),
    };
    error(status, &message)
}

fn database(failure: sqlx::Error) -> Response {
    warn!(target: "admin", error = %failure, "database error");
    error(StatusCode::INTERNAL_SERVER_ERROR, "the database failed")
}

/// Who may change tournaments: an editor. Otherwise the refusal.
fn editor(state: &AdminState, jar: &CookieJar, headers: &HeaderMap) -> Result<String, Response> {
    match state.editor(jar, headers) {
        Ok(editor) => Ok(editor.email),
        Err(StatusCode::UNAUTHORIZED) => Err(error(StatusCode::UNAUTHORIZED, "signed out")),
        Err(status) => Err(status.into_response()),
    }
}

#[derive(Deserialize)]
pub struct NewForm {
    name: String,
    format: String,
    rule_set: i64,
    best_of: u8,
    finals_best_of: u8,
    #[serde(default)]
    max_entrants: Option<i32>,
}

/// `POST /admin/api/tournaments`: a new tournament, open for entries.
pub async fn create(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(form): Json<NewForm>,
) -> Response {
    let email = match editor(&state, &jar, &headers) {
        Ok(email) => email,
        Err(response) => return response,
    };
    let name = form.name.trim();
    if !(1..=64).contains(&name.chars().count()) {
        return error(StatusCode::BAD_REQUEST, "name: 1 to 64 characters");
    }
    let Some(format) = Format::parse(&form.format) else {
        return error(
            StatusCode::BAD_REQUEST,
            "format: single, double or single_ash",
        );
    };
    if !rules::SERIES.contains(&form.best_of) || !rules::SERIES.contains(&form.finals_best_of) {
        return error(StatusCode::BAD_REQUEST, "best of 1, 3, 5 or 7");
    }
    if form
        .max_entrants
        .is_some_and(|max| !(2..=MAX_ENTRANTS as i32).contains(&max))
    {
        return error(
            StatusCode::BAD_REQUEST,
            &format!("max_entrants: 2 to {MAX_ENTRANTS}"),
        );
    }
    let rule_set = match rules::get(&state.pool, form.rule_set).await {
        Ok(Some(rule_set)) => rule_set,
        Ok(None) => return error(StatusCode::NOT_FOUND, "no such rule set"),
        Err(failure) => return database(failure),
    };
    let new = NewTournament {
        name: name.to_owned(),
        organizer: None,
        format,
        rule_set,
        best_of: form.best_of,
        finals_best_of: form.finals_best_of,
        max_entrants: form.max_entrants,
        starts_at: None,
    };
    match store::create(&state.pool, &new).await {
        Ok(id) => {
            info!(target: "admin", %email, id, name, "tournament made");
            (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response()
        }
        Err(failure) => database(failure),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum SeedingForm {
    /// `joined` or `shuffled`.
    Named(String),
    /// The players in seed order.
    Given(Vec<i64>),
}

#[derive(Deserialize)]
pub struct StartForm {
    seeding: SeedingForm,
}

/// `POST /admin/api/tournaments/<id>/start`.
pub async fn start(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(form): Json<StartForm>,
) -> Response {
    let email = match editor(&state, &jar, &headers) {
        Ok(email) => email,
        Err(response) => return response,
    };
    let seeding = match form.seeding {
        SeedingForm::Named(name) if name == "joined" => Seeding::Joined,
        SeedingForm::Named(name) if name == "shuffled" => Seeding::Shuffled,
        SeedingForm::Named(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "seeding: joined, shuffled or players",
            );
        }
        SeedingForm::Given(players) => Seeding::Given(players),
    };
    match store::start(&state.pool, id, seeding).await {
        Ok(Ok(())) => {
            info!(target: "admin", %email, id, "tournament started");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(failure) => database(failure),
    }
}

/// `POST /admin/api/tournaments/<id>/cancel`.
pub async fn cancel(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let email = match editor(&state, &jar, &headers) {
        Ok(email) => email,
        Err(response) => return response,
    };
    match store::cancel(&state.pool, id).await {
        Ok(Ok(called_off)) => {
            for match_id in called_off {
                state.pairings.end(match_id);
            }
            info!(target: "admin", %email, id, "tournament called off");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(failure) => database(failure),
    }
}

/// `POST /admin/api/tournaments/<id>/entrants/<player>/drop`: disqualifies
/// an entrant of a running tournament (or takes one out before it starts).
pub async fn drop_entrant(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((id, player_id)): Path<(i64, i64)>,
) -> Response {
    let email = match editor(&state, &jar, &headers) {
        Ok(email) => email,
        Err(response) => return response,
    };
    match store::leave(&state.pool, id, player_id).await {
        Ok(Ok(called_off)) => {
            for match_id in called_off {
                state.pairings.end(match_id);
            }
            info!(target: "admin", %email, id, player_id, "tournament entrant dropped");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(failure) => database(failure),
    }
}

#[derive(Deserialize)]
pub struct DecideForm {
    /// The player who wins the set; none: it is played again.
    winner: Option<i64>,
}

/// `POST /admin/api/tournaments/<id>/sets/<set>/decide`.
pub async fn decide(
    State(state): State<Arc<AdminState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((id, set_id)): Path<(i64, i64)>,
    Json(form): Json<DecideForm>,
) -> Response {
    let email = match editor(&state, &jar, &headers) {
        Ok(email) => email,
        Err(response) => return response,
    };
    match store::decide(&state.pool, id, set_id, form.winner).await {
        Ok(Ok(called_off)) => {
            for match_id in called_off {
                state.pairings.end(match_id);
            }
            info!(target: "admin", %email, id, set_id, winner = ?form.winner, "tournament set decided");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(failure) => database(failure),
    }
}

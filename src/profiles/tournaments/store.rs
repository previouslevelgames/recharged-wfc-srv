//! Tournaments in the database (docs/tournaments.md): made, joined, started,
//! and moved on by their matches' results. The bracket itself is worked out
//! by [`super::bracket`] from the results and who left; this keeps its sets'
//! players and states up to date with it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;

use super::bracket::{self, Format, Outcome, Slot};
use crate::profiles::{
    matches::{self, Match},
    rules::{RuleSet, Rules},
};

/// How many finished tournaments the list shows.
const FINISHED_LISTED: i64 = 10;

/// A tournament to make.
pub struct NewTournament {
    pub name: String,
    /// The player who runs it; none: the server's admins.
    pub organizer: Option<i64>,
    pub format: Format,
    /// The rules and the name of the rule set they come from.
    pub rule_set: RuleSet,
    pub best_of: u8,
    pub finals_best_of: u8,
    pub max_entrants: Option<i32>,
    pub starts_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Tournament {
    pub id: i64,
    pub name: String,
    pub organizer: Option<i64>,
    pub format: &'static str,
    /// `registration`, `running`, `finished` or `cancelled`.
    pub state: String,
    pub rule_set: String,
    pub rules: Rules,
    pub best_of: u8,
    pub finals_best_of: u8,
    pub max_entrants: Option<i32>,
    #[serde(with = "time::serde::timestamp::option")]
    pub starts_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::timestamp")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::timestamp::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::timestamp::option")]
    pub ended_at: Option<OffsetDateTime>,
    /// Entrants in it now (who left a running one included).
    pub entrants: i64,
}

impl Tournament {
    /// The rules of one of its sets: its own number of games.
    pub fn rules_for(&self, games: u8) -> RuleSet {
        let rules = Rules {
            games,
            ..self.rules.clone()
        };
        RuleSet {
            id: 0,
            name: self.rule_set.clone(),
            hash: rules.hash(),
            rules,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entrant {
    pub player_id: i64,
    /// From 0, once the tournament started.
    pub seed: Option<i32>,
    #[serde(with = "time::serde::timestamp")]
    pub joined_at: OffsetDateTime,
    /// They left, or were disqualified.
    pub dropped: bool,
    /// From 1, once it is over.
    pub place: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Set {
    pub id: i64,
    /// Its place in the bracket's order.
    pub index: i32,
    /// `winners`, `losers`, `grand_final` or `grand_final_reset`.
    pub side: String,
    pub round: i32,
    pub position: i32,
    pub games: u8,
    /// `pending`, `ready`, `playing`, `finished`, `disputed` or `void`.
    pub state: String,
    pub home: Option<i64>,
    pub away: Option<i64>,
    pub winner: Option<i64>,
    /// `match`, `organizer` or `walkover`.
    pub decided_by: Option<String>,
    pub home_wins: Option<i16>,
    pub away_wins: Option<i16>,
    #[serde(rename = "match")]
    pub match_id: Option<i64>,
    /// Where its next game goes on from: its last game broke off.
    pub resume: Option<Resume>,
}

/// A team of a set's game that broke off, as it was then.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResumeTeam {
    pub player: i64,
    pub captain: String,
    /// Three sidekicks, the game's names.
    pub sidekicks: Vec<String>,
    pub score: i32,
}

/// A set's game that broke off (a disconnect, not a forfeit): the next game
/// goes on from it, on the same sides with the same teams in the same
/// stadium, from its score and its clock.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resume {
    pub stadium: String,
    /// Seconds on its clock when it broke off.
    pub seconds: i32,
    /// The game's home and away teams.
    pub home: ResumeTeam,
    pub away: ResumeTeam,
}

impl Resume {
    /// The team of `player_id`, and the other.
    pub fn teams_of(&self, player_id: i64) -> (&ResumeTeam, &ResumeTeam) {
        if self.home.player == player_id {
            (&self.home, &self.away)
        } else {
            (&self.away, &self.home)
        }
    }

    /// `player_id`'s view: the stadium and clock, who is at home, and both
    /// teams as `you` and `opponent`.
    pub fn view(&self, player_id: i64) -> serde_json::Value {
        let (you, opponent) = self.teams_of(player_id);
        let team = |team: &ResumeTeam| serde_json::json!({ "captain": team.captain, "sidekicks": team.sidekicks, "score": team.score });
        serde_json::json!({
            "stadium": self.stadium,
            "seconds": self.seconds,
            "home": if self.home.player == player_id { "you" } else { "opponent" },
            "you": team(you),
            "opponent": team(opponent),
        })
    }
}

impl Set {
    pub fn has(&self, player_id: i64) -> bool {
        self.home == Some(player_id) || self.away == Some(player_id)
    }

    /// Who plays at home (the game's home side) in the set's next game, by
    /// the players' seeds (`seed_of`, from 0 the best): the lower seed in the
    /// first, then each in turn. None until both are known.
    pub fn game_home(&self, seed_of: impl Fn(i64) -> Option<i32>) -> Option<i64> {
        // A game that goes on keeps its sides.
        if let Some(resume) = &self.resume {
            return Some(resume.home.player);
        }
        game_home(
            (self.home?, self.away?),
            seed_of,
            self.home_wins.unwrap_or(0) + self.away_wins.unwrap_or(0),
        )
    }
}

/// Of `players` (a set's home and away places), who plays at home in the
/// game after `played` games won: the one with the worse seed (the higher
/// number; the away place without seeds) in the first, then each in turn.
pub fn game_home(
    players: (i64, i64),
    seed_of: impl Fn(i64) -> Option<i32>,
    played: i16,
) -> Option<i64> {
    let (home, away) = players;
    let worse_is_home = match (seed_of(home), seed_of(away)) {
        (Some(home), Some(away)) => home > away,
        _ => false,
    };
    let (worse, better) = if worse_is_home {
        (home, away)
    } else {
        (away, home)
    };
    Some(if played % 2 == 0 { worse } else { better })
}

/// How the entrants get their seeds when the tournament starts.
pub enum Seeding {
    /// In the order they joined.
    Joined,
    /// At random.
    Shuffled,
    /// In this order of players: all the entrants, each once.
    Given(Vec<i64>),
}

/// Why a request about a tournament is refused.
#[derive(Debug, PartialEq)]
pub enum Refusal {
    NotFound,
    /// The tournament, or the set, is in another state, named.
    State(String),
    /// It has as many entrants as it takes.
    Full,
    /// The player is in it already.
    Joined,
    /// The player isn't in it, or not in that set.
    NotIn,
    /// It needs at least two entrants.
    TooFew,
    /// A seeding that doesn't name each entrant once.
    BadSeeding,
    /// A set whose result the sets after it already build on.
    Settled,
    /// The player or the opponent has a match to play already.
    Busy,
}

pub async fn create(pool: &PgPool, new: &NewTournament) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        "INSERT INTO tournaments (name, organizer, format, rule_set, rules, best_of,
                                  finals_best_of, max_entrants, starts_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         RETURNING tournament_id",
        new.name,
        new.organizer,
        new.format.name(),
        new.rule_set.name,
        new.rule_set.rules.to_json(),
        i16::from(new.best_of),
        i16::from(new.finals_best_of),
        new.max_entrants,
        new.starts_at,
    )
    .fetch_one(pool)
    .await
}

struct TournamentRow {
    tournament_id: i64,
    name: String,
    organizer: Option<i64>,
    format: String,
    state: String,
    rule_set: String,
    rules: String,
    best_of: i16,
    finals_best_of: i16,
    max_entrants: Option<i32>,
    starts_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
    started_at: Option<OffsetDateTime>,
    ended_at: Option<OffsetDateTime>,
    entrants: i64,
}

fn tournament(row: TournamentRow) -> Option<Tournament> {
    let format = Format::parse(&row.format)?;
    let rules = match Rules::from_json(&row.rules) {
        Ok(rules) => rules,
        Err(error) => {
            tracing::warn!(target: "profiles", tournament_id = row.tournament_id, %error, "tournament rules not understood");
            return None;
        }
    };
    Some(Tournament {
        id: row.tournament_id,
        name: row.name,
        organizer: row.organizer,
        format: format.name(),
        state: row.state,
        rule_set: row.rule_set,
        rules,
        best_of: row.best_of as u8,
        finals_best_of: row.finals_best_of as u8,
        max_entrants: row.max_entrants,
        starts_at: row.starts_at,
        created_at: row.created_at,
        started_at: row.started_at,
        ended_at: row.ended_at,
        entrants: row.entrants,
    })
}

pub async fn get(pool: &PgPool, id: i64) -> Result<Option<Tournament>, sqlx::Error> {
    let row = sqlx::query_as!(
        TournamentRow,
        r#"SELECT tournament_id, name, organizer, format, state, rule_set, rules, best_of,
                  finals_best_of, max_entrants, starts_at, created_at, started_at, ended_at,
                  (SELECT count(*) FROM tournament_entrants e
                   WHERE e.tournament_id = t.tournament_id) AS "entrants!"
           FROM tournaments t WHERE tournament_id = $1"#,
        id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(tournament))
}

/// The tournaments open for entries or running, the newest first, then the
/// last few that ended.
pub async fn list(pool: &PgPool) -> Result<Vec<Tournament>, sqlx::Error> {
    let rows = sqlx::query_as!(
        TournamentRow,
        r#"SELECT tournament_id, name, organizer, format, state, rule_set, rules, best_of,
                  finals_best_of, max_entrants, starts_at, created_at, started_at, ended_at,
                  (SELECT count(*) FROM tournament_entrants e
                   WHERE e.tournament_id = t.tournament_id) AS "entrants!"
           FROM tournaments t
           WHERE state IN ('registration', 'running')
              OR tournament_id IN (
                  SELECT tournament_id FROM tournaments WHERE state IN ('finished', 'cancelled')
                  ORDER BY coalesce(ended_at, created_at) DESC LIMIT $1)
           ORDER BY state IN ('registration', 'running') DESC,
                    coalesce(ended_at, created_at) DESC"#,
        FINISHED_LISTED,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().filter_map(tournament).collect())
}

/// Its entrants: by seed once it started, else in the order they joined.
pub async fn entrants(pool: &PgPool, id: i64) -> Result<Vec<Entrant>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT player_id, seed, joined_at, dropped_at, place FROM tournament_entrants
         WHERE tournament_id = $1 ORDER BY seed NULLS LAST, joined_at, player_id",
        id,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Entrant {
            player_id: row.player_id,
            seed: row.seed,
            joined_at: row.joined_at,
            dropped: row.dropped_at.is_some(),
            place: row.place,
        })
        .collect())
}

/// Its sets in the bracket's order; none before it started.
pub async fn sets(pool: &PgPool, id: i64) -> Result<Vec<Set>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT tournament_set_id, index, side, round, position, games, state, home, away,
                winner, decided_by, home_wins, away_wins, match_id, resume
         FROM tournament_sets WHERE tournament_id = $1 ORDER BY index",
        id,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Set {
            id: row.tournament_set_id,
            index: row.index,
            side: row.side,
            round: row.round,
            position: row.position,
            games: row.games as u8,
            state: row.state,
            home: row.home,
            away: row.away,
            winner: row.winner,
            decided_by: row.decided_by,
            home_wins: row.home_wins,
            away_wins: row.away_wins,
            match_id: row.match_id,
            resume: row
                .resume
                .and_then(|resume| serde_json::from_value(resume).ok()),
        })
        .collect())
}

/// A game a set played, as its players reported it: each team's captain
/// and goals, home and away as the set has them.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlayedGame {
    pub home_captain: String,
    pub away_captain: String,
    pub home_score: i32,
    pub away_score: i32,
    pub stadium: Option<String>,
    /// It broke off (or was forfeited) at this score.
    pub interrupted: bool,
}

/// The games each set of the tournament played, by set, in their order:
/// one from each finished match of the set, as its home player reported it
/// (else its away player).
pub async fn played_games(
    pool: &PgPool,
    id: i64,
) -> Result<HashMap<i64, Vec<PlayedGame>>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT s.tournament_set_id, m.match_id, s.home, r.player_id, r.games
           FROM tournament_sets s
           JOIN matches m ON m.tournament_set_id = s.tournament_set_id AND m.state = 'finished'
           JOIN match_reports r ON r.match_id = m.match_id
           WHERE s.tournament_id = $1
           ORDER BY s.tournament_set_id, m.match_id, (r.player_id = s.home) DESC"#,
        id,
    )
    .fetch_all(pool)
    .await?;
    let mut played: HashMap<i64, Vec<PlayedGame>> = Default::default();
    let mut last_match = None;
    for row in rows {
        if last_match == Some(row.match_id) {
            continue;
        }
        let Ok(serde_json::Value::Array(games)) = serde_json::from_str(&row.games) else {
            continue;
        };
        if games.is_empty() {
            continue;
        }
        last_match = Some(row.match_id);
        let home = row.home == Some(row.player_id);
        let text =
            |game: &serde_json::Value, key: &str| game[key].as_str().unwrap_or_default().to_owned();
        let score = |game: &serde_json::Value, key: &str| game[key].as_i64().unwrap_or(0) as i32;
        for game in &games {
            let (mine, theirs) = (text(game, "my_captain"), text(game, "opponent_captain"));
            let (my_score, their_score) = (score(game, "my_score"), score(game, "opponent_score"));
            played
                .entry(row.tournament_set_id)
                .or_default()
                .push(PlayedGame {
                    home_captain: if home { mine.clone() } else { theirs.clone() },
                    away_captain: if home { theirs } else { mine },
                    home_score: if home { my_score } else { their_score },
                    away_score: if home { their_score } else { my_score },
                    stadium: game["stadium"].as_str().map(str::to_owned),
                    interrupted: game["interrupted"].as_bool().unwrap_or(false),
                });
        }
    }
    Ok(played)
}

/// What a match's view says of the tournament set it plays
/// (docs/profiles.md, "Matches"): `{id, name, set, side, round, games,
/// home_wins, away_wins}`: the set's best of and its games won so far.
pub async fn set_context(
    pool: &PgPool,
    set_id: i64,
    player_id: i64,
) -> Result<Option<serde_json::Value>, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT t.tournament_id, t.name, s.side, s.round, s.games, s.state, s.home, s.away,
                s.winner, s.home_wins, s.away_wins,
                (SELECT seed FROM tournament_entrants e
                 WHERE e.tournament_id = s.tournament_id AND e.player_id = s.home) AS home_seed,
                (SELECT seed FROM tournament_entrants e
                 WHERE e.tournament_id = s.tournament_id AND e.player_id = s.away) AS away_seed,
                s.resume
         FROM tournament_sets s
         JOIN tournaments t USING (tournament_id) WHERE s.tournament_set_id = $1",
        set_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| {
        let (home_wins, away_wins) = (row.home_wins.unwrap_or(0), row.away_wins.unwrap_or(0));
        let away = row.away == Some(player_id);
        let winner = match (row.state.as_str(), row.winner) {
            ("finished", Some(winner)) if winner == player_id => Some("you"),
            ("finished", Some(_)) => Some("opponent"),
            _ => None,
        };
        let seeds = (row.home_seed, row.away_seed);
        let resume: Option<Resume> = row
            .resume
            .and_then(|resume| serde_json::from_value(resume).ok());
        let game_home = match (row.home, row.away) {
            // A game that goes on keeps its sides.
            _ if resume.is_some() => resume.as_ref().map(|resume| resume.home.player),
            (Some(home), Some(away)) => game_home(
                (home, away),
                |player| {
                    if player == home { seeds.0 } else { seeds.1 }
                },
                home_wins + away_wins,
            ),
            _ => None,
        };
        serde_json::json!({
            "id": row.tournament_id,
            "name": row.name,
            "set": set_id,
            "side": row.side,
            "round": row.round,
            "games": row.games,
            "home_wins": home_wins,
            "away_wins": away_wins,
            "won": if away { away_wins } else { home_wins },
            "lost": if away { home_wins } else { away_wins },
            "state": row.state,
            "winner": winner,
            "home": game_home.map(|home| if home == player_id { "you" } else { "opponent" }),
            "resume": resume.map(|resume| resume.view(player_id)),
        })
    }))
}

/// The tournament's row, locked for the rest of the transaction: its
/// changes happen one at a time.
async fn lock(
    transaction: &mut Transaction<'_, Postgres>,
    id: i64,
) -> Result<Option<(String, Format, Option<i32>)>, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT state, format, max_entrants FROM tournaments WHERE tournament_id = $1 FOR UPDATE",
        id,
    )
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(row.and_then(|row| Some((row.state, Format::parse(&row.format)?, row.max_entrants))))
}

/// `player_id` joins a tournament open for entries.
pub async fn join(
    pool: &PgPool,
    id: i64,
    player_id: i64,
) -> Result<Result<(), Refusal>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let Some((state, _, max)) = lock(&mut transaction, id).await? else {
        return Ok(Err(Refusal::NotFound));
    };
    if state != "registration" {
        return Ok(Err(Refusal::State(state)));
    }
    let count = sqlx::query_scalar!(
        "SELECT count(*) AS \"count!\" FROM tournament_entrants WHERE tournament_id = $1",
        id,
    )
    .fetch_one(&mut *transaction)
    .await?;
    let most = max.map_or(bracket::MAX_ENTRANTS as i64, i64::from);
    if count >= most {
        return Ok(Err(Refusal::Full));
    }
    let joined = sqlx::query!(
        "INSERT INTO tournament_entrants (tournament_id, player_id) VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
        id,
        player_id,
    )
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    if joined == 0 {
        return Ok(Err(Refusal::Joined));
    }
    transaction.commit().await?;
    Ok(Ok(()))
}

/// `player_id` leaves: before the start they are no longer in it; once it
/// runs they lose their open sets and go no further (as when the organizer
/// disqualifies them). The matches called off by it.
pub async fn leave(
    pool: &PgPool,
    id: i64,
    player_id: i64,
) -> Result<Result<Vec<i64>, Refusal>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let Some((state, ..)) = lock(&mut transaction, id).await? else {
        return Ok(Err(Refusal::NotFound));
    };
    let gone = match state.as_str() {
        "registration" => sqlx::query!(
            "DELETE FROM tournament_entrants WHERE tournament_id = $1 AND player_id = $2",
            id,
            player_id,
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected(),
        "running" => sqlx::query!(
            "UPDATE tournament_entrants SET dropped_at = now()
             WHERE tournament_id = $1 AND player_id = $2 AND dropped_at IS NULL",
            id,
            player_id,
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected(),
        _ => return Ok(Err(Refusal::State(state))),
    };
    if gone == 0 {
        return Ok(Err(Refusal::NotIn));
    }
    let called_off = if state == "running" {
        refresh(&mut transaction, id).await?
    } else {
        Vec::new()
    };
    transaction.commit().await?;
    Ok(Ok(called_off))
}

/// Starts a tournament open for entries: the entrants get their seeds and
/// the bracket its sets.
pub async fn start(
    pool: &PgPool,
    id: i64,
    seeding: Seeding,
) -> Result<Result<(), Refusal>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let Some((state, format, _)) = lock(&mut transaction, id).await? else {
        return Ok(Err(Refusal::NotFound));
    };
    if state != "registration" {
        return Ok(Err(Refusal::State(state)));
    }
    let mut players = sqlx::query_scalar!(
        "SELECT player_id FROM tournament_entrants WHERE tournament_id = $1
         ORDER BY joined_at, player_id",
        id,
    )
    .fetch_all(&mut *transaction)
    .await?;
    if players.len() < 2 {
        return Ok(Err(Refusal::TooFew));
    }
    match seeding {
        Seeding::Joined => {}
        Seeding::Shuffled => {
            for at in (1..players.len()).rev() {
                players.swap(at, rand::random_range(0..=at));
            }
        }
        Seeding::Given(order) => {
            let mut sorted = order.clone();
            sorted.sort_unstable();
            sorted.dedup();
            let mut entrants = players.clone();
            entrants.sort_unstable();
            if sorted != entrants || order.len() != players.len() {
                return Ok(Err(Refusal::BadSeeding));
            }
            players = order;
        }
    }
    for (seed, player_id) in players.iter().enumerate() {
        sqlx::query!(
            "UPDATE tournament_entrants SET seed = $3 WHERE tournament_id = $1 AND player_id = $2",
            id,
            player_id,
            seed as i32,
        )
        .execute(&mut *transaction)
        .await?;
    }
    let row = sqlx::query!(
        "UPDATE tournaments SET state = 'running', started_at = now() WHERE tournament_id = $1
         RETURNING best_of, finals_best_of",
        id,
    )
    .fetch_one(&mut *transaction)
    .await?;
    for (index, set) in bracket::plan(format, players.len()).iter().enumerate() {
        let games = if set.finals {
            row.finals_best_of
        } else {
            row.best_of
        };
        sqlx::query!(
            "INSERT INTO tournament_sets (tournament_id, index, side, round, position, games)
             VALUES ($1, $2, $3, $4, $5, $6)",
            id,
            index as i32,
            set.side.name(),
            set.round as i32,
            set.position as i32,
            games,
        )
        .execute(&mut *transaction)
        .await?;
    }
    refresh(&mut transaction, id).await?;
    transaction.commit().await?;
    Ok(Ok(()))
}

/// Calls a tournament off, before or while it runs. The matches called off
/// by it.
pub async fn cancel(pool: &PgPool, id: i64) -> Result<Result<Vec<i64>, Refusal>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let Some((state, ..)) = lock(&mut transaction, id).await? else {
        return Ok(Err(Refusal::NotFound));
    };
    if !matches!(state.as_str(), "registration" | "running") {
        return Ok(Err(Refusal::State(state)));
    }
    sqlx::query!(
        "UPDATE tournaments SET state = 'cancelled', ended_at = now() WHERE tournament_id = $1",
        id,
    )
    .execute(&mut *transaction)
    .await?;
    let called_off = sqlx::query_scalar!(
        "UPDATE matches SET state = 'cancelled', updated_at = now()
         WHERE state IN ('challenged', 'accepted') AND tournament_set_id IN (
             SELECT tournament_set_id FROM tournament_sets WHERE tournament_id = $1)
         RETURNING match_id",
        id,
    )
    .fetch_all(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(Ok(called_off))
}

/// The organizer decides set `set_id`: `winner` wins it, or with none it is
/// played again (a disputed set, say). Only a set whose result nothing
/// builds on yet. The matches called off by it.
pub async fn decide(
    pool: &PgPool,
    id: i64,
    set_id: i64,
    winner: Option<i64>,
) -> Result<Result<Vec<i64>, Refusal>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let Some((state, ..)) = lock(&mut transaction, id).await? else {
        return Ok(Err(Refusal::NotFound));
    };
    if state != "running" {
        return Ok(Err(Refusal::State(state)));
    }
    let Some(set) = sqlx::query!(
        "SELECT state, home, away, decided_by, match_id FROM tournament_sets
         WHERE tournament_id = $1 AND tournament_set_id = $2",
        id,
        set_id,
    )
    .fetch_optional(&mut *transaction)
    .await?
    else {
        return Ok(Err(Refusal::NotFound));
    };
    let decidable = matches!(set.state.as_str(), "ready" | "playing" | "disputed")
        || set.state == "finished" && set.decided_by.as_deref() != Some("walkover");
    if !decidable {
        return Ok(Err(Refusal::State(set.state)));
    }
    if set.state == "finished" && settled_after(&mut transaction, id, set_id).await? {
        return Ok(Err(Refusal::Settled));
    }
    let result = match winner {
        None => None,
        Some(player) if Some(player) == set.home => Some(0i16),
        Some(player) if Some(player) == set.away => Some(1),
        Some(_) => return Ok(Err(Refusal::NotIn)),
    };
    let mut called_off = Vec::new();
    if let Some(match_id) = set.match_id {
        called_off.extend(
            sqlx::query_scalar!(
                "UPDATE matches SET state = 'cancelled', updated_at = now()
                 WHERE match_id = $1 AND state IN ('challenged', 'accepted')
                 RETURNING match_id",
                match_id,
            )
            .fetch_all(&mut *transaction)
            .await?,
        );
    }
    sqlx::query!(
        "UPDATE tournament_sets SET result = $2, state = 'ready',
                decided_by = CASE WHEN $2::smallint IS NULL THEN NULL ELSE 'organizer' END,
                home_wins = NULL, away_wins = NULL, updated_at = now()
         WHERE tournament_set_id = $1",
        set_id,
        result,
    )
    .execute(&mut *transaction)
    .await?;
    called_off.extend(refresh(&mut transaction, id).await?);
    transaction.commit().await?;
    Ok(Ok(called_off))
}

/// Whether a set after `set_id` that takes a player from it has a result
/// already (played, or decided by the organizer).
async fn settled_after(
    transaction: &mut Transaction<'_, Postgres>,
    id: i64,
    set_id: i64,
) -> Result<bool, sqlx::Error> {
    let (format, count) = sqlx::query!(
        "SELECT format, (SELECT count(*) FROM tournament_entrants e
                         WHERE e.tournament_id = t.tournament_id AND seed IS NOT NULL) AS \"count!\"
         FROM tournaments t WHERE tournament_id = $1",
        id,
    )
    .fetch_one(&mut **transaction)
    .await
    .map(|row| (Format::parse(&row.format), row.count as usize))?;
    let Some(format) = format else {
        return Ok(true);
    };
    let index = sqlx::query_scalar!(
        "SELECT index FROM tournament_sets WHERE tournament_set_id = $1",
        set_id,
    )
    .fetch_one(&mut **transaction)
    .await? as usize;
    let results = sqlx::query!(
        "SELECT index, result FROM tournament_sets WHERE tournament_id = $1 ORDER BY index",
        id,
    )
    .fetch_all(&mut **transaction)
    .await?;
    let plan = bracket::plan(format, count);
    Ok(plan.iter().enumerate().any(|(at, set)| {
        set.from.iter().any(|from| {
            matches!(from, bracket::From::Winner(i) | bracket::From::Loser(i) if *i == index)
        }) && results
            .iter()
            .any(|row| row.index as usize == at && row.result.is_some())
    }))
}

/// `player_id` presses (or takes back) READY for their set `set_id`: the
/// first READY makes the set's match, the second makes it go; from there it
/// is played like any other set. The match, or none for a READY taken back
/// before there was one.
pub async fn ready(
    pool: &PgPool,
    id: i64,
    set_id: i64,
    player_id: i64,
    ready: bool,
) -> Result<Result<Option<Match>, Refusal>, sqlx::Error> {
    let Some(tournament) = get(pool, id).await? else {
        return Ok(Err(Refusal::NotFound));
    };
    if tournament.state != "running" {
        return Ok(Err(Refusal::State(tournament.state)));
    }
    let Some(set) = sets(pool, id)
        .await?
        .into_iter()
        .find(|set| set.id == set_id)
    else {
        return Ok(Err(Refusal::NotFound));
    };
    if !set.has(player_id) {
        return Ok(Err(Refusal::NotIn));
    }
    let (Some(home), Some(away)) = (set.home, set.away) else {
        return Ok(Err(Refusal::State(set.state)));
    };
    match set.state.as_str() {
        "playing" => {
            let match_id = set.match_id.expect("a playing set has its match");
            match matches::ready_to_play(pool, match_id, player_id, ready).await? {
                Ok(found) => Ok(Ok(Some(found))),
                Err(matches::Refusal::State(stage)) => Ok(Err(Refusal::State(stage))),
                Err(_) => Ok(Err(Refusal::NotFound)),
            }
        }
        "ready" if !ready => Ok(Ok(None)),
        "ready" => {
            // One game at a time: each is drafted again, but for one that
            // goes on from a game that broke off: its stadium (the game
            // takes its teams, sides, score and clock from the set).
            let mut rules = tournament.rules_for(1);
            if let Some(resume) = &set.resume {
                rules.rules.stadium = resume.stadium.clone();
                rules.hash = rules.rules.hash();
            }
            let made =
                match matches::tournament_set(pool, home, away, &rules, set.id, player_id).await? {
                    Ok(made) => made,
                    Err(matches::Refusal::Busy) => return Ok(Err(Refusal::Busy)),
                    Err(_) => return Ok(Err(Refusal::NotFound)),
                };
            // Another READY may have made one in between: the first stays.
            let taken = sqlx::query!(
                "UPDATE tournament_sets SET state = 'playing', match_id = $2, updated_at = now()
                 WHERE tournament_set_id = $1 AND state = 'ready'",
                set.id,
                made.id,
            )
            .execute(pool)
            .await?
            .rows_affected();
            if taken == 0 {
                sqlx::query!(
                    "UPDATE matches SET state = 'cancelled', updated_at = now() WHERE match_id = $1",
                    made.id,
                )
                .execute(pool)
                .await?;
                return Box::pin(self::ready(pool, id, set_id, player_id, ready)).await;
            }
            tracing::info!(target: "profiles", tournament_id = id, set_id, match_id = made.id, player_id, "tournament set's match");
            Ok(Ok(Some(made)))
        }
        _ => Ok(Err(Refusal::State(set.state))),
    }
}

/// Moves on the sets whose match ended: a result counts for the set, a set
/// without one is to be played again, and disagreeing reports (or a draw)
/// wait for the organizer. Called with every sweep of the matches.
pub async fn advance(pool: &PgPool) -> Result<(), sqlx::Error> {
    let ended = sqlx::query!(
        "SELECT s.tournament_id, s.tournament_set_id, m.match_id
         FROM tournament_sets s JOIN matches m ON m.match_id = s.match_id
         JOIN tournaments t ON t.tournament_id = s.tournament_id
         WHERE s.state = 'playing' AND t.state = 'running'
           AND m.state NOT IN ('challenged', 'accepted')",
    )
    .fetch_all(pool)
    .await?;
    for row in ended {
        let mut transaction = pool.begin().await?;
        let Some((state, ..)) = lock(&mut transaction, row.tournament_id).await? else {
            continue;
        };
        let Some(found) = matches::find(pool, row.match_id).await? else {
            continue;
        };
        if state != "running" {
            continue;
        }
        let home = sqlx::query_scalar!(
            "SELECT home FROM tournament_sets WHERE tournament_set_id = $1 AND state = 'playing'",
            row.tournament_set_id,
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(home) = home else {
            continue;
        };
        // A game that broke off (not one given up) is not counted: the set's
        // next game goes on from it, from its own game's report (else it is
        // played again from the start).
        if found.forfeited_by.is_none() && matches::broke_off(&mut *transaction, found.id).await? {
            let resume = resume_of(&mut transaction, &found).await?;
            sqlx::query!(
                "UPDATE tournament_sets SET state = 'ready', resume = $2, updated_at = now()
                 WHERE tournament_set_id = $1",
                row.tournament_set_id,
                resume.as_ref().map(|resume| serde_json::json!(resume)),
            )
            .execute(&mut *transaction)
            .await?;
            tracing::info!(target: "profiles", tournament_id = row.tournament_id, set_id = row.tournament_set_id, match_id = row.match_id, goes_on = resume.is_some(), "tournament set's game broke off");
            refresh(&mut transaction, row.tournament_id).await?;
            transaction.commit().await?;
            continue;
        }
        match (found.state.as_str(), found.winner) {
            ("finished", Some(winner)) => {
                // A match is one game of the set: its winner has one more
                // (a forfeit gives that game up, as a game broken off), and
                // the set is theirs once they have enough. Until then it is
                // to be played again: the next game, drafted again.
                let side = if Some(winner) == home { 0 } else { 1 };
                let set = sqlx::query!(
                    "SELECT games, coalesce(home_wins, 0::smallint) AS \"home_wins!\",
                            coalesce(away_wins, 0::smallint) AS \"away_wins!\"
                     FROM tournament_sets WHERE tournament_set_id = $1",
                    row.tournament_set_id,
                )
                .fetch_one(&mut *transaction)
                .await?;
                let mut wins = [set.home_wins, set.away_wins];
                wins[side] += 1;
                let needed = set.games / 2 + 1;
                let result = (wins[side] >= needed).then_some(side as i16);
                sqlx::query!(
                    "UPDATE tournament_sets SET result = $2,
                            decided_by = CASE WHEN $2::smallint IS NULL THEN NULL ELSE 'match' END,
                            state = CASE WHEN $2::smallint IS NULL THEN 'ready' ELSE state END,
                            home_wins = $3, away_wins = $4, resume = NULL, updated_at = now()
                     WHERE tournament_set_id = $1",
                    row.tournament_set_id,
                    result,
                    wins[0],
                    wins[1],
                )
                .execute(&mut *transaction)
                .await?;
            }
            // A game without a winner (a draw) is played again.
            ("finished", None) => {
                sqlx::query!(
                    "UPDATE tournament_sets SET state = 'ready', resume = NULL, updated_at = now()
                     WHERE tournament_set_id = $1",
                    row.tournament_set_id,
                )
                .execute(&mut *transaction)
                .await?;
            }
            ("disputed", _) => {
                sqlx::query!(
                    "UPDATE tournament_sets SET state = 'disputed', resume = NULL, updated_at = now()
                     WHERE tournament_set_id = $1",
                    row.tournament_set_id,
                )
                .execute(&mut *transaction)
                .await?;
            }
            _ => {
                sqlx::query!(
                    "UPDATE tournament_sets SET state = 'ready', updated_at = now()
                     WHERE tournament_set_id = $1",
                    row.tournament_set_id,
                )
                .execute(&mut *transaction)
                .await?;
            }
        }
        tracing::info!(target: "profiles", tournament_id = row.tournament_id, set_id = row.tournament_set_id, match_id = row.match_id, state = found.state, "tournament set's match ended");
        refresh(&mut transaction, row.tournament_id).await?;
        transaction.commit().await?;
    }
    Ok(())
}

/// Where the set's next game goes on from, after `found`'s game broke off:
/// the game's own report of it (online_match_games, the last that broke off
/// by a disconnect since the match was played, from either player's game),
/// with both teams' captains and sidekicks known; None without one.
async fn resume_of(
    transaction: &mut Transaction<'_, Postgres>,
    found: &Match,
) -> Result<Option<Resume>, sqlx::Error> {
    let since = found.playing_at.unwrap_or(found.created_at);
    let Some(row) = sqlx::query!(
        "SELECT g.home_score, g.away_score, g.home_captain, g.away_captain, g.stadium, g.seconds,
                s.side, s.home_sidekicks, s.away_sidekicks, bound.player_id
         FROM online_match_games g
         JOIN online_match_sides s ON s.match_id = g.match_id AND s.side = g.side
         JOIN player_wfc_profiles bound ON bound.profile_id = s.profile_id
         WHERE bound.player_id = ANY($1) AND g.ending = 'disconnect' AND g.reported_at >= $2
         ORDER BY g.reported_at DESC LIMIT 1",
        &[found.home, found.away][..],
        since,
    )
    .fetch_optional(&mut **transaction)
    .await?
    else {
        return Ok(None);
    };
    let known = |captain: &str, sidekicks: &[String]| captain != "unknown" && sidekicks.len() == 3;
    if !known(&row.home_captain, &row.home_sidekicks)
        || !known(&row.away_captain, &row.away_sidekicks)
        || !crate::profiles::rules::STADIUMS.contains(&row.stadium.as_str())
    {
        return Ok(None);
    }
    // The reporter's team is its side; the other player has the other.
    let other = if row.player_id == found.home {
        found.away
    } else {
        found.home
    };
    let (home, away) = if row.side == "home" {
        (row.player_id, other)
    } else {
        (other, row.player_id)
    };
    Ok(Some(Resume {
        stadium: row.stadium,
        seconds: row.seconds,
        home: ResumeTeam {
            player: home,
            captain: row.home_captain,
            sidekicks: row.home_sidekicks,
            score: row.home_score,
        },
        away: ResumeTeam {
            player: away,
            captain: row.away_captain,
            sidekicks: row.away_sidekicks,
            score: row.away_score,
        },
    }))
}

/// Works the bracket out again from the results and who left, and writes
/// each set's players and state; a finished bracket ends the tournament with
/// its standings. Open matches of sets that no longer play (a player left)
/// are called off; returns them.
async fn refresh(
    transaction: &mut Transaction<'_, Postgres>,
    id: i64,
) -> Result<Vec<i64>, sqlx::Error> {
    let Some(format) = sqlx::query_scalar!(
        "SELECT format FROM tournaments WHERE tournament_id = $1",
        id
    )
    .fetch_one(&mut **transaction)
    .await
    .map(|format| Format::parse(&format))?
    else {
        return Ok(Vec::new());
    };
    let entrants = sqlx::query!(
        "SELECT player_id, dropped_at IS NOT NULL AS \"dropped!\" FROM tournament_entrants
         WHERE tournament_id = $1 AND seed IS NOT NULL ORDER BY seed",
        id,
    )
    .fetch_all(&mut **transaction)
    .await?;
    let players: Vec<i64> = entrants.iter().map(|row| row.player_id).collect();
    let dropped: Vec<bool> = entrants.iter().map(|row| row.dropped).collect();
    let stored = sqlx::query!(
        "SELECT tournament_set_id, state, home, away, result, decided_by, winner, match_id
         FROM tournament_sets WHERE tournament_id = $1 ORDER BY index",
        id,
    )
    .fetch_all(&mut **transaction)
    .await?;
    let plan = bracket::plan(format, players.len());
    if plan.len() != stored.len() {
        tracing::error!(target: "profiles", tournament_id = id, "tournament sets don't match its bracket");
        return Ok(Vec::new());
    }
    let results: Vec<Option<usize>> = stored
        .iter()
        .map(|row| row.result.map(|result| result as usize))
        .collect();
    let resolved = bracket::resolve(&plan, players.len(), &results, &dropped);
    let player = |slot: Slot| match slot {
        Slot::Player(seed) => Some(players[seed]),
        _ => None,
    };
    let mut called_off = Vec::new();
    for (row, set) in stored.iter().zip(&resolved) {
        let (home, away) = (player(set.slots[0]), player(set.slots[1]));
        let (state, decided_by) = match set.outcome {
            Outcome::Open if set.playable() => match row.state.as_str() {
                "playing" | "disputed" if (row.home, row.away) == (home, away) => {
                    (row.state.as_str(), row.decided_by.clone())
                }
                _ => ("ready", None),
            },
            Outcome::Open => ("pending", None),
            Outcome::Won { walkover: true, .. } => ("finished", Some("walkover".to_owned())),
            Outcome::Won { .. } => ("finished", row.decided_by.clone()),
            Outcome::Void => ("void", None),
        };
        let winner = set.winner().map(|seed| players[seed]);
        if state != "playing"
            && row.state == "playing"
            && let Some(match_id) = row.match_id
        {
            called_off.extend(
                sqlx::query_scalar!(
                    "UPDATE matches SET state = 'cancelled', updated_at = now()
                     WHERE match_id = $1 AND state IN ('challenged', 'accepted')
                     RETURNING match_id",
                    match_id,
                )
                .fetch_all(&mut **transaction)
                .await?,
            );
        }
        let changed = (
            row.state.as_str(),
            row.home,
            row.away,
            &row.decided_by,
            row.winner,
        ) != (state, home, away, &decided_by, winner);
        if changed {
            sqlx::query!(
                "UPDATE tournament_sets SET state = $2, home = $3, away = $4, decided_by = $5,
                        winner = $6, updated_at = now()
                 WHERE tournament_set_id = $1",
                row.tournament_set_id,
                state,
                home,
                away,
                decided_by,
                winner,
            )
            .execute(&mut **transaction)
            .await?;
        }
    }
    if bracket::finished(&resolved) {
        let places = bracket::standings(&plan, players.len(), &resolved);
        for (seed, place) in places.iter().enumerate() {
            sqlx::query!(
                "UPDATE tournament_entrants SET place = $3 WHERE tournament_id = $1 AND player_id = $2",
                id,
                players[seed],
                place.map(|place| place as i32),
            )
            .execute(&mut **transaction)
            .await?;
        }
        sqlx::query!(
            "UPDATE tournaments SET state = 'finished', ended_at = now()
             WHERE tournament_id = $1 AND state = 'running'",
            id,
        )
        .execute(&mut **transaction)
        .await?;
        tracing::info!(target: "profiles", tournament_id = id, "tournament finished");
    }
    Ok(called_off)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{
        matches::Game,
        players::{self, Identity},
        rules,
    };

    async fn player(pool: &PgPool, name: &str) -> i64 {
        let id = name.to_lowercase();
        players::sign_in(
            pool,
            &Identity {
                provider: "test",
                external_id: &id,
                username: name,
                display_name: name,
                avatar_url: None,
            },
        )
        .await
        .unwrap()
    }

    async fn tournament(pool: &PgPool, format: Format, names: &[&str]) -> (i64, Vec<i64>) {
        let rule_set = rules::list(pool)
            .await
            .unwrap()
            .into_iter()
            .find(|set| set.name == "Competitive")
            .unwrap();
        let id = create(
            pool,
            &NewTournament {
                name: "Cup".into(),
                organizer: None,
                format,
                rule_set,
                best_of: 3,
                finals_best_of: 5,
                max_entrants: None,
                starts_at: None,
            },
        )
        .await
        .unwrap();
        let mut players = Vec::new();
        for name in names {
            let player_id = player(pool, name).await;
            join(pool, id, player_id).await.unwrap().unwrap();
            players.push(player_id);
        }
        (id, players)
    }

    fn game(mine: u32, theirs: u32) -> Game {
        Game {
            my_score: mine,
            opponent_score: theirs,
            my_captain: "mario".into(),
            opponent_captain: "bowser".into(),
            stadium: "the_palace".into(),
            interrupted: false,
            played: None,
        }
    }

    /// The set of `id` that `player_id` has to play.
    async fn set_of(pool: &PgPool, id: i64, player_id: i64) -> Set {
        sets(pool, id)
            .await
            .unwrap()
            .into_iter()
            .find(|set| set.has(player_id) && matches!(set.state.as_str(), "ready" | "playing"))
            .unwrap()
    }

    /// `set` is played to its end, `winner` winning every game: one match a
    /// game, both players pressing READY for each, their games meeting, both
    /// reporting it.
    async fn play(pool: &PgPool, id: i64, set: &Set, winner: i64) {
        let (home, away) = (set.home.unwrap(), set.away.unwrap());
        for game in 0..set.games / 2 + 1 {
            let made = ready(pool, id, set.id, home, true)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(made.stage(), "open");
            assert_eq!(made.rules.games, 1);
            assert_eq!(made.tournament_set, Some(set.id));
            let going = ready(pool, id, set.id, away, true)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!((going.id, going.stage()), (made.id, "go"));
            matches::game_ready(pool, made.id, 0, true).await.unwrap();
            for player_id in [home, away] {
                let won = player_id == winner;
                // The same game from both sides: the home player has Mario.
                let mut seen = if won { game_of(2, 1) } else { game_of(1, 2) };
                if player_id == away {
                    std::mem::swap(&mut seen.my_captain, &mut seen.opponent_captain);
                }
                matches::report(pool, made.id, player_id, &[seen])
                    .await
                    .unwrap()
                    .unwrap();
            }
            advance(pool).await.unwrap();
            let now = sets(pool, id)
                .await
                .unwrap()
                .into_iter()
                .find(|s| s.id == set.id)
                .unwrap();
            let won = i16::from(game + 1);
            let wins = if winner == home { (won, 0) } else { (0, won) };
            assert_eq!((now.home_wins, now.away_wins), (Some(wins.0), Some(wins.1)));
            let last = game + 1 == set.games / 2 + 1;
            assert_eq!(now.state, if last { "finished" } else { "ready" });
        }
    }

    fn game_of(mine: u32, theirs: u32) -> Game {
        game(mine, theirs)
    }

    #[sqlx::test]
    async fn a_single_bracket_is_played_to_its_standings(pool: PgPool) {
        let (id, p) =
            tournament(&pool, Format::Single, &["Mario", "Luigi", "Peach", "Daisy"]).await;
        assert_eq!(start(&pool, id, Seeding::Joined).await.unwrap(), Ok(()));
        assert_eq!(get(&pool, id).await.unwrap().unwrap().state, "running");
        // Seeds 1-4 and 2-3, then the final.
        let first = set_of(&pool, id, p[0]).await;
        assert_eq!(
            (first.home, first.away, first.games),
            (Some(p[0]), Some(p[3]), 3)
        );
        play(&pool, id, &first, p[3]).await;
        let second = set_of(&pool, id, p[1]).await;
        assert_eq!((second.home, second.away), (Some(p[1]), Some(p[2])));
        play(&pool, id, &second, p[1]).await;
        let done = sets(&pool, id).await.unwrap();
        assert_eq!(done[0].winner, Some(p[3]));
        assert_eq!((done[0].home_wins, done[0].away_wins), (Some(0), Some(2)));
        assert_eq!(done[0].decided_by.as_deref(), Some("match"));
        let final_set = set_of(&pool, id, p[3]).await;
        assert_eq!(
            (final_set.home, final_set.away, final_set.games),
            (Some(p[3]), Some(p[1]), 5)
        );
        play(&pool, id, &final_set, p[1]).await;
        let tournament = get(&pool, id).await.unwrap().unwrap();
        assert_eq!(tournament.state, "finished");
        let places: Vec<(i64, Option<i32>)> = entrants(&pool, id)
            .await
            .unwrap()
            .into_iter()
            .map(|entrant| (entrant.player_id, entrant.place))
            .collect();
        assert_eq!(
            places,
            [
                (p[0], Some(3)),
                (p[1], Some(1)),
                (p[2], Some(3)),
                (p[3], Some(2))
            ]
        );
    }

    #[sqlx::test]
    async fn an_ash_bracket_is_played_by_the_first_rounds_losers(pool: PgPool) {
        let (id, p) = tournament(
            &pool,
            Format::SingleAsh,
            &["Mario", "Luigi", "Peach", "Daisy"],
        )
        .await;
        assert_eq!(start(&pool, id, Seeding::Joined).await.unwrap(), Ok(()));
        assert_eq!(sets(&pool, id).await.unwrap().len(), 4);
        let first = set_of(&pool, id, p[0]).await;
        play(&pool, id, &first, p[0]).await;
        // The first loser waits in the ash bracket for the other.
        let ash = |sets: Vec<Set>| sets.into_iter().find(|set| set.side == "ash").unwrap();
        let waiting = ash(sets(&pool, id).await.unwrap());
        assert_eq!(waiting.state, "pending");
        assert_eq!(waiting.away, Some(p[3]));
        let second = set_of(&pool, id, p[1]).await;
        play(&pool, id, &second, p[1]).await;
        // Seeded by the seed expected to lose: 3 before 4. It is a final.
        let ash_final = set_of(&pool, id, p[3]).await;
        assert_eq!(ash_final.side, "ash");
        assert_eq!(
            (ash_final.home, ash_final.away, ash_final.games),
            (Some(p[2]), Some(p[3]), 5)
        );
        // The final and the ash final go on side by side; the tournament
        // ends with both.
        let final_set = set_of(&pool, id, p[0]).await;
        play(&pool, id, &final_set, p[0]).await;
        assert_eq!(get(&pool, id).await.unwrap().unwrap().state, "running");
        play(&pool, id, &ash_final, p[3]).await;
        assert_eq!(get(&pool, id).await.unwrap().unwrap().state, "finished");
        // The places are the main bracket's.
        let places: Vec<Option<i32>> = entrants(&pool, id)
            .await
            .unwrap()
            .into_iter()
            .map(|entrant| entrant.place)
            .collect();
        assert_eq!(places, [Some(1), Some(2), Some(3), Some(3)]);
    }

    #[sqlx::test]
    async fn a_set_lists_the_games_it_played(pool: PgPool) {
        let (id, p) =
            tournament(&pool, Format::Single, &["Mario", "Luigi", "Peach", "Daisy"]).await;
        assert_eq!(start(&pool, id, Seeding::Joined).await.unwrap(), Ok(()));
        let first = set_of(&pool, id, p[0]).await;
        assert!(played_games(&pool, id).await.unwrap().is_empty());
        // The away player wins both: home and away as the set has them.
        play(&pool, id, &first, p[3]).await;
        let lost = PlayedGame {
            home_captain: "mario".into(),
            away_captain: "bowser".into(),
            home_score: 1,
            away_score: 2,
            stadium: Some("the_palace".into()),
            interrupted: false,
        };
        let played = played_games(&pool, id).await.unwrap();
        assert_eq!(played.len(), 1);
        assert_eq!(played[&first.id], [lost.clone(), lost]);
    }

    #[sqlx::test]
    async fn entries_close_when_it_starts_or_is_full(pool: PgPool) {
        let (id, p) = tournament(&pool, Format::Double, &["Mario"]).await;
        assert_eq!(join(&pool, id, p[0]).await.unwrap(), Err(Refusal::Joined));
        assert_eq!(
            start(&pool, id, Seeding::Joined).await.unwrap(),
            Err(Refusal::TooFew)
        );
        let luigi = player(&pool, "Luigi").await;
        join(&pool, id, luigi).await.unwrap().unwrap();
        assert_eq!(
            start(&pool, id, Seeding::Given(vec![luigi, luigi]))
                .await
                .unwrap(),
            Err(Refusal::BadSeeding)
        );
        start(&pool, id, Seeding::Given(vec![luigi, p[0]]))
            .await
            .unwrap()
            .unwrap();
        let peach = player(&pool, "Peach").await;
        assert_eq!(
            join(&pool, id, peach).await.unwrap(),
            Err(Refusal::State("running".into()))
        );
        // Two players in double elimination: the first set, the grand final
        // and its reset.
        let all = sets(&pool, id).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!((all[0].home, all[0].away), (Some(luigi), Some(p[0])));
        sqlx::query!(
            "UPDATE tournaments SET max_entrants = 2 WHERE tournament_id = $1",
            id
        )
        .execute(&pool)
        .await
        .unwrap();
        let (other, _) = tournament(&pool, Format::Single, &["Yoshi", "Wario"]).await;
        sqlx::query!(
            "UPDATE tournaments SET max_entrants = 2 WHERE tournament_id = $1",
            other
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(join(&pool, other, peach).await.unwrap(), Err(Refusal::Full));
    }

    #[sqlx::test]
    async fn a_set_without_a_result_is_played_again(pool: PgPool) {
        let (id, p) = tournament(&pool, Format::Single, &["Mario", "Luigi"]).await;
        start(&pool, id, Seeding::Joined).await.unwrap().unwrap();
        let set = set_of(&pool, id, p[0]).await;
        let made = ready(&pool, id, set.id, p[0], true)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(set_of(&pool, id, p[0]).await.state, "playing");
        // A READY taken back is only the match's.
        let back = ready(&pool, id, set.id, p[0], false)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(back.id, made.id);
        matches::cancel(&pool, made.id, p[1])
            .await
            .unwrap()
            .unwrap();
        advance(&pool).await.unwrap();
        let again = set_of(&pool, id, p[0]).await;
        assert_eq!(
            (again.state.as_str(), again.match_id),
            ("ready", Some(made.id))
        );
        // A READY taken back before there is a match changes nothing.
        assert_eq!(
            ready(&pool, id, set.id, p[1], false).await.unwrap(),
            Ok(None)
        );
        play(&pool, id, &again, p[0]).await;
        assert_eq!(get(&pool, id).await.unwrap().unwrap().state, "finished");
    }

    #[sqlx::test]
    async fn a_player_who_leaves_loses_their_sets(pool: PgPool) {
        let (id, p) =
            tournament(&pool, Format::Double, &["Mario", "Luigi", "Peach", "Daisy"]).await;
        start(&pool, id, Seeding::Joined).await.unwrap().unwrap();
        let set = set_of(&pool, id, p[3]).await;
        let made = ready(&pool, id, set.id, p[3], true)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        // Daisy leaves in the middle of it: Mario goes on, the match is
        // called off, and nobody drops into the losers' bracket from it.
        let called_off = leave(&pool, id, p[3]).await.unwrap().unwrap();
        assert_eq!(called_off, [made.id]);
        let all = sets(&pool, id).await.unwrap();
        assert_eq!(all[0].state, "finished");
        assert_eq!(
            (all[0].winner, all[0].decided_by.as_deref()),
            (Some(p[0]), Some("walkover"))
        );
        assert_eq!(
            matches::find(&pool, made.id).await.unwrap().unwrap().state,
            "cancelled"
        );
        assert_eq!(leave(&pool, id, p[3]).await.unwrap(), Err(Refusal::NotIn));
        // The other loser of the first round goes on without playing.
        let second = set_of(&pool, id, p[1]).await;
        play(&pool, id, &second, p[1]).await;
        let losers_first = sets(&pool, id)
            .await
            .unwrap()
            .into_iter()
            .find(|set| set.side == "losers" && set.round == 1)
            .unwrap();
        assert_eq!(
            (losers_first.state.as_str(), losers_first.winner),
            ("finished", Some(p[2]))
        );
    }

    #[sqlx::test]
    async fn the_organizer_decides_a_disputed_set(pool: PgPool) {
        let (id, p) = tournament(&pool, Format::Single, &["Mario", "Luigi", "Peach"]).await;
        start(&pool, id, Seeding::Joined).await.unwrap().unwrap();
        // Seed 1 has a bye; 2 plays 3, and their reports disagree.
        let set = set_of(&pool, id, p[1]).await;
        ready(&pool, id, set.id, p[1], true).await.unwrap().unwrap();
        let made = ready(&pool, id, set.id, p[2], true)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        matches::game_ready(&pool, made.id, 0, true).await.unwrap();
        for player_id in [p[1], p[2]] {
            matches::report(&pool, made.id, player_id, &[game(3, 0)])
                .await
                .unwrap()
                .unwrap();
        }
        advance(&pool).await.unwrap();
        assert_eq!(set_of_state(&pool, id, set.id).await, "disputed");
        assert_eq!(
            decide(&pool, id, set.id, Some(p[0])).await.unwrap(),
            Err(Refusal::NotIn)
        );
        assert_eq!(
            decide(&pool, id, set.id, Some(p[2])).await.unwrap(),
            Ok(vec![])
        );
        let decided = sets(&pool, id).await.unwrap();
        let decided = decided.iter().find(|s| s.id == set.id).unwrap();
        assert_eq!(
            (decided.winner, decided.decided_by.as_deref()),
            (Some(p[2]), Some("organizer"))
        );
        // The final now has its players; changing the semifinal is still
        // possible until the final has a result.
        let final_set = set_of(&pool, id, p[0]).await;
        assert_eq!(final_set.away, Some(p[2]));
        assert_eq!(
            decide(&pool, id, set.id, Some(p[1])).await.unwrap(),
            Ok(vec![])
        );
        assert_eq!(set_of(&pool, id, p[0]).await.away, Some(p[1]));
        decide(&pool, id, final_set.id, Some(p[0]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            decide(&pool, id, set.id, Some(p[2])).await.unwrap(),
            Err(Refusal::State("finished".into()))
        );
    }

    #[sqlx::test]
    async fn a_forfeit_gives_the_game_up(pool: PgPool) {
        let (id, p) = tournament(&pool, Format::Single, &["Mario", "Luigi"]).await;
        start(&pool, id, Seeding::Joined).await.unwrap().unwrap();
        // The final is a best of 5; Luigi leaves the first game on purpose:
        // Mario has that game, and the set goes on.
        let set = set_of(&pool, id, p[0]).await;
        assert_eq!(set.games, 5);
        let made = ready(&pool, id, set.id, p[0], true)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        ready(&pool, id, set.id, p[1], true).await.unwrap().unwrap();
        matches::game_ready(&pool, made.id, 0, true).await.unwrap();
        matches::forfeit(&pool, made.id, p[1], &[])
            .await
            .unwrap()
            .unwrap();
        advance(&pool).await.unwrap();
        let done = sets(&pool, id).await.unwrap();
        assert_eq!((done[0].state.as_str(), done[0].winner), ("ready", None));
        assert_eq!((done[0].home_wins, done[0].away_wins), (Some(1), Some(0)));
        assert_eq!(get(&pool, id).await.unwrap().unwrap().state, "running");
    }

    #[sqlx::test]
    async fn the_lower_seed_plays_at_home_first_then_each_in_turn(pool: PgPool) {
        let (id, p) = tournament(&pool, Format::Single, &["Mario", "Luigi"]).await;
        start(&pool, id, Seeding::Joined).await.unwrap().unwrap();
        // Mario is seed 1 in the set's home place; Luigi, seed 2, plays the
        // first game at home.
        let set = set_of(&pool, id, p[0]).await;
        assert_eq!((set.home, set.away), (Some(p[0]), Some(p[1])));
        let home_of = |context: serde_json::Value| context["home"].as_str().map(str::to_owned);
        let mario = set_context(&pool, set.id, p[0]).await.unwrap().unwrap();
        assert_eq!(home_of(mario.clone()).as_deref(), Some("opponent"));
        let luigi = set_context(&pool, set.id, p[1]).await.unwrap().unwrap();
        assert_eq!(home_of(luigi).as_deref(), Some("you"));
        assert_eq!(
            (mario["won"].as_i64(), mario["lost"].as_i64()),
            (Some(0), Some(0))
        );
        // Luigi wins the first game: Mario is at home in the second.
        let made = ready(&pool, id, set.id, p[0], true)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        ready(&pool, id, set.id, p[1], true).await.unwrap().unwrap();
        matches::game_ready(&pool, made.id, 0, true).await.unwrap();
        for (player_id, seen) in [(p[0], game(1, 2)), (p[1], game(2, 1))] {
            let mut seen = seen;
            if player_id == p[1] {
                std::mem::swap(&mut seen.my_captain, &mut seen.opponent_captain);
            }
            matches::report(&pool, made.id, player_id, &[seen])
                .await
                .unwrap()
                .unwrap();
        }
        advance(&pool).await.unwrap();
        let mario = set_context(&pool, set.id, p[0]).await.unwrap().unwrap();
        assert_eq!(home_of(mario.clone()).as_deref(), Some("you"));
        assert_eq!(
            (mario["won"].as_i64(), mario["lost"].as_i64()),
            (Some(0), Some(1))
        );
        assert_eq!(mario["state"].as_str(), Some("ready"));
        let luigi = set_context(&pool, set.id, p[1]).await.unwrap().unwrap();
        assert_eq!(
            (luigi["won"].as_i64(), luigi["lost"].as_i64()),
            (Some(1), Some(0))
        );
        let page = sets(&pool, id).await.unwrap();
        let seed_of = |player: i64| (player == p[1]).then_some(1).or(Some(0));
        assert_eq!(page[0].game_home(seed_of), Some(p[0]));
    }

    #[test]
    fn the_away_place_is_at_home_first_without_seeds() {
        assert_eq!(game_home((1, 2), |_| None, 0), Some(2));
        assert_eq!(game_home((1, 2), |_| None, 1), Some(1));
        // Seed 0 is the best: the other starts at home, wherever they are.
        let seeds = |player: i64| Some(if player == 1 { 3 } else { 0 });
        assert_eq!(game_home((1, 2), seeds, 0), Some(1));
        assert_eq!(game_home((1, 2), seeds, 3), Some(2));
    }

    #[sqlx::test]
    async fn a_game_that_broke_off_goes_on_from_where_it_was(pool: PgPool) {
        // Mario's and Luigi's games are Anosch's and Sam's WFC profiles.
        let games = crate::admin::Games::new(pool).await;
        let pool = games.pool.clone();
        let (id, p) = tournament(&pool, Format::Single, &["Mario", "Luigi"]).await;
        players::bind_wfc(&pool, p[0], games.anosch).await.unwrap();
        players::bind_wfc(&pool, p[1], games.sam).await.unwrap();
        start(&pool, id, Seeding::Joined).await.unwrap().unwrap();
        let set = set_of(&pool, id, p[0]).await;
        let made = ready(&pool, id, set.id, p[0], true)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        ready(&pool, id, set.id, p[1], true).await.unwrap().unwrap();
        matches::game_ready(&pool, made.id, 0, true).await.unwrap();
        // Sam's game (Luigi, at home in it) saw it break off at 2-1 after 70
        // seconds, at the Palace.
        let mut luigi = games.report(games.sam, 300, "home", "start");
        luigi["sidekicks"] = serde_json::json!({"home": ["toad", "boo", "koopa"], "away": ["birdo", "boo", "shy_guy"]});
        games.send(&luigi).await;
        let mut broke = crate::admin::Games::game(luigi, 1, 2, 1);
        broke["game"]["ending"] = serde_json::json!("disconnect");
        broke["game"]["seconds"] = serde_json::json!(70);
        games.send(&broke).await;
        for (player_id, mine, theirs) in [(p[0], 1, 2), (p[1], 2, 1)] {
            let mut seen = game(mine, theirs);
            seen.interrupted = true;
            seen.played = Some(0.4);
            if player_id == p[1] {
                std::mem::swap(&mut seen.my_captain, &mut seen.opponent_captain);
            }
            matches::report(&pool, made.id, player_id, &[seen])
                .await
                .unwrap()
                .unwrap();
        }
        advance(&pool).await.unwrap();
        // Not counted: the set's next game goes on from it.
        let now = sets(&pool, id).await.unwrap().remove(0);
        assert_eq!(now.state, "ready");
        assert_eq!((now.home_wins, now.away_wins), (None, None));
        let resume = now.resume.clone().expect("the game goes on");
        assert_eq!(
            (resume.stadium.as_str(), resume.seconds),
            ("the_palace", 70)
        );
        assert_eq!((resume.home.player, resume.home.score), (p[1], 2));
        assert_eq!(resume.home.sidekicks, ["toad", "boo", "koopa"]);
        assert_eq!(
            (
                resume.away.player,
                resume.away.captain.as_str(),
                resume.away.score
            ),
            (p[0], "bowser", 1)
        );
        assert_eq!(now.game_home(|_| None), Some(p[1]));
        let mario = set_context(&pool, set.id, p[0]).await.unwrap().unwrap();
        assert_eq!(mario["home"], "opponent");
        assert_eq!(mario["resume"]["you"]["score"], 1);
        assert_eq!(mario["resume"]["opponent"]["captain"], "mario");
        assert_eq!(mario["resume"]["home"], "opponent");
        // Its match is played in the same stadium; a result clears it.
        let next = ready(&pool, id, set.id, p[0], true)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(next.rules.stadium, "the_palace");
        ready(&pool, id, set.id, p[1], true).await.unwrap().unwrap();
        matches::game_ready(&pool, next.id, 0, true).await.unwrap();
        for (player_id, mine, theirs) in [(p[0], 1, 3), (p[1], 3, 1)] {
            let mut seen = game(mine, theirs);
            if player_id == p[1] {
                std::mem::swap(&mut seen.my_captain, &mut seen.opponent_captain);
            }
            matches::report(&pool, next.id, player_id, &[seen])
                .await
                .unwrap()
                .unwrap();
        }
        advance(&pool).await.unwrap();
        let after = sets(&pool, id).await.unwrap().remove(0);
        assert_eq!(
            (after.home_wins, after.away_wins, after.resume),
            (Some(0), Some(1), None)
        );
    }

    async fn set_of_state(pool: &PgPool, id: i64, set_id: i64) -> String {
        sets(pool, id)
            .await
            .unwrap()
            .into_iter()
            .find(|set| set.id == set_id)
            .unwrap()
            .state
    }
}

//! Matches between two players (docs/profiles.md, "Matches"): a challenge,
//! its answer, and what both games report. Readiness for meeting in RANKED
//! is in [`crate::pairing`].

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;

use super::rules::{RuleSet, Rules};

/// The states of a match still to be played.
const OPEN: [&str; 2] = ["challenged", "accepted"];
/// The most games a report can hold: a series of 7.
const MAX_GAMES: usize = 7;

/// A match as it is stored.
#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub id: i64,
    /// `challenged`, `accepted`, `declined`, `cancelled`, `expired`,
    /// `finished` or `disputed`.
    pub state: String,
    /// The challenger.
    pub home: i64,
    /// The challenged.
    pub away: i64,
    pub rule_set: String,
    pub rules: Rules,
    pub rules_hash: String,
    /// Who won the series; none for a draw or before the result.
    pub winner: Option<i64>,
    pub created_at: OffsetDateTime,
}

impl Match {
    /// The side of `player_id`: 0 the challenger, 1 the challenged.
    pub fn side(&self, player_id: i64) -> Option<usize> {
        if player_id == self.home {
            Some(0)
        } else if player_id == self.away {
            Some(1)
        } else {
            None
        }
    }

    pub fn opponent(&self, player_id: i64) -> i64 {
        if player_id == self.home {
            self.away
        } else {
            self.home
        }
    }

    pub fn is_open(&self) -> bool {
        OPEN.contains(&self.state.as_str())
    }
}

/// One game as a player's game saw it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Game {
    pub my_score: u32,
    pub opponent_score: u32,
    pub my_captain: String,
    pub opponent_captain: String,
    pub stadium: String,
}

impl Game {
    /// The same game as the opponent saw it.
    fn mirrored(&self) -> Self {
        Self {
            my_score: self.opponent_score,
            opponent_score: self.my_score,
            my_captain: self.opponent_captain.clone(),
            opponent_captain: self.my_captain.clone(),
            stadium: self.stadium.clone(),
        }
    }
}

/// Why a request about a match is refused.
#[derive(Debug, PartialEq)]
pub enum Refusal {
    /// No such match of this player's.
    NotFound,
    /// The match is in another state, named.
    State(String),
    /// The challenged player is not the one to answer, or the like.
    NotYours,
    /// The player or the opponent has a match already.
    Busy,
    /// The report is not one of this match's.
    BadReport(String),
    /// This player has reported already.
    Reported,
}

/// Ends the matches that waited too long: challenges after 10 minutes,
/// accepted matches after 2 hours without a result. Returns the ended ones.
pub async fn expire(pool: &PgPool) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        "UPDATE matches SET state = 'expired', updated_at = now()
         WHERE state = 'challenged' AND created_at < now() - interval '10 minutes'
            OR state = 'accepted' AND updated_at < now() - interval '2 hours'
         RETURNING match_id",
    )
    .fetch_all(pool)
    .await
}

/// `player_id`'s match still to be played, or else the last one that ended
/// within the hour, for the launcher to show its outcome.
pub async fn current(pool: &PgPool, player_id: i64) -> Result<Option<Match>, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT match_id, state, home, away, rule_set, rules, rules_hash, winner, created_at
         FROM matches
         WHERE (home = $1 OR away = $1)
           AND (state IN ('challenged', 'accepted') OR updated_at > now() - interval '1 hour')
         ORDER BY state IN ('challenged', 'accepted') DESC, created_at DESC
         LIMIT 1",
        player_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|row| {
        stored(
            row.match_id,
            row.state,
            row.home,
            row.away,
            row.rule_set,
            &row.rules,
            row.rules_hash,
            row.winner,
            row.created_at,
        )
    }))
}

/// `player_id`'s match `match_id`.
pub async fn get(
    pool: &PgPool,
    match_id: i64,
    player_id: i64,
) -> Result<Option<Match>, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT match_id, state, home, away, rule_set, rules, rules_hash, winner, created_at
         FROM matches WHERE match_id = $1 AND (home = $2 OR away = $2)",
        match_id,
        player_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|row| {
        stored(
            row.match_id,
            row.state,
            row.home,
            row.away,
            row.rule_set,
            &row.rules,
            row.rules_hash,
            row.winner,
            row.created_at,
        )
    }))
}

/// `home` challenges `away` to a match by `rule_set`. Each may have one
/// match still to be played at a time.
pub async fn challenge(
    pool: &PgPool,
    home: i64,
    away: i64,
    rule_set: &RuleSet,
) -> Result<Result<Match, Refusal>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    // Two challenges at once must not both pass the check below.
    sqlx::query!(
        "SELECT player_id FROM players WHERE player_id IN ($1, $2) ORDER BY player_id FOR UPDATE",
        home,
        away,
    )
    .fetch_all(&mut *transaction)
    .await?;
    let busy = sqlx::query_scalar!(
        "SELECT count(*) AS \"busy!\" FROM matches
         WHERE state IN ('challenged', 'accepted') AND (home IN ($1, $2) OR away IN ($1, $2))",
        home,
        away,
    )
    .fetch_one(&mut *transaction)
    .await?;
    if busy > 0 {
        return Ok(Err(Refusal::Busy));
    }
    let row = sqlx::query!(
        "INSERT INTO matches (kind, state, home, away, rule_set, rules, rules_hash)
         VALUES ('test', 'challenged', $1, $2, $3, $4, $5)
         RETURNING match_id, created_at",
        home,
        away,
        rule_set.name,
        rule_set.rules.to_json(),
        rule_set.hash,
    )
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(Ok(Match {
        id: row.match_id,
        state: "challenged".into(),
        home,
        away,
        rule_set: rule_set.name.clone(),
        rules: rule_set.rules.clone(),
        rules_hash: rule_set.hash.clone(),
        winner: None,
        created_at: row.created_at,
    }))
}

/// The challenged player accepts or declines.
pub async fn answer(
    pool: &PgPool,
    match_id: i64,
    player_id: i64,
    accept: bool,
) -> Result<Result<Match, Refusal>, sqlx::Error> {
    let Some(found) = get(pool, match_id, player_id).await? else {
        return Ok(Err(Refusal::NotFound));
    };
    if found.away != player_id {
        return Ok(Err(Refusal::NotYours));
    }
    let state = if accept { "accepted" } else { "declined" };
    move_state(pool, found, &["challenged"], state).await
}

/// Either player calls the match off before it has a result.
pub async fn cancel(
    pool: &PgPool,
    match_id: i64,
    player_id: i64,
) -> Result<Result<Match, Refusal>, sqlx::Error> {
    let Some(found) = get(pool, match_id, player_id).await? else {
        return Ok(Err(Refusal::NotFound));
    };
    move_state(pool, found, &OPEN, "cancelled").await
}

/// `player_id`'s game reports the series. The second report settles it: the
/// match is `finished` if both saw the same games, else `disputed`.
pub async fn report(
    pool: &PgPool,
    match_id: i64,
    player_id: i64,
    games: &[Game],
) -> Result<Result<Match, Refusal>, sqlx::Error> {
    let Some(found) = get(pool, match_id, player_id).await? else {
        return Ok(Err(Refusal::NotFound));
    };
    if found.state != "accepted" {
        return Ok(Err(Refusal::State(found.state)));
    }
    if let Err(problem) = check_report(&found.rules, games) {
        return Ok(Err(Refusal::BadReport(problem)));
    }
    let mut transaction = pool.begin().await?;
    let inserted = sqlx::query!(
        "INSERT INTO match_reports (match_id, player_id, games) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING",
        match_id,
        player_id,
        serde_json::to_string(games).expect("games serialize"),
    )
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Ok(Err(Refusal::Reported));
    }
    let opponent = found.opponent(player_id);
    let other = sqlx::query_scalar!(
        "SELECT games FROM match_reports WHERE match_id = $1 AND player_id = $2",
        match_id,
        opponent,
    )
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(other) = other else {
        transaction.commit().await?;
        return Ok(Ok(found));
    };
    let other: Vec<Game> = serde_json::from_str(&other).unwrap_or_default();
    let settled = settle(&mut transaction, &found, player_id, games, &other).await?;
    transaction.commit().await?;
    Ok(Ok(settled))
}

/// Whether `player_id` has reported `match_id`.
pub async fn reported(pool: &PgPool, match_id: i64, player_id: i64) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar!(
        "SELECT 1 AS \"reported!\" FROM match_reports WHERE match_id = $1 AND player_id = $2",
        match_id,
        player_id,
    )
    .fetch_optional(pool)
    .await?
    .is_some())
}

/// The games `player_id` reported of `match_id`, as they saw them.
pub async fn games(pool: &PgPool, match_id: i64, player_id: i64) -> Result<Vec<Game>, sqlx::Error> {
    let games = sqlx::query_scalar!(
        "SELECT games FROM match_reports WHERE match_id = $1 AND player_id = $2",
        match_id,
        player_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(games
        .and_then(|games| serde_json::from_str(&games).ok())
        .unwrap_or_default())
}

/// Both reports are in: the result if they agree.
async fn settle(
    transaction: &mut Transaction<'_, Postgres>,
    found: &Match,
    player_id: i64,
    games: &[Game],
    other: &[Game],
) -> Result<Match, sqlx::Error> {
    let agree = games.len() == other.len()
        && games
            .iter()
            .zip(other)
            .all(|(mine, theirs)| *mine == theirs.mirrored());
    let (state, winner) = if agree {
        let won = games
            .iter()
            .filter(|game| game.my_score > game.opponent_score)
            .count();
        let lost = games
            .iter()
            .filter(|game| game.my_score < game.opponent_score)
            .count();
        let winner = match won.cmp(&lost) {
            std::cmp::Ordering::Greater => Some(player_id),
            std::cmp::Ordering::Less => Some(found.opponent(player_id)),
            std::cmp::Ordering::Equal => None,
        };
        ("finished", winner)
    } else {
        ("disputed", None)
    };
    sqlx::query!(
        "UPDATE matches SET state = $2, winner = $3, updated_at = now() WHERE match_id = $1",
        found.id,
        state,
        winner,
    )
    .execute(&mut **transaction)
    .await?;
    Ok(Match {
        state: state.into(),
        winner,
        ..found.clone()
    })
}

/// A report fits a series by `rules`: no more games than it has, plausible
/// scores and names.
fn check_report(rules: &Rules, games: &[Game]) -> Result<(), String> {
    if games.is_empty() || games.len() > usize::from(rules.games).min(MAX_GAMES) {
        return Err(format!("a report has 1 to {} games", rules.games));
    }
    for game in games {
        if game.my_score > 99 || game.opponent_score > 99 {
            return Err("scores are 0 to 99".into());
        }
        for name in [&game.my_captain, &game.opponent_captain, &game.stadium] {
            let valid = (1..=32).contains(&name.len())
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
            if !valid {
                return Err("captains and stadiums are lowercase names".into());
            }
        }
    }
    Ok(())
}

/// Moves `found` from one of `from` to `to`.
async fn move_state(
    pool: &PgPool,
    found: Match,
    from: &[&str],
    to: &str,
) -> Result<Result<Match, Refusal>, sqlx::Error> {
    let from: Vec<String> = from.iter().map(|state| state.to_string()).collect();
    let moved = sqlx::query!(
        "UPDATE matches SET state = $2, updated_at = now()
         WHERE match_id = $1 AND state = ANY($3)",
        found.id,
        to,
        &from,
    )
    .execute(pool)
    .await?
    .rows_affected();
    if moved == 0 {
        return Ok(Err(Refusal::State(found.state)));
    }
    Ok(Ok(Match {
        state: to.into(),
        ..found
    }))
}

#[allow(clippy::too_many_arguments)]
fn stored(
    id: i64,
    state: String,
    home: i64,
    away: i64,
    rule_set: String,
    rules: &str,
    rules_hash: String,
    winner: Option<i64>,
    created_at: OffsetDateTime,
) -> Option<Match> {
    let rules = match Rules::from_json(rules) {
        Ok(rules) => rules,
        Err(error) => {
            tracing::warn!(target: "profiles", match_id = id, %error, "match rules not understood");
            return None;
        }
    };
    Some(Match {
        id,
        state,
        home,
        away,
        rule_set,
        rules,
        rules_hash,
        winner,
        created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{
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

    async fn competitive(pool: &PgPool) -> RuleSet {
        rules::list(pool)
            .await
            .unwrap()
            .into_iter()
            .find(|set| set.name == "Competitive")
            .unwrap()
    }

    fn game(mine: u32, theirs: u32) -> Game {
        Game {
            my_score: mine,
            opponent_score: theirs,
            my_captain: "mario".into(),
            opponent_captain: "bowser".into(),
            stadium: "the_palace".into(),
        }
    }

    #[sqlx::test]
    async fn a_challenge_is_accepted_played_and_settled(pool: PgPool) {
        let (mario, bowser) = (player(&pool, "Mario").await, player(&pool, "Bowser").await);
        let rules = competitive(&pool).await;
        let made = challenge(&pool, mario, bowser, &rules)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(made.state, "challenged");
        assert_eq!(made.rules_hash, rules.hash);
        assert_eq!(current(&pool, bowser).await.unwrap(), Some(made.clone()));

        // Only the challenged answers; nobody else sees the match.
        let third = player(&pool, "Luigi").await;
        assert_eq!(
            answer(&pool, made.id, mario, true).await.unwrap(),
            Err(Refusal::NotYours)
        );
        assert_eq!(
            answer(&pool, made.id, third, true).await.unwrap(),
            Err(Refusal::NotFound)
        );
        let accepted = answer(&pool, made.id, bowser, true).await.unwrap().unwrap();
        assert_eq!(accepted.state, "accepted");

        let mario_saw = [game(3, 1), game(0, 2), game(2, 1)];
        let bowser_saw: Vec<Game> = mario_saw.iter().map(Game::mirrored).collect();
        let first = report(&pool, made.id, mario, &mario_saw)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.state, "accepted");
        assert!(reported(&pool, made.id, mario).await.unwrap());
        assert_eq!(
            report(&pool, made.id, mario, &mario_saw).await.unwrap(),
            Err(Refusal::Reported)
        );
        let settled = report(&pool, made.id, bowser, &bowser_saw)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settled.state, "finished");
        assert_eq!(settled.winner, Some(mario));
        assert_eq!(games(&pool, made.id, bowser).await.unwrap(), bowser_saw);

        // Both are free for the next one, which the finished one stays
        // behind.
        let next = challenge(&pool, bowser, mario, &rules)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current(&pool, mario).await.unwrap().unwrap().id, next.id);
    }

    #[sqlx::test]
    async fn reports_that_disagree_dispute_the_match(pool: PgPool) {
        let (mario, bowser) = (player(&pool, "Mario").await, player(&pool, "Bowser").await);
        let rules = competitive(&pool).await;
        let found = challenge(&pool, mario, bowser, &rules)
            .await
            .unwrap()
            .unwrap();
        answer(&pool, found.id, bowser, true)
            .await
            .unwrap()
            .unwrap();
        report(&pool, found.id, mario, &[game(3, 1)])
            .await
            .unwrap()
            .unwrap();
        // Bowser claims the win.
        let disputed = report(&pool, found.id, bowser, &[game(3, 1)])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(disputed.state, "disputed");
        assert_eq!(disputed.winner, None);
    }

    #[sqlx::test]
    async fn one_match_at_a_time(pool: PgPool) {
        let (mario, bowser, luigi) = (
            player(&pool, "Mario").await,
            player(&pool, "Bowser").await,
            player(&pool, "Luigi").await,
        );
        let rules = competitive(&pool).await;
        let found = challenge(&pool, mario, bowser, &rules)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            challenge(&pool, luigi, bowser, &rules).await.unwrap(),
            Err(Refusal::Busy)
        );
        assert_eq!(
            challenge(&pool, mario, luigi, &rules).await.unwrap(),
            Err(Refusal::Busy)
        );
        let declined = answer(&pool, found.id, bowser, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(declined.state, "declined");
        assert_eq!(
            cancel(&pool, found.id, mario).await.unwrap(),
            Err(Refusal::State("declined".into()))
        );
        let again = challenge(&pool, luigi, bowser, &rules)
            .await
            .unwrap()
            .unwrap();
        let cancelled = cancel(&pool, again.id, luigi).await.unwrap().unwrap();
        assert_eq!(cancelled.state, "cancelled");
    }

    #[sqlx::test]
    async fn reports_must_fit_the_series(pool: PgPool) {
        let (mario, bowser) = (player(&pool, "Mario").await, player(&pool, "Bowser").await);
        let rules = competitive(&pool).await;
        let found = challenge(&pool, mario, bowser, &rules)
            .await
            .unwrap()
            .unwrap();
        // Not accepted yet.
        assert_eq!(
            report(&pool, found.id, mario, &[game(1, 0)]).await.unwrap(),
            Err(Refusal::State("challenged".into()))
        );
        answer(&pool, found.id, bowser, true)
            .await
            .unwrap()
            .unwrap();
        for games in [vec![], vec![game(1, 0); 4]] {
            assert!(matches!(
                report(&pool, found.id, mario, &games).await.unwrap(),
                Err(Refusal::BadReport(_))
            ));
        }
        let mut odd = game(1, 0);
        odd.stadium = "<the palace>".into();
        assert!(matches!(
            report(&pool, found.id, mario, &[odd]).await.unwrap(),
            Err(Refusal::BadReport(_))
        ));
    }

    #[sqlx::test]
    async fn waiting_too_long_expires_a_challenge(pool: PgPool) {
        let (mario, bowser) = (player(&pool, "Mario").await, player(&pool, "Bowser").await);
        let rules = competitive(&pool).await;
        let found = challenge(&pool, mario, bowser, &rules)
            .await
            .unwrap()
            .unwrap();
        assert!(expire(&pool).await.unwrap().is_empty());
        sqlx::query!(
            "UPDATE matches SET created_at = now() - interval '11 minutes' WHERE match_id = $1",
            found.id,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(expire(&pool).await.unwrap(), [found.id]);
        assert_eq!(
            current(&pool, mario).await.unwrap().unwrap().state,
            "expired"
        );
    }
}

//! Matches between two players (docs/profiles.md, "Matches" and "Sets"): a
//! challenge, its answer, both players' Ready to play!, the games meeting,
//! and what both games report. Readiness for meeting in RANKED is in
//! [`crate::pairing`].

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;

use super::rules::{RuleSet, Rules};

/// The states of a match still to be played.
const OPEN: [&str; 2] = ["challenged", "accepted"];
/// The most games a report can hold: a series of 7.
const MAX_GAMES: usize = 7;
/// A player whose launcher or game made no request for this long has closed
/// Recharged: their sets without a report of theirs end (`dissolved`).
pub const HEARTBEAT_TIMEOUT_SECS: i32 = 60;
/// Both players were ready this long ago, and their games never met: the set
/// goes back to waiting for Ready to play!.
const GO_TIMEOUT_SECS: i32 = 300;

/// A match as it is stored.
#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub id: i64,
    /// `challenged`, `accepted`, `declined`, `cancelled`, `expired`,
    /// `dissolved`, `finished` or `disputed`.
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
    /// When the challenger and the challenged pressed Ready to play!.
    pub ready_at: [Option<OffsetDateTime>; 2],
    /// Both were ready: the launchers start the games.
    pub go_at: Option<OffsetDateTime>,
    /// The two games found each other in RANKED.
    pub playing_at: Option<OffsetDateTime>,
    /// The set this one plays again.
    pub rematch_of: Option<i64>,
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

    /// Where the set is (docs/profiles.md, "Sets"): `invited`, `open`, `go`,
    /// `playing`, `finished`, `disputed` or `dissolved` (then the state says
    /// why).
    pub fn stage(&self) -> &'static str {
        match self.state.as_str() {
            "challenged" => "invited",
            "accepted" if self.playing_at.is_some() => "playing",
            "accepted" if self.go_at.is_some() => "go",
            "accepted" => "open",
            "finished" => "finished",
            "disputed" => "disputed",
            _ => "dissolved",
        }
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
    /// The match is in another state or stage, named.
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

/// A match's row.
struct Row {
    match_id: i64,
    state: String,
    home: i64,
    away: i64,
    rule_set: String,
    rules: String,
    rules_hash: String,
    winner: Option<i64>,
    created_at: OffsetDateTime,
    home_ready_at: Option<OffsetDateTime>,
    away_ready_at: Option<OffsetDateTime>,
    go_at: Option<OffsetDateTime>,
    playing_at: Option<OffsetDateTime>,
    rematch_of: Option<i64>,
}

/// Ends the sets that waited too long or lost a player, and sends back the
/// ones whose games never met. Returns the ended ones:
/// - challenges after 10 minutes, accepted sets after 2 hours without a
///   change (`expired`);
/// - sets with a player who closed Recharged (no request for
///   [`HEARTBEAT_TIMEOUT_SECS`]) and has not reported (`dissolved`);
/// - a go older than [`GO_TIMEOUT_SECS`] without the games meeting: both
///   players are not ready again.
pub async fn sweep(pool: &PgPool) -> Result<Vec<i64>, sqlx::Error> {
    let mut ended = sqlx::query_scalar!(
        "UPDATE matches SET state = 'expired', updated_at = now()
         WHERE state = 'challenged' AND created_at < now() - interval '10 minutes'
            OR state = 'accepted' AND updated_at < now() - interval '2 hours'
         RETURNING match_id",
    )
    .fetch_all(pool)
    .await?;
    ended.extend(
        sqlx::query_scalar!(
            "UPDATE matches SET state = 'dissolved', updated_at = now()
             WHERE state IN ('challenged', 'accepted')
               AND EXISTS (
                   SELECT 1 FROM players
                   WHERE players.player_id IN (matches.home, matches.away)
                     AND coalesce(players.heartbeat_at, matches.created_at)
                         < now() - make_interval(secs => $1)
                     AND NOT EXISTS (
                         SELECT 1 FROM match_reports
                         WHERE match_reports.match_id = matches.match_id
                           AND match_reports.player_id = players.player_id))
             RETURNING match_id",
            f64::from(HEARTBEAT_TIMEOUT_SECS),
        )
        .fetch_all(pool)
        .await?,
    );
    sqlx::query!(
        "UPDATE matches SET home_ready_at = NULL, away_ready_at = NULL, go_at = NULL,
                            updated_at = now()
         WHERE state = 'accepted' AND playing_at IS NULL
           AND go_at < now() - make_interval(secs => $1)",
        f64::from(GO_TIMEOUT_SECS),
    )
    .execute(pool)
    .await?;
    Ok(ended)
}

/// `player_id`'s match still to be played, or else the last one that ended
/// within the hour, for the launcher to show its outcome.
pub async fn current(pool: &PgPool, player_id: i64) -> Result<Option<Match>, sqlx::Error> {
    let row = sqlx::query_as!(
        Row,
        "SELECT match_id, state, home, away, rule_set, rules, rules_hash, winner, created_at,
                home_ready_at, away_ready_at, go_at, playing_at, rematch_of
         FROM matches
         WHERE (home = $1 OR away = $1)
           AND (state IN ('challenged', 'accepted') OR updated_at > now() - interval '1 hour')
         ORDER BY state IN ('challenged', 'accepted') DESC, created_at DESC
         LIMIT 1",
        player_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(stored))
}

/// `player_id`'s match `match_id`.
pub async fn get(
    pool: &PgPool,
    match_id: i64,
    player_id: i64,
) -> Result<Option<Match>, sqlx::Error> {
    let row = sqlx::query_as!(
        Row,
        "SELECT match_id, state, home, away, rule_set, rules, rules_hash, winner, created_at,
                home_ready_at, away_ready_at, go_at, playing_at, rematch_of
         FROM matches WHERE match_id = $1 AND (home = $2 OR away = $2)",
        match_id,
        player_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(stored))
}

/// `home` challenges `away` to a match by `rule_set`, perhaps again
/// (`rematch_of`). Each may have one match still to be played at a time.
pub async fn challenge(
    pool: &PgPool,
    home: i64,
    away: i64,
    rule_set: &RuleSet,
    rematch_of: Option<i64>,
) -> Result<Result<Match, Refusal>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    lock_players(&mut transaction, home, away).await?;
    if busy(&mut transaction, home, away).await? {
        return Ok(Err(Refusal::Busy));
    }
    // A challenge is the challenger's Ready to play!: the accept starts the
    // set.
    let row = sqlx::query!(
        "INSERT INTO matches (kind, state, home, away, rule_set, rules, rules_hash, rematch_of,
                              home_ready_at)
         VALUES ('test', 'challenged', $1, $2, $3, $4, $5, $6, now())
         RETURNING match_id, created_at, home_ready_at",
        home,
        away,
        rule_set.name,
        rule_set.rules.to_json(),
        rule_set.hash,
        rematch_of,
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
        ready_at: [row.home_ready_at, None],
        go_at: None,
        playing_at: None,
        rematch_of,
    }))
}

/// Two challenges at once must not both pass the busy check.
async fn lock_players(
    transaction: &mut Transaction<'_, Postgres>,
    first: i64,
    second: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "SELECT player_id FROM players WHERE player_id IN ($1, $2) ORDER BY player_id FOR UPDATE",
        first,
        second,
    )
    .fetch_all(&mut **transaction)
    .await?;
    Ok(())
}

/// Whether `first` or `second` has a match still to be played.
async fn busy(
    transaction: &mut Transaction<'_, Postgres>,
    first: i64,
    second: i64,
) -> Result<bool, sqlx::Error> {
    let busy = sqlx::query_scalar!(
        "SELECT count(*) AS \"busy!\" FROM matches
         WHERE state IN ('challenged', 'accepted') AND (home IN ($1, $2) OR away IN ($1, $2))",
        first,
        second,
    )
    .fetch_one(&mut **transaction)
    .await?;
    Ok(busy > 0)
}

/// The challenged player accepts (their Ready to play!: with the
/// challenger's, the set goes) or declines.
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
    let answered = move_state(pool, found, &["challenged"], state).await?;
    if !accept || answered.is_err() {
        return Ok(answered);
    }
    ready_to_play(pool, match_id, player_id, true).await
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

/// `player_id` presses (or takes back) Ready to play! in the launcher. Once
/// both are ready the set goes (`go_at`); taking it back before the games
/// met sets it back.
pub async fn ready_to_play(
    pool: &PgPool,
    match_id: i64,
    player_id: i64,
    ready: bool,
) -> Result<Result<Match, Refusal>, sqlx::Error> {
    // In SET, the columns are the row before the change: both sides' new
    // readiness is written out for go_at.
    let moved = sqlx::query!(
        "UPDATE matches SET
             home_ready_at = CASE WHEN home = $2
                                  THEN CASE WHEN $3 THEN coalesce(home_ready_at, now()) END
                                  ELSE home_ready_at END,
             away_ready_at = CASE WHEN away = $2
                                  THEN CASE WHEN $3 THEN coalesce(away_ready_at, now()) END
                                  ELSE away_ready_at END,
             go_at = CASE WHEN (CASE WHEN home = $2 THEN $3 ELSE home_ready_at IS NOT NULL END)
                           AND (CASE WHEN away = $2 THEN $3 ELSE away_ready_at IS NOT NULL END)
                          THEN coalesce(go_at, now()) END,
             updated_at = now()
         WHERE match_id = $1 AND (home = $2 OR away = $2)
           AND state = 'accepted' AND playing_at IS NULL",
        match_id,
        player_id,
        ready,
    )
    .execute(pool)
    .await?
    .rows_affected();
    let Some(found) = get(pool, match_id, player_id).await? else {
        return Ok(Err(Refusal::NotFound));
    };
    if moved == 0 {
        return Ok(Err(Refusal::State(found.stage().into())));
    }
    Ok(Ok(found))
}

/// The game of `side` is online for the set and plays by its rules: that
/// side is ready (an older launcher has no Ready to play!), and once the two
/// games found each other (`paired`) the set is playing.
pub async fn game_ready(
    pool: &PgPool,
    match_id: i64,
    side: usize,
    paired: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE matches SET
             home_ready_at = CASE WHEN $2 = 0 THEN coalesce(home_ready_at, now()) ELSE home_ready_at END,
             away_ready_at = CASE WHEN $2 = 1 THEN coalesce(away_ready_at, now()) ELSE away_ready_at END,
             go_at = CASE WHEN $3 THEN coalesce(go_at, now()) ELSE go_at END,
             playing_at = CASE WHEN $3 THEN coalesce(playing_at, now()) ELSE playing_at END
         WHERE match_id = $1 AND state = 'accepted'",
        match_id,
        i32::try_from(side).unwrap_or(-1),
        paired,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// `player_id` plays the ended set `match_id` again, by the same rules: the
/// opponent's rematch of it, if they offered one already, is accepted;
/// otherwise this is a new challenge.
pub async fn rematch(
    pool: &PgPool,
    match_id: i64,
    player_id: i64,
) -> Result<Result<Match, Refusal>, sqlx::Error> {
    let Some(found) = get(pool, match_id, player_id).await? else {
        return Ok(Err(Refusal::NotFound));
    };
    if !matches!(found.state.as_str(), "finished" | "disputed") {
        return Ok(Err(Refusal::State(found.stage().into())));
    }
    let opponent = found.opponent(player_id);
    let offered = sqlx::query_scalar!(
        "SELECT match_id FROM matches
         WHERE rematch_of = $1 AND state = 'challenged' AND home = $2 AND away = $3",
        match_id,
        opponent,
        player_id,
    )
    .fetch_optional(pool)
    .await?;
    if let Some(offered) = offered {
        return answer(pool, offered, player_id, true).await;
    }
    let rule_set = RuleSet {
        id: 0,
        name: found.rule_set.clone(),
        rules: found.rules.clone(),
        hash: found.rules_hash.clone(),
    };
    challenge(pool, player_id, opponent, &rule_set, Some(match_id)).await
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
    if let Err(problem) = check_report(&found.rules, games) {
        return Ok(Err(Refusal::BadReport(problem)));
    }
    let mut transaction = pool.begin().await?;
    // Both games report right after the final whistle: the second one must
    // see the first's report, and a cancel can't slip in between.
    let state = sqlx::query_scalar!(
        "SELECT state FROM matches WHERE match_id = $1 FOR UPDATE",
        match_id,
    )
    .fetch_one(&mut *transaction)
    .await?;
    if state != "accepted" {
        return Ok(Err(Refusal::State(state)));
    }
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

fn stored(row: Row) -> Option<Match> {
    let rules = match Rules::from_json(&row.rules) {
        Ok(rules) => rules,
        Err(error) => {
            tracing::warn!(target: "profiles", match_id = row.match_id, %error, "match rules not understood");
            return None;
        }
    };
    Some(Match {
        id: row.match_id,
        state: row.state,
        home: row.home,
        away: row.away,
        rule_set: row.rule_set,
        rules,
        rules_hash: row.rules_hash,
        winner: row.winner,
        created_at: row.created_at,
        ready_at: [row.home_ready_at, row.away_ready_at],
        go_at: row.go_at,
        playing_at: row.playing_at,
        rematch_of: row.rematch_of,
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
        let made = challenge(&pool, mario, bowser, &rules, None)
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
        let next = challenge(&pool, bowser, mario, &rules, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current(&pool, mario).await.unwrap().unwrap().id, next.id);
    }

    #[sqlx::test]
    async fn reports_that_disagree_dispute_the_match(pool: PgPool) {
        let (mario, bowser) = (player(&pool, "Mario").await, player(&pool, "Bowser").await);
        let rules = competitive(&pool).await;
        let found = challenge(&pool, mario, bowser, &rules, None)
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
        let found = challenge(&pool, mario, bowser, &rules, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            challenge(&pool, luigi, bowser, &rules, None).await.unwrap(),
            Err(Refusal::Busy)
        );
        assert_eq!(
            challenge(&pool, mario, luigi, &rules, None).await.unwrap(),
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
        let again = challenge(&pool, luigi, bowser, &rules, None)
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
        let found = challenge(&pool, mario, bowser, &rules, None)
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
        let found = challenge(&pool, mario, bowser, &rules, None)
            .await
            .unwrap()
            .unwrap();
        assert!(sweep(&pool).await.unwrap().is_empty());
        sqlx::query!(
            "UPDATE matches SET created_at = now() - interval '11 minutes' WHERE match_id = $1",
            found.id,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(sweep(&pool).await.unwrap(), [found.id]);
        assert_eq!(
            current(&pool, mario).await.unwrap().unwrap().state,
            "expired"
        );
    }

    /// The players' launchers or games made a request `seconds` ago.
    async fn seen(pool: &PgPool, seconds: i32, players: &[i64]) {
        sqlx::query!(
            "UPDATE players SET heartbeat_at = now() - make_interval(secs => $1)
             WHERE player_id = ANY($2)",
            f64::from(seconds),
            players,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[sqlx::test]
    async fn both_ready_go_and_their_games_play(pool: PgPool) {
        let (mario, bowser) = (player(&pool, "Mario").await, player(&pool, "Bowser").await);
        let rules = competitive(&pool).await;
        let found = challenge(&pool, mario, bowser, &rules, None)
            .await
            .unwrap()
            .unwrap();
        // The challenge is the challenger's Ready to play!; the button does
        // nothing before the challenge is accepted.
        assert_eq!(found.stage(), "invited");
        assert!(found.ready_at[0].is_some() && found.ready_at[1].is_none());
        assert_eq!(
            ready_to_play(&pool, found.id, mario, true).await.unwrap(),
            Err(Refusal::State("invited".into()))
        );
        // The accept is the other's: the set goes.
        let both = answer(&pool, found.id, bowser, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(both.stage(), "go");
        assert!(both.ready_at[0].is_some() && both.ready_at[1].is_some());
        // Taken back before the games met: open again, until both are ready.
        let back = ready_to_play(&pool, found.id, mario, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(back.stage(), "open");
        assert_eq!(back.go_at, None);
        ready_to_play(&pool, found.id, bowser, false)
            .await
            .unwrap()
            .unwrap();
        let one = ready_to_play(&pool, found.id, mario, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(one.stage(), "open");
        assert!(one.ready_at[0].is_some() && one.ready_at[1].is_none());
        ready_to_play(&pool, found.id, bowser, true)
            .await
            .unwrap()
            .unwrap();
        // The games: one waits, then both meet.
        game_ready(&pool, found.id, 0, false).await.unwrap();
        assert_eq!(
            get(&pool, found.id, mario).await.unwrap().unwrap().stage(),
            "go"
        );
        game_ready(&pool, found.id, 1, true).await.unwrap();
        assert_eq!(
            get(&pool, found.id, mario).await.unwrap().unwrap().stage(),
            "playing"
        );
        // Playing: Ready to play! no longer changes anything.
        assert_eq!(
            ready_to_play(&pool, found.id, bowser, false).await.unwrap(),
            Err(Refusal::State("playing".into()))
        );
    }

    #[sqlx::test]
    async fn an_older_launchers_game_counts_as_ready(pool: PgPool) {
        let (mario, bowser) = (player(&pool, "Mario").await, player(&pool, "Bowser").await);
        let rules = competitive(&pool).await;
        let found = challenge(&pool, mario, bowser, &rules, None)
            .await
            .unwrap()
            .unwrap();
        answer(&pool, found.id, bowser, true)
            .await
            .unwrap()
            .unwrap();
        game_ready(&pool, found.id, 1, false).await.unwrap();
        let open = ready_to_play(&pool, found.id, mario, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(open.stage(), "go");
    }

    #[sqlx::test]
    async fn closing_recharged_dissolves_the_set(pool: PgPool) {
        let (mario, bowser) = (player(&pool, "Mario").await, player(&pool, "Bowser").await);
        let rules = competitive(&pool).await;
        let found = challenge(&pool, mario, bowser, &rules, None)
            .await
            .unwrap()
            .unwrap();
        answer(&pool, found.id, bowser, true)
            .await
            .unwrap()
            .unwrap();
        seen(&pool, 5, &[mario, bowser]).await;
        assert!(sweep(&pool).await.unwrap().is_empty());
        // Mario reported and closed everything: the set waits for Bowser's.
        report(&pool, found.id, mario, &[game(1, 0)])
            .await
            .unwrap()
            .unwrap();
        seen(&pool, 61, &[mario]).await;
        assert!(sweep(&pool).await.unwrap().is_empty());
        // Bowser's Recharged went away without a report.
        seen(&pool, 61, &[bowser]).await;
        assert_eq!(sweep(&pool).await.unwrap(), [found.id]);
        let ended = get(&pool, found.id, mario).await.unwrap().unwrap();
        assert_eq!(
            (ended.state.as_str(), ended.stage()),
            ("dissolved", "dissolved")
        );
    }

    #[sqlx::test]
    async fn a_go_without_the_games_meeting_waits_again(pool: PgPool) {
        let (mario, bowser) = (player(&pool, "Mario").await, player(&pool, "Bowser").await);
        let rules = competitive(&pool).await;
        let found = challenge(&pool, mario, bowser, &rules, None)
            .await
            .unwrap()
            .unwrap();
        answer(&pool, found.id, bowser, true)
            .await
            .unwrap()
            .unwrap();
        ready_to_play(&pool, found.id, mario, true)
            .await
            .unwrap()
            .unwrap();
        ready_to_play(&pool, found.id, bowser, true)
            .await
            .unwrap()
            .unwrap();
        seen(&pool, 1, &[mario, bowser]).await;
        sqlx::query!(
            "UPDATE matches SET go_at = now() - interval '6 minutes' WHERE match_id = $1",
            found.id,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(sweep(&pool).await.unwrap().is_empty());
        let waiting = get(&pool, found.id, mario).await.unwrap().unwrap();
        assert_eq!(waiting.stage(), "open");
        assert_eq!(waiting.ready_at, [None, None]);
    }

    #[sqlx::test]
    async fn a_rematch_by_both_starts_the_next_set(pool: PgPool) {
        let (mario, bowser) = (player(&pool, "Mario").await, player(&pool, "Bowser").await);
        let rules = competitive(&pool).await;
        let found = challenge(&pool, mario, bowser, &rules, None)
            .await
            .unwrap()
            .unwrap();
        // Not before the set ended.
        assert_eq!(
            rematch(&pool, found.id, bowser).await.unwrap(),
            Err(Refusal::State("invited".into()))
        );
        answer(&pool, found.id, bowser, true)
            .await
            .unwrap()
            .unwrap();
        report(&pool, found.id, mario, &[game(1, 0)])
            .await
            .unwrap()
            .unwrap();
        report(&pool, found.id, bowser, &[game(0, 1)])
            .await
            .unwrap()
            .unwrap();
        // Bowser asks first: a challenge to Mario by the same rules.
        let offered = rematch(&pool, found.id, bowser).await.unwrap().unwrap();
        assert_eq!(
            (offered.stage(), offered.home, offered.rematch_of),
            ("invited", bowser, Some(found.id))
        );
        assert_eq!(offered.rules_hash, found.rules_hash);
        // Mario's Rematch takes it, and the set goes (both asked).
        let next = rematch(&pool, found.id, mario).await.unwrap().unwrap();
        assert_eq!((next.id, next.stage()), (offered.id, "go"));
    }
}

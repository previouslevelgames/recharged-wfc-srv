//! Searches for a random opponent (docs/profiles.md, "Random opponents"). The
//! searcher chooses the rules; the server sends them as a random challenge to
//! a player who is open to challenges or searching too (the one who searched
//! first decides), has Recharged open, plays the same game and has no set to
//! play. A decline, or [`RANDOM_ANSWER_SECS`] without an answer, and it asks
//! another player, until the search stops or a challenge is accepted.

use serde::Serialize;
use sqlx::PgPool;

use super::{
    matches::{self, HEARTBEAT_TIMEOUT_SECS},
    rules::{RuleSet, Rules},
};

/// A search as its player sees it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Search {
    /// The rules' name (the preset the searcher started from).
    pub rule_set: String,
    pub rules: Rules,
    /// When it started (unix seconds).
    pub since: i64,
    /// How many players it asked so far.
    pub asked: i64,
}

/// `player_id` searches by `rule_set` for an opponent playing `game` (the
/// first four characters of its id, R4QE), starting over if they searched
/// already. The search asks its first player at once when it can.
pub async fn start(
    pool: &PgPool,
    player_id: i64,
    rule_set: &RuleSet,
    game: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO searches (player_id, rule_set, rules, rules_hash, game)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (player_id) DO UPDATE
         SET rule_set = EXCLUDED.rule_set, rules = EXCLUDED.rules, rules_hash = EXCLUDED.rules_hash,
             game = EXCLUDED.game, asked = '{}', match_id = NULL, started_at = now()",
        player_id,
        rule_set.name,
        rule_set.rules.to_json(),
        rule_set.hash,
        game,
    )
    .execute(pool)
    .await?;
    advance(pool).await?;
    Ok(())
}

/// The search ends; its challenge still waiting for an answer is withdrawn
/// (`cancelled`). Returns that challenge.
pub async fn stop(pool: &PgPool, player_id: i64) -> Result<Option<i64>, sqlx::Error> {
    let waiting = sqlx::query_scalar!(
        "DELETE FROM searches WHERE player_id = $1 RETURNING match_id",
        player_id,
    )
    .fetch_optional(pool)
    .await?
    .flatten();
    let Some(match_id) = waiting else {
        return Ok(None);
    };
    let cancelled = sqlx::query!(
        "UPDATE matches SET state = 'cancelled', updated_at = now()
         WHERE match_id = $1 AND state = 'challenged' AND random",
        match_id,
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok((cancelled == 1).then_some(match_id))
}

/// `player_id`'s search, if they search.
pub async fn get(pool: &PgPool, player_id: i64) -> Result<Option<Search>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT rule_set, rules, extract(epoch FROM started_at)::BIGINT AS "since!",
                  cardinality(asked)::BIGINT AS "asked!"
           FROM searches WHERE player_id = $1"#,
        player_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|row| {
        Some(Search {
            rule_set: row.rule_set,
            rules: Rules::from_json(&row.rules).ok()?,
            since: row.since,
            asked: row.asked,
        })
    }))
}

/// Moves the searches on: one whose challenge was accepted found its
/// opponent and ends; a player who closed Recharged stops searching; the
/// others without a challenge waiting ask another player (oldest search
/// first). Returns the challenges sent.
pub async fn advance(pool: &PgPool) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query!(
        "DELETE FROM searches USING matches
         WHERE searches.match_id = matches.match_id AND matches.state NOT IN ('challenged', 'declined',
               'cancelled', 'expired')",
    )
    .execute(pool)
    .await?;
    sqlx::query!(
        "DELETE FROM searches USING players
         WHERE searches.player_id = players.player_id
           AND coalesce(players.heartbeat_at, searches.started_at) < now() - make_interval(secs => $1)",
        f64::from(HEARTBEAT_TIMEOUT_SECS),
    )
    .execute(pool)
    .await?;
    let waiting = sqlx::query!(
        "SELECT player_id, rule_set, rules, rules_hash, game, asked FROM searches
         WHERE match_id IS NULL
            OR NOT EXISTS (SELECT 1 FROM matches
                           WHERE matches.match_id = searches.match_id AND matches.state = 'challenged')
         ORDER BY started_at",
    )
    .fetch_all(pool)
    .await?;
    let mut sent = Vec::new();
    for search in waiting {
        let Ok(rules) = Rules::from_json(&search.rules) else {
            continue;
        };
        // Someone free, open to challenges or searching, who plays the same game.
        let opponent = sqlx::query_scalar!(
            "SELECT players.player_id FROM players
             WHERE players.player_id <> $1 AND NOT (players.player_id = ANY($2))
               AND players.heartbeat_at > now() - make_interval(secs => $3)
               AND coalesce(players.activity, '') <> 'match'
               AND (players.available
                    OR EXISTS (SELECT 1 FROM searches WHERE searches.player_id = players.player_id))
               AND EXISTS (SELECT 1 FROM player_wfc_profiles JOIN profiles USING (profile_id)
                           WHERE player_wfc_profiles.player_id = players.player_id
                             AND left(profiles.gsbrcd, 4) = $4)
               AND NOT EXISTS (SELECT 1 FROM matches
                               WHERE matches.state IN ('challenged', 'accepted')
                                 AND players.player_id IN (matches.home, matches.away))
             ORDER BY random() LIMIT 1",
            search.player_id,
            &search.asked,
            f64::from(HEARTBEAT_TIMEOUT_SECS),
            search.game,
        )
        .fetch_optional(pool)
        .await?;
        let Some(opponent) = opponent else {
            continue;
        };
        let rule_set = RuleSet {
            id: 0,
            name: search.rule_set,
            rules,
            hash: search.rules_hash,
        };
        // The searcher has a set to play now (they accepted another
        // challenge, or a search that started earlier asked them): busy.
        if let Ok(found) =
            matches::challenge(pool, search.player_id, opponent, &rule_set, None, true).await?
        {
            sqlx::query!(
                "UPDATE searches SET match_id = $2, asked = array_append(asked, $3)
                 WHERE player_id = $1",
                search.player_id,
                found.id,
                opponent,
            )
            .execute(pool)
            .await?;
            tracing::info!(target: "profiles", match_id = found.id, home = search.player_id, away = opponent, "random challenge");
            sent.push(found.id);
        }
    }
    Ok(sent)
}

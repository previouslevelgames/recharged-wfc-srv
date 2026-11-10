//! The Competitive page: the rule sets matches are played by, which admins
//! edit here, the players who signed in from the launcher, and their latest
//! matches (docs/profiles.md).

use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;

use super::page::snapshot;
use crate::profiles::rules::{self, RuleSet};

/// Matches shown, the latest first.
const SHOWN: i64 = 50;

#[derive(Serialize)]
pub struct Competitive {
    rule_sets: Vec<RuleSet>,
    totals: Totals,
    matches: Vec<Match>,
    /// What a rule set can hold, for the editor.
    choices: Choices,
}

#[derive(Serialize)]
struct Totals {
    players: i64,
    discord: i64,
    test: i64,
    /// Players without Discord or a test name (docs/profiles.md, "Players").
    mii: i64,
    matches: i64,
    finished: i64,
}

#[derive(Serialize)]
struct Match {
    id: i64,
    state: String,
    rule_set: String,
    challenger: String,
    challenged: String,
    /// The winner's name; none for a draw or before a result.
    winner: Option<String>,
    /// The name of the player who gave the set up, which the other won.
    forfeited_by: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

#[derive(Serialize)]
struct Choices {
    tweaks: &'static [&'static str],
    skills: &'static [&'static str],
    stadiums: &'static [&'static str],
    series: &'static [u8],
    stadium_cheats: &'static [&'static str],
    player_cheats: &'static [&'static str],
    powerup_cheats: &'static [&'static str],
}

pub async fn read(pool: &PgPool) -> Result<Competitive, sqlx::Error> {
    let rule_sets = rules::list(pool).await?;
    let mut tx = snapshot(pool).await?;
    let totals = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM players) AS "players!",
             (SELECT count(*) FROM player_identities WHERE provider = 'discord') AS "discord!",
             (SELECT count(*) FROM player_identities WHERE provider = 'test') AS "test!",
             (SELECT count(*) FROM players
              WHERE NOT EXISTS (SELECT 1 FROM player_identities
                                WHERE player_identities.player_id = players.player_id
                                  AND provider IN ('discord', 'test'))) AS "mii!",
             (SELECT count(*) FROM matches) AS "matches!",
             (SELECT count(*) FROM matches WHERE state = 'finished') AS "finished!""#,
    )
    .fetch_one(&mut *tx)
    .await?;
    let matches = sqlx::query_as!(
        Match,
        r#"SELECT matches.match_id AS id, matches.state, matches.rule_set,
                  home.display_name AS challenger, away.display_name AS challenged,
                  winner.display_name AS "winner?", forfeiter.display_name AS "forfeited_by?",
                  matches.created_at, matches.updated_at
           FROM matches
           JOIN players home ON home.player_id = matches.home
           JOIN players away ON away.player_id = matches.away
           LEFT JOIN players winner ON winner.player_id = matches.winner
           LEFT JOIN players forfeiter ON forfeiter.player_id = matches.forfeited_by
           ORDER BY matches.created_at DESC
           LIMIT $1"#,
        SHOWN,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Competitive {
        rule_sets,
        totals: Totals {
            players: totals.players,
            discord: totals.discord,
            test: totals.test,
            mii: totals.mii,
            matches: totals.matches,
            finished: totals.finished,
        },
        matches,
        choices: Choices {
            tweaks: &rules::TWEAKS,
            skills: &rules::SKILLS,
            stadiums: &rules::STADIUMS,
            series: &rules::SERIES,
            stadium_cheats: &rules::STADIUM_CHEATS,
            player_cheats: &rules::PLAYER_CHEATS,
            powerup_cheats: &rules::POWERUP_CHEATS,
        },
    })
}

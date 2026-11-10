//! A player's stats (docs/profiles.md, "Stats"): their record in the series
//! their WFC profiles reported (docs/reports.md), by mode, of all time, of the
//! current ranked season and of today, counted as the admin dashboard counts
//! a profile's.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sqlx::PgPool;
use time::{Date, OffsetDateTime, UtcOffset};

use super::{ProfilesState, database_error};
use crate::{
    gamestats::season,
    reports::{
        self,
        record::{self, MODES, Record},
        report::Side,
    },
};

/// The RANKED season's board (`NETWORK_PERSISTENT_*`), whose season the
/// stats follow.
const SEASON: i32 = 0;

#[derive(Debug, Serialize)]
struct Stats {
    /// The UTC day "today" is.
    today: String,
    season: Season,
    /// One per mode, `ranked` then `friends`.
    modes: Vec<ModeStats>,
}

#[derive(Debug, Serialize)]
struct Season {
    first_day: String,
    last_day: String,
}

/// A mode's record, of the series started in each period (their UTC day).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
struct ModeStats {
    mode: &'static str,
    all: Record,
    season: Record,
    today: Record,
}

/// GET /api/v1/me/stats.
pub(super) async fn stats(State(state): State<Arc<ProfilesState>>, headers: HeaderMap) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    match read(&state.pool, player.id, OffsetDateTime::now_utc().date()).await {
        Ok(stats) => Json(stats).into_response(),
        Err(error) => database_error(error),
    }
}

async fn read(pool: &PgPool, player_id: i64, today: Date) -> Result<Stats, sqlx::Error> {
    let first_day = season::first_day(SEASON, today);
    let last_day = season::last_day(SEASON, first_day);
    let mut conn = pool.acquire().await?;
    // Each match once: one between two of the player's own profiles counts
    // from its home side.
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (m.match_id) m.match_id, m.mode, m.state, m.winner, m.started_at,
                  mine.side
           FROM player_wfc_profiles bound
           JOIN online_match_sides mine ON mine.profile_id = bound.profile_id
           JOIN online_matches m ON m.match_id = mine.match_id
           WHERE bound.player_id = $1
           ORDER BY m.match_id, mine.side = 'home' DESC"#,
        player_id,
    )
    .fetch_all(&mut *conn)
    .await?;
    let ids: Vec<i64> = rows.iter().map(|row| row.match_id).collect();
    let reporters = reports::sides(&mut conn, &ids).await?;

    let mut modes: Vec<ModeStats> = MODES
        .iter()
        .map(|&mode| ModeStats {
            mode,
            ..Default::default()
        })
        .collect();
    for row in rows {
        let Some(side) = Side::parse(&row.side) else {
            continue;
        };
        let Some(mode) = MODES.iter().position(|mode| *mode == row.mode) else {
            continue;
        };
        let series = record::Series {
            state: &row.state,
            winner: row.winner.as_deref().and_then(Side::parse),
            side,
        };
        let games = record::counted(reporters.get(&row.match_id));
        let day = row.started_at.to_offset(UtcOffset::UTC).date();
        let stats = &mut modes[mode];
        stats.all.add(&series, &games);
        if (first_day..=last_day).contains(&day) {
            stats.season.add(&series, &games);
        }
        if day == today {
            stats.today.add(&series, &games);
        }
    }
    Ok(Stats {
        today: today.to_string(),
        season: Season {
            first_day: first_day.to_string(),
            last_day: last_day.to_string(),
        },
        modes,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::Month;

    use super::*;
    use crate::{
        admin::Games,
        profiles::players::{self, Identity},
    };

    async fn player(pool: &PgPool, name: &str) -> i64 {
        players::sign_in(
            pool,
            &Identity {
                provider: "test",
                external_id: name,
                username: name,
                display_name: name,
                avatar_url: None,
            },
        )
        .await
        .unwrap()
    }

    async fn started(pool: &PgPool, match_id: i64, at: &str) {
        sqlx::query!(
            "UPDATE online_matches SET started_at = $2::text::timestamptz WHERE match_id = $1",
            match_id,
            at,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[sqlx::test]
    async fn a_players_profiles_count_together_by_period(pool: PgPool) {
        let today = Date::from_calendar_date(2026, Month::November, 10).unwrap();
        let games = Games::new(pool).await;
        let pool = &games.pool;
        let nelly = player(pool, "nelly").await;
        players::bind_wfc(pool, nelly, games.anosch).await.unwrap();

        // Today in UTC (tomorrow at UTC+2): Anosch wins 2-1 against Sam,
        // both reporting.
        let home = games.report(games.anosch, 100, "home", "start");
        let away = games.report(games.sam, 200, "away", "start");
        let today_match = games.send(&home).await;
        games.send(&away).await;
        for (number, mine, theirs) in [(1, 3, 1), (2, 0, 2), (3, 4, 2)] {
            games
                .send(&Games::game(home.clone(), number, mine, theirs))
                .await;
            games
                .send(&Games::game(away.clone(), number, mine, theirs))
                .await;
        }
        started(pool, today_match, "2026-11-11 00:30:00+02").await;
        // Earlier this season: Sam wins 2-0 against a retail game.
        let alone = games.report(games.sam, 400, "away", "start");
        let season_match = games.send(&alone).await;
        for (number, home, away) in [(1, 1, 2), (2, 0, 3)] {
            games
                .send(&Games::game(alone.clone(), number, home, away))
                .await;
        }
        sqlx::query!(
            "UPDATE online_matches SET state = 'finished', winner = 'away' WHERE match_id = $1",
            season_match,
        )
        .execute(pool)
        .await
        .unwrap();
        started(pool, season_match, "2026-11-06 12:00:00Z").await;
        // Last season: a friend battle of Anosch's, still going.
        let mut friendly = games.report(games.anosch, 300, "away", "start");
        friendly["mode"] = json!("friends");
        let old_match = games.send(&friendly).await;
        started(pool, old_match, "2026-10-01 12:00:00Z").await;

        let ranked = |stats: &Stats| stats.modes[0].clone();
        let won = Record {
            series: record::SeriesRecord {
                played: 1,
                won: 1,
                ..Default::default()
            },
            games: record::GamesRecord {
                played: 3,
                won: 2,
                lost: 1,
                goals_for: 7,
                goals_against: 5,
                ..Default::default()
            },
        };
        // Sam's profile isn't Nelly's yet: only Anosch's series count.
        let stats = read(pool, nelly, today).await.unwrap();
        assert_eq!(
            (
                ranked(&stats).all,
                ranked(&stats).season,
                ranked(&stats).today
            ),
            (won, won, won)
        );

        // Both profiles hers: the series between them counts once, from home.
        players::bind_wfc(pool, nelly, games.sam).await.unwrap();
        let stats = read(pool, nelly, today).await.unwrap();
        let both = Record {
            series: record::SeriesRecord {
                played: 2,
                won: 2,
                ..Default::default()
            },
            games: record::GamesRecord {
                played: 5,
                won: 4,
                lost: 1,
                goals_for: 12,
                goals_against: 6,
                ..Default::default()
            },
        };
        assert_eq!(
            (
                ranked(&stats).all,
                ranked(&stats).season,
                ranked(&stats).today
            ),
            (both, both, won)
        );
        let friends = &stats.modes[1];
        assert_eq!(friends.mode, "friends");
        assert_eq!(
            (friends.all.series.played, friends.all.series.in_progress),
            (1, 1)
        );
        assert_eq!((friends.season, friends.today), Default::default());

        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["today"], "2026-11-10");
        assert_eq!(
            json["season"],
            json!({"first_day": "2026-11-05", "last_day": "2026-11-11"})
        );
        assert_eq!(json["modes"][0]["mode"], "ranked");
        assert_eq!(
            json["modes"][0]["today"],
            json!({
                "series": {"played": 1, "won": 1, "lost": 0, "drawn": 0, "disputed": 0,
                           "abandoned": 0, "in_progress": 0},
                "games": {"played": 3, "won": 2, "lost": 1, "drawn": 0, "goals_for": 7,
                          "goals_against": 5},
            })
        );

        // A player without WFC profiles has played nothing.
        let sam = player(pool, "sam").await;
        let stats = read(pool, sam, today).await.unwrap();
        assert_eq!(stats.modes.len(), 2);
        assert!(
            stats
                .modes
                .iter()
                .all(|mode| { (mode.all, mode.season, mode.today) == Default::default() })
        );
    }
}

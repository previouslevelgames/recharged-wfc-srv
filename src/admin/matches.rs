//! The Matches page: the series Recharged builds report (docs/reports.md),
//! in progress first, then the newest; and a match's detail: both sides,
//! the games as each side saw them, and the reports as they came.

use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;

use super::page::snapshot;
use crate::{
    accounts,
    reports::{
        self,
        outcome::{self, Compared, Reporter},
        report::{Side, Teams},
    },
};

/// Matches listed, in progress first, then the newest.
const SHOWN: i64 = 100;

#[derive(Serialize)]
pub struct Matches {
    totals: Totals,
    matches: Vec<Listed>,
}

#[derive(Serialize)]
pub struct Totals {
    matches: i64,
    in_progress: i64,
    finished: i64,
    disputed: i64,
    abandoned: i64,
    ranked: i64,
    /// Matches only one game reported: the other is a retail game, or its
    /// reports never came.
    one_sided: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    last_report: Option<OffsetDateTime>,
}

/// A match as the list shows it.
#[derive(Serialize)]
pub struct Listed {
    id: i64,
    mode: String,
    release: String,
    best_of: i32,
    state: String,
    /// Once finished: `home` or `away`; none for a draw.
    winner: Option<String>,
    home: Team,
    away: Team,
    /// Games won, of those that count.
    score: Teams<i32>,
    #[serde(with = "time::serde::rfc3339")]
    started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    last_report_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    ended_at: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct Team {
    mii: String,
    captain: String,
    /// The profile whose game reported this side; none when no game did.
    profile_id: Option<i32>,
}

/// A match with all the server knows of it.
#[derive(Serialize)]
pub struct Detail {
    #[serde(rename = "match")]
    summary: Listed,
    /// Home, then away.
    sides: Vec<SideDetail>,
    /// Each game with each side's view of it.
    games: Vec<Compared>,
    /// The reports, as they came.
    events: Vec<Event>,
}

#[derive(Serialize)]
struct SideDetail {
    side: Side,
    mii: String,
    captain: String,
    /// The team's sidekicks: as its own game saw them, else as the other
    /// game did; empty when neither knew them.
    sidekicks: Vec<String>,
    /// The game that reported this side; none for a retail game.
    reporter: Option<ReporterView>,
}

#[derive(Serialize)]
struct ReporterView {
    profile_id: i32,
    /// The code a player of the match's release enters.
    friend_code: String,
    /// The series' start time in Unix seconds, by this game's clock.
    series: i64,
    /// The end's: `decided` or `abandoned`; none before it came.
    reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    opened_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    last_report_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    ended_at: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct Event {
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    side: String,
    event: String,
    /// A game's number.
    game: Option<i32>,
    /// The report's JSON as the game sent it.
    body: String,
}

pub async fn read(pool: &PgPool) -> Result<Matches, sqlx::Error> {
    reports::settle_idle(pool).await?;
    let mut tx = snapshot(pool).await?;
    let matches = Matches {
        totals: totals(&mut tx).await?,
        matches: listed(&mut tx, None, SHOWN).await?,
    };
    tx.commit().await?;
    Ok(matches)
}

pub async fn totals(conn: &mut PgConnection) -> Result<Totals, sqlx::Error> {
    sqlx::query_as!(
        Totals,
        r#"SELECT count(*) AS "matches!",
                  count(*) FILTER (WHERE state = 'in_progress') AS "in_progress!",
                  count(*) FILTER (WHERE state = 'finished') AS "finished!",
                  count(*) FILTER (WHERE state = 'disputed') AS "disputed!",
                  count(*) FILTER (WHERE state = 'abandoned') AS "abandoned!",
                  count(*) FILTER (WHERE mode = 'ranked') AS "ranked!",
                  count(*) FILTER (WHERE sides.reported = 1) AS "one_sided!",
                  max(last_report_at) AS last_report
           FROM online_matches
           JOIN (SELECT match_id, count(*) AS reported FROM online_match_sides GROUP BY match_id)
                sides USING (match_id)"#
    )
    .fetch_one(conn)
    .await
}

/// The match `match_id`, or the first `limit` matches.
async fn listed(
    conn: &mut PgConnection,
    match_id: Option<i64>,
    limit: i64,
) -> Result<Vec<Listed>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT m.match_id, m.mode, m.release, m.best_of, m.state, m.winner, m.home_mii,
                  m.away_mii, m.home_captain, m.away_captain, m.home_wins, m.away_wins,
                  m.started_at, m.last_report_at, m.ended_at,
                  home.profile_id AS "home_profile?", away.profile_id AS "away_profile?"
           FROM online_matches m
           LEFT JOIN online_match_sides home ON home.match_id = m.match_id AND home.side = 'home'
           LEFT JOIN online_match_sides away ON away.match_id = m.match_id AND away.side = 'away'
           WHERE $1::bigint IS NULL OR m.match_id = $1
           ORDER BY (m.state <> 'in_progress'), m.started_at DESC
           LIMIT $2"#,
        match_id,
        limit,
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Listed {
            id: row.match_id,
            mode: row.mode,
            release: row.release,
            best_of: row.best_of,
            state: row.state,
            winner: row.winner,
            home: Team {
                mii: row.home_mii,
                captain: row.home_captain,
                profile_id: row.home_profile,
            },
            away: Team {
                mii: row.away_mii,
                captain: row.away_captain,
                profile_id: row.away_profile,
            },
            score: Teams {
                home: row.home_wins,
                away: row.away_wins,
            },
            started_at: row.started_at,
            last_report_at: row.last_report_at,
            ended_at: row.ended_at,
        })
        .collect())
}

/// The match `match_id`, if there is one.
pub async fn detail(pool: &PgPool, match_id: i64) -> Result<Option<Detail>, sqlx::Error> {
    reports::settle_idle(pool).await?;
    let mut tx = snapshot(pool).await?;
    let Some(summary) = listed(&mut tx, Some(match_id), 1).await?.pop() else {
        tx.commit().await?;
        return Ok(None);
    };
    let reporters = reports::sides(&mut tx, &[match_id])
        .await?
        .remove(&match_id)
        .unwrap_or_default();
    let events = sqlx::query_as!(
        Event,
        r#"SELECT received_at AS at, side, event, game, body FROM online_match_events
           WHERE match_id = $1
           ORDER BY event_id"#,
        match_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let empty = Default::default();
    let games = outcome::compare(
        reporters.home.as_ref().map_or(&empty, |home| &home.games),
        reporters.away.as_ref().map_or(&empty, |away| &away.games),
    );
    let sides = Side::BOTH
        .into_iter()
        .map(|side| {
            let team = match side {
                Side::Home => &summary.home,
                Side::Away => &summary.away,
            };
            SideDetail {
                side,
                mii: team.mii.clone(),
                captain: team.captain.clone(),
                sidekicks: sidekicks(&reporters, side),
                reporter: reporters.get(side).as_ref().map(|reporter| ReporterView {
                    profile_id: reporter.profile_id,
                    friend_code: accounts::friend_code(reporter.profile_id, &summary.release),
                    series: reporter.series,
                    reason: reporter.reason.clone(),
                    opened_at: reporter.opened_at,
                    last_report_at: reporter.last_report_at,
                    ended_at: reporter.ended_at,
                }),
            }
        })
        .collect();
    Ok(Some(Detail {
        summary,
        sides,
        games,
        events,
    }))
}

/// `side`'s sidekicks: as its own game saw them, else as the other did.
fn sidekicks(reporters: &Teams<Option<Reporter>>, side: Side) -> Vec<String> {
    [reporters.get(side), reporters.get(side.other())]
        .into_iter()
        .flatten()
        .map(|reporter| reporter.sidekicks.get(side))
        .find(|sidekicks| !sidekicks.is_empty())
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use axum::{
        body::Body,
        extract::connect_info::MockConnectInfo,
        http::{Request, header},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::{gpcm::Gpcm, reports::outcome::Status};

    /// Two games logged in from one address: Anosch's profile and Sam's.
    pub struct Games {
        pub pool: PgPool,
        pub gpcm: Arc<Gpcm>,
        pub anosch: i32,
        pub sam: i32,
    }

    impl Games {
        pub async fn new(pool: PgPool) -> Self {
            let gpcm = Arc::new(Gpcm::new(pool.clone(), Arc::default(), Arc::default()));
            let mut profiles = Vec::new();
            for (user_id, mii) in [(1, "Anosch"), (2, "Sam")] {
                accounts::log_in(&pool, user_id).await.unwrap();
                let profile =
                    accounts::profile_login(&pool, user_id, "R4QEaaaaaaa", Some(mii), "10.0.0.1")
                        .await
                        .unwrap();
                gpcm.fake_login(profile.profile_id, [10, 0, 0, 1].into());
                profiles.push(profile.profile_id);
            }
            Self {
                pool,
                gpcm,
                anosch: profiles[0],
                sam: profiles[1],
            }
        }

        /// A report of `profile_id`'s series `series`, its team at `side`.
        pub fn report(&self, profile_id: i32, series: i64, side: &str, event: &str) -> Value {
            let (mii, opponent) = if profile_id == self.anosch {
                ("Anosch", "Sam")
            } else {
                ("Sam", "Anosch")
            };
            json!({
                "profile_id": profile_id, "release": "R4QE", "event": event, "series": series,
                "mode": "ranked", "best_of": 3, "side": side, "mii": mii, "opponent_mii": opponent,
                "captains": {"home": "mario", "away": "bowser"},
                "sidekicks": {"home": ["toad", "toad", "boo"], "away": []},
            })
        }

        pub fn game(mut report: Value, number: i32, home: i32, away: i32) -> Value {
            report["event"] = json!("game");
            report["game"] = json!({
                "number": number, "score": {"home": home, "away": away},
                "captains": {"home": "mario", "away": "bowser"},
                "stadium": "the_palace", "ending": "normal", "seconds": 180,
            });
            report
        }

        /// Sends `report` as its game does. The match it went to.
        pub async fn send(&self, report: &Value) -> i64 {
            let app = reports::router(self.pool.clone(), self.gpcm.clone()).layer(MockConnectInfo(
                std::net::SocketAddr::from(([10, 0, 0, 1], 5000)),
            ));
            let request = Request::post("/api/v1/wfc/reports")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(report.to_string()))
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), 200);
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let answer: Value = serde_json::from_slice(&body).unwrap();
            answer["match_id"].as_i64().unwrap()
        }
    }

    #[sqlx::test]
    async fn matches_in_progress_come_first_then_the_newest(pool: PgPool) {
        let games = Games::new(pool).await;
        // A finished match of both games, then one Anosch plays alone.
        let (home, away) = (
            games.report(games.anosch, 100, "home", "start"),
            games.report(games.sam, 200, "away", "start"),
        );
        let first = games.send(&home).await;
        assert_eq!(games.send(&away).await, first);
        for number in [1, 2] {
            games.send(&Games::game(home.clone(), number, 2, 1)).await;
            games.send(&Games::game(away.clone(), number, 2, 1)).await;
        }
        let alone = games
            .send(&games.report(games.anosch, 300, "away", "start"))
            .await;
        // Started earlier, but still in progress.
        sqlx::query!(
            "UPDATE online_matches SET started_at = started_at - interval '1 hour' WHERE match_id = $1",
            alone
        )
        .execute(&games.pool)
        .await
        .unwrap();

        let page = read(&games.pool).await.unwrap();
        let listed: Vec<_> = page
            .matches
            .iter()
            .map(|listed| (listed.id, listed.state.as_str()))
            .collect();
        assert_eq!(listed, [(alone, "in_progress"), (first, "finished")]);
        let finished = &page.matches[1];
        assert_eq!(finished.winner.as_deref(), Some("home"));
        assert_eq!(finished.score, Teams { home: 2, away: 0 });
        assert_eq!(
            (finished.home.profile_id, finished.away.profile_id),
            (Some(games.anosch), Some(games.sam))
        );
        // Anosch's second match: Sam is known by name only.
        let alone = &page.matches[0];
        assert_eq!(
            (alone.home.mii.as_str(), alone.home.profile_id),
            ("Sam", None)
        );
        assert_eq!(
            (
                page.totals.matches,
                page.totals.in_progress,
                page.totals.finished
            ),
            (2, 1, 1)
        );
        assert_eq!((page.totals.ranked, page.totals.one_sided), (2, 1));
    }

    #[sqlx::test]
    async fn a_match_shows_both_views_and_its_reports(pool: PgPool) {
        let games = Games::new(pool).await;
        let home = games.report(games.anosch, 100, "home", "start");
        let away = games.report(games.sam, 200, "away", "start");
        let match_id = games.send(&home).await;
        games.send(&away).await;
        games.send(&Games::game(home.clone(), 1, 3, 1)).await;
        games.send(&Games::game(away.clone(), 1, 1, 3)).await;
        games.send(&Games::game(home.clone(), 2, 1, 0)).await;

        let detail = detail(&games.pool, match_id).await.unwrap().unwrap();
        assert_eq!(detail.summary.state, "in_progress");
        let statuses: Vec<_> = detail
            .games
            .iter()
            .map(|game| (game.number, game.status))
            .collect();
        assert_eq!(statuses, [(1, Status::Disputed), (2, Status::OnlyHome)]);
        assert_eq!(detail.games[0].away.as_ref().unwrap().score.away, 3);

        let [home_side, away_side] = &detail.sides[..] else {
            panic!("two sides");
        };
        let reporter = home_side.reporter.as_ref().unwrap();
        assert_eq!((reporter.profile_id, reporter.series), (games.anosch, 100));
        assert_eq!(
            reporter.friend_code,
            accounts::friend_code(games.anosch, "R4QE")
        );
        assert_eq!(home_side.sidekicks, ["toad", "toad", "boo"]);
        // Sam's game did not know them; Anosch's knew none of Sam's team.
        assert!(away_side.sidekicks.is_empty());
        assert_eq!(
            (away_side.mii.as_str(), away_side.captain.as_str()),
            ("Sam", "bowser")
        );

        let events: Vec<_> = detail
            .events
            .iter()
            .map(|event| (event.side.as_str(), event.event.as_str(), event.game))
            .collect();
        assert_eq!(
            events,
            [
                ("home", "start", None),
                ("away", "start", None),
                ("home", "game", Some(1)),
                ("away", "game", Some(1)),
                ("home", "game", Some(2)),
            ]
        );
        let body: Value = serde_json::from_str(&detail.events[4].body).unwrap();
        assert_eq!(body["game"]["number"], 2);

        assert!(
            super::detail(&games.pool, match_id + 1)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[sqlx::test]
    async fn the_page_settles_matches_that_went_quiet(pool: PgPool) {
        let games = Games::new(pool).await;
        let match_id = games
            .send(&games.report(games.anosch, 100, "home", "start"))
            .await;
        sqlx::query!(
            "UPDATE online_matches SET last_report_at = now() - interval '40 minutes'
             WHERE match_id = $1",
            match_id
        )
        .execute(&games.pool)
        .await
        .unwrap();
        let page = read(&games.pool).await.unwrap();
        assert_eq!(page.matches[0].state, "abandoned");
        assert!(page.matches[0].ended_at.is_some());
    }
}

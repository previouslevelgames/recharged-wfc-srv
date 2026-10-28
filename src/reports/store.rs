//! Where reports go (docs/reports.md, "Pairing" and "States"). A game's
//! series is its side of a match: the first report of the series opens the
//! side, in the match the other game opened or in a new one. After every
//! report, and once nothing came for 30 minutes, the match's state is worked
//! out again from both sides.

use std::collections::{BTreeMap, HashMap};

use sqlx::{PgConnection, PgPool};
use tracing::info;

use super::{
    outcome::{self, Reporter, View},
    report::{Event, Report, SYNC_CHECKS, Side, SyncError, Teams},
};

/// A refusal's problem at most, in characters: a parser's message can quote
/// the body.
const MAX_PROBLEM: usize = 200;

/// What became of a report.
#[derive(Debug, PartialEq)]
pub struct Recorded {
    pub match_id: i64,
    pub side: Side,
    /// The same report came before: nothing changed.
    pub repeated: bool,
    /// The match's state after it.
    pub state: String,
}

/// Stores `report`; `body` is its JSON as it came, for the match's timeline.
pub async fn record(pool: &PgPool, report: &Report, body: &str) -> Result<Recorded, sqlx::Error> {
    let mut tx = pool.begin().await?;
    lock(&mut tx).await?;
    settle_idle_in(&mut tx).await?;

    let (match_id, side) = match find_side(&mut tx, report).await? {
        Some(found) => found,
        None => open_side(&mut tx, report).await?,
    };
    let event_id = sqlx::query_scalar!(
        "INSERT INTO online_match_events (match_id, side, event, game, body)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT DO NOTHING
         RETURNING event_id",
        match_id,
        side.as_str(),
        report.event.name(),
        report.event.game(),
        body,
    )
    .fetch_optional(&mut *tx)
    .await?;
    if event_id.is_none() {
        let state = sqlx::query_scalar!(
            "SELECT state FROM online_matches WHERE match_id = $1",
            match_id
        )
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(Recorded {
            match_id,
            side,
            repeated: true,
            state,
        });
    }

    match &report.event {
        Event::Start => {}
        Event::Game(game) => {
            sqlx::query!(
                "INSERT INTO online_match_games
                     (match_id, side, number, home_score, away_score, home_captain, away_captain,
                      stadium, ending, seconds, sync_check, sync_frame)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                match_id,
                side.as_str(),
                game.number,
                game.score.home,
                game.score.away,
                game.captains.home,
                game.captains.away,
                game.stadium,
                game.ending,
                game.seconds,
                game.sync.as_ref().map(|sync| sync.check),
                game.sync.as_ref().and_then(|sync| sync.frame),
            )
            .execute(&mut *tx)
            .await?;
        }
        Event::End(reason) => {
            sqlx::query!(
                "UPDATE online_match_sides SET reason = $3, cause = $4, ended_at = now()
                 WHERE match_id = $1 AND side = $2",
                match_id,
                side.as_str(),
                reason,
                report.cause,
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    // Sidekicks a later report knows, when the earlier ones did not; the
    // build the last report named.
    let sidekicks = report.sidekicks.map(|names| {
        names
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>()
    });
    sqlx::query!(
        "UPDATE online_match_sides
         SET last_report_at = now(),
             home_sidekicks = CASE WHEN cardinality($3::text[]) > 0 THEN $3 ELSE home_sidekicks END,
             away_sidekicks = CASE WHEN cardinality($4::text[]) > 0 THEN $4 ELSE away_sidekicks END,
             build = coalesce($5, build)
         WHERE match_id = $1 AND side = $2",
        match_id,
        side.as_str(),
        &sidekicks.home,
        &sidekicks.away,
        report.build,
    )
    .execute(&mut *tx)
    .await?;
    let state = settle(&mut tx, match_id, false).await?;
    tx.commit().await?;
    Ok(Recorded {
        match_id,
        side,
        repeated: false,
        state: state.into(),
    })
}

/// Notes that `profile_id`'s GPCM login ended, `how` (`logout`, `closed`,
/// `lost` or `replaced`): its sides without an end that reported within 30
/// minutes left WFC, whatever the other side did. Their reports can no
/// longer come from that login. Returns their matches.
pub async fn left(pool: &PgPool, profile_id: i32, how: &str) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        "UPDATE online_match_sides SET left_at = now(), left_how = $2
         WHERE profile_id = $1 AND ended_at IS NULL AND left_at IS NULL
           AND last_report_at > now() - interval '30 minutes'
         RETURNING match_id",
        profile_id,
        how,
    )
    .fetch_all(pool)
    .await
}

/// Counts a refused report of `profile_id`, `kind` (`elsewhere` or
/// `invalid`), when the profile exists: its last problem and address stay.
pub async fn refused(
    pool: &PgPool,
    profile_id: i32,
    kind: &str,
    problem: &str,
    address: &str,
) -> Result<(), sqlx::Error> {
    let problem: String = problem.chars().take(MAX_PROBLEM).collect();
    sqlx::query!(
        "INSERT INTO report_refusals (profile_id, kind, problem, address)
         SELECT $1, $2, $3, $4 WHERE EXISTS (SELECT FROM profiles WHERE profile_id = $1)
         ON CONFLICT (profile_id, kind) DO UPDATE
         SET count = report_refusals.count + 1, problem = EXCLUDED.problem,
             address = EXCLUDED.address, last_at = now()",
        profile_id,
        kind,
        problem,
        address,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Settles the matches that heard nothing for 30 minutes: finished if
/// their games decide them, else abandoned. The pages that show matches call
/// it first; a report does it too.
pub async fn settle_idle(pool: &PgPool) -> Result<usize, sqlx::Error> {
    // Most calls find none, and then need not wait for a report.
    let any = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM online_matches
                          WHERE state = 'in_progress'
                            AND last_report_at < now() - interval '30 minutes') AS "any!""#
    )
    .fetch_one(pool)
    .await?;
    if !any {
        return Ok(0);
    }
    let mut tx = pool.begin().await?;
    lock(&mut tx).await?;
    let settled = settle_idle_in(&mut tx).await?;
    tx.commit().await?;
    Ok(settled)
}

/// One report or settling at a time: the two games' starts come together,
/// and the second must find the match the first one opened.
async fn lock(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query!("LOCK TABLE online_matches IN SHARE ROW EXCLUSIVE MODE")
        .execute(conn)
        .await?;
    Ok(())
}

async fn settle_idle_in(conn: &mut PgConnection) -> Result<usize, sqlx::Error> {
    let idle = sqlx::query_scalar!(
        "SELECT match_id FROM online_matches
         WHERE state = 'in_progress' AND last_report_at < now() - interval '30 minutes'",
    )
    .fetch_all(&mut *conn)
    .await?;
    for &match_id in &idle {
        let state = settle(conn, match_id, true).await?;
        info!(target: "reports", match_id, state, "nothing came for 30 minutes");
    }
    Ok(idle.len())
}

/// The side of the report's series, if one of its reports came before.
async fn find_side(
    conn: &mut PgConnection,
    report: &Report,
) -> Result<Option<(i64, Side)>, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT match_id, side FROM online_match_sides WHERE profile_id = $1 AND series = $2",
        report.profile_id,
        report.series,
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.and_then(|row| Some((row.match_id, Side::parse(&row.side)?))))
}

/// Opens the report's side: in the open match whose game named this Mii as
/// its opponent and is named by this report, with the same captains, or in
/// a new match.
async fn open_side(conn: &mut PgConnection, report: &Report) -> Result<(i64, Side), sqlx::Error> {
    let side = report.side;
    let joined = sqlx::query_scalar!(
        "SELECT m.match_id FROM online_matches m
         JOIN online_match_sides first ON first.match_id = m.match_id
         WHERE m.state = 'in_progress'
           AND m.release = $1 AND m.mode = $2 AND m.best_of = $3
           AND m.started_at > now() - interval '3 minutes'
           AND first.side <> $4 AND first.reason IS NULL AND first.profile_id <> $5
           AND first.opponent_mii = $6 AND first.mii = $7
           AND (first.home_captain = $8 OR first.home_captain = 'unknown' OR $8 = 'unknown')
           AND (first.away_captain = $9 OR first.away_captain = 'unknown' OR $9 = 'unknown')
           AND NOT EXISTS (SELECT 1 FROM online_match_sides taken
                           WHERE taken.match_id = m.match_id AND taken.side = $4)
         ORDER BY m.started_at DESC
         LIMIT 1",
        report.release,
        report.mode,
        report.best_of,
        side.as_str(),
        report.profile_id,
        report.mii,
        report.opponent_mii,
        report.captains.home,
        report.captains.away,
    )
    .fetch_optional(&mut *conn)
    .await?;
    let match_id = match joined {
        Some(match_id) => match_id,
        None => {
            let mut miis = Teams {
                home: report.opponent_mii.as_str(),
                away: report.opponent_mii.as_str(),
            };
            *miis.get_mut(side) = report.mii.as_str();
            sqlx::query_scalar!(
                "INSERT INTO online_matches
                     (release, mode, best_of, home_mii, away_mii, home_captain, away_captain)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 RETURNING match_id",
                report.release,
                report.mode,
                report.best_of,
                miis.home,
                miis.away,
                report.captains.home,
                report.captains.away,
            )
            .fetch_one(&mut *conn)
            .await?
        }
    };
    sqlx::query!(
        "INSERT INTO online_match_sides
             (match_id, side, profile_id, series, mii, opponent_mii, home_captain, away_captain)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        match_id,
        side.as_str(),
        report.profile_id,
        report.series,
        report.mii,
        report.opponent_mii,
        report.captains.home,
        report.captains.away,
    )
    .execute(conn)
    .await?;
    Ok((match_id, side))
}

/// Works the match's state out again from its sides, and keeps it: after a
/// report, or `idle` once nothing came for 30 minutes. An idle match ended
/// with its last report, or 30 minutes later when abandoned.
async fn settle(
    conn: &mut PgConnection,
    match_id: i64,
    idle: bool,
) -> Result<&'static str, sqlx::Error> {
    let best_of = sqlx::query_scalar!(
        "SELECT best_of FROM online_matches WHERE match_id = $1",
        match_id
    )
    .fetch_one(&mut *conn)
    .await?;
    let sides = sides(&mut *conn, &[match_id])
        .await?
        .remove(&match_id)
        .unwrap_or_default();
    let outcome = outcome::settle(&sides, best_of, idle);
    sqlx::query!(
        "UPDATE online_matches
         SET state = $2, winner = $3, home_wins = $4, away_wins = $5, home_mii = $6,
             away_mii = $7, home_captain = $8, away_captain = $9,
             last_report_at = CASE WHEN $10 THEN last_report_at ELSE now() END,
             ended_at = CASE WHEN $2 = 'in_progress' THEN NULL
                             WHEN $10 AND $2 = 'abandoned' THEN last_report_at + interval '30 minutes'
                             WHEN $10 THEN last_report_at
                             ELSE coalesce(ended_at, now()) END
         WHERE match_id = $1",
        match_id,
        outcome.state,
        outcome.winner.map(Side::as_str),
        outcome.wins.home,
        outcome.wins.away,
        outcome.miis.home,
        outcome.miis.away,
        outcome.captains.home,
        outcome.captains.away,
        idle,
    )
    .execute(conn)
    .await?;
    Ok(outcome.state)
}

/// The sides of the matches `match_ids` with the games they reported, by
/// match. A side nobody reported is `None`.
pub async fn sides(
    conn: &mut PgConnection,
    match_ids: &[i64],
) -> Result<HashMap<i64, Teams<Option<Reporter>>>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT match_id, side, profile_id, series, mii, opponent_mii, home_captain,
                away_captain, home_sidekicks, away_sidekicks, reason, cause, build, left_at,
                left_how, opened_at, last_report_at, ended_at
         FROM online_match_sides
         WHERE match_id = ANY($1)",
        match_ids,
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut games: HashMap<(i64, Side), BTreeMap<i32, View>> = HashMap::new();
    for row in sqlx::query!(
        "SELECT match_id, side, number, home_score, away_score, home_captain, away_captain,
                stadium, ending, seconds, sync_check, sync_frame
         FROM online_match_games
         WHERE match_id = ANY($1)",
        match_ids,
    )
    .fetch_all(&mut *conn)
    .await?
    {
        let Some(side) = Side::parse(&row.side) else {
            continue;
        };
        let view = View {
            score: Teams {
                home: row.home_score,
                away: row.away_score,
            },
            captains: Teams {
                home: row.home_captain,
                away: row.away_captain,
            },
            stadium: row.stadium,
            ending: row.ending,
            seconds: row.seconds,
            sync: row
                .sync_check
                .and_then(|check| SYNC_CHECKS.into_iter().find(|known| *known == check))
                .map(|check| SyncError {
                    check,
                    frame: row.sync_frame,
                }),
        };
        games
            .entry((row.match_id, side))
            .or_default()
            .insert(row.number, view);
    }

    let mut matches: HashMap<i64, Teams<Option<Reporter>>> = HashMap::new();
    for row in rows {
        let Some(side) = Side::parse(&row.side) else {
            continue;
        };
        let reporter = Reporter {
            profile_id: row.profile_id,
            series: row.series,
            mii: row.mii,
            opponent_mii: row.opponent_mii,
            captains: Teams {
                home: row.home_captain,
                away: row.away_captain,
            },
            sidekicks: Teams {
                home: row.home_sidekicks,
                away: row.away_sidekicks,
            },
            reason: row.reason,
            cause: row.cause,
            build: row.build,
            left_at: row.left_at,
            left_how: row.left_how,
            opened_at: row.opened_at,
            last_report_at: row.last_report_at,
            ended_at: row.ended_at,
            games: games.remove(&(row.match_id, side)).unwrap_or_default(),
        };
        *matches.entry(row.match_id).or_default().get_mut(side) = Some(reporter);
    }
    Ok(matches)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{accounts, reports::report::tests::start};

    /// A profile of a console of its own, as GPCM's login makes it.
    async fn profile(pool: &PgPool, user_id: i64, mii: &str) -> i32 {
        accounts::log_in(pool, user_id).await.unwrap();
        accounts::profile_login(pool, user_id, "R4QEaaaaaaa", Some(mii), "10.0.0.1")
            .await
            .unwrap()
            .profile_id
    }

    /// The two games of a match: Anosch at home, Sam away.
    struct Pair {
        anosch: i32,
        sam: i32,
    }

    impl Pair {
        async fn new(pool: &PgPool) -> Self {
            Self {
                anosch: profile(pool, 1, "Anosch").await,
                sam: profile(pool, 2, "Sam").await,
            }
        }

        fn anosch(&self) -> Value {
            let mut body = start();
            body["profile_id"] = json!(self.anosch);
            body
        }

        /// Sam's report of the same series, as his game sends it: his clock
        /// started it a few seconds later.
        fn sam(&self) -> Value {
            let mut body = start();
            body["profile_id"] = json!(self.sam);
            body["series"] = json!(1_792_000_004);
            body["side"] = json!("away");
            body["mii"] = json!("Sam");
            body["opponent_mii"] = json!("Anosch");
            body
        }
    }

    fn game(body: Value, number: i32, home: i32, away: i32) -> Value {
        let mut body = body;
        body["event"] = json!("game");
        body["game"] = json!({
            "number": number, "score": {"home": home, "away": away},
            "captains": {"home": "mario", "away": "bowser"},
            "stadium": "the_palace", "ending": "normal", "seconds": 180,
        });
        body
    }

    fn end(body: Value, reason: &str) -> Value {
        let mut body = body;
        body["event"] = json!("end");
        body["reason"] = json!(reason);
        body
    }

    async fn send(pool: &PgPool, body: &Value) -> Recorded {
        let text = body.to_string();
        let report = Report::parse(text.as_bytes()).unwrap();
        record(pool, &report, &text).await.unwrap()
    }

    async fn state(pool: &PgPool, match_id: i64) -> (String, Option<String>, i32, i32) {
        let row = sqlx::query!(
            "SELECT state, winner, home_wins, away_wins FROM online_matches WHERE match_id = $1",
            match_id
        )
        .fetch_one(pool)
        .await
        .unwrap();
        (row.state, row.winner, row.home_wins, row.away_wins)
    }

    async fn matches(pool: &PgPool) -> i64 {
        sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM online_matches"#)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// As if the match's last report came `minutes` ago.
    async fn quiet(pool: &PgPool, match_id: i64, minutes: f64) {
        sqlx::query!(
            "UPDATE online_matches SET last_report_at = now() - make_interval(secs => $2)
             WHERE match_id = $1",
            match_id,
            minutes * 60.0,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[sqlx::test]
    async fn a_side_keeps_its_build_cause_and_sync_error(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let mut start = pair.anosch();
        start["build"] = json!("1.4.2 (abc1234)");
        let opened = send(&pool, &start).await;
        let mut desync = game(pair.anosch(), 1, 0, 0);
        desync["game"]["ending"] = json!("desync");
        desync["game"]["sync"] = json!({"check": "seed", "frame": 90});
        send(&pool, &desync).await;
        let mut stopped = end(pair.anosch(), "abandoned");
        stopped["cause"] = json!("desync");
        send(&pool, &stopped).await;

        let mut conn = pool.acquire().await.unwrap();
        let anosch = sides(&mut conn, &[opened.match_id]).await.unwrap()[&opened.match_id]
            .home
            .clone()
            .unwrap();
        assert_eq!(anosch.build.as_deref(), Some("1.4.2 (abc1234)"));
        assert_eq!(
            (anosch.reason.as_deref(), anosch.cause.as_deref()),
            (Some("abandoned"), Some("desync"))
        );
        assert_eq!(
            anosch.games[&1].sync,
            Some(SyncError {
                check: "seed",
                frame: Some(90)
            })
        );
    }

    #[sqlx::test]
    async fn a_game_that_leaves_wfc_marks_only_its_open_sides(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let open = send(&pool, &pair.anosch()).await;
        send(&pool, &pair.sam()).await;
        send(&pool, &end(pair.sam(), "abandoned")).await;

        assert_eq!(
            left(&pool, pair.anosch, "closed").await.unwrap(),
            [open.match_id]
        );
        assert_eq!(
            left(&pool, pair.anosch, "lost").await.unwrap(),
            [] as [i64; 0],
            "once"
        );
        assert_eq!(
            left(&pool, pair.sam, "logout").await.unwrap(),
            [] as [i64; 0],
            "Sam had ended"
        );

        let mut conn = pool.acquire().await.unwrap();
        let sides = sides(&mut conn, &[open.match_id]).await.unwrap()[&open.match_id].clone();
        let anosch = sides.home.unwrap();
        assert!(anosch.left_at.is_some());
        assert_eq!(anosch.left_how.as_deref(), Some("closed"));
        assert!(sides.away.unwrap().left_at.is_none());
    }

    #[sqlx::test]
    async fn refusals_are_counted_for_known_profiles(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        for problem in ["not logged in", "logged in from 10.0.0.2"] {
            refused(&pool, pair.anosch, "elsewhere", problem, "10.0.0.9")
                .await
                .unwrap();
        }
        refused(
            &pool,
            7,
            "invalid",
            "mode: one of ranked, friends",
            "10.0.0.9",
        )
        .await
        .unwrap();

        let rows = sqlx::query!("SELECT profile_id, kind, count, problem FROM report_refusals")
            .fetch_all(&pool)
            .await
            .unwrap();
        let rows: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.profile_id,
                    row.kind.as_str(),
                    row.count,
                    row.problem.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [(pair.anosch, "elsewhere", 2, "logged in from 10.0.0.2")],
            "no row for a profile that does not exist"
        );
    }

    #[sqlx::test]
    async fn both_games_report_one_match(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let opened = send(&pool, &pair.anosch()).await;
        assert_eq!((opened.side, opened.repeated), (Side::Home, false));
        assert_eq!(opened.state, "in_progress");
        let joined = send(&pool, &pair.sam()).await;
        assert_eq!(joined.match_id, opened.match_id);
        assert_eq!(joined.side, Side::Away);

        for (number, home, away) in [(1, 3, 1), (2, 0, 2)] {
            for body in [pair.anosch(), pair.sam()] {
                let played = send(&pool, &game(body, number, home, away)).await;
                assert_eq!(
                    (played.match_id, played.state.as_str()),
                    (opened.match_id, "in_progress")
                );
            }
        }
        // Anosch's game 3 is in: Sam's report decides the series.
        let first = send(&pool, &game(pair.anosch(), 3, 2, 1)).await;
        assert_eq!(first.state, "in_progress");
        let last = send(&pool, &game(pair.sam(), 3, 2, 1)).await;
        assert_eq!(last.state, "finished");
        assert_eq!(
            state(&pool, opened.match_id).await,
            ("finished".into(), Some("home".into()), 2, 1)
        );
        // The ends change nothing.
        for body in [pair.anosch(), pair.sam()] {
            assert_eq!(send(&pool, &end(body, "decided")).await.state, "finished");
        }
        assert_eq!(matches(&pool).await, 1);

        let sides = sides(&mut pool.acquire().await.unwrap(), &[opened.match_id])
            .await
            .unwrap()
            .remove(&opened.match_id)
            .unwrap();
        let sam = sides.away.unwrap();
        assert_eq!((sam.profile_id, sam.series), (pair.sam, 1_792_000_004));
        assert_eq!(sam.sidekicks.home, ["toad", "koopa", "boo"]);
        assert_eq!(
            (sam.games.len(), sam.reason.as_deref()),
            (3, Some("decided"))
        );
    }

    #[sqlx::test]
    async fn repeated_reports_change_nothing(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let opened = send(&pool, &pair.anosch()).await;
        let again = send(&pool, &pair.anosch()).await;
        assert_eq!(again.match_id, opened.match_id);
        assert!(again.repeated);

        send(&pool, &game(pair.anosch(), 1, 3, 1)).await;
        // The same game again, even told otherwise: the first one stays.
        let repeated = send(&pool, &game(pair.anosch(), 1, 0, 5)).await;
        assert!(repeated.repeated);
        assert_eq!(state(&pool, opened.match_id).await.2, 1);

        send(&pool, &end(pair.anosch(), "decided")).await;
        let ended = send(&pool, &end(pair.anosch(), "abandoned")).await;
        assert!(ended.repeated);
        assert_eq!(ended.state, "finished");
        let events = sqlx::query_scalar!(
            r#"SELECT count(*) AS "count!" FROM online_match_events WHERE match_id = $1"#,
            opened.match_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(events, 3);
    }

    #[sqlx::test]
    async fn a_game_before_any_start_opens_the_match(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let played = send(&pool, &game(pair.anosch(), 1, 1, 0)).await;
        assert!(!played.repeated);
        // Sam's start was lost too: his first game joins the match.
        let joined = send(&pool, &game(pair.sam(), 1, 1, 0)).await;
        assert_eq!(joined.match_id, played.match_id);
        // The start that comes late belongs to the same series.
        let late = send(&pool, &pair.anosch()).await;
        assert_eq!((late.match_id, late.repeated), (played.match_id, false));
        assert_eq!(matches(&pool).await, 1);
    }

    #[sqlx::test]
    async fn each_series_is_a_side_of_its_own(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let first = send(&pool, &pair.anosch()).await;
        send(&pool, &game(pair.anosch(), 1, 2, 0)).await;
        // The game started a new series, the first one never ended.
        let mut next = pair.anosch();
        next["series"] = json!(1_792_000_900);
        let second = send(&pool, &next).await;
        assert_ne!(second.match_id, first.match_id);
        let played = send(&pool, &game(next, 1, 0, 1)).await;
        assert_eq!((played.match_id, played.repeated), (second.match_id, false));
        assert_eq!(state(&pool, first.match_id).await.2, 1);
        assert_eq!(state(&pool, second.match_id).await.3, 1);
    }

    #[sqlx::test]
    async fn only_the_named_opponent_with_the_same_captains_joins(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let luigi = profile(&pool, 3, "Luigi").await;
        let opened = send(&pool, &pair.anosch()).await;

        // Another Mii, other captains, the same side, another release, mode
        // or series length: each a match of its own.
        let mut others = Vec::new();
        let mut stranger = pair.sam();
        stranger["profile_id"] = json!(luigi);
        stranger["mii"] = json!("Luigi");
        others.push(stranger);
        let mut captains = pair.sam();
        captains["captains"]["away"] = json!("wario");
        others.push(captains);
        let mut same_side = pair.sam();
        same_side["side"] = json!("home");
        others.push(same_side);
        for (field, value) in [
            ("release", json!("R4QP")),
            ("mode", json!("friends")),
            ("best_of", json!(5)),
        ] {
            let mut other = pair.sam();
            other[field] = value;
            others.push(other);
        }
        for (series, mut other) in (1_792_000_100..).zip(others) {
            other["series"] = json!(series);
            let alone = send(&pool, &other).await;
            assert_ne!(alone.match_id, opened.match_id, "{other}");
        }

        // An unknown captain is no claim: Sam joins.
        let mut unsure = pair.sam();
        unsure["captains"]["away"] = json!("unknown");
        let joined = send(&pool, &unsure).await;
        assert_eq!(joined.match_id, opened.match_id);
        // Now the match is full: a third game opens its own.
        let mut third = pair.sam();
        third["profile_id"] = json!(luigi);
        assert_ne!(send(&pool, &third).await.match_id, opened.match_id);
    }

    #[sqlx::test]
    async fn a_match_takes_its_second_side_within_3_minutes(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let opened = send(&pool, &pair.anosch()).await;
        sqlx::query!(
            "UPDATE online_matches SET started_at = now() - interval '4 minutes' WHERE match_id = $1",
            opened.match_id
        )
        .execute(&pool)
        .await
        .unwrap();
        let alone = send(&pool, &pair.sam()).await;
        assert_ne!(alone.match_id, opened.match_id);
    }

    #[sqlx::test]
    async fn a_side_that_ended_takes_nobody_in(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let opened = send(&pool, &pair.anosch()).await;
        // The session was left after the first kickoff.
        let ended = send(&pool, &end(pair.anosch(), "abandoned")).await;
        assert_eq!(ended.state, "abandoned");
        let alone = send(&pool, &pair.sam()).await;
        assert_ne!(alone.match_id, opened.match_id);
    }

    #[sqlx::test]
    async fn sides_that_disagree_dispute_the_match(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let opened = send(&pool, &pair.anosch()).await;
        send(&pool, &pair.sam()).await;
        send(&pool, &game(pair.anosch(), 1, 3, 1)).await;
        // Sam claims the win.
        send(&pool, &game(pair.sam(), 1, 1, 3)).await;
        send(&pool, &end(pair.anosch(), "decided")).await;
        let last = send(&pool, &end(pair.sam(), "decided")).await;
        assert_eq!(last.state, "disputed");
        assert_eq!(
            state(&pool, opened.match_id).await,
            ("disputed".into(), None, 0, 0)
        );
    }

    #[sqlx::test]
    async fn a_retail_opponent_leaves_one_side_that_decides_alone(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let opened = send(&pool, &pair.anosch()).await;
        assert_eq!(
            send(&pool, &game(pair.anosch(), 1, 0, 1)).await.state,
            "in_progress"
        );
        // No end comes: the second game decides it.
        let decided = send(&pool, &game(pair.anosch(), 2, 0, 2)).await;
        assert_eq!(decided.state, "finished");
        assert_eq!(
            state(&pool, opened.match_id).await,
            ("finished".into(), Some("away".into()), 0, 2)
        );
        let row = sqlx::query!(
            "SELECT home_mii, away_mii, ended_at FROM online_matches WHERE match_id = $1",
            opened.match_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (row.home_mii.as_str(), row.away_mii.as_str()),
            ("Anosch", "Sam")
        );
        assert!(row.ended_at.is_some());
    }

    #[sqlx::test]
    async fn a_nine_game_series_goes_to_its_ninth_game(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let mut anosch = pair.anosch();
        anosch["best_of"] = json!(9);
        let opened = send(&pool, &anosch).await;
        for number in 1..=8 {
            let (home, away) = if number <= 4 { (1, 0) } else { (0, 1) };
            let recorded = send(&pool, &game(anosch.clone(), number, home, away)).await;
            assert_eq!(recorded.state, "in_progress", "game {number}");
        }
        let decided = send(&pool, &game(anosch.clone(), 9, 2, 1)).await;
        assert_eq!(decided.state, "finished");
        assert_eq!(
            state(&pool, opened.match_id).await,
            ("finished".into(), Some("home".into()), 5, 4)
        );
    }

    #[sqlx::test]
    async fn nothing_for_30_minutes_abandons_an_undecided_match(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let opened = send(&pool, &pair.anosch()).await;
        send(&pool, &pair.sam()).await;
        for body in [pair.anosch(), pair.sam()] {
            send(&pool, &game(body, 1, 1, 0)).await;
        }
        quiet(&pool, opened.match_id, 29.0).await;
        assert_eq!(settle_idle(&pool).await.unwrap(), 0);
        quiet(&pool, opened.match_id, 31.0).await;
        assert_eq!(settle_idle(&pool).await.unwrap(), 1);
        assert_eq!(state(&pool, opened.match_id).await.0, "abandoned");
        assert_eq!(settle_idle(&pool).await.unwrap(), 0);
        let ended = sqlx::query_scalar!(
            r#"SELECT ended_at - last_report_at AS "after!" FROM online_matches WHERE match_id = $1"#,
            opened.match_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(ended.microseconds, 30 * 60 * 1_000_000);

        // A report that still comes shows the series went on.
        let late = send(&pool, &game(pair.anosch(), 2, 2, 0)).await;
        assert_eq!(
            (late.match_id, late.state.as_str()),
            (opened.match_id, "in_progress")
        );
        let late = send(&pool, &game(pair.sam(), 2, 2, 0)).await;
        assert_eq!(late.state, "finished");
    }

    #[sqlx::test]
    async fn nothing_for_30_minutes_settles_a_series_one_side_decided(pool: PgPool) {
        let pair = Pair::new(&pool).await;
        let opened = send(&pool, &pair.anosch()).await;
        send(&pool, &pair.sam()).await;
        for body in [pair.anosch(), pair.sam()] {
            send(&pool, &game(body, 1, 0, 3)).await;
        }
        // Sam's report of the deciding game, and both ends, never came.
        let decided = send(&pool, &game(pair.anosch(), 2, 1, 2)).await;
        assert_eq!(decided.state, "in_progress");
        quiet(&pool, opened.match_id, 45.0).await;
        assert_eq!(settle_idle(&pool).await.unwrap(), 1);
        assert_eq!(
            state(&pool, opened.match_id).await,
            ("finished".into(), Some("away".into()), 0, 2)
        );
    }
}

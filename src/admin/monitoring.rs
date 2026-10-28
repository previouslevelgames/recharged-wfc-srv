//! The Monitoring page: how online matches go wrong (docs/reports.md,
//! "Diagnostics"). Games that went out of sync or lost their connection, as
//! the Recharged builds reported them; games that left WFC in the middle of
//! a match, as GPCM saw it; the error codes the games showed; and the
//! reports the server refused. Over the last [`DAYS`] days (UTC), by day,
//! build, stadium and code.

use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use time::{Duration, OffsetDateTime, Time};

use super::page::snapshot;
use crate::reports;

/// The days shown, today's included.
const DAYS: i64 = 14;
/// Incidents and refusals shown, the newest first.
const SHOWN: i64 = 50;

#[derive(Serialize)]
pub struct Monitoring {
    days: i64,
    /// Newest first; a day without matches is there too.
    by_day: Vec<Day>,
    by_build: Vec<Build>,
    by_stadium: Vec<Stadium>,
    incidents: Vec<Incident>,
    errors: Vec<ErrorCode>,
    refusals: Vec<Refusal>,
}

/// A game counts once, however many sides reported it: out of sync when a
/// side saw it so, else lost when a side lost its connection.
#[derive(Serialize)]
struct Day {
    day: String,
    matches: i64,
    abandoned: i64,
    games: i64,
    desyncs: i64,
    disconnects: i64,
    /// Sides whose game left WFC before their end.
    left: i64,
    /// Error codes the games showed.
    errors: i64,
}

/// The sides one build reported: its own games' endings.
#[derive(Serialize)]
struct Build {
    /// None: builds from before reports named them.
    build: Option<String>,
    sides: i64,
    games: i64,
    desyncs: i64,
    disconnects: i64,
    left: i64,
    #[serde(with = "time::serde::rfc3339")]
    last_seen: OffsetDateTime,
}

#[derive(Serialize)]
struct Stadium {
    stadium: String,
    games: i64,
    desyncs: i64,
    disconnects: i64,
}

/// Something that went wrong for one side of a match.
#[derive(Serialize)]
struct Incident {
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    match_id: i64,
    side: String,
    mii: String,
    profile_id: i32,
    /// `desync` or `disconnect` (a game ended so), `left` (the game left
    /// WFC), or how a series stopped without either: `offline` or
    /// `abandoned`.
    kind: String,
    /// The game, for `desync` and `disconnect`.
    game: Option<i32>,
    /// For `desync`: the first check that failed, and on which frame.
    sync_check: Option<String>,
    sync_frame: Option<i32>,
    /// For `left`: `logout`, `closed`, `lost` or `replaced`.
    how: Option<String>,
    build: Option<String>,
}

/// An error code the games showed.
#[derive(Serialize)]
struct ErrorCode {
    code: i32,
    count: i64,
    /// Profiles logged in from where their error came, and addresses.
    players: i64,
    addresses: i64,
    /// The session states it came in, and the builds that sent it.
    phases: Vec<String>,
    builds: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    last_at: OffsetDateTime,
}

/// The reports of a profile the server refused.
#[derive(Serialize)]
struct Refusal {
    profile_id: i32,
    mii: Option<String>,
    /// `elsewhere` or `invalid`.
    kind: String,
    count: i64,
    /// The last one's.
    problem: String,
    address: String,
    #[serde(with = "time::serde::rfc3339")]
    first_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    last_at: OffsetDateTime,
}

pub async fn read(pool: &PgPool) -> Result<Monitoring, sqlx::Error> {
    reports::settle_idle(pool).await?;
    let today = OffsetDateTime::now_utc().replace_time(Time::MIDNIGHT);
    let since = today - Duration::days(DAYS - 1);
    let mut tx = snapshot(pool).await?;
    let monitoring = Monitoring {
        days: DAYS,
        by_day: by_day(&mut tx, since).await?,
        by_build: by_build(&mut tx, since).await?,
        by_stadium: by_stadium(&mut tx, since).await?,
        incidents: incidents(&mut tx, since).await?,
        errors: errors(&mut tx, since).await?,
        refusals: refusals(&mut tx).await?,
    };
    tx.commit().await?;
    Ok(monitoring)
}

async fn by_day(conn: &mut PgConnection, since: OffsetDateTime) -> Result<Vec<Day>, sqlx::Error> {
    sqlx::query_as!(
        Day,
        r#"WITH days AS (
               SELECT day::date AS day
               FROM generate_series(($1::timestamptz AT TIME ZONE 'UTC')::date,
                                    (now() AT TIME ZONE 'UTC')::date, interval '1 day') AS day),
           games AS (
               SELECT min(reported_at) AS at, bool_or(ending = 'desync') AS desync,
                      bool_or(ending = 'disconnect') AND NOT bool_or(ending = 'desync') AS disconnect
               FROM online_match_games WHERE reported_at >= $1
               GROUP BY match_id, number),
           m AS (
               SELECT (started_at AT TIME ZONE 'UTC')::date AS day, count(*) AS matches,
                      count(*) FILTER (WHERE state = 'abandoned') AS abandoned
               FROM online_matches WHERE started_at >= $1 GROUP BY 1),
           g AS (
               SELECT (at AT TIME ZONE 'UTC')::date AS day, count(*) AS games,
                      count(*) FILTER (WHERE desync) AS desyncs,
                      count(*) FILTER (WHERE disconnect) AS disconnects
               FROM games GROUP BY 1),
           l AS (
               SELECT (left_at AT TIME ZONE 'UTC')::date AS day, count(*) AS left
               FROM online_match_sides WHERE left_at >= $1 GROUP BY 1),
           e AS (
               SELECT (received_at AT TIME ZONE 'UTC')::date AS day, count(*) AS errors
               FROM client_errors WHERE received_at >= $1 GROUP BY 1)
           SELECT days.day::text AS "day!",
                  coalesce(m.matches, 0) AS "matches!", coalesce(m.abandoned, 0) AS "abandoned!",
                  coalesce(g.games, 0) AS "games!", coalesce(g.desyncs, 0) AS "desyncs!",
                  coalesce(g.disconnects, 0) AS "disconnects!", coalesce(l.left, 0) AS "left!",
                  coalesce(e.errors, 0) AS "errors!"
           FROM days
           LEFT JOIN m USING (day) LEFT JOIN g USING (day) LEFT JOIN l USING (day)
           LEFT JOIN e USING (day)
           ORDER BY days.day DESC"#,
        since,
    )
    .fetch_all(conn)
    .await
}

async fn by_build(
    conn: &mut PgConnection,
    since: OffsetDateTime,
) -> Result<Vec<Build>, sqlx::Error> {
    sqlx::query_as!(
        Build,
        r#"SELECT s.build,
                  count(*) AS "sides!",
                  coalesce(sum(g.games), 0)::bigint AS "games!",
                  coalesce(sum(g.desyncs), 0)::bigint AS "desyncs!",
                  coalesce(sum(g.disconnects), 0)::bigint AS "disconnects!",
                  count(*) FILTER (WHERE s.left_at IS NOT NULL) AS "left!",
                  max(s.opened_at) AS "last_seen!"
           FROM online_match_sides s
           LEFT JOIN (SELECT match_id, side, count(*) AS games,
                             count(*) FILTER (WHERE ending = 'desync') AS desyncs,
                             count(*) FILTER (WHERE ending = 'disconnect') AS disconnects
                      FROM online_match_games GROUP BY match_id, side) g
                  USING (match_id, side)
           WHERE s.opened_at >= $1
           GROUP BY s.build
           ORDER BY max(s.opened_at) DESC"#,
        since,
    )
    .fetch_all(conn)
    .await
}

async fn by_stadium(
    conn: &mut PgConnection,
    since: OffsetDateTime,
) -> Result<Vec<Stadium>, sqlx::Error> {
    sqlx::query_as!(
        Stadium,
        r#"SELECT stadium AS "stadium!", count(*) AS "games!",
                  count(*) FILTER (WHERE desync) AS "desyncs!",
                  count(*) FILTER (WHERE disconnect) AS "disconnects!"
           FROM (SELECT coalesce(max(stadium) FILTER (WHERE stadium <> 'unknown'), 'unknown')
                            AS stadium,
                        bool_or(ending = 'desync') AS desync,
                        bool_or(ending = 'disconnect') AND NOT bool_or(ending = 'desync')
                            AS disconnect
                 FROM online_match_games WHERE reported_at >= $1
                 GROUP BY match_id, number) games
           GROUP BY stadium
           ORDER BY 3 DESC, 4 DESC, 2 DESC, 1"#,
        since,
    )
    .fetch_all(conn)
    .await
}

async fn incidents(
    conn: &mut PgConnection,
    since: OffsetDateTime,
) -> Result<Vec<Incident>, sqlx::Error> {
    // A series that stopped with a desync or a lost connection shows as its
    // game; one that stopped without either, as its end.
    sqlx::query_as!(
        Incident,
        r#"SELECT at AS "at!", match_id AS "match_id!", side AS "side!", mii AS "mii!",
                  profile_id AS "profile_id!", kind AS "kind!", game, sync_check, sync_frame,
                  how, build
           FROM (SELECT g.reported_at AS at, g.match_id, g.side, s.mii, s.profile_id,
                        g.ending AS kind, g.number AS game, g.sync_check, g.sync_frame,
                        NULL::text AS how, s.build
                 FROM online_match_games g JOIN online_match_sides s USING (match_id, side)
                 WHERE g.ending <> 'normal' AND g.reported_at >= $1
                 UNION ALL
                 SELECT s.left_at, s.match_id, s.side, s.mii, s.profile_id, 'left', NULL, NULL,
                        NULL, s.left_how, s.build
                 FROM online_match_sides s
                 WHERE s.left_at >= $1
                 UNION ALL
                 SELECT s.ended_at, s.match_id, s.side, s.mii, s.profile_id,
                        coalesce(s.cause, 'abandoned'), NULL, NULL, NULL, NULL, s.build
                 FROM online_match_sides s
                 WHERE s.reason = 'abandoned' AND s.ended_at >= $1
                   AND NOT EXISTS (SELECT FROM online_match_games g
                                   WHERE g.match_id = s.match_id AND g.side = s.side
                                     AND g.ending <> 'normal')) incidents
           ORDER BY at DESC
           LIMIT $2"#,
        since,
        SHOWN,
    )
    .fetch_all(conn)
    .await
}

async fn errors(
    conn: &mut PgConnection,
    since: OffsetDateTime,
) -> Result<Vec<ErrorCode>, sqlx::Error> {
    sqlx::query_as!(
        ErrorCode,
        r#"SELECT code, count(*) AS "count!",
                  count(DISTINCT profile_id) AS "players!",
                  count(DISTINCT address) AS "addresses!",
                  array_remove(array_agg(DISTINCT phase), NULL) AS "phases!",
                  array_remove(array_agg(DISTINCT build), NULL) AS "builds!",
                  max(received_at) AS "last_at!"
           FROM client_errors
           WHERE received_at >= $1
           GROUP BY code
           ORDER BY count(*) DESC, code"#,
        since,
    )
    .fetch_all(conn)
    .await
}

async fn refusals(conn: &mut PgConnection) -> Result<Vec<Refusal>, sqlx::Error> {
    sqlx::query_as!(
        Refusal,
        r#"SELECT r.profile_id, p.ingamesn AS mii, r.kind, r.count, r.problem, r.address,
                  r.first_at, r.last_at
           FROM report_refusals r JOIN profiles p USING (profile_id)
           ORDER BY r.last_at DESC
           LIMIT $1"#,
        SHOWN,
    )
    .fetch_all(conn)
    .await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::admin::matches::tests::Games;

    #[sqlx::test]
    async fn counts_what_went_wrong_and_lists_it(pool: PgPool) {
        let games = Games::new(pool.clone()).await;
        let mut start = games.report(games.anosch, 1, "home", "start");
        start["build"] = json!("1.4.2");
        games.send(&start).await;
        games
            .send(&games.report(games.sam, 2, "away", "start"))
            .await;
        for profile_id in [games.anosch, games.sam] {
            let (series, side) = if profile_id == games.anosch {
                (1, "home")
            } else {
                (2, "away")
            };
            let mut desync = Games::game(games.report(profile_id, series, side, "game"), 1, 0, 0);
            desync["game"]["ending"] = json!("desync");
            desync["game"]["sync"] = json!({"check": "checksum", "frame": 4812});
            games.send(&desync).await;
        }
        reports::left(&pool, games.sam, "closed").await.unwrap();

        let monitoring = read(&pool).await.unwrap();
        let today = &monitoring.by_day[0];
        assert_eq!(monitoring.by_day.len(), DAYS as usize);
        assert_eq!(
            (
                today.matches,
                today.games,
                today.desyncs,
                today.disconnects,
                today.left
            ),
            (1, 1, 1, 0, 1),
            "one game, though both sides reported it"
        );
        let builds: Vec<_> = monitoring
            .by_build
            .iter()
            .map(|b| (b.build.as_deref(), b.sides, b.games, b.desyncs))
            .collect();
        assert_eq!(builds.len(), 2, "{builds:?}");
        assert!(builds.contains(&(Some("1.4.2"), 1, 1, 1)), "{builds:?}");
        assert_eq!(
            monitoring
                .by_stadium
                .iter()
                .map(|s| (s.stadium.as_str(), s.games, s.desyncs))
                .collect::<Vec<_>>(),
            [("the_palace", 1, 1)]
        );
        let mut kinds: Vec<_> = monitoring
            .incidents
            .iter()
            .map(|i| (i.kind.as_str(), i.sync_check.as_deref(), i.how.as_deref()))
            .collect();
        kinds.sort();
        assert_eq!(
            kinds,
            [
                ("desync", Some("checksum"), None),
                ("desync", Some("checksum"), None),
                ("left", None, Some("closed")),
            ]
        );
    }
}

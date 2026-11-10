//! The Monitoring page: how online matches go wrong (docs/reports.md,
//! "Diagnostics"). Games that went out of sync or lost their connection, as
//! the Recharged builds reported them; games that left WFC in the middle of
//! a match, as GPCM saw it; the error codes the games showed; the desync
//! dossiers and where they diverge; and the reports the server refused. Over the last [`DAYS`] days (UTC), by day,
//! build, stadium and code, and the last [`TIMELINE_HOURS`] hours as a
//! timeline, which also shows the players online (`crate::online`).

use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use time::{Duration, OffsetDateTime, Time};

use super::page::snapshot;
use crate::{
    gpcm::Gpcm,
    online,
    reports::{
        self, dossiers,
        report::{Side, Teams},
    },
};

/// The days shown, today's included.
const DAYS: i64 = 14;
/// Incidents and refusals shown, the newest first.
const SHOWN: i64 = 50;
/// The timeline's span, and the width of its columns.
const TIMELINE_HOURS: i64 = 48;
const COLUMN_MINUTES: i32 = 30;
/// Matches on the timeline at most: far more than two days have had.
const TIMELINE_MATCHES: i64 = 5000;
/// The players online on the timeline: the most of each span this long.
const ONLINE_MINUTES: i32 = 10;

#[derive(Serialize)]
pub struct Monitoring {
    days: i64,
    /// Newest first; a day without matches is there too.
    by_day: Vec<Day>,
    by_build: Vec<Build>,
    by_stadium: Vec<Stadium>,
    incidents: Vec<Incident>,
    errors: Vec<ErrorCode>,
    /// The games with a dossier, the newest first, [`SHOWN`] at most.
    desyncs: Vec<Desync>,
    /// Where games diverged, the most frequent first.
    divergences: Vec<Divergence>,
    refusals: Vec<Refusal>,
    timeline: Timeline,
}

/// The last [`TIMELINE_HOURS`] hours: the matches in progress, and what went
/// wrong by column.
#[derive(Serialize)]
struct Timeline {
    #[serde(with = "time::serde::rfc3339")]
    from: OffsetDateTime,
    column_minutes: i32,
    /// Every match in progress at some point since `from`.
    matches: Vec<Span>,
    /// By column and kind, the oldest first; an error code's by code.
    problems: Vec<Problem>,
    /// The players online, the most in every [`ONLINE_MINUTES`], the oldest
    /// first; none before the server sampled them.
    online_minutes: i32,
    online: Vec<Online>,
    /// The players online now.
    online_now: Online,
}

/// Players online: logged in to WFC, of them in an online match, and with
/// Recharged open (`crate::online`).
#[derive(Serialize)]
struct Online {
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    wfc: i32,
    playing: i32,
    recharged: i32,
}

#[derive(Serialize)]
struct Span {
    #[serde(with = "time::serde::rfc3339")]
    started_at: OffsetDateTime,
    /// When play stopped: an abandoned match's last report. None while in
    /// progress.
    #[serde(with = "time::serde::rfc3339::option")]
    ended_at: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct Problem {
    /// The column's start: every [`COLUMN_MINUTES`] from midnight UTC.
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    /// `desync` or `disconnect` (a game, counted once like [`Day`]'s),
    /// `left` (a side's game left WFC) or `error` (an error code shown).
    kind: String,
    /// For `error`.
    code: Option<i32>,
    count: i64,
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
    /// The most players logged in to WFC at once (sampled every minute),
    /// for days the server sampled.
    peak_online: Option<i32>,
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

/// A game's desync dossiers (docs/reports.md, "Desync dossiers"): one side's
/// or both, and where they diverge once both are in.
#[derive(Serialize)]
pub(super) struct Desync {
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    match_id: i64,
    game: i32,
    release: String,
    /// The step whose checksums differ, as the first dossier has it.
    frame: i32,
    /// Each side's dossier, None until it comes.
    home: Option<DossierSide>,
    away: Option<DossierSide>,
    /// Whether both sides run the same binary: then their callers compare.
    same_binary: bool,
    /// `path`, `value`, `count`, `none` or `missing`; None until both came.
    kind: Option<String>,
    diverged_frame: Option<i32>,
    field_index: Option<i32>,
    /// Each side's field there, its raw caller first and as the game's log
    /// names it (`dossiers::Field::describe`).
    home_field: Option<String>,
    away_field: Option<String>,
}

#[derive(Serialize)]
struct DossierSide {
    profile_id: i32,
    mii: Option<String>,
    build: String,
    /// The kind of game module, `linux x86_64`...; empty when unknown.
    platform: String,
    /// How to name the code at one of its callers' offsets.
    symbolize: String,
}

/// The games that diverged at one field of one pair of binaries.
#[derive(Serialize)]
struct Divergence {
    release: String,
    kind: String,
    home: Spot,
    away: Spot,
    /// The callers are offsets in the same binary.
    same_binary: bool,
    games: i64,
    #[serde(with = "time::serde::rfc3339")]
    last_at: OffsetDateTime,
}

/// A side's field, where its binary took it.
#[derive(Serialize)]
struct Spot {
    build: String,
    platform: String,
    /// Raw; None for the shorter side of a `count`.
    caller: Option<i64>,
    /// The caller as the game's log names it, without the field's value.
    field: Option<String>,
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

pub async fn read(pool: &PgPool, gpcm: &Gpcm) -> Result<Monitoring, sqlx::Error> {
    reports::settle_idle(pool).await?;
    let (wfc, playing, recharged) = online::now(pool, gpcm).await?;
    let online_now = Online {
        at: OffsetDateTime::now_utc(),
        wfc,
        playing,
        recharged,
    };
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
        desyncs: desyncs(&mut tx, None).await?,
        divergences: divergences(&mut tx, since).await?,
        refusals: refusals(&mut tx).await?,
        timeline: timeline(&mut tx, online_now).await?,
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
               FROM client_errors WHERE received_at >= $1 GROUP BY 1),
           minutes AS (
               SELECT at, sum(wfc)::integer AS wfc
               FROM online_samples WHERE at >= $1 GROUP BY at),
           o AS (
               SELECT (at AT TIME ZONE 'UTC')::date AS day, max(wfc) AS peak_online
               FROM minutes GROUP BY 1)
           SELECT days.day::text AS "day!",
                  coalesce(m.matches, 0) AS "matches!", coalesce(m.abandoned, 0) AS "abandoned!",
                  coalesce(g.games, 0) AS "games!", coalesce(g.desyncs, 0) AS "desyncs!",
                  coalesce(g.disconnects, 0) AS "disconnects!", coalesce(l.left, 0) AS "left!",
                  coalesce(e.errors, 0) AS "errors!", o.peak_online
           FROM days
           LEFT JOIN m USING (day) LEFT JOIN g USING (day) LEFT JOIN l USING (day)
           LEFT JOIN e USING (day) LEFT JOIN o USING (day)
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

/// The games with dossiers, the newest first, [`SHOWN`] at most: every
/// match's, or `match_id`'s alone.
pub(super) async fn desyncs(
    conn: &mut PgConnection,
    match_id: Option<i64>,
) -> Result<Vec<Desync>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT d.match_id, d.game, max(d.received_at) AS "at!", min(d.release) AS "release!",
                  (array_agg(d.frame ORDER BY d.received_at))[1] AS "frame!",
                  max(d.profile_id) FILTER (WHERE d.side = 'home') AS home_profile,
                  max(p.ingamesn) FILTER (WHERE d.side = 'home') AS home_mii,
                  max(d.build) FILTER (WHERE d.side = 'home') AS home_build,
                  max(d.platform) FILTER (WHERE d.side = 'home') AS home_platform,
                  max(d.profile_id) FILTER (WHERE d.side = 'away') AS away_profile,
                  max(p.ingamesn) FILTER (WHERE d.side = 'away') AS away_mii,
                  max(d.build) FILTER (WHERE d.side = 'away') AS away_build,
                  max(d.platform) FILTER (WHERE d.side = 'away') AS away_platform,
                  max(v.kind) AS kind, max(v.frame) AS diverged_frame,
                  max(v.field_index) AS field_index,
                  max(v.home_caller) AS home_caller, max(v.away_caller) AS away_caller,
                  max(v.home_size) AS home_size, max(v.away_size) AS away_size,
                  max(v.home_hash) AS home_hash, max(v.away_hash) AS away_hash,
                  max(v.home_bytes) AS home_bytes, max(v.away_bytes) AS away_bytes
           FROM desync_dossiers d
           LEFT JOIN profiles p USING (profile_id)
           LEFT JOIN desync_divergences v USING (match_id, game)
           WHERE $2::bigint IS NULL OR d.match_id = $2
           GROUP BY d.match_id, d.game
           ORDER BY max(d.received_at) DESC
           LIMIT $1"#,
        SHOWN,
        match_id,
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let release = row.release;
            let side = |profile_id: Option<i32>, mii, build: Option<String>, platform| {
                let platform: String = Option::unwrap_or_default(platform);
                Some(DossierSide {
                    profile_id: profile_id?,
                    mii,
                    build: build.unwrap_or_default(),
                    symbolize: dossiers::symbolize(
                        &release,
                        Some(&platform)
                            .filter(|p| !p.is_empty())
                            .map(String::as_str),
                    ),
                    platform,
                })
            };
            let home = side(
                row.home_profile,
                row.home_mii,
                row.home_build,
                row.home_platform,
            );
            let away = side(
                row.away_profile,
                row.away_mii,
                row.away_build,
                row.away_platform,
            );
            let same_binary = match (&home, &away) {
                (Some(home), Some(away)) => {
                    !home.platform.is_empty()
                        && (&home.build, &home.platform) == (&away.build, &away.platform)
                }
                _ => false,
            };
            Desync {
                at: row.at,
                match_id: row.match_id,
                game: row.game,
                frame: row.frame,
                home,
                away,
                same_binary,
                kind: row.kind,
                diverged_frame: row.diverged_frame,
                field_index: row.field_index,
                home_field: field(
                    row.home_caller,
                    row.home_size,
                    row.home_hash,
                    row.home_bytes,
                )
                .map(|field| field.describe()),
                away_field: field(
                    row.away_caller,
                    row.away_size,
                    row.away_hash,
                    row.away_bytes,
                )
                .map(|field| field.describe()),
                release,
            }
        })
        .collect())
}

/// A field of a divergence, from its columns.
fn field(
    caller: Option<i64>,
    size: Option<i64>,
    hash: Option<i64>,
    bytes: Option<Vec<u8>>,
) -> Option<dossiers::Field> {
    Some(dossiers::Field {
        caller: u32::try_from(caller?).ok()?,
        size: u32::try_from(size?).ok()?,
        hash: u32::try_from(hash?).ok()?,
        bytes: bytes?.try_into().ok()?,
    })
}

/// Where a caller is, without a value: the description of a field of
/// zeros, up to its kind.
fn spot(caller: Option<i64>) -> Option<String> {
    let caller = u32::try_from(caller?).ok()?;
    let described = dossiers::Field {
        caller,
        size: 0,
        hash: 0,
        bytes: [0; dossiers::FIELD_BYTES],
    }
    .describe();
    // `0x… pose team 1 player 2 node 3: …`: up to the colon.
    let place = described.split(':').next().unwrap_or(&described);
    Some(place.to_owned())
}

/// Games that found a field: the ones that say where the code differs. A
/// caller is an offset in its side's binary, so they group by both binaries.
async fn divergences(
    conn: &mut PgConnection,
    since: OffsetDateTime,
) -> Result<Vec<Divergence>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT home.release, v.kind,
                  home.build AS home_build, home.platform AS home_platform, v.home_caller,
                  away.build AS away_build, away.platform AS away_platform, v.away_caller,
                  count(*) AS "games!", max(greatest(home.received_at, away.received_at)) AS "last_at!"
           FROM desync_divergences v
           JOIN desync_dossiers home
             ON home.match_id = v.match_id AND home.game = v.game AND home.side = 'home'
           JOIN desync_dossiers away
             ON away.match_id = v.match_id AND away.game = v.game AND away.side = 'away'
           WHERE v.kind IN ('path', 'value', 'count') AND home.received_at >= $1
           GROUP BY 1, 2, 3, 4, 5, 6, 7, 8
           ORDER BY 9 DESC, 10 DESC
           LIMIT $2"#,
        since,
        SHOWN,
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Divergence {
            release: row.release,
            kind: row.kind,
            same_binary: !row.home_platform.is_empty()
                && (&row.home_build, &row.home_platform) == (&row.away_build, &row.away_platform),
            home: Spot {
                build: row.home_build,
                platform: row.home_platform,
                caller: row.home_caller,
                field: spot(row.home_caller),
            },
            away: Spot {
                build: row.away_build,
                platform: row.away_platform,
                caller: row.away_caller,
                field: spot(row.away_caller),
            },
            games: row.games,
            last_at: row.last_at,
        })
        .collect())
}

/// A side's dossier as it came, to read with the tools of its build: its
/// file name and bytes, None for none.
pub async fn dossier(
    pool: &PgPool,
    match_id: i64,
    game: i32,
    side: &str,
) -> Result<Option<(String, Vec<u8>)>, sqlx::Error> {
    let body = sqlx::query_scalar!(
        "SELECT body FROM desync_dossiers WHERE match_id = $1 AND game = $2 AND side = $3",
        match_id,
        game,
        side,
    )
    .fetch_optional(pool)
    .await?;
    Ok(body.map(|body| (format!("desync-{match_id}-{game}-{side}.rcds"), body)))
}

/// A game's dossiers as text (`dossiers::describe`), to save; None for none.
pub async fn dossier_text(
    pool: &PgPool,
    match_id: i64,
    game: i32,
) -> Result<Option<(String, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT d.side, d.body, p.ingamesn AS \"mii?\" FROM desync_dossiers d
         LEFT JOIN profiles p USING (profile_id)
         WHERE d.match_id = $1 AND d.game = $2",
        match_id,
        game,
    )
    .fetch_all(pool)
    .await?;
    let mut sides = Teams {
        home: None,
        away: None,
    };
    for row in rows {
        // Read when it came; only a later version of the format could fail.
        let (Some(side), Ok(dossier)) =
            (Side::parse(&row.side), dossiers::Dossier::parse(&row.body))
        else {
            continue;
        };
        let sided = Some(dossiers::Sided {
            dossier,
            mii: row.mii,
        });
        match side {
            Side::Home => sides.home = sided,
            Side::Away => sides.away = sided,
        }
    }
    if sides.home.is_none() && sides.away.is_none() {
        return Ok(None);
    }
    Ok(Some((
        format!("desync-{match_id}-{game}.txt"),
        dossiers::describe(match_id, game, &sides),
    )))
}

async fn timeline(conn: &mut PgConnection, online_now: Online) -> Result<Timeline, sqlx::Error> {
    let from = OffsetDateTime::now_utc() - Duration::hours(TIMELINE_HOURS);
    let matches = sqlx::query_as!(
        Span,
        r#"SELECT started_at,
                  CASE WHEN state = 'abandoned' THEN last_report_at ELSE ended_at END AS ended_at
           FROM online_matches
           WHERE state = 'in_progress' OR ended_at >= $1
           ORDER BY started_at DESC
           LIMIT $2"#,
        from,
        TIMELINE_MATCHES,
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .filter(|span| span.ended_at.is_none_or(|ended_at| ended_at >= from))
    .collect();
    // The first column starts before `from`: it counts all of its time.
    let problems = sqlx::query_as!(
        Problem,
        r#"WITH bounds AS (
               SELECT make_interval(mins => $2) AS width,
                      date_bin(make_interval(mins => $2), $1::timestamptz,
                               TIMESTAMPTZ '1970-01-01 00:00:00+00') AS first),
           games AS (
               SELECT min(reported_at) AS at,
                      CASE WHEN bool_or(ending = 'desync') THEN 'desync'
                           WHEN bool_or(ending = 'disconnect') THEN 'disconnect' END AS kind
               FROM online_match_games, bounds WHERE reported_at >= bounds.first
               GROUP BY match_id, number),
           events AS (
               SELECT at, kind, NULL::integer AS code FROM games WHERE kind IS NOT NULL
               UNION ALL
               SELECT left_at, 'left', NULL FROM online_match_sides, bounds
               WHERE left_at >= bounds.first
               UNION ALL
               SELECT received_at, 'error', code FROM client_errors, bounds
               WHERE received_at >= bounds.first)
           SELECT date_bin(bounds.width, at, TIMESTAMPTZ '1970-01-01 00:00:00+00') AS "at!",
                  kind AS "kind!", code, count(*) AS "count!"
           FROM events, bounds
           GROUP BY 1, 2, 3
           ORDER BY 1, 2, 3"#,
        from,
        COLUMN_MINUTES,
    )
    .fetch_all(&mut *conn)
    .await?;
    // Each minute the servers' players in WFC and in matches add up; with
    // Recharged open, every server counted all.
    let online = sqlx::query_as!(
        Online,
        r#"WITH minutes AS (
               SELECT at, sum(wfc)::integer AS wfc, sum(playing)::integer AS playing,
                      max(recharged) AS recharged
               FROM online_samples WHERE at >= $1 GROUP BY at)
           SELECT date_bin(make_interval(mins => $2), at, TIMESTAMPTZ '1970-01-01 00:00:00+00') AS "at!",
                  max(wfc) AS "wfc!", max(playing) AS "playing!", max(recharged) AS "recharged!"
           FROM minutes
           GROUP BY 1 ORDER BY 1"#,
        from,
        ONLINE_MINUTES,
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(Timeline {
        from,
        column_minutes: COLUMN_MINUTES,
        matches,
        problems,
        online_minutes: ONLINE_MINUTES,
        online,
        online_now,
    })
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
    use crate::nas::Tickets;
    use std::sync::Arc;

    fn gpcm(pool: &PgPool) -> Gpcm {
        Gpcm::new(pool.clone(), Arc::new(Tickets::default()), Arc::default())
    }

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

        let monitoring = read(&pool, &gpcm(&pool)).await.unwrap();
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

    #[sqlx::test]
    async fn the_timeline_has_the_matches_and_what_went_wrong(pool: PgPool) {
        let games = Games::new(pool.clone()).await;
        games
            .send(&games.report(games.anosch, 1, "home", "start"))
            .await;
        games
            .send(&games.report(games.sam, 2, "away", "start"))
            .await;
        let mut lost = Games::game(games.report(games.anosch, 1, "home", "game"), 1, 0, 0);
        lost["game"]["ending"] = json!("disconnect");
        games.send(&lost).await;
        sqlx::query(
            "INSERT INTO client_errors (code, release, address)
             VALUES (86420, 'R4QE', '10.0.0.1'), (86420, 'R4QE', '10.0.0.2'),
                    (86070, 'R4QE', '10.0.0.1')",
        )
        .execute(&pool)
        .await
        .unwrap();
        // Abandoned matches end at their last report: one 47 hours ago is on
        // the timeline, one whose play stopped before it is not, and neither
        // is one that finished three days ago.
        for (last_report, state) in [
            ("47 hours", "abandoned"),
            ("48 hours 10 minutes", "abandoned"),
            ("3 days", "finished"),
        ] {
            sqlx::query(
                "INSERT INTO online_matches (release, mode, best_of, state, home_mii, away_mii,
                                             home_captain, away_captain, started_at,
                                             last_report_at, ended_at)
                 VALUES ('R4QE', 'ranked', 3, $2, 'A', 'B', 'mario', 'luigi',
                         now() - $1::interval - interval '20 minutes', now() - $1::interval,
                         now() - $1::interval
                             + CASE WHEN $2 = 'abandoned' THEN interval '30 minutes'
                                    ELSE interval '0' END)",
            )
            .bind(last_report)
            .bind(state)
            .execute(&pool)
            .await
            .unwrap();
        }

        let timeline = read(&pool, &gpcm(&pool)).await.unwrap().timeline;
        let mut ended: Vec<_> = timeline
            .matches
            .iter()
            .map(|span| span.ended_at.map(|ended_at| (span.started_at, ended_at)))
            .collect();
        ended.sort();
        assert_eq!(ended.len(), 2, "{ended:?}");
        assert_eq!(ended[0], None, "the match in progress");
        let (started, stopped) = ended[1].unwrap();
        assert_eq!(stopped - started, Duration::minutes(20));
        assert!(stopped > timeline.from);

        let problems: Vec<_> = timeline
            .problems
            .iter()
            .map(|p| (p.kind.as_str(), p.code, p.count))
            .collect();
        assert_eq!(problems.len(), 3, "three rows: {problems:?}");
        for row in [
            ("disconnect", None, 1),
            ("error", Some(86070), 1),
            ("error", Some(86420), 2),
        ] {
            assert!(problems.contains(&row), "{row:?} in {problems:?}");
        }
        let column = Duration::minutes(i64::from(timeline.column_minutes));
        for problem in &timeline.problems {
            assert_eq!(problem.at.unix_timestamp() % column.whole_seconds(), 0);
            assert!(problem.at > timeline.from - column);
        }
    }

    #[sqlx::test]
    async fn the_timeline_has_the_players_online(pool: PgPool) {
        // Nobody sampled yet: no points, and nobody online now.
        let timeline = read(&pool, &gpcm(&pool)).await.unwrap().timeline;
        assert!(timeline.online.is_empty());
        assert_eq!(
            (timeline.online_now.wfc, timeline.online_now.recharged),
            (0, 0)
        );

        // A player with Recharged open, and samples of two servers: three
        // minutes in one span of ten minutes, one in the next, one older than
        // the timeline.
        sqlx::query("INSERT INTO players (display_name, heartbeat_at) VALUES ('Mario', now())")
            .execute(&pool)
            .await
            .unwrap();
        crate::online::sample(&pool, &gpcm(&pool)).await.unwrap();
        let span = i64::from(ONLINE_MINUTES) * 60;
        let base = OffsetDateTime::now_utc().unix_timestamp() / span * span - 3 * span;
        for (server, offset, wfc, playing, recharged) in [
            ("a", 0, 3, 0, 1),
            ("a", 60, 7, 2, 2),
            ("b", 60, 4, 1, 2),
            ("a", 120, 5, 4, 1),
            ("b", 120, 1, 3, 1),
            ("a", span, 2, 0, 0),
            ("a", -3 * 24 * 3600, 40, 10, 9),
            ("b", -3 * 24 * 3600, 5, 0, 9),
        ] {
            sqlx::query(
                "INSERT INTO online_samples (at, server, wfc, playing, recharged)
                 VALUES (to_timestamp($1), $2, $3, $4, $5)",
            )
            .bind((base + offset) as f64)
            .bind(server)
            .bind(wfc)
            .bind(playing)
            .bind(recharged)
            .execute(&pool)
            .await
            .unwrap();
        }
        let monitoring = read(&pool, &gpcm(&pool)).await.unwrap();
        let timeline = monitoring.timeline;
        assert_eq!(timeline.online_now.recharged, 1);
        let points: Vec<_> = timeline
            .online
            .iter()
            .map(|point| {
                (
                    point.at.unix_timestamp() - base,
                    point.wfc,
                    point.playing,
                    point.recharged,
                )
            })
            .collect();
        // Each span's most of the minutes, where the servers' players in WFC
        // and in matches add up and those with Recharged open don't; the
        // sample of now (the player, nobody in WFC) last.
        assert_eq!(&points[..2], [(0, 11, 7, 2), (span, 2, 0, 0)]);
        assert_eq!(points.last().map(|point| (point.1, point.3)), Some((0, 1)));
        // The day's peak: the most logged in to WFC that day (the day of the
        // samples: just after midnight UTC, yesterday).
        let sampled = OffsetDateTime::from_unix_timestamp(base)
            .unwrap()
            .date()
            .to_string();
        let day = monitoring
            .by_day
            .iter()
            .find(|day| day.day == sampled)
            .unwrap();
        assert!(
            day.peak_online.is_some_and(|peak| peak >= 11),
            "{:?}",
            day.peak_online
        );
        let old = OffsetDateTime::from_unix_timestamp(base - 3 * 24 * 3600)
            .unwrap()
            .date()
            .to_string();
        let day = monitoring.by_day.iter().find(|day| day.day == old).unwrap();
        assert_eq!(day.peak_online, Some(45));
    }

    #[sqlx::test]
    async fn the_desync_dossiers_and_where_they_diverge(pool: PgPool) {
        use crate::reports::{
            dossiers::{
                self,
                tests::{dossier, step},
            },
            report::Side,
        };
        let games = Games::new(pool.clone()).await;
        games
            .send(&games.report(games.anosch, 1, "home", "start"))
            .await;
        games
            .send(&games.report(games.sam, 2, "away", "start"))
            .await;
        let mut home = dossier(Side::Home);
        (home.profile_id, home.series) = (games.anosch, 1);
        let mut away = dossier(Side::Away);
        (away.profile_id, away.series) = (games.sam, 2);
        away.steps[1] = step(4812, &[1, 9, 3]);

        let body = home.encode();
        dossiers::store(&pool, &home, &body).await.unwrap();
        let monitoring = read(&pool, &gpcm(&pool)).await.unwrap();
        let desync = &monitoring.desyncs[0];
        assert!(desync.home.is_some() && desync.away.is_none());
        assert_eq!(desync.kind, None);
        assert!(monitoring.divergences.is_empty());

        dossiers::store(&pool, &away, &away.encode()).await.unwrap();
        let monitoring = read(&pool, &gpcm(&pool)).await.unwrap();
        assert_eq!(monitoring.desyncs.len(), 1);
        let desync = &monitoring.desyncs[0];
        let sam = desync.away.as_ref().unwrap();
        assert_eq!(
            (sam.profile_id, sam.build.as_str()),
            (games.sam, "1.1.6+g0123456789ab")
        );
        assert_eq!(
            (
                desync.kind.as_deref(),
                desync.frame,
                desync.diverged_frame,
                desync.field_index,
                desync.same_binary,
            ),
            (Some("value"), 4812, Some(4812), Some(1), true)
        );
        assert_eq!(sam.platform, "linux x86_64");
        assert!(
            sam.symbolize
                .starts_with("addr2line -f -C -e recharged_game_us.so")
        );
        assert_eq!(
            desync.away_field.as_deref(),
            Some("0x00123404 at +0x123404: 4 bytes 09000000, size 4, hash 09000e2b")
        );
        let spots: Vec<_> = monitoring
            .divergences
            .iter()
            .map(|v| {
                (
                    v.kind.as_str(),
                    v.home.caller,
                    v.home.field.as_deref(),
                    v.same_binary,
                    v.games,
                )
            })
            .collect();
        assert_eq!(
            spots,
            [(
                "value",
                Some(0x0012_3404),
                Some("0x00123404 at +0x123404"),
                true,
                1
            )]
        );

        let (name, saved) = dossier_file(&pool, desync.match_id, 2, "home").await;
        assert_eq!(name, format!("desync-{}-2-home.rcds", desync.match_id));
        assert_eq!(saved, body);
        assert!(
            super::dossier(&pool, desync.match_id, 2, "elsewhere")
                .await
                .unwrap()
                .is_none()
        );
        let (name, text) = dossier_text(&pool, desync.match_id, 2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(name, format!("desync-{}-2.txt", desync.match_id));
        assert!(text.contains("Where they diverge: value"), "{text}");
        assert!(text.contains("== away: Sam, profile"), "{text}");
        assert!(
            dossier_text(&pool, desync.match_id, 3)
                .await
                .unwrap()
                .is_none()
        );
    }

    async fn dossier_file(
        pool: &PgPool,
        match_id: i64,
        game: i32,
        side: &str,
    ) -> (String, Vec<u8>) {
        super::dossier(pool, match_id, game, side)
            .await
            .unwrap()
            .unwrap()
    }
}

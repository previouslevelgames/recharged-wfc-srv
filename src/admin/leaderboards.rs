//! The Leaderboards page: the gamestats2 `scores`, by category and region,
//! who leads each category's current season (or day) and how many play it,
//! and how its last one ended (docs/gamestats2.md, "Seasons"). As on the
//! game's boards, a Recharged player's WFC profiles are one row, and as on the
//! launcher's, a row without a game counts no one (`gamestats::players`).

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use time::{Date, OffsetDateTime};

use super::{page::snapshot, players::Search};
use crate::gamestats::{
    players::{self, Line},
    season,
    submission::Submission,
};

/// Rows shown per category, unless `top` asks for more.
const TOP: i64 = 10;
/// The most rows per category `top` gets.
const MOST_SHOWN: usize = 1000;
/// Rows a search shows at most, over all categories.
const FOUND: usize = 100;

/// The season's board (`NETWORK_PERSISTENT_SEASON`), whose players the key
/// figures count.
const SEASON: i32 = 0;

#[derive(Serialize)]
pub struct Leaderboards {
    totals: Totals,
    categories: Vec<Category>,
    /// Each category's current season (or day) that has players.
    current: Vec<Current>,
    /// The first [`TOP`] rows of each category's current season (or day), in
    /// the game's order.
    top: Vec<Entry>,
    /// With a search: the rows of the current boards whose name, Recharged
    /// player or profiles it names (at most [`FOUND`]), ranked like `top`.
    found: Option<Vec<Entry>>,
    /// Each category's last season that has ended, if it had scores.
    past: Vec<Past>,
}

/// All of `scores`, and the players of the current season.
#[derive(Serialize)]
pub struct Totals {
    entries: i64,
    /// Everyone with a game on the season's board, a player's WFC profiles
    /// as one.
    players: usize,
    #[serde(with = "time::serde::rfc3339::option")]
    last_put: Option<OffsetDateTime>,
}

/// A category's current season (or day).
#[derive(Serialize)]
struct Current {
    category: i32,
    first_day: String,
    last_day: String,
    /// Everyone with a game on it, a player's WFC profiles as one.
    players: usize,
}

/// The `scores` of one category, its rows of every season.
#[derive(Serialize)]
struct Category {
    category: i32,
    players: i64,
    jp: i64,
    us: i64,
    eu: i64,
    kr: i64,
    top_score: i32,
    #[serde(with = "time::serde::rfc3339")]
    last_put: OffsetDateTime,
}

/// How the last season (or day) of a category that has ended came out.
#[derive(Serialize)]
struct Past {
    category: i32,
    first_day: String,
    last_day: String,
    players: usize,
    /// Its first [`TOP`] rows, ranked like `top`.
    top: Vec<Entry>,
}

/// A row of `scores` or `past_scores`.
struct Row {
    category: i32,
    profile_id: i32,
    region: i32,
    score: i32,
    userdata: Vec<u8>,
    updated_at: OffsetDateTime,
}

/// A row near the top of a category, with what its userdata tells.
#[derive(Clone, Serialize)]
struct Entry {
    category: i32,
    /// As the game ranks: equal scores share a rank.
    rank: i64,
    /// The row's profile: the player's best one when it is several.
    profile_id: i32,
    /// All the WFC profiles the row adds up (one, unless a player's).
    profile_ids: Vec<i32>,
    /// The Recharged player whose profiles they are, if a launcher proved them.
    player: Option<String>,
    region: i32,
    score: i32,
    name: Option<String>,
    wins: Option<i32>,
    losses: Option<i32>,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

/// How many rows of each category to show: `top`, [`TOP`] by default, at
/// most [`MOST_SHOWN`] (the whole board, for diagnosing).
#[derive(Deserialize, Default)]
pub struct Shown {
    top: Option<usize>,
}

impl Shown {
    #[cfg(test)]
    pub fn new(top: usize) -> Self {
        Self { top: Some(top) }
    }

    fn rows(&self) -> usize {
        self.top.unwrap_or(TOP as usize).clamp(1, MOST_SHOWN)
    }
}

pub async fn read(
    pool: &PgPool,
    search: &Search,
    shown: &Shown,
) -> Result<Leaderboards, sqlx::Error> {
    read_on(pool, search, shown, OffsetDateTime::now_utc().date()).await
}

/// [`read`] on `today` (the UTC date, as [`past`]).
async fn read_on(
    pool: &PgPool,
    search: &Search,
    shown: &Shown,
    today: Date,
) -> Result<Leaderboards, sqlx::Error> {
    let mut tx = snapshot(pool).await?;
    let (top, found, current) = top(&mut tx, search, shown.rows(), today).await?;
    let leaderboards = Leaderboards {
        totals: totals_on(&mut tx, today).await?,
        categories: categories(&mut tx).await?,
        current,
        top,
        found,
        past: past(&mut tx, today).await?,
    };
    tx.commit().await?;
    Ok(leaderboards)
}

pub async fn totals(conn: &mut PgConnection) -> Result<Totals, sqlx::Error> {
    totals_on(conn, OffsetDateTime::now_utc().date()).await
}

async fn totals_on(conn: &mut PgConnection, today: Date) -> Result<Totals, sqlx::Error> {
    let stored = sqlx::query!(
        r#"SELECT count(*) AS "entries!",
                  count(DISTINCT profile_id) AS "players!",
                  max(updated_at) AS last_put
           FROM scores"#
    )
    .fetch_one(&mut *conn)
    .await?;
    let season: Vec<Row> = current(&mut *conn, today)
        .await?
        .into_iter()
        .filter(|row| row.category == SEASON)
        .collect();
    let owners = Owners::read(&mut *conn, &season).await?;
    let (_, players) = leaders(SEASON, season, &owners);
    Ok(Totals {
        entries: stored.entries,
        players,
        last_put: stored.last_put,
    })
}

/// The rows of each category's current season (or day) on `today` (the UTC
/// date, as [`past`]) that have a game on them: a row of an ended one stays
/// in `scores` until its profile's next put, and the game also puts the
/// empty record of a new one ([`players::played`]).
async fn current(conn: &mut PgConnection, today: Date) -> Result<Vec<Row>, sqlx::Error> {
    Ok(sqlx::query_as!(
        Row,
        "SELECT category, profile_id, region, score, userdata, updated_at FROM scores"
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .filter(|row| {
        Submission::parse(&row.userdata)
            .date
            .map(|date| season::first_day(row.category, date))
            == Some(season::first_day(row.category, today))
            && players::played(row.score, &row.userdata)
    })
    .collect())
}

async fn categories(conn: &mut PgConnection) -> Result<Vec<Category>, sqlx::Error> {
    sqlx::query_as!(
        Category,
        r#"SELECT category,
                  count(*) AS "players!",
                  count(*) FILTER (WHERE region = 1) AS "jp!",
                  count(*) FILTER (WHERE region = 2) AS "us!",
                  count(*) FILTER (WHERE region = 4) AS "eu!",
                  count(*) FILTER (WHERE region = 8) AS "kr!",
                  max(score) AS "top_score!",
                  max(updated_at) AS "last_put!"
           FROM scores
           GROUP BY category
           ORDER BY category"#
    )
    .fetch_all(conn)
    .await
}

/// The first `shown` rows of each category's current season (or day) on
/// `today`, with a search the rows it finds there, and how many play each.
async fn top(
    conn: &mut PgConnection,
    search: &Search,
    shown: usize,
    today: Date,
) -> Result<(Vec<Entry>, Option<Vec<Entry>>, Vec<Current>), sqlx::Error> {
    let rows = current(&mut *conn, today).await?;
    let owners = Owners::read(conn, &rows).await?;
    let mut boards: BTreeMap<i32, Vec<Row>> = BTreeMap::new();
    for row in rows {
        boards.entry(row.category).or_default().push(row);
    }
    let text = search.text();
    let numbered = search.profiles(&mut *conn).await?;
    let searching = text.is_some();
    let mut top = Vec::new();
    let mut found = Vec::new();
    let mut current = Vec::new();
    for (category, rows) in boards {
        let (leaders, players, matches) = ranked(category, rows, &owners, shown, |entry| {
            entry.profile_ids.iter().any(|id| numbered.contains(id))
                || text.as_deref().is_some_and(|text| {
                    [&entry.name, &entry.player]
                        .into_iter()
                        .flatten()
                        .any(|name| name.to_lowercase().contains(text))
                })
        });
        top.extend(leaders);
        if searching {
            found.extend(matches);
        }
        let first_day = season::first_day(category, today);
        current.push(Current {
            category,
            first_day: first_day.to_string(),
            last_day: season::last_day(category, first_day).to_string(),
            players,
        });
    }
    found.truncate(FOUND);
    Ok((top, searching.then_some(found), current))
}

/// Who the WFC profiles of some rows belong to, and those players' names.
struct Owners {
    players: HashMap<i32, i64>,
    names: HashMap<i64, String>,
}

impl Owners {
    async fn read(conn: &mut PgConnection, rows: &[Row]) -> Result<Self, sqlx::Error> {
        let profiles: Vec<i32> = rows.iter().map(|row| row.profile_id).collect();
        let players = players::players_of(&mut *conn, &profiles).await?;
        let ids: Vec<i64> = players.values().copied().collect();
        let names = sqlx::query!(
            "SELECT player_id, display_name FROM players WHERE player_id = ANY($1)",
            &ids
        )
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .map(|row| (row.player_id, row.display_name))
        .collect();
        Ok(Self { players, names })
    }
}

/// The first [`TOP`] of a category's `rows` with each player's rows as one,
/// ranked and ordered like the game's descending boards
/// (`gamestats::leaderboard`), and how many rows the board has then.
fn leaders(category: i32, rows: Vec<Row>, owners: &Owners) -> (Vec<Entry>, usize) {
    let (top, count, _) = ranked(category, rows, owners, TOP as usize, |_| false);
    (top, count)
}

/// The first `shown` rows like [`leaders`], and every row of the board that
/// `wanted` takes.
fn ranked(
    category: i32,
    rows: Vec<Row>,
    owners: &Owners,
    shown: usize,
    wanted: impl Fn(&Entry) -> bool,
) -> (Vec<Entry>, usize, Vec<Entry>) {
    let lines = rows
        .into_iter()
        .map(|row| {
            Line::new(
                row.profile_id,
                row.region,
                row.score,
                row.userdata,
                row.updated_at,
            )
        })
        .collect();
    let mut lines = players::by_player(lines, &owners.players, category, |_| 0);
    lines.sort_by(|a, b| (b.score, a.stamp, a.profile_id).cmp(&(a.score, b.stamp, b.profile_id)));
    let count = lines.len();
    let mut top = Vec::new();
    let mut found = Vec::new();
    // Equal scores share a rank: the rows above with a higher score, plus one.
    let (mut rank, mut previous) = (0, None);
    for (index, line) in lines.into_iter().enumerate() {
        if previous != Some(line.score) {
            rank = index as i64 + 1;
            previous = Some(line.score);
        }
        let Submission {
            name, wins, losses, ..
        } = Submission::parse(&line.userdata);
        let entry = Entry {
            category,
            rank,
            profile_id: line.profile_id,
            profile_ids: line.members,
            player: owners
                .players
                .get(&line.profile_id)
                .and_then(|player| owners.names.get(player))
                .cloned(),
            region: line.region,
            score: line.score,
            name,
            wins,
            losses,
            updated_at: line.stamp,
        };
        if wanted(&entry) {
            found.push(entry.clone());
        }
        if index < shown {
            top.push(entry);
        }
    }
    (top, count, found)
}

/// Each category's latest season (or day) before `today` that has scores:
/// the rows `past_scores` kept, and those of the players who have not played
/// since, which are still in `scores`; rows without a game count no one.
/// `today` is the UTC date, the EU clock: US games start a season 8 hours
/// later, JP games 9 hours earlier.
async fn past(conn: &mut PgConnection, today: Date) -> Result<Vec<Past>, sqlx::Error> {
    let ended = |category, first_day| first_day < season::first_day(category, today);
    let unreplaced: Vec<(Date, Row)> = sqlx::query_as!(
        Row,
        "SELECT category, profile_id, region, score, userdata, updated_at FROM scores"
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .filter_map(|row| {
        let first_day = season::first_day(row.category, Submission::parse(&row.userdata).date?);
        (ended(row.category, first_day) && players::played(row.score, &row.userdata))
            .then_some((first_day, row))
    })
    .collect();
    let kept_seasons = sqlx::query!(
        r#"SELECT DISTINCT category AS "category!", first_day AS "first_day!" FROM past_scores"#
    )
    .fetch_all(&mut *conn)
    .await?;

    let mut last: BTreeMap<i32, Date> = BTreeMap::new();
    let seasons = kept_seasons
        .iter()
        .map(|row| (row.category, row.first_day))
        .chain(
            unreplaced
                .iter()
                .map(|(first_day, row)| (row.category, *first_day)),
        );
    for (category, first_day) in seasons.filter(|&(category, first_day)| ended(category, first_day))
    {
        let latest = last.entry(category).or_insert(first_day);
        *latest = (*latest).max(first_day);
    }
    let (categories, first_days): (Vec<i32>, Vec<Date>) =
        last.iter().map(|(&c, &d)| (c, d)).unzip();
    let kept = sqlx::query_as!(
        Row,
        "SELECT category, profile_id, region, score, userdata, updated_at FROM past_scores
         WHERE (category, first_day) IN (SELECT * FROM unnest($1::int4[], $2::date[]))",
        &categories,
        &first_days,
    )
    .fetch_all(&mut *conn)
    .await?;

    let unreplaced = unreplaced
        .into_iter()
        .filter(|(first_day, row)| last.get(&row.category) == Some(first_day))
        .map(|(_, row)| row);
    let rows: Vec<Row> = kept
        .into_iter()
        .filter(|row| players::played(row.score, &row.userdata))
        .chain(unreplaced)
        .collect();
    let owners = Owners::read(conn, &rows).await?;
    let mut boards: BTreeMap<i32, Vec<Row>> = BTreeMap::new();
    for row in rows {
        boards.entry(row.category).or_default().push(row);
    }
    Ok(boards
        .into_iter()
        .map(|(category, rows)| {
            // As `top` and the game rank: equal scores share a rank.
            let (top, players) = leaders(category, rows, &owners);
            let first_day = last[&category];
            Past {
                category,
                first_day: first_day.to_string(),
                last_day: season::last_day(category, first_day).to_string(),
                players,
                top,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The day the tests read the boards on.
    fn today() -> Date {
        Date::from_calendar_date(2026, time::Month::October, 23).unwrap()
    }

    /// A submission's userdata with `wins`, `losses` and a Mii `name`, put
    /// [`today`].
    fn userdata(wins: i32, losses: i32, name: &str) -> Vec<u8> {
        let mut userdata = vec![0; 128];
        userdata[0..4].copy_from_slice(&[23, 10, 0xEA, 0x07]);
        userdata[0x04..0x08].copy_from_slice(&wins.to_le_bytes());
        userdata[0x08..0x0C].copy_from_slice(&losses.to_le_bytes());
        for (unit, at) in name.encode_utf16().zip((0x0E..).step_by(2)) {
            userdata[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        userdata
    }

    /// Two WFC profiles of the games of `user` (European, US) that a
    /// launcher proved for one Recharged player named `name`.
    async fn one_player(pool: &PgPool, user: i64, name: &str) -> [i32; 2] {
        let mut profiles = [0; 2];
        crate::accounts::log_in(pool, user).await.unwrap();
        for (at, game) in ["R4QPabcdefg", "R4QEabcdefg"].into_iter().enumerate() {
            profiles[at] = crate::accounts::profile_login(pool, user, game, Some(name), "10.0.0.1")
                .await
                .unwrap()
                .profile_id;
        }
        let player: i64 = sqlx::query_scalar!(
            "INSERT INTO players (display_name) VALUES ($1) RETURNING player_id",
            name
        )
        .fetch_one(pool)
        .await
        .unwrap();
        for profile in profiles {
            sqlx::query!(
                "INSERT INTO player_wfc_profiles (profile_id, player_id) VALUES ($1, $2)",
                profile,
                player
            )
            .execute(pool)
            .await
            .unwrap();
        }
        profiles
    }

    #[sqlx::test]
    async fn a_players_profiles_lead_as_one(pool: PgPool) {
        let [europe, us] = one_player(&pool, 42, "Birnen").await;
        for (pid, region, score, wins, losses, name) in [
            (europe, 4, 171, 10, 4, "Birnen"),
            (us, 2, 85, 5, 0, "Birnen US"),
            (7, 4, 200, 12, 1, "Raulaso"),
        ] {
            sqlx::query!(
                "INSERT INTO scores (profile_id, category, region, score, userdata)
                 VALUES ($1, 0, $2, $3, $4)",
                pid,
                region,
                score,
                &userdata(wins, losses, name),
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        let boards = read_on(&pool, &Search::default(), &Shown::default(), today())
            .await
            .unwrap();
        assert!(boards.found.is_none());
        let leaders: Vec<_> = boards
            .top
            .iter()
            .map(|entry| {
                (
                    entry.rank,
                    entry.profile_id,
                    entry.profile_ids.clone(),
                    entry.player.as_deref(),
                    entry.name.as_deref(),
                    entry.region,
                    entry.score,
                    entry.wins,
                    entry.losses,
                )
            })
            .collect();
        let mut both = vec![europe, us];
        both.sort_unstable();
        assert_eq!(
            leaders,
            [
                (
                    1,
                    europe,
                    both,
                    Some("Birnen"),
                    Some("Birnen"),
                    4,
                    256,
                    Some(15),
                    Some(4)
                ),
                (
                    2,
                    7,
                    vec![7],
                    None,
                    Some("Raulaso"),
                    4,
                    200,
                    Some(12),
                    Some(1)
                ),
            ]
        );
        // `top` shows as many rows of each category as asked.
        let first = read_on(&pool, &Search::default(), &Shown::new(1), today())
            .await
            .unwrap();
        assert_eq!(first.top.len(), 1);
        assert_eq!(first.top[0].profile_id, europe);

        // A search finds a row by its Mii, its Recharged player or any of
        // its profiles, with its rank.
        let found = |q: &str| {
            let pool = pool.clone();
            let search = Search::new(q);
            async move {
                read_on(&pool, &search, &Shown::default(), today())
                    .await
                    .unwrap()
                    .found
                    .unwrap()
                    .iter()
                    .map(|entry| (entry.rank, entry.profile_id))
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(found("raul").await, [(2, 7)]);
        assert_eq!(found("LASO").await, [(2, 7)]);
        assert_eq!(found("birnen").await, [(1, europe)]);
        assert_eq!(found(&us.to_string()).await, [(1, europe)]);
        assert!(found("kiki").await.is_empty());
    }

    #[sqlx::test]
    async fn reads_the_boards_and_their_leaders(pool: PgPool) {
        for (pid, category, region, score, name) in [
            (1, 0, 4, 10, "Daisy"),
            (2, 0, 2, 30, "Mario"),
            (3, 0, 2, 30, "Luigi"),
            (2, 1, 2, 5, "Mario"),
            // The season's record before a game of it: stored, no player.
            (4, 0, 2, 0, "Wario"),
            // Yesterday's day, not replaced yet: stored, no player today.
            (3, 1, 2, 7, "Luigi"),
        ] {
            let userdata = match (pid, category) {
                (4, _) => userdata(0, 0, name),
                (3, 1) => dated(22, name),
                _ => userdata(score, 1, name),
            };
            sqlx::query!(
                "INSERT INTO scores (profile_id, category, region, score, userdata)
                 VALUES ($1, $2, $3, $4, $5)",
                pid,
                category,
                region,
                score,
                &userdata,
            )
            .execute(&pool)
            .await
            .unwrap();
        }

        let leaderboards = read_on(&pool, &Search::default(), &Shown::default(), today())
            .await
            .unwrap();

        assert_eq!(
            (leaderboards.totals.entries, leaderboards.totals.players),
            (6, 3)
        );
        let categories: Vec<_> = leaderboards
            .categories
            .iter()
            .map(|c| (c.category, c.players, c.us, c.eu, c.top_score))
            .collect();
        assert_eq!(categories, [(0, 4, 3, 1, 30), (1, 2, 2, 0, 7)]);
        let current: Vec<_> = leaderboards
            .current
            .iter()
            .map(|c| {
                (
                    c.category,
                    c.first_day.as_str(),
                    c.last_day.as_str(),
                    c.players,
                )
            })
            .collect();
        assert_eq!(
            current,
            [
                (0, "2026-10-22", "2026-10-28", 3),
                (1, "2026-10-23", "2026-10-23", 1)
            ]
        );

        // Mario and Luigi tie: both rank 1, the earlier put first.
        let top: Vec<_> = leaderboards
            .top
            .iter()
            .map(|e| (e.category, e.rank, e.profile_id, e.name.as_deref(), e.wins))
            .collect();
        assert_eq!(
            top,
            [
                (0, 1, 2, Some("Mario"), Some(30)),
                (0, 1, 3, Some("Luigi"), Some(30)),
                (0, 3, 1, Some("Daisy"), Some(10)),
                (1, 1, 2, Some("Mario"), Some(5)),
            ]
        );
    }

    /// The userdata of a put the game dated `day` October 2026.
    fn dated(day: u8, name: &str) -> Vec<u8> {
        let mut userdata = userdata(0, 0, name);
        userdata[0..4].copy_from_slice(&[day, 10, 0xEA, 0x07]);
        userdata
    }

    #[sqlx::test]
    async fn the_last_season_has_the_kept_rows_and_the_ones_not_replaced(pool: PgPool) {
        // Mario played the season of 15 October and started the next one;
        // Luigi has not played since; Daisy played two seasons ago.
        for (pid, category, score, day, name) in [
            (1, 0, 0, 22, "Mario"),
            (2, 0, 30, 20, "Luigi"),
            (3, 0, 50, 9, "Daisy"),
            (2, 1, 4, 22, "Luigi"),
            // Put before a game of that season, none since: no player of it.
            (4, 0, 0, 16, "Wario"),
        ] {
            sqlx::query!(
                "INSERT INTO scores (profile_id, category, region, score, userdata)
                 VALUES ($1, $2, 2, $3, $4)",
                pid,
                category,
                score,
                &dated(day, name),
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        for (first_day, score) in [(15, 22), (8, 99)] {
            sqlx::query!(
                "INSERT INTO past_scores
                     (profile_id, category, first_day, region, score, userdata, updated_at)
                 VALUES (1, 0, $1, 2, $2, $3, now())",
                Date::from_calendar_date(2026, time::Month::October, first_day).unwrap(),
                score,
                &dated(first_day, "Mario"),
            )
            .execute(&pool)
            .await
            .unwrap();
        }

        let today = Date::from_calendar_date(2026, time::Month::October, 23).unwrap();
        let past = past(&mut pool.acquire().await.unwrap(), today)
            .await
            .unwrap();
        let boards: Vec<_> = past
            .iter()
            .map(|p| {
                let top: Vec<_> = p
                    .top
                    .iter()
                    .map(|e| (e.rank, e.name.as_deref().unwrap(), e.score))
                    .collect();
                (
                    p.category,
                    p.first_day.as_str(),
                    p.last_day.as_str(),
                    p.players,
                    top,
                )
            })
            .collect();
        assert_eq!(
            boards,
            [
                (
                    0,
                    "2026-10-15",
                    "2026-10-21",
                    2,
                    vec![(1, "Luigi", 30), (2, "Mario", 22)]
                ),
                (1, "2026-10-22", "2026-10-22", 1, vec![(1, "Luigi", 4)]),
            ]
        );
    }
}

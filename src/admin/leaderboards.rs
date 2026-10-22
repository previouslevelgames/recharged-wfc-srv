//! The Leaderboards page: the gamestats2 `scores`, by category and region,
//! who leads each category, and how its last season (or day) ended
//! (docs/gamestats2.md, "Seasons").

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use time::{Date, OffsetDateTime};

use super::page::snapshot;
use crate::gamestats::{season, submission::Submission};

/// Rows shown per category.
const TOP: i64 = 10;

#[derive(Serialize)]
pub struct Leaderboards {
    totals: Totals,
    categories: Vec<Category>,
    /// The first [`TOP`] rows of each category, in the game's order.
    top: Vec<Entry>,
    /// Each category's last season that has ended, if it had scores.
    past: Vec<Past>,
}

/// All of `scores`.
#[derive(Serialize)]
pub struct Totals {
    entries: i64,
    players: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    last_put: Option<OffsetDateTime>,
}

/// The `scores` of one category.
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
#[derive(Serialize)]
struct Entry {
    category: i32,
    /// As the game ranks: equal scores share a rank.
    rank: i64,
    profile_id: i32,
    region: i32,
    score: i32,
    name: Option<String>,
    wins: Option<i32>,
    losses: Option<i32>,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

pub async fn read(pool: &PgPool) -> Result<Leaderboards, sqlx::Error> {
    let mut tx = snapshot(pool).await?;
    let leaderboards = Leaderboards {
        totals: totals(&mut tx).await?,
        categories: categories(&mut tx).await?,
        top: top(&mut tx).await?,
        past: past(&mut tx, OffsetDateTime::now_utc().date()).await?,
    };
    tx.commit().await?;
    Ok(leaderboards)
}

pub async fn totals(conn: &mut PgConnection) -> Result<Totals, sqlx::Error> {
    sqlx::query_as!(
        Totals,
        r#"SELECT count(*) AS "entries!",
                  count(DISTINCT profile_id) AS "players!",
                  max(updated_at) AS last_put
           FROM scores"#
    )
    .fetch_one(conn)
    .await
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

async fn top(conn: &mut PgConnection) -> Result<Vec<Entry>, sqlx::Error> {
    // Ranked and ordered like the game's descending boards
    // (`gamestats::leaderboard`).
    let rows = sqlx::query!(
        r#"SELECT category AS "category!", rank AS "rank!", profile_id AS "profile_id!",
                  region AS "region!", score AS "score!", userdata AS "userdata!",
                  updated_at AS "updated_at!"
           FROM (SELECT *,
                        RANK() OVER (PARTITION BY category ORDER BY score DESC) AS rank,
                        row_number() OVER (PARTITION BY category
                                           ORDER BY score DESC, updated_at, profile_id) AS position
                 FROM scores) ranked
           WHERE position <= $1
           ORDER BY category, position"#,
        TOP,
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let Submission {
                name, wins, losses, ..
            } = Submission::parse(&row.userdata);
            Entry {
                category: row.category,
                rank: row.rank,
                profile_id: row.profile_id,
                region: row.region,
                score: row.score,
                name,
                wins,
                losses,
                updated_at: row.updated_at,
            }
        })
        .collect())
}

/// Each category's latest season (or day) before `today` that has scores:
/// the rows `past_scores` kept, and those of the players who have not played
/// since, which are still in `scores`. `today` is the UTC date, the EU
/// clock: US games start a season 8 hours later, JP games 9 hours earlier.
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
        ended(row.category, first_day).then_some((first_day, row))
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

    let mut boards: BTreeMap<i32, Vec<Row>> = BTreeMap::new();
    let unreplaced = unreplaced
        .into_iter()
        .filter(|(first_day, row)| last.get(&row.category) == Some(first_day))
        .map(|(_, row)| row);
    for row in kept.into_iter().chain(unreplaced) {
        boards.entry(row.category).or_default().push(row);
    }
    Ok(boards
        .into_iter()
        .map(|(category, mut rows)| {
            // As `top` and the game rank: equal scores share a rank.
            rows.sort_by(|a, b| {
                (b.score, a.updated_at, a.profile_id).cmp(&(a.score, b.updated_at, b.profile_id))
            });
            let first_day = last[&category];
            let top = rows
                .iter()
                .take(TOP as usize)
                .map(|row| {
                    let Submission {
                        name, wins, losses, ..
                    } = Submission::parse(&row.userdata);
                    Entry {
                        category,
                        rank: 1 + rows.iter().filter(|other| other.score > row.score).count()
                            as i64,
                        profile_id: row.profile_id,
                        region: row.region,
                        score: row.score,
                        name,
                        wins,
                        losses,
                        updated_at: row.updated_at,
                    }
                })
                .collect();
            Past {
                category,
                first_day: first_day.to_string(),
                last_day: season::last_day(category, first_day).to_string(),
                players: rows.len(),
                top,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A submission's userdata with `wins`, `losses` and a Mii `name`.
    fn userdata(wins: i32, losses: i32, name: &str) -> Vec<u8> {
        let mut userdata = vec![0; 128];
        userdata[0x04..0x08].copy_from_slice(&wins.to_le_bytes());
        userdata[0x08..0x0C].copy_from_slice(&losses.to_le_bytes());
        for (unit, at) in name.encode_utf16().zip((0x0E..).step_by(2)) {
            userdata[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        userdata
    }

    #[sqlx::test]
    async fn reads_the_boards_and_their_leaders(pool: PgPool) {
        for (pid, category, region, score, name) in [
            (1, 0, 4, 10, "Daisy"),
            (2, 0, 2, 30, "Mario"),
            (3, 0, 2, 30, "Luigi"),
            (2, 1, 2, 5, "Mario"),
        ] {
            sqlx::query!(
                "INSERT INTO scores (profile_id, category, region, score, userdata)
                 VALUES ($1, $2, $3, $4, $5)",
                pid,
                category,
                region,
                score,
                &userdata(score, 1, name),
            )
            .execute(&pool)
            .await
            .unwrap();
        }

        let leaderboards = read(&pool).await.unwrap();

        assert_eq!(
            (leaderboards.totals.entries, leaderboards.totals.players),
            (4, 3)
        );
        let categories: Vec<_> = leaderboards
            .categories
            .iter()
            .map(|c| (c.category, c.players, c.us, c.eu, c.top_score))
            .collect();
        assert_eq!(categories, [(0, 3, 2, 1, 30), (1, 1, 1, 0, 5)]);

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

//! The Leaderboards page: the gamestats2 `scores`, by category and region,
//! and who leads each category.

use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;

use super::page::snapshot;
use crate::gamestats::submission::Submission;

/// Rows shown per category.
const TOP: i64 = 10;

#[derive(Serialize)]
pub struct Leaderboards {
    totals: Totals,
    categories: Vec<Category>,
    /// The first [`TOP`] rows of each category, in the game's order.
    top: Vec<Entry>,
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
            let Submission { name, wins, losses } = Submission::parse(&row.userdata);
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
}

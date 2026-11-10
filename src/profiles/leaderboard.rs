//! The launcher's Leaderboard page (docs/profiles.md, "Leaderboard"): the
//! game's own RANKED leaderboards of the current season and day, as its
//! LEADERBOARD menu shows them (SEASON LEADERS, DAILY LEADERS, MY RANKING),
//! from the gamestats2 `scores` (docs/gamestats2.md).

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sqlx::PgPool;
use time::{Date, OffsetDateTime};

use super::{ProfilesState, database_error};
use crate::gamestats::{
    players::{self, Line},
    season,
    submission::Submission,
};

/// The season's board and the striker of the day's (`NETWORK_PERSISTENT_*`).
const SEASON: i32 = 0;
const DAILY: i32 = 1;
/// The leaders shown of each board.
const TOP: usize = 20;

#[derive(Serialize)]
struct Leaderboard {
    season: Board,
    daily: Board,
}

/// One board of the current season (or day).
#[derive(Serialize)]
struct Board {
    first_day: String,
    last_day: String,
    /// Everyone with a game on it, a player's WFC profiles as one.
    players: usize,
    top: Vec<Entry>,
    /// The signed-in player's row (all the WFC profiles they proved, as one),
    /// if they are on the board.
    you: Option<Entry>,
}

#[derive(Serialize)]
struct Entry {
    /// As the game ranks: equal points share a rank.
    rank: usize,
    /// The Mii's name, from the game's record.
    mii: Option<String>,
    /// The Recharged player whose profile it is, when a launcher proved it.
    player: Option<String>,
    wins: Option<i32>,
    losses: Option<i32>,
    points: i32,
    /// The game's region: 1 JP, 2 US, 4 EU, 8 KR.
    region: i32,
    you: bool,
}

/// GET /api/v1/leaderboard: the leaders of the season and the day, and the
/// player's own place on each.
pub(super) async fn leaderboard(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    match read(&state.pool, player.id, OffsetDateTime::now_utc().date()).await {
        Ok(leaderboard) => Json(leaderboard).into_response(),
        Err(error) => database_error(error),
    }
}

async fn read(pool: &PgPool, player_id: i64, today: Date) -> Result<Leaderboard, sqlx::Error> {
    let mine: Vec<i32> = sqlx::query_scalar!(
        "SELECT profile_id FROM player_wfc_profiles WHERE player_id = $1",
        player_id
    )
    .fetch_all(pool)
    .await?;
    Ok(Leaderboard {
        season: board(pool, SEASON, today, &mine).await?,
        daily: board(pool, DAILY, today, &mine).await?,
    })
}

async fn board(
    pool: &PgPool,
    category: i32,
    today: Date,
    mine: &[i32],
) -> Result<Board, sqlx::Error> {
    let first_day = season::first_day(category, today);
    // A row of an ended season stays in `scores` until its player's next put:
    // the date in its userdata tells. A row without a game counts no one.
    let lines: Vec<Line<OffsetDateTime>> = sqlx::query!(
        "SELECT profile_id, region, score, userdata, updated_at FROM scores WHERE category = $1",
        category
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .filter(|row| {
        Submission::parse(&row.userdata)
            .date
            .map(|date| season::first_day(category, date))
            == Some(first_day)
            && players::played(row.score, &row.userdata)
    })
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
    // A player's WFC profiles as one row, as the game's boards show them.
    let profiles: Vec<i32> = lines.iter().map(|line| line.profile_id).collect();
    let owners = players::players_of(pool, &profiles).await?;
    let mut rows: Vec<_> = players::by_player(lines, &owners, category, |profile_id| {
        u8::from(mine.contains(&profile_id))
    })
    .into_iter()
    .map(|line| (Submission::parse(&line.userdata), line))
    .collect();
    // As the game's boards: points, then the earlier update.
    rows.sort_by(|(_, a), (_, b)| {
        (b.score, a.stamp, a.profile_id).cmp(&(a.score, b.stamp, b.profile_id))
    });
    let rank = |score: i32| 1 + rows.iter().filter(|(_, other)| other.score > score).count();
    let yours =
        |line: &Line<OffsetDateTime>| line.members.iter().any(|member| mine.contains(member));

    let shown: Vec<i32> = rows
        .iter()
        .enumerate()
        .filter(|(n, (_, row))| *n < TOP || yours(row))
        .map(|(_, (_, row))| row.profile_id)
        .collect();
    let players: HashMap<i32, String> = sqlx::query!(
        "SELECT player_wfc_profiles.profile_id, players.display_name
         FROM player_wfc_profiles JOIN players USING (player_id)
         WHERE player_wfc_profiles.profile_id = ANY($1)",
        &shown
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| (row.profile_id, row.display_name))
    .collect();
    let entry = |submission: &Submission, row: &Line<OffsetDateTime>| Entry {
        rank: rank(row.score),
        mii: submission.name.clone(),
        player: players.get(&row.profile_id).cloned(),
        wins: submission.wins,
        losses: submission.losses,
        points: row.score,
        region: row.region,
        you: yours(row),
    };
    let top = rows
        .iter()
        .take(TOP)
        .map(|(submission, row)| entry(submission, row))
        .collect();
    let you = rows
        .iter()
        .find(|(_, row)| yours(row))
        .map(|(submission, row)| entry(submission, row));
    Ok(Board {
        first_day: first_day.to_string(),
        last_day: season::last_day(category, first_day).to_string(),
        players: rows.len(),
        top,
        you,
    })
}

#[cfg(test)]
mod tests {
    use time::Month;

    use super::*;
    use crate::profiles::players::{self, Identity};

    /// A submission's userdata: the game's `date`, the record and a Mii `name`.
    fn userdata(date: Date, wins: i32, losses: i32, name: &str) -> Vec<u8> {
        let mut userdata = vec![0; 128];
        userdata[0] = date.day();
        userdata[1] = u8::from(date.month());
        userdata[2..4].copy_from_slice(&(date.year() as u16).to_le_bytes());
        userdata[0x04..0x08].copy_from_slice(&wins.to_le_bytes());
        userdata[0x08..0x0C].copy_from_slice(&losses.to_le_bytes());
        for (unit, at) in name.encode_utf16().zip((0x0E..).step_by(2)) {
            userdata[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        userdata
    }

    #[sqlx::test]
    async fn a_players_profiles_are_one_row(pool: PgPool) {
        let today = Date::from_calendar_date(2026, Month::October, 31).unwrap();
        crate::accounts::log_in(&pool, 42).await.unwrap();
        let mut profiles = Vec::new();
        for game in ["R4QPabcdefg", "R4QEabcdefg"] {
            profiles.push(
                crate::accounts::profile_login(&pool, 42, game, Some("Birnen"), "10.0.0.1")
                    .await
                    .unwrap()
                    .profile_id,
            );
        }
        let birnen = players::sign_in(
            &pool,
            &Identity {
                provider: "discord",
                external_id: "2",
                username: "birnen",
                display_name: "Birnen",
                avatar_url: None,
            },
        )
        .await
        .unwrap();
        for profile in &profiles {
            players::bind_wfc(&pool, birnen, *profile).await.unwrap();
        }
        for (pid, region, score, wins, name) in [
            (profiles[0], 4, 171, 10, "Birnen"),
            (profiles[1], 2, 85, 5, "Birnen US"),
            (1001, 4, 200, 12, "Raulaso"),
        ] {
            sqlx::query!(
                "INSERT INTO scores (profile_id, category, region, score, userdata)
                 VALUES ($1, $2, $3, $4, $5)",
                pid,
                SEASON,
                region,
                score,
                &userdata(today, wins, 1, name),
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        let season = read(&pool, birnen, today).await.unwrap().season;
        assert_eq!(season.players, 2);
        let rows: Vec<_> = season
            .top
            .iter()
            .map(|entry| {
                (
                    entry.rank,
                    entry.mii.as_deref(),
                    entry.player.as_deref(),
                    entry.points,
                    entry.wins,
                    entry.losses,
                    entry.region,
                    entry.you,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (
                    1,
                    Some("Birnen"),
                    Some("Birnen"),
                    256,
                    Some(15),
                    Some(2),
                    4,
                    true
                ),
                (2, Some("Raulaso"), None, 200, Some(12), Some(1), 4, false),
            ]
        );
        let you = season.you.unwrap();
        assert_eq!((you.rank, you.points), (1, 256));
    }

    #[sqlx::test]
    async fn the_boards_are_this_season_and_day_with_the_players_place(pool: PgPool) {
        let today = Date::from_calendar_date(2026, Month::October, 31).unwrap();
        let yesterday = today.previous_day().unwrap();
        let last_year = Date::from_calendar_date(2025, Month::December, 1).unwrap();
        crate::accounts::log_in(&pool, 42).await.unwrap();
        let profile =
            crate::accounts::profile_login(&pool, 42, "R4QEabcdefg", Some("Nelly"), "10.0.0.1")
                .await
                .unwrap();
        let nelly = players::sign_in(
            &pool,
            &Identity {
                provider: "discord",
                external_id: "1",
                username: "nelly",
                display_name: "Nelly",
                avatar_url: None,
            },
        )
        .await
        .unwrap();
        players::bind_wfc(&pool, nelly, profile.profile_id)
            .await
            .unwrap();

        for (pid, category, score, date, name) in [
            (1001, SEASON, 50, today, "Mario"),
            (profile.profile_id, SEASON, 30, today, "Nelly"),
            (1002, SEASON, 30, today, "Luigi"),
            // An ended season's row, not replaced yet: left out.
            (1003, SEASON, 99, last_year, "Wario"),
            (1001, DAILY, 5, today, "Mario"),
            (1002, DAILY, 7, yesterday, "Luigi"),
            // Today's record before a game of it: no player of the day.
            (1003, DAILY, 0, today, "Wario"),
        ] {
            sqlx::query!(
                "INSERT INTO scores (profile_id, category, region, score, userdata)
                 VALUES ($1, $2, 2, $3, $4)",
                pid,
                category,
                score,
                &userdata(date, score / 10, i32::from(score > 0), name),
            )
            .execute(&pool)
            .await
            .unwrap();
        }

        let boards = read(&pool, nelly, today).await.unwrap();
        let season = &boards.season;
        assert_eq!(season.players, 3);
        let ranks: Vec<_> = season
            .top
            .iter()
            .map(|entry| {
                (
                    entry.rank,
                    entry.mii.as_deref().unwrap(),
                    entry.points,
                    entry.you,
                )
            })
            .collect();
        assert_eq!(
            ranks,
            [
                (1, "Mario", 50, false),
                (2, "Nelly", 30, true),
                (2, "Luigi", 30, false)
            ]
        );
        let you = season.you.as_ref().unwrap();
        assert_eq!(
            (you.rank, you.player.as_deref(), you.wins, you.losses),
            (2, Some("Nelly"), Some(3), Some(1))
        );
        assert_eq!(season.top[0].player, None);

        let daily = &boards.daily;
        assert_eq!((daily.players, daily.first_day.as_str()), (1, "2026-10-31"));
        assert_eq!(daily.top[0].mii.as_deref(), Some("Mario"));
        assert!(daily.you.is_none());
    }
}

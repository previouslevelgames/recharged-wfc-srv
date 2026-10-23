//! What a put stores and what a get returns (`docs/gamestats2.md`,
//! "put2.asp" and "get2.asp").
//!
//! `body` is the ranking body decoded from `data`: a 16-byte header of
//! little-endian words, then the userdata (put) or the parameters (get).
//!
//! Scores live in the `scores` table (`migrations/`), one leaderboard for
//! every region; a player's row of a season that has ended moves to
//! `past_scores` when the game starts the next one. A get is two steps: [`Leaderboard::ranked`] has the database
//! sort and rank a whole category; [`answer`] picks the rows the
//! request wants and encodes them, without I/O.

use std::collections::HashSet;

use sqlx::PgPool;
use tracing::info;

use super::{GamestatsError, GamestatsState, season, submission::Submission};

/// Highest category the game uses
/// (`NETWORK_PERSISTENT_ALTERNATE_STRIKER_OF_DAY`).
const MAX_CATEGORY: u32 = 4;
/// Reply to a put: the client only checks that it is not empty and is not an
/// `error:`.
const PUT_REPLY: [u8; 4] = [1, 0, 0, 0];
/// Most rows in a TOPLIST or NEAR answer (`DWC_RNK_GET_MAX`).
const GET_MAX: usize = 10;
/// Friend slots in a FRIENDS query (`DWC_RNK_FRIENDS_MAX`).
const FRIEND_SLOTS: usize = 64;

/// Stores a score. A put carries the player's whole running total, so it
/// replaces the row of `(pid, category)`.
///
/// Returns the response payload: the client does not read it, but it must be
/// at least 1 byte and must not start with `error:`.
pub async fn put(state: &GamestatsState, pid: i32, body: &[u8]) -> Result<Vec<u8>, GamestatsError> {
    let region = u32::from_le_bytes(
        body.get(0..=3)
            .ok_or(GamestatsError::BadData)?
            .try_into()
            .map_err(|_| GamestatsError::BadData)?,
    );
    // DWCRnkRegion of the player's game: JP, US, EU or KR.
    if !matches!(region, 1 | 2 | 4 | 8) {
        return Err(GamestatsError::BadData);
    }
    let category = u32::from_le_bytes(
        body.get(4..=7)
            .ok_or(GamestatsError::BadData)?
            .try_into()
            .map_err(|_| GamestatsError::BadData)?,
    );
    if category > MAX_CATEGORY {
        return Err(GamestatsError::BadData);
    }
    let score = i32::from_le_bytes(
        body.get(8..=11)
            .ok_or(GamestatsError::BadData)?
            .try_into()
            .map_err(|_| GamestatsError::BadData)?,
    );
    if !matches!(score, 0..=999999) {
        return Err(GamestatsError::BadData);
    }
    let size = u32::from_le_bytes(
        body.get(12..=15)
            .ok_or(GamestatsError::BadData)?
            .try_into()
            .map_err(|_| GamestatsError::BadData)?,
    ) as usize;
    if body.len() != 16 + size || !size.is_multiple_of(4) || size > 768 {
        return Err(GamestatsError::BadData);
    }
    let userdata = &body[16..(16 + size)];

    state
        .leaderboard
        .store(category, pid, region, score, userdata)
        .await?;
    let Submission { wins, losses, .. } = Submission::parse(userdata);
    info!(target: "gamestats", pid, category, score, wins, losses, "stored");
    Ok(PUT_REPLY.to_vec())
}

/// Answers a leaderboard query. Returns the response payload: `mode`, then
/// `count`, `total` and the rows (or `order`, `total` for ORDER).
pub async fn get(state: &GamestatsState, pid: i32, body: &[u8]) -> Result<Vec<u8>, GamestatsError> {
    let query = Query::parse(body)?;
    let board = state
        .leaderboard
        .ranked(query.category, query.sort, query.since)
        .await?;
    Ok(answer(pid, &query, &board))
}

/// The `scores` table. Cloned with the state: the pool inside is shared.
#[derive(Clone)]
pub struct Leaderboard {
    pool: PgPool,
}

/// One row of a ranked board, as `ranked` reads it from `scores`. It keeps
/// the database's types; `answer` turns them into the protocol's words.
#[derive(Debug, PartialEq)]
struct Standing {
    /// `RANK()`: equal scores share a rank ("1, 2, 2, 4").
    rank: i64,
    profile_id: i32,
    region: i32,
    score: i32,
    userdata: Vec<u8>,
    /// Since `updated_at`, by the database's clock.
    minutes: i64,
}

impl Leaderboard {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Replaces the row of `(pid, category)`, stamped with the database's
    /// clock. A row from an earlier season (or day), by the dates in the
    /// userdata, is kept in `past_scores` first.
    async fn store(
        &self,
        category: u32,
        pid: i32,
        region: u32,
        score: i32,
        userdata: &[u8],
    ) -> Result<(), GamestatsError> {
        // `put` has checked category and region: both fit an INTEGER.
        let category = category as i32;
        let first_day = |userdata: &[u8]| {
            let date = Submission::parse(userdata).date?;
            Some(season::first_day(category, date))
        };
        let mut tx = self.pool.begin().await?;
        let previous = sqlx::query!(
            "SELECT region, score, userdata, updated_at FROM scores
             WHERE profile_id = $1 AND category = $2
             FOR UPDATE",
            pid,
            category,
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(previous) = previous
            && let Some(ended) = first_day(&previous.userdata)
            && first_day(userdata) != Some(ended)
        {
            sqlx::query!(
                "INSERT INTO past_scores
                     (profile_id, category, first_day, region, score, userdata, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (category, first_day, profile_id) DO UPDATE
                 SET region = EXCLUDED.region,
                     score = EXCLUDED.score,
                     userdata = EXCLUDED.userdata,
                     updated_at = EXCLUDED.updated_at",
                pid,
                category,
                ended,
                previous.region,
                previous.score,
                previous.userdata,
                previous.updated_at,
            )
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query!(
            "INSERT INTO scores (profile_id, category, region, score, userdata)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (profile_id, category) DO UPDATE
             SET region = EXCLUDED.region,
                 score = EXCLUDED.score,
                 userdata = EXCLUDED.userdata,
                 updated_at = now()",
            pid,
            category,
            region as i32,
            score,
            userdata,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The rows of `category` updated in the last `since` minutes (all of
    /// them for 0), ranked by score in `sort` order. Rows that share a rank
    /// come earlier update first.
    async fn ranked(
        &self,
        category: u32,
        sort: Sort,
        since: u32,
    ) -> Result<Vec<Standing>, GamestatsError> {
        let category = category as i32;
        let since = i32::try_from(since).unwrap_or(i32::MAX);
        // ORDER BY takes no parameter for its direction: one query each.
        let board = match sort {
            Sort::Descending => {
                sqlx::query_as!(
                    Standing,
                    r#"SELECT RANK() OVER (ORDER BY score DESC) AS "rank!",
                              profile_id, region, score, userdata,
                              floor(extract(epoch FROM now() - updated_at) / 60)::bigint AS "minutes!"
                       FROM scores
                       WHERE category = $1
                         AND ($2 = 0 OR updated_at >= now() - make_interval(mins => $2))
                       ORDER BY score DESC, updated_at, profile_id"#,
                    category,
                    since,
                )
                .fetch_all(&self.pool)
                .await?
            }
            Sort::Ascending => {
                sqlx::query_as!(
                    Standing,
                    r#"SELECT RANK() OVER (ORDER BY score ASC) AS "rank!",
                              profile_id, region, score, userdata,
                              floor(extract(epoch FROM now() - updated_at) / 60)::bigint AS "minutes!"
                       FROM scores
                       WHERE category = $1
                         AND ($2 = 0 OR updated_at >= now() - make_interval(mins => $2))
                       ORDER BY score ASC, updated_at, profile_id"#,
                    category,
                    since,
                )
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(board)
    }
}

/// Picks the rows of a ranked `board` that `query` asks for, as seen by
/// `pid`, and encodes the reply.
fn answer(pid: i32, query: &Query, board: &[Standing]) -> Vec<u8> {
    let total = board.len() as u32;

    let rows: Vec<&Standing> = match &query.mode {
        Mode::Order => {
            // The caller's rank, 0 when not on the board. The game never asks.
            let rank = board
                .iter()
                .find(|row| row.profile_id == pid)
                .map_or(0, |row| row.rank as u32);
            return words(&[MODE_ORDER, rank, total]);
        }
        Mode::TopList { limit } => board.iter().take(*limit).collect(),
        Mode::Near { limit } => {
            let limit = (*limit).min(board.len());
            // Centred on the caller; the top of the board when not on it.
            let start = board
                .iter()
                .position(|row| row.profile_id == pid)
                .map_or(0, |at| {
                    at.saturating_sub(limit / 2).min(board.len() - limit)
                });
            board[start..start + limit].iter().collect()
        }
        Mode::Friends { limit, friends } => board
            .iter()
            .filter(|row| row.profile_id == pid || friends.contains(&row.profile_id))
            .take(*limit)
            .collect(),
    };

    let mut reply = words(&[query.mode.code(), rows.len() as u32, total]);
    for row in rows {
        reply.extend(words(&[
            row.rank as u32,
            row.profile_id as u32,
            row.score as u32,
            row.region as u32,
            u32::try_from(row.minutes.max(0)).unwrap_or(u32::MAX),
            row.userdata.len() as u32,
        ]));
        reply.extend(&row.userdata);
    }
    reply
}

const MODE_ORDER: u32 = 0;
const MODE_TOPLIST: u32 = 1;
const MODE_NEAR: u32 = 2;
const MODE_FRIENDS: u32 = 3;

/// A get2 request (`DWCi_RankingSessionGetAsync`). The region in the header
/// is ignored: there is one leaderboard for every region.
#[derive(Debug, PartialEq)]
struct Query {
    category: u32,
    sort: Sort,
    /// Minutes; 0 keeps every row.
    since: u32,
    mode: Mode,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Sort {
    Ascending,
    Descending,
}

#[derive(Debug, PartialEq)]
enum Mode {
    Order,
    TopList { limit: usize },
    Near { limit: usize },
    Friends { limit: usize, friends: HashSet<i32> },
}

impl Mode {
    fn code(&self) -> u32 {
        match self {
            Self::Order => MODE_ORDER,
            Self::TopList { .. } => MODE_TOPLIST,
            Self::Near { .. } => MODE_NEAR,
            Self::Friends { .. } => MODE_FRIENDS,
        }
    }
}

impl Query {
    /// Parses and checks a get2 body, with the client's own limits
    /// (`DWC_RnkGetScoreAsync`).
    fn parse(body: &[u8]) -> Result<Self, GamestatsError> {
        let category = word(body, 1)?;
        let mode = word(body, 2)?;
        let size = word(body, 3)? as usize;
        let params = &body[16..];
        let expected = match mode {
            MODE_ORDER => 8,
            MODE_TOPLIST | MODE_NEAR => 12,
            MODE_FRIENDS => 12 + 4 * FRIEND_SLOTS,
            _ => return Err(GamestatsError::BadData),
        };
        if category > MAX_CATEGORY || size != expected || params.len() != size {
            return Err(GamestatsError::BadData);
        }

        let sort = match word(params, 0)? {
            0 => Sort::Ascending,
            1 => Sort::Descending,
            _ => return Err(GamestatsError::BadData),
        };
        if mode == MODE_ORDER {
            let since = word(params, 1)?;
            return Ok(Self {
                category,
                sort,
                since,
                mode: Mode::Order,
            });
        }

        let limit = word(params, 1)? as usize;
        let since = word(params, 2)?;
        let (mode, limits) = match mode {
            MODE_TOPLIST => (Mode::TopList { limit }, 1..=GET_MAX),
            MODE_NEAR => (Mode::Near { limit }, 2..=GET_MAX),
            _ => {
                let friends = (0..FRIEND_SLOTS)
                    .map(|slot| word(params, 3 + slot).map(|pid| pid as i32))
                    .filter(|pid| !matches!(pid, Ok(0)))
                    .collect::<Result<_, _>>()?;
                (Mode::Friends { limit, friends }, 2..=FRIEND_SLOTS + 1)
            }
        };
        if !limits.contains(&limit) {
            return Err(GamestatsError::BadData);
        }
        Ok(Self {
            category,
            sort,
            since,
            mode,
        })
    }
}

/// The little-endian word at `index` (counted in words).
fn word(bytes: &[u8], index: usize) -> Result<u32, GamestatsError> {
    let at = index * 4;
    bytes
        .get(at..at + 4)
        .and_then(|word| word.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(GamestatsError::BadData)
}

/// Little-endian words, as the reply carries them.
fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESCENDING: u32 = 1;

    fn get_body(category: u32, mode: u32, params: &[u32]) -> Vec<u8> {
        let mut body = words(&[2, category, mode, params.len() as u32 * 4]);
        body.extend(words(params));
        body
    }

    fn put_body(category: u32, score: u32, fill: u8) -> Vec<u8> {
        let mut body = words(&[2, category, score, 128]);
        body.extend([fill; 128]);
        body
    }

    /// A put of `score` whose userdata the game dated `day` October 2026.
    fn dated_put(category: u32, score: u32, day: u8) -> Vec<u8> {
        let mut body = put_body(category, score, 0);
        body[16..20].copy_from_slice(&[day, 10, 0xEA, 0x07]);
        body
    }

    fn friends_params(limit: u32, friends: &[i32]) -> Vec<u32> {
        let mut params = vec![DESCENDING, limit, 0];
        params.extend((0..FRIEND_SLOTS).map(|slot| friends.get(slot).map_or(0, |&pid| pid as u32)));
        params
    }

    /// A board as `ranked` returns it for descending `scores`, where pid `n`
    /// (1-based) has score `scores[n - 1]`.
    fn board(scores: &[i32]) -> Vec<Standing> {
        scores
            .iter()
            .enumerate()
            .map(|(at, &score)| Standing {
                // RANK(): one more than the rows with a better score.
                rank: scores.iter().filter(|&&other| other > score).count() as i64 + 1,
                profile_id: at as i32 + 1,
                region: 2,
                score,
                userdata: vec![at as u8; 128],
                minutes: 0,
            })
            .collect()
    }

    /// `(mode, total, rows as (order, pid, score))` of a list reply; checks the
    /// row layout on the way.
    fn read_list(reply: &[u8]) -> (u32, u32, Vec<(u32, i32, i32)>) {
        let count = word(reply, 1).unwrap() as usize;
        let mut rows = Vec::new();
        let mut at = 12;
        for _ in 0..count {
            let row = &reply[at..];
            let size = word(row, 5).unwrap() as usize;
            assert_eq!(size, 128);
            rows.push((
                word(row, 0).unwrap(),
                word(row, 1).unwrap() as i32,
                word(row, 2).unwrap() as i32,
            ));
            at += 24 + size;
        }
        assert_eq!(at, reply.len());
        (word(reply, 0).unwrap(), word(reply, 2).unwrap(), rows)
    }

    /// `(rank, pid, score)` of a board.
    fn ranks(board: &[Standing]) -> Vec<(i64, i32, i32)> {
        board
            .iter()
            .map(|row| (row.rank, row.profile_id, row.score))
            .collect()
    }

    /// A season row stored `minutes_ago` minutes before the database's now.
    async fn insert(pool: &PgPool, pid: i32, score: i32, minutes_ago: i32) {
        sqlx::query!(
            "INSERT INTO scores (profile_id, category, region, score, userdata, updated_at)
             VALUES ($1, 0, 2, $2, $3, now() - make_interval(mins => $4))",
            pid,
            score,
            &[0u8; 128][..],
            minutes_ago,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[test]
    fn parses_the_captured_query() {
        // docs/gamestats2.md, "Worked example".
        let body = get_body(1, MODE_NEAR, &[DESCENDING, 10, 382]);
        assert_eq!(
            Query::parse(&body).unwrap(),
            Query {
                category: 1,
                sort: Sort::Descending,
                since: 382,
                mode: Mode::Near { limit: 10 },
            }
        );
    }

    #[test]
    fn refuses_queries_the_client_would_not_send() {
        for body in [
            get_body(0, 7, &[DESCENDING, 10, 0]),
            get_body(5, MODE_TOPLIST, &[DESCENDING, 10, 0]),
            get_body(0, MODE_TOPLIST, &[2, 10, 0]),
            get_body(0, MODE_TOPLIST, &[DESCENDING, 0, 0]),
            get_body(0, MODE_TOPLIST, &[DESCENDING, 11, 0]),
            get_body(0, MODE_NEAR, &[DESCENDING, 1, 0]),
            get_body(0, MODE_TOPLIST, &[DESCENDING, 10]),
            get_body(0, MODE_FRIENDS, &[DESCENDING, 10, 0]),
            get_body(0, MODE_TOPLIST, &[DESCENDING, 10, 0])[..20].to_vec(),
        ] {
            assert!(Query::parse(&body).is_err(), "{body:?}");
        }
    }

    #[test]
    fn an_empty_board_answers_an_empty_list() {
        // The payload of codec's `signs_an_empty_near_list`.
        let query = Query::parse(&get_body(1, MODE_NEAR, &[DESCENDING, 10, 382])).unwrap();
        assert_eq!(answer(1, &query, &[]), [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn toplist_takes_the_first_rows() {
        let query = Query::parse(&get_body(0, MODE_TOPLIST, &[DESCENDING, 3, 0])).unwrap();
        let (mode, total, rows) = read_list(&answer(9, &query, &board(&[40, 40, 20, 10])));
        assert_eq!((mode, total), (MODE_TOPLIST, 4));
        assert_eq!(rows, [(1, 1, 40), (1, 2, 40), (3, 3, 20)]);
    }

    #[test]
    fn near_is_a_window_around_the_caller() {
        // Pid n scores 100 - n: pid 1 is first, pid 10 last.
        let scores: Vec<i32> = (1..=10).map(|pid| 100 - pid).collect();
        let board = board(&scores);
        let query = Query::parse(&get_body(0, MODE_NEAR, &[DESCENDING, 4, 0])).unwrap();
        let ranks = |pid| -> Vec<u32> {
            read_list(&answer(pid, &query, &board))
                .2
                .iter()
                .map(|row| row.0)
                .collect()
        };
        assert_eq!(ranks(8), [6, 7, 8, 9]);
        assert_eq!(ranks(1), [1, 2, 3, 4]);
        assert_eq!(ranks(10), [7, 8, 9, 10]);
        // Not on the board: the top.
        assert_eq!(ranks(99), [1, 2, 3, 4]);
    }

    #[test]
    fn friends_lists_the_caller_and_their_friends_with_global_ranks() {
        let query = Query::parse(&get_body(0, MODE_FRIENDS, &friends_params(10, &[4, 2]))).unwrap();
        let (mode, total, rows) = read_list(&answer(3, &query, &board(&[50, 40, 30, 20])));
        assert_eq!((mode, total), (MODE_FRIENDS, 4));
        assert_eq!(rows, [(2, 2, 40), (3, 3, 30), (4, 4, 20)]);
    }

    #[tokio::test]
    async fn refuses_puts_the_game_would_not_send() {
        // All refused before the database: it is never reached.
        let state = GamestatsState::without_database();
        for body in [
            words(&[2, 1, 100]),
            words(&[3, 1, 100, 0]),
            words(&[2, 5, 100, 0]),
            words(&[2, 1, 1_000_000, 0]),
            words(&[2, 1, 100, 8, 0]),
            [words(&[2, 1, 100, 2]), vec![0, 0]].concat(),
        ] {
            assert!(put(&state, 7, &body).await.is_err(), "{body:?}");
        }
    }

    #[sqlx::test]
    async fn ranked_sorts_by_score_then_by_update(pool: PgPool) {
        // Pids 2 and 3 tie; 3 was stored first.
        for (pid, score, minutes_ago) in [(1, 10, 1), (2, 40, 2), (3, 40, 3), (4, 20, 4)] {
            insert(&pool, pid, score, minutes_ago).await;
        }
        let leaderboard = Leaderboard::new(pool);

        let board = leaderboard.ranked(0, Sort::Descending, 0).await.unwrap();
        assert_eq!(
            ranks(&board),
            [(1, 3, 40), (1, 2, 40), (3, 4, 20), (4, 1, 10)]
        );
        assert_eq!(board[0].minutes, 3);

        let board = leaderboard.ranked(0, Sort::Ascending, 0).await.unwrap();
        assert_eq!(
            ranks(&board),
            [(1, 1, 10), (2, 4, 20), (3, 3, 40), (3, 2, 40)]
        );

        assert!(
            leaderboard
                .ranked(1, Sort::Descending, 0)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[sqlx::test]
    async fn since_keeps_recent_rows_only(pool: PgPool) {
        for (pid, score, minutes_ago) in [(1, 10, 30), (2, 20, 381), (3, 30, 383)] {
            insert(&pool, pid, score, minutes_ago).await;
        }
        let board = Leaderboard::new(pool)
            .ranked(0, Sort::Descending, 382)
            .await
            .unwrap();
        assert_eq!(ranks(&board), [(1, 2, 20), (2, 1, 10)]);
    }

    #[sqlx::test]
    async fn a_put_replaces_the_row_and_a_get_returns_it(pool: PgPool) {
        let state = GamestatsState::with_pool(pool);
        assert_eq!(
            put(&state, 7, &put_body(1, 100, 0xaa)).await.unwrap(),
            PUT_REPLY
        );
        assert_eq!(
            put(&state, 7, &put_body(1, 130, 0xbb)).await.unwrap(),
            PUT_REPLY
        );

        let reply = get(&state, 7, &get_body(1, MODE_TOPLIST, &[DESCENDING, 10, 60]))
            .await
            .unwrap();
        let (_, total, rows) = read_list(&reply);
        assert_eq!((total, rows), (1, vec![(1, 7, 130)]));
        assert_eq!(reply[12 + 24..], [0xbb; 128]);
    }

    #[sqlx::test]
    async fn a_new_season_keeps_the_last_row_of_the_old_one(pool: PgPool) {
        let state = GamestatsState::with_pool(pool.clone());
        // Season of 15 October: two puts, the second one counts.
        for (category, score, day) in [(0, 10, 20), (0, 22, 21), (1, 5, 21), (1, 6, 21)] {
            put(&state, 7, &dated_put(category, score, day))
                .await
                .unwrap();
        }
        // 22 October: a new season and a new day; the game puts 0.
        for category in [0, 1] {
            put(&state, 7, &dated_put(category, 0, 22)).await.unwrap();
        }
        // Later the same season: nothing more is kept.
        put(&state, 7, &dated_put(0, 3, 23)).await.unwrap();

        let past = sqlx::query!(
            "SELECT category, first_day, score FROM past_scores
             WHERE profile_id = 7 ORDER BY category"
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let past: Vec<_> = past
            .into_iter()
            .map(|row| (row.category, row.first_day.to_string(), row.score))
            .collect();
        assert_eq!(
            past,
            [
                (0, "2026-10-15".to_owned(), 22),
                (1, "2026-10-21".to_owned(), 6)
            ]
        );
    }

    #[sqlx::test]
    async fn rows_without_a_date_are_only_replaced(pool: PgPool) {
        let state = GamestatsState::with_pool(pool.clone());
        put(&state, 7, &put_body(0, 10, 0xaa)).await.unwrap();
        put(&state, 7, &dated_put(0, 0, 22)).await.unwrap();
        let kept = sqlx::query_scalar!(r#"SELECT count(*) AS "kept!" FROM past_scores"#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kept, 0);
    }
}

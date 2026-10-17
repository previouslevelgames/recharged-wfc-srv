//! What a put stores and what a get returns (`docs/gamestats2.md`,
//! "put2.asp" and "get2.asp").
//!
//! `body` is the ranking body decoded from `data`: a 16-byte header of
//! little-endian words, then the userdata (put) or the parameters (get).
//!
//! Scores live in memory for now, one leaderboard for every region.

use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Duration, SystemTime},
};

use super::{GamestatsError, GamestatsState};

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

    state.leaderboard.store(
        category,
        pid,
        Entry {
            region,
            score,
            userdata: userdata.to_vec(),
            updated: SystemTime::now(),
        },
    );
    Ok(PUT_REPLY.to_vec())
}

/// Answers a leaderboard query. Returns the response payload: `mode`, then
/// `count`, `total` and the rows (or `order`, `total` for ORDER).
pub async fn get(state: &GamestatsState, pid: i32, body: &[u8]) -> Result<Vec<u8>, GamestatsError> {
    let query = Query::parse(body)?;
    Ok(state.leaderboard.answer(pid, &query, SystemTime::now()))
}

/// Scores by category, then by pid: one row per player and category.
#[derive(Default)]
pub struct Leaderboard {
    boards: Mutex<HashMap<u32, HashMap<i32, Entry>>>,
}

#[derive(Clone, Debug)]
struct Entry {
    region: u32,
    score: i32,
    userdata: Vec<u8>,
    updated: SystemTime,
}

/// A row of a sorted board, with its rank.
struct Ranked {
    order: u32,
    pid: i32,
    entry: Entry,
}

impl Leaderboard {
    fn store(&self, category: u32, pid: i32, entry: Entry) {
        self.boards
            .lock()
            .unwrap()
            .entry(category)
            .or_default()
            .insert(pid, entry);
    }

    fn answer(&self, pid: i32, query: &Query, now: SystemTime) -> Vec<u8> {
        let board = self.ranked(query.category, query.sort, query.since, now);
        let total = board.len() as u32;

        let rows: Vec<&Ranked> = match &query.mode {
            Mode::Order => {
                // The caller's rank, 0 when not on the board. The game never asks.
                let order = board
                    .iter()
                    .find(|row| row.pid == pid)
                    .map_or(0, |row| row.order);
                return words(&[MODE_ORDER, order, total]);
            }
            Mode::TopList { limit } => board.iter().take(*limit).collect(),
            Mode::Near { limit } => {
                let limit = (*limit).min(board.len());
                // Centred on the caller; the top of the board when not on it.
                let start = board.iter().position(|row| row.pid == pid).map_or(0, |at| {
                    at.saturating_sub(limit / 2).min(board.len() - limit)
                });
                board[start..start + limit].iter().collect()
            }
            Mode::Friends { limit, friends } => board
                .iter()
                .filter(|row| row.pid == pid || friends.contains(&row.pid))
                .take(*limit)
                .collect(),
        };

        let mut reply = words(&[query.mode.code(), rows.len() as u32, total]);
        for row in rows {
            let minutes = age(row.entry.updated, now).as_secs() / 60;
            reply.extend(words(&[
                row.order,
                row.pid as u32,
                row.entry.score as u32,
                row.entry.region,
                u32::try_from(minutes).unwrap_or(u32::MAX),
                row.entry.userdata.len() as u32,
            ]));
            reply.extend(&row.entry.userdata);
        }
        reply
    }

    /// The rows of `category` updated in the last `since` minutes (all of
    /// them for 0), in `sort` order and ranked: equal scores share a rank
    /// ("1, 2, 2, 4"), and the earlier update comes first.
    fn ranked(&self, category: u32, sort: Sort, since: u32, now: SystemTime) -> Vec<Ranked> {
        let window = Duration::from_secs(u64::from(since) * 60);
        let mut board: Vec<Ranked> = self
            .boards
            .lock()
            .unwrap()
            .get(&category)
            .into_iter()
            .flatten()
            .filter(|(_, entry)| since == 0 || age(entry.updated, now) <= window)
            .map(|(&pid, entry)| Ranked {
                order: 0,
                pid,
                entry: entry.clone(),
            })
            .collect();

        board.sort_by(|a, b| {
            let by_score = a.entry.score.cmp(&b.entry.score);
            match sort {
                Sort::Ascending => by_score,
                Sort::Descending => by_score.reverse(),
            }
            .then(a.entry.updated.cmp(&b.entry.updated))
            .then(a.pid.cmp(&b.pid))
        });
        for at in 0..board.len() {
            board[at].order = if at > 0 && board[at].entry.score == board[at - 1].entry.score {
                board[at - 1].order
            } else {
                at as u32 + 1
            };
        }
        board
    }
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

/// How long ago `updated` was; zero if the clock went back since.
fn age(updated: SystemTime, now: SystemTime) -> Duration {
    now.duration_since(updated).unwrap_or_default()
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

    fn friends_params(limit: u32, friends: &[i32]) -> Vec<u32> {
        let mut params = vec![DESCENDING, limit, 0];
        params.extend((0..FRIEND_SLOTS).map(|slot| friends.get(slot).map_or(0, |&pid| pid as u32)));
        params
    }

    /// A board of category 0 where pid `n` (1-based) has score `scores[n - 1]`,
    /// updated `minutes_ago[n - 1]` minutes before `now`.
    fn board(scores: &[i32], minutes_ago: &[u64], now: SystemTime) -> Leaderboard {
        let leaderboard = Leaderboard::default();
        for (at, (&score, &minutes)) in scores.iter().zip(minutes_ago).enumerate() {
            leaderboard.store(
                0,
                at as i32 + 1,
                Entry {
                    region: 2,
                    score,
                    userdata: vec![at as u8; 128],
                    updated: now - Duration::from_secs(minutes * 60),
                },
            );
        }
        leaderboard
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
        let reply = Leaderboard::default().answer(1, &query, SystemTime::now());
        assert_eq!(reply, [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn toplist_ranks_best_first_and_ties_share_a_rank() {
        let now = SystemTime::now();
        // Pids 2 and 3 tie; 3 was updated first.
        let leaderboard = board(&[10, 40, 40, 20], &[1, 2, 3, 4], now);
        let query = Query::parse(&get_body(0, MODE_TOPLIST, &[DESCENDING, 3, 0])).unwrap();
        let (mode, total, rows) = read_list(&leaderboard.answer(1, &query, now));
        assert_eq!((mode, total), (MODE_TOPLIST, 4));
        assert_eq!(rows, [(1, 3, 40), (1, 2, 40), (3, 4, 20)]);
    }

    #[test]
    fn since_keeps_recent_rows_only() {
        let now = SystemTime::now();
        let leaderboard = board(&[10, 20, 30], &[30, 382, 383], now);
        let query = Query::parse(&get_body(0, MODE_TOPLIST, &[DESCENDING, 10, 382])).unwrap();
        let (_, total, rows) = read_list(&leaderboard.answer(1, &query, now));
        assert_eq!(total, 2);
        assert_eq!(rows, [(1, 2, 20), (2, 1, 10)]);
    }

    #[test]
    fn near_is_a_window_around_the_caller() {
        let now = SystemTime::now();
        // Pid n scores 100 - n: pid 1 is first, pid 10 last.
        let scores: Vec<i32> = (1..=10).map(|pid| 100 - pid).collect();
        let leaderboard = board(&scores, &[0; 10], now);
        let query = Query::parse(&get_body(0, MODE_NEAR, &[DESCENDING, 4, 0])).unwrap();
        let ranks = |pid| -> Vec<u32> {
            read_list(&leaderboard.answer(pid, &query, now))
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
        let now = SystemTime::now();
        let leaderboard = board(&[50, 40, 30, 20], &[0; 4], now);
        let query = Query::parse(&get_body(0, MODE_FRIENDS, &friends_params(10, &[4, 2]))).unwrap();
        let (mode, total, rows) = read_list(&leaderboard.answer(3, &query, now));
        assert_eq!((mode, total), (MODE_FRIENDS, 4));
        assert_eq!(rows, [(2, 2, 40), (3, 3, 30), (4, 4, 20)]);
    }

    #[tokio::test]
    async fn a_put_replaces_the_row_and_a_get_returns_it() {
        let state = GamestatsState::default();
        let put_body = |score: u32, fill: u8| {
            let mut body = words(&[2, 1, score, 128]);
            body.extend([fill; 128]);
            body
        };
        assert_eq!(
            put(&state, 7, &put_body(100, 0xaa)).await.unwrap(),
            PUT_REPLY
        );
        assert_eq!(
            put(&state, 7, &put_body(130, 0xbb)).await.unwrap(),
            PUT_REPLY
        );

        let reply = get(&state, 7, &get_body(1, MODE_TOPLIST, &[DESCENDING, 10, 60]))
            .await
            .unwrap();
        let (_, total, rows) = read_list(&reply);
        assert_eq!((total, rows), (1, vec![(1, 7, 130)]));
        assert_eq!(reply[12 + 24..], [0xbb; 128]);
    }

    #[tokio::test]
    async fn refuses_puts_the_game_would_not_send() {
        let state = GamestatsState::default();
        for body in [
            words(&[2, 1, 100]),
            words(&[2, 5, 100, 0]),
            words(&[2, 1, 1_000_000, 0]),
            words(&[2, 1, 100, 8, 0]),
            [words(&[2, 1, 100, 2]), vec![0, 0]].concat(),
        ] {
            assert!(put(&state, 7, &body).await.is_err(), "{body:?}");
        }
    }
}

//! What a put stores and what a get returns (`docs/gamestats2.md`,
//! "put2.asp" and "get2.asp").
//!
//! `body` is the ranking body decoded from `data`: a 16-byte header of
//! little-endian words, then the userdata (put) or the parameters (get).
//!
//! Scores live in the `scores` table (`migrations/`), one leaderboard for
//! every region; a player's row of a season that has ended moves to
//! `past_scores` when the game starts the next one. A get is three steps: [`Leaderboard::ranked`] has the database
//! sort and rank a whole category; [`by_player`] makes the rows of one
//! Recharged player one row (`players`); [`answer`] picks the rows the
//! request wants and encodes them, without I/O.

use std::collections::{HashMap, HashSet};

use sqlx::PgPool;
use tracing::info;

use super::{GamestatsError, GamestatsState, players, season, submission::Submission};

/// Highest category the game uses
/// (`NETWORK_PERSISTENT_ALTERNATE_STRIKER_OF_DAY`).
const MAX_CATEGORY: u32 = 4;
/// Reply to a put: the client only checks that it is not empty and is not an
/// `error:`.
const PUT_REPLY: [u8; 4] = [1, 0, 0, 0];
/// A put this soon after its player's set ended is still the set's: the
/// result's put follows the end of its match.
const SET_ENDED_SECS: i32 = 120;
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

    // An admin deleted this profile's scores while its game was logged in:
    // the game still holds its old totals. Its next login finds no row of its
    // own and starts again from 0.
    if state.gpcm.off_boards(pid) {
        info!(target: "gamestats", pid, category, score, "scores deleted: kept off the boards until the next login");
        return Ok(PUT_REPLY.to_vec());
    }
    // A Recharged set's games meet in RANKED only to find each other: their
    // results go to the set, not the boards (a FRIENDS battle uploads
    // nothing). The game hears its put went through: a refused put would
    // stop its later ones until it logs in again.
    if in_a_set(state, pid).await? {
        info!(target: "gamestats", pid, category, score, "a set's game: kept off the boards");
        return Ok(PUT_REPLY.to_vec());
    }

    state
        .leaderboard
        .store(category, pid, region, score, userdata)
        .await?;
    let Submission { wins, losses, .. } = Submission::parse(userdata);
    info!(target: "gamestats", pid, category, score, wins, losses, "stored");
    Ok(PUT_REPLY.to_vec())
}

/// Whether `pid`'s game plays a Recharged set: kept for one in RANKED, or its
/// player's set is playing or ended less than [`SET_ENDED_SECS`] ago (the
/// result's put comes right after the match).
async fn in_a_set(state: &GamestatsState, pid: i32) -> Result<bool, sqlx::Error> {
    if state.pairings.kept(pid) {
        return Ok(true);
    }
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM player_wfc_profiles p
               JOIN matches m ON p.player_id IN (m.home, m.away)
               WHERE p.profile_id = $1 AND m.playing_at IS NOT NULL
                 AND (m.state = 'accepted'
                      OR m.updated_at > now() - make_interval(secs => $2))
           ) AS "in_a_set!""#,
        pid,
        f64::from(SET_ENDED_SECS),
    )
    .fetch_one(&state.leaderboard.pool)
    .await
}

/// Answers a leaderboard query. Returns the response payload: `mode`, then
/// `count`, `total` and the rows (or `order`, `total` for ORDER).
pub async fn get(state: &GamestatsState, pid: i32, body: &[u8]) -> Result<Vec<u8>, GamestatsError> {
    let query = Query::parse(body)?;
    let board = state
        .leaderboard
        .ranked(query.category, query.sort, query.since)
        .await?;
    let profiles: Vec<i32> = board.iter().map(|row| row.profile_id).collect();
    let players = players::players_of(&state.leaderboard.pool, &profiles).await?;
    // The asking Mii stands for its player, and a friend's Mii for theirs.
    let friends = match &query.mode {
        Mode::Friends { friends, .. } => Some(friends),
        _ => None,
    };
    let preference = |profile_id: i32| {
        if profile_id == pid {
            2
        } else {
            u8::from(friends.is_some_and(|friends| friends.contains(&profile_id)))
        }
    };
    let board = by_player(
        board,
        &players,
        query.category as i32,
        preference,
        query.sort,
    );
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
    /// The WFC profiles the row stands for: its own, and once merged
    /// ([`by_player`]) its player's others.
    members: Vec<i32>,
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
                              floor(extract(epoch FROM now() - updated_at) / 60)::bigint AS "minutes!",
                              ARRAY[profile_id] AS "members!"
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
                              floor(extract(epoch FROM now() - updated_at) / 60)::bigint AS "minutes!",
                              ARRAY[profile_id] AS "members!"
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

/// `board` (as `ranked` gives it) with each player's rows as one
/// ([`players::by_player`]), `preference` choosing the row that stands for a
/// player, sorted and ranked again as `ranked` sorts and ranks.
fn by_player(
    board: Vec<Standing>,
    players: &HashMap<i32, i64>,
    category: i32,
    preference: impl Fn(i32) -> u8,
    sort: Sort,
) -> Vec<Standing> {
    if players.is_empty() {
        return board;
    }
    let lines = board
        .into_iter()
        .map(|row| players::Line {
            profile_id: row.profile_id,
            region: row.region,
            score: row.score,
            userdata: row.userdata,
            // The earlier update is the smaller stamp.
            stamp: -row.minutes,
            members: row.members,
        })
        .collect();
    let mut lines = players::by_player(lines, players, category, preference);
    let better = |a: i32, b: i32| match sort {
        Sort::Descending => b.cmp(&a),
        Sort::Ascending => a.cmp(&b),
    };
    lines.sort_by(|a, b| {
        better(a.score, b.score)
            .then(a.stamp.cmp(&b.stamp))
            .then(a.profile_id.cmp(&b.profile_id))
    });
    // RANK(): equal scores share the rank of the first of them.
    let mut board: Vec<Standing> = Vec::with_capacity(lines.len());
    for (at, line) in lines.into_iter().enumerate() {
        let rank = match board.last() {
            Some(last) if last.score == line.score => last.rank,
            _ => at as i64 + 1,
        };
        board.push(Standing {
            rank,
            profile_id: line.profile_id,
            region: line.region,
            score: line.score,
            userdata: line.userdata,
            minutes: -line.stamp,
            members: line.members,
        });
    }
    board
}

/// Picks the rows of a ranked `board` that `query` asks for, as seen by
/// `pid`, and encodes the reply.
fn answer(pid: i32, query: &Query, board: &[Standing]) -> Vec<u8> {
    let total = board.len() as u32;
    // The caller's row, which may stand for its player's other Miis too.
    let mine = |row: &Standing| row.members.contains(&pid);

    let rows: Vec<&Standing> = match &query.mode {
        Mode::Order => {
            // The caller's rank, 0 when not on the board. The game never asks.
            let rank = board
                .iter()
                .find(|row| mine(row))
                .map_or(0, |row| row.rank as u32);
            return words(&[MODE_ORDER, rank, total]);
        }
        Mode::TopList { limit } => board.iter().take(*limit).collect(),
        Mode::Near { limit } => {
            let limit = (*limit).min(board.len());
            // Centred on the caller; the top of the board when not on it.
            let start = board.iter().position(&mine).map_or(0, |at| {
                at.saturating_sub(limit / 2).min(board.len() - limit)
            });
            board[start..start + limit].iter().collect()
        }
        Mode::Friends { limit, friends } => board
            .iter()
            .filter(|row| mine(row) || row.members.iter().any(|member| friends.contains(member)))
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
                // Without a date (day 0): one season for all.
                userdata: [vec![0; 4], vec![at as u8; 124]].concat(),
                minutes: 0,
                members: vec![at as i32 + 1],
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

    #[test]
    fn a_players_rows_are_one_ranked_again() {
        // Pids 2 and 4 are one player's: 40 + 20 = 60 leads.
        let players = HashMap::from([(2, 1), (4, 1)]);
        let merged = |preferred: i32| {
            by_player(
                board(&[50, 40, 30, 20]),
                &players,
                0,
                |pid| u8::from(pid == preferred),
                Sort::Descending,
            )
        };
        let asked_by_4 = merged(4);
        assert_eq!(ranks(&asked_by_4), [(1, 4, 60), (2, 1, 50), (3, 3, 30)]);
        assert_eq!(asked_by_4[0].members, [2, 4]);
        // Pid 4's row is its player's: TOPLIST and NEAR show it as pid 4,
        // asked by pid 2 as pid 2.
        let toplist = Query::parse(&get_body(0, MODE_TOPLIST, &[DESCENDING, 10, 0])).unwrap();
        let (_, total, rows) = read_list(&answer(4, &toplist, &asked_by_4));
        assert_eq!((total, rows), (3, vec![(1, 4, 60), (2, 1, 50), (3, 3, 30)]));
        let (_, _, rows) = read_list(&answer(2, &toplist, &merged(2)));
        assert_eq!(rows[0], (1, 2, 60));
        let near = Query::parse(&get_body(0, MODE_NEAR, &[DESCENDING, 2, 0])).unwrap();
        let (_, _, rows) = read_list(&answer(2, &near, &merged(2)));
        assert_eq!(rows, [(1, 2, 60), (2, 1, 50)]);
        // A friend's other Mii brings their row.
        let friends = Query::parse(&get_body(0, MODE_FRIENDS, &friends_params(10, &[2]))).unwrap();
        let (_, _, rows) = read_list(&answer(3, &friends, &merged(2)));
        assert_eq!(rows, [(1, 2, 60), (3, 3, 30)]);
        // Nobody's: the board as it was.
        let alone = by_player(
            board(&[50, 40]),
            &HashMap::new(),
            0,
            |_| 0,
            Sort::Descending,
        );
        assert_eq!(ranks(&alone), [(1, 1, 50), (2, 2, 40)]);
    }

    #[test]
    fn only_rows_of_one_day_add_up() {
        // Striker of the day: pid 1 (Europe) played yesterday, pid 2 (US)
        // today, both one player's; pid 3 today.
        let dated = |day: u8, wins: i32| {
            let mut userdata = vec![0u8; 128];
            userdata[0] = day;
            userdata[1] = 11;
            userdata[2..4].copy_from_slice(&2026u16.to_le_bytes());
            userdata[0x04..0x08].copy_from_slice(&wins.to_le_bytes());
            userdata
        };
        let row = |pid: i32, score: i32, userdata: Vec<u8>| Standing {
            rank: 0,
            profile_id: pid,
            region: 2,
            score,
            userdata,
            minutes: 0,
            members: vec![pid],
        };
        let players = HashMap::from([(1, 7), (2, 7)]);
        let friends = Query::parse(&get_body(1, MODE_FRIENDS, &friends_params(10, &[1]))).unwrap();
        // Yesterday's row stays its own, as without merging: the game drops
        // it by its date. Today's are not raised by it.
        let board = by_player(
            vec![
                row(1, 50, dated(1, 5)),
                row(2, 30, dated(2, 3)),
                row(3, 20, dated(2, 2)),
            ],
            &players,
            1,
            |pid| u8::from(pid == 1),
            Sort::Descending,
        );
        assert_eq!(ranks(&board), [(1, 1, 50), (2, 2, 30), (3, 3, 20)]);
        let (_, total, rows) = read_list(&answer(3, &friends, &board));
        assert_eq!((total, rows), (3, vec![(1, 1, 50), (3, 3, 20)]));
        // Both today: one row, the friend's Mii standing for it.
        let board = by_player(
            vec![
                row(1, 50, dated(2, 5)),
                row(2, 30, dated(2, 3)),
                row(3, 20, dated(2, 2)),
            ],
            &players,
            1,
            |pid| u8::from(pid == 1),
            Sort::Descending,
        );
        let (_, total, rows) = read_list(&answer(3, &friends, &board));
        assert_eq!((total, rows), (2, vec![(1, 1, 80), (2, 3, 20)]));
        assert_eq!(Submission::parse(&board[0].userdata).wins, Some(8));
    }

    #[sqlx::test]
    async fn a_players_profiles_are_one_row_for_the_game(pool: PgPool) {
        // Birnen's European and US games, each with its WFC profile, and
        // Mario's.
        let mut profiles = Vec::new();
        for (user, game, name) in [
            (1, "R4QPabcdefg", "Birnen"),
            (2, "R4QEabcdefg", "Birnen US"),
            (3, "R4QEhijklmn", "Mario"),
        ] {
            crate::accounts::log_in(&pool, user).await.unwrap();
            let profile = crate::accounts::profile_login(&pool, user, game, Some(name), "10.0.0.1")
                .await
                .unwrap();
            profiles.push(profile.profile_id);
        }
        // A launcher proved both for one Recharged player.
        let birnen: i64 = sqlx::query_scalar!(
            "INSERT INTO players (display_name) VALUES ('Birnen') RETURNING player_id"
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        for profile in &profiles[..2] {
            sqlx::query!(
                "INSERT INTO player_wfc_profiles (profile_id, player_id) VALUES ($1, $2)",
                profile,
                birnen
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        let userdata = |wins: i32, losses: i32| {
            let mut userdata = vec![0u8; 128];
            userdata[0x04..0x08].copy_from_slice(&wins.to_le_bytes());
            userdata[0x08..0x0C].copy_from_slice(&losses.to_le_bytes());
            userdata
        };
        for (pid, score, wins, losses) in [
            (profiles[0], 171, 10, 4),
            (profiles[1], 85, 5, 0),
            (profiles[2], 200, 12, 1),
        ] {
            sqlx::query!(
                "INSERT INTO scores (profile_id, category, region, score, userdata) VALUES ($1, 0, 2, $2, $3)",
                pid,
                score,
                &userdata(wins, losses),
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        let state = GamestatsState::with_pool(pool);
        let toplist = get_body(0, MODE_TOPLIST, &[DESCENDING, 10, 0]);
        // Birnen's US game sees its own Mii with everything Birnen won.
        let reply = get(&state, profiles[1], &toplist).await.unwrap();
        let (_, total, rows) = read_list(&reply);
        assert_eq!(
            (total, rows),
            (2, vec![(1, profiles[1], 256), (2, profiles[2], 200)])
        );
        let record = Submission::parse(&reply[12 + 24..12 + 24 + 128]);
        assert_eq!((record.wins, record.losses), (Some(15), Some(4)));
        // Mario sees Birnen by the best Mii.
        let (_, _, rows) = read_list(&get(&state, profiles[2], &toplist).await.unwrap());
        assert_eq!(rows[0], (1, profiles[0], 256));
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
    async fn a_sets_games_stay_off_the_boards(pool: PgPool) {
        let state = GamestatsState::with_pool(pool.clone());
        crate::accounts::log_in(&pool, 1).await.unwrap();
        let mario =
            crate::accounts::profile_login(&pool, 1, "R4QEabcdefg", Some("Mario"), "10.0.0.1")
                .await
                .unwrap()
                .profile_id;
        // Mario's player plays a set whose games met in RANKED.
        let set: i64 = sqlx::query_scalar(
            "WITH players AS (
                 INSERT INTO players (display_name) VALUES ('Mario'), ('Bowser')
                 RETURNING player_id),
             bound AS (
                 INSERT INTO player_wfc_profiles (profile_id, player_id)
                 SELECT $1, min(player_id) FROM players)
             INSERT INTO matches (kind, state, home, away, rule_set, rules, rules_hash, playing_at)
             SELECT 'test', 'accepted', min(player_id), max(player_id), 'x', '{}', 'h', now()
             FROM players
             RETURNING match_id",
        )
        .bind(mario)
        .fetch_one(&pool)
        .await
        .unwrap();
        let score = async || -> Option<i32> {
            sqlx::query_scalar("SELECT score FROM scores WHERE profile_id = $1 AND category = 1")
                .bind(mario)
                .fetch_optional(&pool)
                .await
                .unwrap()
        };

        // The game hears its put went through; the boards don't change.
        assert_eq!(
            put(&state, mario, &put_body(1, 100, 0xaa)).await.unwrap(),
            PUT_REPLY
        );
        assert_eq!(score().await, None);
        // Its result's put right after the set ended is still the set's.
        sqlx::query(
            "UPDATE matches SET state = 'finished', updated_at = now() WHERE match_id = $1",
        )
        .bind(set)
        .execute(&pool)
        .await
        .unwrap();
        put(&state, mario, &put_body(1, 110, 0xaa)).await.unwrap();
        assert_eq!(score().await, None);
        // Later, RANKED counts again.
        sqlx::query(
            "UPDATE matches SET updated_at = now() - interval '3 minutes' WHERE match_id = $1",
        )
        .bind(set)
        .execute(&pool)
        .await
        .unwrap();
        put(&state, mario, &put_body(1, 120, 0xbb)).await.unwrap();
        assert_eq!(score().await, Some(120));
        // A game kept for a set (ready for it in RANKED) is the set's too.
        state.pairings.ready(99, 0, mario, "R4QE");
        put(&state, mario, &put_body(1, 200, 0xcc)).await.unwrap();
        assert_eq!(score().await, Some(120));
    }

    #[sqlx::test]
    async fn a_login_whose_scores_were_deleted_puts_nothing(pool: PgPool) {
        let state = GamestatsState::with_pool(pool.clone());
        let address = std::net::IpAddr::from([10, 0, 0, 1]);
        state.gpcm.fake_login(7, address);
        let score = async || -> Option<i32> {
            sqlx::query_scalar("SELECT score FROM scores WHERE profile_id = 7 AND category = 0")
                .fetch_optional(&pool)
                .await
                .unwrap()
        };
        put(&state, 7, &put_body(0, 50, 0xaa)).await.unwrap();
        assert_eq!(score().await, Some(50));

        // An admin deletes its scores; profile 8 is not logged in.
        assert_eq!(state.gpcm.keep_off_boards(&[7, 8]), [7]);
        assert_eq!(state.gpcm.keep_off_boards(&[7]), Vec::<i32>::new());
        sqlx::query("DELETE FROM scores")
            .execute(&pool)
            .await
            .unwrap();
        // The game hears its put went through; its old total stays off.
        assert_eq!(
            put(&state, 7, &put_body(0, 60, 0xbb)).await.unwrap(),
            PUT_REPLY
        );
        assert_eq!(score().await, None);

        // Its next login starts again from 0, and counts.
        state.gpcm.fake_login(7, address);
        put(&state, 7, &put_body(0, 0, 0)).await.unwrap();
        assert_eq!(score().await, Some(0));

        // A delete that failed puts it back on the boards.
        state.gpcm.keep_off_boards(&[7]);
        state.gpcm.back_on_boards(&[7]);
        put(&state, 7, &put_body(0, 3, 0)).await.unwrap();
        assert_eq!(score().await, Some(3));
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

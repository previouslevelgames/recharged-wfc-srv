//! What a put stores and what a get returns (`docs/gamestats2.md`,
//! "put2.asp" and "get2.asp").
//!
//! `body` is the ranking body decoded from `data`: a 16-byte header of
//! little-endian words, then the userdata (put) or the parameters (get).
//!
//! Scores live in the `scores` table (`migrations/`): two boards, the season's
//! and the day's, for every region and release, by UTC. A row is its
//! profile's share of its player's total ([`Leaderboard::count`]); a row of a
//! season (or day) that has ended moves to `past_scores` with its profile's
//! next put. A get is three steps: [`Leaderboard::ranked`] reads and ranks a
//! board's rows of this season or day; [`by_player`] makes the rows of one
//! Recharged player one row (`players`); [`answer`] picks the rows the
//! request wants and encodes them as the asking game is shown them, without
//! I/O.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    ops::{Add, Sub},
};

use sqlx::PgPool;
use time::{Date, Duration, OffsetDateTime};
use tracing::{debug, info};

use super::{
    GamestatsError, GamestatsState, players, season,
    submission::{self, Submission},
};

/// Highest category the game uses
/// (`NETWORK_PERSISTENT_ALTERNATE_STRIKER_OF_DAY`).
const MAX_CATEGORY: u32 = 4;
/// The two boards every game's puts count on and its gets read, whatever its
/// region, release or country group: the season's and striker of the day's.
const SEASON: i32 = 0;
const DAILY: i32 = 1;
/// European rankings' friends board (`NETWORK_PERSISTENT_FRIENDS`): EU games
/// put every season game to it as well as to the season's, so its puts count
/// nothing more and its gets read the season's board.
const EU_FRIENDS: u32 = 2;
/// The highest score a game holds: JP and KR games clamp to 99999, the others
/// to 999999 (`ReportGameResult`).
const MAX_SCORE: i32 = 999_999;
const MAX_SCORE_JP_KR: i32 = 99_999;
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

/// The board a category counts on: the EU alternate country group's season
/// (3) and day (4) are the main ones, and so is the EU friends board.
fn board(category: u32) -> i32 {
    if matches!(category, 1 | 4) {
        DAILY
    } else {
        SEASON
    }
}

/// Counts a put: what its game's total gained since the game last held one
/// ([`Leaderboard::count`]).
///
/// Returns the response payload: the client does not read it, but it must be
/// at least 1 byte and must not start with `error:`.
pub async fn put(state: &GamestatsState, pid: i32, body: &[u8]) -> Result<Vec<u8>, GamestatsError> {
    put_at(state, pid, body, OffsetDateTime::now_utc()).await
}

/// [`put`] at the time `now`, in UTC.
pub(super) async fn put_at(
    state: &GamestatsState,
    pid: i32,
    body: &[u8],
    now: OffsetDateTime,
) -> Result<Vec<u8>, GamestatsError> {
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
    let put = Put::new(pid, category, region, score, &body[16..(16 + size)]);

    // An admin deleted this profile's scores while its game was logged in:
    // the game still holds its old totals. Its next login finds no row of its
    // own and starts again from 0.
    if state.gpcm.off_boards(pid) {
        info!(target: "gamestats", pid, category, score, "scores deleted: kept off the boards until the next login");
        state
            .leaderboard
            .record(&put, Outcome::Deleted, now)
            .await?;
        return Ok(PUT_REPLY.to_vec());
    }
    if category == EU_FRIENDS {
        debug!(target: "gamestats", pid, score, "EU friends board: counted on the season's");
        return Ok(PUT_REPLY.to_vec());
    }
    // A Recharged set's games meet in RANKED only to find each other: their
    // results go to the set, not the boards (a FRIENDS battle uploads
    // nothing). The game hears its put went through: a refused put would
    // stop its later ones until it logs in again.
    let kept = in_a_set(state, pid).await?;
    let counted = state.leaderboard.count(&put, kept, now).await?;
    if kept {
        info!(target: "gamestats", pid, category, score, "a set's game: kept off the boards");
    } else {
        let Tally { wins, losses, .. } = put.total;
        info!(
            target: "gamestats",
            pid, category, score, wins, losses,
            change = %counted.change, share = %counted.share,
            "stored"
        );
    }
    Ok(PUT_REPLY.to_vec())
}

/// Points, wins and losses: a game's total, a profile's share of its
/// player's, or a change to one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tally {
    score: i32,
    wins: i32,
    losses: i32,
}

impl Tally {
    /// A row's: its score, and the record in its userdata (none where the
    /// userdata has none).
    fn of(score: i32, userdata: &[u8]) -> Self {
        let record = Submission::parse(userdata);
        Self {
            score,
            wins: record.wins.unwrap_or(0),
            losses: record.losses.unwrap_or(0),
        }
    }

    /// Points, wins or losses, by number.
    fn get(&self, field: usize) -> i32 {
        [self.score, self.wins, self.losses][field]
    }

    fn set(&mut self, field: usize, value: i32) {
        *[&mut self.score, &mut self.wins, &mut self.losses][field] = value;
    }
}

impl Add for Tally {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            score: self.score.saturating_add(other.score),
            wins: self.wins.saturating_add(other.wins),
            losses: self.losses.saturating_add(other.losses),
        }
    }
}

impl Sub for Tally {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self {
            score: self.score.saturating_sub(other.score),
            wins: self.wins.saturating_sub(other.wins),
            losses: self.losses.saturating_sub(other.losses),
        }
    }
}

impl fmt::Display for Tally {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}-{}", self.score, self.wins, self.losses)
    }
}

/// A put as [`Leaderboard::count`] takes it.
struct Put<'a> {
    pid: i32,
    category: u32,
    region: u32,
    userdata: &'a [u8],
    /// The game's total, as it put it.
    total: Tally,
    /// Whether the userdata holds a record: the server writes the share's
    /// into it only then.
    has_record: bool,
    /// The game's date, by its own clock.
    date: Option<Date>,
}

impl<'a> Put<'a> {
    fn new(pid: i32, category: u32, region: u32, score: i32, userdata: &'a [u8]) -> Self {
        let record = Submission::parse(userdata);
        Self {
            pid,
            category,
            region,
            userdata,
            total: Tally::of(score, userdata),
            has_record: record.wins.is_some() && record.losses.is_some(),
            date: record.date,
        }
    }
}

/// What the server made of a put (`score_puts.outcome`).
#[derive(Clone, Copy)]
enum Outcome {
    Counted,
    Set,
    Deleted,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Counted => "counted",
            Self::Set => "set",
            Self::Deleted => "deleted",
        }
    }
}

/// What a put changed: its player's total, and its profile's share after.
struct Counted {
    change: Tally,
    share: Tally,
}

/// Takes what `own` lacks below nothing, field by field, from `others`,
/// the most first: a profile's share of its player's total goes below
/// nothing when its game loses points the player won on another (a series'
/// provisional loss takes 5 points from the game's total). Returns what the
/// others could not give either: a loss of points the board's season (or
/// day) does not hold, from a total of the one before. The game's next gain
/// pays it back first, as when the series' result returns those 5 points.
fn cover(own: &mut Tally, others: &mut [Tally]) -> Tally {
    let mut owed = Tally::default();
    for field in 0..3 {
        let mut short = own.get(field).saturating_neg().max(0);
        if short == 0 {
            continue;
        }
        own.set(field, 0);
        let mut order: Vec<usize> = (0..others.len()).collect();
        order.sort_by_key(|&at| std::cmp::Reverse(others[at].get(field)));
        for at in order {
            let taken = short.min(others[at].get(field).max(0));
            others[at].set(field, others[at].get(field) - taken);
            short -= taken;
        }
        owed.set(field, short);
    }
    owed
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
    get_at(state, pid, body, OffsetDateTime::now_utc()).await
}

/// [`get`] at the time `now`, in UTC.
pub(super) async fn get_at(
    state: &GamestatsState,
    pid: i32,
    body: &[u8],
    now: OffsetDateTime,
) -> Result<Vec<u8>, GamestatsError> {
    let query = Query::parse(body)?;
    let category = board(query.category);
    let board = state.leaderboard.ranked(category, query.sort, now).await?;
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
    let board = by_player(board, &players, category, preference, query.sort);
    let view = View::new(query.region, now);
    let (reply, shown) = answer(pid, &query, &board, &view);
    // The game takes its own row of a NEAR answer as its total
    // (`ApplyLeaderboardToSave`): its next put counts from it.
    if let Some(shown) = shown {
        state
            .leaderboard
            .hold(pid, category, shown, view.today)
            .await?;
    }
    Ok(reply)
}

/// How a game is shown the rows: dated today by its own clock, so that it
/// takes them as this season's or day's at any hour (`FilterCurrentSeason`,
/// `IsNewNetworkSeason`), and with no more points than it holds.
struct View {
    today: Date,
    max_score: i32,
}

impl View {
    /// For a game of the DWC `region` its requests carry.
    fn new(region: u32, now: OffsetDateTime) -> Self {
        Self {
            today: (now + Duration::hours(season::clock_offset(region))).date(),
            max_score: if matches!(region, 1 | 8) {
                MAX_SCORE_JP_KR
            } else {
                MAX_SCORE
            },
        }
    }

    /// A row's score and userdata as the game is shown them. Userdata whose
    /// digest failed stays as it is: the game blanks it anyway.
    fn show(&self, row: &Standing) -> (i32, Vec<u8>) {
        let userdata = if submission::signed(&row.userdata) {
            submission::rewritten(&row.userdata, Some(self.today), None, None)
        } else {
            row.userdata.clone()
        };
        (row.score.min(self.max_score), userdata)
    }
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

    /// Counts a put (docs/gamestats2.md, "What a put counts"). The game puts
    /// its whole running total, which starts from the total it was last
    /// shown, its player's: the put adds what the total gained since the game
    /// last held one (its last put, or the row a NEAR answer showed it) to
    /// its profile's share of the season (or day), by UTC. A total of 0, 0
    /// and 0 is the game starting a season, a day or its first login again,
    /// and adds nothing; a put dated in a later season (or day) than what the
    /// game held, by its own clock, starts from nothing. A set's put (`kept`)
    /// adds nothing either. Either way the game now holds the put.
    ///
    /// A share that would go below nothing takes the rest from the player's
    /// other shares ([`cover`]). A row of an ended season (or day), by the
    /// date in its userdata, is kept in `past_scores` first. Every put goes
    /// to `score_puts`.
    async fn count(
        &self,
        put: &Put<'_>,
        kept: bool,
        now: OffsetDateTime,
    ) -> Result<Counted, sqlx::Error> {
        let category = board(put.category);
        let period = season::first_day(category, now.date());
        let mut tx = self.pool.begin().await?;
        // The player's rows, locked in one order: another of its games may
        // put at once.
        let mut profiles: Vec<i32> = sqlx::query_scalar!(
            "SELECT theirs.profile_id FROM player_wfc_profiles theirs
             JOIN player_wfc_profiles mine ON mine.player_id = theirs.player_id
             WHERE mine.profile_id = $1",
            put.pid,
        )
        .fetch_all(&mut *tx)
        .await?;
        if !profiles.contains(&put.pid) {
            profiles.push(put.pid);
        }
        let rows = sqlx::query!(
            "SELECT profile_id, score, userdata, updated_at,
                    held_score, held_wins, held_losses, held_on
             FROM scores WHERE category = $1 AND profile_id = ANY($2)
             ORDER BY profile_id
             FOR UPDATE",
            category,
            &profiles,
        )
        .fetch_all(&mut *tx)
        .await?;
        let period_of = |userdata: &[u8], updated_at: OffsetDateTime| {
            let date = Submission::parse(userdata).date;
            season::first_day(category, date.unwrap_or(updated_at.date()))
        };

        let own = rows.iter().find(|row| row.profile_id == put.pid);
        let current = own.filter(|row| period_of(&row.userdata, row.updated_at) == period);
        let share = current.map_or(Tally::default(), |row| Tally::of(row.score, &row.userdata));
        let holding = own.and_then(|row| {
            let tally = Tally {
                score: row.held_score?,
                wins: row.held_wins?,
                losses: row.held_losses?,
            };
            Some((tally, row.held_on))
        });
        // What the game held before this put.
        let before = match holding {
            Some((_, Some(on)))
                if put.date.is_some_and(|date| {
                    season::first_day(category, date) != season::first_day(category, on)
                }) =>
            {
                Tally::default()
            }
            Some((tally, _)) => tally,
            // A game not seen since the server counted shares holds its row,
            // as it put it.
            None => share,
        };
        let change = if kept || put.total == Tally::default() {
            Tally::default()
        } else {
            put.total - before
        };
        let mut new = share + change;
        let others: Vec<_> = rows
            .iter()
            .filter(|row| {
                row.profile_id != put.pid && period_of(&row.userdata, row.updated_at) == period
            })
            .map(|row| (row, Tally::of(row.score, &row.userdata)))
            .collect();
        let mut given: Vec<Tally> = others.iter().map(|(_, tally)| *tally).collect();
        // What the game holds from now on, with what the board could not
        // take: its next gain pays that back first.
        let held = put.total + cover(&mut new, &mut given);
        for ((row, before), after) in others.iter().zip(given) {
            if *before == after {
                continue;
            }
            sqlx::query!(
                "UPDATE scores SET score = $3, userdata = $4
                 WHERE profile_id = $1 AND category = $2",
                row.profile_id,
                category,
                after.score,
                submission::with_record(&row.userdata, Some(after.wins), Some(after.losses)),
            )
            .execute(&mut *tx)
            .await?;
        }

        if let Some(row) = own
            && let Some(date) = Submission::parse(&row.userdata).date
            && season::first_day(category, date) != period
        {
            sqlx::query!(
                "INSERT INTO past_scores
                     (profile_id, category, first_day, region, score, userdata, updated_at)
                 SELECT profile_id, category, $3, region, score, userdata, updated_at
                 FROM scores WHERE profile_id = $1 AND category = $2
                 ON CONFLICT (category, first_day, profile_id) DO UPDATE
                 SET region = EXCLUDED.region,
                     score = EXCLUDED.score,
                     userdata = EXCLUDED.userdata,
                     updated_at = EXCLUDED.updated_at",
                put.pid,
                category,
                season::first_day(category, date),
            )
            .execute(&mut *tx)
            .await?;
        }
        // The row of this season (or day): the put's userdata (its Mii) with
        // the share's record, dated today by UTC.
        let record = |value: i32| put.has_record.then_some(value);
        let userdata = submission::rewritten(
            put.userdata,
            Some(now.date()),
            record(new.wins),
            record(new.losses),
        );
        if current.is_none() || new != share {
            sqlx::query!(
                "INSERT INTO scores (profile_id, category, region, score, userdata, updated_at,
                                     held_score, held_wins, held_losses, held_on)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                 ON CONFLICT (profile_id, category) DO UPDATE
                 SET region = EXCLUDED.region,
                     score = EXCLUDED.score,
                     userdata = EXCLUDED.userdata,
                     updated_at = EXCLUDED.updated_at,
                     held_score = EXCLUDED.held_score,
                     held_wins = EXCLUDED.held_wins,
                     held_losses = EXCLUDED.held_losses,
                     held_on = EXCLUDED.held_on",
                put.pid,
                category,
                put.region as i32,
                new.score.min(MAX_SCORE),
                userdata,
                now,
                held.score,
                held.wins,
                held.losses,
                put.date,
            )
            .execute(&mut *tx)
            .await?;
        } else if !kept {
            // Nothing gained, but maybe a new Mii (the game puts its total
            // unchanged after one): its place among equal scores stays.
            sqlx::query!(
                "UPDATE scores SET region = $3, userdata = $4
                 WHERE profile_id = $1 AND category = $2",
                put.pid,
                category,
                put.region as i32,
                userdata,
            )
            .execute(&mut *tx)
            .await?;
            Self::hold_in(&mut tx, put.pid, category, held, put.date).await?;
        } else {
            Self::hold_in(&mut tx, put.pid, category, held, put.date).await?;
        }
        let outcome = if kept { Outcome::Set } else { Outcome::Counted };
        Self::record_in(&mut tx, put, outcome, (!kept).then_some(change), now).await?;
        tx.commit().await?;
        Ok(Counted { change, share: new })
    }

    /// Notes that `pid`'s game holds `held` on the board `category`, dated
    /// `on` by its clock.
    async fn hold(
        &self,
        pid: i32,
        category: i32,
        held: Tally,
        on: Date,
    ) -> Result<(), sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        Self::hold_in(&mut conn, pid, category, held, Some(on)).await
    }

    async fn hold_in(
        conn: &mut sqlx::PgConnection,
        pid: i32,
        category: i32,
        held: Tally,
        on: Option<Date>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE scores SET held_score = $3, held_wins = $4, held_losses = $5, held_on = $6
             WHERE profile_id = $1 AND category = $2",
            pid,
            category,
            held.score,
            held.wins,
            held.losses,
            on,
        )
        .execute(conn)
        .await?;
        Ok(())
    }

    /// Keeps a put that changed nothing in `score_puts`.
    async fn record(
        &self,
        put: &Put<'_>,
        outcome: Outcome,
        now: OffsetDateTime,
    ) -> Result<(), sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        Self::record_in(&mut conn, put, outcome, None, now).await
    }

    async fn record_in(
        conn: &mut sqlx::PgConnection,
        put: &Put<'_>,
        outcome: Outcome,
        change: Option<Tally>,
        now: OffsetDateTime,
    ) -> Result<(), sqlx::Error> {
        let record = |value: i32| put.has_record.then_some(value);
        sqlx::query!(
            "INSERT INTO score_puts (profile_id, category, at, outcome, score, wins, losses,
                                     score_change, wins_change, losses_change)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            put.pid,
            put.category as i32,
            now,
            outcome.as_str(),
            put.total.score,
            record(put.total.wins),
            record(put.total.losses),
            change.map(|change| change.score),
            change.map(|change| change.wins),
            change.map(|change| change.losses),
        )
        .execute(conn)
        .await?;
        Ok(())
    }

    /// The rows of the board `category` of this season (or day) at `now`,
    /// by UTC and the date in their userdata (their update without one),
    /// ranked by score in `sort` order. Rows that share a rank come earlier
    /// update first.
    async fn ranked(
        &self,
        category: i32,
        sort: Sort,
        now: OffsetDateTime,
    ) -> Result<Vec<Standing>, GamestatsError> {
        let period = season::first_day(category, now.date());
        // A row is dated by UTC, or by its game's clock if put before the
        // server counted shares: up to 9 hours ahead of its update.
        let from = period.midnight().assume_utc() - Duration::DAY;
        // ORDER BY takes no parameter for its direction: one query each.
        let rows = match sort {
            Sort::Descending => {
                sqlx::query_as!(
                    Row,
                    r#"SELECT profile_id, region, score, userdata, updated_at,
                              floor(extract(epoch FROM now() - updated_at) / 60)::bigint AS "minutes!"
                       FROM scores
                       WHERE category = $1 AND updated_at >= $2
                       ORDER BY score DESC, updated_at, profile_id"#,
                    category,
                    from,
                )
                .fetch_all(&self.pool)
                .await?
            }
            Sort::Ascending => {
                sqlx::query_as!(
                    Row,
                    r#"SELECT profile_id, region, score, userdata, updated_at,
                              floor(extract(epoch FROM now() - updated_at) / 60)::bigint AS "minutes!"
                       FROM scores
                       WHERE category = $1 AND updated_at >= $2
                       ORDER BY score ASC, updated_at, profile_id"#,
                    category,
                    from,
                )
                .fetch_all(&self.pool)
                .await?
            }
        };
        let mut board: Vec<Standing> = Vec::with_capacity(rows.len());
        for row in rows {
            let date = Submission::parse(&row.userdata).date;
            if season::first_day(category, date.unwrap_or(row.updated_at.date())) != period {
                continue;
            }
            // RANK(): equal scores share the rank of the first of them.
            let rank = match board.last() {
                Some(last) if last.score == row.score => last.rank,
                _ => board.len() as i64 + 1,
            };
            board.push(Standing {
                rank,
                profile_id: row.profile_id,
                region: row.region,
                score: row.score,
                userdata: row.userdata,
                minutes: row.minutes,
                members: vec![row.profile_id],
            });
        }
        Ok(board)
    }
}

/// A row of `scores` as [`Leaderboard::ranked`] reads it.
struct Row {
    profile_id: i32,
    region: i32,
    score: i32,
    userdata: Vec<u8>,
    updated_at: OffsetDateTime,
    minutes: i64,
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
/// `pid`, and encodes the reply as `view` shows them. Also returns the
/// caller's own row of a NEAR answer, which its game takes as its total.
fn answer(pid: i32, query: &Query, board: &[Standing], view: &View) -> (Vec<u8>, Option<Tally>) {
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
            return (words(&[MODE_ORDER, rank, total]), None);
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
    let mut own = None;
    for row in rows {
        let (score, userdata) = view.show(row);
        // The game takes its row only when it shows it: with its digest.
        if matches!(query.mode, Mode::Near { .. })
            && row.profile_id == pid
            && submission::signed(&userdata)
        {
            own = Some(Tally::of(score, &userdata));
        }
        reply.extend(words(&[
            row.rank as u32,
            row.profile_id as u32,
            score as u32,
            row.region as u32,
            u32::try_from(row.minutes.max(0)).unwrap_or(u32::MAX),
            userdata.len() as u32,
        ]));
        reply.extend(&userdata);
    }
    (reply, own)
}

const MODE_ORDER: u32 = 0;
const MODE_TOPLIST: u32 = 1;
const MODE_NEAR: u32 = 2;
const MODE_FRIENDS: u32 = 3;

/// A get2 request (`DWCi_RankingSessionGetAsync`). Every region reads the
/// same boards; its region tells the asking game's clock and score range
/// ([`View`]). Its `since` (minutes since the game's season or day began,
/// by its clock) is not read: the boards' seasons and days are UTC's.
#[derive(Debug, PartialEq)]
struct Query {
    /// DWCRnkRegion of the asking game: JP 1, US 2, EU 4, KR 8.
    region: u32,
    category: u32,
    sort: Sort,
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
        let region = word(body, 0)?;
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
            return Ok(Self {
                region,
                category,
                sort,
                mode: Mode::Order,
            });
        }

        let limit = word(params, 1)? as usize;
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
            region,
            category,
            sort,
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

    /// The reply to `query` from `pid`, as a US game is shown it.
    fn shown(pid: i32, query: &Query, board: &[Standing]) -> Vec<u8> {
        answer(pid, query, board, &View::new(2, OffsetDateTime::now_utc())).0
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

    /// `hour` o'clock UTC on a day.
    fn at(year: i32, month: u8, day: u8, hour: u8) -> OffsetDateTime {
        date(year, month, day)
            .with_hms(hour, 0, 0)
            .unwrap()
            .assume_utc()
    }

    fn date(year: i32, month: u8, day: u8) -> Date {
        Date::from_calendar_date(year, time::Month::try_from(month).unwrap(), day).unwrap()
    }

    /// A row of `category` whose userdata is dated `day` November 2026 (no
    /// date for 0), updated at `updated_at`.
    async fn insert_dated(
        pool: &PgPool,
        pid: i32,
        category: i32,
        score: i32,
        day: u8,
        updated_at: OffsetDateTime,
    ) {
        let mut userdata = vec![0u8; 128];
        if day > 0 {
            userdata[0..4].copy_from_slice(&[day, 11, 0xEA, 0x07]);
        }
        sqlx::query!(
            "INSERT INTO scores (profile_id, category, region, score, userdata, updated_at)
             VALUES ($1, $2, 2, $3, $4, $5)",
            pid,
            category,
            score,
            userdata,
            updated_at,
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
                region: 2,
                category: 1,
                sort: Sort::Descending,
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
        assert_eq!(shown(1, &query, &[]), [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn toplist_takes_the_first_rows() {
        let query = Query::parse(&get_body(0, MODE_TOPLIST, &[DESCENDING, 3, 0])).unwrap();
        let (mode, total, rows) = read_list(&shown(9, &query, &board(&[40, 40, 20, 10])));
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
            read_list(&shown(pid, &query, &board))
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
        let (mode, total, rows) = read_list(&shown(3, &query, &board(&[50, 40, 30, 20])));
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
        let (_, total, rows) = read_list(&shown(4, &toplist, &asked_by_4));
        assert_eq!((total, rows), (3, vec![(1, 4, 60), (2, 1, 50), (3, 3, 30)]));
        let (_, _, rows) = read_list(&shown(2, &toplist, &merged(2)));
        assert_eq!(rows[0], (1, 2, 60));
        let near = Query::parse(&get_body(0, MODE_NEAR, &[DESCENDING, 2, 0])).unwrap();
        let (_, _, rows) = read_list(&shown(2, &near, &merged(2)));
        assert_eq!(rows, [(1, 2, 60), (2, 1, 50)]);
        // A friend's other Mii brings their row.
        let friends = Query::parse(&get_body(0, MODE_FRIENDS, &friends_params(10, &[2]))).unwrap();
        let (_, _, rows) = read_list(&shown(3, &friends, &merged(2)));
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
        let (_, total, rows) = read_list(&shown(3, &friends, &board));
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
        let (_, total, rows) = read_list(&shown(3, &friends, &board));
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

        let board = leaderboard
            .ranked(0, Sort::Descending, OffsetDateTime::now_utc())
            .await
            .unwrap();
        assert_eq!(
            ranks(&board),
            [(1, 3, 40), (1, 2, 40), (3, 4, 20), (4, 1, 10)]
        );
        assert_eq!(board[0].minutes, 3);

        let board = leaderboard
            .ranked(0, Sort::Ascending, OffsetDateTime::now_utc())
            .await
            .unwrap();
        assert_eq!(
            ranks(&board),
            [(1, 1, 10), (2, 4, 20), (3, 3, 40), (3, 2, 40)]
        );

        assert!(
            leaderboard
                .ranked(1, Sort::Descending, OffsetDateTime::now_utc())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[sqlx::test]
    async fn a_board_holds_the_rows_of_this_season_by_utc(pool: PgPool) {
        // 19 November 2026, 03:00 UTC: the season of 12 November ended 3
        // hours ago, though a US game's clock still says the 18th.
        let now = at(2026, 11, 19, 3);
        for (pid, score, day, hours_ago) in [
            // Dated today by UTC: this season's.
            (1, 10, 19, 1),
            // Yesterday's: the last season's.
            (2, 20, 18, 5),
            // Put by a US game before rows were dated by UTC, an hour ago:
            // its own date, the 18th, tells.
            (3, 30, 18, 1),
            // Without a date, its update tells.
            (4, 40, 0, 1),
            (5, 50, 0, 4),
        ] {
            insert_dated(&pool, pid, 0, score, day, now - Duration::hours(hours_ago)).await;
        }
        let leaderboard = Leaderboard::new(pool);
        let board = leaderboard.ranked(0, Sort::Descending, now).await.unwrap();
        assert_eq!(ranks(&board), [(1, 4, 40), (2, 1, 10)]);
        let board = leaderboard.ranked(0, Sort::Ascending, now).await.unwrap();
        assert_eq!(ranks(&board), [(1, 1, 10), (2, 4, 40)]);
    }

    #[sqlx::test]
    async fn a_put_counts_and_a_get_returns_the_row(pool: PgPool) {
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

        // The game hears its put went through; the boards count nothing,
        // but its row notes what the game holds now.
        assert_eq!(
            put(&state, mario, &put_body(1, 100, 0xaa)).await.unwrap(),
            PUT_REPLY
        );
        assert_eq!(score().await, Some(0));
        // Its result's put right after the set ended is still the set's.
        sqlx::query(
            "UPDATE matches SET state = 'finished', updated_at = now() WHERE match_id = $1",
        )
        .bind(set)
        .execute(&pool)
        .await
        .unwrap();
        put(&state, mario, &put_body(1, 110, 0xaa)).await.unwrap();
        assert_eq!(score().await, Some(0));
        // Later, RANKED counts again: what the game gained since the set.
        sqlx::query(
            "UPDATE matches SET updated_at = now() - interval '3 minutes' WHERE match_id = $1",
        )
        .bind(set)
        .execute(&pool)
        .await
        .unwrap();
        put(&state, mario, &put_body(1, 120, 0xbb)).await.unwrap();
        assert_eq!(score().await, Some(10));
        // A game kept for a set (ready for it in RANKED) is the set's too.
        state.pairings.ready(99, 0, mario, "R4QE");
        put(&state, mario, &put_body(1, 200, 0xcc)).await.unwrap();
        assert_eq!(score().await, Some(10));
        let outcomes: Vec<String> =
            sqlx::query_scalar("SELECT outcome FROM score_puts ORDER BY put_id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(outcomes, ["set", "set", "counted", "set"]);
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
        let put_on = async |category, score, day| {
            put_at(
                &state,
                7,
                &dated_put(category, score, day),
                at(2026, 10, day, 12),
            )
            .await
            .unwrap();
        };
        for (category, score, day) in [(0, 10, 20), (0, 22, 21), (1, 5, 21), (1, 6, 21)] {
            put_on(category, score, day).await;
        }
        // 22 October: a new season and a new day; the game puts 0.
        for category in [0, 1] {
            put_on(category, 0, 22).await;
        }
        // Later the same season: nothing more is kept.
        put_on(0, 3, 23).await;

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

    /// A game as `NetworkStatsManager` keeps its total on one board: it takes
    /// its own row of a NEAR answer as its total, puts a provisional loss at
    /// kickoff and the result at full time, and starts again from this game
    /// when its clock says a new season (or day) began since its total's
    /// date.
    struct Game {
        pid: i32,
        /// Its DWC region: its clock and score range.
        region: u32,
        category: u32,
        /// Its total, and the date on it by the game's clock.
        total: Option<(Tally, Date)>,
        mii: &'static str,
    }

    impl Game {
        fn new(pid: i32, region: u32, category: u32) -> Self {
            Self {
                pid,
                region,
                category,
                total: None,
                mii: "BKXO",
            }
        }

        fn today(&self, now: OffsetDateTime) -> Date {
            (now + Duration::hours(season::clock_offset(self.region))).date()
        }

        fn new_period(&self, on: Date, now: OffsetDateTime) -> bool {
            let category = super::board(self.category);
            season::first_day(category, on) != season::first_day(category, self.today(now))
        }

        /// Asks for the board around itself and takes its own row.
        async fn near(&mut self, state: &GamestatsState, now: OffsetDateTime) {
            let mut body = words(&[self.region, self.category, MODE_NEAR, 12]);
            body.extend(words(&[DESCENDING, 10, 1]));
            let reply = get_at(state, self.pid, &body, now).await.unwrap();
            let mut at = 12;
            for _ in 0..word(&reply, 1).unwrap() {
                let row = &reply[at..];
                let size = word(row, 5).unwrap() as usize;
                let userdata = &row[24..24 + size];
                if word(row, 1).unwrap() as i32 == self.pid && submission::signed(userdata) {
                    let total = Tally::of(word(row, 2).unwrap() as i32, userdata);
                    self.total = Some((total, Submission::parse(userdata).date.unwrap()));
                }
                at += 24 + size;
            }
        }

        /// Logs in: takes its row, or puts an empty one first.
        async fn log_in(&mut self, state: &GamestatsState, now: OffsetDateTime) {
            self.total = None;
            self.near(state, now).await;
            if self.total.is_none() {
                self.total = Some((Tally::default(), self.today(now)));
                self.put(state, now).await;
                self.near(state, now).await;
            }
        }

        async fn put(&self, state: &GamestatsState, now: OffsetDateTime) {
            let (total, on) = self.total.unwrap();
            let mut userdata = vec![0u8; 128];
            let year = (on.year() as u16).to_le_bytes();
            userdata[0..4].copy_from_slice(&[on.day(), u8::from(on.month()), year[0], year[1]]);
            userdata[0x04..0x08].copy_from_slice(&total.wins.to_le_bytes());
            userdata[0x08..0x0C].copy_from_slice(&total.losses.to_le_bytes());
            for (at, unit) in self.mii.encode_utf16().enumerate() {
                userdata[0x0E + 2 * at..0x10 + 2 * at].copy_from_slice(&unit.to_le_bytes());
            }
            submission::sign(&mut userdata);
            let mut body = words(&[self.region, self.category, total.score as u32, 128]);
            body.extend(userdata);
            put_at(state, self.pid, &body, now).await.unwrap();
        }

        /// Plays a game: `goals` scored, won or not.
        async fn play(
            &mut self,
            state: &GamestatsState,
            won: bool,
            goals: i32,
            now: OffsetDateTime,
        ) {
            let points = if won { 10 } else { 1 } + goals.min(10);
            let (mut total, on) = self.total.unwrap();
            let today = self.today(now);
            if self.new_period(on, now) {
                // `ReportGameResult` starts fresh; no provisional loss first.
                total = Tally {
                    score: points,
                    wins: i32::from(won),
                    losses: i32::from(!won),
                };
            } else {
                // `ReportDefaultDisconnectLoss`, then the result.
                let lost = total.score.min(5);
                total.score -= lost;
                total.losses += 1;
                self.total = Some((total, today));
                self.put(state, now).await;
                total.score += lost + points;
                if won {
                    total.wins += 1;
                    total.losses -= 1;
                }
            }
            self.total = Some((total, today));
            self.put(state, now).await;
        }
    }

    /// Two WFC profiles of one Recharged player (a European and a US game),
    /// and another player's.
    async fn two_games_of_one_player(pool: &PgPool) -> (i32, i32, i32) {
        let mut profiles = Vec::new();
        for (user, game) in [(1, "R4QPabcdefg"), (2, "R4QEabcdefg"), (3, "R4QEhijklmn")] {
            crate::accounts::log_in(pool, user).await.unwrap();
            let profile =
                crate::accounts::profile_login(pool, user, game, Some("BKXO"), "10.0.0.1")
                    .await
                    .unwrap();
            profiles.push(profile.profile_id);
        }
        let player: i64 = sqlx::query_scalar!(
            "INSERT INTO players (display_name) VALUES ('BKXO') RETURNING player_id"
        )
        .fetch_one(pool)
        .await
        .unwrap();
        for profile in &profiles[..2] {
            sqlx::query!(
                "INSERT INTO player_wfc_profiles (profile_id, player_id) VALUES ($1, $2)",
                profile,
                player
            )
            .execute(pool)
            .await
            .unwrap();
        }
        (profiles[0], profiles[1], profiles[2])
    }

    /// The board as another player's game sees it: `(pid, score, wins,
    /// losses)` of each row.
    async fn season_board(
        state: &GamestatsState,
        asker: i32,
        now: OffsetDateTime,
    ) -> Vec<(i32, i32, i32, i32)> {
        let body = get_body(0, MODE_TOPLIST, &[DESCENDING, 10, 1]);
        let reply = get_at(state, asker, &body, now).await.unwrap();
        let mut rows = Vec::new();
        let mut at = 12;
        for _ in 0..word(&reply, 1).unwrap() {
            let row = &reply[at..];
            let size = word(row, 5).unwrap() as usize;
            let record = Submission::parse(&row[24..24 + size]);
            rows.push((
                word(row, 1).unwrap() as i32,
                word(row, 2).unwrap() as i32,
                record.wins.unwrap(),
                record.losses.unwrap(),
            ));
            at += 24 + size;
        }
        rows
    }

    #[sqlx::test]
    async fn a_players_games_add_up_once_whatever_they_play_on(pool: PgPool) {
        let (europe, us, other) = two_games_of_one_player(&pool).await;
        let state = GamestatsState::with_pool(pool.clone());
        let now = at(2026, 11, 13, 12);
        let mut eu = Game::new(europe, 4, 0);
        let mut na = Game::new(us, 2, 0);

        // The US game first: 3-1 won (13 points), 2-4 lost (3).
        na.log_in(&state, now).await;
        na.play(&state, true, 3, now).await;
        na.play(&state, false, 2, now).await;
        assert_eq!(season_board(&state, other, now).await, [(us, 16, 1, 1)]);

        // The European game logs in and takes the player's total as its own.
        eu.log_in(&state, now).await;
        assert_eq!(
            eu.total.unwrap().0,
            Tally {
                score: 16,
                wins: 1,
                losses: 1
            }
        );
        // Its provisional loss takes 5 points its own share does not have;
        // the result gives them back: 5-0 won (15 points).
        eu.play(&state, true, 5, now).await;
        assert_eq!(season_board(&state, other, now).await, [(europe, 31, 2, 1)]);
        // It asks again and again: nothing counts twice.
        for _ in 0..3 {
            eu.near(&state, now).await;
            na.near(&state, now).await;
        }
        eu.play(&state, false, 1, now).await;
        assert_eq!(season_board(&state, other, now).await, [(europe, 33, 2, 2)]);

        // Back on the US game, which still holds 16 until it asks.
        na.near(&state, now).await;
        assert_eq!(
            na.total.unwrap().0,
            Tally {
                score: 33,
                wins: 2,
                losses: 2
            }
        );
        na.play(&state, true, 10, now).await;
        let total = (33 + 20, 3, 2);
        assert_eq!(
            season_board(&state, other, now).await,
            [(us, total.0, total.1, total.2)]
        );
        // Each game, as it asks, is shown the player's total under its own Mii.
        for game in [&mut eu, &mut na] {
            game.near(&state, now).await;
            assert_eq!(
                game.total.unwrap().0,
                Tally {
                    score: total.0,
                    wins: total.1,
                    losses: total.2
                }
            );
        }
        // No share went below nothing; they add up to the total.
        let shares: Vec<(i32, i32)> = sqlx::query_as(
            "SELECT profile_id, score FROM scores WHERE category = 0 ORDER BY profile_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(shares.iter().all(|&(_, score)| score >= 0), "{shares:?}");
        assert_eq!(shares.iter().map(|&(_, score)| score).sum::<i32>(), total.0);
    }

    #[sqlx::test]
    async fn a_game_whose_day_starts_later_counts_from_its_new_day(pool: PgPool) {
        let state = GamestatsState::with_pool(pool.clone());
        let mut na = Game::new(7, 2, 1);
        let board = async |now| {
            let reply = get_at(
                &state,
                8,
                &get_body(1, MODE_TOPLIST, &[DESCENDING, 10, 1]),
                now,
            )
            .await
            .unwrap();
            read_list(&reply).2
        };
        // 17 November, 20:00 UTC: one game on the US clock's 17th.
        na.log_in(&state, at(2026, 11, 17, 20)).await;
        na.play(&state, true, 2, at(2026, 11, 17, 20)).await;
        assert_eq!(board(at(2026, 11, 17, 21)).await, [(1, 7, 12)]);
        // 02:00 UTC: the boards' day is the 18th; the game's clock still
        // says the 17th, and it is shown the 18th's board as its own day.
        na.near(&state, at(2026, 11, 18, 2)).await;
        // Not on the 18th's board yet: it keeps its total of 12.
        assert_eq!(na.total.unwrap().0.score, 12);
        // Its provisional loss takes 5 points the 18th does not hold; the
        // result gives them back: 3-4 lost, 4 points.
        na.play(&state, false, 3, at(2026, 11, 18, 2)).await;
        assert_eq!(board(at(2026, 11, 18, 3)).await, [(1, 7, 4)]);
        // 09:00 UTC: the game's clock begins the 18th and it starts afresh
        // from this game; the board's day keeps both of its games.
        na.play(&state, true, 1, at(2026, 11, 18, 9)).await;
        assert_eq!(board(at(2026, 11, 18, 10)).await, [(1, 7, 15)]);
        // The 17th's row was kept.
        let past: Vec<(Date, i32)> =
            sqlx::query_as("SELECT first_day, score FROM past_scores WHERE category = 1")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(past, [(date(2026, 11, 17), 12)]);
    }

    #[sqlx::test]
    async fn every_game_puts_and_reads_the_two_boards(pool: PgPool) {
        let state = GamestatsState::with_pool(pool.clone());
        let now = at(2026, 11, 13, 12);
        // A European game of the alternate country group: its season (3)
        // and day (4) are the main boards; its friends board (2) gets the
        // same games and counts nothing more.
        let mut eu = Game::new(7, 4, 3);
        eu.log_in(&state, now).await;
        eu.play(&state, true, 4, now).await;
        let mut friends = Game::new(7, 4, EU_FRIENDS);
        friends.total = eu.total;
        friends.put(&state, now).await;
        let mut daily = Game::new(7, 4, 4);
        daily.log_in(&state, now).await;
        daily.play(&state, true, 4, now).await;
        let rows: Vec<(i32, i32)> =
            sqlx::query_as("SELECT category, score FROM scores ORDER BY category")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows, [(SEASON, 14), (DAILY, 14)]);
        // A US game reads it on the season's board, the EU friends board too.
        let toplist = get_at(
            &state,
            8,
            &get_body(0, MODE_TOPLIST, &[DESCENDING, 10, 1]),
            now,
        )
        .await
        .unwrap();
        assert_eq!(read_list(&toplist).2, [(1, 7, 14)]);
        let friends = get_at(
            &state,
            8,
            &get_body(EU_FRIENDS, MODE_FRIENDS, &friends_params(10, &[7])),
            now,
        )
        .await
        .unwrap();
        assert_eq!(read_list(&friends).2, [(1, 7, 14)]);
    }

    #[sqlx::test]
    async fn a_game_is_shown_rows_by_its_own_clock_and_range(pool: PgPool) {
        let state = GamestatsState::with_pool(pool.clone());
        // 20:00 UTC on the 17th: the 18th on a Japanese game's clock.
        let now = at(2026, 11, 17, 20);
        let mut jp = Game::new(7, 1, 0);
        jp.total = Some((
            Tally {
                score: 0,
                wins: 0,
                losses: 0,
            },
            jp.today(now),
        ));
        jp.put(&state, now).await;
        sqlx::query("UPDATE scores SET score = 123456")
            .execute(&pool)
            .await
            .unwrap();
        jp.near(&state, now).await;
        let (total, on) = jp.total.unwrap();
        assert_eq!((total.score, on), (MAX_SCORE_JP_KR, date(2026, 11, 18)));
        // What it was shown is what its game holds now.
        let held: (i32, Date) =
            sqlx::query_as("SELECT held_score, held_on FROM scores WHERE profile_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(held, (MAX_SCORE_JP_KR, date(2026, 11, 18)));
        // The row itself is dated by UTC.
        let userdata: Vec<u8> = sqlx::query_scalar("SELECT userdata FROM scores")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(Submission::parse(&userdata).date, Some(date(2026, 11, 17)));
    }

    #[sqlx::test]
    async fn a_new_mii_changes_the_row_and_nothing_else(pool: PgPool) {
        let state = GamestatsState::with_pool(pool.clone());
        let now = at(2026, 11, 13, 12);
        let mut game = Game::new(7, 4, 0);
        game.log_in(&state, now).await;
        game.play(&state, true, 2, now).await;
        let row = async || -> (i32, Vec<u8>, OffsetDateTime) {
            sqlx::query_as("SELECT score, userdata, updated_at FROM scores")
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        let (_, _, updated_at) = row().await;
        // At its next login the game puts its total unchanged under its new
        // Mii (`PostResetMyPlayerStats(0, 1)`).
        game.mii = "Anosch";
        game.near(&state, now).await;
        game.put(&state, now + Duration::HOUR).await;
        let (score, userdata, after) = row().await;
        assert_eq!(score, 12);
        assert_eq!(Submission::parse(&userdata).name.as_deref(), Some("Anosch"));
        assert!(submission::signed(&userdata));
        assert_eq!(after, updated_at);
    }

    #[test]
    fn a_share_below_nothing_takes_the_rest_from_the_others() {
        let mut own = Tally {
            score: -7,
            wins: 1,
            losses: -1,
        };
        let mut others = [
            Tally {
                score: 3,
                wins: 0,
                losses: 0,
            },
            Tally {
                score: 5,
                wins: 2,
                losses: 4,
            },
        ];
        assert_eq!(cover(&mut own, &mut others), Tally::default());
        assert_eq!(
            own,
            Tally {
                score: 0,
                wins: 1,
                losses: 0
            }
        );
        assert_eq!(
            others,
            [
                Tally {
                    score: 1,
                    wins: 0,
                    losses: 0
                },
                Tally {
                    score: 0,
                    wins: 2,
                    losses: 3
                }
            ]
        );
        // What nobody has is owed.
        let mut own = Tally {
            score: -9,
            wins: 0,
            losses: 0,
        };
        let mut others = [Tally {
            score: 4,
            wins: 0,
            losses: 0,
        }];
        assert_eq!(
            cover(&mut own, &mut others),
            Tally {
                score: 5,
                wins: 0,
                losses: 0
            }
        );
        assert_eq!((own.score, others[0].score), (0, 0));
    }
}

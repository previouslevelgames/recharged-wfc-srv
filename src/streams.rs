//! Live streams of online games (docs/reports.md, "Live streams"). While a
//! game the server keeps replays of is played ([`crate::replays`]), the
//! Recharged games send it their steps as each becomes final: the same bytes
//! as the game's replay stream, from its start. Both sides send; the two are
//! the same but for one byte of the setup (which machine was local), so the
//! server keeps one buffer per game and takes each byte from whichever side
//! sends it first. Admins watch from the Matches page: its Watch button hands
//! Recharged a ticket, and Recharged plays the match from the stream, about
//! half a second behind the players.
//!
//! Nothing here touches the match: the games send from a thread of their own,
//! at the lowest priority, and the server only keeps the bytes for a while and
//! passes them on.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use axum::body::Bytes;
use futures_util::{Stream, stream};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::sync::watch;

/// A game's stream at most: an hour of a match of four machines fits.
pub const MAX_GAME_BYTES: usize = 8 * 1024 * 1024;
/// All the games' streams kept at once, at most.
const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;
/// A game's stream stays this long after its last bytes, for a spectator
/// who comes late.
const KEPT_FOR: Duration = Duration::from_secs(30 * 60);
/// A spectator hears from the server at least this often.
pub const KEEPALIVE: Duration = Duration::from_secs(5);
/// How long a watch ticket lasts.
const TICKET_TTL: Duration = Duration::from_secs(4 * 60 * 60);
/// Bytes of the HMAC-SHA256 a ticket keeps.
const TAG_LEN: usize = 16;

/// What a spectator's stream is made of: frames of a kind byte, a
/// little-endian length (u32) and that many bytes.
pub const FRAME_KEEPALIVE: u8 = 0;
/// A game of the match starts (its number, u32 LE); its stream follows from
/// its first byte.
pub const FRAME_GAME: u8 = 1;
/// The game's next bytes.
pub const FRAME_DATA: u8 = 2;
/// The game's stream is complete.
pub const FRAME_END: u8 = 3;

/// The live games and the key of the watch tickets.
pub(crate) struct Streams {
    key: [u8; 32],
    games: Mutex<HashMap<(i64, i32), Arc<LiveGame>>>,
    total: AtomicUsize,
    /// Bumped whenever a game starts, for spectators waiting for one.
    started: watch::Sender<u64>,
}

/// One game's stream as the server holds it.
pub(crate) struct LiveGame {
    state: Mutex<GameState>,
    changed: watch::Sender<u64>,
}

struct GameState {
    bytes: Vec<u8>,
    complete: bool,
    touched: Instant,
}

/// Why bytes a game sent were not taken.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// They start past what the server has: it has this many.
    Gap(usize),
    /// The game's stream, or all of them together, would be too big.
    TooBig,
}

impl Default for Streams {
    fn default() -> Self {
        Self {
            key: rand::random(),
            games: Mutex::default(),
            total: AtomicUsize::new(0),
            started: watch::Sender::new(0),
        }
    }
}

impl Streams {
    /// Takes `data`, the bytes of game `game` of `match_id` from `offset` on
    /// as one side sent them: what the server has already stays, the rest is
    /// added.
    pub fn feed(
        &self,
        match_id: i64,
        game: i32,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Refused> {
        let live = self.game_or_start(match_id, game);
        let mut state = live.state.lock().unwrap();
        let have = state.bytes.len();
        if offset > have {
            return Err(Refused::Gap(have));
        }
        let end = offset + data.len();
        state.touched = Instant::now();
        if end <= have {
            return Ok(());
        }
        let new = end - have;
        if end > MAX_GAME_BYTES || self.total.load(Ordering::Relaxed) + new > MAX_TOTAL_BYTES {
            return Err(Refused::TooBig);
        }
        state.bytes.extend_from_slice(&data[have - offset..]);
        self.total.fetch_add(new, Ordering::Relaxed);
        drop(state);
        live.changed.send_modify(|count| *count += 1);
        Ok(())
    }

    /// A side's stream of the game ended whole: the game's stream is complete.
    pub fn end(&self, match_id: i64, game: i32) {
        let Some(live) = self.game(match_id, game) else {
            return;
        };
        let mut state = live.state.lock().unwrap();
        state.complete = true;
        state.touched = Instant::now();
        drop(state);
        live.changed.send_modify(|count| *count += 1);
    }

    /// Whether a game of `match_id` streams now or did lately.
    pub fn live(&self, match_id: i64) -> bool {
        self.games
            .lock()
            .unwrap()
            .keys()
            .any(|&(id, _)| id == match_id)
    }

    fn game(&self, match_id: i64, game: i32) -> Option<Arc<LiveGame>> {
        self.games.lock().unwrap().get(&(match_id, game)).cloned()
    }

    /// The latest game of `match_id` the server holds.
    fn latest(&self, match_id: i64) -> Option<i32> {
        self.games
            .lock()
            .unwrap()
            .keys()
            .filter(|&&(id, _)| id == match_id)
            .map(|&(_, game)| game)
            .max()
    }

    fn game_or_start(&self, match_id: i64, game: i32) -> Arc<LiveGame> {
        let mut games = self.games.lock().unwrap();
        if let Some(live) = games.get(&(match_id, game)) {
            return live.clone();
        }
        // Only as a game starts: the games' bytes come many times a second.
        self.forget_stale(&mut games);
        let live = Arc::new(LiveGame {
            state: Mutex::new(GameState {
                bytes: Vec::new(),
                complete: false,
                touched: Instant::now(),
            }),
            changed: watch::Sender::new(0),
        });
        games.insert((match_id, game), live.clone());
        drop(games);
        self.started.send_modify(|count| *count += 1);
        live
    }

    /// Games whose last bytes are older than [`KEPT_FOR`] go.
    fn forget_stale(&self, games: &mut HashMap<(i64, i32), Arc<LiveGame>>) {
        games.retain(|_, live| {
            let state = live.state.lock().unwrap();
            let keep = state.touched.elapsed() < KEPT_FOR;
            if !keep {
                self.total.fetch_sub(state.bytes.len(), Ordering::Relaxed);
            }
            keep
        });
    }

    /// A ticket to watch `match_id` for [`TICKET_TTL`] from `now` (Unix
    /// seconds): the match, the end of its validity and a tag of both, in hex.
    pub fn ticket(&self, match_id: i64, now: i64) -> String {
        let expires = now + TICKET_TTL.as_secs() as i64;
        let mut bytes = Vec::with_capacity(16 + TAG_LEN);
        bytes.extend_from_slice(&match_id.to_le_bytes());
        bytes.extend_from_slice(&expires.to_le_bytes());
        let tag = self.mac(&bytes).finalize().into_bytes();
        bytes.extend_from_slice(&tag[..TAG_LEN]);
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Whether `ticket` lets its holder watch `match_id` at `now`.
    pub fn ticket_valid(&self, ticket: &str, match_id: i64, now: i64) -> bool {
        let Some(bytes) = unhex(ticket).filter(|bytes| bytes.len() == 16 + TAG_LEN) else {
            return false;
        };
        let id = i64::from_le_bytes(bytes[..8].try_into().unwrap());
        let expires = i64::from_le_bytes(bytes[8..16].try_into().unwrap());
        id == match_id
            && now <= expires
            && self
                .mac(&bytes[..16])
                .verify_truncated_left(&bytes[16..])
                .is_ok()
    }

    fn mac(&self, payload: &[u8]) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("any key length");
        mac.update(payload);
        mac
    }

    /// What a spectator of `match_id` gets: its latest game from the first
    /// byte, then each byte as it comes, then the next games, until the match
    /// is over ([`FRAME_GAME`] and the others).
    pub fn watch(
        self: Arc<Self>,
        match_id: i64,
        pool: PgPool,
    ) -> impl Stream<Item = Result<Bytes, std::convert::Infallible>> + Send + 'static {
        let watching = Watching {
            streams: self,
            pool,
            match_id,
            game: None,
            sent: 0,
            ended: false,
            last_check: Instant::now(),
        };
        stream::unfold(Some(watching), |watching| async move {
            let mut watching = watching?;
            let frame = watching.next().await;
            frame.map(|frame| (Ok(frame), Some(watching)))
        })
    }
}

/// One spectator's place in the match.
struct Watching {
    streams: Arc<Streams>,
    pool: PgPool,
    match_id: i64,
    /// The game followed; none before the first.
    game: Option<i32>,
    /// Its bytes sent so far.
    sent: usize,
    /// Its end sent: the next game is awaited.
    ended: bool,
    last_check: Instant,
}

impl Watching {
    /// The next frame; none once the match is over.
    async fn next(&mut self) -> Option<Bytes> {
        loop {
            // A game to follow: the latest at first, then the one after.
            let wanted = match self.game {
                None => self.streams.latest(self.match_id),
                Some(game) if self.ended => {
                    self.streams.game(self.match_id, game + 1).map(|_| game + 1)
                }
                Some(game) => Some(game),
            };
            let Some(game) = wanted else {
                let mut started = self.streams.started.subscribe();
                // Checked again after subscribing: a game that started in
                // between is not missed.
                let next = match self.game {
                    None => self.streams.latest(self.match_id),
                    Some(game) => self.streams.game(self.match_id, game + 1).map(|_| game + 1),
                };
                if next.is_some() {
                    continue;
                }
                if self.match_over().await {
                    return None;
                }
                if tokio::time::timeout(KEEPALIVE, started.changed())
                    .await
                    .is_err()
                {
                    return Some(frame(FRAME_KEEPALIVE, &[]));
                }
                continue;
            };
            if self.game != Some(game) {
                self.game = Some(game);
                self.sent = 0;
                self.ended = false;
                return Some(frame(FRAME_GAME, &(game as u32).to_le_bytes()));
            }
            let live = self.streams.game(self.match_id, game)?;
            // Subscribed before looking, so that nothing that comes in between
            // is missed: the game's bytes, or a later game.
            let mut changed = live.changed.subscribe();
            let mut started = self.streams.started.subscribe();
            let (data, complete) = {
                let state = live.state.lock().unwrap();
                (
                    state.bytes.get(self.sent..).unwrap_or_default().to_vec(),
                    state.complete,
                )
            };
            if !data.is_empty() {
                self.sent += data.len();
                return Some(frame(FRAME_DATA, &data));
            }
            // Complete, or a later game began: no side sends more of this one
            // (one that gave up its stream leaves it unfinished).
            if complete || self.streams.game(self.match_id, game + 1).is_some() {
                self.ended = true;
                return Some(frame(FRAME_END, &[]));
            }
            tokio::select! {
                _ = changed.changed() => {}
                _ = started.changed() => {}
                () = tokio::time::sleep(KEEPALIVE) => return Some(frame(FRAME_KEEPALIVE, &[])),
            }
        }
    }

    /// Whether the match is no longer in progress, asked at most every
    /// [`KEEPALIVE`].
    async fn match_over(&mut self) -> bool {
        if self.last_check.elapsed() < KEEPALIVE {
            return false;
        }
        self.last_check = Instant::now();
        let state = sqlx::query_scalar!(
            "SELECT state FROM online_matches WHERE match_id = $1",
            self.match_id
        )
        .fetch_optional(&self.pool)
        .await;
        !matches!(state, Ok(Some(state)) if state == "in_progress")
    }
}

fn frame(kind: u8, payload: &[u8]) -> Bytes {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    Bytes::from(out)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

/// The ticket file the Watch button hands out (Recharged's
/// `src/platform/match_replay_file.h`): a replay file of version 2, whose
/// header says what to watch instead of holding a match.
pub fn ticket_file(match_id: i64, ticket: &str, release: &str) -> Vec<u8> {
    let header =
        serde_json::json!({ "match": match_id, "ticket": ticket, "release": release }).to_string();
    let mut file = b"MSCRPLY\x1a".to_vec();
    file.extend_from_slice(&2u16.to_le_bytes());
    file.extend_from_slice(&(header.len() as u16).to_le_bytes());
    file.extend_from_slice(header.as_bytes());
    file
}

/// The Unix time now, for tickets.
pub fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_holds_for_its_match_until_it_expires() {
        let streams = Streams::default();
        let ticket = streams.ticket(12, 1_800_000_000);
        assert!(streams.ticket_valid(&ticket, 12, 1_800_000_000));
        assert!(streams.ticket_valid(&ticket, 12, 1_800_000_000 + 4 * 3600));
        assert!(!streams.ticket_valid(&ticket, 12, 1_800_000_000 + 4 * 3600 + 1));
        assert!(!streams.ticket_valid(&ticket, 13, 1_800_000_000));
        let mut forged = ticket.clone().into_bytes();
        forged[40] = if forged[40] == b'0' { b'1' } else { b'0' };
        assert!(!streams.ticket_valid(&String::from_utf8(forged).unwrap(), 12, 1_800_000_000));
        assert!(!streams.ticket_valid("zz", 12, 1_800_000_000));
        // Another server's key.
        assert!(!Streams::default().ticket_valid(&ticket, 12, 1_800_000_000));
    }

    #[test]
    fn both_sides_feed_one_stream_by_offset() {
        let streams = Streams::default();
        streams.feed(1, 1, 0, b"abcdef").unwrap();
        // The other side, behind, then ahead.
        streams.feed(1, 1, 0, b"abc").unwrap();
        streams.feed(1, 1, 4, b"efgh").unwrap();
        assert_eq!(streams.feed(1, 1, 12, b"x"), Err(Refused::Gap(8)));
        let live = streams.game(1, 1).unwrap();
        assert_eq!(live.state.lock().unwrap().bytes, b"abcdefgh");
        assert_eq!(streams.total.load(Ordering::Relaxed), 8);
        assert_eq!(
            streams.feed(1, 1, 8, &vec![0; MAX_GAME_BYTES]),
            Err(Refused::TooBig)
        );
        assert!(streams.live(1) && !streams.live(2));
    }

    #[test]
    fn a_ticket_file_is_a_replay_file_of_version_2() {
        let file = ticket_file(7, "00ff", "R4QE");
        assert_eq!(&file[..8], b"MSCRPLY\x1a");
        assert_eq!(u16::from_le_bytes([file[8], file[9]]), 2);
        let size = usize::from(u16::from_le_bytes([file[10], file[11]]));
        let header: serde_json::Value = serde_json::from_slice(&file[12..12 + size]).unwrap();
        assert_eq!(
            header,
            serde_json::json!({"match": 7, "ticket": "00ff", "release": "R4QE"})
        );
    }
}

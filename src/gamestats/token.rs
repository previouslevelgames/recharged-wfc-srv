//! Tokens for the first request of every operation.
//!
//! The client never sends the token back, only SHA1(key || token), so the
//! server keeps what it issued. A token is used once and expires quickly,
//! which also stops a captured request from being replayed.
//!
//! Anyone may ask for a token for any pid, so the store bounds itself: each
//! address gets [`TOKENS_PER_ADDRESS`] a minute, and at most [`MAX_ISSUED`]
//! wait to be used.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv6Addr},
    sync::Mutex,
    time::{Duration, Instant},
};

use rand::distr::{Alphanumeric, SampleString};
use sha1::{Digest, Sha1};
use tracing::warn;

use super::{GAMESTATS_KEY, GamestatsError, hex};

/// The client accepts exactly 32 bytes (`DWCi_EncSessionCompleteCallback`).
const TOKEN_LEN: usize = 32;
/// Request 2 usually follows request 1 at once, but the client sends it from
/// its next ranking update and has no limit of its own; a token that expired
/// meanwhile costs the player the result. This only bounds memory and replays.
const TOKEN_TTL: Duration = Duration::from_secs(10 * 60);
/// Tokens one address may get in [`ADDRESS_WINDOW`]. A game asks for one per
/// leaderboard operation: a few at login, after a match and per leaderboard
/// page. This leaves room for a venue's games behind one address; more is a
/// flood.
const TOKENS_PER_ADDRESS: u32 = 300;
const ADDRESS_WINDOW: Duration = Duration::from_secs(60);
/// Tokens waiting to be used, at most. A game uses its token within a
/// second, so a few hundred wait at a time; this many is a flood of made-up
/// pids, and the store stops growing.
const MAX_ISSUED: usize = 50_000;
/// Expired tokens and windows are dropped at most this often, so that a
/// flood does not walk the whole store on every request.
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);

/// Tokens issued and not used yet, one per pid, and how many each address
/// asked for lately. A plain `std` mutex: it is never held across an
/// `.await`.
pub struct TokenStore {
    inner: Mutex<Inner>,
}

struct Inner {
    issued: HashMap<i32, Issued>,
    /// Per address (an IPv6 address with its /64): when its window began and
    /// how many tokens it asked for since.
    windows: HashMap<IpAddr, (Instant, u32)>,
    swept: Instant,
    /// Whether the store was full at the last refusal, so that the log says
    /// it once.
    full: bool,
}

struct Issued {
    token: String,
    at: Instant,
}

impl Default for TokenStore {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner {
                issued: HashMap::new(),
                windows: HashMap::new(),
                swept: Instant::now(),
                full: false,
            }),
        }
    }
}

impl TokenStore {
    /// Issues a new token to `pid` for a request from `address`, replacing
    /// any previous one.
    pub fn issue(&self, pid: i32, address: IpAddr) -> Result<String, GamestatsError> {
        self.issue_at(pid, address, Instant::now())
    }

    fn issue_at(&self, pid: i32, address: IpAddr, now: Instant) -> Result<String, GamestatsError> {
        let mut inner = self.inner.lock().unwrap();
        inner.sweep(now);

        let (_, asked) = inner.windows.entry(bucket(address)).or_insert((now, 0));
        *asked = asked.saturating_add(1);
        if *asked > TOKENS_PER_ADDRESS {
            if *asked == TOKENS_PER_ADDRESS + 1 {
                warn!(target: "gamestats", %address, "too many token requests: refusing this address for a minute");
            }
            return Err(GamestatsError::TooMany);
        }

        if inner.issued.len() >= MAX_ISSUED && !inner.issued.contains_key(&pid) {
            if !std::mem::replace(&mut inner.full, true) {
                warn!(target: "gamestats", waiting = MAX_ISSUED, "token store full: refusing new pids until tokens expire");
            }
            return Err(GamestatsError::Full);
        }
        inner.full = false;

        let token = Alphanumeric.sample_string(&mut rand::rng(), TOKEN_LEN);
        inner.issued.insert(
            pid,
            Issued {
                token: token.clone(),
                at: now,
            },
        );
        Ok(token)
    }

    /// Checks `hash` against the token issued to `pid` and returns its age.
    /// The token is consumed either way.
    pub fn redeem(&self, pid: i32, hash: &str) -> Result<Duration, GamestatsError> {
        self.redeem_at(pid, hash, Instant::now())
    }

    fn redeem_at(&self, pid: i32, hash: &str, now: Instant) -> Result<Duration, GamestatsError> {
        let issued = self
            .inner
            .lock()
            .unwrap()
            .issued
            .remove(&pid)
            .ok_or(GamestatsError::NoToken)?;
        let age = now.duration_since(issued.at);
        if age >= TOKEN_TTL {
            return Err(GamestatsError::TokenExpired(age));
        }
        if hash != token_hash(&issued.token) {
            return Err(GamestatsError::BadHash);
        }
        Ok(age)
    }
}

impl Inner {
    /// Drops expired tokens and finished windows, at most once per
    /// [`SWEEP_INTERVAL`]. A token that expired meanwhile still fails to
    /// redeem: [`TokenStore::redeem`] checks its age.
    fn sweep(&mut self, now: Instant) {
        if now.duration_since(self.swept) < SWEEP_INTERVAL {
            return;
        }
        self.swept = now;
        self.issued
            .retain(|_, entry| now.duration_since(entry.at) < TOKEN_TTL);
        self.windows
            .retain(|_, (since, _)| now.duration_since(*since) < ADDRESS_WINDOW);
    }
}

/// An IPv6 address counts with its whole /64, which one connection usually
/// has.
fn bucket(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !u128::from(u64::MAX))),
    }
}

/// What the client sends for `token`: lowercase hex of SHA1(key || token).
pub(super) fn token_hash(token: &str) -> String {
    let digest = Sha1::new()
        .chain_update(GAMESTATS_KEY)
        .chain_update(token)
        .finalize();
    hex(&digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 4));

    impl TokenStore {
        fn issue_home(&self, pid: i32) -> String {
            self.issue(pid, HOME).unwrap()
        }
    }

    #[test]
    fn token_hash_matches_sha1sum() {
        // printf 'DbfBialvJznkQWYuOrRaCIAOSONOFRANCESCOEMIPIACELAPIZZA' | sha1sum
        assert_eq!(
            token_hash("CIAOSONOFRANCESCOEMIPIACELAPIZZA"),
            "95c926a493cc57732b2a807b76776852db288909"
        );
    }

    #[test]
    fn issued_tokens_are_32_alphanumeric_characters() {
        let token = TokenStore::default().issue_home(1);
        assert_eq!(token.len(), TOKEN_LEN);
        assert!(token.bytes().all(|byte| byte.is_ascii_alphanumeric()));
    }

    #[test]
    fn a_token_redeems_once() {
        let store = TokenStore::default();
        let hash = token_hash(&store.issue_home(1));
        assert!(store.redeem(1, &hash).is_ok());
        assert!(matches!(
            store.redeem(1, &hash),
            Err(GamestatsError::NoToken)
        ));
    }

    #[test]
    fn a_token_expires() {
        let store = TokenStore::default();
        let hash = token_hash(&store.issue_home(1));
        assert!(matches!(
            store.redeem_at(1, &hash, Instant::now() + TOKEN_TTL),
            Err(GamestatsError::TokenExpired(age)) if age >= TOKEN_TTL
        ));
    }

    #[test]
    fn a_token_belongs_to_its_pid() {
        let store = TokenStore::default();
        let hash = token_hash(&store.issue_home(1));
        assert!(matches!(
            store.redeem(2, &hash),
            Err(GamestatsError::NoToken)
        ));
    }

    #[test]
    fn a_wrong_hash_is_refused_and_burns_the_token() {
        let store = TokenStore::default();
        let hash = token_hash(&store.issue_home(1));
        assert!(matches!(
            store.redeem(1, &token_hash("not the token")),
            Err(GamestatsError::BadHash)
        ));
        assert!(matches!(
            store.redeem(1, &hash),
            Err(GamestatsError::NoToken)
        ));
    }

    #[test]
    fn an_address_gets_so_many_tokens_a_minute() {
        let store = TokenStore::default();
        let start = Instant::now();
        let ask = |pid: i32, address: IpAddr, at: Instant| store.issue_at(pid, address, at);
        for pid in 0..TOKENS_PER_ADDRESS as i32 {
            assert!(ask(pid, HOME, start).is_ok());
        }
        assert!(matches!(
            ask(1_000_000_000, HOME, start),
            Err(GamestatsError::TooMany)
        ));
        // Others are not held up, nor the address a minute later.
        assert!(ask(1_000_000_000, IpAddr::from([198, 51, 100, 7]), start).is_ok());
        assert!(ask(1_000_000_000, HOME, start + ADDRESS_WINDOW).is_ok());
    }

    #[test]
    fn an_ipv6_address_counts_with_its_64() {
        let store = TokenStore::default();
        let now = Instant::now();
        for host in 0..TOKENS_PER_ADDRESS as u16 {
            let address = IpAddr::from([0x2001, 0xdb8, 0, 1, 0, 0, 0, host]);
            assert!(store.issue_at(host.into(), address, now).is_ok());
        }
        let same_64 = IpAddr::from([0x2001, 0xdb8, 0, 1, 0xffff, 0, 0, 0]);
        assert!(matches!(
            store.issue_at(1, same_64, now),
            Err(GamestatsError::TooMany)
        ));
        let next_64 = IpAddr::from([0x2001, 0xdb8, 0, 2, 0, 0, 0, 0]);
        assert!(store.issue_at(1, next_64, now).is_ok());
    }

    #[test]
    fn a_full_store_refuses_new_pids_until_tokens_expire() {
        let store = TokenStore::default();
        let start = Instant::now();
        // Each token from an address of its own, as a spread flood would.
        let address = |n: usize| IpAddr::from((n as u32).to_be_bytes());
        for n in 0..MAX_ISSUED {
            assert!(store.issue_at(n as i32, address(n), start).is_ok());
        }
        let late = MAX_ISSUED as i32;
        assert!(matches!(
            store.issue_at(late, address(MAX_ISSUED), start),
            Err(GamestatsError::Full)
        ));
        // A pid that waits already gets a fresh token, and the old one is gone.
        assert!(store.issue_at(7, HOME, start).is_ok());
        assert_eq!(store.inner.lock().unwrap().issued.len(), MAX_ISSUED);

        let later = start + TOKEN_TTL;
        assert!(store.issue_at(late, address(MAX_ISSUED), later).is_ok());
        assert_eq!(store.inner.lock().unwrap().issued.len(), 1);
    }
}

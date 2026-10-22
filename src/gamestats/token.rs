//! Tokens for the first request of every operation.
//!
//! The client never sends the token back, only SHA1(key || token), so the
//! server keeps what it issued. A token is used once and expires quickly,
//! which also stops a captured request from being replayed.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use rand::distr::{Alphanumeric, SampleString};
use sha1::{Digest, Sha1};

use super::{GAMESTATS_KEY, GamestatsError, hex};

/// The client accepts exactly 32 bytes (`DWCi_EncSessionCompleteCallback`).
const TOKEN_LEN: usize = 32;
/// Request 2 follows request 1 right away; this only bounds memory and
/// replays.
const TOKEN_TTL: Duration = Duration::from_secs(60);

/// Tokens issued and not used yet, one per pid. A plain `std` mutex: it is
/// never held across an `.await`.
#[derive(Default)]
pub struct TokenStore {
    issued: Mutex<HashMap<i32, Issued>>,
}

struct Issued {
    token: String,
    at: Instant,
}

impl TokenStore {
    /// Issues a new token to `pid`, replacing any previous one.
    pub fn issue(&self, pid: i32) -> String {
        let token = Alphanumeric.sample_string(&mut rand::rng(), TOKEN_LEN);
        let now = Instant::now();
        let mut issued = self.issued.lock().unwrap();
        issued.retain(|_, entry| now.duration_since(entry.at) < TOKEN_TTL);
        issued.insert(
            pid,
            Issued {
                token: token.clone(),
                at: now,
            },
        );
        token
    }

    /// Checks `hash` against the token issued to `pid`. The token is consumed
    /// either way.
    pub fn redeem(&self, pid: i32, hash: &str) -> Result<(), GamestatsError> {
        let issued = self
            .issued
            .lock()
            .unwrap()
            .remove(&pid)
            .ok_or(GamestatsError::NoToken)?;
        if issued.at.elapsed() >= TOKEN_TTL {
            return Err(GamestatsError::NoToken);
        }
        if hash != token_hash(&issued.token) {
            return Err(GamestatsError::BadHash);
        }
        Ok(())
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
        let token = TokenStore::default().issue(1);
        assert_eq!(token.len(), TOKEN_LEN);
        assert!(token.bytes().all(|byte| byte.is_ascii_alphanumeric()));
    }

    #[test]
    fn a_token_redeems_once() {
        let store = TokenStore::default();
        let hash = token_hash(&store.issue(1));
        assert!(store.redeem(1, &hash).is_ok());
        assert!(matches!(
            store.redeem(1, &hash),
            Err(GamestatsError::NoToken)
        ));
    }

    #[test]
    fn a_token_belongs_to_its_pid() {
        let store = TokenStore::default();
        let hash = token_hash(&store.issue(1));
        assert!(matches!(
            store.redeem(2, &hash),
            Err(GamestatsError::NoToken)
        ));
    }

    #[test]
    fn a_wrong_hash_is_refused_and_burns_the_token() {
        let store = TokenStore::default();
        let hash = token_hash(&store.issue(1));
        assert!(matches!(
            store.redeem(1, &token_hash("not the token")),
            Err(GamestatsError::BadHash)
        ));
        assert!(matches!(
            store.redeem(1, &hash),
            Err(GamestatsError::NoToken)
        ));
    }
}

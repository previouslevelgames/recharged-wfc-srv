//! Sign-ins in progress, from the launcher's request until it picks up its
//! session (docs/profiles.md, "Signing in"). They live in memory: a restart
//! drops them, and the launcher starts again.
//!
//! The launcher shows a code, and so does the browser page it opens. A
//! sign-in link sent to someone else would sign them in to the sender's
//! launcher; the code is how they can tell.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use rand::distr::{Alphanumeric, SampleString};

use super::discord::Authorization;

/// Time to finish signing in in the browser.
pub const SIGN_IN_TTL: Duration = Duration::from_secs(10 * 60);
/// Sign-ins waiting at once. More are refused until some end or expire.
const MAX_PENDING: usize = 1000;
/// Alphanumeric ids of ~256 bits.
const ID_LEN: usize = 43;
/// Letters and digits nobody mixes up, for the code people compare.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 8;

/// The sign-ins waiting for their browser, by id. A plain `std` mutex: it is
/// never held across an `.await`.
#[derive(Default)]
pub struct SignIns {
    pending: Mutex<HashMap<String, Pending>>,
}

struct Pending {
    code: String,
    started: Instant,
    /// The trip to Discord under way, if the browser went there.
    discord: Option<Authorization>,
    outcome: Option<Outcome>,
}

/// How a sign-in ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The launcher's session.
    SignedIn { token: String, player_id: i64 },
    /// Why not, for the launcher to show.
    Refused(String),
}

/// What the launcher learns when it asks.
#[derive(Debug, PartialEq)]
pub enum Poll {
    Waiting,
    /// Given out once: the sign-in is gone afterwards.
    Ended(Outcome),
    /// Expired, picked up already, or never started.
    Unknown,
}

impl SignIns {
    /// A new sign-in: its id and code. `None` while too many are waiting.
    pub fn start(&self) -> Option<(String, String)> {
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|_, sign_in| sign_in.started.elapsed() < SIGN_IN_TTL);
        if pending.len() >= MAX_PENDING {
            return None;
        }
        let id = Alphanumeric.sample_string(&mut rand::rng(), ID_LEN);
        let code: String = (0..CODE_LEN)
            .map(|_| char::from(CODE_ALPHABET[rand::random_range(0..CODE_ALPHABET.len())]))
            .collect();
        pending.insert(
            id.clone(),
            Pending {
                code: code.clone(),
                started: Instant::now(),
                discord: None,
                outcome: None,
            },
        );
        Some((id, code))
    }

    /// The code of the sign-in `id`, while it waits for the browser.
    pub fn code(&self, id: &str) -> Option<String> {
        self.waiting(id, |sign_in| sign_in.code.clone())
    }

    /// A trip to Discord for the sign-in `id`. It replaces an earlier one,
    /// as when the page is opened twice.
    pub fn authorize(&self, id: &str) -> Option<Authorization> {
        self.waiting(id, |sign_in| {
            let authorization = Authorization::new();
            sign_in.discord = Some(authorization.clone());
            authorization
        })
    }

    /// The sign-in whose trip to Discord has `state`, and that trip, which
    /// can come back only once.
    pub fn take_authorization(&self, state: &str) -> Option<(String, Authorization)> {
        let mut pending = self.pending.lock().unwrap();
        let (id, sign_in) = pending.iter_mut().find(|(_, sign_in)| {
            sign_in
                .discord
                .as_ref()
                .is_some_and(|authorization| authorization.state == state)
        })?;
        if sign_in.started.elapsed() >= SIGN_IN_TTL || sign_in.outcome.is_some() {
            return None;
        }
        Some((id.clone(), sign_in.discord.take()?))
    }

    /// Ends the sign-in `id` with `outcome`. False when it expired or has
    /// ended already.
    pub fn finish(&self, id: &str, outcome: Outcome) -> bool {
        self.waiting(id, |sign_in| sign_in.outcome = Some(outcome))
            .is_some()
    }

    pub fn poll(&self, id: &str) -> Poll {
        let mut pending = self.pending.lock().unwrap();
        let Some(sign_in) = pending.get(id) else {
            return Poll::Unknown;
        };
        if sign_in.started.elapsed() >= SIGN_IN_TTL {
            pending.remove(id);
            return Poll::Unknown;
        }
        match &sign_in.outcome {
            None => Poll::Waiting,
            Some(_) => Poll::Ended(pending.remove(id).unwrap().outcome.unwrap()),
        }
    }

    /// `apply` on the sign-in `id` if it has neither expired nor ended.
    fn waiting<T>(&self, id: &str, apply: impl FnOnce(&mut Pending) -> T) -> Option<T> {
        let mut pending = self.pending.lock().unwrap();
        let sign_in = pending.get_mut(id)?;
        if sign_in.started.elapsed() >= SIGN_IN_TTL || sign_in.outcome.is_some() {
            return None;
        }
        Some(apply(sign_in))
    }
}

/// A code as people read it: `ABCD-EFGH`.
pub fn display_code(code: &str) -> String {
    let (first, second) = code.split_at(code.len() / 2);
    format!("{first}-{second}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sign_in_ends_once() {
        let sign_ins = SignIns::default();
        let (id, code) = sign_ins.start().unwrap();
        assert_eq!(id.len(), ID_LEN);
        assert_eq!(code.len(), CODE_LEN);
        assert!(code.bytes().all(|byte| CODE_ALPHABET.contains(&byte)));
        assert_eq!(sign_ins.code(&id), Some(code));
        assert_eq!(sign_ins.poll(&id), Poll::Waiting);

        let outcome = Outcome::SignedIn {
            token: "token".into(),
            player_id: 7,
        };
        assert!(sign_ins.finish(&id, outcome.clone()));
        assert!(!sign_ins.finish(&id, Outcome::Refused("again".into())));
        assert!(sign_ins.code(&id).is_none());
        assert_eq!(sign_ins.poll(&id), Poll::Ended(outcome));
        assert_eq!(sign_ins.poll(&id), Poll::Unknown);
        assert_eq!(sign_ins.poll("never started"), Poll::Unknown);
    }

    #[test]
    fn a_trip_to_discord_comes_back_once() {
        let sign_ins = SignIns::default();
        let (id, _) = sign_ins.start().unwrap();
        let first = sign_ins.authorize(&id).unwrap();
        let second = sign_ins.authorize(&id).unwrap();
        assert!(sign_ins.take_authorization(&first.state).is_none());
        let (back, authorization) = sign_ins.take_authorization(&second.state).unwrap();
        assert_eq!(back, id);
        assert_eq!(authorization.verifier, second.verifier);
        assert!(sign_ins.take_authorization(&second.state).is_none());
    }

    #[test]
    fn codes_read_in_two_halves() {
        assert_eq!(display_code("ABCDEFGH"), "ABCD-EFGH");
    }
}

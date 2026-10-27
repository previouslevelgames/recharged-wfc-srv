//! Who is signed in. Sessions live in memory: a restart signs everyone out.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use rand::distr::{Alphanumeric, SampleString};

use super::google::Account;

/// How long a sign-in lasts.
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// Alphanumeric session ids of ~256 bits.
const ID_LEN: usize = 43;

/// Signed-in accounts by session id. A plain `std` mutex: it is never held
/// across an `.await`.
#[derive(Default)]
pub struct Sessions {
    open: Mutex<HashMap<String, Session>>,
}

struct Session {
    account: Account,
    since: Instant,
}

impl Sessions {
    /// Opens a session for `account`. Returns its id, for the cookie.
    pub fn open(&self, account: Account) -> String {
        let id = Alphanumeric.sample_string(&mut rand::rng(), ID_LEN);
        let now = Instant::now();
        let mut open = self.open.lock().unwrap();
        open.retain(|_, session| now.duration_since(session.since) < SESSION_TTL);
        open.insert(
            id.clone(),
            Session {
                account,
                since: now,
            },
        );
        id
    }

    /// The account signed in with `id`, unless the session expired.
    pub fn get(&self, id: &str) -> Option<Account> {
        self.open
            .lock()
            .unwrap()
            .get(id)
            .filter(|session| session.since.elapsed() < SESSION_TTL)
            .map(|session| session.account.clone())
    }

    /// Closes the session `id`. Returns whose it was.
    pub fn close(&self, id: &str) -> Option<Account> {
        self.open
            .lock()
            .unwrap()
            .remove(id)
            .map(|session| session.account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Account {
        Account {
            email: "admin@example.com".into(),
            name: None,
            picture: None,
            owner: false,
        }
    }

    #[test]
    fn a_session_lasts_until_it_is_closed() {
        let sessions = Sessions::default();
        let id = sessions.open(account());
        assert_eq!(id.len(), ID_LEN);
        assert_eq!(sessions.get(&id).unwrap().email, "admin@example.com");
        assert!(sessions.get("another id").is_none());
        assert_eq!(sessions.close(&id).unwrap().email, "admin@example.com");
        assert!(sessions.get(&id).is_none());
    }
}

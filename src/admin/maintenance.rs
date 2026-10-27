//! The Maintenance page: operations on the server's data that cannot be
//! undone, so each one asks twice. A first request arms it and answers with
//! what it would delete; only a second one runs it, from the same admin
//! within [`CONFIRM_TTL`], with the first one's token and the operation's
//! phrase typed out ([`Confirmations`]). For now, wiping the leaderboards.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use rand::distr::{Alphanumeric, SampleString};
use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::gpcm::Gpcm;

/// How long an armed operation waits for its confirmation.
const CONFIRM_TTL: Duration = Duration::from_secs(5 * 60);
/// Alphanumeric tokens of ~256 bits, like the session ids.
const TOKEN_LEN: usize = 43;

#[derive(Serialize)]
pub struct Maintenance {
    leaderboards: Leaderboards,
}

/// What wiping the leaderboards deletes, and whom it misses for a while.
#[derive(Serialize)]
pub struct Leaderboards {
    /// Rows of `scores`: every board's standings.
    entries: i64,
    /// Profiles with a row in either table.
    players: i64,
    /// Rows of `past_scores`: how ended seasons and days came out.
    past_entries: i64,
    /// Games logged in now. They keep their totals and put them back after
    /// their next match; at their next login they find no row of their own
    /// and start again from 0 (docs/gamestats2.md, "Seasons").
    online: usize,
}

/// What a wipe deleted.
#[derive(Debug, Serialize)]
pub struct Wiped {
    scores: u64,
    past_scores: u64,
}

pub async fn read(pool: &PgPool, gpcm: &Gpcm) -> Result<Maintenance, sqlx::Error> {
    Ok(Maintenance {
        leaderboards: leaderboards(pool, gpcm).await?,
    })
}

pub async fn leaderboards(pool: &PgPool, gpcm: &Gpcm) -> Result<Leaderboards, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT (SELECT count(*) FROM scores) AS "entries!",
                  (SELECT count(*) FROM (SELECT profile_id FROM scores
                                         UNION SELECT profile_id FROM past_scores) p) AS "players!",
                  (SELECT count(*) FROM past_scores) AS "past_entries!""#
    )
    .fetch_one(pool)
    .await?;
    Ok(Leaderboards {
        entries: row.entries,
        players: row.players,
        past_entries: row.past_entries,
        online: gpcm.presence().len(),
    })
}

/// Deletes every board's scores and every ended season's, together.
pub async fn wipe_leaderboards(pool: &PgPool) -> Result<Wiped, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let scores = sqlx::query!("DELETE FROM scores")
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let past_scores = sqlx::query!("DELETE FROM past_scores")
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(Wiped {
        scores,
        past_scores,
    })
}

/// An operation of the Maintenance page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    WipeLeaderboards,
}

impl Operation {
    /// What the admin types out to confirm it.
    pub fn phrase(self) -> &'static str {
        match self {
            Self::WipeLeaderboards => "wipe all leaderboards",
        }
    }
}

/// The answer to an operation's first ask: what the second one sends back.
#[derive(Serialize)]
pub struct Asked<T> {
    token: String,
    phrase: &'static str,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    /// What the operation would change now.
    impact: T,
}

/// Why a confirmation was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No such token for this admin and operation, or it expired or served
    /// already.
    Expired,
    /// The typed phrase is not the operation's.
    Phrase,
}

/// Operations armed and waiting for their confirmation, by token. A plain
/// `std` mutex: it is never held across an `.await`.
#[derive(Default)]
pub struct Confirmations {
    armed: Mutex<HashMap<String, Armed>>,
}

struct Armed {
    operation: Operation,
    email: String,
    expires: Instant,
}

impl Confirmations {
    /// Arms `operation` for `email`, which would change `impact`.
    pub fn arm<T>(&self, operation: Operation, email: &str, impact: T) -> Asked<T> {
        let token = Alphanumeric.sample_string(&mut rand::rng(), TOKEN_LEN);
        let now = Instant::now();
        let mut armed = self.armed.lock().unwrap();
        armed.retain(|_, armed| now < armed.expires);
        armed.insert(
            token.clone(),
            Armed {
                operation,
                email: email.to_owned(),
                expires: now + CONFIRM_TTL,
            },
        );
        Asked {
            token,
            phrase: operation.phrase(),
            expires_at: OffsetDateTime::now_utc() + CONFIRM_TTL,
            impact,
        }
    }

    /// Whether `token` armed `operation` for `email`, unexpired, and `typed`
    /// is its phrase. A token serves once, whatever the answer.
    pub fn confirm(
        &self,
        operation: Operation,
        email: &str,
        token: &str,
        typed: &str,
    ) -> Result<(), Refusal> {
        let armed = self.armed.lock().unwrap().remove(token);
        match armed {
            Some(armed)
                if armed.operation == operation
                    && armed.email == email
                    && Instant::now() < armed.expires =>
            {
                if typed.trim() == operation.phrase() {
                    Ok(())
                } else {
                    Err(Refusal::Phrase)
                }
            }
            _ => Err(Refusal::Expired),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADMIN: &str = "admin@example.com";
    const PHRASE: &str = "wipe all leaderboards";

    #[test]
    fn a_token_confirms_once_for_its_admin_and_phrase() {
        let confirmations = Confirmations::default();
        let wipe = Operation::WipeLeaderboards;

        let token = confirmations.arm(wipe, ADMIN, ()).token;
        assert_eq!(token.len(), TOKEN_LEN);
        assert_eq!(confirmations.confirm(wipe, ADMIN, &token, PHRASE), Ok(()));
        assert_eq!(
            confirmations.confirm(wipe, ADMIN, &token, PHRASE),
            Err(Refusal::Expired),
            "served already"
        );

        let token = confirmations.arm(wipe, ADMIN, ()).token;
        assert_eq!(
            confirmations.confirm(wipe, "second@example.com", &token, PHRASE),
            Err(Refusal::Expired),
            "another admin's"
        );

        let token = confirmations.arm(wipe, ADMIN, ()).token;
        assert_eq!(
            confirmations.confirm(wipe, ADMIN, &token, "wipe all"),
            Err(Refusal::Phrase)
        );
        assert_eq!(
            confirmations.confirm(wipe, ADMIN, &token, PHRASE),
            Err(Refusal::Expired),
            "a wrong phrase uses the token up"
        );

        assert_eq!(
            confirmations.confirm(wipe, ADMIN, "made-up", PHRASE),
            Err(Refusal::Expired)
        );
    }

    #[test]
    fn a_token_expires() {
        let confirmations = Confirmations::default();
        let wipe = Operation::WipeLeaderboards;
        let token = confirmations.arm(wipe, ADMIN, ()).token;
        confirmations
            .armed
            .lock()
            .unwrap()
            .get_mut(&token)
            .unwrap()
            .expires = Instant::now();
        assert_eq!(
            confirmations.confirm(wipe, ADMIN, &token, PHRASE),
            Err(Refusal::Expired)
        );
    }

    #[sqlx::test]
    async fn a_wipe_empties_every_board_and_past_season_only(pool: PgPool) {
        for (pid, category) in [(1, 0), (1, 1), (2, 0)] {
            sqlx::query!(
                "INSERT INTO scores (profile_id, category, region, score, userdata)
                 VALUES ($1, $2, 4, 10, '')",
                pid,
                category,
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query!(
            "INSERT INTO past_scores
                 (profile_id, category, first_day, region, score, userdata, updated_at)
             VALUES (3, 0, '2026-10-15', 4, 20, '', now())"
        )
        .execute(&pool)
        .await
        .unwrap();
        let profiles = || async {
            sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM profiles"#)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        let before = profiles().await;

        let gpcm = Gpcm::new(pool.clone(), Default::default(), Default::default());
        let boards = leaderboards(&pool, &gpcm).await.unwrap();
        assert_eq!(
            (boards.entries, boards.players, boards.past_entries),
            (3, 3, 1)
        );

        let wiped = wipe_leaderboards(&pool).await.unwrap();
        assert_eq!((wiped.scores, wiped.past_scores), (3, 1));
        let boards = leaderboards(&pool, &gpcm).await.unwrap();
        assert_eq!(
            (boards.entries, boards.players, boards.past_entries),
            (0, 0, 0)
        );
        assert_eq!(profiles().await, before, "profiles stay");
    }
}

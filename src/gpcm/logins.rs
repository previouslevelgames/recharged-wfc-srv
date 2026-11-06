//! The logins of the pool (docs/nodes.md): each node writes a row of each of
//! its GPCM logins to `logins`, and its HTTP (the profile service,
//! gamestats2, the reports, `/admin`) asks after the logins of every node:
//! its own sessions first, then the other nodes' rows.
//!
//! A node keeps its rows like its sessions. After a change it writes the rows
//! that changed since it last wrote, and removes those of logins that ended;
//! a login is written before GPCM answers it, as the port shows its login
//! ticket to the profile service at once. Only one write runs at a time, and
//! it writes the sessions as they are then, so the rows end up as the
//! sessions are, whatever the order of the changes. The rows go with the
//! node's lease: a node the others took for dead draws a new lease and
//! writes them all again, and the rows of a dead node are ignored, then
//! removed with its lease.
//!
//! Only a node with a lease is in the pool. Without one (a server on its own,
//! as in tests) it writes no rows and reads none.

use std::{collections::HashMap, net::IpAddr, time::Duration};

use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tracing::warn;

use super::{Gpcm, LoginAddresses, Matchmaking, Presence, Session};
use crate::node;

/// How long a login waits for its row before GPCM answers it anyway: the
/// row follows when the database answers.
const WRITE_WITHIN: Duration = Duration::from_secs(2);
/// After a write that failed, the next try.
const RETRY: Duration = Duration::from_secs(1);

pub(super) fn ticket_hash(login_ticket: &str) -> [u8; 32] {
    Sha256::digest(login_ticket).into()
}

/// A login's row.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    connection: u64,
    nas_address: IpAddr,
    ticket: [u8; 32],
    presence: Presence,
    off_boards: bool,
}

impl Session {
    fn row(&self) -> Row {
        Row {
            connection: self.connection,
            nas_address: self.nas_address,
            ticket: self.ticket,
            presence: self.presence(),
            off_boards: self.off_boards,
        }
    }
}

/// What the node last wrote: under which lease, and the rows.
#[derive(Default)]
pub(super) struct Written {
    lease: Option<i64>,
    rows: HashMap<i32, Row>,
}

/// Another node's login.
struct Other {
    profile_id: i32,
    addresses: LoginAddresses,
    ticket: Vec<u8>,
    presence: Presence,
}

impl Matchmaking {
    fn as_str(self) -> &'static str {
        match self {
            Self::Anybody => "anybody",
            Self::Friends => "friends",
        }
    }

    fn from_str(text: &str) -> Option<Self> {
        match text {
            "anybody" => Some(Self::Anybody),
            "friends" => Some(Self::Friends),
            _ => None,
        }
    }
}

impl Gpcm {
    /// Who is logged in to the pool, by profile: this node's logins, and
    /// the other nodes' for the profiles not logged in here.
    pub async fn presence(&self) -> Result<HashMap<i32, Presence>, sqlx::Error> {
        let mut presence: HashMap<i32, Presence> = self
            .others(None)
            .await?
            .into_iter()
            .map(|other| (other.profile_id, other.presence))
            .collect();
        presence.extend(self.local_presence());
        Ok(presence)
    }

    /// Whether `profile_id` is logged in anywhere in the pool.
    pub async fn is_logged_in(&self, profile_id: i32) -> Result<bool, sqlx::Error> {
        if self.sessions.lock().unwrap().contains_key(&profile_id) {
            return Ok(true);
        }
        Ok(!self.others(Some(profile_id)).await?.is_empty())
    }

    /// Whether `profile_id` is logged in with the login ticket
    /// `login_ticket`, on any node.
    pub async fn logged_in_with(
        &self,
        profile_id: i32,
        login_ticket: &str,
    ) -> Result<bool, sqlx::Error> {
        let ticket = ticket_hash(login_ticket);
        let here = self
            .sessions
            .lock()
            .unwrap()
            .get(&profile_id)
            .is_some_and(|session| session.ticket == ticket);
        if here {
            return Ok(true);
        }
        let others = self.others(Some(profile_id)).await?;
        Ok(others.iter().any(|other| other.ticket == ticket))
    }

    /// Whether a request from `address` may be one of `profile_id`'s login's
    /// own ([`LoginAddresses::contains`]), on any node.
    pub async fn logged_in_at(
        &self,
        profile_id: i32,
        address: IpAddr,
    ) -> Result<bool, sqlx::Error> {
        let here = self
            .sessions
            .lock()
            .unwrap()
            .get(&profile_id)
            .is_some_and(|session| session.addresses().contains(address));
        if here {
            return Ok(true);
        }
        let others = self.others(Some(profile_id)).await?;
        Ok(others.iter().any(|other| other.addresses.contains(address)))
    }

    /// Where `profile_id`'s logins came from, this node's first: none if it
    /// is not logged in. Two only while a login on one node replaces one on
    /// another.
    pub async fn login_addresses(
        &self,
        profile_id: i32,
    ) -> Result<Vec<LoginAddresses>, sqlx::Error> {
        let here = self
            .sessions
            .lock()
            .unwrap()
            .get(&profile_id)
            .map(Session::addresses);
        let others = self.others(Some(profile_id)).await?;
        Ok(here
            .into_iter()
            .chain(others.into_iter().map(|other| other.addresses))
            .collect())
    }

    /// Keeps the puts of these profiles' logins off the boards from now on,
    /// on every node: an admin deletes their scores, and a game puts its
    /// whole running total, so its next match would put the old one back.
    /// Its next login reads no row of its own and starts again from 0
    /// (docs/gamestats2.md, "Seasons"). Returns the profiles it flagged now.
    pub async fn keep_off_boards(&self, profiles: &[i32]) -> Result<Vec<i32>, sqlx::Error> {
        let mut flagged: Vec<i32> = {
            let mut sessions = self.sessions.lock().unwrap();
            profiles
                .iter()
                .copied()
                .filter(|profile_id| {
                    sessions
                        .get_mut(profile_id)
                        .is_some_and(|session| !std::mem::replace(&mut session.off_boards, true))
                })
                .collect()
        };
        self.changed.notify_one();
        if self.node.lease().is_some() {
            let elsewhere = sqlx::query_scalar!(
                "UPDATE logins SET off_boards = true
                 WHERE profile_id = ANY($1) AND NOT off_boards
                 RETURNING profile_id",
                profiles,
            )
            .fetch_all(&self.pool)
            .await?;
            for profile_id in elsewhere {
                if !flagged.contains(&profile_id) {
                    flagged.push(profile_id);
                }
            }
        }
        Ok(flagged)
    }

    /// Undoes [`Self::keep_off_boards`] for `profiles` (a delete that failed).
    pub async fn back_on_boards(&self, profiles: &[i32]) -> Result<(), sqlx::Error> {
        {
            let mut sessions = self.sessions.lock().unwrap();
            for profile_id in profiles {
                if let Some(session) = sessions.get_mut(profile_id) {
                    session.off_boards = false;
                }
            }
        }
        self.changed.notify_one();
        if self.node.lease().is_some() {
            sqlx::query!(
                "UPDATE logins SET off_boards = false WHERE profile_id = ANY($1)",
                profiles,
            )
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Whether `profile_id`'s login keeps its puts off the boards. Another
    /// node's admin may have flagged this node's login, so its row counts
    /// too.
    pub async fn off_boards(&self, profile_id: i32) -> Result<bool, sqlx::Error> {
        let here = self
            .sessions
            .lock()
            .unwrap()
            .get(&profile_id)
            .is_some_and(|session| session.off_boards);
        if here || self.node.lease().is_none() {
            return Ok(here);
        }
        sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM logins JOIN nodes ON nodes.id = logins.node
                              WHERE profile_id = $1 AND off_boards
                                AND heartbeat_at > now() - make_interval(secs => $2))
               AS "off_boards!""#,
            profile_id,
            node::DEAD_AFTER.as_secs_f64(),
        )
        .fetch_one(&self.pool)
        .await
    }

    /// The logins of the other live nodes: all, or `profile_id`'s.
    async fn others(&self, profile_id: Option<i32>) -> Result<Vec<Other>, sqlx::Error> {
        let Some(lease) = self.node.lease() else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query!(
            "SELECT profile_id, address, nas_address, ticket, logged_in_at, status, status_since,
                    matchmaking, room_players, friend_requests, mutual_friends
             FROM logins JOIN nodes ON nodes.id = logins.node
             WHERE node <> $1 AND heartbeat_at > now() - make_interval(secs => $2)
               AND ($3::INTEGER IS NULL OR profile_id = $3)
             ORDER BY logged_in_at",
            lease,
            node::DEAD_AFTER.as_secs_f64(),
            profile_id,
        )
        .fetch_all(&self.pool)
        .await?;
        let count = |n: i32| usize::try_from(n).unwrap_or_default();
        let small = |n: Option<i16>| n.and_then(|n| u8::try_from(n).ok());
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let (gpcm, nas) = (row.address.parse().ok()?, row.nas_address.parse().ok()?);
                Some(Other {
                    profile_id: row.profile_id,
                    addresses: LoginAddresses { gpcm, nas },
                    ticket: row.ticket,
                    presence: Presence {
                        address: gpcm,
                        logged_in_at: row.logged_in_at,
                        status: small(row.status),
                        status_since: row.status_since,
                        matchmaking: row.matchmaking.as_deref().and_then(Matchmaking::from_str),
                        room_players: small(row.room_players),
                        friend_requests: count(row.friend_requests),
                        mutual_friends: count(row.mutual_friends),
                    },
                })
            })
            .collect())
    }

    /// Writes the rows of the sessions that changed since the last write,
    /// and removes those of logins that ended.
    pub(crate) async fn publish(&self) -> Result<(), sqlx::Error> {
        let mut written = self.written.lock().await;
        let lease = self.node.lease();
        if written.lease != lease {
            // The rows of an earlier lease went with it.
            written.rows.clear();
            written.lease = lease;
        }
        let Some(lease) = lease else {
            return Ok(());
        };
        let rows: HashMap<i32, Row> = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .map(|(&profile_id, session)| (profile_id, session.row()))
            .collect();
        let changed: Vec<(i32, &Row)> = rows
            .iter()
            .filter(|(profile_id, row)| written.rows.get(profile_id) != Some(row))
            .map(|(&profile_id, row)| (profile_id, row))
            .collect();
        let ended: Vec<i32> = written
            .rows
            .keys()
            .filter(|profile_id| !rows.contains_key(profile_id))
            .copied()
            .collect();
        if !changed.is_empty() {
            write(&self.pool, lease, &changed).await?;
        }
        if !ended.is_empty() {
            sqlx::query!(
                "DELETE FROM logins WHERE node = $1 AND profile_id = ANY($2)",
                lease,
                &ended,
            )
            .execute(&self.pool)
            .await?;
        }
        written.rows = rows;
        Ok(())
    }

    /// [`Self::publish`] at once, for a login: a failure is logged and left
    /// to [`Self::keep_published`].
    pub(super) async fn publish_now(&self) {
        let failure = match tokio::time::timeout(WRITE_WITHIN, self.publish()).await {
            Ok(Ok(())) => return,
            Ok(Err(error)) => error.to_string(),
            Err(_) => "no answer from the database".to_owned(),
        };
        warn!(target: "gpcm", error = failure, "login not written for the other nodes yet");
        self.changed.notify_one();
    }

    /// Keeps the rows like the sessions for as long as GPCM runs: after each
    /// change, and at each heartbeat for a lease drawn again. A write that
    /// fails is tried again a second later.
    pub(super) async fn keep_published(&self) -> std::io::Result<()> {
        let mut wait = node::HEARTBEAT;
        loop {
            tokio::select! {
                () = self.changed.notified() => {}
                () = tokio::time::sleep(wait) => {}
            }
            wait = match self.publish().await {
                Ok(()) => node::HEARTBEAT,
                Err(error) => {
                    warn!(target: "gpcm", %error, "logins not written for the other nodes");
                    RETRY
                }
            };
        }
    }
}

impl Session {
    fn addresses(&self) -> LoginAddresses {
        LoginAddresses {
            gpcm: self.address,
            nas: self.nas_address,
        }
    }
}

/// Writes `rows` of the node `lease`, new or changed.
async fn write(pool: &sqlx::PgPool, lease: i64, rows: &[(i32, &Row)]) -> Result<(), sqlx::Error> {
    fn column<T>(rows: &[(i32, &Row)], value: impl Fn(&Row) -> T) -> Vec<T> {
        rows.iter().map(|(_, row)| value(row)).collect()
    }
    let count = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
    let profile_ids: Vec<i32> = rows.iter().map(|(profile_id, _)| *profile_id).collect();
    let connections = column(rows, |row| row.connection as i64);
    let addresses = column(rows, |row| row.presence.address.to_string());
    let nas_addresses = column(rows, |row| row.nas_address.to_string());
    let tickets = column(rows, |row| row.ticket.to_vec());
    let logged_in_at = column(rows, |row| row.presence.logged_in_at);
    let statuses = column(rows, |row| row.presence.status.map(i16::from));
    let status_since = column(rows, |row| row.presence.status_since);
    let matchmaking = column(rows, |row| {
        row.presence
            .matchmaking
            .map(|matchmaking| matchmaking.as_str().to_owned())
    });
    let room_players = column(rows, |row| row.presence.room_players.map(i16::from));
    let friend_requests = column(rows, |row| count(row.presence.friend_requests));
    let mutual_friends = column(rows, |row| count(row.presence.mutual_friends));
    let off_boards = column(rows, |row| row.off_boards);
    // Another node's admin may have flagged a login off the boards in its
    // row: the flag stays for that login.
    sqlx::query!(
        "INSERT INTO logins (node, profile_id, connection, address, nas_address, ticket,
                             logged_in_at, status, status_since, matchmaking, room_players,
                             friend_requests, mutual_friends, off_boards)
         SELECT $1::BIGINT, * FROM unnest($2::INTEGER[], $3::BIGINT[], $4::TEXT[], $5::TEXT[],
                                          $6::BYTEA[], $7::TIMESTAMPTZ[], $8::SMALLINT[],
                                          $9::TIMESTAMPTZ[], $10::TEXT[], $11::SMALLINT[],
                                          $12::INTEGER[], $13::INTEGER[], $14::BOOLEAN[])
         ON CONFLICT (node, profile_id) DO UPDATE SET
             connection = EXCLUDED.connection, address = EXCLUDED.address,
             nas_address = EXCLUDED.nas_address, ticket = EXCLUDED.ticket,
             logged_in_at = EXCLUDED.logged_in_at, status = EXCLUDED.status,
             status_since = EXCLUDED.status_since, matchmaking = EXCLUDED.matchmaking,
             room_players = EXCLUDED.room_players, friend_requests = EXCLUDED.friend_requests,
             mutual_friends = EXCLUDED.mutual_friends,
             off_boards = EXCLUDED.off_boards
                 OR (logins.off_boards AND logins.connection = EXCLUDED.connection)",
        lease,
        &profile_ids,
        &connections,
        &addresses,
        &nas_addresses,
        &tickets,
        &logged_in_at,
        // The nullable columns' types are given: the macro takes arrays as
        // of values that are never null.
        &statuses as &[Option<i16>],
        &status_since as &[Option<OffsetDateTime>],
        &matchmaking as &[Option<String>],
        &room_players as &[Option<i16>],
        &friend_requests,
        &mutual_friends,
        &off_boards,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlx::PgPool;

    use super::*;
    use crate::node::{Load, Node};

    /// GPCM on the node `name` of a pool, with its lease.
    async fn joined(pool: &PgPool, name: &str) -> Gpcm {
        let node = Arc::new(Node::new(name.to_owned()));
        node.register(pool, Load::default()).await.unwrap();
        Gpcm::new(pool.clone(), Arc::default(), Arc::default(), node)
    }

    async fn rows(pool: &PgPool) -> Vec<(i32, Option<i16>)> {
        sqlx::query_as("SELECT profile_id, status FROM logins ORDER BY profile_id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    const HOME: [u8; 4] = [198, 51, 100, 1];
    const NAS: [u8; 4] = [198, 51, 100, 2];

    #[sqlx::test]
    async fn a_login_on_one_node_is_seen_on_another(pool: PgPool) {
        let (first, second) = (joined(&pool, "srv-1").await, joined(&pool, "srv-2").await);
        second.fake_login_via(7, HOME.into(), NAS.into());
        second.fake_status(7, 3);
        second.publish().await.unwrap();

        let presence = first.presence().await.unwrap();
        let seen = &presence[&7];
        assert_eq!(seen.address, IpAddr::from(HOME));
        assert_eq!(
            (seen.status, seen.matchmaking),
            (Some(3), Some(Matchmaking::Anybody))
        );
        // The database keeps microseconds.
        let here = &second.local_presence()[&7];
        assert!((seen.logged_in_at - here.logged_in_at).abs() < time::Duration::microseconds(1));
        assert_eq!(seen.friend_requests, here.friend_requests);
        assert!(first.local_presence().is_empty());
        assert!(first.is_logged_in(7).await.unwrap());
        assert!(!first.is_logged_in(8).await.unwrap());
        assert!(first.logged_in_with(7, "ticket-7").await.unwrap());
        assert!(!first.logged_in_with(7, "ticket-8").await.unwrap());
        for (address, counts) in [(HOME, true), (NAS, true), ([198, 51, 100, 3], false)] {
            assert_eq!(first.logged_in_at(7, address.into()).await.unwrap(), counts);
        }
        assert_eq!(
            first.login_addresses(7).await.unwrap(),
            [LoginAddresses {
                gpcm: HOME.into(),
                nas: NAS.into()
            }]
        );

        // Changes are written; a write without any writes nothing.
        second.fake_status(7, 2);
        second.publish().await.unwrap();
        assert_eq!(rows(&pool).await, [(7, Some(2))]);
        assert_eq!(first.presence().await.unwrap()[&7].status, Some(2));
        sqlx::query("UPDATE logins SET status = 5")
            .execute(&pool)
            .await
            .unwrap();
        second.publish().await.unwrap();
        assert_eq!(rows(&pool).await, [(7, Some(5))]);

        // The logout removes the row.
        second.fake_logout(7);
        second.publish().await.unwrap();
        assert!(rows(&pool).await.is_empty());
        assert!(first.presence().await.unwrap().is_empty());
        assert!(!first.logged_in_with(7, "ticket-7").await.unwrap());
    }

    #[sqlx::test]
    async fn the_logins_go_with_their_nodes_lease(pool: PgPool) {
        let (first, second) = (joined(&pool, "srv-1").await, joined(&pool, "srv-2").await);
        second.fake_login(7, HOME.into());
        second.publish().await.unwrap();

        // Not heard from: the node is dead to the others.
        sqlx::query(
            "UPDATE nodes SET heartbeat_at = now() - interval '1 minute' WHERE name = 'srv-2'",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(!first.is_logged_in(7).await.unwrap());
        // The next renewal of the others removes it, its rows with it.
        first.node.renew(&pool, Load::default()).await.unwrap();
        assert!(rows(&pool).await.is_empty());

        // It draws a new lease and writes its rows again.
        second.node.renew(&pool, Load::default()).await.unwrap();
        second.publish().await.unwrap();
        assert_eq!(rows(&pool).await, [(7, None)]);
        assert!(first.is_logged_in(7).await.unwrap());

        // A node that leaves takes its rows.
        second.node.leave(&pool).await.unwrap();
        assert!(rows(&pool).await.is_empty());
        second.publish().await.unwrap();
        assert!(rows(&pool).await.is_empty());
    }

    #[sqlx::test]
    async fn a_login_is_kept_off_the_boards_on_every_node(pool: PgPool) {
        let (first, second) = (joined(&pool, "srv-1").await, joined(&pool, "srv-2").await);
        second.fake_login(7, HOME.into());
        first.fake_login(8, HOME.into());
        second.publish().await.unwrap();
        first.publish().await.unwrap();

        // An admin on the first node deletes both profiles' scores.
        let mut flagged = first.keep_off_boards(&[7, 8, 9]).await.unwrap();
        flagged.sort();
        assert_eq!(flagged, [7, 8]);
        assert_eq!(
            first.keep_off_boards(&[7, 8]).await.unwrap(),
            Vec::<i32>::new()
        );
        for gpcm in [&first, &second] {
            assert!(gpcm.off_boards(7).await.unwrap());
            assert!(gpcm.off_boards(8).await.unwrap());
        }
        // The second node's next write keeps the flag of its login.
        second.fake_status(7, 1);
        second.publish().await.unwrap();
        assert!(first.off_boards(7).await.unwrap());

        // A delete that failed puts them back.
        first.back_on_boards(&[7, 8]).await.unwrap();
        first.publish().await.unwrap();
        for gpcm in [&first, &second] {
            assert!(!gpcm.off_boards(7).await.unwrap());
            assert!(!gpcm.off_boards(8).await.unwrap());
        }

        // The profile's next login, a new connection, starts on the boards.
        first.keep_off_boards(&[7]).await.unwrap();
        second.fake_login(7, HOME.into());
        second
            .sessions
            .lock()
            .unwrap()
            .get_mut(&7)
            .unwrap()
            .connection = 2;
        second.publish().await.unwrap();
        assert!(!first.off_boards(7).await.unwrap());
    }

    #[sqlx::test]
    async fn a_node_without_a_lease_is_a_pool_of_its_own(pool: PgPool) {
        let pooled = joined(&pool, "srv-1").await;
        pooled.fake_login(7, HOME.into());
        pooled.publish().await.unwrap();

        let alone = Gpcm::new(pool.clone(), Arc::default(), Arc::default(), Arc::default());
        alone.fake_login(8, HOME.into());
        alone.publish().await.unwrap();
        assert_eq!(rows(&pool).await, [(7, None)]);
        assert_eq!(
            alone.presence().await.unwrap().keys().collect::<Vec<_>>(),
            [&8]
        );
        assert!(!alone.logged_in_with(7, "ticket-7").await.unwrap());
        assert!(!pooled.is_logged_in(8).await.unwrap());
    }
}

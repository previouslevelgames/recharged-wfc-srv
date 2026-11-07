//! The NAT relay's matches in the database (`relay_sessions`): which pairs of
//! games it carried, why, how long and how much, and the profiles that
//! played. The relay hands each start and end to a [`Recorder`], whose task
//! ([`run`]) writes them, so NAT negotiation never waits for the database.
//! `/admin` shows them on the Matches page, a match's page and a player's
//! page. Rows go after [`KEPT_DAYS`].

use std::{collections::HashMap, net::SocketAddr};

use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use tokio::sync::mpsc;
use tracing::warn;

use crate::{
    online,
    relay::{End, Reason},
};

/// The days a relayed match is kept.
pub const KEPT_DAYS: i32 = 30;

/// A relayed match's start or end, as the relay sends it.
#[derive(Debug, PartialEq)]
pub(crate) enum Record {
    Opened {
        cookie: u32,
        reason: Reason,
        /// The games' public addresses, by client index.
        games: [SocketAddr; 2],
        profiles: [Option<i32>; 2],
        at: OffsetDateTime,
    },
    Ended {
        cookie: u32,
        why: End,
        /// The last packet relayed.
        at: OffsetDateTime,
        packets: u64,
        bytes: u64,
    },
}

/// Where the relay sends its [`Record`]s, for [`run`] to write.
#[derive(Clone, Debug)]
pub(crate) struct Recorder(mpsc::UnboundedSender<Record>);

impl Recorder {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<Record>) {
        let (sender, records) = mpsc::unbounded_channel();
        (Self(sender), records)
    }

    /// Never waits; a record after [`run`] stopped is dropped.
    pub fn send(&self, record: Record) {
        let _ = self.0.send(record);
    }
}

/// Writes the `records` until every [`Recorder`] is gone. First it ends this
/// server's matches a run before left open, and lets the old ones go.
pub(crate) async fn run(pool: PgPool, mut records: mpsc::UnboundedReceiver<Record>) {
    let server = online::server();
    if let Err(error) = settle(&pool, server).await {
        warn!(target: "natneg", %error, "relayed matches not settled");
    }
    // This run's matches in the database, by cookie, until they end.
    let mut stored: HashMap<u32, i64> = HashMap::new();
    while let Some(record) = records.recv().await {
        match record {
            Record::Opened {
                cookie,
                reason,
                games,
                profiles,
                at,
            } => match open(&pool, server, cookie, reason, games, profiles, at).await {
                Ok(relay_id) => {
                    stored.insert(cookie, relay_id);
                }
                Err(error) => {
                    warn!(target: "natneg", cookie = format!("{cookie:08x}"), %error, "relayed match not stored");
                }
            },
            Record::Ended {
                cookie,
                why,
                at,
                packets,
                bytes,
            } => {
                let Some(relay_id) = stored.remove(&cookie) else {
                    continue;
                };
                if let Err(error) = end(&pool, relay_id, why, at, packets, bytes).await {
                    warn!(target: "natneg", cookie = format!("{cookie:08x}"), %error, "relayed match's end not stored");
                }
            }
        }
    }
}

/// Ends `server`'s matches still open, which the relay forgot when the
/// server stopped, and drops those older than [`KEPT_DAYS`].
async fn settle(pool: &PgPool, server: &str) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE relay_sessions SET ended_at = now(), ended_why = 'restart'
         WHERE server = $1 AND ended_at IS NULL",
        server,
    )
    .execute(pool)
    .await?;
    sqlx::query!(
        "DELETE FROM relay_sessions WHERE started_at < now() - make_interval(days => $1)",
        KEPT_DAYS,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Stores a relayed match's start, a profile only if it exists, and lets
/// the oldest go. Its `relay_id`.
async fn open(
    pool: &PgPool,
    server: &str,
    cookie: u32,
    reason: Reason,
    games: [SocketAddr; 2],
    profiles: [Option<i32>; 2],
    at: OffsetDateTime,
) -> Result<i64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "DELETE FROM relay_sessions WHERE started_at < now() - make_interval(days => $1)",
        KEPT_DAYS,
    )
    .execute(&mut *tx)
    .await?;
    let relay_id = sqlx::query_scalar!(
        "INSERT INTO relay_sessions (server, cookie, reason, first_address, second_address,
                                     first_profile, second_profile, started_at)
         VALUES ($1, $2, $3, $4, $5,
                 (SELECT profile_id FROM profiles WHERE profile_id = $6),
                 (SELECT profile_id FROM profiles WHERE profile_id = $7), $8)
         RETURNING relay_id",
        server,
        format!("{cookie:08x}"),
        reason.code(),
        games[0].to_string(),
        games[1].to_string(),
        profiles[0],
        profiles[1],
        at,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(relay_id)
}

async fn end(
    pool: &PgPool,
    relay_id: i64,
    why: End,
    at: OffsetDateTime,
    packets: u64,
    bytes: u64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE relay_sessions
         SET ended_at = GREATEST($2, started_at), ended_why = $3, packets = $4, bytes = $5
         WHERE relay_id = $1",
        relay_id,
        at,
        why.code(),
        i64::try_from(packets).unwrap_or(i64::MAX),
        i64::try_from(bytes).unwrap_or(i64::MAX),
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// A relayed match as `/admin` shows it.
#[derive(Debug, Serialize)]
pub struct Relayed {
    pub id: i64,
    /// The server that relayed it.
    pub server: String,
    pub cookie: String,
    /// `always`, `new_port` or `failed_before`.
    pub reason: String,
    /// By client index.
    pub games: [RelayedGame; 2],
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    /// None while the relay carries it.
    #[serde(with = "time::serde::rfc3339::option")]
    pub ended_at: Option<OffsetDateTime>,
    /// `idle`, `replaced` or `restart`.
    pub ended_why: Option<String>,
    pub packets: i64,
    pub bytes: i64,
    /// The reported match its games played, for a player's page; none when
    /// none was reported.
    pub match_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct RelayedGame {
    pub address: String,
    pub profile_id: Option<i32>,
    pub mii_name: Option<String>,
}

/// The relayed matches of the `profiles` that ran while a match from `from`
/// to `to` did: those that began by `to` and went on to `from` at least.
pub async fn during(
    conn: &mut PgConnection,
    profiles: &[i32],
    from: OffsetDateTime,
    to: OffsetDateTime,
) -> Result<Vec<Relayed>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT r.relay_id, r.server, r.cookie, r.reason, r.first_address, r.second_address,
                  r.first_profile, r.second_profile, first.ingamesn AS "first_mii?",
                  second.ingamesn AS "second_mii?", r.started_at, r.ended_at, r.ended_why,
                  r.packets, r.bytes
           FROM relay_sessions r
           LEFT JOIN profiles first ON first.profile_id = r.first_profile
           LEFT JOIN profiles second ON second.profile_id = r.second_profile
           WHERE (r.first_profile = ANY($1) OR r.second_profile = ANY($1))
             AND r.started_at <= $3 AND (r.ended_at IS NULL OR r.ended_at >= $2)
           ORDER BY r.started_at"#,
        profiles,
        from,
        to,
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Relayed {
            id: row.relay_id,
            server: row.server,
            cookie: row.cookie,
            reason: row.reason,
            games: [
                RelayedGame {
                    address: row.first_address,
                    profile_id: row.first_profile,
                    mii_name: row.first_mii,
                },
                RelayedGame {
                    address: row.second_address,
                    profile_id: row.second_profile,
                    mii_name: row.second_mii,
                },
            ],
            started_at: row.started_at,
            ended_at: row.ended_at,
            ended_why: row.ended_why,
            packets: row.packets,
            bytes: row.bytes,
            match_id: None,
        })
        .collect())
}

/// The latest `limit` relayed matches of `profile_id`, the newest first,
/// each with the match it reported then.
pub async fn of_profile(
    conn: &mut PgConnection,
    profile_id: i32,
    limit: i64,
) -> Result<Vec<Relayed>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT r.relay_id, r.server, r.cookie, r.reason, r.first_address, r.second_address,
                  r.first_profile, r.second_profile, first.ingamesn AS "first_mii?",
                  second.ingamesn AS "second_mii?", r.started_at, r.ended_at, r.ended_why,
                  r.packets, r.bytes, played.match_id AS "match_id?"
           FROM relay_sessions r
           LEFT JOIN profiles first ON first.profile_id = r.first_profile
           LEFT JOIN profiles second ON second.profile_id = r.second_profile
           LEFT JOIN LATERAL (
               SELECT m.match_id FROM online_match_sides s JOIN online_matches m USING (match_id)
               WHERE s.profile_id = $1 AND r.started_at <= m.last_report_at
                 AND (r.ended_at IS NULL OR r.ended_at >= m.started_at)
               ORDER BY m.started_at LIMIT 1
           ) played ON true
           WHERE r.first_profile = $1 OR r.second_profile = $1
           ORDER BY r.started_at DESC
           LIMIT $2"#,
        profile_id,
        limit,
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Relayed {
            id: row.relay_id,
            server: row.server,
            cookie: row.cookie,
            reason: row.reason,
            games: [
                RelayedGame {
                    address: row.first_address,
                    profile_id: row.first_profile,
                    mii_name: row.first_mii,
                },
                RelayedGame {
                    address: row.second_address,
                    profile_id: row.second_profile,
                    mii_name: row.second_mii,
                },
            ],
            started_at: row.started_at,
            ended_at: row.ended_at,
            ended_why: row.ended_why,
            packets: row.packets,
            bytes: row.bytes,
            match_id: row.match_id,
        })
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::accounts;

    /// A profile that exists, for the rows' references.
    pub async fn profile(pool: &PgPool, user_id: i64, mii: &str) -> i32 {
        accounts::log_in(pool, user_id).await.unwrap();
        accounts::profile_login(pool, user_id, "R4QEaaaaaaa", Some(mii), "10.0.0.1")
            .await
            .unwrap()
            .profile_id
    }

    fn address(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    /// Writes `records` as the relay's task does, until they are done.
    pub async fn write(pool: &PgPool, records: Vec<Record>) {
        let (recorder, receiver) = Recorder::new();
        for record in records {
            recorder.send(record);
        }
        drop(recorder);
        run(pool.clone(), receiver).await;
    }

    pub fn opened(cookie: u32, profiles: [Option<i32>; 2], at: OffsetDateTime) -> Record {
        Record::Opened {
            cookie,
            reason: Reason::NewPort,
            games: [address("203.0.113.7:21982"), address("198.51.100.9:65116")],
            profiles,
            at,
        }
    }

    #[sqlx::test]
    async fn a_relayed_match_is_kept_with_its_players(pool: PgPool) {
        let anosch = profile(&pool, 1, "Anosch").await;
        let start = OffsetDateTime::now_utc() - time::Duration::minutes(10);
        let end = start + time::Duration::minutes(8);
        write(
            &pool,
            vec![
                // An unknown profile is left out.
                opened(0x1234_5678, [Some(anosch), Some(1_999_999_999)], start),
                Record::Ended {
                    cookie: 0x1234_5678,
                    why: End::Idle,
                    at: end,
                    packets: 9000,
                    bytes: 1_200_000,
                },
                // Its end never came: still relayed.
                opened(0x0000_00ab, [None, Some(anosch)], end),
                // An end with no start this run.
                Record::Ended {
                    cookie: 7,
                    why: End::Replaced,
                    at: end,
                    packets: 1,
                    bytes: 1,
                },
            ],
        )
        .await;

        let mut conn = pool.acquire().await.unwrap();
        let mut rows = of_profile(&mut conn, anosch, 10).await.unwrap();
        assert_eq!(rows.len(), 2);
        let open = rows.remove(0);
        assert_eq!(
            (
                open.cookie.as_str(),
                open.ended_at,
                open.games[1].profile_id
            ),
            ("000000ab", None, Some(anosch))
        );
        let ended = rows.remove(0);
        assert_eq!(
            (
                ended.cookie.as_str(),
                ended.reason.as_str(),
                ended.ended_why.as_deref()
            ),
            ("12345678", "new_port", Some("idle"))
        );
        assert_eq!(
            (ended.games[0].profile_id, ended.games[1].profile_id),
            (Some(anosch), None)
        );
        assert_eq!(ended.games[0].mii_name.as_deref(), Some("Anosch"));
        assert_eq!(ended.games[1].address, "198.51.100.9:65116");
        assert_eq!((ended.packets, ended.bytes), (9000, 1_200_000));
        assert_eq!(
            ended.ended_at.unwrap().unix_timestamp(),
            end.unix_timestamp()
        );
        assert_eq!(ended.server, online::server());

        // The server's next run ends what it left open.
        write(&pool, Vec::new()).await;
        let open = of_profile(&mut conn, anosch, 1).await.unwrap().remove(0);
        assert_eq!(open.ended_why.as_deref(), Some("restart"));
        assert!(open.ended_at.is_some());
    }

    #[sqlx::test]
    async fn a_match_finds_the_relaying_that_ran_with_it(pool: PgPool) {
        let anosch = profile(&pool, 1, "Anosch").await;
        let sam = profile(&pool, 2, "Sam").await;
        let now = OffsetDateTime::now_utc();
        let minutes = |m: i64| now + time::Duration::minutes(m);
        write(
            &pool,
            vec![
                opened(1, [Some(anosch), Some(sam)], minutes(-60)),
                Record::Ended {
                    cookie: 1,
                    why: End::Idle,
                    at: minutes(-40),
                    packets: 1,
                    bytes: 1,
                },
                opened(2, [Some(sam), None], minutes(-30)),
            ],
        )
        .await;
        let mut conn = pool.acquire().await.unwrap();
        let cookies =
            |rows: Vec<Relayed>| rows.into_iter().map(|row| row.cookie).collect::<Vec<_>>();
        // A match from 50 to 45 minutes ago: the first only.
        let rows = during(&mut conn, &[anosch], minutes(-50), minutes(-45))
            .await
            .unwrap();
        assert_eq!(cookies(rows), ["00000001"]);
        // From 35 minutes ago on: the second, still relayed, has Sam only.
        let rows = during(&mut conn, &[anosch, sam], minutes(-35), minutes(-1))
            .await
            .unwrap();
        assert_eq!(cookies(rows), ["00000002"]);
        assert!(
            during(&mut conn, &[anosch], minutes(-35), minutes(-1))
                .await
                .unwrap()
                .is_empty()
        );
        // Before them both.
        assert!(
            during(&mut conn, &[anosch, sam], minutes(-90), minutes(-70))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[sqlx::test]
    async fn old_relayed_matches_go(pool: PgPool) {
        let old = OffsetDateTime::now_utc() - time::Duration::days(i64::from(KEPT_DAYS) + 1);
        write(&pool, vec![opened(1, [None, None], old)]).await;
        let count = || async {
            sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM relay_sessions"#)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        assert_eq!(count().await, 1, "kept until the next start or run");
        write(
            &pool,
            vec![opened(2, [None, None], OffsetDateTime::now_utc())],
        )
        .await;
        assert_eq!(count().await, 1);
    }
}

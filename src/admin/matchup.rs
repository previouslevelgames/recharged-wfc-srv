//! A profile's matchmaking as the server sees it now, for diagnosing
//! (docs/install.md, "Diagnosing matchmaking"): its WFC login and DWC status,
//! its games' master-server sessions with every heartbeat field and the
//! game's network version (`AP`), how a set keeps it, and its matchmaking
//! events. With a second profile, also whether each one's latest ranked
//! search lists the other's game now, and why not, as the server browser
//! answers it ([`serverbrowser::explain`]). A player's page shows the same
//! live view ([`live`], [`events_of`]).

use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
};

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::{
    activity::{Activity, Event, What},
    gpcm::Gpcm,
    master::{Master, Session},
    pairing::{Pairings, Visible},
    serverbrowser,
};

/// `?with=<profile>`: the second profile.
#[derive(Deserialize)]
pub struct With {
    with: Option<i32>,
}

#[derive(Serialize)]
pub struct Matchup {
    /// The profile asked for, then the one `with` names.
    profiles: Vec<Profile>,
    /// With two profiles: each one's latest ranked search against the
    /// other's games.
    searches: Vec<Search>,
    /// The profiles' matchmaking events, the latest first.
    events: Vec<Event>,
    #[serde(with = "time::serde::rfc3339")]
    now: OffsetDateTime,
}

#[derive(Serialize)]
struct Profile {
    profile_id: i32,
    /// Whether this server has the profile.
    known: bool,
    mii: Option<String>,
    /// The release, from the profile's `gsbrcd`: `R4QE`, `R4QP`, …
    game: Option<String>,
    /// Its WFC login (GPCM), while logged in.
    login: Option<Login>,
    #[serde(flatten)]
    live: Live,
}

/// A profile's games as the master server and the sets see them now.
#[derive(Serialize)]
pub struct Live {
    /// Its games' master-server sessions, the freshest first.
    sessions: Vec<GameSession>,
    /// Whom its ranked search may list: `everyone`, `only` its set's
    /// opponent's game, or `nobody` (a set keeps it and the opponent's game
    /// is not ready).
    sees: &'static str,
    opponent: Option<i32>,
    /// Whether a set keeps its game out of everyone else's searches.
    kept: bool,
}

#[derive(Serialize)]
struct Login {
    /// GPCM's connection, and the NAS login's address.
    from: Option<String>,
    nas_from: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    logged_in_at: OffsetDateTime,
    /// DWC's status (1 online, 2 playing, 3 searching RANKED, 4 to 6 with
    /// friends) and since when.
    status: Option<u8>,
    #[serde(with = "time::serde::rfc3339::option")]
    status_since: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct GameSession {
    /// Where its heartbeats come from.
    address: String,
    /// Whether it answered the challenge.
    listed: bool,
    /// Seconds since its game was last heard from.
    quiet_secs: f64,
    /// Its `AP`, decoded.
    version: Option<Version>,
    /// Every field of its last heartbeat.
    fields: BTreeMap<String, String>,
}

/// A game's network version, the `AP` its ranked search asks for: only games
/// of the same word meet.
#[derive(Debug, PartialEq, Serialize)]
struct Version {
    word: u32,
    /// `original`: the game's own word, the region and the disc's build
    /// (`GetNetworkVersionWord`); `recharged`: the port's word for play
    /// between the releases, by its protocol (Recharged 1.2 on); `unknown`.
    kind: &'static str,
    region: Option<&'static str>,
    build: Option<u16>,
    protocol: Option<u16>,
}

#[derive(Serialize)]
struct Search {
    searcher: i32,
    other: i32,
    /// The searcher's latest ranked search in the activity, and when.
    filter: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    last_at: Option<OffsetDateTime>,
    /// Whether that search lists one of the other's games now.
    lists: bool,
    /// Why not: for each of the other's games, or for the pair.
    why: Vec<String>,
}

pub async fn read(
    pool: &PgPool,
    gpcm: &Gpcm,
    master: &Master,
    activity: &Activity,
    pairings: &Pairings,
    profile_id: i32,
    with: With,
) -> Result<Matchup, sqlx::Error> {
    let mut ids = vec![profile_id];
    ids.extend(with.with.filter(|&other| other != profile_id));
    let rows: HashMap<i32, (String, Option<String>)> = sqlx::query!(
        "SELECT profile_id, gsbrcd, ingamesn FROM profiles WHERE profile_id = ANY($1)",
        &ids,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| (row.profile_id, (row.gsbrcd, row.ingamesn)))
    .collect();

    let presence = gpcm.presence();
    let sessions = master.sessions();
    let events = activity.events();
    let profiles = ids
        .iter()
        .map(|&id| {
            let row = rows.get(&id);
            let login = presence.get(&id).map(|presence| {
                let addresses = gpcm.login_addresses(id);
                Login {
                    from: addresses
                        .as_ref()
                        .map(|addresses| addresses.gpcm.to_string()),
                    nas_from: addresses.map(|addresses| addresses.nas.to_string()),
                    logged_in_at: presence.logged_in_at,
                    status: presence.status,
                    status_since: presence.status_since,
                }
            });
            Profile {
                profile_id: id,
                known: row.is_some(),
                mii: row.and_then(|(_, mii)| mii.clone()),
                game: row.map(|(gsbrcd, _)| gsbrcd.chars().take(4).collect()),
                login,
                live: live(&sessions, pairings, id),
            }
        })
        .collect();

    let searches = match ids[..] {
        [first, second] => vec![
            search(pairings, &sessions, &events, first, second),
            search(pairings, &sessions, &events, second, first),
        ],
        _ => Vec::new(),
    };

    Ok(Matchup {
        profiles,
        searches,
        events: events_of(events, &sessions, &ids),
        now: OffsetDateTime::now_utc(),
    })
}

/// The Mii names of the profiles in `events`.
pub async fn names(
    conn: &mut sqlx::PgConnection,
    events: &[Event],
) -> Result<HashMap<i32, String>, sqlx::Error> {
    let mut profiles: Vec<i32> = events
        .iter()
        .flat_map(|event| match &event.what {
            What::Search {
                profile_id,
                found_profiles,
                missed,
                ..
            } => profile_id
                .iter()
                .copied()
                .chain(found_profiles.iter().copied())
                .chain(missed.iter().map(|miss| miss.profile_id))
                .collect::<Vec<_>>(),
            What::Command { from, to, .. } => from.iter().chain(to).copied().collect(),
            What::Negotiation { games, .. } => {
                games.iter().filter_map(|party| party.profile_id).collect()
            }
        })
        .collect();
    profiles.sort_unstable();
    profiles.dedup();
    Ok(sqlx::query!(
        "SELECT profile_id, ingamesn FROM profiles WHERE profile_id = ANY($1)",
        &profiles,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .filter_map(|row| Some((row.profile_id, row.ingamesn?)))
    .collect())
}

/// `profile_id`'s games now: its sessions in `sessions`, the freshest first,
/// and how the sets' `pairings` keep it.
pub fn live(sessions: &[(SocketAddr, Session)], pairings: &Pairings, profile_id: i32) -> Live {
    let mut games: Vec<&(SocketAddr, Session)> = sessions_of(sessions, profile_id).collect();
    games.sort_by_key(|(_, session)| session.quiet_for());
    let (sees, opponent) = match pairings.visible_to(profile_id) {
        Visible::Everyone => ("everyone", None),
        Visible::Only(opponent) => ("only", Some(opponent)),
        Visible::Nobody => ("nobody", None),
    };
    Live {
        sessions: games
            .into_iter()
            .map(|(address, session)| GameSession {
                address: address.to_string(),
                listed: session.listed,
                quiet_secs: session.quiet_for().as_secs_f64(),
                version: session
                    .field("AP")
                    .and_then(|word| word.parse::<u32>().ok())
                    .map(version),
                fields: session.fields.iter().cloned().collect(),
            })
            .collect(),
        sees,
        opponent,
        kept: pairings.kept(profile_id),
    }
}

/// The `events` of the profiles `ids`, a negotiation's games named by the
/// profile of the session at their address.
pub fn events_of(
    events: Vec<Event>,
    sessions: &[(SocketAddr, Session)],
    ids: &[i32],
) -> Vec<Event> {
    let addresses: HashMap<String, i32> = sessions
        .iter()
        .filter_map(|(address, session)| {
            Some((address.to_string(), session.field("dwc_pid")?.parse().ok()?))
        })
        .collect();
    let ours = |profile: &Option<i32>| profile.is_some_and(|profile| ids.contains(&profile));
    events
        .into_iter()
        .filter_map(|mut event| {
            let involved = match &mut event.what {
                What::Search { profile_id, .. } => ours(profile_id),
                What::Command { from, to, .. } => ours(from) || ours(to),
                What::Negotiation { games, .. } => {
                    for party in games.iter_mut() {
                        party.profile_id = addresses.get(&party.address).copied();
                    }
                    games.iter().any(|party| ours(&party.profile_id))
                }
            };
            involved.then_some(event)
        })
        .collect()
}

/// The live sessions whose heartbeats name `profile_id`.
fn sessions_of(
    sessions: &[(SocketAddr, Session)],
    profile_id: i32,
) -> impl Iterator<Item = &(SocketAddr, Session)> {
    let pid = profile_id.to_string();
    sessions
        .iter()
        .filter(move |(_, session)| session.field("dwc_pid") == Some(pid.as_str()))
}

/// Whether `searcher`'s latest ranked search lists one of `other`'s games now.
fn search(
    pairings: &Pairings,
    sessions: &[(SocketAddr, Session)],
    events: &[Event],
    searcher: i32,
    other: i32,
) -> Search {
    let latest = events.iter().find_map(|event| match &event.what {
        What::Search {
            profile_id: Some(profile),
            filter,
            ..
        } if *profile == searcher => Some((filter.clone(), event.last_at)),
        _ => None,
    });
    let Some((filter, last_at)) = latest else {
        return Search {
            searcher,
            other,
            filter: None,
            last_at: None,
            lists: false,
            why: vec![format!(
                "no ranked search by {searcher} among the server's latest events"
            )],
        };
    };
    let mut why = Vec::new();
    let mut lists = false;
    for (address, session) in sessions_of(sessions, other) {
        match serverbrowser::explain(pairings, sessions, searcher, &filter, session) {
            None => lists = true,
            Some(reason) => why.push(format!("{address}: {reason}")),
        }
    }
    if lists {
        why.clear();
    } else if why.is_empty() {
        why.push(format!(
            "{other} has no master-server session: its game sends no heartbeats here"
        ));
    }
    Search {
        searcher,
        other,
        filter: Some(filter),
        last_at: Some(last_at),
        lists,
        why,
    }
}

/// `AP` as `GetNetworkVersionWord` makes it (`0x1B?3` and the disc's build,
/// the region in the `?`: 10 US, 13 KR, 14 EU, 15 JP), or the port's word
/// for play between the releases (`RC` and its protocol, tweak patch 0058).
fn version(word: u32) -> Version {
    let unknown = Version {
        word,
        kind: "unknown",
        region: None,
        build: None,
        protocol: None,
    };
    if word & 0xFF0F_0000 == 0x1B03_0000 {
        Version {
            kind: "original",
            region: match (word >> 20) & 0xF {
                10 => Some("US"),
                13 => Some("KR"),
                14 => Some("EU"),
                15 => Some("JP"),
                _ => None,
            },
            build: Some(word as u16),
            ..unknown
        }
    } else if word >> 16 == u32::from(u16::from_be_bytes(*b"RC")) {
        Version {
            kind: "recharged",
            protocol: Some(word as u16),
            ..unknown
        }
    } else {
        unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{accounts, activity::Listed};
    use std::sync::Arc;

    /// A ranked search's heartbeat, from the port, with `pid` and `ap`.
    fn searching(pid: i32, ap: u32) -> Vec<u8> {
        let mut packet = b"\x03\x5c\xd6s\x9f".to_vec();
        for (name, value) in [
            ("localip0", "192.168.1.131".to_owned()),
            ("localport", "53103".to_owned()),
            ("natneg", "1".to_owned()),
            ("gamename", "mschargedwii".to_owned()),
            ("numplayers", "0".to_owned()),
            ("maxplayers", "1".to_owned()),
            ("dwc_pid", pid.to_string()),
            ("dwc_mtype", "0".to_owned()),
            ("dwc_mresv", "0".to_owned()),
            ("dwc_mver", "3".to_owned()),
            ("GT", "0".to_owned()),
            ("PI", pid.to_string()),
            ("AP", ap.to_string()),
            ("PT", "0".to_owned()),
            ("CO", "0".to_owned()),
        ] {
            for part in [name, value.as_str()] {
                packet.extend_from_slice(part.as_bytes());
                packet.push(0);
            }
        }
        packet.extend_from_slice(&[0; 3]);
        packet
    }

    fn ranked(pid: i32, ap: u32) -> String {
        format!(
            "dwc_mver = 3 and dwc_pid != {pid} and maxplayers = 1 and numplayers < 1 and \
             dwc_mtype = 0 and dwc_mresv != dwc_pid and (AP = {ap} and GT = 0 and CO = 0)"
        )
    }

    #[test]
    fn versions_name_their_region_and_build_or_protocol() {
        assert_eq!(
            version(463715253),
            Version {
                word: 463715253,
                kind: "original",
                region: Some("US"),
                build: Some(0xBBB5),
                protocol: None,
            }
        );
        let europe = version(467902516);
        assert_eq!((europe.region, europe.build), (Some("EU"), Some(0xA034)));
        let recharged = version(1380122625);
        assert_eq!(
            (recharged.kind, recharged.protocol, recharged.region),
            ("recharged", Some(1), None)
        );
        assert_eq!(version(7).kind, "unknown");
    }

    #[sqlx::test]
    async fn a_pair_says_why_one_search_does_not_list_the_other(pool: PgPool) {
        accounts::log_in(&pool, 42).await.unwrap();
        let profile =
            accounts::profile_login(&pool, 42, "R4QEaaaaaaa", Some("Jacubsas"), "10.0.0.1")
                .await
                .unwrap()
                .profile_id;
        let other = profile + 1;
        let master = Master::default();
        let (recharged, europe) = (1380122625, 467902516);
        master.answer(
            &searching(profile, recharged),
            "198.51.100.1:51677".parse().unwrap(),
        );
        master.answer(
            &searching(other, europe),
            "198.51.100.2:51579".parse().unwrap(),
        );
        let activity = Arc::new(Activity::default());
        activity.search(
            Some(profile),
            &ranked(profile, recharged),
            Listed::default(),
        );
        let gpcm = Gpcm::new(pool.clone(), Default::default(), activity.clone());
        let pairings = Pairings::default();

        let matchup = read(
            &pool,
            &gpcm,
            &master,
            &activity,
            &pairings,
            profile,
            With { with: Some(other) },
        )
        .await
        .unwrap();

        let [first, second] = &matchup.profiles[..] else {
            panic!("two profiles");
        };
        assert!(first.known && !second.known);
        assert_eq!(first.game.as_deref(), Some("R4QE"));
        assert_eq!(first.live.sees, "everyone");
        let session = &first.live.sessions[0];
        assert_eq!(session.version.as_ref().map(|v| v.kind), Some("recharged"));
        assert_eq!(session.fields.get("CO").map(String::as_str), Some("0"));

        let [mine, theirs] = &matchup.searches[..] else {
            panic!("two searches");
        };
        assert!(!mine.lists);
        assert_eq!(
            mine.why,
            [format!(
                "198.51.100.2:51579: AP = {recharged} (AP is {europe})"
            )]
        );
        // The other profile has searched nothing here.
        assert!(!theirs.lists);
        assert!(
            theirs.why[0].starts_with("no ranked search"),
            "{:?}",
            theirs.why
        );
        assert_eq!(matchup.events.len(), 1);

        // Without a session, the other's game sends no heartbeats here.
        let alone = read(
            &pool,
            &gpcm,
            &Master::default(),
            &activity,
            &pairings,
            profile,
            With { with: Some(other) },
        )
        .await
        .unwrap();
        assert!(alone.searches[0].why[0].contains("no master-server session"));
    }
}

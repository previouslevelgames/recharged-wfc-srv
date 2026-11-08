//! The server browser (`<game>.ms11.gs.nintendowifi.net`, TCP 28910): lists
//! the master server's sessions that pass a game's filter. A friend who got a
//! place in a host's group asks for the host's session by profile
//! (`dwc_pid = <pid>`) to learn its addresses; a ranked search asks for every
//! game searching like it (`filter.rs`). The game then sends the session
//! messages through the server browser, which the master server passes on
//! (docs/login.md). Other requests are read and not answered.
//!
//! Games kept for a Recharged match ([`crate::pairing`]) change what a ranked
//! search lists: a kept game's search lists only its opponent's game, and
//! nobody else's lists a kept game (docs/profiles.md, "Meeting in RANKED").
//!
//! For diagnosing, a search tells the activity and the log whom it found and
//! why it left out each other game in a ranked search; [`explain`] says the
//! same for a search and a session the admin asks about.

mod crypt;
mod filter;

use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tracing::{debug, info, trace};

use crate::{
    activity::{Activity, Listed, Miss, Via},
    master::{Master, Session},
    pairing::{Pairings, Visible},
};
use crypt::CHALLENGE_LEN;
use filter::{Failed, Filter};

/// The game's secret key, which also keys its QR2 challenges.
const SECRET_KEY: &[u8] = b"B4LdGW";
/// Requests are short: one longer than this is not one.
const MAX_REQUEST: usize = 0x1000;
/// Request types: the third byte, after the request's length.
const LIST_REQUEST: u8 = 0x00;
/// A message for a session's game: its public IP and port, then the message.
const SEND_MESSAGE_REQUEST: u8 = 0x02;
/// Messages DWC sends are short; WiiLink allows 148 bytes.
const MAX_MESSAGE: usize = 0x200;
/// DWC's match commands start `SBCM`, then the version (4 bytes), the command
/// and the sender's QR2 address and profile (`DWCSBMessageHeader`), in the
/// port's little-endian order.
const MATCH_COMMAND: &[u8; 4] = b"SBCM";
const MATCH_COMMAND_HEADER: usize = 20;
/// List request options.
const NO_SERVER_LIST: u32 = 0x02;
/// `dwc_mtype` of a game in a ranked search (`dwc_match.h`).
const RANKED_SEARCH: &str = "0";

/// Server flags in a list (`sb_internal.h`).
const PRIVATE_IP: u8 = 0x02;
const CONNECT_NEGOTIATE: u8 = 0x04;
const ICMP_IP: u8 = 0x08;
const NONSTANDARD_PORT: u8 = 0x10;
const NONSTANDARD_PRIVATE_PORT: u8 = 0x20;
const HAS_KEYS: u8 = 0x40;
/// A string field's value follows as text (not an index into popular values).
const STRING_VALUE: u8 = 0xFF;
/// The list ends with a server of no flags at 255.255.255.255.
const LAST_SERVER: [u8; 5] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF];

pub async fn serve(
    master: Arc<Master>,
    activity: Arc<Activity>,
    pairings: Arc<Pairings>,
    listener: TcpListener,
) -> std::io::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let (master, activity, pairings) = (master.clone(), activity.clone(), pairings.clone());
        tokio::spawn(async move {
            if let Err(error) = connection(master, activity, pairings, stream, peer).await {
                debug!(target: "serverbrowser", %peer, %error, "connection failed");
            }
        });
    }
}

async fn connection(
    master: Arc<Master>,
    activity: Arc<Activity>,
    pairings: Arc<Pairings>,
    mut stream: TcpStream,
    peer: SocketAddr,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0; 2048];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        // Each request starts with its length, these two bytes included.
        while let Some(&[high, low]) = buffer.first_chunk::<2>() {
            let len = usize::from(u16::from_be_bytes([high, low]));
            if !(3..=MAX_REQUEST).contains(&len) {
                debug!(target: "serverbrowser", %peer, len, "not a request");
                return Ok(());
            }
            if buffer.len() < len {
                break;
            }
            let request: Vec<u8> = buffer.drain(..len).collect();
            trace!(target: "raw::serverbrowser", %peer, ?request, "request");
            if let Some(reply) = answer(&master, &activity, &pairings, &request, peer) {
                stream.write_all(&reply).await?;
            }
        }
        if buffer.len() > MAX_REQUEST {
            return Ok(());
        }
    }
}

fn answer(
    master: &Arc<Master>,
    activity: &Arc<Activity>,
    pairings: &Pairings,
    request: &[u8],
    peer: SocketAddr,
) -> Option<Vec<u8>> {
    match request[2] {
        LIST_REQUEST => {
            let list = ListRequest::parse(request)?;
            let searcher = searcher(&list.filter);
            let sessions = master.sessions();
            let steering = match searcher {
                // A lookup of one host: a friend's, kept or not.
                None => Steering::None,
                Some(profile_id) => Steering::search(pairings, profile_id, &sessions),
            };
            let (reply, listed) = list.reply(&sessions, peer, &steering, searcher.flatten())?;
            if let Some(profile_id) = searcher {
                activity.search(profile_id, &list.filter, listed);
            }
            Some(crypt::encrypt(
                SECRET_KEY,
                &list.challenge,
                rand::random(),
                &reply,
            ))
        }
        SEND_MESSAGE_REQUEST => {
            send_message(master, activity, request, peer);
            None
        }
        kind => {
            debug!(target: "serverbrowser", %peer, kind, "not implemented yet");
            None
        }
    }
}

/// `SEND_MESSAGE_REQUEST`: a message, such as DWC's `SBCM` match commands
/// and NAT negotiation's cookie, for the game of the session at the public
/// address given. The master server delivers it; the client hears nothing.
/// Whether a match command reached its game goes to the activity.
fn send_message(master: &Arc<Master>, activity: &Arc<Activity>, request: &[u8], peer: SocketAddr) {
    let (Some(ip), Some(port), Some(message)) = (
        request.get(3..7),
        request.get(7..9),
        request
            .get(9..)
            .filter(|message| (1..=MAX_MESSAGE).contains(&message.len())),
    ) else {
        debug!(target: "serverbrowser", %peer, "bad message");
        return;
    };
    let to = SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]),
        u16::from_be_bytes([port[0], port[1]]),
    ));
    let kind = match message {
        [0xFD, 0xFC, ..] => "NAT negotiation cookie",
        [b'S', b'B', b'C', b'M', ..] => "match command",
        _ => "other",
    };
    info!(target: "serverbrowser", %peer, %to, kind, "message");
    let command = match_command(message).map(|(command, from)| {
        let to_profile = master
            .session(to)
            .and_then(|session| session.field("dwc_pid")?.parse().ok());
        activity.command(from, to_profile, command, Via::ServerBrowser);
        (from, to_profile, command)
    });
    let (master, activity) = (master.clone(), activity.clone());
    let message = message.to_vec();
    tokio::spawn(async move {
        let acked = master.send_client_message(to, &message).await;
        if let Some((from, to_profile, command)) = command {
            activity.delivered(from, to_profile, command, acked);
            if !acked {
                info!(target: "serverbrowser", %to, command, "match command not acknowledged");
            }
        }
    });
}

/// A DWC match command's command byte and sender's profile.
fn match_command(message: &[u8]) -> Option<(u8, Option<i32>)> {
    let header = message.get(..MATCH_COMMAND_HEADER)?;
    if !header.starts_with(MATCH_COMMAND) {
        return None;
    }
    let profile = i32::from_le_bytes(header[16..20].try_into().ok()?);
    Some((header[8], (profile > 0).then_some(profile)))
}

/// The profile a search is by: the one its filter leaves out
/// (`dwc_pid != <pid>`). A lookup of one profile (`dwc_pid = <pid>`) is no
/// search: `None`.
fn searcher(filter: &str) -> Option<Option<i32>> {
    let filter = filter.trim();
    if filter.starts_with("dwc_pid =") && !filter.contains(" and ") {
        return None;
    }
    Some(filter.split_once("dwc_pid != ").and_then(|(_, rest)| {
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest[..end].parse().ok()
    }))
}

/// What a list holds besides what its filter says.
enum Steering {
    /// The filter alone: a lookup of one host.
    None,
    /// A search by a game in no match: no game kept for one.
    Unkept(Vec<i32>),
    /// A search by a kept game whose opponent is ready: that game alone,
    /// which may be in another region group (`CO`) of the release.
    Only {
        opponent: i32,
        region: Option<String>,
    },
    /// A search by a kept game whose opponent is not ready: nothing.
    Nothing,
}

impl Steering {
    fn search(
        pairings: &Pairings,
        searcher: Option<i32>,
        sessions: &[(SocketAddr, Session)],
    ) -> Self {
        match searcher.map(|profile_id| pairings.visible_to(profile_id)) {
            Some(Visible::Only(opponent)) => Self::Only {
                opponent,
                // The searcher's own region group, from its heartbeat.
                region: sessions
                    .iter()
                    .find(|(_, session)| profile(session) == searcher)
                    .and_then(|(_, session)| session.field("CO"))
                    .map(str::to_owned),
            },
            Some(Visible::Nobody) => Self::Nothing,
            Some(Visible::Everyone) | None => Self::Unkept(
                sessions
                    .iter()
                    .filter_map(|(_, session)| profile(session))
                    .filter(|&profile_id| pairings.kept(profile_id))
                    .collect(),
            ),
        }
    }
}

/// `SERVER_LIST_REQUEST`: after the length and type, the protocol and
/// encoding versions and the game's version, then NUL-terminated the game
/// listed and the game asking, the client's challenge, the filter and the
/// fields (`\name\name`), and the options.
#[derive(Debug, PartialEq)]
struct ListRequest {
    game: String,
    challenge: [u8; CHALLENGE_LEN],
    filter: String,
    fields: Vec<String>,
    options: u32,
}

impl ListRequest {
    fn parse(request: &[u8]) -> Option<Self> {
        let mut rest = request.get(9..)?;
        let game = text(&mut rest)?;
        let _asking = text(&mut rest)?;
        let (&challenge, after) = rest.split_first_chunk::<CHALLENGE_LEN>()?;
        rest = after;
        let filter = text(&mut rest)?;
        let fields = text(&mut rest)?;
        let options = u32::from_be_bytes(*rest.first_chunk::<4>()?);
        Some(Self {
            game,
            challenge,
            filter,
            fields: fields
                .split('\\')
                .filter(|field| !field.trim().is_empty())
                .map(str::to_owned)
                .collect(),
            options,
        })
    }

    /// The list, before encryption: the client's own address, the fields'
    /// names, no popular values, the matching sessions, and the end; and
    /// what it holds, with the other games in a ranked search that a search
    /// by `searcher` left out.
    fn reply(
        &self,
        sessions: &[(SocketAddr, Session)],
        peer: SocketAddr,
        steering: &Steering,
        searcher: Option<i32>,
    ) -> Option<(Vec<u8>, Listed)> {
        if self.game != crate::GAME_NAME {
            debug!(target: "serverbrowser", %peer, game = self.game, "list of another game");
            return None;
        }
        let SocketAddr::V4(client) = peer else {
            return None;
        };
        let mut out = client.ip().octets().to_vec();
        out.extend_from_slice(&client.port().to_be_bytes());
        if self.options & NO_SERVER_LIST != 0 {
            return Some((out, Listed::default()));
        }

        // The addresses are in the server's own bytes, not fields.
        let fields: Vec<&str> = self
            .fields
            .iter()
            .map(String::as_str)
            .filter(|field| {
                !matches!(*field, "publicip" | "publicport" | "localport")
                    && !field.starts_with("localip")
            })
            .collect();
        out.push(fields.len() as u8);
        for field in &fields {
            out.push(0); // A string.
            out.extend_from_slice(field.as_bytes());
            out.push(0);
        }
        out.push(0); // No popular values.

        let Selection { found, missed } = self.select(sessions, steering, searcher);
        let mut profiles: Vec<i32> = found
            .iter()
            .filter_map(|(_, session)| profile(session))
            .collect();
        profiles.sort_unstable();
        profiles.dedup();
        info!(
            target: "serverbrowser",
            %peer,
            filter = self.filter,
            found = found.len(),
            profiles = ?profiles,
            missed = %Misses(&missed),
            "list"
        );
        let count = found.len();
        for (address, session) in found {
            if let Some(server) = server(address, session, &fields) {
                out.extend_from_slice(&server);
            }
        }
        out.extend_from_slice(&LAST_SERVER);
        Some((
            out,
            Listed {
                count,
                profiles,
                missed,
            },
        ))
    }

    /// The game's sessions that pass the filter and the steering; none for a
    /// filter it does not understand.
    #[cfg(test)]
    fn matches<'a>(
        &self,
        sessions: &'a [(SocketAddr, Session)],
        steering: &Steering,
    ) -> Vec<(SocketAddr, &'a Session)> {
        self.select(sessions, steering, None).found
    }

    /// The game's sessions that pass the filter and the steering (none for a
    /// filter it does not understand), and for a search by `searcher` each
    /// other profile in a ranked search that none of them is, with why.
    fn select<'a>(
        &self,
        sessions: &'a [(SocketAddr, Session)],
        steering: &Steering,
        searcher: Option<i32>,
    ) -> Selection<'a> {
        let filter = Filter::parse(&self.filter);
        if filter.is_none() {
            debug!(target: "serverbrowser", filter = self.filter, "filter not understood");
        }
        let mut found = Vec::new();
        let mut missed: Vec<Miss> = Vec::new();
        for (address, session) in sessions {
            if session.field("gamename") != Some(crate::GAME_NAME) {
                continue;
            }
            let Some(skip) = skip(filter.as_ref(), steering, session) else {
                found.push((*address, session));
                continue;
            };
            if let (Some(searcher), Some(profile_id)) = (searcher, profile(session))
                && profile_id != searcher
                && session.field("dwc_mtype") == Some(RANKED_SEARCH)
                && !missed.iter().any(|miss| miss.profile_id == profile_id)
            {
                missed.push(Miss {
                    profile_id,
                    why: skip.describe(steering, session),
                });
            }
        }
        // A profile one of whose sessions is listed is not left out.
        missed.retain(|miss| {
            !found
                .iter()
                .any(|(_, session)| profile(session) == Some(miss.profile_id))
        });
        Selection { found, missed }
    }
}

/// What a list holds, and the games in a ranked search it left out.
struct Selection<'a> {
    found: Vec<(SocketAddr, &'a Session)>,
    missed: Vec<Miss>,
}

/// Why a search leaves out a session of this game.
enum Skip<'f> {
    /// The server does not understand the filter: it lists nothing.
    NotUnderstood,
    /// The searching game is kept for a set whose other game is not ready.
    Waiting,
    /// The searching game is kept for a set against this profile's game.
    KeptFor(i32),
    /// This game is kept for a set.
    Kept,
    /// The session fails this part of the filter.
    Filter(Failed<'f>),
}

impl Skip<'_> {
    fn describe(&self, steering: &Steering, session: &Session) -> String {
        match self {
            Self::NotUnderstood => "the server does not understand the filter".to_owned(),
            Self::Waiting => {
                "the searching game is kept for a set whose other game is not ready".to_owned()
            }
            Self::KeptFor(opponent) => {
                format!("the searching game is kept for a set against {opponent}")
            }
            Self::Kept => "kept for a set: only its opponent's search lists it".to_owned(),
            Self::Filter(failed) => failed.describe(seen(steering, session)),
        }
    }
}

/// Why a search with `filter` (none: not understood) and `steering` leaves
/// out a session of this game; none when it lists it.
fn skip<'f>(
    filter: Option<&'f Filter>,
    steering: &Steering,
    session: &Session,
) -> Option<Skip<'f>> {
    let Some(filter) = filter else {
        return Some(Skip::NotUnderstood);
    };
    match steering {
        Steering::Nothing => return Some(Skip::Waiting),
        Steering::Only { opponent, .. } if profile(session) != Some(*opponent) => {
            return Some(Skip::KeptFor(*opponent));
        }
        Steering::Unkept(kept) if profile(session).is_some_and(|p| kept.contains(&p)) => {
            return Some(Skip::Kept);
        }
        _ => {}
    }
    filter.failed(seen(steering, session)).map(Skip::Filter)
}

/// A session's fields as a search sees them: a kept game's search takes its
/// opponent's game as in its own region group (`CO`).
fn seen<'s>(steering: &'s Steering, session: &'s Session) -> impl Fn(&str) -> Option<&'s str> {
    move |name| match (name, steering) {
        (
            "CO",
            Steering::Only {
                region: Some(region),
                ..
            },
        ) => Some(region.as_str()),
        _ => session.field(name),
    }
}

/// A session's profile, from its heartbeat's `dwc_pid`.
fn profile(session: &Session) -> Option<i32> {
    session.field("dwc_pid")?.parse().ok()
}

/// Left-out games for the log: `<pid>: <why>; …`, `none` for none.
struct Misses<'m>(&'m [Miss]);

impl std::fmt::Display for Misses<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            return f.write_str("none");
        }
        for (at, miss) in self.0.iter().enumerate() {
            if at > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{}: {}", miss.profile_id, miss.why)?;
        }
        Ok(())
    }
}

/// Whether a ranked search by `searcher` with `filter` lists `session` now,
/// as the server browser would answer it with the games' `sessions` and
/// the sets' `pairings`: `None` when it does, else why not.
pub fn explain(
    pairings: &Pairings,
    sessions: &[(SocketAddr, Session)],
    searcher: i32,
    filter: &str,
    session: &Session,
) -> Option<String> {
    if session.field("gamename") != Some(crate::GAME_NAME) {
        return Some("a session of another game".to_owned());
    }
    let steering = Steering::search(pairings, Some(searcher), sessions);
    let filter = Filter::parse(filter);
    skip(filter.as_ref(), &steering, session).map(|skip| skip.describe(&steering, session))
}

/// Takes a NUL-terminated text off the front of `rest`.
fn text(rest: &mut &[u8]) -> Option<String> {
    let end = rest.iter().position(|&byte| byte == 0)?;
    let text = String::from_utf8_lossy(&rest[..end]).into_owned();
    *rest = &rest[end + 1..];
    Some(text)
}

/// A session in the list: its flags, its public address (where its
/// heartbeats come from), its local one, and the fields asked for.
fn server(address: SocketAddr, session: &Session, fields: &[&str]) -> Option<Vec<u8>> {
    let SocketAddr::V4(public) = address else {
        return None;
    };
    let local_ip: Ipv4Addr = session.field("localip0")?.parse().ok()?;
    let local_port: u16 = session.field("localport")?.parse().ok()?;
    let mut flags = HAS_KEYS | NONSTANDARD_PORT | PRIVATE_IP | NONSTANDARD_PRIVATE_PORT | ICMP_IP;
    if session.field("natneg").is_some_and(|natneg| natneg != "0") {
        flags |= CONNECT_NEGOTIATE;
    }
    let mut out = vec![flags];
    out.extend_from_slice(&public.ip().octets());
    out.extend_from_slice(&public.port().to_be_bytes());
    out.extend_from_slice(&local_ip.octets());
    out.extend_from_slice(&local_port.to_be_bytes());
    out.extend_from_slice(&[0; 4]); // No ICMP address.
    for field in fields {
        out.push(STRING_VALUE);
        out.extend_from_slice(session.field(field).unwrap_or_default().as_bytes());
        out.push(0);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The port's request for its host's session, as the server received it.
    const LOOKUP: &[u8] = b"\x00\x8d\x00\x01\x03\x00\x00\x00\x00mschargedwii\x00mschargedwii\x00Y];98goldwc_pid = 1000000000\x00\\numplayers\\maxplayers\\dwc_pid\\dwc_mtype\\dwc_mresv\\dwc_mver\\dwc_eval\x00\x00\x00\x00\x80\x06\x00\x00\x00";
    /// A ranked search, as player 1 sent it.
    const RANKED: &str = "dwc_mver = 3 and dwc_pid != 1000000000 and maxplayers = 1 and numplayers < 1 and dwc_mtype = 0 and dwc_mresv != dwc_pid and (AP = 463715253 and GT = 0 and CO = 0)";
    /// Player 2's heartbeat while it searched.
    const SEARCHING: &[u8] = b"\x03\x5c\xd6s\x9flocalip0\x00192.168.1.131\x00localport\x0053103\x00natneg\x001\x00statechanged\x001\x00gamename\x00mschargedwii\x00publicip\x00-1062731389\x00publicport\x0053103\x00numplayers\x000\x00maxplayers\x001\x00dwc_pid\x001000000001\x00dwc_mtype\x000\x00dwc_mresv\x000\x00dwc_mver\x003\x00dwc_eval\x001\x00GT\x000\x00PI\x001000000001\x00AP\x00463715253\x00PT\x000\x00CO\x000\x00\x00\x00\x00\x00\x00\x00\x00";
    /// The host's heartbeat once its friend reserved a place.
    const HOSTING: &[u8] = b"\x03-\x1e\xa3\x05localip0\x00192.168.1.131\x00localport\x0051021\x00natneg\x001\x00statechanged\x001\x00gamename\x00mschargedwii\x00publicip\x00-1408172031\x00publicport\x0044719\x00numplayers\x000\x00maxplayers\x001\x00dwc_pid\x001000000000\x00dwc_mtype\x002\x00dwc_mresv\x001000000001\x00dwc_mver\x003\x00dwc_eval\x001\x00\x00\x00\x00\x00\x00\x00\x00";

    fn host() -> SocketAddr {
        "172.17.0.1:44719".parse().unwrap()
    }

    fn client() -> SocketAddr {
        "172.17.0.1:37418".parse().unwrap()
    }

    #[test]
    fn list_requests_are_read() {
        let request = ListRequest::parse(LOOKUP).unwrap();
        assert_eq!(request.game, "mschargedwii");
        assert_eq!(&request.challenge, b"Y];98gol");
        assert_eq!(request.filter, "dwc_pid = 1000000000");
        assert_eq!(
            request.fields,
            [
                "numplayers",
                "maxplayers",
                "dwc_pid",
                "dwc_mtype",
                "dwc_mresv",
                "dwc_mver",
                "dwc_eval"
            ]
        );
        assert_eq!(request.options, 0x80);
    }

    #[test]
    fn a_lookup_lists_the_hosts_session() {
        let master = Master::default();
        master.answer(HOSTING, host());
        let (reply, found) = ListRequest::parse(LOOKUP)
            .unwrap()
            .reply(&master.sessions(), client(), &Steering::None, None)
            .unwrap();
        assert_eq!((found.count, found.profiles), (1, vec![1000000000]));

        let mut expected = vec![172, 17, 0, 1, 0x92, 0x2A, 7];
        for field in [
            "numplayers",
            "maxplayers",
            "dwc_pid",
            "dwc_mtype",
            "dwc_mresv",
            "dwc_mver",
            "dwc_eval",
        ] {
            expected.push(0);
            expected.extend_from_slice(field.as_bytes());
            expected.push(0);
        }
        expected.push(0);
        // Every flag but unsolicited UDP and full rules: 0x7E.
        expected.extend_from_slice(&[
            0x7E, 172, 17, 0, 1, 0xAE, 0xAF, 192, 168, 1, 131, 0xC7, 0x4D, 0, 0, 0, 0,
        ]);
        for value in ["0", "1", "1000000000", "2", "1000000001", "3", "1"] {
            expected.push(0xFF);
            expected.extend_from_slice(value.as_bytes());
            expected.push(0);
        }
        expected.extend_from_slice(&LAST_SERVER);
        assert_eq!(reply, expected);
    }

    #[test]
    fn a_ranked_search_lists_the_other_searching_game() {
        let master = Master::default();
        let searching: SocketAddr = "192.168.1.131:53103".parse().unwrap();
        master.answer(SEARCHING, searching);
        master.answer(HOSTING, host());
        let request = ListRequest {
            filter: RANKED.to_owned(),
            ..ListRequest::parse(LOOKUP).unwrap()
        };
        let sessions = master.sessions();
        let found = request.matches(&sessions, &Steering::None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, searching);

        // The same search from player 2 finds nothing: it skips itself.
        let own = ListRequest {
            filter: RANKED.replace("1000000000", "1000000001"),
            ..request
        };
        assert_eq!(own.matches(&sessions, &Steering::None).len(), 0);
    }

    /// `bytes` with each `from` replaced by `to`, of the same length.
    fn replace(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
        let mut out = bytes.to_vec();
        let mut at = 0;
        while let Some(found) = out[at..]
            .windows(from.len())
            .position(|window| window == from)
        {
            out[at + found..at + found + to.len()].copy_from_slice(to);
            at += found + to.len();
        }
        out
    }

    #[test]
    fn games_kept_for_a_match_see_only_each_other() {
        let master = Master::default();
        // Player 2 searching, as captured; player 3 searching in the same
        // region group; player 1 searching in another one.
        let player_2: SocketAddr = "192.168.1.131:53103".parse().unwrap();
        master.answer(SEARCHING, player_2);
        let player_3: SocketAddr = "192.168.1.133:53105".parse().unwrap();
        master.answer(&replace(SEARCHING, b"1000000001", b"1000000002"), player_3);
        let player_1: SocketAddr = "192.168.1.132:53104".parse().unwrap();
        let own = replace(
            &replace(SEARCHING, b"1000000001", b"1000000000"),
            b"CO\x000",
            b"CO\x002",
        );
        master.answer(&own, player_1);
        let sessions = master.sessions();
        let found = |profile_id: i32, pairings: &Pairings| -> Vec<SocketAddr> {
            let region = if profile_id == 1000000000 {
                "CO = 2"
            } else {
                "CO = 0"
            };
            let request = ListRequest {
                filter: RANKED
                    .replace("1000000000", &profile_id.to_string())
                    .replace("CO = 0", region),
                ..ListRequest::parse(LOOKUP).unwrap()
            };
            let steering = Steering::search(pairings, Some(profile_id), &sessions);
            let mut found: Vec<SocketAddr> = request
                .matches(&sessions, &steering)
                .into_iter()
                .map(|(address, _)| address)
                .collect();
            found.sort();
            found
        };

        // Without matches, the region groups keep them apart.
        let pairings = Pairings::default();
        assert!(found(1000000000, &pairings).is_empty());
        assert_eq!(found(1000000002, &pairings), [player_2]);

        // Player 2 is ready for a match whose other game is not: nobody
        // finds it, and it finds nobody.
        pairings.ready(7, 0, 1000000001, "R4QE");
        assert!(found(1000000002, &pairings).is_empty());
        assert!(found(1000000001, &pairings).is_empty());

        // Player 1, its opponent, is ready too: they find each other across
        // the region groups, and player 3 still finds neither.
        pairings.ready(7, 1, 1000000000, "R4QE");
        assert_eq!(found(1000000000, &pairings), [player_2]);
        assert_eq!(found(1000000001, &pairings), [player_1]);
        assert!(found(1000000002, &pairings).is_empty());

        pairings.end(7);
        assert_eq!(found(1000000002, &pairings), [player_2]);
    }

    #[test]
    fn a_search_says_why_it_left_out_the_other_searching_games() {
        let master = Master::default();
        master.answer(
            &replace(SEARCHING, b"CO\x000", b"CO\x001"),
            "192.168.1.131:53103".parse().unwrap(),
        );
        // A host is in no ranked search: no reason for it.
        master.answer(HOSTING, host());
        let sessions = master.sessions();
        let request = ListRequest {
            filter: RANKED.to_owned(),
            ..ListRequest::parse(LOOKUP).unwrap()
        };
        let listed = |steering: &Steering| {
            request
                .reply(&sessions, client(), steering, Some(1000000000))
                .unwrap()
                .1
        };

        let free = listed(&Steering::Unkept(Vec::new()));
        assert_eq!((free.count, free.profiles.len()), (0, 0));
        assert_eq!(
            free.missed,
            [Miss {
                profile_id: 1000000001,
                why: "CO = 0 (CO is 1)".to_owned(),
            }]
        );
        let kept = listed(&Steering::Unkept(vec![1000000001]));
        assert_eq!(
            kept.missed[0].why,
            "kept for a set: only its opponent's search lists it"
        );
        let waiting = listed(&Steering::Nothing);
        assert_eq!(
            waiting.missed[0].why,
            "the searching game is kept for a set whose other game is not ready"
        );

        // A lookup of one host says nothing about the others.
        let lookup = ListRequest::parse(LOOKUP).unwrap();
        let (_, found) = lookup
            .reply(&sessions, client(), &Steering::None, None)
            .unwrap();
        assert!(found.missed.is_empty());
    }

    #[test]
    fn searches_are_by_the_profile_they_leave_out() {
        assert_eq!(searcher(RANKED), Some(Some(1000000000)));
        assert_eq!(searcher("dwc_pid = 1000000000"), None);
        assert_eq!(searcher(" numplayers < 2 "), Some(None));
    }

    #[test]
    fn match_commands_give_their_command_and_sender() {
        // A TELL_ADDR from profile 1000000001, as the port sent it.
        let message = b"SBCM\x03\x00\x00\x00\x06\x08\x5b\xa3\xac\x11\x00\x01\x01\xca\x9a\x3b\xc0\xa8\x01\x83\x2a\xdf\x00\x00";
        assert_eq!(match_command(message), Some((0x06, Some(1000000001))));
        assert_eq!(match_command(b"\xfd\xfc\x1e\x66\x6a\xb2\x03\x00"), None);
        assert_eq!(match_command(b"SBCM\x03"), None);
    }

    #[test]
    fn other_filters_find_nothing() {
        let master = Master::default();
        master.answer(HOSTING, host());
        let request = ListRequest::parse(LOOKUP).unwrap();
        for filter in [
            "dwc_pid = 1000000001",
            "dwc_mver = 3 and dwc_mtype = 0",
            "dwc_pid =",
        ] {
            let request = ListRequest {
                filter: filter.to_owned(),
                ..ListRequest::parse(LOOKUP).unwrap()
            };
            let (reply, _) = request
                .reply(&master.sessions(), client(), &Steering::None, None)
                .unwrap();
            assert!(reply.ends_with(&[0, 0, 0xFF, 0xFF, 0xFF, 0xFF]), "{filter}");
            assert_eq!(
                request.matches(&master.sessions(), &Steering::None).len(),
                0,
                "{filter}"
            );
        }
        let other_game = ListRequest {
            game: "othergame".to_owned(),
            ..request
        };
        assert_eq!(
            other_game
                .reply(&master.sessions(), client(), &Steering::None, None)
                .map(|(reply, _)| reply),
            None
        );
    }
}

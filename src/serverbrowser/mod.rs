//! The server browser (`<game>.ms11.gs.nintendowifi.net`, TCP 28910): lists
//! the master server's sessions that pass a game's filter. A friend who got a
//! place in a host's group asks for the host's session by profile
//! (`dwc_pid = <pid>`) to learn its addresses; a ranked search asks for every
//! game searching like it (`filter.rs`). The game then sends the session
//! messages through the server browser, which the master server passes on
//! (docs/login.md). Other requests are read and not answered.

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

use crate::master::{Master, Session};
use crypt::CHALLENGE_LEN;
use filter::Filter;

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
/// List request options.
const NO_SERVER_LIST: u32 = 0x02;

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

pub async fn serve(master: Arc<Master>, listener: TcpListener) -> std::io::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let master = master.clone();
        tokio::spawn(async move {
            if let Err(error) = connection(master, stream, peer).await {
                debug!(target: "serverbrowser", %peer, %error, "connection failed");
            }
        });
    }
}

async fn connection(
    master: Arc<Master>,
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
            if let Some(reply) = answer(&master, &request, peer) {
                stream.write_all(&reply).await?;
            }
        }
        if buffer.len() > MAX_REQUEST {
            return Ok(());
        }
    }
}

fn answer(master: &Arc<Master>, request: &[u8], peer: SocketAddr) -> Option<Vec<u8>> {
    match request[2] {
        LIST_REQUEST => {
            let list = ListRequest::parse(request)?;
            let reply = list.reply(&master.sessions(), peer)?;
            Some(crypt::encrypt(
                SECRET_KEY,
                &list.challenge,
                rand::random(),
                &reply,
            ))
        }
        SEND_MESSAGE_REQUEST => {
            send_message(master, request, peer);
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
fn send_message(master: &Arc<Master>, request: &[u8], peer: SocketAddr) {
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
    let master = master.clone();
    let message = message.to_vec();
    tokio::spawn(async move { master.send_client_message(to, &message).await });
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
    /// names, no popular values, the matching sessions, and the end.
    fn reply(&self, sessions: &[(SocketAddr, Session)], peer: SocketAddr) -> Option<Vec<u8>> {
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
            return Some(out);
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

        let found = self.matches(sessions);
        info!(target: "serverbrowser", %peer, filter = self.filter, found = found.len(), "list");
        for (address, session) in found {
            if let Some(server) = server(address, session, &fields) {
                out.extend_from_slice(&server);
            }
        }
        out.extend_from_slice(&LAST_SERVER);
        Some(out)
    }

    /// The game's sessions that pass the filter; none for a filter it does
    /// not understand.
    fn matches<'a>(&self, sessions: &'a [(SocketAddr, Session)]) -> Vec<(SocketAddr, &'a Session)> {
        let Some(filter) = Filter::parse(&self.filter) else {
            debug!(target: "serverbrowser", filter = self.filter, "filter not understood");
            return Vec::new();
        };
        sessions
            .iter()
            .filter(|(_, session)| {
                session.field("gamename") == Some(crate::GAME_NAME)
                    && filter.matches(|name| session.field(name))
            })
            .map(|(address, session)| (*address, session))
            .collect()
    }
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
        let reply = ListRequest::parse(LOOKUP)
            .unwrap()
            .reply(&master.sessions(), client())
            .unwrap();

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
        let found = request.matches(&sessions);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, searching);

        // The same search from player 2 finds nothing: it skips itself.
        let own = ListRequest {
            filter: RANKED.replace("1000000000", "1000000001"),
            ..request
        };
        assert_eq!(own.matches(&sessions).len(), 0);
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
            let reply = request.reply(&master.sessions(), client()).unwrap();
            assert!(reply.ends_with(&[0, 0, 0xFF, 0xFF, 0xFF, 0xFF]), "{filter}");
            assert_eq!(request.matches(&master.sessions()).len(), 0, "{filter}");
        }
        let other_game = ListRequest {
            game: "othergame".to_owned(),
            ..request
        };
        assert_eq!(other_game.reply(&master.sessions(), client()), None);
    }
}

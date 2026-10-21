//! The QR2 master server (`<game>.available.gs.nintendowifi.net` and
//! `<game>.master.gs.nintendowifi.net`, UDP 27900). It answers the
//! availability check DWC makes before it logs in, and challenges the
//! heartbeats a logged-in game sends: without a challenge DWC gives up after
//! four heartbeats, 10 seconds apart, with error 94020. Each game's last
//! heartbeat is its session, which a game looking for a match asks the server
//! browser for (docs/login.md).

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::net::UdpSocket;
use tracing::{debug, trace};

/// Packet types: the first byte of the game's packets, the third of the
/// server's (after `FE FD`). The game answers a challenge with one of its own.
const CHALLENGE: u8 = 0x01;
const HEARTBEAT: u8 = 0x03;
const KEEPALIVE: u8 = 0x08;
/// The availability check: `09 00 00 00 00`, then the game name and a NUL.
const AVAILABLE: u8 = 0x09;

/// A heartbeat's `statechanged` when the game stops being listed.
const SHUTDOWN: &str = "2";
/// A game sends a keepalive every 20 seconds and a heartbeat every minute;
/// one not heard from for this long has gone.
const SESSION_TTL: Duration = Duration::from_secs(90);
/// Sessions kept at most. A heartbeat from yet another address is still
/// challenged, but not kept.
const MAX_SESSIONS: usize = 4096;

/// The master server's state: the games' sessions, by the address their
/// heartbeats come from.
#[derive(Default)]
pub struct Master {
    sessions: Mutex<HashMap<SocketAddr, Session>>,
}

/// A game's session: its instance key and its last heartbeat's server fields.
#[derive(Clone, Debug)]
pub struct Session {
    pub key: [u8; 4],
    /// As the heartbeat sent them: `localip0`, `publicport`, `dwc_pid`, …
    pub fields: Vec<(String, String)>,
    /// Whether the game has answered a challenge.
    pub listed: bool,
    seen: Instant,
}

impl Session {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    }

    fn live(&self, now: Instant) -> bool {
        now.duration_since(self.seen) < SESSION_TTL
    }
}

impl Master {
    pub async fn serve(self: Arc<Self>, socket: UdpSocket) -> std::io::Result<()> {
        let mut packet = [0; 1500];
        loop {
            let (len, peer) = socket.recv_from(&mut packet).await?;
            trace!(target: "raw::master", %peer, packet = ?&packet[..len], "packet");
            if let Some(reply) = self.answer(&packet[..len], peer) {
                trace!(target: "raw::master", %peer, ?reply, "reply");
                if let Err(error) = socket.send_to(&reply, peer).await {
                    debug!(target: "master", %peer, %error, "reply failed");
                }
            }
        }
    }

    /// The sessions of the games heard from lately.
    pub fn sessions(&self) -> Vec<(SocketAddr, Session)> {
        let now = Instant::now();
        let sessions = self.sessions.lock().unwrap();
        sessions
            .iter()
            .filter(|(_, session)| session.live(now))
            .map(|(&peer, session)| (peer, session.clone()))
            .collect()
    }

    /// The reply to a packet from `peer`, if any.
    pub(crate) fn answer(&self, packet: &[u8], peer: SocketAddr) -> Option<Vec<u8>> {
        let (&kind, rest) = packet.split_first()?;
        // The game's instance key, which replies carry.
        let (key, body) = rest.split_first_chunk::<4>()?;
        match kind {
            AVAILABLE => {
                let game = body.strip_suffix(b"\0")?;
                // Flags 0: up (1 out of service, 2 maintenance).
                (game == crate::GAME_NAME.as_bytes()).then(|| reply(AVAILABLE, &[0; 4]))
            }
            HEARTBEAT => self.heartbeat(key, body, peer),
            CHALLENGE => {
                if self.seen(key, peer, true) {
                    let sessions = self.sessions.lock().unwrap().len();
                    debug!(target: "master", %peer, sessions, "listed");
                }
                None
            }
            KEEPALIVE => {
                self.seen(key, peer, false);
                Some(reply(KEEPALIVE, key))
            }
            _ => {
                debug!(target: "master", %peer, kind, "not implemented yet");
                None
            }
        }
    }

    /// A heartbeat is kept as the game's session and gets a challenge: 6
    /// random letters, the backend options (`00`: no query challenges) and
    /// the address the heartbeat came from as `%08X%04X`, which is how the
    /// game learns its public address. The game answers with the challenge
    /// encrypted with its secret key, which is not checked.
    fn heartbeat(&self, key: &[u8; 4], body: &[u8], peer: SocketAddr) -> Option<Vec<u8>> {
        let fields: Vec<(String, String)> = fields(body)
            .map(|(name, value)| {
                (
                    String::from_utf8_lossy(name).into_owned(),
                    String::from_utf8_lossy(value).into_owned(),
                )
            })
            .collect();
        let field = |name: &str| {
            fields
                .iter()
                .find(|(field, _)| field == name)
                .map(|(_, value)| value.as_str())
        };
        if field("gamename") != Some(crate::GAME_NAME) {
            debug!(target: "master", %peer, "heartbeat of another game");
            return None;
        }
        if field("statechanged") == Some(SHUTDOWN) {
            self.sessions.lock().unwrap().remove(&peer);
            debug!(target: "master", %peer, "unlisted");
            return None;
        }
        let SocketAddr::V4(address) = peer else {
            return None;
        };
        debug!(
            target: "master",
            %peer,
            pid = field("dwc_pid"),
            mtype = field("dwc_mtype"),
            mresv = field("dwc_mresv"),
            "heartbeat"
        );
        self.keep(key, fields, peer);

        let letters: String = (0..6)
            .map(|_| char::from(rand::random_range(b'A'..=b'Z')))
            .collect();
        let challenge = format!(
            "{letters}00{:08X}{:04X}",
            address.ip().to_bits(),
            address.port()
        );
        let mut reply = reply(CHALLENGE, key);
        reply.extend_from_slice(challenge.as_bytes());
        reply.push(0);
        Some(reply)
    }

    /// Keeps a heartbeat's fields as the session of `peer`. Another instance
    /// key is another game there, which starts unlisted.
    fn keep(&self, key: &[u8; 4], fields: Vec<(String, String)>, peer: SocketAddr) {
        let now = Instant::now();
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.len() >= MAX_SESSIONS && !sessions.contains_key(&peer) {
            sessions.retain(|_, session| session.live(now));
            if sessions.len() >= MAX_SESSIONS {
                debug!(target: "master", %peer, "too many sessions");
                return;
            }
        }
        let listed = sessions
            .get(&peer)
            .is_some_and(|session| session.key == *key && session.listed);
        sessions.insert(
            peer,
            Session {
                key: *key,
                fields,
                listed,
                seen: now,
            },
        );
    }

    /// A keepalive or a challenge's answer from the session's game: it is
    /// still there, and `listed` once it answered. Whether it was the
    /// session's.
    fn seen(&self, key: &[u8; 4], peer: SocketAddr, listed: bool) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions
            .get_mut(&peer)
            .filter(|session| session.key == *key)
        else {
            return false;
        };
        session.seen = Instant::now();
        session.listed |= listed;
        true
    }
}

/// A heartbeat's server fields: `name\0value\0` up to an empty name, after
/// which come the player and team fields.
fn fields(body: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    let mut parts = body.split(|&byte| byte == 0);
    std::iter::from_fn(move || {
        let name = parts.next().filter(|name| !name.is_empty())?;
        Some((name, parts.next()?))
    })
}

fn reply(kind: u8, key: &[u8; 4]) -> Vec<u8> {
    let mut reply = vec![0xFE, 0xFD, kind];
    reply.extend_from_slice(key);
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The port's first heartbeat after logging in, as the server received it.
    const HEARTBEAT_PACKET: &[u8] = b"\x03*\x8fPMlocalip0\x00192.168.1.131\x00localport\x0064244\x00natneg\x001\x00statechanged\x001\x00gamename\x00mschargedwii\x00publicip\x000\x00publicport\x000\x00numplayers\x000\x00maxplayers\x000\x00unknown\x001000000000\x00unknown\x000\x00unknown\x000\x00unknown\x003\x00unknown\x001\x00\x00\x00\x00\x00\x00\x00\x00";
    /// A host's heartbeat once a friend reserved a place (`dwc_mresv`).
    const HOSTING_PACKET: &[u8] = b"\x03-\x1e\xa3\x05localip0\x00192.168.1.131\x00localport\x0051021\x00natneg\x001\x00statechanged\x001\x00gamename\x00mschargedwii\x00publicip\x00-1408172031\x00publicport\x0044719\x00numplayers\x000\x00maxplayers\x001\x00dwc_pid\x001000000000\x00dwc_mtype\x002\x00dwc_mresv\x001000000001\x00dwc_mver\x003\x00dwc_eval\x001\x00\x00\x00\x00\x00\x00\x00\x00";

    fn peer() -> SocketAddr {
        "203.0.113.7:64244".parse().unwrap()
    }

    #[test]
    fn the_game_is_available() {
        assert_eq!(
            Master::default()
                .answer(b"\x09\0\0\0\0mschargedwii\0", peer())
                .unwrap(),
            b"\xFE\xFD\x09\0\0\0\0"
        );
    }

    #[test]
    fn a_heartbeat_is_challenged_with_the_public_address() {
        let reply = Master::default().answer(HEARTBEAT_PACKET, peer()).unwrap();
        let (header, challenge) = reply.split_at(7);
        assert_eq!(header, b"\xFE\xFD\x01*\x8fPM");
        let challenge = challenge.strip_suffix(b"\0").unwrap();
        let (letters, rest) = challenge.split_at(6);
        assert!(letters.iter().all(u8::is_ascii_uppercase), "{challenge:?}");
        // 203.0.113.7, port 64244.
        assert_eq!(rest, b"00CB007107FAF4");
    }

    #[test]
    fn a_heartbeat_is_the_session_until_the_game_stops() {
        let master = Master::default();
        master.answer(HOSTING_PACKET, peer()).unwrap();
        let [(address, session)] = &master.sessions()[..] else {
            panic!("one session");
        };
        assert_eq!(*address, peer());
        assert_eq!(session.key, *b"-\x1e\xa3\x05");
        assert_eq!(session.field("dwc_pid"), Some("1000000000"));
        assert_eq!(session.field("dwc_mresv"), Some("1000000001"));
        assert_eq!(session.field("localip0"), Some("192.168.1.131"));
        assert!(!session.listed);

        // Its challenge's answer lists it; another key's does not.
        master.answer(b"\x01*\x8fPMresponse\0", peer());
        assert!(!master.sessions()[0].1.listed);
        master.answer(b"\x01-\x1e\xa3\x05response\0", peer());
        assert!(master.sessions()[0].1.listed);
        // The next heartbeat keeps it listed.
        master.answer(HOSTING_PACKET, peer());
        assert!(master.sessions()[0].1.listed);

        let mut shutdown = HOSTING_PACKET.to_vec();
        let at = shutdown
            .windows(14)
            .position(|field| field == b"statechanged\x001")
            .unwrap();
        shutdown[at + 13] = b'2';
        assert_eq!(master.answer(&shutdown, peer()), None);
        assert!(master.sessions().is_empty());
    }

    #[test]
    fn sessions_not_heard_from_go() {
        let master = Master::default();
        master.answer(HOSTING_PACKET, peer());
        let long_ago = Instant::now().checked_sub(SESSION_TTL).unwrap();
        master
            .sessions
            .lock()
            .unwrap()
            .get_mut(&peer())
            .unwrap()
            .seen = long_ago;
        assert!(master.sessions().is_empty());

        // A keepalive of its game keeps it.
        master.answer(b"\x08-\x1e\xa3\x05", peer());
        assert_eq!(master.sessions().len(), 1);
    }

    #[test]
    fn keepalives_are_answered() {
        assert_eq!(
            Master::default().answer(b"\x08*\x8fPM", peer()).unwrap(),
            b"\xFE\xFD\x08*\x8fPM"
        );
    }

    #[test]
    fn other_packets_are_not_answered() {
        let master = Master::default();
        for packet in [
            &b""[..],
            b"\x03*\x8f",
            b"\x09\0\0\0\0othergame\0",
            b"\x09\0\0\0\0mschargedwii",
            b"\x03*\x8fPMgamename\0othergame\0\0",
            b"\x01*\x8fPMresponse\0",
            b"\x07*\x8fPM",
        ] {
            assert_eq!(master.answer(packet, peer()), None, "{packet:?}");
        }
        assert!(master.sessions().is_empty());
    }
}

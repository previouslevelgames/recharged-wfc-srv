//! NAT negotiation (`natneg1/2/3.gs.nintendowifi.net`, UDP 27901): tells two
//! games that want to play their public addresses, so they can open their
//! routers to each other (docs/login.md). `P/src/RVL_SDK/gamespy/natneg/
//! natneg.c` is the client, protocol version 3.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::net::UdpSocket;
use tracing::{debug, info, trace};

use crate::activity::Activity;

/// Every packet starts with this, then the version, the type and the cookie
/// the two games share (big-endian).
const MAGIC: [u8; 6] = [0xFD, 0xFC, 0x1E, 0x66, 0x6A, 0xB2];
const HEADER_LEN: usize = 12;
/// Packet types (`nninternal.h`).
const INIT: u8 = 0x00;
const INIT_ACK: u8 = 0x01;
const CONNECT: u8 = 0x05;
const CONNECT_ACK: u8 = 0x06;
const REPORT: u8 = 0x0D;
const REPORT_ACK: u8 = 0x0E;
/// An INIT's port: the game's own socket, or one of the three of the
/// negotiation socket.
const GAME_PORT: u8 = 0;
/// A CONNECT is sent again this often until its game acks it.
const CONNECT_RETRY: Duration = Duration::from_millis(500);
const CONNECT_TRIES: u32 = 20;
/// A negotiation older than this is over.
const SESSION_TTL: Duration = Duration::from_secs(30);
/// Negotiations kept at most.
const MAX_SESSIONS: usize = 4096;

/// The negotiations under way, by cookie.
#[derive(Default)]
pub struct Natneg {
    sessions: Mutex<HashMap<u32, Session>>,
    activity: Arc<Activity>,
}

struct Session {
    version: u8,
    started: Instant,
    /// The two games, by their client index (0 and 1).
    clients: HashMap<u8, Client>,
    /// Whether the CONNECTs went out.
    connecting: bool,
}

#[derive(Default)]
struct Client {
    /// Where its negotiation socket's INITs come from: CONNECTs go there.
    negotiate: Option<SocketAddr>,
    /// Where its game socket's INIT comes from: the other game pings it.
    game: Option<SocketAddr>,
    acked: bool,
}

impl Client {
    fn mapped(&self) -> bool {
        self.negotiate.is_some() && self.game.is_some()
    }
}

impl Natneg {
    pub fn new(activity: Arc<Activity>) -> Self {
        Self {
            sessions: Mutex::default(),
            activity,
        }
    }

    pub async fn serve(self: Arc<Self>, socket: UdpSocket) -> std::io::Result<()> {
        let socket = Arc::new(socket);
        let mut packet = [0; 512];
        loop {
            let (len, peer) = socket.recv_from(&mut packet).await?;
            let packet = &packet[..len];
            trace!(target: "raw::natneg", %peer, ?packet, "packet");
            for (to, reply) in self.answer(packet, peer) {
                trace!(target: "raw::natneg", %to, ?reply, "reply");
                if let Err(error) = socket.send_to(&reply, to).await {
                    debug!(target: "natneg", %to, %error, "reply failed");
                }
            }
            if let Some(cookie) = self.ready(packet) {
                tokio::spawn(self.clone().connect(socket.clone(), cookie));
            }
        }
    }

    /// The replies to a packet from `peer`, and where they go.
    fn answer(&self, packet: &[u8], peer: SocketAddr) -> Vec<(SocketAddr, Vec<u8>)> {
        let Some((version, kind, cookie, body)) = parse(packet) else {
            debug!(target: "natneg", %peer, "not a NAT negotiation packet");
            return Vec::new();
        };
        match kind {
            INIT => self.init(version, cookie, body, peer),
            CONNECT_ACK => {
                let index = body.get(1).copied();
                let mut sessions = self.sessions.lock().unwrap();
                if let Some(client) =
                    index.and_then(|index| sessions.get_mut(&cookie)?.clients.get_mut(&index))
                {
                    client.acked = true;
                }
                Vec::new()
            }
            REPORT => {
                // The game's result: 1 when the two reached each other.
                let (index, result) = (body.get(1), body.get(2));
                info!(target: "natneg", cookie = format!("{cookie:08x}"), %peer, ?index, ?result, "report");
                self.activity
                    .report(cookie, index.copied(), result.copied());
                let mut reply = header(version, REPORT_ACK, cookie);
                let mut echo = [0; 9];
                let len = body.len().min(echo.len());
                echo[..len].copy_from_slice(&body[..len]);
                echo[2] = 0;
                reply.extend_from_slice(&echo);
                vec![(peer, reply)]
            }
            kind => {
                debug!(target: "natneg", %peer, kind, "not implemented yet");
                Vec::new()
            }
        }
    }

    /// `INIT`: one of a game's sockets, with its port type, its client
    /// index, whether it negotiates for its game socket, and its local
    /// address. Each gets an `INIT_ACK`.
    fn init(
        &self,
        version: u8,
        cookie: u32,
        body: &[u8],
        peer: SocketAddr,
    ) -> Vec<(SocketAddr, Vec<u8>)> {
        let (Some(&port_type), Some(&index), Some(&use_game_port)) =
            (body.first(), body.get(1), body.get(2))
        else {
            return Vec::new();
        };
        if port_type > 3 || index > 1 {
            debug!(target: "natneg", %peer, port_type, index, "bad init");
            return Vec::new();
        }
        let mut sessions = self.sessions.lock().unwrap();
        let now = Instant::now();
        if sessions.len() >= MAX_SESSIONS && !sessions.contains_key(&cookie) {
            sessions.retain(|_, session| now.duration_since(session.started) < SESSION_TTL);
            if sessions.len() >= MAX_SESSIONS {
                return Vec::new();
            }
        }
        let session = sessions.entry(cookie).or_insert_with(|| {
            debug!(target: "natneg", cookie = format!("{cookie:08x}"), "negotiation");
            Session {
                version,
                started: now,
                clients: HashMap::new(),
                connecting: false,
            }
        });
        let client = session.clients.entry(index).or_default();
        if port_type != GAME_PORT {
            client.negotiate = Some(peer);
        }
        if use_game_port == 0 || port_type == GAME_PORT {
            client.game = Some(peer);
        }
        self.activity.negotiation(cookie, client.game);

        let mut ack = header(version, INIT_ACK, cookie);
        ack.extend_from_slice(&[port_type, index, 0xFF, 0xFF, 0x6D, 0x16, 0xB5, 0x7D, 0xEA]);
        vec![(peer, ack)]
    }

    /// The cookie of a negotiation whose two games are both known now, once.
    fn ready(&self, packet: &[u8]) -> Option<u32> {
        let (_, INIT, cookie, _) = parse(packet)? else {
            return None;
        };
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.get_mut(&cookie)?;
        let both = session.clients.len() == 2 && session.clients.values().all(Client::mapped);
        if !both || session.connecting {
            return None;
        }
        session.connecting = true;
        Some(cookie)
    }

    /// Sends each game of the negotiation a `CONNECT` with the other's game
    /// address, again until it acks.
    async fn connect(self: Arc<Self>, socket: Arc<UdpSocket>, cookie: u32) {
        info!(target: "natneg", cookie = format!("{cookie:08x}"), "connect");
        self.activity.connected(cookie);
        for _ in 0..CONNECT_TRIES {
            let packets = {
                let sessions = self.sessions.lock().unwrap();
                let Some(session) = sessions.get(&cookie) else {
                    return;
                };
                connects(session, cookie)
            };
            if packets.is_empty() {
                return;
            }
            for (to, packet) in packets {
                trace!(target: "raw::natneg", %to, ?packet, "connect");
                let _ = socket.send_to(&packet, to).await;
            }
            tokio::time::sleep(CONNECT_RETRY).await;
        }
        debug!(target: "natneg", cookie = format!("{cookie:08x}"), "connect not acked");
    }
}

/// The `CONNECT`s still unacked: to a game's negotiation socket, the other
/// game's public address (IP and port, big-endian), "got your data" and
/// "finished" (no error).
fn connects(session: &Session, cookie: u32) -> Vec<(SocketAddr, Vec<u8>)> {
    let (Some(first), Some(second)) = (session.clients.get(&0), session.clients.get(&1)) else {
        return Vec::new();
    };
    [(first, second), (second, first)]
        .into_iter()
        .filter(|(to, _)| !to.acked)
        .filter_map(|(to, other)| {
            let (Some(to), Some(SocketAddr::V4(other))) = (to.negotiate, other.game) else {
                return None;
            };
            let mut packet = header(session.version, CONNECT, cookie);
            packet.extend_from_slice(&other.ip().octets());
            packet.extend_from_slice(&other.port().to_be_bytes());
            packet.extend_from_slice(&[0x42, 0]);
            Some((to, packet))
        })
        .collect()
}

/// A packet's version, type, cookie and body.
fn parse(packet: &[u8]) -> Option<(u8, u8, u32, &[u8])> {
    if packet.len() < HEADER_LEN || packet[..6] != MAGIC {
        return None;
    }
    let cookie = u32::from_be_bytes(packet[8..12].try_into().ok()?);
    Some((packet[6], packet[7], cookie, &packet[HEADER_LEN..]))
}

fn header(version: u8, kind: u8, cookie: u32) -> Vec<u8> {
    let mut packet = MAGIC.to_vec();
    packet.extend_from_slice(&[version, kind]);
    packet.extend_from_slice(&cookie.to_be_bytes());
    packet
}

/// A local address as an `INIT` carries it, for the tests.
#[cfg(test)]
fn local(address: std::net::SocketAddrV4) -> [u8; 6] {
    let mut local = [0; 6];
    local[..4].copy_from_slice(&address.ip().octets());
    local[4..].copy_from_slice(&address.port().to_be_bytes());
    local
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use super::*;

    const COOKIE: u32 = 0x1234_5678;

    fn init(port_type: u8, index: u8) -> Vec<u8> {
        let mut packet = header(3, INIT, COOKIE);
        packet.extend_from_slice(&[port_type, index, 1]);
        packet.extend_from_slice(&local(SocketAddrV4::new(
            Ipv4Addr::new(192, 168, 1, 131),
            51021,
        )));
        packet.extend_from_slice(b"mschargedwii\0");
        packet
    }

    fn address(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    #[test]
    fn inits_are_acked_with_their_port_type() {
        let natneg = Natneg::default();
        let replies = natneg.answer(&init(2, 1), address("203.0.113.7:40000"));
        let [(to, ack)] = &replies[..] else {
            panic!("one ack");
        };
        assert_eq!(*to, address("203.0.113.7:40000"));
        assert_eq!(ack[..12], header(3, INIT_ACK, COOKIE)[..]);
        assert_eq!(ack[12..14], [2, 1]);
        assert_eq!(ack.len(), 21);
    }

    #[test]
    fn the_two_games_get_each_others_game_address() {
        let natneg = Natneg::default();
        // Each game: its game socket, then its negotiation socket's three.
        for (index, game, negotiate) in [
            (0, "203.0.113.7:51021", "203.0.113.7:40000"),
            (1, "198.51.100.9:52000", "198.51.100.9:41000"),
        ] {
            assert_eq!(natneg.ready(&init(0, index)), None);
            natneg.answer(&init(0, index), address(game));
            for port_type in 1..=3 {
                natneg.answer(&init(port_type, index), address(negotiate));
            }
        }
        assert_eq!(natneg.ready(&init(1, 1)), Some(COOKIE));
        assert_eq!(natneg.ready(&init(1, 1)), None, "only once");

        let sessions = natneg.sessions.lock().unwrap();
        let packets = connects(&sessions[&COOKIE], COOKIE);
        assert_eq!(packets.len(), 2);
        let (to, connect) = &packets[0];
        assert_eq!(*to, address("203.0.113.7:40000"));
        assert_eq!(connect[7], CONNECT);
        // 198.51.100.9:52000, got your data, no error.
        assert_eq!(connect[12..], [198, 51, 100, 9, 0xCB, 0x20, 0x42, 0]);
        assert_eq!(packets[1].0, address("198.51.100.9:41000"));
    }

    #[test]
    fn reports_are_acked() {
        let natneg = Natneg::default();
        let mut report = header(3, REPORT, COOKIE);
        report.extend_from_slice(&[1, 0, 1, 2, 0, 0, 0, 1, 0, 0, 0]);
        report.extend_from_slice(&[0; 50]);
        let replies = natneg.answer(&report, address("203.0.113.7:40000"));
        let [(_, ack)] = &replies[..] else {
            panic!("one ack");
        };
        assert_eq!(ack[7], REPORT_ACK);
        assert_eq!(ack[12..], [1, 0, 0, 2, 0, 0, 0, 1, 0]);
    }

    #[test]
    fn other_packets_get_nothing() {
        let natneg = Natneg::default();
        for packet in [&b""[..], b"\xFD\xFC\x1E\x66\x6A", b"0123456789ab"] {
            assert!(
                natneg
                    .answer(packet, address("203.0.113.7:40000"))
                    .is_empty()
            );
        }
    }
}

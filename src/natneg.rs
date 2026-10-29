//! NAT negotiation (`natneg1/2/3.gs.nintendowifi.net`, UDP 27901): tells two
//! games that want to play their public addresses, so they can open their
//! routers to each other (docs/login.md). `P/src/RVL_SDK/gamespy/natneg/
//! natneg.c` is the client, protocol version 3. Two games that will not
//! reach each other get the relay's address instead (src/relay.rs).

use std::{
    collections::HashMap,
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::net::UdpSocket;
use tracing::{debug, info, trace, warn};

use crate::{
    activity::Activity,
    master::Master,
    relay::{self, Mode, Relays},
};

/// Every packet starts with this, then the version, the type and the cookie
/// the two games share (big-endian).
const MAGIC: [u8; 6] = [0xFD, 0xFC, 0x1E, 0x66, 0x6A, 0xB2];
const HEADER_LEN: usize = 12;
/// Packet types (`nninternal.h`).
const INIT: u8 = 0x00;
const INIT_ACK: u8 = 0x01;
const CONNECT: u8 = 0x05;
const CONNECT_ACK: u8 = 0x06;
/// The games' pings to each other, which the relay passes on.
const CONNECT_PING: u8 = 0x07;
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
/// Two IPs whose games failed to reach each other get the relay for this long.
const FAILED_FOR: Duration = Duration::from_secs(24 * 60 * 60);
/// Such pairs kept at most.
const MAX_FAILED: usize = 4096;
/// How often the relay looks for matches that are over.
const RELAY_SWEEP: Duration = Duration::from_secs(10);

/// The negotiations under way, by cookie.
#[derive(Default)]
pub struct Natneg {
    sessions: Mutex<HashMap<u32, Session>>,
    activity: Arc<Activity>,
    /// Its sessions tell where each game socket's heartbeats come from.
    master: Arc<Master>,
    relays: Option<Relays>,
    /// The pairs of IPs whose games failed without the relay, and when.
    failed: Mutex<HashMap<(IpAddr, IpAddr), Instant>>,
}

struct Session {
    version: u8,
    started: Instant,
    /// The two games, by their client index (0 and 1).
    clients: HashMap<u8, Client>,
    /// Whether the CONNECTs went out.
    connecting: bool,
    /// Whether they named the relay.
    relayed: bool,
}

#[derive(Default)]
struct Client {
    /// Where its negotiation socket's INITs come from: CONNECTs go there.
    negotiate: Option<SocketAddr>,
    /// Where its game socket's INIT comes from: the other game pings it.
    game: Option<SocketAddr>,
    /// The game socket's own address, as the negotiation socket's INITs
    /// carry it.
    local: Option<SocketAddrV4>,
    acked: bool,
}

impl Client {
    fn mapped(&self) -> bool {
        self.negotiate.is_some() && self.game.is_some()
    }
}

/// Whether a game socket's public port stays the same for every address
/// it sends to, as far as the master server can tell.
enum Mapping {
    /// No heartbeats from that socket.
    Unknown,
    /// Its heartbeats come from the same port.
    Kept,
    /// They come from this other port: the router gives each address its
    /// own port, so the other game's pings would not arrive.
    Changes(SocketAddr),
}

impl fmt::Display for Mapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mapping::Unknown => f.write_str("no heartbeats"),
            Mapping::Kept => f.write_str("same port as its heartbeats"),
            Mapping::Changes(heard) => write!(f, "heartbeats from {heard}: new port per address"),
        }
    }
}

impl Natneg {
    pub fn new(activity: Arc<Activity>, master: Arc<Master>, relay: Option<relay::Config>) -> Self {
        Self {
            sessions: Mutex::default(),
            activity,
            master,
            relays: relay.map(Relays::new),
            failed: Mutex::default(),
        }
    }

    pub async fn serve(self: Arc<Self>, socket: UdpSocket) -> std::io::Result<()> {
        let socket = Arc::new(socket);
        if self.relays.is_some() {
            let natneg = self.clone();
            tokio::spawn(async move {
                let mut sweep = tokio::time::interval(RELAY_SWEEP);
                loop {
                    sweep.tick().await;
                    if let Some(relays) = &natneg.relays {
                        relays.end_idle();
                    }
                }
            });
        }
        // Relayed matches send packets of any size up to a datagram.
        let mut packet = vec![0; 2048];
        loop {
            let (len, peer) = socket.recv_from(&mut packet).await?;
            let packet = &packet[..len];
            if let Some(to) = self.relayed_to(packet, peer) {
                trace!(target: "raw::relay", %peer, %to, len, "relayed");
                if let Err(error) = socket.send_to(packet, to).await {
                    debug!(target: "natneg", %to, %error, "relay failed");
                }
                continue;
            }
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

    /// Where the relay sends a packet from `peer`: to the other game, when
    /// `peer` is a relayed game and the packet is not for NAT negotiation
    /// itself, apart from the games' pings.
    fn relayed_to(&self, packet: &[u8], peer: SocketAddr) -> Option<SocketAddr> {
        let relays = self.relays.as_ref()?;
        match parse(packet) {
            Some((_, CONNECT_PING, cookie, _)) => {
                relays.destination(peer, packet.len()).or_else(|| {
                    relays
                        .moved(cookie, peer)
                        .then(|| relays.destination(peer, packet.len()))
                        .flatten()
                })
            }
            Some(_) => None,
            None => relays.destination(peer, packet.len()),
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
                let relayed = self.reported(cookie, result.copied());
                info!(target: "natneg", cookie = format!("{cookie:08x}"), %peer, ?index, ?result, relayed, "report");
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
                relayed: false,
            }
        });
        let client = session.clients.entry(index).or_default();
        if port_type != GAME_PORT {
            client.negotiate = Some(peer);
            if use_game_port != 0
                && let Some(&[a, b, c, d, high, low]) = body.get(3..9)
                && (high, low) != (0, 0)
            {
                client.local = Some(SocketAddrV4::new(
                    Ipv4Addr::new(a, b, c, d),
                    u16::from_be_bytes([high, low]),
                ));
            }
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
    /// address, or the relay's, again until it acks.
    async fn connect(self: Arc<Self>, socket: Arc<UdpSocket>, cookie: u32) {
        let relay = self.relay_for(cookie).await;
        self.activity.connected(cookie, relay.is_some());
        for _ in 0..CONNECT_TRIES {
            let packets = {
                let sessions = self.sessions.lock().unwrap();
                let Some(session) = sessions.get(&cookie) else {
                    return;
                };
                connects(session, cookie, relay)
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

    /// The relay's address for the negotiation `cookie`, if the relay takes
    /// it: always, or when the two failed without it before, or when one's
    /// router gives a new port per address. Logs the two games either way.
    async fn relay_for(&self, cookie: u32) -> Option<SocketAddrV4> {
        let games = {
            let sessions = self.sessions.lock().unwrap();
            let session = sessions.get(&cookie)?;
            [0, 1].map(|index| {
                let client = session.clients.get(&index)?;
                Some((client.game?, client.local))
            })
        };
        let [Some((first, first_local)), Some((second, second_local))] = games else {
            return None;
        };
        let mappings = [
            self.mapping(first, first_local),
            self.mapping(second, second_local),
        ];
        let reason = match &self.relays {
            None => None,
            Some(relays) if relays.config.mode == Mode::Always => Some("always"),
            Some(_) if self.failed_before(first.ip(), second.ip()) => {
                Some("failed without it before")
            }
            Some(_)
                if mappings
                    .iter()
                    .any(|mapping| matches!(mapping, Mapping::Changes(_))) =>
            {
                Some("new port per address")
            }
            Some(_) => None,
        };
        let cookie_text = format!("{cookie:08x}");
        info!(
            target: "natneg",
            cookie = cookie_text,
            %first,
            first_nat = %mappings[0],
            %second,
            second_nat = %mappings[1],
            relay = reason.unwrap_or("no"),
            "connect"
        );
        let relays = self.relays.as_ref()?;
        reason?;
        let address = relays.address().await?;
        if relays.open(cookie, [first, second]).is_err() {
            warn!(target: "natneg", cookie = cookie_text, max = relays.config.max, "relay full: the games try without it");
            return None;
        }
        if let Some(session) = self.sessions.lock().unwrap().get_mut(&cookie) {
            session.relayed = true;
        }
        Some(SocketAddrV4::new(address, relays.config.port))
    }

    /// Whether the game socket at `game`, whose own address is `local`, sends
    /// its heartbeats from the same public port.
    fn mapping(&self, game: SocketAddr, local: Option<SocketAddrV4>) -> Mapping {
        let Some(local) = local else {
            return Mapping::Unknown;
        };
        match self.master.heard_at(game.ip(), local) {
            None => Mapping::Unknown,
            Some(heard) if heard == game => Mapping::Kept,
            Some(heard) => Mapping::Changes(heard),
        }
    }

    /// A game's report on the negotiation `cookie`: a failure without the
    /// relay gives the two IPs the relay next time. Whether it was relayed.
    fn reported(&self, cookie: u32, result: Option<u8>) -> bool {
        let sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get(&cookie) else {
            return false;
        };
        if result != Some(1) && session.connecting {
            let game = |index| session.clients.get(&index).and_then(|client| client.game);
            if session.relayed {
                warn!(target: "natneg", cookie = format!("{cookie:08x}"), "failed although relayed");
            } else if self.relays.is_some()
                && let (Some(first), Some(second)) = (game(0), game(1))
            {
                self.remember_failure(first.ip(), second.ip());
            }
        }
        session.relayed
    }

    fn remember_failure(&self, first: IpAddr, second: IpAddr) {
        let now = Instant::now();
        let mut failed = self.failed.lock().unwrap();
        let pair = pair(first, second);
        if failed.len() >= MAX_FAILED && !failed.contains_key(&pair) {
            failed.retain(|_, at| now.duration_since(*at) < FAILED_FOR);
            if failed.len() >= MAX_FAILED {
                return;
            }
        }
        if failed.insert(pair, now).is_none() {
            info!(target: "natneg", %first, %second, "failed without the relay: relayed next time");
        }
    }

    fn failed_before(&self, first: IpAddr, second: IpAddr) -> bool {
        self.failed
            .lock()
            .unwrap()
            .get(&pair(first, second))
            .is_some_and(|at| at.elapsed() < FAILED_FOR)
    }
}

/// Two IPs in either order, as one key.
fn pair(first: IpAddr, second: IpAddr) -> (IpAddr, IpAddr) {
    (first.min(second), first.max(second))
}

/// The `CONNECT`s still unacked: to a game's negotiation socket, the other
/// game's public address, or the relay's (IP and port, big-endian), "got
/// your data" and "finished" (no error).
fn connects(
    session: &Session,
    cookie: u32,
    relay: Option<SocketAddrV4>,
) -> Vec<(SocketAddr, Vec<u8>)> {
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
            let other = relay.unwrap_or(other);
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
        init_for(COOKIE, port_type, index)
    }

    fn init_for(cookie: u32, port_type: u8, index: u8) -> Vec<u8> {
        let mut packet = header(3, INIT, cookie);
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
        let packets = connects(&sessions[&COOKIE], COOKIE, None);
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

    fn natneg(mode: Option<Mode>) -> Natneg {
        Natneg::new(
            Arc::default(),
            Arc::default(),
            mode.map(|mode| relay::Config {
                mode,
                address: "192.0.2.1".into(),
                max: 4,
                port: 27901,
            }),
        )
    }

    /// The INITs of both games of the negotiation `cookie`, each from its
    /// game socket and its negotiation socket, all with the local address
    /// 192.168.1.131:51021.
    fn negotiate(natneg: &Natneg, cookie: u32, games: [(&str, &str); 2]) {
        for (index, (game, negotiate)) in (0..).zip(games) {
            natneg.answer(&init_for(cookie, 0, index), address(game));
            for port_type in 1..=3 {
                natneg.answer(&init_for(cookie, port_type, index), address(negotiate));
            }
        }
        assert_eq!(natneg.ready(&init_for(cookie, 1, 1)), Some(cookie));
    }

    const GAMES: [(&str, &str); 2] = [
        ("203.0.113.7:21982", "203.0.113.7:40000"),
        ("198.51.100.9:65116", "198.51.100.9:41000"),
    ];

    fn report(cookie: u32, index: u8, result: u8) -> Vec<u8> {
        let mut report = header(3, REPORT, cookie);
        report.extend_from_slice(&[1, index, result, 0, 0, 0, 0, 0, 0, 0, 0]);
        report.extend_from_slice(&[0; 50]);
        report
    }

    /// A heartbeat from the game socket at 192.168.1.131:51021.
    fn heartbeat(master: &Master, from: &str) {
        let mut packet = b"\x03KEY!".to_vec();
        packet.extend_from_slice(
            b"localip0\x00192.168.1.131\x00localport\x0051021\x00gamename\x00mschargedwii\x00\x00",
        );
        assert!(master.answer(&packet, address(from)).is_some());
    }

    #[tokio::test]
    async fn without_the_relay_the_games_get_each_others_address() {
        let natneg = natneg(None);
        negotiate(&natneg, COOKIE, GAMES);
        assert_eq!(natneg.relay_for(COOKIE).await, None);
        let auto = self::natneg(Some(Mode::Auto));
        negotiate(&auto, COOKIE, GAMES);
        assert_eq!(
            auto.relay_for(COOKIE).await,
            None,
            "nothing says they need it"
        );
    }

    #[tokio::test]
    async fn the_relay_names_itself_and_passes_pings_and_matches_on() {
        let natneg = natneg(Some(Mode::Always));
        negotiate(&natneg, COOKIE, GAMES);
        let relay = natneg.relay_for(COOKIE).await;
        assert_eq!(relay, Some("192.0.2.1:27901".parse().unwrap()));
        {
            let sessions = natneg.sessions.lock().unwrap();
            for (_, connect) in connects(&sessions[&COOKIE], COOKIE, relay) {
                assert_eq!(connect[12..], [192, 0, 2, 1, 0x6C, 0xFD, 0x42, 0]);
            }
        }

        let (first, second) = (address(GAMES[0].0), address(GAMES[1].0));
        let mut ping = header(3, CONNECT_PING, COOKIE);
        ping.extend_from_slice(&[192, 0, 2, 1, 0x6C, 0xFD, 0, 0]);
        assert_eq!(natneg.relayed_to(&ping, first), Some(second));
        assert_eq!(natneg.relayed_to(b"\xFE\xFEmatch", second), Some(first));
        assert_eq!(
            natneg.relayed_to(&init(0, 0), first),
            None,
            "NAT negotiation's own"
        );
        assert_eq!(
            natneg.relayed_to(b"\xFE\xFEmatch", address(GAMES[0].1)),
            None
        );

        // The pings of a game whose router took another port for them.
        let moved = address("203.0.113.7:21990");
        assert_eq!(natneg.relayed_to(b"\xFE\xFEmatch", moved), None);
        assert_eq!(natneg.relayed_to(&ping, moved), Some(second));
        assert_eq!(natneg.relayed_to(b"\xFE\xFEmatch", second), Some(moved));

        natneg.answer(&report(COOKIE, 0, 1), address(GAMES[0].1));
        assert!(natneg.reported(COOKIE, Some(1)));
    }

    #[tokio::test]
    async fn a_router_with_a_new_port_per_address_gets_the_relay() {
        let natneg = natneg(Some(Mode::Auto));
        // The second game's heartbeats come from its natneg port, the
        // first's from another one.
        heartbeat(&natneg.master, "198.51.100.9:65116");
        negotiate(&natneg, COOKIE, GAMES);
        assert_eq!(natneg.relay_for(COOKIE).await, None, "both keep their port");
        heartbeat(&natneg.master, "203.0.113.7:21980");
        assert!(matches!(
            natneg.mapping(address(GAMES[0].0), Some("192.168.1.131:51021".parse().unwrap())),
            Mapping::Changes(heard) if heard == address("203.0.113.7:21980")
        ));
        negotiate(&natneg, COOKIE + 1, GAMES);
        assert!(natneg.relay_for(COOKIE + 1).await.is_some());
    }

    #[tokio::test]
    async fn two_that_failed_get_the_relay_next_time() {
        let natneg = natneg(Some(Mode::Auto));
        negotiate(&natneg, COOKIE, GAMES);
        assert_eq!(natneg.relay_for(COOKIE).await, None);
        for index in 0..2 {
            natneg.answer(
                &report(COOKIE, index, 0),
                address(GAMES[usize::from(index)].1),
            );
        }
        // The same two IPs, other ports, either way round.
        negotiate(
            &natneg,
            COOKIE + 1,
            [
                ("198.51.100.9:50000", "198.51.100.9:50001"),
                ("203.0.113.7:50002", "203.0.113.7:50003"),
            ],
        );
        assert!(natneg.relay_for(COOKIE + 1).await.is_some());
        // A failure although relayed is not remembered again.
        natneg.answer(&report(COOKIE + 1, 0, 0), address("198.51.100.9:50001"));
        assert!(natneg.reported(COOKIE + 1, Some(0)));
    }
}

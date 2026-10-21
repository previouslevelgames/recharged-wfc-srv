//! The QR2 master server (`<game>.available.gs.nintendowifi.net` and
//! `<game>.master.gs.nintendowifi.net`, UDP 27900). It answers the
//! availability check DWC makes before it logs in, and challenges the
//! heartbeats a logged-in game sends: without a challenge DWC gives up after
//! four heartbeats, 10 seconds apart, with error 94020. Server lists and
//! matchmaking come later.

use std::net::SocketAddr;

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
const SHUTDOWN: &[u8] = b"2";

pub async fn serve(socket: UdpSocket) -> std::io::Result<()> {
    let mut packet = [0; 1500];
    loop {
        let (len, peer) = socket.recv_from(&mut packet).await?;
        trace!(target: "raw::master", %peer, packet = ?&packet[..len], "packet");
        if let Some(reply) = answer(&packet[..len], peer) {
            trace!(target: "raw::master", %peer, ?reply, "reply");
            if let Err(error) = socket.send_to(&reply, peer).await {
                debug!(target: "master", %peer, %error, "reply failed");
            }
        }
    }
}

fn answer(packet: &[u8], peer: SocketAddr) -> Option<Vec<u8>> {
    let (&kind, rest) = packet.split_first()?;
    // The game's instance key, which replies carry.
    let (key, body) = rest.split_first_chunk::<4>()?;
    match kind {
        AVAILABLE => {
            let game = body.strip_suffix(b"\0")?;
            // Flags 0: up (1 out of service, 2 maintenance).
            (game == crate::GAME_NAME.as_bytes()).then(|| reply(AVAILABLE, &[0; 4]))
        }
        HEARTBEAT => heartbeat(key, body, peer),
        CHALLENGE => {
            debug!(target: "master", %peer, "listed");
            None
        }
        KEEPALIVE => Some(reply(KEEPALIVE, key)),
        _ => {
            debug!(target: "master", %peer, kind, "not implemented yet");
            None
        }
    }
}

/// A heartbeat gets a challenge: 6 random letters, the backend options (`00`:
/// no query challenges) and the address the heartbeat came from as
/// `%08X%04X`, which is how the game learns its public address. The game
/// answers with the challenge encrypted with its secret key. Nothing is kept
/// yet, so every heartbeat is challenged.
fn heartbeat(key: &[u8; 4], body: &[u8], peer: SocketAddr) -> Option<Vec<u8>> {
    let (mut game, mut state) = (None, None);
    for (name, value) in fields(body) {
        match name {
            b"gamename" => game = Some(value),
            b"statechanged" => state = Some(value),
            _ => {}
        }
    }
    if game != Some(crate::GAME_NAME.as_bytes()) {
        debug!(target: "master", %peer, "heartbeat of another game");
        return None;
    }
    if state == Some(SHUTDOWN) {
        debug!(target: "master", %peer, "unlisted");
        return None;
    }
    let SocketAddr::V4(peer) = peer else {
        return None;
    };
    debug!(target: "master", %peer, "heartbeat");

    let letters: String = (0..6)
        .map(|_| char::from(rand::random_range(b'A'..=b'Z')))
        .collect();
    let challenge = format!("{letters}00{:08X}{:04X}", peer.ip().to_bits(), peer.port());
    let mut reply = reply(CHALLENGE, key);
    reply.extend_from_slice(challenge.as_bytes());
    reply.push(0);
    Some(reply)
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

    fn peer() -> SocketAddr {
        "203.0.113.7:64244".parse().unwrap()
    }

    #[test]
    fn the_game_is_available() {
        assert_eq!(
            answer(b"\x09\0\0\0\0mschargedwii\0", peer()).unwrap(),
            b"\xFE\xFD\x09\0\0\0\0"
        );
    }

    #[test]
    fn a_heartbeat_is_challenged_with_the_public_address() {
        let reply = answer(HEARTBEAT_PACKET, peer()).unwrap();
        let (header, challenge) = reply.split_at(7);
        assert_eq!(header, b"\xFE\xFD\x01*\x8fPM");
        let challenge = challenge.strip_suffix(b"\0").unwrap();
        let (letters, rest) = challenge.split_at(6);
        assert!(letters.iter().all(u8::is_ascii_uppercase), "{challenge:?}");
        // 203.0.113.7, port 64244.
        assert_eq!(rest, b"00CB007107FAF4");
    }

    #[test]
    fn a_game_that_stops_is_not_challenged() {
        let mut shutdown = HEARTBEAT_PACKET.to_vec();
        let at = shutdown
            .windows(14)
            .position(|field| field == b"statechanged\x001")
            .unwrap();
        shutdown[at + 13] = b'2';
        assert_eq!(answer(&shutdown, peer()), None);
    }

    #[test]
    fn keepalives_are_answered() {
        assert_eq!(
            answer(b"\x08*\x8fPM", peer()).unwrap(),
            b"\xFE\xFD\x08*\x8fPM"
        );
    }

    #[test]
    fn other_packets_are_not_answered() {
        for packet in [
            &b""[..],
            b"\x03*\x8f",
            b"\x09\0\0\0\0othergame\0",
            b"\x09\0\0\0\0mschargedwii",
            b"\x03*\x8fPMgamename\0othergame\0\0",
            b"\x01*\x8fPMresponse\0",
            b"\x07*\x8fPM",
        ] {
            assert_eq!(answer(packet, peer()), None, "{packet:?}");
        }
    }
}

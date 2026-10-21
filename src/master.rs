//! The QR2 master server (`<game>.available.gs.nintendowifi.net`, UDP 27900).
//! For now it answers the availability check DWC makes before it logs in;
//! heartbeats and matchmaking come later.

use tokio::net::UdpSocket;
use tracing::{debug, trace};

/// The availability check: `09 00 00 00 00`, then the game name and a NUL.
const AVAILABLE_REQUEST: u8 = 0x09;
/// `FE FD 09`, then flags: 0 is up (1 out of service, 2 maintenance).
const AVAILABLE: [u8; 7] = [0xFE, 0xFD, 0x09, 0, 0, 0, 0];

pub async fn serve(socket: UdpSocket) -> std::io::Result<()> {
    let mut packet = [0; 1500];
    loop {
        let (len, peer) = socket.recv_from(&mut packet).await?;
        trace!(target: "raw::master", %peer, packet = ?&packet[..len], "packet");
        match answer(&packet[..len]) {
            Some(reply) => {
                if let Err(error) = socket.send_to(reply, peer).await {
                    debug!(target: "master", %peer, %error, "reply failed");
                }
            }
            None => debug!(target: "master", %peer, kind = packet[0], "not implemented yet"),
        }
    }
}

fn answer(packet: &[u8]) -> Option<&'static [u8]> {
    let (&kind, rest) = packet.split_first()?;
    let game = rest.get(4..)?.strip_suffix(b"\0")?;
    (kind == AVAILABLE_REQUEST && game == crate::GAME_NAME.as_bytes()).then_some(&AVAILABLE[..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_game_is_available() {
        assert_eq!(answer(b"\x09\0\0\0\0mschargedwii\0"), Some(&AVAILABLE[..]));
    }

    #[test]
    fn other_packets_are_not_answered_yet() {
        for packet in [
            &b""[..],
            b"\x09\0\0\0\0othergame\0",
            b"\x09\0\0\0\0mschargedwii",
            b"\x03\0\0\0\0mschargedwii\0",
        ] {
            assert_eq!(answer(packet), None, "{packet:?}");
        }
    }
}

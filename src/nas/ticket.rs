//! NAS login tokens: what GPCM needs to know of a NAS login, signed, so the
//! server keeps nothing. The nodes of a pool share the key (docs/nodes.md):
//! a token from one logs in to another. A token can be used more than once
//! within [`TTL`]: on a profile's first login, DWC logs in to GPCM twice with
//! it.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use time::OffsetDateTime;

use super::base64;

/// How long a token stays good.
const TTL: Duration = Duration::from_secs(10 * 60);
/// Bytes of the HMAC-SHA256 kept in a token.
const TAG_LEN: usize = 16;
/// The prefix of Nintendo's tokens, which WiiLink's keep too.
const PREFIX: &str = "NDS";

/// A NAS login.
#[derive(Clone, Debug, PartialEq)]
pub struct Ticket {
    pub user_id: i64,
    /// The game and Mii slot: GPCM's profile is per userid and gsbrcd.
    pub gsbrcd: String,
    /// 8 characters. GPCM's login proofs start from its MD5.
    pub challenge: String,
    /// The Mii name.
    pub ingamesn: Option<String>,
    /// Where the NAS login came from. The game's HTTP (NAS, gamestats2, the
    /// reports) can leave a player's network by another address than its
    /// GPCM connection, so the login's requests count from either.
    pub address: IpAddr,
}

#[derive(Debug, PartialEq)]
pub enum TicketError {
    /// Not a token of this server process: forged, garbled, or from before a
    /// restart.
    Invalid,
    Expired,
}

/// Issues and checks tokens with a key of this process.
pub struct Tickets {
    key: [u8; 32],
}

impl Default for Tickets {
    fn default() -> Self {
        Self {
            key: rand::random(),
        }
    }
}

impl Tickets {
    /// With the key the nodes share, so that each takes the others' tokens.
    pub fn with_key(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub fn issue(&self, ticket: &Ticket) -> String {
        self.issue_at(ticket, OffsetDateTime::now_utc().unix_timestamp())
    }

    pub fn redeem(&self, token: &str) -> Result<Ticket, TicketError> {
        self.redeem_at(token, OffsetDateTime::now_utc().unix_timestamp())
    }

    fn issue_at(&self, ticket: &Ticket, now: i64) -> String {
        let mut bytes = Vec::with_capacity(128);
        bytes.extend(now.to_le_bytes());
        bytes.extend(ticket.user_id.to_le_bytes());
        for text in [
            &ticket.challenge,
            &ticket.gsbrcd,
            ticket.ingamesn.as_deref().unwrap_or_default(),
        ] {
            // NAS checks the lengths before it issues a ticket.
            bytes.push(text.len() as u8);
            bytes.extend(text.as_bytes());
        }
        let address = match ticket.address {
            IpAddr::V4(v4) => v4.octets().to_vec(),
            IpAddr::V6(v6) => v6.octets().to_vec(),
        };
        bytes.push(address.len() as u8);
        bytes.extend(address);
        let tag = self.mac(&bytes).finalize().into_bytes();
        bytes.extend(&tag[..TAG_LEN]);
        format!("{PREFIX}{}", base64::encode(bytes))
    }

    fn redeem_at(&self, token: &str, now: i64) -> Result<Ticket, TicketError> {
        let bytes = token
            .strip_prefix(PREFIX)
            .and_then(base64::decode)
            .ok_or(TicketError::Invalid)?;
        let (payload, tag) = bytes
            .split_at_checked(bytes.len().saturating_sub(TAG_LEN))
            .filter(|(_, tag)| tag.len() == TAG_LEN)
            .ok_or(TicketError::Invalid)?;
        self.mac(payload)
            .verify_truncated_left(tag)
            .map_err(|_| TicketError::Invalid)?;

        // Signed by this process, so well-formed.
        let issued = i64::from_le_bytes(payload[0..8].try_into().unwrap());
        let user_id = i64::from_le_bytes(payload[8..16].try_into().unwrap());
        let mut rest = &payload[16..];
        let mut field = || {
            let (len, tail) = rest.split_first().unwrap();
            let (field, tail) = tail.split_at(*len as usize);
            rest = tail;
            field
        };
        let mut text = || String::from_utf8(field().to_vec()).unwrap();
        let (challenge, gsbrcd, ingamesn) = (text(), text(), text());
        let address = match field() {
            &[a, b, c, d] => IpAddr::V4(Ipv4Addr::new(a, b, c, d)),
            octets => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(octets).unwrap())),
        };
        let ticket = Ticket {
            user_id,
            challenge,
            gsbrcd,
            ingamesn: Some(ingamesn).filter(|name| !name.is_empty()),
            address,
        };
        if now - issued > TTL.as_secs() as i64 {
            return Err(TicketError::Expired);
        }
        Ok(ticket)
    }

    fn mac(&self, payload: &[u8]) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("any key length");
        mac.update(payload);
        mac
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn ticket() -> Ticket {
        Ticket {
            user_id: 1234567890123,
            gsbrcd: "R4QEabcdefg".into(),
            challenge: "ABCDEFGH".into(),
            ingamesn: Some("Gold€nP2☆☆".into()),
            address: IpAddr::from([12, 75, 115, 108]),
        }
    }

    #[test]
    fn a_token_carries_its_ticket_for_ten_minutes() {
        let tickets = Tickets::default();
        let token = tickets.issue_at(&ticket(), NOW);
        assert!(token.starts_with("NDS"));
        assert!(token.len() <= 255, "DWC keeps 255 characters");
        assert_eq!(tickets.redeem_at(&token, NOW + 600), Ok(ticket()));
        assert_eq!(tickets.redeem_at(&token, NOW + 600), Ok(ticket()));
        assert_eq!(
            tickets.redeem_at(&token, NOW + 601),
            Err(TicketError::Expired)
        );
    }

    #[test]
    fn the_longest_token_fits_dwc() {
        // NAS takes a gsbrcd of up to 16 bytes and a name of up to 64.
        let longest = Ticket {
            gsbrcd: "R4QEabcdefghijkl".into(),
            ingamesn: Some("☆".repeat(21) + "a"),
            address: IpAddr::from([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]),
            ..ticket()
        };
        let tickets = Tickets::default();
        let token = tickets.issue_at(&longest, NOW);
        assert!(
            token.len() <= 255,
            "DWC keeps 255 characters: {}",
            token.len()
        );
        assert_eq!(tickets.redeem_at(&token, NOW), Ok(longest));
    }

    #[test]
    fn a_name_is_optional() {
        let tickets = Tickets::default();
        let nameless = Ticket {
            ingamesn: None,
            ..ticket()
        };
        let token = tickets.issue_at(&nameless, NOW);
        assert_eq!(tickets.redeem_at(&token, NOW), Ok(nameless));
    }

    #[test]
    fn a_token_logs_in_wherever_the_key_is_the_same() {
        let key = rand::random();
        let token = Tickets::with_key(key).issue_at(&ticket(), NOW);
        assert_eq!(Tickets::with_key(key).redeem_at(&token, NOW), Ok(ticket()));
    }

    #[test]
    fn other_tokens_are_invalid() {
        let tickets = Tickets::default();
        let token = tickets.issue_at(&ticket(), NOW);
        let restarted = Tickets::default();
        let mut tampered = token.clone().into_bytes();
        tampered[10] = if tampered[10] == b'A' { b'B' } else { b'A' };
        for token in [
            restarted.issue_at(&ticket(), NOW),
            String::from_utf8(tampered).unwrap(),
            token[3..].to_owned(),
            "NDS".to_owned(),
            "NDSAAAA".to_owned(),
        ] {
            assert_eq!(
                tickets.redeem_at(&token, NOW),
                Err(TicketError::Invalid),
                "{token}"
            );
        }
    }
}

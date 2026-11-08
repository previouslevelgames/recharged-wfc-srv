//! The `NetworkRankingSubmission` the game puts as userdata
//! (`docs/gamestats2.md`, "put2.asp"), as far as `/admin` shows it and its
//! date tells the season, and its digest: a game shows a row only when the
//! digest holds, so userdata the server changes is signed again.

use hmac::{Hmac, KeyInit, Mac};
use md5::Md5;
use time::{Date, Month};

/// The game clamps wins and losses to this.
const MAX_RECORD: i32 = 9999;
/// The game's userdata: 0x80 bytes, the last 16 of them HMAC-MD5 of the
/// rest under `sRankingHmacKey` (`NetworkStats.cpp`, the same in every
/// release). The key ships in every binary: integrity, not authenticity.
const SIZE: usize = 0x80;
const DIGEST: usize = 0x70;
const HMAC_KEY: [u8; 32] = [
    0x17, 0xB2, 0x40, 0x52, 0x5F, 0x89, 0xF0, 0xFF, 0xE9, 0xB9, 0xB0, 0x17, 0x66, 0x06, 0x37, 0xA1,
    0x63, 0x70, 0x9E, 0xC9, 0xCF, 0x42, 0xD1, 0x89, 0x91, 0x36, 0xC7, 0xBD, 0x2B, 0x58, 0xBC, 0x39,
];

/// What a row's userdata says about its player. Each field is `None` when its
/// bytes hold no plausible value: the server stores userdata unchecked.
#[derive(Debug, PartialEq)]
pub struct Submission {
    /// The game's date when it put the row, by its own clock.
    pub date: Option<Date>,
    /// The Mii name.
    pub name: Option<String>,
    pub wins: Option<i32>,
    pub losses: Option<i32>,
}

impl Submission {
    pub fn parse(userdata: &[u8]) -> Self {
        let record = |at: usize| {
            userdata
                .get(at..at + 4)
                .map(|bytes| i32::from_le_bytes(bytes.try_into().unwrap()))
                .filter(|value| (0..=MAX_RECORD).contains(value))
        };
        // UTF-16LE at 0x0E, 11 units, NUL-terminated.
        let name = userdata.get(0x0E..0x24).and_then(|bytes| {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                .take_while(|&unit| unit != 0)
                .collect();
            String::from_utf16(&units)
                .ok()
                .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
        });
        // Day, month, then the year as a little-endian u16.
        let date = userdata.get(0..4).and_then(|bytes| {
            let year = u16::from_le_bytes([bytes[2], bytes[3]]);
            Date::from_calendar_date(year.into(), Month::try_from(bytes[1]).ok()?, bytes[0]).ok()
        });
        Self {
            date,
            name,
            wins: record(0x04),
            losses: record(0x08),
        }
    }
}

/// `userdata` with its wins and losses replaced by these, as far as they are
/// given and it has room for them; clamped as the game clamps them. Its
/// digest is made again when it held before, so the game still shows the
/// row (`ProcessLeaderboardResults` blanks the name, Mii and record of one
/// whose digest fails); userdata whose digest failed already stays unsigned.
pub fn with_record(userdata: &[u8], wins: Option<i32>, losses: Option<i32>) -> Vec<u8> {
    let was_signed = signed(userdata);
    let mut userdata = userdata.to_vec();
    for (at, value) in [(0x04, wins), (0x08, losses)] {
        if let (Some(value), Some(bytes)) = (value, userdata.get_mut(at..at + 4)) {
            bytes.copy_from_slice(&value.clamp(0, MAX_RECORD).to_le_bytes());
        }
    }
    if was_signed {
        sign(&mut userdata);
    }
    userdata
}

/// Makes the digest of the game's 0x80 bytes of `userdata` again; other
/// userdata has none.
pub fn sign(userdata: &mut [u8]) {
    if userdata.len() == SIZE {
        let digest = digest(&userdata[..DIGEST]);
        userdata[DIGEST..].copy_from_slice(&digest);
    }
}

/// Whether `userdata` is the game's 0x80 bytes and its digest holds, as the
/// game checks a row before it shows it.
pub fn signed(userdata: &[u8]) -> bool {
    userdata.len() == SIZE && digest(&userdata[..DIGEST]) == userdata[DIGEST..]
}

fn digest(body: &[u8]) -> [u8; 16] {
    let mut mac = Hmac::<Md5>::new_from_slice(&HMAC_KEY).expect("HMAC takes any key");
    mac.update(body);
    mac.finalize().into_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A put the port's game sent on 2026-11-02 (`Anosch`, 3 wins and 1
    /// loss), as the server stored it.
    fn game_put() -> Vec<u8> {
        let mut userdata = vec![0u8; SIZE];
        userdata[0..4].copy_from_slice(&[2, 11, 0xEA, 0x07]);
        userdata[0x04..0x08].copy_from_slice(&3i32.to_le_bytes());
        userdata[0x08..0x0C].copy_from_slice(&1i32.to_le_bytes());
        userdata[0x0C..0x0E].copy_from_slice(&2u16.to_le_bytes());
        for (at, unit) in "Anosch".encode_utf16().enumerate() {
            userdata[0x0E + 2 * at..0x10 + 2 * at].copy_from_slice(&unit.to_le_bytes());
        }
        sign(&mut userdata);
        userdata
    }

    #[test]
    fn a_changed_record_is_signed_again() {
        let put = game_put();
        assert!(signed(&put));
        let merged = with_record(&put, Some(10), Some(4));
        assert!(signed(&merged), "the game would blank this row");
        let parsed = Submission::parse(&merged);
        assert_eq!((parsed.wins, parsed.losses), (Some(10), Some(4)));
        assert_eq!(parsed.name.as_deref(), Some("Anosch"));
        // Only the record and the digest changed.
        assert_eq!(merged[..0x04], put[..0x04]);
        assert_eq!(merged[0x0C..DIGEST], put[0x0C..DIGEST]);
    }

    #[test]
    fn userdata_whose_digest_failed_stays_unsigned() {
        let mut forged = game_put();
        forged[0x04] = 99;
        assert!(!signed(&forged));
        assert!(!signed(&with_record(&forged, Some(1), Some(1))));
        // Short userdata has no digest to make.
        assert_eq!(
            with_record(&[0; 8], Some(5), Some(6)),
            [0, 0, 0, 0, 5, 0, 0, 0]
        );
    }

    #[test]
    fn garbage_parses_to_nothing() {
        let nothing = Submission {
            date: None,
            name: None,
            wins: None,
            losses: None,
        };
        assert_eq!(Submission::parse(&[]), nothing);
        // Records of -1 and a name of unpaired surrogates.
        let mut garbage = [0xFF; 128];
        for at in (0x0E..0x24).step_by(2) {
            garbage[at..at + 2].copy_from_slice(&0xD800_u16.to_le_bytes());
        }
        assert_eq!(Submission::parse(&garbage), nothing);
    }
}

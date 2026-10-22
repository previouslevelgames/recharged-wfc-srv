//! The `NetworkRankingSubmission` the game puts as userdata
//! (`docs/gamestats2.md`, "put2.asp"), as far as `/admin` shows it and its
//! date tells the season.

use time::{Date, Month};

/// The game clamps wins and losses to this.
const MAX_RECORD: i32 = 9999;

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

#[cfg(test)]
mod tests {
    use super::*;

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

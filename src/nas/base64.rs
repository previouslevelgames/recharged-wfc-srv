//! The Base64 of NAS: every form value the client sends and every value of the
//! reply. The alphabet ends in `.` and `-` instead of `+` and `/`, and pads
//! with `*` instead of `=`.

use base64::{
    Engine as _,
    alphabet::Alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};

const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789.-";

fn engine() -> GeneralPurpose {
    GeneralPurpose::new(
        &Alphabet::new(ALPHABET).expect("64 distinct characters"),
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    )
}

pub fn encode(bytes: impl AsRef<[u8]>) -> String {
    engine().encode(bytes).replace('=', "*")
}

/// Decodes a value, padded or not. `None` when it is not this Base64.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    engine().decode(text.replace('*', "=")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_its_own_alphabet_and_padding() {
        // 0xFB 0xFF starts with the last two characters of the alphabet.
        assert_eq!(encode([0xFB, 0xFF]), ".-8*");
        assert_eq!(encode("gamespy.com"), "Z2FtZXNweS5jb20*");
        assert_eq!(encode("001"), "MDAx");
        assert_eq!(decode("Z2FtZXNweS5jb20*").unwrap(), b"gamespy.com");
        assert_eq!(decode("Z2FtZXNweS5jb20").unwrap(), b"gamespy.com");
        assert_eq!(decode(".-8*").unwrap(), [0xFB, 0xFF]);
    }

    #[test]
    fn refuses_other_base64() {
        assert!(decode("+/8=").is_none());
        assert!(decode("not base64!").is_none());
    }
}

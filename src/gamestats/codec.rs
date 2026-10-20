//! Encoding of `data` and of responses (`docs/gamestats2.md`, "Transport").
//!
//! To write: [`decode_data`] and [`sign`]. The tests at the bottom come from
//! captured or independently computed bytes; they are ignored until the
//! functions exist (`cargo test -- --ignored` runs them).

use super::{GAMESTATS_KEY, GamestatsError, hex};
use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use sha1::{Digest, Sha1};

const KEY1: u32 = 0x00022825;
const KEY2: u32 = 0x00000ef9;
const KEY3: u32 = 0x00000200;
const KEY4: u32 = 0x42db60ec;

/// Decodes the `data` query parameter: base64 (URL-safe alphabet, padded),
/// the big-endian checksum, the XOR keystream, then the `pid` and `size`
/// header.
///
/// Returns the pid inside `data` and the ranking body after the header
/// (`size` bytes).
pub fn decode_data(data: &str) -> Result<(i32, Vec<u8>), GamestatsError> {
    let bytes = URL_SAFE.decode(data).map_err(|_| GamestatsError::BadHash)?;
    if bytes.len() < 12 {
        return Err(GamestatsError::BadData);
    }
    let checksum = u32::from_be_bytes(
        bytes
            .get(0..=3)
            .ok_or(GamestatsError::BadData)?
            .try_into()
            .map_err(|_| GamestatsError::BadData)?,
    );
    let sum = checksum ^ KEY4;
    let mut seed = sum | (sum << 16);
    let data: Vec<u8> = bytes[4..]
        .iter()
        .map(|b| {
            seed = KEY1.wrapping_mul(seed).wrapping_add(KEY2) % KEY3;
            b ^ (seed >> 16) as u8
        })
        .collect();
    if data.iter().copied().map(u32::from).sum::<u32>() != sum {
        return Err(GamestatsError::BadData);
    }
    let pid = i32::from_le_bytes(
        data.get(0..=3)
            .ok_or(GamestatsError::BadData)?
            .try_into()
            .map_err(|_| GamestatsError::BadData)?,
    );
    let size = u32::from_le_bytes(
        data.get(4..=7)
            .ok_or(GamestatsError::BadData)?
            .try_into()
            .map_err(|_| GamestatsError::BadData)?,
    ) as usize;
    if bytes.len() - 12 != size {
        return Err(GamestatsError::BadData);
    }
    let body = data.get(8..(8 + size)).ok_or(GamestatsError::BadData)?;
    Ok((pid, body.into()))
}

/// Appends the response hash to `payload`: lowercase hex of
/// SHA1(key || base64url(payload) || key).
///
/// Until it is written the payload goes out unsigned: the client rejects it
/// ("invalid HMAC", or "response too short") and fails the ranking request
/// only.
pub fn sign(payload: Vec<u8>) -> Vec<u8> {
    let mut res = payload.clone();
    let encoded = URL_SAFE.encode(&payload);

    let hash = Sha1::new()
        .chain_update(GAMESTATS_KEY)
        .chain_update(encoded)
        .chain_update(GAMESTATS_KEY)
        .finalize();

    let signature = hex(&hash);

    res.extend_from_slice(signature.as_bytes());
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A get2 captured against wfc-server (`docs/gamestats2.md`, "Worked
    /// example").
    const CAPTURED_DATA: &str = "QttitQPKmjscAAAAAgAAAAEAAAACAAAADAAAAAEAAAAKAAAAfgEAAA==";

    #[test]
    fn decodes_captured_get2_data() {
        let (pid, body) = decode_data(CAPTURED_DATA).unwrap();
        assert_eq!(pid, 1_000_000_003);
        let (words, rest) = body.as_chunks::<4>();
        assert!(rest.is_empty());
        let words: Vec<u32> = words.iter().map(|word| u32::from_le_bytes(*word)).collect();
        // Region US, category 1, mode NEAR, 12 bytes of parameters:
        // sort descending, limit 10, since 382 minutes.
        assert_eq!(words, [2, 1, 2, 12, 1, 10, 382]);
    }

    #[test]
    fn signs_an_empty_near_list() {
        // Mode NEAR, count 0, total 0. Hash from:
        // printf 'DbfBialvJznkQWYuOrRaAgAAAAAAAAAAAAAADbfBialvJznkQWYuOrRa' | sha1sum
        let payload = vec![2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let signed = sign(payload.clone());
        assert_eq!(signed[..12], payload[..]);
        assert_eq!(&signed[12..], b"7f8f2285e7f8ba005df4776523720c46fdc8b94d");
    }
}

//! Requests captured from two port clients during a ranked match against the
//! dev stack (2026-10-17, game 2 of the series), replayed through decoding and
//! the leaderboard.

use super::{GamestatsState, codec::decode_data, leaderboard};

/// put2 from pid 1000000004 ("Gold€nP2☆☆"): season (category 0), score 28,
/// 2 wins, 0 losses.
const PUT_P4_SEASON: &str = "Qtt_8QTKmjuQAAAAAgAAAAAAAAAcAAAAgAAAABEK6gcCAAAAAAAAAAEARwBvAGwAZACsIG4AUAAyAAYmBiYAAIAUAEcAbwBsAGQgrABuAFAAMiYGJgYAAImii6HC4jdbAgQ8QHH9DQJszg5AIUFQDACKAIolBAAAAAAAAAAAAAAAAAAAAAAAAAAAh5nMqLL0fhy5GTuh0NutIUAr";
/// put2 from pid 1000000003 ("67"): season, score 4, 0 wins, 2 losses.
const PUT_P3_SEASON: &str = "Qtt_TgPKmjuQAAAAAgAAAAAAAAAEAAAAgAAAABEK6gcAAAAAAgAAAAEANgA3AAAANVUAADVVsYwRIQAAqYpWZoAAADYANwAAAAAAAAAAAAAAAAAAAABAQIHhh-vCf0e4AARCQDG9KKIIjAhAFEm4jQCKAIolBAAAAAAAAAAAAAAAAAAAAAAAAAAAjB7mkvwrX7unwpn1_riIvhwQ";
/// get2 from pid 1000000003: season TOPLIST, descending, limit 10, since 3345
/// minutes.
const GET_P3_SEASON_TOP: &str = "QtthGgPKmjscAAAAAgAAAAAAAAABAAAADAAAAAEAAAAKAAAAEQ0AAA==";
/// get2 from pid 1000000004: season FRIENDS, descending, limit 65, since 0,
/// friends 1000000003 and 1000000005.
const GET_P4_SEASON_FRIENDS: &str = "QttltgTKmjscAQAAAgAAAAAAAAADAAAADAEAAAEAAABBAAAAAAAAAAPKmjsFypo7AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

const P3: i32 = 1_000_000_003;
const P4: i32 = 1_000_000_004;

fn word(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
}

/// The player name in a `NetworkRankingSubmission`: UTF-16LE at 0x0E, 11 units,
/// NUL-terminated.
fn name(userdata: &[u8]) -> String {
    let units: Vec<u16> = userdata[0x0E..0x24]
        .chunks(2)
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16(&units).unwrap()
}

/// `(order, pid, score, userdata)` of each row of a list reply.
fn rows(reply: &[u8]) -> Vec<(u32, i32, i32, Vec<u8>)> {
    let mut rows = Vec::new();
    let mut at = 12;
    for _ in 0..word(reply, 1) {
        let row = &reply[at..];
        let size = word(row, 5) as usize;
        rows.push((
            word(row, 0),
            word(row, 1) as i32,
            word(row, 2) as i32,
            row[24..24 + size].to_vec(),
        ));
        at += 24 + size;
    }
    assert_eq!(at, reply.len());
    rows
}

#[test]
fn decodes_the_captured_puts() {
    for (data, pid, score, wins, losses, player) in [
        (PUT_P4_SEASON, P4, 28, 2, 0, "Gold€nP2☆☆"),
        (PUT_P3_SEASON, P3, 4, 0, 2, "67"),
    ] {
        let (data_pid, body) = decode_data(data).unwrap();
        assert_eq!(data_pid, pid);
        // Region US, category 0 (season), score, 128 bytes of userdata.
        assert_eq!(
            [
                word(&body, 0),
                word(&body, 1),
                word(&body, 2),
                word(&body, 3)
            ],
            [2, 0, score, 128]
        );

        let userdata = &body[16..];
        assert_eq!(userdata.len(), 128);
        assert_eq!(
            (
                userdata[0],
                userdata[1],
                u16::from_le_bytes([userdata[2], userdata[3]])
            ),
            (17, 10, 2026)
        );
        assert_eq!((word(userdata, 1), word(userdata, 2)), (wins, losses));
        assert_eq!(u16::from_le_bytes([userdata[0x0C], userdata[0x0D]]), 1);
        assert_eq!(name(userdata), player);
    }
}

#[tokio::test]
async fn replays_the_match() {
    let state = GamestatsState::default();
    let mut userdata = Vec::new();
    for data in [PUT_P4_SEASON, PUT_P3_SEASON] {
        let (pid, body) = decode_data(data).unwrap();
        leaderboard::put(&state, pid, &body).await.unwrap();
        userdata.push(body[16..].to_vec());
    }
    let expected = vec![
        (1, P4, 28, userdata[0].clone()),
        (2, P3, 4, userdata[1].clone()),
    ];

    for (data, mode) in [(GET_P3_SEASON_TOP, 1), (GET_P4_SEASON_FRIENDS, 3)] {
        let (pid, body) = decode_data(data).unwrap();
        let reply = leaderboard::get(&state, pid, &body).await.unwrap();
        // mode, count 2, total 2: the winner first, userdata as it was put.
        assert_eq!(
            [word(&reply, 0), word(&reply, 1), word(&reply, 2)],
            [mode, 2, 2]
        );
        assert_eq!(rows(&reply), expected);
    }
}

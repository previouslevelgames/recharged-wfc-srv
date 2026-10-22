//! Requests captured from two port clients during a ranked match against the
//! dev stack (2026-10-17, game 2 of the series), replayed through decoding and
//! the leaderboard.

use std::net::IpAddr;

use axum::extract::Query;
use time::{Date, Month};

use super::{
    GamestatsError, GamestatsState, Op, Params, codec::decode_data, leaderboard, process,
    submission::Submission, token::token_hash,
};

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
        assert_eq!(u16::from_le_bytes([userdata[0x0C], userdata[0x0D]]), 1);
        assert_eq!(
            Submission::parse(userdata),
            Submission {
                date: Date::from_calendar_date(2026, Month::October, 17).ok(),
                name: Some(player.to_owned()),
                wins: Some(wins),
                losses: Some(losses),
            }
        );
    }
}

#[sqlx::test]
async fn replays_the_match(pool: sqlx::PgPool) {
    let state = GamestatsState::with_pool(pool);
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

#[sqlx::test]
async fn a_put_counts_only_from_where_its_pid_is_logged_in(pool: sqlx::PgPool) {
    let state = GamestatsState::with_pool(pool);
    let (home, elsewhere) = (
        IpAddr::from([203, 0, 113, 4]),
        IpAddr::from([198, 51, 100, 7]),
    );
    let put = |from: IpAddr| {
        let params = Params {
            pid: Some(P4.to_string()),
            hash: Some(token_hash(&state.tokens.issue(P4))),
            data: Some(PUT_P4_SEASON.to_owned()),
        };
        process(&state, Op::Put, from, Ok(Query(params)))
    };

    assert!(matches!(put(home).await, Err(GamestatsError::NotLoggedIn)));
    state.gpcm.fake_login(P4, home);
    assert!(matches!(
        put(elsewhere).await,
        Err(GamestatsError::NotLoggedIn)
    ));
    assert_eq!(season_top(&state).await, []);

    assert!(put(home).await.is_ok());
    assert_eq!(season_top(&state).await, [(1, P4, 28)]);
}

/// `(order, pid, score)` of each row of the season's top list.
async fn season_top(state: &GamestatsState) -> Vec<(u32, i32, i32)> {
    let (pid, body) = decode_data(GET_P3_SEASON_TOP).unwrap();
    let reply = leaderboard::get(state, pid, &body).await.unwrap();
    rows(&reply)
        .into_iter()
        .map(|(order, pid, score, _)| (order, pid, score))
        .collect()
}

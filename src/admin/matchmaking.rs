//! The Matchmaking page: the master server's sessions, one per game that
//! sends heartbeats, and what each is doing in matchmaking (docs/login.md,
//! "Sessions" and "Filters"). DWC adds its keys to the heartbeats once
//! matchmaking starts; before that a session is only online.

use std::{collections::HashMap, net::SocketAddr};

use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::master::{Master, Session};

/// DWC's `dwc_mtype` (`dwc_match.h`): a ranked search, a search among
/// friends, hosting a friend battle, joining one.
const ANYBODY: u8 = 0;
const JOINING: u8 = 3;

#[derive(Serialize)]
pub struct Matchmaking {
    totals: Totals,
    /// Matchmaking first, then the games heard from last.
    sessions: Vec<Game>,
}

#[derive(Serialize)]
pub struct Totals {
    /// Games sending heartbeats.
    sessions: usize,
    /// In a ranked search.
    searching: usize,
    /// Searching among friends, hosting a friend battle or joining one.
    with_friends: usize,
}

#[derive(Serialize)]
struct Game {
    /// Where its heartbeats come from.
    address: String,
    /// Its own address in its network: `localip0` and `localport`.
    local_address: Option<String>,
    profile_id: Option<i32>,
    mii_name: Option<String>,
    /// `dwc_mtype`; none before matchmaking starts.
    match_type: Option<u8>,
    /// `numplayers` and `maxplayers`: the others in its group, and how many
    /// it wants.
    players: Option<u8>,
    max_players: Option<u8>,
    /// `dwc_mresv`: the profile it reserved a place with.
    reserved_with: Option<i32>,
    /// The game's own keys (`NetworkLobby.cpp`): `GT` (1 tournament, 2 two
    /// players at one console), `CO` (the region group) and `PT` (points).
    mode: Option<u32>,
    region_group: Option<u32>,
    points: Option<u32>,
    natneg: bool,
    /// Whether it answered the challenge.
    listed: bool,
    #[serde(with = "time::serde::rfc3339")]
    heard_at: OffsetDateTime,
}

pub fn totals(master: &Master) -> Totals {
    let sessions = games(master);
    let count = |matching: fn(u8) -> bool| {
        sessions
            .iter()
            .filter(|(_, session)| number(session, "dwc_mtype").is_some_and(matching))
            .count()
    };
    Totals {
        sessions: sessions.len(),
        searching: count(|kind| kind == ANYBODY),
        with_friends: count(|kind| (ANYBODY + 1..=JOINING).contains(&kind)),
    }
}

pub async fn read(pool: &PgPool, master: &Master) -> Result<Matchmaking, sqlx::Error> {
    let totals = totals(master);
    let now = OffsetDateTime::now_utc();
    let mut games: Vec<Game> = games(master)
        .iter()
        .map(|(address, session)| Game::new(*address, session, now))
        .collect();

    let profiles: Vec<i32> = games.iter().filter_map(|game| game.profile_id).collect();
    let names: HashMap<i32, Option<String>> = sqlx::query!(
        "SELECT profile_id, ingamesn FROM profiles WHERE profile_id = ANY($1)",
        &profiles,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| (row.profile_id, row.ingamesn))
    .collect();
    for game in &mut games {
        game.mii_name = game
            .profile_id
            .and_then(|profile| names.get(&profile).cloned().flatten());
    }

    games.sort_by(|a, b| {
        (a.match_type.is_none(), b.heard_at).cmp(&(b.match_type.is_none(), a.heard_at))
    });
    Ok(Matchmaking {
        totals,
        sessions: games,
    })
}

/// The master server's sessions of this game.
fn games(master: &Master) -> Vec<(SocketAddr, Session)> {
    let mut sessions = master.sessions();
    sessions.retain(|(_, session)| session.field("gamename") == Some(crate::GAME_NAME));
    sessions
}

fn number<T: std::str::FromStr>(session: &Session, field: &str) -> Option<T> {
    session.field(field)?.parse().ok()
}

impl Game {
    fn new(address: SocketAddr, session: &Session, now: OffsetDateTime) -> Self {
        let profile_id = number(session, "dwc_pid");
        let local_address = session
            .field("localip0")
            .zip(session.field("localport"))
            .map(|(ip, port)| format!("{ip}:{port}"));
        Self {
            address: address.to_string(),
            local_address,
            profile_id,
            mii_name: None,
            match_type: number(session, "dwc_mtype"),
            players: number(session, "numplayers"),
            max_players: number(session, "maxplayers"),
            reserved_with: number(session, "dwc_mresv")
                .filter(|&with| with != 0 && Some(with) != profile_id),
            mode: number(session, "GT"),
            region_group: number(session, "CO"),
            points: number(session, "PT"),
            natneg: session.field("natneg").is_some_and(|natneg| natneg != "0"),
            listed: session.listed,
            heard_at: now - session.quiet_for(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts;

    /// Player 2's heartbeat during a ranked search.
    const SEARCHING: &[u8] = b"\x03\x5c\xd6s\x9flocalip0\x00192.168.1.131\x00localport\x0053103\x00natneg\x001\x00statechanged\x001\x00gamename\x00mschargedwii\x00publicip\x00-1062731389\x00publicport\x0053103\x00numplayers\x000\x00maxplayers\x001\x00dwc_pid\x001000000001\x00dwc_mtype\x000\x00dwc_mresv\x000\x00dwc_mver\x003\x00dwc_eval\x001\x00GT\x000\x00PI\x001000000001\x00AP\x00463715253\x00PT\x000\x00CO\x000\x00\x00\x00\x00\x00\x00\x00\x00";
    /// A host's heartbeat once its friend reserved a place.
    const HOSTING: &[u8] = b"\x03-\x1e\xa3\x05localip0\x00192.168.1.131\x00localport\x0051021\x00natneg\x001\x00statechanged\x001\x00gamename\x00mschargedwii\x00publicip\x00-1408172031\x00publicport\x0044719\x00numplayers\x000\x00maxplayers\x001\x00dwc_pid\x001000000000\x00dwc_mtype\x002\x00dwc_mresv\x001000000001\x00dwc_mver\x003\x00dwc_eval\x001\x00\x00\x00\x00\x00\x00\x00\x00";
    /// A game that is online, before matchmaking.
    const ONLINE: &[u8] = b"\x03\x01\x02\x03\x04localip0\x0010.0.0.2\x00localport\x0040000\x00natneg\x001\x00gamename\x00mschargedwii\x00publicip\x000\x00publicport\x000\x00numplayers\x000\x00maxplayers\x000\x00\x00\x00\x00";

    #[sqlx::test]
    async fn sessions_show_what_each_game_does(pool: PgPool) {
        accounts::log_in(&pool, 42).await.unwrap();
        let profile = accounts::profile_login(&pool, 42, "R4QEaaaaaaa", Some("Mario"), "10.0.0.1")
            .await
            .unwrap();
        assert_eq!(profile.profile_id, 1000000000);

        let master = Master::default();
        master.answer(ONLINE, "203.0.113.9:40000".parse().unwrap());
        master.answer(SEARCHING, "192.168.1.131:53103".parse().unwrap());
        master.answer(HOSTING, "172.17.0.1:44719".parse().unwrap());

        let totals = totals(&master);
        assert_eq!(
            (totals.sessions, totals.searching, totals.with_friends),
            (3, 1, 1)
        );
        let page = read(&pool, &master).await.unwrap();
        let [first, second, online] = &page.sessions[..] else {
            panic!("three sessions");
        };
        // The two in matchmaking first.
        assert!(first.match_type.is_some() && second.match_type.is_some());
        let (searching, hosting) = if first.match_type == Some(ANYBODY) {
            (first, second)
        } else {
            (second, first)
        };

        assert_eq!(searching.address, "192.168.1.131:53103");
        assert_eq!(searching.profile_id, Some(1000000001));
        assert_eq!(searching.mii_name, None); // No such profile here.
        assert_eq!(
            (searching.players, searching.max_players),
            (Some(0), Some(1))
        );
        assert_eq!(searching.reserved_with, None);
        assert_eq!(
            (searching.mode, searching.region_group, searching.points),
            (Some(0), Some(0), Some(0))
        );
        assert!(searching.natneg);

        assert_eq!(hosting.profile_id, Some(1000000000));
        assert_eq!(hosting.mii_name.as_deref(), Some("Mario"));
        assert_eq!(hosting.match_type, Some(2));
        assert_eq!(hosting.reserved_with, Some(1000000001));
        assert_eq!(
            hosting.local_address.as_deref(),
            Some("192.168.1.131:51021")
        );

        assert_eq!(online.match_type, None);
        assert_eq!(online.profile_id, None);
        assert!(!online.listed);
    }
}

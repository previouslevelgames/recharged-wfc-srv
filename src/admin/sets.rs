//! A competitive set's live state, for diagnosing (docs/install.md,
//! "Diagnosing a set"): the set as the database has it, how the server keeps
//! its games for RANKED (and any game of a player that another replaced),
//! each player's heartbeat, Ready to play! and game calls, their WFC
//! profiles with their logins and master-server sessions, the reports, and
//! the matchmaking events of those profiles since the set began: searches
//! and what they found, match commands and whether they reached their game,
//! NAT negotiations.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::{
    activity::{Activity, Event, What},
    gpcm::Gpcm,
    master::Master,
    pairing::{Pairings, View},
};

#[derive(Serialize)]
pub struct Timeline {
    set: Set,
    /// How the server keeps the set's games for RANKED (in memory).
    pairing: View,
    /// The challenger's side, then the challenged's.
    sides: Vec<Side>,
    reports: Vec<Report>,
    /// The matchmaking events of the players' WFC profiles since the set
    /// began, the latest first.
    events: Vec<Event>,
    #[serde(with = "time::serde::rfc3339")]
    now: OffsetDateTime,
}

#[derive(Serialize)]
struct Set {
    id: i64,
    state: String,
    rule_set: String,
    random: bool,
    rematch_of: Option<i64>,
    winner: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
    /// Both players ready: the games went to RANKED.
    #[serde(with = "time::serde::rfc3339::option")]
    go_at: Option<OffsetDateTime>,
    /// The games found each other.
    #[serde(with = "time::serde::rfc3339::option")]
    playing_at: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct Side {
    player_id: i64,
    name: String,
    /// The player's launcher's or game's last request.
    #[serde(with = "time::serde::rfc3339::option")]
    heartbeat_at: Option<OffsetDateTime>,
    /// Ready to play!
    #[serde(with = "time::serde::rfc3339::option")]
    ready_at: Option<OffsetDateTime>,
    /// The set's game's last call saying it is ready (also refused ones).
    #[serde(with = "time::serde::rfc3339::option")]
    game_called_at: Option<OffsetDateTime>,
    /// The latest bound first.
    profiles: Vec<Profile>,
}

#[derive(Serialize)]
struct Profile {
    profile_id: i32,
    /// The release, from the profile's `gsbrcd`.
    game: String,
    mii: Option<String>,
    /// Logged in to WFC (GPCM) now: from where.
    logged_in_from: Option<String>,
    /// Its game's master-server session, while it sends heartbeats.
    session: Option<Session>,
}

#[derive(Serialize)]
struct Session {
    address: String,
    /// `localip0` and `localport`.
    local_address: Option<String>,
    /// `dwc_mtype` (0: a ranked search); none before matchmaking.
    match_type: Option<u8>,
    /// `dwc_mresv`: the profile it reserved a place with.
    reserved_with: Option<i32>,
    /// Whether it answered the challenge.
    listed: bool,
    /// Seconds since its last heartbeat.
    quiet_secs: f64,
}

#[derive(Serialize)]
struct Report {
    player_id: i64,
    #[serde(with = "time::serde::rfc3339")]
    reported_at: OffsetDateTime,
    games: serde_json::Value,
}

/// Set `id`'s timeline; `None` for no such set.
pub async fn timeline(
    pool: &PgPool,
    gpcm: &Gpcm,
    master: &Master,
    activity: &Activity,
    pairings: &Pairings,
    id: i64,
) -> Result<Option<Timeline>, sqlx::Error> {
    let Some(set) = sqlx::query!(
        "SELECT match_id, state, home, away, rule_set, random, rematch_of, winner,
                created_at, updated_at, go_at, playing_at,
                home_ready_at, away_ready_at, home_game_ready_at, away_game_ready_at
         FROM matches WHERE match_id = $1",
        id,
    )
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let players: HashMap<i64, (String, Option<OffsetDateTime>)> = sqlx::query!(
        "SELECT player_id, display_name, heartbeat_at FROM players WHERE player_id = ANY($1)",
        &[set.home, set.away],
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| (row.player_id, (row.display_name, row.heartbeat_at)))
    .collect();
    let bound = sqlx::query!(
        "SELECT player_wfc_profiles.player_id, profiles.profile_id, profiles.gsbrcd,
                profiles.ingamesn
         FROM player_wfc_profiles JOIN profiles USING (profile_id)
         WHERE player_wfc_profiles.player_id = ANY($1)
         ORDER BY player_wfc_profiles.bound_at DESC",
        &[set.home, set.away],
    )
    .fetch_all(pool)
    .await?;
    let reports = sqlx::query!(
        "SELECT player_id, games, reported_at FROM match_reports
         WHERE match_id = $1 ORDER BY reported_at",
        id,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| Report {
        player_id: row.player_id,
        reported_at: row.reported_at,
        games: serde_json::from_str(&row.games).unwrap_or_default(),
    })
    .collect();

    let presence = gpcm.presence().await?;
    // Each profile's freshest session.
    let sessions = master.sessions();
    let session_of = |profile_id: i32| {
        let pid = profile_id.to_string();
        sessions
            .iter()
            .filter(|(_, session)| session.field("dwc_pid") == Some(pid.as_str()))
            .min_by_key(|(_, session)| session.quiet_for())
    };
    let mut addresses: HashMap<String, i32> = HashMap::new();
    let mut sides = Vec::new();
    for (player_id, ready_at, game_called_at) in [
        (set.home, set.home_ready_at, set.home_game_ready_at),
        (set.away, set.away_ready_at, set.away_game_ready_at),
    ] {
        let (name, heartbeat_at) = players.get(&player_id).cloned().unwrap_or_default();
        let profiles = bound
            .iter()
            .filter(|row| row.player_id == player_id)
            .map(|row| {
                let session = session_of(row.profile_id).map(|(address, session)| {
                    addresses.insert(address.to_string(), row.profile_id);
                    Session {
                        address: address.to_string(),
                        local_address: session
                            .field("localip0")
                            .zip(session.field("localport"))
                            .map(|(ip, port)| format!("{ip}:{port}")),
                        match_type: session
                            .field("dwc_mtype")
                            .and_then(|value| value.parse().ok()),
                        reserved_with: session
                            .field("dwc_mresv")
                            .and_then(|value| value.parse().ok()),
                        listed: session.listed,
                        quiet_secs: session.quiet_for().as_secs_f64(),
                    }
                });
                Profile {
                    profile_id: row.profile_id,
                    game: row.gsbrcd.chars().take(4).collect(),
                    mii: row.ingamesn.clone(),
                    logged_in_from: presence
                        .get(&row.profile_id)
                        .map(|online| online.address.to_string()),
                    session,
                }
            })
            .collect();
        sides.push(Side {
            player_id,
            name,
            heartbeat_at,
            ready_at,
            game_called_at,
            profiles,
        });
    }

    // The events of the players' profiles since the set began; a
    // negotiation's games by the profile of the session at their address.
    let profiles: Vec<i32> = bound.iter().map(|row| row.profile_id).collect();
    let ours = |profile: &Option<i32>| profile.is_some_and(|profile| profiles.contains(&profile));
    let events = activity
        .events()
        .into_iter()
        .filter(|event| event.last_at >= set.created_at)
        .filter_map(|mut event| {
            let involved = match &mut event.what {
                What::Search { profile_id, .. } => ours(profile_id),
                What::Command { from, to, .. } => ours(from) || ours(to),
                What::Negotiation { games, .. } => {
                    for party in games.iter_mut() {
                        party.profile_id = addresses.get(&party.address).copied();
                    }
                    games.iter().any(|party| ours(&party.profile_id))
                }
            };
            involved.then_some(event)
        })
        .collect();

    Ok(Some(Timeline {
        set: Set {
            id: set.match_id,
            state: set.state,
            rule_set: set.rule_set,
            random: set.random,
            rematch_of: set.rematch_of,
            winner: set.winner,
            created_at: set.created_at,
            updated_at: set.updated_at,
            go_at: set.go_at,
            playing_at: set.playing_at,
        },
        pairing: pairings.view(id).await?,
        sides,
        reports,
        events,
        now: OffsetDateTime::now_utc(),
    }))
}

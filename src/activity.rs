//! What happened in matchmaking lately, for `/admin`: the ranked searches,
//! the match commands games send each other (reservations and their answers,
//! through the server browser or GPCM, and whether the server browser's
//! reached the game) and the NAT negotiations. In memory only, the last
//! [`KEPT`] events; a search repeated every few seconds, or the same command
//! again, is one event with a count. A search keeps whom it found and, for
//! diagnosing, the other games in a ranked search it left out and why.

use std::{collections::VecDeque, net::SocketAddr, sync::Mutex, time::Duration};

use serde::Serialize;
use time::OffsetDateTime;

/// Events kept, the latest: enough for a set's whole search on a busy server
/// (a set's timeline picks its players' from them).
const KEPT: usize = 1000;
/// The same search or command again within this long is the same event.
const REPEAT: Duration = Duration::from_secs(30);
/// Profiles a search event keeps, found and left out: more than a busy
/// server has searching at once.
const KEPT_PROFILES: usize = 32;

#[derive(Default)]
pub struct Activity {
    /// The oldest first.
    events: Mutex<VecDeque<Event>>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    #[serde(with = "time::serde::rfc3339")]
    pub first_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_at: OffsetDateTime,
    #[serde(flatten)]
    pub what: What,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum What {
    /// A search for sessions by a filter (docs/login.md, "Filters"), by the
    /// profile the filter leaves out.
    Search {
        profile_id: Option<i32>,
        filter: String,
        searches: u32,
        /// Sessions the last one found.
        found: usize,
        /// The profiles its searches found, each once.
        found_profiles: Vec<i32>,
        /// The other games in a ranked search (`dwc_mtype` 0) its searches
        /// left out, each once with the latest reason.
        missed: Vec<Miss>,
    },
    /// A DWC match command (`dwc_match.h`) from one game to another.
    Command {
        from: Option<i32>,
        to: Option<i32>,
        command: u8,
        via: Via,
        count: u32,
        /// Through the server browser: how many the game acknowledged, and
        /// how many it never did (the master server gave up resending).
        delivered: u32,
        undelivered: u32,
    },
    /// A NAT negotiation, by its cookie.
    Negotiation {
        cookie: String,
        /// The games' sockets the other one pings.
        games: Vec<Party>,
        /// Whether both got the other's address.
        connected: bool,
        /// Whether that address was the relay's (src/relay.rs).
        relayed: bool,
        reports: Vec<Report>,
    },
}

/// What one search listed: how many sessions, the profiles among them, and
/// the other games in a ranked search it left out.
#[derive(Debug, Default)]
pub struct Listed {
    pub count: usize,
    pub profiles: Vec<i32>,
    pub missed: Vec<Miss>,
}

/// A game a search left out, and why: the part of the filter its session
/// failed with its values, or how a set keeps the games.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Miss {
    pub profile_id: i32,
    pub why: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    ServerBrowser,
    Gpcm,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Party {
    pub address: String,
    /// Filled in for the page, from the session at that address.
    pub profile_id: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    /// The game's client index, 0 or 1.
    pub index: Option<u8>,
    /// 1 when the two reached each other.
    pub result: Option<u8>,
}

impl Activity {
    /// The events, the latest first.
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().iter().rev().cloned().collect()
    }

    pub fn search(&self, profile_id: Option<i32>, filter: &str, listed: Listed) {
        let Listed {
            count,
            profiles,
            missed,
        } = listed;
        let remember = |found: &mut Vec<i32>, left_out: &mut Vec<Miss>| {
            for profile in &profiles {
                if !found.contains(profile) && found.len() < KEPT_PROFILES {
                    found.push(*profile);
                }
            }
            for miss in &missed {
                match left_out
                    .iter()
                    .position(|was| was.profile_id == miss.profile_id)
                {
                    Some(at) => left_out[at].why.clone_from(&miss.why),
                    None if left_out.len() < KEPT_PROFILES => left_out.push(miss.clone()),
                    None => {}
                }
            }
        };
        self.record(
            |what| {
                let What::Search {
                    profile_id: was,
                    filter: was_filter,
                    searches,
                    found: was_found,
                    found_profiles,
                    missed,
                } = what
                else {
                    return false;
                };
                if *was != profile_id || was_filter != filter {
                    return false;
                }
                *searches += 1;
                *was_found = count;
                remember(found_profiles, missed);
                true
            },
            || {
                let (mut found_profiles, mut left_out) = (Vec::new(), Vec::new());
                remember(&mut found_profiles, &mut left_out);
                What::Search {
                    profile_id,
                    filter: filter.to_owned(),
                    searches: 1,
                    found: count,
                    found_profiles,
                    missed: left_out,
                }
            },
        );
    }

    pub fn command(&self, from: Option<i32>, to: Option<i32>, command: u8, via: Via) {
        self.record(
            |what| match what {
                What::Command {
                    from: was_from,
                    to: was_to,
                    command: was,
                    via: was_via,
                    count,
                    ..
                } if (*was_from, *was_to, *was, *was_via) == (from, to, command, via) => {
                    *count += 1;
                    true
                }
                _ => false,
            },
            || What::Command {
                from,
                to,
                command,
                via,
                count: 1,
                delivered: 0,
                undelivered: 0,
            },
        );
    }

    /// A server-browser command reached its game (`acked`) or never did: its
    /// latest event counts it. Neither moves the event nor makes it recent.
    pub fn delivered(&self, from: Option<i32>, to: Option<i32>, command: u8, acked: bool) {
        let mut events = self.events.lock().unwrap();
        let latest = events
            .iter_mut()
            .rev()
            .find_map(|event| match &mut event.what {
                What::Command {
                    from: was_from,
                    to: was_to,
                    command: was,
                    via: Via::ServerBrowser,
                    delivered,
                    undelivered,
                    ..
                } if (*was_from, *was_to, *was) == (from, to, command) => {
                    Some(if acked { delivered } else { undelivered })
                }
                _ => None,
            });
        if let Some(counter) = latest {
            *counter += 1;
        }
    }

    /// A game's socket in the negotiation `cookie`, which starts it.
    pub fn negotiation(&self, cookie: u32, game: Option<SocketAddr>) {
        self.update_negotiation(cookie, |games, _, _, _| {
            if let Some(game) = game.map(|game| game.to_string())
                && !games.iter().any(|party| party.address == game)
            {
                games.push(Party {
                    address: game,
                    profile_id: None,
                });
            }
        });
    }

    /// Both games got the other's address, or the relay's.
    pub fn connected(&self, cookie: u32, relay: bool) {
        self.update_negotiation(cookie, |_, connected, relayed, _| {
            *connected = true;
            *relayed = relay;
        });
    }

    pub fn report(&self, cookie: u32, index: Option<u8>, result: Option<u8>) {
        self.update_negotiation(cookie, |_, _, _, reports| {
            reports.retain(|report| report.index != index);
            reports.push(Report { index, result });
        });
    }

    fn update_negotiation(
        &self,
        cookie: u32,
        update: impl Fn(&mut Vec<Party>, &mut bool, &mut bool, &mut Vec<Report>),
    ) {
        let cookie = format!("{cookie:08x}");
        self.record(
            |what| match what {
                What::Negotiation {
                    cookie: was,
                    games,
                    connected,
                    relayed,
                    reports,
                } if *was == cookie => {
                    update(games, connected, relayed, reports);
                    true
                }
                _ => false,
            },
            || {
                let (mut games, mut connected, mut relayed, mut reports) =
                    (Vec::new(), false, false, Vec::new());
                update(&mut games, &mut connected, &mut relayed, &mut reports);
                What::Negotiation {
                    cookie: cookie.clone(),
                    games,
                    connected,
                    relayed,
                    reports,
                }
            },
        );
    }

    /// Updates the latest recent event `again` takes as the same, which then
    /// moves to the end; or adds `new`.
    fn record(&self, mut again: impl FnMut(&mut What) -> bool, new: impl FnOnce() -> What) {
        let now = OffsetDateTime::now_utc();
        let mut events = self.events.lock().unwrap();
        let recent = events
            .iter_mut()
            .enumerate()
            .rev()
            .take_while(|(_, event)| now - event.last_at < REPEAT)
            .find_map(|(at, event)| again(&mut event.what).then_some(at));
        let event = match recent.and_then(|at| events.remove(at)) {
            Some(event) => Event {
                last_at: now,
                ..event
            },
            None => Event {
                first_at: now,
                last_at: now,
                what: new(),
            },
        };
        events.push_back(event);
        while events.len() > KEPT {
            events.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RANKED: &str = "dwc_mver = 3 and dwc_pid != 1000000000 and (GT = 0)";

    /// A search that found `count` sessions without profiles.
    fn found(count: usize) -> Listed {
        Listed {
            count,
            ..Listed::default()
        }
    }

    #[test]
    fn a_repeated_search_is_one_event() {
        let activity = Activity::default();
        activity.search(Some(1000000000), RANKED, found(0));
        activity.search(Some(1000000000), RANKED, found(0));
        activity.search(Some(1000000001), RANKED, found(0));
        activity.search(Some(1000000000), RANKED, found(1));

        let events = activity.events();
        let [latest, other] = &events[..] else {
            panic!("two events: {events:?}");
        };
        assert_eq!(
            latest.what,
            What::Search {
                profile_id: Some(1000000000),
                filter: RANKED.to_owned(),
                searches: 3,
                found: 1,
                found_profiles: Vec::new(),
                missed: Vec::new(),
            }
        );
        assert!(latest.first_at <= latest.last_at);
        assert!(matches!(
            other.what,
            What::Search {
                profile_id: Some(1000000001),
                searches: 1,
                ..
            }
        ));
    }

    #[test]
    fn a_search_keeps_whom_it_found_and_why_it_left_others_out() {
        let activity = Activity::default();
        let miss = |profile_id: i32, why: &str| Miss {
            profile_id,
            why: why.to_owned(),
        };
        activity.search(
            Some(1),
            RANKED,
            Listed {
                count: 1,
                profiles: vec![2],
                missed: vec![miss(3, "CO = 0 (CO is 1)"), miss(4, "kept")],
            },
        );
        activity.search(
            Some(1),
            RANKED,
            Listed {
                count: 1,
                profiles: vec![3],
                missed: vec![miss(4, "GT = 0 (GT is 2)")],
            },
        );
        let What::Search {
            found,
            found_profiles,
            missed,
            ..
        } = &activity.events()[0].what
        else {
            panic!("a search");
        };
        assert_eq!(*found, 1);
        assert_eq!(found_profiles, &[2, 3]);
        assert_eq!(
            missed,
            &[miss(3, "CO = 0 (CO is 1)"), miss(4, "GT = 0 (GT is 2)")]
        );
    }

    #[test]
    fn commands_count_their_repeats() {
        let activity = Activity::default();
        activity.command(Some(1), Some(2), 0x01, Via::ServerBrowser);
        activity.command(Some(2), Some(1), 0x02, Via::ServerBrowser);
        activity.command(Some(2), Some(1), 0x02, Via::ServerBrowser);
        activity.command(Some(2), Some(1), 0x02, Via::Gpcm);

        let counts: Vec<(u8, u32, Via)> = activity
            .events()
            .into_iter()
            .map(|event| match event.what {
                What::Command {
                    command,
                    count,
                    via,
                    ..
                } => (command, count, via),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            counts,
            [
                (0x02, 1, Via::Gpcm),
                (0x02, 2, Via::ServerBrowser),
                (0x01, 1, Via::ServerBrowser)
            ]
        );
    }

    #[test]
    fn a_command_counts_whether_it_reached_its_game() {
        let activity = Activity::default();
        activity.command(Some(1), Some(2), 0x06, Via::ServerBrowser);
        activity.command(Some(2), Some(1), 0x02, Via::ServerBrowser);
        activity.delivered(Some(1), Some(2), 0x06, true);
        activity.command(Some(1), Some(2), 0x06, Via::ServerBrowser);
        activity.delivered(Some(1), Some(2), 0x06, false);
        // Another command than any event's: nothing to count it in.
        activity.delivered(Some(3), Some(4), 0x01, true);

        let events = activity.events();
        let [latest, other] = &events[..] else {
            panic!("two events: {events:?}");
        };
        assert!(matches!(
            latest.what,
            What::Command {
                command: 0x06,
                count: 2,
                delivered: 1,
                undelivered: 1,
                ..
            }
        ));
        assert!(matches!(
            other.what,
            What::Command {
                command: 0x02,
                delivered: 0,
                undelivered: 0,
                ..
            }
        ));
    }

    #[test]
    fn a_negotiation_gathers_its_games_and_reports() {
        let activity = Activity::default();
        let (one, two) = (
            "172.17.0.1:52263".parse().ok(),
            "192.168.1.131:53103".parse().ok(),
        );
        activity.negotiation(0x65e4ca01, one);
        activity.negotiation(0x65e4ca01, None);
        activity.negotiation(0x65e4ca01, two);
        activity.negotiation(0x65e4ca01, one);
        activity.connected(0x65e4ca01, true);
        activity.report(0x65e4ca01, Some(1), Some(1));
        activity.report(0x65e4ca01, Some(0), Some(1));
        activity.report(0x65e4ca01, Some(1), Some(1));

        let [event] = &activity.events()[..] else {
            panic!("one event");
        };
        let What::Negotiation {
            cookie,
            games,
            connected,
            relayed,
            reports,
        } = &event.what
        else {
            panic!("{event:?}");
        };
        assert_eq!(cookie, "65e4ca01");
        let addresses: Vec<&str> = games.iter().map(|party| party.address.as_str()).collect();
        assert_eq!(addresses, ["172.17.0.1:52263", "192.168.1.131:53103"]);
        assert!(*connected && *relayed);
        assert_eq!(reports.len(), 2);
    }

    #[test]
    fn only_the_latest_events_are_kept() {
        let activity = Activity::default();
        for profile in 0..(KEPT as i32 + 10) {
            activity.search(Some(profile), RANKED, found(0));
        }
        let events = activity.events();
        assert_eq!(events.len(), KEPT);
        assert!(matches!(
            events[0].what,
            What::Search { profile_id: Some(profile), .. } if profile == KEPT as i32 + 9
        ));
    }

    #[test]
    fn events_serialize_with_their_kind() {
        let activity = Activity::default();
        activity.command(Some(1), None, 0x06, Via::ServerBrowser);
        let json = serde_json::to_value(&activity.events()[0]).unwrap();
        assert_eq!(json["kind"], "command");
        assert_eq!(json["via"], "server_browser");
        assert_eq!(json["command"], 6);
        assert!(json["first_at"].is_string());
    }
}

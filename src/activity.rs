//! What happened in matchmaking lately, for `/admin`: the ranked searches,
//! the match commands games send each other (reservations and their answers,
//! through the server browser or GPCM) and the NAT negotiations. In memory
//! only, the last [`KEPT`] events; a search repeated every few seconds, or the
//! same command again, is one event with a count.

use std::{collections::VecDeque, net::SocketAddr, sync::Mutex, time::Duration};

use serde::Serialize;
use time::OffsetDateTime;

/// Events kept, the latest.
const KEPT: usize = 50;
/// The same search or command again within this long is the same event.
const REPEAT: Duration = Duration::from_secs(30);

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
    },
    /// A DWC match command (`dwc_match.h`) from one game to another.
    Command {
        from: Option<i32>,
        to: Option<i32>,
        command: u8,
        via: Via,
        count: u32,
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

    pub fn search(&self, profile_id: Option<i32>, filter: &str, found: usize) {
        self.record(
            |what| {
                let What::Search {
                    profile_id: was,
                    filter: was_filter,
                    searches,
                    found: was_found,
                } = what
                else {
                    return false;
                };
                if *was != profile_id || was_filter != filter {
                    return false;
                }
                *searches += 1;
                *was_found = found;
                true
            },
            || What::Search {
                profile_id,
                filter: filter.to_owned(),
                searches: 1,
                found,
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
            },
        );
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

    #[test]
    fn a_repeated_search_is_one_event() {
        let activity = Activity::default();
        activity.search(Some(1000000000), RANKED, 0);
        activity.search(Some(1000000000), RANKED, 0);
        activity.search(Some(1000000001), RANKED, 0);
        activity.search(Some(1000000000), RANKED, 1);

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
            activity.search(Some(profile), RANKED, 0);
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

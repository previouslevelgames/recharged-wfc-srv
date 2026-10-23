//! Games kept for a match (docs/profiles.md, "Meeting in RANKED"): once a
//! match between two players is accepted, each player's game says it is
//! ready, logged in to WFC as one of the player's profiles. A ranked search
//! by a ready game then lists only the other one, as soon as it is ready
//! too, and nobody else's search lists either of them. Games of different
//! releases can't play each other, so they are never paired.
//!
//! Readiness lives in memory and lasts [`READY_TTL`]: the port says it again
//! while the game is online, so a game that went away stops being kept.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

/// How long a game stays ready without saying it again.
pub const READY_TTL: Duration = Duration::from_secs(30);

/// The ready games of each match, by match.
#[derive(Default)]
pub struct Pairings {
    matches: Mutex<HashMap<i64, [Option<Ready>; 2]>>,
}

#[derive(Clone)]
struct Ready {
    profile_id: i32,
    /// The release, `R4QE` or `R4QP`.
    game: String,
    at: Instant,
}

impl Ready {
    fn live(&self, now: Instant) -> bool {
        now.duration_since(self.at) < READY_TTL
    }
}

/// How a match's games stand.
#[derive(Debug, PartialEq)]
pub enum Pairing {
    /// The opponent's game is not ready.
    Waiting,
    /// Both are: they find each other in RANKED.
    Paired,
    /// Both are, but of different releases.
    OtherRelease,
}

/// Whom a ranked search may find.
#[derive(Debug, PartialEq)]
pub enum Visible {
    /// The searcher is in no match: every game but those kept for one.
    Everyone,
    /// The searcher's opponent, who is ready too.
    Only(i32),
    /// Nobody yet: the opponent is not ready, or can't be played.
    Nobody,
}

impl Pairings {
    /// `profile_id`, a game of `game`, side `side` (0 the challenger, 1 the
    /// challenged) of `match_id`, is ready.
    pub fn ready(&self, match_id: i64, side: usize, profile_id: i32, game: &str) -> Pairing {
        let now = Instant::now();
        let mut matches = self.matches.lock().unwrap();
        matches.retain(|_, sides| sides.iter().flatten().any(|ready| ready.live(now)));
        // A profile is kept for one match at a time.
        for (other, sides) in matches.iter_mut() {
            if *other != match_id {
                for slot in sides.iter_mut() {
                    if slot
                        .as_ref()
                        .is_some_and(|ready| ready.profile_id == profile_id)
                    {
                        *slot = None;
                    }
                }
            }
        }
        let sides = matches.entry(match_id).or_default();
        sides[side] = Some(Ready {
            profile_id,
            game: game.to_owned(),
            at: now,
        });
        pairing(sides, now)
    }

    /// The match `match_id` ended: its games are no longer kept.
    pub fn end(&self, match_id: i64) {
        self.matches.lock().unwrap().remove(&match_id);
    }

    /// How the games of `match_id` stand.
    pub fn pairing(&self, match_id: i64) -> Pairing {
        let now = Instant::now();
        let matches = self.matches.lock().unwrap();
        matches
            .get(&match_id)
            .map_or(Pairing::Waiting, |sides| pairing(sides, now))
    }

    /// Whom a ranked search by `profile_id` may find.
    pub fn visible_to(&self, profile_id: i32) -> Visible {
        let now = Instant::now();
        let matches = self.matches.lock().unwrap();
        for sides in matches.values() {
            for side in 0..2 {
                if sides[side]
                    .as_ref()
                    .is_some_and(|ready| ready.profile_id == profile_id && ready.live(now))
                {
                    return match (&sides[1 - side], pairing(sides, now)) {
                        (Some(other), Pairing::Paired) => Visible::Only(other.profile_id),
                        _ => Visible::Nobody,
                    };
                }
            }
        }
        Visible::Everyone
    }

    /// Whether `profile_id`'s game is kept for a match.
    pub fn kept(&self, profile_id: i32) -> bool {
        self.visible_to(profile_id) != Visible::Everyone
    }
}

fn pairing(sides: &[Option<Ready>; 2], now: Instant) -> Pairing {
    match sides {
        [Some(home), Some(away)] if home.live(now) && away.live(now) => {
            if home.game == away.game {
                Pairing::Paired
            } else {
                Pairing::OtherRelease
            }
        }
        _ => Pairing::Waiting,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_ready_games_see_only_each_other() {
        let pairings = Pairings::default();
        assert_eq!(pairings.visible_to(1), Visible::Everyone);

        assert_eq!(pairings.ready(7, 0, 1, "R4QE"), Pairing::Waiting);
        assert_eq!(pairings.visible_to(1), Visible::Nobody);
        assert!(pairings.kept(1));
        assert_eq!(pairings.pairing(7), Pairing::Waiting);

        assert_eq!(pairings.ready(7, 1, 2, "R4QE"), Pairing::Paired);
        assert_eq!(pairings.pairing(7), Pairing::Paired);
        assert_eq!(pairings.visible_to(1), Visible::Only(2));
        assert_eq!(pairings.visible_to(2), Visible::Only(1));
        assert_eq!(pairings.visible_to(3), Visible::Everyone);

        pairings.end(7);
        assert_eq!(pairings.visible_to(1), Visible::Everyone);
        assert!(!pairings.kept(2));
    }

    #[test]
    fn releases_are_never_paired() {
        let pairings = Pairings::default();
        pairings.ready(7, 0, 1, "R4QE");
        assert_eq!(pairings.ready(7, 1, 2, "R4QP"), Pairing::OtherRelease);
        assert_eq!(pairings.visible_to(1), Visible::Nobody);
        assert_eq!(pairings.visible_to(2), Visible::Nobody);
    }

    #[test]
    fn a_profile_is_kept_for_its_latest_match() {
        let pairings = Pairings::default();
        pairings.ready(7, 0, 1, "R4QE");
        pairings.ready(7, 1, 2, "R4QE");
        pairings.ready(8, 0, 1, "R4QE");
        assert_eq!(pairings.visible_to(1), Visible::Nobody);
        assert_eq!(pairings.visible_to(2), Visible::Nobody);
        assert_eq!(pairings.pairing(7), Pairing::Waiting);
    }
}

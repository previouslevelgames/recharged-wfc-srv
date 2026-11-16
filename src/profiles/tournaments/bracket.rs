//! A tournament's bracket, without the database (docs/tournaments.md,
//! "Brackets"): the sets of single or double elimination for a number of
//! entrants in seed order, where each set's two players come from, and, from
//! the results so far, who plays each set, who won it and the standings.
//!
//! A bracket is planned once, when the tournament starts: [`plan`]. After
//! that only the results change, and [`resolve`] works the rest out again
//! from them, so the store keeps nothing that could disagree with them.

/// Single elimination, or double: a player is out after two lost sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Single,
    Double,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Single => "single",
            Format::Double => "double",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "single" => Some(Format::Single),
            "double" => Some(Format::Double),
            _ => None,
        }
    }
}

/// The part of the bracket a set is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Winners,
    Losers,
    /// Double elimination: the winners' bracket winner against the losers'.
    GrandFinal,
    /// Played only when the player from the losers' bracket won the grand
    /// final: then both have lost once.
    GrandFinalReset,
}

impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Side::Winners => "winners",
            Side::Losers => "losers",
            Side::GrandFinal => "grand_final",
            Side::GrandFinalReset => "grand_final_reset",
        }
    }
}

/// Where a set's player comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum From {
    /// The entrant of this seed (0 the first); a seed past the entrants is a
    /// bye: nobody comes.
    Seed(usize),
    /// The winner of the set at this index of the plan.
    Winner(usize),
    /// Its loser.
    Loser(usize),
}

/// A set of the bracket as it was planned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetPlan {
    pub side: Side,
    /// From 1, within its side.
    pub round: u32,
    /// From 0, within its round.
    pub position: u32,
    pub from: [From; 2],
    /// A final (single: the last round; double: the winners' and losers'
    /// finals and the grand final, reset included): it may be played to
    /// more games.
    pub finals: bool,
}

/// The most entrants a bracket takes.
pub const MAX_ENTRANTS: usize = 256;

/// The order of the seeds in the first round of a bracket of `size` (a power
/// of 2) so that the best seeds meet last: 1-8, 4-5, 2-7, 3-6 for 8 (from 0
/// here).
pub fn seed_order(size: usize) -> Vec<usize> {
    let mut order = vec![0];
    while order.len() < size {
        let next = order.len() * 2;
        order = order
            .iter()
            .flat_map(|&seed| [seed, next - 1 - seed])
            .collect();
    }
    order
}

/// The sets of a bracket for `entrants` players in seed order (at least 2,
/// at most [`MAX_ENTRANTS`]), every set after the ones it takes its players
/// from. Byes fill the first round up to a power of 2, against the best
/// seeds.
pub fn plan(format: Format, entrants: usize) -> Vec<SetPlan> {
    assert!((2..=MAX_ENTRANTS).contains(&entrants));
    let size = entrants.next_power_of_two();
    let rounds = size.trailing_zeros();
    let order = seed_order(size);
    let mut sets = Vec::new();
    // The winners' bracket, round by round: the index of each round's sets.
    let mut winners: Vec<Vec<usize>> = Vec::new();
    for round in 1..=rounds {
        let mut this = Vec::new();
        let count = size >> round;
        for position in 0..count {
            let from = if round == 1 {
                [
                    From::Seed(order[2 * position]),
                    From::Seed(order[2 * position + 1]),
                ]
            } else {
                let before = &winners[round as usize - 2];
                [
                    From::Winner(before[2 * position]),
                    From::Winner(before[2 * position + 1]),
                ]
            };
            this.push(sets.len());
            sets.push(SetPlan {
                side: Side::Winners,
                round,
                position: position as u32,
                from,
                finals: round == rounds,
            });
        }
        winners.push(this);
    }
    if format == Format::Single {
        return sets;
    }
    // The losers' bracket: the first round pairs the losers of the winners'
    // first; then each round takes the losers of the next winners' round
    // (crossed, so that two players don't meet again soon) and a round
    // halves the field, until one is left for the grand final.
    let mut losers: Vec<usize> = Vec::new();
    if rounds >= 2 {
        let mut round = 1;
        let first = &winners[0];
        for position in 0..first.len() / 2 {
            losers.push(sets.len());
            sets.push(SetPlan {
                side: Side::Losers,
                round,
                position: position as u32,
                from: [
                    From::Loser(first[2 * position]),
                    From::Loser(first[2 * position + 1]),
                ],
                finals: false,
            });
        }
        for winners_round in 2..=rounds as usize {
            let dropping = &winners[winners_round - 1];
            let crossed: Vec<usize> = if winners_round % 2 == 0 {
                dropping.iter().rev().copied().collect()
            } else {
                let half = dropping.len() / 2;
                dropping[half..]
                    .iter()
                    .chain(&dropping[..half])
                    .copied()
                    .collect()
            };
            // The losers dropping in, against the ones left.
            round += 1;
            let left = std::mem::take(&mut losers);
            for (position, (&stayed, &dropped)) in left.iter().zip(&crossed).enumerate() {
                losers.push(sets.len());
                sets.push(SetPlan {
                    side: Side::Losers,
                    round,
                    position: position as u32,
                    from: [From::Winner(stayed), From::Loser(dropped)],
                    finals: winners_round == rounds as usize,
                });
            }
            if winners_round == rounds as usize {
                break;
            }
            // Then the ones left play each other.
            round += 1;
            let left = std::mem::take(&mut losers);
            for position in 0..left.len() / 2 {
                losers.push(sets.len());
                sets.push(SetPlan {
                    side: Side::Losers,
                    round,
                    position: position as u32,
                    from: [
                        From::Winner(left[2 * position]),
                        From::Winner(left[2 * position + 1]),
                    ],
                    finals: false,
                });
            }
        }
    }
    let winners_final = winners[rounds as usize - 1][0];
    // Two entrants: the losers' bracket is the first set's loser alone.
    let from_losers = match losers.as_slice() {
        [last] => From::Winner(*last),
        _ => From::Loser(winners_final),
    };
    let grand_final = sets.len();
    sets.push(SetPlan {
        side: Side::GrandFinal,
        round: 1,
        position: 0,
        from: [From::Winner(winners_final), from_losers],
        finals: true,
    });
    // The winners' side player (the grand final's loser when the reset is
    // played) stays first.
    sets.push(SetPlan {
        side: Side::GrandFinalReset,
        round: 1,
        position: 0,
        from: [From::Loser(grand_final), From::Winner(grand_final)],
        finals: true,
    });
    sets
}

/// Who is in one of a set's two places.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// Not known yet: the set it comes from isn't decided.
    Pending,
    /// The entrant of this seed.
    Player(usize),
    /// Nobody will come: a bye, or a player who left the tournament.
    Empty,
}

/// How a set ended, or that it hasn't.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// To be played, or waiting for its players.
    Open,
    /// The player in this place (0 or 1) won it: by the result, or without
    /// playing (`walkover`: nobody came against them, or the other left the
    /// tournament).
    Won { slot: usize, walkover: bool },
    /// Nobody plays it: both places are empty, or it's a grand final reset
    /// that isn't needed.
    Void,
}

/// A set worked out from the results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub slots: [Slot; 2],
    pub outcome: Outcome,
}

impl Resolved {
    /// Both players are known and it has no result: it's to be played.
    pub fn playable(&self) -> bool {
        self.outcome == Outcome::Open && matches!(self.slots, [Slot::Player(_), Slot::Player(_)])
    }

    pub fn winner(&self) -> Option<usize> {
        match (self.outcome, self.slots) {
            (Outcome::Won { slot, .. }, slots) => match slots[slot] {
                Slot::Player(player) => Some(player),
                _ => None,
            },
            _ => None,
        }
    }

    /// The loser of a set that was played (none for a walkover).
    pub fn loser(&self) -> Option<usize> {
        match (self.outcome, self.slots) {
            (
                Outcome::Won {
                    slot,
                    walkover: false,
                },
                slots,
            ) => match slots[1 - slot] {
                Slot::Player(player) => Some(player),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Each set's players and outcome, from the plan, the number of entrants,
/// each set's result (the place, 0 or 1, of its winner; only for sets
/// played) and the entrants who left (`dropped`, by seed). A player who left
/// loses the sets still open that they are in, without playing; they go no
/// further, not into the losers' bracket either. Results of sets that can't
/// be played (yet) are ignored.
pub fn resolve(
    plan: &[SetPlan],
    entrants: usize,
    results: &[Option<usize>],
    dropped: &[bool],
) -> Vec<Resolved> {
    let left = |player: usize| dropped.get(player).copied().unwrap_or(false);
    let mut resolved: Vec<Resolved> = Vec::with_capacity(plan.len());
    for (index, set) in plan.iter().enumerate() {
        let slot = |from: From, resolved: &[Resolved]| match from {
            From::Seed(seed) if seed < entrants => Slot::Player(seed),
            From::Seed(_) => Slot::Empty,
            From::Winner(at) => match resolved[at].outcome {
                Outcome::Open => Slot::Pending,
                Outcome::Void => Slot::Empty,
                Outcome::Won { .. } => match resolved[at].winner() {
                    Some(player) => Slot::Player(player),
                    None => Slot::Empty,
                },
            },
            From::Loser(at) => match resolved[at].outcome {
                Outcome::Open => Slot::Pending,
                Outcome::Void => Slot::Empty,
                Outcome::Won { .. } => match resolved[at].loser() {
                    Some(player) if !left(player) => Slot::Player(player),
                    _ => Slot::Empty,
                },
            },
        };
        let slots = [slot(set.from[0], &resolved), slot(set.from[1], &resolved)];
        let outcome = if set.side == Side::GrandFinalReset {
            // Only when the grand final's player from the losers' bracket
            // (its second place) won it.
            let grand_final = match set.from[1] {
                From::Winner(at) => resolved[at],
                _ => unreachable!("a reset follows its grand final"),
            };
            match grand_final.outcome {
                Outcome::Open => Outcome::Open,
                Outcome::Won {
                    slot: 1,
                    walkover: false,
                } => decide(slots, results.get(index), &left),
                _ => Outcome::Void,
            }
        } else {
            decide(slots, results.get(index), &left)
        };
        resolved.push(Resolved { slots, outcome });
    }
    resolved
}

fn decide(
    slots: [Slot; 2],
    result: Option<&Option<usize>>,
    left: &impl Fn(usize) -> bool,
) -> Outcome {
    match slots {
        [Slot::Player(first), Slot::Player(second)] => {
            if let Some(&Some(slot)) = result
                && slot < 2
            {
                return Outcome::Won {
                    slot,
                    walkover: false,
                };
            }
            match (left(first), left(second)) {
                (false, false) => Outcome::Open,
                (true, true) => Outcome::Void,
                (false, true) => Outcome::Won {
                    slot: 0,
                    walkover: true,
                },
                (true, false) => Outcome::Won {
                    slot: 1,
                    walkover: true,
                },
            }
        }
        [Slot::Player(player), Slot::Empty] if !left(player) => Outcome::Won {
            slot: 0,
            walkover: true,
        },
        [Slot::Empty, Slot::Player(player)] if !left(player) => Outcome::Won {
            slot: 1,
            walkover: true,
        },
        [Slot::Player(_) | Slot::Empty, Slot::Player(_) | Slot::Empty] => Outcome::Void,
        _ => Outcome::Open,
    }
}

/// The bracket is over: every set is decided or void.
pub fn finished(resolved: &[Resolved]) -> bool {
    resolved.iter().all(|set| set.outcome != Outcome::Open)
}

/// The place of each entrant (by seed) once the bracket is over, from 1:
/// the champion 1, the last set's loser 2, and so on; players out in the
/// same round share a place (two 3rds in single elimination, for instance).
/// A player who left before playing a set has none. Before the end, `None`
/// for all.
pub fn standings(plan: &[SetPlan], entrants: usize, resolved: &[Resolved]) -> Vec<Option<u32>> {
    let mut places = vec![None; entrants];
    if !finished(resolved) {
        return places;
    }
    // Where each player went out: the index of the round (in plan order) of
    // the last set they lost; the champion lost none that ended it.
    let mut out: Vec<Option<usize>> = vec![None; entrants];
    let mut rounds: Vec<(Side, u32)> = Vec::new();
    let mut played = vec![false; entrants];
    for (set, done) in plan.iter().zip(resolved) {
        let key = (set.side, set.round);
        if !rounds.contains(&key) {
            rounds.push(key);
        }
        let round = rounds.iter().position(|&k| k == key).unwrap();
        let Outcome::Won { slot, walkover } = done.outcome else {
            continue;
        };
        if let Slot::Player(winner) = done.slots[slot] {
            played[winner] |= !walkover;
        }
        if let Slot::Player(loser) = done.slots[1 - slot] {
            out[loser] = Some(round);
            played[loser] |= !walkover;
        }
    }
    // The champion: the winner of the last set decided.
    let last = resolved
        .iter()
        .rposition(|set| matches!(set.outcome, Outcome::Won { .. }));
    let champion = last.and_then(|at| resolved[at].winner());
    // Out later = better: players are ranked by the round they went out in,
    // from the last one back.
    let mut ranked: Vec<(usize, usize)> = (0..entrants)
        .filter(|&player| Some(player) != champion && played[player])
        .filter_map(|player| out[player].map(|round| (round, player)))
        .collect();
    ranked.sort_by_key(|&(round, _)| std::cmp::Reverse(round));
    if let Some(champion) = champion {
        places[champion] = Some(1);
    }
    let mut place = 2;
    let mut at = 0;
    while at < ranked.len() {
        let round = ranked[at].0;
        let same: Vec<usize> = ranked[at..]
            .iter()
            .take_while(|(r, _)| *r == round)
            .map(|&(_, player)| player)
            .collect();
        for &player in &same {
            places[player] = Some(place);
        }
        place += same.len() as u32;
        at += same.len();
    }
    places
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays every set the way `pick` says (the place of the winner),
    /// nobody leaving, until the bracket is over.
    fn play(
        format: Format,
        entrants: usize,
        pick: impl Fn(usize, usize) -> usize,
    ) -> (Vec<SetPlan>, Vec<Resolved>) {
        let sets = plan(format, entrants);
        let mut results = vec![None; sets.len()];
        let dropped = vec![false; entrants];
        loop {
            let resolved = resolve(&sets, entrants, &results, &dropped);
            let Some(next) = resolved.iter().position(Resolved::playable) else {
                return (sets, resolved);
            };
            let [Slot::Player(a), Slot::Player(b)] = resolved[next].slots else {
                unreachable!()
            };
            results[next] = Some(pick(a, b));
        }
    }

    /// The better seed (the lower) always wins.
    fn seeds_win(a: usize, b: usize) -> usize {
        if a < b { 0 } else { 1 }
    }

    #[test]
    fn seeds_meet_in_the_usual_order() {
        assert_eq!(seed_order(2), [0, 1]);
        assert_eq!(seed_order(4), [0, 3, 1, 2]);
        assert_eq!(seed_order(8), [0, 7, 3, 4, 1, 6, 2, 5]);
    }

    #[test]
    fn sets_come_after_the_ones_they_take_players_from() {
        for format in [Format::Single, Format::Double] {
            for entrants in 2..=64 {
                let sets = plan(format, entrants);
                for (index, set) in sets.iter().enumerate() {
                    for from in set.from {
                        if let From::Winner(at) | From::Loser(at) = from {
                            assert!(at < index, "{format:?} {entrants}: set {index} from {at}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_single_bracket_has_one_set_less_than_its_size() {
        for entrants in 2..=64 {
            let sets = plan(Format::Single, entrants);
            assert_eq!(sets.len(), entrants.next_power_of_two() - 1);
            assert_eq!(sets.iter().filter(|set| set.finals).count(), 1);
        }
    }

    #[test]
    fn everybody_plays_and_the_top_seed_wins() {
        for format in [Format::Single, Format::Double] {
            for entrants in 2..=64 {
                let (sets, resolved) = play(format, entrants, seeds_win);
                assert!(finished(&resolved), "{format:?} {entrants}");
                let places = standings(&sets, entrants, &resolved);
                assert_eq!(places[0], Some(1), "{format:?} {entrants}");
                assert_eq!(places[1], Some(2), "{format:?} {entrants}");
                assert!(
                    places.iter().all(Option::is_some),
                    "{format:?} {entrants}: {places:?}"
                );
                // Each player loses once (single) or twice (double), but the
                // champion; a bye is no set.
                let mut losses = vec![0; entrants];
                for set in &resolved {
                    if let Some(loser) = set.loser() {
                        losses[loser] += 1;
                    }
                }
                let most = if format == Format::Single { 1 } else { 2 };
                assert_eq!(losses[0], 0);
                assert!(
                    losses[1..].iter().all(|&n| n == most),
                    "{format:?} {entrants}: {losses:?}"
                );
            }
        }
    }

    #[test]
    fn byes_go_to_the_top_seeds() {
        let sets = plan(Format::Single, 5);
        let resolved = resolve(&sets, 5, &vec![None; sets.len()], &[false; 5]);
        // 1, 2 and 3 have a bye; 4 plays 5, and 2 plays 3 next.
        let walkovers: Vec<usize> = resolved
            .iter()
            .filter(|set| matches!(set.outcome, Outcome::Won { walkover: true, .. }))
            .filter_map(Resolved::winner)
            .collect();
        assert_eq!(walkovers, [0, 1, 2]);
        let playable: Vec<[Slot; 2]> = resolved
            .iter()
            .filter(|set| set.playable())
            .map(|set| set.slots)
            .collect();
        assert_eq!(
            playable,
            [
                [Slot::Player(3), Slot::Player(4)],
                [Slot::Player(1), Slot::Player(2)]
            ]
        );
    }

    #[test]
    fn single_elimination_places() {
        let (sets, resolved) = play(Format::Single, 8, seeds_win);
        let places = standings(&sets, 8, &resolved);
        assert_eq!(
            places,
            [
                Some(1),
                Some(2),
                Some(3),
                Some(3),
                Some(5),
                Some(5),
                Some(5),
                Some(5)
            ]
        );
    }

    #[test]
    fn double_elimination_places() {
        let (sets, resolved) = play(Format::Double, 8, seeds_win);
        let places = standings(&sets, 8, &resolved);
        assert_eq!(
            places,
            [
                Some(1),
                Some(2),
                Some(3),
                Some(4),
                Some(5),
                Some(5),
                Some(7),
                Some(7)
            ]
        );
    }

    #[test]
    fn the_reset_is_played_only_when_the_losers_side_wins() {
        // Two players: the first set, then the grand final.
        let sets = plan(Format::Double, 2);
        assert_eq!(sets.len(), 3);
        let resolve_with = |results: &[Option<usize>]| resolve(&sets, 2, results, &[false, false]);
        // Seed 1 wins the first set and the grand final: no reset.
        let done = resolve_with(&[Some(0), Some(0), None]);
        assert_eq!(done[2].outcome, Outcome::Void);
        assert!(finished(&done));
        // Seed 2 wins the grand final: the reset, seed 1 first.
        let reset = resolve_with(&[Some(0), Some(1), None]);
        assert!(reset[2].playable());
        assert_eq!(reset[2].slots, [Slot::Player(0), Slot::Player(1)]);
        let done = resolve_with(&[Some(0), Some(1), Some(1)]);
        assert_eq!(standings(&sets, 2, &done), [Some(2), Some(1)]);
    }

    #[test]
    fn upsets_are_followed() {
        // The worse seed always wins: the last seed wins the winners'
        // bracket and the grand final.
        let (sets, resolved) = play(Format::Double, 4, |a, b| if a > b { 0 } else { 1 });
        let places = standings(&sets, 4, &resolved);
        assert_eq!(places[3], Some(1));
        assert!(finished(&resolved));
    }

    #[test]
    fn a_player_who_leaves_loses_their_open_sets() {
        let sets = plan(Format::Double, 4);
        let none = vec![None; sets.len()];
        // Seed 4 leaves before playing: seed 1 goes on without playing, and
        // nobody drops from that set into the losers' bracket.
        let resolved = resolve(&sets, 4, &none, &[false, false, false, true]);
        assert_eq!(
            resolved[0].outcome,
            Outcome::Won {
                slot: 0,
                walkover: true
            }
        );
        assert_eq!(resolved[0].loser(), None);
        // The losers' first set waits for the other first-round loser, then
        // gives them a walkover.
        let losers_first = sets
            .iter()
            .position(|set| set.side == Side::Losers)
            .unwrap();
        assert_eq!(resolved[losers_first].slots, [Slot::Empty, Slot::Pending]);
        let mut results = none.clone();
        results[1] = Some(0);
        let resolved = resolve(&sets, 4, &results, &[false, false, false, true]);
        assert_eq!(
            resolved[losers_first].outcome,
            Outcome::Won {
                slot: 1,
                walkover: true
            }
        );
        // A result already in stays when its winner leaves later; their next
        // set is lost.
        let results = [Some(0), Some(0), None, None, None, None, None];
        let resolved = resolve(&sets, 4, &results, &[true, false, false, false]);
        assert_eq!(resolved[0].winner(), Some(0));
        assert_eq!(resolved[2].slots, [Slot::Player(0), Slot::Player(1)]);
        assert_eq!(
            resolved[2].outcome,
            Outcome::Won {
                slot: 1,
                walkover: true
            }
        );
        // Its loser still drops into the losers' bracket.
        assert!(resolved[losers_first].playable());
    }

    #[test]
    fn a_result_for_a_set_not_ready_is_ignored() {
        let sets = plan(Format::Single, 4);
        let resolved = resolve(&sets, 4, &[None, None, Some(0)], &[false; 4]);
        assert_eq!(resolved[2].outcome, Outcome::Open);
        assert_eq!(resolved[2].slots, [Slot::Pending, Slot::Pending]);
    }

    #[test]
    fn losers_rounds_cross_the_players_dropping_in() {
        // 8 entrants: the winners' semifinal losers drop in reversed, so
        // the loser of the top semifinal meets the bottom half's losers.
        let sets = plan(Format::Double, 8);
        let semis: Vec<usize> = (0..sets.len())
            .filter(|&i| sets[i].side == Side::Winners && sets[i].round == 2)
            .collect();
        let drop_in: Vec<From> = sets
            .iter()
            .filter(|set| set.side == Side::Losers && set.round == 2)
            .map(|set| set.from[1])
            .collect();
        assert_eq!(drop_in, [From::Loser(semis[1]), From::Loser(semis[0])]);
        assert_eq!(sets.iter().filter(|set| set.finals).count(), 4);
    }
}

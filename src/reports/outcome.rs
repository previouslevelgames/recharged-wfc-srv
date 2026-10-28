//! How a match stands, from its sides' reports (docs/reports.md, "States"):
//! a game both sides saw the same counts, one they saw differently is
//! disputed, and one only a side reported counts as that side saw it. The
//! match is in progress until its sides ended, then finished or disputed,
//! or abandoned.

use std::{cmp::Ordering, collections::BTreeMap};

use serde::Serialize;
use time::OffsetDateTime;

use super::report::{Side, SyncError, Teams, UNKNOWN};

/// A game as one side saw it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct View {
    pub score: Teams<i32>,
    pub captains: Teams<String>,
    pub stadium: String,
    pub ending: String,
    pub seconds: i32,
    /// For a `desync`, what the game knew of it.
    pub sync: Option<SyncError>,
}

impl View {
    /// Whether two views tell the same game: the score, the captains and the
    /// stadium. An `unknown` name is no claim, so it agrees with any.
    fn agrees(&self, other: &Self) -> bool {
        let same = |a: &str, b: &str| a == b || a == UNKNOWN || b == UNKNOWN;
        self.score == other.score
            && same(&self.captains.home, &other.captains.home)
            && same(&self.captains.away, &other.captains.away)
            && same(&self.stadium, &other.stadium)
    }

    /// This view, with the names it does not know taken from `other`.
    fn merged(&self, other: &Self) -> Self {
        let known =
            |mine: &String, theirs: &String| if mine == UNKNOWN { theirs } else { mine }.clone();
        Self {
            captains: Teams {
                home: known(&self.captains.home, &other.captains.home),
                away: known(&self.captains.away, &other.captains.away),
            },
            stadium: known(&self.stadium, &other.stadium),
            ..self.clone()
        }
    }

    /// Who won the game; none for a draw.
    pub fn winner(&self) -> Option<Side> {
        match self.score.home.cmp(&self.score.away) {
            Ordering::Greater => Some(Side::Home),
            Ordering::Less => Some(Side::Away),
            Ordering::Equal => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Both sides saw it the same.
    Agreed,
    /// They saw it differently: it does not count.
    Disputed,
    /// Only one side reported it.
    OnlyHome,
    OnlyAway,
}

/// A game of the match, with each side's view.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Compared {
    pub number: i32,
    pub status: Status,
    pub home: Option<View>,
    pub away: Option<View>,
}

impl Compared {
    /// The game as it counts: what both saw, or what the one side saw. None
    /// when disputed.
    pub fn counted(&self) -> Option<View> {
        match (&self.home, &self.away) {
            (Some(home), Some(away)) if self.status == Status::Agreed => Some(home.merged(away)),
            (Some(view), None) | (None, Some(view)) => Some(view.clone()),
            _ => None,
        }
    }
}

/// The games both sides reported, by number.
pub fn compare(home: &BTreeMap<i32, View>, away: &BTreeMap<i32, View>) -> Vec<Compared> {
    let mut numbers: Vec<i32> = home.keys().chain(away.keys()).copied().collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers
        .into_iter()
        .map(|number| {
            let (home, away) = (home.get(&number).cloned(), away.get(&number).cloned());
            let status = match (&home, &away) {
                (Some(home), Some(away)) if home.agrees(away) => Status::Agreed,
                (Some(_), Some(_)) => Status::Disputed,
                (Some(_), None) => Status::OnlyHome,
                _ => Status::OnlyAway,
            };
            Compared {
                number,
                status,
                home,
                away,
            }
        })
        .collect()
}

/// Games won by each side, of the games that count.
pub fn wins(games: &[Compared]) -> Teams<i32> {
    let mut wins = Teams::default();
    for winner in games
        .iter()
        .filter_map(Compared::counted)
        .filter_map(|game| game.winner())
    {
        *wins.get_mut(winner) += 1;
    }
    wins
}

/// A side of a match as its game reported it.
#[derive(Clone, Debug, PartialEq)]
pub struct Reporter {
    pub profile_id: i32,
    /// The series' start time in Unix seconds, by its game's clock.
    pub series: i64,
    pub mii: String,
    pub opponent_mii: String,
    /// As its start said them; captains may be `unknown`.
    pub captains: Teams<String>,
    pub sidekicks: Teams<Vec<String>>,
    /// The end's: `decided` or `abandoned`.
    pub reason: Option<String>,
    /// Why the series ended, as the end said: `decided`, `desync`,
    /// `disconnect` or `offline`.
    pub cause: Option<String>,
    /// The build that reported, when it said.
    pub build: Option<String>,
    /// When its game's GPCM login ended before the end came, and how.
    pub left_at: Option<OffsetDateTime>,
    pub left_how: Option<String>,
    pub opened_at: OffsetDateTime,
    pub last_report_at: OffsetDateTime,
    pub ended_at: Option<OffsetDateTime>,
    pub games: BTreeMap<i32, View>,
}

/// What the match's row keeps.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// `in_progress`, `finished`, `disputed` or `abandoned`.
    pub state: &'static str,
    /// Once finished: the side that won the series; none for a draw.
    pub winner: Option<Side>,
    /// Games won, of the games that count.
    pub wins: Teams<i32>,
    pub miis: Teams<String>,
    pub captains: Teams<String>,
}

/// The match of `best_of` games with these sides; `idle` once nothing came
/// for 30 minutes. A side nobody reported is `None`.
///
/// A side that won more than half the games has won the series, ends or
/// not. While both sides report, only the games both saw the same decide
/// it; a game one side alone reported counts once the other's report can
/// no longer come: when every side ended or nothing came for 30 minutes. A
/// game they saw differently disputes the match.
pub fn settle(sides: &Teams<Option<Reporter>>, best_of: i32, idle: bool) -> Outcome {
    let reporters: Vec<&Reporter> = Side::BOTH
        .iter()
        .filter_map(|&side| sides.get(side).as_ref())
        .collect();
    let empty = BTreeMap::new();
    let games = compare(
        sides.home.as_ref().map_or(&empty, |home| &home.games),
        sides.away.as_ref().map_or(&empty, |away| &away.games),
    );
    let counted = wins(&games);
    let ended = reporters.iter().all(|reporter| reporter.reason.is_some());
    let abandoned = reporters
        .iter()
        .any(|reporter| reporter.reason.as_deref() == Some("abandoned"));
    let disputed = games.iter().any(|game| game.status == Status::Disputed);

    let deciding = if reporters.len() < 2 || ended || idle {
        counted
    } else {
        let agreed: Vec<Compared> = games
            .iter()
            .filter(|game| game.status == Status::Agreed)
            .cloned()
            .collect();
        wins(&agreed)
    };
    let series_won = Side::BOTH
        .into_iter()
        .find(|&side| 2 * deciding.get(side) > best_of);
    let more_games = match counted.home.cmp(&counted.away) {
        Ordering::Greater => Some(Side::Home),
        Ordering::Less => Some(Side::Away),
        Ordering::Equal => None,
    };
    let (state, winner) = match series_won {
        Some(_) if disputed => ("disputed", None),
        Some(side) => ("finished", Some(side)),
        None if abandoned || (idle && !ended) => ("abandoned", None),
        None if ended && disputed => ("disputed", None),
        // Every side ended: the side that won more games, or a draw.
        None if ended => ("finished", more_games),
        None => ("in_progress", None),
    };

    // A team's Mii and captain as its own game named them, else as the
    // other game showed them.
    let other_view = |side: Side| sides.get(side.other()).as_ref();
    let teams = Teams {
        home: Side::Home,
        away: Side::Away,
    };
    let miis = teams.map(|&side| match (sides.get(side), other_view(side)) {
        (Some(own), _) => own.mii.clone(),
        (None, Some(other)) => other.opponent_mii.clone(),
        (None, None) => String::new(),
    });
    let captains = teams.map(|&side| {
        [sides.get(side).as_ref(), other_view(side)]
            .into_iter()
            .flatten()
            .map(|reporter| reporter.captains.get(side).as_str())
            .find(|captain| *captain != UNKNOWN)
            .unwrap_or(UNKNOWN)
            .to_owned()
    });

    Outcome {
        state,
        winner,
        wins: counted,
        miis,
        captains,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(home: i32, away: i32) -> View {
        View {
            score: Teams { home, away },
            captains: Teams {
                home: "mario".into(),
                away: "bowser".into(),
            },
            stadium: "the_palace".into(),
            ending: "normal".into(),
            seconds: 180,
            sync: None,
        }
    }

    fn reporter(mii: &str, opponent: &str, games: &[View]) -> Reporter {
        let now = OffsetDateTime::now_utc();
        Reporter {
            profile_id: 1_000_000_000,
            series: 1,
            mii: mii.into(),
            opponent_mii: opponent.into(),
            captains: Teams {
                home: "mario".into(),
                away: "bowser".into(),
            },
            sidekicks: Teams::default(),
            reason: None,
            cause: None,
            build: None,
            left_at: None,
            left_how: None,
            opened_at: now,
            last_report_at: now,
            ended_at: None,
            games: (1..).zip(games.iter().cloned()).collect(),
        }
    }

    fn ended(mut reporter: Reporter, reason: &str) -> Reporter {
        reporter.reason = Some(reason.into());
        reporter
    }

    #[test]
    fn games_agree_on_score_captains_and_stadium_only() {
        let mut other_clock = view(3, 1);
        other_clock.seconds = 181;
        other_clock.ending = "disconnect".into();
        assert!(view(3, 1).agrees(&other_clock));
        assert!(!view(3, 1).agrees(&view(1, 3)));

        let mut elsewhere = view(3, 1);
        elsewhere.stadium = "lava_pit".into();
        assert!(!view(3, 1).agrees(&elsewhere));
        let mut peach = view(3, 1);
        peach.captains.home = "peach".into();
        assert!(!view(3, 1).agrees(&peach));

        // An unknown name agrees with any, and the known one counts.
        let mut unsure = view(3, 1);
        unsure.stadium = UNKNOWN.into();
        unsure.captains.away = UNKNOWN.into();
        assert!(unsure.agrees(&elsewhere));
        let games = compare(
            &BTreeMap::from([(1, unsure)]),
            &BTreeMap::from([(1, elsewhere)]),
        );
        assert_eq!(games[0].status, Status::Agreed);
        let counted = games[0].counted().unwrap();
        assert_eq!(
            (counted.stadium.as_str(), counted.captains.away.as_str()),
            ("lava_pit", "bowser")
        );
    }

    #[test]
    fn each_game_is_agreed_disputed_or_one_sided() {
        let home = BTreeMap::from([(1, view(3, 1)), (2, view(0, 2)), (3, view(2, 1))]);
        let away = BTreeMap::from([(1, view(3, 1)), (2, view(2, 0)), (4, view(1, 0))]);
        let games = compare(&home, &away);
        let statuses: Vec<_> = games
            .iter()
            .map(|game| (game.number, game.status))
            .collect();
        assert_eq!(
            statuses,
            [
                (1, Status::Agreed),
                (2, Status::Disputed),
                (3, Status::OnlyHome),
                (4, Status::OnlyAway),
            ]
        );
        assert!(games[1].counted().is_none());
        // Games 1, 3 and 4 count: home won 1 and 3, home won 4 as away saw it.
        assert_eq!(wins(&games), Teams { home: 3, away: 0 });
    }

    fn both(home: Reporter, away: Reporter) -> Teams<Option<Reporter>> {
        Teams {
            home: Some(home),
            away: Some(away),
        }
    }

    #[test]
    fn the_games_both_saw_decide_the_series_ends_or_not() {
        let anosch = reporter("Anosch", "Sam", &[view(3, 1)]);
        let sam = reporter("Sam", "Anosch", &[view(3, 1)]);
        assert_eq!(
            settle(&both(anosch.clone(), sam.clone()), 3, false).state,
            "in_progress"
        );
        // Best of 1: one game decides it.
        let outcome = settle(&both(anosch.clone(), sam.clone()), 1, false);
        assert_eq!(
            (outcome.state, outcome.winner),
            ("finished", Some(Side::Home))
        );

        // Game 2 as Anosch saw it, before Sam's report of it: not yet.
        let mut anosch = reporter("Anosch", "Sam", &[view(3, 1), view(2, 0)]);
        assert_eq!(
            settle(&both(anosch.clone(), sam.clone()), 3, false).state,
            "in_progress"
        );
        let sam = reporter("Sam", "Anosch", &[view(3, 1), view(2, 0)]);
        let outcome = settle(&both(anosch.clone(), sam.clone()), 3, false);
        assert_eq!(
            (outcome.state, outcome.winner),
            ("finished", Some(Side::Home))
        );
        assert_eq!(outcome.wins, Teams { home: 2, away: 0 });
        assert_eq!(
            outcome.miis,
            Teams {
                home: "Anosch".to_owned(),
                away: "Sam".to_owned()
            }
        );

        // An end that says otherwise comes too late to change it.
        anosch.reason = Some("abandoned".into());
        assert_eq!(settle(&both(anosch, sam), 3, false).state, "finished");
    }

    #[test]
    fn a_game_one_side_alone_reported_counts_once_nothing_more_can_come() {
        // Sam's report of game 2 never came.
        let anosch = reporter("Anosch", "Sam", &[view(3, 1), view(2, 0)]);
        let sam = reporter("Sam", "Anosch", &[view(3, 1)]);
        assert_eq!(
            settle(&both(anosch.clone(), sam.clone()), 3, false).state,
            "in_progress"
        );
        let outcome = settle(&both(anosch.clone(), sam.clone()), 3, true);
        assert_eq!(
            (outcome.state, outcome.winner),
            ("finished", Some(Side::Home))
        );
        let outcome = settle(
            &both(ended(anosch, "decided"), ended(sam, "decided")),
            3,
            false,
        );
        assert_eq!(
            (outcome.state, outcome.winner),
            ("finished", Some(Side::Home))
        );
    }

    #[test]
    fn ended_sides_finish_a_series_nobody_won_outright() {
        // Best of 3 with a drawn game: 1 to 1 after three games.
        let games = [view(1, 0), view(0, 1), view(2, 2)];
        let anosch = ended(reporter("Anosch", "Sam", &games), "decided");
        let sam = reporter("Sam", "Anosch", &games);
        assert_eq!(
            settle(&both(anosch.clone(), sam.clone()), 3, false).state,
            "in_progress"
        );
        let outcome = settle(&both(anosch, ended(sam, "decided")), 3, false);
        assert_eq!((outcome.state, outcome.winner), ("finished", None));
        assert_eq!(outcome.wins, Teams { home: 1, away: 1 });
    }

    #[test]
    fn sides_that_saw_other_games_dispute_the_match() {
        let anosch = reporter("Anosch", "Sam", &[view(3, 1), view(2, 0), view(1, 0)]);
        let sam = reporter("Sam", "Anosch", &[view(3, 1), view(0, 2), view(1, 0)]);
        // Two games both saw the same decide the series, but one is disputed.
        let outcome = settle(&both(anosch.clone(), sam.clone()), 3, false);
        assert_eq!((outcome.state, outcome.winner), ("disputed", None));

        let anosch = reporter("Anosch", "Sam", &[view(3, 1), view(2, 0)]);
        let sam = reporter("Sam", "Anosch", &[view(3, 1), view(0, 2)]);
        assert_eq!(
            settle(&both(anosch.clone(), sam.clone()), 3, false).state,
            "in_progress"
        );
        let outcome = settle(
            &both(ended(anosch, "decided"), ended(sam, "decided")),
            3,
            false,
        );
        assert_eq!((outcome.state, outcome.winner), ("disputed", None));
    }

    #[test]
    fn an_undecided_match_is_abandoned() {
        // The session was left after the first kickoff: no game.
        let anosch = ended(reporter("Anosch", "Sam", &[]), "abandoned");
        let sam = reporter("Sam", "Anosch", &[]);
        let outcome = settle(&both(anosch, sam.clone()), 3, false);
        assert_eq!((outcome.state, outcome.winner), ("abandoned", None));

        // Nothing came for 30 minutes after one game.
        let anosch = reporter("Anosch", "Sam", &[view(1, 0)]);
        let sam = reporter("Sam", "Anosch", &[view(1, 0)]);
        assert_eq!(settle(&both(anosch, sam), 3, true).state, "abandoned");
    }

    #[test]
    fn one_reporter_decides_the_match_with_its_view() {
        // The home game is a retail build: Anosch is known by name only.
        let mut sam = reporter("Sam", "Anosch", &[view(1, 2)]);
        sam.captains.home = UNKNOWN.into();
        let alone = |sam: &Reporter| Teams {
            home: None,
            away: Some(sam.clone()),
        };
        assert_eq!(settle(&alone(&sam), 3, false).state, "in_progress");
        sam.games.insert(2, view(0, 3));
        let outcome = settle(&alone(&sam), 3, false);
        assert_eq!(
            (outcome.state, outcome.winner),
            ("finished", Some(Side::Away))
        );
        assert_eq!(outcome.wins, Teams { home: 0, away: 2 });
        assert_eq!(
            outcome.miis,
            Teams {
                home: "Anosch".to_owned(),
                away: "Sam".to_owned()
            }
        );
        assert_eq!(outcome.captains.home, UNKNOWN);
    }

    #[test]
    fn a_known_captain_wins_over_an_unknown_one() {
        let mut anosch = reporter("Anosch", "Sam", &[]);
        anosch.captains.away = UNKNOWN.into();
        let mut sam = reporter("Sam", "Anosch", &[]);
        sam.captains = Teams {
            home: "luigi".into(),
            away: "wario".into(),
        };
        let outcome = settle(
            &Teams {
                home: Some(anosch),
                away: Some(sam),
            },
            3,
            false,
        );
        // Each team's own game first.
        assert_eq!(
            outcome.captains,
            Teams {
                home: "mario".to_owned(),
                away: "wario".to_owned()
            }
        );
    }
}

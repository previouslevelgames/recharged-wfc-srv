//! A record in the reported matches (docs/reports.md, "Games and states"):
//! the series played from a side, and the games that count with their goals.
//! A profile's page on the admin dashboard and a Recharged player's stats
//! (docs/profiles.md, "Stats") count the same way.

use std::{cmp::Ordering, collections::BTreeMap};

use serde::Serialize;

use super::{
    outcome::{self, Compared, Reporter, View},
    report::{Side, Teams},
};

/// The modes of a match, as reports name them.
pub const MODES: [&str; 2] = ["ranked", "friends"];

/// The series of a record. `played` counts them all, also those without a
/// result yet (or ever).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SeriesRecord {
    pub played: u32,
    pub won: u32,
    pub lost: u32,
    pub drawn: u32,
    pub disputed: u32,
    pub abandoned: u32,
    pub in_progress: u32,
}

/// The games that count (docs/reports.md, "Games and states").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GamesRecord {
    pub played: u32,
    pub won: u32,
    pub lost: u32,
    pub drawn: u32,
    pub goals_for: u32,
    pub goals_against: u32,
}

/// The series and their games.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Record {
    pub series: SeriesRecord,
    pub games: GamesRecord,
}

/// A series as a side played it: the match's `state` and `winner` (as
/// `online_matches` keeps them) and the side.
pub struct Series<'a> {
    pub state: &'a str,
    /// Once finished: the side that won; none for a draw.
    pub winner: Option<Side>,
    pub side: Side,
}

impl Series<'_> {
    /// Once finished: `won`, `lost` or `drawn`.
    pub fn result(&self) -> Option<&'static str> {
        (self.state == "finished").then(|| match self.winner {
            Some(winner) if winner == self.side => "won",
            Some(_) => "lost",
            None => "drawn",
        })
    }
}

/// The games of a match that count, from what its sides reported
/// (`crate::reports::sides`; none when nobody reported).
pub fn counted(sides: Option<&Teams<Option<Reporter>>>) -> Vec<View> {
    let empty = BTreeMap::new();
    let views = |side: Side| {
        sides
            .and_then(|sides| sides.get(side).as_ref())
            .map_or(&empty, |reporter| &reporter.games)
    };
    outcome::compare(views(Side::Home), views(Side::Away))
        .iter()
        .filter_map(Compared::counted)
        .collect()
}

impl SeriesRecord {
    pub fn add(&mut self, series: &Series<'_>) {
        self.played += 1;
        match (series.state, series.result()) {
            (_, Some("won")) => self.won += 1,
            (_, Some("lost")) => self.lost += 1,
            (_, Some(_)) => self.drawn += 1,
            ("disputed", _) => self.disputed += 1,
            ("abandoned", _) => self.abandoned += 1,
            _ => self.in_progress += 1,
        }
    }
}

impl GamesRecord {
    /// Adds a game that counts, as `side` played it.
    pub fn add(&mut self, game: &View, side: Side) {
        let (mine, theirs) = (*game.score.get(side), *game.score.get(side.other()));
        self.played += 1;
        self.goals_for += mine as u32;
        self.goals_against += theirs as u32;
        match mine.cmp(&theirs) {
            Ordering::Greater => self.won += 1,
            Ordering::Less => self.lost += 1,
            Ordering::Equal => self.drawn += 1,
        }
    }
}

impl Record {
    /// Adds `series` with its games that count ([`counted`]).
    pub fn add(&mut self, series: &Series<'_>, games: &[View]) {
        self.series.add(series);
        for game in games {
            self.games.add(game, series.side);
        }
    }
}

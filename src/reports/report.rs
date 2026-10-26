//! A match report as a Recharged build sends it (docs/reports.md, "Body"),
//! read and checked field by field.

use serde::{Deserialize, Serialize};

use crate::profiles::rules::{SERIES, STADIUMS};

/// The releases' game codes.
pub const RELEASES: [&str; 4] = ["R4QE", "R4QP", "R4QJ", "R4QK"];
/// The captains, in the game's order (`eTeamID`).
pub const CAPTAINS: [&str; 12] = [
    "mario",
    "bowser",
    "daisy",
    "donkey_kong",
    "luigi",
    "peach",
    "waluigi",
    "wario",
    "yoshi",
    "bowser_jr",
    "diddy_kong",
    "petey",
];
/// The sidekicks, in the game's order (`eSidekickID`).
pub const SIDEKICKS: [&str; 8] = [
    "toad",
    "koopa",
    "hammer_bro",
    "birdo",
    "boo",
    "dry_bones",
    "monty_mole",
    "shy_guy",
];
/// A captain or a stadium the game does not know.
pub const UNKNOWN: &str = "unknown";
/// A team's sidekicks, when the game knows them all.
const TEAM_SIDEKICKS: usize = 3;
/// A Mii name's characters at most.
const MII_NAME: usize = 10;
/// Goals in a game at most, as in the profiles' match reports.
const MAX_SCORE: i64 = 99;
/// A game's length at most, in seconds: longer is no game's clock.
const MAX_SECONDS: i64 = 3600;

/// A team, by where it plays: the host's is home.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Home,
    Away,
}

impl Side {
    pub const BOTH: [Self; 2] = [Self::Home, Self::Away];

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "home" => Some(Self::Home),
            "away" => Some(Self::Away),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Away => "away",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Home => Self::Away,
            Self::Away => Self::Home,
        }
    }
}

/// One of each for the two teams.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Teams<T> {
    pub home: T,
    pub away: T,
}

impl<T> Teams<T> {
    pub fn get(&self, side: Side) -> &T {
        match side {
            Side::Home => &self.home,
            Side::Away => &self.away,
        }
    }

    pub fn get_mut(&mut self, side: Side) -> &mut T {
        match side {
            Side::Home => &mut self.home,
            Side::Away => &mut self.away,
        }
    }

    pub fn map<U>(&self, mut f: impl FnMut(&T) -> U) -> Teams<U> {
        Teams {
            home: f(&self.home),
            away: f(&self.away),
        }
    }
}

/// A report, checked.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub profile_id: i32,
    pub release: &'static str,
    /// The reporter's number for the series: its start time in Unix
    /// seconds, by the game's clock. A later series has a larger number.
    pub series: i64,
    pub mode: &'static str,
    pub best_of: i32,
    /// The reporter's team.
    pub side: Side,
    pub mii: String,
    pub opponent_mii: String,
    /// Captains may be [`UNKNOWN`].
    pub captains: Teams<&'static str>,
    /// Up to three a team; fewer, or none, when the game does not know them.
    pub sidekicks: Teams<Vec<&'static str>>,
    pub event: Event,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The series begins: its first kickoff.
    Start,
    /// A game ended.
    Game(Game),
    /// The series is over: `decided` (a side won it) or `abandoned` (it
    /// stopped before).
    End(&'static str),
}

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Game(_) => "game",
            Self::End(_) => "end",
        }
    }

    /// A game's number.
    pub fn game(&self) -> Option<i32> {
        match self {
            Self::Game(game) => Some(game.number),
            _ => None,
        }
    }
}

/// A game as the reporter's game saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Game {
    /// From 1.
    pub number: i32,
    pub score: Teams<i32>,
    /// Captains may be [`UNKNOWN`].
    pub captains: Teams<&'static str>,
    /// One of [`STADIUMS`], or [`UNKNOWN`].
    pub stadium: &'static str,
    /// `normal`, `disconnect` (the score is what this side saw) or `desync`.
    pub ending: &'static str,
    /// The game's length on its clock.
    pub seconds: i32,
}

impl Report {
    /// The report in `body`, or what is wrong with it.
    pub fn parse(body: &[u8]) -> Result<Self, String> {
        let raw: RawReport = serde_json::from_slice(body).map_err(|error| error.to_string())?;
        raw.check()
    }
}

/// A report as JSON has it. Fields the contract does not know are ignored.
#[derive(Deserialize)]
struct RawReport {
    profile_id: i64,
    release: String,
    event: String,
    series: i64,
    mode: String,
    best_of: i64,
    side: String,
    mii: String,
    opponent_mii: String,
    captains: Teams<String>,
    #[serde(default)]
    sidekicks: Option<RawSidekicks>,
    #[serde(default)]
    game: Option<RawGame>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct RawSidekicks {
    #[serde(default)]
    home: Option<Vec<String>>,
    #[serde(default)]
    away: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RawGame {
    number: i64,
    score: Teams<i64>,
    captains: Teams<String>,
    stadium: String,
    ending: String,
    seconds: i64,
}

impl RawReport {
    fn check(self) -> Result<Report, String> {
        let profile_id = i32::try_from(self.profile_id)
            .ok()
            .filter(|&id| id > 0)
            .ok_or("profile_id: a WFC profile")?;
        let release = one_of("release", &self.release, &RELEASES)?;
        let series = Some(self.series)
            .filter(|&series| series >= 0)
            .ok_or("series: the series' start time, in Unix seconds")?;
        let mode = one_of("mode", &self.mode, &["ranked", "friends"])?;
        let best_of = SERIES
            .into_iter()
            .map(i32::from)
            .find(|&games| i64::from(games) == self.best_of)
            .ok_or("best_of: 1, 3, 5 or 7")?;
        let side = Side::parse(&self.side).ok_or("side: home or away")?;
        let mii = mii_name("mii", self.mii)?;
        let opponent_mii = mii_name("opponent_mii", self.opponent_mii)?;
        let captains = captains("captains", &self.captains)?;
        let sidekicks = self.sidekicks.unwrap_or_default();
        let sidekicks = Teams {
            home: sidekicks_of("sidekicks.home", sidekicks.home.unwrap_or_default())?,
            away: sidekicks_of("sidekicks.away", sidekicks.away.unwrap_or_default())?,
        };

        let event = match self.event.as_str() {
            "start" => Event::Start,
            "game" => {
                let game = self
                    .game
                    .as_ref()
                    .ok_or("game: a game event needs its game")?;
                Event::Game(game.check(best_of)?)
            }
            "end" => match self.reason.as_deref() {
                Some("decided") => Event::End("decided"),
                Some("abandoned") => Event::End("abandoned"),
                _ => return Err("reason: decided or abandoned".into()),
            },
            _ => return Err("event: start, game or end".into()),
        };
        if !matches!(event, Event::Game(_)) && self.game.is_some() {
            return Err("game: only for a game event".into());
        }
        if !matches!(event, Event::End(_)) && self.reason.is_some() {
            return Err("reason: only for an end event".into());
        }

        Ok(Report {
            profile_id,
            release,
            series,
            mode,
            best_of,
            side,
            mii,
            opponent_mii,
            captains,
            sidekicks,
            event,
        })
    }
}

impl RawGame {
    fn check(&self, best_of: i32) -> Result<Game, String> {
        let number = i32::try_from(self.number)
            .ok()
            .filter(|number| (1..=best_of).contains(number))
            .ok_or_else(|| format!("game.number: 1 to {best_of}"))?;
        let score = |goals: i64| {
            (0..=MAX_SCORE)
                .contains(&goals)
                .then_some(goals as i32)
                .ok_or_else(|| format!("game.score: 0 to {MAX_SCORE} goals"))
        };
        let score = Teams {
            home: score(self.score.home)?,
            away: score(self.score.away)?,
        };
        let captains = captains("game.captains", &self.captains)?;
        let stadium = if self.stadium == UNKNOWN {
            UNKNOWN
        } else {
            STADIUMS
                .into_iter()
                .find(|stadium| *stadium == self.stadium)
                .ok_or_else(|| format!("game.stadium: unknown or one of {}", STADIUMS.join(", ")))?
        };
        let ending = one_of(
            "game.ending",
            &self.ending,
            &["normal", "disconnect", "desync"],
        )?;
        let seconds = (0..=MAX_SECONDS)
            .contains(&self.seconds)
            .then_some(self.seconds as i32)
            .ok_or_else(|| format!("game.seconds: 0 to {MAX_SECONDS}"))?;
        Ok(Game {
            number,
            score,
            captains,
            stadium,
            ending,
            seconds,
        })
    }
}

/// `value` if it is one of `names`.
fn one_of(field: &str, value: &str, names: &[&'static str]) -> Result<&'static str, String> {
    names
        .iter()
        .find(|name| **name == value)
        .copied()
        .ok_or_else(|| format!("{field}: one of {}", names.join(", ")))
}

fn captains(field: &str, captains: &Teams<String>) -> Result<Teams<&'static str>, String> {
    let captain = |team: &str, name: &str| {
        if name == UNKNOWN {
            return Ok(UNKNOWN);
        }
        CAPTAINS
            .into_iter()
            .find(|captain| *captain == name)
            .ok_or_else(|| format!("{field}.{team}: unknown or one of {}", CAPTAINS.join(", ")))
    };
    Ok(Teams {
        home: captain("home", &captains.home)?,
        away: captain("away", &captains.away)?,
    })
}

fn sidekicks_of(field: &str, names: Vec<String>) -> Result<Vec<&'static str>, String> {
    let problem = || {
        format!(
            "{field}: up to {TEAM_SIDEKICKS} of {}",
            SIDEKICKS.join(", ")
        )
    };
    if names.len() > TEAM_SIDEKICKS {
        return Err(problem());
    }
    names
        .iter()
        .map(|name| {
            SIDEKICKS
                .into_iter()
                .find(|sidekick| sidekick == name)
                .ok_or_else(problem)
        })
        .collect()
}

/// A Mii name as the game shows it: 1 to 10 characters, none of them
/// control characters.
fn mii_name(field: &str, name: String) -> Result<String, String> {
    let length = name.chars().count();
    if !(1..=MII_NAME).contains(&length) || name.chars().any(char::is_control) {
        return Err(format!("{field}: a Mii name of 1 to {MII_NAME} characters"));
    }
    Ok(name)
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// A start of profile 1000000000 at home.
    pub fn start() -> Value {
        json!({
            "profile_id": 1_000_000_000, "release": "R4QE", "event": "start", "series": 1_792_000_000,
            "mode": "ranked", "best_of": 3, "side": "home", "mii": "Anosch", "opponent_mii": "Sam",
            "captains": {"home": "mario", "away": "bowser"},
            "sidekicks": {"home": ["toad", "koopa", "boo"], "away": ["birdo", "boo", "dry_bones"]},
            "game": null, "reason": null,
        })
    }

    fn parse(body: &Value) -> Result<Report, String> {
        Report::parse(body.to_string().as_bytes())
    }

    /// `body` with `change` applied.
    fn with(mut body: Value, change: impl FnOnce(&mut Value)) -> Value {
        change(&mut body);
        body
    }

    fn game_event() -> Value {
        with(start(), |body| {
            body["event"] = json!("game");
            body["game"] = json!({
                "number": 1, "score": {"home": 3, "away": 1},
                "captains": {"home": "mario", "away": "bowser"},
                "stadium": "the_palace", "ending": "normal", "seconds": 180,
            });
        })
    }

    #[test]
    fn the_contracts_example_is_read() {
        let report = parse(&start()).unwrap();
        assert_eq!(report.profile_id, 1_000_000_000);
        assert_eq!(
            (report.release, report.series, report.mode, report.best_of),
            ("R4QE", 1_792_000_000, "ranked", 3)
        );
        assert_eq!(report.side, Side::Home);
        assert_eq!(
            (report.mii.as_str(), report.opponent_mii.as_str()),
            ("Anosch", "Sam")
        );
        assert_eq!(
            report.captains,
            Teams {
                home: "mario",
                away: "bowser"
            }
        );
        assert_eq!(report.sidekicks.away, ["birdo", "boo", "dry_bones"]);
        assert_eq!(report.event, Event::Start);

        let game = parse(&game_event()).unwrap();
        let Event::Game(game) = game.event else {
            panic!("a game");
        };
        assert_eq!(game.number, 1);
        assert_eq!(game.score, Teams { home: 3, away: 1 });
        assert_eq!(
            (game.stadium, game.ending, game.seconds),
            ("the_palace", "normal", 180)
        );

        let end = with(start(), |body| {
            body["event"] = json!("end");
            body["reason"] = json!("abandoned");
        });
        assert_eq!(parse(&end).unwrap().event, Event::End("abandoned"));
    }

    #[test]
    fn unknown_names_and_missing_sidekicks_are_fine() {
        let report = parse(&with(game_event(), |body| {
            body["captains"]["away"] = json!("unknown");
            body["game"]["captains"]["home"] = json!("unknown");
            body["game"]["stadium"] = json!("unknown");
            body["sidekicks"] = json!({"home": ["toad"], "away": []});
            body["extra"] = json!("a field of a later build");
        }))
        .unwrap();
        assert_eq!(report.captains.away, UNKNOWN);
        assert_eq!(
            report.sidekicks,
            Teams {
                home: vec!["toad"],
                away: vec![]
            }
        );
        let Event::Game(game) = report.event else {
            panic!("a game");
        };
        assert_eq!((game.captains.home, game.stadium), (UNKNOWN, UNKNOWN));

        for sidekicks in [json!(null), json!({}), json!({"home": null})] {
            let report = parse(&with(start(), |body| body["sidekicks"] = sidekicks)).unwrap();
            assert!(report.sidekicks.home.is_empty() && report.sidekicks.away.is_empty());
        }
        let without = with(start(), |body| {
            let body = body.as_object_mut().unwrap();
            for field in ["sidekicks", "game", "reason"] {
                body.remove(field);
            }
        });
        assert!(parse(&without).is_ok());
    }

    #[test]
    fn mii_names_count_characters_not_bytes() {
        // Ten characters, sixteen bytes in UTF-8.
        let name = "Ålésund☆ßé";
        assert_eq!((name.chars().count(), name.len()), (10, 16));
        assert!(parse(&with(start(), |body| body["mii"] = json!(name))).is_ok());
        for bad in ["", "Elevenchars", "Tab\there"] {
            let error =
                parse(&with(start(), |body| body["opponent_mii"] = json!(bad))).unwrap_err();
            assert!(error.starts_with("opponent_mii:"), "{bad}: {error}");
        }
    }

    #[test]
    fn invalid_reports_say_what_is_wrong() {
        type Change = fn(&mut Value);
        let cases: [(&str, Value, Change); 25] = [
            ("profile_id", start(), |body| body["profile_id"] = json!(0)),
            ("profile_id", start(), |body| {
                body["profile_id"] = json!(1_i64 << 31)
            }),
            ("release", start(), |body| body["release"] = json!("R4QX")),
            ("event", start(), |body| body["event"] = json!("pause")),
            ("series", start(), |body| body["series"] = json!(-1)),
            ("mode", start(), |body| body["mode"] = json!("cup")),
            ("best_of", start(), |body| body["best_of"] = json!(2)),
            ("side", start(), |body| body["side"] = json!("left")),
            ("mii", start(), |body| body["mii"] = json!("Eleven char")),
            ("captains.home", start(), |body| {
                body["captains"]["home"] = json!("toad")
            }),
            ("captains.away", start(), |body| {
                body["captains"]["away"] = json!("Bowser")
            }),
            ("sidekicks.home", start(), |body| {
                body["sidekicks"]["home"] = json!(["toad", "toad", "toad", "toad"])
            }),
            ("sidekicks.away", start(), |body| {
                body["sidekicks"]["away"] = json!(["mario"])
            }),
            ("game", start(), |body| {
                body["game"] = json!({"number": 1, "score": {"home": 0, "away": 0},
                    "captains": {"home": "mario", "away": "bowser"}, "stadium": "the_palace",
                    "ending": "normal", "seconds": 1})
            }),
            ("reason", start(), |body| body["reason"] = json!("decided")),
            ("reason", start(), |body| body["event"] = json!("end")),
            ("reason", start(), |body| {
                body["event"] = json!("end");
                body["reason"] = json!("quit");
            }),
            ("game", start(), |body| body["event"] = json!("game")),
            ("game.number", game_event(), |body| {
                body["game"]["number"] = json!(0)
            }),
            ("game.number", game_event(), |body| {
                body["game"]["number"] = json!(4)
            }),
            ("game.score", game_event(), |body| {
                body["game"]["score"]["away"] = json!(100)
            }),
            ("game.captains.away", game_event(), |body| {
                body["game"]["captains"]["away"] = json!("koopa")
            }),
            ("game.stadium", game_event(), |body| {
                body["game"]["stadium"] = json!("random")
            }),
            ("game.ending", game_event(), |body| {
                body["game"]["ending"] = json!("quit")
            }),
            ("game.seconds", game_event(), |body| {
                body["game"]["seconds"] = json!(3601)
            }),
        ];
        for (field, body, change) in cases {
            let body = with(body, change);
            let error = parse(&body).unwrap_err();
            assert!(error.starts_with(&format!("{field}:")), "{field}: {error}");
        }
    }

    #[test]
    fn malformed_json_is_refused() {
        for body in [
            "",
            "not json",
            "[]",
            r#"{"profile_id": 1}"#,
            r#"{"profile_id": "1000000000"}"#,
        ] {
            assert!(Report::parse(body.as_bytes()).is_err(), "{body}");
        }
        let float = start()
            .to_string()
            .replace("\"series\":1792000000", "\"series\":1792000000.5");
        assert_ne!(float, start().to_string());
        assert!(Report::parse(float.as_bytes()).is_err());
        let missing = with(start(), |body| {
            body.as_object_mut().unwrap().remove("captains");
        });
        assert!(parse(&missing).unwrap_err().contains("captains"));
    }
}

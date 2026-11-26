//! `just tournament-samples`: sample tournaments in the development database
//! (DATABASE_URL), for looking at brackets of every shape on the dashboard
//! and in the game: each format with entrants that are and aren't a power
//! of 2 (byes), at each stage, with players who left, a grand final's
//! reset, long names. They are made through the store as the admin's and
//! the matches' would be, past the routes' Discord check, by sample players
//! (a `test` identity `sample-…`). `just tournament-samples-clear` takes
//! them away again, the players too.
//!
//! `TOURNAMENT_SAMPLES`, words split by commas, makes only the samples whose
//! name has one of them: `double`, `single 13`, `reset`.

use sqlx::PgPool;

use super::{
    bracket::Format,
    store::{self, NewTournament, Seeding},
};
use crate::profiles::rules;

/// Their names start with it: the clearing finds them by it.
const PREFIX: &str = "Sample · ";

/// How far a sample is played.
#[derive(Clone, Copy, PartialEq)]
enum Stage {
    /// Open for entries, not started.
    Registration,
    /// Started: only the byes are decided.
    Started,
    /// About half its sets decided.
    Half,
    /// Everything but its last set.
    LastSet,
    /// All played: finished.
    Finished,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Stage::Registration => "registration",
            Stage::Started => "started",
            Stage::Half => "half",
            Stage::LastSet => "last set",
            Stage::Finished => "finished",
        }
    }
}

struct Sample {
    format: Format,
    entrants: usize,
    stage: Stage,
    /// Entrants who leave once it runs (by seed, from 0), after the first
    /// round is played.
    leaving: &'static [usize],
    /// The grand final goes to the losers' side: the reset is played.
    reset: bool,
}

impl Sample {
    fn new(format: Format, entrants: usize, stage: Stage) -> Self {
        Sample {
            format,
            entrants,
            stage,
            leaving: &[],
            reset: false,
        }
    }

    fn name(&self) -> String {
        let format = match self.format {
            Format::Single => "Single",
            Format::Double => "Double",
            Format::SingleAsh => "Single ash",
        };
        let mut name = format!("{PREFIX}{format} {} · {}", self.entrants, self.stage.name());
        if !self.leaving.is_empty() {
            name.push_str(" · left");
        }
        if self.reset {
            name.push_str(" · reset");
        }
        name
    }
}

const FORMATS: [Format; 3] = [Format::Single, Format::SingleAsh, Format::Double];

/// Every sample: each format at each size half played, each stage at 6
/// entrants, then the special ones.
fn samples() -> Vec<Sample> {
    let mut samples = Vec::new();
    for format in FORMATS {
        for entrants in [2, 3, 4, 5, 7, 9, 12, 13, 16, 17, 24, 31, 33] {
            samples.push(Sample::new(format, entrants, Stage::Half));
        }
        for stage in [
            Stage::Registration,
            Stage::Started,
            Stage::LastSet,
            Stage::Finished,
        ] {
            samples.push(Sample::new(format, 6, stage));
        }
        samples.push(Sample {
            leaving: &[1, 6],
            ..Sample::new(format, 10, Stage::Half)
        });
        samples.push(Sample::new(format, 64, Stage::Half));
    }
    samples.push(Sample {
        reset: true,
        ..Sample::new(Format::Double, 4, Stage::LastSet)
    });
    samples.push(Sample {
        reset: true,
        ..Sample::new(Format::Double, 8, Stage::Finished)
    });
    samples.push(Sample::new(Format::Single, 128, Stage::Half));
    samples
}

/// The sample players' names: some short, some as long as a name goes.
fn player_name(n: usize) -> String {
    const NAMES: [&str; 8] = [
        "Ari",
        "Bowser Jr Fan",
        "Captain_Strikerz",
        "Dee",
        "Electro Ball Enjoyer",
        "Fio",
        "Gustavo the Very Long Named Goalkeeper",
        "Hana",
    ];
    format!("{} {}", NAMES[n % NAMES.len()], n + 1)
}

/// The sample players, `count` of them, made the first time.
async fn players(pool: &PgPool, count: usize) -> Vec<i64> {
    let mut players = Vec::new();
    for n in 0..count {
        let external_id = format!("sample-{}", n + 1);
        let found: Option<i64> = sqlx::query_scalar(
            "SELECT player_id FROM player_identities WHERE provider = 'test' AND external_id = $1",
        )
        .bind(&external_id)
        .fetch_optional(pool)
        .await
        .unwrap();
        if let Some(player_id) = found {
            players.push(player_id);
            continue;
        }
        let name = player_name(n);
        let player_id: i64 = sqlx::query_scalar(
            "INSERT INTO players (display_name) VALUES ($1) RETURNING player_id",
        )
        .bind(&name)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO player_identities (provider, external_id, player_id, username)
             VALUES ('test', $1, $2, $1)",
        )
        .bind(&external_id)
        .bind(player_id)
        .execute(pool)
        .await
        .unwrap();
        players.push(player_id);
    }
    players
}

/// The sets still to be played: neither decided nor void, nor a grand
/// final's reset waiting for the grand final (it may never be played).
async fn open_sets(pool: &PgPool, id: i64) -> Vec<store::Set> {
    store::sets(pool, id)
        .await
        .unwrap()
        .into_iter()
        .filter(|set| !matches!(set.state.as_str(), "finished" | "void"))
        .filter(|set| !(set.side == "grand_final_reset" && set.state == "pending"))
        .collect()
}

/// Decides the first set to play: the home player wins it, but about one
/// in three goes to the away player (upsets, so the losers' side has
/// players from all over), the grand final to the losers' side when
/// `reset`. It goes as a series played to the end: its winner has the
/// games they needed, the loser some. False when no set is to play.
async fn play_one(pool: &PgPool, id: i64, reset: bool) -> bool {
    let sets = store::sets(pool, id).await.unwrap();
    let Some(set) = sets.iter().find(|set| set.state == "ready") else {
        return false;
    };
    let away_wins = if set.side == "grand_final" {
        reset
    } else {
        set.id % 3 == 0
    };
    let winner = if away_wins { set.away } else { set.home };
    store::decide(pool, id, set.id, winner)
        .await
        .unwrap()
        .unwrap();
    let needed = i16::from(set.games) / 2 + 1;
    let lost = (set.id % i64::from(needed)) as i16;
    let (home, away) = if away_wins {
        (lost, needed)
    } else {
        (needed, lost)
    };
    sqlx::query(
        "UPDATE tournament_sets SET home_wins = $2, away_wins = $3, decided_by = 'match'
         WHERE tournament_set_id = $1",
    )
    .bind(set.id)
    .bind(home)
    .bind(away)
    .execute(pool)
    .await
    .unwrap();
    true
}

async fn make(pool: &PgPool, sample: &Sample, players: &[i64]) {
    let rule_set = rules::list(pool)
        .await
        .unwrap()
        .into_iter()
        .find(|set| set.name == "Competitive")
        .expect("the Competitive rule set");
    let new = NewTournament {
        name: sample.name(),
        organizer: None,
        format: sample.format,
        rule_set,
        best_of: 3,
        semifinals_best_of: 5,
        finals_best_of: 7,
        max_entrants: None,
        starts_at: None,
    };
    let id = store::create(pool, &new).await.unwrap();
    for &player_id in &players[..sample.entrants] {
        store::join(pool, id, player_id).await.unwrap().unwrap();
    }
    if sample.stage == Stage::Registration {
        return;
    }
    store::start(pool, id, Seeding::Joined)
        .await
        .unwrap()
        .unwrap();
    if !sample.leaving.is_empty() {
        // The first round, then the ones who leave go.
        let first: Vec<i64> = store::sets(pool, id)
            .await
            .unwrap()
            .into_iter()
            .filter(|set| set.round == 1 && set.side == "winners" && set.state == "ready")
            .map(|set| set.id)
            .collect();
        for _ in &first {
            play_one(pool, id, sample.reset).await;
        }
        for &seed in sample.leaving {
            store::leave(pool, id, players[seed])
                .await
                .unwrap()
                .unwrap();
        }
    }
    let total = open_sets(pool, id).await.len();
    loop {
        let open = open_sets(pool, id).await;
        let enough = match sample.stage {
            Stage::Registration | Stage::Started => true,
            Stage::Half => open.len() <= total / 2,
            // With `reset`, the last set is the reset itself.
            Stage::LastSet if sample.reset => {
                open.iter().any(|set| set.side == "grand_final_reset")
            }
            Stage::LastSet => open.len() <= 1,
            Stage::Finished => open.is_empty(),
        };
        if enough || !play_one(pool, id, sample.reset).await {
            break;
        }
    }
}

async fn dev_database() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL, the development database's");
    PgPool::connect(&url).await.unwrap()
}

#[tokio::test]
#[ignore = "writes to the development database: just tournament-samples"]
async fn make_tournament_samples() {
    let pool = dev_database().await;
    let wanted: Vec<String> = std::env::var("TOURNAMENT_SAMPLES")
        .unwrap_or_default()
        .split(',')
        .map(|word| word.trim().to_lowercase())
        .filter(|word| !word.is_empty())
        .collect();
    let samples: Vec<Sample> = samples()
        .into_iter()
        .filter(|sample| {
            let name = sample.name().to_lowercase();
            wanted.is_empty() || wanted.iter().any(|word| name.contains(word))
        })
        .collect();
    let most = samples
        .iter()
        .map(|sample| sample.entrants)
        .max()
        .unwrap_or(0);
    let players = players(&pool, most).await;
    for sample in &samples {
        make(&pool, sample, &players).await;
        println!("made {}", sample.name());
    }
    println!("{} samples", samples.len());
}

#[tokio::test]
#[ignore = "writes to the development database: just tournament-samples-clear"]
async fn clear_tournament_samples() {
    let pool = dev_database().await;
    // Their entrants and sets go with them; the samples play no matches.
    let tournaments = sqlx::query("DELETE FROM tournaments WHERE starts_with(name, $1)")
        .bind(PREFIX)
        .execute(&pool)
        .await
        .unwrap()
        .rows_affected();
    let players = sqlx::query(
        "DELETE FROM players WHERE player_id IN
             (SELECT player_id FROM player_identities
              WHERE provider = 'test' AND starts_with(external_id, 'sample-'))",
    )
    .execute(&pool)
    .await
    .unwrap()
    .rows_affected();
    println!("cleared {tournaments} tournaments and {players} players");
}

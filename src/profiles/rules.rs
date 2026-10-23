//! The rules a match is played by (docs/profiles.md, "Rules"), which the
//! server's admins set: gameplay tweaks, skill level, how a game ends, the
//! games of a series and the stadium. The port applies them in both games.
//! Both must play by the very same rules, or the match goes out of sync, so
//! the server pairs two games only once both confirmed the rules' hash.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// The port's gameplay tweaks a rule set may switch on: the ones that change
/// play for both teams alike. Its other tweaks stay off in a match; fair ball
/// contests are always on online.
pub const TWEAKS: [&str; 3] = ["fast_terrain", "no_mega_strikes", "thunder_island_walls"];
/// The game's skill levels (`GameplaySettings::eSkillLevel`).
pub const SKILLS: [&str; 5] = ["training", "rookie", "professional", "superstar", "legend"];
/// The stadiums a match can be played in (`eStadiumID` without the tutorial
/// field and the low-poly one), or `random`: the host picks one, as in the
/// game.
pub const STADIUMS: [&str; 16] = [
    "battle_dome",
    "bowser_stadium",
    "crater_field",
    "crystal_canyon",
    "the_dump",
    "galactic_stadium",
    "konga_coliseum",
    "lava_pit",
    "pipeline_central",
    "storm_ship",
    "the_palace",
    "thunder_island",
    "the_underground",
    "the_vice",
    "the_wastelands",
    "sand_tomb",
];
/// Games in a series.
pub const SERIES: [u8; 4] = [1, 3, 5, 7];

/// A rule set's rules. Serialized in this field order with the tweaks
/// sorted, which is what [`Rules::hash`] hashes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub tweaks: Vec<String>,
    pub skill: String,
    /// `time` or `goals`: what ends a game.
    pub limit: String,
    /// A game's length in seconds, when time ends it.
    pub time: u32,
    /// The goals that end a game, when goals do.
    pub goals: u32,
    /// Games in the series.
    pub games: u8,
    /// One of [`STADIUMS`], or `random`.
    pub stadium: String,
}

impl Rules {
    /// The rules if they are valid, with the tweaks sorted and once each;
    /// otherwise what is wrong.
    pub fn check(mut self) -> Result<Self, String> {
        self.tweaks.sort();
        self.tweaks.dedup();
        if let Some(tweak) = self
            .tweaks
            .iter()
            .find(|tweak| !TWEAKS.contains(&tweak.as_str()))
        {
            return Err(format!(
                "tweaks: {tweak} is not one of {}",
                TWEAKS.join(", ")
            ));
        }
        if !SKILLS.contains(&self.skill.as_str()) {
            return Err(format!("skill: one of {}", SKILLS.join(", ")));
        }
        if !matches!(self.limit.as_str(), "time" | "goals") {
            return Err("limit: time or goals".into());
        }
        if !(60..=900).contains(&self.time) || !self.time.is_multiple_of(30) {
            return Err("time: 60 to 900 seconds, in steps of 30".into());
        }
        if !(1..=10).contains(&self.goals) {
            return Err("goals: 1 to 10".into());
        }
        if !SERIES.contains(&self.games) {
            return Err("games: 1, 3, 5 or 7".into());
        }
        if self.stadium != "random" && !STADIUMS.contains(&self.stadium.as_str()) {
            return Err(format!("stadium: random or one of {}", STADIUMS.join(", ")));
        }
        Ok(self)
    }

    /// The rules as JSON, in their canonical form.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("rules serialize")
    }

    /// SHA-256 of [`Rules::to_json`], in lowercase hex.
    pub fn hash(&self) -> String {
        Sha256::digest(self.to_json().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Rules read back from the database: checked when they were saved.
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str::<Self>(json)
            .map_err(|error| error.to_string())?
            .check()
    }
}

/// A rule set as the launcher and `/admin` see it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RuleSet {
    pub id: i64,
    pub name: String,
    pub rules: Rules,
    pub hash: String,
}

/// Why a rule set could not be saved.
#[derive(Debug, PartialEq)]
pub enum SaveError {
    Invalid(String),
    NameTaken,
    NotFound,
}

pub async fn list(pool: &PgPool) -> Result<Vec<RuleSet>, sqlx::Error> {
    let rows = sqlx::query!("SELECT rule_set_id, name, rules FROM rule_sets ORDER BY name")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| rule_set(row.rule_set_id, row.name, &row.rules))
        .collect())
}

pub async fn get(pool: &PgPool, id: i64) -> Result<Option<RuleSet>, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT rule_set_id, name, rules FROM rule_sets WHERE rule_set_id = $1",
        id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|row| rule_set(row.rule_set_id, row.name, &row.rules)))
}

/// Saves a new rule set (`id` none) or changes one. Matches made earlier keep
/// the rules they were made with.
pub async fn save(
    pool: &PgPool,
    id: Option<i64>,
    name: &str,
    rules: Rules,
) -> Result<Result<RuleSet, SaveError>, sqlx::Error> {
    let name = name.trim();
    if !(1..=48).contains(&name.chars().count()) {
        return Ok(Err(SaveError::Invalid("name: 1 to 48 characters".into())));
    }
    let rules = match rules.check() {
        Ok(rules) => rules,
        Err(error) => return Ok(Err(SaveError::Invalid(error))),
    };
    let json = rules.to_json();
    let saved = match id {
        None => {
            sqlx::query_scalar!(
                "INSERT INTO rule_sets (name, rules) VALUES ($1, $2)
                 ON CONFLICT (name) DO NOTHING
                 RETURNING rule_set_id",
                name,
                json,
            )
            .fetch_optional(pool)
            .await?
        }
        Some(id) => {
            if sqlx::query_scalar!(
                "SELECT rule_set_id FROM rule_sets WHERE name = $1 AND rule_set_id <> $2",
                name,
                id,
            )
            .fetch_optional(pool)
            .await?
            .is_some()
            {
                return Ok(Err(SaveError::NameTaken));
            }
            let updated = sqlx::query_scalar!(
                "UPDATE rule_sets SET name = $2, rules = $3, updated_at = now()
                 WHERE rule_set_id = $1
                 RETURNING rule_set_id",
                id,
                name,
                json,
            )
            .fetch_optional(pool)
            .await?;
            if updated.is_none() {
                return Ok(Err(SaveError::NotFound));
            }
            updated
        }
    };
    Ok(match saved {
        Some(id) => Ok(RuleSet {
            id,
            name: name.to_owned(),
            hash: rules.hash(),
            rules,
        }),
        None => Err(SaveError::NameTaken),
    })
}

pub async fn delete(pool: &PgPool, id: i64) -> Result<bool, sqlx::Error> {
    let deleted = sqlx::query!("DELETE FROM rule_sets WHERE rule_set_id = $1", id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(deleted == 1)
}

fn rule_set(id: i64, name: String, json: &str) -> Option<RuleSet> {
    match Rules::from_json(json) {
        Ok(rules) => Some(RuleSet {
            id,
            name,
            hash: rules.hash(),
            rules,
        }),
        Err(error) => {
            tracing::warn!(target: "profiles", id, %error, "rule set not understood");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn competitive() -> Rules {
        Rules {
            tweaks: vec![
                "thunder_island_walls".into(),
                "no_mega_strikes".into(),
                "fast_terrain".into(),
                "no_mega_strikes".into(),
            ],
            skill: "rookie".into(),
            limit: "time".into(),
            time: 180,
            goals: 5,
            games: 3,
            stadium: "random".into(),
        }
    }

    #[test]
    fn rules_are_canonical_and_hashed() {
        let rules = competitive().check().unwrap();
        assert_eq!(
            rules.tweaks,
            ["fast_terrain", "no_mega_strikes", "thunder_island_walls"]
        );
        assert_eq!(
            rules.to_json(),
            r#"{"tweaks":["fast_terrain","no_mega_strikes","thunder_island_walls"],"skill":"rookie","limit":"time","time":180,"goals":5,"games":3,"stadium":"random"}"#
        );
        assert_eq!(rules.hash().len(), 64);
        assert_eq!(Rules::from_json(&rules.to_json()).unwrap(), rules);
    }

    #[test]
    fn invalid_rules_say_what_is_wrong() {
        type Change = fn(&mut Rules);
        let cases: [(&str, Change); 7] = [
            ("tweaks", |rules| rules.tweaks.push("super_team".into())),
            ("skill", |rules| rules.skill = "easy".into()),
            ("limit", |rules| rules.limit = "score".into()),
            ("time", |rules| rules.time = 185),
            ("goals", |rules| rules.goals = 0),
            ("games", |rules| rules.games = 2),
            ("stadium", |rules| rules.stadium = "tutorial_field".into()),
        ];
        for (field, change) in cases {
            let mut rules = competitive();
            change(&mut rules);
            let error = rules.check().unwrap_err();
            assert!(error.starts_with(field), "{field}: {error}");
        }
        assert!(Rules::from_json(r#"{"tweaks": []}"#).is_err());
        assert!(
            Rules::from_json(
                r#"{"tweaks":[],"skill":"rookie","limit":"time","time":180,"goals":5,"games":3,"stadium":"random","cheats":true}"#
            )
            .is_err()
        );
    }

    #[sqlx::test]
    async fn the_seeded_rule_sets_are_valid(pool: PgPool) {
        let sets = list(&pool).await.unwrap();
        let names: Vec<&str> = sets.iter().map(|set| set.name.as_str()).collect();
        assert_eq!(names, ["Competitive", "Retail ranked"]);
        assert_eq!(
            sets[0].rules.tweaks,
            ["fast_terrain", "no_mega_strikes", "thunder_island_walls"]
        );
    }

    #[sqlx::test]
    async fn rule_sets_are_saved_and_deleted(pool: PgPool) {
        let saved = save(&pool, None, " Long games ", {
            let mut rules = competitive();
            rules.time = 300;
            rules
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(saved.name, "Long games");
        assert_eq!(get(&pool, saved.id).await.unwrap(), Some(saved.clone()));

        assert_eq!(
            save(&pool, None, "Long games", competitive())
                .await
                .unwrap(),
            Err(SaveError::NameTaken)
        );
        assert_eq!(
            save(&pool, Some(saved.id), "Competitive", competitive())
                .await
                .unwrap(),
            Err(SaveError::NameTaken)
        );
        assert!(matches!(
            save(&pool, Some(saved.id), "Long games", {
                let mut rules = competitive();
                rules.games = 4;
                rules
            })
            .await
            .unwrap(),
            Err(SaveError::Invalid(_))
        ));
        let renamed = save(&pool, Some(saved.id), "Longer games", competitive())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(renamed.id, saved.id);
        assert_ne!(renamed.hash, saved.hash);

        assert!(delete(&pool, saved.id).await.unwrap());
        assert!(!delete(&pool, saved.id).await.unwrap());
        assert_eq!(
            save(&pool, Some(saved.id), "Gone", competitive())
                .await
                .unwrap(),
            Err(SaveError::NotFound)
        );
    }
}

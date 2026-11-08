//! One row per player on every board (docs/gamestats2.md, "One row per
//! player"): the WFC profiles a launcher proved for one Recharged player
//! (`player_wfc_profiles`) are that player's, whichever game they come from
//! (the US and the European game have a profile each), so their rows count as
//! one. The game's own boards (`leaderboard`), the launcher's Leaderboard page
//! and the admin site's merge them alike: points, wins and losses added up,
//! shown with the Mii of the player's best row. Only rows of one season (or
//! day) add up: a row of an ended one stays in `scores` until its profile's
//! next put, and stays a row of its own. Profiles of no player stay rows of
//! their own.

use std::collections::HashMap;

use sqlx::PgExecutor;
use time::Date;

use super::{
    season,
    submission::{self, Submission},
};

/// The highest score a put takes (`put`): a sum stays within it.
const MAX_SCORE: i32 = 999_999;

/// A row of a board as merging sees it. `stamp` orders the updates, the
/// later one greater: a time, or minus the minutes since.
#[derive(Clone, Debug, PartialEq)]
pub struct Line<T> {
    pub profile_id: i32,
    pub region: i32,
    pub score: i32,
    pub userdata: Vec<u8>,
    pub stamp: T,
    /// The WFC profiles it stands for: itself, and the player's others once
    /// merged.
    pub members: Vec<i32>,
}

impl<T> Line<T> {
    pub fn new(profile_id: i32, region: i32, score: i32, userdata: Vec<u8>, stamp: T) -> Self {
        Self {
            profile_id,
            region,
            score,
            userdata,
            stamp,
            members: vec![profile_id],
        }
    }
}

/// The player of each of `profile_ids` that a launcher proved for one.
pub async fn players_of<'e>(
    executor: impl PgExecutor<'e>,
    profile_ids: &[i32],
) -> Result<HashMap<i32, i64>, sqlx::Error> {
    Ok(sqlx::query!(
        "SELECT profile_id, player_id FROM player_wfc_profiles WHERE profile_id = ANY($1)",
        profile_ids
    )
    .fetch_all(executor)
    .await?
    .into_iter()
    .map(|row| (row.profile_id, row.player_id))
    .collect())
}

/// `lines` of a board of `category` with each player's rows of one season
/// (or day, by the date in their userdata) as one, in no particular order:
/// the score, wins and losses of all of them, the latest stamp, and the
/// profile, region and Mii (the rest of the userdata) of the player's best
/// row. The row of the highest `preference` stands for its player first (the
/// game asking: its own Mii stays its own). Rows without a date count as one
/// season.
pub fn by_player<T: Copy + Ord>(
    lines: Vec<Line<T>>,
    players: &HashMap<i32, i64>,
    category: i32,
    preference: impl Fn(i32) -> u8,
) -> Vec<Line<T>> {
    let mut merged: Vec<Line<T>> = Vec::with_capacity(lines.len());
    let mut groups: HashMap<(i64, Option<Date>), Vec<Line<T>>> = HashMap::new();
    for line in lines {
        match players.get(&line.profile_id) {
            Some(&player) => {
                let first_day = Submission::parse(&line.userdata)
                    .date
                    .map(|date| season::first_day(category, date));
                groups.entry((player, first_day)).or_default().push(line)
            }
            None => merged.push(line),
        }
    }
    for (_, mut rows) in groups {
        if rows.len() == 1 {
            merged.extend(rows);
            continue;
        }
        // The best row first: preferred, then the highest score, then the
        // latest.
        rows.sort_by(|a, b| {
            (preference(b.profile_id), b.score, b.stamp, b.profile_id).cmp(&(
                preference(a.profile_id),
                a.score,
                a.stamp,
                a.profile_id,
            ))
        });
        let score = rows
            .iter()
            .map(|row| i64::from(row.score))
            .sum::<i64>()
            .min(i64::from(MAX_SCORE)) as i32;
        let stamp = rows.iter().map(|row| row.stamp).max().unwrap();
        let mut members: Vec<i32> = rows.iter().map(|row| row.profile_id).collect();
        members.sort_unstable();
        // Wins and losses where the rows' userdata tell them.
        let records: Vec<Submission> = rows
            .iter()
            .map(|row| Submission::parse(&row.userdata))
            .collect();
        let total = |field: fn(&Submission) -> Option<i32>| {
            records
                .iter()
                .filter_map(field)
                .reduce(|sum, value| sum + value)
        };
        let (wins, losses) = (total(|record| record.wins), total(|record| record.losses));
        let best = rows.swap_remove(0);
        let userdata = submission::with_record(&best.userdata, wins, losses);
        merged.push(Line {
            profile_id: best.profile_id,
            region: best.region,
            score,
            userdata,
            stamp,
            members,
        });
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn userdata(wins: i32, losses: i32, name: &str) -> Vec<u8> {
        let mut userdata = vec![0; 128];
        userdata[0x04..0x08].copy_from_slice(&wins.to_le_bytes());
        userdata[0x08..0x0C].copy_from_slice(&losses.to_le_bytes());
        for (unit, at) in name.encode_utf16().zip((0x0E..).step_by(2)) {
            userdata[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        // Signed, as the game puts it.
        submission::sign(&mut userdata);
        userdata
    }

    fn line(
        profile_id: i32,
        region: i32,
        score: i32,
        record: (i32, i32),
        name: &str,
        stamp: i64,
    ) -> Line<i64> {
        Line::new(
            profile_id,
            region,
            score,
            userdata(record.0, record.1, name),
            stamp,
        )
    }

    #[test]
    fn a_players_rows_add_up_under_their_best_mii() {
        // Birnen's European and US profiles, Raulaso's two Miis of no
        // player, and Mario alone.
        let players = HashMap::from([(108, 1), (107, 1), (50, 2)]);
        let lines = vec![
            line(108, 4, 171, (10, 4), "Birnen", 10),
            line(95, 4, 161, (11, 2), "Raulaso", 20),
            line(62, 4, 147, (10, 0), "RAULASO", 30),
            line(107, 2, 85, (5, 0), "Birnen US", 40),
            line(50, 2, 60, (3, 3), "Mario", 5),
        ];
        let mut merged = by_player(lines, &players, 0, |_| 0);
        merged.sort_by_key(|line| -line.score);
        let shown: Vec<_> = merged
            .iter()
            .map(|line| {
                let record = Submission::parse(&line.userdata);
                (
                    line.profile_id,
                    line.region,
                    line.score,
                    record.name.unwrap(),
                    record.wins.unwrap(),
                    record.losses.unwrap(),
                    line.stamp,
                    line.members.clone(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (108, 4, 256, "Birnen".into(), 15, 4, 40, vec![107, 108]),
                (95, 4, 161, "Raulaso".into(), 11, 2, 20, vec![95]),
                (62, 4, 147, "RAULASO".into(), 10, 0, 30, vec![62]),
                (50, 2, 60, "Mario".into(), 3, 3, 5, vec![50]),
            ]
        );
        // The game shows every row: Birnen's merged one too, whose record
        // the server changed.
        for line in &merged {
            assert!(submission::signed(&line.userdata), "{}", line.profile_id);
        }
    }

    #[test]
    fn the_asking_mii_stands_for_its_player() {
        let players = HashMap::from([(108, 1), (107, 1)]);
        let lines = vec![
            line(108, 4, 171, (10, 4), "Birnen", 10),
            line(107, 2, 85, (5, 0), "Birnen US", 40),
        ];
        let merged = by_player(lines, &players, 0, |pid| u8::from(pid == 107));
        assert_eq!(merged.len(), 1);
        assert_eq!(
            (merged[0].profile_id, merged[0].region, merged[0].score),
            (107, 2, 256)
        );
        assert_eq!(
            Submission::parse(&merged[0].userdata).name.as_deref(),
            Some("Birnen US")
        );
    }

    #[test]
    fn rows_of_another_season_stay_apart() {
        let players = HashMap::from([(108, 1), (107, 1)]);
        let dated = |year: u16, month: u8, day: u8| {
            let mut userdata = userdata(1, 0, "Birnen");
            userdata[0] = day;
            userdata[1] = month;
            userdata[2..4].copy_from_slice(&year.to_le_bytes());
            userdata
        };
        // The season board: one row from last year's season, one from now.
        let lines = vec![
            Line::new(108, 4, 171, dated(2025, 12, 1), 10),
            Line::new(107, 2, 85, dated(2026, 10, 31), 40),
        ];
        let mut merged = by_player(lines, &players, 0, |_| 0);
        merged.sort_by_key(|line| line.profile_id);
        let scores: Vec<_> = merged
            .iter()
            .map(|line| (line.profile_id, line.score, line.members.clone()))
            .collect();
        assert_eq!(scores, [(107, 85, vec![107]), (108, 171, vec![108])]);
    }

    #[test]
    fn sums_stay_in_the_games_range() {
        let players = HashMap::from([(1, 9), (2, 9)]);
        let lines = vec![
            line(1, 2, 900_000, (9000, 0), "A", 1),
            line(2, 2, 900_000, (9000, 1), "B", 2),
        ];
        let merged = by_player(lines, &players, 0, |_| 0);
        let record = Submission::parse(&merged[0].userdata);
        assert_eq!(
            (merged[0].score, record.wins, record.losses),
            (MAX_SCORE, Some(9999), Some(1))
        );
    }
}

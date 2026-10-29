//! The Players page: the accounts consoles registered, their profiles (one per
//! Mii slot) with their friend codes, and who is logged in to GPCM. A
//! profile's own page adds its record in the matches Recharged builds report
//! (docs/reports.md), its standing on the leaderboards and its matches.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;

use super::page::snapshot;
use crate::{
    accounts,
    gamestats::submission::Submission,
    gpcm::{Gpcm, Presence},
    reports::{
        self,
        outcome::{self, Compared},
        report::{Side, Teams, UNKNOWN},
    },
};

/// Profiles shown, the last logins first.
const SHOWN: i64 = 200;
/// A profile's matches shown, the newest first; its record counts them all.
const MATCHES_SHOWN: i64 = 100;
/// The modes of a match, as reports name them.
const MODES: [&str; 2] = ["ranked", "friends"];

#[derive(Serialize)]
pub struct Players {
    totals: Totals,
    /// The [`SHOWN`] profiles that logged in last, online ones first.
    profiles: Vec<Player>,
}

#[derive(Serialize)]
pub struct Totals {
    accounts: i64,
    profiles: i64,
    /// Logged in to GPCM now; the CPU buddy does not count.
    online: usize,
    #[serde(with = "time::serde::rfc3339::option")]
    last_login: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct Player {
    profile_id: i32,
    user_id: i64,
    gsbrcd: String,
    /// The code a player of the profile's own release enters.
    friend_code: String,
    mii_name: Option<String>,
    /// Answered by the server itself (docs/login.md, "The CPU buddy").
    cpu_buddy: bool,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    last_login_at: OffsetDateTime,
    last_ip: Option<String>,
    /// Rows in `scores`.
    scores: i64,
    online: Option<Online>,
}

/// A login to GPCM.
#[derive(Serialize)]
struct Online {
    #[serde(with = "time::serde::rfc3339")]
    since: OffsetDateTime,
    status: Option<u8>,
    /// When `status` began.
    #[serde(with = "time::serde::rfc3339::option")]
    status_since: Option<OffsetDateTime>,
    friend_requests: usize,
    mutual_friends: usize,
}

impl From<&Presence> for Online {
    fn from(presence: &Presence) -> Self {
        Self {
            since: presence.logged_in_at,
            status: presence.status,
            status_since: presence.status_since,
            friend_requests: presence.friend_requests,
            mutual_friends: presence.mutual_friends,
        }
    }
}

pub async fn read(pool: &PgPool, gpcm: &Gpcm) -> Result<Players, sqlx::Error> {
    let presence = gpcm.presence();
    let mut tx = snapshot(pool).await?;
    let players = Players {
        totals: totals(&mut tx, &presence).await?,
        profiles: profiles(&mut tx, &presence).await?,
    };
    tx.commit().await?;
    Ok(players)
}

pub async fn totals(
    conn: &mut PgConnection,
    presence: &HashMap<i32, Presence>,
) -> Result<Totals, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT (SELECT count(*) FROM accounts WHERE user_id <> $1) AS "accounts!",
                  (SELECT count(*) FROM profiles WHERE profile_id <> $2) AS "profiles!",
                  (SELECT max(last_login_at) FROM profiles WHERE profile_id <> $2) AS last_login"#,
        accounts::CPU_BUDDY_ACCOUNT,
        accounts::CPU_BUDDY,
    )
    .fetch_one(conn)
    .await?;
    Ok(Totals {
        accounts: row.accounts,
        profiles: row.profiles,
        online: presence.len(),
        last_login: row.last_login,
    })
}

async fn profiles(
    conn: &mut PgConnection,
    presence: &HashMap<i32, Presence>,
) -> Result<Vec<Player>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT p.profile_id, p.user_id, p.gsbrcd, p.ingamesn, p.created_at,
                  p.last_login_at, p.last_ip,
                  (SELECT count(*) FROM scores s WHERE s.profile_id = p.profile_id) AS "scores!"
           FROM profiles p
           ORDER BY p.last_login_at DESC, p.profile_id
           LIMIT $1"#,
        SHOWN,
    )
    .fetch_all(conn)
    .await?;
    let mut players: Vec<Player> = rows
        .into_iter()
        .map(|row| Player {
            friend_code: accounts::friend_code(
                row.profile_id,
                row.gsbrcd.get(..4).unwrap_or_default(),
            ),
            cpu_buddy: row.profile_id == accounts::CPU_BUDDY,
            online: presence.get(&row.profile_id).map(Online::from),
            profile_id: row.profile_id,
            user_id: row.user_id,
            gsbrcd: row.gsbrcd,
            mii_name: row.ingamesn,
            created_at: row.created_at,
            last_login_at: row.last_login_at,
            last_ip: row.last_ip,
            scores: row.scores,
        })
        .collect();
    // The CPU buddy, then who is online; each in the order of their logins.
    players.sort_by_key(|player| (!player.cpu_buddy, player.online.is_none()));
    Ok(players)
}

/// A profile with its record.
#[derive(Serialize)]
pub struct Detail {
    profile: Profile,
    /// Its record in the reported matches, ranked and with friends.
    stats: Vec<ModeStats>,
    /// Its rows in the leaderboards, ranked as the Leaderboards page ranks.
    standing: Vec<Standing>,
    /// Its latest [`MATCHES_SHOWN`] matches, the newest first.
    matches: Vec<Played>,
}

#[derive(Serialize)]
struct Profile {
    profile_id: i32,
    /// The account of the console.
    user_id: i64,
    gsbrcd: String,
    /// The game code its game plays.
    release: String,
    friend_code: String,
    mii_name: Option<String>,
    cpu_buddy: bool,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    last_login_at: OffsetDateTime,
    last_ip: Option<String>,
    online: Option<Online>,
    /// The Recharged player who proved the profile theirs
    /// (docs/profiles.md, "Friend codes").
    player: Option<Bound>,
}

#[derive(Serialize)]
struct Bound {
    name: String,
    discord: Option<String>,
}

#[derive(Default, Serialize)]
struct ModeStats {
    mode: &'static str,
    series: SeriesRecord,
    games: GamesRecord,
    /// The captains the profile's team played its games with, most used
    /// first.
    captains: Vec<CaptainUse>,
}

#[derive(Default, Serialize)]
struct SeriesRecord {
    played: u32,
    won: u32,
    lost: u32,
    drawn: u32,
    disputed: u32,
    abandoned: u32,
    in_progress: u32,
}

/// The games that count (docs/reports.md, "Games and states").
#[derive(Default, Serialize)]
struct GamesRecord {
    played: u32,
    won: u32,
    lost: u32,
    drawn: u32,
    goals_for: u32,
    goals_against: u32,
}

#[derive(Serialize)]
struct CaptainUse {
    captain: String,
    games: u32,
}

#[derive(Serialize)]
struct Standing {
    category: i32,
    /// Equal scores share a rank.
    rank: i64,
    /// Rows on the board.
    entries: i64,
    score: i32,
    wins: Option<i32>,
    losses: Option<i32>,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

/// A match the profile reported, from its side.
#[derive(Serialize)]
struct Played {
    id: i64,
    mode: String,
    release: String,
    best_of: i32,
    state: String,
    side: Side,
    /// Once finished: `won`, `lost` or `drawn`.
    result: Option<&'static str>,
    captain: String,
    opponent: Opponent,
    /// Games won by the profile's side and by the opponent's.
    games_won: i32,
    games_lost: i32,
    #[serde(with = "time::serde::rfc3339")]
    started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    ended_at: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct Opponent {
    mii: String,
    captain: String,
    /// The profile whose game reported the other side; none when no game did.
    profile_id: Option<i32>,
}

/// The profile `profile_id`, if there is one.
pub async fn detail(
    pool: &PgPool,
    gpcm: &Gpcm,
    profile_id: i32,
) -> Result<Option<Detail>, sqlx::Error> {
    let online = gpcm.presence().get(&profile_id).map(Online::from);
    reports::settle_idle(pool).await?;
    let mut tx = snapshot(pool).await?;
    let Some(row) = sqlx::query!(
        r#"SELECT p.profile_id, p.user_id, p.gsbrcd, p.ingamesn, p.created_at, p.last_login_at,
                  p.last_ip, players.display_name AS "player?", discord.username AS "discord?"
           FROM profiles p
           LEFT JOIN player_wfc_profiles bound ON bound.profile_id = p.profile_id
           LEFT JOIN players ON players.player_id = bound.player_id
           LEFT JOIN player_identities discord
                  ON discord.player_id = players.player_id AND discord.provider = 'discord'
           WHERE p.profile_id = $1"#,
        profile_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        tx.commit().await?;
        return Ok(None);
    };
    let release: String = row.gsbrcd.chars().take(4).collect();
    let profile = Profile {
        profile_id: row.profile_id,
        user_id: row.user_id,
        friend_code: accounts::friend_code(row.profile_id, &release),
        release,
        gsbrcd: row.gsbrcd,
        mii_name: row.ingamesn,
        cpu_buddy: row.profile_id == accounts::CPU_BUDDY,
        created_at: row.created_at,
        last_login_at: row.last_login_at,
        last_ip: row.last_ip,
        online,
        player: row.player.map(|name| Bound {
            name,
            discord: row.discord,
        }),
    };
    let standing = standing(&mut tx, profile_id).await?;
    let (stats, matches) = record(&mut tx, profile_id).await?;
    tx.commit().await?;
    Ok(Some(Detail {
        profile,
        stats,
        standing,
        matches,
    }))
}

async fn standing(conn: &mut PgConnection, profile_id: i32) -> Result<Vec<Standing>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT category AS "category!", rank AS "rank!", entries AS "entries!",
                  score AS "score!", userdata AS "userdata!", updated_at AS "updated_at!"
           FROM (SELECT profile_id, category, score, userdata, updated_at,
                        RANK() OVER (PARTITION BY category ORDER BY score DESC) AS rank,
                        count(*) OVER (PARTITION BY category) AS entries
                 FROM scores) ranked
           WHERE profile_id = $1
           ORDER BY category"#,
        profile_id,
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let Submission { wins, losses, .. } = Submission::parse(&row.userdata);
            Standing {
                category: row.category,
                rank: row.rank,
                entries: row.entries,
                score: row.score,
                wins,
                losses,
                updated_at: row.updated_at,
            }
        })
        .collect())
}

/// The profile's record by mode, from every match it reported, and its
/// latest matches.
async fn record(
    conn: &mut PgConnection,
    profile_id: i32,
) -> Result<(Vec<ModeStats>, Vec<Played>), sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT m.match_id, m.mode, m.release, m.best_of, m.state, m.winner, m.home_mii,
                  m.away_mii, m.home_captain, m.away_captain, m.home_wins, m.away_wins,
                  m.started_at, m.ended_at, mine.side, other.profile_id AS "other_profile?"
           FROM online_match_sides mine
           JOIN online_matches m ON m.match_id = mine.match_id
           LEFT JOIN online_match_sides other
                  ON other.match_id = mine.match_id AND other.side <> mine.side
           WHERE mine.profile_id = $1
           ORDER BY mine.opened_at DESC"#,
        profile_id,
    )
    .fetch_all(&mut *conn)
    .await?;
    let ids: Vec<i64> = rows.iter().map(|row| row.match_id).collect();
    let reporters = reports::sides(conn, &ids).await?;

    let mut stats: Vec<ModeStats> = MODES
        .iter()
        .map(|&mode| ModeStats {
            mode,
            ..Default::default()
        })
        .collect();
    let mut captains: Vec<BTreeMap<String, u32>> = vec![BTreeMap::new(); MODES.len()];
    let mut played = Vec::new();
    for row in rows {
        let Some(side) = Side::parse(&row.side) else {
            continue;
        };
        let Some(mode) = MODES.iter().position(|mode| *mode == row.mode) else {
            continue;
        };
        let winner = row.winner.as_deref().and_then(Side::parse);
        let result = (row.state == "finished").then(|| match winner {
            Some(winner) if winner == side => "won",
            Some(_) => "lost",
            None => "drawn",
        });

        let series = &mut stats[mode].series;
        series.played += 1;
        match (row.state.as_str(), result) {
            (_, Some("won")) => series.won += 1,
            (_, Some("lost")) => series.lost += 1,
            (_, Some(_)) => series.drawn += 1,
            ("disputed", _) => series.disputed += 1,
            ("abandoned", _) => series.abandoned += 1,
            _ => series.in_progress += 1,
        }

        let sides = reporters.get(&row.match_id);
        let empty = BTreeMap::new();
        let views = |side: Side| {
            sides
                .and_then(|sides| sides.get(side).as_ref())
                .map_or(&empty, |reporter| &reporter.games)
        };
        let games = &mut stats[mode].games;
        for game in outcome::compare(views(Side::Home), views(Side::Away))
            .iter()
            .filter_map(Compared::counted)
        {
            let (mine, theirs) = (*game.score.get(side), *game.score.get(side.other()));
            games.played += 1;
            games.goals_for += mine as u32;
            games.goals_against += theirs as u32;
            match mine.cmp(&theirs) {
                std::cmp::Ordering::Greater => games.won += 1,
                std::cmp::Ordering::Less => games.lost += 1,
                std::cmp::Ordering::Equal => games.drawn += 1,
            }
            let captain = game.captains.get(side);
            if captain != UNKNOWN {
                *captains[mode].entry(captain.clone()).or_default() += 1;
            }
        }

        if played.len() < MATCHES_SHOWN as usize {
            let teams = Teams {
                home: (
                    row.home_mii.clone(),
                    row.home_captain.clone(),
                    row.home_wins,
                ),
                away: (
                    row.away_mii.clone(),
                    row.away_captain.clone(),
                    row.away_wins,
                ),
            };
            let (_, captain, games_won) = teams.get(side).clone();
            let (mii, opponent_captain, games_lost) = teams.get(side.other()).clone();
            played.push(Played {
                id: row.match_id,
                mode: row.mode,
                release: row.release,
                best_of: row.best_of,
                state: row.state,
                side,
                result,
                captain,
                opponent: Opponent {
                    mii,
                    captain: opponent_captain,
                    profile_id: row.other_profile,
                },
                games_won,
                games_lost,
                started_at: row.started_at,
                ended_at: row.ended_at,
            });
        }
    }
    for (stats, captains) in stats.iter_mut().zip(captains) {
        let mut used: Vec<CaptainUse> = captains
            .into_iter()
            .map(|(captain, games)| CaptainUse { captain, games })
            .collect();
        used.sort_by_key(|used| std::cmp::Reverse(used.games));
        stats.captains = used;
    }
    Ok((stats, played))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nas::Tickets;
    use std::sync::Arc;

    #[sqlx::test]
    async fn players_list_profiles_with_friend_codes_and_the_cpu_buddy_first(pool: PgPool) {
        accounts::log_in(&pool, 42).await.unwrap();
        let profile = accounts::profile_login(&pool, 42, "R4QEaaaaaaa", Some("Mario"), "10.0.0.1")
            .await
            .unwrap();
        let gpcm = Gpcm::new(pool.clone(), Arc::new(Tickets::default()), Arc::default());

        let players = read(&pool, &gpcm).await.unwrap();
        assert_eq!((players.totals.accounts, players.totals.profiles), (1, 1));
        assert_eq!(players.totals.online, 0);
        let [buddy, mario] = &players.profiles[..] else {
            panic!("two profiles");
        };
        assert!(buddy.cpu_buddy);
        assert_eq!(buddy.friend_code, "1770-9365-9135");
        assert_eq!(mario.profile_id, profile.profile_id);
        assert_eq!(mario.friend_code, "0868-9934-5920");
        assert_eq!(mario.mii_name.as_deref(), Some("Mario"));
        assert!(mario.online.is_none());
    }

    #[sqlx::test]
    async fn a_profile_shows_its_record_standing_and_matches(pool: PgPool) {
        use crate::admin::matches::tests::Games;

        let games = Games::new(pool).await;
        // Anosch wins a ranked series 2-1 against Sam, both reporting.
        let home = games.report(games.anosch, 100, "home", "start");
        let away = games.report(games.sam, 200, "away", "start");
        let won = games.send(&home).await;
        games.send(&away).await;
        for (number, mine, theirs) in [(1, 3, 1), (2, 0, 2), (3, 4, 2)] {
            games
                .send(&Games::game(home.clone(), number, mine, theirs))
                .await;
            games
                .send(&Games::game(away.clone(), number, mine, theirs))
                .await;
        }
        // Then a friend battle against a retail game, still going.
        let mut friendly = games.report(games.anosch, 300, "away", "start");
        friendly["mode"] = serde_json::json!("friends");
        let going = games.send(&friendly).await;
        sqlx::query!(
            "INSERT INTO scores (profile_id, category, region, score, userdata)
             VALUES ($1, 0, 2, 13, $2), (7, 0, 2, 20, $2), (8, 0, 2, 13, $2)",
            games.anosch,
            &vec![0u8; 128],
        )
        .execute(&games.pool)
        .await
        .unwrap();

        let gpcm = Gpcm::new(
            games.pool.clone(),
            Arc::new(Tickets::default()),
            Arc::default(),
        );
        let detail = detail(&games.pool, &gpcm, games.anosch)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.profile.mii_name.as_deref(), Some("Anosch"));
        assert_eq!(detail.profile.friend_code, "0868-9934-5920");
        assert_eq!(
            (detail.profile.user_id, detail.profile.release.as_str()),
            (1, "R4QE")
        );

        let [ranked, friends] = &detail.stats[..] else {
            panic!("two modes");
        };
        assert_eq!((ranked.mode, friends.mode), ("ranked", "friends"));
        let series = &ranked.series;
        assert_eq!(
            (series.played, series.won, series.lost, series.drawn),
            (1, 1, 0, 0)
        );
        let played = &ranked.games;
        assert_eq!((played.played, played.won, played.lost), (3, 2, 1));
        assert_eq!((played.goals_for, played.goals_against), (7, 5));
        assert_eq!(ranked.captains[0].captain, "mario");
        assert_eq!(ranked.captains[0].games, 3);
        assert_eq!((friends.series.played, friends.series.in_progress), (1, 1));

        // 13 points: second with Profile 8, behind 20.
        let [season] = &detail.standing[..] else {
            panic!("one board");
        };
        assert_eq!((season.rank, season.entries, season.score), (2, 3, 13));

        let ids: Vec<_> = detail.matches.iter().map(|played| played.id).collect();
        assert_eq!(ids, [going, won]);
        let last = &detail.matches[1];
        assert_eq!(
            (last.result, last.games_won, last.games_lost),
            (Some("won"), 2, 1)
        );
        assert_eq!(
            (last.opponent.mii.as_str(), last.opponent.profile_id),
            ("Sam", Some(games.sam))
        );
        let first = &detail.matches[0];
        assert_eq!((first.side, first.result), (Side::Away, None));
        assert_eq!(
            (first.opponent.mii.as_str(), first.opponent.profile_id),
            ("Sam", None)
        );

        // Sam lost the same series.
        let sam = detail_of(&games, games.sam).await;
        assert_eq!(sam.stats[0].series.lost, 1);
        assert_eq!(sam.stats[0].games.goals_for, 5);
        assert!(
            super::detail(&games.pool, &gpcm, 1_234_567_890)
                .await
                .unwrap()
                .is_none()
        );
    }

    async fn detail_of(games: &crate::admin::matches::tests::Games, profile_id: i32) -> Detail {
        let gpcm = Gpcm::new(
            games.pool.clone(),
            Arc::new(Tickets::default()),
            Arc::default(),
        );
        detail(&games.pool, &gpcm, profile_id)
            .await
            .unwrap()
            .unwrap()
    }
}

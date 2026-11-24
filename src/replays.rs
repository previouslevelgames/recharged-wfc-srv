//! Replays of online games (docs/reports.md, "Replays"). `REPLAYS` says
//! which games the server keeps: all of them (for tests), the ranked ones
//! that ended normally, or those of them with a player in the first
//! `REPLAY_TOP` places of the season's board, this season or the last one
//! that ended. For such a game the server answers a side's report of it with
//! `"replay": true`, if the game said it can send one (Recharged 1.2; older
//! games are never asked, and their answers do not change). The game then
//! sends the game's replay file (`POST /api/v1/wfc/replays`), and the server
//! keeps the first copy of each game in `REPLAY_DIR`, by year and month, with
//! a row in `replays`.
//!
//! Nothing of this runs during a match: the reports come after a game, the
//! top players are worked out once a minute in the background
//! ([`Replays::run`]), and a file's hash and writes run on Tokio's blocking
//! threads, not on those that serve GPCM, NAT negotiation and the relay.

use std::{
    collections::HashMap,
    env, fs, io,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use axum::body::Bytes;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::OffsetDateTime;
use tracing::{debug, info, warn};

use crate::reports::report::Side;

/// The variables replays read.
pub const VARIABLES: [&str; 3] = ["REPLAYS", "REPLAY_TOP", "REPLAY_DIR"];
/// Where replays go without `REPLAY_DIR`: the container's path, which the
/// install guide maps to a folder on the host.
pub const DEFAULT_DIR: &str = "/var/lib/recharged-wfc-srv/replays";
/// A replay's size at most. A 3-minute game of two players is tens of KB.
pub const MAX_BYTES: usize = 1024 * 1024;
/// How often the top players are worked out again.
const REFRESH: Duration = Duration::from_secs(60);
/// The places counted at most: a typo should not make every game wanted.
const MOST_PLACES: i64 = 1000;
/// The places `top` counts without `REPLAY_TOP`.
const DEFAULT_PLACES: i64 = 10;

/// Which games' replays the server keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Games {
    /// Every online game, ranked or with friends, however it ended: for
    /// tests.
    All,
    /// The ranked games that ended normally.
    Ranked,
    /// The ranked games that ended normally with a player in the first so
    /// many places of the season's board, this season or the last.
    Top(i64),
}

/// Replays' settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub games: Games,
    /// The folder the files go to.
    pub dir: PathBuf,
}

impl Config {
    /// Reads [`VARIABLES`]. `Ok(None)` when replays are off: `REPLAYS` is
    /// unset or `off`.
    pub fn from_env() -> Result<Option<Self>, String> {
        Self::from_vars(|name| env::var(name).ok())
    }

    fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let get = |name: &str| {
            var(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let places = match get("REPLAY_TOP") {
            None => DEFAULT_PLACES,
            Some(places) => places
                .parse()
                .ok()
                .filter(|places| (1..=MOST_PLACES).contains(places))
                .ok_or_else(|| {
                    format!(
                        "REPLAY_TOP: {places} is not a number of places from 1 to {MOST_PLACES}"
                    )
                })?,
        };
        let games = match get("REPLAYS").map(|value| value.to_lowercase()).as_deref() {
            None | Some("off") => return Ok(None),
            Some("all") => Games::All,
            Some("ranked") => Games::Ranked,
            Some("top") => Games::Top(places),
            Some(value) => {
                return Err(format!("REPLAYS: {value} is not all, ranked, top or off"));
            }
        };
        let dir = get("REPLAY_DIR").unwrap_or_else(|| DEFAULT_DIR.to_owned());
        Ok(Some(Self {
            games,
            dir: PathBuf::from(dir),
        }))
    }

    /// What the server keeps, for its log: "every online game", ...
    pub fn describe(&self) -> String {
        match self.games {
            Games::All => "every online game".into(),
            Games::Ranked => "every ranked game".into(),
            Games::Top(places) => {
                format!("ranked games with a player in the first {places} places")
            }
        }
    }

    /// Makes the folder if needed and checks the server can write to it.
    pub fn prepare(&self) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let probe = self.dir.join(".write-check");
        fs::write(&probe, b"")?;
        fs::remove_file(probe)
    }
}

/// The settings and the top players' WFC profiles with their places.
pub(crate) struct Replays {
    config: Config,
    leaders: RwLock<Arc<HashMap<i32, i64>>>,
}

/// What became of a replay a game sent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    /// The first copy: kept as `file` under the folder.
    First { match_id: i64, file: String },
    /// A copy was kept before, the other side's or this one's.
    Before { match_id: i64 },
    /// The profile reported no such series.
    NoSeries,
    /// The server did not ask for this game's replay.
    NotAsked { match_id: i64 },
}

impl Replays {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            leaders: RwLock::default(),
        }
    }

    /// Works the top players out once a minute, for good; without `top`,
    /// waits for good.
    pub async fn run(&self, pool: PgPool) -> io::Result<()> {
        if !matches!(self.config.games, Games::Top(_)) {
            return std::future::pending().await;
        }
        loop {
            match self.refresh(&pool).await {
                Ok(profiles) => debug!(target: "replays", profiles, "top players worked out"),
                Err(error) => warn!(target: "replays", %error, "top players not worked out"),
            }
            tokio::time::sleep(REFRESH).await;
        }
    }

    /// Works out the top players now; how many WFC profiles they have.
    pub async fn refresh(&self, pool: &PgPool) -> Result<usize, sqlx::Error> {
        let Games::Top(places) = self.config.games else {
            return Ok(0);
        };
        let mut conn = pool.acquire().await?;
        let today = OffsetDateTime::now_utc().date();
        let leaders = crate::admin::season_leaders(&mut conn, today, places).await?;
        let profiles = leaders.len();
        *self.leaders.write().unwrap() = Arc::new(leaders);
        Ok(profiles)
    }

    fn leaders(&self) -> Arc<HashMap<i32, i64>> {
        self.leaders.read().unwrap().clone()
    }

    /// Whether the server wants the replay of `game` of `match_id` from the
    /// side that reported it, a game of a `ranked` series or not that ended
    /// `normally` or not: when [`Games`] takes it, until a copy came. The
    /// first side's report decides and the row keeps the answer, so the
    /// other side's (or the same report sent again) gets the same.
    pub async fn wanted(
        &self,
        pool: &PgPool,
        match_id: i64,
        game: i32,
        ranked: bool,
        normally: bool,
    ) -> Result<bool, sqlx::Error> {
        let taken = matches!(self.config.games, Games::All) || (ranked && normally);
        if !taken {
            return Ok(false);
        }
        if let Some(kept) = sqlx::query_scalar!(
            r#"SELECT file IS NOT NULL AS "kept!" FROM replays WHERE match_id = $1 AND game = $2"#,
            match_id,
            game,
        )
        .fetch_optional(pool)
        .await?
        {
            return Ok(!kept);
        }
        let (home, away) = match self.config.games {
            Games::All | Games::Ranked => (None, None),
            Games::Top(_) => match self.places(pool, match_id).await? {
                (None, None) => return Ok(false),
                places => places,
            },
        };
        let asked = sqlx::query!(
            "INSERT INTO replays (match_id, game, home_place, away_place) VALUES ($1, $2, $3, $4)
             ON CONFLICT DO NOTHING",
            match_id,
            game,
            home,
            away,
        )
        .execute(pool)
        .await?
        .rows_affected()
            == 1;
        if asked {
            info!(target: "replays", match_id, game, ?home, ?away, "replay asked");
        }
        Ok(true)
    }

    /// Whether the games of the series a start report opened stream as they
    /// are played: the same rules as their replays', at its start.
    pub async fn streamed(
        &self,
        pool: &PgPool,
        match_id: i64,
        ranked: bool,
    ) -> Result<bool, sqlx::Error> {
        Ok(match self.config.games {
            Games::All => true,
            Games::Ranked => ranked,
            Games::Top(_) => ranked && self.places(pool, match_id).await? != (None, None),
        })
    }

    /// The match of `profile_id`'s `series`, when its games stream.
    pub async fn streamed_match(
        &self,
        pool: &PgPool,
        profile_id: i32,
        series: i64,
    ) -> Result<Option<i64>, sqlx::Error> {
        let Some(row) = sqlx::query!(
            "SELECT s.match_id, m.mode FROM online_match_sides s JOIN online_matches m USING (match_id)
             WHERE s.profile_id = $1 AND s.series = $2",
            profile_id,
            series,
        )
        .fetch_optional(pool)
        .await?
        else {
            return Ok(None);
        };
        let streamed = self
            .streamed(pool, row.match_id, row.mode == "ranked")
            .await?;
        Ok(streamed.then_some(row.match_id))
    }

    /// Each side's player's place among the top players, if they are one.
    async fn places(
        &self,
        pool: &PgPool,
        match_id: i64,
    ) -> Result<(Option<i32>, Option<i32>), sqlx::Error> {
        let leaders = self.leaders();
        if leaders.is_empty() {
            return Ok((None, None));
        }
        let sides = sqlx::query!(
            "SELECT side, profile_id FROM online_match_sides WHERE match_id = $1",
            match_id
        )
        .fetch_all(pool)
        .await?;
        let place = |side: Side| {
            sides
                .iter()
                .find(|row| row.side == side.as_str())
                .and_then(|row| leaders.get(&row.profile_id))
                .map(|&place| place as i32)
        };
        Ok((place(Side::Home), place(Side::Away)))
    }

    /// Keeps `body`, the replay of `game` of `profile_id`'s `series`, if the
    /// server asked for it and no copy came before. The file is written whole
    /// under a hidden name first; the first side to claim the row renames it
    /// to `YYYY/MM/<match>-<game>.msc`, the other's goes.
    pub async fn keep(
        &self,
        pool: &PgPool,
        profile_id: i32,
        series: i64,
        game: i32,
        header: &Header,
        body: Bytes,
    ) -> Result<Kept, KeepError> {
        let Some(row) = sqlx::query!(
            "SELECT match_id, side FROM online_match_sides WHERE profile_id = $1 AND series = $2",
            profile_id,
            series,
        )
        .fetch_optional(pool)
        .await?
        else {
            return Ok(Kept::NoSeries);
        };
        let (match_id, side) = (row.match_id, row.side);
        let Some(asked) = sqlx::query!(
            "SELECT file FROM replays WHERE match_id = $1 AND game = $2",
            match_id,
            game,
        )
        .fetch_optional(pool)
        .await?
        else {
            return Ok(Kept::NotAsked { match_id });
        };
        if asked.file.is_some() {
            self.other_side_too(pool, match_id, game, &side).await?;
            return Ok(Kept::Before { match_id });
        }

        let now = OffsetDateTime::now_utc();
        let folder = format!("{:04}/{:02}", now.year(), u8::from(now.month()));
        let file = format!("{folder}/{match_id}-{game}.msc");
        let part = format!("{folder}/.{match_id}-{game}-{side}.part");
        let dir = self.config.dir.clone();
        let (written, sha256) = {
            let (dir, folder, part) = (dir.clone(), folder.clone(), part.clone());
            tokio::task::spawn_blocking(move || {
                let sha256 = Sha256::digest(&body).to_vec();
                write_whole(&dir.join(folder), &dir.join(part), &body)
                    .map(|()| (body.len(), sha256))
            })
            .await
            .map_err(|error| KeepError::Io(io::Error::other(error)))??
        };

        let claimed = sqlx::query!(
            "UPDATE replays
             SET side = $3, file = $4, bytes = $5, sha256 = $6, steps = $7, received_at = now()
             WHERE match_id = $1 AND game = $2 AND file IS NULL",
            match_id,
            game,
            side.as_str(),
            file,
            written as i32,
            sha256,
            header.steps.and_then(|steps| i32::try_from(steps).ok()),
        )
        .execute(pool)
        .await;
        let claimed = match claimed {
            Ok(done) => done.rows_affected() == 1,
            Err(error) => {
                remove_quietly(dir.join(&part)).await;
                return Err(error.into());
            }
        };
        if !claimed {
            remove_quietly(dir.join(&part)).await;
            self.other_side_too(pool, match_id, game, &side).await?;
            return Ok(Kept::Before { match_id });
        }
        let renamed = {
            let (from, to) = (dir.join(&part), dir.join(&file));
            tokio::task::spawn_blocking(move || fs::rename(from, to))
                .await
                .map_err(io::Error::other)
                .and_then(|result| result)
        };
        if let Err(error) = renamed {
            remove_quietly(dir.join(&part)).await;
            sqlx::query!(
                "UPDATE replays
                 SET side = NULL, file = NULL, bytes = NULL, sha256 = NULL, steps = NULL, received_at = NULL
                 WHERE match_id = $1 AND game = $2 AND file = $3",
                match_id,
                game,
                file,
            )
            .execute(pool)
            .await?;
            return Err(error.into());
        }
        info!(target: "replays", match_id, game, side = %side, bytes = written, steps = ?header.steps, build = %header.build, file = %file, "replay kept");
        Ok(Kept::First { match_id, file })
    }

    /// Notes that the side that is not the kept copy's sent one too.
    async fn other_side_too(
        &self,
        pool: &PgPool,
        match_id: i64,
        game: i32,
        side: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE replays SET both_sides = true WHERE match_id = $1 AND game = $2 AND side <> $3",
            match_id,
            game,
            side,
        )
        .execute(pool)
        .await?;
        debug!(target: "replays", match_id, game, side, "replay came before");
        Ok(())
    }
}

/// Why a replay could not be kept: the server's own trouble, which the game
/// tries again later.
#[derive(Debug)]
pub(crate) enum KeepError {
    Database(sqlx::Error),
    Io(io::Error),
}

impl From<sqlx::Error> for KeepError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<io::Error> for KeepError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for KeepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(f, "database: {error}"),
            Self::Io(error) => write!(f, "file: {error}"),
        }
    }
}

/// Writes `bytes` to `path` in `folder` (made if needed), flushed to the disk.
fn write_whole(folder: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(folder)?;
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_data()
}

async fn remove_quietly(path: PathBuf) {
    let _ = tokio::task::spawn_blocking(move || fs::remove_file(path)).await;
}

/// What the server reads of a replay file (Recharged's
/// `src/platform/match_replay_file.h`): little-endian, `MSCRPLY` 0x1A, the
/// file version and the header's size, the header (the build's length and
/// text, the 6-character game ID, the revision, the creation time, the
/// stream's size and its compressed size, then the match's summary, which
/// begins with its steps), then the compressed stream. The stream itself is
/// not read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub version: u16,
    pub build: String,
    pub game_id: String,
    /// The game steps it holds, from the summary, when the header has one.
    pub steps: Option<u32>,
}

const MAGIC: &[u8; 8] = b"MSCRPLY\x1a";
/// Magic, version and header size.
const FIXED: usize = 12;
/// The header without the build's text and the summary.
const BASE: usize = 1 + 6 + 1 + 8 + 4 + 4;

impl Header {
    /// The header of `body`, a whole replay file, or what is wrong with it.
    pub fn parse(body: &[u8]) -> Result<Self, &'static str> {
        if body.len() < FIXED || &body[..8] != MAGIC {
            return Err("not a replay");
        }
        let word = |at: usize, bytes: usize| {
            body[at..at + bytes]
                .iter()
                .rev()
                .fold(0u64, |value, &byte| value << 8 | u64::from(byte))
        };
        let version = word(8, 2) as u16;
        let size = word(10, 2) as usize;
        if size < BASE || body.len() < FIXED + size {
            return Err("replay header cut short");
        }
        let build_len = usize::from(body[FIXED]);
        if BASE + build_len > size {
            return Err("replay header damaged");
        }
        let at = FIXED + 1 + build_len;
        let build = String::from_utf8_lossy(&body[FIXED + 1..at]).into_owned();
        let game_id = String::from_utf8_lossy(&body[at..at + 6])
            .trim_end()
            .to_owned();
        let packed = word(at + 19, 4) as usize;
        if body.len() - (FIXED + size) != packed {
            return Err("replay cut short");
        }
        // The summary follows in file version 1; a newer version may differ.
        let steps = (version == 1 && BASE + build_len + 4 <= size).then(|| word(at + 23, 4) as u32);
        Ok(Self {
            version,
            build,
            game_id,
            steps,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A replay file as Recharged writes it, with `packed` as its stream.
    pub(crate) fn replay(build: &str, steps: u32, packed: &[u8]) -> Vec<u8> {
        let mut header = vec![build.len() as u8];
        header.extend_from_slice(build.as_bytes());
        header.extend_from_slice(b"R4QE01");
        header.push(1);
        header.extend_from_slice(&1_795_000_000i64.to_le_bytes());
        header.extend_from_slice(&(packed.len() as u32 * 4).to_le_bytes());
        header.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        header.extend_from_slice(&steps.to_le_bytes());
        header.extend_from_slice(&[1, 0, 2, 0, 16, 0, 3, 0, 1, 0, 0]);
        let mut file = MAGIC.to_vec();
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&(header.len() as u16).to_le_bytes());
        file.extend_from_slice(&header);
        file.extend_from_slice(packed);
        file
    }

    #[test]
    fn a_replay_header_reads() {
        let file = replay("1.2.0 (abc1234)", 9000, &[7; 100]);
        assert_eq!(
            Header::parse(&file),
            Ok(Header {
                version: 1,
                build: "1.2.0 (abc1234)".into(),
                game_id: "R4QE01".into(),
                steps: Some(9000),
            })
        );
    }

    #[test]
    fn a_damaged_replay_is_refused() {
        let file = replay("1.2.0", 9000, &[7; 100]);
        assert_eq!(Header::parse(b"MSCRPLY"), Err("not a replay"));
        assert_eq!(
            Header::parse(b"RCDS\x02\0\0\0\0\0\0\0"),
            Err("not a replay")
        );
        assert_eq!(Header::parse(&file[..20]), Err("replay header cut short"));
        assert_eq!(
            Header::parse(&file[..file.len() - 1]),
            Err("replay cut short")
        );
        let mut longer = file.clone();
        longer.push(0);
        assert_eq!(Header::parse(&longer), Err("replay cut short"));
        let mut build = file.clone();
        build[FIXED] = 255;
        assert_eq!(Header::parse(&build), Err("replay header damaged"));
    }

    #[test]
    fn a_newer_file_version_keeps_its_steps_unread() {
        let mut file = replay("1.3.0", 9000, &[7; 10]);
        file[8] = 2;
        assert_eq!(Header::parse(&file).unwrap().steps, None);
    }

    #[test]
    fn settings_read() {
        let vars = |pairs: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> = pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
            }
        };
        let config = |games| {
            Ok(Some(Config {
                games,
                dir: PathBuf::from(DEFAULT_DIR),
            }))
        };
        assert_eq!(Config::from_vars(vars(&[])), Ok(None));
        assert_eq!(Config::from_vars(vars(&[("REPLAYS", "off")])), Ok(None));
        assert_eq!(Config::from_vars(vars(&[("REPLAY_TOP", "10")])), Ok(None));
        assert_eq!(
            Config::from_vars(vars(&[("REPLAYS", " All ")])),
            config(Games::All)
        );
        assert_eq!(
            Config::from_vars(vars(&[("REPLAYS", "ranked")])),
            config(Games::Ranked)
        );
        assert_eq!(
            Config::from_vars(vars(&[("REPLAYS", "top")])),
            config(Games::Top(10))
        );
        assert_eq!(
            Config::from_vars(vars(&[
                ("REPLAYS", "top"),
                ("REPLAY_TOP", "25"),
                ("REPLAY_DIR", "/data/replays")
            ])),
            Ok(Some(Config {
                games: Games::Top(25),
                dir: PathBuf::from("/data/replays")
            }))
        );
        assert!(Config::from_vars(vars(&[("REPLAYS", "some")])).is_err());
        for places in ["ten", "0", "-1", "100000"] {
            assert!(
                Config::from_vars(vars(&[("REPLAYS", "top"), ("REPLAY_TOP", places)])).is_err()
            );
        }
    }
}

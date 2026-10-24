//! The Logs page: the daily files the server writes to `LOG_DIR`, for admins
//! who cannot reach its disk.

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize, Serializer};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tracing::Level;

/// How much of a file's end the page reads: most days whole.
const SCAN_BYTES: u64 = 8 << 20;
/// The lines the page shows at most.
const LIMIT: usize = 500;

#[derive(Serialize)]
pub struct Logs {
    /// `LOG_DIR`, or `None` when the server logs to stderr only.
    dir: Option<String>,
    /// Newest first.
    files: Vec<LogFile>,
    /// The day of the file shown.
    day: Option<String>,
    /// Its last lines that pass the filter, newest first.
    lines: Vec<Line>,
    /// How many lines passed, of those read.
    matched: usize,
    scanned_bytes: u64,
    /// Whether the file's start was left out, past [`SCAN_BYTES`].
    partial: bool,
}

#[derive(Serialize)]
struct LogFile {
    /// The day (UTC) it covers, e.g. `2026-10-24`.
    day: String,
    bytes: u64,
    #[serde(with = "time::serde::rfc3339")]
    modified: OffsetDateTime,
}

/// An event, as the server wrote it.
#[derive(Debug, PartialEq, Serialize)]
struct Line {
    #[serde(with = "time::serde::rfc3339")]
    time: OffsetDateTime,
    #[serde(serialize_with = "level_name")]
    level: Level,
    /// `None` when spans come first, e.g. `request{uri=/}: tower_http…`:
    /// they stay in `text`.
    target: Option<String>,
    /// The message and its fields, with the event's further lines.
    text: String,
}

fn level_name<S: Serializer>(level: &Level, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(level.as_str())
}

/// What the page asks for.
#[derive(Deserialize)]
pub struct Filter {
    /// A file's day; the newest file without one.
    day: Option<String>,
    /// The least severe level shown, e.g. `warn`.
    level: Option<String>,
    /// Text the lines contain, in any case.
    q: Option<String>,
}

impl Filter {
    /// Whether a line is as severe as `level` or more, and contains `q`.
    fn admits(&self) -> impl Fn(&Line) -> bool {
        let least: Option<Level> = self.level.as_deref().and_then(|level| level.parse().ok());
        let text = self
            .q
            .as_deref()
            .map(|q| q.trim().to_lowercase())
            .filter(|q| !q.is_empty());
        move |line| {
            // ERROR is the least of tracing's levels.
            least.is_none_or(|least| line.level <= least)
                && text.as_deref().is_none_or(|text| {
                    line.text.to_lowercase().contains(text)
                        || line
                            .target
                            .as_deref()
                            .is_some_and(|target| target.to_lowercase().contains(text))
                })
        }
    }
}

pub async fn read(dir: Option<PathBuf>, filter: Filter) -> io::Result<Logs> {
    tokio::task::spawn_blocking(move || read_dir(dir.as_deref(), &filter))
        .await
        .map_err(io::Error::other)?
}

/// A day's whole file and its name, or `None` without one.
pub async fn file(dir: Option<PathBuf>, day: String) -> io::Result<Option<(String, Vec<u8>)>> {
    let (Some(dir), true) = (dir, is_day(&day)) else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || {
        let name = file_name(&day);
        match fs::read(dir.join(&name)) {
            Ok(bytes) => Ok(Some((name, bytes))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    })
    .await
    .map_err(io::Error::other)?
}

fn read_dir(dir: Option<&Path>, filter: &Filter) -> io::Result<Logs> {
    let mut logs = Logs {
        dir: dir.map(|dir| dir.display().to_string()),
        files: Vec::new(),
        day: None,
        lines: Vec::new(),
        matched: 0,
        scanned_bytes: 0,
        partial: false,
    };
    let Some(dir) = dir else {
        return Ok(logs);
    };
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Some(day) = entry
            .file_name()
            .to_str()
            .and_then(day_of)
            .map(str::to_owned)
        else {
            continue;
        };
        // Gone since the listing: the oldest file, deleted as a day began.
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_file() {
            logs.files.push(LogFile {
                day,
                bytes: metadata.len(),
                modified: metadata.modified()?.into(),
            });
        }
    }
    logs.files.sort_by(|a, b| b.day.cmp(&a.day));

    let Some(shown) = logs
        .files
        .iter()
        .find(|file| filter.day.as_ref() == Some(&file.day))
        .or(logs.files.first())
    else {
        return Ok(logs);
    };
    let (text, start, end) = tail(&dir.join(file_name(&shown.day)))?;
    let admits = filter.admits();
    for line in lines(&text).into_iter().rev().filter(admits) {
        logs.matched += 1;
        if logs.lines.len() < LIMIT {
            logs.lines.push(line);
        }
    }
    logs.day = Some(shown.day.clone());
    logs.scanned_bytes = end - start;
    logs.partial = start > 0;
    Ok(logs)
}

/// The file's whole lines in its last [`SCAN_BYTES`], and where those bytes
/// start and end in it.
fn tail(path: &Path) -> io::Result<(String, u64, u64)> {
    let mut file = File::open(path)?;
    let start = file.metadata()?.len().saturating_sub(SCAN_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(SCAN_BYTES).read_to_end(&mut bytes)?;
    let newline = |index: usize| index + 1;
    // A line still being written waits for the next read.
    let last = bytes
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, newline);
    let first = match start {
        0 => 0,
        _ => bytes
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(last, newline),
    };
    let text = String::from_utf8_lossy(&bytes[first.min(last)..last]).into_owned();
    Ok((text, start, start + bytes.len() as u64))
}

/// The events in `text`. A line that does not begin one belongs to the event
/// before; any before the first event belong to one that began earlier.
fn lines(text: &str) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    for line in text.lines() {
        match Line::parse(line) {
            Some(event) => lines.push(event),
            None => {
                if let Some(event) = lines.last_mut() {
                    event.text.push('\n');
                    event.text.push_str(line);
                }
            }
        }
    }
    lines
}

impl Line {
    /// `2026-10-24T09:12:33.123456Z  INFO gpcm: logged in profile_id=1`, as
    /// `tracing_subscriber`'s default format writes it without colors.
    fn parse(line: &str) -> Option<Self> {
        let (time, rest) = line.split_once(' ')?;
        let time = OffsetDateTime::parse(time, &Rfc3339).ok()?;
        let (level, rest) = rest.trim_start().split_once(' ')?;
        let level = level.parse().ok()?;
        let (target, text) = match rest.split_once(": ") {
            Some((target, text))
                if !target.is_empty()
                    && !target.contains(|c: char| c.is_whitespace() || c == '{') =>
            {
                (Some(target.to_owned()), text)
            }
            _ => (None, rest),
        };
        Some(Self {
            time,
            level,
            target,
            text: text.to_owned(),
        })
    }
}

/// `recharged-wfc-srv.2026-10-24.log`, as `tracing_appender` names the day's
/// file.
fn file_name(day: &str) -> String {
    format!(
        "{}.{day}.{}",
        crate::LOG_FILE_PREFIX,
        crate::LOG_FILE_SUFFIX
    )
}

/// The day in a daily file's name.
fn day_of(name: &str) -> Option<&str> {
    let day = name
        .strip_prefix(crate::LOG_FILE_PREFIX)?
        .strip_prefix('.')?
        .strip_suffix(crate::LOG_FILE_SUFFIX)?
        .strip_suffix('.')?;
    is_day(day).then_some(day)
}

/// Whether `day` is a date like `2026-10-24`, and nothing else: it names a
/// file.
fn is_day(day: &str) -> bool {
    day.len() == 10
        && day.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: &str = "\
continued from the day before
2026-10-24T09:00:00.000001Z  INFO recharged_wfc_srv: recharged-wfc-srv listening on 0.0.0.0:8080 (HTTP)
2026-10-24T09:01:00.000002Z  WARN admin: sign-in refused error=someone@example.com is not an admin
2026-10-24T09:02:00.000003Z ERROR gpcm: database error error=pool timed out
  caused by: connection refused
2026-10-24T09:03:00.000004Z DEBUG request{method=GET uri=/status}: tower_http::trace::on_request: started processing request
2026-10-24T09:04:00.000005Z  INFO gpcm: logged in profile_id=7
2026-10-24T09:05:00.0000";

    fn filter(level: Option<&str>, q: Option<&str>) -> Filter {
        Filter {
            day: None,
            level: level.map(str::to_owned),
            q: q.map(str::to_owned),
        }
    }

    /// A new folder `name` in the system's temporary one, with `files`.
    fn folder(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (file, text) in files {
            fs::write(dir.join(file), text).unwrap();
        }
        dir
    }

    #[test]
    fn reads_the_lines_the_server_writes() {
        let lines = lines(DAY);
        assert_eq!(lines.len(), 5, "{lines:#?}");
        assert_eq!(
            lines[0],
            Line {
                time: OffsetDateTime::parse("2026-10-24T09:00:00.000001Z", &Rfc3339).unwrap(),
                level: Level::INFO,
                target: Some("recharged_wfc_srv".into()),
                text: "recharged-wfc-srv listening on 0.0.0.0:8080 (HTTP)".into(),
            }
        );
        assert_eq!(lines[2].level, Level::ERROR);
        assert_eq!(
            lines[2].text,
            "database error error=pool timed out\n  caused by: connection refused"
        );
        assert_eq!(lines[3].target, None);
        assert!(
            lines[3]
                .text
                .starts_with("request{method=GET uri=/status}: tower_http")
        );
        assert_eq!(serde_json::to_value(&lines[1]).unwrap()["level"], "WARN");
    }

    /// The file layer of `main.rs`, writing to memory.
    #[test]
    fn reads_what_tracing_subscriber_writes() {
        use std::sync::{Arc, Mutex};

        use tracing_subscriber::{fmt, layer::SubscriberExt};

        #[derive(Clone, Default)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);

        impl io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().write(bytes)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::registry().with(
            fmt::layer()
                .with_writer(move || writer.clone())
                .with_ansi(false),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "gpcm", profile_id = 7, "logged in");
            tracing::info_span!("request", uri = "/status")
                .in_scope(|| tracing::error!(target: "admin", "database error"));
        });

        let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        let lines = lines(&text);
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(lines[0].level, Level::INFO);
        assert_eq!(lines[0].target.as_deref(), Some("gpcm"));
        assert_eq!(lines[0].text, "logged in profile_id=7");
        assert_eq!(lines[1].level, Level::ERROR);
        assert_eq!(lines[1].target, None);
        assert!(lines[1].text.ends_with("admin: database error"), "{text}");
    }

    #[test]
    fn filters_by_level_and_text() {
        let lines = lines(DAY);
        let shown = |filter: Filter| {
            let admits = filter.admits();
            lines.iter().filter(|&line| admits(line)).count()
        };
        assert_eq!(shown(filter(None, None)), 5);
        assert_eq!(shown(filter(Some("warn"), None)), 2);
        assert_eq!(shown(filter(Some("error"), None)), 1);
        assert_eq!(shown(filter(Some("nonsense"), None)), 5);
        // Targets count, in any case.
        assert_eq!(shown(filter(None, Some("GPCM"))), 2);
        assert_eq!(shown(filter(None, Some(" connection refused "))), 1);
        assert_eq!(shown(filter(Some("warn"), Some("profile_id"))), 0);
    }

    #[tokio::test]
    async fn shows_the_newest_file_unless_asked_for_another() {
        let dir = folder(
            "recharged-wfc-srv-logs",
            &[
                (
                    "recharged-wfc-srv.2026-10-23.log",
                    "2026-10-23T23:59:59.000000Z  INFO gpcm: logged out profile_id=7\n",
                ),
                ("recharged-wfc-srv.2026-10-24.log", DAY),
                ("recharged-wfc-srv.2026-10-24.log.old", DAY),
                ("notes.txt", DAY),
            ],
        );

        let logs = read(Some(dir.clone()), filter(None, None)).await.unwrap();
        let days: Vec<&str> = logs.files.iter().map(|file| file.day.as_str()).collect();
        assert_eq!(days, ["2026-10-24", "2026-10-23"]);
        assert_eq!(logs.day.as_deref(), Some("2026-10-24"));
        assert_eq!(logs.matched, 5);
        assert_eq!(
            logs.lines[0].text, "logged in profile_id=7",
            "newest first, without the line still being written"
        );
        assert!(!logs.partial);

        let earlier = Filter {
            day: Some("2026-10-23".into()),
            ..filter(None, Some("logged"))
        };
        let logs = read(Some(dir.clone()), earlier).await.unwrap();
        assert_eq!(logs.day.as_deref(), Some("2026-10-23"));
        assert_eq!(logs.matched, 1);

        let (name, bytes) = file(Some(dir.clone()), "2026-10-24".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(name, "recharged-wfc-srv.2026-10-24.log");
        assert_eq!(bytes, DAY.as_bytes());
        for day in ["2026-10-22", "../notes.txt", "2026-10-24.log.old"] {
            assert!(
                file(Some(dir.clone()), day.into()).await.unwrap().is_none(),
                "{day}"
            );
        }

        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn without_a_folder_there_are_no_files() {
        let logs = read(None, filter(None, None)).await.unwrap();
        assert!(logs.dir.is_none() && logs.files.is_empty() && logs.day.is_none());
        assert!(file(None, "2026-10-24".into()).await.unwrap().is_none());
    }

    #[test]
    fn only_the_end_of_a_long_file_is_read() {
        let line = "2026-10-24T09:00:00.000000Z  INFO gpcm: logged in profile_id=7\n";
        let long = line.repeat(SCAN_BYTES as usize / line.len() + 10);
        let dir = folder(
            "recharged-wfc-srv-long-log",
            &[("recharged-wfc-srv.2026-10-24.log", &long)],
        );

        let (text, start, end) = tail(&dir.join("recharged-wfc-srv.2026-10-24.log")).unwrap();
        assert_eq!(end, long.len() as u64);
        assert_eq!(end - start, SCAN_BYTES);
        // Whole lines only: the first, cut, is left out.
        assert!(text.lines().all(|text| Line::parse(text).is_some()));
        assert_eq!(text.lines().count(), SCAN_BYTES as usize / line.len());

        fs::remove_dir_all(dir).unwrap();
    }
}

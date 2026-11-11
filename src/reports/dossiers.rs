//! Desync dossiers: what a Recharged build's rollback knew at a network
//! match's first desync (its `src/platform/desync_dossier.h`), read and
//! checked byte by byte, and where the two sides' first differ.
//!
//! A dossier is binary, little-endian: a 160-byte header, then two steps (the
//! one before the desync's and the desync's: checksums go every other step,
//! so the machines may have diverged a step earlier), each three words and
//! its fields of 28 bytes. A field is one block of the step's checksum as the
//! game took it: where (an offset in the game module), its size, the FNV-1a
//! hash of all its bytes and its first 16. The first field that differs
//! between the two sides is where the match diverged.
//!
//! From version 2 (Recharged 1.1.7) the game as the side's rollback played it
//! follows ([`Netplay`]): its counts since the game began and the inputs of
//! the 32 steps through the desync's, once all had arrived. The two sides'
//! inputs should be the same: where they differ ([`diverge_inputs`]) the
//! network or the rollback lost one, and the checksums only showed it later.
//!
//! A caller is an offset in a game module: the same code has the same
//! callers only in the same binary (release, build and platform, the game
//! module's system and CPU, which the game names). Two
//! sides of different binaries are compared field by field, their callers
//! aside.

use super::report::{RELEASES, Side, Teams};
use sqlx::PgPool;

/// A dossier's size at most: two steps of [`MAX_FIELDS`] fields, then the
/// game and its inputs, every machine's.
pub const MAX_BYTES: usize = HEADER
    + STEPS * (STEP + MAX_FIELDS * FIELD)
    + MATCH
    + INPUT_STEPS * (INPUT_STEP + MACHINES * MACHINE_INPUT);
/// The bytes of a field kept as they are.
pub const FIELD_BYTES: usize = 16;
/// The steps whose inputs a version 2 dossier lists, the desync's the last.
pub const INPUT_STEPS: usize = 32;
/// Machines in a game at most.
pub const MACHINES: usize = 4;

const MAGIC: &[u8; 4] = b"RCDS";
/// Version 1 (1.1.6): the header and the fields; version 2 adds [`Netplay`].
const VERSIONS: std::ops::RangeInclusive<u32> = 1..=2;
const MATCH: usize = 88;
const INPUT_STEP: usize = 20;
const MACHINE_INPUT: usize = 216;
const RECORD: usize = 16;
const EVENT: usize = 0x24;
const HEADER: usize = 160;
const STEP: usize = 12;
const FIELD: usize = 12 + FIELD_BYTES;
const STEPS: usize = 2;
const MAX_FIELDS: usize = 512;
/// A series' games at most, as the match reports'.
const MAX_GAMES: u32 = 9;
const BUILD: usize = 64;
const PLATFORM: usize = 16;

/// One block of a step's checksum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// An offset in the game module of the dossier's build, release and
    /// system (for addr2line), or, from 0x8000_0000 up, one of the
    /// rollback's own kinds (a pose, a physics body, a collision...) by its
    /// top four bits.
    pub caller: u32,
    pub size: u32,
    pub hash: u32,
    /// The first ones, zero-padded.
    pub bytes: [u8; FIELD_BYTES],
}

/// A step's fields, in the order the game took them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub frame: u32,
    /// The step took more fields than the dossier lists.
    pub overflowed: bool,
    /// None: the step's were not kept.
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dossier {
    /// The series' number in the match reports (`online_match_sides.series`).
    pub series: i64,
    /// When the game wrote it, Unix seconds by its clock.
    pub written_at: i64,
    pub profile_id: i32,
    /// The game of the series, from 1.
    pub game: i32,
    /// As this side's game knew it.
    pub side: Side,
    /// The step whose checksums differ.
    pub frame: u32,
    /// This side's, the other's.
    pub checksum: [u32; 2],
    pub seed: [u32; 2],
    /// The other machine's index.
    pub machine: i32,
    /// Steps, at the desync.
    pub input_delay: u32,
    /// Inputs predicted wrong in the match, every machine's.
    pub mispredictions: u32,
    /// The rollback's diagnostic switches: bit 0 the poses, 1 the physics
    /// bodies, 2 the collisions.
    pub options: u32,
    /// One of [`RELEASES`].
    pub release: &'static str,
    pub build: String,
    /// The game module's system and CPU, as the game was built for them:
    /// `linux x86_64`, `windows x86_64` (Wine too), `macos arm64`... Empty
    /// when the game did not know it.
    pub platform: String,
    /// The step before the desync's, then the desync's.
    pub steps: [Step; STEPS],
    /// 1 or 2.
    pub version: u32,
    /// From version 2.
    pub netplay: Option<Netplay>,
}

/// Version 2: the game as the side's rollback played it, its counts since
/// the game began. A misprediction it could no longer correct
/// (`kept_predicted`, `impossible`, `stopped_predicted`) desyncs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Netplay {
    pub local_machine: i32,
    pub machines: u32,
    /// The step the rollback's tracking last started after.
    pub tracking_from: u32,
    /// Still tracking at the desync.
    pub tracking: bool,
    /// Steps: the input delay the game began with.
    pub start_delay: u32,
    /// Times this machine's input delay grew, and shrank.
    pub delay_longer: u32,
    pub delay_shorter: u32,
    /// The step of its last change, 0: none.
    pub delay_changed_at: u32,
    /// By machine: steps whose inputs came other than predicted.
    pub mispredicted: [u32; MACHINES],
    pub predicted_steps: u32,
    pub correction_rollbacks: u32,
    /// Predicted steps undone: they changed the phase.
    pub undone: u32,
    /// Replays cut at a phase change.
    pub truncated: u32,
    /// Predicted steps kept for a disc read.
    pub kept_predicted: u32,
    /// Rollbacks that could not go back.
    pub impossible: u32,
    /// Tracking stopped with inputs still predicted.
    pub stopped_predicted: u32,
    /// Steps that waited for inputs as the original router did, and with the
    /// prediction window full.
    pub lockstep_waits: u32,
    pub window_waits: u32,
    /// [`INPUT_STEPS`] steps, the oldest first, the desync's the last.
    pub inputs: Vec<InputStep>,
}

/// A step's inputs once all had arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputStep {
    pub frame: u32,
    /// False: gone from the rollback's ring, and nothing else set.
    pub kept: bool,
    /// One bit per machine: its inputs were predicted when the step first
    /// ran, and they came other than predicted.
    pub predicted: u8,
    pub mispredicted: u8,
    /// By team and player (the game's `cTeam::m_pPlayers`): the pad
    /// controlling it, machine * 4 + the machine's player; -1 none (the AI).
    pub pads: [[i8; 5]; 2],
    /// By machine.
    pub machines: Vec<MachineInput>,
}

/// A machine's inputs of a step, as its input message made them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MachineInput {
    pub players: u8,
    pub event_count: u8,
    pub remap: u16,
    /// Each player's connection state.
    pub states: [u8; 4],
    /// Each player's `PackedDetInput`.
    pub records: [[u8; RECORD]; 4],
    /// Its deterministic data events (`DetermDataEvent`).
    pub events: [[u8; EVENT]; 4],
}

/// The parts of a `PackedDetInput`, by its bytes (the game's
/// `rollback_netplay.cpp` `MispredictedParts`).
const RECORD_PARTS: [(&str, std::ops::Range<usize>); 7] = [
    ("buttons", 0..2),
    ("triggers", 2..3),
    ("remote accel", 3..6),
    ("nunchuk accel", 6..9),
    ("pointer targets", 9..10),
    ("pointer", 10..12),
    ("sticks", 12..16),
];

impl MachineInput {
    /// What counts of it: the players' records and states, the events.
    fn same(&self, other: &MachineInput) -> bool {
        self.differences(other).is_empty()
    }

    /// The parts that differ from `other`'s, by name.
    fn differences(&self, other: &MachineInput) -> Vec<String> {
        let mut parts = Vec::new();
        if self.players != other.players {
            parts.push("players".to_owned());
        }
        if self.remap != other.remap {
            parts.push("remap angle".to_owned());
        }
        for player in 0..usize::from(self.players.min(other.players)) {
            if self.states[player] != other.states[player] {
                parts.push(format!("player {player}'s state"));
            }
            for (name, range) in &RECORD_PARTS {
                if self.records[player][range.clone()] != other.records[player][range.clone()] {
                    parts.push(format!("player {player}'s {name}"));
                }
            }
        }
        let events = usize::from(self.event_count.min(other.event_count));
        if self.event_count != other.event_count || self.events[..events] != other.events[..events]
        {
            parts.push("events".to_owned());
        }
        parts
    }

    /// One line: each player's record, read part by part.
    fn describe(&self) -> String {
        let signed = |b: u8| b as i8;
        let mut text = format!("remap {:04x}", self.remap);
        for player in 0..usize::from(self.players.min(4)) {
            let r = &self.records[player];
            text += &format!(
                "; player {player} state {}: buttons {:02x}{:02x} triggers {:02x} accel {} {} {} \
                 nunchuk {} {} {} pointer {:02x} {:02x}{:02x} stick {} {} c-stick {} {}",
                self.states[player],
                r[1],
                r[0],
                r[2],
                signed(r[3]),
                signed(r[4]),
                signed(r[5]),
                signed(r[6]),
                signed(r[7]),
                signed(r[8]),
                r[9],
                r[11],
                r[10],
                signed(r[12]),
                signed(r[13]),
                signed(r[14]),
                signed(r[15]),
            );
        }
        for event in &self.events[..usize::from(self.event_count.min(4))] {
            let hex: String = event.iter().map(|b| format!("{b:02x}")).collect();
            text += &format!("; event {hex}");
        }
        text
    }
}

/// The little-endian words of a dossier, in order.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        if self.bytes.len() < count {
            return Err("dossier cut short".to_owned());
        }
        let (taken, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        Ok(self.take(N)?.try_into().expect("taken N bytes"))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    /// Version 2's game and inputs, of the steps through `frame`.
    fn netplay(&mut self, frame: u32) -> Result<Netplay, String> {
        let local_machine = self.i32()?;
        let machines = self.u32()?;
        if machines == 0 || machines as usize > MACHINES {
            return Err(format!("{machines} machines"));
        }
        if !(0..machines as i32).contains(&local_machine) {
            return Err(format!("local machine {local_machine}"));
        }
        let tracking_from = self.u32()?;
        let tracking = match self.u32()? {
            0 => false,
            1 => true,
            other => return Err(format!("tracking {other}")),
        };
        let mut words = [0u32; 18];
        for word in &mut words {
            *word = self.u32()?;
        }
        let [
            start_delay,
            delay_longer,
            delay_shorter,
            delay_changed_at,
            m0,
            m1,
            m2,
            m3,
            predicted_steps,
            correction_rollbacks,
            undone,
            truncated,
            kept_predicted,
            impossible,
            stopped_predicted,
            lockstep_waits,
            window_waits,
            input_steps,
        ] = words;
        if input_steps as usize != INPUT_STEPS {
            return Err(format!("{input_steps} input steps"));
        }
        let first = frame.wrapping_sub(INPUT_STEPS as u32 - 1);
        let inputs = (0..INPUT_STEPS as u32)
            .map(|i| self.input_step(first.wrapping_add(i), machines))
            .collect::<Result<_, String>>()?;
        Ok(Netplay {
            local_machine,
            machines,
            tracking_from,
            tracking,
            start_delay,
            delay_longer,
            delay_shorter,
            delay_changed_at,
            mispredicted: [m0, m1, m2, m3],
            predicted_steps,
            correction_rollbacks,
            undone,
            truncated,
            kept_predicted,
            impossible,
            stopped_predicted,
            lockstep_waits,
            window_waits,
            inputs,
        })
    }

    fn step(&mut self, expected: u32) -> Result<Step, String> {
        let frame = self.u32()?;
        if frame != expected {
            return Err(format!("step of frame {frame}, not {expected}"));
        }
        let count = self.u32()? as usize;
        if count > MAX_FIELDS {
            return Err(format!("{count} fields in a step"));
        }
        let overflowed = match self.u32()? {
            0 => false,
            1 => true,
            other => return Err(format!("overflowed {other}")),
        };
        let fields = (0..count)
            .map(|_| {
                Ok(Field {
                    caller: self.u32()?,
                    size: self.u32()?,
                    hash: self.u32()?,
                    bytes: self.array()?,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Step {
            frame,
            overflowed,
            fields,
        })
    }

    fn input_step(&mut self, expected: u32, machines: u32) -> Result<InputStep, String> {
        let frame = self.u32()?;
        if frame != expected {
            return Err(format!("input step of frame {frame}, not {expected}"));
        }
        let kept = match self.u8()? {
            0 => false,
            1 => true,
            other => return Err(format!("input step kept {other}")),
        };
        let count = self.u8()?;
        let predicted = self.u8()?;
        let mispredicted = self.u8()?;
        let mut pads = [[0i8; 5]; 2];
        for pad in pads.iter_mut().flatten() {
            *pad = self.u8()? as i8;
            if !(-1..16).contains(pad) {
                return Err(format!("pad {pad}"));
            }
        }
        if self.array::<2>()? != [0, 0] {
            return Err("input step reserved bytes set".to_owned());
        }
        if kept && u32::from(count) != machines {
            return Err(format!("{count} machines' inputs of {machines}"));
        }
        if !kept && (count != 0 || predicted != 0 || mispredicted != 0 || pads != [[-1; 5]; 2]) {
            return Err("an input step not kept has inputs".to_owned());
        }
        if (predicted | mispredicted) >> count != 0 {
            return Err("an input step flags a machine it does not list".to_owned());
        }
        let machines = (0..count)
            .map(|_| {
                let players = self.u8()?;
                let event_count = self.u8()?;
                if players > 4 || event_count > 4 {
                    return Err(format!("{players} players, {event_count} events"));
                }
                let remap = self.u16()?;
                let states = self.array()?;
                let mut records = [[0; RECORD]; 4];
                for record in &mut records {
                    *record = self.array()?;
                }
                let mut events = [[0; EVENT]; 4];
                for event in &mut events {
                    *event = self.array()?;
                }
                Ok(MachineInput {
                    players,
                    event_count,
                    remap,
                    states,
                    records,
                    events,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(InputStep {
            frame,
            kept,
            predicted,
            mispredicted,
            pads,
            machines,
        })
    }
}

/// A NUL-padded string: printable ASCII up to the first NUL, only NULs after.
fn text(bytes: &[u8], name: &str) -> Result<String, String> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let (text, padding) = bytes.split_at(end);
    if !text.iter().all(|b| (0x20..0x7F).contains(b)) {
        return Err(format!("{name} is not printable"));
    }
    if padding.iter().any(|&b| b != 0) {
        return Err(format!("{name} has bytes after its end"));
    }
    Ok(String::from_utf8(text.to_vec()).expect("printable ASCII"))
}

impl Dossier {
    pub fn parse(bytes: &[u8]) -> Result<Dossier, String> {
        if bytes.len() < HEADER {
            return Err("dossier cut short".to_owned());
        }
        if bytes.len() > MAX_BYTES {
            return Err(format!("dossier over {MAX_BYTES} bytes"));
        }
        let mut r = Reader { bytes };
        if &r.array::<4>()? != MAGIC {
            return Err("not a dossier".to_owned());
        }
        let version = r.u32()?;
        if !VERSIONS.contains(&version) {
            return Err(format!("dossier version {version}"));
        }
        let series = i64::try_from(r.u64()?).map_err(|_| "series out of range".to_owned())?;
        let written_at =
            i64::try_from(r.u64()?).map_err(|_| "written_at out of range".to_owned())?;
        let profile_id = match i32::try_from(r.u32()?) {
            Ok(id) if id > 0 => id,
            _ => return Err("profile_id out of range".to_owned()),
        };
        let game = r.u32()?;
        if !(1..=MAX_GAMES).contains(&game) {
            return Err(format!("game {game}"));
        }
        let side = match r.u32()? {
            0 => Side::Home,
            1 => Side::Away,
            side => return Err(format!("side {side}")),
        };
        let frame = r.u32()?;
        // The tables keep frames as INTEGER.
        if i32::try_from(frame).is_err() {
            return Err("frame out of range".to_owned());
        }
        let checksum = [r.u32()?, r.u32()?];
        let seed = [r.u32()?, r.u32()?];
        let machine = r.i32()?;
        let input_delay = r.u32()?;
        let mispredictions = r.u32()?;
        let options = r.u32()?;
        let release = r.array::<4>()?;
        let release = RELEASES
            .into_iter()
            .find(|name| name.as_bytes() == release)
            .ok_or_else(|| "unknown release".to_owned())?;
        let build = text(r.take(BUILD)?, "build")?;
        if build.is_empty() {
            return Err("no build".to_owned());
        }
        let platform = text(r.take(PLATFORM)?, "platform")?;
        if !platform.is_empty() && !is_platform(&platform) {
            return Err("platform is not a system and a CPU".to_owned());
        }
        let steps = r.u32()?;
        if steps as usize != STEPS {
            return Err(format!("{steps} steps"));
        }
        let before = r.step(frame.wrapping_sub(1))?;
        let desync = r.step(frame)?;
        let netplay = if version >= 2 {
            Some(r.netplay(frame)?)
        } else {
            None
        };
        if !r.bytes.is_empty() {
            return Err("bytes after the dossier".to_owned());
        }
        Ok(Dossier {
            series,
            written_at,
            profile_id,
            game: game as i32,
            side,
            frame,
            checksum,
            seed,
            machine,
            input_delay,
            mispredictions,
            options,
            release,
            build,
            platform,
            steps: [before, desync],
            version,
            netplay,
        })
    }

    /// The bytes the game sends for it.
    #[cfg(test)]
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let padded = |out: &mut Vec<u8>, text: &str, size: usize| {
            out.extend_from_slice(text.as_bytes());
            out.resize(out.len() + size - text.len(), 0);
        };
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&(self.series as u64).to_le_bytes());
        out.extend_from_slice(&(self.written_at as u64).to_le_bytes());
        let side = match self.side {
            Side::Home => 0,
            Side::Away => 1,
        };
        for word in [
            self.profile_id as u32,
            self.game as u32,
            side,
            self.frame,
            self.checksum[0],
            self.checksum[1],
            self.seed[0],
            self.seed[1],
            self.machine as u32,
            self.input_delay,
            self.mispredictions,
            self.options,
        ] {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(self.release.as_bytes());
        padded(&mut out, &self.build, BUILD);
        padded(&mut out, &self.platform, PLATFORM);
        out.extend_from_slice(&(STEPS as u32).to_le_bytes());
        for step in &self.steps {
            for word in [step.frame, step.fields.len() as u32, step.overflowed as u32] {
                out.extend_from_slice(&word.to_le_bytes());
            }
            for field in &step.fields {
                for word in [field.caller, field.size, field.hash] {
                    out.extend_from_slice(&word.to_le_bytes());
                }
                out.extend_from_slice(&field.bytes);
            }
        }
        if let Some(n) = &self.netplay {
            for word in [
                n.local_machine as u32,
                n.machines,
                n.tracking_from,
                n.tracking as u32,
                n.start_delay,
                n.delay_longer,
                n.delay_shorter,
                n.delay_changed_at,
            ]
            .into_iter()
            .chain(n.mispredicted)
            .chain([
                n.predicted_steps,
                n.correction_rollbacks,
                n.undone,
                n.truncated,
                n.kept_predicted,
                n.impossible,
                n.stopped_predicted,
                n.lockstep_waits,
                n.window_waits,
                n.inputs.len() as u32,
            ]) {
                out.extend_from_slice(&word.to_le_bytes());
            }
            for step in &n.inputs {
                out.extend_from_slice(&step.frame.to_le_bytes());
                out.extend([
                    step.kept as u8,
                    step.machines.len() as u8,
                    step.predicted,
                    step.mispredicted,
                ]);
                out.extend(step.pads.iter().flatten().map(|&pad| pad as u8));
                out.extend([0, 0]);
                for input in &step.machines {
                    out.extend([input.players, input.event_count]);
                    out.extend_from_slice(&input.remap.to_le_bytes());
                    out.extend_from_slice(&input.states);
                    out.extend(input.records.iter().flatten());
                    out.extend(input.events.iter().flatten());
                }
            }
        }
        out
    }
}

/// Whether `platform` is a lowercase system and CPU (`linux x86_64`).
fn is_platform(platform: &str) -> bool {
    let word = |word: &str| {
        !word.is_empty()
            && word
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    platform
        .split_once(' ')
        .is_some_and(|(system, cpu)| word(system) && word(cpu))
}

impl Dossier {
    /// Whether `other`'s callers are offsets in the same binary as this one's.
    pub fn same_binary(&self, other: &Dossier) -> bool {
        self.release == other.release
            && self.build == other.build
            && !self.platform.is_empty()
            && self.platform == other.platform
    }
}

/// The game module of a release (`recharged_game_us`...), without its
/// extension: `.so` on Linux and macOS, `.dll` on Windows.
pub fn module(release: &str) -> &'static str {
    match release {
        "R4QP" => "recharged_game_eu",
        "R4QJ" => "recharged_game_jp",
        "R4QK" => "recharged_game_kr",
        _ => "recharged_game_us",
    }
}

/// How to name the code at an offset in a platform's game module.
pub fn symbolize(release: &str, platform: Option<&str>) -> String {
    let module = module(release);
    match platform.and_then(|platform| platform.split(' ').next()) {
        Some("windows") => format!(
            "addr2line -f -C -e {module}.dll <ImageBase + offset> (objdump -p {module}.dll | grep ImageBase)"
        ),
        Some("macos") => format!("atos -o {module}.so -l 0 <offset>"),
        Some(_) => format!("addr2line -f -C -e {module}.so <offset>"),
        None => "the system is unknown: the offset's module cannot be told".to_owned(),
    }
}

/// The rollback's own kinds of fields, by a caller's top four bits
/// (`rollback_fields.cpp`'s `k*Field`); below them a caller is an offset.
const POSE: u32 = 0xF;
const BODY: u32 = 0xE;
const PAIR: u32 = 0xD;
const CONTACT: u32 = 0xC;
const PRINT: u32 = 0xB;
const VALUES: u32 = 0xA;
const CHAIN: u32 = 0x9;
const SET: u32 = 0x8;
/// A contact's words, four to a field.
const CONTACT_PARTS: [&str; 6] = [
    "mode mu mu2 bounce",
    "bounce_vel soft_erp soft_cfm motion1",
    "motion2 slip1 slip2 pos.x",
    "pos.y pos.z normal.x normal.y",
    "normal.z depth fdir.x fdir.y",
    "fdir.z",
];

impl Field {
    /// The field as the game's log has it (`rollback_fields.cpp`'s
    /// `LogFields`), its raw caller first.
    pub fn describe(&self) -> String {
        let caller = self.caller;
        let low = caller & 0x0FFF_FFFF;
        let float = |at: usize| f32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap());
        let floats = |count: usize| {
            (0..count.min(4))
                .map(|word| float(4 * word).to_string())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let words = (self.size / 4) as usize;
        let what = match caller >> 28 {
            POSE => format!(
                "pose team {} player {} node {}: translation {} {} {}",
                caller >> 12 & 0xF,
                caller >> 8 & 0xF,
                caller & 0xFF,
                float(0),
                float(4),
                float(8)
            ),
            BODY => format!(
                "body {low}: position {} {} {}",
                float(0),
                float(4),
                float(8)
            ),
            PAIR => {
                let int =
                    |at: usize| i32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap());
                format!(
                    "collision {low}: {} contacts, Contact() {} and {}",
                    int(0),
                    int(4),
                    int(8)
                )
            }
            CONTACT => {
                let part = (caller & 0xF) as usize;
                let mut text = format!(
                    "contact {} ({}):",
                    caller >> 4 & 0xFF_FFFF,
                    CONTACT_PARTS[part.min(5)]
                );
                for word in 0..words.min(4) {
                    if part == 0 && word == 0 {
                        let mode = u32::from_le_bytes(self.bytes[0..4].try_into().unwrap());
                        text += &format!(" {mode:08x}");
                    } else {
                        text += &format!(" {}", float(4 * word));
                    }
                }
                text
            }
            PRINT => {
                let shown: String = self
                    .bytes
                    .iter()
                    .take_while(|&&b| b != 0)
                    .map(|&b| match b {
                        b'\n' | b'\r' => ' ',
                        0x20..0x7F => b as char,
                        _ => '?',
                    })
                    .collect();
                let more = if self.size as usize > FIELD_BYTES {
                    "..."
                } else {
                    ""
                };
                format!("branch at +0x{low:x}: \"{shown}{more}\"")
            }
            VALUES => {
                let mut text = "values:".to_owned();
                for value in 0..if caller & 4 != 0 { 2 } else { 1 } {
                    let bytes: [u8; 8] = self.bytes[8 * value..8 * value + 8].try_into().unwrap();
                    if caller >> value & 1 != 0 {
                        let double = f64::from_le_bytes(bytes);
                        text += &format!(" {double} ({:016x})", u64::from_le_bytes(bytes));
                    } else {
                        text += &format!(" {}", i64::from_le_bytes(bytes));
                    }
                }
                text
            }
            CHAIN => format!(
                "ball chain {} part {}: {}",
                caller >> 4 & 0xF,
                caller & 0xF,
                floats(words)
            ),
            SET => {
                let coordinates = match u32::from_le_bytes(self.bytes[12..16].try_into().unwrap()) {
                    0 => "world",
                    _ => "the parent's",
                };
                format!(
                    "ball set at +0x{low:x} in {coordinates} coordinates: {} {} {}",
                    float(0),
                    float(4),
                    float(8)
                )
            }
            _ => {
                let shown = (self.size as usize).min(FIELD_BYTES);
                let hex: String = self.bytes[..shown]
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                let more = if self.size as usize > FIELD_BYTES {
                    "..."
                } else {
                    ""
                };
                format!("at +0x{caller:x}: {} bytes {hex}{more}", self.size)
            }
        };
        format!(
            "0x{caller:08x} {what}, size {}, hash {:08x}",
            self.size, self.hash
        )
    }
}

/// How the two sides' dossiers of a game differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The games took a field from different places: they ran different
    /// code. Only between sides of the same binary.
    Path,
    /// The same place, different bytes (or a different size); between
    /// different binaries, the same field by its place in the step.
    Value,
    /// The same fields, but one side took more.
    Count,
    /// Every field the same: the random seed, or state outside the checksum.
    None,
    /// A step one side did not keep (or kept only part of), or no step both
    /// kept.
    Missing,
}

impl Kind {
    /// What it tells, for a reader.
    pub fn meaning(self) -> &'static str {
        match self {
            Self::Path => "the games took the field from different places: they ran different code",
            Self::Value => "the same field, different bytes or size",
            Self::Count => "the same fields, but one game took more",
            Self::None => "every field the same: the random seed, or state outside the checksum",
            Self::Missing => "a step one game did not keep whole",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Value => "value",
            Self::Count => "count",
            Self::None => "none",
            Self::Missing => "missing",
        }
    }
}

/// Where the two sides first differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    pub kind: Kind,
    /// The step's; none for [`Kind::None`], and for a [`Kind::Missing`] with
    /// no step both kept.
    pub frame: Option<u32>,
    pub field_index: Option<usize>,
    /// Each side's field there; for [`Kind::Count`], only the longer side's.
    pub fields: Teams<Option<Field>>,
}

impl Divergence {
    fn found(
        kind: Kind,
        frame: u32,
        index: usize,
        home: Option<Field>,
        away: Option<Field>,
    ) -> Self {
        Self {
            kind,
            frame: Some(frame),
            field_index: Some(index),
            fields: Teams { home, away },
        }
    }

    fn unknown(kind: Kind, frame: Option<u32>) -> Self {
        Self {
            kind,
            frame,
            field_index: None,
            fields: Teams {
                home: None,
                away: None,
            },
        }
    }
}

/// The first field that differs between both sides' dossiers of a game, step
/// by step from the earlier one. A step one side did not keep whole hides
/// what it held: a difference in another step comes first, else it is
/// [`Kind::Missing`].
///
/// Between sides of different binaries, whose callers are offsets in
/// different modules, a field differs by its size, hash and bytes alone.
pub fn diverge(dossiers: &Teams<Dossier>) -> Divergence {
    let same_binary = dossiers.home.same_binary(&dossiers.away);
    let differs = |h: &Field, a: &Field| {
        if same_binary {
            h != a
        } else {
            (h.size, h.hash, h.bytes) != (a.size, a.hash, a.bytes)
        }
    };
    let mut missing = None;
    let mut compared = false;
    for home in &dossiers.home.steps {
        let Some(away) = dossiers
            .away
            .steps
            .iter()
            .find(|step| step.frame == home.frame)
        else {
            continue;
        };
        if home.fields.is_empty() || away.fields.is_empty() {
            missing.get_or_insert(home.frame);
            continue;
        }
        compared = true;
        let pairs = home.fields.iter().zip(&away.fields);
        if let Some((index, (h, a))) = pairs.enumerate().find(|(_, (h, a))| differs(h, a)) {
            let kind = if same_binary && h.caller != a.caller {
                Kind::Path
            } else {
                Kind::Value
            };
            return Divergence::found(kind, home.frame, index, Some(*h), Some(*a));
        }
        let partial = home.overflowed || away.overflowed;
        if home.fields.len() != away.fields.len() && !partial {
            let index = home.fields.len().min(away.fields.len());
            let (h, a) = (home.fields.get(index), away.fields.get(index));
            return Divergence::found(Kind::Count, home.frame, index, h.copied(), a.copied());
        }
        if partial {
            missing.get_or_insert(home.frame);
        }
    }
    match missing {
        Some(frame) => Divergence::unknown(Kind::Missing, Some(frame)),
        None if compared => Divergence::unknown(Kind::None, None),
        None => Divergence::unknown(Kind::Missing, None),
    }
}

/// Where the two sides' inputs first differ: inputs that all had arrived
/// should be the same on every machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputDivergence {
    /// A side's dossier lists no inputs (version 1).
    Unknown,
    /// The same inputs at every step both kept, `steps` of them.
    Same { steps: usize },
    /// A machine's inputs of a step differ, by these parts. `shift`: its
    /// inputs on the away side are those of the home side that many steps
    /// before (negative: after), at every step both kept.
    Differs {
        frame: u32,
        machine: usize,
        parts: Vec<String>,
        shift: Option<i32>,
    },
}

/// The steps from the oldest, machine by machine.
pub fn diverge_inputs(dossiers: &Teams<Dossier>) -> InputDivergence {
    let (Some(home), Some(away)) = (&dossiers.home.netplay, &dossiers.away.netplay) else {
        return InputDivergence::Unknown;
    };
    let mut steps = 0;
    for h in home.inputs.iter().filter(|step| step.kept) {
        let Some(a) = kept_step(away, h.frame) else {
            continue;
        };
        steps += 1;
        let machines = h.machines.len().max(a.machines.len());
        for machine in 0..machines {
            let (hi, ai) = (h.machines.get(machine), a.machines.get(machine));
            let parts = match (hi, ai) {
                (Some(hi), Some(ai)) if hi.same(ai) => continue,
                (Some(hi), Some(ai)) => hi.differences(ai),
                _ => vec!["machines".to_owned()],
            };
            return InputDivergence::Differs {
                frame: h.frame,
                machine,
                parts,
                shift: shift(home, away, machine),
            };
        }
    }
    InputDivergence::Same { steps }
}

fn kept_step(netplay: &Netplay, frame: u32) -> Option<&InputStep> {
    netplay
        .inputs
        .iter()
        .find(|step| step.kept && step.frame == frame)
}

/// The steps by which `machine`'s inputs on the away side follow the home
/// side's, when at every step both kept (eight at least, not all the same
/// inputs) they are the home side's that many steps before; the nearest.
fn shift(home: &Netplay, away: &Netplay, machine: usize) -> Option<i32> {
    fn input(netplay: &Netplay, frame: u32, machine: usize) -> Option<&MachineInput> {
        kept_step(netplay, frame).and_then(|step| step.machines.get(machine))
    }
    (1..=8).flat_map(|k| [k, -k]).find(|&k| {
        let mut compared = Vec::new();
        for step in &home.inputs {
            let (Some(h), Some(a)) = (
                input(home, step.frame, machine),
                input(away, step.frame.wrapping_add_signed(k), machine),
            ) else {
                continue;
            };
            if !h.same(a) {
                return false;
            }
            compared.push(h);
        }
        compared.len() >= 8 && compared.iter().any(|h| !h.same(compared[0]))
    })
}

impl InputDivergence {
    /// One line for a reader.
    pub fn describe(&self) -> String {
        match self {
            Self::Unknown => "a dossier from before 1.1.7 lists no inputs".to_owned(),
            Self::Same { steps } => format!(
                "the same at all {steps} steps both kept: the inputs did not diverge, the state did"
            ),
            Self::Differs {
                frame,
                machine,
                parts,
                shift,
            } => {
                let mut text = format!("at frame {frame}, machine {machine}: {}", parts.join(", "));
                match shift {
                    Some(k) if *k > 0 => {
                        text +=
                            &format!("; away has machine {machine}'s inputs {k} steps after home")
                    }
                    Some(k) => {
                        text += &format!(
                            "; away has machine {machine}'s inputs {} steps before home",
                            -k
                        )
                    }
                    None => {}
                }
                text
            }
        }
    }
}

/// Version 2's game and inputs, for [`describe`]; `marked` the step of the
/// inputs' divergence.
fn describe_netplay(out: &mut String, n: &Netplay, marked: Option<u32>) {
    use std::fmt::Write;
    let changed = match n.delay_changed_at {
        0 => "never".to_owned(),
        frame => format!("last at frame {frame}"),
    };
    let _ = writeln!(
        out,
        "this machine {} of {}; tracking from frame {}{}; input delay {} steps at the start, \
         longer {} times and shorter {}, {changed}",
        n.local_machine,
        n.machines,
        n.tracking_from,
        if n.tracking {
            ""
        } else {
            ", stopped at the desync"
        },
        n.start_delay,
        n.delay_longer,
        n.delay_shorter,
    );
    let mispredicted: Vec<String> = n.mispredicted[..n.machines as usize]
        .iter()
        .map(u32::to_string)
        .collect();
    let _ = writeln!(
        out,
        "in this game: mispredicted steps by machine {}; steps predicted {}, rollbacks to correct {}; \
         undone {}, replays cut {}; waits as the original router {}, window full {}",
        mispredicted.join(" "),
        n.predicted_steps,
        n.correction_rollbacks,
        n.undone,
        n.truncated,
        n.lockstep_waits,
        n.window_waits,
    );
    let _ = writeln!(
        out,
        "mispredictions it could no longer correct: predicted steps kept for a disc read {}, \
         rollbacks impossible {}, tracking stopped with inputs predicted {}",
        n.kept_predicted, n.impossible, n.stopped_predicted,
    );
    let _ = writeln!(
        out,
        "\ninputs of the steps through the desync's, once all had arrived (P: predicted when the \
         step first ran, M: and they came otherwise; pads by team and player, machine.player):"
    );
    for step in &n.inputs {
        let mark = if marked == Some(step.frame) { ">" } else { " " };
        if !step.kept {
            let _ = writeln!(out, "{mark} frame {}: not kept", step.frame);
            continue;
        }
        let pads = |team: usize| {
            step.pads[team]
                .iter()
                .map(|&pad| match pad {
                    -1 => "-".to_owned(),
                    pad => format!("{}.{}", pad / 4, pad % 4),
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let _ = writeln!(
            out,
            "{mark} frame {}: team 0 pads {}, team 1 pads {}",
            step.frame,
            pads(0),
            pads(1)
        );
        for (machine, input) in step.machines.iter().enumerate() {
            let flag = |bits: u8, letter: char| {
                if bits >> machine & 1 != 0 {
                    letter
                } else {
                    ' '
                }
            };
            let _ = writeln!(
                out,
                "    machine {machine} {}{} {}",
                flag(step.predicted, 'P'),
                flag(step.mispredicted, 'M'),
                input.describe()
            );
        }
    }
}

/// A side's dossier for [`describe`], with its Mii.
pub struct Sided {
    pub dossier: Dossier,
    pub mii: Option<String>,
}

/// A game's dossiers as text, to read or hand to a debugger: where they
/// diverge, then each side's header and fields, every caller raw and named.
pub fn describe(match_id: i64, game: i32, sides: &Teams<Option<Sided>>) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let release = [&sides.home, &sides.away]
        .into_iter()
        .flatten()
        .map(|side| side.dossier.release)
        .next()
        .unwrap_or("R4QE");
    let _ = writeln!(
        out,
        "Desync dossiers of match {match_id}, game {game} ({release})"
    );
    let mut diverged = None;
    let mut diverged_inputs = None;
    match (&sides.home, &sides.away) {
        (Some(home), Some(away)) => {
            let pair = Teams {
                home: home.dossier.clone(),
                away: away.dossier.clone(),
            };
            let divergence = diverge(&pair);
            let _ = writeln!(
                out,
                "\nWhere they diverge: {} ({})",
                divergence.kind.as_str(),
                divergence.kind.meaning()
            );
            if let (Some(frame), Some(index)) = (divergence.frame, divergence.field_index) {
                let _ = writeln!(out, "  at frame {frame}, field {index}");
                for (name, field) in [
                    ("home", divergence.fields.home),
                    ("away", divergence.fields.away),
                ] {
                    match field {
                        Some(field) => {
                            let _ = writeln!(out, "  {name} {}", field.describe());
                        }
                        None => {
                            let _ = writeln!(out, "  {name} has no field there");
                        }
                    }
                }
                diverged = Some((frame, index));
            }
            let inputs = diverge_inputs(&pair);
            let _ = writeln!(out, "Where the inputs diverge: {}", inputs.describe());
            if let InputDivergence::Differs { frame, .. } = inputs {
                diverged_inputs = Some(frame);
            }
            if pair.home.same_binary(&pair.away) {
                let _ = writeln!(
                    out,
                    "Both sides run the same binary: their callers are offsets in the same module."
                );
            } else {
                let _ = writeln!(
                    out,
                    "The sides run different binaries: their fields are compared by their place in \
                     the step, and each caller is an offset in its own side's module."
                );
            }
        }
        _ => {
            let _ = writeln!(out, "\nOnly one side's dossier came: nothing to compare.");
        }
    }
    for (name, side) in [("home", &sides.home), ("away", &sides.away)] {
        let Some(Sided { dossier: d, mii }) = side else {
            let _ = writeln!(out, "\n== {name}: no dossier ==");
            continue;
        };
        let who = mii.as_deref().unwrap_or("unknown Mii");
        let platform = Some(d.platform.as_str()).filter(|platform| !platform.is_empty());
        let written = time::OffsetDateTime::from_unix_timestamp(d.written_at)
            .ok()
            .and_then(|at| {
                at.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .unwrap_or_else(|| d.written_at.to_string());
        let options: Vec<_> = [(1, "poses"), (2, "physics bodies"), (4, "collisions")]
            .into_iter()
            .filter(|(bit, _)| d.options & bit != 0)
            .map(|(_, name)| name)
            .collect();
        let _ = writeln!(out, "\n== {name}: {who}, profile {} ==", d.profile_id);
        let _ = writeln!(
            out,
            "build {}, {}",
            d.build,
            platform.unwrap_or("unknown platform")
        );
        let _ = writeln!(out, "code at an offset: {}", symbolize(d.release, platform));
        let _ = writeln!(
            out,
            "series {}, written {written}, desync at frame {}",
            d.series, d.frame
        );
        let _ = writeln!(
            out,
            "checksums: this side {:08x}, the other {:08x}; seeds {:08x} and {:08x}",
            d.checksum[0], d.checksum[1], d.seed[0], d.seed[1]
        );
        let _ = writeln!(
            out,
            "other machine {}, input delay {} steps, {} steps predicted wrong {}, diagnostics: {}",
            d.machine,
            d.input_delay,
            d.mispredictions,
            if d.netplay.is_some() {
                "in the game"
            } else {
                "in the session (1.1.6)"
            },
            if options.is_empty() {
                "none".to_owned()
            } else {
                options.join(", ")
            }
        );
        for step in &d.steps {
            if step.fields.is_empty() {
                let _ = writeln!(out, "\nframe {}: not kept", step.frame);
                continue;
            }
            let more = if step.overflowed {
                ", and more not kept"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "\nframe {}, {} fields{more}:",
                step.frame,
                step.fields.len()
            );
            for (index, field) in step.fields.iter().enumerate() {
                let mark = if diverged == Some((step.frame, index)) {
                    ">"
                } else {
                    " "
                };
                let _ = writeln!(out, "{mark}{index:4} {}", field.describe());
            }
        }
        if let Some(netplay) = &d.netplay {
            out.push('\n');
            describe_netplay(&mut out, netplay, diverged_inputs);
        }
    }
    out
}

/// Days a dossier is kept.
pub const KEPT_DAYS: i32 = 14;
/// A profile's dossiers at most a day: a desync ends its series, so a player
/// sends a few at most.
pub const PER_DAY: i64 = 20;

/// What became of a dossier.
pub enum Stored {
    /// Kept, now or before. `diverged` once both sides' are in.
    Kept {
        match_id: i64,
        diverged: Option<Kind>,
    },
    /// Its profile reported no such series.
    NoSeries,
    /// Its profile played the series on the other side.
    WrongSide,
    /// Its profile sent [`PER_DAY`] already.
    TooMany,
}

/// Keeps `dossier` (whose bytes are `body`) with its series' match and, when
/// the other side's of the game is in, where they diverge.
pub async fn store(pool: &PgPool, dossier: &Dossier, body: &[u8]) -> Result<Stored, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "DELETE FROM desync_dossiers WHERE received_at < now() - make_interval(days => $1)",
        KEPT_DAYS,
    )
    .execute(&mut *tx)
    .await?;

    let Some(row) = sqlx::query!(
        "SELECT match_id, side FROM online_match_sides WHERE profile_id = $1 AND series = $2",
        dossier.profile_id,
        dossier.series,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(Stored::NoSeries);
    };
    if Side::parse(&row.side) != Some(dossier.side) {
        return Ok(Stored::WrongSide);
    }
    let match_id = row.match_id;

    // The other side's dossier may come at the same time: the second waits
    // here for the first's, so one of them sees both.
    sqlx::query!(
        "SELECT match_id FROM online_matches WHERE match_id = $1 FOR UPDATE",
        match_id,
    )
    .fetch_one(&mut *tx)
    .await?;

    let inserted = sqlx::query!(
        "INSERT INTO desync_dossiers
             (match_id, side, game, profile_id, release, build, platform, frame, written_at, body)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, to_timestamp($9), $10)
         ON CONFLICT DO NOTHING",
        match_id,
        dossier.side.as_str(),
        dossier.game,
        dossier.profile_id,
        dossier.release,
        dossier.build,
        dossier.platform,
        dossier.frame as i32,
        dossier.written_at as f64,
        body,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    // A dossier sent again (its answer was lost) is not counted twice.
    if inserted {
        let today = sqlx::query_scalar!(
            r#"SELECT count(*) AS "count!" FROM desync_dossiers
               WHERE profile_id = $1 AND received_at > now() - interval '1 day'"#,
            dossier.profile_id,
        )
        .fetch_one(&mut *tx)
        .await?;
        // This one included: dropping the transaction takes it back.
        if today > PER_DAY {
            return Ok(Stored::TooMany);
        }
    }

    let other = sqlx::query_scalar!(
        "SELECT body FROM desync_dossiers WHERE match_id = $1 AND game = $2 AND side = $3",
        match_id,
        dossier.game,
        dossier.side.other().as_str(),
    )
    .fetch_optional(&mut *tx)
    .await?;
    // Read when it came in; only a later version of the format could fail.
    let other = other.and_then(|body| Dossier::parse(&body).ok());
    let diverged = match other {
        Some(other) => {
            let dossiers = match dossier.side {
                Side::Home => Teams {
                    home: dossier.clone(),
                    away: other,
                },
                Side::Away => Teams {
                    home: other,
                    away: dossier.clone(),
                },
            };
            let divergence = diverge(&dossiers);
            let fields = divergence.fields;
            sqlx::query!(
                "INSERT INTO desync_divergences
                     (match_id, game, kind, frame, field_index, home_caller, away_caller,
                      home_size, away_size, home_hash, away_hash, home_bytes, away_bytes)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                 ON CONFLICT DO NOTHING",
                match_id,
                dossier.game,
                divergence.kind.as_str(),
                divergence.frame.map(|frame| frame as i32),
                divergence.field_index.map(|index| index as i32),
                fields.home.map(|field| i64::from(field.caller)),
                fields.away.map(|field| i64::from(field.caller)),
                fields.home.map(|field| i64::from(field.size)),
                fields.away.map(|field| i64::from(field.size)),
                fields.home.map(|field| i64::from(field.hash)),
                fields.away.map(|field| i64::from(field.hash)),
                fields.home.map(|field| field.bytes.to_vec()),
                fields.away.map(|field| field.bytes.to_vec()),
            )
            .execute(&mut *tx)
            .await?;
            Some(divergence.kind)
        }
        None => None,
    };

    tx.commit().await?;
    Ok(Stored::Kept { match_id, diverged })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn field(caller: u32, value: u32) -> Field {
        let mut bytes = [0; FIELD_BYTES];
        bytes[..4].copy_from_slice(&value.to_le_bytes());
        Field {
            caller,
            size: 4,
            hash: value.wrapping_mul(16_777_619),
            bytes,
        }
    }

    pub fn step(frame: u32, values: &[u32]) -> Step {
        Step {
            frame,
            overflowed: false,
            fields: values
                .iter()
                .enumerate()
                .map(|(i, &v)| field(0x0012_3400 + 4 * i as u32, v))
                .collect(),
        }
    }

    /// One player's inputs, its stick pushed by `stick` on x.
    pub fn machine_input(stick: i8) -> MachineInput {
        let mut records = [[0; RECORD]; 4];
        records[0][12] = stick as u8;
        MachineInput {
            players: 1,
            event_count: 0,
            remap: 0,
            states: [1, 0, 0, 0],
            records,
            events: [[0; EVENT]; 4],
        }
    }

    /// Two machines' inputs of the steps through `frame`: machine 0's stick
    /// by step from `stick`, machine 1's still.
    pub fn netplay(frame: u32, stick: impl Fn(u32) -> i8) -> Netplay {
        let first = frame - (INPUT_STEPS as u32 - 1);
        Netplay {
            local_machine: 0,
            machines: 2,
            tracking_from: 0,
            tracking: true,
            start_delay: 1,
            delay_longer: 0,
            delay_shorter: 0,
            delay_changed_at: 0,
            mispredicted: [0, 3, 0, 0],
            predicted_steps: 40,
            correction_rollbacks: 3,
            undone: 0,
            truncated: 0,
            kept_predicted: 0,
            impossible: 0,
            stopped_predicted: 0,
            lockstep_waits: 25,
            window_waits: 0,
            inputs: (first..=frame)
                .map(|frame| InputStep {
                    frame,
                    kept: true,
                    predicted: 0b10,
                    mispredicted: 0,
                    pads: [[0, -1, -1, -1, -1], [4, -1, -1, -1, -1]],
                    machines: vec![machine_input(stick(frame)), machine_input(0)],
                })
                .collect(),
        }
    }

    /// Machine 0 pushes its stick at 4800.
    pub fn pushed(frame: u32) -> i8 {
        if frame >= 4800 { 40 } else { 0 }
    }

    pub fn dossier(side: Side) -> Dossier {
        let mut netplay = netplay(4812, pushed);
        netplay.local_machine = match side {
            Side::Home => 0,
            Side::Away => 1,
        };
        Dossier {
            series: 1_761_500_000,
            written_at: 1_761_500_300,
            profile_id: 1_000_000_042,
            game: 2,
            side,
            frame: 4812,
            checksum: [0x1111_1111, 0x2222_2222],
            seed: [7, 7],
            machine: 1,
            input_delay: 2,
            mispredictions: 31,
            options: 0,
            release: "R4QE",
            build: "1.1.6+g0123456789ab".to_owned(),
            platform: "linux x86_64".to_owned(),
            steps: [step(4811, &[1, 2, 3]), step(4812, &[1, 2, 3])],
            version: 2,
            netplay: Some(netplay),
        }
    }

    fn pair() -> Teams<Dossier> {
        Teams {
            home: dossier(Side::Home),
            away: dossier(Side::Away),
        }
    }

    #[test]
    fn a_dossier_reads_back() {
        let original = dossier(Side::Away);
        let bytes = original.encode();
        assert_eq!(
            bytes.len(),
            HEADER
                + 2 * (STEP + 3 * FIELD)
                + MATCH
                + INPUT_STEPS * (INPUT_STEP + 2 * MACHINE_INPUT)
        );
        assert_eq!(Dossier::parse(&bytes), Ok(original));
    }

    /// 1.1.6's: the header and the fields alone.
    #[test]
    fn a_version_1_dossier_reads_back() {
        let mut original = dossier(Side::Away);
        original.version = 1;
        original.netplay = None;
        let bytes = original.encode();
        assert_eq!(bytes.len(), HEADER + 2 * (STEP + 3 * FIELD));
        assert_eq!(Dossier::parse(&bytes), Ok(original));
        // Or one with more after its steps.
        let mut more = bytes.clone();
        more.extend(dossier(Side::Away).encode()[bytes.len()..].iter());
        assert!(Dossier::parse(&more).unwrap_err().contains("bytes after"));
    }

    // The offsets of the game's desync_dossier.h, which the encoder keeps to.
    #[test]
    fn the_layout_is_the_games() {
        let bytes = dossier(Side::Away).encode();
        assert_eq!(&bytes[0..4], b"RCDS");
        assert_eq!(bytes[8..16], 1_761_500_000u64.to_le_bytes());
        assert_eq!(bytes[16..24], 1_761_500_300u64.to_le_bytes());
        assert_eq!(bytes[24..28], 1_000_000_042u32.to_le_bytes());
        assert_eq!(bytes[28..32], 2u32.to_le_bytes());
        assert_eq!(bytes[32..36], 1u32.to_le_bytes());
        assert_eq!(bytes[36..40], 4812u32.to_le_bytes());
        assert_eq!(bytes[44..48], 0x2222_2222u32.to_le_bytes());
        assert_eq!(bytes[64..68], 31u32.to_le_bytes());
        assert_eq!(&bytes[72..76], b"R4QE");
        assert_eq!(&bytes[76..95], b"1.1.6+g0123456789ab");
        assert_eq!(&bytes[140..152], b"linux x86_64");
        assert_eq!(bytes[156..160], 2u32.to_le_bytes());
        assert_eq!(bytes[160..164], 4811u32.to_le_bytes());
        assert_eq!(bytes[164..168], 3u32.to_le_bytes());
        assert_eq!(bytes[172..176], 0x0012_3400u32.to_le_bytes());
        // Version 2's game after the steps, then its inputs.
        assert_eq!(bytes[4..8], 2u32.to_le_bytes());
        let game = HEADER + 2 * (STEP + 3 * FIELD);
        assert_eq!(bytes[game..game + 4], 1u32.to_le_bytes()); // this machine
        assert_eq!(bytes[game + 4..game + 8], 2u32.to_le_bytes()); // machines
        assert_eq!(bytes[game + 16..game + 20], 1u32.to_le_bytes()); // the starting delay
        assert_eq!(bytes[game + 36..game + 40], 3u32.to_le_bytes()); // machine 1's mispredictions
        assert_eq!(bytes[game + 84..game + 88], 32u32.to_le_bytes()); // input steps
        let first = game + MATCH;
        assert_eq!(bytes[first..first + 4], 4781u32.to_le_bytes());
        assert_eq!(bytes[first + 4..first + 8], [1, 2, 0b10, 0]);
        assert_eq!(
            bytes[first + 8..first + 18],
            [0, 0xFF, 0xFF, 0xFF, 0xFF, 4, 0xFF, 0xFF, 0xFF, 0xFF]
        );
        let input = first + INPUT_STEP;
        assert_eq!(bytes[input..input + 8], [1, 0, 0, 0, 1, 0, 0, 0]);
        let last = first + 31 * (INPUT_STEP + 2 * MACHINE_INPUT);
        assert_eq!(bytes[last..last + 4], 4812u32.to_le_bytes());
        assert_eq!(bytes[last + INPUT_STEP + 8 + 12], 40); // machine 0's stick
        assert_eq!(MAX_BYTES, 57_232);
    }

    #[test]
    fn the_largest_dossier_fits() {
        let mut big = dossier(Side::Home);
        let values: Vec<u32> = (0..MAX_FIELDS as u32).collect();
        big.steps = [step(4811, &values), step(4812, &values)];
        let netplay = big.netplay.as_mut().unwrap();
        netplay.machines = 4;
        for step in &mut netplay.inputs {
            step.machines.resize(4, machine_input(0));
        }
        let bytes = big.encode();
        assert_eq!(bytes.len(), MAX_BYTES);
        assert!(Dossier::parse(&bytes).is_ok());
    }

    /// Why a dossier changed by `change` is refused.
    fn refused(change: impl FnOnce(&mut Vec<u8>)) -> String {
        let mut bytes = dossier(Side::Home).encode();
        change(&mut bytes);
        Dossier::parse(&bytes).expect_err("refused")
    }

    fn word(bytes: &mut [u8], at: usize, value: u32) {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn a_bad_dossier_is_refused() {
        assert!(refused(|b| b.truncate(100)).contains("cut short"));
        assert!(refused(|b| b.truncate(b.len() - 1)).contains("cut short"));
        assert!(refused(|b| b.push(0)).contains("bytes after"));
        assert!(refused(|b| b.resize(MAX_BYTES + 1, 0)).contains("over"));
        assert!(refused(|b| b[0] = b'X').contains("not a dossier"));
        assert!(refused(|b| word(b, 4, 3)).contains("version 3"));
        assert!(refused(|b| b[15] = 0x80).contains("series"));
        assert!(refused(|b| word(b, 24, 0)).contains("profile_id"));
        assert!(refused(|b| word(b, 24, 0x8000_0000)).contains("profile_id"));
        assert!(refused(|b| word(b, 28, 0)).contains("game 0"));
        assert!(refused(|b| word(b, 28, 10)).contains("game 10"));
        assert!(refused(|b| word(b, 32, 2)).contains("side 2"));
        assert!(refused(|b| word(b, 36, 0x8000_0000)).contains("frame"));
        assert!(refused(|b| b[72..76].copy_from_slice(b"RMCE")).contains("release"));
        assert!(refused(|b| b[76..140].fill(0)).contains("no build"));
        assert!(refused(|b| b[80] = 0x07).contains("build is not printable"));
        assert!(refused(|b| b[139] = b'x').contains("build has bytes after"));
        assert!(refused(|b| b[154] = b'x').contains("platform has bytes after"));
        assert!(
            refused(|b| b[140..152].copy_from_slice(b"linux_x86_64"))
                .contains("a system and a CPU")
        );
        assert!(
            refused(|b| b[140..152].copy_from_slice(b"Linux x86_64"))
                .contains("a system and a CPU")
        );
        assert!(refused(|b| word(b, 156, 3)).contains("3 steps"));
        assert!(refused(|b| word(b, 160, 4810)).contains("frame 4810"));
        assert!(refused(|b| word(b, 164, 513)).contains("513 fields"));
        assert!(refused(|b| word(b, 168, 2)).contains("overflowed 2"));
        // Version 2's game and inputs.
        let game = HEADER + 2 * (STEP + 3 * FIELD);
        let first = game + MATCH;
        assert!(refused(|b| word(b, game + 4, 0)).contains("0 machines"));
        assert!(refused(|b| word(b, game + 4, 5)).contains("5 machines"));
        assert!(refused(|b| word(b, game, 2)).contains("local machine 2"));
        assert!(refused(|b| word(b, game + 12, 2)).contains("tracking 2"));
        assert!(refused(|b| word(b, game + 84, 31)).contains("31 input steps"));
        assert!(refused(|b| word(b, first, 4780)).contains("frame 4780, not 4781"));
        assert!(refused(|b| b[first + 4] = 2).contains("kept 2"));
        assert!(refused(|b| b[first + 5] = 1).contains("1 machines' inputs of 2"));
        assert!(refused(|b| b[first + 6] = 0b100).contains("does not list"));
        assert!(refused(|b| b[first + 8] = 16).contains("pad 16"));
        assert!(refused(|b| b[first + 18] = 1).contains("reserved"));
        assert!(refused(|b| b[first + INPUT_STEP] = 5).contains("5 players"));
        assert!(refused(|b| b.truncate(b.len() - 1)).contains("cut short"));
    }

    /// An input step gone from the ring: nothing but its frame.
    #[test]
    fn an_input_step_not_kept_lists_nothing() {
        let mut d = dossier(Side::Home);
        let step = &mut d.netplay.as_mut().unwrap().inputs[0];
        (step.kept, step.machines, step.predicted, step.pads) = (false, vec![], 0, [[-1; 5]; 2]);
        assert_eq!(Dossier::parse(&d.encode()), Ok(d.clone()));
        d.netplay.as_mut().unwrap().inputs[0].pads[1][0] = 4;
        assert!(
            Dossier::parse(&d.encode())
                .unwrap_err()
                .contains("not kept has inputs")
        );
    }

    #[test]
    fn the_same_inputs_are_the_same() {
        assert_eq!(diverge_inputs(&pair()), InputDivergence::Same { steps: 32 });
        let mut p = pair();
        p.away.version = 1;
        p.away.netplay = None;
        assert_eq!(diverge_inputs(&p), InputDivergence::Unknown);
        // A step one side lost is not compared.
        let mut p = pair();
        let step = &mut p.away.netplay.as_mut().unwrap().inputs[31];
        (step.kept, step.machines, step.predicted, step.pads) = (false, vec![], 0, [[-1; 5]; 2]);
        assert_eq!(diverge_inputs(&p), InputDivergence::Same { steps: 31 });
    }

    #[test]
    fn other_inputs_are_named_by_their_parts() {
        let mut p = pair();
        p.away.netplay.as_mut().unwrap().inputs[20].machines[1].records[0][1] = 0x80;
        p.away.netplay.as_mut().unwrap().inputs[20].machines[1].records[0][5] = 3;
        assert_eq!(
            diverge_inputs(&p),
            InputDivergence::Differs {
                frame: 4801,
                machine: 1,
                parts: vec![
                    "player 0's buttons".to_owned(),
                    "player 0's remote accel".to_owned()
                ],
                shift: None,
            }
        );
    }

    /// The away side got machine 0's inputs two steps late: its stick moves
    /// at 4802 there.
    #[test]
    fn inputs_two_steps_late_are_a_shift() {
        let mut p = pair();
        p.away.netplay = Some(netplay(4812, |frame| pushed(frame - 2)));
        let d = diverge_inputs(&p);
        assert_eq!(
            d,
            InputDivergence::Differs {
                frame: 4800,
                machine: 0,
                parts: vec!["player 0's sticks".to_owned()],
                shift: Some(2),
            }
        );
        assert_eq!(
            d.describe(),
            "at frame 4800, machine 0: player 0's sticks; away has machine 0's inputs 2 steps after home"
        );
        // Early on the home side instead.
        p.away.netplay = Some(netplay(4812, |frame| pushed(frame + 3)));
        assert!(matches!(
            diverge_inputs(&p),
            InputDivergence::Differs {
                frame: 4797,
                shift: Some(-3),
                ..
            }
        ));
    }

    #[test]
    fn an_unknown_platform_is_empty() {
        let mut d = dossier(Side::Home);
        d.platform = String::new();
        assert_eq!(Dossier::parse(&d.encode()).unwrap().platform, "");
    }

    #[test]
    fn the_same_fields_point_at_the_seed() {
        let d = diverge(&pair());
        assert_eq!((d.kind, d.frame, d.field_index), (Kind::None, None, None));
    }

    #[test]
    fn different_bytes_are_a_value() {
        let mut p = pair();
        p.away.steps[1] = step(4812, &[1, 9, 3]);
        let d = diverge(&p);
        assert_eq!(
            (d.kind, d.frame, d.field_index),
            (Kind::Value, Some(4812), Some(1))
        );
        assert_eq!(d.fields.home, Some(field(0x0012_3404, 2)));
        assert_eq!(d.fields.away, Some(field(0x0012_3404, 9)));
    }

    #[test]
    fn the_earlier_step_comes_first() {
        let mut p = pair();
        p.home.steps[0] = step(4811, &[1, 2, 8]);
        p.away.steps[1] = step(4812, &[5, 2, 3]);
        let d = diverge(&p);
        assert_eq!(
            (d.kind, d.frame, d.field_index),
            (Kind::Value, Some(4811), Some(2))
        );
    }

    #[test]
    fn another_place_is_a_path() {
        let mut p = pair();
        p.away.steps[0].fields[0].caller = 0x0055_0000;
        let d = diverge(&p);
        assert_eq!(
            (d.kind, d.frame, d.field_index),
            (Kind::Path, Some(4811), Some(0))
        );
    }

    #[test]
    fn more_fields_are_a_count() {
        let mut p = pair();
        p.home.steps[1] = step(4812, &[1, 2, 3, 4]);
        let d = diverge(&p);
        assert_eq!(
            (d.kind, d.frame, d.field_index),
            (Kind::Count, Some(4812), Some(3))
        );
        assert_eq!(d.fields.home, Some(field(0x0012_340C, 4)));
        assert_eq!(d.fields.away, None);
    }

    #[test]
    fn a_step_not_kept_is_missing() {
        let mut p = pair();
        p.away.steps[0].fields.clear();
        let d = diverge(&p);
        assert_eq!((d.kind, d.frame), (Kind::Missing, Some(4811)));
        // Unless the other step tells.
        p.away.steps[1] = step(4812, &[1, 2, 4]);
        assert_eq!(diverge(&p).kind, Kind::Value);
    }

    #[test]
    fn a_step_kept_in_part_is_missing() {
        let mut p = pair();
        p.home.steps[1].overflowed = true;
        p.home.steps[1].fields.pop();
        let d = diverge(&p);
        assert_eq!((d.kind, d.frame), (Kind::Missing, Some(4812)));
    }

    /// A Linux side and a Windows one: the same code, other offsets.
    #[test]
    fn other_binaries_compare_their_fields_by_place() {
        let mut p = pair();
        p.away.platform = "windows x86_64".to_owned();
        for field in p.away.steps.iter_mut().flat_map(|step| &mut step.fields) {
            field.caller += 0x1000;
        }
        assert_eq!(diverge(&p).kind, Kind::None);
        p.away.steps[1].fields[2].bytes[0] = 9;
        p.away.steps[1].fields[2].hash = 1;
        let d = diverge(&p);
        assert_eq!(
            (d.kind, d.frame, d.field_index),
            (Kind::Value, Some(4812), Some(2))
        );
        // The same binary: the same callers.
        let mut p = pair();
        p.away.steps[0].fields[1].caller = 0x0055_0000;
        assert_eq!(diverge(&p).kind, Kind::Path);
        // Unknown platforms: not the same binary.
        p.home.platform.clear();
        p.away.platform.clear();
        assert_eq!(diverge(&p).kind, Kind::None);
    }

    #[test]
    fn a_field_is_described_as_the_log_has_it() {
        let with = |caller: u32, size: u32, bytes: &[u8]| {
            let mut field = Field {
                caller,
                size,
                hash: 0xABCD_0123,
                bytes: [0; FIELD_BYTES],
            };
            field.bytes[..bytes.len()].copy_from_slice(bytes);
            field.describe()
        };
        let floats = |values: &[f32]| {
            values
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            with(0x0012_3400, 20, &[1, 2, 3, 4]),
            "0x00123400 at +0x123400: 20 bytes 01020304000000000000000000000000..., size 20, hash abcd0123"
        );
        assert_eq!(
            with(0xF000_1203, 64, &floats(&[1.5, -2.0, 0.25])),
            "0xf0001203 pose team 1 player 2 node 3: translation 1.5 -2 0.25, size 64, hash abcd0123"
        );
        assert_eq!(
            with(0xE000_0007, 40, &floats(&[1.0, 2.0, 3.0])),
            "0xe0000007 body 7: position 1 2 3, size 40, hash abcd0123"
        );
        assert_eq!(
            with(0xB012_3456, 20, b"shoot %d\n"),
            "0xb0123456 branch at +0x123456: \"shoot %d ...\", size 20, hash abcd0123"
        );
        let mut values = 7i64.to_le_bytes().to_vec();
        values.extend(0.5f64.to_le_bytes());
        assert_eq!(
            with(0xA000_0006, 16, &values),
            "0xa0000006 values: 7 0.5 (3fe0000000000000), size 16, hash abcd0123"
        );
        assert_eq!(
            with(0x9000_0012, 16, &floats(&[1.0, 2.0, 3.0, 4.0])),
            "0x90000012 ball chain 1 part 2: 1 2 3 4, size 16, hash abcd0123"
        );
        let mut set = floats(&[1.0, 2.0, 3.0]);
        set.extend(1u32.to_le_bytes());
        assert_eq!(
            with(0x8000_0040, 16, &set),
            "0x80000040 ball set at +0x40 in the parent's coordinates: 1 2 3, size 16, hash abcd0123"
        );
        let pair_words: Vec<u8> = [2i32, 1, 0].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(
            with(0xD000_0003, 12, &pair_words),
            "0xd0000003 collision 3: 2 contacts, Contact() 1 and 0, size 12, hash abcd0123"
        );
        let mut contact = 0x1Fu32.to_le_bytes().to_vec();
        contact.extend(floats(&[0.5, 1.0, 0.0]));
        assert_eq!(
            with(0xC000_0050, 16, &contact),
            "0xc0000050 contact 5 (mode mu mu2 bounce): 0000001f 0.5 1 0, size 16, hash abcd0123"
        );
    }

    #[test]
    fn the_text_has_both_sides_and_where_they_diverge() {
        let mut p = pair();
        p.away.steps[1] = step(4812, &[1, 9, 3]);
        let sides = Teams {
            home: Some(Sided {
                dossier: p.home,
                mii: Some("Anosch".to_owned()),
            }),
            away: Some(Sided {
                dossier: p.away,
                mii: None,
            }),
        };
        let text = describe(42, 2, &sides);
        for line in [
            "Desync dossiers of match 42, game 2 (R4QE)",
            "Where they diverge: value (the same field, different bytes or size)",
            "  at frame 4812, field 1",
            "  away 0x00123404 at +0x123404: 4 bytes 09000000, size 4, hash 09000e2b",
            "Both sides run the same binary",
            "== home: Anosch, profile 1000000042 ==",
            "== away: unknown Mii, profile 1000000042 ==",
            "build 1.1.6+g0123456789ab, linux x86_64",
            "code at an offset: addr2line -f -C -e recharged_game_us.so <offset>",
            "checksums: this side 11111111, the other 22222222; seeds 00000007 and 00000007",
            "frame 4811, 3 fields:",
            ">   1 0x00123404 at +0x123404: 4 bytes 02000000, size 4, hash 02000326",
            "    2 0x00123408",
            "Where the inputs diverge: the same at all 32 steps both kept",
            "31 steps predicted wrong in the game",
            "this machine 0 of 2; tracking from frame 0; input delay 1 steps at the start",
            "in this game: mispredicted steps by machine 0 3; steps predicted 40",
            "rollbacks impossible 0, tracking stopped with inputs predicted 0",
            "  frame 4800: team 0 pads 0.0 - - - -, team 1 pads 1.0 - - - -",
            "    machine 0    remap 0000; player 0 state 1: buttons 0000 triggers 00 accel 0 0 0 \
             nunchuk 0 0 0 pointer 00 0000 stick 40 0 c-stick 0 0",
            "    machine 1 P  remap 0000",
        ] {
            assert!(text.contains(line), "{line:?} in:\n{text}");
        }
        // One side alone.
        let alone = Teams {
            home: None,
            away: sides.away,
        };
        let text = describe(42, 2, &alone);
        assert!(text.contains("Only one side's dossier came"), "{text}");
        assert!(text.contains("== home: no dossier =="), "{text}");
    }

    #[test]
    fn no_step_in_common_is_missing() {
        let mut p = pair();
        p.away.frame = 5000;
        p.away.steps = [step(4999, &[1]), step(5000, &[1])];
        let d = diverge(&p);
        assert_eq!((d.kind, d.frame), (Kind::Missing, None));
    }
}

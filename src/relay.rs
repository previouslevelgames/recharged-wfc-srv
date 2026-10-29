//! The relay for two games that cannot reach each other. NAT negotiation tells
//! each game the other's public address, and the two ping there. Some routers,
//! carrier-grade NAT above all, give a socket a new public port for every
//! address it sends to: the other game's pings then go to a port nobody
//! listens on, and the game stops with error 86070. For such a pair the
//! CONNECTs name this server instead. Each game then sends its pings, and the
//! match after them, to the NAT negotiation port its game socket has already
//! talked to, and the relay passes every packet on to the other game, from
//! that same port, which both routers have already let through.
//!
//! A relayed game is known by the address its game socket's INIT came from.
//! Packets from it go to the other game of its negotiation, and nowhere else.

use std::{
    collections::HashMap,
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

use tracing::{info, warn};

/// The variables the relay reads.
pub const VARIABLES: [&str; 3] = ["NATNEG_RELAY", "NATNEG_RELAY_ADDRESS", "NATNEG_RELAY_MAX"];

/// The port games send NAT negotiation to, whatever port the server listens
/// on inside its container: the game has it built in.
const GAME_PORT: u16 = 27901;
/// Relayed matches at once, unless `NATNEG_RELAY_MAX` says otherwise.
const DEFAULT_MAX: usize = 16;
/// A relayed match not heard from for this long is over.
const IDLE: Duration = Duration::from_secs(60);
/// How long a host name in `NATNEG_RELAY_ADDRESS` stays resolved.
const RESOLVED_FOR: Duration = Duration::from_secs(60);

/// Which pairs the relay takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The pairs that would not reach each other: one game's router gives
    /// it a new port per address, or the two failed without the relay before.
    Auto,
    /// Every pair, to try the relay out.
    Always,
}

/// The relay's settings, from the environment.
#[derive(Clone, Debug)]
pub struct Config {
    pub mode: Mode,
    /// The address games reach this server at, as their `online.server`
    /// names it: an IPv4 address or a host name.
    pub address: String,
    /// Relayed matches at once, at most; more pairs try without the relay.
    pub max: usize,
    /// The port games send NAT negotiation to: 27901, another in tests.
    pub port: u16,
}

impl Config {
    /// Reads [`VARIABLES`]. `Ok(None)` when the relay is off: `NATNEG_RELAY`
    /// is `off`, or `NATNEG_RELAY_ADDRESS` unset.
    pub fn from_env() -> Result<Option<Self>, String> {
        Self::from_vars(|name| env::var(name).ok())
    }

    fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let get = |name: &str| {
            var(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let mode = match get("NATNEG_RELAY").map(|value| value.to_lowercase()) {
            None => Mode::Auto,
            Some(value) if value == "auto" => Mode::Auto,
            Some(value) if value == "always" => Mode::Always,
            Some(value) if value == "off" => return Ok(None),
            Some(value) => {
                return Err(format!("NATNEG_RELAY: {value} is not auto, always or off"));
            }
        };
        let Some(address) = get("NATNEG_RELAY_ADDRESS") else {
            if mode == Mode::Always {
                return Err("NATNEG_RELAY=always needs NATNEG_RELAY_ADDRESS".into());
            }
            return Ok(None);
        };
        if let Ok(IpAddr::V6(_)) = address.parse::<IpAddr>() {
            return Err(format!(
                "NATNEG_RELAY_ADDRESS: {address} is IPv6; games reach the server over IPv4"
            ));
        }
        let max = match get("NATNEG_RELAY_MAX") {
            None => DEFAULT_MAX,
            Some(value) => value
                .parse()
                .ok()
                .filter(|max| *max > 0)
                .ok_or_else(|| format!("NATNEG_RELAY_MAX: {value} is not a positive number"))?,
        };
        Ok(Some(Self {
            mode,
            address,
            max,
            port: GAME_PORT,
        }))
    }
}

/// The relayed matches.
pub(crate) struct Relays {
    pub config: Config,
    state: Mutex<State>,
    /// `config.address` as an IPv4 address, and when it was resolved.
    resolved: Mutex<Option<(Ipv4Addr, Instant)>>,
}

#[derive(Default)]
struct State {
    /// By their negotiation's cookie.
    matches: HashMap<u32, Relayed>,
    /// Each relayed game's address, and the cookie of its match.
    games: HashMap<SocketAddr, u32>,
}

struct Relayed {
    /// By client index.
    games: [SocketAddr; 2],
    started: Instant,
    last: Instant,
    packets: u64,
    bytes: u64,
}

/// Why a pair is not relayed after all.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Full;

impl Relays {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            state: Mutex::default(),
            resolved: Mutex::default(),
        }
    }

    /// The address to give the games, `config.address` resolved if it is a
    /// name. A failed lookup keeps the last address, if there was one.
    pub async fn address(&self) -> Option<Ipv4Addr> {
        if let Ok(address) = self.config.address.parse() {
            return Some(address);
        }
        let last = *self.resolved.lock().unwrap();
        if let Some((address, at)) = last
            && at.elapsed() < RESOLVED_FOR
        {
            return Some(address);
        }
        let found = tokio::net::lookup_host((self.config.address.as_str(), 0))
            .await
            .ok()
            .and_then(|mut addresses| {
                addresses.find_map(|address| match address.ip() {
                    IpAddr::V4(address) => Some(address),
                    IpAddr::V6(_) => None,
                })
            });
        match found {
            Some(address) => {
                *self.resolved.lock().unwrap() = Some((address, Instant::now()));
                Some(address)
            }
            None => {
                warn!(target: "natneg", address = self.config.address, "relay address not found");
                last.map(|(address, _)| address)
            }
        }
    }

    /// Relays the games of the negotiation `cookie`, by client index, from
    /// now on. A match either game was relayed in before ends.
    pub fn open(&self, cookie: u32, games: [SocketAddr; 2]) -> Result<(), Full> {
        let mut state = self.state.lock().unwrap();
        let now = Instant::now();
        state.end_idle(now);
        for game in games {
            if let Some(old) = state.games.get(&game).copied() {
                state.end(old, "a new one");
            }
        }
        if state.matches.len() >= self.config.max {
            return Err(Full);
        }
        for game in games {
            state.games.insert(game, cookie);
        }
        state.matches.insert(
            cookie,
            Relayed {
                games,
                started: now,
                last: now,
                packets: 0,
                bytes: 0,
            },
        );
        Ok(())
    }

    /// Where a packet of `len` bytes from `from` goes: the other game of its
    /// relayed match, if `from` is a relayed game.
    pub fn destination(&self, from: SocketAddr, len: usize) -> Option<SocketAddr> {
        let mut state = self.state.lock().unwrap();
        let cookie = *state.games.get(&from)?;
        let relayed = state.matches.get_mut(&cookie)?;
        relayed.last = Instant::now();
        relayed.packets += 1;
        relayed.bytes += len as u64;
        let index = usize::from(relayed.games[0] == from);
        Some(relayed.games[index])
    }

    /// A ping of the relayed match `cookie` from an address the relay does
    /// not know: when it comes from the IP of one of the two games only, that
    /// game's router has given its pings another port, which is the game's
    /// address from now on. Whether it was.
    pub fn moved(&self, cookie: u32, from: SocketAddr) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(relayed) = state.matches.get_mut(&cookie) else {
            return false;
        };
        let same_ip: Vec<usize> = (0..2)
            .filter(|&index| relayed.games[index].ip() == from.ip())
            .collect();
        let [index] = same_ip[..] else {
            return false;
        };
        let old = std::mem::replace(&mut relayed.games[index], from);
        state.games.remove(&old);
        state.games.insert(from, cookie);
        info!(target: "natneg", cookie = format!("{cookie:08x}"), %old, new = %from, "relayed game moved");
        true
    }

    /// Ends the matches not heard from lately.
    pub fn end_idle(&self) {
        self.state.lock().unwrap().end_idle(Instant::now());
    }

    /// The matches relayed now.
    #[cfg(test)]
    pub fn active(&self) -> usize {
        self.state.lock().unwrap().matches.len()
    }
}

impl State {
    fn end_idle(&mut self, now: Instant) {
        let idle: Vec<u32> = self
            .matches
            .iter()
            .filter(|(_, relayed)| now.duration_since(relayed.last) >= IDLE)
            .map(|(&cookie, _)| cookie)
            .collect();
        for cookie in idle {
            self.end(cookie, "idle");
        }
    }

    fn end(&mut self, cookie: u32, why: &str) {
        let Some(relayed) = self.matches.remove(&cookie) else {
            return;
        };
        for game in relayed.games {
            if self.games.get(&game) == Some(&cookie) {
                self.games.remove(&game);
            }
        }
        info!(
            target: "natneg",
            cookie = format!("{cookie:08x}"),
            why,
            seconds = relayed.last.duration_since(relayed.started).as_secs(),
            packets = relayed.packets,
            kib = relayed.bytes / 1024,
            "relay ended"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(vars: &[(&str, &str)]) -> Result<Option<Config>, String> {
        Config::from_vars(|name| {
            vars.iter()
                .find(|(var, _)| *var == name)
                .map(|(_, value)| value.to_string())
        })
    }

    fn relays(max: usize) -> Relays {
        Relays::new(Config {
            mode: Mode::Auto,
            address: "198.51.100.1".into(),
            max,
            port: GAME_PORT,
        })
    }

    fn address(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    #[test]
    fn the_relay_needs_its_address() {
        assert!(config(&[]).unwrap().is_none());
        assert!(
            config(&[("NATNEG_RELAY", "off"), ("NATNEG_RELAY_ADDRESS", "1.2.3.4")])
                .unwrap()
                .is_none()
        );
        assert!(config(&[("NATNEG_RELAY", "always")]).is_err());
        assert!(
            config(&[
                ("NATNEG_RELAY", "sometimes"),
                ("NATNEG_RELAY_ADDRESS", "1.2.3.4")
            ])
            .is_err()
        );
        assert!(config(&[("NATNEG_RELAY_ADDRESS", "2001:db8::1")]).is_err());
        assert!(
            config(&[
                ("NATNEG_RELAY_ADDRESS", "1.2.3.4"),
                ("NATNEG_RELAY_MAX", "0")
            ])
            .is_err()
        );

        let auto = config(&[("NATNEG_RELAY_ADDRESS", " wfc.example.com ")])
            .unwrap()
            .unwrap();
        assert_eq!(
            (auto.mode, auto.address.as_str(), auto.max, auto.port),
            (Mode::Auto, "wfc.example.com", 16, 27901)
        );
        let always = config(&[
            ("NATNEG_RELAY", "Always"),
            ("NATNEG_RELAY_ADDRESS", "1.2.3.4"),
            ("NATNEG_RELAY_MAX", "3"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!((always.mode, always.max), (Mode::Always, 3));
    }

    #[test]
    fn each_game_reaches_the_other() {
        let relays = relays(4);
        let (one, two) = (address("203.0.113.7:21982"), address("198.51.100.9:65116"));
        relays.open(7, [one, two]).unwrap();
        assert_eq!(relays.destination(one, 20), Some(two));
        assert_eq!(relays.destination(two, 20), Some(one));
        assert_eq!(relays.destination(address("203.0.113.7:21983"), 20), None);
    }

    #[test]
    fn a_new_match_ends_the_old_one() {
        let relays = relays(4);
        let (one, two, three) = (
            address("203.0.113.7:21982"),
            address("198.51.100.9:65116"),
            address("192.0.2.5:50000"),
        );
        relays.open(7, [one, two]).unwrap();
        relays.open(8, [three, one]).unwrap();
        assert_eq!(relays.active(), 1);
        assert_eq!(relays.destination(one, 20), Some(three));
        assert_eq!(relays.destination(two, 20), None);
    }

    #[test]
    fn at_most_max_matches() {
        let relays = relays(1);
        relays
            .open(7, [address("203.0.113.7:1"), address("198.51.100.9:2")])
            .unwrap();
        assert_eq!(
            relays.open(8, [address("192.0.2.5:3"), address("192.0.2.6:4")]),
            Err(Full)
        );
    }

    #[test]
    fn a_game_whose_pings_take_another_port_moves() {
        let relays = relays(4);
        let (one, two) = (address("203.0.113.7:21982"), address("198.51.100.9:65116"));
        relays.open(7, [one, two]).unwrap();
        let moved = address("203.0.113.7:21990");
        assert!(!relays.moved(8, moved), "another negotiation");
        assert!(!relays.moved(7, address("192.0.2.5:21990")), "another IP");
        assert!(relays.moved(7, moved));
        assert_eq!(relays.destination(two, 20), Some(moved));
        assert_eq!(relays.destination(moved, 20), Some(two));
        assert_eq!(relays.destination(one, 20), None);

        // Two games behind one IP: which one moved is unknown.
        let relays = self::relays(4);
        relays
            .open(9, [address("203.0.113.7:1"), address("203.0.113.7:2")])
            .unwrap();
        assert!(!relays.moved(9, address("203.0.113.7:3")));
    }
}

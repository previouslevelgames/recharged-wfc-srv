//! Players without Discord (docs/profiles.md, "Playing without Discord"):
//! the launcher's **Play with a Mii** makes a player under a name of its own,
//! without an account. The first WFC profile it proves becomes what
//! identifies it ([`super::players::bind_identity`]), and the game's WFC
//! login then signs it in again on any computer.

use std::{
    collections::HashMap,
    convert::Infallible,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{ConnectInfo, FromRequestParts, State},
    http::{HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use super::{
    ProfilesState, database_error, json_error,
    play::WfcLogin,
    players::{self, Doing},
    signed_in,
};

/// The longest profile name, in characters, as long as a Discord display
/// name the launcher shows. (The Mii's own name in its record is the
/// client's to fit.)
pub const MAX_NAME: usize = 32;
/// New Mii players one address can make in [`WINDOW`].
pub const NEW_PLAYERS_PER_WINDOW: u32 = 5;
const WINDOW: Duration = Duration::from_secs(60 * 60);
/// [`known_wfc`] asks one address can make in [`KNOWN_WINDOW`]: a signed-out
/// launcher asks once for its game save's Miis, and again when they change;
/// many more is going through profile ids.
pub const KNOWN_PER_WINDOW: u32 = 20;
const KNOWN_WINDOW: Duration = Duration::from_secs(60);

/// How often each address did something lately (made a Mii player, asked
/// [`known_wfc`]). A plain `std` mutex: it is never held across an `.await`.
pub struct PerAddress {
    windows: Mutex<HashMap<IpAddr, (Instant, u32)>>,
    per_window: u32,
    window: Duration,
}

impl PerAddress {
    /// At most `per_window` in each `window` per address.
    pub fn new(per_window: u32, window: Duration) -> Self {
        Self {
            windows: Mutex::default(),
            per_window,
            window,
        }
    }

    /// New Mii players: [`NEW_PLAYERS_PER_WINDOW`] an hour.
    pub fn creations() -> Self {
        Self::new(NEW_PLAYERS_PER_WINDOW, WINDOW)
    }

    /// [`known_wfc`]'s asks: [`KNOWN_PER_WINDOW`] a minute.
    pub fn known() -> Self {
        Self::new(KNOWN_PER_WINDOW, KNOWN_WINDOW)
    }

    /// Whether `address` may do it once more now; it counts.
    pub fn allow(&self, address: IpAddr) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock().unwrap();
        windows.retain(|_, (since, _)| now.duration_since(*since) < self.window);
        let (_, done) = windows.entry(bucket(address)).or_insert((now, 0));
        *done += 1;
        *done <= self.per_window
    }
}

/// An IPv6 address counts with its whole /64, which one connection usually
/// has.
fn bucket(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !u128::from(u64::MAX))),
    }
}

/// The address a request comes from: the peer's, or, from a proxy on the
/// server's own network (a Cloudflare Tunnel's connector, a reverse proxy),
/// the one it names in `CF-Connecting-IP`, else last in `X-Forwarded-For`.
pub struct ClientAddress(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientAddress {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let peer = ConnectInfo::<SocketAddr>::from_request_parts(parts, state)
            .await
            .ok()
            .map(|ConnectInfo(peer)| peer.ip());
        Ok(Self(client_address(peer, &parts.headers)))
    }
}

fn client_address(peer: Option<IpAddr>, headers: &HeaderMap) -> IpAddr {
    let peer = peer.map(|peer| peer.to_canonical());
    if peer.is_none_or(is_local) {
        let header = |name: &str| {
            headers
                .get_all(name)
                .iter()
                .next_back()
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.rsplit(',').next())
                .and_then(|value| value.trim().parse::<IpAddr>().ok())
        };
        if let Some(named) = header("cf-connecting-ip").or_else(|| header("x-forwarded-for")) {
            return named.to_canonical();
        }
    }
    peer.unwrap_or(IpAddr::V6(Ipv6Addr::UNSPECIFIED))
}

/// An address of the server's own network, where a proxy runs.
fn is_local(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            let [first, second, ..] = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                // 100.64.0.0/10, shared address space (e.g. Tailscale).
                || (first == 100 && second & 0xc0 == 64)
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// `name` as a profile name, if it is one: trimmed, 1 to [`MAX_NAME`]
/// characters, none of them a control or an invisible formatting character.
pub fn profile_name(name: &str) -> Option<String> {
    let name = name.trim();
    let valid = (1..=MAX_NAME).contains(&name.chars().count())
        && !name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '\u{200b}'..='\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2060}'..='\u{2069}'
                        | '\u{feff}'
                )
        });
    valid.then(|| name.to_owned())
}

#[derive(Deserialize)]
pub(super) struct MiiSignIn {
    name: String,
}

/// **Play with a Mii**: a new player under `name`, signed in at once.
pub(super) async fn sign_in_mii(
    State(state): State<Arc<ProfilesState>>,
    ClientAddress(address): ClientAddress,
    Json(request): Json<MiiSignIn>,
) -> Response {
    let Some(name) = profile_name(&request.name) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("a name has 1 to {MAX_NAME} characters, without control characters"),
        );
    };
    if !state.mii_players.allow(address) {
        info!(target: "profiles", %address, "too many new Mii players");
        return json_error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many new players from here: try again in an hour",
        );
    }
    let player_id = match players::create_mii(&state.pool, &name).await {
        Ok(player_id) => player_id,
        Err(error) => return database_error(error),
    };
    info!(target: "profiles", player_id, "Mii player made");
    session(&state, player_id).await
}

/// The game, after its WFC login, signs in the player whose identity that
/// WFC profile is: a Mii player back on another computer, or without its
/// session. The proof is the one of `POST /api/v1/wfc`, and binds the
/// profile as that does.
pub(super) async fn sign_in_wfc(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    Json(login): Json<WfcLogin>,
) -> Response {
    match state
        .gpcm
        .logged_in_with(login.profile_id, &login.login_ticket)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return json_error(
                StatusCode::FORBIDDEN,
                "that profile is not logged in to WFC with this login ticket",
            );
        }
        Err(error) => return database_error(error),
    }
    let player_id = match players::by_wfc_identity(&state.pool, login.profile_id).await {
        Ok(Some(player_id)) => player_id,
        Ok(None) => {
            return json_error(
                StatusCode::NOT_FOUND,
                "no Recharged profile signs in with this WFC profile",
            );
        }
        Err(error) => return database_error(error),
    };
    if let Err(error) = players::bind_wfc(&state.pool, player_id, login.profile_id).await {
        return database_error(error);
    }
    if let Err(error) = players::heartbeat(
        &state.pool,
        player_id,
        Doing::from_headers(&headers),
        players::game_from_headers(&headers).as_deref(),
    )
    .await
    {
        return database_error(error);
    }
    info!(target: "profiles", player_id, profile_id = login.profile_id, "signed in by WFC login");
    session(&state, player_id).await
}

/// The most WFC profiles one [`known_wfc`] asks about.
pub const MAX_KNOWN: usize = 32;

#[derive(Deserialize)]
pub(super) struct KnownWfc {
    profile_ids: Vec<i32>,
}

/// Signed out, the launcher asks which of its game save's Miis (their WFC
/// profiles) play for a Recharged profile already, so a new profile isn't
/// made for one: `game` signs in again by that Mii's WFC login, `discord`
/// by Discord. It names nobody: a WFC profile id tells no one who plays.
pub(super) async fn known_wfc(
    State(state): State<Arc<ProfilesState>>,
    ClientAddress(address): ClientAddress,
    Json(known): Json<KnownWfc>,
) -> Response {
    if !state.known_asks.allow(address) {
        return json_error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many asks from this address; try again in a minute",
        );
    }
    if known.profile_ids.len() > MAX_KNOWN {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("at most {MAX_KNOWN} profile_ids"),
        );
    }
    match players::sign_in_kinds(&state.pool, &known.profile_ids).await {
        Ok(kinds) => {
            let profiles: Vec<_> = kinds
                .into_iter()
                .map(|(profile_id, kind)| json!({ "profile_id": profile_id, "sign_in": kind }))
                .collect();
            Json(json!({ "profiles": profiles })).into_response()
        }
        Err(error) => database_error(error),
    }
}

/// A new session for `player_id`, answered as a sign-in that ended.
async fn session(state: &ProfilesState, player_id: i64) -> Response {
    let token = match players::open_session(&state.pool, player_id).await {
        Ok(token) => token,
        Err(error) => return database_error(error),
    };
    match players::player(&state.pool, player_id).await {
        Ok(Some(player)) => signed_in(&token, &player, None),
        Ok(None) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "the player went away"),
        Err(error) => database_error(error),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn profile_names_are_trimmed_and_visible() {
        assert_eq!(profile_name("  Nelly ").as_deref(), Some("Nelly"));
        assert_eq!(
            profile_name("Mário Ünïcode ☆").as_deref(),
            Some("Mário Ünïcode ☆")
        );
        let longest = "ß".repeat(MAX_NAME);
        assert_eq!(profile_name(&longest), Some(longest.clone()));
        for name in [
            "",
            "   ",
            &"a".repeat(MAX_NAME + 1),
            "two\nlines",
            "tab\there",
            "bell\u{7}",
            "right\u{202e}left",
            "zero\u{200b}width",
        ] {
            assert!(profile_name(name).is_none(), "{name:?}");
        }
    }

    #[test]
    fn an_address_makes_a_few_players_an_hour() {
        let creations = PerAddress::creations();
        let (here, there) = ([203, 0, 113, 1].into(), [203, 0, 113, 2].into());
        assert!((0..NEW_PLAYERS_PER_WINDOW).all(|_| creations.allow(here)));
        assert!(!creations.allow(here));
        assert!(creations.allow(there));
        // One IPv6 connection's /64 is one address.
        let first: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let second: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let other: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert!((0..NEW_PLAYERS_PER_WINDOW).all(|_| creations.allow(first)));
        assert!(!creations.allow(second));
        assert!(creations.allow(other));
    }

    #[test]
    fn an_address_asks_for_known_miis_a_few_times_a_minute() {
        let asks = PerAddress::known();
        let (here, there) = ([203, 0, 113, 1].into(), [203, 0, 113, 2].into());
        assert!((0..KNOWN_PER_WINDOW).all(|_| asks.allow(here)));
        assert!(!asks.allow(here));
        assert!(asks.allow(there));
    }

    #[test]
    fn a_local_proxy_names_the_client() {
        let mut headers = HeaderMap::new();
        let public: IpAddr = [203, 0, 113, 7].into();
        let tunnel: IpAddr = [172, 17, 0, 1].into();
        assert_eq!(client_address(Some(public), &headers), public);
        assert_eq!(client_address(Some(tunnel), &headers), tunnel);

        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("1.1.1.1, 198.51.100.4"),
        );
        assert_eq!(
            client_address(Some(tunnel), &headers),
            IpAddr::from([198, 51, 100, 4])
        );
        headers.insert("cf-connecting-ip", HeaderValue::from_static("2001:db8::5"));
        assert_eq!(
            client_address(Some("::1".parse().unwrap()), &headers),
            "2001:db8::5".parse::<IpAddr>().unwrap()
        );
        // Someone on the internet can't name another address.
        assert_eq!(client_address(Some(public), &headers), public);
        // An IPv4 peer seen through IPv6.
        assert_eq!(
            client_address(
                Some("::ffff:203.0.113.7".parse().unwrap()),
                &HeaderMap::new()
            ),
            public
        );
        headers.insert(
            "cf-connecting-ip",
            HeaderValue::from_static("not an address"),
        );
        headers.remove("x-forwarded-for");
        assert_eq!(client_address(Some(tunnel), &headers), tunnel);
    }
}

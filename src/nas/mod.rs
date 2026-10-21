//! NAS (`naswii.nintendowifi.net`, HTTP): the account server. `POST /ac`
//! registers a console (`acctcreate`) and logs it in to a game (`login`),
//! handing out the token the console then logs in to GPCM with
//! (docs/login.md).
//!
//! Every form value and every reply value is in NAS's own [`base64`]. The
//! client retries a request on any status but 200, so refusals are 200 with
//! a `returncd`.

mod base64;
mod ticket;

use std::{collections::BTreeMap, collections::HashMap, sync::Arc};

use axum::{
    Form, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use sqlx::PgPool;
use time::{OffsetDateTime, macros::format_description};
use tracing::{debug, info, warn};

pub use ticket::{Ticket, TicketError, Tickets};

use crate::accounts;

/// What `login` gives GPCM as the place to log in to; DWC ignores it.
const LOCATOR: &str = "gamespy.com";

#[derive(Clone)]
struct NasState {
    pool: PgPool,
    tickets: Arc<Tickets>,
}

pub fn router(pool: PgPool, tickets: Arc<Tickets>) -> Router {
    Router::new()
        .route("/ac", post(account))
        .with_state(NasState { pool, tickets })
}

/// `returncd`s: two digits and up mean failure to the client.
mod returncd {
    pub const LOGGED_IN: &str = "001";
    pub const REGISTERED: &str = "002";
    /// A field is missing or does not parse.
    pub const BAD_REQUEST: &str = "103";
}

async fn account(
    State(state): State<NasState>,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let fields = decode(form);
    let action = fields
        .get("action")
        .map(|action| String::from_utf8_lossy(action).to_lowercase());
    debug!(target: "raw::nas", ?action, fields = ?fields.keys().collect::<Vec<_>>(), "request");

    let reply = match action.as_deref() {
        Some("acctcreate") => register(&state).await,
        Some("login") => login(&state, &fields).await,
        // `svcloc` and the rest: the game never asks for them.
        _ => {
            warn!(target: "nas", ?action, "unsupported action");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
    match reply {
        Ok(reply) => reply.into_response(),
        Err(error) => {
            warn!(target: "nas", %error, "database error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// The form's values, decoded. The first of a repeated key counts; keys
/// starting with `_` are other servers' extras.
fn decode(form: Vec<(String, String)>) -> HashMap<String, Vec<u8>> {
    let mut fields = HashMap::new();
    for (key, value) in form {
        if key.starts_with('_') || fields.contains_key(&key) {
            continue;
        }
        if let Some(value) = base64::decode(&value) {
            fields.insert(key, value);
        }
    }
    fields
}

/// `acctcreate`: a new account, whose userid the console keeps.
async fn register(state: &NasState) -> Result<Reply, sqlx::Error> {
    let user_id = accounts::register(&state.pool).await?;
    info!(target: "nas", user_id, "registered");
    Ok(Reply::new(returncd::REGISTERED).with("userid", user_id.to_string()))
}

/// `login`: a token and a challenge for GPCM.
async fn login(state: &NasState, fields: &HashMap<String, Vec<u8>>) -> Result<Reply, sqlx::Error> {
    let Some(login) = LoginFields::parse(fields) else {
        warn!(target: "nas", fields = ?fields.keys().collect::<Vec<_>>(), "bad login");
        return Ok(Reply::new(returncd::BAD_REQUEST));
    };
    accounts::log_in(&state.pool, login.user_id).await?;
    let ticket = Ticket {
        user_id: login.user_id,
        gsbrcd: login.gsbrcd,
        challenge: (0..8)
            .map(|_| char::from(rand::random_range(b'A'..=b'Z')))
            .collect(),
        ingamesn: login.ingamesn,
    };
    info!(target: "nas", user_id = ticket.user_id, gsbrcd = %ticket.gsbrcd, ingamesn = ?ticket.ingamesn, "logged in");
    Ok(Reply::new(returncd::LOGGED_IN)
        .with("challenge", ticket.challenge.clone())
        .with("locator", LOCATOR.into())
        .with("token", state.tickets.issue(&ticket)))
}

/// The fields of a `login` that matter.
#[derive(Debug, PartialEq)]
struct LoginFields {
    user_id: i64,
    gsbrcd: String,
    ingamesn: Option<String>,
}

impl LoginFields {
    fn parse(fields: &HashMap<String, Vec<u8>>) -> Option<Self> {
        let text = |key: &str| {
            fields
                .get(key)
                .and_then(|value| std::str::from_utf8(value).ok())
        };
        let user_id = text("userid")?
            .parse()
            .ok()
            .filter(|user_id| (0..accounts::USER_ID_LIMIT).contains(user_id))?;
        // The game code, then the Mii slot's id: R4QE and 7 characters.
        let gsbrcd = text("gsbrcd").filter(|gsbrcd| {
            (4..=16).contains(&gsbrcd.len())
                && gsbrcd.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })?;
        // UTF-16, big-endian from a Wii (`unitcd` 1), little-endian from a DS.
        let big_endian = text("unitcd") != Some("0");
        let ingamesn = fields.get("ingamesn").and_then(|bytes| {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|unit| {
                    let unit = [unit[0], unit[1]];
                    if big_endian {
                        u16::from_be_bytes(unit)
                    } else {
                        u16::from_le_bytes(unit)
                    }
                })
                .take_while(|&unit| unit != 0)
                .collect();
            String::from_utf16(&units)
                .ok()
                .filter(|name| !name.is_empty() && name.len() <= 64)
        });
        Some(Self {
            user_id,
            gsbrcd: gsbrcd.to_owned(),
            ingamesn,
        })
    }
}

/// A NAS reply: `key=value&…` in key order, each value in NAS Base64, ended
/// by a NUL as WiiLink's replies are.
struct Reply {
    fields: BTreeMap<&'static str, String>,
}

impl Reply {
    fn new(returncd: &str) -> Self {
        let datetime = OffsetDateTime::now_utc()
            .format(format_description!(
                "[year][month][day][hour][minute][second]"
            ))
            .expect("formats");
        Self {
            fields: BTreeMap::from([
                ("returncd", returncd.to_owned()),
                // The game's clock: seasons and striker of the day follow it.
                ("datetime", datetime),
                ("retry", "0".to_owned()),
            ]),
        }
    }

    fn with(mut self, key: &'static str, value: String) -> Self {
        self.fields.insert(key, value);
        self
    }
}

impl IntoResponse for Reply {
    fn into_response(self) -> Response {
        let mut body = self
            .fields
            .iter()
            .map(|(key, value)| format!("{key}={}", base64::encode(value)))
            .collect::<Vec<_>>()
            .join("&");
        body.push('\0');
        ([(header::CONTENT_TYPE, "text/plain")], body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &[u8])]) -> HashMap<String, Vec<u8>> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_vec()))
            .collect()
    }

    #[test]
    fn reads_what_the_port_sends_at_login() {
        let name: Vec<u8> = "Gold€n".encode_utf16().flat_map(u16::to_be_bytes).collect();
        let login = LoginFields::parse(&fields(&[
            ("userid", b"0001234567890"),
            ("gsbrcd", b"R4QEa1b2c3d"),
            ("unitcd", b"1"),
            ("ingamesn", &name),
        ]))
        .unwrap();
        assert_eq!(
            login,
            LoginFields {
                user_id: 1234567890,
                gsbrcd: "R4QEa1b2c3d".into(),
                ingamesn: Some("Gold€n".into()),
            }
        );
    }

    #[test]
    fn refuses_logins_it_cannot_use() {
        for bad in [
            fields(&[("gsbrcd", b"R4QEa1b2c3d")]),
            fields(&[("userid", b"8796093022208"), ("gsbrcd", b"R4QEa1b2c3d")]),
            fields(&[("userid", b"-1"), ("gsbrcd", b"R4QEa1b2c3d")]),
            fields(&[("userid", b"1"), ("gsbrcd", b"R4Q")]),
            fields(&[("userid", b"1"), ("gsbrcd", b"R4QE/../")]),
        ] {
            assert!(LoginFields::parse(&bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn decodes_the_first_of_each_key() {
        let form = vec![
            ("action".into(), "bG9naW4*".into()),
            ("action".into(), "YWNjdGNyZWF0ZQ**".into()),
            ("_extra".into(), "eA**".into()),
            ("broken".into(), "!".into()),
        ];
        assert_eq!(decode(form), fields(&[("action", b"login")]));
    }

    #[tokio::test]
    async fn replies_in_key_order_in_nas_base64() {
        let reply = Reply::new("001").with("token", "NDSx".into());
        let datetime = reply.fields["datetime"].clone();
        assert_eq!(datetime.len(), 14);
        let response = reply.into_response();
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/plain");
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            body,
            format!(
                "datetime={}&retry=MA**&returncd=MDAx&token=TkRTeA**\0",
                base64::encode(datetime)
            )
        );
    }
}

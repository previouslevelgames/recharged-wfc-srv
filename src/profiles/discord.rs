//! Discord sign-in: OAuth2's authorization code flow with PKCE and the
//! `identify` scope, which gives the user's id, username, display name and
//! avatar, and nothing else.
//!
//! The launcher's sign-in page sends the browser to Discord with a fresh
//! [`Authorization`], kept with the sign-in ([`super::sign_in`]). Discord
//! sends the browser back to `/sign-in/discord/callback` with a code, which
//! [`exchange`] trades for the user. The access token is used once and
//! forgotten.

use std::fmt;

use axum::http::StatusCode;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::distr::{Alphanumeric, SampleString};
use reqwest::Url;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::config::{Config, DiscordClient};

/// Alphanumeric secrets of ~256 bits; also the shortest PKCE verifier.
const SECRET_LEN: usize = 43;
/// Discord's user ids are snowflakes: 64-bit numbers as text.
const MAX_ID_LEN: usize = 20;

/// One trip to Discord's authorization page and back.
#[derive(Clone)]
pub struct Authorization {
    /// Goes to Discord and comes back: finds the sign-in again.
    pub state: String,
    /// PKCE: Discord gets its hash first, then the verifier with the code.
    pub verifier: String,
}

impl Authorization {
    pub fn new() -> Self {
        let secret = || Alphanumeric.sample_string(&mut rand::rng(), SECRET_LEN);
        Self {
            state: secret(),
            verifier: secret(),
        }
    }

    /// Discord's authorization page for this trip.
    pub fn url(&self, config: &Config, discord: &DiscordClient) -> Url {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(&self.verifier).as_slice());
        let mut url = discord.endpoints.authorize.clone();
        url.query_pairs_mut()
            .append_pair("client_id", &discord.client_id)
            .append_pair("redirect_uri", &config.discord_redirect_uri())
            .append_pair("response_type", "code")
            .append_pair("scope", "identify")
            .append_pair("state", &self.state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        url
    }
}

/// The Discord account that signed in.
#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    /// The user id.
    pub id: String,
    /// The unique username, which other players challenge by.
    pub username: String,
    /// The display name, or the username without one.
    pub display_name: String,
    pub avatar_url: String,
}

/// Trades the callback's `code` for the account that signed in.
pub async fn exchange(
    http: &reqwest::Client,
    config: &Config,
    discord: &DiscordClient,
    authorization: &Authorization,
    code: &str,
) -> Result<Account, SignInError> {
    #[derive(Deserialize)]
    struct Tokens {
        access_token: String,
    }

    let redirect_uri = config.discord_redirect_uri();
    let response = http
        .post(discord.endpoints.token.clone())
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", discord.client_id.as_str()),
            ("client_secret", discord.client_secret.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("code_verifier", authorization.verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|error| SignInError::Exchange(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        // Discord's error body names the problem, e.g. `invalid_grant`.
        let body = response.text().await.unwrap_or_default();
        let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
        return Err(SignInError::Exchange(format!("{status}: {body}")));
    }
    let tokens: Tokens = response
        .json()
        .await
        .map_err(|error| SignInError::Exchange(error.to_string()))?;

    let response = http
        .get(discord.endpoints.user.clone())
        .bearer_auth(&tokens.access_token)
        .send()
        .await
        .map_err(|error| SignInError::User(error.to_string()))?;
    if !response.status().is_success() {
        return Err(SignInError::User(response.status().to_string()));
    }
    let user: User = response
        .json()
        .await
        .map_err(|error| SignInError::User(error.to_string()))?;
    user.account()
}

/// The fields of Discord's user object that a sign-in keeps.
#[derive(Deserialize)]
struct User {
    id: String,
    username: String,
    global_name: Option<String>,
    avatar: Option<String>,
}

impl User {
    fn account(self) -> Result<Account, SignInError> {
        let Ok(id) = self.id.parse::<u64>() else {
            return Err(SignInError::User(format!("user id {}", self.id)));
        };
        if self.id.len() > MAX_ID_LEN || self.username.trim().is_empty() {
            return Err(SignInError::User("unexpected user".into()));
        }
        // An account without its own picture shows one of Discord's six
        // default avatars, picked from the id for usernames without a
        // discriminator.
        let avatar_url = match &self.avatar {
            Some(avatar) => {
                format!("https://cdn.discordapp.com/avatars/{id}/{avatar}.png?size=128")
            }
            None => format!(
                "https://cdn.discordapp.com/embed/avatars/{}.png",
                (id >> 22) % 6
            ),
        };
        let display_name = self
            .global_name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.username.clone());
        Ok(Account {
            id: self.id,
            username: self.username,
            display_name: display_name.chars().take(64).collect(),
            avatar_url,
        })
    }
}

/// Why a Discord sign-in failed. `Display` gives the details for the log;
/// [`SignInError::message`] is for the sign-in page.
#[derive(Debug)]
pub enum SignInError {
    /// Discord returned an error instead of a code, e.g. `access_denied`.
    Discord(String),
    /// No sign-in with the callback's state: it expired, or was used.
    Expired,
    /// The callback has no code.
    BadCallback,
    /// The token endpoint failed or could not be reached.
    Exchange(String),
    /// The user could not be read.
    User(String),
}

impl SignInError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Exchange(_) | Self::User(_) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::BAD_REQUEST,
        }
    }

    /// What the sign-in page tells the user.
    pub fn message(&self) -> String {
        match self {
            Self::Discord(error) if error == "access_denied" => "Sign-in was cancelled.".into(),
            Self::Discord(_) => "Discord could not sign you in. Please try again.".into(),
            Self::Expired => "This sign-in expired. Start again in Recharged's launcher.".into(),
            Self::BadCallback => "Discord's answer was incomplete. Please try again.".into(),
            Self::Exchange(_) | Self::User(_) => {
                "Discord could not be reached to finish signing in. Please try again.".into()
            }
        }
    }
}

impl fmt::Display for SignInError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discord(error) => write!(f, "Discord returned the error {error}"),
            Self::Expired => f.write_str("no sign-in with this state"),
            Self::BadCallback => f.write_str("the callback has no code"),
            Self::Exchange(detail) => write!(f, "token exchange failed: {detail}"),
            Self::User(detail) => write!(f, "reading the user failed: {detail}"),
        }
    }
}

impl std::error::Error for SignInError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::config::DiscordEndpoints;

    fn user(avatar: Option<&str>, global_name: Option<&str>) -> User {
        User {
            id: "80351110224678912".into(),
            username: "nelly".into(),
            global_name: global_name.map(str::to_owned),
            avatar: avatar.map(str::to_owned),
        }
    }

    #[test]
    fn an_account_has_a_name_and_a_picture() {
        let account = user(Some("8342729096ea3675442027381ff50dfe"), Some("Nelly"))
            .account()
            .unwrap();
        assert_eq!(account.username, "nelly");
        assert_eq!(account.display_name, "Nelly");
        assert_eq!(
            account.avatar_url,
            "https://cdn.discordapp.com/avatars/80351110224678912/8342729096ea3675442027381ff50dfe.png?size=128"
        );

        // No display name or picture of its own.
        let account = user(None, None).account().unwrap();
        assert_eq!(account.display_name, "nelly");
        assert_eq!(
            account.avatar_url,
            format!(
                "https://cdn.discordapp.com/embed/avatars/{}.png",
                (80351110224678912u64 >> 22) % 6
            )
        );
    }

    #[test]
    fn unexpected_users_are_refused() {
        let mut bad = user(None, None);
        bad.id = "not-a-number".into();
        assert!(bad.account().is_err());
        let mut bad = user(None, None);
        bad.username = " ".into();
        assert!(bad.account().is_err());
    }

    #[test]
    fn the_authorization_asks_for_identify_with_s256() {
        // RFC 7636, appendix B.
        let authorization = Authorization {
            state: "state".into(),
            verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into(),
        };
        let discord = DiscordClient {
            client_id: "123".into(),
            client_secret: "secret".into(),
            endpoints: DiscordEndpoints::default(),
        };
        let config = Config::new("https://wfc.example.com", Some(discord.clone()), false).unwrap();
        let url = authorization.url(&config, &discord);
        assert!(
            url.as_str()
                .starts_with("https://discord.com/oauth2/authorize?")
        );
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let get = |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get("scope"), Some("identify"));
        assert_eq!(
            get("code_challenge"),
            Some("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
        );
        assert_eq!(
            get("redirect_uri"),
            Some("https://wfc.example.com/sign-in/discord/callback")
        );
        assert_eq!(get("state"), Some("state"));
    }
}

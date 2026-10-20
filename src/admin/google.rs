//! Google sign-in: OpenID Connect's authorization code flow, with PKCE.
//!
//! `/admin/login` sends the browser to Google with a fresh [`Login`], whose
//! secrets wait in a cookie. Google sends the browser back to
//! `/admin/callback` with a code, which [`exchange`] trades for an ID token
//! straight from Google's token endpoint.

use std::fmt;

use axum::http::StatusCode;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::distr::{Alphanumeric, SampleString};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::Config;

/// Alphanumeric secrets of ~256 bits; also the shortest PKCE verifier.
const SECRET_LEN: usize = 43;
/// The `iss` of Google's ID tokens.
const ISSUERS: [&str; 2] = ["https://accounts.google.com", "accounts.google.com"];
/// Clock skew allowed on `exp`, in seconds.
const LEEWAY: i64 = 60;

/// The secrets of one sign-in, from `/admin/login` to the callback.
pub struct Login {
    /// Goes to Google and comes back: ties the callback to this browser.
    pub state: String,
    /// PKCE: Google gets its hash first, then the verifier with the code.
    verifier: String,
    /// Google copies it into the ID token: ties the token to this sign-in.
    nonce: String,
}

impl Login {
    pub fn new() -> Self {
        let secret = || Alphanumeric.sample_string(&mut rand::rng(), SECRET_LEN);
        Self {
            state: secret(),
            verifier: secret(),
            nonce: secret(),
        }
    }

    /// The cookie value: the three secrets, dot-separated.
    pub fn to_cookie(&self) -> String {
        format!("{}.{}.{}", self.state, self.verifier, self.nonce)
    }

    pub fn from_cookie(value: &str) -> Option<Self> {
        let mut parts = value.split('.');
        let (Some(state), Some(verifier), Some(nonce), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        Some(Self {
            state: state.to_owned(),
            verifier: verifier.to_owned(),
            nonce: nonce.to_owned(),
        })
    }

    /// Google's sign-in page for this sign-in.
    pub fn authorize_url(&self, config: &Config) -> Url {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(&self.verifier).as_slice());
        let mut url = config.google.authorize.clone();
        url.query_pairs_mut()
            .append_pair("client_id", config.client_id.as_str())
            .append_pair("redirect_uri", &config.redirect_uri())
            .append_pair("response_type", "code")
            .append_pair("scope", "openid email profile")
            .append_pair("state", &self.state)
            .append_pair("nonce", &self.nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("prompt", "select_account");
        url
    }
}

/// A Google account with a verified address, from its ID token.
#[derive(Clone, Debug, Serialize)]
pub struct Account {
    /// Lowercase.
    pub email: String,
    pub name: Option<String>,
    /// The profile photo's URL.
    pub picture: Option<String>,
}

/// Trades the callback's `code` for the account that signed in.
pub async fn exchange(
    http: &reqwest::Client,
    config: &Config,
    login: &Login,
    code: &str,
) -> Result<Account, SignInError> {
    #[derive(Deserialize)]
    struct Tokens {
        id_token: String,
    }

    let redirect_uri = config.redirect_uri();
    let response = http
        .post(config.google.token.clone())
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("code_verifier", login.verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|error| SignInError::Exchange(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        // Google's error body names the problem, e.g. `invalid_grant`. It is
        // pretty-printed JSON: keep the log entry on one line.
        let body = response.text().await.unwrap_or_default();
        let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
        return Err(SignInError::Exchange(format!("{status}: {body}")));
    }
    let tokens: Tokens = response
        .json()
        .await
        .map_err(|error| SignInError::Exchange(error.to_string()))?;
    verify(
        &tokens.id_token,
        &config.client_id,
        &login.nonce,
        OffsetDateTime::now_utc().unix_timestamp(),
    )
}

/// The claims of an ID token that [`verify`] reads.
#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: String,
    exp: i64,
    nonce: Option<String>,
    email: Option<String>,
    #[serde(default)]
    email_verified: bool,
    name: Option<String>,
    picture: Option<String>,
}

/// Checks an ID token's claims at `now` (Unix seconds).
///
/// The signature is not checked: the token came straight from Google's token
/// endpoint over TLS, which OpenID Connect accepts in its place (Core 1.0,
/// 3.1.3.7).
fn verify(id_token: &str, client_id: &str, nonce: &str, now: i64) -> Result<Account, SignInError> {
    let mut parts = id_token.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(SignInError::Token("not a JWT"));
    };
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| SignInError::Token("payload is not Base64url"))?;
    let claims: Claims =
        serde_json::from_slice(&payload).map_err(|_| SignInError::Token("unexpected claims"))?;

    if !ISSUERS.contains(&claims.iss.as_str()) {
        return Err(SignInError::Token("issuer"));
    }
    if claims.aud != client_id {
        return Err(SignInError::Token("audience"));
    }
    if claims.exp + LEEWAY < now {
        return Err(SignInError::Token("expired"));
    }
    if claims.nonce.as_deref() != Some(nonce) {
        return Err(SignInError::Token("nonce"));
    }
    let email = claims
        .email
        .filter(|_| claims.email_verified)
        .ok_or(SignInError::Unverified)?;
    Ok(Account {
        email: email.to_lowercase(),
        name: claims.name,
        picture: claims.picture,
    })
}

/// Why a sign-in failed. `Display` gives the details for the log;
/// [`SignInError::message`] is for the sign-in page.
#[derive(Debug)]
pub enum SignInError {
    /// Google returned an error instead of a code, e.g. `access_denied`.
    Google(String),
    /// No sign-in cookie: it expired, or the sign-in began on another host.
    NoLogin,
    /// The callback's state differs from the cookie's, or it has no code.
    BadCallback,
    /// The token endpoint failed or could not be reached.
    Exchange(String),
    /// The ID token failed a check.
    Token(&'static str),
    /// The account has no verified address.
    Unverified,
    /// A verified account that is not in `ADMIN_GOOGLE_EMAILS`.
    NotAdmin(String),
}

impl SignInError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Exchange(_) => StatusCode::BAD_GATEWAY,
            Self::Unverified | Self::NotAdmin(_) => StatusCode::FORBIDDEN,
            _ => StatusCode::BAD_REQUEST,
        }
    }

    /// What the sign-in page tells the user.
    pub fn message(&self) -> String {
        match self {
            Self::Google(error) if error == "access_denied" => "Sign-in was cancelled.".into(),
            Self::Google(_) => "Google could not sign you in. Please try again.".into(),
            Self::NoLogin => "This sign-in expired. Please try again.".into(),
            Self::BadCallback => {
                "This sign-in did not start in this browser. Please try again.".into()
            }
            Self::Exchange(_) => {
                "Google could not be reached to finish signing in. Please try again.".into()
            }
            Self::Token(_) => "Google's answer could not be verified. Please try again.".into(),
            Self::Unverified => "This Google account has no verified email address.".into(),
            Self::NotAdmin(email) => format!("{email} is not an admin of this server."),
        }
    }
}

impl fmt::Display for SignInError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Google(error) => write!(f, "Google returned the error {error}"),
            Self::NoLogin => f.write_str("no sign-in cookie"),
            Self::BadCallback => f.write_str("the callback's state or code does not match"),
            Self::Exchange(detail) => write!(f, "token exchange failed: {detail}"),
            Self::Token(check) => write!(f, "the ID token failed the {check} check"),
            Self::Unverified => f.write_str("the account has no verified address"),
            Self::NotAdmin(email) => write!(f, "{email} is not in ADMIN_GOOGLE_EMAILS"),
        }
    }
}

impl std::error::Error for SignInError {}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT_ID: &str = "id.apps.googleusercontent.com";
    const NOW: i64 = 1_800_000_000;

    fn token(claims: serde_json::Value) -> String {
        let encode = |value: &serde_json::Value| URL_SAFE_NO_PAD.encode(value.to_string());
        format!(
            "{}.{}.signature",
            encode(&serde_json::json!({"alg": "RS256"})),
            encode(&claims)
        )
    }

    fn claims() -> serde_json::Value {
        serde_json::json!({
            "iss": "https://accounts.google.com",
            "aud": CLIENT_ID,
            "sub": "1234567890",
            "exp": NOW + 3600,
            "nonce": "the-nonce",
            "email": "Admin@Example.com",
            "email_verified": true,
            "name": "Admin",
            "picture": "https://lh3.googleusercontent.com/a/photo",
        })
    }

    fn check(claims: serde_json::Value) -> Result<Account, SignInError> {
        verify(&token(claims), CLIENT_ID, "the-nonce", NOW)
    }

    #[test]
    fn accepts_a_valid_token() {
        let account = check(claims()).unwrap();
        assert_eq!(account.email, "admin@example.com");
        assert_eq!(account.name.as_deref(), Some("Admin"));
    }

    #[test]
    fn refuses_tokens_for_someone_else() {
        for (claim, value) in [
            ("iss", serde_json::json!("https://evil.example.com")),
            ("aud", serde_json::json!("another-client")),
            ("exp", serde_json::json!(NOW - LEEWAY - 1)),
            ("nonce", serde_json::json!("another-nonce")),
        ] {
            let mut claims = claims();
            claims[claim] = value;
            assert!(
                matches!(check(claims), Err(SignInError::Token(_))),
                "{claim}"
            );
        }
    }

    #[test]
    fn refuses_unverified_addresses() {
        let mut claims = claims();
        claims["email_verified"] = serde_json::json!(false);
        assert!(matches!(check(claims), Err(SignInError::Unverified)));
    }

    #[test]
    fn refuses_malformed_tokens() {
        for id_token in ["", "a.b", "a.!!!.c", "a.b.c.d"] {
            assert!(matches!(
                verify(id_token, CLIENT_ID, "the-nonce", NOW),
                Err(SignInError::Token(_))
            ));
        }
    }

    #[test]
    fn a_login_survives_its_cookie() {
        let login = Login::new();
        let back = Login::from_cookie(&login.to_cookie()).unwrap();
        assert_eq!(
            (back.state, back.verifier, back.nonce),
            (login.state, login.verifier, login.nonce)
        );
        assert!(Login::from_cookie("a.b").is_none());
        assert!(Login::from_cookie("a.b.c.d").is_none());
    }

    #[test]
    fn the_challenge_is_s256_of_the_verifier() {
        // RFC 7636, appendix B.
        let login = Login {
            state: "state".into(),
            verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into(),
            nonce: "nonce".into(),
        };
        let config = Config::new(
            "https://wfc.example.com",
            CLIENT_ID,
            "secret",
            "a@example.com",
        )
        .unwrap();
        let url = login.authorize_url(&config);
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let get = |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        assert!(
            url.as_str()
                .starts_with("https://accounts.google.com/o/oauth2/v2/auth?")
        );
        assert_eq!(
            get("code_challenge"),
            Some("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
        );
        assert_eq!(
            get("redirect_uri"),
            Some("https://wfc.example.com/admin/callback")
        );
        assert_eq!(get("state"), Some("state"));
        assert_eq!(get("nonce"), Some("nonce"));
    }
}

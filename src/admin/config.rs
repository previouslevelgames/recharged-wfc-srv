//! `/admin`'s settings, from the environment.

use std::{env, fmt};

use reqwest::Url;

/// Every variable `/admin` reads. Set all of them, or none to run the server
/// without `/admin`.
pub const VARIABLES: [&str; 4] = [
    "PUBLIC_URL",
    "GOOGLE_CLIENT_ID",
    "GOOGLE_CLIENT_SECRET",
    "ADMIN_GOOGLE_EMAILS",
];

/// What `/admin` needs: where browsers reach it, the Google OAuth client and
/// who may sign in.
#[derive(Clone)]
pub struct Config {
    /// The origin browsers use, e.g. `https://wfc.example.com`, without a
    /// trailing slash. Google returns to [`Config::redirect_uri`] on it.
    pub public_url: String,
    pub client_id: String,
    pub client_secret: String,
    /// The Google accounts that may sign in, lowercase.
    pub admins: Vec<String>,
    pub google: GoogleEndpoints,
}

/// Google's OpenID Connect endpoints. Tests point them at a fake.
#[derive(Clone, Debug)]
pub struct GoogleEndpoints {
    pub authorize: Url,
    pub token: Url,
}

impl Default for GoogleEndpoints {
    fn default() -> Self {
        Self {
            authorize: Url::parse("https://accounts.google.com/o/oauth2/v2/auth").unwrap(),
            token: Url::parse("https://oauth2.googleapis.com/token").unwrap(),
        }
    }
}

impl Config {
    /// Reads [`VARIABLES`]. `Ok(None)` when none of them is set; an error
    /// when only some are, or one is invalid.
    pub fn from_env() -> Result<Option<Self>, String> {
        Self::from_vars(|name| env::var(name).ok())
    }

    fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let values = VARIABLES.map(|name| var(name).filter(|value| !value.trim().is_empty()));
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        let missing: Vec<&str> = VARIABLES
            .iter()
            .zip(&values)
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| *name)
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "/admin needs {} as well (or unset {} to run without it)",
                missing.join(", "),
                VARIABLES.join(", "),
            ));
        }
        let [public_url, client_id, client_secret, admins] = values.map(Option::unwrap);
        Self::new(&public_url, client_id.trim(), client_secret.trim(), &admins).map(Some)
    }

    /// Checks and normalizes the settings. `admins` is a comma-separated
    /// list of Google addresses.
    pub fn new(
        public_url: &str,
        client_id: &str,
        client_secret: &str,
        admins: &str,
    ) -> Result<Self, String> {
        let admins: Vec<String> = admins
            .split(',')
            .map(|email| email.trim().to_lowercase())
            .filter(|email| !email.is_empty())
            .collect();
        if admins.is_empty() || admins.iter().any(|email| !email.contains('@')) {
            return Err("ADMIN_GOOGLE_EMAILS: give comma-separated Google addresses".into());
        }
        Ok(Self {
            public_url: origin(public_url)?,
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
            admins,
            google: GoogleEndpoints::default(),
        })
    }

    /// Where Google sends the browser back to. Register it in the OAuth
    /// client's "Authorized redirect URIs".
    pub fn redirect_uri(&self) -> String {
        format!("{}/admin/callback", self.public_url)
    }

    /// Whether cookies need `Secure`: everywhere but plain-HTTP localhost.
    pub fn secure(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}

/// The secret stays out of logs.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("public_url", &self.public_url)
            .field("client_id", &self.client_id)
            .field("admins", &self.admins)
            .field("google", &self.google)
            .finish_non_exhaustive()
    }
}

/// `PUBLIC_URL` as an origin. Google takes `http://` redirect URIs only for
/// localhost, so anything else must be `https://`.
fn origin(public_url: &str) -> Result<String, String> {
    let url = Url::parse(public_url.trim()).map_err(|error| format!("PUBLIC_URL: {error}"))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback) {
        return Err("PUBLIC_URL: use https:// (http:// works only for localhost)".into());
    }
    if url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
    {
        return Err(
            "PUBLIC_URL: give only the scheme and host, e.g. https://wfc.example.com".into(),
        );
    }
    Ok(url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn from(vars: &[(&str, &str)]) -> Result<Option<Config>, String> {
        let vars: HashMap<&str, &str> = vars.iter().copied().collect();
        Config::from_vars(|name| vars.get(name).map(|value| value.to_string()))
    }

    const ALL: [(&str, &str); 4] = [
        ("PUBLIC_URL", "https://wfc.example.com/"),
        ("GOOGLE_CLIENT_ID", "id.apps.googleusercontent.com"),
        ("GOOGLE_CLIENT_SECRET", "the-client-secret"),
        (
            "ADMIN_GOOGLE_EMAILS",
            " Admin@Example.com, second@example.com ,",
        ),
    ];

    #[test]
    fn no_variables_disable_admin() {
        assert!(from(&[]).unwrap().is_none());
        assert!(from(&[("PUBLIC_URL", " ")]).unwrap().is_none());
    }

    #[test]
    fn some_variables_name_the_missing_ones() {
        let error = from(&ALL[..2]).unwrap_err();
        assert!(
            error.contains("GOOGLE_CLIENT_SECRET, ADMIN_GOOGLE_EMAILS"),
            "{error}"
        );
    }

    #[test]
    fn all_variables_are_normalized() {
        let config = from(&ALL).unwrap().unwrap();
        assert_eq!(config.public_url, "https://wfc.example.com");
        assert_eq!(
            config.redirect_uri(),
            "https://wfc.example.com/admin/callback"
        );
        assert_eq!(config.admins, ["admin@example.com", "second@example.com"]);
        assert!(config.secure());
        assert!(!format!("{config:?}").contains("the-client-secret"));
    }

    #[test]
    fn plain_http_is_for_localhost_only() {
        for url in [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "http://[::1]:8080",
        ] {
            let config = Config::new(url, "id", "secret", "a@example.com").unwrap();
            assert_eq!(config.public_url, url);
            assert!(!config.secure());
        }
        assert!(Config::new("http://wfc.example.com", "id", "secret", "a@example.com").is_err());
        assert!(Config::new("http://192.168.1.2:8080", "id", "secret", "a@example.com").is_err());
    }

    #[test]
    fn public_url_is_an_origin() {
        for url in [
            "https://wfc.example.com/admin",
            "https://wfc.example.com/?x=1",
            "https://user@wfc.example.com",
            "wfc.example.com",
        ] {
            assert!(
                Config::new(url, "id", "secret", "a@example.com").is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn admins_are_addresses() {
        for admins in ["", " , ", "admin"] {
            assert!(Config::new("https://wfc.example.com", "id", "secret", admins).is_err());
        }
    }
}

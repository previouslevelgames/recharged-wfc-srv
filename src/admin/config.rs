//! `/admin`'s settings, from the environment.

use std::{env, fmt, path::PathBuf};

use reqwest::Url;

/// Every variable `/admin` needs. Set all of them, or none of the Google
/// ones to run the server without `/admin`: `PUBLIC_URL` serves the profile
/// service too. It also reads `LOG_DIR`, for the Logs page, and
/// `OWNER_GOOGLE_EMAILS`, for the owners' pages.
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
    /// The accounts above admins, lowercase: they sign in too, and alone see
    /// the Maintenance page. None when `OWNER_GOOGLE_EMAILS` is unset.
    pub owners: Vec<String>,
    pub google: GoogleEndpoints,
    /// Where the server writes its daily log files, if anywhere.
    pub log_dir: Option<PathBuf>,
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
        if values[1..].iter().all(Option::is_none) {
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
                VARIABLES[1..].join(", "),
            ));
        }
        let [public_url, client_id, client_secret, admins] = values.map(Option::unwrap);
        let mut config = Self::new(&public_url, client_id.trim(), client_secret.trim(), &admins)?;
        config.log_dir = var("LOG_DIR")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from);
        if let Some(owners) = var("OWNER_GOOGLE_EMAILS").filter(|owners| !owners.trim().is_empty())
        {
            config.owners = emails(&owners).ok_or(
                "OWNER_GOOGLE_EMAILS: give comma-separated Google addresses, or leave it unset",
            )?;
        }
        Ok(Some(config))
    }

    /// Checks and normalizes the settings. `admins` is a comma-separated
    /// list of Google addresses.
    pub fn new(
        public_url: &str,
        client_id: &str,
        client_secret: &str,
        admins: &str,
    ) -> Result<Self, String> {
        let admins =
            emails(admins).ok_or("ADMIN_GOOGLE_EMAILS: give comma-separated Google addresses")?;
        Ok(Self {
            public_url: origin(public_url)?,
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
            admins,
            owners: Vec::new(),
            google: GoogleEndpoints::default(),
            log_dir: None,
        })
    }

    /// Whether `email`, lowercase, may sign in: an admin's or an owner's.
    pub fn may_sign_in(&self, email: &str) -> bool {
        self.admins
            .iter()
            .chain(&self.owners)
            .any(|admin| admin == email)
    }

    /// Whether `email`, lowercase, is an owner's.
    pub fn is_owner(&self, email: &str) -> bool {
        self.owners.iter().any(|owner| owner == email)
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
            .field("owners", &self.owners)
            .field("google", &self.google)
            .field("log_dir", &self.log_dir)
            .finish_non_exhaustive()
    }
}

/// A comma-separated list of addresses, lowercase; `None` for none, or for
/// one that is not an address.
fn emails(list: &str) -> Option<Vec<String>> {
    let emails: Vec<String> = list
        .split(',')
        .map(|email| email.trim().to_lowercase())
        .filter(|email| !email.is_empty())
        .collect();
    (!emails.is_empty() && emails.iter().all(|email| email.contains('@'))).then_some(emails)
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
        // The profile service's address alone.
        assert!(
            from(&[("PUBLIC_URL", "https://wfc.example.com")])
                .unwrap()
                .is_none()
        );
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
        assert!(config.owners.is_empty());
        assert!(config.secure());
        assert!(!format!("{config:?}").contains("the-client-secret"));
        assert_eq!(config.log_dir, None);

        let config = from(&[ALL.as_slice(), &[("LOG_DIR", "/var/log/wfc")]].concat())
            .unwrap()
            .unwrap();
        assert_eq!(config.log_dir, Some("/var/log/wfc".into()));
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

    #[test]
    fn owners_sign_in_and_are_above_admins() {
        let config = from(
            &[
                ALL.as_slice(),
                &[("OWNER_GOOGLE_EMAILS", " Owner@Example.com ")],
            ]
            .concat(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.owners, ["owner@example.com"]);
        assert!(config.may_sign_in("owner@example.com") && config.is_owner("owner@example.com"));
        assert!(config.may_sign_in("admin@example.com") && !config.is_owner("admin@example.com"));
        assert!(!config.may_sign_in("someone@example.com"));

        let blank = from(&[ALL.as_slice(), &[("OWNER_GOOGLE_EMAILS", " ")]].concat());
        assert!(blank.unwrap().unwrap().owners.is_empty());
        let error = from(&[ALL.as_slice(), &[("OWNER_GOOGLE_EMAILS", "owner")]].concat())
            .err()
            .unwrap();
        assert!(error.starts_with("OWNER_GOOGLE_EMAILS"), "{error}");
    }
}

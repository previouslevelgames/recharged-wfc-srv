//! The profile service's settings, from the environment.

use std::{env, fmt, net::IpAddr};

use reqwest::Url;

/// Every variable the profile service reads. `PUBLIC_URL` is shared with
/// `/admin`; Discord's client, test sign-in, or both turn the service on. It
/// also reads `SERVER_LABEL`.
pub const VARIABLES: [&str; 4] = [
    "PUBLIC_URL",
    "DISCORD_CLIENT_ID",
    "DISCORD_CLIENT_SECRET",
    "PROFILES_TEST_SIGN_IN",
];

/// What the profile service needs: where launchers and browsers reach it and
/// how players sign in.
#[derive(Clone)]
pub struct Config {
    /// The origin launchers and browsers use, e.g. `https://wfc.example.com`,
    /// without a trailing slash.
    pub public_url: String,
    /// Discord's OAuth client: signing in with Discord.
    pub discord: Option<DiscordClient>,
    /// Signing in with a name alone, so that a second game on one PC has a
    /// player of its own. For local and test servers only: anyone can be
    /// anyone.
    pub test_sign_in: bool,
    /// `SERVER_LABEL`, which the sign-in pages show, e.g. `DEV`.
    pub label: Option<String>,
}

#[derive(Clone)]
pub struct DiscordClient {
    pub client_id: String,
    pub client_secret: String,
    pub endpoints: DiscordEndpoints,
}

/// Discord's OAuth endpoints and the signed-in user's. Tests point them at a
/// fake.
#[derive(Clone, Debug)]
pub struct DiscordEndpoints {
    pub authorize: Url,
    pub token: Url,
    pub user: Url,
}

impl Default for DiscordEndpoints {
    fn default() -> Self {
        Self {
            authorize: Url::parse("https://discord.com/oauth2/authorize").unwrap(),
            token: Url::parse("https://discord.com/api/oauth2/token").unwrap(),
            user: Url::parse("https://discord.com/api/users/@me").unwrap(),
        }
    }
}

impl Config {
    /// Reads [`VARIABLES`]. `Ok(None)` when neither Discord nor test sign-in
    /// is set up; an error when a setting is incomplete or invalid.
    pub fn from_env() -> Result<Option<Self>, String> {
        Self::from_vars(|name| env::var(name).ok())
    }

    fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let get = |name: &str| {
            var(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let discord = match (get("DISCORD_CLIENT_ID"), get("DISCORD_CLIENT_SECRET")) {
            (Some(client_id), Some(client_secret)) => Some(DiscordClient {
                client_id,
                client_secret,
                endpoints: DiscordEndpoints::default(),
            }),
            (None, None) => None,
            _ => {
                return Err(
                    "Discord sign-in needs both DISCORD_CLIENT_ID and DISCORD_CLIENT_SECRET".into(),
                );
            }
        };
        let test_sign_in = match get("PROFILES_TEST_SIGN_IN").map(|value| value.to_lowercase()) {
            None => false,
            Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on") => true,
            Some(value) if matches!(value.as_str(), "0" | "false" | "no" | "off") => false,
            Some(value) => {
                return Err(format!(
                    "PROFILES_TEST_SIGN_IN: {value} is not true or false"
                ));
            }
        };
        if discord.is_none() && !test_sign_in {
            return Ok(None);
        }
        let public_url = get("PUBLIC_URL")
            .ok_or("profiles need PUBLIC_URL, the address launchers and browsers use")?;
        let mut config = Self::new(&public_url, discord, test_sign_in)?;
        config.label = crate::html::server_label(&var)?;
        Ok(Some(config))
    }

    pub fn new(
        public_url: &str,
        discord: Option<DiscordClient>,
        test_sign_in: bool,
    ) -> Result<Self, String> {
        Ok(Self {
            public_url: origin(public_url)?,
            discord,
            test_sign_in,
            label: None,
        })
    }

    /// Where Discord sends the browser back to. Add it to the Discord app's
    /// OAuth2 redirects.
    pub fn discord_redirect_uri(&self) -> String {
        format!("{}/sign-in/discord/callback", self.public_url)
    }
}

/// The secret stays out of logs.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("public_url", &self.public_url)
            .field(
                "discord",
                &self.discord.as_ref().map(|discord| &discord.client_id),
            )
            .field("test_sign_in", &self.test_sign_in)
            .field("label", &self.label)
            .finish()
    }
}

/// `PUBLIC_URL` as an origin. Plain `http://` only for this computer and the
/// home network, where a test server runs; anywhere else the launcher's
/// tokens would cross the internet unencrypted.
fn origin(public_url: &str) -> Result<String, String> {
    let url = Url::parse(public_url.trim()).map_err(|error| format!("PUBLIC_URL: {error}"))?;
    let local = match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| match ip {
                IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
                IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
            }),
        None => false,
    };
    if !(url.scheme() == "https" || url.scheme() == "http" && local) {
        return Err("PUBLIC_URL: use https:// (http:// works only on a local network)".into());
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

    #[test]
    fn nothing_to_sign_in_with_disables_profiles() {
        assert!(from(&[]).unwrap().is_none());
        assert!(
            from(&[("PUBLIC_URL", "https://wfc.example.com")])
                .unwrap()
                .is_none()
        );
        assert!(
            from(&[("PROFILES_TEST_SIGN_IN", "false")])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn discord_needs_its_secret_and_the_public_url() {
        assert!(from(&[("DISCORD_CLIENT_ID", "123")]).is_err());
        assert!(from(&[("DISCORD_CLIENT_ID", "123"), ("DISCORD_CLIENT_SECRET", "s")]).is_err());
        let config = from(&[
            ("PUBLIC_URL", "https://wfc.example.com/"),
            ("DISCORD_CLIENT_ID", " 123 "),
            ("DISCORD_CLIENT_SECRET", "the-secret"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(config.public_url, "https://wfc.example.com");
        assert_eq!(config.discord.as_ref().unwrap().client_id, "123");
        assert_eq!(
            config.discord_redirect_uri(),
            "https://wfc.example.com/sign-in/discord/callback"
        );
        assert!(!config.test_sign_in);
        assert!(!format!("{config:?}").contains("the-secret"));
    }

    #[test]
    fn test_sign_in_alone_turns_profiles_on() {
        let config = from(&[
            ("PUBLIC_URL", "http://127.0.0.1:8080"),
            ("PROFILES_TEST_SIGN_IN", "True"),
        ])
        .unwrap()
        .unwrap();
        assert!(config.test_sign_in);
        assert!(config.discord.is_none());
        assert!(
            from(&[
                ("PUBLIC_URL", "http://127.0.0.1"),
                ("PROFILES_TEST_SIGN_IN", "maybe")
            ])
            .is_err()
        );
    }

    #[test]
    fn plain_http_is_for_local_networks_only() {
        for url in [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "http://[::1]:8080",
            "http://192.168.1.20:8080",
            "http://10.0.0.5",
        ] {
            assert_eq!(Config::new(url, None, true).unwrap().public_url, url);
        }
        for url in ["http://wfc.example.com", "http://203.0.113.10:8080"] {
            assert!(Config::new(url, None, true).is_err(), "{url}");
        }
        for url in [
            "https://wfc.example.com/api",
            "https://wfc.example.com/?x=1",
            "https://user@wfc.example.com",
            "wfc.example.com",
        ] {
            assert!(Config::new(url, None, true).is_err(), "{url}");
        }
    }
}

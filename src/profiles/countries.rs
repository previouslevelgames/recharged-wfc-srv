//! A player's country, from the address their launcher or game connects
//! from, for the flag the game menu and the launcher show beside a name
//! (docs/profiles.md, "Countries"). The lookup uses DB-IP's free "IP to
//! Country Lite" database (CC BY 4.0, <https://db-ip.com>), which the image
//! carries; only the two letters of ISO 3166-1 are kept, never the address.

use std::{net::IpAddr, path::Path};

use maxminddb::{PathElement, Reader};
use tracing::{info, warn};

/// Where the image puts the database; `GEOIP_COUNTRY_DB` names another.
pub const DEFAULT_DATABASE: &str = "/usr/local/share/recharged-wfc-srv/dbip-country-lite.mmdb";

/// The country database, or none: then nobody has a country.
#[derive(Default)]
pub struct Countries {
    reader: Option<Reader<Vec<u8>>>,
    /// Tests' countries, by address.
    #[cfg(test)]
    fixed: std::collections::HashMap<IpAddr, &'static str>,
}

impl Countries {
    /// The database at `path`; none when it is missing or unreadable.
    pub fn open(path: &Path) -> Self {
        match Reader::open_readfile(path) {
            Ok(reader) => {
                info!(target: "profiles", path = %path.display(), "countries from the database");
                Self {
                    reader: Some(reader),
                    #[cfg(test)]
                    fixed: Default::default(),
                }
            }
            Err(error) => {
                warn!(target: "profiles", path = %path.display(), %error, "no country database: no flags");
                Self::default()
            }
        }
    }

    /// Countries for tests: `address` is in `country`.
    #[cfg(test)]
    pub fn fixed(countries: &[(IpAddr, &'static str)]) -> Self {
        Self {
            reader: None,
            fixed: countries.iter().copied().collect(),
        }
    }

    /// The country of `address`: two capital letters, none when unknown
    /// (a local address, or none in the database).
    pub fn of(&self, address: IpAddr) -> Option<String> {
        #[cfg(test)]
        if let Some(country) = self.fixed.get(&address) {
            return Some((*country).to_owned());
        }
        let found = self.reader.as_ref()?.lookup(address.to_canonical()).ok()?;
        let country: String = found
            .decode_path(&[PathElement::Key("country"), PathElement::Key("iso_code")])
            .ok()??;
        is_country(&country).then_some(country)
    }
}

/// Two capital letters, as ISO 3166-1 alpha-2 codes are.
pub fn is_country(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_country_is_two_capital_letters() {
        assert!(is_country("CH"));
        for bad in ["", "C", "ch", "CHE", "C1", "ÄT"] {
            assert!(!is_country(bad), "{bad}");
        }
    }

    #[test]
    fn without_a_database_nobody_has_a_country() {
        let countries = Countries::open(Path::new("/nonexistent/countries.mmdb"));
        assert_eq!(countries.of("8.8.8.8".parse().unwrap()), None);
        let fixed = Countries::fixed(&[("203.0.113.7".parse().unwrap(), "CH")]);
        assert_eq!(fixed.of("203.0.113.7".parse().unwrap()), Some("CH".into()));
        assert_eq!(fixed.of("203.0.113.8".parse().unwrap()), None);
    }

    /// With the database `just countries` downloads, real addresses.
    #[test]
    fn the_database_knows_real_addresses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/dbip-country-lite.mmdb");
        if !path.exists() {
            return;
        }
        let countries = Countries::open(&path);
        assert_eq!(
            countries.of("8.8.8.8".parse().unwrap()).as_deref(),
            Some("US")
        );
        assert_eq!(countries.of("127.0.0.1".parse().unwrap()), None);
        assert_eq!(countries.of("192.168.1.10".parse().unwrap()), None);
    }
}

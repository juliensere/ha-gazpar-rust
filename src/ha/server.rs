//! Ce que Home Assistant dit de lui-même : son fuseau horaire (commande
//! `get_config`) et sa version (annoncée à l'authentification).
//!
//! Le fuseau de HA est celui des jours : `period: "day"` regroupe les
//! statistiques selon lui (PLAN.md §2.3). Il n'y a donc pas d'option de
//! fuseau : on le lit à chaque connexion, et on le convertit avec la base de
//! fuseaux embarquée dans le programme (`jiff`, `tzdb-bundle-always`), sans
//! dépendre de `/usr/share/zoneinfo` (absent de l'image `scratch`).
//!
//! Décisions par fonctions pures ; seule [`fetch_time_zone`] envoie une
//! commande.
//!
//! Dépendances : `ha::session`, `error`, `serde_json`, `jiff`.

use jiff::tz::TimeZone;
use serde_json::{Value, json};

use crate::error::Error;
use crate::ha::session::{Session, Transport};

/// Longueur maximale d'un nom de fuseau recopié dans une erreur.
const MAX_NAME_CHARS: usize = 64;

/// Demande `get_config` et renvoie le fuseau de Home Assistant.
pub(crate) fn fetch_time_zone<T: Transport>(session: &mut Session<T>) -> Result<TimeZone, Error> {
    let config = session.command(json!({ "type": "get_config" }))?;
    time_zone_from_config(&config)
}

/// Fuseau de la réponse à `get_config` (champ `time_zone`, nom IANA).
pub fn time_zone_from_config(config: &Value) -> Result<TimeZone, Error> {
    let Some(name) = config.get("time_zone").and_then(Value::as_str) else {
        return Err(Error::Protocol("réponse sans time_zone".into()));
    };
    TimeZone::get(name).map_err(|_| {
        let shown: String = name.chars().take(MAX_NAME_CHARS).collect();
        Error::HomeAssistant(format!(
            "fuseau horaire {shown:?} inconnu de ha-gazpar (réglage « Fuseau horaire » \
             de Home Assistant) : choisissez un fuseau IANA, par exemple Europe/Paris"
        ))
    })
}

/// La version `version` de Home Assistant est-elle antérieure à `reference`
/// (`AAAA.M`) ? Seuls les deux premiers nombres comptent, comparés comme
/// des nombres (`2026.10` est postérieure à `2026.9`). `None` si l'une des
/// deux est illisible (`inconnue`).
fn is_older_than(version: &str, reference: &str) -> Option<bool> {
    Some(year_month(version)? < year_month(reference)?)
}

/// Home Assistant `version` est-il antérieur à la version de référence du
/// programme ([`crate::HA_REFERENCE`]) ? Seul chemin de cette comparaison,
/// pour `app` comme pour les essais. `None` si la version est illisible.
pub fn older_than_reference(version: &str) -> Option<bool> {
    is_older_than(version, crate::HA_REFERENCE)
}

/// `(année, mois)` d'une version `AAAA.M[.N…]` ; le mois peut être suivi
/// d'un suffixe (`2026.10b0`).
fn year_month(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.trim().split('.');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?;
    let digits = month
        .find(|c: char| !c.is_ascii_digit())
        .map_or(month, |end| &month[..end]);
    Some((year, digits.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::session::fake::{FakeTransport, ok};
    use crate::secret::Secret;

    #[test]
    fn time_zone_from_typical_config() {
        let config = json!({
            "latitude": 48.85, "longitude": 2.35, "elevation": 35,
            "unit_system": { "length": "km" },
            "location_name": "Maison",
            "time_zone": "Europe/Paris",
            "version": "2026.9.4",
        });
        let tz = time_zone_from_config(&config).unwrap();
        assert_eq!(tz.iana_name(), Some("Europe/Paris"));
        let utc = time_zone_from_config(&json!({ "time_zone": "UTC" })).unwrap();
        assert_eq!(utc.iana_name(), Some("UTC"));
    }

    #[test]
    fn missing_time_zone_is_a_protocol_error() {
        for config in [
            json!({}),
            json!(null),
            json!({ "time_zone": null }),
            json!({ "time_zone": 1 }),
        ] {
            assert!(
                matches!(time_zone_from_config(&config), Err(Error::Protocol(_))),
                "{config}"
            );
        }
    }

    #[test]
    fn unknown_time_zone_is_named_in_the_error() {
        let e = time_zone_from_config(&json!({ "time_zone": "Mars/Olympus_Mons" })).unwrap_err();
        assert!(
            matches!(&e, Error::HomeAssistant(m) if m.contains("\"Mars/Olympus_Mons\"")),
            "{e}"
        );
        let long = "x".repeat(500);
        let e = time_zone_from_config(&json!({ "time_zone": long })).unwrap_err();
        assert!(e.to_string().len() < 300, "{e}");
        assert!(time_zone_from_config(&json!({ "time_zone": "" })).is_err());
    }

    #[test]
    fn fetch_sends_get_config() {
        let transport =
            FakeTransport::ha(|msg| vec![ok(msg, json!({ "time_zone": "America/New_York" }))]);
        let mut session = Session::authenticate(transport, &Secret::new("jeton")).unwrap();
        let tz = fetch_time_zone(&mut session).unwrap();
        assert_eq!(tz.iana_name(), Some("America/New_York"));
        assert_eq!(
            session.transport().commands(),
            [json!({ "type": "get_config", "id": 1 })]
        );
    }

    #[test]
    fn version_comparison_is_numeric() {
        assert_eq!(is_older_than("2026.9.4", "2026.9"), Some(false));
        assert_eq!(is_older_than("2026.9", "2026.9"), Some(false));
        assert_eq!(is_older_than("2026.10.0b1", "2026.9"), Some(false));
        assert_eq!(is_older_than("2026.10b0", "2026.9"), Some(false));
        assert_eq!(is_older_than("2027.1.0", "2026.9"), Some(false));
        assert_eq!(is_older_than("2026.8.3", "2026.9"), Some(true));
        assert_eq!(is_older_than("2025.12.3", "2026.9"), Some(true));
        assert_eq!(is_older_than("2026.9.4", "2026.10"), Some(true));
    }

    #[test]
    fn unreadable_version_gives_no_answer() {
        for version in ["inconnue", "", "2026", "2026.", "2026.x", "v2026.9", "-1.2"] {
            assert_eq!(is_older_than(version, "2026.9"), None, "{version:?}");
        }
        assert_eq!(is_older_than("2026.9.4", "référence"), None);
    }
}

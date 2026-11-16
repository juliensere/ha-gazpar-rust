//! Validations pures des valeurs d'option : PCE, nombres bornés, e-mail,
//! niveau de journal, préfixe des statistiques, `HA_URL` et adresses de
//! développement (analysées par [`crate::url`], le même analyseur que celui
//! des connexions : une adresse acceptée ici ne peut pas être refusée à la
//! connexion).
//!
//! Chaque fonction rend la valeur normalisée ou un motif en français qui ne
//! recopie jamais la valeur reçue ; le chargeur ([`super::loader`]) y ajoute
//! le nom de l'option et sa provenance.
//!
//! Dépendances : `url`, `domain`, `log` (type `Level`).

use crate::domain::{Measure, StatisticId};
use crate::log::Level;
use crate::url::{InvalidUrl, Url};

/// Chemin de l'API WebSocket de Home Assistant, ajouté à `HA_URL` s'il manque.
pub const HA_WEBSOCKET_PATH: &str = "/api/websocket";

/// PCE : exactement 14 chiffres.
pub(super) fn check_pce(pce: &str) -> Result<String, String> {
    if pce.len() == 14 && pce.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(pce.to_owned());
    }
    let count = pce.chars().count();
    Err(if count == 14 {
        "14 chiffres attendus (caractère non numérique présent)".to_owned()
    } else {
        format!("14 chiffres attendus ({count} caractères reçus)")
    })
}

/// Entier borné. Le message ne recopie pas la valeur reçue.
pub(super) fn parse_number(raw: &str, min: u32, max: u32, unit: &str) -> Result<u32, String> {
    let bounds = if max == u32::MAX {
        format!("au moins {min}")
    } else {
        format!("entre {min} et {max}")
    };
    match raw.trim().parse::<u32>() {
        Ok(n) if (min..=max).contains(&n) => Ok(n),
        Ok(n) => Err(format!("{n} {unit} hors limites ({bounds})")),
        Err(_) => Err(format!("nombre entier de {unit} attendu ({bounds})")),
    }
}

/// Exemple d'adresse de Home Assistant, cité dans les messages.
const HA_URL_EXAMPLE: &str = "ex. ws://192.168.1.10:8123";

/// Valide l'URL Home Assistant avec l'analyseur de connexion
/// ([`Url::ws`]) et ajoute `/api/websocket` si elle n'a pas de chemin.
/// Requête (`?`) et fragment (`#`) sont refusés : l'API WebSocket de HA n'en
/// a pas l'usage. Rend la forme normalisée (schéma et hôte en minuscules).
pub(super) fn check_ha_url(text: &str) -> Result<String, String> {
    if text
        .get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("wss://"))
    {
        return Err(format!(
            "wss:// (WebSocket chiffré) n'est pas pris en charge ; utiliser ws:// vers \
             Home Assistant sur le réseau local, {HA_URL_EXAMPLE}"
        ));
    }
    let url = Url::ws(text).map_err(|e| match e {
        InvalidUrl::Scheme(_) => format!("doit commencer par ws://, {HA_URL_EXAMPLE}"),
        InvalidUrl::Credentials => {
            "identifiants dans l'URL non pris en charge (utiliser HA_TOKEN)".to_owned()
        }
        other => format!("adresse invalide : {other} ({HA_URL_EXAMPLE})"),
    })?;
    if text.contains(['?', '#']) {
        return Err(format!(
            "requête (?) et fragment (#) non pris en charge ({HA_URL_EXAMPLE})"
        ));
    }
    Ok(if url.path() == "/" {
        format!("{}{HA_WEBSOCKET_PATH}", url.origin())
    } else {
        url.full()
    })
}

/// Adresse de développement analysée par [`Url::http`], vers `localhost`
/// ou `127.0.0.1` seulement, sans requête ni fragment ; `None` sinon. Rend
/// la forme normalisée, sans `/` final.
pub(super) fn local_url(text: &str) -> Option<String> {
    let url = Url::http(text).ok()?;
    let local = matches!(url.host(), "localhost" | "127.0.0.1");
    (local && !text.contains(['?', '#'])).then(|| url.full().trim_end_matches('/').to_owned())
}

/// E-mail du compte : la présence d'un « @ » suffit (GRDF juge du reste).
pub(super) fn check_username(username: &str) -> Result<String, String> {
    username
        .contains('@')
        .then(|| username.to_owned())
        .ok_or_else(|| "une adresse e-mail est attendue (pas de « @ »)".to_owned())
}

/// Niveau de journal réglable : `info` ou `debug`.
pub(super) fn check_log_level(level: &str) -> Result<Level, String> {
    Level::parse(level).ok_or_else(|| "« info » ou « debug » attendu".to_owned())
}

/// Préfixe des statistiques et les deux identifiants qu'il donne pour ce
/// PCE.
pub(super) fn check_statistic_prefix(
    prefix: &str,
    pce: &str,
) -> Result<(String, StatisticId, StatisticId), String> {
    match Measure::ALL.map(|m| StatisticId::for_measure(prefix, pce, m)) {
        [Ok(energy), Ok(volume)] => Ok((prefix.to_owned(), energy, volume)),
        _ => Err("préfixe invalide (minuscules, chiffres et _, ex. gazpar, \
                  ou sensor.<nom> pour reprendre des statistiques d'entité)"
            .to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::loader::load;
    use super::super::loader::testing::*;
    use super::*;

    // --- Validation des valeurs ---

    #[test]
    fn pce_must_be_14_digits() {
        for bad in [
            "1234567890123",
            "123456789012345",
            "1234567890123a",
            "12345 78901234",
        ] {
            let p = single(from_env(&minimal_with(&[("GRDF_PCE", bad)])));
            assert!(
                p.starts_with("GRDF_PCE (environnement) : 14 chiffres"),
                "{p}"
            );
        }
    }

    #[test]
    fn username_must_be_an_email() {
        let p = single(from_env(&minimal_with(&[("GRDF_USERNAME", "jean.dupont")])));
        assert_eq!(
            p,
            "GRDF_USERNAME (environnement) : une adresse e-mail est attendue (pas de « @ »)"
        );
    }

    #[test]
    fn ha_url_path_is_added_when_missing() {
        for (given, kept) in [
            (
                "ws://192.168.1.10:8123",
                "ws://192.168.1.10:8123/api/websocket",
            ),
            (
                "ws://192.168.1.10:8123/",
                "ws://192.168.1.10:8123/api/websocket",
            ),
            ("ws://homeassistant", "ws://homeassistant/api/websocket"),
            ("ws://ha:8123/api/websocket", "ws://ha:8123/api/websocket"),
            ("ws://ha:8123/autre/chemin", "ws://ha:8123/autre/chemin"),
        ] {
            let c = accepted(from_env(&minimal_with(&[("HA_URL", given)])));
            assert_eq!(c.ha_url, kept, "{given}");
            assert_eq!(setting(&c, "HA_URL").shown, kept);
        }
    }

    #[test]
    fn ha_url_must_be_plain_websocket() {
        let p = single(from_env(&minimal_with(&[(
            "HA_URL",
            "wss://ha.example.org",
        )])));
        assert!(
            p.contains("wss://") && p.contains("pas pris en charge"),
            "{p}"
        );
        for bad in [
            "http://192.168.0.88:8123",
            "192.168.0.88:8123",
            "ws://",
            "ws:///api",
            "ws://:8123/",
        ] {
            let p = single(from_env(&minimal_with(&[("HA_URL", bad)])));
            assert!(p.starts_with("HA_URL (environnement) : "), "{bad} : {p}");
        }
        let p = single(from_env(&minimal_with(&[("HA_URL", "ws://u:p@ha:8123")])));
        assert!(p.contains("identifiants"), "{p}");
        assert!(!p.contains("u:p"), "{p}");
    }

    #[test]
    fn ha_url_is_checked_by_the_connection_parser() {
        // Refusées ici, et non plus seulement à la connexion (code 1, puis
        // nouvel essai toutes les 8 h par le démon).
        for (bad, why) in [
            ("ws://ha:abc", "adresse invalide : port invalide"),
            ("ws://ha:0", "adresse invalide : port invalide"),
            ("ws://ha:", "adresse invalide : port invalide"),
            (
                "ws://ha:99999/api/websocket",
                "adresse invalide : port invalide",
            ),
            (
                "ws://[::1/api/websocket",
                "adresse invalide : hôte manquant ou invalide",
            ),
            (
                "ws://[secret]:8123",
                "adresse invalide : hôte manquant ou invalide",
            ),
            (
                "ws://ha/api websocket",
                "adresse invalide : espace ou caractère de contrôle dans l'adresse",
            ),
            (
                "ws://ha:8123?x",
                "requête (?) et fragment (#) non pris en charge",
            ),
            (
                "ws://ha:8123/api/websocket?x=1",
                "requête (?) et fragment (#)",
            ),
            ("ws://ha:8123#x", "requête (?) et fragment (#)"),
            ("ws://ha:8123/#x", "requête (?) et fragment (#)"),
        ] {
            let p = single(from_env(&minimal_with(&[("HA_URL", bad)])));
            assert!(
                p.starts_with(&format!("HA_URL (environnement) : {why}")),
                "{bad} : {p}"
            );
            assert!(p.ends_with("(ex. ws://192.168.1.10:8123)"), "{bad} : {p}");
            assert!(!p.contains("secret"), "{p}");
        }
    }

    #[test]
    fn ha_url_ipv6_and_case() {
        for (given, kept) in [
            ("ws://[::1]:8123", "ws://[::1]:8123/api/websocket"),
            ("WS://HA.Local:8123/", "ws://ha.local:8123/api/websocket"),
            (
                "ws://[fd00::1]/api/websocket",
                "ws://[fd00::1]/api/websocket",
            ),
        ] {
            let c = accepted(from_env(&minimal_with(&[("HA_URL", given)])));
            assert_eq!(c.ha_url, kept, "{given}");
        }
    }

    #[test]
    fn numeric_bounds() {
        assert_eq!(
            single(from_env(&minimal_with(&[("GAZPAR_SCAN_INTERVAL", "59")]))),
            "GAZPAR_SCAN_INTERVAL (environnement) : 59 minutes hors limites (au moins 60)"
        );
        let c = accepted(from_env(&minimal_with(&[("GAZPAR_SCAN_INTERVAL", " 60 ")])));
        assert_eq!(c.scan_interval_minutes, 60);
        for (name, bad) in [
            ("GAZPAR_OVERLAP_DAYS", "0"),
            ("GAZPAR_OVERLAP_DAYS", "61"),
            ("GAZPAR_HISTORY_DAYS", "0"),
            ("GAZPAR_HISTORY_DAYS", "3651"),
            ("GAZPAR_SCAN_INTERVAL", "-480"),
            ("GAZPAR_SCAN_INTERVAL", "8h"),
            ("GAZPAR_SCAN_INTERVAL", "99999999999"),
        ] {
            let p = single(from_env(&minimal_with(&[(name, bad)])));
            assert!(p.starts_with(name), "{name}={bad} : {p}");
        }
        for (name, ok) in [("GAZPAR_OVERLAP_DAYS", "1"), ("GAZPAR_HISTORY_DAYS", "1")] {
            accepted(from_env(&minimal_with(&[(name, ok)])));
        }
    }

    #[test]
    fn invalid_value_is_not_quoted() {
        let fs = Fs::default().with(CONF, "GAZPAR_OVERLAP_DAYS=deux\n");
        assert_eq!(
            single(load(&line(Some(CONF), &[]), &env(&minimal()), &fs)),
            format!(
                "GAZPAR_OVERLAP_DAYS (fichier {CONF}) : nombre entier de jours attendu \
                 (entre 1 et 60)"
            )
        );
    }

    #[test]
    fn log_level_values() {
        let p = single(from_env(&minimal_with(&[("GAZPAR_LOG_LEVEL", "warn")])));
        assert_eq!(
            p,
            "GAZPAR_LOG_LEVEL (environnement) : « info » ou « debug » attendu"
        );
        let c = accepted(from_env(&minimal_with(&[("GAZPAR_LOG_LEVEL", "DEBUG")])));
        assert_eq!(setting(&c, "GAZPAR_LOG_LEVEL").shown, "debug");
    }

    #[test]
    fn old_log_level_variable_has_no_effect() {
        let c = accepted(from_env(&minimal_with(&[("LOG_LEVEL", "debug")])));
        assert_eq!(c.log_level, Level::Info);
    }

    #[test]
    fn statistic_prefix_is_validated() {
        for bad in ["Gazpar", "gaz-par", "gaz:par", "sensor.", "_gazpar"] {
            let p = single(from_env(&minimal_with(&[("GAZPAR_STATISTIC_PREFIX", bad)])));
            assert!(p.starts_with("GAZPAR_STATISTIC_PREFIX"), "{bad}");
            // « sensor. » figure dans l'exemple du message lui-même.
            assert!(bad == "sensor." || !p.contains(bad), "{p}");
        }
        let c = accepted(from_env(&minimal_with(&[(
            "GAZPAR_STATISTIC_PREFIX",
            "gaz",
        )])));
        assert_eq!(c.energy_statistic.as_str(), "gaz:12345678901234_energy");
    }

    #[test]
    fn bad_prefix_reported_even_with_bad_pce() {
        let e = minimal_with(&[("GRDF_PCE", "x"), ("GAZPAR_STATISTIC_PREFIX", "A")]);
        assert_eq!(refused(from_env(&e)).len(), 2);
    }

    // --- Adresses GRDF de développement ---

    #[test]
    fn dev_urls_only_towards_localhost() {
        for (ok, kept) in [
            ("http://localhost", "http://localhost"),
            ("http://localhost:8080/x", "http://localhost:8080/x"),
            ("https://127.0.0.1:8443", "https://127.0.0.1:8443"),
            ("http://LOCALHOST:1/", "http://localhost:1"),
        ] {
            assert_eq!(local_url(ok).as_deref(), Some(kept), "{ok}");
        }
        for bad in [
            "http://localhost.evil.com",
            "http://127.0.0.1@evil.com",
            "http://evil.com@localhost.evil.com",
            "http://127.0.0.1.evil.com:80",
            "http://localhost:80@evil.com",
            "http://localhost:",
            "http://localhost:80x",
            "http://localhost:0",
            "http://localhost:+80",
            "http://localhost/?x=1",
            "http://localhost/#x",
            "ftp://localhost",
            "ws://localhost",
            "localhost",
            "https://monespace.grdf.fr",
            "http://[::1]:8080",
            "http://127.0.0.2",
        ] {
            assert_eq!(local_url(bad), None, "{bad} devrait être refusé");
        }
    }

    #[test]
    fn dev_url_towards_real_host_is_refused() {
        let e = minimal_with(&[("GRDF_BASE_URL", "https://monespace.grdf.fr.evil.com")]);
        assert!(single(from_env(&e)).starts_with("GRDF_BASE_URL : réservé au développement"));
        let e = minimal_with(&[("GRDF_LOGIN_URL", "http://127.0.0.1@evil.com")]);
        assert!(single(from_env(&e)).starts_with("GRDF_LOGIN_URL"));
    }

    #[test]
    fn dev_urls_are_read_from_environment_only() {
        let fs = Fs::default().with(CONF, "GRDF_BASE_URL=http://127.0.0.1:8080\n");
        let p = single(load(&line(Some(CONF), &[]), &env(&minimal()), &fs));
        assert_eq!(
            p,
            format!("fichier de configuration {CONF}, ligne 1 : option GRDF_BASE_URL inconnue")
        );
    }
}

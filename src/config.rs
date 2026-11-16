//! Lecture et validation de la configuration (variables d'environnement,
//! PLAN.md §5).
//!
//! [`Config::from_lookup`] reçoit la lecture des variables et des fichiers en
//! paramètres : les tests n'ont besoin ni de l'environnement réel ni du
//! disque. [`Config::from_env`] branche ces deux fonctions sur la
//! bibliothèque standard.
//!
//! Règles :
//! - une variable vide compte comme absente (cas fréquent avec Docker
//!   Compose) ;
//! - `GRDF_USERNAME`, `GRDF_PASSWORD`, `GRDF_PCE` et `HA_TOKEN` acceptent une
//!   variante `…_FILE` (chemin d'un fichier, secrets Docker), dont l'espace
//!   final est retiré ; définir les deux formes est une erreur ;
//! - tous les problèmes sont relevés d'un coup dans une seule
//!   [`Error::Config`] ;
//! - aucun message ni affichage ne contient de secret : l'e-mail, le mot de
//!   passe et le jeton sont des [`Secret`]. Le PCE n'est pas traité comme un
//!   secret : il figure dans les identifiants de statistiques.
//!
//! Dépendances : `domain`, `secret`, `log` (type `Level`), `error`, `jiff`.

use std::fmt::Write as _;

use jiff::tz::TimeZone;

use crate::domain::{Measure, StatisticId};
use crate::error::Error;
use crate::log::Level;
use crate::secret::Secret;

/// Adresse de Mon Espace GRDF.
pub const DEFAULT_GRDF_BASE_URL: &str = "https://monespace.grdf.fr";
/// Adresse du service de connexion de GRDF.
pub const DEFAULT_GRDF_LOGIN_URL: &str = "https://connexion.grdf.fr";

/// Noms de toutes les variables lues.
pub const VARIABLES: [&str; 18] = [
    "GRDF_USERNAME",
    "GRDF_USERNAME_FILE",
    "GRDF_PASSWORD",
    "GRDF_PASSWORD_FILE",
    "GRDF_PCE",
    "GRDF_PCE_FILE",
    "HA_URL",
    "HA_TOKEN",
    "HA_TOKEN_FILE",
    "GAZPAR_SCAN_INTERVAL",
    "GAZPAR_HISTORY_DAYS",
    "GAZPAR_OVERLAP_DAYS",
    "GAZPAR_CONSUMPTION_TYPE",
    "GAZPAR_TIMEZONE",
    "GAZPAR_STATISTIC_PREFIX",
    "LOG_LEVEL",
    "GRDF_BASE_URL",
    "GRDF_LOGIN_URL",
];

/// Type de relevés demandés à GRDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumptionType {
    /// Relevés informatifs : un relevé par jour.
    Informative,
    /// Relevés publiés : des périodes à répartir sur leurs jours. Pas encore
    /// pris en charge (PLAN.md §11, étape 7) : refusé par la configuration.
    Published,
}

impl ConsumptionType {
    pub fn as_str(self) -> &'static str {
        match self {
            ConsumptionType::Informative => "informative",
            ConsumptionType::Published => "published",
        }
    }
}

/// Configuration validée.
///
/// `Debug` ne montre aucun secret (voir le test `debug_shows_no_secret`).
#[derive(Debug)]
pub struct Config {
    /// E-mail du compte Mon Espace GRDF.
    pub grdf_username: Secret,
    pub grdf_password: Secret,
    /// Point de comptage et d'estimation : 14 chiffres.
    pub pce: String,
    /// URL WebSocket de Home Assistant, `ws://…`.
    pub ha_url: String,
    pub ha_token: Secret,
    /// Intervalle entre deux relevés, en minutes (≥ 60).
    pub scan_interval_minutes: u32,
    /// Profondeur du premier import, en jours.
    pub history_days: u32,
    /// Fenêtre de rattrapage, en jours.
    pub overlap_days: u32,
    pub consumption_type: ConsumptionType,
    pub timezone: TimeZone,
    pub log_level: Level,
    pub energy_statistic: StatisticId,
    pub volume_statistic: StatisticId,
    /// Adresse de Mon Espace GRDF (surchargeable vers `localhost` seulement).
    pub grdf_base_url: String,
    /// Adresse de connexion GRDF (surchargeable vers `localhost` seulement).
    pub grdf_login_url: String,
}

impl Config {
    /// Lit la configuration dans l'environnement du processus.
    pub fn from_env() -> Result<Config, Error> {
        let not_unicode: Vec<String> = VARIABLES
            .iter()
            .filter(|name| matches!(std::env::var(name), Err(std::env::VarError::NotUnicode(_))))
            .map(|name| format!("{name} : la valeur n'est pas de l'UTF-8 valide"))
            .collect();
        if !not_unicode.is_empty() {
            return Err(Error::Config(not_unicode));
        }
        Config::from_lookup(
            |name| std::env::var(name).ok(),
            |path| std::fs::read_to_string(path),
        )
    }

    /// Lit la configuration avec `env` (valeur d'une variable) et `read_file`
    /// (contenu d'un fichier, pour les variantes `_FILE`).
    pub fn from_lookup(
        env: impl Fn(&str) -> Option<String>,
        read_file: impl Fn(&str) -> std::io::Result<String>,
    ) -> Result<Config, Error> {
        let mut r = Reader {
            env: &env,
            read_file: &read_file,
            problems: Vec::new(),
        };

        let username = r.secret_or_file("GRDF_USERNAME");
        if username.as_deref().is_some_and(|u| !u.contains('@')) {
            r.problem("GRDF_USERNAME : une adresse e-mail est attendue (pas de « @ »)");
        }
        let password = r.secret_or_file("GRDF_PASSWORD");
        let pce = r.secret_or_file("GRDF_PCE");
        let pce = pce.filter(|p| {
            let valid = is_valid_pce(p);
            if !valid {
                let count = p.chars().count();
                r.problem(if count == 14 {
                    "GRDF_PCE : 14 chiffres attendus (caractère non numérique présent)".to_owned()
                } else {
                    format!("GRDF_PCE : 14 chiffres attendus ({count} caractères reçus)")
                });
            }
            valid
        });

        let ha_url = r.value("HA_URL");
        match &ha_url {
            None => r.problem("HA_URL manquant (ex. ws://192.168.0.88:8123/api/websocket)"),
            Some(url) => {
                if let Some(why) = check_ha_url(url) {
                    r.problem(format!("HA_URL : {why}"));
                }
            }
        }
        let ha_token = r.secret_or_file("HA_TOKEN");

        let scan_interval_minutes = r.number("GAZPAR_SCAN_INTERVAL", 480, 60, u32::MAX, "minutes");
        let history_days = r.number("GAZPAR_HISTORY_DAYS", 1095, 1, 3650, "jours");
        let overlap_days = r.number("GAZPAR_OVERLAP_DAYS", 14, 1, 60, "jours");

        let consumption_type = match r.value("GAZPAR_CONSUMPTION_TYPE").as_deref().map(str::trim) {
            None | Some("informative") => Some(ConsumptionType::Informative),
            Some("published") => {
                r.problem(
                    "GAZPAR_CONSUMPTION_TYPE : « published » n'est pas encore pris en charge, \
                     utiliser « informative » (valeur par défaut)",
                );
                None
            }
            Some(_) => {
                r.problem("GAZPAR_CONSUMPTION_TYPE : « informative » attendu");
                None
            }
        };

        let tz_name = r
            .value("GAZPAR_TIMEZONE")
            .unwrap_or_else(|| "Europe/Paris".to_owned());
        let timezone = match TimeZone::get(tz_name.trim()) {
            Ok(tz) => Some(tz),
            Err(_) => {
                r.problem(format!(
                    "GAZPAR_TIMEZONE : fuseau « {} » inconnu (ex. Europe/Paris)",
                    tz_name.trim()
                ));
                None
            }
        };

        let log_level = match r.value("LOG_LEVEL") {
            None => Some(Level::Info),
            Some(v) => {
                let level = Level::parse(&v);
                if level.is_none() {
                    r.problem("LOG_LEVEL : « info » ou « debug » attendu");
                }
                level
            }
        };

        // Le préfixe est validé même si le PCE est faux, avec un PCE fictif.
        let prefix = r
            .value("GAZPAR_STATISTIC_PREFIX")
            .unwrap_or_else(|| "gazpar".to_owned());
        let pce_for_ids = pce.as_deref().unwrap_or("00000000000000");
        let ids = Measure::ALL.map(|m| StatisticId::for_measure(&prefix, pce_for_ids, m));
        let [energy_statistic, volume_statistic] = match ids {
            [Ok(e), Ok(v)] => [Some(e), Some(v)],
            _ => {
                r.problem(format!(
                    "GAZPAR_STATISTIC_PREFIX : « {prefix} » invalide (minuscules, chiffres et _, \
                     ex. gazpar, ou sensor.<nom> pour reprendre des statistiques d'entité)"
                ));
                [None, None]
            }
        };

        let grdf_base_url = r.dev_url("GRDF_BASE_URL", DEFAULT_GRDF_BASE_URL);
        let grdf_login_url = r.dev_url("GRDF_LOGIN_URL", DEFAULT_GRDF_LOGIN_URL);

        if !r.problems.is_empty() {
            return Err(Error::Config(r.problems));
        }
        match (
            username,
            password,
            pce,
            ha_url,
            ha_token,
            consumption_type,
            timezone,
            log_level,
            energy_statistic,
            volume_statistic,
        ) {
            (
                Some(username),
                Some(password),
                Some(pce),
                Some(ha_url),
                Some(ha_token),
                Some(consumption_type),
                Some(timezone),
                Some(log_level),
                Some(energy_statistic),
                Some(volume_statistic),
            ) => Ok(Config {
                grdf_username: Secret::new(username),
                grdf_password: Secret::new(password),
                pce,
                ha_url,
                ha_token: Secret::new(ha_token),
                scan_interval_minutes,
                history_days,
                overlap_days,
                consumption_type,
                timezone,
                log_level,
                energy_statistic,
                volume_statistic,
                grdf_base_url,
                grdf_login_url,
            }),
            // Chaque valeur absente a déjà ajouté un problème ; ce cas ne
            // devrait pas se produire, mais on ne panique pas.
            _ => Err(Error::Config(vec![
                "configuration incomplète (erreur interne de validation)".to_owned(),
            ])),
        }
    }

    /// Identifiant de statistique d'une mesure.
    pub fn statistic_id(&self, measure: Measure) -> &StatisticId {
        match measure {
            Measure::Energy => &self.energy_statistic,
            Measure::Volume => &self.volume_statistic,
        }
    }

    /// Résumé lisible, sans aucun secret (sous-commande `check`).
    pub fn summary(&self) -> String {
        let tz = self.timezone.iana_name().unwrap_or("?");
        let mut s = String::new();
        // `write!` sur une String ne peut pas échouer.
        let _ = writeln!(
            s,
            "GRDF           : compte {}, PCE {}, relevés {}",
            self.grdf_username,
            self.pce,
            self.consumption_type.as_str()
        );
        let _ = writeln!(
            s,
            "Home Assistant : {} (jeton {})",
            self.ha_url, self.ha_token
        );
        for m in Measure::ALL {
            let _ = writeln!(
                s,
                "Statistique    : {} ({})",
                self.statistic_id(m),
                m.unit()
            );
        }
        let _ = writeln!(
            s,
            "Relevés        : toutes les {} min, recouvrement {} j, historique initial {} j",
            self.scan_interval_minutes, self.overlap_days, self.history_days
        );
        let _ = writeln!(s, "Fuseau         : {tz}");
        let _ = writeln!(s, "Journal        : {}", self.log_level);
        if self.grdf_base_url != DEFAULT_GRDF_BASE_URL
            || self.grdf_login_url != DEFAULT_GRDF_LOGIN_URL
        {
            let _ = writeln!(
                s,
                "GRDF (dév.)    : {} / {}",
                self.grdf_base_url, self.grdf_login_url
            );
        }
        s
    }
}

/// Accumule les problèmes pendant la lecture.
struct Reader<'a, E, F> {
    env: &'a E,
    read_file: &'a F,
    problems: Vec<String>,
}

impl<E, F> Reader<'_, E, F>
where
    E: Fn(&str) -> Option<String>,
    F: Fn(&str) -> std::io::Result<String>,
{
    fn problem(&mut self, message: impl Into<String>) {
        self.problems.push(message.into());
    }

    /// Valeur d'une variable ; vide = absente.
    fn value(&self, name: &str) -> Option<String> {
        (self.env)(name).filter(|v| !v.is_empty())
    }

    /// Variable requise acceptant une variante `_FILE`.
    fn secret_or_file(&mut self, name: &str) -> Option<String> {
        let file_var = format!("{name}_FILE");
        match (self.value(name), self.value(&file_var)) {
            (Some(_), Some(_)) => {
                self.problem(format!(
                    "{name} et {file_var} sont définis tous les deux : n'en garder qu'un"
                ));
                None
            }
            (Some(v), None) => Some(v),
            (None, Some(path)) => match (self.read_file)(&path) {
                Ok(content) => {
                    let v = content.trim_end().to_owned();
                    if v.is_empty() {
                        self.problem(format!("{file_var} : le fichier {path} est vide"));
                        None
                    } else {
                        Some(v)
                    }
                }
                Err(e) => {
                    self.problem(format!("{file_var} : lecture de {path} impossible ({e})"));
                    None
                }
            },
            (None, None) => {
                self.problem(format!("{name} manquant (ou {file_var})"));
                None
            }
        }
    }

    /// Entier optionnel borné ; en cas d'erreur, le problème est noté et la
    /// valeur par défaut renvoyée (elle ne sera pas utilisée).
    fn number(&mut self, name: &str, default: u32, min: u32, max: u32, unit: &str) -> u32 {
        let Some(raw) = self.value(name) else {
            return default;
        };
        let bounds = if max == u32::MAX {
            format!("au moins {min}")
        } else {
            format!("entre {min} et {max}")
        };
        match raw.trim().parse::<u32>() {
            Ok(n) if (min..=max).contains(&n) => n,
            Ok(n) => {
                self.problem(format!("{name} : {n} {unit} hors limites ({bounds})"));
                default
            }
            Err(_) => {
                self.problem(format!(
                    "{name} : nombre entier de {unit} attendu ({bounds}), reçu « {} »",
                    raw.trim()
                ));
                default
            }
        }
    }

    /// Adresse GRDF surchargeable pour le développement, vers `localhost` ou
    /// `127.0.0.1` uniquement (jamais vers un autre hôte).
    fn dev_url(&mut self, name: &str, default: &str) -> String {
        match self.value(name) {
            None => default.to_owned(),
            Some(url) if is_local_url(&url) => url.trim_end_matches('/').to_owned(),
            Some(_) => {
                self.problem(format!(
                    "{name} : réservé au développement, seuls http(s)://localhost et \
                     http(s)://127.0.0.1 sont acceptés"
                ));
                default.to_owned()
            }
        }
    }
}

/// PCE : exactement 14 chiffres.
fn is_valid_pce(pce: &str) -> bool {
    pce.len() == 14 && pce.bytes().all(|b| b.is_ascii_digit())
}

/// Motif du refus d'une URL Home Assistant, ou `None` si elle convient.
fn check_ha_url(url: &str) -> Option<&'static str> {
    if url.starts_with("wss://") {
        return Some(
            "wss:// (WebSocket chiffré) n'est pas pris en charge ; utiliser ws:// vers \
             Home Assistant sur le réseau local, ex. ws://192.168.0.88:8123/api/websocket",
        );
    }
    let Some(rest) = url.strip_prefix("ws://") else {
        return Some("doit commencer par ws://, ex. ws://192.168.0.88:8123/api/websocket");
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.starts_with(':') {
        return Some("hôte manquant après ws://");
    }
    if authority.contains('@') {
        return Some("identifiants dans l'URL non pris en charge (utiliser HA_TOKEN)");
    }
    None
}

/// L'URL désigne-t-elle `localhost` ou `127.0.0.1` (port facultatif) ?
fn is_local_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = match authority.split_once(':') {
        Some((host, port)) => {
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            host
        }
        None => authority,
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const PASSWORD: &str = "P@ssw0rd-tres-secret";
    const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.jeton-tres-secret";
    const EMAIL: &str = "jean.dupont@example.org";

    fn minimal() -> HashMap<String, String> {
        [
            ("GRDF_USERNAME", EMAIL),
            ("GRDF_PASSWORD", PASSWORD),
            ("GRDF_PCE", "12345678901234"),
            ("HA_URL", "ws://127.0.0.1:8123/api/websocket"),
            ("HA_TOKEN", TOKEN),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
    }

    fn with(
        mut vars: HashMap<String, String>,
        changes: &[(&str, &str)],
    ) -> HashMap<String, String> {
        for (k, v) in changes {
            vars.insert((*k).to_owned(), (*v).to_owned());
        }
        vars
    }

    fn without(mut vars: HashMap<String, String>, names: &[&str]) -> HashMap<String, String> {
        for n in names {
            vars.remove(*n);
        }
        vars
    }

    fn no_files(path: &str) -> std::io::Result<String> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{path} introuvable"),
        ))
    }

    fn load(vars: &HashMap<String, String>) -> Result<Config, Error> {
        Config::from_lookup(|k| vars.get(k).cloned(), no_files)
    }

    fn load_with_files(
        vars: &HashMap<String, String>,
        files: &[(&str, &str)],
    ) -> Result<Config, Error> {
        let files: HashMap<String, String> = files
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(
            |k| vars.get(k).cloned(),
            |p| files.get(p).cloned().map_or_else(|| no_files(p), Ok),
        )
    }

    /// Problèmes relevés, ou échec du test si la configuration est acceptée.
    fn problems(vars: &HashMap<String, String>) -> Vec<String> {
        match load(vars) {
            Err(Error::Config(p)) => p,
            Err(e) => panic!("erreur inattendue : {e}"),
            Ok(c) => panic!("configuration acceptée à tort : {c:?}"),
        }
    }

    fn single_problem(vars: &HashMap<String, String>) -> String {
        let p = problems(vars);
        assert_eq!(p.len(), 1, "{p:#?}");
        p[0].clone()
    }

    // --- Cas nominal et valeurs par défaut ---

    #[test]
    fn minimal_config_uses_defaults() {
        let c = load(&minimal()).unwrap();
        assert_eq!(c.grdf_username.expose(), EMAIL);
        assert_eq!(c.grdf_password.expose(), PASSWORD);
        assert_eq!(c.pce, "12345678901234");
        assert_eq!(c.ha_url, "ws://127.0.0.1:8123/api/websocket");
        assert_eq!(c.ha_token.expose(), TOKEN);
        assert_eq!(c.scan_interval_minutes, 480);
        assert_eq!(c.history_days, 1095);
        assert_eq!(c.overlap_days, 14);
        assert_eq!(c.consumption_type, ConsumptionType::Informative);
        assert_eq!(c.timezone.iana_name(), Some("Europe/Paris"));
        assert_eq!(c.log_level, Level::Info);
        assert_eq!(c.energy_statistic.as_str(), "gazpar:12345678901234_energy");
        assert_eq!(c.volume_statistic.as_str(), "gazpar:12345678901234_volume");
        assert_eq!(c.statistic_id(Measure::Volume), &c.volume_statistic);
        assert_eq!(c.grdf_base_url, DEFAULT_GRDF_BASE_URL);
        assert_eq!(c.grdf_login_url, DEFAULT_GRDF_LOGIN_URL);
    }

    #[test]
    fn every_option_set() {
        let vars = with(
            minimal(),
            &[
                ("GAZPAR_SCAN_INTERVAL", "60"),
                ("GAZPAR_HISTORY_DAYS", "3650"),
                ("GAZPAR_OVERLAP_DAYS", "60"),
                ("GAZPAR_CONSUMPTION_TYPE", "informative"),
                ("GAZPAR_TIMEZONE", "America/New_York"),
                ("GAZPAR_STATISTIC_PREFIX", "sensor.gazpar2haws"),
                ("LOG_LEVEL", "debug"),
                ("GRDF_BASE_URL", "http://127.0.0.1:8080/"),
                ("GRDF_LOGIN_URL", "http://localhost:8081"),
            ],
        );
        let c = load(&vars).unwrap();
        assert_eq!(c.scan_interval_minutes, 60);
        assert_eq!(c.history_days, 3650);
        assert_eq!(c.overlap_days, 60);
        assert_eq!(c.consumption_type, ConsumptionType::Informative);
        assert_eq!(c.timezone.iana_name(), Some("America/New_York"));
        assert_eq!(c.log_level, Level::Debug);
        assert_eq!(c.energy_statistic.as_str(), "sensor.gazpar2haws_energy");
        assert_eq!(c.grdf_base_url, "http://127.0.0.1:8080");
        assert_eq!(c.grdf_login_url, "http://localhost:8081");
    }

    #[test]
    fn empty_values_count_as_absent() {
        let vars = with(
            minimal(),
            &[("GAZPAR_SCAN_INTERVAL", ""), ("LOG_LEVEL", "")],
        );
        let c = load(&vars).unwrap();
        assert_eq!(c.scan_interval_minutes, 480);
        assert_eq!(c.log_level, Level::Info);
        let vars = with(minimal(), &[("HA_TOKEN", "")]);
        assert_eq!(
            single_problem(&vars),
            "HA_TOKEN manquant (ou HA_TOKEN_FILE)"
        );
    }

    // --- Variables manquantes : toutes listées d'un coup ---

    #[test]
    fn empty_environment_lists_every_missing_variable() {
        let p = problems(&HashMap::new());
        assert_eq!(p.len(), 5, "{p:#?}");
        for name in [
            "GRDF_USERNAME",
            "GRDF_PASSWORD",
            "GRDF_PCE",
            "HA_URL",
            "HA_TOKEN",
        ] {
            assert!(
                p.iter().any(|m| m.starts_with(&format!("{name} manquant"))),
                "{name} absent de {p:#?}"
            );
        }
    }

    #[test]
    fn several_invalid_values_reported_together() {
        let vars = with(
            minimal(),
            &[
                ("GRDF_PCE", "123"),
                ("GAZPAR_SCAN_INTERVAL", "10"),
                ("LOG_LEVEL", "trace"),
            ],
        );
        assert_eq!(problems(&vars).len(), 3);
    }

    // --- Validation des valeurs ---

    #[test]
    fn pce_must_be_14_digits() {
        for bad in [
            "1234567890123",
            "123456789012345",
            "1234567890123a",
            "12345 78901234",
        ] {
            let vars = with(minimal(), &[("GRDF_PCE", bad)]);
            assert!(
                single_problem(&vars).starts_with("GRDF_PCE : 14 chiffres"),
                "{bad}"
            );
        }
    }

    #[test]
    fn username_must_be_an_email() {
        let vars = with(minimal(), &[("GRDF_USERNAME", "jean.dupont")]);
        let p = single_problem(&vars);
        assert!(p.contains("e-mail"), "{p}");
        assert!(
            !p.contains("jean.dupont"),
            "l'identifiant ne doit pas être recopié : {p}"
        );
    }

    #[test]
    fn ha_url_must_be_plain_websocket() {
        let wss = with(
            minimal(),
            &[("HA_URL", "wss://ha.example.org/api/websocket")],
        );
        let p = single_problem(&wss);
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
            let vars = with(minimal(), &[("HA_URL", bad)]);
            assert!(single_problem(&vars).starts_with("HA_URL : "), "{bad}");
        }
        let creds = with(minimal(), &[("HA_URL", "ws://u:p@ha:8123/api/websocket")]);
        assert!(single_problem(&creds).contains("identifiants"));

        let ok = with(
            minimal(),
            &[("HA_URL", "ws://homeassistant:8123/api/websocket")],
        );
        assert!(load(&ok).is_ok());
    }

    #[test]
    fn scan_interval_minimum_is_60() {
        let vars = with(minimal(), &[("GAZPAR_SCAN_INTERVAL", "59")]);
        assert_eq!(
            single_problem(&vars),
            "GAZPAR_SCAN_INTERVAL : 59 minutes hors limites (au moins 60)"
        );
        let vars = with(minimal(), &[("GAZPAR_SCAN_INTERVAL", " 60 ")]);
        assert_eq!(load(&vars).unwrap().scan_interval_minutes, 60);
    }

    #[test]
    fn numeric_bounds() {
        for (name, bad) in [
            ("GAZPAR_OVERLAP_DAYS", "0"),
            ("GAZPAR_OVERLAP_DAYS", "61"),
            ("GAZPAR_HISTORY_DAYS", "0"),
            ("GAZPAR_HISTORY_DAYS", "3651"),
            ("GAZPAR_SCAN_INTERVAL", "-480"),
            ("GAZPAR_SCAN_INTERVAL", "8h"),
            ("GAZPAR_SCAN_INTERVAL", "99999999999"),
        ] {
            let vars = with(minimal(), &[(name, bad)]);
            assert!(single_problem(&vars).starts_with(name), "{name}={bad}");
        }
        for (name, ok) in [("GAZPAR_OVERLAP_DAYS", "1"), ("GAZPAR_HISTORY_DAYS", "1")] {
            assert!(load(&with(minimal(), &[(name, ok)])).is_ok(), "{name}={ok}");
        }
    }

    #[test]
    fn non_numeric_value_is_quoted() {
        let vars = with(minimal(), &[("GAZPAR_OVERLAP_DAYS", "deux")]);
        assert_eq!(
            single_problem(&vars),
            "GAZPAR_OVERLAP_DAYS : nombre entier de jours attendu (entre 1 et 60), reçu « deux »"
        );
    }

    #[test]
    fn consumption_type_values() {
        let vars = with(minimal(), &[("GAZPAR_CONSUMPTION_TYPE", "daily")]);
        assert!(single_problem(&vars).starts_with("GAZPAR_CONSUMPTION_TYPE"));
        let vars = with(minimal(), &[("GAZPAR_CONSUMPTION_TYPE", "informative")]);
        assert_eq!(
            load(&vars).unwrap().consumption_type,
            ConsumptionType::Informative
        );
    }

    #[test]
    fn published_is_not_supported_yet() {
        let vars = with(minimal(), &[("GAZPAR_CONSUMPTION_TYPE", "published")]);
        assert_eq!(
            single_problem(&vars),
            "GAZPAR_CONSUMPTION_TYPE : « published » n'est pas encore pris en charge, \
             utiliser « informative » (valeur par défaut)"
        );
        let vars = with(minimal(), &[("GAZPAR_CONSUMPTION_TYPE", " published ")]);
        assert!(single_problem(&vars).contains("pas encore pris en charge"));
    }

    #[test]
    fn timezone_must_be_known() {
        let vars = with(minimal(), &[("GAZPAR_TIMEZONE", "Europe/Pariss")]);
        assert_eq!(
            single_problem(&vars),
            "GAZPAR_TIMEZONE : fuseau « Europe/Pariss » inconnu (ex. Europe/Paris)"
        );
        let vars = with(minimal(), &[("GAZPAR_TIMEZONE", "UTC")]);
        assert!(load(&vars).is_ok());
    }

    #[test]
    fn log_level_values() {
        let vars = with(minimal(), &[("LOG_LEVEL", "warn")]);
        assert!(single_problem(&vars).starts_with("LOG_LEVEL"));
    }

    #[test]
    fn statistic_prefix_is_validated() {
        for bad in ["Gazpar", "gaz-par", "gaz:par", "sensor.", "_gazpar"] {
            let vars = with(minimal(), &[("GAZPAR_STATISTIC_PREFIX", bad)]);
            assert!(
                single_problem(&vars).starts_with("GAZPAR_STATISTIC_PREFIX"),
                "{bad}"
            );
        }
        let vars = with(minimal(), &[("GAZPAR_STATISTIC_PREFIX", "gaz")]);
        assert_eq!(
            load(&vars).unwrap().energy_statistic.as_str(),
            "gaz:12345678901234_energy"
        );
    }

    #[test]
    fn bad_prefix_reported_even_with_bad_pce() {
        let vars = with(
            minimal(),
            &[("GRDF_PCE", "x"), ("GAZPAR_STATISTIC_PREFIX", "A")],
        );
        assert_eq!(problems(&vars).len(), 2);
    }

    // --- Adresses GRDF de développement ---

    #[test]
    fn dev_urls_only_towards_localhost() {
        for ok in [
            "http://localhost",
            "http://localhost:8080/x",
            "https://127.0.0.1:8443",
            "http://LOCALHOST:1/",
        ] {
            assert!(is_local_url(ok), "{ok} devrait être accepté");
        }
        for bad in [
            "http://localhost.evil.com",
            "http://127.0.0.1@evil.com",
            "http://evil.com@localhost.evil.com",
            "http://127.0.0.1.evil.com:80",
            "http://localhost:80@evil.com",
            "http://localhost:",
            "http://localhost:80x",
            "ftp://localhost",
            "localhost",
            "https://monespace.grdf.fr",
            "http://[::1]:8080",
        ] {
            assert!(!is_local_url(bad), "{bad} devrait être refusé");
        }
    }

    #[test]
    fn dev_url_towards_real_host_is_refused() {
        let vars = with(
            minimal(),
            &[("GRDF_BASE_URL", "https://monespace.grdf.fr.evil.com")],
        );
        assert!(single_problem(&vars).starts_with("GRDF_BASE_URL : réservé au développement"));
        let vars = with(
            minimal(),
            &[("GRDF_LOGIN_URL", "http://127.0.0.1@evil.com")],
        );
        assert!(single_problem(&vars).starts_with("GRDF_LOGIN_URL"));
    }

    // --- Variantes _FILE ---

    #[test]
    fn file_variants_are_read_and_trimmed() {
        let vars = without(
            with(
                minimal(),
                &[
                    ("GRDF_USERNAME_FILE", "/run/secrets/user"),
                    ("GRDF_PASSWORD_FILE", "/run/secrets/pass"),
                    ("GRDF_PCE_FILE", "/run/secrets/pce"),
                    ("HA_TOKEN_FILE", "/run/secrets/token"),
                ],
            ),
            &["GRDF_USERNAME", "GRDF_PASSWORD", "GRDF_PCE", "HA_TOKEN"],
        );
        let c = load_with_files(
            &vars,
            &[
                ("/run/secrets/user", "a@b.fr\n"),
                ("/run/secrets/pass", " mot de passe \r\n"),
                ("/run/secrets/pce", "12345678901234\n\n"),
                ("/run/secrets/token", "abc\t"),
            ],
        )
        .unwrap();
        assert_eq!(c.grdf_username.expose(), "a@b.fr");
        // Seul l'espace final est retiré.
        assert_eq!(c.grdf_password.expose(), " mot de passe");
        assert_eq!(c.pce, "12345678901234");
        assert_eq!(c.ha_token.expose(), "abc");
    }

    #[test]
    fn both_forms_is_an_error() {
        let vars = with(minimal(), &[("GRDF_PASSWORD_FILE", "/run/secrets/pass")]);
        assert_eq!(
            single_problem(&vars),
            "GRDF_PASSWORD et GRDF_PASSWORD_FILE sont définis tous les deux : n'en garder qu'un"
        );
    }

    #[test]
    fn unreadable_or_empty_file() {
        let vars = without(
            with(minimal(), &[("HA_TOKEN_FILE", "/absent")]),
            &["HA_TOKEN"],
        );
        let p = single_problem(&vars);
        assert!(
            p.starts_with("HA_TOKEN_FILE : lecture de /absent impossible"),
            "{p}"
        );

        let vars = without(
            with(minimal(), &[("HA_TOKEN_FILE", "/vide")]),
            &["HA_TOKEN"],
        );
        let r = load_with_files(&vars, &[("/vide", " \n")]);
        assert_eq!(
            r.unwrap_err(),
            Error::Config(vec!["HA_TOKEN_FILE : le fichier /vide est vide".to_owned()])
        );
    }

    // --- Aucun secret affiché ---

    #[test]
    fn debug_shows_no_secret() {
        let c = load(&minimal()).unwrap();
        let shown = format!("{c:?} {c:#?}");
        for secret in [PASSWORD, TOKEN, EMAIL, "jeton-tres-secret", "dupont"] {
            assert!(!shown.contains(secret), "{secret} visible dans {shown}");
        }
        assert!(shown.contains("***"));
    }

    #[test]
    fn summary_shows_no_secret() {
        let c = load(&minimal()).unwrap();
        let s = c.summary();
        for secret in [PASSWORD, TOKEN, EMAIL, "dupont"] {
            assert!(!s.contains(secret), "{secret} visible dans {s}");
        }
        assert!(s.contains("12345678901234"));
        assert!(s.contains("gazpar:12345678901234_energy (kWh)"));
        assert!(s.contains("gazpar:12345678901234_volume (m³)"));
        assert!(s.contains("Europe/Paris"));
        assert!(!s.contains("dév."));
    }

    #[test]
    fn errors_show_no_secret() {
        // Mot de passe et jeton présents, autre chose invalide partout.
        let vars = with(
            minimal(),
            &[
                ("GRDF_USERNAME", "pas-un-email"),
                ("GRDF_PCE", "x"),
                ("HA_URL", "wss://x"),
                ("GRDF_PASSWORD_FILE", "/f"),
                ("HA_TOKEN_FILE", "/g"),
            ],
        );
        let e = load(&vars).unwrap_err();
        let shown = format!("{e} {e:?}");
        for secret in [PASSWORD, TOKEN, "pas-un-email"] {
            assert!(!shown.contains(secret), "{secret} visible dans {shown}");
        }
    }

    #[test]
    fn variables_list_is_complete() {
        assert!(VARIABLES.contains(&"GAZPAR_STATISTIC_PREFIX"));
        assert!(VARIABLES.contains(&"GRDF_LOGIN_URL"));
    }
}

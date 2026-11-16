//! Configuration : fusion des trois sources, provenance de chaque valeur et
//! validation (MANUEL.md §4, PLAN.md §5).
//!
//! Priorité, de la plus forte à la plus faible : paramètre de la commande,
//! variable d'environnement, fichier `NOM=valeur` (désigné par `--config` ou
//! `GAZPAR_CONFIG`), valeur par défaut. Une seule table, [`OPTIONS`], décrit
//! les options : les clés du fichier sont les noms des variables, les
//! paramètres s'en déduisent ([`parameter`] : `GRDF_PCE` ↔ `--grdf-pce`).
//!
//! Règles (MANUEL.md §4.3) :
//! - une option et sa variante `…_FILE` sont une seule option : la source la
//!   plus forte qui donne l'une des deux formes l'emporte entièrement, et la
//!   variante `_FILE` d'une source écartée n'est pas lue ; les deux formes
//!   dans une même source sont une erreur, même si cette source est écartée ;
//! - une valeur vide dans le fichier ou l'environnement compte comme absente
//!   (un paramètre vide est refusé plus tôt, par `cli`) ;
//! - les chemins `…_FILE` sont absolus ; l'espace final du contenu est
//!   retiré ;
//! - `HA_URL` et les adresses de développement sont analysées par
//!   [`crate::url`], le même analyseur que celui des connexions : une adresse
//!   acceptée ici ne peut pas être refusée à la connexion ;
//! - `GRDF_BASE_URL` et `GRDF_LOGIN_URL` (développement) ne sont lues que dans
//!   l'environnement, et seulement vers `localhost` ou `127.0.0.1` ;
//! - tous les problèmes sont relevés d'un coup dans une seule
//!   [`Error::Config`] ; les avertissements (variable inconnue, fichier
//!   lisible par d'autres) sont **renvoyés** dans [`Loaded::warnings`], même
//!   en cas d'erreur, et c'est `cli` qui les journalise.
//!
//! Aucun message, avertissement ni affichage ne contient de valeur d'option :
//! seulement des noms, des chemins de fichiers et des nombres. L'e-mail, le
//! mot de passe et le jeton sont des [`Secret`], affichés `***` par
//! [`Config::report`]. Le PCE n'est pas un secret : il figure dans les
//! identifiants de statistiques.
//!
//! Les entrées-sorties sont injectées : [`load`] reçoit l'environnement sous
//! forme de table et l'accès aux fichiers par le trait [`FileSystem`] ;
//! [`load_from_process`] les branche sur la bibliothèque standard.
//!
//! Organisation :
//! - [`options`] : table des options et noms qui s'en déduisent ;
//! - [`loader`] : fusion des trois sources, provenance, accès aux fichiers ;
//! - [`validate`] : validations pures des valeurs ;
//! - ici : la [`Config`] validée et son tableau pour `check`.
//!
//! Dépendances : `config_file`, `domain`, `secret`, `url`, `log` (type
//! `Level`), `error`.

mod loader;
mod options;
mod validate;

use std::fmt::Write as _;

use crate::config_file::CONFIG_VARIABLE;
use crate::domain::{Measure, StatisticId};
use crate::log::Level;
use crate::secret::Secret;

pub use loader::{
    CommandLine, FileSystem, Loaded, Origin, RealFileSystem, Source, environment_from, load,
    load_from_process,
};
pub use options::{
    DEV_VARIABLES, OPTIONS, OptionSpec, ParameterTarget, file_keys, parameter, parameters,
};
pub use validate::HA_WEBSOCKET_PATH;

/// Adresse de Mon Espace GRDF.
pub const DEFAULT_GRDF_BASE_URL: &str = "https://monespace.grdf.fr";
/// Adresse du service de connexion de GRDF.
pub const DEFAULT_GRDF_LOGIN_URL: &str = "https://connexion.grdf.fr";

/// Une ligne du tableau de `check` : valeur retenue (masquée si secrète) et
/// provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    pub name: &'static str,
    /// Valeur affichée : `***` pour un secret, forme normalisée sinon.
    pub shown: String,
    pub origin: Origin,
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
    /// URL WebSocket de Home Assistant, `ws://…`, chemin `/api/websocket`
    /// ajouté s'il manquait.
    pub ha_url: String,
    pub ha_token: Secret,
    /// Intervalle entre deux relevés, en minutes (≥ 60).
    pub scan_interval_minutes: u32,
    /// Profondeur du premier import, en jours.
    pub history_days: u32,
    /// Fenêtre de rattrapage, en jours.
    pub overlap_days: u32,
    pub log_level: Level,
    pub energy_statistic: StatisticId,
    pub volume_statistic: StatisticId,
    /// Adresse de Mon Espace GRDF (surchargeable vers `localhost` seulement).
    pub grdf_base_url: String,
    /// Adresse de connexion GRDF (surchargeable vers `localhost` seulement).
    pub grdf_login_url: String,
    /// Fichier de configuration lu et source de son chemin (`--config` ou
    /// `GAZPAR_CONFIG`).
    pub config_file: Option<(String, Source)>,
    /// Valeur retenue et provenance de chaque option, pour `check`.
    pub settings: Vec<Setting>,
}

impl Config {
    /// Identifiant de statistique d'une mesure.
    pub fn statistic_id(&self, measure: Measure) -> &StatisticId {
        match measure {
            Measure::Energy => &self.energy_statistic,
            Measure::Volume => &self.volume_statistic,
        }
    }

    /// Configuration retenue, sans aucun secret (commande `check`) : le
    /// fichier lu, puis une ligne par option au format du manuel (§4.1),
    /// puis les statistiques.
    pub fn report(&self) -> String {
        let mut s = String::new();
        // `write!` sur une String ne peut pas échouer.
        match &self.config_file {
            None => {
                let _ = writeln!(s, "Fichier de configuration : aucun");
            }
            Some((path, Source::Environment)) => {
                let _ = writeln!(
                    s,
                    "Fichier de configuration : {path} (environnement, {CONFIG_VARIABLE})"
                );
            }
            Some((path, source)) => {
                let _ = writeln!(s, "Fichier de configuration : {path} ({source})");
            }
        }
        let _ = writeln!(s);
        let rows: Vec<(&str, &str, String)> = self
            .settings
            .iter()
            .map(|r| (r.name, r.shown.as_str(), r.origin.to_string()))
            .collect();
        let name_width = rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(0) + 2;
        let value_width = rows.iter().map(|r| r.1.chars().count()).max().unwrap_or(0) + 3;
        for (name, shown, origin) in rows {
            let _ = writeln!(s, "{name:<name_width$}{shown:<value_width$}({origin})");
        }
        let _ = writeln!(s);
        let ids: Vec<String> = Measure::ALL
            .iter()
            .map(|m| format!("{} ({})", self.statistic_id(*m), m.unit()))
            .collect();
        let _ = writeln!(s, "Statistiques : {}", ids.join(", "));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::loader::testing::*;
    use super::*;

    // --- Tableau de check (MANUEL.md §4.1) ---

    #[test]
    fn report_matches_the_manual_format() {
        let e = minimal_with(&[("HA_TOKEN", ""), ("HA_TOKEN_FILE", "/run/secrets/ha_token")]);
        let fs = Fs::default().with("/run/secrets/ha_token", TOKEN);
        let c = accepted(load(
            &line(None, &[("GRDF_PCE", "01234567890123")]),
            &e,
            &fs,
        ));
        let r = c.report();
        let lines: Vec<&str> = r.lines().collect();
        assert_eq!(lines[0], "Fichier de configuration : aucun");
        let width = "GAZPAR_STATISTIC_PREFIX".len() + 2;
        let value = "ws://127.0.0.1:8123/api/websocket".len() + 3;
        let expect = |name: &str, shown: &str, origin: &str| {
            format!("{name:<width$}{shown:<value$}({origin})")
        };
        for wanted in [
            expect("GRDF_PCE", "01234567890123", "paramètre"),
            expect(
                "HA_TOKEN",
                "***",
                "fichier /run/secrets/ha_token, via HA_TOKEN_FILE",
            ),
            expect("GAZPAR_LOG_LEVEL", "info", "défaut"),
            expect("GRDF_USERNAME", "***", "environnement"),
            expect("GRDF_PASSWORD", "***", "environnement"),
        ] {
            assert!(lines.contains(&wanted.as_str()), "{wanted}\n{r}");
        }
        assert!(r.contains(
            "Statistiques : gazpar:01234567890123_energy (kWh), gazpar:01234567890123_volume (m³)"
        ));
    }

    #[test]
    fn report_names_the_config_file() {
        let fs = Fs::default().with(CONF, "");
        let c = accepted(load(&line(Some(CONF), &[]), &env(&minimal()), &fs));
        assert!(
            c.report()
                .starts_with(&format!("Fichier de configuration : {CONF} (paramètre)\n"))
        );
        let e = minimal_with(&[("GAZPAR_CONFIG", CONF)]);
        let c = accepted(load(&CommandLine::default(), &e, &fs));
        assert!(c.report().starts_with(&format!(
            "Fichier de configuration : {CONF} (environnement, GAZPAR_CONFIG)\n"
        )));
    }

    // --- Aucun secret affiché ---

    const SECRETS: [&str; 5] = [PASSWORD, TOKEN, EMAIL, "jeton-tres-secret", "dupont"];

    fn assert_no_secret(shown: &str) {
        for secret in SECRETS {
            assert!(!shown.contains(secret), "{secret} visible dans {shown}");
        }
    }

    #[test]
    fn secret_options_are_masked_in_the_table() {
        let c = accepted(from_env(&env(&minimal())));
        for o in &OPTIONS {
            let shown = &setting(&c, o.name).shown;
            assert_eq!(shown == Secret::MASK, o.secret, "{} : {shown}", o.name);
        }
        let secrets: Vec<&str> = OPTIONS
            .iter()
            .filter(|o| o.secret)
            .map(|o| o.name)
            .collect();
        assert_eq!(secrets, ["GRDF_USERNAME", "GRDF_PASSWORD", "HA_TOKEN"]);
    }

    #[test]
    fn debug_shows_no_secret() {
        let c = accepted(from_env(&env(&minimal())));
        assert_no_secret(&format!("{c:?} {c:#?}"));
        assert!(format!("{c:?}").contains("***"));
    }

    #[test]
    fn report_shows_no_secret() {
        let fs = Fs::default().with(
            CONF,
            &format!("GRDF_PASSWORD={PASSWORD}\nHA_TOKEN={TOKEN}\n"),
        );
        let e = minimal_with(&[("GRDF_PASSWORD", ""), ("HA_TOKEN", "")]);
        let c = accepted(load(
            &line(Some(CONF), &[("GRDF_USERNAME", EMAIL)]),
            &e,
            &fs,
        ));
        let r = c.report();
        assert_no_secret(&r);
        assert!(r.contains("12345678901234"));
    }

    #[test]
    fn command_line_debug_shows_no_value() {
        let l = line(Some(CONF), &[("GRDF_USERNAME", EMAIL)]);
        let shown = format!("{l:?} {l:#?}");
        assert_no_secret(&shown);
        assert!(shown.contains("GRDF_USERNAME"));
    }

    #[test]
    fn errors_and_warnings_show_no_secret() {
        // Secrets présents partout, autre chose invalide partout.
        let fs = Fs::default().with(
            CONF,
            &format!(
                "GRDF_PASSWORD={PASSWORD}\nGRDF_PASSWORD={PASSWORD}\n{TOKEN}\n\
                 HA_TOKEN_FILE={TOKEN}\nGAZPAR_OVERLAP_DAYS={PASSWORD}\n"
            ),
        );
        let mut e = minimal_with(&[
            ("GRDF_USERNAME", "pas-un-email-dupont"),
            ("GRDF_PCE", PASSWORD),
            ("HA_URL", "wss://x"),
            ("GRDF_PASSWORD_FILE", "/f"),
            ("GAZPAR_LOG_LEVEL", PASSWORD),
            ("GAZPAR_STATISTIC_PREFIX", PASSWORD),
        ]);
        e.insert("GRDF_X".to_owned(), TOKEN.to_owned());
        let loaded = load(
            &line(Some(CONF), &[("GAZPAR_HISTORY_DAYS", PASSWORD)]),
            &e,
            &fs,
        );
        let warnings = loaded.warnings.join("\n");
        let err = loaded.config.unwrap_err();
        assert_no_secret(&format!("{err} {err:?} {warnings}"));
    }
}

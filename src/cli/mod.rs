//! Ligne de commande : point d'entrée [`main`], aiguillage par commande et
//! codes de sortie (MANUEL.md §5, §7). L'analyse des arguments est dans
//! [`args`], la mise en forme dans [`report`].
//!
//! Les connexions passent par [`app::Connector`] : `main` donne les vrais
//! services ([`app::Network`]), les tests des services en mémoire. Ce module
//! ne construit aucun client lui-même.
//!
//! - `check` : tableau de configuration, puis Home Assistant (version,
//!   fuseau, dernier jour connu de chaque statistique), puis un relevé
//!   simulé (une connexion à GRDF) : derniers jours lus et ce qui serait
//!   importé. N'écrit rien.
//! - `once` : un relevé, journalisé ; code 1 en cas d'erreur.
//! - `reset --yes` : efface les deux statistiques, sans contacter GRDF ;
//!   sans `--yes`, affiche ce qui serait effacé et ne contacte rien.
//! - `run` : boucle de [`crate::daemon`], un relevé par intervalle.
//!
//! Journal (MANUEL.md §7) : lignes horodatées sur la sortie d'erreur
//! injectée, au seuil de `GAZPAR_LOG_LEVEL`. Les détails journalisés par
//! `grdf` et `ha` (macros de [`crate::log`]) vont, eux, directement sur la
//! sortie d'erreur du processus, au même format.
//!
//! Codes de sortie : 0 succès, 1 erreur d'exécution (GRDF ou Home
//! Assistant injoignable ou en refus), 2 configuration ou ligne de commande
//! incorrecte, ou `reset` sans `--yes`.
//!
//! Dépendances : `app`, `daemon`, `config`, `domain`, `sync` (types), `log`,
//! `error`, `jiff`.

pub(crate) mod args;
pub(crate) mod report;

use std::ffi::OsString;
use std::io::Write;
use std::time::Duration;

use jiff::Timestamp;

use crate::app::{self, Connector, Session};
use crate::config::{self, CommandLine, Config, Loaded};
use crate::daemon;
use crate::domain::Measure;
use crate::error::Error;
use crate::log::Level;
use crate::sync::{Mode, Report};
use crate::{HA_REFERENCE, VERSION};

use args::USAGE_HINT;
pub(crate) use args::{Command, Invocation, Parsed, parse_args, usage};

/// Code de sortie de `reset` sans `--yes` (MANUEL.md §5).
pub(crate) const EXIT_RESET_NOT_CONFIRMED: i32 = 2;

/// Ce que les commandes utilisent du monde extérieur, hors configuration.
pub(crate) struct Runtime<'a> {
    /// Ouvre les connexions à Home Assistant et à GRDF.
    pub connector: &'a mut dyn Connector,
    /// Attente entre deux relevés de `run`.
    pub sleep: &'a mut dyn FnMut(Duration),
    /// Nombre de relevés de `run` : `None` (indéfiniment) en production.
    pub max_cycles: Option<u64>,
    /// Horloge, lue à chaque connexion à Home Assistant pour calculer la
    /// date du jour : [`Timestamp::now`] en production, une heure fixe dans
    /// les tests (qui ne dépendent ainsi pas du passage de minuit).
    pub now: fn() -> Timestamp,
}

/// Point d'entrée appelé par `main.rs` : renvoie le code de sortie.
pub fn main(args: impl IntoIterator<Item = OsString>) -> i32 {
    let mut sleep = std::thread::sleep;
    run(
        args,
        config::load_from_process,
        Runtime {
            connector: &mut app::Network,
            sleep: &mut sleep,
            max_cycles: None,
            now: Timestamp::now,
        },
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

/// Exécute la ligne de commande avec un chargement de configuration, des
/// services et des sorties injectés (testable sans environnement, sans
/// réseau ni terminal).
pub(crate) fn run(
    args: impl IntoIterator<Item = OsString>,
    load_config: impl FnOnce(&CommandLine) -> Loaded,
    runtime: Runtime<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let result = parse_args(args).and_then(|parsed| match parsed {
        Parsed::Help => {
            write!(out, "{}", usage())?;
            Ok(0)
        }
        Parsed::Version => {
            writeln!(
                out,
                "ha-gazpar {VERSION} (Home Assistant de référence {HA_REFERENCE})"
            )?;
            Ok(0)
        }
        Parsed::Execute(invocation) => execute(&invocation, load_config, runtime, out, err),
    });
    match result {
        Ok(code) => code,
        Err(e) => {
            // Rien de plus à faire si la sortie d'erreur elle-même échoue.
            let _ = writeln!(err, "ha-gazpar : {e}");
            if matches!(e, Error::Usage(_)) {
                let _ = writeln!(err, "{USAGE_HINT}");
            }
            e.exit_code()
        }
    }
}

fn execute(
    invocation: &Invocation,
    load_config: impl FnOnce(&CommandLine) -> Loaded,
    runtime: Runtime<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, Error> {
    let config = load(|| load_config(&invocation.config), err)?;
    crate::log::init(config.log_level);
    if invocation.command == Command::Reset && !invocation.yes {
        let ids: Vec<_> = Measure::ALL
            .iter()
            .map(|m| config.statistic_id(*m))
            .collect();
        write!(out, "{}", report::reset_preview(&ids))?;
        writeln!(
            err,
            "ha-gazpar : rien n'a été effacé ; pour confirmer : ha-gazpar reset --yes"
        )?;
        return Ok(EXIT_RESET_NOT_CONFIRMED);
    }
    let mut journal = Journal { sink: err };
    journal.log(
        Level::Info,
        &report::startup(invocation.command, config.scan_interval_minutes),
    );
    let services = Services {
        connector: runtime.connector,
        now: runtime.now,
    };
    match invocation.command {
        Command::Check => check(&config, services, out, &mut journal),
        Command::Once => match reading(services, &config, &mut journal) {
            Ok(_) => Ok(0),
            Err(e) => {
                journal.log(Level::Error, &e.to_string());
                Ok(e.exit_code())
            }
        },
        Command::Reset => {
            let ids = with_session(services, &config, &mut journal, |session, _| {
                session.clear()
            })?;
            write!(out, "{}", report::reset_done(&ids))?;
            Ok(0)
        }
        Command::Run => {
            let interval = Duration::from_secs(u64::from(config.scan_interval_minutes) * 60);
            let mut host = DaemonHost {
                services,
                config: &config,
                journal: &mut journal,
                sleep: runtime.sleep,
            };
            daemon::run(&mut host, interval, runtime.max_cycles);
            Ok(0)
        }
    }
}

/// `check` : configuration, Home Assistant, puis relevé simulé.
fn check(
    config: &Config,
    services: Services<'_>,
    out: &mut dyn Write,
    journal: &mut Journal<'_>,
) -> Result<i32, Error> {
    writeln!(out, "Configuration valide.")?;
    write!(out, "{}", config.report())?;
    with_session(services, config, journal, |session, connector| {
        let known = session.last_known_days()?;
        writeln!(out)?;
        write!(
            out,
            "{}",
            report::check_home_assistant(session.info(), &known)
        )?;
        let simulated = session.sync(connector, Mode::DryRun)?;
        writeln!(out)?;
        write!(out, "{}", report::check_grdf(&simulated))?;
        Ok(0)
    })
}

/// Un relevé complet, journalisé : connexion à HA, relevé, fermeture.
fn reading(
    services: Services<'_>,
    config: &Config,
    journal: &mut Journal<'_>,
) -> Result<Report, Error> {
    let done = with_session(services, config, journal, |session, connector| {
        session.sync(connector, Mode::Apply)
    })?;
    for (level, line) in report::reading(&done) {
        journal.log(level, &line);
    }
    Ok(done)
}

/// Ouvre une session Home Assistant (journalise version et fuseau), y
/// exécute `work`, puis la ferme. Un échec de la fermeture est journalisé
/// mais ne change pas le résultat.
fn with_session<T>(
    services: Services<'_>,
    config: &Config,
    journal: &mut Journal<'_>,
    work: impl FnOnce(&mut Session<'_>, &mut dyn Connector) -> Result<T, Error>,
) -> Result<T, Error> {
    let Services { connector, now } = services;
    let mut session = Session::open(connector, config, now())?;
    for (level, line) in report::connected(session.info()) {
        journal.log(level, &line);
    }
    let result = work(&mut session, connector);
    if let Err(e) = session.close() {
        journal.log(
            Level::Warn,
            &format!("{e} ; sans conséquence sur ce qui a été fait"),
        );
    }
    result
}

/// Connexions et horloge, prêtées à une commande.
struct Services<'a> {
    connector: &'a mut dyn Connector,
    now: fn() -> Timestamp,
}

impl Services<'_> {
    /// Nouvel emprunt, pour une session de plus (un relevé de `run`).
    fn reborrow(&mut self) -> Services<'_> {
        Services {
            connector: &mut *self.connector,
            now: self.now,
        }
    }
}

/// Branchement de la boucle du démon sur les vrais relevés.
struct DaemonHost<'a, 'w> {
    services: Services<'a>,
    config: &'a Config,
    journal: &'a mut Journal<'w>,
    sleep: &'a mut dyn FnMut(Duration),
}

impl daemon::Host for DaemonHost<'_, '_> {
    fn reading(&mut self) -> Result<Report, Error> {
        reading(self.services.reborrow(), self.config, self.journal)
    }

    fn sleep(&mut self, duration: Duration) {
        (self.sleep)(duration);
    }

    fn log(&mut self, level: Level, message: &str) {
        self.journal.log(level, message);
    }
}

/// Journal sur la sortie d'erreur injectée, par le chemin d'écriture unique
/// de [`crate::log`] ([`crate::log::write_to`]). Les macros de journal des
/// autres modules, elles, écrivent sur la vraie sortie d'erreur.
struct Journal<'w> {
    sink: &'w mut dyn Write,
}

impl Journal<'_> {
    fn log(&mut self, level: Level, message: &str) {
        crate::log::write_to(self.sink, level, format_args!("{message}"));
    }
}

/// Charge la configuration et journalise ses avertissements sur `err`, même
/// si elle est refusée : ils expliquent souvent l'erreur (faute de frappe
/// dans un nom de variable).
fn load(load_config: impl FnOnce() -> Loaded, err: &mut dyn Write) -> Result<Config, Error> {
    let loaded = load_config();
    let mut journal = Journal { sink: err };
    for warning in &loaded.warnings {
        journal.log(Level::Warn, warning);
    }
    loaded.config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::fake::{FakeConnector, Script};
    use crate::domain::{DailyReading, Quantity};
    use std::collections::BTreeMap;

    const PASSWORD: &str = "mot-de-passe-secret";
    const TOKEN: &str = "jeton-secret";
    const EMAIL: &str = "jean@example.org";
    /// Jeton refusé, tel que le client Home Assistant le rend.
    const TOKEN_REFUSED: &str = "Home Assistant (127.0.0.1:8123), authentification : jeton refusé \
                                 (« Invalid access token or password »)";
    /// Home Assistant injoignable, tel que le client Home Assistant le rend.
    const HA_DOWN: &str = "Home Assistant (127.0.0.1:8123), connexion TCP : connexion refusée";
    /// Échec de la fermeture, tel que le client Home Assistant le rend.
    const CLOSE_FAILED: &str =
        "Home Assistant (127.0.0.1:8123), fermeture de la connexion : connexion coupée";
    /// Refus d'identifiants, tel que le client GRDF le rend.
    const REJECTED: &str = "GRDF, connexion 4/5 (mot de passe Okta) : identifiants refusés par \
                            GRDF (statut HTTP 401, connexion.grdf.fr:443)";
    /// Panne passagère de GRDF, telle que le client GRDF la rend.
    const TIMEOUT: &str = "GRDF, relevés du 2026-11-02 au 2026-11-15 : pas de réponse en 60 s \
                           (monespace.grdf.fr:443) ; abandon après 4 tentatives";

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn valid_env() -> BTreeMap<String, String> {
        [
            ("GRDF_USERNAME", EMAIL),
            ("GRDF_PASSWORD", PASSWORD),
            ("GRDF_PCE", "12345678901234"),
            ("HA_URL", "ws://127.0.0.1:8123"),
            ("HA_TOKEN", TOKEN),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
    }

    struct NoFiles;

    impl config::FileSystem for NoFiles {
        fn read_to_string(&self, _: &str) -> std::io::Result<String> {
            Err(std::io::ErrorKind::NotFound.into())
        }
        fn mode(&self, _: &str) -> std::io::Result<Option<u32>> {
            Err(std::io::ErrorKind::NotFound.into())
        }
    }

    fn valid_config(command_line: &CommandLine) -> Loaded {
        config::load(command_line, &valid_env(), &NoFiles)
    }

    fn incomplete_config(command_line: &CommandLine) -> Loaded {
        config::load(command_line, &BTreeMap::new(), &NoFiles)
    }

    fn config_with_warning(command_line: &CommandLine) -> Loaded {
        let mut env = valid_env();
        env.insert("GAZPAR_TIMEZONE".into(), "Europe/Paris".into());
        config::load(command_line, &env, &NoFiles)
    }

    fn not_called(_: &CommandLine) -> Loaded {
        panic!("la configuration ne doit pas être chargée")
    }

    /// Lance la ligne de commande avec des services en mémoire qui
    /// répondent normalement ; renvoie (code, stdout, stderr).
    fn launch(list: &[&str], config: fn(&CommandLine) -> Loaded) -> (i32, String, String) {
        launch_with(list, config, &mut FakeConnector::default(), &mut Vec::new())
    }

    /// Lance la ligne de commande avec ces services ; `run` fait au plus 3
    /// relevés et ses attentes sont notées dans `sleeps`.
    fn launch_with(
        list: &[&str],
        config: fn(&CommandLine) -> Loaded,
        connector: &mut FakeConnector,
        sleeps: &mut Vec<Duration>,
    ) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut sleep = |d| sleeps.push(d);
        let runtime = Runtime {
            connector,
            sleep: &mut sleep,
            max_cycles: Some(3),
            now: fixed_now,
        };
        let code = run(args(list), config, runtime, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    /// Heure fixe des tests : le 16 novembre 2026 à 23 h 30 à Paris, juste
    /// avant minuit, sans qu'aucun test dépende de l'heure réelle.
    fn fixed_now() -> Timestamp {
        "2026-11-16T22:30:00Z".parse().unwrap()
    }

    /// Relevés publiés par GRDF : les `days` derniers jours avant le
    /// 16 novembre 2026 (aujourd'hui pour [`fixed_now`]), le plus ancien
    /// d'abord.
    fn published(days: i64) -> Vec<DailyReading> {
        let today = jiff::civil::date(2026, 11, 16);
        (1..=days)
            .rev()
            .map(|n| DailyReading {
                day: today.checked_sub(jiff::Span::new().days(n)).unwrap(),
                energy_kwh: Some(Quantity::from_millis(n * 1000)),
                volume_m3: Some(Quantity::from_millis(n * 100)),
            })
            .collect()
    }

    fn connector_publishing(days: i64) -> FakeConnector {
        FakeConnector::new(Script {
            published: published(days),
            ..Script::default()
        })
    }

    #[test]
    fn usage_errors_never_echo_values_or_stray_arguments() {
        for list in [
            &["check", PASSWORD][..],
            &["check", "--grdf-pce", "1", PASSWORD],
            &["check", "--ha-token", TOKEN],
            &["check", "--grdf-password=mot-de-passe-secret"],
            &["check", "--Mot-De-Passe-Secret"],
            &["check", "--inconnue=mot-de-passe-secret"],
            &["check", "--P@ss=mot-de-passe-secret"],
            &["mot-de-passe-secret"],
            &["Mot-De-Passe-Secret", "check"],
        ] {
            let (code, out, err) = launch(list, not_called);
            assert_eq!(code, 2, "{list:?}");
            assert!(out.is_empty());
            for secret in [PASSWORD, TOKEN, "Mot-De-Passe", "P@ss"] {
                assert!(!err.contains(secret), "{list:?} : {err}");
            }
        }
    }

    // --- Exécution ---

    #[test]
    fn version_and_help_need_no_config() {
        let (code, out, _) = launch(&["--version"], not_called);
        assert_eq!(code, 0);
        assert_eq!(
            out,
            format!("ha-gazpar {VERSION} (Home Assistant de référence {HA_REFERENCE})\n")
        );
        let (code, out, _) = launch(&["--help"], not_called);
        assert_eq!(code, 0);
        assert_eq!(out, usage());
        let (code, out, _) = launch(&["reset", "--help"], not_called);
        assert_eq!(code, 0);
        assert_eq!(out, usage());
    }

    #[test]
    fn usage_error_exits_2_with_hint() {
        let (code, out, err) = launch(&["frobnicate"], not_called);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert_eq!(
            err,
            format!(
                "ha-gazpar : usage incorrect : commande inconnue « frobnicate » \
                 (run, once, check ou reset)\n{USAGE_HINT}\n"
            )
        );
    }

    #[test]
    fn check_refuses_incomplete_config_with_clear_message() {
        let (code, out, err) = launch(&["check"], incomplete_config);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(
            err.starts_with("ha-gazpar : configuration invalide\n"),
            "{err}"
        );
        for name in [
            "GRDF_USERNAME",
            "GRDF_PASSWORD",
            "GRDF_PCE",
            "HA_URL",
            "HA_TOKEN",
        ] {
            assert!(
                err.contains(&format!("  - {name} manquant")),
                "{name} : {err}"
            );
        }
        assert!(!err.contains("Usage"));
    }

    #[test]
    fn check_prints_settings_with_origin_and_no_secret() {
        let (code, out, err) = launch(
            &[
                "check",
                "--grdf-pce",
                "01234567890123",
                "--grdf-username",
                "autre@example.org",
            ],
            valid_config,
        );
        assert_eq!(code, 0, "{err}");
        assert!(out.starts_with("Configuration valide.\n"), "{out}");
        let pce_line = out
            .lines()
            .find(|l| l.starts_with("GRDF_PCE "))
            .unwrap_or_default();
        assert!(pce_line.contains("01234567890123"), "{out}");
        assert!(pce_line.ends_with("(paramètre)"), "{out}");
        assert!(out.contains("gazpar:01234567890123_energy"));
        assert!(out.contains("ws://127.0.0.1:8123/api/websocket"));
        for secret in [PASSWORD, TOKEN, EMAIL, "autre@example.org"] {
            assert!(!out.contains(secret) && !err.contains(secret), "{secret}");
        }
    }

    #[test]
    fn check_connects_to_both_and_writes_nothing() {
        let mut connector = connector_publishing(10);
        let (code, out, err) =
            launch_with(&["check"], valid_config, &mut connector, &mut Vec::new());
        assert_eq!(code, 0, "{err}");
        let ha = out
            .find("Home Assistant : connecté, version 2026.9.4, fuseau Europe/Paris")
            .unwrap_or_else(|| panic!("{out}"));
        let grdf = out
            .find("GRDF : connecté")
            .unwrap_or_else(|| panic!("{out}"));
        assert!(ha > out.find("GRDF_PCE").unwrap() && grdf > ha, "{out}");
        assert!(
            out.contains("  gazpar:12345678901234_energy : aucun jour connu\n"),
            "{out}"
        );
        assert!(out.contains("Derniers jours lus chez GRDF"), "{out}");
        assert_eq!(out.matches("\n  20").count(), 7, "7 jours au plus : {out}");
        assert!(
            out.contains("gazpar:12345678901234_volume : 10 jours, du "),
            "{out}"
        );
        let seen = connector.seen.borrow();
        assert_eq!(seen.imports, 0, "check n'écrit rien");
        assert_eq!((seen.ha_connections, seen.ha_closes), (1, 1));
        assert_eq!(seen.grdf_calls.len(), 1, "une seule connexion à GRDF");
        assert!(err.contains(" INFO ha-gazpar "), "{err}");
        assert!(
            err.contains(" INFO connecté à Home Assistant 2026.9.4"),
            "{err}"
        );
    }

    #[test]
    fn check_fails_with_code_1_when_a_service_fails() {
        let mut connector = FakeConnector::new(Script {
            ha_error: Some(Error::HomeAssistant(TOKEN_REFUSED.into())),
            ..Script::default()
        });
        let (code, out, err) =
            launch_with(&["check"], valid_config, &mut connector, &mut Vec::new());
        assert_eq!(code, 1);
        assert!(
            out.contains("GRDF_PCE"),
            "le tableau est affiché d'abord : {out}"
        );
        assert!(
            err.ends_with(&format!("ha-gazpar : {TOKEN_REFUSED}\n")),
            "{err}"
        );
        assert_eq!(
            connector.seen.borrow().grdf_clients,
            0,
            "GRDF n'est pas contacté"
        );

        let mut connector = FakeConnector::new(Script {
            grdf_errors: vec![Error::GrdfCredentialsRejected(REJECTED.into())],
            ..Script::default()
        });
        let (code, out, err) =
            launch_with(&["check"], valid_config, &mut connector, &mut Vec::new());
        assert_eq!(code, 1);
        assert!(out.contains("Home Assistant : connecté"), "{out}");
        assert!(
            err.contains(&format!(
                "ha-gazpar : {REJECTED} ; vérifiez GRDF_USERNAME et GRDF_PASSWORD"
            )),
            "{err}"
        );
        assert_eq!(
            connector.seen.borrow().ha_closes,
            1,
            "HA fermé malgré l'erreur"
        );
    }

    #[test]
    fn once_imports_then_imports_nothing() {
        let mut connector = connector_publishing(3);
        let (code, out, err) =
            launch_with(&["once"], valid_config, &mut connector, &mut Vec::new());
        assert_eq!(code, 0, "{err}");
        assert!(out.is_empty(), "{out}");
        assert!(
            err.contains(" INFO relevé : 3 jours importés, dernier jour connu "),
            "{err}"
        );
        assert_eq!(connector.seen.borrow().imports, 2);

        let (code, _, err) = launch_with(&["once"], valid_config, &mut connector, &mut Vec::new());
        assert_eq!(code, 0, "{err}");
        assert!(
            err.contains(" INFO relevé : aucun jour importé, dernier jour connu "),
            "{err}"
        );
        let seen = connector.seen.borrow();
        assert_eq!(seen.imports, 2, "rien de plus");
        assert_eq!(
            (seen.ha_connections, seen.ha_closes, seen.grdf_clients),
            (2, 2, 2)
        );
    }

    #[test]
    fn once_logs_the_error_and_exits_1() {
        let mut connector = FakeConnector::new(Script {
            grdf_errors: vec![Error::Http(TIMEOUT.into())],
            ..Script::default()
        });
        let (code, out, err) =
            launch_with(&["once"], valid_config, &mut connector, &mut Vec::new());
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert!(
            err.trim_end().ends_with(&format!(" ERREUR {TIMEOUT}")),
            "{err}"
        );
        assert_eq!(connector.seen.borrow().ha_closes, 1);
    }

    #[test]
    fn failed_close_of_home_assistant_is_not_fatal() {
        let mut connector = FakeConnector::new(Script {
            close_error: Some(Error::Io(CLOSE_FAILED.into())),
            published: published(2),
            ..Script::default()
        });
        let (code, _, err) = launch_with(&["once"], valid_config, &mut connector, &mut Vec::new());
        assert_eq!(code, 0, "{err}");
        assert!(
            err.contains(&format!(
                " ATTENTION {CLOSE_FAILED} ; sans conséquence sur ce qui a été fait\n"
            )),
            "{err}"
        );
        assert!(err.contains("relevé : 2 jours importés"), "{err}");
    }

    #[test]
    fn old_home_assistant_gives_a_warning() {
        let mut connector = FakeConnector::new(Script {
            ha_version: "2026.8.3".into(),
            ..Script::default()
        });
        let (code, _, err) = launch_with(&["once"], valid_config, &mut connector, &mut Vec::new());
        assert_eq!(code, 0, "{err}");
        assert!(
            err.contains(
                " ATTENTION Home Assistant 2026.8.3 est antérieur à la version de référence"
            ),
            "{err}"
        );
    }

    #[test]
    fn reset_with_yes_clears_without_grdf() {
        let mut connector = FakeConnector::default();
        let (code, out, err) = launch_with(
            &["reset", "--yes"],
            valid_config,
            &mut connector,
            &mut Vec::new(),
        );
        assert_eq!(code, 0, "{err}");
        assert!(
            out.starts_with(
                "Statistiques effacées dans Home Assistant :\n  gazpar:12345678901234_energy\n  \
                 gazpar:12345678901234_volume\n"
            ),
            "{out}"
        );
        let seen = connector.seen.borrow();
        assert_eq!(seen.clears.len(), 1);
        assert_eq!(seen.grdf_clients, 0);
        assert_eq!(seen.ha_closes, 1);
    }

    #[test]
    fn run_keeps_going_and_waits_the_full_interval() {
        let mut connector = FakeConnector::new(Script {
            grdf_errors: vec![Error::Http(TIMEOUT.into())],
            published: published(2),
            ..Script::default()
        });
        let mut sleeps = Vec::new();
        let (code, _, err) = launch_with(
            &["run", "--gazpar-scan-interval", "60"],
            valid_config,
            &mut connector,
            &mut sleeps,
        );
        assert_eq!(code, 0, "{err}");
        assert_eq!(sleeps, [Duration::from_secs(3600); 3]);
        assert!(
            err.contains(&format!(
                " ATTENTION {TIMEOUT} ; nouvel essai au prochain relevé, dans 60 min"
            )),
            "{err}"
        );
        assert!(err.contains("relevé : 2 jours importés"), "{err}");
        assert!(err.contains("relevé : aucun jour importé"), "{err}");
        let seen = connector.seen.borrow();
        assert_eq!(
            (seen.ha_connections, seen.grdf_clients),
            (3, 3),
            "connexions neuves à chaque relevé"
        );
    }

    #[test]
    fn run_stops_contacting_grdf_after_rejected_credentials() {
        let mut connector = FakeConnector::new(Script {
            grdf_errors: vec![Error::GrdfCredentialsRejected(REJECTED.into())],
            ..Script::default()
        });
        let mut sleeps = Vec::new();
        let (code, _, err) = launch_with(&["run"], valid_config, &mut connector, &mut sleeps);
        assert_eq!(code, 0, "{err}");
        assert_eq!(sleeps.len(), 3);
        assert_eq!(err.matches(" ERREUR ").count(), 3, "{err}");
        let seen = connector.seen.borrow();
        assert_eq!(seen.grdf_calls.len(), 1, "plus aucun appel à GRDF");
        assert_eq!(seen.ha_connections, 1);
    }

    #[test]
    fn run_does_not_contact_grdf_when_home_assistant_is_down() {
        let mut connector = FakeConnector::new(Script {
            ha_error: Some(Error::Io(HA_DOWN.into())),
            ..Script::default()
        });
        let (code, _, err) = launch_with(&["run"], valid_config, &mut connector, &mut Vec::new());
        assert_eq!(code, 0);
        assert_eq!(
            err.matches(&format!(
                " ATTENTION {HA_DOWN} ; nouvel essai au prochain relevé, dans 480 min\n"
            ))
            .count(),
            3,
            "{err}"
        );
        assert_eq!(connector.seen.borrow().grdf_clients, 0);
    }

    #[test]
    fn config_warnings_are_logged_on_stderr() {
        let (code, _, err) = launch(&["check"], config_with_warning);
        assert_eq!(code, 0);
        assert!(
            err.contains(" ATTENTION variable d'environnement GAZPAR_TIMEZONE inconnue"),
            "{err}"
        );
    }

    #[test]
    fn reset_without_yes_lists_statistics_and_exits_2() {
        let (code, out, err) = launch(&["reset"], valid_config);
        assert_eq!(code, EXIT_RESET_NOT_CONFIRMED);
        assert_eq!(
            out,
            "Statistiques qui seraient effacées dans Home Assistant :\n  \
             gazpar:12345678901234_energy\n  gazpar:12345678901234_volume\n"
        );
        assert_eq!(
            err,
            "ha-gazpar : rien n'a été effacé ; pour confirmer : ha-gazpar reset --yes\n"
        );
        let (code, _, err) = launch(&["reset"], incomplete_config);
        assert_eq!(code, 2);
        assert!(err.contains("configuration invalide"), "{err}");
    }

    #[test]
    fn invalid_ha_url_is_a_configuration_error() {
        fn bad_port(command_line: &CommandLine) -> Loaded {
            let mut env = valid_env();
            env.insert("HA_URL".into(), "ws://ha:abc".into());
            config::load(command_line, &env, &NoFiles)
        }
        for cmd in ["check", "once", "run", "reset"] {
            let mut connector = FakeConnector::default();
            let (code, out, err) = launch_with(&[cmd], bad_port, &mut connector, &mut Vec::new());
            assert_eq!(code, 2, "{cmd} : {err}");
            assert!(out.is_empty(), "{cmd} : {out}");
            assert!(
                err.contains("HA_URL (environnement) : adresse invalide : port invalide"),
                "{cmd} : {err}"
            );
            assert_eq!(connector.seen.borrow().ha_connections, 0, "{cmd}");
        }
    }

    #[test]
    fn other_commands_validate_config_first() {
        for cmd in ["run", "once", "reset"] {
            let (code, _, err) = launch(&[cmd], incomplete_config);
            assert_eq!(code, 2, "{cmd}");
            assert!(err.contains("configuration invalide"), "{cmd} : {err}");
        }
    }

    #[test]
    fn command_line_reaches_the_configuration() {
        fn capture(command_line: &CommandLine) -> Loaded {
            assert_eq!(command_line.config_path.as_deref(), Some("/etc/x.conf"));
            assert_eq!(
                command_line.params,
                vec![("GAZPAR_OVERLAP_DAYS", "60".to_owned())]
            );
            valid_config(&CommandLine::default())
        }
        let (code, _, err) = launch(
            &[
                "check",
                "--config=/etc/x.conf",
                "--gazpar-overlap-days",
                "60",
            ],
            capture,
        );
        assert_eq!(code, 0, "{err}");
    }
}

//! Ligne de commande : point d'entrée [`main`], aiguillage par commande et
//! codes de sortie (MANUEL.md §5, §7). L'analyse des arguments est dans
//! [`args`].
//!
//! C'est le seul module qui assemble les autres. À ce stade, `check` valide
//! la configuration et l'affiche avec la provenance de chaque valeur ;
//! `reset` sans `--yes` affiche les statistiques qui seraient effacées et
//! sort avec le code 2 ; `run`, `once` et `reset --yes` valident la
//! configuration puis signalent qu'ils ne sont pas encore implémentés.
//!
//! Codes de sortie (MANUEL.md §7) : 0 succès, 1 erreur d'exécution, 2
//! configuration ou ligne de commande incorrecte, ou `reset` sans `--yes`.
//!
//! Dépendances : tous les modules (point d'assemblage).

pub mod args;

use std::ffi::OsString;
use std::io::Write;

use crate::config::{self, CommandLine, Config, Loaded};
use crate::domain::Measure;
use crate::error::Error;
use crate::{HA_REFERENCE, VERSION};

use args::USAGE_HINT;
pub use args::{Command, Invocation, Parsed, parse_args, usage};

/// Code de sortie de `reset` sans `--yes` (MANUEL.md §5).
pub const EXIT_RESET_NOT_CONFIRMED: i32 = 2;

/// Point d'entrée appelé par `main.rs` : renvoie le code de sortie.
pub fn main(args: impl IntoIterator<Item = OsString>) -> i32 {
    run(
        args,
        config::load_from_process,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

/// Exécute la ligne de commande avec un chargement de configuration et des
/// sorties injectés (testable sans environnement ni terminal).
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    load_config: impl FnOnce(&CommandLine) -> Loaded,
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
        Parsed::Execute(invocation) => execute(&invocation, load_config, out, err),
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
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, Error> {
    let config = load(|| load_config(&invocation.config), err)?;
    crate::log::init(config.log_level);
    match invocation.command {
        Command::Check => {
            writeln!(out, "Configuration valide.")?;
            write!(out, "{}", config.report())?;
            writeln!(
                out,
                "\nConnexions à Home Assistant et à GRDF : pas encore vérifiées, \
                 ces vérifications viendront dans une prochaine version."
            )?;
            Ok(0)
        }
        Command::Reset if !invocation.yes => {
            writeln!(
                out,
                "Statistiques qui seraient effacées dans Home Assistant :"
            )?;
            for m in Measure::ALL {
                writeln!(out, "  {}", config.statistic_id(m))?;
            }
            writeln!(
                err,
                "ha-gazpar : rien n'a été effacé ; pour confirmer : ha-gazpar reset --yes"
            )?;
            Ok(EXIT_RESET_NOT_CONFIRMED)
        }
        Command::Reset => Err(Error::NotImplemented(
            "ha-gazpar reset --yes (itération ultérieure)",
        )),
        Command::Run => Err(Error::NotImplemented(
            "ha-gazpar run (itération ultérieure)",
        )),
        Command::Once => Err(Error::NotImplemented(
            "ha-gazpar once (itération ultérieure)",
        )),
    }
}

/// Charge la configuration et journalise ses avertissements sur `err`, même
/// si elle est refusée : ils expliquent souvent l'erreur (faute de frappe
/// dans un nom de variable).
fn load(load_config: impl FnOnce() -> Loaded, err: &mut dyn Write) -> Result<Config, Error> {
    let loaded = load_config();
    for warning in &loaded.warnings {
        let line = crate::log::format_line(
            jiff::Timestamp::now(),
            crate::log::Level::Warn,
            format_args!("{warning}"),
        );
        writeln!(err, "{line}")?;
    }
    loaded.config
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const PASSWORD: &str = "mot-de-passe-secret";
    const TOKEN: &str = "jeton-secret";
    const EMAIL: &str = "jean@example.org";

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

    /// Lance la ligne de commande ; renvoie (code, stdout, stderr).
    fn launch(list: &[&str], config: fn(&CommandLine) -> Loaded) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(args(list), config, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
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
        assert!(
            out.trim_end().ends_with(
                "Connexions à Home Assistant et à GRDF : pas encore vérifiées, \
                 ces vérifications viendront dans une prochaine version."
            ),
            "{out}"
        );
        for secret in [PASSWORD, TOKEN, EMAIL, "autre@example.org"] {
            assert!(!out.contains(secret) && !err.contains(secret), "{secret}");
        }
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
    fn other_commands_validate_config_first() {
        for cmd in ["run", "once", "reset"] {
            let (code, _, err) = launch(&[cmd], incomplete_config);
            assert_eq!(code, 2, "{cmd}");
            assert!(err.contains("configuration invalide"), "{cmd} : {err}");
        }
    }

    #[test]
    fn other_commands_are_not_implemented_yet() {
        for list in [&["run"][..], &["once"], &["reset", "--yes"]] {
            let (code, out, err) = launch(list, valid_config);
            assert_eq!(code, 1, "{list:?}");
            assert!(out.is_empty());
            assert!(
                err.starts_with(&format!(
                    "ha-gazpar : pas encore implémenté : ha-gazpar {}",
                    list.join(" ")
                )),
                "{list:?} : {err}"
            );
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

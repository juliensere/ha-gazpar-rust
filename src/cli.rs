//! Ligne de commande : sous-commandes `run`, `once`, `check`, `reset`, plus
//! `--version` et `--help` (bibliothèque standard uniquement).
//!
//! C'est le seul module qui assemble les autres : configuration, client GRDF,
//! client Home Assistant et synchronisation. À ce stade (étape 1), `check`
//! valide la configuration et en affiche un résumé sans secret ; `run`,
//! `once` et `reset` valident la configuration puis signalent qu'ils ne sont
//! pas encore implémentés.
//!
//! Codes de sortie : 0 succès, 2 erreur de configuration ou d'usage, 1 autre
//! erreur.
//!
//! Dépendances : tous les modules (point d'assemblage).

use std::ffi::OsString;
use std::io::Write;

use crate::config::{self, CommandLine, Config, Loaded};
use crate::error::Error;
use crate::{HA_REFERENCE, VERSION};

/// Texte d'aide.
pub const USAGE: &str = "\
Usage : ha-gazpar <commande>

Importe la consommation de gaz GRDF (Gazpar) dans les statistiques long terme
de Home Assistant.

Commandes :
  run        démon : un relevé au démarrage puis toutes les GAZPAR_SCAN_INTERVAL minutes
  once       un seul relevé puis sortie
  check      valide la configuration et affiche ce qui serait fait, sans rien écrire
  reset      efface les statistiques importées puis sort
  --version  affiche la version
  --help     affiche cette aide

Configuration par variables d'environnement : voir .env.example.
Codes de sortie : 0 succès, 2 configuration ou usage incorrects, 1 autre erreur.
";

/// Sous-commande demandée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Run,
    Once,
    Check,
    Reset,
    Version,
    Help,
}

/// Analyse les arguments (sans le nom du programme).
pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Command, Error> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Err(Error::Usage("commande manquante".to_owned()));
    };
    let Some(first) = first.to_str() else {
        return Err(Error::Usage("argument non UTF-8".to_owned()));
    };
    let command = match first {
        "run" => Command::Run,
        "once" => Command::Once,
        "check" => Command::Check,
        "reset" => Command::Reset,
        "--version" | "-V" | "version" => Command::Version,
        "--help" | "-h" | "help" => Command::Help,
        other => return Err(Error::Usage(format!("commande inconnue « {other} »"))),
    };
    if let Some(extra) = args.next() {
        return Err(Error::Usage(format!(
            "argument en trop « {} »",
            extra.to_string_lossy()
        )));
    }
    Ok(command)
}

/// Point d'entrée appelé par `main.rs` : renvoie le code de sortie.
pub fn main(args: impl IntoIterator<Item = OsString>) -> i32 {
    run(
        args,
        || config::load_from_process(&CommandLine::default()),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

/// Exécute la ligne de commande avec une configuration et des sorties
/// injectées (testable sans environnement ni terminal).
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    load_config: impl FnOnce() -> Loaded,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let result = parse_args(args).and_then(|command| execute(command, load_config, out, err));
    match result {
        Ok(()) => 0,
        Err(e) => {
            // Rien de plus à faire si la sortie d'erreur elle-même échoue.
            let _ = writeln!(err, "ha-gazpar : {e}");
            if matches!(e, Error::Usage(_)) {
                let _ = write!(err, "\n{USAGE}");
            }
            e.exit_code()
        }
    }
}

fn execute(
    command: Command,
    load_config: impl FnOnce() -> Loaded,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Error> {
    match command {
        Command::Help => {
            write!(out, "{USAGE}")?;
            return Ok(());
        }
        Command::Version => {
            writeln!(
                out,
                "ha-gazpar {VERSION} (Home Assistant de référence {HA_REFERENCE})"
            )?;
            return Ok(());
        }
        Command::Run | Command::Once | Command::Check | Command::Reset => {}
    }

    let config = load(load_config, err)?;
    crate::log::init(config.log_level);
    match command {
        Command::Check => {
            writeln!(out, "Configuration valide.")?;
            write!(out, "{}", config.report())?;
            writeln!(
                out,
                "Connexions à GRDF et à Home Assistant : pas encore vérifiées (itération ultérieure)."
            )?;
            Ok(())
        }
        Command::Run => Err(Error::NotImplemented(
            "ha-gazpar run (itération ultérieure)",
        )),
        Command::Once => Err(Error::NotImplemented(
            "ha-gazpar once (itération ultérieure)",
        )),
        Command::Reset => Err(Error::NotImplemented(
            "ha-gazpar reset (itération ultérieure)",
        )),
        Command::Help | Command::Version => Ok(()),
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

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn valid_config() -> Loaded {
        let env: std::collections::BTreeMap<String, String> = [
            ("GRDF_USERNAME", "jean@example.org"),
            ("GRDF_PASSWORD", "mot-de-passe-secret"),
            ("GRDF_PCE", "12345678901234"),
            ("HA_URL", "ws://127.0.0.1:8123/api/websocket"),
            ("HA_TOKEN", "jeton-secret"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        config::load(&CommandLine::default(), &env, &NoFiles)
    }

    fn incomplete_config() -> Loaded {
        config::load(&CommandLine::default(), &Default::default(), &NoFiles)
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

    /// Lance la ligne de commande ; renvoie (code, stdout, stderr).
    fn launch(list: &[&str], config: fn() -> Loaded) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(args(list), config, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    // --- Analyse des arguments ---

    #[test]
    fn parse_subcommands() {
        for (word, expected) in [
            ("run", Command::Run),
            ("once", Command::Once),
            ("check", Command::Check),
            ("reset", Command::Reset),
            ("--version", Command::Version),
            ("-V", Command::Version),
            ("--help", Command::Help),
            ("-h", Command::Help),
            ("help", Command::Help),
        ] {
            assert_eq!(parse_args(args(&[word])).unwrap(), expected, "{word}");
        }
    }

    #[test]
    fn parse_errors() {
        assert_eq!(
            parse_args(args(&[])).unwrap_err(),
            Error::Usage("commande manquante".into())
        );
        assert_eq!(
            parse_args(args(&["sync"])).unwrap_err(),
            Error::Usage("commande inconnue « sync »".into())
        );
        assert_eq!(
            parse_args(args(&["run", "--force"])).unwrap_err(),
            Error::Usage("argument en trop « --force »".into())
        );
        assert!(parse_args(args(&["RUN"])).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn parse_non_utf8_argument() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![0x72, 0xff]);
        assert_eq!(
            parse_args([bad]).unwrap_err(),
            Error::Usage("argument non UTF-8".into())
        );
    }

    // --- Exécution ---

    #[test]
    fn version_and_help_need_no_config() {
        let (code, out, _) = launch(&["--version"], incomplete_config);
        assert_eq!(code, 0);
        assert_eq!(
            out,
            format!("ha-gazpar {VERSION} (Home Assistant de référence {HA_REFERENCE})\n")
        );
        let (code, out, _) = launch(&["--help"], incomplete_config);
        assert_eq!(code, 0);
        assert!(out.starts_with("Usage : ha-gazpar"));
    }

    #[test]
    fn usage_error_exits_2_with_help() {
        let (code, out, err) = launch(&["frobnicate"], valid_config);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(
            err.starts_with("ha-gazpar : usage incorrect : commande inconnue"),
            "{err}"
        );
        assert!(err.contains("Usage : ha-gazpar"));
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
    }

    #[test]
    fn check_prints_summary_without_secret() {
        let (code, out, err) = launch(&["check"], valid_config);
        assert_eq!(code, 0, "{err}");
        assert!(out.starts_with("Configuration valide.\n"), "{out}");
        assert!(out.contains("gazpar:12345678901234_energy"));
        for secret in ["mot-de-passe-secret", "jeton-secret", "jean@example.org"] {
            assert!(!out.contains(secret) && !err.contains(secret), "{secret}");
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
    fn other_commands_are_not_implemented_yet() {
        for cmd in ["run", "once", "reset"] {
            let (code, out, err) = launch(&[cmd], valid_config);
            assert_eq!(code, 1, "{cmd}");
            assert!(out.is_empty());
            assert!(
                err.starts_with(&format!(
                    "ha-gazpar : pas encore implémenté : ha-gazpar {cmd}"
                )),
                "{cmd} : {err}"
            );
        }
    }
}

//! Ligne de commande (MANUEL.md §5) : `ha-gazpar <commande> [options]`,
//! commandes `run`, `once`, `check`, `reset`, plus `--help` et `--version`
//! (bibliothèque standard uniquement).
//!
//! La commande vient d'abord, les options ensuite, sous la forme
//! `--option valeur` ou `--option=valeur`. Options de la commande :
//! `--config`, `--yes` (avec `reset` seulement), `--help` (seule ou après une
//! commande), `--version` (seule). Les paramètres de configuration se
//! déduisent de la table de `config` ([`config::parameters`]) : aucune liste
//! n'est recopiée ici. Une option répétée, un paramètre vide, une option
//! inconnue (avec suggestion), `--grdf-password` et `--ha-token` (message qui
//! renvoie vers les variantes `-file`) sont des erreurs d'usage.
//!
//! **Aucun message d'erreur ne recopie une valeur ni un argument quelconque**
//! : seulement des noms d'options connus ou qui en ont la forme
//! (`--grdf-pce`). Un mot de passe tapé par erreur sur la ligne de commande
//! ne doit pas finir dans le journal.
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

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write;

use crate::config::{self, CommandLine, Config, Loaded, OPTIONS, ParameterTarget};
use crate::config_file;
use crate::domain::Measure;
use crate::error::Error;
use crate::{HA_REFERENCE, VERSION};

/// Code de sortie de `reset` sans `--yes` (MANUEL.md §5).
pub const EXIT_RESET_NOT_CONFIRMED: i32 = 2;

/// Longueur maximale d'un nom d'option inconnu recopié dans un message.
const MAX_ECHOED_OPTION: usize = 64;

/// Commande demandée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Run,
    Once,
    Check,
    Reset,
}

impl Command {
    const ALL: [Command; 4] = [Command::Run, Command::Once, Command::Check, Command::Reset];

    pub fn name(self) -> &'static str {
        match self {
            Command::Run => "run",
            Command::Once => "once",
            Command::Check => "check",
            Command::Reset => "reset",
        }
    }
}

/// Une commande et ses options.
///
/// `Debug` ne montre pas les valeurs des paramètres (voir [`CommandLine`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub command: Command,
    /// `--yes` (accepté avec `reset` seulement).
    pub yes: bool,
    /// `--config` et paramètres de configuration, transmis à `config`.
    pub config: CommandLine,
}

/// Ce que demande la ligne de commande.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Help,
    Version,
    Execute(Invocation),
}

/// Texte d'aide. La liste des options de configuration est tirée de la
/// table de `config`.
pub fn usage() -> String {
    let mut s = String::from(
        "\
Usage : ha-gazpar <commande> [options]

Importe la consommation de gaz GRDF (Gazpar) dans les statistiques long terme
de Home Assistant.

Commandes :
  run     mode démon : un relevé tout de suite, puis toutes les
          GAZPAR_SCAN_INTERVAL minutes
  once    un seul relevé, puis sortie
  check   vérifie la configuration (puis, dans une prochaine version, les
          connexions à Home Assistant et à GRDF) ; n'écrit rien
  reset   efface dans Home Assistant les deux statistiques de ha-gazpar ;
          exige --yes

Options de la commande, écrites après la commande (--option valeur ou
--option=valeur) :
  --config <chemin>  fichier de configuration NOM=valeur (sinon : variable
                     GAZPAR_CONFIG ; aucun fichier lu par défaut)
  --yes              confirme reset ; refusé avec les autres commandes
  --help             cette aide ; seule ou après une commande
  --version          version du programme et de Home Assistant de référence

Options de configuration : chacune se donne en paramètre, en variable
d'environnement ou dans le fichier, sous le nom de la variable (GRDF_PCE pour
--grdf-pce). Priorité : paramètre, puis environnement, puis fichier, puis
défaut. Une variante -file (_FILE) donne le chemin absolu d'un fichier qui
contient la valeur.
",
    );
    let rows: Vec<(String, String)> = OPTIONS
        .iter()
        .map(|o| {
            let variant = o.file_variant.map(config::parameter);
            let left = match (o.on_command_line, variant) {
                (true, Some(v)) => format!("{}, {v}", config::parameter(o.name)),
                (true, None) => config::parameter(o.name),
                (false, Some(v)) => v,
                (false, None) => o.name.to_owned(),
            };
            let mut right = o.help.to_owned();
            match o.default {
                Some(d) => {
                    let _ = write!(right, " ; défaut {d}");
                }
                None => right.push_str(" ; obligatoire"),
            }
            if !o.on_command_line {
                let _ = write!(
                    right,
                    " ; jamais {} en paramètre",
                    config::parameter(o.name)
                );
            }
            (left, right)
        })
        .collect();
    let width = rows
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    for (left, right) in rows {
        // `write!` sur une String ne peut pas échouer.
        let _ = writeln!(s, "  {left:<width$}{right}");
    }
    s.push_str(
        "
Codes de sortie : 0 succès ; 1 erreur d'exécution (GRDF ou Home Assistant) ;
2 configuration ou ligne de commande incorrecte, ou reset sans --yes.
Manuel complet : docs/MANUEL.md
",
    );
    s
}

/// Rappel affiché après une erreur d'usage.
const USAGE_HINT: &str = "Usage : ha-gazpar <commande> [options] ; aide : ha-gazpar --help";

fn usage_error(message: impl Into<String>) -> Error {
    Error::Usage(message.into())
}

/// Analyse les arguments (sans le nom du programme).
pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Parsed, Error> {
    let args: Vec<String> = args
        .into_iter()
        .map(|a| {
            a.into_string()
                .map_err(|_| usage_error("argument non UTF-8"))
        })
        .collect::<Result<_, _>>()?;
    let Some((first, rest)) = args.split_first() else {
        return Err(usage_error("commande manquante"));
    };
    let command = match first.as_str() {
        "--help" if rest.is_empty() => return Ok(Parsed::Help),
        "--version" if rest.is_empty() => return Ok(Parsed::Version),
        "--help" => {
            return Err(usage_error(
                "--help s'emploie seul ou après la commande (ex. ha-gazpar check --help)",
            ));
        }
        "--version" => return Err(usage_error("--version s'emploie seul")),
        word => match Command::ALL.into_iter().find(|c| c.name() == word) {
            Some(command) => command,
            None if word.starts_with('-') => {
                return Err(usage_error(
                    "commande manquante : la commande vient d'abord, les options ensuite \
                     (ex. ha-gazpar check --config /etc/ha-gazpar.conf)",
                ));
            }
            None if looks_like_command(word) => {
                return Err(usage_error(format!(
                    "commande inconnue « {word} » (run, once, check ou reset)"
                )));
            }
            None => {
                return Err(usage_error("commande inconnue (run, once, check ou reset)"));
            }
        },
    };
    if rest.iter().any(|a| a == "--help") {
        return Ok(Parsed::Help);
    }

    let parameters = config::parameters();
    let mut invocation = Invocation {
        command,
        yes: false,
        config: CommandLine::default(),
    };
    let mut seen: Vec<String> = Vec::new();
    let mut index = 0;
    while let Some(arg) = rest.get(index) {
        index += 1;
        let Some(stripped) = arg.strip_prefix("--") else {
            // L'argument n'est pas recopié : ce peut être un secret.
            return Err(usage_error(format!(
                "argument inattendu en position {} (les options s'écrivent --nom valeur \
                 ou --nom=valeur)",
                index + 1
            )));
        };
        let (name, inline) = match stripped.split_once('=') {
            Some((n, v)) => (format!("--{n}"), Some(v.to_owned())),
            None => (arg.clone(), None),
        };
        if seen.contains(&name) {
            return Err(usage_error(format!("{name} donné deux fois")));
        }
        match name.as_str() {
            "--yes" => {
                if inline.is_some() {
                    return Err(usage_error("--yes ne prend pas de valeur"));
                }
                invocation.yes = true;
            }
            "--version" => return Err(usage_error("--version s'emploie seul")),
            "--config" => {
                let value = take_value(&name, inline, rest, &mut index)?;
                invocation.config.config_path = Some(value);
            }
            _ => match parameters.iter().find(|(p, _)| *p == name) {
                Some((_, ParameterTarget::Accepted(variable))) => {
                    let value = take_value(&name, inline, rest, &mut index)?;
                    invocation.config.params.push((variable, value));
                }
                Some((_, ParameterTarget::Refused { instead, .. })) => {
                    return Err(usage_error(format!(
                        "{name} refusé : un paramètre est visible de tous les utilisateurs \
                         de la machine (ps, docker inspect) et reste dans l'historique du \
                         shell ; utiliser le fichier de configuration, une variable \
                         d'environnement ou {instead}"
                    )));
                }
                None => return Err(unknown_option(&name, &parameters)),
            },
        }
        seen.push(name);
    }
    if invocation.yes && command != Command::Reset {
        return Err(usage_error("--yes n'est accepté qu'avec reset"));
    }
    Ok(Parsed::Execute(invocation))
}

/// Valeur d'une option : après `=`, sinon l'argument suivant. Vide ou
/// absente : erreur.
fn take_value(
    name: &str,
    inline: Option<String>,
    rest: &[String],
    index: &mut usize,
) -> Result<String, Error> {
    let value = match inline {
        Some(v) => v,
        None => match rest.get(*index) {
            Some(next) if !next.starts_with("--") => {
                *index += 1;
                next.clone()
            }
            _ => return Err(usage_error(format!("{name} : valeur manquante"))),
        },
    };
    if value.is_empty() {
        return Err(usage_error(format!("{name} : valeur vide")));
    }
    Ok(value)
}

/// Erreur d'option inconnue, avec suggestion. Le nom n'est recopié que s'il
/// a la forme d'une option.
fn unknown_option(name: &str, parameters: &[(String, ParameterTarget)]) -> Error {
    let own = ["--config", "--yes", "--help", "--version"];
    let candidates = parameters.iter().map(|(p, _)| p.as_str()).chain(own);
    let hint = config_file::closest(name, candidates)
        .map(|s| {
            // Pas de suggestion vers un paramètre refusé : sa variante -file.
            match parameters.iter().find(|(p, _)| p == s) {
                Some((_, ParameterTarget::Refused { instead, .. })) => instead.clone(),
                _ => s.to_owned(),
            }
        })
        .map(|s| format!(" (voulez-vous dire {s} ?)"))
        .unwrap_or_default();
    if looks_like_option(name) {
        usage_error(format!("option inconnue {name}{hint}"))
    } else {
        usage_error(format!("option inconnue{hint}"))
    }
}

/// Forme d'une option : `--` puis minuscules, chiffres ou `-`.
fn looks_like_option(name: &str) -> bool {
    name.len() <= MAX_ECHOED_OPTION
        && name.strip_prefix("--").is_some_and(|n| {
            !n.is_empty()
                && n.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Forme d'un nom de commande : quelques minuscules.
fn looks_like_command(word: &str) -> bool {
    (1..=16).contains(&word.len()) && word.bytes().all(|b| b.is_ascii_lowercase())
}

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

    fn parse(list: &[&str]) -> Result<Parsed, Error> {
        parse_args(args(list))
    }

    fn invocation(list: &[&str]) -> Invocation {
        match parse(list) {
            Ok(Parsed::Execute(i)) => i,
            other => panic!("{list:?} : {other:?}"),
        }
    }

    fn usage_message(list: &[&str]) -> String {
        match parse(list) {
            Err(Error::Usage(m)) => m,
            other => panic!("{list:?} : {other:?}"),
        }
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

    // --- Commandes ---

    #[test]
    fn parse_commands() {
        for (word, expected) in [
            ("run", Command::Run),
            ("once", Command::Once),
            ("check", Command::Check),
            ("reset", Command::Reset),
        ] {
            let i = invocation(&[word]);
            assert_eq!(i.command, expected, "{word}");
            assert!(!i.yes);
            assert_eq!(i.config, CommandLine::default());
        }
        assert_eq!(parse(&["--help"]).unwrap(), Parsed::Help);
        assert_eq!(parse(&["--version"]).unwrap(), Parsed::Version);
    }

    #[test]
    fn missing_or_unknown_command() {
        assert_eq!(usage_message(&[]), "commande manquante");
        assert_eq!(
            usage_message(&["sync"]),
            "commande inconnue « sync » (run, once, check ou reset)"
        );
        // Ni majuscules, ni abréviations hors du manuel.
        for word in ["RUN", "help", "version"] {
            assert!(
                usage_message(&[word]).starts_with("commande inconnue"),
                "{word}"
            );
        }
        for word in ["-h", "-V"] {
            assert!(
                usage_message(&[word]).starts_with("commande manquante"),
                "{word}"
            );
        }
    }

    #[test]
    fn options_come_after_the_command() {
        assert_eq!(
            usage_message(&["--config", "/etc/x.conf", "check"]),
            "commande manquante : la commande vient d'abord, les options ensuite \
             (ex. ha-gazpar check --config /etc/ha-gazpar.conf)"
        );
    }

    #[test]
    fn help_alone_or_after_a_command() {
        for list in [
            &["check", "--help"][..],
            &["reset", "--yes", "--help"],
            &["run", "--grdf-pce", "1", "--help"],
            // Même avec une option fautive : l'aide est demandée.
            &["once", "--inconnue", "--help"],
        ] {
            assert_eq!(parse(list).unwrap(), Parsed::Help, "{list:?}");
        }
        assert!(usage_message(&["--help", "check"]).starts_with("--help s'emploie seul"));
        assert!(usage_message(&["frobnicate", "--help"]).starts_with("commande inconnue"));
    }

    #[test]
    fn version_only_alone() {
        assert_eq!(
            usage_message(&["--version", "check"]),
            "--version s'emploie seul"
        );
        assert_eq!(
            usage_message(&["check", "--version"]),
            "--version s'emploie seul"
        );
    }

    // --- Options ---

    #[test]
    fn both_option_forms() {
        let i = invocation(&[
            "once",
            "--config",
            "/etc/ha-gazpar.conf",
            "--gazpar-log-level=debug",
            "--grdf-pce",
            "01234567890123",
            "--grdf-username",
            EMAIL,
            "--ha-token-file=/run/secrets/ha_token",
            "--gazpar-statistic-prefix=a=b",
        ]);
        assert_eq!(i.command, Command::Once);
        assert_eq!(i.config.config_path.as_deref(), Some("/etc/ha-gazpar.conf"));
        assert_eq!(
            i.config.params,
            vec![
                ("GAZPAR_LOG_LEVEL", "debug".to_owned()),
                ("GRDF_PCE", "01234567890123".to_owned()),
                ("GRDF_USERNAME", EMAIL.to_owned()),
                ("HA_TOKEN_FILE", "/run/secrets/ha_token".to_owned()),
                ("GAZPAR_STATISTIC_PREFIX", "a=b".to_owned()),
            ]
        );
    }

    #[test]
    fn every_derived_parameter_is_accepted() {
        for (param, target) in config::parameters() {
            if let ParameterTarget::Accepted(variable) = target {
                let i = invocation(&["check", param.as_str(), "/x"]);
                assert_eq!(
                    i.config.params,
                    vec![(variable, "/x".to_owned())],
                    "{param}"
                );
            }
        }
    }

    #[test]
    fn yes_only_with_reset() {
        assert!(invocation(&["reset", "--yes"]).yes);
        for cmd in ["run", "once", "check"] {
            assert_eq!(
                usage_message(&[cmd, "--yes"]),
                "--yes n'est accepté qu'avec reset",
                "{cmd}"
            );
        }
        assert_eq!(
            usage_message(&["reset", "--yes=oui"]),
            "--yes ne prend pas de valeur"
        );
    }

    #[test]
    fn repeated_option_is_an_error() {
        assert_eq!(
            usage_message(&["check", "--grdf-pce=1", "--grdf-pce", "2"]),
            "--grdf-pce donné deux fois"
        );
        assert_eq!(
            usage_message(&["check", "--config", "/a", "--config=/b"]),
            "--config donné deux fois"
        );
        assert_eq!(
            usage_message(&["reset", "--yes", "--yes"]),
            "--yes donné deux fois"
        );
    }

    #[test]
    fn empty_or_missing_value_is_an_error() {
        assert_eq!(
            usage_message(&["check", "--grdf-pce="]),
            "--grdf-pce : valeur vide"
        );
        assert_eq!(
            usage_message(&["check", "--grdf-pce", ""]),
            "--grdf-pce : valeur vide"
        );
        assert_eq!(
            usage_message(&["check", "--config="]),
            "--config : valeur vide"
        );
        assert_eq!(
            usage_message(&["check", "--grdf-pce"]),
            "--grdf-pce : valeur manquante"
        );
        assert_eq!(
            usage_message(&["check", "--config", "--gazpar-log-level=debug"]),
            "--config : valeur manquante"
        );
    }

    #[test]
    fn password_and_token_are_refused_on_the_command_line() {
        for (list, instead) in [
            (
                &["check", "--grdf-password", PASSWORD][..],
                "--grdf-password-file",
            ),
            (
                &["check", "--grdf-password=mot-de-passe-secret"],
                "--grdf-password-file",
            ),
            (&["run", "--ha-token", TOKEN], "--ha-token-file"),
            (&["once", "--ha-token=jeton-secret"], "--ha-token-file"),
        ] {
            let m = usage_message(list);
            assert!(m.contains("refusé"), "{m}");
            assert!(m.contains(instead), "{m}");
            assert!(m.contains("ps, docker inspect"), "{m}");
            assert!(!m.contains(PASSWORD) && !m.contains(TOKEN), "{m}");
        }
        assert!(usage_message(&["check", "--grdf-password"]).starts_with("--grdf-password refusé"));
    }

    #[test]
    fn unknown_option_with_suggestion() {
        assert_eq!(
            usage_message(&["check", "--grdf-pcee", "1"]),
            "option inconnue --grdf-pcee (voulez-vous dire --grdf-pce ?)"
        );
        assert_eq!(
            usage_message(&["check", "--confg=/x"]),
            "option inconnue --confg (voulez-vous dire --config ?)"
        );
        assert_eq!(
            usage_message(&["check", "--log-level", "debug"]),
            "option inconnue --log-level (voulez-vous dire --gazpar-log-level ?)"
        );
        // Vers un paramètre refusé : la suggestion est sa variante -file.
        assert_eq!(
            usage_message(&["check", "--grdf-pasword=x"]),
            "option inconnue --grdf-pasword (voulez-vous dire --grdf-password-file ?)"
        );
        assert_eq!(
            usage_message(&["check", "--gazpar-timezone", "UTC"]),
            "option inconnue --gazpar-timezone"
        );
        // Développement : environnement seulement.
        assert!(
            usage_message(&["check", "--grdf-base-url=http://127.0.0.1"])
                .starts_with("option inconnue --grdf-base-url")
        );
        // Variante courte : inconnue.
        assert!(usage_message(&["reset", "-y"]).starts_with("argument inattendu"));
    }

    #[test]
    fn non_utf8_argument() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let bad = OsString::from_vec(vec![0x72, 0xff]);
            assert_eq!(
                parse_args([OsString::from("check"), bad]).unwrap_err(),
                Error::Usage("argument non UTF-8".into())
            );
        }
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
        assert_eq!(
            usage_message(&["check", "--grdf-pce", "1", "secret"]),
            "argument inattendu en position 4 (les options s'écrivent --nom valeur \
             ou --nom=valeur)"
        );
    }

    #[test]
    fn invocation_debug_shows_no_value() {
        let i = invocation(&[
            "check",
            "--grdf-username",
            EMAIL,
            "--grdf-pce=12345678901234",
        ]);
        let shown = format!("{i:?} {i:#?}");
        assert!(
            !shown.contains(EMAIL) && !shown.contains("12345678901234"),
            "{shown}"
        );
        assert!(shown.contains("GRDF_USERNAME"));
    }

    // --- Aide ---

    #[test]
    fn help_lists_every_option_of_the_table() {
        let help = usage();
        assert!(help.starts_with("Usage : ha-gazpar <commande> [options]\n"));
        for (param, target) in config::parameters() {
            match target {
                ParameterTarget::Accepted(_) => assert!(help.contains(&param), "{param}"),
                ParameterTarget::Refused { instead, .. } => {
                    assert!(
                        help.contains(&format!("jamais {param} en paramètre")),
                        "{param}"
                    );
                    assert!(help.contains(&instead), "{instead}");
                }
            }
        }
        for word in [
            "run",
            "once",
            "check",
            "reset",
            "--config",
            "--yes",
            "--help",
            "--version",
        ] {
            assert!(help.contains(word), "{word}");
        }
        assert!(help.contains("défaut 480"));
        for old in ["LOG_LEVEL=", "TIMEZONE", "CONSUMPTION"] {
            assert!(!help.contains(old), "{old}");
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

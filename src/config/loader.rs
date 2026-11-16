//! Chargement de la configuration : fusion des trois sources (paramètre,
//! environnement, fichier `NOM=valeur`), provenance de chaque valeur,
//! variantes `…_FILE`, avertissements, puis validation par
//! [`super::validate`] et construction de la [`Config`].
//!
//! Les entrées-sorties sont injectées : [`load`] reçoit l'environnement sous
//! forme de table et l'accès aux fichiers par le trait [`FileSystem`] ;
//! [`load_from_process`] les branche sur la bibliothèque standard.
//!
//! Dépendances : `config::options`, `config::validate`, `config_file`,
//! `secret`, `error`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::Path;

use super::options::{DEV_VARIABLES, OPTIONS, OptionSpec, WATCHED_PREFIXES, file_keys, parameter};
use super::validate::{
    check_ha_url, check_log_level, check_pce, check_statistic_prefix, check_username, local_url,
    parse_number,
};
use super::{Config, DEFAULT_GRDF_BASE_URL, DEFAULT_GRDF_LOGIN_URL, Setting};
use crate::config_file::{self, CONFIG_VARIABLE, Entry};
use crate::error::Error;
use crate::secret::Secret;

/// Ce que la ligne de commande apporte à la configuration.
#[derive(Default, Clone, PartialEq, Eq)]
pub struct CommandLine {
    /// `--config <chemin>`.
    pub config_path: Option<String>,
    /// Paramètres de configuration : (nom de variable, valeur non vide), au
    /// plus un par variable.
    pub params: Vec<(&'static str, String)>,
}

impl fmt::Debug for CommandLine {
    // Les valeurs ne sont pas montrées : `--grdf-username` porte l'e-mail.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.params.iter().map(|(n, _)| *n).collect();
        f.debug_struct("CommandLine")
            .field("config_path", &self.config_path)
            .field("params", &names)
            .finish()
    }
}

/// Accès aux fichiers (injecté pour les tests).
pub trait FileSystem {
    /// Contenu d'un fichier texte UTF-8.
    fn read_to_string(&self, path: &str) -> std::io::Result<String>;
    /// Droits Unix du fichier (`mode & 0o777`) ; `None` si le système n'en a
    /// pas.
    fn mode(&self, path: &str) -> std::io::Result<Option<u32>>;
}

/// Le vrai système de fichiers.
pub struct RealFileSystem;

impl FileSystem for RealFileSystem {
    fn read_to_string(&self, path: &str) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }

    #[cfg(unix)]
    fn mode(&self, path: &str) -> std::io::Result<Option<u32>> {
        use std::os::unix::fs::PermissionsExt;
        Ok(Some(std::fs::metadata(path)?.permissions().mode() & 0o777))
    }

    #[cfg(not(unix))]
    fn mode(&self, _path: &str) -> std::io::Result<Option<u32>> {
        Ok(None)
    }
}

/// D'où vient une valeur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Default,
    /// Fichier de configuration, avec son chemin.
    File(String),
    Environment,
    Parameter,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Default => f.write_str("défaut"),
            Source::File(path) => write!(f, "fichier {path}"),
            Source::Environment => f.write_str("environnement"),
            Source::Parameter => f.write_str("paramètre"),
        }
    }
}

/// Provenance complète d'une valeur : la source qui l'a donnée et, pour une
/// variante `_FILE`, la variante et le chemin du fichier lu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub source: Source,
    /// (variante `_FILE`, chemin du fichier lu).
    pub via: Option<(&'static str, String)>,
}

impl Origin {
    fn direct(source: Source) -> Origin {
        Origin { source, via: None }
    }
}

impl fmt::Display for Origin {
    /// Forme du manuel (§4.1) : `paramètre`, `environnement`,
    /// `fichier /etc/ha-gazpar.conf`, `défaut`, ou pour une variante `_FILE`
    /// `fichier /run/secrets/ha_token, via HA_TOKEN_FILE` (précisé quand la
    /// variante vient d'un paramètre ou du fichier de configuration).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.via {
            None => self.source.fmt(f),
            Some((variant, path)) => match &self.source {
                Source::Parameter => {
                    write!(f, "fichier {path}, via {}", parameter(variant))
                }
                Source::File(config) => {
                    write!(f, "fichier {path}, via {variant} du fichier {config}")
                }
                Source::Environment | Source::Default => {
                    write!(f, "fichier {path}, via {variant}")
                }
            },
        }
    }
}

/// Résultat du chargement : la configuration ou toutes ses erreurs, et dans
/// les deux cas les avertissements à journaliser.
#[derive(Debug)]
pub struct Loaded {
    pub warnings: Vec<String>,
    pub config: Result<Config, Error>,
}

/// Charge la configuration du processus : environnement réel et vrai
/// système de fichiers.
pub fn load_from_process(command_line: &CommandLine) -> Loaded {
    match environment_from(std::env::vars_os()) {
        Ok(env) => load(command_line, &env, &RealFileSystem),
        Err(problems) => Loaded {
            warnings: Vec::new(),
            config: Err(Error::Config(problems)),
        },
    }
}

/// Table des variables d'environnement en UTF-8. Une variable lue par le
/// programme dont la valeur n'est pas de l'UTF-8 est une erreur ; les autres
/// variables non UTF-8 sont ignorées.
pub fn environment_from(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<BTreeMap<String, String>, Vec<String>> {
    let mut env = BTreeMap::new();
    let mut problems = Vec::new();
    for (name, value) in vars {
        let Ok(name) = name.into_string() else {
            continue;
        };
        match value.into_string() {
            Ok(value) => {
                env.insert(name, value);
            }
            Err(_) if is_known_variable(&name) => {
                problems.push(format!("{name} : la valeur n'est pas de l'UTF-8 valide"));
            }
            Err(_) => {}
        }
    }
    if problems.is_empty() {
        Ok(env)
    } else {
        Err(problems)
    }
}

/// Variable d'environnement lue par le programme.
fn is_known_variable(name: &str) -> bool {
    name == CONFIG_VARIABLE || DEV_VARIABLES.contains(&name) || file_keys().contains(&name)
}

/// Charge et valide la configuration à partir de la ligne de commande, de
/// l'environnement `env` et des fichiers lus par `fs`.
pub fn load(
    command_line: &CommandLine,
    env: &BTreeMap<String, String>,
    fs: &dyn FileSystem,
) -> Loaded {
    let mut loader = Loader {
        params: &command_line.params,
        env,
        fs,
        file: Vec::new(),
        file_source: None,
        file_unreadable: false,
        problems: Vec::new(),
        warnings: Vec::new(),
    };
    loader.warn_unknown_variables();
    let config_file = loader.read_config_file(command_line);
    let config = loader.build(config_file);
    let warnings = loader.warnings;
    match config {
        Some(config) if loader.problems.is_empty() => Loaded {
            warnings,
            config: Ok(config),
        },
        _ if loader.problems.is_empty() => Loaded {
            // Chaque valeur absente a déjà ajouté un problème ; ce cas ne
            // devrait pas se produire, mais on ne panique pas.
            warnings,
            config: Err(Error::Config(vec![
                "configuration incomplète (erreur interne de validation)".to_owned(),
            ])),
        },
        _ => Loaded {
            warnings,
            config: Err(Error::Config(loader.problems)),
        },
    }
}

/// Source lue, de la plus forte à la plus faible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    Parameter,
    Environment,
    File,
}

const LAYERS: [Layer; 3] = [Layer::Parameter, Layer::Environment, Layer::File];

/// Forme trouvée dans la source retenue.
enum Found {
    Value(String),
    Path(&'static str, String),
}

/// Valeur brute retenue pour une option, avant validation.
struct Picked {
    value: String,
    origin: Origin,
}

/// Accumule valeurs, problèmes et avertissements pendant le chargement.
struct Loader<'a> {
    params: &'a [(&'static str, String)],
    env: &'a BTreeMap<String, String>,
    fs: &'a dyn FileSystem,
    /// Options lues dans le fichier de configuration.
    file: Vec<Entry>,
    /// Source « fichier » (avec son chemin), si un fichier est désigné.
    file_source: Option<Source>,
    /// Le fichier désigné n'a pas pu être lu : les options manquantes n'en
    /// sont que la conséquence et ne sont pas signalées.
    file_unreadable: bool,
    problems: Vec<String>,
    warnings: Vec<String>,
}

impl Loader<'_> {
    fn problem(&mut self, message: impl Into<String>) {
        self.problems.push(message.into());
    }

    /// Variable d'environnement non vide.
    fn env_value(&self, name: &str) -> Option<&str> {
        self.env
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// Valeur non vide de `name` dans une source.
    fn get(&self, layer: Layer, name: &str) -> Option<&str> {
        match layer {
            Layer::Parameter => self
                .params
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.as_str())
                .filter(|v| !v.is_empty()),
            Layer::Environment => self.env_value(name),
            Layer::File => self
                .file
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.value.as_str()),
        }
    }

    fn source(&self, layer: Layer) -> Source {
        match layer {
            Layer::Parameter => Source::Parameter,
            Layer::Environment => Source::Environment,
            // Une valeur ne vient du fichier que s'il a été lu.
            Layer::File => self.file_source.clone().unwrap_or(Source::Default),
        }
    }

    /// Avertit pour chaque variable `GAZPAR_…` ou `GRDF_…` inconnue.
    fn warn_unknown_variables(&mut self) {
        let mut known: Vec<&str> = file_keys();
        known.push(CONFIG_VARIABLE);
        known.extend(DEV_VARIABLES);
        for name in self.env.keys() {
            let watched = WATCHED_PREFIXES.iter().any(|p| name.starts_with(p));
            if !watched || known.contains(&name.as_str()) {
                continue;
            }
            let hint = config_file::closest(name, known.iter().copied())
                .map(|s| format!(" (voulez-vous dire {s} ?)"))
                .unwrap_or_default();
            self.warnings.push(format!(
                "variable d'environnement {name} inconnue, ignorée{hint}"
            ));
        }
    }

    /// Lit et analyse le fichier désigné par `--config` ou `GAZPAR_CONFIG`.
    fn read_config_file(&mut self, command_line: &CommandLine) -> Option<(String, Source)> {
        let (path, designated_by) = match &command_line.config_path {
            Some(path) => (path.clone(), Source::Parameter),
            None => (
                self.env_value(CONFIG_VARIABLE)?.to_owned(),
                Source::Environment,
            ),
        };
        match self.fs.read_to_string(&path) {
            Ok(content) => {
                let parsed = config_file::parse(&content, &file_keys());
                for e in &parsed.errors {
                    self.problems
                        .push(format!("fichier de configuration {path}, {e}"));
                }
                self.file = parsed.entries;
                self.file_source = Some(Source::File(path.clone()));
                if let Ok(Some(mode)) = self.fs.mode(&path)
                    && let Some(who) = readable_by_others(mode)
                {
                    self.warnings.push(format!(
                        "le fichier de configuration {path} est lisible par {who} \
                         alors qu'il contient des secrets (chmod 600 conseillé)"
                    ));
                }
            }
            Err(e) => {
                self.file_unreadable = true;
                self.problem(format!(
                    "fichier de configuration {path} : {}",
                    describe_io(&e)
                ));
            }
        }
        Some((path, designated_by))
    }

    /// Valeur retenue pour une option : source la plus forte qui donne l'une
    /// des deux formes, sinon défaut. `None` (avec un problème noté, sauf
    /// conséquence d'un fichier illisible) si elle manque ou est
    /// contradictoire.
    fn pick(&mut self, spec: &OptionSpec) -> Option<Picked> {
        let mut chosen: Option<(Found, Layer)> = None;
        let mut conflict = false;
        for layer in LAYERS {
            let plain = self.get(layer, spec.name);
            let path = spec
                .file_variant
                .and_then(|v| self.get(layer, v).map(|p| (v, p)));
            let found = match (plain, path) {
                (Some(_), Some((variant, _))) => {
                    let message = both_forms(spec.name, variant, &self.source(layer));
                    self.problem(message);
                    conflict |= chosen.is_none();
                    continue;
                }
                (Some(v), None) => Found::Value(v.to_owned()),
                (None, Some((variant, p))) => Found::Path(variant, p.to_owned()),
                (None, None) => continue,
            };
            if chosen.is_none() && !conflict {
                chosen = Some((found, layer));
            }
        }
        match chosen {
            Some((Found::Value(value), layer)) => Some(Picked {
                value,
                origin: Origin::direct(self.source(layer)),
            }),
            Some((Found::Path(variant, path), layer)) => self.read_variant(variant, path, layer),
            None if conflict => None,
            None => match spec.default {
                Some(default) => Some(Picked {
                    value: default.to_owned(),
                    origin: Origin::direct(Source::Default),
                }),
                None => {
                    if !self.file_unreadable {
                        let message = match spec.file_variant {
                            Some(variant) => format!("{} manquant (ou {variant})", spec.name),
                            None if spec.name == "HA_URL" => {
                                "HA_URL manquant (ex. ws://192.168.1.10:8123)".to_owned()
                            }
                            None => format!("{} manquant", spec.name),
                        };
                        self.problem(message);
                    }
                    None
                }
            },
        }
    }

    /// Lit la valeur désignée par une variante `_FILE`.
    fn read_variant(
        &mut self,
        variant: &'static str,
        path: String,
        layer: Layer,
    ) -> Option<Picked> {
        let source = self.source(layer);
        // Le chemin n'est pas recopié : une valeur relative est peut-être un
        // secret mis à la mauvaise place.
        if !Path::new(&path).is_absolute() {
            self.problem(format!("{variant} ({source}) : chemin absolu attendu"));
            return None;
        }
        match self.fs.read_to_string(&path) {
            Ok(content) => {
                let value = content.trim_end();
                if value.is_empty() {
                    self.problem(format!("{variant} ({source}) : le fichier {path} est vide"));
                    return None;
                }
                Some(Picked {
                    value: value.to_owned(),
                    origin: Origin {
                        source,
                        via: Some((variant, path)),
                    },
                })
            }
            Err(e) => {
                self.problem(format!(
                    "{variant} ({source}) : lecture de {path} impossible ({})",
                    describe_io(&e)
                ));
                None
            }
        }
    }

    /// Lit toutes les options, les valide et construit la configuration.
    fn build(&mut self, config_file: Option<(String, Source)>) -> Option<Config> {
        let mut settings = Vec::new();
        let mut picked: Vec<Option<Picked>> = OPTIONS.iter().map(|s| self.pick(s)).collect();
        // Retire la valeur de l'option `name` (présente dans la table).
        let mut take = |name: &str| {
            OPTIONS
                .iter()
                .position(|s| s.name == name)
                .and_then(|i| picked.get_mut(i))
                .and_then(Option::take)
        };
        let username = take("GRDF_USERNAME");
        let password = take("GRDF_PASSWORD");
        let pce = take("GRDF_PCE");
        let ha_url = take("HA_URL");
        let ha_token = take("HA_TOKEN");
        let scan = take("GAZPAR_SCAN_INTERVAL");
        let history = take("GAZPAR_HISTORY_DAYS");
        let overlap = take("GAZPAR_OVERLAP_DAYS");
        let prefix = take("GAZPAR_STATISTIC_PREFIX");
        let level = take("GAZPAR_LOG_LEVEL");

        let username = self.check(username, "GRDF_USERNAME", check_username);
        let password = self.check(password, "GRDF_PASSWORD", |v| Ok(v.to_owned()));
        let pce = self.check(pce, "GRDF_PCE", check_pce);
        let ha_url = self.check(ha_url, "HA_URL", check_ha_url);
        let ha_token = self.check(ha_token, "HA_TOKEN", |v| Ok(v.to_owned()));
        let scan = self.check(scan, "GAZPAR_SCAN_INTERVAL", |v| {
            parse_number(v, 60, u32::MAX, "minutes")
        });
        let history = self.check(history, "GAZPAR_HISTORY_DAYS", |v| {
            parse_number(v, 1, 3650, "jours")
        });
        let overlap = self.check(overlap, "GAZPAR_OVERLAP_DAYS", |v| {
            parse_number(v, 1, 60, "jours")
        });
        let level = self.check(level, "GAZPAR_LOG_LEVEL", check_log_level);
        // Le préfixe est validé même si le PCE est faux, avec un PCE fictif.
        let pce_for_ids = pce.as_ref().map_or("00000000000000", |(p, _)| p.as_str());
        let ids = self.check(prefix, "GAZPAR_STATISTIC_PREFIX", |v| {
            check_statistic_prefix(v, pce_for_ids)
        });
        let grdf_base_url = self.dev_url("GRDF_BASE_URL", DEFAULT_GRDF_BASE_URL, &mut settings);
        let grdf_login_url = self.dev_url("GRDF_LOGIN_URL", DEFAULT_GRDF_LOGIN_URL, &mut settings);

        let (username, u_origin) = username?;
        let (password, p_origin) = password?;
        let (pce, pce_origin) = pce?;
        let (ha_url, url_origin) = ha_url?;
        let (ha_token, t_origin) = ha_token?;
        let (scan, scan_origin) = scan?;
        let (history, history_origin) = history?;
        let (overlap, overlap_origin) = overlap?;
        let ((prefix, energy, volume), prefix_origin) = ids?;
        let (level, level_origin) = level?;

        let mut rows = vec![
            row("GRDF_USERNAME", &username, u_origin),
            row("GRDF_PASSWORD", &password, p_origin),
            row("GRDF_PCE", &pce, pce_origin),
            row("HA_URL", &ha_url, url_origin),
            row("HA_TOKEN", &ha_token, t_origin),
            row("GAZPAR_SCAN_INTERVAL", &scan.to_string(), scan_origin),
            row("GAZPAR_HISTORY_DAYS", &history.to_string(), history_origin),
            row("GAZPAR_OVERLAP_DAYS", &overlap.to_string(), overlap_origin),
            row("GAZPAR_STATISTIC_PREFIX", &prefix, prefix_origin),
            row("GAZPAR_LOG_LEVEL", &level.to_string(), level_origin),
        ];
        rows.append(&mut settings);

        Some(Config {
            grdf_username: Secret::new(username),
            grdf_password: Secret::new(password),
            pce,
            ha_url,
            ha_token: Secret::new(ha_token),
            scan_interval_minutes: scan,
            history_days: history,
            overlap_days: overlap,
            log_level: level,
            energy_statistic: energy,
            volume_statistic: volume,
            grdf_base_url,
            grdf_login_url,
            config_file,
            settings: rows,
        })
    }

    /// Valide une valeur retenue ; en cas de refus, note le problème avec le
    /// nom et la provenance (jamais la valeur).
    fn check<T>(
        &mut self,
        picked: Option<Picked>,
        name: &str,
        validate: impl FnOnce(&str) -> Result<T, String>,
    ) -> Option<(T, Origin)> {
        let picked = picked?;
        match validate(&picked.value) {
            Ok(v) => Some((v, picked.origin)),
            Err(why) => {
                self.problem(format!("{name} ({}) : {why}", picked.origin));
                None
            }
        }
    }

    /// Adresse GRDF surchargeable pour le développement, dans l'environnement
    /// seulement, vers `localhost` ou `127.0.0.1` uniquement.
    fn dev_url(&mut self, name: &'static str, default: &str, rows: &mut Vec<Setting>) -> String {
        let Some(text) = self.env_value(name) else {
            return default.to_owned();
        };
        if let Some(url) = local_url(text) {
            rows.push(row(name, &url, Origin::direct(Source::Environment)));
            url
        } else {
            self.problem(format!(
                "{name} : réservé au développement, seuls http(s)://localhost et \
                 http(s)://127.0.0.1 sont acceptés"
            ));
            default.to_owned()
        }
    }
}

/// Ligne du tableau de `check` : la valeur est remplacée par `***` pour une
/// option marquée secrète dans [`OPTIONS`].
fn row(name: &'static str, value: &str, origin: Origin) -> Setting {
    let secret = OPTIONS.iter().any(|o| o.name == name && o.secret);
    Setting {
        name,
        shown: if secret { Secret::MASK } else { value }.to_owned(),
        origin,
    }
}

/// Message des deux formes d'une option dans une même source.
fn both_forms(name: &str, variant: &str, source: &Source) -> String {
    match source {
        Source::Parameter => format!(
            "{} et {} donnés tous les deux : n'en garder qu'un",
            parameter(name),
            parameter(variant)
        ),
        Source::Environment => format!(
            "{name} et {variant} définis tous les deux dans l'environnement : n'en garder qu'un"
        ),
        Source::File(path) => format!(
            "{name} et {variant} définis tous les deux dans le fichier {path} : n'en garder qu'un"
        ),
        Source::Default => {
            format!("{name} et {variant} définis tous les deux : n'en garder qu'un")
        }
    }
}

/// Qui, en plus du propriétaire, peut lire un fichier de ces droits.
fn readable_by_others(mode: u32) -> Option<&'static str> {
    match (mode & 0o040 != 0, mode & 0o004 != 0) {
        (true, true) => Some("le groupe et les autres utilisateurs"),
        (true, false) => Some("le groupe"),
        (false, true) => Some("les autres utilisateurs"),
        (false, false) => None,
    }
}

/// Motif d'une erreur d'entrée-sortie, en français pour les cas courants.
fn describe_io(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "introuvable".to_owned(),
        std::io::ErrorKind::PermissionDenied => "accès refusé".to_owned(),
        std::io::ErrorKind::InvalidData => "contenu qui n'est pas de l'UTF-8 valide".to_owned(),
        std::io::ErrorKind::IsADirectory => "c'est un dossier".to_owned(),
        _ => e.to_string(),
    }
}

/// Outils des tests de `config` : faux système de fichiers, environnement
/// minimal valide, lecture du résultat.
#[cfg(test)]
pub(super) mod testing {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    pub(crate) const PASSWORD: &str = "P@ssw0rd-tres-secret";
    pub(crate) const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.jeton-tres-secret";
    pub(crate) const EMAIL: &str = "jean.dupont@example.org";
    pub(crate) const CONF: &str = "/etc/ha-gazpar/ha-gazpar.conf";

    /// Faux système de fichiers : contenu et droits par chemin, lectures
    /// enregistrées.
    #[derive(Default)]
    pub(crate) struct Fs {
        files: HashMap<String, (String, u32)>,
        pub(crate) reads: RefCell<Vec<String>>,
    }

    impl Fs {
        pub(crate) fn with(mut self, path: &str, content: &str) -> Fs {
            self.files
                .insert(path.to_owned(), (content.to_owned(), 0o600));
            self
        }

        pub(crate) fn with_mode(mut self, path: &str, content: &str, mode: u32) -> Fs {
            self.files
                .insert(path.to_owned(), (content.to_owned(), mode));
            self
        }

        pub(crate) fn was_read(&self, path: &str) -> bool {
            self.reads.borrow().iter().any(|p| p == path)
        }
    }

    impl FileSystem for Fs {
        fn read_to_string(&self, path: &str) -> std::io::Result<String> {
            self.reads.borrow_mut().push(path.to_owned());
            self.files
                .get(path)
                .map(|(c, _)| c.clone())
                .ok_or_else(|| std::io::ErrorKind::NotFound.into())
        }

        fn mode(&self, path: &str) -> std::io::Result<Option<u32>> {
            self.files
                .get(path)
                .map(|(_, m)| Some(*m))
                .ok_or_else(|| std::io::ErrorKind::NotFound.into())
        }
    }

    pub(crate) fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    pub(crate) fn minimal() -> Vec<(&'static str, &'static str)> {
        vec![
            ("GRDF_USERNAME", EMAIL),
            ("GRDF_PASSWORD", PASSWORD),
            ("GRDF_PCE", "12345678901234"),
            ("HA_URL", "ws://127.0.0.1:8123/api/websocket"),
            ("HA_TOKEN", TOKEN),
        ]
    }

    /// `minimal()` modifié : une valeur vide retire la variable.
    pub(crate) fn minimal_with(
        changes: &[(&'static str, &'static str)],
    ) -> BTreeMap<String, String> {
        let mut e = env(&minimal());
        for (k, v) in changes {
            if v.is_empty() {
                e.remove(*k);
            } else {
                e.insert((*k).to_owned(), (*v).to_owned());
            }
        }
        e
    }

    pub(crate) fn line(config: Option<&str>, params: &[(&'static str, &str)]) -> CommandLine {
        CommandLine {
            config_path: config.map(str::to_owned),
            params: params.iter().map(|(k, v)| (*k, (*v).to_owned())).collect(),
        }
    }

    pub(crate) fn from_env(e: &BTreeMap<String, String>) -> Loaded {
        load(&CommandLine::default(), e, &Fs::default())
    }

    pub(crate) fn accepted(loaded: Loaded) -> Config {
        match loaded.config {
            Ok(c) => c,
            Err(e) => panic!("configuration refusée : {e}"),
        }
    }

    pub(crate) fn refused(loaded: Loaded) -> Vec<String> {
        match loaded.config {
            Err(Error::Config(p)) => p,
            Err(e) => panic!("erreur inattendue : {e}"),
            Ok(c) => panic!("configuration acceptée à tort : {c:?}"),
        }
    }

    pub(crate) fn single(loaded: Loaded) -> String {
        let p = refused(loaded);
        assert_eq!(p.len(), 1, "{p:#?}");
        p[0].clone()
    }

    pub(crate) fn setting<'c>(c: &'c Config, name: &str) -> &'c Setting {
        c.settings
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{name} absent du tableau"))
    }

    pub(crate) fn origin(c: &Config, name: &str) -> String {
        setting(c, name).origin.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::domain::Measure;
    use crate::log::Level;

    // --- Cas nominal et valeurs par défaut ---

    #[test]
    fn minimal_config_uses_defaults() {
        let c = accepted(from_env(&env(&minimal())));
        assert_eq!(c.grdf_username.expose(), EMAIL);
        assert_eq!(c.grdf_password.expose(), PASSWORD);
        assert_eq!(c.pce, "12345678901234");
        assert_eq!(c.ha_url, "ws://127.0.0.1:8123/api/websocket");
        assert_eq!(c.ha_token.expose(), TOKEN);
        assert_eq!(c.scan_interval_minutes, 480);
        assert_eq!(c.history_days, 1095);
        assert_eq!(c.overlap_days, 14);
        assert_eq!(c.log_level, Level::Info);
        assert_eq!(c.energy_statistic.as_str(), "gazpar:12345678901234_energy");
        assert_eq!(c.volume_statistic.as_str(), "gazpar:12345678901234_volume");
        assert_eq!(c.statistic_id(Measure::Volume), &c.volume_statistic);
        assert_eq!(c.grdf_base_url, DEFAULT_GRDF_BASE_URL);
        assert_eq!(c.grdf_login_url, DEFAULT_GRDF_LOGIN_URL);
        assert_eq!(c.config_file, None);
        assert_eq!(origin(&c, "GRDF_PCE"), "environnement");
        assert_eq!(origin(&c, "GAZPAR_LOG_LEVEL"), "défaut");
        assert_eq!(setting(&c, "GAZPAR_SCAN_INTERVAL").shown, "480");
        assert_eq!(c.settings.len(), 10);
    }

    #[test]
    fn every_option_set() {
        let e = minimal_with(&[
            ("GAZPAR_SCAN_INTERVAL", "60"),
            ("GAZPAR_HISTORY_DAYS", "3650"),
            ("GAZPAR_OVERLAP_DAYS", "60"),
            ("GAZPAR_STATISTIC_PREFIX", "sensor.gazpar2haws"),
            ("GAZPAR_LOG_LEVEL", "debug"),
            ("GRDF_BASE_URL", "http://127.0.0.1:8080/"),
            ("GRDF_LOGIN_URL", "http://localhost:8081"),
        ]);
        let c = accepted(from_env(&e));
        assert_eq!(c.scan_interval_minutes, 60);
        assert_eq!(c.history_days, 3650);
        assert_eq!(c.overlap_days, 60);
        assert_eq!(c.log_level, Level::Debug);
        assert_eq!(c.energy_statistic.as_str(), "sensor.gazpar2haws_energy");
        assert_eq!(
            setting(&c, "GAZPAR_STATISTIC_PREFIX").shown,
            "sensor.gazpar2haws"
        );
        assert_eq!(c.grdf_base_url, "http://127.0.0.1:8080");
        assert_eq!(c.grdf_login_url, "http://localhost:8081");
        assert_eq!(setting(&c, "GRDF_BASE_URL").shown, "http://127.0.0.1:8080");
        assert_eq!(c.settings.len(), 12);
    }

    #[test]
    fn empty_environment_value_counts_as_absent() {
        let mut e = minimal_with(&[]);
        e.insert("GAZPAR_SCAN_INTERVAL".into(), String::new());
        e.insert("GAZPAR_LOG_LEVEL".into(), String::new());
        let c = accepted(from_env(&e));
        assert_eq!(c.scan_interval_minutes, 480);
        assert_eq!(origin(&c, "GAZPAR_SCAN_INTERVAL"), "défaut");
        e.insert("HA_TOKEN".into(), String::new());
        assert_eq!(single(from_env(&e)), "HA_TOKEN manquant (ou HA_TOKEN_FILE)");
    }

    // --- Priorité des sources ---

    #[test]
    fn manual_example_parameter_beats_file() {
        // MANUEL.md §4.1 : fichier GAZPAR_LOG_LEVEL=info, paramètre debug.
        let fs = Fs::default().with(CONF, "GAZPAR_LOG_LEVEL=info\n");
        let c = accepted(load(
            &line(Some(CONF), &[("GAZPAR_LOG_LEVEL", "debug")]),
            &env(&minimal()),
            &fs,
        ));
        assert_eq!(c.log_level, Level::Debug);
        assert_eq!(origin(&c, "GAZPAR_LOG_LEVEL"), "paramètre");
    }

    #[test]
    fn priority_parameter_environment_file_default() {
        let fs = Fs::default().with(
            CONF,
            "GAZPAR_SCAN_INTERVAL=61\nGAZPAR_HISTORY_DAYS=100\nGAZPAR_OVERLAP_DAYS=20\n",
        );
        let mut e = env(&minimal());
        e.insert("GAZPAR_SCAN_INTERVAL".into(), "62".into());
        e.insert("GAZPAR_HISTORY_DAYS".into(), "200".into());
        let c = accepted(load(
            &line(Some(CONF), &[("GAZPAR_SCAN_INTERVAL", "63")]),
            &e,
            &fs,
        ));
        assert_eq!(c.scan_interval_minutes, 63);
        assert_eq!(origin(&c, "GAZPAR_SCAN_INTERVAL"), "paramètre");
        assert_eq!(c.history_days, 200);
        assert_eq!(origin(&c, "GAZPAR_HISTORY_DAYS"), "environnement");
        assert_eq!(c.overlap_days, 20);
        assert_eq!(origin(&c, "GAZPAR_OVERLAP_DAYS"), format!("fichier {CONF}"));
        assert_eq!(c.log_level, Level::Info);
        assert_eq!(origin(&c, "GAZPAR_LOG_LEVEL"), "défaut");
        assert_eq!(c.config_file, Some((CONF.to_owned(), Source::Parameter)));
    }

    #[test]
    fn everything_from_the_file() {
        let fs = Fs::default().with(
            CONF,
            &format!(
                "\u{feff}# Fichier complet\r\n\
                 GRDF_USERNAME={EMAIL}\r\n\
                 GRDF_PASSWORD=\"{PASSWORD} # avec dièse\"\r\n\
                 GRDF_PCE=01234567890123\r\n\
                 HA_URL=ws://192.168.1.10:8123\r\n\
                 HA_TOKEN={TOKEN}\r\n"
            ),
        );
        let mut e = BTreeMap::new();
        e.insert("GAZPAR_CONFIG".to_owned(), CONF.to_owned());
        let loaded = load(&CommandLine::default(), &e, &fs);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let c = accepted(loaded);
        assert_eq!(c.grdf_password.expose(), format!("{PASSWORD} # avec dièse"));
        assert_eq!(c.pce, "01234567890123");
        assert_eq!(c.ha_url, "ws://192.168.1.10:8123/api/websocket");
        assert_eq!(c.config_file, Some((CONF.to_owned(), Source::Environment)));
        assert_eq!(origin(&c, "HA_TOKEN"), format!("fichier {CONF}"));
    }

    #[test]
    fn empty_file_value_falls_back_to_default() {
        let fs = Fs::default().with(CONF, "GAZPAR_OVERLAP_DAYS=\n");
        let c = accepted(load(&line(Some(CONF), &[]), &env(&minimal()), &fs));
        assert_eq!(c.overlap_days, 14);
        assert_eq!(origin(&c, "GAZPAR_OVERLAP_DAYS"), "défaut");
    }

    #[test]
    fn config_parameter_beats_config_variable() {
        let fs = Fs::default()
            .with("/a.conf", "GAZPAR_OVERLAP_DAYS=1")
            .with("/b.conf", "GAZPAR_OVERLAP_DAYS=2");
        let mut e = env(&minimal());
        e.insert("GAZPAR_CONFIG".into(), "/b.conf".into());
        let c = accepted(load(&line(Some("/a.conf"), &[]), &e, &fs));
        assert_eq!(c.overlap_days, 1);
        assert!(!fs.was_read("/b.conf"));
        // Sans --config, la variable désigne le fichier ; vide, elle est absente.
        let c = accepted(load(&CommandLine::default(), &e, &fs));
        assert_eq!(c.overlap_days, 2);
        e.insert("GAZPAR_CONFIG".into(), String::new());
        let fs = Fs::default();
        let c = accepted(load(&CommandLine::default(), &e, &fs));
        assert_eq!(c.config_file, None);
        assert!(fs.reads.borrow().is_empty());
    }

    #[test]
    fn missing_config_file_is_an_error_without_the_consequences() {
        let loaded = load(
            &line(Some("/absent.conf"), &[]),
            &BTreeMap::new(),
            &Fs::default(),
        );
        assert_eq!(
            refused(loaded),
            vec!["fichier de configuration /absent.conf : introuvable".to_owned()]
        );
    }

    #[test]
    fn file_syntax_errors_are_listed_with_path_and_line() {
        let fs = Fs::default().with(CONF, "GRDF_PCEE=1\nexport HA_URL=ws://x\n");
        let p = refused(load(&line(Some(CONF), &[]), &env(&minimal()), &fs));
        assert_eq!(
            p,
            vec![
                format!(
                    "fichier de configuration {CONF}, ligne 1 : option GRDF_PCEE inconnue \
                     (voulez-vous dire GRDF_PCE ?)"
                ),
                format!(
                    "fichier de configuration {CONF}, ligne 2 : « export » n'est pas accepté : \
                     écrire NOM=valeur, sans export"
                ),
            ]
        );
    }

    #[test]
    fn errors_of_every_source_listed_together() {
        let fs = Fs::default().with(CONF, "GRDF_PCEE=1\nGAZPAR_OVERLAP_DAYS=99\n");
        let p = refused(load(
            &line(Some(CONF), &[("GAZPAR_HISTORY_DAYS", "0")]),
            &minimal_with(&[("GAZPAR_SCAN_INTERVAL", "10"), ("HA_TOKEN", "")]),
            &fs,
        ));
        assert_eq!(p.len(), 5, "{p:#?}");
    }

    // --- Variantes _FILE ---

    #[test]
    fn file_variants_are_read_and_trimmed() {
        let e = minimal_with(&[
            ("GRDF_USERNAME", ""),
            ("GRDF_PASSWORD", ""),
            ("GRDF_PCE", ""),
            ("HA_TOKEN", ""),
            ("GRDF_USERNAME_FILE", "/run/secrets/user"),
            ("GRDF_PASSWORD_FILE", "/run/secrets/pass"),
            ("GRDF_PCE_FILE", "/run/secrets/pce"),
            ("HA_TOKEN_FILE", "/run/secrets/ha_token"),
        ]);
        let fs = Fs::default()
            .with("/run/secrets/user", "a@b.fr\n")
            .with("/run/secrets/pass", " mot de passe \r\n")
            .with("/run/secrets/pce", "12345678901234\n\n")
            .with("/run/secrets/ha_token", "abc\t");
        let c = accepted(load(&CommandLine::default(), &e, &fs));
        assert_eq!(c.grdf_username.expose(), "a@b.fr");
        // Seul l'espace final est retiré.
        assert_eq!(c.grdf_password.expose(), " mot de passe");
        assert_eq!(c.pce, "12345678901234");
        assert_eq!(c.ha_token.expose(), "abc");
        assert_eq!(
            origin(&c, "HA_TOKEN"),
            "fichier /run/secrets/ha_token, via HA_TOKEN_FILE"
        );
    }

    #[test]
    fn manual_example_variable_file_variant_beats_file_value() {
        // MANUEL.md §4.3 : GRDF_PASSWORD_FILE en variable, GRDF_PASSWORD
        // dans le fichier → la variable gagne, le fichier désigné est lu.
        let fs = Fs::default()
            .with(CONF, "GRDF_PASSWORD=ancien-mot-de-passe\n")
            .with("/run/secrets/pass", "nouveau\n");
        let e = minimal_with(&[
            ("GRDF_PASSWORD", ""),
            ("GRDF_PASSWORD_FILE", "/run/secrets/pass"),
        ]);
        let c = accepted(load(&line(Some(CONF), &[]), &e, &fs));
        assert_eq!(c.grdf_password.expose(), "nouveau");
        assert_eq!(
            origin(&c, "GRDF_PASSWORD"),
            "fichier /run/secrets/pass, via GRDF_PASSWORD_FILE"
        );
    }

    #[test]
    fn overridden_file_variant_is_not_read() {
        // Sens inverse : la variable donne la valeur, la variante _FILE du
        // fichier est écartée sans être lue (son chemin n'existe pas).
        let fs = Fs::default().with(CONF, "GRDF_PASSWORD_FILE=/absent/pass\n");
        let c = accepted(load(&line(Some(CONF), &[]), &env(&minimal()), &fs));
        assert_eq!(c.grdf_password.expose(), PASSWORD);
        assert_eq!(origin(&c, "GRDF_PASSWORD"), "environnement");
        assert!(!fs.was_read("/absent/pass"));
    }

    #[test]
    fn file_variant_origin_names_its_source() {
        let fs = Fs::default()
            .with(CONF, "HA_TOKEN_FILE=/run/secrets/t\n")
            .with("/run/secrets/t", "jeton")
            .with("/run/secrets/u", "u@example.org");
        let e = minimal_with(&[("HA_TOKEN", ""), ("GRDF_USERNAME", "")]);
        let c = accepted(load(
            &line(Some(CONF), &[("GRDF_USERNAME_FILE", "/run/secrets/u")]),
            &e,
            &fs,
        ));
        assert_eq!(
            origin(&c, "HA_TOKEN"),
            format!("fichier /run/secrets/t, via HA_TOKEN_FILE du fichier {CONF}")
        );
        assert_eq!(
            origin(&c, "GRDF_USERNAME"),
            "fichier /run/secrets/u, via --grdf-username-file"
        );
    }

    #[test]
    fn plain_parameter_beats_file_variant_in_environment() {
        let e = minimal_with(&[("GRDF_PCE", ""), ("GRDF_PCE_FILE", "/absent")]);
        let fs = Fs::default();
        let c = accepted(load(
            &line(None, &[("GRDF_PCE", "98765432109876")]),
            &e,
            &fs,
        ));
        assert_eq!(c.pce, "98765432109876");
        assert!(!fs.was_read("/absent"));
    }

    #[test]
    fn both_forms_in_one_source_is_an_error() {
        let e = minimal_with(&[("GRDF_PASSWORD_FILE", "/run/secrets/pass")]);
        assert_eq!(
            single(from_env(&e)),
            "GRDF_PASSWORD et GRDF_PASSWORD_FILE définis tous les deux dans l'environnement : \
             n'en garder qu'un"
        );
        let fs = Fs::default().with(CONF, "HA_TOKEN=a\nHA_TOKEN_FILE=/t\n");
        let e = minimal_with(&[("HA_TOKEN", "")]);
        assert_eq!(
            single(load(&line(Some(CONF), &[]), &e, &fs)),
            format!(
                "HA_TOKEN et HA_TOKEN_FILE définis tous les deux dans le fichier {CONF} : \
                 n'en garder qu'un"
            )
        );
        let p = line(None, &[("GRDF_PCE", "1"), ("GRDF_PCE_FILE", "/p")]);
        assert_eq!(
            single(load(&p, &env(&minimal()), &Fs::default())),
            "--grdf-pce et --grdf-pce-file donnés tous les deux : n'en garder qu'un"
        );
    }

    #[test]
    fn both_forms_in_an_overridden_source_is_still_an_error() {
        let fs = Fs::default().with(CONF, "GRDF_PCE=1\nGRDF_PCE_FILE=/p\n");
        let p = refused(load(&line(Some(CONF), &[]), &env(&minimal()), &fs));
        assert_eq!(p.len(), 1, "{p:#?}");
        assert!(p[0].starts_with("GRDF_PCE et GRDF_PCE_FILE"), "{}", p[0]);
    }

    #[test]
    fn file_variant_path_must_be_absolute_and_is_not_shown() {
        let e = minimal_with(&[("GRDF_PASSWORD", ""), ("GRDF_PASSWORD_FILE", PASSWORD)]);
        let fs = Fs::default();
        let p = single(load(&CommandLine::default(), &e, &fs));
        assert_eq!(
            p,
            "GRDF_PASSWORD_FILE (environnement) : chemin absolu attendu"
        );
        assert!(fs.reads.borrow().is_empty());
    }

    #[test]
    fn unreadable_or_empty_variant_file() {
        let e = minimal_with(&[("HA_TOKEN", ""), ("HA_TOKEN_FILE", "/absent")]);
        assert_eq!(
            single(from_env(&e)),
            "HA_TOKEN_FILE (environnement) : lecture de /absent impossible (introuvable)"
        );
        let e = minimal_with(&[("HA_TOKEN", ""), ("HA_TOKEN_FILE", "/vide")]);
        let fs = Fs::default().with("/vide", " \n");
        assert_eq!(
            single(load(&CommandLine::default(), &e, &fs)),
            "HA_TOKEN_FILE (environnement) : le fichier /vide est vide"
        );
    }

    // --- Variables manquantes : toutes listées d'un coup ---

    #[test]
    fn empty_environment_lists_every_missing_option() {
        let p = refused(from_env(&BTreeMap::new()));
        assert_eq!(
            p,
            [
                "GRDF_USERNAME manquant (ou GRDF_USERNAME_FILE)",
                "GRDF_PASSWORD manquant (ou GRDF_PASSWORD_FILE)",
                "GRDF_PCE manquant (ou GRDF_PCE_FILE)",
                "HA_URL manquant (ex. ws://192.168.1.10:8123)",
                "HA_TOKEN manquant (ou HA_TOKEN_FILE)",
            ]
        );
    }

    // --- Avertissements ---

    #[test]
    fn unknown_watched_variables_are_warned_about() {
        let mut e = env(&minimal());
        for (k, v) in [
            ("GRDF_PCEE", "1"),
            ("GAZPAR_TIMEZONE", "Europe/Paris"),
            ("GAZPAR_CONSUMPTION_TYPE", "informative"),
            // Connues : aucune alerte.
            ("GAZPAR_CONFIG", ""),
            ("GRDF_BASE_URL", ""),
            ("HA_TOKEN_FILE", ""),
            // Hors des préfixes surveillés.
            ("HA_TEST_TOKEN_FILE", "/x"),
            ("LOG_LEVEL", "debug"),
            ("PATH", "/bin"),
        ] {
            e.insert(k.into(), v.into());
        }
        let loaded = from_env(&e);
        assert_eq!(
            loaded.warnings,
            [
                "variable d'environnement GAZPAR_CONSUMPTION_TYPE inconnue, ignorée",
                "variable d'environnement GAZPAR_TIMEZONE inconnue, ignorée",
                "variable d'environnement GRDF_PCEE inconnue, ignorée \
                 (voulez-vous dire GRDF_PCE ?)",
            ]
        );
        accepted(loaded);
    }

    #[test]
    fn warnings_are_returned_even_when_config_is_refused() {
        let e = minimal_with(&[("GRDF_PCE", ""), ("GRDF_PCEE", "12345678901234")]);
        let loaded = from_env(&e);
        assert_eq!(loaded.warnings.len(), 1);
        assert_eq!(single(loaded), "GRDF_PCE manquant (ou GRDF_PCE_FILE)");
    }

    #[test]
    fn config_file_readable_by_others_is_warned_about() {
        for (mode, expected) in [
            (0o600, None),
            (0o400, None),
            (0o620, None),
            (0o640, Some("le groupe")),
            (0o604, Some("les autres utilisateurs")),
            (0o644, Some("le groupe et les autres utilisateurs")),
            (0o777, Some("le groupe et les autres utilisateurs")),
        ] {
            let fs = Fs::default().with_mode(CONF, "", mode);
            let loaded = load(&line(Some(CONF), &[]), &env(&minimal()), &fs);
            let expected: Vec<String> = expected
                .map(|who| {
                    format!(
                        "le fichier de configuration {CONF} est lisible par {who} \
                         alors qu'il contient des secrets (chmod 600 conseillé)"
                    )
                })
                .into_iter()
                .collect();
            assert_eq!(loaded.warnings, expected, "{mode:o}");
            accepted(loaded);
        }
    }

    #[test]
    fn unknown_mode_gives_no_warning() {
        struct NoMode;
        impl FileSystem for NoMode {
            fn read_to_string(&self, _: &str) -> std::io::Result<String> {
                Ok(String::new())
            }
            fn mode(&self, _: &str) -> std::io::Result<Option<u32>> {
                Ok(None)
            }
        }
        let loaded = load(&line(Some(CONF), &[]), &env(&minimal()), &NoMode);
        assert!(loaded.warnings.is_empty());
        accepted(loaded);
    }

    // --- Environnement du processus ---

    #[cfg(unix)]
    #[test]
    fn non_utf8_environment() {
        use std::os::unix::ffi::OsStringExt;
        let bad = || OsString::from_vec(vec![0x61, 0xff]);
        let ok = |s: &str| OsString::from(s);
        let e = environment_from([
            (ok("GRDF_PCE"), ok("12345678901234")),
            (ok("AUTRE"), bad()),
            (OsString::from_vec(vec![0xff]), ok("x")),
        ])
        .unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(
            environment_from([(ok("HA_TOKEN_FILE"), bad()), (ok("GAZPAR_CONFIG"), bad())])
                .unwrap_err(),
            [
                "HA_TOKEN_FILE : la valeur n'est pas de l'UTF-8 valide",
                "GAZPAR_CONFIG : la valeur n'est pas de l'UTF-8 valide",
            ]
        );
    }
}

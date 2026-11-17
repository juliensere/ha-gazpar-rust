//! Essais du binaire `ha-gazpar` tel qu'il sera lancé : environnement vidé,
//! vrais fichiers temporaires, codes de sortie et sorties standard. Aucun
//! accès réseau : GRDF est redirigé vers un port fermé de 127.0.0.1 et
//! Home Assistant aussi (sauf `HA_URL` du jeu d'essai `complet.conf`, sur
//! 127.0.0.1:8124, qui ne mène jamais à GRDF : la connexion à HA vient
//! d'abord et échoue faute de bon jeton). `check` affiche donc la
//! configuration puis sort avec le code 1. Les commandes complètes sont
//! essayées contre les faux serveurs dans `commands.rs`.

// Fichier de test : un échec doit faire paniquer le test.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_ha-gazpar");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/config");

/// Port de 127.0.0.1 où rien n'écoute (réservé, jamais ouvert par un
/// utilisateur ordinaire).
const CLOSED: &str = "127.0.0.1:1";

fn launch(args: &[&str], env: &[(&str, &str)]) -> Output {
    let closed = format!("http://{CLOSED}");
    let mut command = Command::new(BIN);
    command
        .args(args)
        .env_clear()
        // Jamais vers grdf.fr, quoi qu'il arrive.
        .env("GRDF_BASE_URL", &closed)
        .env("GRDF_LOGIN_URL", &closed)
        .envs(env.iter().copied());
    // Couverture : le binaire instrumenté écrit son profil ici.
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    command.output().expect("lancement du binaire")
}

/// Configuration acceptée et affichée par `check`, puis Home Assistant
/// injoignable (ou en refus) : code 1, sans erreur de configuration.
#[track_caller]
fn config_accepted(out: &Output) {
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stdout.starts_with("Configuration valide.\n"), "{stdout}");
    assert!(stderr.contains("Home Assistant"), "{stderr}");
    assert!(!stderr.contains("configuration invalide"), "{stderr}");
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}/{name}")).unwrap()
}

const VALID: [(&str, &str); 5] = [
    ("GRDF_USERNAME", "jean@example.org"),
    ("GRDF_PASSWORD", "mot-de-passe-secret"),
    ("GRDF_PCE", "12345678901234"),
    ("HA_URL", "ws://127.0.0.1:1/api/websocket"),
    ("HA_TOKEN", "jeton-secret"),
];

/// Dossier temporaire propre à un test : les tests d'un même binaire
/// partagent le pid et tournent en parallèle. Supprimé à la fin.
struct TempDir(PathBuf);

impl TempDir {
    fn new(test: &str) -> TempDir {
        let dir =
            std::env::temp_dir().join(format!("ha-gazpar-test-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    /// Écrit un fichier avec les droits demandés ; renvoie son chemin.
    fn file(&self, name: &str, content: &str, mode: u32) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, content).unwrap();
        set_mode(&path, mode);
        path.to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) {}

/// Ligne du tableau de `check` pour une option.
fn row<'a>(stdout: &'a str, name: &str) -> &'a str {
    stdout
        .lines()
        .find(|l| l.split_whitespace().next() == Some(name))
        .unwrap_or_else(|| panic!("{name} absent de :\n{stdout}"))
}

#[test]
fn check_refuses_empty_environment() {
    let out = launch(&["check"], &[]);
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    assert!(err.contains("configuration invalide"), "{err}");
    assert!(err.contains("GRDF_PASSWORD manquant"), "{err}");
    assert!(out.stdout.is_empty());
}

#[test]
fn check_accepts_valid_environment() {
    let out = launch(&["check"], &VALID);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    config_accepted(&out);
    assert!(
        stderr.contains("ha-gazpar : Home Assistant (127.0.0.1:1), connexion TCP : connexion refusée ; vérifiez HA_URL"),
        "{stderr}"
    );
    assert!(!stdout.contains("GRDF : connecté"), "{stdout}");
    for secret in ["mot-de-passe-secret", "jeton-secret", "jean@example.org"] {
        assert!(
            !stdout.contains(secret) && !stderr.contains(secret),
            "{secret}"
        );
    }
}

#[test]
fn secret_file_variant() {
    let dir = TempDir::new("secret_file_variant");
    let path = dir.file("token", "jeton-du-fichier\n", 0o600);
    let mut env: Vec<(&str, &str)> = VALID
        .iter()
        .copied()
        .filter(|(k, _)| *k != "HA_TOKEN")
        .collect();
    env.push(("HA_TOKEN_FILE", &path));
    let out = launch(&["check"], &env);
    let stdout = text(&out.stdout);
    config_accepted(&out);
    assert!(
        row(&stdout, "HA_TOKEN").ends_with(&format!("(fichier {path}, via HA_TOKEN_FILE)")),
        "{stdout}"
    );
    assert!(!stdout.contains("jeton-du-fichier"));
}

#[test]
fn priority_parameter_environment_file_default() {
    let dir = TempDir::new("priority");
    let conf = dir.file("ha-gazpar.conf", &fixture("complet.conf"), 0o600);
    let out = launch(
        &[
            "check",
            "--gazpar-log-level",
            "debug",
            "--grdf-pce=98765432109876",
        ],
        &[
            ("GAZPAR_CONFIG", &conf),
            ("GAZPAR_HISTORY_DAYS", "200"),
            ("GRDF_PCE", "11111111111111"),
        ],
    );
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    config_accepted(&out);
    // Aucun avertissement : fichier en 600, aucune variable inconnue.
    assert!(!stderr.contains("ATTENTION"), "{stderr}");
    assert!(
        stdout.contains(&format!(
            "Fichier de configuration : {conf} (environnement, GAZPAR_CONFIG)"
        )),
        "{stdout}"
    );
    let file = format!("(fichier {conf})");
    for (name, value, origin) in [
        ("GAZPAR_LOG_LEVEL", "debug", "(paramètre)"),
        ("GRDF_PCE", "98765432109876", "(paramètre)"),
        ("GAZPAR_HISTORY_DAYS", "200", "(environnement)"),
        ("GAZPAR_OVERLAP_DAYS", "20", file.as_str()),
        ("HA_URL", "ws://127.0.0.1:8124/api/websocket", file.as_str()),
        ("GRDF_PASSWORD", "***", file.as_str()),
        ("HA_TOKEN", "***", file.as_str()),
        ("GAZPAR_SCAN_INTERVAL", "480", "(défaut)"),
        // Vide dans le fichier : comptée comme absente.
        ("GAZPAR_STATISTIC_PREFIX", "gazpar", "(défaut)"),
    ] {
        let line = row(&stdout, name);
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields.get(1), Some(&value), "{line}");
        assert!(line.ends_with(origin), "{line}");
    }
    for secret in [
        "mot de passe factice",
        "jeton-factice-du-fichier",
        "prenom.nom@example.org",
    ] {
        assert!(!stdout.contains(secret), "{secret}");
    }
}

#[test]
fn config_parameter_beats_config_variable() {
    let dir = TempDir::new("config_parameter");
    let conf = dir.file("a.conf", &fixture("complet.conf"), 0o600);
    let out = launch(
        &["check", "--config", &conf],
        &[("GAZPAR_CONFIG", "/chemin/absent.conf")],
    );
    let stdout = text(&out.stdout);
    config_accepted(&out);
    assert!(stdout.contains(&format!("Fichier de configuration : {conf} (paramètre)")));
}

#[test]
fn windows_line_endings_and_byte_order_mark() {
    let dir = TempDir::new("crlf");
    let windows = format!("\u{feff}{}", fixture("complet.conf").replace('\n', "\r\n"));
    let conf = dir.file("windows.conf", &windows, 0o600);
    let out = launch(&["check", "--config", &conf], &[]);
    let stdout = text(&out.stdout);
    config_accepted(&out);
    assert!(row(&stdout, "GRDF_PCE").contains("01234567890123"));
}

#[cfg(unix)]
#[test]
fn config_file_readable_by_others_gives_a_warning() {
    let dir = TempDir::new("mode");
    let conf = dir.file("ouvert.conf", &fixture("complet.conf"), 0o644);
    let out = launch(&["check", "--config", &conf], &[]);
    let stderr = text(&out.stderr);
    config_accepted(&out);
    assert!(
        stderr.contains(&format!(
            "ATTENTION le fichier de configuration {conf} est lisible par le groupe \
             et les autres utilisateurs"
        )),
        "{stderr}"
    );
}

#[test]
fn config_file_errors_cite_line_and_name_only() {
    let dir = TempDir::new("errors");
    let conf = dir.file(
        "faux.conf",
        "GRDF_PASSWORD=\"mot-de-passe-secret\nmot-de-passe-secret\nGRDF_PCEE=1\n",
        0o600,
    );
    let out = launch(&["check", "--config", &conf], &VALID[..1]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr.contains(&format!(
            "fichier de configuration {conf}, ligne 1 : GRDF_PASSWORD : \
             guillemet ouvrant non refermé"
        )),
        "{stderr}"
    );
    assert!(stderr.contains("ligne 2 : « = » manquant"), "{stderr}");
    assert!(
        stderr.contains("ligne 3 : option GRDF_PCEE inconnue (voulez-vous dire GRDF_PCE ?)"),
        "{stderr}"
    );
    assert!(!stderr.contains("mot-de-passe-secret"), "{stderr}");
}

#[test]
fn missing_config_file_is_an_error() {
    let out = launch(&["check", "--config", "/chemin/absent.conf"], &VALID);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        text(&out.stderr),
        "ha-gazpar : configuration invalide\n  \
         - fichier de configuration /chemin/absent.conf : introuvable\n"
    );
}

#[test]
fn reset_without_yes_lists_statistics_and_exits_2() {
    let out = launch(&["reset"], &VALID);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        text(&out.stdout),
        "Statistiques qui seraient effacées dans Home Assistant :\n  \
         gazpar:12345678901234_energy\n  gazpar:12345678901234_volume\n"
    );
    let stderr = text(&out.stderr);
    assert!(stderr.contains("reset --yes"), "{stderr}");
    assert!(!stderr.contains("Usage"), "{stderr}");
}

#[test]
fn reset_with_yes_needs_home_assistant() {
    let out = launch(&["reset", "--yes"], &VALID);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(
        text(&out.stderr)
            .contains("Home Assistant (127.0.0.1:1), connexion TCP : connexion refusée"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn help_alone_or_after_a_command() {
    for args in [
        &["--help"][..],
        &["check", "--help"],
        &["reset", "--yes", "--help"],
    ] {
        let out = launch(args, &[]);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        let stdout = text(&out.stdout);
        assert!(
            stdout.starts_with("Usage : ha-gazpar <commande> [options]"),
            "{stdout}"
        );
        assert!(stdout.contains("--grdf-password-file"));
    }
}

#[test]
fn secret_refused_as_parameter() {
    for args in [
        &["check", "--grdf-password", "mot-de-passe-secret"][..],
        &["check", "--ha-token=jeton-secret"],
    ] {
        let out = launch(args, &VALID);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains("-file"), "{stderr}");
        assert!(
            !stderr.contains("mot-de-passe-secret") && !stderr.contains("jeton-secret"),
            "{stderr}"
        );
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn version() {
    let out = launch(&["--version"], &[]);
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stdout).starts_with(&format!("ha-gazpar {}", env!("CARGO_PKG_VERSION"))));
}

#[test]
fn usage_errors_exit_2() {
    assert_eq!(launch(&[], &[]).status.code(), Some(2));
    assert_eq!(launch(&["nimporte"], &[]).status.code(), Some(2));
    assert_eq!(launch(&["check", "en-trop"], &VALID).status.code(), Some(2));
    assert_eq!(launch(&["check", "--yes"], &VALID).status.code(), Some(2));
    assert_eq!(
        launch(&["check", "--grdf-pce="], &VALID).status.code(),
        Some(2)
    );
}

#[test]
fn once_with_home_assistant_unreachable_exits_1() {
    let out = launch(&["once"], &VALID);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(" ERREUR Home Assistant (127.0.0.1:1), connexion TCP : connexion refusée ; vérifiez HA_URL"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("GRDF"),
        "GRDF n'est pas contacté : {stderr}"
    );
}

#[test]
fn env_example_is_valid() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/.env.example");
    let example = std::fs::read_to_string(path).unwrap();
    let vars: Vec<(&str, &str)> = example
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .collect();
    assert_eq!(vars.len(), 10, "{vars:?}");
    let out = launch(&["check"], &vars);
    config_accepted(&out);
    assert!(
        !text(&out.stderr).contains("ATTENTION"),
        "{}",
        text(&out.stderr)
    );

    // Le même fichier est aussi un fichier de configuration valide.
    let dir = TempDir::new("env_example");
    let conf = dir.file("env.conf", &example, 0o600);
    let out = launch(&["check", "--config", &conf], &[]);
    config_accepted(&out);
}

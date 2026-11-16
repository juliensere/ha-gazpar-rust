//! Essais du binaire `ha-gazpar` tel qu'il sera lancé : environnement vidé,
//! codes de sortie et sorties standard. Aucun accès réseau.

// Fichier de test : un échec doit faire paniquer le test.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_ha-gazpar");

fn launch(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(BIN)
        .args(args)
        .env_clear()
        .envs(env.iter().copied())
        .output()
        .expect("lancement du binaire")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

const VALID: [(&str, &str); 5] = [
    ("GRDF_USERNAME", "jean@example.org"),
    ("GRDF_PASSWORD", "mot-de-passe-secret"),
    ("GRDF_PCE", "12345678901234"),
    ("HA_URL", "ws://127.0.0.1:8123/api/websocket"),
    ("HA_TOKEN", "jeton-secret"),
];

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
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("Configuration valide."));
    for secret in ["mot-de-passe-secret", "jeton-secret", "jean@example.org"] {
        assert!(
            !stdout.contains(secret) && !stderr.contains(secret),
            "{secret}"
        );
    }
}

#[test]
fn secret_file_variant() {
    let dir = std::env::temp_dir().join(format!("ha-gazpar-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("token");
    std::fs::write(&path, "jeton-du-fichier\n").unwrap();
    let path_text = path.to_string_lossy().into_owned();
    let mut env: Vec<(&str, &str)> = VALID
        .iter()
        .copied()
        .filter(|(k, _)| *k != "HA_TOKEN")
        .collect();
    env.push(("HA_TOKEN_FILE", &path_text));
    let out = launch(&["check"], &env);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
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
}

#[test]
fn unimplemented_commands_exit_1() {
    let out = launch(&["once"], &VALID);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("pas encore implémenté"));
}

#[test]
fn env_example_is_valid() {
    let example =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/.env.example")).unwrap();
    let vars: Vec<(&str, &str)> = example
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .collect();
    assert!(vars.len() >= 10, "{vars:?}");
    let out = launch(&["check"], &vars);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

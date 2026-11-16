//! Essais de bout en bout du vrai binaire, hors ligne : `once`, `check`,
//! `reset` et `run` contre le faux GRDF et le faux Home Assistant de
//! `tests/support`, tous deux sur 127.0.0.1. Aucun accès au vrai GRDF.
//!
//! Le binaire prend la vraie date du jour, dans le fuseau du faux HA
//! (Europe/Paris) : les relevés du faux GRDF sont donc datés par rapport à
//! aujourd'hui, du 12e au 3e jour avant, avec un jour pas encore publié au
//! milieu (5e jour avant), comme GRDF le fait souvent.

// Fichier de test : un échec doit faire paniquer le test.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use support::fake_grdf::{self, EMAIL, FakeGrdf, PASSWORD, PCE};
use support::fake_ha::{self, FakeHa};

const BIN: &str = env!("CARGO_BIN_EXE_ha-gazpar");
const TOKEN: &str = "jeton-ha-de-test-tres-secret";
const WRONG_PASSWORD: &str = "Mauvais-mot-de-passe-secret";
const ENERGY: &str = "gazpar:98765432109876_energy";
const VOLUME: &str = "gazpar:98765432109876_volume";

/// Jours publiés : du 12e au 3e jour avant aujourd'hui.
const FIRST_AGO: i64 = 12;
const LAST_AGO: i64 = 3;
/// Jour pas encore publié par GRDF au premier relevé.
const GAP_AGO: i64 = 5;

/// Faux GRDF et faux HA démarrés pour un essai.
struct Servers {
    grdf: FakeGrdf,
    ha: FakeHa,
}

impl Servers {
    /// Serveurs prêts, relevés récents publiés sauf le jour [`GAP_AGO`].
    fn start() -> Servers {
        let grdf = FakeGrdf::start();
        grdf.set_records(
            (LAST_AGO..=FIRST_AGO)
                .rev()
                .map(|ago| {
                    if ago == GAP_AGO {
                        fake_grdf::record(&ago_day(ago).to_string(), None, None)
                    } else {
                        let (energy, volume) = values(ago);
                        fake_grdf::record(&ago_day(ago).to_string(), Some(energy), Some(volume))
                    }
                })
                .collect(),
        );
        Servers {
            grdf,
            ha: FakeHa::start(TOKEN),
        }
    }

    /// Variables d'environnement du binaire, avec ce mot de passe GRDF.
    fn env_with_password(&self, password: &str) -> Vec<(String, String)> {
        [
            ("GRDF_USERNAME", EMAIL),
            ("GRDF_PASSWORD", password),
            ("GRDF_PCE", PCE),
            ("HA_URL", self.ha.url().as_str()),
            ("HA_TOKEN", TOKEN),
            ("GRDF_BASE_URL", self.grdf.base_url()),
            ("GRDF_LOGIN_URL", self.grdf.login_url()),
            ("GAZPAR_HISTORY_DAYS", "30"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
    }

    fn env(&self) -> Vec<(String, String)> {
        self.env_with_password(PASSWORD)
    }

    fn launch(&self, args: &[&str]) -> Output {
        command(args, &self.env()).output().unwrap()
    }

    /// Jours et sommes d'une statistique dans le faux HA.
    fn sums(&self, id: &str) -> Vec<(Date, f64)> {
        let tz = TimeZone::get(fake_ha::TIME_ZONE).unwrap();
        self.ha
            .state()
            .series
            .get(id)
            .map(|series| {
                series
                    .iter()
                    .map(|(ms, row)| {
                        let day = Timestamp::from_millisecond(*ms)
                            .unwrap()
                            .to_zoned_datetime(tz.clone())
                            .date();
                        (day, row.sum)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn imports(&self) -> usize {
        self.ha.state().count("recorder/import_statistics")
    }
}

/// Énergie et volume publiés pour un jour : distincts d'un jour à l'autre.
fn values(ago: i64) -> (f64, f64) {
    let n = ago as f64;
    (10.0 + n, 1.0 + n / 10.0)
}

/// Aujourd'hui dans le fuseau du faux HA, moins `ago` jours.
fn ago_day(ago: i64) -> Date {
    let tz = TimeZone::get(fake_ha::TIME_ZONE).unwrap();
    Timestamp::now()
        .to_zoned_datetime(tz)
        .date()
        .checked_sub(Span::new().days(ago))
        .unwrap()
}

fn command(args: &[&str], env: &[(String, String)]) -> Command {
    let mut command = Command::new(BIN);
    command
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)));
    // Couverture : le binaire instrumenté écrit son profil ici.
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    command
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Sorties standard et d'erreur, après avoir vérifié le code de sortie et
/// l'absence de tout secret de test.
#[track_caller]
fn outputs(out: &Output, code: i32) -> (String, String) {
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(code), "{stdout}{stderr}");
    no_secret(&stdout);
    no_secret(&stderr);
    (stdout, stderr)
}

#[track_caller]
fn no_secret(output: &str) {
    for secret in [EMAIL, PASSWORD, WRONG_PASSWORD, TOKEN, "jean.dupont"] {
        assert!(
            !output.contains(secret),
            "secret « {secret} » dans :\n{output}"
        );
    }
}

#[test]
fn once_twice_imports_then_nothing() {
    let servers = Servers::start();

    let (stdout, stderr) = outputs(&servers.launch(&["once"]), 0);
    assert!(stdout.is_empty(), "{stdout}");
    let last = ago_day(LAST_AGO);
    assert!(
        stderr.contains(&format!(
            " INFO relevé : 9 jours importés, dernier jour connu {last}"
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            " INFO connecté à Home Assistant {}, fuseau Europe/Paris",
            fake_ha::VERSION
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(" INFO ha-gazpar {}", env!("CARGO_PKG_VERSION"))),
        "{stderr}"
    );
    assert_eq!(servers.imports(), 2, "un import par statistique");
    let energy = servers.sums(ENERGY);
    assert_eq!(energy.len(), 9);
    assert!(energy.iter().all(|(day, _)| *day != ago_day(GAP_AGO)));
    let total: f64 = (LAST_AGO..=FIRST_AGO)
        .filter(|ago| *ago != GAP_AGO)
        .map(|ago| values(ago).0)
        .sum();
    assert_eq!(energy.last(), Some(&(last, total)));
    assert_eq!(servers.sums(VOLUME).len(), 9);

    // Deuxième relevé : rien de nouveau, rien n'est écrit (PLAN.md §11, étape 5).
    let (_, stderr) = outputs(&servers.launch(&["once"]), 0);
    assert!(
        stderr.contains(&format!(
            " INFO relevé : aucun jour importé, dernier jour connu {last}"
        )),
        "{stderr}"
    );
    assert_eq!(servers.imports(), 2, "aucun import de plus");
    assert_eq!(servers.sums(ENERGY), energy);

    let c = servers.grdf.counters();
    assert!(c.problems.is_empty(), "{:?}", c.problems);
    assert_eq!(c.logins, 2, "une connexion à GRDF par relevé");
    assert_eq!(c.foreign_hosts, 0);
    assert_eq!(servers.ha.state().connections, 2);
}

#[test]
fn day_published_later_is_caught_up_at_the_next_reading() {
    let servers = Servers::start();
    outputs(&servers.launch(&["once"]), 0);
    let before = servers.sums(ENERGY);

    // GRDF publie le jour manquant.
    let gap = ago_day(GAP_AGO);
    servers
        .grdf
        .publish(fake_grdf::record(&gap.to_string(), Some(42.5), Some(3.75)));
    let (_, stderr) = outputs(&servers.launch(&["once"]), 0);
    // Le jour rattrapé et les deux suivants, dont la somme change.
    assert!(
        stderr.contains(&format!(
            " INFO relevé : 3 jours importés, dernier jour connu {}",
            ago_day(LAST_AGO)
        )),
        "{stderr}"
    );
    let after = servers.sums(ENERGY);
    assert_eq!(after.len(), 10);
    let sum_on =
        |series: &[(Date, f64)], day: Date| series.iter().find(|(d, _)| *d == day).map(|(_, s)| *s);
    let previous = sum_on(&before, ago_day(GAP_AGO + 1)).unwrap();
    assert_eq!(sum_on(&after, gap), Some(previous + 42.5));
    assert_eq!(
        after.last().unwrap().1,
        before.last().unwrap().1 + 42.5,
        "toutes les sommes suivantes sont décalées"
    );
    let volume = servers.sums(VOLUME);
    assert_eq!(volume.len(), 10);
}

#[test]
fn check_shows_home_assistant_and_last_grdf_days_and_writes_nothing() {
    let servers = Servers::start();
    let (stdout, stderr) = outputs(&servers.launch(&["check"]), 0);
    assert!(stdout.starts_with("Configuration valide.\n"), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "Home Assistant : connecté, version {}, fuseau Europe/Paris (date du jour {})",
            fake_ha::VERSION,
            ago_day(0)
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("  {ENERGY} : aucun jour connu\n")),
        "{stdout}"
    );
    assert!(
        stdout.contains("GRDF : connecté, relevés demandés du "),
        "{stdout}"
    );
    // Les 7 derniers jours lus, du 9e au 3e jour avant aujourd'hui.
    let shown: Vec<&str> = stdout.lines().filter(|l| l.starts_with("  20")).collect();
    assert_eq!(shown.len(), 7, "{stdout}");
    assert!(
        shown[0].starts_with(&format!("  {}", ago_day(9))),
        "{stdout}"
    );
    assert!(
        shown[6].starts_with(&format!("  {}", ago_day(LAST_AGO))),
        "{stdout}"
    );
    let gap_line = shown
        .iter()
        .find(|l| l.starts_with(&format!("  {}", ago_day(GAP_AGO))))
        .unwrap();
    assert!(gap_line.contains("non publié"), "{gap_line}");
    assert!(
        stdout.contains(&format!(
            "  {ENERGY} : 9 jours, du {} au {}",
            ago_day(FIRST_AGO),
            ago_day(LAST_AGO)
        )),
        "{stdout}"
    );
    assert!(
        stderr.contains(" INFO connecté à Mon Espace GRDF"),
        "{stderr}"
    );

    // Rien n'est écrit.
    assert_eq!(servers.imports(), 0);
    assert!(servers.ha.state().series.is_empty());
    assert_eq!(servers.grdf.counters().logins, 1);

    // Après un import, check montre le dernier jour connu et n'importerait rien.
    outputs(&servers.launch(&["once"]), 0);
    let (stdout, _) = outputs(&servers.launch(&["check"]), 0);
    assert!(
        stdout.contains(&format!(
            "  {ENERGY} : dernier jour connu {}\n",
            ago_day(LAST_AGO)
        )),
        "{stdout}"
    );
    assert!(stdout.contains(&format!("  {VOLUME} : rien\n")), "{stdout}");
    assert_eq!(servers.imports(), 2);
}

#[test]
fn reset_with_yes_clears_and_without_yes_contacts_nothing() {
    let servers = Servers::start();
    outputs(&servers.launch(&["once"]), 0);
    assert!(!servers.sums(ENERGY).is_empty());
    let ha_connections = servers.ha.state().connections;
    let grdf_pages = servers.grdf.counters().start_pages;

    let (stdout, stderr) = outputs(&servers.launch(&["reset"]), 2);
    assert!(
        stdout.starts_with("Statistiques qui seraient effacées dans Home Assistant :"),
        "{stdout}"
    );
    assert!(stderr.contains("rien n'a été effacé"), "{stderr}");
    assert_eq!(servers.ha.state().connections, ha_connections);
    assert_eq!(servers.ha.state().count("recorder/clear_statistics"), 0);

    let (stdout, _) = outputs(&servers.launch(&["reset", "--yes"]), 0);
    assert!(
        stdout.starts_with(&format!(
            "Statistiques effacées dans Home Assistant :\n  {ENERGY}\n  {VOLUME}\n"
        )),
        "{stdout}"
    );
    assert_eq!(servers.ha.state().count("recorder/clear_statistics"), 1);
    assert!(servers.ha.state().series.is_empty());
    assert_eq!(servers.ha.state().connections, ha_connections + 1);
    assert_eq!(
        servers.grdf.counters().start_pages,
        grdf_pages,
        "reset ne contacte jamais GRDF"
    );

    // Le relevé suivant réimporte tout.
    let (_, stderr) = outputs(&servers.launch(&["once"]), 0);
    assert!(stderr.contains("relevé : 9 jours importés"), "{stderr}");
}

#[test]
fn rejected_password_single_attempt_clear_message_no_secret() {
    let servers = Servers::start();
    let mut env = servers.env_with_password(WRONG_PASSWORD);
    env.push(("GAZPAR_LOG_LEVEL".into(), "debug".into()));

    let out = command(&["once"], &env).output().unwrap();
    let (stdout, stderr) = outputs(&out, 1);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains(
            " ERREUR connexion à GRDF refusée : mot de passe refusé par GRDF (statut HTTP 401) \
             : Authentication failed ; vérifiez GRDF_USERNAME et GRDF_PASSWORD"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains(" DEBUG connexion 4/5"),
        "niveau debug : {stderr}"
    );
    let c = servers.grdf.counters();
    assert_eq!(
        (c.start_pages, c.identify, c.challenge, c.logins),
        (1, 1, 1, 0),
        "une seule tentative"
    );
    assert!(c.consumption.is_empty());
    assert_eq!(servers.imports(), 0);

    // check : même refus, code 1, la configuration a été affichée.
    let out = command(&["check"], &env).output().unwrap();
    let (stdout, stderr) = outputs(&out, 1);
    assert!(stdout.contains("Home Assistant : connecté"), "{stdout}");
    assert!(
        stderr.contains("ha-gazpar : connexion à GRDF refusée"),
        "{stderr}"
    );
    assert_eq!(servers.grdf.counters().challenge, 2);
}

/// Lance `run` et rend le processus et ses lignes de journal au fil de
/// l'eau.
fn spawn_run(env: &[(String, String)]) -> (Child, mpsc::Receiver<String>) {
    let mut env = env.to_vec();
    env.push(("GAZPAR_SCAN_INTERVAL".into(), "60".into()));
    let mut child = command(&["run"], &env)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if send.send(line).is_err() {
                break;
            }
        }
    });
    (child, receive)
}

/// Lignes du journal jusqu'à celle qui contient `needle` comprise.
fn wait_for(lines: &mpsc::Receiver<String>, needle: &str) -> Vec<String> {
    let mut seen = Vec::new();
    loop {
        match lines.recv_timeout(Duration::from_secs(30)) {
            Ok(line) => {
                let found = line.contains(needle);
                seen.push(line);
                if found {
                    return seen;
                }
            }
            Err(e) => panic!(
                "« {needle} » attendu ({e}) ; journal :\n{}",
                seen.join("\n")
            ),
        }
    }
}

#[test]
fn run_reads_at_once_and_keeps_running() {
    let servers = Servers::start();
    let (mut child, lines) = spawn_run(&servers.env());
    let seen = wait_for(&lines, " INFO relevé : ");
    let journal = seen.join("\n");
    assert!(
        journal.contains("un relevé maintenant, puis toutes les 60 min"),
        "{journal}"
    );
    assert!(journal.contains("relevé : 9 jours importés"), "{journal}");
    no_secret(&journal);
    // Toujours en vie : il attend le relevé suivant.
    std::thread::sleep(Duration::from_millis(200));
    assert!(child.try_wait().unwrap().is_none(), "le démon est sorti");
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(servers.imports(), 2);
}

#[test]
fn run_with_rejected_password_stays_up_after_a_single_attempt() {
    let servers = Servers::start();
    let (mut child, lines) = spawn_run(&servers.env_with_password(WRONG_PASSWORD));
    let seen = wait_for(&lines, " ERREUR connexion à GRDF refusée");
    let journal = seen.join("\n");
    assert!(
        journal.contains("Plus aucune connexion à GRDF jusqu'au redémarrage de ha-gazpar"),
        "{journal}"
    );
    no_secret(&journal);
    std::thread::sleep(Duration::from_millis(200));
    assert!(child.try_wait().unwrap().is_none(), "le démon est sorti");
    child.kill().unwrap();
    child.wait().unwrap();
    let c = servers.grdf.counters();
    assert_eq!((c.identify, c.challenge, c.logins), (1, 1, 0));
}

/// Démonstration pour `make dev` : `check`, puis `once` deux fois, contre
/// les faux serveurs, sorties affichées telles quelles.
#[test]
#[ignore = "démonstration : make dev"]
fn demo() {
    let servers = Servers::start();
    for args in [&["check"][..], &["once"], &["once"]] {
        let out = servers.launch(args);
        println!(
            "$ ha-gazpar {}   (code {})",
            args.join(" "),
            out.status.code().unwrap_or(-1)
        );
        print!("{}", text(&out.stdout));
        print!("{}", text(&out.stderr));
        println!();
    }
}

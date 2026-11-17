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
use support::fake_grdf::{
    self, CODE_PREFIX, DEVICE_COOKIE_VALUE, EMAIL, FakeGrdf, PASSWORD, PCE, SESSION_PREFIX,
    STATE_HANDLE, STATE_TOKEN, STATE_TOKEN_IN_PAGE,
};
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

/// Aucun secret : identifiants, jeton, jetons de connexion d'Okta, valeurs
/// des cookies, codes OAuth, ni requête d'une adresse du parcours de
/// connexion.
#[track_caller]
fn no_secret(output: &str) {
    for secret in [
        EMAIL,
        PASSWORD,
        WRONG_PASSWORD,
        TOKEN,
        "jean.dupont",
        STATE_TOKEN,
        STATE_TOKEN_IN_PAGE,
        STATE_HANDLE,
        "FAKE-HANDLE",
        DEVICE_COOKIE_VALUE,
        SESSION_PREFIX,
        CODE_PREFIX,
        "stateToken=",
        "code=",
        "redirect_uri",
        "jeton-perime-secret",
    ] {
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
    let login = servers.grdf.login_url().trim_start_matches("http://");
    assert!(
        stderr.contains(&format!(
            " ERREUR GRDF, connexion 4/5 (mot de passe Okta) : identifiants refusés par GRDF \
             (statut HTTP 401, {login}, Okta : « Authentication failed » [errors.E0000004]) ; \
             vérifiez GRDF_USERNAME et GRDF_PASSWORD (identifiants de Mon Espace GRDF)\n"
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains(" DEBUG GRDF, connexion 4/5 (mot de passe Okta)\n"),
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
        stderr
            .contains("ha-gazpar : GRDF, connexion 4/5 (mot de passe Okta) : identifiants refusés"),
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
    let seen = wait_for(
        &lines,
        " ERREUR GRDF, connexion 4/5 (mot de passe Okta) : identifiants refusés",
    );
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

/// Port de 127.0.0.1 où plus personne n'écoute.
fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// `env` avec `key` remplacée par `value`.
fn with(env: &[(String, String)], key: &str, value: &str) -> Vec<(String, String)> {
    let mut env: Vec<_> = env.iter().filter(|(k, _)| k != key).cloned().collect();
    env.push((key.to_owned(), value.to_owned()));
    env
}

/// `127.0.0.1:port` d'une adresse `http://127.0.0.1:port` ou `ws://…`.
fn host_port(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest).to_owned()
}

/// Lance `once` au niveau `debug` avec `env` : code de sortie et journal,
/// après avoir vérifié qu'aucun secret n'y figure.
#[track_caller]
fn once_at_debug(env: &[(String, String)], code: i32) -> String {
    let env = with(env, "GAZPAR_LOG_LEVEL", "debug");
    let (_, stderr) = outputs(&command(&["once"], &env).output().unwrap(), code);
    stderr
}

#[track_caller]
fn assert_line(journal: &str, expected: &str) {
    assert!(
        journal.lines().any(|l| l.contains(expected)),
        "« {expected} » absent :\n{journal}"
    );
}

#[test]
fn debug_journal_shows_the_grdf_exchange_without_secrets() {
    let servers = Servers::start();
    let journal = once_at_debug(&servers.env(), 0);
    let base = host_port(servers.grdf.base_url());
    let login = host_port(servers.grdf.login_url());
    for expected in [
        format!(" DEBUG GRDF : GET {base}/ → 302 en "),
        format!(
            " DEBUG GRDF : redirection 302 vers {login}/oauth2/aus5y2ta2uEHjCWIR417/v1/authorize (saut 1/10)"
        ),
        format!(" DEBUG GRDF : GET {login}/oauth2/aus5y2ta2uEHjCWIR417/v1/authorize → 200 en "),
        " DEBUG GRDF : cookie reçu de 127.0.0.1 : DT (hôte 127.0.0.1 seul, chemin /)".to_owned(),
        " DEBUG GRDF, connexion 3/5 (identification Okta)\n"
            .trim_end()
            .to_owned(),
        format!(" DEBUG GRDF : POST {login}/idp/idx/identify → 200 en "),
        "ms (cookies envoyés : DT, ln)".to_owned(),
        " DEBUG GRDF, connexion 3/5 (identification Okta) : statut 200, étapes proposées : \
         challenge-authenticator, select-authenticator-authenticate"
            .to_owned(),
        format!(" DEBUG GRDF, connexion 5/5 (fin de connexion) : success.href vers {login}"),
        " DEBUG GRDF : cookie reçu de 127.0.0.1 : auth_token (hôte 127.0.0.1 seul, chemin /)"
            .to_owned(),
        " DEBUG GRDF, liste des PCE : 1 PCE sur le compte".to_owned(),
        format!(
            " DEBUG GRDF, relevés du {} au {} : 10 jours reçus, du {} au {}, dont 1 sans valeur ; \
             aucun relevé pour dateFin",
            ago_day(30),
            ago_day(1),
            ago_day(FIRST_AGO),
            ago_day(LAST_AGO)
        ),
        " DEBUG Home Assistant : recorder/import_statistics (n° ".to_owned(),
    ] {
        assert_line(&journal, &expected);
    }
    // Niveau info : sobre, sans le détail des échanges.
    let (_, info) = outputs(&servers.launch(&["once"]), 0);
    assert!(!info.contains(" DEBUG "), "{info}");
    assert!(info.lines().count() <= 5, "{info}");
}

/// Une panne de chaque famille, au niveau `debug` : la ligne d'erreur dit
/// l'opération, la cible, la cause et quoi faire ; aucun secret nulle part.
#[test]
fn every_error_family_is_named_without_secrets_at_debug_level() {
    let servers = Servers::start();
    let env = servers.env();
    let login = host_port(servers.grdf.login_url());
    let base = host_port(servers.grdf.base_url());

    // Mot de passe refusé.
    let journal = once_at_debug(&servers.env_with_password(WRONG_PASSWORD), 1);
    assert_line(
        &journal,
        &format!(
            " ERREUR GRDF, connexion 4/5 (mot de passe Okta) : identifiants refusés par GRDF \
             (statut HTTP 401, {login}, Okta : « Authentication failed » [errors.E0000004]) ; \
             vérifiez GRDF_USERNAME et GRDF_PASSWORD"
        ),
    );

    // Service de connexion en panne : pas un refus d'identifiants.
    servers.grdf.set_faults(fake_grdf::Faults {
        login_unavailable: 1,
        ..fake_grdf::Faults::default()
    });
    let journal = once_at_debug(&env, 1);
    assert_line(
        &journal,
        &format!(
            " ERREUR GRDF, connexion 4/5 (mot de passe Okta) : service de connexion \
             indisponible (statut HTTP 503, page HTML, {login}, Okta : aucun motif donné)"
        ),
    );

    // GRDF injoignable : passager.
    let closed = format!("http://127.0.0.1:{}", closed_port());
    let down = with(
        &with(&env, "GRDF_BASE_URL", &closed),
        "GRDF_LOGIN_URL",
        &closed,
    );
    let journal = once_at_debug(&down, 1);
    assert_line(
        &journal,
        &format!(
            " ERREUR GRDF, connexion 1/5 (page d'accueil) : connexion refusée ({}) ; panne \
             passagère, relancez plus tard",
            host_port(&closed)
        ),
    );

    // Débit limité jusqu'à l'abandon : chaque tentative, puis l'abandon.
    servers.grdf.set_faults(fake_grdf::Faults {
        rate_limit: 10,
        retry_after: Some("0".into()),
        ..fake_grdf::Faults::default()
    });
    let journal = once_at_debug(&env, 1);
    for k in 1..=3 {
        assert_line(
            &journal,
            &format!(
                " ATTENTION GRDF, liste des PCE : tentative {k}/4 échouée, débit limité par GRDF \
                 (statut HTTP 429, Retry-After « 0 », {base}) ; nouvel essai dans 0 s"
            ),
        );
    }
    assert_line(
        &journal,
        &format!(
            " ERREUR GRDF, liste des PCE : débit limité par GRDF (statut HTTP 429, Retry-After \
             « 0 », {base}) ; abandon après 4 tentatives"
        ),
    );
    servers.grdf.set_faults(fake_grdf::Faults::default());

    // Home Assistant injoignable : passager, vérifier HA_URL.
    let port = closed_port();
    let ha_down = format!("ws://127.0.0.1:{port}/api/websocket");
    let journal = once_at_debug(&with(&env, "HA_URL", &ha_down), 1);
    assert_line(
        &journal,
        &format!(
            " ERREUR Home Assistant (127.0.0.1:{port}), connexion TCP : connexion refusée ; \
             vérifiez HA_URL (hôte et port de Home Assistant, 8123 en général) et que Home \
             Assistant est démarré ; panne passagère, relancez plus tard"
        ),
    );

    // Jeton refusé.
    let journal = once_at_debug(&with(&env, "HA_TOKEN", "jeton-perime-secret"), 1);
    assert_line(
        &journal,
        &format!(
            " ERREUR Home Assistant ({}), authentification : jeton refusé (« Invalid access \
             token or password ») ; créez un jeton d'accès longue durée",
            host_port(&servers.ha.url())
        ),
    );

    // Adresse qui n'est pas l'API WebSocket de Home Assistant.
    let not_ha = format!("ws://{base}/api/websocket");
    let journal = once_at_debug(&with(&env, "HA_URL", &not_ha), 1);
    assert_line(
        &journal,
        &format!(
            " ERREUR Home Assistant ({base}), poignée de main WebSocket : réponse « HTTP/1.1 401 \
             Unauthorized » au lieu de « HTTP/1.1 101 » : ce n'est pas l'API WebSocket de Home \
             Assistant ; vérifiez HA_URL"
        ),
    );
}

/// Démonstration pour `make dev`, contre les faux serveurs, sorties
/// affichées telles quelles : `check`, `once` deux fois (le second au
/// niveau `debug`), puis une panne de chaque famille (GRDF et Home
/// Assistant injoignables, identifiants refusés, reprises…).
#[test]
#[ignore = "démonstration : make dev"]
fn demo() {
    let servers = Servers::start();
    let show = |title: &str, args: &[&str], env: &[(String, String)]| {
        let out = command(args, env).output().unwrap();
        println!(
            "$ ha-gazpar {}   ({title}, code {})",
            args.join(" "),
            out.status.code().unwrap_or(-1)
        );
        print!("{}", text(&out.stdout));
        print!("{}", text(&out.stderr));
        println!();
    };
    let env = servers.env();
    let debug = with(&env, "GAZPAR_LOG_LEVEL", "debug");
    show("vérification", &["check"], &env);
    show("premier relevé", &["once"], &env);
    show("second relevé, niveau debug", &["once"], &debug);

    show(
        "mot de passe refusé",
        &["once"],
        &servers.env_with_password(WRONG_PASSWORD),
    );
    servers.grdf.set_faults(fake_grdf::Faults {
        login_unavailable: 1,
        ..fake_grdf::Faults::default()
    });
    show("service de connexion en panne (503)", &["once"], &env);
    let closed = format!("http://127.0.0.1:{}", closed_port());
    let grdf_down = with(
        &with(&env, "GRDF_BASE_URL", &closed),
        "GRDF_LOGIN_URL",
        &closed,
    );
    show("GRDF injoignable", &["once"], &grdf_down);
    servers.grdf.set_faults(fake_grdf::Faults {
        rate_limit: 1,
        retry_after: Some("1".into()),
        html_errors: 1,
        ..fake_grdf::Faults::default()
    });
    show("reprises : 429 puis page d'erreur", &["once"], &env);
    servers.grdf.set_faults(fake_grdf::Faults {
        rate_limit: 10,
        retry_after: Some("0".into()),
        ..fake_grdf::Faults::default()
    });
    show("reprises épuisées : 429", &["once"], &env);
    servers.grdf.set_faults(fake_grdf::Faults::default());

    let ha_down = format!("ws://127.0.0.1:{}/api/websocket", closed_port());
    show(
        "Home Assistant injoignable",
        &["once"],
        &with(&env, "HA_URL", &ha_down),
    );
    show(
        "jeton Home Assistant refusé",
        &["once"],
        &with(&env, "HA_TOKEN", "jeton-perime"),
    );
    let not_ha = format!(
        "{}/api/websocket",
        servers.grdf.base_url().replacen("http", "ws", 1)
    );
    show(
        "HA_URL vers un serveur qui n'est pas Home Assistant",
        &["once"],
        &with(&env, "HA_URL", &not_ha),
    );
}

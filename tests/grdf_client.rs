//! Client GRDF de bout en bout contre le faux serveur (`tests/support`),
//! sur 127.0.0.1 uniquement : connexion, liste des PCE, relevés, reprises.

// Fichier de test : un échec doit faire paniquer le test.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use ha_gazpar::Error;
use ha_gazpar::domain::Quantity;
use ha_gazpar::grdf::api::{GrdfClient, GrdfSettings, MAX_ATTEMPTS, MAX_WINDOW_DAYS};
use ha_gazpar::grdf::auth::Credentials;
use ha_gazpar::ports::ConsumptionSource;
use ha_gazpar::secret::Secret;
use jiff::civil::{Date, date};
use support::fake_grdf::{
    CODE_PREFIX, DEVICE_COOKIE_VALUE, EMAIL, Expiry, FakeGrdf, Faults, PASSWORD, PCE,
    SESSION_PREFIX, STATE_HANDLE, STATE_TOKEN, STATE_TOKEN_IN_PAGE,
};

type Sleeps = Rc<RefCell<Vec<Duration>>>;

fn client_for(fake: &FakeGrdf, pce: &str, password: &str) -> (GrdfClient, Sleeps) {
    let sleeps: Sleeps = Rc::default();
    let recorded = Rc::clone(&sleeps);
    let settings = GrdfSettings {
        base_url: fake.base_url().to_owned(),
        login_url: fake.login_url().to_owned(),
        credentials: Credentials {
            username: Secret::new(EMAIL),
            password: Secret::new(password),
        },
        pce: pce.to_owned(),
    };
    let client =
        GrdfClient::new(settings, Box::new(move |d| recorded.borrow_mut().push(d))).unwrap();
    (client, sleeps)
}

fn client(fake: &FakeGrdf) -> (GrdfClient, Sleeps) {
    client_for(fake, PCE, PASSWORD)
}

/// Fenêtre du jeu d'essai : 28 jours, du 2021-07-26 au 2021-08-22.
const FROM: Date = date(2021, 7, 26);
const TO: Date = date(2021, 8, 22);

/// `127.0.0.1:port` d'une adresse `http://127.0.0.1:port`.
fn host_port(url: &str) -> &str {
    url.trim_start_matches("http://")
}

fn secs(sleeps: &Sleeps) -> Vec<u64> {
    sleeps.borrow().iter().map(Duration::as_secs).collect()
}

#[test]
fn login_then_readings_from_fixture() {
    let fake = FakeGrdf::start();
    let (mut grdf, sleeps) = client(&fake);
    let readings = grdf.daily_readings(FROM, TO).unwrap();

    assert_eq!(readings.len(), 28);
    assert_eq!(readings[0].day, FROM);
    assert_eq!(readings[0].energy_kwh, Some(Quantity::from_millis(10_000)));
    assert_eq!(readings[0].volume_m3, Some(Quantity::from_millis(1_000)));
    let gap = readings
        .iter()
        .find(|r| r.day == date(2021, 8, 10))
        .unwrap();
    assert_eq!((gap.energy_kwh, gap.volume_m3), (None, None));

    let c = fake.counters();
    assert!(c.problems.is_empty(), "{:?}", c.problems);
    assert_eq!(
        (c.start_pages, c.identify, c.challenge, c.logins, c.pce_list),
        (1, 1, 1, 1, 1)
    );
    assert_eq!(
        c.consumption,
        vec![("2021-07-26".to_owned(), "2021-08-22".to_owned())]
    );
    assert!(sleeps.borrow().is_empty());

    // Deuxième relevé : même session, PCE déjà vérifié.
    let again = grdf
        .daily_readings(date(2021, 8, 1), date(2021, 8, 2))
        .unwrap();
    assert_eq!(again.len(), 2);
    let c = fake.counters();
    assert_eq!((c.logins, c.pce_list, c.consumption.len()), (1, 1, 2));
}

#[test]
fn readings_outside_fixture_are_empty() {
    let fake = FakeGrdf::start();
    let (mut grdf, _) = client(&fake);
    assert!(
        grdf.daily_readings(date(2010, 1, 1), date(2010, 1, 7))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn wrong_password_is_a_clear_error_after_a_single_attempt() {
    let fake = FakeGrdf::start();
    let (mut grdf, sleeps) = client_for(&fake, PCE, "mauvais-mot-de-passe");
    let e = grdf.daily_readings(FROM, TO).unwrap_err();

    assert_eq!(
        e.to_string(),
        format!(
            "GRDF, connexion 4/5 (mot de passe Okta) : identifiants refusés par GRDF \
             (statut HTTP 401, {}, Okta : « Authentication failed » [errors.E0000004]) ; \
             vérifiez GRDF_USERNAME et GRDF_PASSWORD (identifiants de Mon Espace GRDF)",
            host_port(fake.login_url())
        )
    );
    assert!(matches!(e, Error::GrdfCredentialsRejected(_)), "{e:?}");
    for secret in [EMAIL, "mauvais-mot-de-passe", STATE_HANDLE, STATE_TOKEN] {
        assert!(!format!("{e} {e:?}").contains(secret), "{secret}");
    }

    let c = fake.counters();
    assert!(c.problems.is_empty(), "{:?}", c.problems);
    assert_eq!((c.identify, c.challenge, c.logins), (1, 1, 0));
    assert_eq!((c.pce_list, c.consumption.len()), (0, 0));
    assert!(sleeps.borrow().is_empty(), "pas de nouvel essai");
}

#[test]
fn unavailable_login_service_is_not_a_credentials_rejection() {
    let fake = FakeGrdf::start();
    fake.set_faults(Faults {
        login_unavailable: 1,
        ..Faults::default()
    });
    let (mut grdf, sleeps) = client(&fake);
    let e = grdf.daily_readings(FROM, TO).unwrap_err();
    assert_eq!(
        e,
        Error::GrdfLogin(format!(
            "GRDF, connexion 4/5 (mot de passe Okta) : service de connexion indisponible \
             (statut HTTP 503, page HTML, {}, Okta : aucun motif donné)",
            host_port(fake.login_url())
        ))
    );
    let c = fake.counters();
    assert_eq!((c.identify, c.challenge, c.logins), (1, 1, 0));
    assert!(sleeps.borrow().is_empty(), "pas de nouvel essai");

    // Le relevé suivant se connecte normalement.
    assert_eq!(grdf.daily_readings(FROM, TO).unwrap().len(), 28);
    assert_eq!(fake.counters().logins, 1);
}

#[test]
fn login_page_without_state_token_names_the_step_and_the_page() {
    let fake = FakeGrdf::start();
    fake.set_faults(Faults {
        no_state_token: true,
        ..Faults::default()
    });
    let (mut grdf, _) = client(&fake);
    let e = grdf.daily_readings(FROM, TO).unwrap_err();
    assert_eq!(
        e,
        Error::GrdfLogin(format!(
            "GRDF, connexion 1/5 (page d'accueil) : jeton « stateToken » introuvable dans la \
             page reçue (statut HTTP 200, page HTML, {}) : le parcours de connexion de GRDF a \
             peut-être changé",
            host_port(fake.login_url())
        ))
    );
    assert!(!e.is_transient());
    assert_eq!(fake.counters().identify, 0);
}

#[test]
fn unreachable_grdf_names_the_login_step_and_the_target() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let closed = format!("http://127.0.0.1:{port}");
    let settings = GrdfSettings {
        base_url: closed.clone(),
        login_url: closed,
        credentials: Credentials {
            username: Secret::new(EMAIL),
            password: Secret::new(PASSWORD),
        },
        pce: PCE.to_owned(),
    };
    let mut grdf = GrdfClient::new(settings, Box::new(|_| panic!("pas d'attente"))).unwrap();
    let e = grdf.daily_readings(FROM, TO).unwrap_err();
    assert_eq!(
        e,
        Error::Http(format!(
            "GRDF, connexion 1/5 (page d'accueil) : connexion refusée (127.0.0.1:{port})"
        ))
    );
    assert!(e.is_transient());
}

#[test]
fn unknown_pce_is_refused_before_any_reading() {
    let fake = FakeGrdf::start_with_pces(&[PCE, "22222222222222"]);
    let (mut grdf, _) = client_for(&fake, "11111111111111", PASSWORD);
    let e = grdf.daily_readings(FROM, TO).unwrap_err();
    assert_eq!(
        e,
        Error::Grdf(format!(
            "GRDF, liste des PCE : le PCE 11111111111111 n'est pas rattaché à ce compte GRDF \
             (PCE du compte : {PCE}, 22222222222222) ; vérifiez GRDF_PCE"
        ))
    );
    assert!(fake.counters().consumption.is_empty());
}

#[test]
fn rate_limit_waits_retry_after_then_succeeds() {
    let fake = FakeGrdf::start();
    let (mut grdf, sleeps) = client(&fake);
    // La liste des PCE passe ; la limite tombe sur les relevés.
    grdf.daily_readings(FROM, FROM).unwrap();
    fake.set_faults(Faults {
        rate_limit: 1,
        retry_after: Some("2".into()),
        ..Faults::default()
    });
    let readings = grdf.daily_readings(FROM, TO).unwrap();
    assert_eq!(readings.len(), 28);
    assert_eq!(secs(&sleeps), [2]);
    let c = fake.counters();
    assert_eq!((c.logins, c.consumption.len()), (1, 3));
}

#[test]
fn rate_limit_without_retry_after_uses_backoff() {
    let fake = FakeGrdf::start();
    fake.set_faults(Faults {
        rate_limit: 2,
        retry_after: Some("bientôt".into()),
        ..Faults::default()
    });
    let (mut grdf, sleeps) = client(&fake);
    grdf.daily_readings(FROM, TO).unwrap();
    assert_eq!(secs(&sleeps), [3, 6]);
}

#[test]
fn expired_session_relogs_once_and_repeats_the_call() {
    for expiry in [Expiry::Unauthorized, Expiry::LoginPage] {
        let fake = FakeGrdf::start();
        let (mut grdf, sleeps) = client(&fake);
        grdf.daily_readings(FROM, FROM).unwrap();
        fake.set_faults(Faults {
            expire_session: 1,
            expiry,
            ..Faults::default()
        });
        let readings = grdf.daily_readings(FROM, TO).unwrap();
        assert_eq!(readings.len(), 28, "{expiry:?}");
        let c = fake.counters();
        assert!(c.problems.is_empty(), "{:?}", c.problems);
        assert_eq!((c.start_pages, c.logins), (2, 2), "{expiry:?}");
        assert!(sleeps.borrow().is_empty());
    }
}

#[test]
fn session_expired_again_after_relogin_is_an_error() {
    let fake = FakeGrdf::start();
    let (mut grdf, _) = client(&fake);
    grdf.daily_readings(FROM, FROM).unwrap();
    fake.set_faults(Faults {
        expire_session: 10,
        ..Faults::default()
    });
    let e = grdf.daily_readings(FROM, TO).unwrap_err();
    assert!(
        matches!(&e, Error::GrdfLogin(m) if m.contains("de nouveau")),
        "{e:?}"
    );
    assert_eq!(fake.counters().logins, 2, "une seule reconnexion");
}

#[test]
fn html_error_pages_are_retried_with_backoff() {
    let fake = FakeGrdf::start();
    fake.set_faults(Faults {
        html_errors: 2,
        ..Faults::default()
    });
    let (mut grdf, sleeps) = client(&fake);
    assert_eq!(grdf.daily_readings(FROM, TO).unwrap().len(), 28);
    assert_eq!(secs(&sleeps), [3, 6]);
}

#[test]
fn persistent_html_errors_give_up_after_max_attempts() {
    let fake = FakeGrdf::start();
    let (mut grdf, sleeps) = client(&fake);
    grdf.daily_readings(FROM, FROM).unwrap();
    fake.set_faults(Faults {
        html_errors: 100,
        ..Faults::default()
    });
    let e = grdf.daily_readings(FROM, TO).unwrap_err();
    assert_eq!(
        e,
        Error::Grdf(format!(
            "GRDF, relevés du 2021-07-26 au 2021-08-22 : page HTML au lieu de JSON \
             (statut HTTP 500, {}) ; abandon après 4 tentatives",
            host_port(fake.base_url())
        ))
    );
    assert_eq!(secs(&sleeps), [3, 6, 12]);
    let calls = fake.counters().consumption.len() - 1;
    assert_eq!(calls, MAX_ATTEMPTS as usize);
}

#[test]
fn persistent_network_failure_names_the_call_the_target_and_the_attempts() {
    let fake = FakeGrdf::start();
    let (mut grdf, sleeps) = client(&fake);
    grdf.daily_readings(FROM, FROM).unwrap();
    fake.set_faults(Faults {
        drop_connections: 100,
        ..Faults::default()
    });
    let e = grdf.daily_readings(FROM, TO).unwrap_err();
    assert!(e.is_transient(), "{e:?}");
    let shown = e.to_string();
    assert!(
        shown.starts_with("GRDF, relevés du 2021-07-26 au 2021-08-22 : "),
        "{shown}"
    );
    assert!(
        shown.ends_with(&format!(
            " ({}) ; abandon après 4 tentatives",
            host_port(fake.base_url())
        )),
        "{shown}"
    );
    assert_eq!(secs(&sleeps), [3, 6, 12]);
}

#[test]
fn unexpected_status_names_the_call_and_the_target() {
    let fake = FakeGrdf::start();
    let (mut grdf, _) = client(&fake);
    grdf.daily_readings(FROM, FROM).unwrap();
    fake.set_faults(Faults {
        json_errors: 1,
        ..Faults::default()
    });
    let e = grdf.daily_readings(FROM, TO).unwrap_err();
    assert_eq!(
        e,
        Error::Grdf(format!(
            "GRDF, relevés du 2021-07-26 au 2021-08-22 : statut HTTP 500 ({})",
            host_port(fake.base_url())
        ))
    );
    assert!(!e.is_transient());
}

#[test]
fn dropped_connection_is_retried() {
    let fake = FakeGrdf::start();
    fake.set_faults(Faults {
        drop_connections: 1,
        ..Faults::default()
    });
    let (mut grdf, sleeps) = client(&fake);
    assert_eq!(grdf.daily_readings(FROM, TO).unwrap().len(), 28);
    assert_eq!(secs(&sleeps), [3]);
}

#[test]
fn three_years_are_split_into_windows_of_at_most_365_days() {
    let fake = FakeGrdf::start();
    let (mut grdf, _) = client(&fake);
    let (from, to) = (date(2019, 11, 30), date(2022, 11, 29));
    let readings = grdf.daily_readings(from, to).unwrap();
    assert_eq!(readings.len(), 28, "pas de doublon aux jointures");
    let windows = fake.counters().consumption;
    assert!((3..=4).contains(&windows.len()), "{windows:?}");
    assert_eq!(windows[0].0, "2019-11-30");
    assert_eq!(windows.last().unwrap().1, "2022-11-29");
    for (start, end) in &windows {
        let (start, end): (Date, Date) = (start.parse().unwrap(), end.parse().unwrap());
        let days = start.until(end).unwrap().get_days() + 1;
        assert!(days <= MAX_WINDOW_DAYS, "{start} → {end} : {days} jours");
    }
    for pair in windows.windows(2) {
        assert!(pair[1].0 <= pair[0].1, "trou entre {pair:?}");
    }
}

#[test]
fn empty_range_makes_no_request() {
    let fake = FakeGrdf::start();
    let (mut grdf, _) = client(&fake);
    assert!(grdf.daily_readings(TO, FROM).unwrap().is_empty());
    let c = fake.counters();
    assert_eq!((c.start_pages, c.pce_list), (0, 0));
}

// --- Hôtes autorisés (manuel §9 : redirections hors de grdf.fr refusées) ---

#[test]
fn redirect_to_another_host_is_refused_without_following_it() {
    let fake = FakeGrdf::start();
    // La page d'accueil renvoie vers la page de connexion sur « localhost ».
    fake.set_faults(Faults {
        foreign_redirect: true,
        ..Faults::default()
    });
    let (mut grdf, sleeps) = client(&fake);
    let e = grdf.daily_readings(FROM, TO).unwrap_err();
    assert_eq!(
        e,
        Error::Protocol(
            "GRDF, connexion 1/5 (page d'accueil) : redirection vers l'hôte localhost refusée : \
             seuls 127.0.0.1 sont autorisés"
                .into()
        )
    );
    let c = fake.counters();
    assert_eq!(c.foreign_hosts, 0, "rien n'a été envoyé à l'autre hôte");
    assert_eq!((c.start_pages, c.identify, c.logins), (0, 0, 0));
    assert!(sleeps.borrow().is_empty(), "un refus n'est pas réessayé");
}

#[test]
fn success_href_to_another_host_is_not_followed() {
    let fake = FakeGrdf::start();
    fake.set_faults(Faults {
        foreign_success_href: true,
        ..Faults::default()
    });
    let (mut grdf, sleeps) = client(&fake);
    let e = grdf.daily_readings(FROM, TO).unwrap_err();
    assert!(
        matches!(&e, Error::GrdfLogin(m) if m.contains("success.href mène à l'hôte localhost")),
        "{e}"
    );
    // La requête de success.href porte le stateHandle : jamais dans l'erreur.
    assert!(!format!("{e} {e:?}").contains(STATE_HANDLE));
    let c = fake.counters();
    assert_eq!(c.challenge, 1, "le mot de passe a été accepté");
    assert_eq!((c.foreign_hosts, c.logins), (0, 0));
    assert!(sleeps.borrow().is_empty());
}

// --- Aucun secret dans les journaux ni les erreurs ---

const LEAK_CHILD: &str = "HA_GAZPAR_TEST_LEAK_CHILD";

/// Scénarios exécutés dans un processus fils au niveau `debug` : le journal
/// est écrit sur la vraie sortie d'erreur, que le parent relit.
fn leak_scenarios() {
    ha_gazpar::log::init(ha_gazpar::log::Level::Debug);
    let show = |label: &str, e: &Error| eprintln!("ERREUR {label} : {e} / {e:?}");

    let fake = FakeGrdf::start();
    let (mut grdf, _) = client(&fake);
    grdf.daily_readings(FROM, TO).unwrap();
    fake.set_faults(Faults {
        rate_limit: 1,
        html_errors: 1,
        drop_connections: 1,
        expire_session: 1,
        expiry: Expiry::LoginPage,
        ..Faults::default()
    });
    grdf.daily_readings(FROM, TO).unwrap();
    fake.set_faults(Faults {
        expire_session: 10,
        ..Faults::default()
    });
    show("session", &grdf.daily_readings(FROM, TO).unwrap_err());
    fake.set_faults(Faults::default());

    let (mut wrong, _) = client_for(&fake, PCE, "mauvais-mot-de-passe");
    show("mot de passe", &wrong.daily_readings(FROM, TO).unwrap_err());
    let (mut unknown, _) = client_for(&fake, "11111111111111", PASSWORD);
    show("pce", &unknown.daily_readings(FROM, TO).unwrap_err());

    for faults in [
        Faults {
            foreign_redirect: true,
            ..Faults::default()
        },
        Faults {
            foreign_success_href: true,
            ..Faults::default()
        },
    ] {
        fake.set_faults(faults);
        let (mut grdf, _) = client(&fake);
        show("hôte", &grdf.daily_readings(FROM, TO).unwrap_err());
    }
}

#[test]
fn secrets_never_reach_logs_or_errors() {
    if std::env::var_os(LEAK_CHILD).is_some() {
        leak_scenarios();
        return;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "secrets_never_reach_logs_or_errors",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(LEAK_CHILD, "1")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    // Le journal a bien été capturé, au niveau debug, avec les erreurs.
    for expected in [
        " DEBUG GRDF, connexion 4/5 (mot de passe Okta)",
        " INFO connecté à Mon Espace GRDF",
        " ATTENTION GRDF, relevés du 2021-07-26 au 2021-08-22 : session GRDF expirée",
        " ATTENTION GRDF, relevés du 2021-07-26 au 2021-08-22 : débit limité par GRDF",
        "ERREUR mot de passe",
        "ERREUR session",
        "ERREUR pce",
        "ERREUR hôte : GRDF, connexion 1/5 (page d'accueil) : redirection vers l'hôte \
         localhost refusée",
        "ERREUR hôte : GRDF, connexion 5/5 (fin de connexion) : success.href mène à l'hôte \
         localhost",
        "ERREUR mot de passe : GRDF, connexion 4/5 (mot de passe Okta) : identifiants refusés",
    ] {
        assert!(text.contains(expected), "« {expected} » absent :\n{text}");
    }
    for secret in [
        PASSWORD,
        "mauvais-mot-de-passe",
        EMAIL,
        STATE_TOKEN,
        STATE_TOKEN_IN_PAGE,
        STATE_HANDLE,
        "FAKE-HANDLE",
        DEVICE_COOKIE_VALUE,
        SESSION_PREFIX,
        CODE_PREFIX,
    ] {
        assert!(!text.contains(secret), "secret « {secret} » dans :\n{text}");
    }
}

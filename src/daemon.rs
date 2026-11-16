//! Boucle du mode démon (`run`, MANUEL.md §3 et §9, PLAN.md §3).
//!
//! - un relevé tout de suite, puis un toutes les `interval` ;
//! - **une erreur n'avance jamais le relevé suivant** : après chaque relevé,
//!   réussi ou non, on attend l'intervalle complet ;
//! - toute erreur est journalisée (`ATTENTION` pour une panne de réseau,
//!   `ERREUR` sinon) et la boucle continue : seule une erreur de
//!   configuration, détectée avant la boucle, fait sortir le programme ;
//! - si GRDF refuse les identifiants ([`Error::GrdfCredentialsRejected`]),
//!   plus aucun relevé n'est tenté (donc plus aucune connexion à GRDF)
//!   jusqu'au redémarrage du processus ; une erreur est journalisée à chaque
//!   intervalle pour expliquer quoi faire. Le démon ne sort pas, pour ne pas
//!   être relancé en boucle par `restart: unless-stopped`.
//!
//! Testable sans attendre ni réseau : le relevé, le sommeil et le journal
//! passent par le trait [`Host`], et le nombre de tours peut être borné.
//!
//! Dépendances : `error`, `log` (niveaux), `sync` (type `Report`).

use std::time::Duration;

use crate::error::Error;
use crate::log::Level;
use crate::sync::Report;

/// Ce dont la boucle a besoin du reste du programme.
pub trait Host {
    /// Un relevé complet : nouvelle connexion à HA, nouveau client GRDF,
    /// import, journal du compte rendu.
    fn reading(&mut self) -> Result<Report, Error>;
    /// Attend (`std::thread::sleep` en production).
    fn sleep(&mut self, duration: Duration);
    /// Écrit une ligne de journal.
    fn log(&mut self, level: Level, message: &str);
}

/// Fait tourner le démon : `max_cycles` tours (relevé puis attente), ou
/// indéfiniment si `None` (production).
pub fn run(host: &mut dyn Host, interval: Duration, max_cycles: Option<u64>) {
    let minutes = interval.as_secs() / 60;
    // Motif du refus d'identifiants : une fois connu, plus aucun relevé.
    let mut rejected: Option<String> = None;
    let mut cycle = 0;
    while max_cycles.is_none_or(|max| cycle < max) {
        cycle += 1;
        match &rejected {
            Some(reason) => host.log(Level::Error, &still_rejected(reason)),
            None => match host.reading() {
                Ok(_) => host.log(Level::Debug, &format!("prochain relevé dans {minutes} min")),
                Err(Error::GrdfCredentialsRejected(reason)) => {
                    let e = Error::GrdfCredentialsRejected(reason.clone());
                    host.log(
                        Level::Error,
                        &format!(
                            "{e}. Plus aucune connexion à GRDF jusqu'au redémarrage de \
                             ha-gazpar : corrigez la configuration, puis redémarrez le service"
                        ),
                    );
                    rejected = Some(reason);
                }
                Err(e) => host.log(
                    severity(&e),
                    &format!("{e} ; nouvel essai au prochain relevé, dans {minutes} min"),
                ),
            },
        }
        host.sleep(interval);
    }
}

/// Message répété à chaque intervalle après un refus d'identifiants.
fn still_rejected(reason: &str) -> String {
    format!(
        "aucun relevé : GRDF a refusé les identifiants ({reason}) ; plus aucune connexion \
         à GRDF jusqu'au redémarrage de ha-gazpar. Corrigez GRDF_USERNAME ou GRDF_PASSWORD, \
         puis redémarrez le service"
    )
}

/// Niveau de journal d'une erreur de relevé : `ATTENTION` pour une panne de
/// réseau (GRDF ou Home Assistant injoignable, délai dépassé), `ERREUR`
/// pour un refus ou une réponse inattendue, qui demandent sans doute une
/// action.
pub fn severity(error: &Error) -> Level {
    match error {
        Error::Http(_) | Error::Io(_) => Level::Warn,
        _ => Level::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Mode;

    const INTERVAL: Duration = Duration::from_secs(480 * 60);

    /// Hôte scripté : une réponse par relevé, puis des succès.
    #[derive(Default)]
    struct Script {
        outcomes: Vec<Result<Report, Error>>,
        readings: usize,
        /// Événements dans l'ordre : `relevé`, `dort N s`, ou `NIVEAU message`.
        events: Vec<String>,
    }

    impl Script {
        fn new(outcomes: Vec<Result<Report, Error>>) -> Script {
            Script {
                outcomes,
                ..Script::default()
            }
        }

        fn logs(&self, level: Level) -> Vec<&str> {
            let prefix = format!("{level} ");
            self.events
                .iter()
                .filter_map(|e| e.strip_prefix(prefix.as_str()))
                .collect()
        }

        fn sleeps(&self) -> Vec<&str> {
            self.events
                .iter()
                .filter(|e| e.starts_with("dort"))
                .map(String::as_str)
                .collect()
        }
    }

    impl Host for Script {
        fn reading(&mut self) -> Result<Report, Error> {
            self.readings += 1;
            self.events.push("relevé".into());
            if self.outcomes.is_empty() {
                Ok(report())
            } else {
                self.outcomes.remove(0)
            }
        }

        fn sleep(&mut self, duration: Duration) {
            self.events.push(format!("dort {} s", duration.as_secs()));
        }

        fn log(&mut self, level: Level, message: &str) {
            self.events.push(format!("{level} {message}"));
        }
    }

    fn report() -> Report {
        Report {
            mode: Mode::Apply,
            requested: None,
            readings: Vec::new(),
            measures: Vec::new(),
        }
    }

    fn rejected() -> Error {
        Error::GrdfCredentialsRejected("mot de passe refusé par GRDF (statut HTTP 401)".into())
    }

    #[test]
    fn reading_at_once_then_every_interval() {
        let mut host = Script::default();
        run(&mut host, INTERVAL, Some(3));
        assert_eq!(
            host.events
                .iter()
                .filter(|e| !e.starts_with("debug"))
                .collect::<Vec<_>>(),
            [
                "relevé",
                "dort 28800 s",
                "relevé",
                "dort 28800 s",
                "relevé",
                "dort 28800 s"
            ]
        );
    }

    #[test]
    fn transient_error_then_success() {
        let mut host = Script::new(vec![
            Err(Error::Http(
                "GRDF injoignable (connexion impossible)".into(),
            )),
            Ok(report()),
        ]);
        run(&mut host, INTERVAL, Some(2));
        assert_eq!(host.readings, 2);
        assert_eq!(
            host.logs(Level::Warn),
            [
                "HTTP : GRDF injoignable (connexion impossible) ; nouvel essai au prochain \
              relevé, dans 480 min"
            ]
        );
        assert!(host.logs(Level::Error).is_empty());
    }

    #[test]
    fn an_error_never_brings_the_next_reading_forward() {
        let mut host = Script::new(vec![
            Err(Error::Http("délai dépassé".into())),
            Err(Error::Grdf("réponse inattendue".into())),
            Err(Error::Io("Home Assistant injoignable".into())),
        ]);
        run(&mut host, Duration::from_secs(3600), Some(3));
        assert_eq!(host.sleeps(), ["dort 3600 s"; 3]);
        // Chaque relevé est suivi d'une attente complète.
        let order: Vec<&str> = host
            .events
            .iter()
            .filter(|e| *e == "relevé" || e.starts_with("dort"))
            .map(String::as_str)
            .collect();
        assert_eq!(
            order,
            [
                "relevé",
                "dort 3600 s",
                "relevé",
                "dort 3600 s",
                "relevé",
                "dort 3600 s"
            ]
        );
    }

    #[test]
    fn rejected_credentials_stop_every_grdf_connection_but_not_the_daemon() {
        let mut host = Script::new(vec![Err(rejected())]);
        run(&mut host, INTERVAL, Some(4));
        assert_eq!(host.readings, 1, "plus aucun relevé après le refus");
        assert_eq!(host.sleeps(), ["dort 28800 s"; 4]);
        let errors = host.logs(Level::Error);
        assert_eq!(errors.len(), 4, "une erreur par intervalle : {errors:?}");
        assert!(
            errors[0].starts_with("connexion à GRDF refusée : mot de passe refusé"),
            "{}",
            errors[0]
        );
        assert!(
            errors[0].contains("jusqu'au redémarrage") && errors[0].contains("redémarrez"),
            "{}",
            errors[0]
        );
        for later in &errors[1..] {
            assert!(
                later.starts_with("aucun relevé : GRDF a refusé les identifiants"),
                "{later}"
            );
            assert!(later.contains("GRDF_PASSWORD"), "{later}");
            assert!(later.contains("redémarrez le service"), "{later}");
        }
    }

    #[test]
    fn home_assistant_error_is_not_fatal() {
        let mut host = Script::new(vec![
            Err(Error::HomeAssistant("authentification refusée".into())),
            Err(Error::Io(
                "Home Assistant injoignable (connexion impossible)".into(),
            )),
        ]);
        run(&mut host, INTERVAL, Some(3));
        assert_eq!(host.readings, 3);
        assert_eq!(host.logs(Level::Error).len(), 1);
        assert_eq!(host.logs(Level::Warn).len(), 1);
    }

    #[test]
    fn other_grdf_login_failures_keep_trying() {
        let mut host = Script::new(vec![
            Err(Error::GrdfLogin("page d'accueil : statut HTTP 503".into())),
            Err(Error::GrdfLogin("page d'accueil : statut HTTP 503".into())),
        ]);
        run(&mut host, INTERVAL, Some(3));
        assert_eq!(host.readings, 3);
    }

    #[test]
    fn severity_by_kind() {
        assert_eq!(severity(&Error::Http("x".into())), Level::Warn);
        assert_eq!(severity(&Error::Io("x".into())), Level::Warn);
        for e in [
            Error::HomeAssistant("x".into()),
            Error::Grdf("x".into()),
            Error::GrdfLogin("x".into()),
            Error::Protocol("x".into()),
            rejected(),
        ] {
            assert_eq!(severity(&e), Level::Error, "{e:?}");
        }
    }
}

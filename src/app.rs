//! Racine de composition : **seul endroit** qui construit les vrais clients
//! ([`HomeAssistantStore::connect`], [`GrdfClient::new`]) à partir de la
//! [`Config`], et qui appelle `sync`.
//!
//! - [`Connector`] ouvre les connexions : [`Network`] en production, une
//!   implémentation en mémoire dans les tests ([`fake`]) ;
//! - [`Session`] est une connexion à Home Assistant ouverte pour une
//!   commande : elle calcule la date du jour dans le fuseau de HA, lit les
//!   derniers jours connus, lance un relevé ([`crate::sync::run_once`]) ou
//!   efface les statistiques.
//!
//! Chaque session ouvre une nouvelle connexion WebSocket (une connexion
//! inactive 8 h serait coupée) et chaque relevé un nouveau [`GrdfClient`]
//! (connexion à GRDF neuve, paresseuse : rien n'est envoyé à GRDF avant
//! la première demande de relevés, donc jamais si Home Assistant est
//! injoignable).
//!
//! Ce module n'écrit rien lui-même : `cli` met en forme et journalise ce
//! qu'il rend. Les clients qu'il construit journalisent en revanche leurs
//! détails (reprises GRDF, session expirée, commandes de Home Assistant) par
//! les macros de [`crate::log`], directement sur la sortie d'erreur.
//!
//! Dépendances : `config`, `sync`, `ports`, `domain`, `grdf`, `ha`,
//! `secret`, `error`, `jiff`. `grdf` et `ha` ne se connaissent pas : ils ne
//! se rencontrent qu'ici.

use std::net::TcpStream;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::config::Config;
use crate::domain::{Measure, StatisticId};
use crate::error::Error;
use crate::grdf::api::{GrdfClient, GrdfSettings};
use crate::grdf::auth::Credentials;
use crate::ha::stats::HomeAssistantStore;
use crate::ha::websocket::{Timeouts, WebSocket};
use crate::ports::{ConsumptionSource, StatisticsStore};
use crate::secret::Secret;
use crate::sync::{self, Mode, Report, StatisticIds, SyncSettings};

/// Home Assistant vu par les commandes : les statistiques, plus ce que HA
/// dit de lui-même.
pub trait HomeAssistant: StatisticsStore {
    /// Version annoncée à l'authentification.
    fn version(&self) -> &str;
    /// Fuseau horaire de HA, celui des jours.
    fn time_zone(&self) -> &TimeZone;
    /// Ferme la connexion proprement.
    fn close(self: Box<Self>) -> Result<(), Error>;
}

impl HomeAssistant for HomeAssistantStore<WebSocket<TcpStream>> {
    fn version(&self) -> &str {
        self.ha_version()
    }

    fn time_zone(&self) -> &TimeZone {
        HomeAssistantStore::time_zone(self)
    }

    fn close(self: Box<Self>) -> Result<(), Error> {
        HomeAssistantStore::close(*self)
    }
}

/// Ouvre les connexions aux deux services.
pub trait Connector {
    /// Connexion authentifiée à Home Assistant, fuseau lu.
    fn home_assistant(&mut self, config: &Config) -> Result<Box<dyn HomeAssistant>, Error>;
    /// Client GRDF neuf. Ne contacte pas GRDF : la connexion se fait à la
    /// première demande de relevés.
    fn grdf(&mut self, config: &Config) -> Result<Box<dyn ConsumptionSource>, Error>;
}

/// Les vrais services, avec les délais réseau par défaut
/// ([`Timeouts::default`] pour HA, ceux de `grdf::http` pour GRDF).
pub struct Network;

impl Connector for Network {
    fn home_assistant(&mut self, config: &Config) -> Result<Box<dyn HomeAssistant>, Error> {
        let store =
            HomeAssistantStore::connect(&config.ha_url, &config.ha_token, Timeouts::default())?;
        Ok(Box::new(store))
    }

    fn grdf(&mut self, config: &Config) -> Result<Box<dyn ConsumptionSource>, Error> {
        let settings = GrdfSettings {
            base_url: config.grdf_base_url.clone(),
            login_url: config.grdf_login_url.clone(),
            credentials: Credentials {
                username: Secret::new(config.grdf_username.expose()),
                password: Secret::new(config.grdf_password.expose()),
            },
            pce: config.pce.clone(),
        };
        let client = GrdfClient::new(settings, Box::new(std::thread::sleep))?;
        Ok(Box::new(client))
    }
}

/// Ce que Home Assistant a dit de lui-même à la connexion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaInfo {
    pub version: String,
    /// Nom IANA du fuseau (`sans nom` si HA n'en donne pas).
    pub time_zone: String,
    /// HA est-il antérieur à [`crate::HA_REFERENCE`] ? `None` si sa
    /// version est illisible.
    pub older_than_reference: Option<bool>,
    /// Date du jour dans le fuseau de HA.
    pub today: Date,
}

/// Une connexion à Home Assistant ouverte pour une commande.
pub struct Session<'c> {
    config: &'c Config,
    ha: Box<dyn HomeAssistant>,
    info: HaInfo,
}

impl<'c> Session<'c> {
    /// Se connecte à Home Assistant et calcule la date du jour de l'instant
    /// `now` dans son fuseau.
    pub fn open(
        connector: &mut dyn Connector,
        config: &'c Config,
        now: Timestamp,
    ) -> Result<Session<'c>, Error> {
        let ha = connector.home_assistant(config)?;
        let tz = ha.time_zone().clone();
        let info = HaInfo {
            version: ha.version().to_owned(),
            time_zone: tz.iana_name().unwrap_or("sans nom").to_owned(),
            older_than_reference: crate::ha::older_than_reference(ha.version()),
            today: now.to_zoned_datetime(tz).date(),
        };
        Ok(Session { config, ha, info })
    }

    pub fn info(&self) -> &HaInfo {
        &self.info
    }

    /// Dernier jour connu dans HA de chaque statistique (sans GRDF).
    pub fn last_known_days(&mut self) -> Result<Vec<(StatisticId, Option<Date>)>, Error> {
        let ids = statistic_ids(self.config);
        let known = sync::last_known_days(
            self.ha.as_mut(),
            &ids,
            sync_settings(self.config),
            self.info.today,
        )?;
        Ok(known
            .into_iter()
            .map(|(measure, day)| (ids.get(measure).clone(), day))
            .collect())
    }

    /// Un relevé : nouveau client GRDF, puis [`sync::run_once`].
    pub fn sync(&mut self, connector: &mut dyn Connector, mode: Mode) -> Result<Report, Error> {
        let mut grdf = connector.grdf(self.config)?;
        sync::run_once(
            self.ha.as_mut(),
            grdf.as_mut(),
            &statistic_ids(self.config),
            sync_settings(self.config),
            self.info.today,
            mode,
        )
    }

    /// Efface les deux statistiques ; renvoie leurs identifiants.
    pub fn clear(&mut self) -> Result<Vec<StatisticId>, Error> {
        let ids: Vec<StatisticId> = Measure::ALL
            .iter()
            .map(|m| self.config.statistic_id(*m).clone())
            .collect();
        self.ha.clear(&ids)?;
        Ok(ids)
    }

    /// Ferme la connexion à HA. Un échec n'annule pas ce qui a été fait :
    /// à journaliser seulement.
    pub fn close(self) -> Result<(), Error> {
        self.ha.close()
    }
}

/// Réglages d'un relevé tirés de la configuration.
pub fn sync_settings(config: &Config) -> SyncSettings {
    SyncSettings {
        history_days: config.history_days,
        overlap_days: config.overlap_days,
    }
}

/// Identifiants des deux statistiques.
pub fn statistic_ids(config: &Config) -> StatisticIds {
    StatisticIds {
        energy: config.statistic_id(Measure::Energy).clone(),
        volume: config.statistic_id(Measure::Volume).clone(),
    }
}

/// Services en mémoire pour les tests de `cli` et de `app` : aucun réseau.
#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::domain::{DailyReading, StatPoint};

    type Points =
        std::collections::BTreeMap<StatisticId, std::collections::BTreeMap<Date, StatPoint>>;

    /// Ce que les faux services ont vu, partagé avec le test.
    #[derive(Debug, Default)]
    pub struct Seen {
        pub ha_connections: usize,
        pub ha_closes: usize,
        pub grdf_clients: usize,
        /// Appels à `daily_readings`.
        pub grdf_calls: Vec<(Date, Date)>,
        pub imports: usize,
        pub clears: Vec<Vec<StatisticId>>,
        pub points: Points,
    }

    /// Comportement des faux services.
    #[derive(Debug, Clone)]
    pub struct Script {
        pub ha_version: String,
        pub time_zone: String,
        /// Erreur à la connexion à HA.
        pub ha_error: Option<Error>,
        /// Erreur à la fermeture de HA.
        pub close_error: Option<Error>,
        /// Erreurs successives de GRDF (une par relevé), puis succès.
        pub grdf_errors: Vec<Error>,
        /// Relevés publiés par GRDF.
        pub published: Vec<DailyReading>,
    }

    impl Default for Script {
        fn default() -> Self {
            Script {
                ha_version: "2026.9.4".into(),
                time_zone: "Europe/Paris".into(),
                ha_error: None,
                close_error: None,
                grdf_errors: Vec::new(),
                published: Vec::new(),
            }
        }
    }

    #[derive(Default)]
    pub struct FakeConnector {
        pub script: Script,
        pub seen: Rc<RefCell<Seen>>,
    }

    impl FakeConnector {
        pub fn new(script: Script) -> FakeConnector {
            FakeConnector {
                script,
                seen: Rc::default(),
            }
        }
    }

    struct FakeHa {
        tz: TimeZone,
        version: String,
        close_error: Option<Error>,
        seen: Rc<RefCell<Seen>>,
    }

    impl StatisticsStore for FakeHa {
        fn daily_points(
            &mut self,
            id: &StatisticId,
            from: Date,
            to: Date,
        ) -> Result<Vec<StatPoint>, Error> {
            Ok(self
                .seen
                .borrow()
                .points
                .get(id)
                .map(|m| m.range(from..=to).map(|(_, p)| *p).collect())
                .unwrap_or_default())
        }

        fn import(
            &mut self,
            id: &StatisticId,
            _: Measure,
            points: &[StatPoint],
        ) -> Result<(), Error> {
            let mut seen = self.seen.borrow_mut();
            seen.imports += 1;
            let series = seen.points.entry(id.clone()).or_default();
            series.extend(points.iter().map(|p| (p.day, *p)));
            Ok(())
        }

        fn clear(&mut self, ids: &[StatisticId]) -> Result<(), Error> {
            let mut seen = self.seen.borrow_mut();
            for id in ids {
                seen.points.remove(id);
            }
            seen.clears.push(ids.to_vec());
            Ok(())
        }
    }

    impl HomeAssistant for FakeHa {
        fn version(&self) -> &str {
            &self.version
        }

        fn time_zone(&self) -> &TimeZone {
            &self.tz
        }

        fn close(self: Box<Self>) -> Result<(), Error> {
            self.seen.borrow_mut().ha_closes += 1;
            match self.close_error {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
    }

    struct FakeGrdf {
        published: Vec<DailyReading>,
        error: Option<Error>,
        seen: Rc<RefCell<Seen>>,
    }

    impl ConsumptionSource for FakeGrdf {
        fn daily_readings(&mut self, from: Date, to: Date) -> Result<Vec<DailyReading>, Error> {
            self.seen.borrow_mut().grdf_calls.push((from, to));
            if let Some(e) = self.error.take() {
                return Err(e);
            }
            Ok(self
                .published
                .iter()
                .filter(|r| (from..=to).contains(&r.day))
                .cloned()
                .collect())
        }
    }

    impl Connector for FakeConnector {
        fn home_assistant(&mut self, _: &Config) -> Result<Box<dyn HomeAssistant>, Error> {
            if let Some(e) = &self.script.ha_error {
                return Err(e.clone());
            }
            self.seen.borrow_mut().ha_connections += 1;
            Ok(Box::new(FakeHa {
                tz: TimeZone::get(&self.script.time_zone)
                    .map_err(|e| Error::Invalid(e.to_string()))?,
                version: self.script.ha_version.clone(),
                close_error: self.script.close_error.clone(),
                seen: Rc::clone(&self.seen),
            }))
        }

        fn grdf(&mut self, _: &Config) -> Result<Box<dyn ConsumptionSource>, Error> {
            self.seen.borrow_mut().grdf_clients += 1;
            let error =
                (!self.script.grdf_errors.is_empty()).then(|| self.script.grdf_errors.remove(0));
            Ok(Box::new(FakeGrdf {
                published: self.script.published.clone(),
                error,
                seen: Rc::clone(&self.seen),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeConnector, Script};
    use super::*;
    use crate::config::{self, CommandLine};
    use crate::domain::{DailyReading, Quantity};
    use std::collections::BTreeMap;

    fn test_config() -> Config {
        let env: BTreeMap<String, String> = [
            ("GRDF_USERNAME", "jean@example.org"),
            ("GRDF_PASSWORD", "mot-de-passe"),
            ("GRDF_PCE", "12345678901234"),
            ("HA_URL", "ws://127.0.0.1:1"),
            ("HA_TOKEN", "jeton"),
            ("GAZPAR_HISTORY_DAYS", "30"),
            ("GAZPAR_OVERLAP_DAYS", "5"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        struct NoFiles;
        impl config::FileSystem for NoFiles {
            fn read_to_string(&self, _: &str) -> std::io::Result<String> {
                Err(std::io::ErrorKind::NotFound.into())
            }
            fn mode(&self, _: &str) -> std::io::Result<Option<u32>> {
                Err(std::io::ErrorKind::NotFound.into())
            }
        }
        config::load(&CommandLine::default(), &env, &NoFiles)
            .config
            .unwrap()
    }

    /// Heure fixe des tests : le 16 novembre 2026 à midi (UTC).
    fn now() -> Timestamp {
        "2026-11-16T12:00:00Z".parse().unwrap()
    }

    fn reading(day: Date, kwh: i64) -> DailyReading {
        DailyReading {
            day,
            energy_kwh: Some(Quantity::from_millis(kwh * 1000)),
            volume_m3: Some(Quantity::from_millis(kwh * 100)),
        }
    }

    #[test]
    fn session_reports_what_home_assistant_says() {
        let config = test_config();
        let mut connector = FakeConnector::new(Script {
            ha_version: "2026.8.3".into(),
            time_zone: "America/New_York".into(),
            ..Script::default()
        });
        // 2 h du matin à Paris le 17, encore le 16 à New York.
        let now: Timestamp = "2026-11-17T01:00:00Z".parse().unwrap();
        let session = Session::open(&mut connector, &config, now).unwrap();
        let info = session.info().clone();
        assert_eq!(info.version, "2026.8.3");
        assert_eq!(info.time_zone, "America/New_York");
        assert_eq!(info.older_than_reference, Some(true));
        assert_eq!(info.today, jiff::civil::date(2026, 11, 16));
        session.close().unwrap();
        assert_eq!(connector.seen.borrow().ha_closes, 1);
    }

    #[test]
    fn sync_uses_the_configuration_and_a_new_grdf_client() {
        let config = test_config();
        let mut connector = FakeConnector::default();
        let mut session = Session::open(&mut connector, &config, now()).unwrap();
        let today = session.info().today;
        let yesterday = today.yesterday().unwrap();
        connector.script.published = vec![reading(yesterday, 7)];

        assert_eq!(session.last_known_days().unwrap()[0].1, None);
        let dry = session.sync(&mut connector, Mode::DryRun).unwrap();
        assert_eq!(dry.imported(), 2);
        assert_eq!(connector.seen.borrow().imports, 0);
        let applied = session.sync(&mut connector, Mode::Apply).unwrap();
        assert_eq!(applied.imported(), 2);
        let known = session.last_known_days().unwrap();
        assert_eq!(
            known,
            vec![
                (config.energy_statistic.clone(), Some(yesterday)),
                (config.volume_statistic.clone(), Some(yesterday)),
            ]
        );
        let seen = connector.seen.borrow();
        assert_eq!(seen.grdf_clients, 2, "un client GRDF par relevé");
        // Premier import : toute la profondeur d'historique configurée.
        assert_eq!(
            seen.grdf_calls[0],
            (
                today.checked_sub(jiff::Span::new().days(30)).unwrap(),
                yesterday
            )
        );
        assert_eq!(seen.imports, 2);
    }

    #[test]
    fn clear_erases_both_statistics() {
        let config = test_config();
        let mut connector = FakeConnector::default();
        let mut session = Session::open(&mut connector, &config, now()).unwrap();
        let ids = session.clear().unwrap();
        assert_eq!(
            ids,
            vec![
                config.energy_statistic.clone(),
                config.volume_statistic.clone()
            ]
        );
        assert_eq!(connector.seen.borrow().clears, vec![ids]);
        assert_eq!(connector.seen.borrow().grdf_clients, 0);
    }

    #[test]
    fn unreachable_home_assistant_is_named() {
        let mut config = test_config();
        // Port réservé, jamais en écoute pour un utilisateur ordinaire.
        config.ha_url = "ws://127.0.0.1:1/api/websocket".into();
        match Network.home_assistant(&config) {
            Err(Error::Io(m)) => assert!(m.starts_with("Home Assistant injoignable ("), "{m}"),
            Err(e) => panic!("{e}"),
            Ok(_) => panic!("connexion inattendue"),
        }
    }

    #[test]
    fn unreachable_grdf_is_named() {
        let mut config = test_config();
        config.grdf_base_url = "http://127.0.0.1:1".into();
        config.grdf_login_url = "http://127.0.0.1:1".into();
        let mut grdf = Network.grdf(&config).unwrap();
        let today = jiff::civil::date(2026, 11, 16);
        match grdf.daily_readings(today, today) {
            Err(e) => assert_eq!(
                e,
                Error::Http(
                    "GRDF, connexion 1/5 (page d'accueil) : connexion refusée (127.0.0.1:1)".into()
                )
            ),
            other => panic!("{other:?}"),
        }
    }
}

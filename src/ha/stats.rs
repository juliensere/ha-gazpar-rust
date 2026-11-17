//! Statistiques long terme de Home Assistant : [`HomeAssistantStore`]
//! implémente [`StatisticsStore`].
//!
//! - Lecture : `recorder/statistics_during_period`, `period: "day"`, types
//!   `sum` et `state`. HA renvoie `start` en millisecondes Unix (voir
//!   `timestamp_ms_to_date` dans gazpar2haws) ; une chaîne RFC 3339 est
//!   aussi acceptée. Un point sans `sum` ou `state` est ignoré (journalisé) :
//!   le jour paraît manquant et sera réimporté.
//! - Import : `recorder/import_statistics` par lots de [`IMPORT_BATCH_SIZE`]
//!   points ; `source` vient de [`StatisticId::source`].
//! - Effacement : `recorder/clear_statistics`.
//!
//! Seul module à convertir un jour civil en minuit dans un fuseau (et
//! inversement). Les jours de HA sont ceux de **son** fuseau (`period:
//! "day"`) : [`HomeAssistantStore::connect`] le lit par `get_config`
//! ([`super::server`]) ; il n'y a pas d'option de fuseau.
//!
//! Dépendances : `ha::session`, `ha::server`, `ha::websocket` (pour
//! [`HomeAssistantStore::connect`]), `domain`, `ports`, `secret`, `error`,
//! `log`, `serde_json`, `jiff`.

use std::fmt;
use std::net::TcpStream;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use crate::domain::{Measure, Quantity, StatPoint, StatisticId};
use crate::error::Error;
use crate::ha::server;
use crate::ha::session::{Session, Transport};
use crate::ha::websocket::{self, Timeouts, WebSocket};
use crate::ports::StatisticsStore;
use crate::secret::Secret;
use crate::url::Url;

/// Nombre maximal de points par commande `recorder/import_statistics`.
pub const IMPORT_BATCH_SIZE: usize = 500;

/// Opération sur Home Assistant, nommée dans les erreurs avec la cible :
/// « Home Assistant (192.168.0.10:8123), import de 9 points dans
/// gazpar:12345678901234_energy ». La connexion TCP et la poignée de main
/// sont nommées par [`websocket::connect`].
#[derive(Debug, Clone, Copy)]
enum Step<'a> {
    Auth,
    GetConfig,
    Read {
        id: &'a StatisticId,
        from: Date,
        to: Date,
    },
    Import {
        id: &'a StatisticId,
        count: usize,
    },
    Clear {
        ids: &'a [StatisticId],
    },
    Close,
}

impl fmt::Display for Step<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::Auth => f.write_str("authentification"),
            Step::GetConfig => f.write_str("lecture du fuseau horaire (get_config)"),
            Step::Read { id, from, to } => write!(f, "lecture de {id} du {from} au {to}"),
            Step::Import { id, count } => write!(f, "import de {count} points dans {id}"),
            Step::Clear { ids } => {
                let ids: Vec<&str> = ids.iter().map(StatisticId::as_str).collect();
                write!(f, "effacement de {}", ids.join(", "))
            }
            Step::Close => f.write_str("fermeture de la connexion"),
        }
    }
}

/// Contexte complet d'une erreur : service, cible, opération.
struct At<'a>(&'a str, Step<'a>);

impl fmt::Display for At<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Home Assistant ({}), {}", self.0, self.1)
    }
}

/// Statistiques de Home Assistant, lues et écrites par une session.
pub struct HomeAssistantStore<T: Transport = WebSocket<TcpStream>> {
    session: Session<T>,
    tz: TimeZone,
    /// Hôte et port de Home Assistant, nommés dans les erreurs.
    target: String,
}

impl HomeAssistantStore {
    /// Se connecte à `url` (`ws://hôte:port/api/websocket`), s'authentifie
    /// et lit le fuseau horaire de Home Assistant. Chaque erreur nomme
    /// l'étape (connexion TCP, poignée de main, authentification,
    /// `get_config`) et la cible.
    pub fn connect(url: &str, token: &Secret, timeouts: Timeouts) -> Result<Self, Error> {
        let ws = websocket::connect(url, timeouts)?;
        let target = Url::ws(url)?.host_port();
        let session =
            Session::authenticate(ws, token).map_err(|e| e.context(At(&target, Step::Auth)))?;
        debug!(
            "Home Assistant ({target}) : authentifié, version {}",
            session.ha_version()
        );
        HomeAssistantStore::open(session, target)
    }
}

impl<T: Transport> HomeAssistantStore<T> {
    /// Sur une session authentifiée auprès de `target` (hôte:port) : lit le
    /// fuseau de HA (`get_config`).
    pub(crate) fn open(mut session: Session<T>, target: String) -> Result<Self, Error> {
        let tz = server::fetch_time_zone(&mut session)
            .map_err(|e| e.context(At(&target, Step::GetConfig)))?;
        debug!(
            "fuseau horaire de Home Assistant : {}",
            tz.iana_name().unwrap_or("sans nom")
        );
        Ok(HomeAssistantStore::new(session, tz, target))
    }

    /// Sur une session authentifiée, avec un fuseau déjà lu.
    fn new(session: Session<T>, tz: TimeZone, target: String) -> Self {
        HomeAssistantStore {
            session,
            tz,
            target,
        }
    }

    /// Contexte d'une erreur de l'opération `step`, cible comprise.
    fn during(&self, step: Step<'_>) -> String {
        At(&self.target, step).to_string()
    }

    /// Fuseau des jours : celui de Home Assistant.
    pub fn time_zone(&self) -> &TimeZone {
        &self.tz
    }

    /// Version de Home Assistant annoncée à l'authentification.
    pub fn ha_version(&self) -> &str {
        self.session.ha_version()
    }

    /// Ferme la connexion proprement.
    pub fn close(self) -> Result<(), Error> {
        let during = self.during(Step::Close);
        self.session.close().map_err(|e| e.context(during))
    }
}

impl<T: Transport> StatisticsStore for HomeAssistantStore<T> {
    fn daily_points(
        &mut self,
        id: &StatisticId,
        from: Date,
        to: Date,
    ) -> Result<Vec<StatPoint>, Error> {
        if from > to {
            return Ok(Vec::new());
        }
        let during = self.during(Step::Read { id, from, to });
        let result = period_query(id, from, to, &self.tz)
            .and_then(|query| self.session.command(query))
            .and_then(|result| read_points(&result, id, from, to, &self.tz))
            .map_err(|e| e.context(during))?;
        debug!("{id} : {} points lus du {from} au {to}", result.len());
        Ok(result)
    }

    fn import(
        &mut self,
        id: &StatisticId,
        measure: Measure,
        points: &[StatPoint],
    ) -> Result<(), Error> {
        let during = self.during(Step::Import {
            id,
            count: points.len(),
        });
        import_commands(id, measure, points, &self.tz)
            .and_then(|commands| {
                commands
                    .into_iter()
                    .try_for_each(|command| self.session.command(command).map(drop))
            })
            .map_err(|e| e.context(during))?;
        debug!("{id} : {} points importés", points.len());
        Ok(())
    }

    fn clear(&mut self, ids: &[StatisticId]) -> Result<(), Error> {
        if ids.is_empty() {
            return Ok(());
        }
        let during = self.during(Step::Clear { ids });
        let names: Vec<&str> = ids.iter().map(StatisticId::as_str).collect();
        self.session
            .command(json!({
                "type": "recorder/clear_statistics",
                "statistic_ids": names,
            }))
            .map_err(|e| e.context(during))?;
        debug!("statistiques effacées : {}", names.join(", "));
        Ok(())
    }
}

/// Minuit du jour dans le fuseau, en RFC 3339 avec décalage
/// (`2026-03-29T00:00:00+01:00`), sans l'annotation `[Europe/Paris]` que
/// Home Assistant refuserait. Si minuit n'existe pas (passage à l'heure
/// d'été à minuit, hors France), l'heure qui suit le saut.
fn midnight(day: Date, tz: &TimeZone) -> Result<String, Error> {
    let start = day
        .to_zoned_datetime(tz.clone())
        .map_err(|e| Error::Invalid(format!("minuit du {day} : {e}")))?;
    Ok(start.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string())
}

/// Commande de lecture des points quotidiens de `from` à `to` inclus.
fn period_query(id: &StatisticId, from: Date, to: Date, tz: &TimeZone) -> Result<Value, Error> {
    let end = to
        .tomorrow()
        .map_err(|e| Error::Invalid(format!("lendemain du {to} : {e}")))?;
    Ok(json!({
        "type": "recorder/statistics_during_period",
        "start_time": midnight(from, tz)?,
        "end_time": midnight(end, tz)?,
        "statistic_ids": [id.as_str()],
        "period": "day",
        "types": ["sum", "state"],
    }))
}

/// Commandes d'import, une par lot ; aucune pour une liste vide.
fn import_commands(
    id: &StatisticId,
    measure: Measure,
    points: &[StatPoint],
    tz: &TimeZone,
) -> Result<Vec<Value>, Error> {
    let metadata = json!({
        "has_mean": false,
        "mean_type": 0,
        "has_sum": true,
        "statistic_id": id.as_str(),
        "source": id.source(),
        "name": measure.display_name(),
        "unit_class": measure.unit_class(),
        "unit_of_measurement": measure.unit(),
    });
    points
        .chunks(IMPORT_BATCH_SIZE)
        .map(|batch| {
            let stats = batch
                .iter()
                .map(|p| {
                    Ok(json!({
                        "start": midnight(p.day, tz)?,
                        "state": p.state.to_f64(),
                        "sum": p.sum.to_f64(),
                    }))
                })
                .collect::<Result<Vec<_>, Error>>()?;
            Ok(json!({
                "type": "recorder/import_statistics",
                "metadata": metadata,
                "stats": stats,
            }))
        })
        .collect()
}

/// Points d'une réponse à `statistics_during_period`, jours de `from` à `to`,
/// triés ; un jour en double (fuseaux différents) garde son premier point.
fn read_points(
    result: &Value,
    id: &StatisticId,
    from: Date,
    to: Date,
    tz: &TimeZone,
) -> Result<Vec<StatPoint>, Error> {
    let Some(by_id) = result.as_object() else {
        return Err(protocol("statistics_during_period : objet attendu"));
    };
    let rows = match by_id.get(id.as_str()) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(rows)) => rows,
        Some(_) => return Err(protocol("statistics_during_period : liste attendue")),
    };
    let mut points = Vec::with_capacity(rows.len());
    for row in rows {
        let day = start_day(&row["start"], tz)?;
        let (Some(state), Some(sum)) = (quantity(&row["state"])?, quantity(&row["sum"])?) else {
            warn!("{id} : point du {day} sans state ou sum, ignoré");
            continue;
        };
        if (from..=to).contains(&day) {
            points.push(StatPoint { day, state, sum });
        }
    }
    points.sort_by_key(|p| p.day);
    points.dedup_by_key(|p| p.day);
    Ok(points)
}

/// Jour civil, dans le fuseau, du début de période renvoyé par HA.
fn start_day(start: &Value, tz: &TimeZone) -> Result<Date, Error> {
    let timestamp = match start {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| {
                // Conversion saturante, bornée par le filtre.
                n.as_f64()
                    .filter(|f| f.is_finite() && f.abs() < 1e16)
                    .map(|f| f.round() as i64)
            })
            .and_then(|ms| Timestamp::from_millisecond(ms).ok()),
        Value::String(s) => s.parse::<Timestamp>().ok(),
        _ => None,
    };
    timestamp
        .map(|t| t.to_zoned_datetime(tz.clone()).date())
        .ok_or_else(|| protocol(format!("début de période illisible : {start}")))
}

/// Valeur numérique de HA en millièmes, arrondie au plus proche ; `None` si
/// elle est absente ou `null`.
fn quantity(value: &Value) -> Result<Option<Quantity>, Error> {
    match value {
        Value::Null => Ok(None),
        Value::Number(n) => Quantity::parse_decimal(&n.to_string()).map(Some),
        _ => Err(protocol(format!("nombre attendu, reçu {value}"))),
    }
}

fn protocol(message: impl Into<String>) -> Error {
    Error::Protocol(format!("réponse inattendue : {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::session::fake::{FakeTransport, ok};
    use jiff::civil::date;

    fn paris() -> TimeZone {
        TimeZone::get("Europe/Paris").unwrap()
    }

    fn energy() -> StatisticId {
        StatisticId::parse("gazpar:12345678901234_energy").unwrap()
    }

    fn point(day: Date, state: i64, sum: i64) -> StatPoint {
        StatPoint {
            day,
            state: Quantity::from_millis(state),
            sum: Quantity::from_millis(sum),
        }
    }

    fn store(
        respond: impl FnMut(&Value) -> Vec<Value> + 'static,
    ) -> HomeAssistantStore<FakeTransport> {
        let session =
            Session::authenticate(FakeTransport::ha(respond), &Secret::new("jeton")).unwrap();
        HomeAssistantStore::new(session, paris(), "ha.local:8123".into())
    }

    fn commands(store: &HomeAssistantStore<FakeTransport>) -> &[Value] {
        store.session.transport().commands()
    }

    // --- Minuit dans le fuseau ---

    #[test]
    fn paris_midnight_around_dst_changes() {
        let tz = paris();
        for (day, text) in [
            (date(2026, 3, 28), "2026-03-28T00:00:00+01:00"),
            (date(2026, 3, 29), "2026-03-29T00:00:00+01:00"),
            (date(2026, 3, 30), "2026-03-30T00:00:00+02:00"),
            (date(2026, 10, 25), "2026-10-25T00:00:00+02:00"),
            (date(2026, 10, 26), "2026-10-26T00:00:00+01:00"),
        ] {
            assert_eq!(midnight(day, &tz).unwrap(), text);
        }
    }

    #[test]
    fn midnight_in_other_zones() {
        let utc = TimeZone::UTC;
        assert_eq!(
            midnight(date(2026, 1, 1), &utc).unwrap(),
            "2026-01-01T00:00:00+00:00"
        );
        let ny = TimeZone::get("America/New_York").unwrap();
        assert_eq!(
            midnight(date(2026, 7, 1), &ny).unwrap(),
            "2026-07-01T00:00:00-04:00"
        );
    }

    // --- Forme exacte des messages ---

    #[test]
    fn period_query_shape() {
        let q = period_query(&energy(), date(2026, 3, 28), date(2026, 3, 29), &paris()).unwrap();
        assert_eq!(
            q,
            json!({
                "type": "recorder/statistics_during_period",
                "start_time": "2026-03-28T00:00:00+01:00",
                "end_time": "2026-03-30T00:00:00+02:00",
                "statistic_ids": ["gazpar:12345678901234_energy"],
                "period": "day",
                "types": ["sum", "state"],
            })
        );
    }

    #[test]
    fn import_command_shape_external_energy() {
        let points = [
            point(date(2026, 3, 29), 12_345, 100_000),
            point(date(2026, 3, 30), 1_000, 101_000),
        ];
        let commands = import_commands(&energy(), Measure::Energy, &points, &paris()).unwrap();
        assert_eq!(
            commands,
            [json!({
                "type": "recorder/import_statistics",
                "metadata": {
                    "has_mean": false,
                    "mean_type": 0,
                    "has_sum": true,
                    "statistic_id": "gazpar:12345678901234_energy",
                    "source": "gazpar",
                    "name": "Gaz (énergie)",
                    "unit_class": "energy",
                    "unit_of_measurement": "kWh",
                },
                "stats": [
                    { "start": "2026-03-29T00:00:00+01:00", "state": 12.345, "sum": 100.0 },
                    { "start": "2026-03-30T00:00:00+02:00", "state": 1.0, "sum": 101.0 },
                ],
            })]
        );
    }

    #[test]
    fn import_metadata_sensor_volume_uses_recorder_source() {
        let id = StatisticId::parse("sensor.gazpar2haws_volume").unwrap();
        let points = [point(date(2026, 1, 1), 1, 1)];
        let commands = import_commands(&id, Measure::Volume, &points, &paris()).unwrap();
        assert_eq!(
            commands[0]["metadata"],
            json!({
                "has_mean": false,
                "mean_type": 0,
                "has_sum": true,
                "statistic_id": "sensor.gazpar2haws_volume",
                "source": "recorder",
                "name": "Gaz (volume)",
                "unit_class": "volume",
                "unit_of_measurement": "m³",
            })
        );
        assert_eq!(commands[0]["stats"][0]["state"], json!(0.001));
    }

    #[test]
    fn import_is_split_into_batches() {
        let start = date(2023, 1, 1);
        let points: Vec<StatPoint> = (0..1_001)
            .map(|i| point(start.checked_add(jiff::Span::new().days(i)).unwrap(), i, i))
            .collect();
        let commands = import_commands(&energy(), Measure::Energy, &points, &paris()).unwrap();
        let sizes: Vec<usize> = commands
            .iter()
            .map(|c| c["stats"].as_array().unwrap().len())
            .collect();
        assert_eq!(sizes, [500, 500, 1]);
        assert_eq!(commands[2]["stats"][0]["sum"], json!(1.0));
        assert!(
            import_commands(&energy(), Measure::Energy, &[], &paris())
                .unwrap()
                .is_empty()
        );
    }

    // --- Lecture des réponses ---

    #[test]
    fn read_typical_response() {
        let result = json!({
            "gazpar:12345678901234_energy": [
                // Désordre volontaire ; champs en plus ignorés.
                { "start": 1774821600000_i64, "end": 1774908000000_i64,
                  "state": 30.5, "sum": 1030.5, "change": 30.5 },
                { "start": 1774652400000.0, "state": 10, "sum": 1000 },
                { "start": "2026-03-29T00:00:00+01:00", "state": 0.1234, "sum": 1000.0005 },
            ]
        });
        let points = read_points(
            &result,
            &energy(),
            date(2026, 3, 28),
            date(2026, 3, 30),
            &paris(),
        )
        .unwrap();
        assert_eq!(
            points,
            [
                point(date(2026, 3, 28), 10_000, 1_000_000),
                point(date(2026, 3, 29), 123, 1_000_001),
                point(date(2026, 3, 30), 30_500, 1_030_500),
            ]
        );
    }

    #[test]
    fn read_skips_incomplete_and_out_of_range_points() {
        let result = json!({
            "gazpar:12345678901234_energy": [
                { "start": 1774652400000_i64, "state": null, "sum": 1 },
                { "start": 1774738800000_i64, "state": 1 },
                { "start": 1774821600000_i64, "state": 2, "sum": 3 },
                { "start": 1774908000000_i64, "state": 4, "sum": 5 },
            ]
        });
        let points = read_points(
            &result,
            &energy(),
            date(2026, 3, 28),
            date(2026, 3, 30),
            &paris(),
        )
        .unwrap();
        assert_eq!(points, [point(date(2026, 3, 30), 2_000, 3_000)]);
    }

    #[test]
    fn read_missing_identifier_is_empty() {
        let (from, to) = (date(2026, 1, 1), date(2026, 1, 31));
        for result in [
            json!({}),
            json!({ "gazpar:autre_energy": [{ "start": 0, "state": 1, "sum": 1 }] }),
            json!({ "gazpar:12345678901234_energy": null }),
            json!({ "gazpar:12345678901234_energy": [] }),
        ] {
            assert!(
                read_points(&result, &energy(), from, to, &paris())
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn read_refuses_malformed_responses() {
        let (from, to) = (date(2026, 1, 1), date(2026, 1, 31));
        let id = "gazpar:12345678901234_energy";
        for result in [
            json!([]),
            json!({ id: {} }),
            json!({ id: [{ "state": 1, "sum": 1 }] }),
            json!({ id: [{ "start": "hier", "state": 1, "sum": 1 }] }),
            json!({ id: [{ "start": "2026-01-01T00:00:00", "state": 1, "sum": 1 }] }),
            json!({ id: [{ "start": true, "state": 1, "sum": 1 }] }),
            json!({ id: [{ "start": 1767222000000_i64, "state": "1", "sum": 1 }] }),
        ] {
            assert!(
                matches!(
                    read_points(&result, &energy(), from, to, &paris()),
                    Err(Error::Protocol(_))
                ),
                "{result}"
            );
        }
    }

    // --- Par la session ---

    #[test]
    fn open_reads_the_time_zone_and_uses_it_for_days() {
        let session = Session::authenticate(
            FakeTransport::ha(|msg| match msg["type"].as_str() {
                Some("get_config") => vec![ok(msg, json!({ "time_zone": "America/New_York" }))],
                _ => vec![ok(msg, Value::Null)],
            }),
            &Secret::new("jeton"),
        )
        .unwrap();
        let mut s = HomeAssistantStore::open(session, "ha.local:8123".into()).unwrap();
        assert_eq!(s.time_zone().iana_name(), Some("America/New_York"));
        assert_eq!(crate::ha::older_than_reference(s.ha_version()), Some(false));
        s.import(&energy(), Measure::Energy, &[point(date(2026, 7, 1), 1, 1)])
            .unwrap();
        let sent = commands(&s);
        assert_eq!(sent[0], json!({ "type": "get_config", "id": 1 }));
        assert_eq!(
            sent[1]["stats"][0]["start"], "2026-07-01T00:00:00-04:00",
            "minuit de New York, fuseau de HA"
        );
    }

    #[test]
    fn open_refuses_an_unknown_time_zone() {
        let session = Session::authenticate(
            FakeTransport::ha(|msg| vec![ok(msg, json!({ "time_zone": "Pas/Un_Fuseau" }))]),
            &Secret::new("jeton"),
        )
        .unwrap();
        match HomeAssistantStore::open(session, "ha.local:8123".into()) {
            Err(e) => assert_eq!(
                e,
                Error::HomeAssistant(
                    "Home Assistant (ha.local:8123), lecture du fuseau horaire (get_config) : \
                     fuseau horaire \"Pas/Un_Fuseau\" inconnu de ha-gazpar (réglage « Fuseau \
                     horaire » de Home Assistant) : choisissez un fuseau IANA, par exemple \
                     Europe/Paris"
                        .into()
                )
            ),
            Ok(_) => panic!("fuseau inconnu accepté"),
        }
    }

    #[test]
    fn daily_points_sends_query_and_reads_result() {
        let mut s = store(|msg| {
            vec![ok(
                msg,
                json!({ "gazpar:12345678901234_energy": [
                    { "start": 1774738800000_i64, "state": 1.5, "sum": 2.5 }
                ]}),
            )]
        });
        assert_eq!(s.ha_version(), "2026.9.1");
        let points = s
            .daily_points(&energy(), date(2026, 3, 29), date(2026, 3, 29))
            .unwrap();
        assert_eq!(points, [point(date(2026, 3, 29), 1_500, 2_500)]);
        let sent = commands(&s);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["type"], "recorder/statistics_during_period");
        assert_eq!(sent[0]["end_time"], "2026-03-30T00:00:00+02:00");
        assert_eq!(sent[0]["id"], 1);
    }

    #[test]
    fn empty_range_or_list_sends_nothing() {
        let mut s = store(|msg| vec![ok(msg, Value::Null)]);
        assert!(
            s.daily_points(&energy(), date(2026, 2, 1), date(2026, 1, 1))
                .unwrap()
                .is_empty()
        );
        s.import(&energy(), Measure::Energy, &[]).unwrap();
        s.clear(&[]).unwrap();
        assert!(commands(&s).is_empty());
    }

    #[test]
    fn import_sends_one_command_per_batch() {
        let mut s = store(|msg| vec![ok(msg, Value::Null)]);
        let start = date(2024, 1, 1);
        let points: Vec<StatPoint> = (0..1_200)
            .map(|i| point(start.checked_add(jiff::Span::new().days(i)).unwrap(), 1, i))
            .collect();
        s.import(&energy(), Measure::Energy, &points).unwrap();
        let sent = commands(&s);
        assert_eq!(sent.len(), 3);
        assert!(
            sent.iter()
                .all(|c| c["type"] == "recorder/import_statistics")
        );
        let ids: Vec<_> = sent.iter().map(|c| c["id"].clone()).collect();
        assert_eq!(ids, [json!(1), json!(2), json!(3)]);
    }

    #[test]
    fn import_error_stops_and_propagates() {
        let mut s = store(|msg| {
            vec![json!({ "type": "result", "id": msg["id"], "success": false,
                         "error": { "code": "home_assistant_error", "message": "refus" } })]
        });
        let points: Vec<StatPoint> = (0..600)
            .map(|i| {
                point(
                    date(2024, 1, 1)
                        .checked_add(jiff::Span::new().days(i))
                        .unwrap(),
                    1,
                    i,
                )
            })
            .collect();
        let e = s.import(&energy(), Measure::Energy, &points).unwrap_err();
        assert!(
            matches!(&e, Error::HomeAssistant(m) if m.contains("refus")),
            "{e}"
        );
        assert_eq!(commands(&s).len(), 1, "le second lot n'est pas envoyé");
    }

    #[test]
    fn clear_message_shape() {
        let mut s = store(|msg| vec![ok(msg, Value::Null)]);
        let volume = StatisticId::parse("gazpar:12345678901234_volume").unwrap();
        s.clear(&[energy(), volume]).unwrap();
        assert_eq!(
            commands(&s),
            [json!({
                "type": "recorder/clear_statistics",
                "statistic_ids": ["gazpar:12345678901234_energy", "gazpar:12345678901234_volume"],
                "id": 1,
            })]
        );
        s.close().unwrap();
    }
}

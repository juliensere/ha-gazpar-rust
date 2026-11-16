//! Essais contre un vrai Home Assistant (PLAN.md §8, étape 2), lancés par
//! `make test-ha` sur le conteneur jetable de `scripts/ha-test.sh`.
//!
//! Ignorés par défaut ; avec `--ignored`, chaque essai se saute (message sur
//! la sortie d'erreur) si `HA_TEST_URL` ou `HA_TEST_TOKEN_FILE` manque.
//! `HA_TEST_TAG`, facultatif, est l'étiquette de l'image : si c'est une
//! version (`2026.9`), la version annoncée par HA doit en commencer.
//!
//! Identifiants jetables (`gazpar:test_<aléa>_…`, `sensor.gazpar_test_<aléa>_…`),
//! effacés à la fin de chaque essai, même en cas d'échec.
//!
//! Le recorder de HA écrit en file d'attente : `import_statistics` et
//! `clear_statistics` répondent `success` avant l'écriture. Les relectures
//! sont donc répétées jusqu'au résultat attendu, avec un délai maximal.
//!
//! Observé sur HA 2026.9.4 (2026-11-16) :
//! - une relecture juste après un import ne voit jamais les nouveaux points
//!   (30 essais sur 30), ils sont là 100 ms plus tard ; un remplacement est
//!   visible tantôt aussitôt, tantôt 100 ms après ; un effacement l'a
//!   toujours été aussitôt, sans garantie ;
//! - `start` et `end` de `statistics_during_period` sont des entiers
//!   (millisecondes Unix) ;
//! - `period: "day"` regroupe les heures selon le fuseau de HA ;
//! - une statistique `sensor.…` en source `recorder` est acceptée sans
//!   entité correspondante, de même que `m³` avec `unit_class: volume`.

// Fichier de test : un échec doit faire paniquer le test.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::hash::{BuildHasher, RandomState};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ha_gazpar::domain::{Measure, Quantity, StatPoint, StatisticId};
use ha_gazpar::ha::session::Session;
use ha_gazpar::ha::stats::HomeAssistantStore;
use ha_gazpar::ha::websocket::{self, Timeouts, WebSocket};
use ha_gazpar::ports::StatisticsStore;
use ha_gazpar::secret::Secret;
use jiff::Timestamp;
use jiff::civil::{Date, date};
use jiff::tz::TimeZone;
use serde_json::{Value, json};

/// Délai maximal d'attente d'une écriture du recorder.
const WRITE_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

struct Env {
    url: String,
    token: Secret,
    tag: Option<String>,
}

/// Paramètres de l'essai, ou `None` (essai sauté) s'ils manquent.
fn env() -> Option<Env> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let (Some(url), Some(token_file)) = (var("HA_TEST_URL"), var("HA_TEST_TOKEN_FILE")) else {
        eprintln!("essai sauté : HA_TEST_URL et HA_TEST_TOKEN_FILE requis (make test-ha)");
        return None;
    };
    let token = std::fs::read_to_string(&token_file)
        .unwrap_or_else(|e| panic!("lecture de {token_file} : {e}"));
    Some(Env {
        url,
        token: Secret::new(token.trim()),
        tag: var("HA_TEST_TAG"),
    })
}

fn timeouts() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(5),
        read: Duration::from_secs(30),
        write: Duration::from_secs(10),
    }
}

fn paris() -> TimeZone {
    TimeZone::get("Europe/Paris").unwrap()
}

fn store(env: &Env) -> HomeAssistantStore {
    HomeAssistantStore::connect(&env.url, &env.token, paris(), timeouts()).unwrap()
}

/// Session brute, pour observer les réponses de HA telles quelles.
fn session(env: &Env) -> Session<WebSocket<TcpStream>> {
    let ws = websocket::connect(&env.url, timeouts()).unwrap();
    Session::authenticate(ws, &env.token).unwrap()
}

/// Suffixe aléatoire en hexadécimal minuscule, différent à chaque appel.
fn random_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:016x}", RandomState::new().hash_one(n))
}

/// Identifiants jetables d'une statistique externe et d'une statistique
/// d'entité ; effacés quand le garde est détruit.
struct Scratch<'a> {
    env: &'a Env,
    ids: Vec<StatisticId>,
}

impl<'a> Scratch<'a> {
    fn new(env: &'a Env) -> Self {
        Scratch {
            env,
            ids: Vec::new(),
        }
    }

    fn external(&mut self, measure: Measure) -> StatisticId {
        self.add(format!("gazpar:test_{}_{}", random_suffix(), measure.key()))
    }

    fn entity(&mut self, measure: Measure) -> StatisticId {
        self.add(format!(
            "sensor.gazpar_test_{}_{}",
            random_suffix(),
            measure.key()
        ))
    }

    fn add(&mut self, id: String) -> StatisticId {
        let id = StatisticId::parse(&id).unwrap();
        self.ids.push(id.clone());
        id
    }
}

impl Drop for Scratch<'_> {
    fn drop(&mut self) {
        // Au mieux : une erreur ici ne doit pas masquer celle de l'essai.
        let cleared =
            HomeAssistantStore::connect(&self.env.url, &self.env.token, paris(), timeouts())
                .and_then(|mut s| {
                    s.clear(&self.ids)?;
                    s.close()
                });
        if let Err(e) = cleared {
            eprintln!("nettoyage de {:?} impossible : {e}", self.ids);
        }
    }
}

/// `count` jours consécutifs à partir de `first`, avec cumul croissant.
fn points(first: Date, count: i64) -> Vec<StatPoint> {
    let mut sum = Quantity::from_millis(1_000_000);
    (0..count)
        .map(|i| {
            let state = Quantity::from_millis(10_500 + 1_037 * i);
            sum = sum + state;
            StatPoint {
                day: first.checked_add(jiff::Span::new().days(i)).unwrap(),
                state,
                sum,
            }
        })
        .collect()
}

/// Relit `id` de `from` à `to` jusqu'à obtenir `expected` (délai maximal
/// [`WRITE_TIMEOUT`]) ; rapporte la première lecture et l'attente.
#[track_caller]
fn read_until(
    store: &mut HomeAssistantStore,
    id: &StatisticId,
    from: Date,
    to: Date,
    expected: &[StatPoint],
) {
    let begin = Instant::now();
    let mut first = None;
    let mut attempts = 0;
    loop {
        attempts += 1;
        let got = store.daily_points(id, from, to).unwrap();
        let first = first.get_or_insert(got.len());
        if got == expected {
            eprintln!(
                "{id} : {} points attendus, {first} à la relecture immédiate, \
                 obtenus après {attempts} lecture(s) en {} ms",
                expected.len(),
                begin.elapsed().as_millis()
            );
            return;
        }
        if begin.elapsed() > WRITE_TIMEOUT {
            panic!(
                "{id} : après {attempts} lectures en {WRITE_TIMEOUT:?}, \
                 obtenu {got:?}, attendu {expected:?}"
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Métadonnées enregistrées par HA pour `id` (`null` si absent).
fn metadata(env: &Env, id: &StatisticId) -> Value {
    let mut s = session(env);
    let result = s
        .command(json!({
            "type": "recorder/get_statistics_metadata",
            "statistic_ids": [id.as_str()],
        }))
        .unwrap();
    s.close().unwrap();
    let list = result.as_array().expect("liste de métadonnées");
    list.iter()
        .find(|m| m["statistic_id"] == id.as_str())
        .cloned()
        .unwrap_or(Value::Null)
}

/// Importe dix points à partir de `first`, les relit, puis vérifie les
/// métadonnées enregistrées par HA.
fn import_ten_and_read_back(env: &Env, id: &StatisticId, measure: Measure, first: Date) {
    let mut store = store(env);
    let ten = points(first, 10);
    let last = ten[9].day;
    assert!(store.daily_points(id, first, last).unwrap().is_empty());
    store.import(id, measure, &ten).unwrap();
    read_until(&mut store, id, first, last, &ten);
    // Sous-intervalle : bornes en minuit de Paris.
    assert_eq!(
        store.daily_points(id, ten[4].day, ten[5].day).unwrap(),
        ten[4..6]
    );
    store.close().unwrap();

    let meta = metadata(env, id);
    eprintln!("{id} : métadonnées {meta}");
    assert_eq!(meta["source"], id.source(), "{meta}");
    assert_eq!(meta["unit_class"], measure.unit_class(), "{meta}");
    assert_eq!(
        meta["statistics_unit_of_measurement"],
        measure.unit(),
        "{meta}"
    );
    assert_eq!(meta["has_sum"], true, "{meta}");
    assert_eq!(meta["mean_type"], 0, "{meta}");
    assert_eq!(meta["name"], measure.display_name(), "{meta}");
}

// --- Essais ---

#[test]
#[ignore = "Home Assistant réel requis : make test-ha"]
fn version_is_read_at_authentication() {
    let Some(env) = env() else { return };
    let store = store(&env);
    let version = store.ha_version().to_owned();
    store.close().unwrap();
    eprintln!("Home Assistant {version}");
    let parts: Vec<&str> = version.split('.').collect();
    assert!(
        parts.len() >= 3 && parts[..2].iter().all(|p| p.parse::<u32>().is_ok()),
        "version inattendue : {version}"
    );
    if let Some(tag) = env
        .tag
        .filter(|t| t.starts_with(|c: char| c.is_ascii_digit()))
    {
        assert!(
            version.starts_with(&format!("{tag}.")) || version == tag,
            "version {version} pour l'image {tag}"
        );
    }
}

#[test]
#[ignore = "Home Assistant réel requis : make test-ha"]
fn external_energy_around_spring_dst() {
    let Some(env) = env() else { return };
    let mut scratch = Scratch::new(&env);
    let id = scratch.external(Measure::Energy);
    // Passage à l'heure d'été le 29 mars 2026.
    import_ten_and_read_back(&env, &id, Measure::Energy, date(2026, 3, 25));
}

#[test]
#[ignore = "Home Assistant réel requis : make test-ha"]
fn entity_volume_with_recorder_source_around_autumn_dst() {
    let Some(env) = env() else { return };
    let mut scratch = Scratch::new(&env);
    // Aucune entité sensor.gazpar_test_… n'existe dans ce HA.
    let id = scratch.entity(Measure::Volume);
    assert_eq!(id.source(), "recorder");
    // Retour à l'heure d'hiver le 25 octobre 2026.
    import_ten_and_read_back(&env, &id, Measure::Volume, date(2026, 10, 21));
}

#[test]
#[ignore = "Home Assistant réel requis : make test-ha"]
fn external_volume_in_cubic_metres() {
    let Some(env) = env() else { return };
    let mut scratch = Scratch::new(&env);
    let id = scratch.external(Measure::Volume);
    import_ten_and_read_back(&env, &id, Measure::Volume, date(2026, 1, 10));
}

#[test]
#[ignore = "Home Assistant réel requis : make test-ha"]
fn import_on_same_day_replaces_point() {
    let Some(env) = env() else { return };
    let mut scratch = Scratch::new(&env);
    let id = scratch.external(Measure::Energy);
    let mut store = store(&env);
    let ten = points(date(2026, 3, 25), 10);
    let (first, last) = (ten[0].day, ten[9].day);
    store.import(&id, Measure::Energy, &ten).unwrap();
    read_until(&mut store, &id, first, last, &ten);

    let mut changed = ten[5];
    changed.state = Quantity::from_millis(99_999);
    changed.sum = Quantity::from_millis(5_000_001);
    store.import(&id, Measure::Energy, &[changed]).unwrap();
    let mut expected = ten.clone();
    expected[5] = changed;
    read_until(&mut store, &id, first, last, &expected);
    store.close().unwrap();
}

#[test]
#[ignore = "Home Assistant réel requis : make test-ha"]
fn clear_removes_points_and_metadata() {
    let Some(env) = env() else { return };
    let mut scratch = Scratch::new(&env);
    let energy = scratch.external(Measure::Energy);
    let volume = scratch.external(Measure::Volume);
    let mut store = store(&env);
    let ten = points(date(2026, 2, 1), 10);
    let (first, last) = (ten[0].day, ten[9].day);
    for (id, measure) in [(&energy, Measure::Energy), (&volume, Measure::Volume)] {
        store.import(id, measure, &ten).unwrap();
        read_until(&mut store, id, first, last, &ten);
    }

    store.clear(&[energy.clone(), volume.clone()]).unwrap();
    for id in [&energy, &volume] {
        read_until(&mut store, id, first, last, &[]);
        assert_eq!(metadata(&env, id), Value::Null, "{id}");
    }
    store.close().unwrap();
}

/// Millisecondes Unix d'un instant RFC 3339.
fn ms(instant: &str) -> i64 {
    instant.parse::<Timestamp>().unwrap().as_millisecond()
}

/// Relit par la session brute jusqu'à ce que `id` ait `rows` lignes.
fn raw_rows(env: &Env, query: &Value, id: &StatisticId, rows: usize) -> Vec<Value> {
    let mut s = session(env);
    let begin = Instant::now();
    loop {
        let result = s.command(query.clone()).unwrap();
        let got = result[id.as_str()].as_array().cloned().unwrap_or_default();
        if got.len() >= rows || begin.elapsed() > WRITE_TIMEOUT {
            s.close().unwrap();
            return got;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn raw_import(env: &Env, id: &StatisticId, stats: Value) {
    let mut s = session(env);
    s.command(json!({
        "type": "recorder/import_statistics",
        "metadata": {
            "has_mean": false,
            "mean_type": 0,
            "has_sum": true,
            "statistic_id": id.as_str(),
            "source": id.source(),
            "name": "Essai",
            "unit_class": "energy",
            "unit_of_measurement": "kWh",
        },
        "stats": stats,
    }))
    .unwrap();
    s.close().unwrap();
}

#[test]
#[ignore = "Home Assistant réel requis : make test-ha"]
fn day_period_start_is_unix_milliseconds_at_paris_midnight() {
    let Some(env) = env() else { return };
    let mut scratch = Scratch::new(&env);
    let id = scratch.external(Measure::Energy);
    raw_import(
        &env,
        &id,
        json!([{ "start": "2026-01-15T00:00:00+01:00", "state": 1.5, "sum": 10.25 }]),
    );
    let query = json!({
        "type": "recorder/statistics_during_period",
        "start_time": "2026-01-15T00:00:00+01:00",
        "end_time": "2026-01-16T00:00:00+01:00",
        "statistic_ids": [id.as_str()],
        "period": "day",
        "types": ["sum", "state"],
    });
    let rows = raw_rows(&env, &query, &id, 1);
    eprintln!("réponse brute de statistics_during_period : {rows:?}");
    assert_eq!(rows.len(), 1, "{rows:?}");
    let start = &rows[0]["start"];
    assert!(
        start.is_i64() || start.is_u64(),
        "start entier attendu : {start}"
    );
    assert_eq!(start.as_i64(), Some(ms("2026-01-14T23:00:00Z")), "{start}");
    assert_eq!(rows[0]["sum"], json!(10.25));
    assert_eq!(rows[0]["state"], json!(1.5));
}

#[test]
#[ignore = "Home Assistant réel requis : make test-ha"]
fn day_period_groups_hours_in_ha_time_zone() {
    let Some(env) = env() else { return };
    let mut scratch = Scratch::new(&env);
    let id = scratch.external(Measure::Energy);
    // Deux heures du même jour à Paris (29 mars), mais de deux jours UTC
    // différents : 00:00 CET = 28 mars 23:00Z, 03:00 CEST = 29 mars 01:00Z.
    raw_import(
        &env,
        &id,
        json!([
            { "start": "2026-03-28T23:00:00Z", "state": 1.0, "sum": 101.0 },
            { "start": "2026-03-29T01:00:00Z", "state": 2.0, "sum": 103.0 },
        ]),
    );
    let query = json!({
        "type": "recorder/statistics_during_period",
        "start_time": "2026-03-28T00:00:00Z",
        "end_time": "2026-03-30T00:00:00Z",
        "statistic_ids": [id.as_str()],
        "period": "day",
        "types": ["sum", "state"],
    });
    // Attente de l'écriture par une lecture horaire (deux lignes).
    let mut hourly = query.clone();
    hourly["period"] = json!("hour");
    let hours = raw_rows(&env, &hourly, &id, 2);
    assert_eq!(hours.len(), 2, "{hours:?}");

    let days = raw_rows(&env, &query, &id, 1);
    eprintln!("regroupement par jour : {days:?}");
    // Regroupé en UTC, il y aurait deux lignes (28 et 29 mars à 00:00Z).
    assert_eq!(days.len(), 1, "{days:?}");
    assert_eq!(days[0]["start"].as_i64(), Some(ms("2026-03-28T23:00:00Z")));
    assert_eq!(days[0]["sum"], json!(103.0));
    assert_eq!(days[0]["state"], json!(2.0));
}

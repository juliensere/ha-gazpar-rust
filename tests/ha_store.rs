//! Essais de bout en bout du client Home Assistant (WebSocket, session,
//! statistiques) contre le faux serveur de `tests/support/fake_ha.rs`, sur
//! 127.0.0.1 uniquement.

// Fichier de test : un échec doit faire paniquer le test.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::time::Duration;

use ha_gazpar::Error;
use ha_gazpar::domain::{Measure, Quantity, StatPoint, StatisticId};
use ha_gazpar::ha::stats::{HomeAssistantStore, IMPORT_BATCH_SIZE};
use ha_gazpar::ha::websocket::Timeouts;
use ha_gazpar::ports::StatisticsStore;
use ha_gazpar::secret::Secret;
use jiff::civil::{Date, date};
use jiff::tz::TimeZone;
use support::fake_ha::{FakeHa, VERSION};

const TOKEN: &str = "jeton-longue-duree-de-test";

fn connect(ha: &FakeHa, token: &str) -> Result<HomeAssistantStore, Error> {
    let timeouts = Timeouts {
        connect: Duration::from_secs(2),
        read: Duration::from_secs(5),
        write: Duration::from_secs(5),
    };
    let paris = TimeZone::get("Europe/Paris").unwrap();
    HomeAssistantStore::connect(&ha.url(), &Secret::new(token), paris, timeouts)
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

fn energy() -> StatisticId {
    StatisticId::parse("gazpar:12345678901234_energy").unwrap()
}

#[test]
fn import_read_replace_clear() {
    let ha = FakeHa::start(TOKEN);
    let mut store = connect(&ha, TOKEN).unwrap();
    assert_eq!(store.ha_version(), VERSION);
    let id = energy();
    // Dix jours autour du passage à l'heure d'été (29 mars 2026).
    let (from, to) = (date(2026, 3, 25), date(2026, 4, 3));
    let ten = points(from, 10);
    assert!(store.daily_points(&id, from, to).unwrap().is_empty());

    store.import(&id, Measure::Energy, &ten).unwrap();
    assert_eq!(store.daily_points(&id, from, to).unwrap(), ten);
    assert_eq!(
        store
            .daily_points(&id, date(2026, 3, 29), date(2026, 3, 30))
            .unwrap(),
        ten[4..6]
    );

    // Un import au même jour remplace le point.
    let mut changed = ten[5];
    changed.state = Quantity::from_millis(99_999);
    changed.sum = Quantity::from_millis(5_000_001);
    store.import(&id, Measure::Energy, &[changed]).unwrap();
    let mut expected = ten.clone();
    expected[5] = changed;
    assert_eq!(store.daily_points(&id, from, to).unwrap(), expected);
    {
        let state = ha.state();
        assert_eq!(state.series[id.as_str()].len(), 10);
        let metadata = &state.metadata[id.as_str()];
        assert_eq!(metadata["source"], "gazpar");
        assert_eq!(metadata["unit_class"], "energy");
        assert_eq!(metadata["unit_of_measurement"], "kWh");
        assert_eq!(metadata["mean_type"], 0);
    }

    store.clear(std::slice::from_ref(&id)).unwrap();
    assert!(store.daily_points(&id, from, to).unwrap().is_empty());
    store.close().unwrap();
}

#[test]
fn large_import_is_batched_and_read_back() {
    let ha = FakeHa::start(TOKEN);
    let mut store = connect(&ha, TOKEN).unwrap();
    let id = energy();
    let (from, count) = (date(2023, 1, 1), 1_200);
    let all = points(from, count);
    store.import(&id, Measure::Energy, &all).unwrap();
    assert_eq!(
        ha.state().count("recorder/import_statistics"),
        1_200_usize.div_ceil(IMPORT_BATCH_SIZE)
    );
    // Réponse de plus de 64 Kio : trame à longueur sur 64 bits.
    let last = all.last().unwrap().day;
    assert_eq!(store.daily_points(&id, from, last).unwrap(), all);
    store.close().unwrap();
}

#[test]
fn entity_statistic_uses_recorder_source() {
    let ha = FakeHa::start(TOKEN);
    let mut store = connect(&ha, TOKEN).unwrap();
    let id = StatisticId::parse("sensor.gazpar2haws_volume").unwrap();
    let three = points(date(2026, 10, 24), 3);
    store.import(&id, Measure::Volume, &three).unwrap();
    assert_eq!(
        store
            .daily_points(&id, date(2026, 10, 24), date(2026, 10, 26))
            .unwrap(),
        three
    );
    let state = ha.state();
    assert_eq!(state.metadata[id.as_str()]["source"], "recorder");
    assert_eq!(state.metadata[id.as_str()]["unit_of_measurement"], "m³");
}

#[test]
fn wrong_token_is_refused_without_leaking_it() {
    let ha = FakeHa::start(TOKEN);
    let bad = "mauvais-jeton-secret";
    match connect(&ha, bad) {
        Err(e) => {
            assert!(
                matches!(&e, Error::HomeAssistant(m) if m.contains("Invalid access token")),
                "{e}"
            );
            let shown = format!("{e} {e:?}");
            assert!(!shown.contains(bad), "{shown}");
        }
        Ok(_) => panic!("mauvais jeton accepté"),
    }
    assert_eq!(ha.state().rejected_auths, 1);
    // Le serveur reste disponible : le bon jeton passe ensuite.
    connect(&ha, TOKEN).unwrap().close().unwrap();
}

#[test]
fn unreachable_home_assistant() {
    let ha = FakeHa::start(TOKEN);
    let url = ha.url();
    drop(ha);
    let timeouts = Timeouts::default();
    let paris = TimeZone::get("Europe/Paris").unwrap();
    match HomeAssistantStore::connect(&url, &Secret::new(TOKEN), paris, timeouts) {
        Err(Error::Io(m)) => assert!(m.contains("impossible"), "{m}"),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("connexion inattendue"),
    }
}

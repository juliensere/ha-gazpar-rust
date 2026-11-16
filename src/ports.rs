//! Interfaces aux frontières du programme (« ports ») : d'où viennent les
//! relevés ([`ConsumptionSource`], implémentée par `grdf`) et où vont les
//! statistiques ([`StatisticsStore`], implémentée par `ha`).
//!
//! `sync` ne connaît que ces traits : il se teste avec des implémentations en
//! mémoire. Il n'y a pas de trait d'horloge : `sync` reçoit la date du jour
//! en paramètre.
//!
//! Les jours sont des dates civiles ; la conversion en minuit dans le fuseau
//! configuré appartient à l'implémentation de `ha/`.
//!
//! Dépendances : `domain` et `error` uniquement.

use jiff::civil::Date;

use crate::domain::{DailyReading, Measure, StatPoint, StatisticId};
use crate::error::Error;

/// Source des relevés de consommation (GRDF).
pub trait ConsumptionSource {
    /// Relevés quotidiens de `from` à `to` inclus, triés, un par jour présent chez GRDF.
    ///
    /// Un jour absent chez GRDF n'apparaît pas ; un jour présent dont une
    /// valeur manque a cette valeur à `None`.
    fn daily_readings(&mut self, from: Date, to: Date) -> Result<Vec<DailyReading>, Error>;
}

/// Destination des statistiques long terme (Home Assistant).
pub trait StatisticsStore {
    /// Points déjà présents dans HA pour `id`, jours de `from` à `to` inclus, triés.
    fn daily_points(
        &mut self,
        id: &StatisticId,
        from: Date,
        to: Date,
    ) -> Result<Vec<StatPoint>, Error>;

    /// Importe (ou remplace, à jour égal) des points.
    fn import(
        &mut self,
        id: &StatisticId,
        measure: Measure,
        points: &[StatPoint],
    ) -> Result<(), Error>;

    /// Efface toutes les statistiques de ces identifiants.
    fn clear(&mut self, ids: &[StatisticId]) -> Result<(), Error>;
}

#[cfg(test)]
mod tests {
    //! Vérifie que les traits s'implémentent simplement en mémoire, comme le
    //! feront les tests de `sync`.

    use super::*;
    use crate::domain::Quantity;
    use jiff::civil::date;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct MemoryStore {
        points: BTreeMap<StatisticId, BTreeMap<Date, StatPoint>>,
    }

    impl StatisticsStore for MemoryStore {
        fn daily_points(
            &mut self,
            id: &StatisticId,
            from: Date,
            to: Date,
        ) -> Result<Vec<StatPoint>, Error> {
            Ok(self
                .points
                .get(id)
                .map(|m| m.range(from..=to).map(|(_, p)| *p).collect())
                .unwrap_or_default())
        }

        fn import(
            &mut self,
            id: &StatisticId,
            _measure: Measure,
            points: &[StatPoint],
        ) -> Result<(), Error> {
            let series = self.points.entry(id.clone()).or_default();
            for p in points {
                series.insert(p.day, *p);
            }
            Ok(())
        }

        fn clear(&mut self, ids: &[StatisticId]) -> Result<(), Error> {
            for id in ids {
                self.points.remove(id);
            }
            Ok(())
        }
    }

    struct FixedSource(Vec<DailyReading>);

    impl ConsumptionSource for FixedSource {
        fn daily_readings(&mut self, from: Date, to: Date) -> Result<Vec<DailyReading>, Error> {
            Ok(self
                .0
                .iter()
                .filter(|r| (from..=to).contains(&r.day))
                .cloned()
                .collect())
        }
    }

    fn point(day: Date, state: i64, sum: i64) -> StatPoint {
        StatPoint {
            day,
            state: Quantity::from_millis(state),
            sum: Quantity::from_millis(sum),
        }
    }

    #[test]
    fn memory_store_replaces_same_day_and_clears() {
        let id = StatisticId::parse("gazpar:1_energy").unwrap();
        let mut store: Box<dyn StatisticsStore> = Box::new(MemoryStore::default());
        let (d1, d2) = (date(2026, 1, 1), date(2026, 1, 2));
        store
            .import(&id, Measure::Energy, &[point(d1, 1, 1), point(d2, 2, 3)])
            .unwrap();
        store
            .import(&id, Measure::Energy, &[point(d2, 5, 6)])
            .unwrap();
        assert_eq!(
            store.daily_points(&id, d1, d2).unwrap(),
            vec![point(d1, 1, 1), point(d2, 5, 6)]
        );
        store.clear(std::slice::from_ref(&id)).unwrap();
        assert!(store.daily_points(&id, d1, d2).unwrap().is_empty());
    }

    #[test]
    fn source_bounds_are_inclusive() {
        let reading = |day| DailyReading {
            day,
            energy_kwh: None,
            volume_m3: None,
        };
        let mut src: Box<dyn ConsumptionSource> = Box::new(FixedSource(vec![
            reading(date(2026, 1, 1)),
            reading(date(2026, 1, 2)),
            reading(date(2026, 1, 3)),
        ]));
        let got = src
            .daily_readings(date(2026, 1, 2), date(2026, 1, 3))
            .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].day, date(2026, 1, 2));
    }
}

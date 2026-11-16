//! Algorithme d'un relevé (PLAN.md §3, manuel §1).
//!
//! Deux parties :
//!
//! - [`plan`] : calcul pur, des données en entrée, des données en sortie ;
//! - [`run_once`] : orchestration sur les traits de [`crate::ports`].
//!
//! Règles du calcul :
//!
//! - la valeur quotidienne de référence d'un jour déjà présent dans Home
//!   Assistant est la **différence de `sum`** avec le point précédent ; le
//!   champ `state` n'est jamais lu (celui des statistiques reprises de
//!   gazpar2haws est un cumul) ;
//! - une valeur publiée par GRDF l'emporte ; un jour sans valeur chez GRDF
//!   (`None`) ne change pas la valeur de Home Assistant et ne crée aucun
//!   point ; un jour absent des deux côtés reste absent (jamais zéro) ;
//! - le cumul repart de la `sum` du dernier point strictement antérieur au
//!   premier jour recalculé (0 sans un tel point) ; chaque point vaut
//!   `state = sum = cumul`, comme un compteur ;
//! - seuls les points qui diffèrent de l'existant sont importés : deux
//!   relevés identiques successifs n'écrivent rien.
//!
//! Home Assistant écrit de façon asynchrone (PLAN.md §2.3) : on ne relit
//! jamais juste après avoir écrit. Le relevé suivant relit, et répare ce
//! qu'un relevé interrompu aurait laissé (voir [`run_once`]).
//!
//! Aucune entrée-sortie : ni réseau, ni horloge, ni environnement, ni
//! journal. La date du jour est reçue en paramètre ; GRDF et HA sont
//! atteints uniquement par les traits de `ports`, ce qui permet de tester
//! avec des implémentations en mémoire. C'est `app` qui appelle
//! [`run_once`] et `cli` qui journalise le [`Report`].
//!
//! Dépendances : `domain`, `ports`, `error` (et `jiff` pour les dates).

use std::collections::BTreeMap;

use jiff::Span;
use jiff::civil::Date;

use crate::domain::{DailyReading, Measure, Quantity, StatPoint, StatisticId};
use crate::error::Error;
use crate::ports::{ConsumptionSource, StatisticsStore};

// --- Calcul pur ---

/// Points à importer pour une mesure, à partir de `from` : ceux dont la
/// valeur recalculée diffère de `existing`, triés par jour.
///
/// - `existing` : points de Home Assistant pour cette mesure (dans
///   n'importe quel ordre ; un jour en double garde son premier point). Ils
///   doivent inclure le dernier point antérieur à `from`, qui donne la base
///   du cumul.
/// - `readings` : relevés GRDF ; ceux d'avant `from` sont ignorés.
///
/// Sans point antérieur à `from`, la base vaut 0 et le premier point
/// existant garde sa `sum` comme valeur du jour : la série existante est
/// conservée telle quelle, sauf si GRDF publie une valeur pour ce jour-là.
pub fn plan(
    existing: &[StatPoint],
    readings: &[DailyReading],
    measure: Measure,
    from: Date,
) -> Vec<StatPoint> {
    let mut by_day: BTreeMap<Date, StatPoint> = BTreeMap::new();
    for point in existing {
        by_day.entry(point.day).or_insert(*point);
    }

    // 1. Valeurs quotidiennes de Home Assistant : différences de `sum`.
    let mut base = Quantity::ZERO;
    let mut previous_sum = Quantity::ZERO;
    let mut daily: BTreeMap<Date, Quantity> = BTreeMap::new();
    for (&day, point) in &by_day {
        if day < from {
            base = point.sum;
        } else {
            daily.insert(day, point.sum - previous_sum);
        }
        previous_sum = point.sum;
    }

    // 2. Les valeurs publiées par GRDF l'emportent ; `None` ne change rien.
    for reading in readings.iter().filter(|r| r.day >= from) {
        if let Some(value) = reading.get(measure) {
            daily.insert(reading.day, value);
        }
    }

    // 3. Cumul jour par jour ; seuls les points modifiés sont gardés.
    let mut cumul = base;
    let mut changed = Vec::new();
    for (day, value) in daily {
        cumul = cumul + value;
        let point = StatPoint {
            day,
            state: cumul,
            sum: cumul,
        };
        if by_day.get(&day) != Some(&point) {
            changed.push(point);
        }
    }
    changed
}

// --- Orchestration ---

/// Réglages d'un relevé (issus de la configuration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncSettings {
    /// Profondeur du premier import, en jours (`GAZPAR_HISTORY_DAYS`).
    pub history_days: u32,
    /// Jours déjà importés redemandés à chaque relevé (`GAZPAR_OVERLAP_DAYS`).
    pub overlap_days: u32,
}

/// Identifiants des deux statistiques.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatisticIds {
    pub energy: StatisticId,
    pub volume: StatisticId,
}

impl StatisticIds {
    pub fn get(&self, measure: Measure) -> &StatisticId {
        match measure {
            Measure::Energy => &self.energy,
            Measure::Volume => &self.volume,
        }
    }
}

/// Appliquer le relevé, ou seulement le simuler (`check`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Importe les points modifiés dans Home Assistant.
    Apply,
    /// N'écrit rien : le rapport dit ce qui serait importé.
    DryRun,
}

/// Ce qu'un relevé a importé (ou importerait, en simulation) pour une mesure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasureReport {
    pub measure: Measure,
    pub id: StatisticId,
    /// Points importés (ou qui le seraient), triés par jour.
    pub points: Vec<StatPoint>,
    /// Dernier jour présent dans Home Assistant après le relevé (points
    /// existants et importés) ; `None` s'il n'y en a aucun.
    pub last_known: Option<Date>,
}

impl MeasureReport {
    /// Nombre de points importés.
    pub fn count(&self) -> usize {
        self.points.len()
    }

    /// Premier jour modifié.
    pub fn first_changed(&self) -> Option<Date> {
        self.points.first().map(|p| p.day)
    }

    /// Dernier jour modifié.
    pub fn last_changed(&self) -> Option<Date> {
        self.points.last().map(|p| p.day)
    }
}

/// Compte rendu d'un relevé, à journaliser par `cli`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub mode: Mode,
    /// Période demandée à GRDF (bornes comprises) ; `None` si rien n'a été
    /// demandé (tout est déjà connu).
    pub requested: Option<(Date, Date)>,
    /// Relevés reçus de GRDF, triés (pour `check`).
    pub readings: Vec<DailyReading>,
    /// Une entrée par mesure, dans l'ordre de [`Measure::ALL`].
    pub measures: Vec<MeasureReport>,
}

impl Report {
    /// Nombre total de points importés, toutes mesures confondues.
    pub fn imported(&self) -> usize {
        self.measures.iter().map(MeasureReport::count).sum()
    }
}

/// Un relevé complet.
///
/// 1. Lit dans HA les points des deux mesures sur
///    `[today - max(history_days, overlap_days) - 1, today]` : le jour de
///    plus donne la base du cumul quand le calcul part du début de la
///    fenêtre.
/// 2. Choisit `from`, le minimum sur les deux mesures de
///    `min(dernier jour connu + 1, today - overlap_days)`, ou
///    `today - history_days` pour une mesure sans aucun point ; `to` est
///    la veille de `today`.
/// 3. Si `from <= to`, demande à GRDF les relevés de `from` à `to` (un seul
///    appel pour les deux mesures), calcule [`plan`] par mesure, puis
///    importe les points modifiés (sauf en [`Mode::DryRun`]). Sinon, rien
///    n'est demandé ni écrit.
///
/// `today` est la date du jour dans le fuseau de Home Assistant.
///
/// **En cas d'erreur**, rien n'est écrit tant que les lectures et l'appel à
/// GRDF n'ont pas tous réussi : les deux calculs sont faits avant le premier
/// import. Les imports se font ensuite mesure par mesure (énergie puis
/// volume) ; si celui du volume échoue après celui de l'énergie, l'erreur
/// est renvoyée et l'énergie reste écrite. Chaque statistique reste
/// cohérente en elle-même, et le relevé suivant répare : le volume a un
/// dernier jour connu plus ancien (ou aucun point), donc `from` recule
/// d'autant, et l'énergie, déjà à jour, n'est pas réécrite.
///
/// Limite : un import de plus de 500 points est envoyé en plusieurs
/// commandes (voir `ha::stats`) ; si une commande échoue au milieu, les
/// points des commandes suivantes ne sont pas écrits. C'est sans
/// conséquence au premier import (le dernier jour connu recule), mais une
/// correction de plus de 500 jours interrompue laisserait un saut de cumul
/// hors de la fenêtre de rattrapage.
pub fn run_once(
    store: &mut dyn StatisticsStore,
    source: &mut dyn ConsumptionSource,
    ids: &StatisticIds,
    settings: SyncSettings,
    today: Date,
    mode: Mode,
) -> Result<Report, Error> {
    let existing = read_existing(store, ids, settings, today)?;

    let history_start = days_before(today, settings.history_days)?;
    let overlap_start = days_before(today, settings.overlap_days)?;
    let mut from = overlap_start;
    for points in &existing {
        let candidate = match last_day(points) {
            Some(last) => next_day(last)?.min(overlap_start),
            None => history_start,
        };
        from = from.min(candidate);
    }
    let to = days_before(today, 1)?;

    let (requested, readings) = if from <= to {
        (Some((from, to)), source.daily_readings(from, to)?)
    } else {
        (None, Vec::new())
    };

    let mut measures = Vec::with_capacity(Measure::ALL.len());
    for (measure, points) in Measure::ALL.into_iter().zip(&existing) {
        let changed = if requested.is_some() {
            plan(points, &readings, measure, from)
        } else {
            Vec::new()
        };
        let last_known = last_day(points).max(changed.last().map(|p| p.day));
        measures.push(MeasureReport {
            measure,
            id: ids.get(measure).clone(),
            points: changed,
            last_known,
        });
    }

    if mode == Mode::Apply {
        for report in measures.iter().filter(|m| !m.points.is_empty()) {
            store.import(&report.id, report.measure, &report.points)?;
        }
    }

    Ok(Report {
        mode,
        requested,
        readings,
        measures,
    })
}

/// Dernier jour présent dans Home Assistant pour chaque mesure, dans
/// l'ordre de [`Measure::ALL`], sur la période que lit [`run_once`] (au-delà,
/// `None`). Pour `check`, avant tout appel à GRDF.
pub fn last_known_days(
    store: &mut dyn StatisticsStore,
    ids: &StatisticIds,
    settings: SyncSettings,
    today: Date,
) -> Result<Vec<(Measure, Option<Date>)>, Error> {
    let existing = read_existing(store, ids, settings, today)?;
    Ok(Measure::ALL
        .into_iter()
        .zip(existing.iter().map(|points| last_day(points)))
        .collect())
}

/// Points de HA des deux mesures sur
/// `[today - max(history_days, overlap_days) - 1, today]` (étape 1 de
/// [`run_once`]), dans l'ordre de [`Measure::ALL`].
fn read_existing(
    store: &mut dyn StatisticsStore,
    ids: &StatisticIds,
    settings: SyncSettings,
    today: Date,
) -> Result<Vec<Vec<StatPoint>>, Error> {
    let depth = settings.history_days.max(settings.overlap_days);
    let read_start = days_before(today, depth.saturating_add(1))?;
    let mut existing = Vec::with_capacity(Measure::ALL.len());
    for measure in Measure::ALL {
        existing.push(store.daily_points(ids.get(measure), read_start, today)?);
    }
    Ok(existing)
}

fn last_day(points: &[StatPoint]) -> Option<Date> {
    points.iter().map(|p| p.day).max()
}

fn days_before(day: Date, days: u32) -> Result<Date, Error> {
    day.checked_sub(Span::new().days(i64::from(days)))
        .map_err(|e| Error::Invalid(format!("{days} jours avant le {day} : {e}")))
}

fn next_day(day: Date) -> Result<Date, Error> {
    day.tomorrow()
        .map_err(|e| Error::Invalid(format!("lendemain du {day} : {e}")))
}

#[cfg(test)]
mod tests {
    //! Chaque test raconte un relevé : ce que Home Assistant contient, ce
    //! que GRDF publie, ce qui est importé. Quantités entières pour la
    //! lisibilité (`kwh(12)` = 12 unités), en novembre 2026 sauf mention.

    use super::*;
    use jiff::civil::date;

    // --- Fabriques ---

    fn kwh(n: i64) -> Quantity {
        Quantity::from_millis(n * 1000)
    }

    fn day(d: i8) -> Date {
        date(2026, 11, d)
    }

    /// Point de compteur : `state = sum`.
    fn point_on(day: Date, sum: i64) -> StatPoint {
        StatPoint {
            day,
            state: kwh(sum),
            sum: kwh(sum),
        }
    }

    fn counter(d: i8, sum: i64) -> StatPoint {
        point_on(day(d), sum)
    }

    /// Relevé GRDF : énergie et volume (`None` = pas encore publié).
    fn reading(d: i8, energy: Option<i64>, volume: Option<i64>) -> DailyReading {
        reading_on(day(d), energy, volume)
    }

    fn reading_on(day: Date, energy: Option<i64>, volume: Option<i64>) -> DailyReading {
        DailyReading {
            day,
            energy_kwh: energy.map(kwh),
            volume_m3: volume.map(kwh),
        }
    }

    /// Relevé GRDF avec la même valeur en énergie et en volume.
    fn both(d: i8, value: i64) -> DailyReading {
        reading(d, Some(value), Some(value))
    }

    /// Jours de `first` à `last` inclus.
    fn days(first: Date, last: Date) -> Vec<Date> {
        first
            .series(Span::new().days(1))
            .take_while(|d| *d <= last)
            .collect()
    }

    fn ids() -> StatisticIds {
        StatisticIds {
            energy: StatisticId::parse("gazpar:12345678901234_energy").unwrap(),
            volume: StatisticId::parse("gazpar:12345678901234_volume").unwrap(),
        }
    }

    const SETTINGS: SyncSettings = SyncSettings {
        history_days: 30,
        overlap_days: 3,
    };

    // --- Faux Home Assistant et faux GRDF ---

    /// Statistiques en mémoire ; l'import remplace le point de même jour,
    /// comme HA. Peut refuser les lectures ou le n-ième import.
    #[derive(Default)]
    struct MemoryStore {
        points: BTreeMap<StatisticId, BTreeMap<Date, StatPoint>>,
        /// Lectures demandées : `(id, from, to)`.
        reads: Vec<(StatisticId, Date, Date)>,
        /// Imports reçus, dans l'ordre (refusés compris).
        imports: Vec<(StatisticId, Measure, Vec<StatPoint>)>,
        fail_reads: bool,
        /// Numéro (à partir de 1) de l'import à refuser.
        fail_import: Option<usize>,
    }

    impl MemoryStore {
        fn with(ids: &StatisticIds, energy: &[StatPoint], volume: &[StatPoint]) -> Self {
            let mut store = MemoryStore::default();
            for (id, points) in [(&ids.energy, energy), (&ids.volume, volume)] {
                if !points.is_empty() {
                    let series = store.points.entry(id.clone()).or_default();
                    series.extend(points.iter().map(|p| (p.day, *p)));
                }
            }
            store
        }

        fn series(&self, id: &StatisticId) -> Vec<StatPoint> {
            self.points
                .get(id)
                .map(|m| m.values().copied().collect())
                .unwrap_or_default()
        }
    }

    impl StatisticsStore for MemoryStore {
        fn daily_points(
            &mut self,
            id: &StatisticId,
            from: Date,
            to: Date,
        ) -> Result<Vec<StatPoint>, Error> {
            self.reads.push((id.clone(), from, to));
            if self.fail_reads {
                return Err(Error::HomeAssistant("connexion fermée".into()));
            }
            Ok(self
                .points
                .get(id)
                .map(|m| m.range(from..=to).map(|(_, p)| *p).collect())
                .unwrap_or_default())
        }

        fn import(
            &mut self,
            id: &StatisticId,
            measure: Measure,
            points: &[StatPoint],
        ) -> Result<(), Error> {
            self.imports.push((id.clone(), measure, points.to_vec()));
            if self.fail_import == Some(self.imports.len()) {
                return Err(Error::HomeAssistant("import refusé".into()));
            }
            let series = self.points.entry(id.clone()).or_default();
            series.extend(points.iter().map(|p| (p.day, *p)));
            Ok(())
        }

        fn clear(&mut self, ids: &[StatisticId]) -> Result<(), Error> {
            for id in ids {
                self.points.remove(id);
            }
            Ok(())
        }
    }

    /// GRDF en mémoire : relevés publiés, appels enregistrés, panne possible.
    #[derive(Default)]
    struct FakeSource {
        published: Vec<DailyReading>,
        calls: Vec<(Date, Date)>,
        fail: bool,
    }

    impl FakeSource {
        fn publishing(published: Vec<DailyReading>) -> Self {
            FakeSource {
                published,
                ..FakeSource::default()
            }
        }
    }

    impl ConsumptionSource for FakeSource {
        fn daily_readings(&mut self, from: Date, to: Date) -> Result<Vec<DailyReading>, Error> {
            self.calls.push((from, to));
            if self.fail {
                return Err(Error::Http("GRDF injoignable".into()));
            }
            Ok(self
                .published
                .iter()
                .filter(|r| (from..=to).contains(&r.day))
                .cloned()
                .collect())
        }
    }

    fn run(
        store: &mut MemoryStore,
        source: &mut FakeSource,
        today: Date,
        mode: Mode,
    ) -> Result<Report, Error> {
        run_once(store, source, &ids(), SETTINGS, today, mode)
    }

    fn run_with(
        store: &mut MemoryStore,
        source: &mut FakeSource,
        settings: SyncSettings,
        today: Date,
    ) -> Result<Report, Error> {
        run_once(store, source, &ids(), settings, today, Mode::Apply)
    }

    fn energy_report(report: &Report) -> &MeasureReport {
        &report.measures[0]
    }

    fn volume_report(report: &Report) -> &MeasureReport {
        &report.measures[1]
    }

    // --- plan : calcul pur ---

    #[test]
    fn plan_first_import_accumulates_from_zero() {
        let readings = [both(1, 10), both(2, 12), both(3, 8)];
        assert_eq!(
            plan(&[], &readings, Measure::Energy, day(1)),
            [counter(1, 10), counter(2, 22), counter(3, 30)]
        );
    }

    #[test]
    fn plan_identical_reading_changes_nothing() {
        let existing = [counter(1, 10), counter(2, 22), counter(3, 30)];
        let readings = [both(2, 12), both(3, 8)];
        assert!(plan(&existing, &readings, Measure::Energy, day(2)).is_empty());
    }

    #[test]
    fn plan_starts_from_the_sum_of_the_last_point_before_from() {
        // Historique ancien : cumul de 1 000 au 1er ; GRDF publie le 2 et le 3.
        let existing = [counter(1, 1_000)];
        let readings = [both(2, 5), both(3, 7)];
        assert_eq!(
            plan(&existing, &readings, Measure::Energy, day(2)),
            [counter(2, 1_005), counter(3, 1_012)]
        );
    }

    #[test]
    fn plan_ignores_points_and_readings_before_from() {
        // Le 1er diffère chez GRDF, mais il est avant `from` : intouché.
        let existing = [counter(1, 10), counter(2, 20)];
        let readings = [both(1, 99), both(2, 10), both(3, 4)];
        assert_eq!(
            plan(&existing, &readings, Measure::Energy, day(2)),
            [counter(3, 24)]
        );
    }

    #[test]
    fn plan_grdf_correction_shifts_every_following_sum() {
        // GRDF corrige le 2 (12 → 15) : le 2 et tous les jours suivants changent.
        let existing = [
            counter(1, 10),
            counter(2, 22),
            counter(3, 30),
            counter(4, 31),
        ];
        let readings = [both(2, 15), both(3, 8), both(4, 1)];
        assert_eq!(
            plan(&existing, &readings, Measure::Energy, day(2)),
            [counter(2, 25), counter(3, 33), counter(4, 34)]
        );
    }

    #[test]
    fn plan_none_day_keeps_ha_value_but_follows_the_new_cumul() {
        // GRDF corrige le 2 (+3), puis n'a pas de valeur le 3 ; HA y a 8.
        // Le 3 garde sa consommation de 8 mais suit le nouveau cumul.
        let existing = [counter(1, 10), counter(2, 22), counter(3, 30)];
        let readings = [both(2, 15), reading(3, None, None)];
        assert_eq!(
            plan(&existing, &readings, Measure::Energy, day(2)),
            [counter(2, 25), counter(3, 33)]
        );
    }

    #[test]
    fn plan_none_day_without_ha_point_creates_nothing() {
        let readings = [both(1, 10), reading(2, None, None), both(3, 5)];
        assert_eq!(
            plan(&[], &readings, Measure::Energy, day(1)),
            [counter(1, 10), counter(3, 15)]
        );
    }

    #[test]
    fn plan_reads_only_the_requested_measure() {
        let readings = [reading(1, Some(10), None), reading(2, None, Some(3))];
        assert_eq!(
            plan(&[], &readings, Measure::Energy, day(1)),
            [counter(1, 10)]
        );
        assert_eq!(
            plan(&[], &readings, Measure::Volume, day(1)),
            [counter(2, 3)]
        );
    }

    #[test]
    fn plan_late_day_is_inserted_and_following_sums_recomputed() {
        // Le 2 manquait (ni HA ni GRDF) ; GRDF le publie enfin : 4.
        let existing = [counter(1, 10), counter(3, 18)];
        let readings = [both(2, 4), both(3, 8)];
        assert_eq!(
            plan(&existing, &readings, Measure::Energy, day(2)),
            [counter(2, 14), counter(3, 22)]
        );
    }

    #[test]
    fn plan_uses_sum_differences_never_state() {
        // Statistiques de gazpar2haws : `state` est un cumul sans rapport
        // avec la consommation du jour.
        let gazpar2haws = |d, state: i64, sum: i64| StatPoint {
            day: day(d),
            state: kwh(state),
            sum: kwh(sum),
        };
        let existing = [gazpar2haws(1, 5_010, 110), gazpar2haws(2, 5_022, 122)];
        // GRDF ne publie que le 3 : le 2 garde sa valeur (122 − 110 = 12)
        // et passe en compteur (`state = sum`).
        let readings = [reading(2, None, None), both(3, 6)];
        assert_eq!(
            plan(&existing, &readings, Measure::Energy, day(2)),
            [counter(2, 122), counter(3, 128)]
        );
    }

    #[test]
    fn plan_compensating_corrections_leave_later_days_alone() {
        // +2 le 2, −2 le 3 : les cumuls du 3 et du 4 sont inchangés.
        let existing = [
            counter(1, 10),
            counter(2, 20),
            counter(3, 30),
            counter(4, 40),
        ];
        let readings = [both(2, 12), both(3, 8), both(4, 10)];
        assert_eq!(
            plan(&existing, &readings, Measure::Energy, day(2)),
            [counter(2, 22)]
        );
    }

    #[test]
    fn plan_without_base_keeps_the_existing_series() {
        // Aucun point avant `from` : la série existante est gardée telle
        // quelle (le premier point garde sa `sum`).
        let existing = [counter(2, 500), counter(3, 510)];
        let readings = [reading(2, None, None), both(4, 5)];
        assert_eq!(
            plan(&existing, &readings, Measure::Energy, day(1)),
            [counter(4, 515)]
        );
    }

    #[test]
    fn plan_accepts_unsorted_input() {
        let existing = [counter(2, 22), counter(1, 10)];
        let readings = [both(3, 8), both(2, 12)];
        assert_eq!(
            plan(&existing, &readings, Measure::Energy, day(2)),
            [counter(3, 30)]
        );
    }

    #[test]
    fn plan_keeps_exact_millis() {
        let tenth = Quantity::from_millis(100);
        let readings: Vec<DailyReading> = (1..=30)
            .map(|d| DailyReading {
                day: day(d),
                energy_kwh: Some(tenth),
                volume_m3: None,
            })
            .collect();
        let points = plan(&[], &readings, Measure::Energy, day(1));
        assert_eq!(points.len(), 30);
        assert_eq!(points[29].sum, Quantity::from_millis(3_000));
    }

    // --- run_once : scénarios de relevés ---

    #[test]
    fn first_import_asks_for_the_whole_history() {
        let ids = ids();
        let mut store = MemoryStore::default();
        let mut grdf = FakeSource::publishing(vec![
            both(10, 10),
            reading(11, Some(12), Some(1)),
            both(12, 8),
        ]);
        let today = day(16);
        let report = run(&mut store, &mut grdf, today, Mode::Apply).unwrap();

        // Un seul appel à GRDF, de today − 30 jours à hier.
        assert_eq!(grdf.calls, [(date(2026, 10, 17), day(15))]);
        assert_eq!(report.requested, Some((date(2026, 10, 17), day(15))));
        assert_eq!(report.readings.len(), 3);
        // Lecture préalable de HA sur 30 jours (+ 1 pour la base), deux mesures.
        assert_eq!(
            store.reads,
            [
                (ids.energy.clone(), date(2026, 10, 16), today),
                (ids.volume.clone(), date(2026, 10, 16), today),
            ]
        );
        assert_eq!(
            store.series(&ids.energy),
            [counter(10, 10), counter(11, 22), counter(12, 30)]
        );
        assert_eq!(
            store.series(&ids.volume),
            [counter(10, 10), counter(11, 11), counter(12, 19)]
        );
        let energy = energy_report(&report);
        assert_eq!(energy.measure, Measure::Energy);
        assert_eq!(energy.id, ids.energy);
        assert_eq!(energy.count(), 3);
        assert_eq!(energy.first_changed(), Some(day(10)));
        assert_eq!(energy.last_changed(), Some(day(12)));
        assert_eq!(energy.last_known, Some(day(12)));
        assert_eq!(volume_report(&report).measure, Measure::Volume);
        assert_eq!(report.imported(), 6);
        assert_eq!(report.mode, Mode::Apply);
        assert_eq!(store.imports.len(), 2);
    }

    #[test]
    fn next_reading_without_news_writes_nothing() {
        let mut store = MemoryStore::default();
        let mut grdf = FakeSource::publishing(vec![both(10, 10), both(11, 12), both(12, 8)]);
        run(&mut store, &mut grdf, day(16), Mode::Apply).unwrap();
        let imports = store.imports.len();

        // Le lendemain : GRDF n'a rien de neuf.
        let report = run(&mut store, &mut grdf, day(17), Mode::Apply).unwrap();
        assert_eq!(store.imports.len(), imports, "aucun import");
        assert_eq!(report.imported(), 0);
        assert_eq!(energy_report(&report).last_known, Some(day(12)));
        assert_eq!(energy_report(&report).first_changed(), None);
        assert_eq!(energy_report(&report).last_changed(), None);
        // GRDF est redemandé à partir du lendemain du dernier jour connu.
        assert_eq!(grdf.calls[1], (day(13), day(16)));
    }

    #[test]
    fn new_day_published_is_appended() {
        let ids = ids();
        let mut store = MemoryStore::with(
            &ids,
            &[counter(10, 10), counter(11, 22)],
            &[counter(10, 1), counter(11, 2)],
        );
        let mut grdf = FakeSource::publishing(vec![both(10, 10), both(11, 12), both(12, 8)]);
        let report = run(&mut store, &mut grdf, day(16), Mode::Apply).unwrap();

        // Fenêtre : min(dernier connu + 1 = 12, today − 3 = 13) → le 12.
        assert_eq!(grdf.calls, [(day(12), day(15))]);
        assert_eq!(energy_report(&report).points, [counter(12, 30)]);
        assert_eq!(volume_report(&report).points, [counter(12, 10)]);
        assert_eq!(volume_report(&report).last_known, Some(day(12)));
    }

    #[test]
    fn overlap_window_rereads_recent_known_days() {
        let ids = ids();
        let known: Vec<StatPoint> = (1..=15).map(|d| counter(d, i64::from(d) * 10)).collect();
        let mut store = MemoryStore::with(&ids, &known, &known);
        let mut grdf = FakeSource::default();
        run(&mut store, &mut grdf, day(16), Mode::Apply).unwrap();
        // Tout est connu jusqu'à hier : GRDF relit quand même les 3 derniers jours.
        assert_eq!(grdf.calls, [(day(13), day(15))]);
        assert!(store.imports.is_empty());
    }

    #[test]
    fn empty_day_published_later_is_caught_up_with_following_sums() {
        let ids = ids();
        let mut store = MemoryStore::default();
        // Relevé du 14 au matin : GRDF n'a pas encore de valeur pour le 11.
        let mut grdf = FakeSource::publishing(vec![
            both(10, 10),
            reading(11, None, None),
            both(12, 8),
            both(13, 5),
        ]);
        run(&mut store, &mut grdf, day(14), Mode::Apply).unwrap();
        assert_eq!(
            store.series(&ids.energy),
            [counter(10, 10), counter(12, 18), counter(13, 23)],
            "le 11 n'est pas importé à zéro"
        );

        // Relevé du 14 au soir : GRDF a publié le 11 (12), dans la fenêtre.
        grdf.published[1] = both(11, 12);
        let report = run(&mut store, &mut grdf, day(14), Mode::Apply).unwrap();
        assert_eq!(grdf.calls[1], (day(11), day(13)));
        assert_eq!(
            energy_report(&report).points,
            [counter(11, 22), counter(12, 30), counter(13, 35)]
        );
        assert_eq!(
            store.series(&ids.energy),
            [
                counter(10, 10),
                counter(11, 22),
                counter(12, 30),
                counter(13, 35)
            ]
        );
    }

    #[test]
    fn grdf_correction_rewrites_the_day_and_the_following_sums() {
        let ids = ids();
        let history = [counter(10, 10), counter(11, 22), counter(12, 30)];
        let mut store = MemoryStore::with(&ids, &history, &history);
        // GRDF corrige l'énergie du 11 de 12 à 9 ; le volume ne change pas.
        let mut grdf = FakeSource::publishing(vec![reading(11, Some(9), Some(12)), both(12, 8)]);
        let report = run(&mut store, &mut grdf, day(14), Mode::Apply).unwrap();
        assert_eq!(grdf.calls, [(day(11), day(13))]);
        assert_eq!(
            energy_report(&report).points,
            [counter(11, 19), counter(12, 27)]
        );
        assert!(volume_report(&report).points.is_empty());
        assert_eq!(store.imports.len(), 1, "seule l'énergie est réécrite");
        assert_eq!(store.imports[0].0, ids.energy);
    }

    #[test]
    fn base_is_taken_from_the_point_just_before_the_window() {
        let ids = ids();
        // Long historique (cumul 5 000 au 1er) dont le dernier jour est le 12.
        let mut history = vec![counter(1, 5_000)];
        history.extend((2..=12).map(|d| counter(d, 5_000 + i64::from(d))));
        let mut store = MemoryStore::with(&ids, &history, &history);
        let mut grdf = FakeSource::publishing(vec![both(13, 7), both(14, 3)]);
        let report = run(&mut store, &mut grdf, day(16), Mode::Apply).unwrap();
        // Fenêtre à partir du 13 ; base = sum du 12 = 5 012.
        assert_eq!(grdf.calls, [(day(13), day(15))]);
        assert_eq!(
            energy_report(&report).points,
            [counter(13, 5_019), counter(14, 5_022)]
        );
    }

    #[test]
    fn none_day_does_not_overwrite_an_existing_value() {
        let ids = ids();
        let history = [counter(12, 100), counter(13, 108), counter(14, 111)];
        let mut store = MemoryStore::with(&ids, &history, &history);
        // GRDF n'a plus (ou pas encore) les valeurs du 13 et du 14.
        let mut grdf =
            FakeSource::publishing(vec![reading(13, None, None), reading(14, None, None)]);
        let report = run(&mut store, &mut grdf, day(16), Mode::Apply).unwrap();
        assert_eq!(report.imported(), 0);
        assert_eq!(store.series(&ids.energy), history);
    }

    #[test]
    fn takes_over_gazpar2haws_statistics_with_cumulative_state() {
        let ids = ids();
        // gazpar2haws : `state` cumulé (ici un index), `sum` = cumul depuis
        // son premier import ; il a importé le 13 à zéro faute de valeur.
        let g2h = |d, index: i64, sum: i64| StatPoint {
            day: day(d),
            state: kwh(index),
            sum: kwh(sum),
        };
        let history = [
            g2h(10, 9_090, 90),
            g2h(11, 9_100, 100),
            g2h(12, 9_110, 110),
            g2h(13, 9_110, 110),
        ];
        let mut store = MemoryStore::with(&ids, &history, &history);
        let mut grdf =
            FakeSource::publishing(vec![reading(12, None, None), both(13, 6), both(14, 4)]);
        let settings = SyncSettings {
            history_days: 30,
            overlap_days: 4,
        };
        let report = run_with(&mut store, &mut grdf, settings, day(15)).unwrap();
        assert_eq!(grdf.calls, [(day(11), day(14))]);
        // Base = sum du 10 (90) ; valeurs du jour tirées des `sum` (11 : 10,
        // 12 : 10), jamais des `state` ; le zéro du 13 est corrigé ; tout
        // passe en compteur (`state = sum`).
        assert_eq!(
            energy_report(&report).points,
            [
                counter(11, 100),
                counter(12, 110),
                counter(13, 116),
                counter(14, 120)
            ]
        );
    }

    #[test]
    fn nothing_to_ask_when_from_is_after_yesterday() {
        let ids = ids();
        let history = [counter(14, 10), counter(15, 20)];
        let mut store = MemoryStore::with(&ids, &history, &history);
        let mut grdf = FakeSource::default();
        let settings = SyncSettings {
            history_days: 30,
            overlap_days: 0,
        };
        let report = run_with(&mut store, &mut grdf, settings, day(16)).unwrap();
        assert!(grdf.calls.is_empty(), "GRDF n'est pas appelé");
        assert_eq!(report.requested, None);
        assert!(report.readings.is_empty());
        assert_eq!(report.imported(), 0);
        assert_eq!(energy_report(&report).last_known, Some(day(15)));
        assert!(store.imports.is_empty());
    }

    #[test]
    fn measure_without_any_point_reaches_back_to_the_whole_history() {
        let ids = ids();
        // L'énergie a un long historique, commencé avant la fenêtre de 30
        // jours, et elle est à jour ; le volume n'a jamais été importé.
        let all = days(date(2026, 9, 1), day(15));
        let energy: Vec<StatPoint> = all
            .iter()
            .zip(1_000..)
            .map(|(&d, sum)| point_on(d, sum))
            .collect();
        let mut store = MemoryStore::with(&ids, &energy, &[]);
        let mut grdf = FakeSource::publishing(
            all.iter()
                .map(|&d| reading_on(d, Some(1), Some(1)))
                .collect(),
        );
        let report = run(&mut store, &mut grdf, day(16), Mode::Apply).unwrap();
        assert_eq!(grdf.calls, [(date(2026, 10, 17), day(15))]);
        // La base de l'énergie est lue la veille de la fenêtre : rien ne bouge.
        assert!(energy_report(&report).points.is_empty());
        assert_eq!(store.series(&ids.energy), energy);
        let volume = volume_report(&report);
        assert_eq!(volume.count(), 30);
        assert_eq!(volume.first_changed(), Some(date(2026, 10, 17)));
        assert_eq!(volume.points[29], counter(15, 30));
    }

    #[test]
    fn dry_run_reports_the_same_but_writes_nothing() {
        let published = vec![both(10, 10), both(11, 12)];
        let mut applied_store = MemoryStore::default();
        let applied = run(
            &mut applied_store,
            &mut FakeSource::publishing(published.clone()),
            day(16),
            Mode::Apply,
        )
        .unwrap();

        let mut store = MemoryStore::default();
        let mut grdf = FakeSource::publishing(published);
        let simulated = run(&mut store, &mut grdf, day(16), Mode::DryRun).unwrap();
        assert_eq!(grdf.calls.len(), 1, "GRDF est lu une fois");
        assert!(store.imports.is_empty(), "rien n'est écrit");
        assert!(store.points.is_empty());
        assert_eq!(simulated.mode, Mode::DryRun);
        assert_eq!(simulated.measures, applied.measures);
        assert_eq!(simulated.readings, applied.readings);
        assert_eq!(simulated.requested, applied.requested);
        assert_eq!(simulated.imported(), 4);
    }

    #[test]
    fn last_known_days_reads_the_same_period_as_a_reading() {
        let ids = ids();
        let mut store = MemoryStore::with(&ids, &[counter(1, 5), counter(13, 9)], &[]);
        let known = last_known_days(&mut store, &ids, SETTINGS, day(16)).unwrap();
        assert_eq!(
            known,
            vec![(Measure::Energy, Some(day(13))), (Measure::Volume, None)]
        );
        // Même période que run_once : history_days + 1 jours avant aujourd'hui.
        let start = date(2026, 10, 16);
        assert_eq!(
            store.reads,
            vec![
                (ids.energy.clone(), start, day(16)),
                (ids.volume.clone(), start, day(16)),
            ]
        );
        assert!(store.imports.is_empty());
    }

    #[test]
    fn grdf_failure_writes_nothing() {
        let ids = ids();
        let history = [counter(10, 10)];
        let mut store = MemoryStore::with(&ids, &history, &history);
        let mut grdf = FakeSource {
            fail: true,
            ..FakeSource::default()
        };
        let e = run(&mut store, &mut grdf, day(16), Mode::Apply).unwrap_err();
        assert_eq!(e, Error::Http("GRDF injoignable".into()));
        assert!(store.imports.is_empty());
    }

    #[test]
    fn home_assistant_read_failure_does_not_call_grdf() {
        let mut store = MemoryStore {
            fail_reads: true,
            ..MemoryStore::default()
        };
        let mut grdf = FakeSource::publishing(vec![both(10, 10)]);
        let e = run(&mut store, &mut grdf, day(16), Mode::Apply).unwrap_err();
        assert!(matches!(e, Error::HomeAssistant(_)), "{e}");
        assert!(grdf.calls.is_empty(), "pas de connexion GRDF inutile");
        assert!(store.imports.is_empty());
    }

    #[test]
    fn first_import_failure_stops_before_the_second() {
        let mut store = MemoryStore {
            fail_import: Some(1),
            ..MemoryStore::default()
        };
        let mut grdf = FakeSource::publishing(vec![both(10, 10)]);
        let e = run(&mut store, &mut grdf, day(16), Mode::Apply).unwrap_err();
        assert!(matches!(e, Error::HomeAssistant(_)), "{e}");
        assert_eq!(store.imports.len(), 1, "le volume n'est pas tenté");
        assert!(store.points.is_empty());
    }

    #[test]
    fn interrupted_reading_is_repaired_by_the_next_one() {
        let ids = ids();
        let published: Vec<DailyReading> = (1..=12)
            .map(|d| reading(d, Some(i64::from(d)), Some(1)))
            .collect();
        // Référence : le même relevé sans incident.
        let mut clean = MemoryStore::default();
        run(
            &mut clean,
            &mut FakeSource::publishing(published.clone()),
            day(16),
            Mode::Apply,
        )
        .unwrap();

        // Relevé du 16 : l'énergie est écrite, l'import du volume échoue.
        let mut store = MemoryStore {
            fail_import: Some(2),
            ..MemoryStore::default()
        };
        let mut grdf = FakeSource::publishing(published);
        assert!(run(&mut store, &mut grdf, day(16), Mode::Apply).is_err());
        assert_eq!(store.series(&ids.energy), clean.series(&ids.energy));
        assert!(store.series(&ids.volume).is_empty());

        // Relevé suivant, HA rétabli : le volume est rattrapé en entier,
        // l'énergie, déjà juste, n'est pas réécrite.
        store.fail_import = None;
        let report = run(&mut store, &mut grdf, day(16), Mode::Apply).unwrap();
        assert_eq!(grdf.calls[1], (date(2026, 10, 17), day(15)));
        assert!(energy_report(&report).points.is_empty());
        assert_eq!(volume_report(&report).count(), 12);
        assert_eq!(store.points, clean.points);
    }

    #[test]
    fn overlap_longer_than_history_still_finds_the_base() {
        let ids = ids();
        let history: Vec<StatPoint> = (1..=10).map(|d| counter(d, i64::from(d) * 10)).collect();
        let mut store = MemoryStore::with(&ids, &history, &history);
        let mut grdf = FakeSource::publishing(vec![both(6, 3)]);
        let settings = SyncSettings {
            history_days: 2,
            overlap_days: 10,
        };
        let report = run_with(&mut store, &mut grdf, settings, day(16)).unwrap();
        // HA est lu sur 10 jours + 1 (et non 2) : la base du 5 (50) est lue.
        assert_eq!(store.reads[0].1, day(5));
        assert_eq!(grdf.calls, [(day(6), day(15))]);
        // Le 6 passe de 10 à 3 : 53, et tous les cumuls suivants baissent de 7.
        assert_eq!(energy_report(&report).first_changed(), Some(day(6)));
        assert_eq!(energy_report(&report).points[0], counter(6, 53));
        assert_eq!(energy_report(&report).points[4], counter(10, 93));
    }

    #[test]
    fn statistic_ids_by_measure() {
        let ids = ids();
        assert_eq!(ids.get(Measure::Energy), &ids.energy);
        assert_eq!(ids.get(Measure::Volume), &ids.volume);
    }
}

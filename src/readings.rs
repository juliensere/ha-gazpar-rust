//! Transformation des relevés GRDF (JSON) en [`crate::domain::DailyReading`].
//!
//! Fonction pure ([`parse_informative`]) : reçoit le texte de la réponse
//! `consommation/informatives` et le PCE, renvoie les relevés quotidiens
//! triés **et** les avertissements, que l'appelant journalise. Aucune
//! entrée-sortie ici, pas même le journal.
//!
//! Règles :
//!
//! - un relevé par `journeeGaziere` (jour gazier), qui fixe la date ; les
//!   champs `dateDebutReleve`/`dateFinReleve` ne servent pas ;
//! - les nombres sont lus sous forme de texte décimal (représentation de
//!   `serde_json::Number`) puis convertis en millièmes par
//!   [`Quantity::parse_decimal`] ;
//! - `energieConsomme` ou `volumeBrutConsomme` à `null` (ou absent) : la
//!   valeur reste `None`, jamais zéro ;
//! - relevé invalide (date illisible, valeur négative ou non numérique) :
//!   ignoré avec un avertissement, le reste est conservé ;
//! - relevé sans `journeeGaziere` (période publiée) : ignoré avec un
//!   avertissement, le type `published` n'est pas pris en charge ;
//! - même jour en double : le dernier relevé l'emporte, avec un
//!   avertissement ;
//! - réponse `[]` ou PCE absent de la réponse : aucun relevé.
//!
//! Contrairement à PyGazpar, un jour sans donnée n'est pas reconstruit à
//! partir des index : il sera rattrapé quand GRDF publiera la valeur.
//!
//! Dépendances : `domain`, `error`, `jiff`, `serde_json`.

use jiff::civil::Date;
use serde_json::Value;

use crate::domain::{DailyReading, Quantity};
use crate::error::Error;

/// Résultat de l'analyse : relevés triés par jour et avertissements à
/// journaliser.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    pub readings: Vec<DailyReading>,
    pub warnings: Vec<String>,
}

/// Analyse une réponse `consommation/informatives` pour un PCE.
///
/// Erreur seulement si la réponse elle-même est inexploitable (JSON
/// invalide, forme inattendue) ; un relevé isolé invalide n'est qu'un
/// avertissement.
pub fn parse_informative(json: &str, pce: &str) -> Result<Parsed, Error> {
    let bad = |why: String| Error::Grdf(format!("réponse de consommation : {why}"));
    let root: Value =
        serde_json::from_str(json).map_err(|e| bad(format!("JSON invalide ({e})")))?;

    let mut parsed = Parsed::default();
    let by_pce = match &root {
        Value::Array(items) if items.is_empty() => return Ok(parsed),
        Value::Object(map) => map,
        _ => return Err(bad("objet indexé par PCE ou [] attendu".to_owned())),
    };
    let Some(consumption) = by_pce.get(pce) else {
        parsed
            .warnings
            .push(format!("le PCE {pce} est absent de la réponse de GRDF"));
        return Ok(parsed);
    };
    let Some(records) = consumption.get("releves").and_then(Value::as_array) else {
        return Err(bad(format!(
            "« releves » absent ou invalide pour le PCE {pce}"
        )));
    };

    for (i, record) in records.iter().enumerate() {
        match parse_record(record) {
            Ok(reading) => parsed.readings.push(reading),
            Err(why) => parsed
                .warnings
                .push(format!("relevé n° {} ignoré : {why}", i + 1)),
        }
    }

    // Tri stable : à jour égal, l'ordre de la réponse est conservé et le
    // dernier relevé l'emporte.
    parsed.readings.sort_by_key(|r| r.day);
    let mut unique: Vec<DailyReading> = Vec::with_capacity(parsed.readings.len());
    for reading in parsed.readings.drain(..) {
        match unique.last_mut() {
            Some(last) if last.day == reading.day => {
                parsed.warnings.push(format!(
                    "jour {} présent plusieurs fois : dernier relevé retenu",
                    reading.day
                ));
                *last = reading;
            }
            _ => unique.push(reading),
        }
    }
    parsed.readings = unique;
    Ok(parsed)
}

/// Un relevé informatif, ou le motif de son rejet.
fn parse_record(record: &Value) -> Result<DailyReading, String> {
    if !record.is_object() {
        return Err("objet attendu".to_owned());
    }
    let day = match record.get("journeeGaziere") {
        None | Some(Value::Null) => {
            return Err(if is_present(record.get("dateDebutReleve")) {
                "période publiée (sans journeeGaziere), type « published » non pris en charge"
                    .to_owned()
            } else {
                "sans journeeGaziere".to_owned()
            });
        }
        Some(Value::String(s)) => s
            .parse::<Date>()
            .map_err(|_| format!("journeeGaziere illisible : {:?}", truncate(s)))?,
        Some(other) => {
            return Err(format!(
                "journeeGaziere illisible : {}",
                truncate(&other.to_string())
            ));
        }
    };
    Ok(DailyReading {
        day,
        energy_kwh: quantity(record, "energieConsomme").map_err(|e| format!("{day} : {e}"))?,
        volume_m3: quantity(record, "volumeBrutConsomme").map_err(|e| format!("{day} : {e}"))?,
    })
}

fn is_present(v: Option<&Value>) -> bool {
    v.is_some_and(|v| !v.is_null())
}

/// Valeur positive ou nulle d'un champ, `None` s'il est absent ou `null`.
fn quantity(record: &Value, field: &str) -> Result<Option<Quantity>, String> {
    match record.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            // Le signe est lu sur le nombre lui-même : « -0.0001 » s'arrondirait
            // à zéro en millièmes mais reste une valeur négative.
            if n.as_f64().is_some_and(|f| f < 0.0) {
                return Err(format!("{field} négatif ({n})"));
            }
            Quantity::parse_decimal(&n.to_string())
                .map(Some)
                .map_err(|e| format!("{field} : {e}"))
        }
        Some(other) => Err(format!(
            "{field} n'est pas un nombre ({})",
            truncate(&other.to_string())
        )),
    }
}

/// Coupe un texte venu de GRDF pour l'afficher dans un avertissement.
fn truncate(s: &str) -> String {
    s.chars().take(40).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    const PCE: &str = "98765432109876";
    const EXTRACT: &str =
        include_str!("../tests/fixtures/pygazpar/donnees_informatives_extrait.json");
    const DECIMALS: &str =
        include_str!("../tests/fixtures/pygazpar/journee_gaziere_decimales.json");

    fn q(millis: i64) -> Option<Quantity> {
        Some(Quantity::from_millis(millis))
    }

    fn parse(json: &str) -> Parsed {
        parse_informative(json, PCE).unwrap()
    }

    /// Réponse d'un seul PCE avec ces relevés.
    fn response(records: &str) -> String {
        format!(r#"{{"{PCE}": {{"idPce": "{PCE}", "releves": [{records}]}}}}"#)
    }

    fn record(day: &str, energy: &str, volume: &str) -> String {
        format!(
            r#"{{"journeeGaziere": "{day}", "energieConsomme": {energy}, "volumeBrutConsomme": {volume},
                "dateDebutReleve": "{day}T06:00:00+00:00", "qualificationReleve": "Mesuré"}}"#
        )
    }

    // --- Jeux d'essai de PyGazpar ---

    #[test]
    fn pygazpar_extract_daily_values() {
        let parsed = parse(EXTRACT);
        // Source : fichier (energieConsomme et volumeBrutConsomme de chaque
        // journeeGaziere, du 2021-07-26 au 2021-08-22), en unités entières.
        // PyGazpar produit les mêmes valeurs sur cet extrait (voir le README
        // des jeux d'essai). `None` : 2021-08-10, « Absence de Données ».
        #[rustfmt::skip]
        let expected: [(Option<i64>, Option<i64>); 28] = [
            // 26 au 31 juillet
            (Some(10), Some(1)), (Some(18), Some(2)), (Some(11), Some(1)),
            (Some(16), Some(1)), (Some(15), Some(1)), (Some(13), Some(1)),
            // 1er au 9 août
            (Some(8), Some(1)), (Some(21), Some(2)), (Some(17), Some(2)),
            (Some(17), Some(2)), (Some(16), Some(1)), (Some(18), Some(2)),
            (Some(23), Some(2)), (Some(3), Some(0)), (Some(0), Some(0)),
            // 10 août : jour sans donnée
            (None, None),
            // 11 au 22 août
            (Some(0), Some(0)), (Some(0), Some(0)), (Some(0), Some(0)),
            (Some(0), Some(0)), (Some(19), Some(2)), (Some(9), Some(1)),
            (Some(0), Some(0)), (Some(0), Some(0)), (Some(0), Some(0)),
            (Some(0), Some(0)), (Some(0), Some(0)), (Some(0), Some(0)),
        ];
        let units = |v: Option<i64>| v.map(|v| Quantity::from_millis(v * 1000));
        assert_eq!(parsed.readings.len(), expected.len());
        let mut day = date(2021, 7, 26);
        for (r, (energy, volume)) in parsed.readings.iter().zip(expected) {
            assert_eq!(r.day, day);
            assert_eq!(r.energy_kwh, units(energy), "{day}");
            assert_eq!(r.volume_m3, units(volume), "{day}");
            day = day.tomorrow().unwrap();
        }
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    }

    #[test]
    fn pygazpar_extract_totals_and_month_change() {
        let parsed = parse(EXTRACT);
        // Source : fichier (somme des valeurs non nulles de l'extrait).
        let energy: Quantity = parsed.readings.iter().filter_map(|r| r.energy_kwh).sum();
        let volume: Quantity = parsed.readings.iter().filter_map(|r| r.volume_m3).sum();
        assert_eq!(energy, Quantity::from_millis(234_000));
        assert_eq!(volume, Quantity::from_millis(22_000));
        let july = parsed
            .readings
            .iter()
            .filter(|r| r.day.month() == 7)
            .count();
        assert_eq!(july, 6);
    }

    #[test]
    fn pygazpar_day_without_data_stays_without_data() {
        // Source : fichier (2021-08-10, « Absence de Données ») ; même
        // résultat que PyGazpar, qui ne peut pas reconstruire ce jour.
        let parsed = parse(EXTRACT);
        let find = |d: Date| parsed.readings.iter().find(|r| r.day == d).unwrap();
        let gap = find(date(2021, 8, 10));
        assert_eq!((gap.energy_kwh, gap.volume_m3), (None, None));
        // 2021-08-11 : indexDebut à null mais valeurs présentes, à zéro.
        let next = find(date(2021, 8, 11));
        assert_eq!((next.energy_kwh, next.volume_m3), (q(0), q(0)));
    }

    #[test]
    fn pygazpar_gas_day_with_decimals() {
        // Source : assertion PyGazpar (test_informative_readings_use_gas_day) :
        // le jour est journeeGaziere (23/07/2026), pas dateFinReleve (24).
        // Valeurs : fichier (0.28 m³, 3.16 kWh).
        let parsed = parse(DECIMALS);
        assert_eq!(
            parsed.readings,
            vec![DailyReading {
                day: date(2026, 7, 23),
                energy_kwh: q(3_160),
                volume_m3: q(280),
            }]
        );
    }

    // --- Réponses vides ou inattendues ---

    #[test]
    fn empty_list_gives_nothing() {
        assert_eq!(parse("[]"), Parsed::default());
        assert_eq!(parse(" [ ] \n"), Parsed::default());
    }

    #[test]
    fn absent_pce_gives_nothing_with_a_warning() {
        let parsed = parse_informative(EXTRACT, "11111111111111").unwrap();
        assert!(parsed.readings.is_empty());
        assert_eq!(parsed.warnings.len(), 1);
        assert!(parsed.warnings[0].contains("11111111111111"));
    }

    #[test]
    fn empty_records_give_nothing() {
        assert_eq!(parse(&response("")), Parsed::default());
    }

    #[test]
    fn unusable_responses_are_errors() {
        for json in [
            "",
            "{",
            "<html>Erreur</html>",
            "null",
            "42",
            "[1]",
            "\"texte\"",
            &format!(r#"{{"{PCE}": {{}}}}"#),
            &format!(r#"{{"{PCE}": {{"releves": {{}}}}}}"#),
            &format!(r#"{{"{PCE}": []}}"#),
        ] {
            let e = parse_informative(json, PCE).unwrap_err();
            assert!(matches!(e, Error::Grdf(_)), "{json:?} : {e}");
        }
    }

    // --- Relevés ---

    #[test]
    fn decimals_become_thousandths() {
        let parsed = parse(&response(&record("2026-01-15", "23.1", "2.075")));
        assert_eq!(parsed.readings[0].energy_kwh, q(23_100));
        assert_eq!(parsed.readings[0].volume_m3, q(2_075));
        // Au-delà du millième, arrondi au plus proche.
        let parsed = parse(&response(&record("2026-01-15", "1.23456", "0.0004")));
        assert_eq!(parsed.readings[0].energy_kwh, q(1_235));
        assert_eq!(parsed.readings[0].volume_m3, q(0));
    }

    #[test]
    fn exponent_and_large_values() {
        let parsed = parse(&response(&record("2026-01-15", "1.5e2", "1E-7")));
        assert_eq!(parsed.readings[0].energy_kwh, q(150_000));
        assert_eq!(parsed.readings[0].volume_m3, q(0));
    }

    #[test]
    fn null_or_missing_values_stay_none() {
        let records = [
            record("2026-01-01", "null", "null"),
            record("2026-01-02", "12", "null"),
            r#"{"journeeGaziere": "2026-01-03"}"#.to_owned(),
        ]
        .join(",");
        let parsed = parse(&response(&records));
        let values: Vec<_> = parsed
            .readings
            .iter()
            .map(|r| (r.energy_kwh, r.volume_m3))
            .collect();
        assert_eq!(values, vec![(None, None), (q(12_000), None), (None, None)]);
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn readings_are_sorted() {
        let records = [
            record("2026-03-02", "2", "1"),
            record("2026-02-28", "1", "1"),
            record("2026-03-01", "3", "1"),
        ]
        .join(",");
        let days: Vec<Date> = parse(&response(&records))
            .readings
            .iter()
            .map(|r| r.day)
            .collect();
        assert_eq!(
            days,
            vec![date(2026, 2, 28), date(2026, 3, 1), date(2026, 3, 2)]
        );
    }

    #[test]
    fn duplicate_day_keeps_the_last_with_a_warning() {
        let records = [
            record("2026-01-02", "5", "1"),
            record("2026-01-01", "1", "1"),
            record("2026-01-02", "7", "1"),
        ]
        .join(",");
        let parsed = parse(&response(&records));
        assert_eq!(parsed.readings.len(), 2);
        assert_eq!(parsed.readings[1].energy_kwh, q(7_000));
        assert_eq!(parsed.warnings.len(), 1);
        assert!(
            parsed.warnings[0].contains("2026-01-02"),
            "{:?}",
            parsed.warnings
        );
    }

    #[test]
    fn invalid_records_are_skipped_with_a_warning() {
        let records = [
            record("2026-01-01", "10", "1"),
            record("2026-13-01", "10", "1"),
            record("01/02/2026", "10", "1"),
            record("2026-01-03", "-1", "1"),
            record("2026-01-04", "10", "-0.0001"),
            record("2026-01-05", "\"12\"", "1"),
            record("2026-01-06", "true", "1"),
            record("2026-01-07", "1e30", "1"),
            r#"{"journeeGaziere": 20260108, "energieConsomme": 1}"#.to_owned(),
            "42".to_owned(),
            record("2026-01-09", "0", "0"),
        ]
        .join(",");
        let parsed = parse(&response(&records));
        let days: Vec<Date> = parsed.readings.iter().map(|r| r.day).collect();
        assert_eq!(days, vec![date(2026, 1, 1), date(2026, 1, 9)]);
        assert_eq!(parsed.warnings.len(), 9, "{:#?}", parsed.warnings);
        assert!(parsed.warnings[0].contains("2026-13-01"));
        assert!(parsed.warnings[2].contains("energieConsomme négatif"));
        assert!(parsed.warnings[3].contains("volumeBrutConsomme négatif"));
        assert!(parsed.warnings[4].contains("n'est pas un nombre"));
        assert!(parsed.warnings[6].contains("valeur trop grande"));
    }

    #[test]
    fn negative_zero_is_accepted() {
        let parsed = parse(&response(&record("2026-01-01", "-0", "-0.0")));
        assert_eq!(parsed.readings[0].energy_kwh, q(0));
        assert_eq!(parsed.readings[0].volume_m3, q(0));
    }

    #[test]
    fn published_period_is_ignored_with_a_warning() {
        // Forme d'un relevé publié de PyGazpar (donnees_publiees.json).
        let published = r#"{"journeeGaziere": null, "dateDebutReleve": "2026-01-01T06:00:00+00:00",
            "dateFinReleve": "2026-01-04T06:00:00+00:00", "indexDebut": 100, "indexFin": 110,
            "volumeBrutConsomme": 10, "energieConsomme": 100, "coeffConversion": 11.0}"#;
        let records = [published.to_owned(), record("2026-01-05", "4", "1")].join(",");
        let parsed = parse(&response(&records));
        assert_eq!(parsed.readings.len(), 1);
        assert_eq!(parsed.warnings.len(), 1);
        assert!(
            parsed.warnings[0].contains("published"),
            "{:?}",
            parsed.warnings
        );
    }

    #[test]
    fn record_without_any_date_is_ignored() {
        // Cas de PyGazpar (test_readings_without_any_date_are_ignored).
        let parsed = parse(&response(
            r#"{"journeeGaziere": null, "dateFinReleve": null, "temperature": null}"#,
        ));
        assert!(parsed.readings.is_empty());
        assert!(parsed.warnings[0].contains("sans journeeGaziere"));
    }

    #[test]
    fn other_pces_are_ignored() {
        let json = format!(
            r#"{{"11111111111111": {{"releves": [{}]}}, "{PCE}": {{"releves": [{}]}}}}"#,
            record("2026-01-01", "99", "9"),
            record("2026-01-01", "1", "1"),
        );
        let parsed = parse(&json);
        assert_eq!(parsed.readings.len(), 1);
        assert_eq!(parsed.readings[0].energy_kwh, q(1_000));
    }

    #[test]
    fn warnings_quote_at_most_40_characters() {
        let long = "x".repeat(500);
        let parsed = parse(&response(&record(&long, "1", "1")));
        assert!(parsed.warnings[0].len() < 120, "{}", parsed.warnings[0]);
    }
}

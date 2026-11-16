//! Types du métier : quantités, relevés quotidiens, mesures, identifiants et
//! points de statistiques.
//!
//! Aucune entrée-sortie ni lecture de l'horloge ici : ce module ne fait que
//! des calculs et des validations. Les jours sont des dates civiles
//! (`jiff::civil::Date`) ; leur conversion en minuit dans le fuseau de Home
//! Assistant appartient à `ha/`.
//!
//! Dépendances : `jiff` (dates civiles) et `error`.

use std::fmt;
use std::iter::Sum;
use std::ops::{Add, Sub};
use std::str::FromStr;

use jiff::civil::Date;

use crate::error::Error;

/// Quantité en millièmes d'unité (kWh ou m³), stockée en entier pour que les
/// cumuls soient exacts.
///
/// Les sommes ne débordent pas en pratique : `i64` en millièmes permet plus
/// de 9 × 10¹² kWh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Quantity(i64);

impl Quantity {
    pub const ZERO: Quantity = Quantity(0);

    /// Quantité à partir d'un nombre de millièmes.
    pub const fn from_millis(millis: i64) -> Self {
        Quantity(millis)
    }

    /// Nombre de millièmes.
    pub const fn millis(self) -> i64 {
        self.0
    }

    /// Valeur flottante, pour l'envoi à Home Assistant uniquement (jamais
    /// pour calculer).
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / 1000.0
    }

    /// Lit un nombre écrit selon la grammaire JSON (`-?entier(.fraction)?(e±exposant)?`) :
    /// `"12"`, `"1.5"`, `"0.123"`, `"-0"`, `"1e-7"`…
    ///
    /// L'exposant est accepté car `serde_json` (sans `arbitrary_precision`)
    /// écrit ainsi les très petits et très grands flottants.
    ///
    /// Au-delà de 3 décimales, la valeur est arrondie au millième le plus
    /// proche, **demi vers l'extérieur** (en s'éloignant de zéro) :
    /// `0.0005` → `0.001`, `-0.0005` → `-0.001`, `0.00049` → `0`.
    ///
    /// Refusé (comme en JSON) : texte vide, espaces, signe `+`, zéro non
    /// significatif (`01`), point sans chiffre de part ou d'autre (`1.`,
    /// `.5`), virgule décimale, et toute valeur hors des bornes d'un `i64`
    /// en millièmes.
    pub fn parse_decimal(text: &str) -> Result<Quantity, Error> {
        let invalid = |why: &str| {
            let shown: String = text.chars().take(40).collect();
            Error::Invalid(format!("quantité {shown:?} : {why}"))
        };

        let (negative, rest) = match text.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, text),
        };
        let (mantissa, exponent) = match rest.find(['e', 'E']) {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        let (int_part, frac_part) = match mantissa.split_once('.') {
            Some((i, f)) => (i, f),
            None => (mantissa, ""),
        };

        if !is_digits(int_part) {
            return Err(invalid("partie entière absente ou non numérique"));
        }
        if int_part.len() > 1 && int_part.starts_with('0') {
            return Err(invalid("zéro non significatif"));
        }
        if mantissa.contains('.') && !is_digits(frac_part) {
            return Err(invalid("partie décimale absente ou non numérique"));
        }
        let exponent = match exponent {
            None => 0,
            Some(e) => parse_exponent(e).ok_or_else(|| invalid("exposant invalide"))?,
        };

        // valeur = chiffres × 10^(exposant − nb de décimales) ;
        // en millièmes : chiffres × 10^(3 + exposant − nb de décimales).
        let digits: String = [int_part, frac_part].concat();
        let frac_len = i64::try_from(frac_part.len()).map_err(|_| invalid("trop de décimales"))?;
        let shift = 3 + exponent - frac_len;

        let magnitude = if digits.bytes().all(|b| b == b'0') {
            0
        } else if shift >= 0 {
            let factor = u32::try_from(shift)
                .ok()
                .and_then(|s| 10_i64.checked_pow(s))
                .ok_or_else(|| invalid("valeur trop grande"))?;
            digits_value(&digits)
                .and_then(|v| v.checked_mul(factor))
                .ok_or_else(|| invalid("valeur trop grande"))?
        } else {
            let drop = usize::try_from(-shift).unwrap_or(usize::MAX);
            if drop > digits.len() {
                0
            } else {
                let cut = digits.len() - drop;
                let kept =
                    digits_value(&digits[..cut]).ok_or_else(|| invalid("valeur trop grande"))?;
                // Premier chiffre abandonné ≥ 5 : on s'éloigne de zéro.
                let round_up = digits.as_bytes()[cut] >= b'5';
                kept.checked_add(i64::from(round_up))
                    .ok_or_else(|| invalid("valeur trop grande"))?
            }
        };
        Ok(Quantity(if negative { -magnitude } else { magnitude }))
    }
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Valeur entière d'une suite de chiffres déjà validée (vide = 0) ; `None`
/// si elle dépasse un `i64`.
fn digits_value(digits: &str) -> Option<i64> {
    digits.bytes().try_fold(0_i64, |acc, b| {
        acc.checked_mul(10)?.checked_add(i64::from(b - b'0'))
    })
}

/// Exposant décimal (`[+-]?chiffres`), borné à ±10 000 : au-delà, la valeur
/// déborde ou s'arrondit à zéro de toute façon.
fn parse_exponent(e: &str) -> Option<i64> {
    let (negative, digits) = match e.as_bytes().first()? {
        b'-' => (true, &e[1..]),
        b'+' => (false, &e[1..]),
        _ => (false, e),
    };
    if !is_digits(digits) {
        return None;
    }
    let value = digits_value(digits).unwrap_or(i64::MAX).min(10_000);
    Some(if negative { -value } else { value })
}

impl Add for Quantity {
    type Output = Quantity;
    fn add(self, rhs: Quantity) -> Quantity {
        Quantity(self.0 + rhs.0)
    }
}

impl Sub for Quantity {
    type Output = Quantity;
    fn sub(self, rhs: Quantity) -> Quantity {
        Quantity(self.0 - rhs.0)
    }
}

impl Sum for Quantity {
    fn sum<I: Iterator<Item = Quantity>>(iter: I) -> Quantity {
        iter.fold(Quantity::ZERO, Add::add)
    }
}

impl<'a> Sum<&'a Quantity> for Quantity {
    fn sum<I: Iterator<Item = &'a Quantity>>(iter: I) -> Quantity {
        iter.copied().sum()
    }
}

/// Affichage décimal sans zéros finaux inutiles : `1.5`, `12`, `-0.003`.
impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let (int, frac) = (abs / 1000, abs % 1000);
        if frac == 0 {
            write!(f, "{sign}{int}")
        } else {
            let frac = format!("{frac:03}");
            write!(f, "{sign}{int}.{}", frac.trim_end_matches('0'))
        }
    }
}

/// Relevé d'un jour gazier. Une valeur absente chez GRDF reste `None` : elle
/// n'est jamais remplacée par zéro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyReading {
    pub day: Date,
    pub energy_kwh: Option<Quantity>,
    pub volume_m3: Option<Quantity>,
}

impl DailyReading {
    /// Valeur du relevé pour une mesure.
    pub fn get(&self, measure: Measure) -> Option<Quantity> {
        match measure {
            Measure::Energy => self.energy_kwh,
            Measure::Volume => self.volume_m3,
        }
    }
}

/// Grandeur importée dans Home Assistant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Measure {
    Energy,
    Volume,
}

impl Measure {
    pub const ALL: [Measure; 2] = [Measure::Energy, Measure::Volume];

    /// Suffixe de l'identifiant de statistique.
    pub fn key(self) -> &'static str {
        match self {
            Measure::Energy => "energy",
            Measure::Volume => "volume",
        }
    }

    /// Unité attendue par Home Assistant (`unit_of_measurement`).
    pub fn unit(self) -> &'static str {
        match self {
            Measure::Energy => "kWh",
            Measure::Volume => "m³",
        }
    }

    /// Classe d'unité de Home Assistant (`unit_class`).
    pub fn unit_class(self) -> &'static str {
        match self {
            Measure::Energy => "energy",
            Measure::Volume => "volume",
        }
    }

    /// Nom affiché dans Home Assistant.
    pub fn display_name(self) -> &'static str {
        match self {
            Measure::Energy => "Gaz (énergie)",
            Measure::Volume => "Gaz (volume)",
        }
    }
}

/// Identifiant validé d'une statistique Home Assistant.
///
/// Deux formes, suivant les règles de Home Assistant (minuscules, chiffres,
/// `_`, pas de `_` en début ou fin de partie, pas de `__`) :
///
/// - externe : `<source>:<objet>`, par exemple `gazpar:12345678901234_energy` ;
/// - entité : `sensor.<objet>`, par exemple `sensor.gazpar2haws_energy`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StatisticId(String);

impl StatisticId {
    /// Valide un identifiant complet.
    pub fn parse(s: &str) -> Result<StatisticId, Error> {
        let invalid = |why: &str| {
            Error::Invalid(format!(
                "identifiant de statistique {s:?} : {why} (attendu <source>:<objet> ou sensor.<objet>, en minuscules, chiffres et _)"
            ))
        };
        if let Some(object) = s.strip_prefix("sensor.") {
            if !is_slug(object) {
                return Err(invalid("nom d'entité invalide"));
            }
        } else if let Some((source, object)) = s.split_once(':') {
            if !is_slug(source) || !is_slug(object) {
                return Err(invalid("source ou objet invalide"));
            }
            if source == "recorder" {
                return Err(invalid("la source « recorder » est réservée aux entités"));
            }
        } else {
            return Err(invalid("forme inconnue"));
        }
        Ok(StatisticId(s.to_owned()))
    }

    /// Identifiant d'une mesure pour un PCE.
    ///
    /// - Préfixe sans point (par défaut `gazpar`) : statistique externe
    ///   `<prefix>:<pce>_<energy|volume>`.
    /// - Préfixe `sensor.<nom>` : statistique d'entité `sensor.<nom>_<energy|volume>`,
    ///   sans le PCE, pour reprendre l'historique de gazpar2haws
    ///   (`sensor.gazpar2haws` → `sensor.gazpar2haws_energy`).
    pub fn for_measure(prefix: &str, pce: &str, measure: Measure) -> Result<StatisticId, Error> {
        let key = measure.key();
        if prefix.starts_with("sensor.") {
            StatisticId::parse(&format!("{prefix}_{key}"))
        } else {
            StatisticId::parse(&format!("{prefix}:{pce}_{key}"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Statistique externe (`<source>:<objet>`) plutôt qu'entité ?
    pub fn is_external(&self) -> bool {
        !self.0.starts_with("sensor.")
    }

    /// Source à déclarer à l'import : le préfixe avant `:` pour une
    /// statistique externe, `recorder` pour une entité. Home Assistant
    /// refuse un import dont la source ne correspond pas.
    pub fn source(&self) -> &str {
        if self.is_external() {
            self.0.split_once(':').map_or("", |(source, _)| source)
        } else {
            "recorder"
        }
    }
}

/// Partie d'identifiant : `[a-z0-9_]+`, sans `_` au début, à la fin, ni doublé.
fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !s.starts_with('_')
        && !s.ends_with('_')
        && !s.contains("__")
}

impl fmt::Display for StatisticId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for StatisticId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        StatisticId::parse(s)
    }
}

/// Point de statistique quotidien : `sum` est le cumul depuis l'origine.
///
/// `sync` écrit `state = sum` (un compteur, comme gazpar2haws) et ne lit
/// jamais `state` : la consommation d'un jour est la différence de `sum`
/// avec le point précédent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatPoint {
    pub day: Date,
    pub state: Quantity,
    pub sum: Quantity,
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn q(text: &str) -> i64 {
        Quantity::parse_decimal(text)
            .unwrap_or_else(|e| panic!("{text:?} refusé : {e}"))
            .millis()
    }

    fn refused(text: &str) -> bool {
        Quantity::parse_decimal(text).is_err()
    }

    // --- Quantity::parse_decimal : cas nominaux ---

    #[test]
    fn parse_integers() {
        assert_eq!(q("0"), 0);
        assert_eq!(q("12"), 12_000);
        assert_eq!(q("-0"), 0);
        assert_eq!(q("-7"), -7_000);
    }

    #[test]
    fn parse_decimals_up_to_three_places() {
        assert_eq!(q("1.5"), 1_500);
        assert_eq!(q("0.123"), 123);
        assert_eq!(q("0.1"), 100);
        assert_eq!(q("10.05"), 10_050);
        assert_eq!(q("-2.25"), -2_250);
        assert_eq!(q("0.000"), 0);
        assert_eq!(q("-0.0"), 0);
    }

    #[test]
    fn parse_rounds_half_away_from_zero() {
        assert_eq!(q("0.0005"), 1);
        assert_eq!(q("-0.0005"), -1);
        assert_eq!(q("0.00049"), 0);
        assert_eq!(q("-0.00049"), 0);
        assert_eq!(q("0.00050000001"), 1);
        assert_eq!(q("1.2344"), 1_234);
        assert_eq!(q("1.2345"), 1_235);
        assert_eq!(q("1.9999"), 2_000);
        assert_eq!(q("-1.9995"), -2_000);
        assert_eq!(q("0.1234567890123456789012345"), 123);
    }

    #[test]
    fn parse_exponents() {
        assert_eq!(q("1e3"), 1_000_000);
        assert_eq!(q("1E3"), 1_000_000);
        assert_eq!(q("1.5e+2"), 150_000);
        assert_eq!(q("25e-1"), 2_500);
        assert_eq!(q("1e-3"), 1);
        assert_eq!(q("5e-4"), 1);
        assert_eq!(q("4e-4"), 0);
        assert_eq!(q("1e-7"), 0);
        assert_eq!(q("0e999999999999"), 0);
        assert_eq!(q("1e-999999999999"), 0);
    }

    #[test]
    fn parse_serde_json_float_output() {
        // Ce que produit serde_json en relisant un nombre JSON flottant.
        for (value, millis) in [
            (12.345, 12_345),
            (1e-7, 0),
            (0.5, 500),
            (1e15, 10_i64.pow(18)),
        ] {
            let text = serde_json::Number::from_f64(value).unwrap().to_string();
            assert_eq!(q(&text), millis, "{text}");
        }
        let v: serde_json::Value = serde_json::from_str(r#"{"e": 23.1, "v": 2}"#).unwrap();
        assert_eq!(q(&v["e"].to_string()), 23_100);
        assert_eq!(q(&v["v"].to_string()), 2_000);
    }

    #[test]
    fn parse_limits() {
        let max = i64::MAX / 1000;
        assert_eq!(q(&max.to_string()), max * 1000);
        assert!(refused(&(max + 1).to_string()));
        assert!(refused("99999999999999999999"));
        assert!(refused("1e16"));
        assert!(refused("1e99999"));
        assert_eq!(q("0.0000000000000000000000000000001"), 0);
    }

    #[test]
    fn parse_refuses_non_json_numbers() {
        for text in [
            "", " ", " 1", "1 ", "+1", "01", "-01", "00", "1.", ".5", "-.5", "-", "1,5", "1.2.3",
            "abc", "1e", "1e+", "1e1.5", "e3", "NaN", "inf", "0x10", "1_000", "١",
        ] {
            assert!(refused(text), "{text:?} aurait dû être refusé");
        }
    }

    #[test]
    fn parse_error_mentions_value() {
        let e = Quantity::parse_decimal("1,5").unwrap_err();
        assert!(e.to_string().contains("\"1,5\""), "{e}");
    }

    // --- Quantity : opérations ---

    #[test]
    fn arithmetic_and_sum() {
        let a = Quantity::from_millis(1_500);
        let b = Quantity::from_millis(250);
        assert_eq!(a + b, Quantity::from_millis(1_750));
        assert_eq!(b - a, Quantity::from_millis(-1_250));
        assert_eq!(
            [a, b, b].into_iter().sum::<Quantity>(),
            Quantity::from_millis(2_000)
        );
        assert_eq!(
            [a, b].iter().sum::<Quantity>(),
            Quantity::from_millis(1_750)
        );
        assert_eq!(
            Vec::<Quantity>::new().into_iter().sum::<Quantity>(),
            Quantity::ZERO
        );
    }

    #[test]
    fn sum_is_exact_where_floats_drift() {
        let tenth = Quantity::parse_decimal("0.1").unwrap();
        let total: Quantity = std::iter::repeat_n(tenth, 1000).sum();
        assert_eq!(total, Quantity::from_millis(100_000));
    }

    #[test]
    fn to_f64() {
        assert_eq!(Quantity::from_millis(12_345).to_f64(), 12.345);
        assert_eq!(Quantity::from_millis(-1).to_f64(), -0.001);
        assert_eq!(Quantity::ZERO.to_f64(), 0.0);
    }

    #[test]
    fn display() {
        assert_eq!(Quantity::from_millis(1_500).to_string(), "1.5");
        assert_eq!(Quantity::from_millis(12_000).to_string(), "12");
        assert_eq!(Quantity::from_millis(-3).to_string(), "-0.003");
        assert_eq!(Quantity::from_millis(10_050).to_string(), "10.05");
        assert_eq!(Quantity::ZERO.to_string(), "0");
        assert_eq!(
            Quantity::from_millis(i64::MIN).to_string(),
            "-9223372036854775.808"
        );
    }

    // --- Measure, DailyReading ---

    #[test]
    fn measure_metadata() {
        assert_eq!(Measure::Energy.unit(), "kWh");
        assert_eq!(Measure::Energy.unit_class(), "energy");
        assert_eq!(Measure::Volume.unit(), "m³");
        assert_eq!(Measure::Volume.unit_class(), "volume");
        assert_eq!(Measure::ALL.map(Measure::key), ["energy", "volume"]);
    }

    #[test]
    fn reading_get_by_measure() {
        let r = DailyReading {
            day: date(2026, 3, 29),
            energy_kwh: Some(Quantity::from_millis(42_000)),
            volume_m3: None,
        };
        assert_eq!(r.get(Measure::Energy), Some(Quantity::from_millis(42_000)));
        assert_eq!(r.get(Measure::Volume), None);
    }

    // --- StatisticId ---

    #[test]
    fn statistic_id_external() {
        let id = StatisticId::parse("gazpar:12345678901234_energy").unwrap();
        assert!(id.is_external());
        assert_eq!(id.source(), "gazpar");
        assert_eq!(id.as_str(), "gazpar:12345678901234_energy");
        assert_eq!(id.to_string(), "gazpar:12345678901234_energy");
    }

    #[test]
    fn statistic_id_sensor_uses_recorder_source() {
        let id: StatisticId = "sensor.gazpar2haws_volume".parse().unwrap();
        assert!(!id.is_external());
        assert_eq!(id.source(), "recorder");
    }

    #[test]
    fn statistic_id_refusals() {
        for s in [
            "",
            "gazpar",
            "gazpar:",
            ":x",
            "Gazpar:x",
            "gazpar:x-y",
            "gazpar:x:y",
            "gazpar:_x",
            "gazpar:x_",
            "gazpar:x__y",
            "gaz par:x",
            "sensor.",
            "sensor.A",
            "sensor.x.y",
            "light.x",
            "recorder:x",
            "gazpar:é",
        ] {
            assert!(
                StatisticId::parse(s).is_err(),
                "{s:?} aurait dû être refusé"
            );
        }
    }

    #[test]
    fn for_measure_default_prefix() {
        let pce = "12345678901234";
        assert_eq!(
            StatisticId::for_measure("gazpar", pce, Measure::Energy)
                .unwrap()
                .as_str(),
            "gazpar:12345678901234_energy"
        );
        assert_eq!(
            StatisticId::for_measure("gazpar", pce, Measure::Volume)
                .unwrap()
                .as_str(),
            "gazpar:12345678901234_volume"
        );
    }

    #[test]
    fn for_measure_sensor_prefix_takes_over_gazpar2haws() {
        let id = StatisticId::for_measure("sensor.gazpar2haws", "12345678901234", Measure::Energy)
            .unwrap();
        assert_eq!(id.as_str(), "sensor.gazpar2haws_energy");
        assert_eq!(id.source(), "recorder");
    }

    #[test]
    fn for_measure_invalid_prefix() {
        assert!(StatisticId::for_measure("Gaz", "1", Measure::Energy).is_err());
        assert!(StatisticId::for_measure("a:b", "1", Measure::Energy).is_err());
        assert!(StatisticId::for_measure("", "1", Measure::Energy).is_err());
    }
}

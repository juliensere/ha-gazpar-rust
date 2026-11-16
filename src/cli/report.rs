//! Mise en forme de ce qui est affiché (`check`, `reset`) ou journalisé
//! (démarrage, connexion à Home Assistant, compte rendu d'un relevé), en
//! fonctions pures : des données en entrée, du texte en sortie.
//!
//! Aucun secret ne passe par ici : seulement des versions, des dates, des
//! quantités et des identifiants de statistiques (le PCE n'est pas un
//! secret, MANUEL.md §6).
//!
//! Dépendances : `app` (type `HaInfo`), `sync` (type `Report`), `domain`,
//! `log` (niveaux), `jiff` (dates).

use std::fmt::Write as _;

use jiff::civil::Date;

use crate::app::HaInfo;
use crate::cli::Command;
use crate::domain::{Measure, Quantity, StatisticId};
use crate::log::Level;
use crate::sync::{MeasureReport, Report};
use crate::{HA_REFERENCE, VERSION};

/// Nombre de jours lus chez GRDF montrés par `check`.
pub const CHECK_DAYS_SHOWN: usize = 7;

/// Ligne de journal du démarrage : version et commande.
pub fn startup(command: Command, scan_interval_minutes: u32) -> String {
    let what = match command {
        Command::Run => {
            format!("démon : un relevé maintenant, puis toutes les {scan_interval_minutes} min")
        }
        Command::Once => "un relevé".to_owned(),
        Command::Check => "vérification, rien n'est écrit".to_owned(),
        Command::Reset => "effacement des statistiques".to_owned(),
    };
    format!("ha-gazpar {VERSION} (Home Assistant de référence {HA_REFERENCE}) : {what}")
}

/// Lignes de journal après la connexion à Home Assistant : version et
/// fuseau, avertissement si HA est antérieur à la version de référence.
pub fn connected(info: &HaInfo) -> Vec<(Level, String)> {
    let mut lines = vec![(
        Level::Info,
        format!(
            "connecté à Home Assistant {}, fuseau {}",
            info.version, info.time_zone
        ),
    )];
    if let Some(warning) = version_warning(info) {
        lines.push((Level::Warn, warning));
    }
    lines
}

fn version_warning(info: &HaInfo) -> Option<String> {
    match info.older_than_reference {
        Some(true) => Some(format!(
            "Home Assistant {} est antérieur à la version de référence {HA_REFERENCE} : \
             ha-gazpar n'est pas validé avec cette version, mettez Home Assistant à jour",
            info.version
        )),
        Some(false) => None,
        None => Some(format!(
            "version de Home Assistant illisible ({}) : comparaison avec la version de \
             référence {HA_REFERENCE} impossible",
            info.version
        )),
    }
}

/// Lignes de journal du compte rendu d'un relevé (MANUEL.md §7), par
/// exemple « relevé : 3 jours importés, dernier jour connu 2026-11-13 ».
/// Une ligne par mesure si l'énergie et le volume diffèrent.
pub fn reading(report: &Report) -> Vec<(Level, String)> {
    let mut lines = Vec::new();
    if let Some((from, to)) = report.requested {
        let missing = report
            .readings
            .iter()
            .filter(|r| r.energy_kwh.is_none() || r.volume_m3.is_none())
            .count();
        lines.push((
            Level::Debug,
            format!(
                "GRDF : relevés demandés du {from} au {to}, {} dont {} sans valeur",
                counted(report.readings.len(), "reçu"),
                missing
            ),
        ));
    }
    let summary = |m: &MeasureReport| {
        format!(
            "{}, {}",
            counted(m.count(), "importé"),
            last_known(m.last_known)
        )
    };
    let same = report
        .measures
        .windows(2)
        .all(|w| (w[0].count(), w[0].last_known) == (w[1].count(), w[1].last_known));
    match report.measures.first() {
        Some(first) if same => lines.push((Level::Info, format!("relevé : {}", summary(first)))),
        _ => {
            for m in &report.measures {
                lines.push((
                    Level::Info,
                    format!("relevé ({}) : {}", label(m.measure), summary(m)),
                ));
            }
        }
    }
    lines
}

/// Partie Home Assistant de `check`.
pub fn check_home_assistant(info: &HaInfo, known: &[(StatisticId, Option<Date>)]) -> String {
    let mut s = String::new();
    // `write!` sur une String ne peut pas échouer.
    let _ = writeln!(
        s,
        "Home Assistant : connecté, version {}, fuseau {} (date du jour {})",
        info.version, info.time_zone, info.today
    );
    for (id, day) in known {
        let _ = writeln!(s, "  {id} : {}", last_known(*day));
    }
    if let Some(warning) = version_warning(info) {
        let _ = writeln!(s, "ATTENTION : {warning}");
    }
    s
}

/// Partie GRDF de `check` : les derniers jours lus chez GRDF
/// ([`CHECK_DAYS_SHOWN`] au plus) et ce qui serait importé.
pub fn check_grdf(report: &Report) -> String {
    let mut s = String::new();
    let Some((from, to)) = report.requested else {
        let _ = writeln!(
            s,
            "GRDF : rien à demander, tout est déjà connu ; GRDF n'a pas été contacté"
        );
        return s;
    };
    let _ = writeln!(
        s,
        "GRDF : connecté, relevés demandés du {from} au {to}, {}",
        counted(report.readings.len(), "reçu")
    );
    if report.readings.is_empty() {
        let _ = writeln!(s, "Aucun relevé publié par GRDF sur cette période.");
    } else {
        let shown = &report.readings[report.readings.len().saturating_sub(CHECK_DAYS_SHOWN)..];
        let _ = writeln!(s, "Derniers jours lus chez GRDF :");
        let _ = writeln!(
            s,
            "  {:<10}  {:>13}  {:>11}",
            "jour", "énergie (kWh)", "volume (m³)"
        );
        for r in shown {
            let _ = writeln!(
                s,
                "  {:<10}  {:>13}  {:>11}",
                r.day.to_string(),
                value(r.energy_kwh),
                value(r.volume_m3)
            );
        }
    }
    let _ = writeln!(s, "Serait importé (simulation, rien n'est écrit) :");
    for m in &report.measures {
        let what = match (m.first_changed(), m.last_changed()) {
            (Some(first), Some(last)) if first == last => {
                format!("{}, le {first}", counted(m.count(), ""))
            }
            (Some(first), Some(last)) => {
                format!("{}, du {first} au {last}", counted(m.count(), ""))
            }
            _ => "rien".to_owned(),
        };
        let _ = writeln!(s, "  {} : {what}", m.id);
    }
    s
}

/// Ce que `reset` sans `--yes` effacerait.
pub fn reset_preview(ids: &[&StatisticId]) -> String {
    let mut s = String::from("Statistiques qui seraient effacées dans Home Assistant :\n");
    for id in ids {
        let _ = writeln!(s, "  {id}");
    }
    s
}

/// Ce que `reset --yes` a effacé.
pub fn reset_done(ids: &[StatisticId]) -> String {
    let mut s = String::from("Statistiques effacées dans Home Assistant :\n");
    for id in ids {
        let _ = writeln!(s, "  {id}");
    }
    s.push_str(
        "Home Assistant termine l'effacement en arrière-plan. Le prochain relevé \
         réimporte l'historique disponible chez GRDF.\n",
    );
    s
}

/// `3 jours importés`, `1 jour importé`, `aucun jour importé` ; sans
/// participe : `3 jours`.
fn counted(n: usize, participle: &str) -> String {
    let text = match n {
        0 => format!("aucun jour {participle}"),
        1 => format!("1 jour {participle}"),
        n if participle.is_empty() => format!("{n} jours"),
        n => format!("{n} jours {participle}s"),
    };
    text.trim_end().to_owned()
}

fn last_known(day: Option<Date>) -> String {
    match day {
        Some(day) => format!("dernier jour connu {day}"),
        None => "aucun jour connu".to_owned(),
    }
}

fn value(q: Option<Quantity>) -> String {
    q.map_or_else(|| "non publié".to_owned(), |q| q.to_string())
}

fn label(measure: Measure) -> &'static str {
    match measure {
        Measure::Energy => "énergie",
        Measure::Volume => "volume",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DailyReading, StatPoint};
    use crate::sync::Mode;
    use jiff::civil::date;

    fn day(d: i8) -> Date {
        date(2026, 11, d)
    }

    fn id(measure: Measure) -> StatisticId {
        StatisticId::for_measure("gazpar", "98765432109876", measure).unwrap()
    }

    fn kwh(n: i64) -> Quantity {
        Quantity::from_millis(n)
    }

    fn points(days: &[i8]) -> Vec<StatPoint> {
        days.iter()
            .map(|d| StatPoint {
                day: day(*d),
                state: kwh(1000),
                sum: kwh(1000),
            })
            .collect()
    }

    fn measure(measure: Measure, changed: &[i8], last: Option<i8>) -> MeasureReport {
        MeasureReport {
            measure,
            id: id(measure),
            points: points(changed),
            last_known: last.map(day),
        }
    }

    fn report(energy: MeasureReport, volume: MeasureReport) -> Report {
        Report {
            mode: Mode::Apply,
            requested: Some((day(2), day(15))),
            readings: vec![
                DailyReading {
                    day: day(12),
                    energy_kwh: Some(kwh(10_500)),
                    volume_m3: Some(kwh(940)),
                },
                DailyReading {
                    day: day(13),
                    energy_kwh: None,
                    volume_m3: None,
                },
            ],
            measures: vec![energy, volume],
        }
    }

    fn info(version: &str, older: Option<bool>) -> HaInfo {
        HaInfo {
            version: version.into(),
            time_zone: "Europe/Paris".into(),
            older_than_reference: older,
            today: day(16),
        }
    }

    fn texts(lines: &[(Level, String)], level: Level) -> Vec<&str> {
        lines
            .iter()
            .filter(|(l, _)| *l == level)
            .map(|(_, t)| t.as_str())
            .collect()
    }

    #[test]
    fn reading_line_follows_the_manual() {
        let r = report(
            measure(Measure::Energy, &[11, 12, 14], Some(14)),
            measure(Measure::Volume, &[11, 12, 14], Some(14)),
        );
        let lines = reading(&r);
        assert_eq!(
            texts(&lines, Level::Info),
            ["relevé : 3 jours importés, dernier jour connu 2026-11-14"]
        );
        assert_eq!(
            texts(&lines, Level::Debug),
            [
                "GRDF : relevés demandés du 2026-11-02 au 2026-11-15, 2 jours reçus dont 1 sans valeur"
            ]
        );
    }

    #[test]
    fn reading_line_without_news_or_history() {
        let r = report(
            measure(Measure::Energy, &[], Some(14)),
            measure(Measure::Volume, &[], Some(14)),
        );
        assert_eq!(
            texts(&reading(&r), Level::Info),
            ["relevé : aucun jour importé, dernier jour connu 2026-11-14"]
        );
        let r = report(
            measure(Measure::Energy, &[], None),
            measure(Measure::Volume, &[], None),
        );
        assert_eq!(
            texts(&reading(&r), Level::Info),
            ["relevé : aucun jour importé, aucun jour connu"]
        );
    }

    #[test]
    fn reading_lines_per_measure_when_they_differ() {
        let r = report(
            measure(Measure::Energy, &[14], Some(14)),
            measure(Measure::Volume, &[], Some(13)),
        );
        assert_eq!(
            texts(&reading(&r), Level::Info),
            [
                "relevé (énergie) : 1 jour importé, dernier jour connu 2026-11-14",
                "relevé (volume) : aucun jour importé, dernier jour connu 2026-11-13",
            ]
        );
    }

    #[test]
    fn startup_names_version_and_command() {
        let line = startup(Command::Run, 480);
        assert!(line.starts_with(&format!("ha-gazpar {VERSION} (Home Assistant de référence")));
        assert!(
            line.ends_with("un relevé maintenant, puis toutes les 480 min"),
            "{line}"
        );
        assert!(startup(Command::Once, 480).ends_with(" : un relevé"));
    }

    #[test]
    fn connected_warns_about_old_or_unreadable_versions() {
        let lines = connected(&info("2026.9.1", Some(false)));
        assert_eq!(
            lines,
            [(
                Level::Info,
                "connecté à Home Assistant 2026.9.1, fuseau Europe/Paris".to_owned()
            )]
        );
        let lines = connected(&info("2026.8.3", Some(true)));
        let warn = texts(&lines, Level::Warn);
        assert_eq!(warn.len(), 1);
        assert!(
            warn[0].starts_with(&format!(
                "Home Assistant 2026.8.3 est antérieur à la version de référence {HA_REFERENCE}"
            )),
            "{}",
            warn[0]
        );
        let lines = connected(&info("inconnue", None));
        assert!(texts(&lines, Level::Warn)[0].contains("illisible (inconnue)"));
    }

    #[test]
    fn check_home_assistant_section() {
        let known = [
            (id(Measure::Energy), Some(day(13))),
            (id(Measure::Volume), None),
        ];
        assert_eq!(
            check_home_assistant(&info("2026.9.1", Some(false)), &known),
            "Home Assistant : connecté, version 2026.9.1, fuseau Europe/Paris \
             (date du jour 2026-11-16)\n\
             \x20 gazpar:98765432109876_energy : dernier jour connu 2026-11-13\n\
             \x20 gazpar:98765432109876_volume : aucun jour connu\n"
        );
        let old = check_home_assistant(&info("2026.8.3", Some(true)), &known);
        assert!(
            old.lines()
                .last()
                .unwrap()
                .starts_with("ATTENTION : Home Assistant 2026.8.3")
        );
    }

    #[test]
    fn check_grdf_section() {
        let r = report(
            measure(Measure::Energy, &[11, 12, 14], Some(14)),
            measure(Measure::Volume, &[12], Some(14)),
        );
        assert_eq!(
            check_grdf(&r),
            "GRDF : connecté, relevés demandés du 2026-11-02 au 2026-11-15, 2 jours reçus\n\
             Derniers jours lus chez GRDF :\n\
             \x20 jour        énergie (kWh)  volume (m³)\n\
             \x20 2026-11-12           10.5         0.94\n\
             \x20 2026-11-13     non publié   non publié\n\
             Serait importé (simulation, rien n'est écrit) :\n\
             \x20 gazpar:98765432109876_energy : 3 jours, du 2026-11-11 au 2026-11-14\n\
             \x20 gazpar:98765432109876_volume : 1 jour, le 2026-11-12\n"
        );
    }

    #[test]
    fn check_grdf_shows_the_last_seven_days_at_most() {
        let mut r = report(
            measure(Measure::Energy, &[], Some(14)),
            measure(Measure::Volume, &[], Some(14)),
        );
        r.readings = (1..=10)
            .map(|d| DailyReading {
                day: day(d),
                energy_kwh: Some(kwh(1000)),
                volume_m3: Some(kwh(100)),
            })
            .collect();
        let text = check_grdf(&r);
        let days: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("  2026-"))
            .map(|l| &l[2..12])
            .collect();
        assert_eq!(days.len(), CHECK_DAYS_SHOWN);
        assert_eq!(days[0], "2026-11-04");
        assert_eq!(days[6], "2026-11-10");
        assert!(text.contains("_energy : rien\n"), "{text}");
        r.readings.clear();
        assert!(check_grdf(&r).contains("Aucun relevé publié par GRDF"));
    }

    #[test]
    fn reset_messages() {
        let (e, v) = (id(Measure::Energy), id(Measure::Volume));
        assert_eq!(
            reset_preview(&[&e, &v]),
            "Statistiques qui seraient effacées dans Home Assistant :\n  \
             gazpar:98765432109876_energy\n  gazpar:98765432109876_volume\n"
        );
        let done = reset_done(&[e, v]);
        assert!(done.starts_with(
            "Statistiques effacées dans Home Assistant :\n  gazpar:98765432109876_energy\n"
        ));
    }

    #[test]
    fn counting_agrees_in_number() {
        assert_eq!(counted(0, "importé"), "aucun jour importé");
        assert_eq!(counted(1, "importé"), "1 jour importé");
        assert_eq!(counted(2, "importé"), "2 jours importés");
        assert_eq!(counted(0, ""), "aucun jour");
        assert_eq!(counted(5, ""), "5 jours");
    }
}

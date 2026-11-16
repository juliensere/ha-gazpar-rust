//! Journal minimal sur la sortie d'erreur, sans dépendance de journalisation.
//!
//! Chaque ligne : horodatage RFC 3339 en UTC à la seconde, niveau, message.
//! Deux niveaux réglables (`info`, `debug`), fixés une seule fois au
//! démarrage par [`init`] ; `warn!` et `error!` sont toujours écrits.
//! Pas de fichier : Docker conserve la sortie d'erreur.
//!
//! Un seul chemin d'écriture, [`write_to`] (filtrage par niveau, horodatage,
//! mise en forme), emprunté par deux portes :
//!
//! - les macros `error!`, `warn!`, `info!`, `debug!` écrivent **toujours sur
//!   la vraie sortie d'erreur du processus** ([`std::io::stderr`]) : c'est
//!   ainsi que `grdf` et `ha` journalisent leurs détails (reprises, session
//!   expirée, commandes lues), même quand `cli` travaille sur une sortie
//!   injectée (tests) ;
//! - le journal de `cli` écrit sur la sortie d'erreur qu'on lui donne, par
//!   [`write_to`], au même format.
//!
//! Ne jamais passer de secret en clair à ces macros : les secrets sont des
//! [`crate::secret::Secret`], qui s'affichent `***`.
//!
//! Dépendances : `jiff` (horodatage) ; aucun autre module du programme.

use std::fmt;
use std::io::Write;
use std::sync::OnceLock;

/// Seuil de journalisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    /// Lit un niveau réglable par l'utilisateur : `info` ou `debug`.
    pub fn parse(s: &str) -> Option<Level> {
        match s.trim().to_ascii_lowercase().as_str() {
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Level::Error => "ERREUR",
            Level::Warn => "ATTENTION",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
        })
    }
}

static LEVEL: OnceLock<Level> = OnceLock::new();

/// Fixe le niveau global. Seul le premier appel compte ; renvoie `false` si
/// le niveau était déjà fixé.
pub fn init(level: Level) -> bool {
    LEVEL.set(level).is_ok()
}

/// Niveau courant (`info` tant que [`init`] n'a pas été appelé).
fn level() -> Level {
    LEVEL.get().copied().unwrap_or(Level::Info)
}

/// Le message de ce niveau serait-il écrit ?
fn enabled(level: Level) -> bool {
    level <= self::level()
}

/// Met en forme une ligne de journal (fonction pure, testée).
fn format_line(now: jiff::Timestamp, level: Level, args: fmt::Arguments<'_>) -> String {
    format!("{now:.0} {} {args}", level.label())
}

/// Écrit une ligne sur `sink` si le niveau est actif : seul chemin
/// d'écriture du journal.
pub fn write_to(sink: &mut dyn Write, level: Level, args: fmt::Arguments<'_>) {
    if enabled(level) {
        let line = format_line(jiff::Timestamp::now(), level, args);
        // Une erreur d'écriture du journal n'a nulle part où être signalée.
        let _ = writeln!(sink, "{line}");
    }
}

/// Écrit une ligne sur la sortie d'erreur du processus si le niveau est
/// actif. Utiliser plutôt les macros.
pub fn write(level: Level, args: fmt::Arguments<'_>) {
    write_to(&mut std::io::stderr().lock(), level, args);
}

/// Journalise une erreur (toujours écrite).
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Error, format_args!($($arg)*)) };
}

/// Journalise un avertissement (toujours écrit).
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Warn, format_args!($($arg)*)) };
}

/// Journalise une information (niveau `info`).
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Info, format_args!($($arg)*)) };
}

/// Journalise un détail de diagnostic (niveau `debug`).
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Debug, format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::Secret;

    fn ts(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn line_is_rfc3339_utc_to_the_second() {
        let line = format_line(
            ts("2026-11-16T07:05:09.123456Z"),
            Level::Info,
            format_args!("relevé de {} jours", 14),
        );
        assert_eq!(line, "2026-11-16T07:05:09Z INFO relevé de 14 jours");
    }

    #[test]
    fn labels() {
        let t = ts("2026-01-01T00:00:00Z");
        assert!(format_line(t, Level::Debug, format_args!("x")).contains(" DEBUG x"));
        assert!(format_line(t, Level::Warn, format_args!("x")).contains(" ATTENTION x"));
        assert!(format_line(t, Level::Error, format_args!("x")).contains(" ERREUR x"));
    }

    #[test]
    fn secret_stays_masked_in_line() {
        let token = Secret::new("tok123");
        let line = format_line(
            ts("2026-01-01T00:00:00Z"),
            Level::Debug,
            format_args!("jeton {token}"),
        );
        assert!(!line.contains("tok123"));
    }

    #[test]
    fn parse_user_levels() {
        assert_eq!(Level::parse("info"), Some(Level::Info));
        assert_eq!(Level::parse("DEBUG"), Some(Level::Debug));
        assert_eq!(Level::parse(" debug "), Some(Level::Debug));
        assert_eq!(Level::parse("warn"), None);
        assert_eq!(Level::parse("trace"), None);
        assert_eq!(Level::parse(""), None);
    }

    #[test]
    fn ordering_drives_filtering() {
        assert!(Level::Error < Level::Info);
        assert!(Level::Info < Level::Debug);
        // Sans init() dans ce test, le niveau par défaut est au moins info.
        assert!(enabled(Level::Error));
        assert!(enabled(Level::Info));
    }

    #[test]
    fn write_to_uses_the_line_format() {
        let mut sink = Vec::new();
        write_to(
            &mut sink,
            Level::Error,
            format_args!("relevé : {}", "échec"),
        );
        write_to(&mut sink, Level::Warn, format_args!("deux"));
        let text = String::from_utf8(sink).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        let (stamp, rest) = lines[0].split_once(' ').unwrap();
        assert!(stamp.parse::<jiff::Timestamp>().is_ok(), "{stamp}");
        assert!(stamp.ends_with('Z') && !stamp.contains('.'), "{stamp}");
        assert_eq!(rest, "ERREUR relevé : échec");
        assert!(lines[1].ends_with(" ATTENTION deux"), "{text}");
    }

    #[test]
    fn macros_compile_and_run() {
        // Niveau debug inactif par défaut : rien n'est écrit pendant les tests.
        crate::debug!("message de test {}", 1);
    }
}

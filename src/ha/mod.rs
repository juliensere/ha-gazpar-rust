//! Client Home Assistant : implémente [`crate::ports::StatisticsStore`]
//! (étape 2).
//!
//! Ne connaît pas `grdf`. Convertit les jours en minuit dans le fuseau
//! configuré (et inversement).
//!
//! Dépendances : `domain`, `ports`, `secret`, `error`, `log`, `serde_json`,
//! `jiff`.

pub mod session;
pub mod stats;
pub mod websocket;

//! Client Home Assistant : implémente [`crate::ports::StatisticsStore`]
//! avec [`stats::HomeAssistantStore`].
//!
//! Trois couches, chacune ignorant la suivante : [`websocket`] (RFC 6455,
//! bibliothèque standard), [`session`] (authentification et commandes
//! numérotées de l'API WebSocket de HA), [`stats`] (statistiques long terme).
//!
//! Ne connaît pas `grdf`. Convertit les jours en minuit dans le fuseau
//! configuré (et inversement).
//!
//! Dépendances : `domain`, `ports`, `secret`, `error`, `log`, `serde_json`,
//! `jiff`.

pub mod session;
pub mod stats;
pub mod websocket;

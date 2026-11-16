//! Client Home Assistant : implémente [`crate::ports::StatisticsStore`]
//! avec [`stats::HomeAssistantStore`].
//!
//! Trois couches, chacune ignorant la suivante : [`websocket`] (RFC 6455,
//! bibliothèque standard), [`session`] (authentification et commandes
//! numérotées de l'API WebSocket de HA), [`stats`] (statistiques long terme).
//! À côté, [`server`] lit le fuseau horaire de HA (`get_config`) et compare
//! sa version à la version de référence.
//!
//! Ne connaît pas `grdf`. Convertit les jours en minuit dans le fuseau de
//! Home Assistant, lu à la connexion (et inversement).
//!
//! Dépendances : `domain`, `ports`, `secret`, `error`, `log`, `serde_json`,
//! `jiff`.

pub mod server;
pub mod session;
pub mod stats;
pub mod websocket;

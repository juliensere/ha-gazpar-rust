//! Client Home Assistant : implémente [`crate::ports::StatisticsStore`]
//! avec [`stats::HomeAssistantStore`].
//!
//! Trois couches, chacune ignorant la suivante : [`websocket`] (RFC 6455,
//! bibliothèque standard), [`session`] (authentification et commandes
//! numérotées de l'API WebSocket de HA), [`stats`] (statistiques long terme).
//! À côté, [`server`] lit le fuseau horaire de HA (`get_config`) et compare
//! sa version à la version de référence.
//!
//! Seuls [`stats::HomeAssistantStore`], [`Timeouts`] et
//! [`older_than_reference`] sont publics ; les couches `websocket`,
//! `session` et `server` sont internes, et ce dont les essais d'intégration
//! ont besoin passe par la frontière [`crate::testing`].
//!
//! Ne connaît pas `grdf`. Convertit les jours en minuit dans le fuseau de
//! Home Assistant, lu à la connexion (et inversement).
//!
//! Dépendances : `domain`, `ports`, `secret`, `error`, `log`, `serde_json`,
//! `jiff`.

pub(crate) mod server;
pub(crate) mod session;
pub mod stats;
pub(crate) mod websocket;

pub use server::older_than_reference;
pub use websocket::Timeouts;

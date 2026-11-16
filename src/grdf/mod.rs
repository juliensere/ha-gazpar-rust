//! Client Mon Espace GRDF : implémente [`crate::ports::ConsumptionSource`]
//! (étape 4).
//!
//! Ne connaît pas `ha`. Seul `grdf/http.rs` importe `ureq`.
//!
//! Dépendances : `domain`, `ports`, `readings`, `secret`, `error`, `log`.

pub mod api;
pub mod auth;
pub mod http;

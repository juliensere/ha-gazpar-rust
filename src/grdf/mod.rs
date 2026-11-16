//! Client Mon Espace GRDF : implémente [`crate::ports::ConsumptionSource`]
//! (étape 4).
//!
//! - `url` et `cookies` : adresses et bocal à cookies, fonctions pures ;
//! - `http` : transport, seul module qui importe `ureq` ;
//! - `auth` : connexion Okta IDX ;
//! - `api` : appels, reprises, expiration de session.
//!
//! Ne connaît pas `ha`, `sync` ni `cli`. Les valeurs de configuration sont
//! reçues en paramètres.
//!
//! Dépendances : `domain`, `ports`, `readings`, `secret`, `error`, `log`,
//! `jiff`, `serde_json`, `ureq` (dans `http` seulement).

pub mod api;
pub mod auth;
pub mod cookies;
pub mod http;
pub mod url;

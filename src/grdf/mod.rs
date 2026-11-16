//! Client Mon Espace GRDF : implémente [`crate::ports::ConsumptionSource`]
//! (étape 4).
//!
//! - `cookies` : bocal à cookies, fonctions pures (adresses : [`crate::url`]) ;
//! - `http` : transport, seul module qui importe `ureq` ;
//! - `auth` : connexion Okta IDX ;
//! - `api` : appels, reprises, expiration de session.
//!
//! Ne connaît pas `ha`, `sync` ni `cli`. Les valeurs de configuration sont
//! reçues en paramètres.
//!
//! Dépendances : `url`, `domain`, `ports`, `readings`, `secret`, `error`,
//! `log`, `jiff`, `serde_json`, `ureq` (dans `http` seulement).

pub mod api;
pub mod auth;
pub(crate) mod cookies;
pub(crate) mod http;

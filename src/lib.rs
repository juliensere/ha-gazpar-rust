//! ha-gazpar : importe la consommation de gaz GRDF (Gazpar) dans les
//! statistiques long terme de Home Assistant.
//!
//! Toute la logique vit dans cette bibliothèque ; `main.rs` ne fait que lire
//! les arguments et appeler [`cli::main`].
//!
//! Règles de dépendance entre modules (PLAN.md §6) :
//!
//! - `domain` ne dépend que de `jiff` (dates) et de `error` ;
//!   `ports` ne dépend que de `domain` et `error`.
//! - `sync` et `readings` ne font aucune entrée-sortie : ni réseau, ni
//!   horloge, ni environnement.
//! - `url` est le seul analyseur d'adresse (`http`, `https`, `ws`) : module
//!   neutre, bibliothèque standard seulement, partagé par `grdf`, `ha` et
//!   `config` ; il ne dépend que de `error`.
//! - `grdf` et `ha` ne se connaissent pas ; seul `app` (racine de
//!   composition) les assemble et construit les vrais clients. `cli` analyse
//!   la ligne de commande, aiguille et met en forme ; `daemon` cadence les
//!   relevés de `run`.
//! - Seul `grdf/http.rs` importe `ureq`.

#![forbid(unsafe_code)]

// Déclaré en premier : ses macros sont alors visibles dans les autres modules.
#[macro_use]
pub mod log;

// API publique : ce dont `main.rs` et les essais d'intégration (`tests/`)
// ont besoin, rien de plus. Le reste est interne à la bibliothèque.
pub mod cli;
pub mod domain;
pub mod error;
pub mod grdf;
pub mod ha;
pub mod ports;
pub mod secret;

pub(crate) mod app;
pub(crate) mod config;
pub(crate) mod config_file;
pub(crate) mod daemon;
pub(crate) mod readings;
pub(crate) mod sync;
pub(crate) mod url;

pub use error::Error;

/// Frontière réservée aux essais d'intégration (`tests/`) : les couches du
/// client Home Assistant dont ont besoin le faux Home Assistant de
/// `tests/support/` (WebSocket côté serveur) et les essais contre un vrai
/// Home Assistant (session brute, lecture du fuseau). **Ne fait pas partie
/// de l'API** : le programme lui-même n'utilise que `ha::stats`.
///
/// Rien ici n'est une dépendance de plus : ce sont des ré-exportations de
/// modules internes.
#[doc(hidden)]
pub mod testing {
    pub use crate::ha::server::time_zone_from_config;
    pub use crate::ha::session::{Session, Transport};
    pub use crate::ha::websocket::{Role, WebSocket, accept_key, connect};
}

/// Version du programme, tirée de `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Version de Home Assistant de référence (`AAAA.M`), c'est-à-dire les deux
/// premiers nombres de la version du programme (PLAN.md §10.1). Le `Makefile`
/// vérifie qu'elle concorde avec `HA_VERSION`.
pub const HA_REFERENCE: &str = concat!(
    env!("CARGO_PKG_VERSION_MAJOR"),
    ".",
    env!("CARGO_PKG_VERSION_MINOR")
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ha_reference_is_version_prefix() {
        assert!(VERSION.starts_with(&format!("{HA_REFERENCE}.")));
        assert_eq!(HA_REFERENCE.split('.').count(), 2);
    }
}

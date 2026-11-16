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
//! - `grdf` et `ha` ne se connaissent pas ; seul `app` (racine de
//!   composition) les assemble et construit les vrais clients. `cli` analyse
//!   la ligne de commande, aiguille et met en forme ; `daemon` cadence les
//!   relevés de `run`.
//! - Seul `grdf/http.rs` importe `ureq`.

#![forbid(unsafe_code)]

// Déclaré en premier : ses macros sont alors visibles dans les autres modules.
#[macro_use]
pub mod log;

pub mod app;
pub mod cli;
pub mod config;
pub mod config_file;
pub mod daemon;
pub mod domain;
pub mod error;
pub mod grdf;
pub mod ha;
pub mod ports;
pub mod readings;
pub mod secret;
pub mod sync;

pub use error::Error;

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

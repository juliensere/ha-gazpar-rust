//! Faux serveurs pour les essais hors ligne (PLAN.md §6, §8), bibliothèque
//! standard uniquement. Chaque fichier de `tests/` qui en a besoin déclare
//! `mod support;` ; un faux serveur inutilisé par un fichier de test donné
//! n'y est pas une erreur.

#![allow(dead_code)]

pub mod fake_grdf;
pub mod fake_ha;

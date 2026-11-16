//! Appels à l'API e-conso de GRDF (PLAN.md §2.2) — à écrire à l'étape 4.
//!
//! Liste des PCE, relevés informatifs ou publiés, détection d'une session
//! expirée (une seule reconnexion), reprises sur erreur serveur (3/6/12/15 s)
//! et `Retry-After` plafonné à 60 s. Construction des URL et calcul des
//! délais par des fonctions pures.
//!
//! Dépendances : `grdf::http`, `grdf::auth`, `readings`, `domain`, `ports`,
//! `error`, `log`.

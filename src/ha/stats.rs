//! Statistiques long terme de Home Assistant — à écrire à l'étape 2.
//!
//! `recorder/statistics_during_period` (lecture des points quotidiens),
//! `recorder/import_statistics` (métadonnées : `source`, `unit_class`,
//! `mean_type`… ; points `start`/`state`/`sum`, `start` = minuit dans le
//! fuseau configuré), `recorder/clear_statistics`.
//!
//! Dépendances : `ha::session`, `domain`, `ports`, `error`, `serde_json`,
//! `jiff`.

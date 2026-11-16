//! Connexion à Mon Espace GRDF par le flux Okta IDX (PLAN.md §2.1) — à écrire
//! à l'étape 4.
//!
//! Extraction de `stateToken` (avec `\x2D` → `-`), `identify`, puis
//! `challenge/answer`, puis `success.href`. Le `stateHandle` et le mot de
//! passe sont des secrets : jamais journalisés, jamais dans une erreur.
//! L'analyse des réponses est faite par des fonctions pures, testées sans
//! réseau.
//!
//! Dépendances : `grdf::http`, `secret`, `error`, `log`, `serde_json`.

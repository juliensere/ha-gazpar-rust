//! Transport HTTP vers GRDF — à écrire à l'étape 4.
//!
//! Seul module du programme qui importe `ureq` : requêtes avec délais
//! (10 s de connexion, 60 s de lecture), bocal à cookies maison (domaine,
//! chemin, remplacement, expiration) et redirections suivies à la main pour
//! lire `Set-Cookie` à chaque saut, avec une limite de sauts.
//!
//! Dépendances : `ureq`, `error`, `log`.

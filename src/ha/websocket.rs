//! Client WebSocket RFC 6455 minimal, bibliothèque standard uniquement — à
//! écrire à l'étape 2.
//!
//! `ws://` seulement : poignée de main, trames texte masquées, longueurs
//! 7/16/64 bits, fragmentation, ping/pong, fermeture. Le pair est le Home
//! Assistant de l'utilisateur.
//!
//! Dépendances : `error` (et `std::net`).

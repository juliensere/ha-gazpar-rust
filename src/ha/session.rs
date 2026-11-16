//! Session de l'API WebSocket de Home Assistant — à écrire à l'étape 2.
//!
//! `auth_required` → `auth` (jeton, jamais journalisé) → `auth_ok`, puis
//! commandes numérotées par `id` croissant et lecture de la réponse
//! correspondante ; une réponse `success: false` devient une
//! `Error::HomeAssistant`.
//!
//! Dépendances : `ha::websocket`, `secret`, `error`, `log`, `serde_json`.

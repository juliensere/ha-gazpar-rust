//! Transformation des relevés GRDF (JSON) en [`crate::domain::DailyReading`]
//! — à écrire à l'étape 3.
//!
//! Fonction pure : reçoit une `serde_json::Value` déjà lue, renvoie les
//! relevés quotidiens triés. Les valeurs décimales sont converties en
//! millièmes par `Quantity::parse_decimal` ; un enregistrement invalide est
//! ignoré, un jour sans valeur reste sans valeur.
//!
//! Aucune entrée-sortie. Dépendances : `domain`, `error`, `serde_json`.

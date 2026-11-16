//! Type d'erreur unique du programme.
//!
//! Une variante par domaine ; chaque variante porte un message en français
//! déjà mis en forme. Règle : **un message d'erreur ne contient jamais de
//! secret** (mot de passe, jeton, `stateHandle`). Les secrets sont des
//! [`crate::secret::Secret`], qui s'affichent `***` même si on les formate
//! par erreur.
//!
//! Dépendances : aucune (bibliothèque standard).

use std::fmt;

/// Erreur du programme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Configuration invalide : la liste de tous les problèmes relevés.
    Config(Vec<String>),
    /// Ligne de commande invalide.
    Usage(String),
    /// Entrée-sortie locale (fichier, socket).
    Io(String),
    /// Transport HTTP vers GRDF (connexion, TLS, délai).
    Http(String),
    /// Échec de la connexion à Mon Espace GRDF (identifiants, flux Okta).
    GrdfLogin(String),
    /// Réponse inattendue de l'API GRDF.
    Grdf(String),
    /// Erreur renvoyée par Home Assistant.
    HomeAssistant(String),
    /// Violation de protocole (WebSocket, JSON, format inattendu).
    Protocol(String),
    /// Valeur du domaine invalide (quantité, identifiant de statistique).
    Invalid(String),
    /// Fonction prévue mais pas encore écrite.
    NotImplemented(&'static str),
}

impl Error {
    /// Code de sortie du processus : 2 pour une erreur de configuration ou
    /// d'usage (inutile de relancer sans corriger), 1 sinon.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Config(_) | Error::Usage(_) => 2,
            _ => 1,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(problems) => {
                f.write_str("configuration invalide")?;
                for p in problems {
                    write!(f, "\n  - {p}")?;
                }
                Ok(())
            }
            Error::Usage(m) => write!(f, "usage incorrect : {m}"),
            Error::Io(m) => write!(f, "entrée-sortie : {m}"),
            Error::Http(m) => write!(f, "HTTP : {m}"),
            Error::GrdfLogin(m) => write!(f, "connexion à GRDF refusée : {m}"),
            Error::Grdf(m) => write!(f, "API GRDF : {m}"),
            Error::HomeAssistant(m) => write!(f, "Home Assistant : {m}"),
            Error::Protocol(m) => write!(f, "protocole : {m}"),
            Error::Invalid(m) => write!(f, "valeur invalide : {m}"),
            Error::NotImplemented(what) => write!(f, "pas encore implémenté : {what}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::Secret;

    #[test]
    fn config_lists_every_problem() {
        let e = Error::Config(vec!["A manquant".into(), "B invalide".into()]);
        assert_eq!(
            e.to_string(),
            "configuration invalide\n  - A manquant\n  - B invalide"
        );
    }

    #[test]
    fn display_is_french_and_prefixed() {
        assert_eq!(
            Error::GrdfLogin("mot de passe incorrect".into()).to_string(),
            "connexion à GRDF refusée : mot de passe incorrect"
        );
        assert_eq!(
            Error::NotImplemented("run").to_string(),
            "pas encore implémenté : run"
        );
        assert_eq!(
            Error::HomeAssistant("x".into()).to_string(),
            "Home Assistant : x"
        );
    }

    #[test]
    fn exit_codes() {
        assert_eq!(Error::Config(vec![]).exit_code(), 2);
        assert_eq!(Error::Usage("x".into()).exit_code(), 2);
        assert_eq!(Error::Http("x".into()).exit_code(), 1);
        assert_eq!(Error::NotImplemented("x").exit_code(), 1);
    }

    #[test]
    fn secret_formatted_into_message_stays_masked() {
        let token = Secret::new("eyJhbGciOi.secret");
        let e = Error::HomeAssistant(format!("jeton {token} refusé"));
        let shown = format!("{e} {e:?}");
        assert!(!shown.contains("eyJ"), "{shown}");
    }

    #[test]
    fn from_io_error() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "absent");
        assert_eq!(Error::from(io), Error::Io("absent".into()));
    }

    #[test]
    fn is_std_error() {
        fn takes(_: &dyn std::error::Error) {}
        takes(&Error::Io("x".into()));
    }
}

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
    /// Entrée-sortie (fichier, socket). Une connexion à Home Assistant
    /// impossible le dit dès sa source (`ha::websocket::connect`) :
    /// « Home Assistant injoignable (…) ».
    Io(String),
    /// Transport HTTP vers GRDF (connexion, TLS, délai). Le message nomme
    /// GRDF dès sa source (`grdf::http`) : « GRDF injoignable (…) ».
    Http(String),
    /// Échec de la connexion à Mon Espace GRDF qui ne prouve pas un refus
    /// des identifiants (page inattendue, étape d'Okta absente, session
    /// expirée…) : un nouvel essai au relevé suivant est permis.
    GrdfLogin(String),
    /// GRDF refuse clairement les identifiants (mot de passe faux, compte
    /// inconnu). `run` cesse alors toute connexion à GRDF jusqu'à son
    /// redémarrage (MANUEL.md §9). Voir `grdf::auth::credentials_rejected`.
    GrdfCredentialsRejected(String),
    /// Réponse inattendue de l'API GRDF.
    Grdf(String),
    /// Erreur renvoyée par Home Assistant.
    HomeAssistant(String),
    /// Violation de protocole (WebSocket, JSON, format inattendu).
    Protocol(String),
    /// Valeur du domaine invalide (quantité, identifiant de statistique).
    Invalid(String),
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

    /// Panne passagère : le même essai, sans rien changer, peut réussir plus
    /// tard (GRDF ou Home Assistant injoignable, délai dépassé). Seule
    /// classification des erreurs du programme :
    ///
    /// - le démon journalise une erreur passagère en `ATTENTION`, toute
    ///   autre en `ERREUR` (elle demande sans doute une action) ;
    /// - `grdf::api` ne reprend aussitôt (délais de 3, 6, 12 s) qu'un appel
    ///   en erreur passagère.
    ///
    /// « Non passagère » ne veut pas dire « abandonnée » : le démon retente
    /// toute erreur au relevé suivant, sauf
    /// [`Error::GrdfCredentialsRejected`], seul cas définitif (plus aucune
    /// connexion à GRDF jusqu'au redémarrage, pour ne pas faire bloquer le
    /// compte). En particulier, un jeton Home Assistant refusé
    /// ([`Error::HomeAssistant`]) est retenté à chaque intervalle : c'est
    /// inoffensif (une connexion par intervalle, aucun compte tiers en jeu).
    ///
    /// Le `match` est exhaustif : une nouvelle variante doit être classée ici.
    pub fn is_transient(&self) -> bool {
        match self {
            // Réseau : GRDF ou Home Assistant injoignable, délai dépassé.
            Error::Http(_) | Error::Io(_) => true,
            // Définitif pour le démon : plus aucune connexion à GRDF.
            Error::GrdfCredentialsRejected(_) => false,
            // Jeton refusé, commande refusée, fuseau inconnu : action
            // requise, mais retenté à chaque intervalle (inoffensif).
            Error::HomeAssistant(_) => false,
            // Réponse inattendue : passer en ERREUR, retenter au relevé
            // suivant seulement.
            Error::GrdfLogin(_) | Error::Grdf(_) | Error::Protocol(_) | Error::Invalid(_) => false,
            // Détectées avant tout relevé : le programme sort (code 2).
            Error::Config(_) | Error::Usage(_) => false,
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
            Error::GrdfLogin(m) => write!(f, "échec de la connexion à GRDF : {m}"),
            Error::GrdfCredentialsRejected(m) => write!(
                f,
                "connexion à GRDF refusée : {m} ; vérifiez GRDF_USERNAME et GRDF_PASSWORD \
                 (identifiants de Mon Espace GRDF)"
            ),
            Error::Grdf(m) => write!(f, "API GRDF : {m}"),
            Error::HomeAssistant(m) => write!(f, "Home Assistant : {m}"),
            Error::Protocol(m) => write!(f, "protocole : {m}"),
            Error::Invalid(m) => write!(f, "valeur invalide : {m}"),
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
            Error::GrdfLogin("page d'accueil : statut HTTP 503".into()).to_string(),
            "échec de la connexion à GRDF : page d'accueil : statut HTTP 503"
        );
        assert_eq!(
            Error::GrdfCredentialsRejected("mot de passe refusé".into()).to_string(),
            "connexion à GRDF refusée : mot de passe refusé ; vérifiez GRDF_USERNAME et \
             GRDF_PASSWORD (identifiants de Mon Espace GRDF)"
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
        assert_eq!(Error::GrdfCredentialsRejected("x".into()).exit_code(), 1);
    }

    #[test]
    fn only_network_failures_are_transient() {
        assert!(Error::Http("GRDF injoignable (délai dépassé)".into()).is_transient());
        assert!(Error::Io("Home Assistant injoignable (refusé)".into()).is_transient());
        for e in [
            Error::Config(vec![]),
            Error::Usage("x".into()),
            Error::GrdfLogin("x".into()),
            Error::GrdfCredentialsRejected("x".into()),
            Error::Grdf("x".into()),
            Error::HomeAssistant("authentification refusée".into()),
            Error::Protocol("x".into()),
            Error::Invalid("x".into()),
        ] {
            assert!(!e.is_transient(), "{e:?}");
        }
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

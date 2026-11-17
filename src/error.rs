//! Type d'erreur unique du programme.
//!
//! Une variante par domaine ; chaque variante porte un message en français
//! déjà mis en forme. Règle : **un message d'erreur ne contient jamais de
//! secret** (mot de passe, jeton, `stateHandle`). Les secrets sont des
//! [`crate::secret::Secret`], qui s'affichent `***` même si on les formate
//! par erreur.
//!
//! **Contexte.** Une erreur remonte en se replaçant dans l'opération qui l'a
//! produite ([`Error::context`]) : chaque couche ajoute ce qu'elle seule sait
//! (l'étape de connexion, la fenêtre de dates, la statistique…) devant le
//! message, sans changer de variante, donc sans changer la classification
//! ([`Error::is_transient`]) ni le code de sortie ([`Error::exit_code`]). Le
//! contexte est une valeur typée de l'appelant (une étape, une opération) qui
//! s'affiche en français. Une ligne d'erreur complète se lit ainsi :
//!
//! ```text
//! <service>, <opération> : <cause, avec l'hôte et le port> ; <que faire>
//! ```
//!
//! Exemple : « GRDF, connexion 3/5 (identification Okta) : pas de réponse de
//! connexion.grdf.fr:443 en 60 s ». Jamais d'adresse complète : l'hôte et le
//! port seulement, le chemin et la requête pouvant porter un jeton.
//!
//! [`io_cause`] traduit une erreur d'entrée-sortie en français, pour `grdf`
//! comme pour `ha`, qui partagent ainsi le même vocabulaire sans se
//! connaître.
//!
//! Dépendances : aucune (bibliothèque standard).

use std::fmt;
use std::io;

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
    /// Transport HTTP vers GRDF (connexion, TLS, délai) : la cause et l'hôte
    /// visé (`grdf::http`), précédés de l'opération par le client GRDF.
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

    /// Replace l'erreur dans l'opération qui l'a produite : le message
    /// devient « `context` : message ». Même variante, donc même
    /// classification et même code de sortie. Sans effet sur
    /// [`Error::Config`], qui est une liste de problèmes.
    pub fn context(self, context: impl fmt::Display) -> Error {
        self.map_message(|m| format!("{context} : {m}"))
    }

    /// Transforme le message, dans la même variante (sans effet sur
    /// [`Error::Config`]). Base de [`Error::context`] ; sert aussi à
    /// compléter une cause (« abandon après 4 tentatives »).
    pub fn map_message(self, f: impl FnOnce(String) -> String) -> Error {
        match self {
            Error::Config(problems) => Error::Config(problems),
            Error::Usage(m) => Error::Usage(f(m)),
            Error::Io(m) => Error::Io(f(m)),
            Error::Http(m) => Error::Http(f(m)),
            Error::GrdfLogin(m) => Error::GrdfLogin(f(m)),
            Error::GrdfCredentialsRejected(m) => Error::GrdfCredentialsRejected(f(m)),
            Error::Grdf(m) => Error::Grdf(f(m)),
            Error::HomeAssistant(m) => Error::HomeAssistant(f(m)),
            Error::Protocol(m) => Error::Protocol(f(m)),
            Error::Invalid(m) => Error::Invalid(f(m)),
        }
    }
}

/// Cause d'une erreur d'entrée-sortie, en français : « connexion refusée »,
/// « délai dépassé »… Le texte du système (souvent en anglais) n'est repris
/// que pour un type d'erreur sans traduction (échec de résolution DNS, par
/// exemple). Le délai lui-même est ajouté par l'appelant, qui le connaît.
pub fn io_cause(e: &io::Error) -> String {
    use io::ErrorKind::*;
    let known = match e.kind() {
        ConnectionRefused => "connexion refusée",
        ConnectionReset => "connexion réinitialisée par le pair",
        ConnectionAborted => "connexion interrompue",
        BrokenPipe => "connexion coupée",
        UnexpectedEof => "connexion fermée avant la fin de l'échange",
        TimedOut | WouldBlock => "délai dépassé",
        HostUnreachable => "hôte injoignable",
        NetworkUnreachable => "réseau injoignable",
        NetworkDown => "réseau indisponible",
        AddrNotAvailable => "adresse indisponible",
        NotConnected => "connexion non établie",
        PermissionDenied => "permission refusée",
        _ => return format!("erreur d'entrée-sortie ({e})"),
    };
    known.to_owned()
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
            // Le message nomme déjà le service et l'opération (contexte).
            Error::Http(m) | Error::Grdf(m) | Error::Protocol(m) => f.write_str(m),
            Error::GrdfLogin(m) => f.write_str(m),
            Error::GrdfCredentialsRejected(m) => write!(
                f,
                "{m} ; vérifiez GRDF_USERNAME et GRDF_PASSWORD (identifiants de Mon Espace GRDF)"
            ),
            Error::HomeAssistant(m) => write!(f, "Home Assistant : {m}"),
            Error::Invalid(m) => write!(f, "valeur invalide : {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(io_cause(&e))
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
    fn display_is_the_message_with_its_advice() {
        // Le message nomme le service et l'étape (contexte) : pas de préfixe.
        let login = "GRDF, connexion 1/5 (page d'accueil) : service indisponible \
                     (statut HTTP 503, monespace.grdf.fr:443)";
        assert_eq!(Error::GrdfLogin(login.into()).to_string(), login);
        let http = "GRDF, liste des PCE : pas de réponse en 60 s (monespace.grdf.fr:443)";
        assert_eq!(Error::Http(http.into()).to_string(), http);
        // Refus d'identifiants : le message, puis ce qu'il faut faire.
        assert_eq!(
            Error::GrdfCredentialsRejected(
                "GRDF, connexion 4/5 (mot de passe Okta) : identifiants refusés".into()
            )
            .to_string(),
            "GRDF, connexion 4/5 (mot de passe Okta) : identifiants refusés ; vérifiez \
             GRDF_USERNAME et GRDF_PASSWORD (identifiants de Mon Espace GRDF)"
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
        assert!(
            Error::Http("pas de réponse en 60 s (connexion.grdf.fr:443)".into()).is_transient()
        );
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
        let io = io::Error::from(io::ErrorKind::ConnectionRefused);
        assert_eq!(Error::from(io), Error::Io("connexion refusée".into()));
    }

    #[test]
    fn io_causes_are_french() {
        use io::ErrorKind::*;
        for (kind, cause) in [
            (ConnectionRefused, "connexion refusée"),
            (TimedOut, "délai dépassé"),
            (WouldBlock, "délai dépassé"),
            (ConnectionReset, "connexion réinitialisée par le pair"),
            (UnexpectedEof, "connexion fermée avant la fin de l'échange"),
            (HostUnreachable, "hôte injoignable"),
        ] {
            assert_eq!(io_cause(&io::Error::from(kind)), cause, "{kind:?}");
        }
        // Sans traduction : le texte du système, entre parenthèses.
        let dns = io::Error::other("failed to lookup address information");
        assert_eq!(
            io_cause(&dns),
            "erreur d'entrée-sortie (failed to lookup address information)"
        );
    }

    #[test]
    fn context_prefixes_the_message_and_keeps_the_variant() {
        let e = Error::Http("pas de réponse de connexion.grdf.fr:443 en 60 s".into())
            .context("GRDF, connexion 3/5 (identification Okta)");
        assert_eq!(
            e,
            Error::Http(
                "GRDF, connexion 3/5 (identification Okta) : pas de réponse de \
                 connexion.grdf.fr:443 en 60 s"
                    .into()
            )
        );
        assert!(e.is_transient());

        // Un refus d'identifiants reste un refus : le démon cesse alors de
        // se connecter à GRDF.
        let rejected = Error::GrdfCredentialsRejected("identifiants refusés".into())
            .context("GRDF, connexion 4/5 (mot de passe Okta)");
        assert!(matches!(&rejected, Error::GrdfCredentialsRejected(m)
            if m == "GRDF, connexion 4/5 (mot de passe Okta) : identifiants refusés"));
        assert!(!rejected.is_transient());
        assert_eq!(rejected.exit_code(), 1);

        // Contexte typé de l'appelant : n'importe quel Display.
        struct Window(&'static str, &'static str);
        impl fmt::Display for Window {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "relevés du {} au {}", self.0, self.1)
            }
        }
        let e = Error::Grdf("statut HTTP 500".into()).context(Window("2026-01-01", "2026-01-31"));
        assert_eq!(
            e,
            Error::Grdf("relevés du 2026-01-01 au 2026-01-31 : statut HTTP 500".into())
        );

        // Une configuration invalide reste la liste de ses problèmes.
        let config = Error::Config(vec!["A manquant".into()]).context("x");
        assert_eq!(config, Error::Config(vec!["A manquant".into()]));
        assert_eq!(config.exit_code(), 2);
    }

    #[test]
    fn map_message_completes_the_cause() {
        let e = Error::Io("délai dépassé".into())
            .map_message(|m| format!("{m} ; abandon après 4 tentatives"));
        assert_eq!(
            e,
            Error::Io("délai dépassé ; abandon après 4 tentatives".into())
        );
        for e in [
            Error::Usage("u".into()),
            Error::GrdfLogin("u".into()),
            Error::Grdf("u".into()),
            Error::HomeAssistant("u".into()),
            Error::Protocol("u".into()),
            Error::Invalid("u".into()),
        ] {
            let before = std::mem::discriminant(&e);
            let after = e.map_message(|m| m + "!");
            assert_eq!(std::mem::discriminant(&after), before);
        }
    }

    #[test]
    fn is_std_error() {
        fn takes(_: &dyn std::error::Error) {}
        takes(&Error::Io("x".into()));
    }
}

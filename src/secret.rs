//! Type [`Secret`] : enveloppe une valeur sensible (mot de passe, jeton) pour
//! qu'elle ne puisse pas fuiter par mégarde dans un journal ou un message
//! d'erreur. `Debug` et `Display` affichent `***` ; la valeur n'est lisible
//! que par un appel explicite à [`Secret::expose`].
//!
//! Dépendances : aucune (bibliothèque standard).

use std::fmt;

/// Valeur sensible, masquée à l'affichage.
///
/// Volontairement sans `Clone` ni `PartialEq` : une copie ou une comparaison
/// doit passer par `expose()`, ce qui la rend visible à la relecture.
pub struct Secret(String);

impl Secret {
    /// Texte affiché à la place de la valeur.
    pub const MASK: &'static str = "***";

    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Donne accès à la valeur en clair. À n'utiliser qu'au moment de
    /// l'envoyer à son destinataire (GRDF, Home Assistant).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(Self::MASK)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(Self::MASK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_masked() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "***");
        assert_eq!(format!("{s:#?}"), "***");
    }

    #[test]
    fn display_is_masked() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s}"), "***");
        assert_eq!(s.to_string(), "***");
    }

    #[test]
    fn masked_inside_derived_debug() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Holder {
            token: Secret,
        }
        let h = Holder {
            token: Secret::new("abc.def.ghi"),
        };
        let shown = format!("{h:?}");
        assert!(!shown.contains("abc"), "{shown}");
        assert!(shown.contains("***"));
    }

    #[test]
    fn expose_returns_value() {
        assert_eq!(Secret::new("hunter2").expose(), "hunter2");
        assert_eq!(Secret::new(String::new()).expose(), "");
    }
}

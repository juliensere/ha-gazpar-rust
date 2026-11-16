//! Type [`Secret`] : enveloppe une valeur sensible (mot de passe, jeton) pour
//! qu'elle ne puisse pas fuiter par mégarde dans un journal ou un message
//! d'erreur. `Debug` et `Display` affichent `***` ; la valeur n'est lisible
//! que par un appel explicite à [`Secret::expose`].
//!
//! [`scrub`] est la seule fonction d'expurgation du programme : tout texte
//! venu d'un tiers (motif d'Okta, message de Home Assistant) y passe avant
//! d'entrer dans une erreur ou un journal.
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

/// Texte venu d'un tiers, prêt pour un message : chaque secret non vide est
/// remplacé par [`Secret::MASK`], puis le texte est raccourci à `max_chars`
/// caractères.
///
/// - Les secrets les plus longs sont remplacés d'abord : si l'un commence
///   comme un autre (e-mail préfixe du mot de passe), rien du plus long ne
///   reste.
/// - L'expurgation précède la troncature : un secret coupé à la limite ne
///   peut pas fuir en partie.
pub fn scrub(text: &str, secrets: &[&Secret], max_chars: usize) -> String {
    let mut values: Vec<&str> = secrets
        .iter()
        .map(|s| s.0.as_str())
        .filter(|v| !v.is_empty())
        .collect();
    values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    let mut out = text.to_owned();
    for value in values {
        out = out.replace(value, Secret::MASK);
    }
    out.chars().take(max_chars).collect()
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
    fn scrub_removes_every_secret() {
        let (email, password, handle, empty) = (
            Secret::new("jean@example.org"),
            Secret::new("hunter2"),
            Secret::new("02.id.H"),
            Secret::new(""),
        );
        let reason =
            "Utilisateur jean@example.org inconnu (jeton 02.id.H, mdp hunter2) jean@example.org";
        let clean = scrub(reason, &[&email, &password, &handle, &empty], 200);
        assert_eq!(clean, "Utilisateur *** inconnu (jeton ***, mdp ***) ***");
        assert_eq!(scrub("abc", &[&empty], 200), "abc");
    }

    #[test]
    fn scrub_masks_then_shortens() {
        let token = Secret::new("abc");
        assert_eq!(scrub("xabcx", &[&token], 200), "x***x");
        // Le secret à cheval sur la limite est masqué avant la coupe.
        let long = format!("{}abc", "y".repeat(199));
        let shown = scrub(&long, &[&token], 200);
        assert!(!shown.contains('a'), "{shown}");
        assert_eq!(shown.chars().count(), 200);
        assert_eq!(scrub("ééé", &[], 2), "éé");
    }

    #[test]
    fn scrub_replaces_longest_secret_first() {
        let short = Secret::new("jean");
        let long = Secret::new("jean-secret");
        assert_eq!(scrub("a jean-secret b", &[&short, &long], 200), "a *** b");
        assert_eq!(scrub("a jean-secret b", &[&long, &short], 200), "a *** b");
    }

    #[test]
    fn expose_returns_value() {
        assert_eq!(Secret::new("hunter2").expose(), "hunter2");
        assert_eq!(Secret::new(String::new()).expose(), "");
    }
}

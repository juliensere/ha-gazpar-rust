//! Algorithme d'un relevé (PLAN.md §3) — à écrire à l'étape 5.
//!
//! Lit d'abord dans Home Assistant les points de la fenêtre de recouvrement,
//! ne demande à GRDF que ce qui manque, recalcule les cumuls à partir de la
//! somme du jour précédant la fenêtre, n'importe jamais un jour sans valeur.
//! Impose un délai minimal entre deux connexions à GRDF.
//!
//! Aucune entrée-sortie : ni réseau, ni horloge, ni environnement. La date du
//! jour est reçue en paramètre ; GRDF et HA sont atteints uniquement par les
//! traits de `ports`, ce qui permet de tester avec des implémentations en
//! mémoire.
//!
//! Dépendances : `domain`, `ports`, `error`.

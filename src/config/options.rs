//! Table des options ([`OPTIONS`], MANUEL.md §4.2) et noms qui s'en
//! déduisent : clés du fichier (les noms des variables), paramètres
//! ([`parameter`] : `GRDF_PCE` ↔ `--grdf-pce`), variables surveillées.
//!
//! Données et fonctions pures. Dépendances : aucune.

/// Variables réservées au développement, lues dans l'environnement seulement.
pub const DEV_VARIABLES: [&str; 2] = ["GRDF_BASE_URL", "GRDF_LOGIN_URL"];

/// Préfixes des variables d'environnement surveillées : une variable inconnue
/// qui commence ainsi est probablement une faute de frappe.
pub(super) const WATCHED_PREFIXES: [&str; 2] = ["GAZPAR_", "GRDF_"];

/// Une option de configuration (MANUEL.md §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionSpec {
    /// Nom de la variable d'environnement et de la clé du fichier.
    pub name: &'static str,
    /// Variante `…_FILE` (valeur lue dans un fichier), s'il y en a une.
    pub file_variant: Option<&'static str>,
    /// Valeur masquée (`***`) dans le tableau de `check`.
    pub secret: bool,
    /// La forme simple est-elle acceptée en paramètre ? Non pour le mot de
    /// passe et le jeton (visibles dans `ps`) ; la variante `_FILE` l'est
    /// toujours.
    pub on_command_line: bool,
    /// Valeur par défaut ; `None` pour une option obligatoire.
    pub default: Option<&'static str>,
    /// Rôle, pour l'aide.
    pub help: &'static str,
}

/// Les options, dans l'ordre du manuel (§4.2).
pub const OPTIONS: [OptionSpec; 10] = [
    OptionSpec {
        name: "GRDF_USERNAME",
        file_variant: Some("GRDF_USERNAME_FILE"),
        secret: true,
        on_command_line: true,
        default: None,
        help: "e-mail du compte Mon Espace GRDF",
    },
    OptionSpec {
        name: "GRDF_PASSWORD",
        file_variant: Some("GRDF_PASSWORD_FILE"),
        secret: true,
        on_command_line: false,
        default: None,
        help: "mot de passe Mon Espace GRDF",
    },
    OptionSpec {
        name: "GRDF_PCE",
        file_variant: Some("GRDF_PCE_FILE"),
        secret: false,
        on_command_line: true,
        default: None,
        help: "PCE du compteur, 14 chiffres",
    },
    OptionSpec {
        name: "HA_URL",
        file_variant: None,
        secret: false,
        on_command_line: true,
        default: None,
        help: "adresse de Home Assistant, ws://hôte:port",
    },
    OptionSpec {
        name: "HA_TOKEN",
        file_variant: Some("HA_TOKEN_FILE"),
        secret: true,
        on_command_line: false,
        default: None,
        help: "jeton d'accès longue durée",
    },
    OptionSpec {
        name: "GAZPAR_SCAN_INTERVAL",
        file_variant: None,
        secret: false,
        on_command_line: true,
        default: Some("480"),
        help: "minutes entre deux relevés en mode run, 60 au minimum",
    },
    OptionSpec {
        name: "GAZPAR_HISTORY_DAYS",
        file_variant: None,
        secret: false,
        on_command_line: true,
        default: Some("1095"),
        help: "jours d'historique demandés au premier import (1 à 3650)",
    },
    OptionSpec {
        name: "GAZPAR_OVERLAP_DAYS",
        file_variant: None,
        secret: false,
        on_command_line: true,
        default: Some("14"),
        help: "jours déjà importés relus à chaque relevé (1 à 60)",
    },
    OptionSpec {
        name: "GAZPAR_STATISTIC_PREFIX",
        file_variant: None,
        secret: false,
        on_command_line: true,
        default: Some("gazpar"),
        help: "préfixe des statistiques",
    },
    OptionSpec {
        name: "GAZPAR_LOG_LEVEL",
        file_variant: None,
        secret: false,
        on_command_line: true,
        default: Some("info"),
        help: "info ou debug",
    },
];

/// Nom du paramètre d'une variable : minuscules, `_` remplacé par `-`
/// (`GRDF_PCE` → `--grdf-pce`).
pub fn parameter(variable: &str) -> String {
    let mut p = String::with_capacity(variable.len() + 2);
    p.push_str("--");
    p.extend(variable.chars().map(|c| {
        if c == '_' {
            '-'
        } else {
            c.to_ascii_lowercase()
        }
    }));
    p
}

/// Ce que désigne un paramètre de configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParameterTarget {
    /// Paramètre accepté : nom de la variable correspondante.
    Accepted(&'static str),
    /// Mot de passe ou jeton en paramètre, refusé : nom de la variable et
    /// paramètre à utiliser à la place (`--…-file`).
    Refused {
        variable: &'static str,
        instead: String,
    },
}

/// Tous les paramètres de configuration, acceptés ou refusés, avec ce qu'ils
/// désignent.
pub fn parameters() -> Vec<(String, ParameterTarget)> {
    let mut list = Vec::new();
    for spec in &OPTIONS {
        let target = match (spec.on_command_line, spec.file_variant) {
            (true, _) => ParameterTarget::Accepted(spec.name),
            (false, Some(variant)) => ParameterTarget::Refused {
                variable: spec.name,
                instead: parameter(variant),
            },
            // Une option sans variante doit être acceptée en paramètre ;
            // vérifié par le test `table_is_consistent`.
            (false, None) => continue,
        };
        list.push((parameter(spec.name), target));
        if let Some(variant) = spec.file_variant {
            list.push((parameter(variant), ParameterTarget::Accepted(variant)));
        }
    }
    list
}

/// Noms acceptés dans le fichier : chaque option et sa variante `_FILE`.
pub fn file_keys() -> Vec<&'static str> {
    OPTIONS
        .iter()
        .flat_map(|s| std::iter::once(s.name).chain(s.file_variant))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Table des options ---

    #[test]
    fn options_are_those_of_the_manual() {
        let names: Vec<&str> = OPTIONS.iter().map(|o| o.name).collect();
        assert_eq!(
            names,
            [
                "GRDF_USERNAME",
                "GRDF_PASSWORD",
                "GRDF_PCE",
                "HA_URL",
                "HA_TOKEN",
                "GAZPAR_SCAN_INTERVAL",
                "GAZPAR_HISTORY_DAYS",
                "GAZPAR_OVERLAP_DAYS",
                "GAZPAR_STATISTIC_PREFIX",
                "GAZPAR_LOG_LEVEL",
            ]
        );
        for removed in ["GAZPAR_TIMEZONE", "GAZPAR_CONSUMPTION_TYPE", "LOG_LEVEL"] {
            assert!(!file_keys().contains(&removed), "{removed}");
        }
    }

    #[test]
    fn table_is_consistent() {
        for o in &OPTIONS {
            if let Some(v) = o.file_variant {
                assert_eq!(v, format!("{}_FILE", o.name));
            } else {
                assert!(o.on_command_line, "{} : ni paramètre ni _FILE", o.name);
            }
            assert_eq!(
                o.default.is_none(),
                o.name.starts_with("GRDF_") || o.name.starts_with("HA_")
            );
            assert!(!o.help.is_empty());
        }
    }

    #[test]
    fn parameter_names_are_derived() {
        assert_eq!(parameter("GRDF_PCE"), "--grdf-pce");
        assert_eq!(parameter("GAZPAR_LOG_LEVEL"), "--gazpar-log-level");
        assert_eq!(parameter("HA_TOKEN_FILE"), "--ha-token-file");
        let params = parameters();
        let get = |p: &str| params.iter().find(|(n, _)| n == p).map(|(_, t)| t.clone());
        assert_eq!(
            get("--grdf-pce"),
            Some(ParameterTarget::Accepted("GRDF_PCE"))
        );
        assert_eq!(
            get("--grdf-username"),
            Some(ParameterTarget::Accepted("GRDF_USERNAME"))
        );
        assert_eq!(
            get("--grdf-password"),
            Some(ParameterTarget::Refused {
                variable: "GRDF_PASSWORD",
                instead: "--grdf-password-file".to_owned()
            })
        );
        assert_eq!(
            get("--ha-token"),
            Some(ParameterTarget::Refused {
                variable: "HA_TOKEN",
                instead: "--ha-token-file".to_owned()
            })
        );
        assert_eq!(
            get("--ha-token-file"),
            Some(ParameterTarget::Accepted("HA_TOKEN_FILE"))
        );
        // Développement et fichier : pas de paramètre.
        for absent in ["--grdf-base-url", "--grdf-login-url", "--gazpar-config"] {
            assert_eq!(get(absent), None, "{absent}");
        }
        assert_eq!(params.len(), 14);
    }

    #[test]
    fn file_keys_are_options_and_variants() {
        let keys = file_keys();
        assert_eq!(keys.len(), 14);
        assert!(keys.contains(&"GRDF_PASSWORD_FILE"));
        for absent in ["GAZPAR_CONFIG", "GRDF_BASE_URL", "GRDF_LOGIN_URL"] {
            assert!(!keys.contains(&absent), "{absent}");
        }
    }
}

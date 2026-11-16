//! Analyse du fichier de configuration `NOM=valeur` (MANUEL.md §4.4).
//!
//! Fonction pure : [`parse`] reçoit le texte du fichier (déjà lu, donc déjà
//! de l'UTF-8) et la liste des noms acceptés ; la lecture du fichier, ses
//! droits et la fusion avec les autres sources sont l'affaire de `config`.
//!
//! Règles exactes (MANUEL.md §4.4) :
//! - une marque d'ordre d'octets (BOM) en tête est ignorée ; fins de ligne
//!   Unix ou Windows ;
//! - lignes vides ou blanches ignorées ; une ligne dont le premier caractère
//!   non blanc est `#` est un commentaire ; pas de commentaire en fin de
//!   ligne ;
//! - `NOM=valeur`, découpée au premier `=` ; espaces et tabulations retirés
//!   autour du nom et de la valeur ; une ligne sans `=` est une erreur ;
//! - nom inconnu : erreur, avec suggestion d'un nom proche ; `export NOM=…`
//!   refusé ; nom répété : erreur ; `GAZPAR_CONFIG` refusé ;
//! - une valeur entourée du même guillemet (`"…"` ou `'…'`) perd ces deux
//!   guillemets, le contenu est pris tel quel ; guillemet ouvrant non refermé
//!   : erreur ; ni échappement ni substitution ;
//! - une valeur vide (y compris `""`) compte comme absente.
//!
//! **Les messages d'erreur citent le numéro de ligne et un nom d'option,
//! jamais la valeur ni la ligne** : un mot de passe mal placé ne doit pas
//! finir dans le journal. Un nom inconnu n'est recopié que s'il a la forme
//! d'un nom de variable (majuscules, chiffres, `_`) : un secret en base64
//! collé seul sur une ligne (`dGVzdA==`) donnerait sinon un « nom » qui en
//! est un morceau.
//!
//! Dépendances : aucune (bibliothèque standard).

use std::fmt;

/// Nom de la variable qui désigne le fichier : refusé dans le fichier.
pub const CONFIG_VARIABLE: &str = "GAZPAR_CONFIG";

/// Longueur maximale d'un nom inconnu recopié dans un message.
const MAX_ECHOED_NAME: usize = 64;

/// Une option lue dans le fichier, valeur non vide.
pub struct Entry {
    /// Numéro de ligne (à partir de 1).
    pub line: usize,
    /// Nom de l'option, pris dans la liste des noms acceptés.
    pub name: &'static str,
    /// Valeur, guillemets retirés. Peut être un secret : jamais affichée.
    pub value: String,
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("line", &self.line)
            .field("name", &self.name)
            .field("value", &"***")
            .finish()
    }
}

/// Erreur d'une ligne. Le message ne contient ni la valeur ni la ligne.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for LineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ligne {} : {}", self.line, self.message)
    }
}

/// Résultat de l'analyse : les options bien formées et les erreurs de toutes
/// les lignes fautives (relevées d'un coup).
#[derive(Debug, Default)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    pub errors: Vec<LineError>,
}

/// Analyse le texte d'un fichier de configuration. `known` liste les noms
/// acceptés (sans [`CONFIG_VARIABLE`]).
pub fn parse(content: &str, known: &[&'static str]) -> Parsed {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut parsed = Parsed::default();
    // Nom et ligne de première apparition, valeur vide comprise.
    let mut seen: Vec<(&'static str, usize)> = Vec::new();

    // Découpage à la main : `str::lines` garde le `\r` d'une dernière ligne
    // sans saut de ligne final.
    for (index, raw) in content.split('\n').enumerate() {
        let line = index + 1;
        let text = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = trim_blank(text);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut fail = |message: String| parsed.errors.push(LineError { line, message });

        if is_export(trimmed) {
            fail("« export » n'est pas accepté : écrire NOM=valeur, sans export".to_owned());
            continue;
        }
        let Some((raw_name, raw_value)) = trimmed.split_once('=') else {
            fail("« = » manquant (format attendu : NOM=valeur)".to_owned());
            continue;
        };
        let name = trim_blank(raw_name);
        if name.is_empty() {
            fail("nom d'option manquant avant « = »".to_owned());
            continue;
        }
        if name == CONFIG_VARIABLE {
            fail(format!(
                "{CONFIG_VARIABLE} n'est pas accepté dans le fichier de configuration \
                 (utiliser --config ou la variable d'environnement)"
            ));
            continue;
        }
        let Some(&name) = known.iter().find(|k| **k == name) else {
            fail(unknown_name(name, known));
            continue;
        };
        if let Some((_, first)) = seen.iter().find(|(n, _)| *n == name) {
            fail(format!("{name} déjà défini ligne {first}"));
            continue;
        }
        seen.push((name, line));
        match unquote(trim_blank(raw_value)) {
            Some("") => {}
            Some(value) => parsed.entries.push(Entry {
                line,
                name,
                value: value.to_owned(),
            }),
            None => fail(format!("{name} : guillemet ouvrant non refermé")),
        }
    }
    parsed
}

/// Retire espaces et tabulations aux deux bouts.
fn trim_blank(s: &str) -> &str {
    s.trim_matches([' ', '\t'])
}

/// La ligne commence-t-elle par le mot `export` suivi d'un blanc ?
fn is_export(line: &str) -> bool {
    line.strip_prefix("export")
        .is_some_and(|rest| rest.starts_with([' ', '\t']))
}

/// Retire une paire de guillemets identiques ; `None` si un guillemet
/// ouvrant n'est pas refermé.
fn unquote(value: &str) -> Option<&str> {
    let Some(quote) = value.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
        return Some(value);
    };
    // Le guillemet ouvrant fait un octet : `value[1..]` est bien découpé.
    value.get(1..).and_then(|rest| rest.strip_suffix(quote))
}

/// Message d'un nom inconnu, sans recopier un nom qui n'a pas la forme d'un
/// nom de variable.
fn unknown_name(name: &str, known: &[&'static str]) -> String {
    if let Some(upper) = known.iter().find(|k| k.eq_ignore_ascii_case(name)) {
        return format!("les noms s'écrivent en majuscules : {upper}");
    }
    let hint = closest(name, known.iter().copied())
        .map(|s| format!(" (voulez-vous dire {s} ?)"))
        .unwrap_or_default();
    if looks_like_variable(name) {
        format!("option {name} inconnue{hint}")
    } else {
        format!("nom d'option inconnu{hint}")
    }
}

/// Forme d'un nom de variable : une majuscule puis majuscules, chiffres ou
/// `_`, longueur raisonnable.
fn looks_like_variable(name: &str) -> bool {
    name.len() <= MAX_ECHOED_NAME
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Nom le plus proche de `name` parmi `candidates`, s'il l'est assez pour
/// être une faute de frappe probable. La casse est ignorée et `-` vaut `_` :
/// la même fonction sert aux paramètres (`--grdf-pce`).
pub fn closest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let wanted = normalize(name);
    let mut best: Option<(usize, &'a str)> = None;
    for candidate in candidates {
        let other = normalize(candidate);
        let distance = levenshtein(&wanted, &other);
        let longest = wanted.chars().count().max(other.chars().count());
        let contained =
            wanted.chars().count() >= 4 && (other.contains(&wanted) || wanted.contains(&other));
        let close = distance <= 2 || distance * 3 <= longest || contained;
        if close && best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, c)| c)
}

fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '-' {
                '_'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect()
}

/// Distance d'édition (insertions, suppressions, substitutions).
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = Vec::with_capacity(b.len() + 1);
        current.push(i + 1);
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != *cb);
            let deletion = previous[j + 1] + 1;
            let insertion = current[j] + 1;
            current.push(substitution.min(deletion).min(insertion));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOWN: [&str; 6] = [
        "GRDF_USERNAME",
        "GRDF_PASSWORD",
        "GRDF_PASSWORD_FILE",
        "GRDF_PCE",
        "GAZPAR_LOG_LEVEL",
        "GAZPAR_OVERLAP_DAYS",
    ];
    const PASSWORD: &str = "P@ss=w0rd#tres-secret";

    fn parse_known(content: &str) -> Parsed {
        parse(content, &KNOWN)
    }

    /// (nom, valeur) des options lues ; échec du test en cas d'erreur.
    fn values(content: &str) -> Vec<(&'static str, String)> {
        let p = parse_known(content);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        p.entries.into_iter().map(|e| (e.name, e.value)).collect()
    }

    fn value_of(content: &str) -> String {
        let v = values(content);
        assert_eq!(v.len(), 1, "{v:?}");
        v[0].1.clone()
    }

    /// Erreurs affichées (« ligne N : … »).
    fn errors(content: &str) -> Vec<String> {
        parse_known(content)
            .errors
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn single_error(content: &str) -> String {
        let e = errors(content);
        assert_eq!(e.len(), 1, "{e:?}");
        e[0].clone()
    }

    // --- Lignes ignorées, encodage, fins de ligne ---

    #[test]
    fn simple_lines() {
        assert_eq!(
            values("GRDF_PCE=01234567890123\nGAZPAR_LOG_LEVEL=debug\n"),
            vec![
                ("GRDF_PCE", "01234567890123".to_owned()),
                ("GAZPAR_LOG_LEVEL", "debug".to_owned())
            ]
        );
    }

    #[test]
    fn empty_file_has_nothing() {
        let p = parse_known("");
        assert!(p.entries.is_empty() && p.errors.is_empty());
    }

    #[test]
    fn blank_lines_and_comments_are_ignored() {
        let content =
            "\n   \n\t\n# commentaire\n   # commentaire indenté\n#GRDF_PCE=1\nGRDF_PCE=2\n";
        assert_eq!(values(content), vec![("GRDF_PCE", "2".to_owned())]);
    }

    #[test]
    fn byte_order_mark_is_ignored() {
        assert_eq!(value_of("\u{feff}GRDF_PCE=1\n"), "1");
        // Seulement en tête du fichier.
        assert_eq!(
            single_error("GRDF_PCE=1\n\u{feff}GAZPAR_LOG_LEVEL=info"),
            "ligne 2 : nom d'option inconnu (voulez-vous dire GAZPAR_LOG_LEVEL ?)"
        );
    }

    #[test]
    fn windows_line_endings() {
        assert_eq!(
            values("GRDF_PCE=1\r\nGAZPAR_LOG_LEVEL=\"debug\"\r\n"),
            vec![
                ("GRDF_PCE", "1".to_owned()),
                ("GAZPAR_LOG_LEVEL", "debug".to_owned())
            ]
        );
    }

    #[test]
    fn last_line_without_newline_even_with_cr() {
        assert_eq!(value_of("GRDF_PCE=1"), "1");
        assert_eq!(value_of("GRDF_PCE=1\r"), "1");
        assert_eq!(value_of("GRDF_PCE=\"x\"\r"), "x");
    }

    #[test]
    fn line_numbers_count_every_line() {
        assert_eq!(
            single_error("# a\n\nGRDF_PCE=1\r\nsans egal\n"),
            "ligne 4 : « = » manquant (format attendu : NOM=valeur)"
        );
    }

    // --- Découpage et espaces ---

    #[test]
    fn split_at_first_equal_sign() {
        assert_eq!(value_of("GRDF_PASSWORD=a=b==c"), "a=b==c");
    }

    #[test]
    fn spaces_around_name_and_value_are_removed() {
        assert_eq!(value_of("  GRDF_PCE \t=\t 12 34  "), "12 34");
    }

    #[test]
    fn no_end_of_line_comment() {
        assert_eq!(value_of("GRDF_PASSWORD=valeur # note"), "valeur # note");
    }

    #[test]
    fn line_without_equal_sign_is_an_error() {
        assert_eq!(
            single_error("GRDF_PCE"),
            "ligne 1 : « = » manquant (format attendu : NOM=valeur)"
        );
    }

    #[test]
    fn missing_name_is_an_error() {
        assert_eq!(
            single_error("  =valeur"),
            "ligne 1 : nom d'option manquant avant « = »"
        );
    }

    // --- Guillemets ---

    #[test]
    fn matching_quotes_are_removed() {
        assert_eq!(
            value_of("GRDF_PASSWORD=\"mot de passe avec # et espaces\""),
            "mot de passe avec # et espaces"
        );
        assert_eq!(value_of("GRDF_PASSWORD='simple'"), "simple");
        // Contenu pris tel quel : espaces intérieurs, autre guillemet.
        assert_eq!(value_of("GRDF_PASSWORD=\" a'b \""), " a'b ");
        assert_eq!(value_of("GRDF_PASSWORD=\"a\"b\""), "a\"b");
    }

    #[test]
    fn quote_not_at_start_is_kept() {
        assert_eq!(value_of("GRDF_PASSWORD=ab\"c\""), "ab\"c\"");
        assert_eq!(value_of("GRDF_PASSWORD=abc'"), "abc'");
    }

    #[test]
    fn no_escape_and_no_substitution() {
        assert_eq!(value_of(r#"GRDF_PASSWORD="a\nb\"""#), r#"a\nb\""#);
        assert_eq!(value_of("GRDF_PASSWORD=$HOME${USER}"), "$HOME${USER}");
        assert_eq!(value_of("GRDF_PASSWORD='$HOME'"), "$HOME");
    }

    #[test]
    fn unclosed_quote_is_an_error() {
        for line in [
            "GRDF_PASSWORD=\"abc",
            "GRDF_PASSWORD='abc\"",
            "GRDF_PASSWORD=\"",
            "GRDF_PASSWORD='",
            // Ce qu'écrirait un habitué des fichiers .env.
            "GRDF_PASSWORD=\"abc\" # note",
        ] {
            assert_eq!(
                single_error(line),
                "ligne 1 : GRDF_PASSWORD : guillemet ouvrant non refermé",
                "{line}"
            );
        }
    }

    #[test]
    fn empty_value_counts_as_absent() {
        for content in ["GRDF_PCE=", "GRDF_PCE=   ", "GRDF_PCE=\"\"", "GRDF_PCE=''"] {
            assert!(values(content).is_empty(), "{content}");
        }
    }

    // --- Noms ---

    #[test]
    fn unknown_name_with_suggestion() {
        assert_eq!(
            single_error("GRDF_PCEE=1"),
            "ligne 1 : option GRDF_PCEE inconnue (voulez-vous dire GRDF_PCE ?)"
        );
        assert_eq!(
            single_error("GRDF_PASWORD=x"),
            "ligne 1 : option GRDF_PASWORD inconnue (voulez-vous dire GRDF_PASSWORD ?)"
        );
        // Ancien nom : retrouvé dans le nouveau.
        assert_eq!(
            single_error("LOG_LEVEL=debug"),
            "ligne 1 : option LOG_LEVEL inconnue (voulez-vous dire GAZPAR_LOG_LEVEL ?)"
        );
    }

    #[test]
    fn unknown_name_without_suggestion() {
        assert_eq!(
            single_error("GAZPAR_TIMEZONE=Europe/Paris"),
            "ligne 1 : option GAZPAR_TIMEZONE inconnue"
        );
    }

    #[test]
    fn lowercase_name_is_refused_with_its_spelling() {
        assert_eq!(
            single_error("grdf_pce=1"),
            "ligne 1 : les noms s'écrivent en majuscules : GRDF_PCE"
        );
    }

    #[test]
    fn export_is_refused() {
        for line in [
            "export GRDF_PCE=1",
            "  export\tGRDF_PCE=1",
            "export GRDF_PCE",
        ] {
            assert_eq!(
                single_error(line),
                "ligne 1 : « export » n'est pas accepté : écrire NOM=valeur, sans export",
                "{line}"
            );
        }
        // Un nom qui commence par « export » n'est pas le mot-clé.
        assert_eq!(
            single_error("EXPORTED=1"),
            "ligne 1 : option EXPORTED inconnue"
        );
    }

    #[test]
    fn repeated_name_is_an_error() {
        assert_eq!(
            single_error("GRDF_PCE=1\n# x\nGRDF_PCE=2"),
            "ligne 3 : GRDF_PCE déjà défini ligne 1"
        );
        // Même si l'une des deux valeurs est vide.
        assert_eq!(
            single_error("GRDF_PCE=\nGRDF_PCE=2"),
            "ligne 2 : GRDF_PCE déjà défini ligne 1"
        );
    }

    #[test]
    fn config_variable_is_refused() {
        assert_eq!(
            single_error("GAZPAR_CONFIG=/etc/autre.conf"),
            "ligne 1 : GAZPAR_CONFIG n'est pas accepté dans le fichier de configuration \
             (utiliser --config ou la variable d'environnement)"
        );
    }

    #[test]
    fn every_faulty_line_is_reported_and_good_ones_kept() {
        let p = parse_known("GRDF_PCE=1\nmauvais\nGRDF_PCE=2\nGAZPAR_LOG_LEVEL=debug\nX=\"");
        let lines: Vec<usize> = p.errors.iter().map(|e| e.line).collect();
        assert_eq!(lines, vec![2, 3, 5]);
        let names: Vec<&str> = p.entries.iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["GRDF_PCE", "GAZPAR_LOG_LEVEL"]);
        assert_eq!(p.entries[0].line, 1);
    }

    // --- Aucun secret dans les messages ni dans Debug ---

    #[test]
    fn errors_never_show_value_or_line() {
        let content = format!(
            "GRDF_PASSWORD={PASSWORD}\n\
             GRDF_PASSWORD={PASSWORD}\n\
             GRDF_PASSWORD_FILE=\"{PASSWORD}\n\
             {PASSWORD}\n\
             export GRDF_PASSWORD={PASSWORD}\n\
             dGVzdC1zZWNyZXQ=\n\
             cGFzc3dvcmQ==\n\
             mot de passe=tres-secret\n\
             grdf_passwrd={PASSWORD}\n\
             GAZPAR_CONFIG={PASSWORD}\n"
        );
        let p = parse_known(&content);
        assert_eq!(p.errors.len(), 9, "{:?}", p.errors);
        let shown = format!(
            "{} {:?} {:?}",
            p.errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
            p.errors,
            p.entries
        );
        for secret in [
            PASSWORD,
            "P@ss",
            "w0rd",
            "tres-secret",
            "dGVzdC1zZWNyZXQ",
            "cGFzc3dvcmQ",
            "mot de passe",
            "passwrd",
        ] {
            assert!(!shown.contains(secret), "{secret} visible dans {shown}");
        }
    }

    #[test]
    fn entry_debug_is_masked() {
        let p = parse_known(&format!("GRDF_PASSWORD={PASSWORD}"));
        let shown = format!("{:?} {:#?}", p.entries, p);
        assert!(!shown.contains("P@ss"), "{shown}");
        assert!(shown.contains("GRDF_PASSWORD") && shown.contains("***"));
        assert_eq!(p.entries[0].value, PASSWORD);
    }

    // --- Suggestions ---

    #[test]
    fn closest_name() {
        let names = ["GRDF_PCE", "GRDF_PASSWORD", "GAZPAR_OVERLAP_DAYS"];
        assert_eq!(closest("GRDF_PCF", names), Some("GRDF_PCE"));
        assert_eq!(closest("grdf-pce", names), Some("GRDF_PCE"));
        assert_eq!(
            closest("GAZPAR_OVERLAP", names),
            Some("GAZPAR_OVERLAP_DAYS")
        );
        assert_eq!(closest("TOUT_AUTRE_CHOSE", names), None);
        assert_eq!(closest("", names), None);
    }

    #[test]
    fn levenshtein_distance() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("chat", "chats"), 1);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("été", "ete"), 2);
    }
}

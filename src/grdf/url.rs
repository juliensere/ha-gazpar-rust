//! Adresses HTTP(S) réduites à ce dont le client GRDF a besoin : analyse,
//! résolution d'une `Location` relative (RFC 3986 §5.2, simplifiée),
//! encodage des paramètres de requête, et forme **expurgée** pour les
//! journaux et les erreurs.
//!
//! Une adresse du flux de connexion peut porter un secret dans sa requête
//! (`stateToken`, `code` OAuth) : seule [`Url::redacted`] (schéma, hôte,
//! chemin) peut apparaître dans un message, jamais l'adresse complète.
//!
//! Fonctions pures. Dépendances : `error`.

use std::fmt;

use crate::error::Error;

/// Adresse absolue `http://` ou `https://`, sans fragment ni identifiants.
#[derive(Clone, PartialEq, Eq)]
pub struct Url {
    https: bool,
    /// Hôte en minuscules (adresse IPv6 entre crochets).
    host: String,
    port: Option<u16>,
    /// Chemin, commençant toujours par `/`.
    path: String,
    /// Requête, sans le `?`.
    query: Option<String>,
}

fn invalid(why: &str) -> Error {
    // L'adresse elle-même n'est jamais recopiée : elle peut porter un jeton.
    Error::Protocol(format!("adresse invalide : {why}"))
}

impl Url {
    /// Analyse une adresse absolue. Le fragment (`#…`) est ignoré.
    pub fn parse(text: &str) -> Result<Url, Error> {
        let text = text.trim();
        let text = text.split_once('#').map_or(text, |(before, _)| before);
        let (https, rest) = if let Some(rest) = strip_prefix_ignore_case(text, "https://") {
            (true, rest)
        } else if let Some(rest) = strip_prefix_ignore_case(text, "http://") {
            (false, rest)
        } else {
            return Err(invalid("seuls http:// et https:// sont pris en charge"));
        };
        let end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        if authority.contains('@') {
            return Err(invalid("identifiants dans l'adresse"));
        }
        let (host, port) = split_host_port(authority)?;
        let (path, query) = split_path_query(tail);
        let path = if path.is_empty() {
            "/".to_owned()
        } else {
            remove_dot_segments(path)
        };
        Ok(Url {
            https,
            host,
            port,
            path,
            query,
        })
    }

    /// Résout une référence (en-tête `Location`) par rapport à cette
    /// adresse : absolue, sans schéma (`//hôte/…`), chemin absolu, chemin
    /// relatif ou requête seule.
    pub fn resolve(&self, reference: &str) -> Result<Url, Error> {
        let reference = reference.trim();
        let reference = reference
            .split_once('#')
            .map_or(reference, |(before, _)| before);
        if has_scheme(reference) {
            return Url::parse(reference);
        }
        if let Some(rest) = reference.strip_prefix("//") {
            let scheme = if self.https { "https://" } else { "http://" };
            return Url::parse(&format!("{scheme}{rest}"));
        }
        let (ref_path, ref_query) = split_path_query(reference);
        let (path, query) = if ref_path.is_empty() {
            (self.path.clone(), ref_query.or_else(|| self.query.clone()))
        } else if ref_path.starts_with('/') {
            (remove_dot_segments(ref_path), ref_query)
        } else {
            let dir = self.path.rfind('/').map_or("/", |i| &self.path[..=i]);
            (remove_dot_segments(&format!("{dir}{ref_path}")), ref_query)
        };
        Ok(Url {
            path,
            query,
            ..self.clone()
        })
    }

    pub fn is_https(&self) -> bool {
        self.https
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Schéma, hôte, port et chemin, sans la requête : seule forme
    /// autorisée dans un journal ou une erreur.
    pub fn redacted(&self) -> String {
        format!("{}{}", self.origin(), self.path)
    }

    fn origin(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        match self.port {
            Some(port) => format!("{scheme}://{}:{port}", self.host),
            None => format!("{scheme}://{}", self.host),
        }
    }

    /// Adresse complète, à transmettre au transport uniquement.
    pub fn full(&self) -> String {
        match &self.query {
            Some(q) => format!("{}{}?{q}", self.origin(), self.path),
            None => self.redacted(),
        }
    }

    /// Ajoute des paramètres de requête, encodés.
    pub fn with_query(mut self, pairs: &[(&str, &str)]) -> Url {
        if pairs.is_empty() {
            return self;
        }
        let added = encode_query(pairs);
        self.query = Some(match self.query.take() {
            Some(q) if !q.is_empty() => format!("{q}&{added}"),
            _ => added,
        });
        self
    }
}

/// Masque la requête : un `Debug` par mégarde ne montre pas de jeton.
impl fmt::Debug for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Url({})", self.redacted())
    }
}

/// Encode des paires `clé=valeur` : tout sauf `A-Z a-z 0-9 - . _ ~` devient
/// `%XX` (`pceList[]` → `pceList%5B%5D`).
pub fn encode_query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// La référence commence-t-elle par un schéma (`lettres:` avant tout `/`, `?`) ?
fn has_scheme(reference: &str) -> bool {
    let end = reference.find(['/', '?']).unwrap_or(reference.len());
    reference[..end].split_once(':').is_some_and(|(scheme, _)| {
        scheme
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic())
            && scheme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    })
}

fn split_host_port(authority: &str) -> Result<(String, Option<u16>), Error> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (inside, after) = rest
            .split_once(']')
            .ok_or_else(|| invalid("crochet IPv6 non fermé"))?;
        (format!("[{inside}]"), after.strip_prefix(':'))
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h.to_owned(), Some(p)),
            None => (authority.to_owned(), None),
        }
    };
    let valid_host = if host.starts_with('[') {
        host.len() > 2
    } else {
        !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
    };
    if !valid_host {
        return Err(invalid("hôte manquant ou invalide"));
    }
    let port = match port {
        None | Some("") => None,
        Some(p) => Some(p.parse::<u16>().map_err(|_| invalid("port invalide"))?),
    };
    Ok((host.to_ascii_lowercase(), port))
}

fn split_path_query(tail: &str) -> (&str, Option<String>) {
    match tail.split_once('?') {
        Some((path, query)) => (path, Some(query.to_owned())),
        None => (tail, None),
    }
}

/// Retire les segments `.` et `..` d'un chemin (RFC 3986 §5.2.4).
fn remove_dot_segments(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len().saturating_sub(1);
    for (i, segment) in segments.iter().enumerate() {
        match *segment {
            "." => {}
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
            }
            s => out.push(s),
        }
        // Un chemin finissant par « . » ou « .. » désigne un répertoire.
        if i == last && matches!(*segment, "." | "..") {
            out.push("");
        }
    }
    let joined = out.join("/");
    if joined.starts_with('/') {
        joined
    } else {
        format!("/{joined}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn parse_parts() {
        let u = url("HTTPS://MonEspace.GRDF.fr:8443/api/e-conso?x=1#frag");
        assert!(u.is_https());
        assert_eq!(u.host(), "monespace.grdf.fr");
        assert_eq!(u.path(), "/api/e-conso");
        assert_eq!(u.full(), "https://monespace.grdf.fr:8443/api/e-conso?x=1");
        assert_eq!(u.redacted(), "https://monespace.grdf.fr:8443/api/e-conso");
        let u = url("http://127.0.0.1:40123");
        assert!(!u.is_https());
        assert_eq!(u.path(), "/");
        assert_eq!(u.full(), "http://127.0.0.1:40123/");
        assert_eq!(url("http://h?q").full(), "http://h/?q");
        assert_eq!(url("http://[::1]:80/a").host(), "[::1]");
    }

    #[test]
    fn parse_refusals_never_echo_the_address() {
        for s in [
            "",
            "ftp://h/",
            "javascript:alert(1)",
            "monespace.grdf.fr/",
            "http://",
            "http:///x",
            "http://:80/",
            "http://user:secret@h/",
            "http://h:99999/",
            "http://h:x/",
            "http://[::1/",
            "http://a b/",
            "http://h%2F/",
        ] {
            let e = Url::parse(s).unwrap_err();
            let shown = e.to_string();
            assert!(
                shown.starts_with("protocole : adresse invalide"),
                "{s:?} {shown}"
            );
            assert!(!shown.contains("secret"));
        }
    }

    #[test]
    fn debug_hides_query() {
        let u = url("https://connexion.grdf.fr/login/token/redirect?stateToken=SECRET");
        let shown = format!("{u:?} {}", u.redacted());
        assert!(!shown.contains("SECRET"), "{shown}");
        assert!(u.full().contains("SECRET"));
    }

    #[test]
    fn resolve_rfc3986_examples() {
        // RFC 3986 §5.4.1, base http://a/b/c/d;p?q (fragments retirés).
        let base = url("http://a/b/c/d;p?q");
        for (reference, expected) in [
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g/"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q"),
            ("g#s", "http://a/b/c/g"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            ("..g", "http://a/b/c/..g"),
            ("./../g", "http://a/b/g"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("https://autre.fr/x?y=1", "https://autre.fr/x?y=1"),
        ] {
            assert_eq!(
                base.resolve(reference).unwrap().full(),
                expected,
                "{reference:?}"
            );
        }
    }

    #[test]
    fn resolve_keeps_scheme_and_port() {
        let base = url("http://127.0.0.1:8080/a/b?x");
        assert_eq!(
            base.resolve("/_codexch?code=1").unwrap().full(),
            "http://127.0.0.1:8080/_codexch?code=1"
        );
        assert_eq!(
            url("https://h/x").resolve("//k:9/y").unwrap().full(),
            "https://k:9/y"
        );
    }

    #[test]
    fn resolve_refuses_other_schemes() {
        assert!(url("https://h/").resolve("ftp://h/x").is_err());
        assert!(url("https://h/").resolve("javascript:x").is_err());
        // « a:b » après un « / » n'est pas un schéma.
        assert_eq!(
            url("https://h/d/").resolve("x/a:b").unwrap().path(),
            "/d/x/a:b"
        );
    }

    #[test]
    fn query_encoding() {
        assert_eq!(
            encode_query(&[("dateDebut", "2026-01-01"), ("pceList[]", "12345678901234")]),
            "dateDebut=2026-01-01&pceList%5B%5D=12345678901234"
        );
        assert_eq!(
            encode_query(&[("a b", "é&=?/+~")]),
            "a%20b=%C3%A9%26%3D%3F%2F%2B~"
        );
        let u = url("http://h/p").with_query(&[("x", "1"), ("y", "2")]);
        assert_eq!(u.full(), "http://h/p?x=1&y=2");
        let u = url("http://h/p?a=0").with_query(&[("x", "1")]);
        assert_eq!(u.full(), "http://h/p?a=0&x=1");
        assert_eq!(url("http://h/p").with_query(&[]).full(), "http://h/p");
    }
}

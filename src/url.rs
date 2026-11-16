//! Adresses `http://`, `https://` et `ws://` : seul analyseur d'adresse du
//! programme, partagé par `grdf` (requêtes et redirections), `ha` (connexion
//! WebSocket) et `config` (validation de `HA_URL` et des adresses de
//! développement).
//!
//! Analyse, résolution d'une `Location` relative (RFC 3986 §5.2,
//! simplifiée), encodage des paramètres de requête, et forme **expurgée**
//! pour les journaux et les erreurs.
//!
//! Règles, volontairement plus strictes que la RFC 3986 :
//!
//! - deux familles de schémas, choisies par l'appelant : [`Url::http`]
//!   (`http`, `https`) et [`Url::ws`] (`ws`) ; une référence absolue résolue
//!   par [`Url::resolve`] doit rester dans la famille de sa base ;
//! - identifiants (`user:pass@`) refusés ;
//! - hôte : nom fait de lettres, chiffres, `-`, `_` et `.` sans étiquette
//!   vide, ou adresse IPv6 valide entre crochets ;
//! - port : chiffres ASCII seulement, de 1 à 65535 ; un port vide est refusé ;
//! - aucun espace ni caractère de contrôle, nulle part (pas d'injection
//!   d'en-tête dans une ligne de requête) ;
//! - le fragment (`#…`) est ignoré.
//!
//! Une adresse du flux de connexion peut porter un secret dans sa requête
//! (`stateToken`, `code` OAuth) : seule [`Url::redacted`] (schéma, hôte,
//! port, chemin) peut apparaître dans un message, jamais l'adresse
//! complète ; et un refus ([`InvalidUrl`]) ne recopie jamais l'adresse.
//!
//! Fonctions pures, bibliothèque standard seulement. Dépendances : `error`
//! (conversion d'[`InvalidUrl`]).

use std::fmt;
use std::net::Ipv6Addr;

use crate::error::Error;

/// Schéma d'une adresse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
    Ws,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
            Scheme::Ws => "ws",
        }
    }

    /// Port utilisé quand l'adresse n'en donne pas.
    pub fn default_port(self) -> u16 {
        match self {
            Scheme::Http | Scheme::Ws => 80,
            Scheme::Https => 443,
        }
    }

    fn family(self) -> Family {
        match self {
            Scheme::Http | Scheme::Https => Family::Http,
            Scheme::Ws => Family::WebSocket,
        }
    }
}

/// Famille de schémas acceptée par un analyseur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// `http://` ou `https://`.
    Http,
    /// `ws://`.
    WebSocket,
}

/// Motif du refus d'une adresse. Ne contient jamais l'adresse elle-même,
/// qui peut porter un jeton.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidUrl {
    /// Schéma absent ou hors de la famille attendue.
    Scheme(Family),
    /// Identifiants (`user:pass@`) dans l'adresse.
    Credentials,
    /// Hôte manquant ou invalide (IPv6 comprise).
    Host,
    /// Port vide, non numérique, nul ou supérieur à 65535.
    Port,
    /// Espace ou caractère de contrôle.
    Character,
}

impl fmt::Display for InvalidUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            InvalidUrl::Scheme(Family::Http) => "seuls http:// et https:// sont pris en charge",
            InvalidUrl::Scheme(Family::WebSocket) => "seul ws:// est pris en charge",
            InvalidUrl::Credentials => "identifiants dans l'adresse",
            InvalidUrl::Host => "hôte manquant ou invalide",
            InvalidUrl::Port => "port invalide",
            InvalidUrl::Character => "espace ou caractère de contrôle dans l'adresse",
        })
    }
}

impl From<InvalidUrl> for Error {
    fn from(e: InvalidUrl) -> Error {
        Error::Protocol(format!("adresse invalide : {e}"))
    }
}

/// Adresse absolue, sans fragment ni identifiants.
#[derive(Clone, PartialEq, Eq)]
pub struct Url {
    scheme: Scheme,
    /// Hôte en minuscules (adresse IPv6 entre crochets).
    host: String,
    port: Option<u16>,
    /// Chemin, commençant toujours par `/`.
    path: String,
    /// Requête, sans le `?`.
    query: Option<String>,
}

impl Url {
    /// Analyse une adresse absolue `http://` ou `https://`.
    pub fn http(text: &str) -> Result<Url, InvalidUrl> {
        Url::parse(text, Family::Http)
    }

    /// Analyse une adresse absolue `ws://`.
    pub fn ws(text: &str) -> Result<Url, InvalidUrl> {
        Url::parse(text, Family::WebSocket)
    }

    /// Analyse une adresse absolue de la famille `family`. Le fragment
    /// (`#…`) est ignoré.
    pub fn parse(text: &str, family: Family) -> Result<Url, InvalidUrl> {
        let text = text.trim();
        check_characters(text)?;
        let text = without_fragment(text);
        let (scheme, rest) = split_scheme(text)
            .filter(|(scheme, _)| scheme.family() == family)
            .ok_or(InvalidUrl::Scheme(family))?;
        let end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        if authority.contains('@') {
            return Err(InvalidUrl::Credentials);
        }
        let (host, port) = split_host_port(authority)?;
        let (path, query) = split_path_query(tail);
        let path = if path.is_empty() {
            "/".to_owned()
        } else {
            remove_dot_segments(path)
        };
        Ok(Url {
            scheme,
            host,
            port,
            path,
            query,
        })
    }

    /// Résout une référence (en-tête `Location`) par rapport à cette
    /// adresse : absolue (de la même famille de schémas), sans schéma
    /// (`//hôte/…`), chemin absolu, chemin relatif ou requête seule.
    pub fn resolve(&self, reference: &str) -> Result<Url, InvalidUrl> {
        let family = self.scheme.family();
        let reference = reference.trim();
        check_characters(reference)?;
        let reference = without_fragment(reference);
        if has_scheme(reference) {
            return Url::parse(reference, family);
        }
        if let Some(rest) = reference.strip_prefix("//") {
            return Url::parse(&format!("{}://{rest}", self.scheme.as_str()), family);
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
        self.scheme == Scheme::Https
    }

    /// Hôte en minuscules ; une adresse IPv6 garde ses crochets (`[::1]`).
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Port donné, sinon celui du schéma.
    pub fn port_or_default(&self) -> u16 {
        self.port.unwrap_or(self.scheme.default_port())
    }

    /// Chemin, commençant toujours par `/`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Hôte et port tels qu'attendus dans l'en-tête `Host` : le port est
    /// omis s'il est celui du schéma.
    pub fn authority(&self) -> String {
        match self.port {
            Some(port) if port != self.scheme.default_port() => format!("{}:{port}", self.host),
            _ => self.host.clone(),
        }
    }

    /// Schéma, hôte et port : `https://monespace.grdf.fr`,
    /// `ws://127.0.0.1:8123`.
    pub fn origin(&self) -> String {
        let scheme = self.scheme.as_str();
        match self.port {
            Some(port) => format!("{scheme}://{}:{port}", self.host),
            None => format!("{scheme}://{}", self.host),
        }
    }

    /// Schéma, hôte, port et chemin, sans la requête : seule forme
    /// autorisée dans un journal ou une erreur.
    pub fn redacted(&self) -> String {
        format!("{}{}", self.origin(), self.path)
    }

    /// Chemin et requête, cible d'une ligne de requête HTTP.
    pub fn path_and_query(&self) -> String {
        match &self.query {
            Some(q) => format!("{}?{q}", self.path),
            None => self.path.clone(),
        }
    }

    /// Adresse complète, à transmettre au transport uniquement.
    pub fn full(&self) -> String {
        format!("{}{}", self.origin(), self.path_and_query())
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

/// Refuse les espaces et caractères de contrôle (CR, LF compris).
fn check_characters(text: &str) -> Result<(), InvalidUrl> {
    if text.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Err(InvalidUrl::Character)
    } else {
        Ok(())
    }
}

fn without_fragment(text: &str) -> &str {
    text.split_once('#').map_or(text, |(before, _)| before)
}

/// Schéma connu en tête de `text` (casse indifférente) et la suite.
fn split_scheme(text: &str) -> Option<(Scheme, &str)> {
    [Scheme::Https, Scheme::Http, Scheme::Ws]
        .into_iter()
        .find_map(|scheme| {
            let prefix_len = scheme.as_str().len() + 3;
            let head = text.get(..prefix_len)?;
            let expected = format!("{}://", scheme.as_str());
            head.eq_ignore_ascii_case(&expected)
                .then(|| (scheme, &text[prefix_len..]))
        })
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

fn split_host_port(authority: &str) -> Result<(String, Option<u16>), InvalidUrl> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (inside, after) = rest.split_once(']').ok_or(InvalidUrl::Host)?;
        inside.parse::<Ipv6Addr>().map_err(|_| InvalidUrl::Host)?;
        let port = match after {
            "" => None,
            _ => Some(after.strip_prefix(':').ok_or(InvalidUrl::Host)?),
        };
        (format!("[{inside}]"), port)
    } else {
        let (name, port) = match authority.split_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (authority, None),
        };
        let valid = name.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        });
        if !valid {
            return Err(InvalidUrl::Host);
        }
        (name.to_owned(), port)
    };
    let port = match port {
        None => None,
        Some(p) => {
            // `u16::from_str` accepterait « +80 » : chiffres ASCII exigés.
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(InvalidUrl::Port);
            }
            let port: u16 = p.parse().map_err(|_| InvalidUrl::Port)?;
            if port == 0 {
                return Err(InvalidUrl::Port);
            }
            Some(port)
        }
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
        Url::http(s).unwrap()
    }

    #[track_caller]
    fn refused(s: &str, family: Family) -> InvalidUrl {
        match Url::parse(s, family) {
            Err(e) => e,
            Ok(u) => panic!("{s:?} accepté à tort : {u:?}"),
        }
    }

    // --- Analyse ---

    #[test]
    fn parse_parts() {
        let u = url("HTTPS://MonEspace.GRDF.fr:8443/api/e-conso?x=1#frag");
        assert!(u.is_https());
        assert_eq!(u.host(), "monespace.grdf.fr");
        assert_eq!(u.path(), "/api/e-conso");
        assert_eq!(u.full(), "https://monespace.grdf.fr:8443/api/e-conso?x=1");
        assert_eq!(u.redacted(), "https://monespace.grdf.fr:8443/api/e-conso");
        assert_eq!(u.origin(), "https://monespace.grdf.fr:8443");
        let u = url("http://127.0.0.1:40123");
        assert!(!u.is_https());
        assert_eq!(u.path(), "/");
        assert_eq!(u.full(), "http://127.0.0.1:40123/");
        assert_eq!(url("http://h?q").full(), "http://h/?q");
        assert_eq!(url("  http://h/p  ").full(), "http://h/p");
        assert_eq!(url("http://a_b-c.d/").host(), "a_b-c.d");
    }

    #[test]
    fn default_ports_and_authority() {
        assert_eq!(url("http://h/").port_or_default(), 80);
        assert_eq!(url("https://h/").port_or_default(), 443);
        assert_eq!(Url::ws("ws://h").unwrap().port_or_default(), 80);
        assert_eq!(url("https://h:8443/").port_or_default(), 8443);
        assert_eq!(url("https://h:443/").authority(), "h");
        assert_eq!(url("https://h:80/").authority(), "h:80");
        assert_eq!(url("http://h:8080/").authority(), "h:8080");
        assert_eq!(url("http://[::1]:8080/").authority(), "[::1]:8080");
    }

    #[test]
    fn websocket_family() {
        let u = Url::ws("WS://HA.local:8123/api/websocket").unwrap();
        assert!(!u.is_https());
        assert_eq!(u.full(), "ws://ha.local:8123/api/websocket");
        assert_eq!(u.path_and_query(), "/api/websocket");
        assert_eq!(Url::ws("ws://h?x=1").unwrap().path_and_query(), "/?x=1");
        for (s, family) in [
            ("ws://h/", Family::Http),
            ("wss://h/", Family::WebSocket),
            ("http://h/", Family::WebSocket),
            ("https://h/", Family::WebSocket),
        ] {
            assert_eq!(refused(s, family), InvalidUrl::Scheme(family), "{s}");
        }
    }

    #[test]
    fn unknown_or_missing_schemes() {
        for s in [
            "",
            "ftp://h/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "monespace.grdf.fr/",
            "http:/h",
            "http:h",
            "//h/",
            "httpx://h/",
        ] {
            assert_eq!(
                refused(s, Family::Http),
                InvalidUrl::Scheme(Family::Http),
                "{s:?}"
            );
        }
    }

    #[test]
    fn ipv6_hosts() {
        let u = url("http://[::1]:80/a");
        assert_eq!(u.host(), "[::1]");
        assert_eq!(u.port_or_default(), 80);
        assert_eq!(url("http://[2001:DB8::1]/").host(), "[2001:db8::1]");
        assert_eq!(
            url("http://[::ffff:127.0.0.1]/").host(),
            "[::ffff:127.0.0.1]"
        );
        for s in [
            "http://[::1/",
            "http://[]/",
            "http://[evil]/",
            "http://[127.0.0.1]/",
            "http://[::1]x/",
            "http://[::1]x:80/",
            "http://[fe80::1%25eth0]/",
            "http://::1/",
        ] {
            let e = refused(s, Family::Http);
            assert!(
                matches!(e, InvalidUrl::Host | InvalidUrl::Port),
                "{s} : {e:?}"
            );
        }
        assert_eq!(refused("http://[::1]:/", Family::Http), InvalidUrl::Port);
        assert_eq!(refused("http://[::1]:0/", Family::Http), InvalidUrl::Port);
    }

    #[test]
    fn invalid_ports() {
        assert_eq!(url("http://h:1/").port_or_default(), 1);
        assert_eq!(url("http://h:65535/").port_or_default(), 65535);
        assert_eq!(url("http://h:080/").port_or_default(), 80);
        for s in [
            "http://h:/",
            "http://h:",
            "http://h:0/",
            "http://h:00/",
            "http://h:65536/",
            "http://h:99999/",
            "http://h:+80/",
            "http://h:-1/",
            "http://h:x/",
            "http://h:80x/",
            "http://h:８０/",
            "http://h:80:81/",
        ] {
            assert_eq!(refused(s, Family::Http), InvalidUrl::Port, "{s}");
        }
    }

    #[test]
    fn empty_or_invalid_hosts() {
        for s in [
            "http://",
            "http:///x",
            "http://:80/",
            "http://?x",
            "http://./",
            "http://a..b/",
            "http://.grdf.fr/",
            "http://grdf.fr./",
            "http://h%2F/",
            "http://h\\evil.com/",
            "http://é.fr/",
        ] {
            assert_eq!(refused(s, Family::Http), InvalidUrl::Host, "{s}");
        }
    }

    #[test]
    fn credentials_are_refused() {
        for s in [
            "http://user:secret@h/",
            "http://u@h/",
            "http://@h/",
            "http://h@/",
            "https://monespace.grdf.fr@evil.example/",
            "http://evil.example\\@grdf.fr/",
        ] {
            assert_eq!(refused(s, Family::Http), InvalidUrl::Credentials, "{s}");
        }
        assert_eq!(
            refused("ws://u:p@ha:8123", Family::WebSocket),
            InvalidUrl::Credentials
        );
    }

    #[test]
    fn query_and_fragment_cannot_hide_the_host() {
        // Après « ? » ou « # », un « @ » n'est plus dans l'autorité.
        let u = url("http://h?@evil.example/");
        assert_eq!((u.host(), u.path()), ("h", "/"));
        assert_eq!(u.full(), "http://h/?@evil.example/");
        let u = url("http://h#@evil.example/");
        assert_eq!(u.full(), "http://h/");
        let u = url("http://h/p#frag?x=1");
        assert_eq!(u.full(), "http://h/p");
    }

    #[test]
    fn spaces_and_control_characters_are_refused() {
        for s in [
            "http://a b/",
            "http://h/a b",
            "http://h/?a b",
            "ws://h/\r\nX: y",
            "http://h/\tx",
            "http://h/\u{0}x",
            "http://h/\u{85}x",
        ] {
            let family = if s.starts_with("ws") {
                Family::WebSocket
            } else {
                Family::Http
            };
            assert_eq!(refused(s, family), InvalidUrl::Character, "{s:?}");
        }
    }

    #[test]
    fn refusals_never_echo_the_address() {
        for s in [
            "http://user:secret@h/",
            "ftp://secret/",
            "http://h:secret/",
            "http://[secret]/",
            "http://h/secret x",
        ] {
            let e = Error::from(refused(s, Family::Http));
            let shown = format!("{e} {e:?}");
            assert!(
                shown.starts_with("protocole : adresse invalide : "),
                "{shown}"
            );
            assert!(!shown.contains("secret"), "{shown}");
        }
    }

    #[test]
    fn debug_hides_query() {
        let u = url("https://connexion.grdf.fr/login/token/redirect?stateToken=SECRET");
        let shown = format!("{u:?} {}", u.redacted());
        assert!(!shown.contains("SECRET"), "{shown}");
        assert!(u.full().contains("SECRET"));
    }

    // --- Résolution ---

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
    fn resolve_stays_in_the_family_of_the_base() {
        for reference in ["ftp://h/x", "javascript:x", "ws://h/x", "wss://h/x"] {
            assert_eq!(
                url("https://h/").resolve(reference),
                Err(InvalidUrl::Scheme(Family::Http)),
                "{reference}"
            );
        }
        let ws = Url::ws("ws://h:8123/api").unwrap();
        assert_eq!(
            ws.resolve("http://h/"),
            Err(InvalidUrl::Scheme(Family::WebSocket))
        );
        assert_eq!(ws.resolve("//k/x").unwrap().full(), "ws://k/x");
        // « a:b » après un « / » n'est pas un schéma.
        assert_eq!(
            url("https://h/d/").resolve("x/a:b").unwrap().path(),
            "/d/x/a:b"
        );
    }

    #[test]
    fn resolve_checks_what_it_builds() {
        let base = url("https://h/d/");
        for reference in ["x\r\nSet-Cookie: a=1", "/a b", "?a\tb"] {
            assert_eq!(
                base.resolve(reference),
                Err(InvalidUrl::Character),
                "{reference:?}"
            );
        }
        assert_eq!(base.resolve("//u@k/"), Err(InvalidUrl::Credentials));
        assert_eq!(base.resolve("//k:0/"), Err(InvalidUrl::Port));
        assert_eq!(base.resolve("//[::1/"), Err(InvalidUrl::Host));
        assert_eq!(base.resolve(" /x ").unwrap().full(), "https://h/x");
    }

    // --- Requête ---

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

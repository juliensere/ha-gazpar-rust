//! Bocal à cookies maison (PLAN.md §4.2), suffisant pour la session de Mon
//! Espace GRDF : l'option `cookies` d'`ureq` tirerait 45 crates de plus.
//!
//! RFC 6265 pour l'essentiel, simplifiée :
//!
//! - `Set-Cookie` : `nom=valeur`, attributs `Domain`, `Path`, `Max-Age`
//!   (prioritaire), `Expires` (dates lues comme au §5.1.1), `Secure` ;
//!   `HttpOnly` et `SameSite` sont ignorés (pas de script, un seul site) ;
//! - sans `Domain`, le cookie ne revient qu'au même hôte ; avec `Domain`, il
//!   revient aussi aux sous-domaines, et il est refusé si l'hôte qui le pose
//!   n'appartient pas à ce domaine ou si le domaine n'a pas de point (`fr`) ;
//!   pas de liste des suffixes publics : le client ne parle qu'à GRDF ;
//! - le port n'isole pas les cookies (comme dans la RFC) ;
//! - `Secure` : envoyé seulement en https ;
//! - même nom, domaine et chemin : remplacement ; date dépassée : suppression ;
//! - envoi : chemins les plus longs d'abord, puis ordre d'arrivée.
//!
//! Les valeurs sont des secrets (session, e-mail du cookie `ln`) : `Debug`
//! ne montre que les noms, et [`CookieJar::store`] rend ce qu'il a fait d'un
//! `Set-Cookie` ([`Stored`] : nom, domaine, chemin, ou motif du refus) pour
//! le journal de diagnostic, sans la valeur. L'heure est passée en
//! paramètre pour les tests.
//!
//! Fonctions pures. Dépendances : `url`, `jiff`.

use std::fmt;

use jiff::{SignedDuration, Timestamp};

use crate::url::Url;

/// Au-delà, un `Set-Cookie` est ignoré (protection contre un serveur fou).
const MAX_COOKIE_LEN: usize = 4096;
const MAX_COOKIES: usize = 100;

#[derive(Clone, PartialEq, Eq)]
struct Cookie {
    name: String,
    value: String,
    domain: String,
    host_only: bool,
    path: String,
    secure: bool,
    expires: Option<Timestamp>,
}

impl Cookie {
    fn same_key(&self, other: &Cookie) -> bool {
        self.name == other.name && self.domain == other.domain && self.path == other.path
    }

    fn expired(&self, now: Timestamp) -> bool {
        self.expires.is_some_and(|t| t <= now)
    }

    fn matches(&self, url: &Url) -> bool {
        let host_ok = if self.host_only {
            url.host() == self.domain
        } else {
            domain_match(url.host(), &self.domain)
        };
        host_ok && path_match(url.path(), &self.path) && (!self.secure || url.is_https())
    }
}

/// Longueur maximale d'un nom de cookie repris dans le journal.
const MAX_NAME_CHARS: usize = 64;

/// Ce qu'est devenu un `Set-Cookie` reçu : de quoi diagnostiquer les
/// domaines des cookies entre les hôtes de GRDF, **jamais la valeur**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stored {
    /// Enregistré, ou remplace le cookie de même nom, domaine et chemin.
    Kept {
        name: String,
        domain: String,
        /// Renvoyé seulement à cet hôte (pas d'attribut `Domain`).
        host_only: bool,
        path: String,
        secure: bool,
    },
    /// Date dépassée : le cookie de même clé est supprimé.
    Expired { name: String },
    /// Ignoré, avec le motif.
    Rejected {
        name: Option<String>,
        reason: String,
    },
}

impl fmt::Display for Stored {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short = |name: &str| name.chars().take(MAX_NAME_CHARS).collect::<String>();
        match self {
            Stored::Kept {
                name,
                domain,
                host_only,
                path,
                secure,
            } => {
                let scope = if *host_only {
                    format!("hôte {domain} seul")
                } else {
                    format!("domaine {domain} et sous-domaines")
                };
                let secure = if *secure { ", Secure" } else { "" };
                write!(f, "{} ({scope}, chemin {path}{secure})", short(name))
            }
            Stored::Expired { name } => write!(f, "{} supprimé (expiré)", short(name)),
            Stored::Rejected { name, reason } => match name {
                Some(name) => write!(f, "{} ignoré : {reason}", short(name)),
                None => write!(f, "en-tête ignoré : {reason}"),
            },
        }
    }
}

/// Cookies de la session HTTP.
#[derive(Clone, Default)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
}

impl CookieJar {
    pub fn new() -> CookieJar {
        CookieJar::default()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// Enregistre un en-tête `Set-Cookie` reçu en réponse à `url`. Un
    /// en-tête invalide est ignoré, comme le ferait un navigateur ; le
    /// résultat dit ce qui a été fait, pour le journal.
    pub fn store(&mut self, set_cookie: &str, url: &Url, now: Timestamp) -> Stored {
        match parse_set_cookie(set_cookie, url, now) {
            Ok(cookie) => self.put(cookie, now),
            Err(reason) => Stored::Rejected {
                name: cookie_name(set_cookie),
                reason,
            },
        }
    }

    /// Pose un cookie de session pour l'hôte de `url` (chemin `/`), comme le
    /// `session.cookies.set("ln", …)` de PyGazpar.
    pub fn insert(&mut self, name: &str, value: &str, url: &Url) {
        let cookie = Cookie {
            name: name.to_owned(),
            value: value.to_owned(),
            domain: url.host().to_owned(),
            host_only: true,
            path: "/".to_owned(),
            secure: false,
            expires: None,
        };
        self.put(cookie, Timestamp::MIN);
    }

    fn put(&mut self, cookie: Cookie, now: Timestamp) -> Stored {
        self.cookies.retain(|c| !c.same_key(&cookie));
        if cookie.expired(now) {
            return Stored::Expired { name: cookie.name };
        }
        if self.cookies.len() >= MAX_COOKIES {
            return Stored::Rejected {
                name: Some(cookie.name),
                reason: format!("plus de {MAX_COOKIES} cookies"),
            };
        }
        let stored = Stored::Kept {
            name: cookie.name.clone(),
            domain: cookie.domain.clone(),
            host_only: cookie.host_only,
            path: cookie.path.clone(),
            secure: cookie.secure,
        };
        self.cookies.push(cookie);
        stored
    }

    /// Valeur de l'en-tête `Cookie` pour une requête vers `url`, ou `None`.
    /// Les cookies expirés sont supprimés au passage.
    pub fn header(&mut self, url: &Url, now: Timestamp) -> Option<String> {
        self.cookies.retain(|c| !c.expired(now));
        let mut selected: Vec<&Cookie> = self.cookies.iter().filter(|c| c.matches(url)).collect();
        // Tri stable : à longueur égale, l'ordre d'arrivée est conservé.
        selected.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        let pairs: Vec<String> = selected
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        (!pairs.is_empty()).then(|| pairs.join("; "))
    }
}

/// Noms et portées seulement : les valeurs sont des secrets.
impl fmt::Debug for CookieJar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(
                self.cookies
                    .iter()
                    .map(|c| format!("{}@{}{}", c.name, c.domain, c.path)),
            )
            .finish()
    }
}

/// Nom d'un `Set-Cookie`, s'il se lit (pour le journal).
fn cookie_name(header: &str) -> Option<String> {
    let (name, _) = header.split(';').next()?.split_once('=')?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.chars().take(MAX_NAME_CHARS).collect())
}

/// Cookie d'un `Set-Cookie`, ou le motif du refus (sans la valeur).
fn parse_set_cookie(header: &str, url: &Url, now: Timestamp) -> Result<Cookie, String> {
    if header.len() > MAX_COOKIE_LEN {
        return Err(format!("plus de {MAX_COOKIE_LEN} octets"));
    }
    let mut parts = header.split(';');
    let Some((name, value)) = parts.next().and_then(|p| p.split_once('=')) else {
        return Err("pas de paire nom=valeur".to_owned());
    };
    let (name, value) = (name.trim(), value.trim());
    if name.is_empty() {
        return Err("nom vide".to_owned());
    }

    let mut domain: Option<String> = None;
    let mut path: Option<String> = None;
    let mut secure = false;
    let mut expires: Option<Timestamp> = None;
    let mut max_age: Option<Timestamp> = None;
    for attribute in parts {
        let (key, val) = match attribute.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (attribute.trim(), ""),
        };
        match key.to_ascii_lowercase().as_str() {
            "expires" => expires = parse_cookie_date(val).or(expires),
            "max-age" => max_age = parse_max_age(val, now).or(max_age),
            "domain" if !val.is_empty() => {
                domain = Some(val.trim_start_matches('.').to_ascii_lowercase());
            }
            "path" => path = val.starts_with('/').then(|| val.to_owned()),
            "secure" => secure = true,
            _ => {}
        }
    }

    let (domain, host_only) = match domain {
        Some(d) if d == url.host() => (d, false),
        Some(d) if d.contains('.') && domain_match(url.host(), &d) => (d, false),
        Some(d) => {
            let d: String = d.chars().take(MAX_NAME_CHARS).collect();
            return Err(format!(
                "domaine {d} refusé pour un cookie posé par {}",
                url.host()
            ));
        }
        None => (url.host().to_owned(), true),
    };
    Ok(Cookie {
        name: name.to_owned(),
        value: value.to_owned(),
        domain,
        host_only,
        path: path.unwrap_or_else(|| default_path(url.path())),
        secure,
        expires: max_age.or(expires),
    })
}

/// `Max-Age` : secondes depuis `now` ; zéro ou négatif = expiré.
fn parse_max_age(val: &str, now: Timestamp) -> Option<Timestamp> {
    let digits = val.strip_prefix('-').unwrap_or(val);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if val.starts_with('-') {
        return Some(Timestamp::MIN);
    }
    let seconds = digits.parse::<i64>().unwrap_or(i64::MAX);
    if seconds == 0 {
        return Some(Timestamp::MIN);
    }
    Some(
        now.checked_add(SignedDuration::from_secs(seconds))
            .unwrap_or(Timestamp::MAX),
    )
}

/// L'hôte appartient-il au domaine (égal, ou sous-domaine d'un nom) ?
fn domain_match(host: &str, domain: &str) -> bool {
    host == domain
        || (host.ends_with(domain)
            && host[..host.len() - domain.len()].ends_with('.')
            && !host.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            && !host.starts_with('['))
}

/// Chemin de la requête couvert par le chemin du cookie (RFC 6265 §5.1.4).
fn path_match(request_path: &str, cookie_path: &str) -> bool {
    request_path == cookie_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/')
                || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')))
}

/// Chemin par défaut : le « répertoire » du chemin de la requête.
fn default_path(request_path: &str) -> String {
    match request_path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(i) => request_path[..i].to_owned(),
    }
}

/// Date d'un cookie, lue avec tolérance (RFC 6265 §5.1.1) : accepte
/// `Wed, 21 Oct 2015 07:28:00 GMT`, `Wednesday, 21-Oct-15 07:28:00 GMT`,
/// `Wed Oct 21 07:28:00 2015`… Toujours en UTC.
pub fn parse_cookie_date(text: &str) -> Option<Timestamp> {
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    let tokens = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == ':'))
        .filter(|t| !t.is_empty());
    for token in tokens {
        if time.is_none()
            && let Some(t) = parse_time(token)
        {
            time = Some(t);
            continue;
        }
        let numeric = token.bytes().all(|b| b.is_ascii_digit());
        if day.is_none() && numeric && token.len() <= 2 {
            day = token.parse::<i8>().ok();
        } else if month.is_none() && token.len() >= 3 && month_number(token).is_some() {
            month = month_number(token);
        } else if year.is_none() && numeric && (2..=4).contains(&token.len()) {
            year = token.parse::<i16>().ok();
        }
    }
    let (hour, minute, second) = time?;
    let year = match year? {
        y @ 70..=99 => y + 1900,
        y @ 0..=69 => y + 2000,
        y => y,
    };
    if year < 1601 {
        return None;
    }
    let datetime = jiff::civil::DateTime::new(year, month?, day?, hour, minute, second, 0).ok()?;
    jiff::tz::TimeZone::UTC.to_timestamp(datetime).ok()
}

fn parse_time(token: &str) -> Option<(i8, i8, i8)> {
    let mut parts = token.split(':');
    let mut next = |max: i8| -> Option<i8> {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 2 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse::<i8>().ok().filter(|v| *v <= max)
    };
    let time = (next(23)?, next(59)?, next(59)?);
    parts.next().is_none().then_some(time)
}

fn month_number(token: &str) -> Option<i8> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let prefix = token.get(..3)?.to_ascii_lowercase();
    let index = MONTHS.iter().position(|m| *m == prefix)?;
    i8::try_from(index + 1).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::http(s).unwrap()
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn now() -> Timestamp {
        ts("2026-11-16T12:00:00Z")
    }

    fn jar_with(headers: &[&str], from: &str) -> CookieJar {
        let mut jar = CookieJar::new();
        for h in headers {
            jar.store(h, &url(from), now());
        }
        jar
    }

    fn header(jar: &mut CookieJar, to: &str) -> Option<String> {
        jar.header(&url(to), now())
    }

    // --- Compte rendu pour le journal (jamais la valeur) ---

    #[test]
    fn store_reports_name_scope_and_never_the_value() {
        let mut jar = CookieJar::new();
        let from = url("https://connexion.grdf.fr/idp/idx/identify");
        let kept = jar.store("DT=SECRET-DT; Path=/; HttpOnly; Secure", &from, now());
        assert_eq!(
            kept.to_string(),
            "DT (hôte connexion.grdf.fr seul, chemin /, Secure)"
        );
        let wide = jar.store("JSESSIONID=SECRET; Domain=.grdf.fr", &from, now());
        assert_eq!(
            wide.to_string(),
            "JSESSIONID (domaine grdf.fr et sous-domaines, chemin /idp/idx)"
        );
        let expired = jar.store("DT=; Max-Age=0", &from, now());
        assert_eq!(expired.to_string(), "DT supprimé (expiré)");
        let foreign = jar.store("x=SECRET; Domain=example.org", &from, now());
        assert_eq!(
            foreign.to_string(),
            "x ignoré : domaine example.org refusé pour un cookie posé par connexion.grdf.fr"
        );
        assert_eq!(
            jar.store("SECRET-sans-egal", &from, now()).to_string(),
            "en-tête ignoré : pas de paire nom=valeur"
        );
        assert_eq!(
            jar.store("=SECRET", &from, now()).to_string(),
            "en-tête ignoré : nom vide"
        );
        let long = format!("big={}", "v".repeat(5000));
        assert_eq!(
            jar.store(&long, &from, now()).to_string(),
            "big ignoré : plus de 4096 octets"
        );
        for stored in [kept, wide, expired, foreign] {
            assert!(!format!("{stored} {stored:?}").contains("SECRET"));
        }
    }

    // --- Lecture de Set-Cookie ---

    #[test]
    fn name_value_trimmed_and_attributes_ignored_case() {
        let mut jar = jar_with(
            &[" sid = abc=def ; PATH=/ ; HttpOnly; SameSite=Lax"],
            "https://monespace.grdf.fr/x",
        );
        assert_eq!(
            header(&mut jar, "https://monespace.grdf.fr/"),
            Some("sid=abc=def".into())
        );
    }

    #[test]
    fn invalid_headers_are_ignored() {
        let long = format!("a={}", "x".repeat(MAX_COOKIE_LEN));
        let jar = jar_with(
            &["", "sansegal", "=valeur", "; a=b", &long],
            "https://h.fr/",
        );
        assert!(jar.is_empty());
    }

    #[test]
    fn empty_value_is_kept() {
        let mut jar = jar_with(&["vide="], "https://h.fr/");
        assert_eq!(header(&mut jar, "https://h.fr/"), Some("vide=".into()));
    }

    // --- Domaine ---

    #[test]
    fn host_only_without_domain() {
        let mut jar = jar_with(&["a=1"], "https://monespace.grdf.fr/");
        assert!(header(&mut jar, "https://monespace.grdf.fr/").is_some());
        assert!(header(&mut jar, "https://connexion.grdf.fr/").is_none());
        assert!(header(&mut jar, "https://x.monespace.grdf.fr/").is_none());
    }

    #[test]
    fn domain_covers_subdomains() {
        let mut jar = jar_with(&["a=1; Domain=.GRDF.fr"], "https://connexion.grdf.fr/");
        assert!(header(&mut jar, "https://monespace.grdf.fr/api").is_some());
        assert!(header(&mut jar, "https://grdf.fr/").is_some());
        assert!(header(&mut jar, "https://evilgrdf.fr/").is_none());
        assert!(header(&mut jar, "https://grdf.fr.evil.com/").is_none());
    }

    #[test]
    fn foreign_or_top_level_domain_is_refused() {
        let jar = jar_with(
            &[
                "a=1; Domain=autre.fr",
                "b=1; Domain=fr",
                "c=1; Domain=x.connexion.grdf.fr",
            ],
            "https://connexion.grdf.fr/",
        );
        assert!(jar.is_empty(), "{jar:?}");
    }

    #[test]
    fn ip_address_host() {
        let mut jar = jar_with(&["a=1", "b=2; Domain=127.0.0.1"], "http://127.0.0.1:4000/");
        // Le port n'isole pas les cookies.
        assert_eq!(
            header(&mut jar, "http://127.0.0.1:5000/"),
            Some("a=1; b=2".into())
        );
        let jar = jar_with(&["c=1; Domain=0.0.1"], "http://127.0.0.1/");
        assert!(jar.is_empty());
    }

    // --- Chemin ---

    #[test]
    fn path_matching() {
        let mut jar = jar_with(&["a=1; Path=/api"], "https://h.fr/");
        assert!(header(&mut jar, "https://h.fr/api").is_some());
        assert!(header(&mut jar, "https://h.fr/api/e-conso").is_some());
        assert!(header(&mut jar, "https://h.fr/apix").is_none());
        assert!(header(&mut jar, "https://h.fr/").is_none());
        let mut jar = jar_with(&["a=1; Path=/api/"], "https://h.fr/");
        assert!(header(&mut jar, "https://h.fr/api/x").is_some());
        assert!(header(&mut jar, "https://h.fr/api").is_none());
    }

    #[test]
    fn default_path_is_directory_of_request() {
        assert_eq!(default_path("/"), "/");
        assert_eq!(default_path("/login"), "/");
        assert_eq!(default_path("/idp/idx/identify"), "/idp/idx");
        assert_eq!(default_path(""), "/");
        let mut jar = jar_with(
            &["a=1", "b=2; Path=relatif"],
            "https://h.fr/idp/idx/identify",
        );
        assert_eq!(
            header(&mut jar, "https://h.fr/idp/idx/challenge/answer"),
            Some("a=1; b=2".into())
        );
        assert!(header(&mut jar, "https://h.fr/").is_none());
    }

    #[test]
    fn longest_path_first_then_arrival_order() {
        let mut jar = jar_with(
            &["court=1; Path=/", "b=2; Path=/api/e-conso", "a=3; Path=/"],
            "https://h.fr/",
        );
        assert_eq!(
            header(&mut jar, "https://h.fr/api/e-conso/pce"),
            Some("b=2; court=1; a=3".into())
        );
    }

    // --- Secure ---

    #[test]
    fn secure_only_over_https() {
        let mut jar = jar_with(&["a=1; Secure", "b=2"], "https://h.fr/");
        assert_eq!(header(&mut jar, "https://h.fr/"), Some("a=1; b=2".into()));
        assert_eq!(header(&mut jar, "http://h.fr/"), Some("b=2".into()));
    }

    // --- Remplacement et expiration ---

    #[test]
    fn same_name_domain_path_replaces() {
        let mut jar = jar_with(
            &["sid=ancien", "sid=nouveau", "sid=autre; Path=/api"],
            "https://h.fr/",
        );
        assert_eq!(jar.len(), 2);
        assert_eq!(
            header(&mut jar, "https://h.fr/"),
            Some("sid=nouveau".into())
        );
    }

    #[test]
    fn expiry_with_injected_clock() {
        let mut jar = CookieJar::new();
        let u = url("https://h.fr/");
        jar.store("a=1; Max-Age=60", &u, now());
        jar.store("b=2; Expires=Mon, 16 Nov 2026 13:00:00 GMT", &u, now());
        jar.store("c=3", &u, now());
        assert_eq!(jar.header(&u, now()), Some("a=1; b=2; c=3".into()));
        assert_eq!(
            jar.header(&u, ts("2026-11-16T12:00:59Z")),
            Some("a=1; b=2; c=3".into())
        );
        assert_eq!(
            jar.header(&u, ts("2026-11-16T12:01:00Z")),
            Some("b=2; c=3".into())
        );
        assert_eq!(
            jar.header(&u, ts("2026-11-16T13:00:00Z")),
            Some("c=3".into())
        );
        assert_eq!(jar.len(), 1, "les cookies expirés sont retirés");
        // Un cookie de session ne périme pas.
        assert_eq!(
            jar.header(&u, ts("2099-01-01T00:00:00Z")),
            Some("c=3".into())
        );
    }

    #[test]
    fn max_age_wins_over_expires_whatever_the_order() {
        let u = url("https://h.fr/");
        let mut jar = CookieJar::new();
        jar.store(
            "a=1; Max-Age=10; Expires=Fri, 01 Jan 2100 00:00:00 GMT",
            &u,
            now(),
        );
        jar.store(
            "b=1; Expires=Fri, 01 Jan 2100 00:00:00 GMT; Max-Age=10",
            &u,
            now(),
        );
        assert_eq!(jar.header(&u, ts("2026-11-16T12:00:11Z")), None);
    }

    #[test]
    fn deletion_by_past_date_or_non_positive_max_age() {
        let u = url("https://h.fr/");
        for deletion in [
            "sid=; Max-Age=0",
            "sid=; Max-Age=-1",
            "sid=; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
        ] {
            let mut jar = CookieJar::new();
            jar.store("sid=abc", &u, now());
            jar.store(deletion, &u, now());
            assert!(jar.is_empty(), "{deletion}");
        }
    }

    #[test]
    fn invalid_max_age_or_expires_is_ignored() {
        let u = url("https://h.fr/");
        let mut jar = CookieJar::new();
        jar.store("a=1; Max-Age=bientôt; Expires=demain", &u, now());
        jar.store("b=1; Max-Age=; Max-Age=1x", &u, now());
        assert_eq!(
            jar.header(&u, ts("2099-01-01T00:00:00Z")),
            Some("a=1; b=1".into())
        );
        jar.store("c=1; Max-Age=99999999999999999999", &u, now());
        assert_eq!(jar.len(), 3);
    }

    #[test]
    fn insert_is_host_only_session_cookie() {
        let mut jar = CookieJar::new();
        jar.insert(
            "ln",
            "jean@example.org",
            &url("https://connexion.grdf.fr/x/y"),
        );
        assert_eq!(
            header(&mut jar, "https://connexion.grdf.fr/idp/idx/identify"),
            Some("ln=jean@example.org".into())
        );
        assert!(header(&mut jar, "https://monespace.grdf.fr/").is_none());
    }

    #[test]
    fn too_many_cookies_are_dropped() {
        let u = url("https://h.fr/");
        let mut jar = CookieJar::new();
        for i in 0..(MAX_COOKIES + 10) {
            jar.store(&format!("c{i}=1"), &u, now());
        }
        assert_eq!(jar.len(), MAX_COOKIES);
    }

    #[test]
    fn debug_shows_names_not_values() {
        let mut jar = jar_with(&["sid=SECRET-SESSION"], "https://h.fr/");
        jar.insert("ln", "jean@example.org", &url("https://h.fr/"));
        let shown = format!("{jar:?}");
        assert!(
            !shown.contains("SECRET") && !shown.contains("jean"),
            "{shown}"
        );
        assert!(shown.contains("sid@h.fr/"), "{shown}");
    }

    // --- Dates ---

    #[test]
    fn cookie_date_formats() {
        let expected = ts("2015-10-21T07:28:00Z");
        for text in [
            "Wed, 21 Oct 2015 07:28:00 GMT",
            "Wednesday, 21-Oct-15 07:28:00 GMT",
            "Wed, 21-Oct-2015 07:28:00 GMT",
            "Wed Oct 21 07:28:00 2015",
            "21 october 2015 7:28:0",
        ] {
            assert_eq!(parse_cookie_date(text), Some(expected), "{text}");
        }
        assert_eq!(
            parse_cookie_date("Thu, 01 Jan 1970 00:00:00 GMT"),
            Some(Timestamp::UNIX_EPOCH)
        );
        assert_eq!(
            parse_cookie_date("Sat, 01 Jan 2069 00:00:00 GMT"),
            Some(ts("2069-01-01T00:00:00Z"))
        );
    }

    #[test]
    fn invalid_cookie_dates() {
        for text in [
            "",
            "demain",
            "Wed, 21 Oct 2015 GMT",
            "Wed, 21 Foo 2015 07:28:00 GMT",
            "Wed, 31 Feb 2015 07:28:00 GMT",
            "Wed, 21 Oct 2015 24:00:00 GMT",
            "Wed, 21 Oct 2015 07:60:00 GMT",
            "Wed, 21 Oct 1600 07:28:00 GMT",
            "Wed, 21 Oct 07:28:00 GMT",
        ] {
            assert_eq!(parse_cookie_date(text), None, "{text}");
        }
    }
}

//! Transport HTTP vers GRDF : seul module du programme qui importe `ureq`.
//!
//! - délais de 10 s pour la connexion et de 60 s par lecture, comme PyGazpar ;
//! - `User-Agent` de navigateur, comme PyGazpar ;
//! - corps de réponse borné à [`MAX_BODY_BYTES`], lu en UTF-8 (avec
//!   remplacement des octets invalides) ;
//! - statuts HTTP rendus tels quels, jamais transformés en erreur ; une
//!   panne de transport devient [`Error::Http`] « GRDF injoignable (…) » ;
//! - redirections **suivies à la main** (301, 302, 303, 307, 308 ; 303, et
//!   301/302 sur un POST, deviennent un GET sans corps), au plus
//!   [`MAX_REDIRECTS`] sauts, pour lire `Set-Cookie` à chaque saut et
//!   renvoyer les bons cookies au suivant ([`super::cookies`]) ;
//! - **hôtes limités** ([`AllowedHosts`]) : chaque requête, première comme
//!   redirection, ne part que vers un hôte autorisé, c'est-à-dire, en
//!   production, `grdf.fr` et ses sous-domaines (manuel §9) ; sinon erreur
//!   [`Error::Protocol`], qui ne nomme que l'hôte refusé ;
//! - variables `HTTP(S)_PROXY` ignorées : le démon parle directement à GRDF.
//!
//! Interface étroite, sans type `ureq` : [`HttpClient::get`],
//! [`HttpClient::post_json`], [`HttpClient::set_cookie`] et [`Response`].
//! Le suivi des redirections est testé avec un faux transport ; le
//! transport `ureq` est mince et testé contre un serveur local.
//!
//! Aucune adresse complète (sa requête peut porter un jeton), aucun cookie
//! ni corps de requête n'apparaît dans un journal ou une erreur.
//!
//! Dépendances : `ureq`, `jiff`, `serde_json`, `error`, `log`,
//! `url`, `grdf::cookies`.

use std::fmt;
use std::time::Duration;

use jiff::Timestamp;

use super::cookies::CookieJar;
use crate::error::Error;
use crate::url::Url;

/// `User-Agent` envoyé, celui de PyGazpar.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64)";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// Taille maximale d'un corps de réponse. Un an de relevés quotidiens pèse
/// environ 260 Ko.
const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;
/// Nombre maximal de redirections suivies pour une requête.
const MAX_REDIRECTS: usize = 10;

/// Hôtes vers lesquels le client accepte d'envoyer une requête, déduits des
/// adresses configurées (Mon Espace GRDF et connexion).
///
/// Pour chaque hôte configuré :
///
/// - `localhost` ou adresse IP (faux serveur de développement) : cet hôte
///   exactement ;
/// - nom d'au moins trois étiquettes (`monespace.grdf.fr`) : son domaine
///   parent (`grdf.fr`) et tous ses sous-domaines ;
/// - nom de deux étiquettes (`grdf.fr`) : ce domaine et ses sous-domaines ;
/// - nom d'une seule étiquette : cet hôte exactement.
///
/// La comparaison se fait étiquette par étiquette : `evilgrdf.fr` et
/// `grdf.fr.example.org` ne sont pas dans `grdf.fr`. Le port n'est pas
/// comparé. Hypothèse : le domaine parent n'est pas un suffixe public
/// (`co.uk`), ce que garantit la configuration (`grdf.fr` ou `localhost`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedHosts {
    rules: Vec<HostRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostRule {
    /// Cet hôte exactement.
    Exact(String),
    /// Ce domaine et ses sous-domaines.
    Domain(String),
}

impl AllowedHosts {
    /// Hôtes autorisés pour ces adresses configurées.
    pub fn from_urls(urls: &[&str]) -> Result<AllowedHosts, Error> {
        let mut rules = Vec::new();
        for url in urls {
            let rule = HostRule::for_configured(Url::http(url)?.host());
            if !rules.contains(&rule) {
                rules.push(rule);
            }
        }
        Ok(AllowedHosts { rules })
    }

    /// `host` est-il autorisé ? (Fonction pure ; casse indifférente.)
    pub fn allows(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.rules.iter().any(|rule| match rule {
            HostRule::Exact(exact) => host == *exact,
            HostRule::Domain(domain) => {
                host == *domain
                    || host
                        .strip_suffix(domain.as_str())
                        .is_some_and(|sub| sub.len() > 1 && sub.ends_with('.'))
            }
        })
    }

    /// Hôtes autorisés, pour un message : `grdf.fr`, `127.0.0.1`.
    pub fn describe(&self) -> String {
        let names: Vec<&str> = self
            .rules
            .iter()
            .map(|rule| match rule {
                HostRule::Exact(host) | HostRule::Domain(host) => host.as_str(),
            })
            .collect();
        names.join(", ")
    }

    /// Erreur si l'hôte de `url` n'est pas autorisé ; le message ne nomme
    /// que l'hôte (jamais le chemin ni la requête).
    fn check(&self, url: &Url, redirected: bool) -> Result<(), Error> {
        if self.allows(url.host()) {
            return Ok(());
        }
        let what = if redirected {
            "redirection"
        } else {
            "requête"
        };
        Err(Error::Protocol(format!(
            "{what} vers l'hôte {} refusée : seuls {} sont autorisés",
            url.host(),
            self.describe()
        )))
    }
}

impl HostRule {
    fn for_configured(host: &str) -> HostRule {
        let host = host.to_ascii_lowercase();
        let is_ip = host.starts_with('[') || host.parse::<std::net::Ipv4Addr>().is_ok();
        let labels = host.split('.').count();
        if is_ip || host == "localhost" || labels < 2 {
            HostRule::Exact(host)
        } else if labels == 2 {
            HostRule::Domain(host)
        } else {
            // Au moins trois étiquettes : il y a un point.
            let parent = host.split_once('.').map_or(host.as_str(), |(_, p)| p);
            HostRule::Domain(parent.to_owned())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// Réponse finale, après les redirections.
pub struct Response {
    pub status: u16,
    /// Adresse de la dernière requête (après redirections).
    pub final_url: Url,
    pub content_type: Option<String>,
    pub body: String,
    /// En-tête `Retry-After` brut.
    pub retry_after: Option<String>,
}

impl Response {
    /// Page HTML (et non JSON) ?
    pub fn is_html(&self) -> bool {
        self.content_type
            .as_deref()
            .is_some_and(|t| t.to_ascii_lowercase().contains("text/html"))
    }
}

/// Sans le corps ni la requête de l'adresse, qui peuvent porter des secrets.
impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("final_url", &self.final_url)
            .field("content_type", &self.content_type)
            .field("body_len", &self.body.len())
            .field("retry_after", &self.retry_after)
            .finish()
    }
}

/// Une requête, sans redirection.
struct RawRequest {
    method: Method,
    url: Url,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

/// Une réponse, sans redirection. Noms d'en-têtes en minuscules.
struct RawResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl RawResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Envoi d'une seule requête : `ureq` en production, faux transport en test.
trait Transport {
    fn send(&mut self, request: &RawRequest) -> Result<RawResponse, Error>;
}

/// Client HTTP avec sa session (cookies). Une nouvelle session = un nouveau
/// client.
pub struct HttpClient {
    transport: Box<dyn Transport>,
    jar: CookieJar,
    allowed: AllowedHosts,
}

impl fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpClient")
            .field("cookies", &self.jar)
            .field("allowed", &self.allowed)
            .finish_non_exhaustive()
    }
}

impl HttpClient {
    /// Client qui n'envoie de requête que vers `allowed`.
    pub fn new(allowed: AllowedHosts) -> HttpClient {
        HttpClient::with_transport(Box::new(UreqTransport::new(MAX_BODY_BYTES)), allowed)
    }

    fn with_transport(transport: Box<dyn Transport>, allowed: AllowedHosts) -> HttpClient {
        HttpClient {
            transport,
            jar: CookieJar::new(),
            allowed,
        }
    }

    /// Hôtes vers lesquels ce client accepte d'envoyer une requête.
    pub fn allowed_hosts(&self) -> &AllowedHosts {
        &self.allowed
    }

    /// `GET url?query`, paramètres encodés ici, redirections suivies.
    pub fn get(
        &mut self,
        url: &str,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
    ) -> Result<Response, Error> {
        let url = Url::http(url)?.with_query(query);
        self.execute(Method::Get, url, headers, None)
    }

    /// `POST` d'un corps JSON (`Content-Type: application/json` sauf s'il est
    /// fourni), redirections suivies.
    pub fn post_json(
        &mut self,
        url: &str,
        body: &serde_json::Value,
        headers: &[(&str, &str)],
    ) -> Result<Response, Error> {
        let url = Url::http(url)?;
        let mut all: Vec<(&str, &str)> = headers.to_vec();
        if !all
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        {
            all.push(("Content-Type", "application/json"));
        }
        self.execute(Method::Post, url, &all, Some(body.to_string().into_bytes()))
    }

    /// Pose un cookie de session pour l'hôte de `url`.
    pub fn set_cookie(&mut self, name: &str, value: &str, url: &Url) {
        self.jar.insert(name, value, url);
    }

    fn execute(
        &mut self,
        mut method: Method,
        mut url: Url,
        headers: &[(&str, &str)],
        mut body: Option<Vec<u8>>,
    ) -> Result<Response, Error> {
        let mut headers: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
        {
            headers.insert(0, ("User-Agent".to_owned(), USER_AGENT.to_owned()));
        }

        for hop in 0..=MAX_REDIRECTS {
            // Avant tout envoi, cookies compris.
            self.allowed.check(&url, hop > 0)?;
            let mut sent = headers.clone();
            if let Some(cookie) = self.jar.header(&url, Timestamp::now()) {
                sent.push(("Cookie".to_owned(), cookie));
            }
            let request = RawRequest {
                method,
                url,
                headers: sent,
                body,
            };
            let raw = self.transport.send(&request)?;
            let RawRequest {
                url: sent_url,
                body: sent_body,
                ..
            } = request;
            url = sent_url;
            body = sent_body;
            debug!("HTTP {method:?} {} → {}", url.redacted(), raw.status);

            let now = Timestamp::now();
            for (_, value) in raw.headers.iter().filter(|(k, _)| k == "set-cookie") {
                self.jar.store(value, &url, now);
            }

            let next = next_method(raw.status, method);
            let location = raw.header("location");
            let (Some(next), Some(location)) = (next, location) else {
                return Ok(Response {
                    status: raw.status,
                    content_type: raw.header("content-type").map(str::to_owned),
                    retry_after: raw.header("retry-after").map(str::to_owned),
                    final_url: url,
                    body: raw.body,
                });
            };
            url = url.resolve(location)?;
            if next != method || raw.status == 303 {
                body = None;
                headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
            }
            method = next;
        }
        Err(Error::Protocol(format!(
            "plus de {MAX_REDIRECTS} redirections, abandon"
        )))
    }
}

/// Méthode de la requête suivante si `status` est une redirection suivie.
fn next_method(status: u16, method: Method) -> Option<Method> {
    match status {
        301 | 302 if method == Method::Post => Some(Method::Get),
        301 | 302 | 307 | 308 => Some(method),
        303 => Some(Method::Get),
        _ => None,
    }
}

/// Transport réel, au-dessus d'un agent `ureq` sans redirection automatique.
struct UreqTransport {
    agent: ureq::Agent,
    max_body: u64,
}

impl UreqTransport {
    fn new(max_body: u64) -> UreqTransport {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .user_agent(USER_AGENT)
            .timeout_resolve(Some(CONNECT_TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_per_read(Some(READ_TIMEOUT))
            .build();
        UreqTransport {
            agent: ureq::Agent::new_with_config(config),
            max_body,
        }
    }
}

impl Transport for UreqTransport {
    fn send(&mut self, request: &RawRequest) -> Result<RawResponse, Error> {
        let uri = request.url.full();
        let result = match request.method {
            Method::Get => {
                let mut builder = self.agent.get(&uri);
                for (k, v) in &request.headers {
                    builder = builder.header(k.as_str(), v.as_str());
                }
                builder.call()
            }
            Method::Post => {
                let mut builder = self.agent.post(&uri);
                for (k, v) in &request.headers {
                    builder = builder.header(k.as_str(), v.as_str());
                }
                builder.send(request.body.as_deref().unwrap_or_default())
            }
        };
        let mut response = result.map_err(transport_error)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned())))
            .collect();
        let body = response
            .body_mut()
            .with_config()
            .limit(self.max_body)
            .lossy_utf8(true)
            .read_to_string()
            .map_err(transport_error)?;
        Ok(RawResponse {
            status,
            headers,
            body,
        })
    }
}

/// Erreur `ureq` en message français qui nomme GRDF (« GRDF injoignable
/// (délai dépassé) »), sans adresse ni en-tête : certaines variantes
/// d'`ureq` recopient l'adresse ou un en-tête reçu. Ce client ne parle qu'à
/// GRDF, le contexte est donc donné ici, à la source.
fn transport_error(e: ureq::Error) -> Error {
    let message = match e {
        ureq::Error::Timeout(_) => "délai dépassé".to_owned(),
        ureq::Error::HostNotFound => "hôte introuvable".to_owned(),
        ureq::Error::ConnectionFailed => "connexion impossible".to_owned(),
        ureq::Error::Io(io) => format!("entrée-sortie : {}", io.kind()),
        ureq::Error::BodyExceedsLimit(n) => {
            format!("réponse trop volumineuse (plus de {n} octets)")
        }
        ureq::Error::BadUri(_) => "adresse refusée par le client HTTP".to_owned(),
        ureq::Error::Protocol(_) | ureq::Error::BodyStalled => "réponse HTTP invalide".to_owned(),
        ureq::Error::Tls(m) => format!("TLS : {m}"),
        ureq::Error::Rustls(e) => format!("TLS : {e}"),
        _ => "échec de la requête".to_owned(),
    };
    Error::Http(format!("GRDF injoignable ({message})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::rc::Rc;

    // --- Faux transport : réponses programmées, requêtes enregistrées ---

    #[derive(Debug, Clone)]
    struct Seen {
        method: Method,
        url: String,
        headers: Vec<(String, String)>,
        body: Option<String>,
    }

    impl Seen {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    struct Scripted {
        responses: VecDeque<RawResponse>,
        seen: Rc<RefCell<Vec<Seen>>>,
    }

    impl Transport for Scripted {
        fn send(&mut self, request: &RawRequest) -> Result<RawResponse, Error> {
            self.seen.borrow_mut().push(Seen {
                method: request.method,
                url: request.url.full(),
                headers: request.headers.clone(),
                body: request
                    .body
                    .as_ref()
                    .map(|b| String::from_utf8_lossy(b).into_owned()),
            });
            self.responses
                .pop_front()
                .ok_or_else(|| Error::Http("plus de réponse programmée".into()))
        }
    }

    fn raw(status: u16, headers: &[(&str, &str)], body: &str) -> RawResponse {
        RawResponse {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), (*v).to_owned()))
                .collect(),
            body: body.to_owned(),
        }
    }

    fn scripted(responses: Vec<RawResponse>) -> (HttpClient, Rc<RefCell<Vec<Seen>>>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let transport = Scripted {
            responses: responses.into(),
            seen: Rc::clone(&seen),
        };
        let allowed =
            AllowedHosts::from_urls(&["https://monespace.grdf.fr", "https://h.fr"]).unwrap();
        (
            HttpClient::with_transport(Box::new(transport), allowed),
            seen,
        )
    }

    #[test]
    fn redirect_method_table() {
        use Method::*;
        assert_eq!(next_method(301, Get), Some(Get));
        assert_eq!(next_method(302, Get), Some(Get));
        assert_eq!(next_method(301, Post), Some(Get));
        assert_eq!(next_method(302, Post), Some(Get));
        assert_eq!(next_method(303, Post), Some(Get));
        assert_eq!(next_method(303, Get), Some(Get));
        assert_eq!(next_method(307, Post), Some(Post));
        assert_eq!(next_method(308, Post), Some(Post));
        for status in [200, 204, 300, 304, 305, 306, 309, 401, 429, 500] {
            assert_eq!(next_method(status, Get), None, "{status}");
        }
    }

    #[test]
    fn simple_get_with_query_and_headers() {
        let (mut client, seen) = scripted(vec![raw(
            200,
            &[("Content-Type", "application/json"), ("Retry-After", "7")],
            "[]",
        )]);
        let r = client
            .get(
                "https://monespace.grdf.fr/api/e-conso/pce/consommation/informatives",
                &[("dateDebut", "2026-01-01"), ("pceList[]", "123")],
                &[("Accept", "application/json")],
            )
            .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, "[]");
        assert_eq!(r.content_type.as_deref(), Some("application/json"));
        assert_eq!(r.retry_after.as_deref(), Some("7"));
        assert!(!r.is_html());
        let seen = seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].url,
            "https://monespace.grdf.fr/api/e-conso/pce/consommation/informatives?dateDebut=2026-01-01&pceList%5B%5D=123"
        );
        assert_eq!(seen[0].header("user-agent"), Some(USER_AGENT));
        assert_eq!(seen[0].header("accept"), Some("application/json"));
        assert_eq!(seen[0].header("cookie"), None);
    }

    #[test]
    fn post_json_sets_content_type_and_body() {
        let (mut client, seen) = scripted(vec![raw(200, &[], "{}")]);
        let body = serde_json::json!({"identifier": "a@b.fr"});
        client
            .post_json("https://h.fr/idp", &body, &[("Accept", "x")])
            .unwrap();
        let seen = seen.borrow();
        assert_eq!(seen[0].method, Method::Post);
        assert_eq!(seen[0].header("content-type"), Some("application/json"));
        assert_eq!(seen[0].body.as_deref(), Some(r#"{"identifier":"a@b.fr"}"#));
    }

    #[test]
    fn redirects_carry_cookies_across_hosts_and_hops() {
        let (mut client, seen) = scripted(vec![
            raw(
                302,
                &[
                    ("Location", "https://connexion.grdf.fr/oauth2/authorize?x=1"),
                    ("Set-Cookie", "accueil=1; Path=/"),
                ],
                "",
            ),
            raw(
                200,
                &[
                    ("Content-Type", "text/html; charset=utf-8"),
                    ("Set-Cookie", "DT=okta; Path=/"),
                    ("Set-Cookie", "large=1; Domain=grdf.fr; Path=/"),
                ],
                "<html>",
            ),
            raw(200, &[], ""),
            raw(200, &[], ""),
        ]);
        let r = client.get("https://monespace.grdf.fr/", &[], &[]).unwrap();
        assert_eq!(
            r.final_url.full(),
            "https://connexion.grdf.fr/oauth2/authorize?x=1"
        );
        assert!(r.is_html());
        client
            .get("https://connexion.grdf.fr/idp/idx/identify", &[], &[])
            .unwrap();
        client
            .get("https://monespace.grdf.fr/api", &[], &[])
            .unwrap();
        let seen = seen.borrow();
        assert_eq!(seen[0].header("cookie"), None);
        // Le cookie de monespace ne part pas vers connexion.
        assert_eq!(seen[1].header("cookie"), None);
        assert_eq!(seen[2].header("cookie"), Some("DT=okta; large=1"));
        assert_eq!(seen[3].header("cookie"), Some("accueil=1; large=1"));
    }

    #[test]
    fn post_redirected_by_302_becomes_get_without_body() {
        let (mut client, seen) = scripted(vec![
            raw(302, &[("Location", "/suite")], ""),
            raw(200, &[], "ok"),
        ]);
        let r = client
            .post_json(
                "https://h.fr/form",
                &serde_json::json!({"a": 1}),
                &[("Accept", "x")],
            )
            .unwrap();
        assert_eq!(r.body, "ok");
        let seen = seen.borrow();
        assert_eq!(seen[1].method, Method::Get);
        assert_eq!(seen[1].url, "https://h.fr/suite");
        assert_eq!(seen[1].body, None);
        assert_eq!(seen[1].header("content-type"), None);
        assert_eq!(seen[1].header("accept"), Some("x"));
    }

    #[test]
    fn post_redirected_by_307_keeps_method_and_body() {
        for status in [307, 308] {
            let (mut client, seen) = scripted(vec![
                raw(status, &[("Location", "autre")], ""),
                raw(200, &[], ""),
            ]);
            client
                .post_json("https://h.fr/a/form", &serde_json::json!(1), &[])
                .unwrap();
            let seen = seen.borrow();
            assert_eq!(seen[1].method, Method::Post);
            assert_eq!(seen[1].url, "https://h.fr/a/autre");
            assert_eq!(seen[1].body.as_deref(), Some("1"));
            assert_eq!(seen[1].header("content-type"), Some("application/json"));
        }
    }

    #[test]
    fn redirect_303_turns_get_into_get_without_body() {
        let (mut client, seen) = scripted(vec![
            raw(303, &[("Location", "?page=2")], ""),
            raw(200, &[], ""),
        ]);
        client.get("https://h.fr/liste?page=1", &[], &[]).unwrap();
        assert_eq!(seen.borrow()[1].url, "https://h.fr/liste?page=2");
    }

    #[test]
    fn redirect_without_location_is_returned() {
        let (mut client, _) = scripted(vec![raw(302, &[], "sans location")]);
        let r = client.get("https://h.fr/", &[], &[]).unwrap();
        assert_eq!(r.status, 302);
    }

    #[test]
    fn too_many_redirects() {
        let loops: Vec<RawResponse> = (0..20)
            .map(|_| raw(302, &[("Location", "/boucle")], ""))
            .collect();
        let (mut client, seen) = scripted(loops);
        let e = client.get("https://h.fr/", &[], &[]).unwrap_err();
        assert!(e.to_string().contains("redirections"), "{e}");
        assert_eq!(seen.borrow().len(), MAX_REDIRECTS + 1);
    }

    #[test]
    fn redirect_to_other_scheme_is_refused_without_echo() {
        for location in [
            "ftp://h.fr/x?code=SECRET",
            "ws://h.fr/x?code=SECRET",
            "wss://h.fr/x?code=SECRET",
        ] {
            let (mut client, seen) = scripted(vec![
                raw(302, &[("Location", location)], ""),
                raw(200, &[], "ne doit pas être lu"),
            ]);
            let e = client.get("https://h.fr/", &[], &[]).unwrap_err();
            assert_eq!(
                e,
                Error::Protocol(
                    "adresse invalide : seuls http:// et https:// sont pris en charge".into()
                ),
                "{location}"
            );
            assert_eq!(seen.borrow().len(), 1, "{location} : rien n'est envoyé");
        }
    }

    #[test]
    fn request_to_websocket_address_is_refused_before_sending() {
        let (mut client, seen) = scripted(vec![raw(200, &[], "")]);
        assert!(client.get("ws://h.fr/", &[], &[]).is_err());
        assert!(AllowedHosts::from_urls(&["ws://h.fr"]).is_err());
        assert!(seen.borrow().is_empty());
    }

    // --- Hôtes autorisés ---

    fn production() -> AllowedHosts {
        AllowedHosts::from_urls(&["https://monespace.grdf.fr", "https://connexion.grdf.fr"])
            .unwrap()
    }

    #[test]
    fn production_allows_grdf_fr_and_its_subdomains_only() {
        let allowed = production();
        assert_eq!(allowed.describe(), "grdf.fr");
        for host in [
            "monespace.grdf.fr",
            "connexion.grdf.fr",
            "grdf.fr",
            "sso.connexion.grdf.fr",
            "MonEspace.GRDF.fr",
        ] {
            assert!(allowed.allows(host), "{host} devrait être accepté");
        }
        for host in [
            "evilgrdf.fr",
            "grdf.fr.example.org",
            "monespace.grdf.fr.example.org",
            "grdf.com",
            "fr",
            ".grdf.fr",
            "",
            "grdf-fr.okta.com",
            "127.0.0.1",
            "localhost",
        ] {
            assert!(!allowed.allows(host), "{host} devrait être refusé");
        }
    }

    #[test]
    fn local_addresses_must_match_exactly() {
        let allowed =
            AllowedHosts::from_urls(&["http://127.0.0.1:4000", "http://127.0.0.1:4001"]).unwrap();
        assert_eq!(allowed.describe(), "127.0.0.1");
        assert!(allowed.allows("127.0.0.1"));
        for host in ["localhost", "127.0.0.2", "0.0.1", "1.127.0.0.1", "grdf.fr"] {
            assert!(!allowed.allows(host), "{host}");
        }
        let allowed = AllowedHosts::from_urls(&["http://localhost:4000"]).unwrap();
        assert!(allowed.allows("localhost") && allowed.allows("LOCALHOST"));
        for host in ["127.0.0.1", "evil.localhost", "localhost.example.org"] {
            assert!(!allowed.allows(host), "{host}");
        }
        let allowed = AllowedHosts::from_urls(&["http://[::1]:4000"]).unwrap();
        assert!(allowed.allows("[::1]"));
        assert!(!allowed.allows("[::2]"));
    }

    #[test]
    fn short_configured_names() {
        // Domaine de deux étiquettes : lui et ses sous-domaines, jamais « fr ».
        let allowed = AllowedHosts::from_urls(&["https://grdf.fr"]).unwrap();
        assert!(allowed.allows("grdf.fr") && allowed.allows("a.grdf.fr"));
        assert!(!allowed.allows("fr") && !allowed.allows("evil.fr"));
        // Une seule étiquette : exactement.
        let allowed = AllowedHosts::from_urls(&["http://nas:8080"]).unwrap();
        assert!(allowed.allows("nas"));
        assert!(!allowed.allows("a.nas"));
        assert!(AllowedHosts::from_urls(&["pas une adresse"]).is_err());
    }

    #[test]
    fn redirect_to_foreign_host_is_refused_before_sending() {
        let (mut client, seen) = scripted(vec![
            raw(
                302,
                &[(
                    "Location",
                    "https://grdf.fr.example.org/oauth2/authorize?code=SECRET",
                )],
                "",
            ),
            raw(200, &[], "ne doit pas être lu"),
        ]);
        let e = client
            .get("https://monespace.grdf.fr/", &[], &[])
            .unwrap_err();
        assert_eq!(
            e,
            Error::Protocol(
                "redirection vers l'hôte grdf.fr.example.org refusée : \
                 seuls grdf.fr, h.fr sont autorisés"
                    .into()
            )
        );
        assert!(!format!("{e:?}").contains("SECRET"));
        assert_eq!(seen.borrow().len(), 1, "rien n'est envoyé à l'autre hôte");
    }

    #[test]
    fn redirect_within_grdf_fr_is_followed() {
        let (mut client, seen) = scripted(vec![
            raw(302, &[("Location", "https://sso.grdf.fr/x")], ""),
            raw(200, &[], "ok"),
        ]);
        let r = client.get("https://monespace.grdf.fr/", &[], &[]).unwrap();
        assert_eq!(r.body, "ok");
        assert_eq!(seen.borrow()[1].url, "https://sso.grdf.fr/x");
    }

    #[test]
    fn request_to_foreign_host_is_refused_before_sending() {
        let (mut client, seen) = scripted(vec![raw(200, &[], "")]);
        let e = client
            .post_json(
                "https://example.org/idp?stateToken=SECRET",
                &serde_json::json!({}),
                &[],
            )
            .unwrap_err();
        assert!(
            matches!(&e, Error::Protocol(m) if m.starts_with("requête vers l'hôte example.org refusée")),
            "{e}"
        );
        assert!(!e.to_string().contains("SECRET"));
        assert!(seen.borrow().is_empty());
        assert!(client.allowed_hosts().allows("grdf.fr"));
    }

    #[test]
    fn set_cookie_and_cookie_expiry_through_client() {
        let (mut client, seen) = scripted(vec![
            raw(200, &[("Set-Cookie", "sid=1; Max-Age=0")], ""),
            raw(200, &[], ""),
        ]);
        client.set_cookie(
            "ln",
            "jean@example.org",
            &Url::http("https://h.fr/").unwrap(),
        );
        client.get("https://h.fr/", &[], &[]).unwrap();
        client.get("https://h.fr/", &[], &[]).unwrap();
        let seen = seen.borrow();
        assert_eq!(seen[0].header("cookie"), Some("ln=jean@example.org"));
        assert_eq!(seen[1].header("cookie"), Some("ln=jean@example.org"));
        assert!(!format!("{client:?}").contains("jean"));
    }

    #[test]
    fn response_debug_hides_body_and_query() {
        let r = Response {
            status: 200,
            final_url: Url::http("https://h.fr/x?stateToken=SECRET").unwrap(),
            content_type: None,
            body: "SECRET-BODY".into(),
            retry_after: None,
        };
        let shown = format!("{r:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
        assert!(shown.contains("body_len: 11"));
    }

    #[test]
    fn transport_errors_never_echo_addresses() {
        let e = transport_error(ureq::Error::BadUri("https://h/?code=SECRET".into()));
        assert!(!e.to_string().contains("SECRET"));
        let e = transport_error(ureq::Error::HostNotFound);
        assert_eq!(e, Error::Http("GRDF injoignable (hôte introuvable)".into()));
        assert!(matches!(
            transport_error(ureq::Error::ConnectionFailed),
            Error::Http(_)
        ));
    }

    // --- Transport ureq contre un serveur local (127.0.0.1) ---

    /// Serveur d'un nombre fixe de connexions : renvoie les réponses
    /// données, dans l'ordre, et les requêtes reçues (ligne et en-têtes).
    fn serve(responses: Vec<String>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    head.push_str(&line);
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                head.push_str(&String::from_utf8_lossy(&body));
                requests.push(head);
                let mut stream = stream;
                stream.write_all(response.as_bytes()).unwrap();
            }
            requests
        });
        (port, handle)
    }

    fn local(port: u16) -> AllowedHosts {
        AllowedHosts::from_urls(&[&format!("http://127.0.0.1:{port}")]).unwrap()
    }

    fn http_response(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn ureq_plain_http_redirects_and_cookies() {
        let (port, server) = serve(vec![
            http_response(
                "302 Found",
                "Location: /suite\r\nSet-Cookie: sid=abc; Path=/\r\n",
                "",
            ),
            http_response(
                "404 Not Found",
                "Content-Type: text/html\r\nRetry-After: 3\r\n",
                "absent é",
            ),
        ]);
        let mut client = HttpClient::new(local(port));
        let r = client
            .post_json(
                &format!("http://127.0.0.1:{port}/depart"),
                &serde_json::json!({"k": "v"}),
                &[],
            )
            .unwrap();
        assert_eq!(r.status, 404);
        assert_eq!(r.body, "absent é");
        assert!(r.is_html());
        assert_eq!(r.retry_after.as_deref(), Some("3"));
        assert_eq!(r.final_url.full(), format!("http://127.0.0.1:{port}/suite"));
        let requests = server.join().unwrap();
        assert!(
            requests[0].starts_with("POST /depart HTTP/1.1"),
            "{}",
            requests[0]
        );
        assert!(requests[0].contains(r#"{"k":"v"}"#));
        let second = requests[1].to_ascii_lowercase();
        assert!(second.starts_with("get /suite http/1.1"), "{second}");
        assert!(second.contains("cookie: sid=abc"), "{second}");
        assert!(second.contains(&format!("user-agent: {}", USER_AGENT.to_ascii_lowercase())));
    }

    #[test]
    fn ureq_body_limit() {
        let (port, server) = serve(vec![http_response("200 OK", "", &"x".repeat(100))]);
        let mut client = HttpClient::with_transport(Box::new(UreqTransport::new(10)), local(port));
        let e = client
            .get(&format!("http://127.0.0.1:{port}/"), &[], &[])
            .unwrap_err();
        assert!(e.to_string().contains("trop volumineuse"), "{e}");
        server.join().unwrap();
    }

    #[test]
    fn ureq_connection_refused_is_http_error() {
        // Port libéré juste après la réservation : plus personne n'écoute.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let e = HttpClient::new(local(port))
            .get(&format!("http://127.0.0.1:{port}/?code=SECRET"), &[], &[])
            .unwrap_err();
        match &e {
            Error::Http(m) => assert!(m.starts_with("GRDF injoignable ("), "{m}"),
            other => panic!("{other:?}"),
        }
        assert!(e.is_transient());
        assert!(!e.to_string().contains("SECRET"));
    }
}

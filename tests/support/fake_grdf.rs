//! Faux Mon Espace GRDF : deux serveurs HTTP/1.1 minimaux sur
//! `127.0.0.1:0` (« monespace » et « connexion »), bibliothèque standard
//! seulement. Aucun accès au vrai GRDF.
//!
//! Rejoue le flux de connexion Okta IDX de PLAN.md §2.1 :
//!
//! 1. `GET <base>/` sans session → 302 vers `<login>/oauth2/…/authorize`, qui
//!    sert une page contenant `"stateToken":"…\x2D…"` et pose le cookie `DT` ;
//! 2. `POST <login>/idp/idx/identify` : exige `DT`, `ln=<e-mail>`, l'en-tête
//!    `Accept` d'Okta et le bon `stateToken` ; répond avec la vraie réponse
//!    d'Okta capturée par PyGazpar (`okta_identify_response.json`, sans
//!    `stateHandle` à la racine) ;
//! 3. `POST <login>/idp/idx/challenge/answer` : 401 « Authentication failed »
//!    si le mot de passe est faux, sinon `success.href` ;
//! 4. `GET success.href` → 302 vers `<base>/_codexch?code=…` → cookie de
//!    session `auth_token` et 302 vers `/` → 200.
//!
//! Puis l'API : `GET /api/e-conso/pce` et
//! `GET /api/e-conso/pce/consommation/informatives` (jeu d'essai de PyGazpar
//! filtré sur `dateDebut..=dateFin`, `[]` si rien). Les relevés servis
//! peuvent être remplacés ([`FakeGrdf::set_records`]) ou complétés
//! ([`FakeGrdf::publish`], pour un jour publié ou corrigé en retard) :
//! les essais du binaire, qui suit la vraie date du jour, s'en servent.
//!
//! Pannes pilotables ([`Faults`]) : 429, session expirée (401 ou redirection
//! vers la page de connexion), page HTML d'erreur, connexion coupée,
//! redirection ou `success.href` vers un autre hôte (`localhost` au lieu de
//! `127.0.0.1`, donc toujours local si le client le suivait à tort),
//! service de connexion indisponible (503 à `challenge/answer`).
//! Compteurs et anomalies constatées côté serveur : [`Counters`].

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{Value, json};

pub const EMAIL: &str = "jean.dupont@example.org";
pub const PASSWORD: &str = "Mot-de-passe-tres-secret";
pub const PCE: &str = "98765432109876";
/// `stateToken` tel qu'écrit dans la page, puis tel qu'attendu.
pub const STATE_TOKEN_IN_PAGE: &str = r"02.id.FAKE\x2DSTATE\x2DTOKEN";
pub const STATE_TOKEN: &str = "02.id.FAKE-STATE-TOKEN";
pub const STATE_HANDLE: &str = "02.id.FAKE-HANDLE-AFTER-IDENTIFY";
pub const DEVICE_COOKIE_VALUE: &str = "FAKE-DEVICE-COOKIE";
/// Préfixes des valeurs de session et des codes OAuth.
pub const SESSION_PREFIX: &str = "FAKE-SESSION-";
pub const CODE_PREFIX: &str = "FAKE-CODE-";
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64)";

/// Relevés servis : extrait de `donnees_informatives.json` de PyGazpar.
pub const FIXTURE: &str = include_str!("../fixtures/pygazpar/donnees_informatives_extrait.json");
const OKTA_IDENTIFY: &str = include_str!("../fixtures/pygazpar/okta_identify_response.json");
const OKTA_FIXTURE_HANDLE: &str =
    "02.id.FIXTURE-STATE-HANDLE-0000000000000000000000000000000000000000000000000000000000000000";

/// Forme d'une session expirée.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Expiry {
    /// 401 sur l'appel d'API.
    #[default]
    Unauthorized,
    /// Redirection vers la page de connexion (HTML hors de `/api`).
    LoginPage,
}

/// Pannes à provoquer sur les prochains appels d'API (`/api/…`).
#[derive(Debug, Clone, Default)]
pub struct Faults {
    /// Nombre de réponses 429.
    pub rate_limit: u32,
    /// En-tête `Retry-After` des 429.
    pub retry_after: Option<String>,
    /// Nombre d'appels répondus « session expirée » (la session est
    /// invalidée : il faut se reconnecter).
    pub expire_session: u32,
    pub expiry: Expiry,
    /// Nombre de pages HTML d'erreur (statut 500) servies sous `/api`.
    pub html_errors: u32,
    /// Nombre de connexions fermées sans réponse.
    pub drop_connections: u32,
    /// `GET /` redirige vers la page de connexion sur l'hôte `localhost`.
    pub foreign_redirect: bool,
    /// `success.href` désigne l'hôte `localhost`.
    pub foreign_success_href: bool,
    /// Nombre de réponses 503 (page HTML) à `challenge/answer` : panne
    /// passagère du service de connexion, pas un refus d'identifiants.
    pub login_unavailable: u32,
}

/// Ce que le serveur a vu.
#[derive(Debug, Clone, Default)]
pub struct Counters {
    /// Pages de connexion servies (avec `stateToken`).
    pub start_pages: u32,
    pub identify: u32,
    pub challenge: u32,
    /// Sessions ouvertes (`_codexch` réussi).
    pub logins: u32,
    pub pce_list: u32,
    /// `(dateDebut, dateFin)` de chaque appel de consommation, pannes comprises.
    pub consumption: Vec<(String, String)>,
    /// Anomalies du client (cookie, en-tête ou jeton manquant…).
    pub problems: Vec<String>,
    /// Requêtes reçues avec un en-tête `Host` autre que `127.0.0.1:…`.
    pub foreign_hosts: u32,
}

struct State {
    base: String,
    login: String,
    pces: Vec<String>,
    records: Vec<Value>,
    faults: Faults,
    counters: Counters,
    identified: bool,
    pending_code: Option<String>,
    session: Option<String>,
}

#[derive(Clone, Copy)]
enum Role {
    Base,
    Login,
}

pub struct FakeGrdf {
    base: String,
    login: String,
    addresses: Vec<SocketAddr>,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl FakeGrdf {
    /// Serveur dont le compte porte le seul PCE [`PCE`].
    pub fn start() -> FakeGrdf {
        FakeGrdf::start_with_pces(&[PCE])
    }

    pub fn start_with_pces(pces: &[&str]) -> FakeGrdf {
        let base_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let login_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_addr = base_listener.local_addr().unwrap();
        let login_addr = login_listener.local_addr().unwrap();
        let base = format!("http://{base_addr}");
        let login = format!("http://{login_addr}");
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let records = fixture[PCE]["releves"].as_array().unwrap().clone();
        let state = Arc::new(Mutex::new(State {
            base: base.clone(),
            login: login.clone(),
            pces: pces.iter().map(|p| (*p).to_owned()).collect(),
            records,
            faults: Faults::default(),
            counters: Counters::default(),
            identified: false,
            pending_code: None,
            session: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let threads = [(base_listener, Role::Base), (login_listener, Role::Login)]
            .into_iter()
            .map(|(listener, role)| {
                let state = Arc::clone(&state);
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || serve(listener, role, &state, &stop))
            })
            .collect();
        FakeGrdf {
            base,
            login,
            addresses: vec![base_addr, login_addr],
            state,
            stop,
            threads,
        }
    }

    /// Adresse de « monespace », ex. `http://127.0.0.1:40123`.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Adresse de « connexion ».
    pub fn login_url(&self) -> &str {
        &self.login
    }

    /// Remplace les relevés servis (voir [`record`]).
    pub fn set_records(&self, records: Vec<Value>) {
        self.state.lock().unwrap().records = records;
    }

    /// Publie un relevé : remplace celui du même jour gazier, ou l'ajoute.
    pub fn publish(&self, record: Value) {
        let mut state = self.state.lock().unwrap();
        let day = record["journeeGaziere"].clone();
        state.records.retain(|r| r["journeeGaziere"] != day);
        state.records.push(record);
    }

    pub fn set_faults(&self, faults: Faults) {
        self.state.lock().unwrap().faults = faults;
    }

    pub fn counters(&self) -> Counters {
        self.state.lock().unwrap().counters.clone()
    }
}

impl Drop for FakeGrdf {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for address in &self.addresses {
            // Débloque `accept`.
            let _ = TcpStream::connect(address);
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn serve(listener: TcpListener, role: Role, state: &Mutex<State>, stop: &AtomicBool) {
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(mut stream) = stream else { continue };
        let Some(request) = read_request(&stream) else {
            continue;
        };
        let reply = {
            let mut state = state.lock().unwrap();
            if !request
                .header("host")
                .is_some_and(|h| h.starts_with("127.0.0.1:"))
            {
                state.counters.foreign_hosts += 1;
            }
            match role {
                Role::Base => base_route(&mut state, &request),
                Role::Login => login_route(&mut state, &request),
            }
        };
        if let Some(reply) = reply {
            let _ = stream.write_all(reply.to_bytes().as_slice());
        }
        // Fermeture : une connexion par requête (« Connection: close »).
    }
}

// --- Requêtes et réponses HTTP minimales ---

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: Vec<(String, String)>,
    body: String,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn cookie(&self, name: &str) -> Option<String> {
        self.header("cookie")?.split(';').find_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            (k == name).then(|| v.to_owned())
        })
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
        }
    }
    let length: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_owned(), parse_query(q)),
        None => (target, HashMap::new()),
    };
    Some(Request {
        method,
        path,
        query,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            Some((percent_decode(k)?, percent_decode(v)?))
        })
        .collect()
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn new(status: u16, content_type: &str, body: impl Into<String>) -> Reply {
        Reply {
            status,
            headers: vec![("Content-Type".into(), content_type.into())],
            body: body.into(),
        }
    }

    fn json(status: u16, body: &Value) -> Reply {
        Reply::new(status, "application/json", body.to_string())
    }

    fn html(status: u16, body: &str) -> Reply {
        Reply::new(status, "text/html; charset=UTF-8", body)
    }

    fn redirect(location: &str) -> Reply {
        Reply {
            status: 302,
            headers: vec![("Location".into(), location.into())],
            body: String::new(),
        }
    }

    fn with(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn to_bytes(&self) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            302 => "Found",
            400 => "Bad Request",
            401 => "Unauthorized",
            404 => "Not Found",
            429 => "Too Many Requests",
            503 => "Service Unavailable",
            _ => "Error",
        };
        let mut out = format!("HTTP/1.1 {} {reason}\r\n", self.status);
        for (k, v) in &self.headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        out.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            self.body.len()
        ));
        out.push_str(&self.body);
        out.into_bytes()
    }
}

// --- « monespace » ---

fn session_valid(state: &State, request: &Request) -> bool {
    state.session.is_some() && request.cookie("auth_token") == state.session
}

fn base_route(state: &mut State, request: &Request) -> Option<Reply> {
    if request.header("user-agent") != Some(BROWSER_USER_AGENT) {
        state
            .counters
            .problems
            .push(format!("User-Agent inattendu sur {}", request.path));
    }
    if request.path.starts_with("/api/") {
        return api_route(state, request);
    }
    Some(match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") if session_valid(state, request) => {
            Reply::html(200, "<html>Mon espace</html>")
        }
        ("GET", "/") => Reply::redirect(&format!(
            "{}/oauth2/aus5y2ta2uEHjCWIR417/v1/authorize?client_id=fake&redirect_uri={}%2F_codexch&state=%2F",
            foreign_if(state.faults.foreign_redirect, &state.login),
            state.base
        )),
        ("GET", "/_codexch") => {
            let code = request.query.get("code");
            if code.is_some() && code == state.pending_code.as_ref() {
                state.pending_code = None;
                state.counters.logins += 1;
                let session = format!("{SESSION_PREFIX}{}", state.counters.logins);
                state.session = Some(session.clone());
                Reply::redirect("/").with(
                    "Set-Cookie",
                    &format!("auth_token={session}; Path=/; HttpOnly; SameSite=Lax"),
                )
            } else {
                state.counters.problems.push("code OAuth invalide".into());
                Reply::html(400, "<html>code invalide</html>")
            }
        }
        _ => Reply::html(404, "<html>absent</html>"),
    })
}

fn expired(state: &State) -> Reply {
    match state.faults.expiry {
        Expiry::Unauthorized => Reply::json(401, &json!({"code": 401, "message": "Unauthorized"})),
        Expiry::LoginPage => Reply::redirect(&format!("{}/login?fromURI=%2Fapi", state.login)),
    }
}

fn api_route(state: &mut State, request: &Request) -> Option<Reply> {
    let consumption = request.path == "/api/e-conso/pce/consommation/informatives";
    if consumption {
        let get = |k: &str| request.query.get(k).cloned().unwrap_or_default();
        state
            .counters
            .consumption
            .push((get("dateDebut"), get("dateFin")));
    }
    if state.faults.drop_connections > 0 {
        state.faults.drop_connections -= 1;
        return None;
    }
    if !session_valid(state, request) {
        return Some(expired(state));
    }
    if state.faults.expire_session > 0 {
        state.faults.expire_session -= 1;
        state.session = None;
        return Some(expired(state));
    }
    if state.faults.rate_limit > 0 {
        state.faults.rate_limit -= 1;
        let reply = Reply::html(429, "<html>Too Many Requests</html>");
        return Some(match &state.faults.retry_after {
            Some(v) => reply.with("Retry-After", v),
            None => reply,
        });
    }
    if state.faults.html_errors > 0 {
        state.faults.html_errors -= 1;
        return Some(Reply::html(500, "<html>Erreur technique</html>"));
    }
    if request.path == "/api/e-conso/pce" {
        state.counters.pce_list += 1;
        let list: Vec<Value> = state
            .pces
            .iter()
            .map(|p| json!({"idObject": p, "alias": "Maison", "etat": "Actif"}))
            .collect();
        return Some(Reply::json(200, &Value::Array(list)));
    }
    if consumption {
        let (Some(from), Some(to), Some(pce)) = (
            request.query.get("dateDebut"),
            request.query.get("dateFin"),
            request.query.get("pceList[]"),
        ) else {
            state
                .counters
                .problems
                .push("paramètres de consommation manquants".into());
            return Some(Reply::json(
                400,
                &json!({"message": "paramètres manquants"}),
            ));
        };
        let selected: Vec<Value> = state
            .records
            .iter()
            .filter(|r| {
                let day = r["journeeGaziere"].as_str().unwrap_or("");
                pce == PCE && from.as_str() <= day && day <= to.as_str()
            })
            .cloned()
            .collect();
        if selected.is_empty() {
            return Some(Reply::json(200, &json!([])));
        }
        let mut by_pce = serde_json::Map::new();
        by_pce.insert(
            PCE.to_owned(),
            json!({"idPce": PCE, "frequence": null, "releves": selected}),
        );
        return Some(Reply::json(200, &Value::Object(by_pce)));
    }
    Some(Reply::json(404, &json!({"message": "inconnu"})))
}

// --- « connexion » (Okta) ---

fn login_route(state: &mut State, request: &Request) -> Option<Reply> {
    Some(match (request.method.as_str(), request.path.as_str()) {
        ("GET", p) if p.starts_with("/oauth2/") => {
            state.counters.start_pages += 1;
            state.identified = false;
            let page = format!(
                "<html><script>var oktaData = {{\"redirectUri\":\"x\",\"stateToken\":\"{STATE_TOKEN_IN_PAGE}\",\"x\":1}};</script></html>"
            );
            Reply::html(200, &page).with(
                "Set-Cookie",
                &format!("DT={DEVICE_COOKIE_VALUE}; Path=/; HttpOnly"),
            )
        }
        ("POST", "/idp/idx/identify") => identify(state, request),
        ("POST", "/idp/idx/challenge/answer") => challenge(state, request),
        ("GET", "/login/token/redirect") => {
            if request.query.get("stateToken").map(String::as_str) != Some(STATE_HANDLE) {
                state
                    .counters
                    .problems
                    .push("success.href inattendu".into());
                return Some(Reply::html(400, "<html>jeton invalide</html>"));
            }
            let code = format!("{CODE_PREFIX}{}", state.counters.challenge);
            state.pending_code = Some(code.clone());
            Reply::redirect(&format!("{}/_codexch?code={code}&state=%2F", state.base))
        }
        ("GET", "/login") => Reply::html(200, "<html>Connexion à Mon Espace GRDF</html>"),
        _ => Reply::html(404, "<html>absent</html>"),
    })
}

fn okta_checks(state: &mut State, request: &Request, step: &str) {
    let mut problems = Vec::new();
    if request.header("accept") != Some("application/json; okta-version=1.0.0") {
        problems.push(format!("{step} : en-tête Accept d'Okta absent"));
    }
    if request.header("content-type") != Some("application/json") {
        problems.push(format!("{step} : Content-Type JSON absent"));
    }
    if request.cookie("DT").as_deref() != Some(DEVICE_COOKIE_VALUE) {
        problems.push(format!("{step} : cookie DT non renvoyé"));
    }
    if request.cookie("ln").as_deref() != Some(EMAIL) {
        problems.push(format!("{step} : cookie ln absent ou faux"));
    }
    state.counters.problems.extend(problems);
}

fn identify(state: &mut State, request: &Request) -> Reply {
    state.counters.identify += 1;
    okta_checks(state, request, "identify");
    let body = request.json();
    if body["stateHandle"] != STATE_TOKEN {
        state
            .counters
            .problems
            .push("identify : stateToken faux".into());
        return Reply::json(
            400,
            &json!({"version": "1.0.0", "errorSummary": "The session has expired."}),
        );
    }
    let identifier = body["identifier"].as_str().unwrap_or("");
    state.identified = identifier == EMAIL;
    // Vraie réponse d'Okta (PyGazpar), adaptée à ce serveur.
    let answer = OKTA_IDENTIFY
        .replace(OKTA_FIXTURE_HANDLE, STATE_HANDLE)
        .replace("https://connexion.grdf.fr", &state.login)
        .replace("user@example.com", identifier);
    Reply::new(200, "application/ion+json; okta-version=1.0.0", answer)
}

fn challenge(state: &mut State, request: &Request) -> Reply {
    state.counters.challenge += 1;
    okta_checks(state, request, "challenge");
    if state.faults.login_unavailable > 0 {
        state.faults.login_unavailable -= 1;
        return Reply::html(503, "<html>Service indisponible</html>");
    }
    let body = request.json();
    if body["stateHandle"] != STATE_HANDLE {
        state
            .counters
            .problems
            .push("challenge : stateHandle faux".into());
        return Reply::json(
            400,
            &json!({"version": "1.0.0", "errorSummary": "The session has expired."}),
        );
    }
    if !state.identified || body["credentials"]["passcode"] != PASSWORD {
        return Reply::json(
            401,
            &json!({
                "version": "1.0.0",
                "stateHandle": STATE_HANDLE,
                "messages": {"type": "array", "value": [{
                    "message": "Authentication failed",
                    "i18n": {"key": "errors.E0000004"},
                    "class": "ERROR"
                }]},
                "user": {"type": "object", "value": {"identifier": EMAIL}}
            }),
        );
    }
    Reply::json(
        200,
        &json!({
            "version": "1.0.0",
            "success": {
                "name": "success-redirect",
                "href": format!(
                    "{}/login/token/redirect?stateToken={STATE_HANDLE}",
                    foreign_if(state.faults.foreign_success_href, &state.login)
                )
            }
        }),
    )
}

/// Relevé informatif d'un jour gazier (`AAAA-MM-JJ`), au format de GRDF ;
/// `None` = valeur pas encore publiée (`null`).
pub fn record(day: &str, energy_kwh: Option<f64>, volume_m3: Option<f64>) -> Value {
    json!({
        "coeffConversion": 11.17,
        "dateDebutReleve": format!("{day}T06:00:00+00:00"),
        "dateFinReleve": null,
        "energieConsomme": energy_kwh,
        "frequenceReleve": null,
        "indexDebut": null,
        "indexFin": null,
        "journeeGaziere": day,
        "natureReleve": "Informative Journalier",
        "pcs": null,
        "pta": null,
        "qualificationReleve": if energy_kwh.is_some() { "Mesuré" } else { "Absence de Données" },
        "status": null,
        "temperature": null,
        "volumeBrutConsomme": volume_m3,
        "volumeConverti": null
    })
}

/// `http://127.0.0.1:port` devenu `http://localhost:port` si `foreign`.
fn foreign_if(foreign: bool, address: &str) -> String {
    if foreign {
        address.replacen("127.0.0.1", "localhost", 1)
    } else {
        address.to_owned()
    }
}

//! Appels à l'API e-conso de GRDF (PLAN.md §2.2) : [`GrdfClient`]
//! implémente [`ConsumptionSource`].
//!
//! - connexion paresseuse au premier appel, puis vérification que le PCE
//!   configuré figure dans `GET /api/e-conso/pce` (`idObject`) ;
//! - relevés par `GET /api/e-conso/pce/consommation/informatives`, plage
//!   découpée en fenêtres d'au plus 365 jours (au-delà, GRDF renvoie un
//!   historique incomplet : ticket gazpar2haws #131), fenêtres
//!   consécutives se chevauchant d'un jour (on ignore si `dateFin` est
//!   inclus), puis tri, dédoublonnage et filtrage sur la plage demandée ;
//! - reprises comme `api_client.py` : 401, ou HTML hors de `<base>/api`
//!   = session expirée → **une seule** reconnexion ; 429 → `Retry-After`
//!   plafonné à 60 s ; HTML ou erreur réseau → délais de 3, 6, 12 puis 15 s,
//!   [`MAX_ATTEMPTS`] tentatives au plus ; autre statut → erreur ;
//! - attente par un « dormeur » injecté : les tests ne dorment pas.
//!
//! Les décisions (fenêtres, délais, reprise) sont des fonctions pures.
//! L'analyse des relevés est déléguée à [`crate::readings`].
//!
//! Dépendances : `grdf::http`, `grdf::auth`, `url`, `readings`,
//! `domain`, `ports`, `secret`, `error`, `log`, `jiff`, `serde_json`.

use std::time::Duration;

use jiff::ToSpan;
use jiff::civil::Date;
use serde_json::Value;

use super::auth::{self, Credentials};
use super::http::{AllowedHosts, HttpClient, Response};
use crate::domain::DailyReading;
use crate::error::Error;
use crate::ports::ConsumptionSource;
use crate::readings;
use crate::url::Url;

/// Tentatives au plus pour un appel (PyGazpar : 10).
pub const MAX_ATTEMPTS: u32 = 4;
/// Premier délai de reprise, doublé à chaque tentative.
const RETRY_DELAY: Duration = Duration::from_secs(3);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(15);
/// Plafond d'un `Retry-After`.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Longueur maximale d'une fenêtre de relevés, en jours (bornes comprises).
pub const MAX_WINDOW_DAYS: i32 = 365;

const PCE_LIST_PATH: &str = "/e-conso/pce";
const INFORMATIVE_PATH: &str = "/e-conso/pce/consommation/informatives";

/// Fonction d'attente, `std::thread::sleep` en production.
pub type Sleeper = Box<dyn FnMut(Duration)>;

/// Appel à l'API, tel que nommé dans les erreurs et le journal :
/// « GRDF, liste des PCE », « GRDF, relevés du 2026-10-18 au 2026-11-16 ».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    /// `GET /api/e-conso/pce`.
    PceList,
    /// `GET /api/e-conso/pce/consommation/informatives` sur une fenêtre,
    /// numérotée `index`/`count` quand la plage en compte plusieurs.
    Readings {
        from: Date,
        to: Date,
        index: usize,
        count: usize,
    },
}

impl Call {
    fn path(self) -> &'static str {
        match self {
            Call::PceList => PCE_LIST_PATH,
            Call::Readings { .. } => INFORMATIVE_PATH,
        }
    }
}

impl std::fmt::Display for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Call::PceList => f.write_str("GRDF, liste des PCE"),
            Call::Readings {
                from,
                to,
                index,
                count,
            } => {
                write!(f, "GRDF, relevés du {from} au {to}")?;
                if count > 1 {
                    write!(f, " (fenêtre {index}/{count})")?;
                }
                Ok(())
            }
        }
    }
}

/// Paramètres du client, reçus de la configuration.
#[derive(Debug)]
pub struct GrdfSettings {
    /// Ex. `https://monespace.grdf.fr`.
    pub base_url: String,
    /// Ex. `https://connexion.grdf.fr`.
    pub login_url: String,
    pub credentials: Credentials,
    /// Point de comptage (14 chiffres).
    pub pce: String,
}

/// Client Mon Espace GRDF.
pub struct GrdfClient {
    base_url: String,
    login_url: String,
    /// Préfixe des réponses de l'API : une page HTML ailleurs signale une
    /// session expirée.
    api_prefix: String,
    /// Hôtes joignables : ceux de `base_url` et `login_url` (manuel §9).
    allowed: AllowedHosts,
    credentials: Credentials,
    pce: String,
    sleep: Sleeper,
    /// Session ouverte, ou `None` avant la connexion.
    session: Option<HttpClient>,
    pce_checked: bool,
}

impl GrdfClient {
    pub fn new(settings: GrdfSettings, sleep: Sleeper) -> Result<GrdfClient, Error> {
        let base_url = settings.base_url.trim_end_matches('/').to_owned();
        let login_url = settings.login_url.trim_end_matches('/').to_owned();
        let base = Url::http(&base_url)?;
        let allowed = AllowedHosts::from_urls(&[&base_url, &login_url])?;
        let api_prefix = format!("{}/api", base.redacted().trim_end_matches('/'));
        Ok(GrdfClient {
            base_url,
            login_url,
            api_prefix,
            allowed,
            credentials: settings.credentials,
            pce: settings.pce,
            sleep,
            session: None,
            pce_checked: false,
        })
    }

    /// Ouvre une nouvelle session (cookies neufs), sans reprise.
    fn login(&mut self) -> Result<(), Error> {
        self.session = None;
        let mut http = HttpClient::new(self.allowed.clone());
        auth::login(
            &mut http,
            &self.base_url,
            &self.login_url,
            &self.credentials,
        )?;
        self.session = Some(http);
        Ok(())
    }

    /// Connexion paresseuse et vérification unique du PCE.
    fn ensure_ready(&mut self) -> Result<(), Error> {
        if self.session.is_none() {
            self.login()?;
        }
        if !self.pce_checked {
            let call = Call::PceList;
            let response = self.api_get(call, &[])?;
            let pces = parse_pce_list(&response).map_err(|e| e.context(call))?;
            debug!("{call} : {} PCE sur le compte", pces.len());
            if !pces.contains(&self.pce) {
                let known = if pces.is_empty() {
                    "aucun".to_owned()
                } else {
                    pces.join(", ")
                };
                return Err(Error::Grdf(format!(
                    "le PCE {} n'est pas rattaché à ce compte GRDF (PCE du compte : {known}) ; \
                     vérifiez GRDF_PCE",
                    self.pce
                ))
                .context(call));
            }
            self.pce_checked = true;
        }
        Ok(())
    }

    /// `GET <base>/api<chemin de call>` avec reprises et reconnexion
    /// unique. Les erreurs de la requête sont replacées dans `call` ; celles
    /// d'une reconnexion nomment déjà leur étape de connexion.
    fn api_get(&mut self, call: Call, query: &[(&str, &str)]) -> Result<Response, Error> {
        let url = format!("{}/api{}", self.base_url, call.path());
        let mut attempt = 1;
        let mut logged_in_again = false;
        loop {
            let http = match self.session.as_mut() {
                Some(http) => http,
                None => {
                    self.login()?;
                    continue;
                }
            };
            let outcome = http.get(&url, query, &[("Accept", "application/json")]);
            match decide(&outcome, &self.api_prefix, attempt, logged_in_again) {
                Decision::Accept => {
                    return outcome;
                }
                Decision::Relogin { reason } => {
                    warn!("{call} : {reason} ; reconnexion à GRDF (une seule par appel)");
                    logged_in_again = true;
                    self.login()?;
                }
                Decision::Retry { delay, reason } => {
                    warn!("{}", retry_line(call, attempt, &reason, delay));
                    (self.sleep)(delay);
                    attempt += 1;
                }
                Decision::Fail(e) => return Err(e.context(call)),
            }
        }
    }

    /// Relevés d'une fenêtre (une requête).
    fn informative_window(&mut self, call: Call) -> Result<Vec<DailyReading>, Error> {
        let Call::Readings { from, to, .. } = call else {
            return Ok(Vec::new());
        };
        let (from_text, to_text, pce) = (from.to_string(), to.to_string(), self.pce.clone());
        debug!("{call} : demande (dateDebut {from_text}, dateFin {to_text})");
        let response = self.api_get(
            call,
            &[
                ("dateDebut", &from_text),
                ("dateFin", &to_text),
                ("pceList[]", &pce),
            ],
        )?;
        let parsed =
            readings::parse_informative(&response.body, &pce).map_err(|e| e.context(call))?;
        for warning in &parsed.warnings {
            warn!("{call} : {warning}");
        }
        debug!("{call} : {}", received(&parsed.readings, to));
        Ok(parsed.readings)
    }
}

impl ConsumptionSource for GrdfClient {
    fn daily_readings(&mut self, from: Date, to: Date) -> Result<Vec<DailyReading>, Error> {
        let windows = windows(from, to);
        if windows.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_ready()?;
        let mut all = Vec::new();
        let count = windows.len();
        for (i, (start, end)) in windows.into_iter().enumerate() {
            all.extend(self.informative_window(Call::Readings {
                from: start,
                to: end,
                index: i + 1,
                count,
            })?);
        }
        Ok(merge(all, from, to))
    }
}

/// Fenêtres `(début, fin)` d'au plus [`MAX_WINDOW_DAYS`] jours bornes
/// comprises, couvrant `from..=to` ; chaque fenêtre commence le dernier jour
/// de la précédente. Vide si `from > to`.
fn windows(from: Date, to: Date) -> Vec<(Date, Date)> {
    let mut out = Vec::new();
    let mut start = from;
    while start <= to {
        let end = start
            .checked_add((MAX_WINDOW_DAYS - 1).days())
            .map_or(to, |d| d.min(to));
        out.push((start, end));
        if end >= to {
            break;
        }
        start = end;
    }
    out
}

/// Ce qu'une fenêtre a rapporté, pour le journal de diagnostic : nombre de
/// jours, premier et dernier, et si le jour `dateFin` (`to`) figure dans la
/// réponse (on ignore encore si GRDF l'inclut).
fn received(readings: &[DailyReading], to: Date) -> String {
    let first = readings.iter().map(|r| r.day).min();
    let last = readings.iter().map(|r| r.day).max();
    let (Some(first), Some(last)) = (first, last) else {
        return "aucun jour reçu".to_owned();
    };
    let empty = readings
        .iter()
        .filter(|r| r.energy_kwh.is_none() && r.volume_m3.is_none())
        .count();
    let to_day = if readings.iter().any(|r| r.day == to) {
        "dateFin incluse dans la réponse"
    } else {
        "aucun relevé pour dateFin"
    };
    let count = match readings.len() {
        1 => "1 jour reçu".to_owned(),
        n => format!("{n} jours reçus"),
    };
    format!("{count}, du {first} au {last}, dont {empty} sans valeur ; {to_day}")
}

/// Trie, garde un relevé par jour (celui qui a le plus de valeurs, à défaut
/// le dernier reçu) et ne garde que `from..=to`.
fn merge(mut readings: Vec<DailyReading>, from: Date, to: Date) -> Vec<DailyReading> {
    readings.retain(|r| (from..=to).contains(&r.day));
    readings.sort_by_key(|r| r.day);
    let filled =
        |r: &DailyReading| usize::from(r.energy_kwh.is_some()) + usize::from(r.volume_m3.is_some());
    let mut out: Vec<DailyReading> = Vec::with_capacity(readings.len());
    for reading in readings {
        match out.last_mut() {
            Some(last) if last.day == reading.day => {
                if filled(&reading) >= filled(last) {
                    *last = reading;
                }
            }
            _ => out.push(reading),
        }
    }
    out
}

/// Délai avant la tentative suivante : 3, 6, 12 puis 15 s au plus.
fn backoff(attempt: u32) -> Duration {
    let factor = 2_u32.saturating_pow(attempt.saturating_sub(1));
    RETRY_DELAY.saturating_mul(factor).min(MAX_RETRY_DELAY)
}

/// Délai demandé par `Retry-After` en secondes (pas les dates HTTP),
/// plafonné à 60 s ; `None` s'il est absent ou illisible.
fn retry_after(header: Option<&str>) -> Option<Duration> {
    let seconds: f64 = header?.trim().parse().ok()?;
    if !seconds.is_finite() {
        return None;
    }
    Some(Duration::from_secs_f64(
        seconds.clamp(0.0, MAX_RETRY_AFTER.as_secs_f64()),
    ))
}

/// Session refusée : 401, ou page HTML servie hors de l'API (redirection
/// vers la page de connexion).
fn is_session_expired(response: &Response, api_prefix: &str) -> bool {
    response.status == 401
        || (response.is_html() && !response.final_url.redacted().starts_with(api_prefix))
}

/// Suite à donner à une tentative.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Accept,
    /// Session expirée : reconnexion, avec le signe qui l'a montré.
    Relogin {
        reason: String,
    },
    Retry {
        delay: Duration,
        reason: String,
    },
    Fail(Error),
}

/// Choix après une tentative, dans l'ordre d'`api_client.py` : erreur
/// réseau, session expirée, 429, HTML, statut autre que 200.
///
/// Les causes nomment l'hôte et le port qui ont répondu, jamais l'adresse ;
/// l'appelant ajoute l'opération ([`Call`]). Un abandon après
/// [`MAX_ATTEMPTS`] tentatives garde la cause de la dernière et le dit.
fn decide(
    outcome: &Result<Response, Error>,
    api_prefix: &str,
    attempt: u32,
    logged_in_again: bool,
) -> Decision {
    let retry = |delay: Duration, reason: String, last_error: Error| {
        if attempt >= MAX_ATTEMPTS {
            Decision::Fail(
                last_error.map_message(|m| format!("{m} ; abandon après {attempt} tentatives")),
            )
        } else {
            Decision::Retry { delay, reason }
        }
    };
    let response = match outcome {
        // La cause nomme déjà l'hôte et le port (`grdf::http`).
        Err(e) if e.is_transient() => {
            return retry(backoff(attempt), e.to_string(), e.clone());
        }
        Err(other) => return Decision::Fail(other.clone()),
        Ok(response) => response,
    };
    let target = response.final_url.host_port();
    if is_session_expired(response, api_prefix) {
        return if logged_in_again {
            Decision::Fail(Error::GrdfLogin(format!(
                "session expirée de nouveau juste après la reconnexion ({}, {target})",
                expiry_sign(response)
            )))
        } else {
            Decision::Relogin {
                reason: format!("session expirée ({}, {target})", expiry_sign(response)),
            }
        };
    }
    if response.status == 429 {
        let delay = retry_after(response.retry_after.as_deref()).unwrap_or(backoff(attempt));
        // Valeur brute de l'en-tête, raccourcie : un nombre ou une date.
        let announced = response.retry_after.as_deref().map_or(String::new(), |v| {
            let v: String = v.trim().chars().take(40).collect();
            format!(", Retry-After « {v} »")
        });
        let reason = format!("débit limité par GRDF (statut HTTP 429{announced}, {target})");
        return retry(delay, reason.clone(), Error::Grdf(reason));
    }
    if response.is_html() {
        let reason = format!(
            "page HTML au lieu de JSON (statut HTTP {}, {target})",
            response.status
        );
        return retry(backoff(attempt), reason.clone(), Error::Grdf(reason));
    }
    if response.status != 200 {
        return Decision::Fail(Error::Grdf(format!(
            "statut HTTP {} ({target})",
            response.status
        )));
    }
    Decision::Accept
}

/// Ligne de journal d'une reprise : l'appel, la tentative échouée sur
/// [`MAX_ATTEMPTS`], le motif et le délai avant la suivante.
fn retry_line(call: Call, attempt: u32, reason: &str, delay: Duration) -> String {
    format!(
        "{call} : tentative {attempt}/{MAX_ATTEMPTS} échouée, {reason} ; nouvel essai dans {}",
        seconds(delay)
    )
}

/// Délai lisible : « 3 s », « 1,5 s ».
fn seconds(delay: Duration) -> String {
    if delay.subsec_millis() == 0 {
        format!("{} s", delay.as_secs())
    } else {
        format!("{:.1} s", delay.as_secs_f64()).replace('.', ",")
    }
}

/// Signe d'une session expirée, pour un message.
fn expiry_sign(response: &Response) -> String {
    if response.status == 401 {
        "statut HTTP 401".to_owned()
    } else {
        format!("page HTML hors de l'API, statut HTTP {}", response.status)
    }
}

/// `idObject` de chaque PCE du compte.
fn parse_pce_list(response: &Response) -> Result<Vec<String>, Error> {
    let bad = |why: &str| {
        Error::Grdf(format!(
            "réponse inattendue, {why} (statut HTTP {}, {})",
            response.status,
            response.final_url.host_port()
        ))
    };
    let value: Value = serde_json::from_str(&response.body).map_err(|_| bad("JSON illisible"))?;
    let items = value.as_array().ok_or_else(|| bad("liste attendue"))?;
    Ok(items
        .iter()
        .filter_map(|item| item.get("idObject").and_then(Value::as_str))
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Quantity;
    use jiff::civil::date;

    const PREFIX: &str = "https://monespace.grdf.fr/api";

    fn response(status: u16, content_type: &str, url: &str) -> Response {
        Response {
            status,
            final_url: Url::http(url).unwrap(),
            content_type: (!content_type.is_empty()).then(|| content_type.to_owned()),
            body: String::new(),
            retry_after: None,
        }
    }

    fn ok() -> Result<Response, Error> {
        Ok(response(
            200,
            "application/json",
            "https://monespace.grdf.fr/api/e-conso/pce",
        ))
    }

    fn decide1(outcome: Result<Response, Error>) -> Decision {
        decide(&outcome, PREFIX, 1, false)
    }

    // --- Délais ---

    #[test]
    fn backoff_grows_then_caps() {
        let secs: Vec<u64> = (1..=7).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(secs, [3, 6, 12, 15, 15, 15, 15]);
        assert_eq!(backoff(0), Duration::from_secs(3));
        assert_eq!(backoff(u32::MAX), MAX_RETRY_DELAY);
    }

    #[test]
    fn retry_after_values() {
        // Cas de PyGazpar : 7 → 7, 0 → 0, 600 → 60, soon → défaut.
        assert_eq!(retry_after(Some("7")), Some(Duration::from_secs(7)));
        assert_eq!(retry_after(Some("0")), Some(Duration::ZERO));
        assert_eq!(retry_after(Some("600")), Some(MAX_RETRY_AFTER));
        assert_eq!(retry_after(Some("soon")), None);
        assert_eq!(
            retry_after(Some(" 1.5 ")),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(retry_after(Some("-3")), Some(Duration::ZERO));
        assert_eq!(retry_after(Some("inf")), None);
        assert_eq!(retry_after(Some("NaN")), None);
        assert_eq!(retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")), None);
        assert_eq!(retry_after(None), None);
    }

    // --- Décisions ---

    #[test]
    fn success_is_accepted() {
        assert_eq!(decide1(ok()), Decision::Accept);
        // Sans Content-Type : pas une erreur (PyGazpar, test_a_missing_content_type).
        assert_eq!(
            decide1(Ok(response(200, "", "https://monespace.grdf.fr/api/x"))),
            Decision::Accept
        );
    }

    #[test]
    fn expired_session_relogs_once() {
        for expired in [
            response(
                401,
                "application/json",
                "https://monespace.grdf.fr/api/e-conso/pce",
            ),
            response(
                200,
                "text/html; charset=UTF-8",
                "https://connexion.grdf.fr/login",
            ),
            response(200, "text/html", "https://monespace.grdf.fr/"),
        ] {
            let outcome = Ok(expired);
            assert!(matches!(decide(&outcome, PREFIX, 1, false),
                    Decision::Relogin { reason } if reason.starts_with("session expirée (")),);
            let again = decide(&outcome, PREFIX, 1, true);
            assert!(
                matches!(&again, Decision::Fail(Error::GrdfLogin(m))
                    if m.starts_with("session expirée de nouveau juste après la reconnexion (")),
                "{again:?}"
            );
        }
    }

    #[test]
    fn rate_limit_waits_retry_after_or_backoff() {
        let mut throttled = response(429, "text/html", PREFIX);
        throttled.retry_after = Some("7".into());
        let outcome = Ok(throttled);
        assert!(matches!(
            decide(&outcome, PREFIX, 1, false),
            Decision::Retry { delay, .. } if delay == Duration::from_secs(7)
        ));
        let outcome = Ok(response(429, "text/html", PREFIX));
        assert!(matches!(
            decide(&outcome, PREFIX, 2, false),
            Decision::Retry { delay, .. } if delay == Duration::from_secs(6)
        ));
        let last = decide(&outcome, PREFIX, MAX_ATTEMPTS, false);
        assert_eq!(
            last,
            Decision::Fail(Error::Grdf(
                "débit limité par GRDF (statut HTTP 429, monespace.grdf.fr:443) ; \
                 abandon après 4 tentatives"
                    .into()
            ))
        );
        let mut throttled = response(429, "text/html", PREFIX);
        throttled.retry_after = Some(" 600 ".into());
        assert_eq!(
            decide(&Ok(throttled), PREFIX, 1, false),
            Decision::Retry {
                delay: MAX_RETRY_AFTER,
                reason: "débit limité par GRDF (statut HTTP 429, Retry-After « 600 », \
                         monespace.grdf.fr:443)"
                    .into()
            }
        );
    }

    #[test]
    fn html_inside_api_is_a_server_error_retried() {
        let outcome = Ok(response(
            200,
            "text/html",
            "https://monespace.grdf.fr/api/e-conso/pce",
        ));
        let delays: Vec<Decision> = (1..=MAX_ATTEMPTS)
            .map(|a| decide(&outcome, PREFIX, a, false))
            .collect();
        assert_eq!(
            delays[..3]
                .iter()
                .map(|d| match d {
                    Decision::Retry { delay, .. } => delay.as_secs(),
                    _ => 0,
                })
                .collect::<Vec<_>>(),
            [3, 6, 12]
        );
        assert_eq!(
            delays[3],
            Decision::Fail(Error::Grdf(
                "page HTML au lieu de JSON (statut HTTP 200, monespace.grdf.fr:443) ; \
                 abandon après 4 tentatives"
                    .into()
            ))
        );
    }

    #[test]
    fn network_errors_are_retried_then_raised() {
        let error = Error::Http("pas de réponse en 60 s (monespace.grdf.fr:443)".into());
        let outcome = Err(error);
        assert_eq!(
            decide(&outcome, PREFIX, 1, false),
            Decision::Retry {
                delay: Duration::from_secs(3),
                reason: "pas de réponse en 60 s (monespace.grdf.fr:443)".into()
            }
        );
        // L'erreur finale est celle du transport, toujours passagère, qui
        // dit qu'on a abandonné.
        let last = decide(&outcome, PREFIX, MAX_ATTEMPTS, false);
        assert_eq!(
            last,
            Decision::Fail(Error::Http(
                "pas de réponse en 60 s (monespace.grdf.fr:443) ; abandon après 4 tentatives"
                    .into()
            ))
        );
    }

    #[test]
    fn retry_lines_say_attempt_reason_and_delay() {
        let call = Call::Readings {
            from: date(2026, 10, 18),
            to: date(2026, 11, 16),
            index: 1,
            count: 1,
        };
        assert_eq!(
            retry_line(
                call,
                1,
                "pas de réponse en 60 s (monespace.grdf.fr:443)",
                Duration::from_secs(3)
            ),
            "GRDF, relevés du 2026-10-18 au 2026-11-16 : tentative 1/4 échouée, pas de réponse \
             en 60 s (monespace.grdf.fr:443) ; nouvel essai dans 3 s"
        );
        assert_eq!(
            retry_line(
                Call::PceList,
                3,
                "débit limité par GRDF (statut HTTP 429, Retry-After « 1.5 », \
                 monespace.grdf.fr:443)",
                Duration::from_millis(1500)
            ),
            "GRDF, liste des PCE : tentative 3/4 échouée, débit limité par GRDF (statut HTTP \
             429, Retry-After « 1.5 », monespace.grdf.fr:443) ; nouvel essai dans 1,5 s"
        );
        let expired = Ok(response(401, "application/json", PREFIX));
        assert_eq!(
            decide(&expired, PREFIX, 1, false),
            Decision::Relogin {
                reason: "session expirée (statut HTTP 401, monespace.grdf.fr:443)".into()
            }
        );
        let login_page = Ok(response(
            200,
            "text/html",
            "https://connexion.grdf.fr/login",
        ));
        assert_eq!(
            decide(&login_page, PREFIX, 1, false),
            Decision::Relogin {
                reason: "session expirée (page HTML hors de l'API, statut HTTP 200, \
                         connexion.grdf.fr:443)"
                    .into()
            }
        );
    }

    #[test]
    fn received_days_are_summed_up_for_the_debug_log() {
        let r = |day: Date, e: Option<i64>| DailyReading {
            day,
            energy_kwh: e.map(Quantity::from_millis),
            volume_m3: None,
        };
        let to = date(2026, 11, 16);
        assert_eq!(received(&[], to), "aucun jour reçu");
        assert_eq!(
            received(
                &[
                    r(date(2026, 11, 3), Some(1)),
                    r(date(2026, 11, 16), None),
                    r(date(2026, 11, 4), Some(1)),
                ],
                to
            ),
            "3 jours reçus, du 2026-11-03 au 2026-11-16, dont 1 sans valeur ; dateFin incluse \
             dans la réponse"
        );
        assert_eq!(
            received(&[r(date(2026, 11, 14), Some(1))], to),
            "1 jour reçu, du 2026-11-14 au 2026-11-14, dont 0 sans valeur ; aucun relevé \
             pour dateFin"
        );
    }

    #[test]
    fn calls_are_named_for_messages() {
        assert_eq!(Call::PceList.to_string(), "GRDF, liste des PCE");
        let one = Call::Readings {
            from: date(2026, 10, 18),
            to: date(2026, 11, 16),
            index: 1,
            count: 1,
        };
        assert_eq!(one.to_string(), "GRDF, relevés du 2026-10-18 au 2026-11-16");
        let third = Call::Readings {
            from: date(2025, 11, 30),
            to: date(2026, 11, 29),
            index: 3,
            count: 4,
        };
        assert_eq!(
            third.to_string(),
            "GRDF, relevés du 2025-11-30 au 2026-11-29 (fenêtre 3/4)"
        );
        let e = Error::Grdf("statut HTTP 500 (monespace.grdf.fr:443)".into()).context(third);
        assert_eq!(
            e.to_string(),
            "GRDF, relevés du 2025-11-30 au 2026-11-29 (fenêtre 3/4) : statut HTTP 500 \
             (monespace.grdf.fr:443)"
        );
    }

    #[test]
    fn other_errors_fail_at_once() {
        let outcome = Err(Error::Protocol("trop de redirections".into()));
        assert!(matches!(
            decide1(outcome),
            Decision::Fail(Error::Protocol(_))
        ));
        for status in [400, 403, 404, 500, 503] {
            let d = decide1(Ok(response(status, "application/json", PREFIX)));
            assert_eq!(
                d,
                Decision::Fail(Error::Grdf(format!(
                    "statut HTTP {status} (monespace.grdf.fr:443)"
                )))
            );
        }
    }

    // --- Fenêtres ---

    #[test]
    fn windows_cover_range_with_one_day_overlap() {
        let (from, to) = (date(2019, 11, 30), date(2022, 11, 29));
        let w = windows(from, to);
        assert_eq!(w.len(), 4);
        assert_eq!(w[0].0, from);
        assert_eq!(w.last().unwrap().1, to);
        for (start, end) in &w {
            let days = start.until(*end).unwrap().get_days() + 1;
            assert!((1..=MAX_WINDOW_DAYS).contains(&days), "{start} {end}");
        }
        for pair in w.windows(2) {
            assert_eq!(pair[0].1, pair[1].0);
        }
    }

    #[test]
    fn windows_small_and_empty_ranges() {
        let d = date(2026, 1, 1);
        assert_eq!(windows(d, d), vec![(d, d)]);
        assert!(windows(d, date(2025, 12, 31)).is_empty());
        let year_end = date(2026, 12, 31);
        assert_eq!(windows(d, year_end), vec![(d, year_end)]);
        let w = windows(d, date(2027, 1, 1));
        assert_eq!(w, vec![(d, year_end), (year_end, date(2027, 1, 1))]);
    }

    #[test]
    fn merge_sorts_dedups_and_filters() {
        let r = |day: Date, e: Option<i64>| DailyReading {
            day,
            energy_kwh: e.map(Quantity::from_millis),
            volume_m3: None,
        };
        let merged = merge(
            vec![
                r(date(2026, 1, 3), Some(3)),
                r(date(2026, 1, 2), Some(2)),
                r(date(2025, 12, 31), Some(9)),
                r(date(2026, 1, 2), None),
                r(date(2026, 1, 3), Some(30)),
                r(date(2026, 1, 9), Some(9)),
            ],
            date(2026, 1, 1),
            date(2026, 1, 3),
        );
        assert_eq!(
            merged,
            vec![r(date(2026, 1, 2), Some(2)), r(date(2026, 1, 3), Some(30))]
        );
    }

    // --- Liste des PCE ---

    #[test]
    fn pce_list() {
        let body =
            r#"[{"idObject": "12345678901234", "alias": "Maison"}, {"x": 1}, {"idObject": "2"}]"#;
        let with_body = |body: &str| Response {
            body: body.to_owned(),
            ..response(
                200,
                "application/json",
                "https://monespace.grdf.fr/api/e-conso/pce",
            )
        };
        assert_eq!(
            parse_pce_list(&with_body(body)).unwrap(),
            ["12345678901234", "2"]
        );
        assert!(parse_pce_list(&with_body("[]")).unwrap().is_empty());
        for (bad, why) in [
            ("{}", "liste attendue"),
            ("<html>", "JSON illisible"),
            ("", "JSON illisible"),
        ] {
            assert_eq!(
                parse_pce_list(&with_body(bad)).unwrap_err(),
                Error::Grdf(format!(
                    "réponse inattendue, {why} (statut HTTP 200, monespace.grdf.fr:443)"
                ))
            );
        }
    }

    #[test]
    fn empty_range_needs_no_login() {
        let settings = GrdfSettings {
            base_url: "http://127.0.0.1:9/".into(),
            login_url: "http://127.0.0.1:9".into(),
            credentials: Credentials {
                username: crate::secret::Secret::new("a@b.fr"),
                password: crate::secret::Secret::new("x"),
            },
            pce: "12345678901234".into(),
        };
        let mut client = GrdfClient::new(settings, Box::new(|_| panic!("pas d'attente"))).unwrap();
        assert!(
            client
                .daily_readings(date(2026, 1, 2), date(2026, 1, 1))
                .unwrap()
                .is_empty()
        );
        assert!(client.session.is_none());
        assert_eq!(client.api_prefix, "http://127.0.0.1:9/api");
        assert_eq!(client.allowed.describe(), "127.0.0.1");
    }

    #[test]
    fn invalid_base_url_is_refused() {
        let settings = GrdfSettings {
            base_url: "ftp://x".into(),
            login_url: "https://connexion.grdf.fr".into(),
            credentials: Credentials {
                username: crate::secret::Secret::new("a@b.fr"),
                password: crate::secret::Secret::new("x"),
            },
            pce: "1".into(),
        };
        assert!(GrdfClient::new(settings, Box::new(|_| {})).is_err());
    }
}

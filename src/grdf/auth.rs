//! Connexion à Mon Espace GRDF par le flux Okta IDX (PLAN.md §2.1), comme
//! `api_client.py` de PyGazpar :
//!
//! 1. `GET <base>/` (redirections suivies) → `"stateToken":"…"` dans le HTML,
//!    `\x2D` remplacé par `-` ;
//! 2. cookie `ln=<e-mail>` ;
//! 3. `POST <login>/idp/idx/identify` `{"identifier", "stateHandle"}` →
//!    `stateHandle` ;
//! 4. `POST <login>/idp/idx/challenge/answer`
//!    `{"credentials": {"passcode"}, "stateHandle"}` → `success.href` ;
//! 5. `GET success.href` (redirections suivies) → 200 : session ouverte.
//!    `success.href` n'est suivie que si son hôte est autorisé par le
//!    client HTTP (`grdf.fr` en production, voir [`super::http::AllowedHosts`]).
//!
//! Écart avec PyGazpar : la vraie réponse d'`identify` (jeu d'essai
//! `okta_identify_response.json`) n'a pas de `stateHandle` à la racine,
//! seulement dans le formulaire de l'étape `challenge-authenticator` ;
//! PyGazpar enverrait alors `null`, ce client envoie la valeur du formulaire.
//!
//! Chaque étape porte un nom ([`Step`], « GRDF, connexion 3/5
//! (identification Okta) ») qui précède toute erreur et toute ligne de
//! journal. En cas de refus, l'erreur donne le statut, l'hôte et le port qui
//! ont répondu, et ce qu'Okta dit : textes de `messages.value[].message` (ou
//! `errorSummary`) et codes (`i18n.key`, `errorCode`). **Aucun secret** (e-mail, mot de passe, `stateToken`,
//! `stateHandle`, cookie, adresse complète) n'apparaît dans un journal ou une
//! erreur : `stateToken` et `stateHandle` sont gardés en [`Secret`], et les
//! motifs d'Okta sont expurgés de tous les secrets, puis raccourcis
//! ([`crate::secret::scrub`]), avant usage. La connexion n'est
//! jamais retentée ici : un mauvais mot de passe = une seule tentative.
//!
//! **Refus des identifiants ou échec passager.** Un refus clair des
//! identifiants devient [`Error::GrdfCredentialsRejected`] (le démon cesse
//! alors de se connecter), tout autre échec [`Error::GrdfLogin`] (nouvel
//! essai au relevé suivant). Règle, dans [`credentials_rejected`] :
//!
//! - seulement en réponse à `identify` ou `challenge/answer`, avec un statut
//!   4xx autre que 429 ;
//! - jamais si Okta parle d'une session expirée (`idx.session.expired`,
//!   `errorSummary` « The session has expired. ») ;
//! - refus si le message d'Okta porte une clé de refus d'identifiants
//!   ([`REJECTION_KEYS`] : mot de passe faux, compte inconnu) ;
//! - refus aussi pour un 401 à `challenge/answer` sans clé connue : c'est
//!   l'étape qui vérifie le mot de passe, quelques millisecondes après
//!   `identify` (un `stateHandle` expiré entre les deux est invraisemblable),
//!   et la clé exacte d'Okta peut changer sans prévenir.
//!
//! Tout le reste est passager : statut 5xx ou 429, 400 ou 403 sans clé
//! connue, page d'accueil sans `stateToken`, étape `challenge-authenticator`
//! absente, `success.href` manquant ou hors de `grdf.fr`. Dans le doute, une
//! tentative par relevé (toutes les 8 h par défaut) vaut mieux qu'un démon
//! qui cesse de relever sans raison. Un compte inconnu est en pratique
//! refusé à `challenge/answer` : Okta répond normalement à `identify` pour
//! ne pas révéler quels comptes existent (comme le faux serveur des tests).
//!
//! L'analyse des réponses est faite par des fonctions pures.
//!
//! Dépendances : `grdf::http`, `url`, `secret`, `error`, `log`,
//! `serde_json`.

use std::fmt;

use serde_json::{Value, json};

use super::http::{AllowedHosts, HttpClient, Response};
use crate::error::Error;
use crate::secret::{self, Secret};
use crate::url::Url;

const IDENTIFY_PATH: &str = "/idp/idx/identify";
const CHALLENGE_PATH: &str = "/idp/idx/challenge/answer";
const OKTA_ACCEPT: &str = "application/json; okta-version=1.0.0";

/// Clés `i18n` d'Okta qui signifient un refus des identifiants : mot de
/// passe faux (`incorrectPassword` ; `errors.E0000004`, « Authentication
/// failed ») ou compte inconnu (`idx.unknown.user`).
const REJECTION_KEYS: [&str; 3] = ["incorrectPassword", "errors.E0000004", "idx.unknown.user"];
/// Clés `i18n` d'Okta d'une session de connexion expirée : passager.
const EXPIRED_KEYS: [&str; 1] = ["idx.session.expired"];

/// Étape du parcours de connexion (PLAN.md §2.1), nommée de la même façon
/// dans le journal et dans les erreurs : « GRDF, connexion 3/5
/// (identification Okta) ».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// `GET <base>/`, redirections suivies jusqu'à la page de connexion.
    Home,
    /// Cookie `ln` (aucune requête).
    Cookie,
    /// `POST /idp/idx/identify` (e-mail).
    Identify,
    /// `POST /idp/idx/challenge/answer` (mot de passe).
    Challenge,
    /// `GET success.href`, redirections suivies jusqu'à Mon Espace GRDF.
    Finish,
}

impl Step {
    const COUNT: u8 = 5;

    fn number(self) -> u8 {
        match self {
            Step::Home => 1,
            Step::Cookie => 2,
            Step::Identify => 3,
            Step::Challenge => 4,
            Step::Finish => 5,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Step::Home => "page d'accueil",
            Step::Cookie => "cookie ln",
            Step::Identify => "identification Okta",
            Step::Challenge => "mot de passe Okta",
            Step::Finish => "fin de connexion",
        }
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "GRDF, connexion {}/{} ({})",
            self.number(),
            Step::COUNT,
            self.label()
        )
    }
}

/// Longueur maximale d'un motif d'Okta repris dans une erreur.
const MAX_REASON_CHARS: usize = 200;
/// Nombre maximal de noms d'étapes Okta cités dans un message.
const MAX_REMEDIATIONS: usize = 10;

/// Identifiants du compte Mon Espace GRDF.
#[derive(Debug)]
pub struct Credentials {
    /// Adresse e-mail.
    pub username: Secret,
    pub password: Secret,
}

/// Ouvre une session : à la fin, `http` porte les cookies de Mon Espace GRDF.
///
/// `base_url` et `login_url` sans `/` final (ex. `https://monespace.grdf.fr`,
/// `https://connexion.grdf.fr`). Toute erreur est replacée dans son étape
/// ([`Step`]).
pub(crate) fn login(
    http: &mut HttpClient,
    base_url: &str,
    login_url: &str,
    credentials: &Credentials,
) -> Result<(), Error> {
    // En clair seulement pour être envoyés à GRDF ; les messages passent par
    // `reason`, qui expurge `credentials`.
    let username = credentials.username.expose();
    let password = credentials.password.expose();
    info!("connexion à Mon Espace GRDF");

    let step = Step::Home;
    debug!("{step}");
    let start = http
        .get(&format!("{base_url}/"), &[], &[])
        .map_err(|e| e.context(step))?;
    if start.status != 200 {
        return Err(Error::GrdfLogin(status_cause(&start)).context(step));
    }
    let state_token = extract_state_token(&start.body)
        .map(Secret::new)
        .ok_or_else(|| {
            Error::GrdfLogin(format!(
                "jeton « stateToken » introuvable dans la page reçue ({}) : le parcours de \
                 connexion de GRDF a peut-être changé",
                describe(&start)
            ))
            .context(step)
        })?;
    debug!(
        "{step} : stateToken trouvé dans la page de {}",
        start.final_url.host_port()
    );

    let step = Step::Cookie;
    debug!("{step}");
    let login = Url::http(login_url).map_err(|e| Error::from(e).context(step))?;
    let base = Url::http(base_url).map_err(|e| Error::from(e).context(step))?;
    http.set_cookie("ln", username, &login);
    http.set_cookie("ln", username, &base);

    let step = Step::Identify;
    debug!("{step}");
    let okta_headers = [
        ("Accept", OKTA_ACCEPT),
        ("Content-Type", "application/json"),
    ];
    let identify = http
        .post_json(
            &format!("{login_url}{IDENTIFY_PATH}"),
            &identify_payload(username, state_token.expose()),
            &okta_headers,
        )
        .map_err(|e| e.context(step))?;
    let secrets = [&credentials.username, &credentials.password, &state_token];
    if identify.status != 200 {
        return Err(refused(step, &identify, &secrets));
    }
    let identify_body: Value = serde_json::from_str(&identify.body).map_err(|_| {
        Error::GrdfLogin(format!("réponse JSON illisible ({})", describe(&identify))).context(step)
    })?;
    debug!(
        "{step} : statut 200, étapes proposées : {}",
        remediation_names(&identify_body)
    );
    if let Some(warning) =
        check_remediation(&identify_body, &format!("{login_url}{CHALLENGE_PATH}"))
    {
        warn!("{step} : {warning}");
    }
    let state_handle = read_state_handle(&identify_body)
        .map(Secret::new)
        .ok_or_else(|| {
            Error::GrdfLogin(format!(
                "pas de stateHandle dans la réponse ({}, Okta : {})",
                describe(&identify),
                reason(&identify.body, &secrets)
            ))
            .context(step)
        })?;

    let step = Step::Challenge;
    debug!("{step}");
    let secrets = [
        &credentials.username,
        &credentials.password,
        &state_token,
        &state_handle,
    ];
    let answer = http
        .post_json(
            &format!("{login_url}{CHALLENGE_PATH}"),
            &challenge_payload(password, state_handle.expose()),
            &okta_headers,
        )
        .map_err(|e| e.context(step))?;
    if answer.status != 200 {
        return Err(refused(step, &answer, &secrets));
    }
    let answer_body: Value = serde_json::from_str(&answer.body).unwrap_or(Value::Null);
    let success_href = read_success_href(&answer_body).ok_or_else(|| {
        Error::GrdfLogin(format!(
            "réponse sans success.href, étape supplémentaire demandée par GRDF ? \
             (étapes proposées : {} ; {}, Okta : {})",
            remediation_names(&answer_body),
            describe(&answer),
            reason(&answer.body, &secrets)
        ))
        .context(step)
    })?;

    let step = Step::Finish;
    let target =
        check_success_href(&success_href, http.allowed_hosts()).map_err(|e| e.context(step))?;
    debug!("{step} : success.href vers {target}");
    let done = http
        .get(&success_href, &[], &[])
        .map_err(|e| e.context(step))?;
    if done.status != 200 {
        return Err(Error::GrdfLogin(status_cause(&done)).context(step));
    }
    info!("connecté à Mon Espace GRDF");
    Ok(())
}

/// Statut, nature et cible d'une réponse, pour un message : « statut HTTP
/// 503, page HTML, connexion.grdf.fr:443 ». Jamais le corps ni l'adresse.
fn describe(response: &Response) -> String {
    let html = if response.is_html() {
        ", page HTML"
    } else {
        ""
    };
    format!(
        "statut HTTP {}{html}, {}",
        response.status,
        response.final_url.host_port()
    )
}

/// Cause d'un statut inattendu sur une page (accueil, fin de connexion).
fn status_cause(response: &Response) -> String {
    let what = match response.status {
        429 => "débit limité par GRDF",
        500..=599 => "service indisponible",
        _ => "réponse inattendue",
    };
    let retry = response.retry_after.as_deref().map_or(String::new(), |v| {
        let v: String = v.trim().chars().take(40).collect();
        format!(", Retry-After « {v} »")
    });
    format!("{what} ({}{retry})", describe(response))
}

/// Erreur d'un refus d'Okta à `step`, replacée dans l'étape :
/// [`Error::GrdfCredentialsRejected`] si [`credentials_rejected`], sinon
/// [`Error::GrdfLogin`]. Le message donne le statut, la cible et ce
/// qu'Okta dit ([`reason`]).
fn refused(step: Step, response: &Response, secrets: &[&Secret]) -> Error {
    let details = format!(
        "{}, Okta : {}",
        describe(response),
        reason(&response.body, secrets)
    );
    let error = if credentials_rejected(step, response.status, &response.body) {
        Error::GrdfCredentialsRejected(format!("identifiants refusés par GRDF ({details})"))
    } else {
        let parsed: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
        let what = match response.status {
            _ if is_expired(&parsed) => "session de connexion Okta expirée",
            429 => "débit limité par GRDF",
            500..=599 => "service de connexion indisponible",
            _ => "réponse inattendue d'Okta",
        };
        Error::GrdfLogin(format!("{what} ({details})"))
    };
    error.context(step)
}

/// La réponse d'Okta à `step` (statut et corps) refuse-t-elle clairement
/// les identifiants ? Règle détaillée en tête du module ; dans le doute,
/// `false` (échec passager).
fn credentials_rejected(step: Step, status: u16, body: &str) -> bool {
    if !(400..500).contains(&status) || status == 429 {
        return false;
    }
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if is_expired(&parsed) {
        return false;
    }
    message_keys(&parsed)
        .iter()
        .any(|k| REJECTION_KEYS.contains(k))
        || (step == Step::Challenge && status == 401)
}

/// Okta parle-t-il d'une session de connexion expirée (clé
/// `idx.session.expired` ou `errorSummary` « … expired … ») ?
fn is_expired(body: &Value) -> bool {
    message_keys(body).iter().any(|k| EXPIRED_KEYS.contains(k))
        || body
            .get("errorSummary")
            .and_then(Value::as_str)
            .is_some_and(|s| s.to_ascii_lowercase().contains("expired"))
}

/// Clés `messages.value[].i18n.key` d'une réponse d'Okta.
fn message_keys(body: &Value) -> Vec<&str> {
    body.get("messages")
        .and_then(|m| m.get("value"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.get("i18n")?.get("key")?.as_str())
                .collect()
        })
        .unwrap_or_default()
}

/// `"stateToken" : "…"` dans la page de connexion, avec `\x2D` → `-`.
fn extract_state_token(html: &str) -> Option<String> {
    const KEY: &str = "\"stateToken\"";
    let mut rest = html;
    while let Some(i) = rest.find(KEY) {
        rest = &rest[i + KEY.len()..];
        let Some(after_colon) = rest.trim_start().strip_prefix(':') else {
            continue;
        };
        let Some(value) = after_colon.trim_start().strip_prefix('"') else {
            continue;
        };
        let end = value.find('"')?;
        if end > 0 {
            return Some(value[..end].replace("\\x2D", "-"));
        }
    }
    None
}

fn identify_payload(username: &str, state_token: &str) -> Value {
    json!({"identifier": username, "stateHandle": state_token})
}

fn challenge_payload(password: &str, state_handle: &str) -> Value {
    json!({"credentials": {"passcode": password}, "stateHandle": state_handle})
}

/// Étape IDX proposée sous ce nom (`remediation.value[].name`).
fn find_remediation<'a>(body: &'a Value, name: &str) -> Option<&'a Value> {
    body.get("remediation")?
        .get("value")?
        .as_array()?
        .iter()
        .find(|r| r.get("name").and_then(Value::as_str) == Some(name))
}

/// `stateHandle` de la réponse d'`identify` : à la racine, sinon le champ
/// `stateHandle` du formulaire de l'étape `challenge-authenticator`.
fn read_state_handle(body: &Value) -> Option<String> {
    let non_empty = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    if let Some(handle) = body.get("stateHandle").and_then(non_empty) {
        return Some(handle);
    }
    find_remediation(body, "challenge-authenticator")?
        .get("value")?
        .as_array()?
        .iter()
        .find(|field| field.get("name").and_then(Value::as_str) == Some("stateHandle"))?
        .get("value")
        .and_then(non_empty)
}

/// Noms des étapes IDX proposées (`remediation.value[].name`), pour un
/// message : « select-authenticator-authenticate, challenge-authenticator »,
/// ou « aucune ». Des noms seulement, jamais leurs formulaires (qui portent
/// le `stateHandle`).
fn remediation_names(body: &Value) -> String {
    let names: Vec<String> = body
        .get("remediation")
        .and_then(|r| r.get("value"))
        .and_then(Value::as_array)
        .map(|steps| {
            steps
                .iter()
                .filter_map(|s| s.get("name").and_then(Value::as_str))
                .take(MAX_REMEDIATIONS)
                .map(|name| name.chars().take(64).collect())
                .collect()
        })
        .unwrap_or_default();
    if names.is_empty() {
        "aucune".to_owned()
    } else {
        names.join(", ")
    }
}

/// Avertissement si l'étape `challenge-authenticator` manque ou vise une
/// autre adresse que celle où le mot de passe est envoyé (diagnostic de
/// PyGazpar, ticket gazpar2haws #118). Noms d'étapes, hôtes et chemins
/// seulement, jamais de requête.
fn check_remediation(body: &Value, expected_href: &str) -> Option<String> {
    let Some(challenge) = find_remediation(body, "challenge-authenticator") else {
        return Some(format!(
            "GRDF ne propose pas l'étape « challenge-authenticator » après l'identification \
             (étapes proposées : {})",
            remediation_names(body)
        ));
    };
    let href = challenge.get("href").and_then(Value::as_str).unwrap_or("");
    let shown = |h: &str| Url::http(h).map_or_else(|_| "illisible".to_owned(), |u| u.redacted());
    (shown(href) != shown(expected_href)).then(|| {
        format!(
            "l'étape « challenge-authenticator » vise {} au lieu de {}",
            shown(href),
            shown(expected_href)
        )
    })
}

/// `success.href` de la réponse au mot de passe.
fn read_success_href(body: &Value) -> Option<String> {
    body.get("success")?
        .get("href")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// `success.href` ne doit mener qu'à un hôte autorisé ; renvoie l'hôte et
/// le port visés. L'erreur ne nomme que l'hôte (la requête porte un jeton).
fn check_success_href(href: &str, allowed: &AllowedHosts) -> Result<String, Error> {
    let url = Url::http(href).map_err(|_| Error::GrdfLogin("success.href illisible".to_owned()))?;
    if allowed.allows(url.host()) {
        Ok(url.host_port())
    } else {
        Err(Error::GrdfLogin(format!(
            "success.href mène à l'hôte {}, hors de {} : refusé",
            url.host(),
            allowed.describe()
        )))
    }
}

/// Ce qu'une réponse d'Okta dit d'un refus, par ses seuls champs connus :
/// les textes `messages.value[].message` (à défaut `errorSummary`) entre
/// guillemets, puis les codes (`messages.value[].i18n.key`, `errorCode`)
/// entre crochets : « Authentication failed » [errors.E0000004]. Jamais le
/// reste de la réponse, qui contient le `stateHandle` et l'identifiant
/// saisi. Ni expurgé ni raccourci : passer par [`reason`].
fn login_error_reason(body: &str) -> String {
    let Ok(body) = serde_json::from_str::<Value>(body) else {
        return "aucun motif donné".to_owned();
    };
    let values: &[Value] = body
        .get("messages")
        .and_then(|m| m.get("value"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let non_empty = |v: &&str| !v.trim().is_empty();
    let mut texts: Vec<&str> = values
        .iter()
        .filter_map(|v| v.get("message").and_then(Value::as_str))
        .filter(non_empty)
        .collect();
    if texts.is_empty() {
        texts.extend(
            body.get("errorSummary")
                .and_then(Value::as_str)
                .filter(non_empty),
        );
    }
    let mut codes: Vec<&str> = message_keys(&body);
    codes.extend(body.get("errorCode").and_then(Value::as_str));
    codes.retain(|c| !c.trim().is_empty());
    let mut out: Vec<String> = texts.iter().map(|t| format!("« {} »", t.trim())).collect();
    if !codes.is_empty() {
        out.push(format!("[{}]", codes.join(", ")));
    }
    if out.is_empty() {
        "aucun motif donné".to_owned()
    } else {
        out.join(" ")
    }
}

/// Motif d'un refus d'Okta prêt pour un message : expurgé des secrets (un
/// motif d'Okta peut recopier l'identifiant saisi), puis raccourci.
fn reason(body: &str, secrets: &[&Secret]) -> String {
    secret::scrub(&login_error_reason(body), secrets, MAX_REASON_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OKTA_IDENTIFY: &str =
        include_str!("../../tests/fixtures/pygazpar/okta_identify_response.json");
    const FIXTURE_HANDLE: &str = "02.id.FIXTURE-STATE-HANDLE-0000000000000000000000000000000000000000000000000000000000000000";

    fn value(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    // --- stateToken ---

    #[test]
    fn state_token_with_x2d_escapes() {
        let html = r#"<script>var oktaData = {"redirectUri":"x","stateToken":"02.id.abc\x2Ddef\x2D123","x":1};</script>"#;
        assert_eq!(
            extract_state_token(html).as_deref(),
            Some("02.id.abc-def-123")
        );
    }

    #[test]
    fn state_token_spacing_like_the_regex() {
        // Motif de PyGazpar : "stateToken"\s*:\s*"([^"]+)".
        assert_eq!(
            extract_state_token("\"stateToken\" : \"STATE\"").as_deref(),
            Some("STATE")
        );
        assert_eq!(
            extract_state_token("\"stateToken\"\n:\t\"S\"").as_deref(),
            Some("S")
        );
    }

    #[test]
    fn state_token_skips_non_matching_occurrences() {
        let html = r#"{"stateToken": null, "stateToken":"", "stateToken":"BON"}"#;
        assert_eq!(extract_state_token(html).as_deref(), Some("BON"));
    }

    #[test]
    fn state_token_absent() {
        for html in [
            "",
            "<html>maintenance</html>",
            "stateToken:\"X\"",
            "\"stateToken\":\"non fermé",
            "\"stateToken\"",
        ] {
            assert_eq!(extract_state_token(html), None, "{html:?}");
        }
    }

    // --- Charges utiles ---

    #[test]
    fn identify_payload_shape() {
        assert_eq!(
            identify_payload("user@example.com", "STATE"),
            json!({"identifier": "user@example.com", "stateHandle": "STATE"})
        );
    }

    #[test]
    fn password_is_sent_exactly_as_typed() {
        // Mots de passe de test_login_payload.py (PyGazpar).
        for password in [
            "plain",
            "with\"quote",
            "back\\slash",
            "tab\there",
            "hash#%&+ok",
            "accent-é",
            "euro€",
        ] {
            let payload = challenge_payload(password, "HANDLE").to_string();
            let back: Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(back["credentials"]["passcode"], password);
            assert_eq!(back["stateHandle"], "HANDLE");
        }
    }

    // --- Réponse d'identify ---

    #[test]
    fn real_identify_response_has_handle_only_in_forms() {
        let body = value(OKTA_IDENTIFY);
        assert!(body.get("stateHandle").is_none());
        assert_eq!(read_state_handle(&body).as_deref(), Some(FIXTURE_HANDLE));
    }

    #[test]
    fn top_level_handle_wins() {
        let body = json!({"stateHandle": "RACINE", "remediation": {"value": [
            {"name": "challenge-authenticator", "value": [{"name": "stateHandle", "value": "FORM"}]}
        ]}});
        assert_eq!(read_state_handle(&body).as_deref(), Some("RACINE"));
    }

    #[test]
    fn missing_handle() {
        for body in [
            json!({}),
            json!({"stateHandle": ""}),
            json!({"remediation": {"value": [{"name": "select-authenticator-authenticate",
                "value": [{"name": "stateHandle", "value": "X"}]}]}}),
            json!({"remediation": {"value": "pas une liste"}}),
        ] {
            assert_eq!(read_state_handle(&body), None, "{body}");
        }
    }

    #[test]
    fn real_identify_response_matches_fixed_challenge_url() {
        // Assertions de PyGazpar (TestRealGrdfIdentifyResponse).
        let body = value(OKTA_IDENTIFY);
        let challenge = find_remediation(&body, "challenge-authenticator").unwrap();
        assert_eq!(
            challenge["href"],
            "https://connexion.grdf.fr/idp/idx/challenge/answer"
        );
        assert!(find_remediation(&body, "select-authenticator-authenticate").is_some());
        assert_eq!(
            check_remediation(&body, "https://connexion.grdf.fr/idp/idx/challenge/answer"),
            None
        );
    }

    #[test]
    fn remediation_warnings() {
        let other =
            json!({"remediation": {"value": [{"name": "select-authenticator-authenticate"}]}});
        let w = check_remediation(&other, "https://connexion.grdf.fr/idp/idx/challenge/answer")
            .unwrap();
        assert!(w.contains("select-authenticator-authenticate"), "{w}");
        let w = check_remediation(&json!({}), "https://x/").unwrap();
        assert!(w.contains("aucune"), "{w}");
        let elsewhere = json!({"remediation": {"value": [{"name": "challenge-authenticator",
            "href": "https://connexion.grdf.fr/idp/idx/elsewhere?token=SECRET"}]}});
        let w = check_remediation(
            &elsewhere,
            "https://connexion.grdf.fr/idp/idx/challenge/answer",
        )
        .unwrap();
        assert!(w.contains("idp/idx/elsewhere"), "{w}");
        assert!(!w.contains("SECRET"), "{w}");
    }

    // --- Réponse au mot de passe ---

    #[test]
    fn success_href() {
        let body = json!({"success": {"name": "success-redirect", "href": "https://connexion.grdf.fr/login/token/redirect?stateToken=X"}});
        assert_eq!(
            read_success_href(&body).as_deref(),
            Some("https://connexion.grdf.fr/login/token/redirect?stateToken=X")
        );
        for body in [
            json!({}),
            json!({"success": {}}),
            json!({"success": {"href": ""}}),
            json!({"success": {"href": 3}}),
        ] {
            assert_eq!(read_success_href(&body), None);
        }
    }

    #[test]
    fn success_href_must_stay_on_grdf_fr() {
        let allowed =
            AllowedHosts::from_urls(&["https://monespace.grdf.fr", "https://connexion.grdf.fr"])
                .unwrap();
        assert_eq!(
            check_success_href(
                "https://connexion.grdf.fr/login/token/redirect?stateToken=X",
                &allowed,
            )
            .unwrap(),
            "connexion.grdf.fr:443"
        );
        let e = check_success_href(
            "https://connexion.grdf.fr.example.org/login/token/redirect?stateToken=SECRET",
            &allowed,
        )
        .unwrap_err();
        assert_eq!(
            e,
            Error::GrdfLogin(
                "success.href mène à l'hôte connexion.grdf.fr.example.org, hors de grdf.fr : \
                 refusé"
                    .into()
            )
        );
        let e = check_success_href("/relatif?stateToken=SECRET", &allowed).unwrap_err();
        assert!(
            matches!(&e, Error::GrdfLogin(m) if !m.contains("SECRET")),
            "{e}"
        );
    }

    // --- Motifs de refus ---

    #[test]
    fn messages_are_kept_and_the_token_is_not() {
        let body = r#"{"version": "1.0.0", "stateHandle": "02.id.SECRET-TOKEN",
            "messages": {"type": "array", "value": [{"message": "Authentication failed", "class": "ERROR"}]}}"#;
        assert_eq!(login_error_reason(body), "« Authentication failed »");
        let two = r#"{"messages": {"value": [{"message": "A"}, {"x": 1}, {"message": "B"}]}}"#;
        assert_eq!(login_error_reason(two), "« A » « B »");
        let keyed = okta_message("errors.E0000004", "Authentication failed");
        assert_eq!(
            login_error_reason(&keyed),
            "« Authentication failed » [errors.E0000004]"
        );
    }

    #[test]
    fn error_summary_and_code_are_the_fallback() {
        let body = r#"{"errorSummary": "Bad request", "stateHandle": "SECRET-TOKEN"}"#;
        assert_eq!(login_error_reason(body), "« Bad request »");
        // Erreur d'API classique d'Okta.
        let body = r#"{"errorCode": "E0000011", "errorSummary": "Invalid token provided",
            "errorLink": "E0000011", "errorId": "oaeXYZ", "errorCauses": []}"#;
        assert_eq!(
            login_error_reason(body),
            "« Invalid token provided » [E0000011]"
        );
        let body = r#"{"errorCode": "E0000047"}"#;
        assert_eq!(login_error_reason(body), "[E0000047]");
    }

    #[test]
    fn neutral_text_without_reason() {
        for body in [
            "{}",
            "[]",
            "\"texte\"",
            "null",
            r#"{"errorSummary": " ", "errorCode": ""}"#,
            "<html>SECRET-TOKEN</html>",
            "",
        ] {
            assert_eq!(login_error_reason(body), "aucun motif donné", "{body}");
        }
    }

    #[test]
    fn long_reason_is_cut_after_scrubbing() {
        let body = json!({"errorSummary": "x".repeat(1000)}).to_string();
        assert_eq!(reason(&body, &[]).chars().count(), MAX_REASON_CHARS);
        assert!(reason(&body, &[]).starts_with("« xxx"));
        // Secret à cheval sur la limite : masqué, pas coupé en deux.
        let email = Secret::new("jean@example.org");
        let text = format!("{}jean@example.org", "x".repeat(MAX_REASON_CHARS - 5));
        let body = json!({"errorSummary": text}).to_string();
        let shown = reason(&body, &[&email]);
        assert!(!shown.contains("jean"), "{shown}");
        assert!(shown.ends_with("***"), "{shown}");
        assert_eq!(shown.chars().count(), MAX_REASON_CHARS);
    }

    #[test]
    fn refusal_error_holds_no_secret() {
        let body = json!({
            "stateHandle": "02.id.HANDLE",
            "messages": {"value": [{"message": "Compte jean@example.org bloqué"}]}
        })
        .to_string();
        let e = refused(
            Step::Challenge,
            &okta_response(401, &body),
            &[
                &Secret::new("jean@example.org"),
                &Secret::new("pw"),
                &Secret::new("02.id.HANDLE"),
            ],
        );
        let shown = format!("{e} {e:?}");
        assert!(
            !shown.contains("jean@") && !shown.contains("HANDLE"),
            "{shown}"
        );
        assert!(shown.contains("statut HTTP 401"), "{shown}");
        assert!(shown.contains("« Compte *** bloqué »"), "{shown}");
    }

    // --- Refus des identifiants ou échec passager ---

    fn okta_message(key: &str, message: &str) -> String {
        json!({
            "version": "1.0.0",
            "stateHandle": "02.id.HANDLE",
            "messages": {"type": "array", "value": [
                {"message": message, "i18n": {"key": key}, "class": "ERROR"}
            ]}
        })
        .to_string()
    }

    #[test]
    fn wrong_password_is_a_rejection() {
        for key in ["errors.E0000004", "incorrectPassword"] {
            let body = okta_message(key, "Authentication failed");
            assert!(credentials_rejected(Step::Challenge, 401, &body), "{key}");
            assert!(credentials_rejected(Step::Challenge, 400, &body), "{key}");
        }
        // 401 au mot de passe, sans motif reconnu : refus.
        assert!(credentials_rejected(Step::Challenge, 401, "{}"));
        assert!(credentials_rejected(Step::Challenge, 401, "<html>"));
    }

    #[test]
    fn unknown_account_is_a_rejection() {
        let body = okta_message("idx.unknown.user", "There is no account with the Username.");
        assert!(credentials_rejected(Step::Identify, 400, &body));
        assert!(credentials_rejected(Step::Challenge, 401, &body));
    }

    #[test]
    fn expired_session_is_transient() {
        // Réponse du faux serveur à un stateHandle faux.
        let summary =
            json!({"version": "1.0.0", "errorSummary": "The session has expired."}).to_string();
        for (step, status) in [
            (Step::Challenge, 400),
            (Step::Challenge, 401),
            (Step::Identify, 401),
        ] {
            assert!(
                !credentials_rejected(step, status, &summary),
                "{step:?} {status}"
            );
            let keyed = okta_message("idx.session.expired", "The session has expired.");
            assert!(
                !credentials_rejected(step, status, &keyed),
                "{step:?} {status}"
            );
        }
    }

    #[test]
    fn other_failures_are_transient() {
        let wrong = okta_message("errors.E0000004", "Authentication failed");
        // Débit limité ou panne de GRDF, même avec un motif de refus.
        for status in [429, 500, 502, 503, 200, 302] {
            assert!(
                !credentials_rejected(Step::Challenge, status, &wrong),
                "{status}"
            );
        }
        // Refus sans motif reconnu ailleurs qu'au mot de passe en 401.
        for (step, status) in [
            (Step::Identify, 400),
            (Step::Identify, 401),
            (Step::Identify, 403),
            (Step::Challenge, 400),
            (Step::Challenge, 403),
        ] {
            let body = okta_message("errors.E0000001", "Api validation failed");
            assert!(
                !credentials_rejected(step, status, &body),
                "{step:?} {status}"
            );
            assert!(
                !credentials_rejected(step, status, "{}"),
                "{step:?} {status}"
            );
        }
    }

    /// Réponse d'Okta reçue de `connexion.grdf.fr`.
    fn okta_response(status: u16, body: &str) -> Response {
        Response {
            status,
            final_url: Url::http("https://connexion.grdf.fr/idp/idx/challenge/answer").unwrap(),
            content_type: Some("application/ion+json; okta-version=1.0.0".into()),
            body: body.to_owned(),
            retry_after: None,
        }
    }

    #[test]
    fn steps_are_named_for_logs_and_errors() {
        let names: Vec<String> = [
            Step::Home,
            Step::Cookie,
            Step::Identify,
            Step::Challenge,
            Step::Finish,
        ]
        .iter()
        .map(Step::to_string)
        .collect();
        assert_eq!(
            names,
            [
                "GRDF, connexion 1/5 (page d'accueil)",
                "GRDF, connexion 2/5 (cookie ln)",
                "GRDF, connexion 3/5 (identification Okta)",
                "GRDF, connexion 4/5 (mot de passe Okta)",
                "GRDF, connexion 5/5 (fin de connexion)",
            ]
        );
    }

    #[test]
    fn rejection_gives_the_dedicated_error_without_secret() {
        let body = okta_message("errors.E0000004", "Authentication failed jean@example.org");
        let e = refused(
            Step::Challenge,
            &okta_response(401, &body),
            &[
                &Secret::new("jean@example.org"),
                &Secret::new("pw"),
                &Secret::new("02.id.HANDLE"),
            ],
        );
        assert_eq!(
            e,
            Error::GrdfCredentialsRejected(
                "GRDF, connexion 4/5 (mot de passe Okta) : identifiants refusés par GRDF \
                 (statut HTTP 401, connexion.grdf.fr:443, Okta : « Authentication failed *** » \
                 [errors.E0000004])"
                    .into()
            )
        );
    }

    #[test]
    fn other_refusals_say_what_happened_not_password_refused() {
        let html = |status| Response {
            content_type: Some("text/html; charset=UTF-8".into()),
            ..okta_response(status, "<html>Service indisponible</html>")
        };
        let cases = [
            (
                Step::Challenge,
                html(503),
                "GRDF, connexion 4/5 (mot de passe Okta) : service de connexion indisponible \
                 (statut HTTP 503, page HTML, connexion.grdf.fr:443, Okta : aucun motif donné)",
            ),
            (
                Step::Identify,
                okta_response(429, "{}"),
                "GRDF, connexion 3/5 (identification Okta) : débit limité par GRDF \
                 (statut HTTP 429, connexion.grdf.fr:443, Okta : aucun motif donné)",
            ),
            (
                Step::Identify,
                okta_response(
                    400,
                    &json!({"version": "1.0.0", "errorSummary": "The session has expired."})
                        .to_string(),
                ),
                "GRDF, connexion 3/5 (identification Okta) : session de connexion Okta expirée \
                 (statut HTTP 400, connexion.grdf.fr:443, Okta : « The session has expired. »)",
            ),
            (
                Step::Identify,
                okta_response(
                    403,
                    &okta_message("errors.E0000001", "Api validation failed"),
                ),
                "GRDF, connexion 3/5 (identification Okta) : réponse inattendue d'Okta \
                 (statut HTTP 403, connexion.grdf.fr:443, Okta : « Api validation failed » \
                 [errors.E0000001])",
            ),
        ];
        for (step, response, expected) in cases {
            let e = refused(step, &response, &[]);
            assert_eq!(e, Error::GrdfLogin(expected.into()));
            assert_eq!(e.to_string(), expected);
        }
    }

    #[test]
    fn page_status_causes() {
        let mut page = okta_response(429, "");
        page.content_type = Some("text/html".into());
        page.retry_after = Some("30".into());
        assert_eq!(
            status_cause(&page),
            "débit limité par GRDF (statut HTTP 429, page HTML, connexion.grdf.fr:443, \
             Retry-After « 30 »)"
        );
        assert_eq!(
            status_cause(&okta_response(502, "")),
            "service indisponible (statut HTTP 502, connexion.grdf.fr:443)"
        );
        assert_eq!(
            status_cause(&okta_response(404, "")),
            "réponse inattendue (statut HTTP 404, connexion.grdf.fr:443)"
        );
    }

    #[test]
    fn remediation_names_only() {
        assert_eq!(
            remediation_names(&value(OKTA_IDENTIFY)),
            "challenge-authenticator, select-authenticator-authenticate"
        );
        assert_eq!(remediation_names(&json!({})), "aucune");
        // Jamais le contenu des formulaires (stateHandle).
        assert!(!remediation_names(&value(OKTA_IDENTIFY)).contains("FIXTURE"));
    }

    #[test]
    fn credentials_debug_is_masked() {
        let c = Credentials {
            username: Secret::new("jean@example.org"),
            password: Secret::new("hunter2"),
        };
        let shown = format!("{c:?}");
        assert!(
            !shown.contains("jean") && !shown.contains("hunter2"),
            "{shown}"
        );
    }
}

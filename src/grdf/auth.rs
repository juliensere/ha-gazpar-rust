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
//!
//! Écart avec PyGazpar : la vraie réponse d'`identify` (jeu d'essai
//! `okta_identify_response.json`) n'a pas de `stateHandle` à la racine,
//! seulement dans le formulaire de l'étape `challenge-authenticator` ;
//! PyGazpar enverrait alors `null`, ce client envoie la valeur du formulaire.
//!
//! En cas de refus, le motif vient de `messages.value[].message` ou de
//! `errorSummary`. **Aucun secret** (e-mail, mot de passe, `stateToken`,
//! `stateHandle`, cookie, adresse complète) n'apparaît dans un journal ou une
//! erreur : les motifs d'Okta sont expurgés avant usage. La connexion n'est
//! jamais retentée ici : un mauvais mot de passe = une seule tentative.
//!
//! L'analyse des réponses est faite par des fonctions pures.
//!
//! Dépendances : `grdf::http`, `grdf::url`, `secret`, `error`, `log`,
//! `serde_json`.

use serde_json::{Value, json};

use super::http::HttpClient;
use super::url::Url;
use crate::error::Error;
use crate::secret::Secret;

pub const IDENTIFY_PATH: &str = "/idp/idx/identify";
pub const CHALLENGE_PATH: &str = "/idp/idx/challenge/answer";
const OKTA_ACCEPT: &str = "application/json; okta-version=1.0.0";
/// Longueur maximale d'un motif d'Okta repris dans une erreur.
const MAX_REASON_CHARS: usize = 200;

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
/// `https://connexion.grdf.fr`).
pub fn login(
    http: &mut HttpClient,
    base_url: &str,
    login_url: &str,
    credentials: &Credentials,
) -> Result<(), Error> {
    let username = credentials.username.expose();
    let password = credentials.password.expose();
    info!("connexion à Mon Espace GRDF");

    debug!("connexion 1/5 : page d'accueil");
    let start = http.get(&format!("{base_url}/"), &[], &[])?;
    if start.status != 200 {
        return Err(Error::GrdfLogin(format!(
            "page d'accueil : statut HTTP {} ({})",
            start.status,
            start.final_url.redacted()
        )));
    }
    let state_token = extract_state_token(&start.body).ok_or_else(|| {
        Error::GrdfLogin(
            "jeton « stateToken » introuvable dans la page de connexion \
             (le parcours de connexion de GRDF a peut-être changé)"
                .to_owned(),
        )
    })?;

    debug!("connexion 2/5 : cookie ln");
    http.set_cookie("ln", username, login_url)?;
    http.set_cookie("ln", username, base_url)?;

    debug!("connexion 3/5 : identification");
    let okta_headers = [
        ("Accept", OKTA_ACCEPT),
        ("Content-Type", "application/json"),
    ];
    let identify = http.post_json(
        &format!("{login_url}{IDENTIFY_PATH}"),
        &identify_payload(username, &state_token),
        &okta_headers,
    )?;
    let secrets = [username, password, state_token.as_str()];
    if identify.status != 200 {
        return Err(refused(
            "identification",
            identify.status,
            &identify.body,
            &secrets,
        ));
    }
    let identify_body: Value = serde_json::from_str(&identify.body)
        .map_err(|_| Error::GrdfLogin("identification : réponse JSON illisible".to_owned()))?;
    if let Some(warning) =
        check_remediation(&identify_body, &format!("{login_url}{CHALLENGE_PATH}"))
    {
        warn!("{warning}");
    }
    let state_handle = read_state_handle(&identify_body).ok_or_else(|| {
        Error::GrdfLogin(format!(
            "identification : pas de stateHandle dans la réponse ({})",
            scrub(&login_error_reason(&identify.body), &secrets)
        ))
    })?;

    debug!("connexion 4/5 : mot de passe");
    let secrets = [
        username,
        password,
        state_token.as_str(),
        state_handle.as_str(),
    ];
    let answer = http.post_json(
        &format!("{login_url}{CHALLENGE_PATH}"),
        &challenge_payload(password, &state_handle),
        &okta_headers,
    )?;
    if answer.status != 200 {
        return Err(refused(
            "mot de passe",
            answer.status,
            &answer.body,
            &secrets,
        ));
    }
    let success_href = serde_json::from_str::<Value>(&answer.body)
        .ok()
        .as_ref()
        .and_then(read_success_href)
        .ok_or_else(|| {
            Error::GrdfLogin(format!(
                "mot de passe : réponse sans success.href, étape supplémentaire demandée par GRDF ? ({})",
                scrub(&login_error_reason(&answer.body), &secrets)
            ))
        })?;

    debug!("connexion 5/5 : fin de connexion");
    let done = http.get(&success_href, &[], &[])?;
    if done.status != 200 {
        return Err(Error::GrdfLogin(format!(
            "fin de connexion : statut HTTP {} ({})",
            done.status,
            done.final_url.redacted()
        )));
    }
    info!("connecté à Mon Espace GRDF");
    Ok(())
}

fn refused(step: &str, status: u16, body: &str, secrets: &[&str]) -> Error {
    Error::GrdfLogin(format!(
        "{step} refusé par GRDF (statut HTTP {status}) : {}",
        scrub(&login_error_reason(body), secrets)
    ))
}

/// `"stateToken" : "…"` dans la page de connexion, avec `\x2D` → `-`.
pub fn extract_state_token(html: &str) -> Option<String> {
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

pub fn identify_payload(username: &str, state_token: &str) -> Value {
    json!({"identifier": username, "stateHandle": state_token})
}

pub fn challenge_payload(password: &str, state_handle: &str) -> Value {
    json!({"credentials": {"passcode": password}, "stateHandle": state_handle})
}

/// Étape IDX proposée sous ce nom (`remediation.value[].name`).
pub fn find_remediation<'a>(body: &'a Value, name: &str) -> Option<&'a Value> {
    body.get("remediation")?
        .get("value")?
        .as_array()?
        .iter()
        .find(|r| r.get("name").and_then(Value::as_str) == Some(name))
}

/// `stateHandle` de la réponse d'`identify` : à la racine, sinon le champ
/// `stateHandle` du formulaire de l'étape `challenge-authenticator`.
pub fn read_state_handle(body: &Value) -> Option<String> {
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

/// Avertissement si l'étape `challenge-authenticator` manque ou vise une
/// autre adresse que celle où le mot de passe est envoyé (diagnostic de
/// PyGazpar, ticket gazpar2haws #118). Noms d'étapes et adresses sans
/// requête seulement.
pub fn check_remediation(body: &Value, expected_href: &str) -> Option<String> {
    let Some(challenge) = find_remediation(body, "challenge-authenticator") else {
        let offered: Vec<&str> = body
            .get("remediation")
            .and_then(|r| r.get("value"))
            .and_then(Value::as_array)
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(|s| s.get("name").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        let offered = if offered.is_empty() {
            "aucune".to_owned()
        } else {
            offered.join(", ")
        };
        return Some(format!(
            "GRDF ne propose pas l'étape « challenge-authenticator » après l'identification \
             (étapes proposées : {offered})"
        ));
    };
    let href = challenge.get("href").and_then(Value::as_str).unwrap_or("");
    let shown = |h: &str| Url::parse(h).map_or_else(|_| "illisible".to_owned(), |u| u.redacted());
    (shown(href) != shown(expected_href)).then(|| {
        format!(
            "l'étape « challenge-authenticator » vise {} au lieu de {}",
            shown(href),
            shown(expected_href)
        )
    })
}

/// `success.href` de la réponse au mot de passe.
pub fn read_success_href(body: &Value) -> Option<String> {
    body.get("success")?
        .get("href")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Motif d'un refus d'Okta : `messages.value[].message`, sinon
/// `errorSummary`. Jamais le reste de la réponse, qui contient le
/// `stateHandle`. À expurger avec [`scrub`] avant usage.
pub fn login_error_reason(body: &str) -> String {
    let Ok(body) = serde_json::from_str::<Value>(body) else {
        return "aucun motif donné".to_owned();
    };
    let mut reasons: Vec<&str> = body
        .get("messages")
        .and_then(|m| m.get("value"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.get("message").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    if reasons.is_empty() {
        reasons.extend(body.get("errorSummary").and_then(Value::as_str));
    }
    let joined = reasons.join(" ; ");
    if joined.trim().is_empty() {
        "aucun motif donné".to_owned()
    } else {
        joined.chars().take(MAX_REASON_CHARS).collect()
    }
}

/// Remplace chaque secret non vide par `***` (un motif d'Okta peut recopier
/// l'identifiant saisi).
pub fn scrub(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_owned();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        out = out.replace(secret, Secret::MASK);
    }
    out
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

    // --- Motifs de refus ---

    #[test]
    fn messages_are_kept_and_the_token_is_not() {
        let body = r#"{"version": "1.0.0", "stateHandle": "02.id.SECRET-TOKEN",
            "messages": {"type": "array", "value": [{"message": "Authentication failed", "class": "ERROR"}]}}"#;
        assert_eq!(login_error_reason(body), "Authentication failed");
        let two = r#"{"messages": {"value": [{"message": "A"}, {"x": 1}, {"message": "B"}]}}"#;
        assert_eq!(login_error_reason(two), "A ; B");
    }

    #[test]
    fn error_summary_is_the_fallback() {
        let body = r#"{"errorSummary": "Bad request", "stateHandle": "SECRET-TOKEN"}"#;
        assert_eq!(login_error_reason(body), "Bad request");
    }

    #[test]
    fn neutral_text_without_reason() {
        for body in [
            "{}",
            "[]",
            "\"texte\"",
            "null",
            "<html>SECRET-TOKEN</html>",
            "",
        ] {
            assert_eq!(login_error_reason(body), "aucun motif donné", "{body}");
        }
    }

    #[test]
    fn long_reason_is_cut() {
        let body = json!({"errorSummary": "x".repeat(1000)}).to_string();
        assert_eq!(login_error_reason(&body).chars().count(), MAX_REASON_CHARS);
    }

    #[test]
    fn scrub_removes_every_secret() {
        let reason =
            "Utilisateur jean@example.org inconnu (jeton 02.id.H, mdp hunter2) jean@example.org";
        let clean = scrub(reason, &["jean@example.org", "hunter2", "02.id.H", ""]);
        assert_eq!(clean, "Utilisateur *** inconnu (jeton ***, mdp ***) ***");
    }

    #[test]
    fn refusal_error_holds_no_secret() {
        let body = json!({
            "stateHandle": "02.id.HANDLE",
            "messages": {"value": [{"message": "Compte jean@example.org bloqué"}]}
        })
        .to_string();
        let e = refused(
            "mot de passe",
            401,
            &body,
            &["jean@example.org", "pw", "02.id.HANDLE"],
        );
        let shown = format!("{e} {e:?}");
        assert!(
            !shown.contains("jean@") && !shown.contains("HANDLE"),
            "{shown}"
        );
        assert!(shown.contains("statut HTTP 401"), "{shown}");
        assert!(shown.contains("Compte *** bloqué"), "{shown}");
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

//! Session de l'API WebSocket de Home Assistant.
//!
//! `auth_required` → `auth` → `auth_ok` (version de HA retenue) ou
//! `auth_invalid` ; puis commandes numérotées par `id` croissant, dont on
//! attend la réponse `result` de même `id` en ignorant les autres messages.
//! Une réponse `success: false` devient une [`Error::HomeAssistant`] qui
//! cite la commande, le message et le code d'erreur de HA, raccourcis ; le
//! contexte (cible, statistique) est ajouté par `stats`.
//!
//! Le jeton n'est lu en clair ([`Secret::expose`]) que pour construire le
//! message `auth`, qui n'est jamais journalisé ; tout texte renvoyé par HA
//! pendant l'authentification en est expurgé ([`secret::scrub`]) avant
//! d'entrer dans une erreur.
//!
//! La session ne connaît du WebSocket que le trait [`Transport`] : elle se
//! teste avec un transport en mémoire.
//!
//! Dépendances : `ha::websocket`, `secret`, `error`, `log`, `serde_json`.

use std::io::{Read, Write};
use std::time::Instant;

use serde_json::{Value, json};

use crate::error::Error;
use crate::ha::websocket::WebSocket;
use crate::secret::{self, Secret};

/// Longueur maximale d'un texte de Home Assistant recopié dans une erreur.
const MAX_TEXT_CHARS: usize = 200;
/// Longueur maximale d'un code d'erreur de Home Assistant recopié.
const MAX_CODE_CHARS: usize = 64;

/// Échange de messages texte : ce que la session demande au WebSocket.
pub trait Transport {
    fn send_text(&mut self, text: &str) -> Result<(), Error>;
    /// Prochain message ; `None` si le pair a fermé la connexion.
    fn recv_text(&mut self) -> Result<Option<String>, Error>;
    fn close(&mut self) -> Result<(), Error>;
}

impl<S: Read + Write> Transport for WebSocket<S> {
    fn send_text(&mut self, text: &str) -> Result<(), Error> {
        WebSocket::send_text(self, text)
    }

    fn recv_text(&mut self) -> Result<Option<String>, Error> {
        WebSocket::recv_text(self)
    }

    fn close(&mut self) -> Result<(), Error> {
        WebSocket::close(self)
    }
}

/// Session authentifiée auprès de Home Assistant.
pub struct Session<T: Transport> {
    transport: T,
    next_id: u64,
    ha_version: String,
}

impl<T: Transport> Session<T> {
    /// Joue la phase d'authentification sur un transport ouvert.
    pub fn authenticate(mut transport: T, token: &Secret) -> Result<Self, Error> {
        let hello = receive(&mut transport)?;
        if message_type(&hello) != "auth_required" {
            return Err(unexpected("auth_required", &hello, token));
        }
        let auth = json!({ "type": "auth", "access_token": token.expose() });
        transport.send_text(&auth.to_string())?;
        let reply = receive(&mut transport)?;
        match message_type(&reply) {
            "auth_ok" => {
                let ha_version = reply
                    .get("ha_version")
                    .and_then(Value::as_str)
                    .unwrap_or("inconnue")
                    .to_owned();
                Ok(Session {
                    transport,
                    next_id: 1,
                    ha_version,
                })
            }
            "auth_invalid" => {
                let why = reply.get("message").and_then(Value::as_str);
                Err(Error::HomeAssistant(format!(
                    "jeton refusé (« {} ») ; créez un jeton d'accès longue durée (profil de \
                     l'utilisateur dans Home Assistant, onglet Sécurité) et mettez-le dans \
                     HA_TOKEN",
                    scrub(why.unwrap_or("motif non précisé"), token)
                )))
            }
            _ => Err(unexpected("auth_ok ou auth_invalid", &reply, token)),
        }
    }

    /// Version annoncée par Home Assistant dans `auth_ok` (`inconnue` si absente).
    pub fn ha_version(&self) -> &str {
        &self.ha_version
    }

    /// Envoie une commande (objet JSON avec `type`), lui ajoute un `id`
    /// croissant et renvoie le champ `result` de la réponse de même `id`
    /// (`null` s'il est absent).
    pub fn command(&mut self, mut payload: Value) -> Result<Value, Error> {
        let id = self.next_id;
        let kind = message_type(&payload).to_owned();
        let Some(object) = payload.as_object_mut() else {
            return Err(Error::Protocol(
                "commande Home Assistant : objet JSON attendu".into(),
            ));
        };
        object.insert("id".into(), id.into());
        self.next_id += 1;
        let started = Instant::now();
        self.transport.send_text(&payload.to_string())?;
        loop {
            let mut reply = receive(&mut self.transport)?;
            if message_type(&reply) != "result"
                || reply.get("id").and_then(Value::as_u64) != Some(id)
            {
                debug!("Home Assistant : message ignoré en attendant la réponse {id} ({kind})");
                continue;
            }
            let elapsed = started.elapsed().as_millis();
            if reply.get("success").and_then(Value::as_bool) == Some(true) {
                debug!("Home Assistant : {kind} (n° {id}) → succès en {elapsed} ms");
                return Ok(reply.get_mut("result").map(Value::take).unwrap_or_default());
            }
            debug!("Home Assistant : {kind} (n° {id}) → refus en {elapsed} ms");
            let error = &reply["error"];
            let code = error["code"].as_str().unwrap_or("sans code");
            let advice = if code == "unauthorized" {
                " ; l'utilisateur du jeton HA_TOKEN doit être administrateur"
            } else {
                ""
            };
            return Err(Error::HomeAssistant(format!(
                "{kind} refusée par Home Assistant : {} (code {}){advice}",
                secret::scrub(
                    error["message"].as_str().unwrap_or("sans message"),
                    &[],
                    MAX_TEXT_CHARS
                ),
                secret::scrub(code, &[], MAX_CODE_CHARS),
            )));
        }
    }

    /// Ferme la connexion proprement.
    pub fn close(mut self) -> Result<(), Error> {
        self.transport.close()
    }

    #[cfg(test)]
    pub(crate) fn transport(&self) -> &T {
        &self.transport
    }
}

fn receive<T: Transport>(transport: &mut T) -> Result<Value, Error> {
    let text = transport
        .recv_text()?
        .ok_or_else(|| Error::HomeAssistant("connexion fermée par Home Assistant".into()))?;
    // L'erreur de serde_json donne la position, jamais le contenu.
    serde_json::from_str(&text).map_err(|e| Error::Protocol(format!("message JSON invalide ({e})")))
}

fn message_type(message: &Value) -> &str {
    message.get("type").and_then(Value::as_str).unwrap_or("")
}

fn unexpected(expected: &str, message: &Value, token: &Secret) -> Error {
    Error::Protocol(format!(
        "message {expected} attendu, reçu le type {:?}",
        scrub(message_type(message), token)
    ))
}

/// Texte reçu de HA, expurgé du jeton puis raccourci ([`secret::scrub`]).
fn scrub(text: &str, token: &Secret) -> String {
    secret::scrub(text, &[token], MAX_TEXT_CHARS)
}

#[cfg(test)]
pub(crate) mod fake {
    //! Transport en mémoire pour les tests de `session` et `stats` : chaque
    //! message envoyé est relu en JSON, conservé, et reçoit les réponses que
    //! calcule `respond`.

    use super::*;
    use std::collections::VecDeque;

    pub(crate) const VERSION: &str = "2026.9.1";

    type Respond = Box<dyn FnMut(&Value) -> Vec<Value>>;

    pub(crate) struct FakeTransport {
        pub(crate) incoming: VecDeque<String>,
        respond: Respond,
        /// Messages envoyés par la session, dans l'ordre.
        pub(crate) sent: Vec<Value>,
    }

    impl FakeTransport {
        pub(crate) fn new(
            incoming: Vec<Value>,
            respond: impl FnMut(&Value) -> Vec<Value> + 'static,
        ) -> Self {
            FakeTransport {
                incoming: incoming.iter().map(Value::to_string).collect(),
                respond: Box::new(respond),
                sent: Vec::new(),
            }
        }

        /// Home Assistant qui accepte tout jeton puis répond aux commandes
        /// avec `respond`.
        pub(crate) fn ha(mut respond: impl FnMut(&Value) -> Vec<Value> + 'static) -> Self {
            let hello = json!({ "type": "auth_required", "ha_version": VERSION });
            FakeTransport::new(vec![hello], move |msg| {
                if msg["type"] == "auth" {
                    vec![json!({ "type": "auth_ok", "ha_version": VERSION })]
                } else {
                    respond(msg)
                }
            })
        }

        /// Commandes envoyées après le message d'authentification.
        pub(crate) fn commands(&self) -> &[Value] {
            self.sent.get(1..).unwrap_or_default()
        }
    }

    impl Transport for FakeTransport {
        fn send_text(&mut self, text: &str) -> Result<(), Error> {
            let message: Value = serde_json::from_str(text).unwrap();
            let replies = (self.respond)(&message);
            self.incoming.extend(replies.iter().map(Value::to_string));
            self.sent.push(message);
            Ok(())
        }

        fn recv_text(&mut self) -> Result<Option<String>, Error> {
            Ok(self.incoming.pop_front())
        }

        fn close(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }

    /// Réponse de succès à `command`.
    pub(crate) fn ok(command: &Value, result: Value) -> Value {
        json!({ "id": command["id"], "type": "result", "success": true, "result": result })
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeTransport, VERSION, ok};
    use super::*;

    const TOKEN: &str = "eyJhbGciOi.jeton-de-test";

    fn session(respond: impl FnMut(&Value) -> Vec<Value> + 'static) -> Session<FakeTransport> {
        Session::authenticate(FakeTransport::ha(respond), &Secret::new(TOKEN)).unwrap()
    }

    /// Le jeton n'apparaît ni dans l'affichage ni dans le `Debug` de l'erreur.
    #[track_caller]
    fn assert_no_token(e: &Error) {
        let shown = format!("{e} {e:?}");
        assert!(!shown.contains(TOKEN), "{shown}");
        assert!(!shown.contains("jeton-de-test"), "{shown}");
    }

    fn auth_failure(incoming: Vec<Value>, reply: Vec<Value>) -> Error {
        let transport = FakeTransport::new(incoming, move |_| reply.clone());
        match Session::authenticate(transport, &Secret::new(TOKEN)) {
            Err(e) => {
                assert_no_token(&e);
                e
            }
            Ok(_) => panic!("authentification acceptée à tort"),
        }
    }

    fn required() -> Vec<Value> {
        vec![json!({ "type": "auth_required", "ha_version": VERSION })]
    }

    // --- Authentification ---

    #[test]
    fn auth_sequence_sends_token_and_keeps_version() {
        let s = session(|_| vec![]);
        assert_eq!(s.ha_version(), "2026.9.1");
        assert_eq!(
            s.transport().sent,
            [json!({ "type": "auth", "access_token": TOKEN })]
        );
    }

    #[test]
    fn auth_ok_without_version() {
        let transport = FakeTransport::new(required(), |_| vec![json!({ "type": "auth_ok" })]);
        let s = Session::authenticate(transport, &Secret::new(TOKEN)).unwrap();
        assert_eq!(s.ha_version(), "inconnue");
    }

    #[test]
    fn auth_invalid_is_home_assistant_error_without_token() {
        let reply =
            json!({ "type": "auth_invalid", "message": "Invalid access token or password" });
        let e = auth_failure(required(), vec![reply]);
        assert_eq!(
            e,
            Error::HomeAssistant(
                "jeton refusé (« Invalid access token or password ») ; créez un jeton d'accès \
                 longue durée (profil de l'utilisateur dans Home Assistant, onglet Sécurité) et \
                 mettez-le dans HA_TOKEN"
                    .into()
            )
        );
        // Même si HA renvoyait le jeton dans son message, il est masqué.
        let reply = json!({ "type": "auth_invalid", "message": format!("jeton {TOKEN} refusé") });
        let e = auth_failure(required(), vec![reply]);
        assert!(
            e.to_string()
                .starts_with("jeton refusé (« jeton *** refusé ») ; "),
            "{e}"
        );
        let e = auth_failure(required(), vec![json!({ "type": "auth_invalid" })]);
        assert!(e.to_string().contains("motif non précisé"), "{e}");
    }

    #[test]
    fn auth_unexpected_messages() {
        let e = auth_failure(vec![json!({ "type": "auth_ok" })], vec![]);
        assert!(
            matches!(&e, Error::Protocol(m) if m.contains("auth_required attendu")),
            "{e}"
        );
        let e = auth_failure(required(), vec![json!({ "type": TOKEN })]);
        assert!(matches!(&e, Error::Protocol(m) if m.contains("***")), "{e}");
        let e = auth_failure(required(), vec![]);
        assert_eq!(
            e,
            Error::HomeAssistant("connexion fermée par Home Assistant".into())
        );
        let e = auth_failure(vec![], vec![]);
        assert!(matches!(e, Error::HomeAssistant(_)));
    }

    #[test]
    fn non_json_message_is_protocol_error() {
        let mut transport = FakeTransport::new(vec![], |_| vec![]);
        transport.incoming.push_back(format!("pas du JSON {TOKEN}"));
        match Session::authenticate(transport, &Secret::new(TOKEN)) {
            Err(e) => {
                assert!(
                    matches!(&e, Error::Protocol(m) if m.contains("JSON invalide")),
                    "{e}"
                );
                assert_no_token(&e);
            }
            Ok(_) => panic!("message invalide accepté"),
        }
    }

    // --- Commandes ---

    #[test]
    fn commands_are_numbered_from_one() {
        let mut s = session(|msg| vec![ok(msg, json!(msg["id"]))]);
        for expected in 1..=3 {
            let result = s.command(json!({ "type": "ping" })).unwrap();
            assert_eq!(result, json!(expected));
        }
        let ids: Vec<_> = s
            .transport()
            .commands()
            .iter()
            .map(|m| m["id"].clone())
            .collect();
        assert_eq!(ids, [json!(1), json!(2), json!(3)]);
        assert_eq!(
            s.transport().commands()[0],
            json!({ "type": "ping", "id": 1 })
        );
    }

    #[test]
    fn waits_for_result_with_same_id_ignoring_others() {
        let mut s = session(|msg| {
            let id = msg["id"].as_u64().unwrap();
            vec![
                json!({ "type": "event", "id": id, "event": {} }),
                json!({ "type": "result", "id": id + 100, "success": true, "result": "autre" }),
                json!({ "type": "pong", "id": id }),
                ok(msg, json!({ "bon": true })),
            ]
        });
        assert_eq!(
            s.command(json!({ "type": "x" })).unwrap(),
            json!({ "bon": true })
        );
    }

    #[test]
    fn success_without_result_is_null() {
        let mut s =
            session(|msg| vec![json!({ "type": "result", "id": msg["id"], "success": true })]);
        assert_eq!(s.command(json!({ "type": "x" })).unwrap(), Value::Null);
    }

    #[test]
    fn error_response_carries_code_and_message() {
        let mut s = session(|msg| {
            vec![json!({
                "type": "result", "id": msg["id"], "success": false,
                "error": { "code": "invalid_format", "message": "Invalid statistic_id" }
            })]
        });
        let e = s
            .command(json!({ "type": "recorder/import_statistics" }))
            .unwrap_err();
        assert_eq!(
            e,
            Error::HomeAssistant(
                "recorder/import_statistics refusée par Home Assistant : Invalid statistic_id \
                 (code invalid_format)"
                    .into()
            )
        );
        let mut s =
            session(|msg| vec![json!({ "type": "result", "id": msg["id"], "success": false })]);
        let e = s.command(json!({ "type": "x" })).unwrap_err();
        assert_eq!(
            e,
            Error::HomeAssistant(
                "x refusée par Home Assistant : sans message (code sans code)".into()
            )
        );
    }

    #[test]
    fn refusal_text_is_shortened_and_admin_rights_are_suggested() {
        let mut s = session(|msg| {
            vec![json!({
                "type": "result", "id": msg["id"], "success": false,
                "error": { "code": "unauthorized", "message": "x".repeat(1000) }
            })]
        });
        let e = s
            .command(json!({ "type": "recorder/clear_statistics" }))
            .unwrap_err();
        let shown = e.to_string();
        assert!(shown.len() < 400, "{shown}");
        assert!(
            shown.ends_with(
                " (code unauthorized) ; l'utilisateur du jeton HA_TOKEN doit être administrateur"
            ),
            "{shown}"
        );
    }

    #[test]
    fn closed_before_response() {
        let mut s = session(|_| vec![]);
        let e = s.command(json!({ "type": "x" })).unwrap_err();
        assert_eq!(
            e,
            Error::HomeAssistant("connexion fermée par Home Assistant".into())
        );
    }

    #[test]
    fn command_must_be_an_object() {
        let mut s = session(|_| vec![]);
        assert!(matches!(s.command(json!([1])), Err(Error::Protocol(_))));
        assert!(s.transport().commands().is_empty(), "rien ne doit partir");
        // L'id refusé n'est pas consommé.
        let mut s = session(|msg| vec![ok(msg, Value::Null)]);
        let _ = s.command(json!("x"));
        s.command(json!({ "type": "x" })).unwrap();
        assert_eq!(s.transport().commands()[0]["id"], 1);
    }
}

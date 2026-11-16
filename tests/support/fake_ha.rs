//! Faux Home Assistant : serveur WebSocket sur `127.0.0.1` (port choisi par
//! le système), un thread, une connexion à la fois.
//!
//! Il joue l'authentification (`auth_required`, puis `auth_ok` ou
//! `auth_invalid` selon le jeton) et répond à `get_config` (fuseau
//! [`TIME_ZONE`] par défaut, réglable), `recorder/statistics_during_period`,
//! `recorder/import_statistics` et `recorder/clear_statistics` à partir d'un
//! état en mémoire, consultable par [`FakeHa::state`].
//!
//! Comportements de HA imités : `start` rendu en millisecondes Unix ; import
//! qui remplace le point de même `start` ; refus d'une `source` qui ne
//! correspond pas à l'identifiant, d'une métadonnée manquante, d'un `start`
//! sans décalage ou qui n'est pas une heure pile ; identifiant sans données
//! absent de la réponse ; seuls les `types` demandés sont rendus.
//!
//! Ce n'est pas HA : les points sont rendus tels qu'importés, sans le
//! regroupement par jour dans le fuseau de HA, et l'import est immédiat
//! alors que le vrai recorder le traite en file d'attente.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use ha_gazpar::Error;
use ha_gazpar::ha::websocket::{Role, WebSocket, accept_key};
use jiff::Timestamp;
use serde_json::{Map, Value, json};

/// Version annoncée dans `auth_ok`.
pub const VERSION: &str = "2026.9.1";
/// Fuseau rendu par `get_config`, sauf [`FakeHa::start_with_time_zone`].
pub const TIME_ZONE: &str = "Europe/Paris";

/// Point stocké.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Row {
    pub state: f64,
    pub sum: f64,
}

/// État en mémoire du faux HA.
#[derive(Debug, Default)]
pub struct State {
    /// Points par identifiant, puis par début en millisecondes Unix.
    pub series: BTreeMap<String, BTreeMap<i64, Row>>,
    /// Dernières métadonnées importées, par identifiant.
    pub metadata: BTreeMap<String, Value>,
    /// Commandes reçues après authentification, dans l'ordre.
    pub commands: Vec<Value>,
    /// Fuseau rendu par `get_config`.
    pub time_zone: String,
    /// Nombre d'authentifications refusées.
    pub rejected_auths: usize,
    /// Connexions TCP reçues (poignée de main comprise ou non).
    pub connections: usize,
}

impl State {
    /// Nombre de commandes reçues de ce type.
    pub fn count(&self, kind: &str) -> usize {
        self.commands.iter().filter(|c| c["type"] == kind).count()
    }
}

/// Serveur en marche ; arrêté quand la valeur est abandonnée.
pub struct FakeHa {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeHa {
    /// Démarre un faux HA qui n'accepte que `token`, dans le fuseau [`TIME_ZONE`].
    pub fn start(token: &str) -> FakeHa {
        FakeHa::start_with_time_zone(token, TIME_ZONE)
    }

    /// Démarre un faux HA dont `get_config` rend le fuseau `time_zone`.
    pub fn start_with_time_zone(token: &str, time_zone: &str) -> FakeHa {
        let listener = TcpListener::bind("127.0.0.1:0").expect("écoute sur 127.0.0.1");
        let addr = listener.local_addr().expect("adresse locale");
        let state = Arc::new(Mutex::new(State {
            time_zone: time_zone.to_owned(),
            ..State::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (state, stop, token) = (Arc::clone(&state), Arc::clone(&stop), token.to_owned());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        state.lock().expect("état").connections += 1;
                        // Une connexion en erreur n'arrête pas le serveur.
                        let _ = serve(stream, &token, &state);
                    }
                }
            })
        };
        FakeHa {
            addr,
            state,
            stop,
            thread: Some(thread),
        }
    }

    /// URL à donner au client.
    pub fn url(&self) -> String {
        format!("ws://{}/api/websocket", self.addr)
    }

    /// État courant (verrouillé tant que la garde vit).
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("état du faux HA")
    }
}

impl Drop for FakeHa {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Réveille accept() pour que le thread voie l'arrêt.
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Sert une connexion jusqu'à sa fermeture.
fn serve(mut stream: TcpStream, token: &str, state: &Mutex<State>) -> Result<(), Error> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let key = read_request(&mut stream)?;
    write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(&key)
    )?;
    let mut ws = WebSocket::new(stream, Role::Server);
    send(
        &mut ws,
        json!({ "type": "auth_required", "ha_version": VERSION }),
    )?;
    let Some(auth) = ws.recv_text()? else {
        return Ok(());
    };
    let auth = parse(&auth)?;
    if auth["type"] != "auth" || auth["access_token"] != token {
        state.lock().expect("état").rejected_auths += 1;
        send(
            &mut ws,
            json!({ "type": "auth_invalid", "message": "Invalid access token or password" }),
        )?;
        return ws.close();
    }
    send(&mut ws, json!({ "type": "auth_ok", "ha_version": VERSION }))?;
    while let Some(text) = ws.recv_text()? {
        let command = parse(&text)?;
        let reply = {
            let mut state = state.lock().expect("état");
            state.commands.push(command.clone());
            respond(&command, &mut state)
        };
        send(&mut ws, reply)?;
    }
    Ok(())
}

/// Lit la requête de poignée de main et renvoie `Sec-WebSocket-Key`.
fn read_request(stream: &mut TcpStream) -> Result<String, Error> {
    let invalid = || Error::Protocol("requête de poignée de main invalide".into());
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 || stream.read(&mut byte)? == 0 {
            return Err(invalid());
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    if !head.starts_with("GET /api/websocket HTTP/1.1\r\n") {
        return Err(invalid());
    }
    head.split("\r\n")
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("sec-websocket-key")
                .then(|| value.trim().to_owned())
        })
        .ok_or_else(invalid)
}

fn send<S: Read + Write>(ws: &mut WebSocket<S>, message: Value) -> Result<(), Error> {
    ws.send_text(&message.to_string())
}

fn parse(text: &str) -> Result<Value, Error> {
    serde_json::from_str(text).map_err(|e| Error::Protocol(e.to_string()))
}

/// Erreur au sens de HA : code et message.
type Failure = (&'static str, String);

fn respond(command: &Value, state: &mut State) -> Value {
    let outcome = match command["type"].as_str().unwrap_or_default() {
        "get_config" => Ok(get_config(state)),
        "recorder/statistics_during_period" => during_period(command, state),
        "recorder/import_statistics" => import(command, state),
        "recorder/clear_statistics" => clear(command, state),
        _ => Err(("unknown_command", "Unknown command.".to_owned())),
    };
    match outcome {
        Ok(result) => {
            json!({ "id": command["id"], "type": "result", "success": true, "result": result })
        }
        Err((code, message)) => json!({
            "id": command["id"], "type": "result", "success": false,
            "error": { "code": code, "message": message },
        }),
    }
}

/// Extrait de la réponse de HA à `get_config`.
fn get_config(state: &State) -> Value {
    json!({
        "latitude": 48.8566,
        "longitude": 2.3522,
        "elevation": 35,
        "unit_system": { "length": "km", "mass": "g", "temperature": "°C", "volume": "L" },
        "location_name": "Maison",
        "time_zone": state.time_zone,
        "version": VERSION,
        "state": "RUNNING",
    })
}

fn during_period(command: &Value, state: &State) -> Result<Value, Failure> {
    let start = millis(&command["start_time"])?;
    let end = millis(&command["end_time"])?;
    if command["period"] != "day" {
        return Err(invalid("ce faux HA ne sert que period: day"));
    }
    let types = command["types"]
        .as_array()
        .ok_or_else(|| invalid("types"))?;
    let wanted = |t: &str| types.iter().any(|x| x == t);
    let ids = command["statistic_ids"]
        .as_array()
        .ok_or_else(|| invalid("statistic_ids"))?;
    let mut result = Map::new();
    for id in ids {
        let id = id.as_str().ok_or_else(|| invalid("statistic_ids"))?;
        let rows: Vec<Value> = state
            .series
            .get(id)
            .into_iter()
            .flat_map(|series| series.range(start..end))
            .map(|(&ms, row)| {
                // `end` approximatif (jours de 23 ou 25 h), non lu par le client.
                let mut out = json!({ "start": ms, "end": ms + 86_400_000 });
                if wanted("state") {
                    out["state"] = json!(row.state);
                }
                if wanted("sum") {
                    out["sum"] = json!(row.sum);
                }
                out
            })
            .collect();
        if !rows.is_empty() {
            result.insert(id.to_owned(), Value::Array(rows));
        }
    }
    Ok(Value::Object(result))
}

fn import(command: &Value, state: &mut State) -> Result<Value, Failure> {
    let metadata = &command["metadata"];
    let id = metadata["statistic_id"]
        .as_str()
        .ok_or_else(|| invalid("statistic_id"))?;
    for key in [
        "has_mean",
        "mean_type",
        "has_sum",
        "name",
        "source",
        "unit_class",
        "unit_of_measurement",
    ] {
        if metadata.get(key).is_none() {
            return Err(invalid(&format!("métadonnée {key} manquante")));
        }
    }
    let expected = id.split_once(':').map_or("recorder", |(source, _)| source);
    if metadata["source"] != expected {
        return Err(invalid(&format!("Invalid source for {id}")));
    }
    let mut rows = Vec::new();
    for stat in command["stats"]
        .as_array()
        .ok_or_else(|| invalid("stats"))?
    {
        let ms = millis(&stat["start"])?;
        if ms % 3_600_000 != 0 {
            return Err(invalid(
                "Invalid timestamp: timestamps must be from the top of the hour",
            ));
        }
        let number = |key: &str| stat[key].as_f64().ok_or_else(|| invalid(key));
        rows.push((
            ms,
            Row {
                state: number("state")?,
                sum: number("sum")?,
            },
        ));
    }
    state.metadata.insert(id.to_owned(), metadata.clone());
    state.series.entry(id.to_owned()).or_default().extend(rows);
    Ok(Value::Null)
}

fn clear(command: &Value, state: &mut State) -> Result<Value, Failure> {
    let ids = command["statistic_ids"]
        .as_array()
        .ok_or_else(|| invalid("statistic_ids"))?;
    for id in ids.iter().filter_map(Value::as_str) {
        state.series.remove(id);
        state.metadata.remove(id);
    }
    Ok(Value::Null)
}

/// Date RFC 3339 avec décalage obligatoire, en millisecondes Unix.
fn millis(value: &Value) -> Result<i64, Failure> {
    value
        .as_str()
        .and_then(|s| s.parse::<Timestamp>().ok())
        .map(Timestamp::as_millisecond)
        .ok_or_else(|| invalid(&format!("Invalid datetime {value}")))
}

fn invalid(message: &str) -> Failure {
    ("invalid_format", message.to_owned())
}

//! Client WebSocket RFC 6455 minimal, bibliothèque standard uniquement.
//!
//! `ws://` seulement (`wss://` est refusé par la configuration) : poignée de
//! main HTTP/1.1, trames texte masquées à l'envoi, longueurs 7/16/64 bits,
//! fragmentation, réponse automatique aux `ping`, fermeture. Le pair est le
//! Home Assistant de l'utilisateur, sur son réseau local.
//!
//! Choix et limites :
//!
//! - **`Sec-WebSocket-Accept` est vérifié.** La vérification exige SHA-1,
//!   écrit ici en une quarantaine de lignes et testé sur les vecteurs de la
//!   FIPS 180 et l'exemple de la RFC 6455. Elle coûte peu, écarte un
//!   intermédiaire (mandataire, autre service sur le port) qui répondrait
//!   `101` sans parler WebSocket, et la même fonction [`accept_key`] sert au
//!   faux serveur des tests. SHA-1 n'a ici aucun rôle de sécurité.
//! - **Aléa sans dépendance.** La clé de poignée de main et les masques
//!   viennent de SipHash à clés aléatoires ([`std::hash::RandomState`],
//!   graine tirée du système par processus) appliqué à un compteur. Ce n'est
//!   pas un générateur cryptographique, et le masquage n'en exige pas ici :
//!   il protège les intermédiaires HTTP contre des scripts de navigateur
//!   hostiles, situation absente entre ce démon et son Home Assistant.
//! - Messages binaires refusés (Home Assistant n'envoie que du texte), aucune
//!   extension négociée (bits `RSV` à 1 refusés), taille d'un message reçu
//!   plafonnée à [`MAX_MESSAGE_LEN`], vérifiée avant toute allocation.
//! - La logique de trame est générique sur `Read + Write` et se teste avec des
//!   tampons en mémoire ; seul [`connect`] touche au réseau. [`Role::Server`]
//!   n'existe que pour le faux Home Assistant des tests.
//!
//! Dépendances : `url`, `error` et la bibliothèque standard.

use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{self, BufReader, ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::error::Error;
use crate::url::Url;

/// Taille maximale d'un message reçu, fragments réunis (16 Mio).
pub const MAX_MESSAGE_LEN: usize = 16 * 1024 * 1024;
/// Taille maximale de l'en-tête HTTP de la réponse à la poignée de main.
const MAX_HANDSHAKE_LEN: usize = 16 * 1024;
/// Identifiant fixe de la RFC 6455 §1.3 pour calculer `Sec-WebSocket-Accept`.
const ACCEPT_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Délais réseau. Aucun ne doit être nul (refusé par le système).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// Établissement de la connexion TCP, par adresse essayée.
    pub connect: Duration,
    /// Attente de données reçues, donc aussi d'une réponse de Home Assistant.
    pub read: Duration,
    /// Envoi d'une trame.
    pub write: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(10),
            read: Duration::from_secs(60),
            write: Duration::from_secs(30),
        }
    }
}

/// Côté de la connexion : le client masque ce qu'il envoie et refuse les
/// trames masquées ; le serveur fait l'inverse (RFC 6455 §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Client,
    Server,
}

/// Code d'opération d'une trame (RFC 6455 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpCode {
    Continuation = 0x0,
    Text = 0x1,
    Binary = 0x2,
    Close = 0x8,
    Ping = 0x9,
    Pong = 0xA,
}

impl OpCode {
    fn from_bits(bits: u8) -> Option<OpCode> {
        Some(match bits {
            0x0 => OpCode::Continuation,
            0x1 => OpCode::Text,
            0x2 => OpCode::Binary,
            0x8 => OpCode::Close,
            0x9 => OpCode::Ping,
            0xA => OpCode::Pong,
            _ => return None,
        })
    }

    fn is_control(self) -> bool {
        matches!(self, OpCode::Close | OpCode::Ping | OpCode::Pong)
    }
}

/// Trame reçue, charge utile déjà démasquée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: OpCode,
    pub payload: Vec<u8>,
}

/// Encode une trame ; `mask` est la clé de masquage (obligatoire côté client).
pub fn encode_frame(fin: bool, opcode: OpCode, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let len = payload.len();
    let mut out = Vec::with_capacity(len + 14);
    out.push(u8::from(fin) << 7 | opcode as u8);
    let mask_bit = if mask.is_some() { 0x80 } else { 0 };
    // Les conversions `as` sont sans perte : chaque bras borne `len`.
    match len {
        0..=125 => out.push(mask_bit | len as u8),
        126..=0xFFFF => {
            out.push(mask_bit | 126);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        }
        _ => {
            out.push(mask_bit | 127);
            out.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    if let Some(key) = mask {
        out.extend_from_slice(&key);
    }
    let start = out.len();
    out.extend_from_slice(payload);
    if let Some(key) = mask {
        apply_mask(&mut out[start..], key);
    }
    out
}

/// Lit une trame. `max_len` borne sa charge utile, avant toute allocation.
pub fn read_frame<R: Read>(r: &mut R, role: Role, max_len: usize) -> Result<Frame, Error> {
    let mut head = [0u8; 2];
    if !read_first_byte(r, &mut head[0])? {
        return Err(Error::Io(
            "connexion WebSocket fermée par le pair sans trame close".into(),
        ));
    }
    read_part(r, &mut head[1..])?;
    if head[0] & 0x70 != 0 {
        return Err(protocol("bits réservés à 1 (aucune extension négociée)"));
    }
    let fin = head[0] & 0x80 != 0;
    let opcode = OpCode::from_bits(head[0] & 0x0F)
        .ok_or_else(|| protocol(format!("code d'opération inconnu {:#x}", head[0] & 0x0F)))?;
    let masked = head[1] & 0x80 != 0;
    if masked != (role == Role::Server) {
        return Err(protocol(if masked {
            "trame masquée reçue du serveur"
        } else {
            "trame non masquée reçue du client"
        }));
    }
    let len = match head[1] & 0x7F {
        126 => {
            let mut b = [0u8; 2];
            read_part(r, &mut b)?;
            u64::from(u16::from_be_bytes(b))
        }
        127 => {
            let mut b = [0u8; 8];
            read_part(r, &mut b)?;
            let len = u64::from_be_bytes(b);
            if len >> 63 != 0 {
                return Err(protocol(
                    "longueur sur 64 bits avec le bit de poids fort à 1",
                ));
            }
            len
        }
        n => u64::from(n),
    };
    if opcode.is_control() && (!fin || len > 125) {
        return Err(protocol(
            "trame de contrôle fragmentée ou de plus de 125 octets",
        ));
    }
    let len = usize::try_from(len)
        .ok()
        .filter(|&l| l <= max_len)
        .ok_or_else(|| protocol(format!("message de plus de {max_len} octets")))?;
    let mut key = [0u8; 4];
    if masked {
        read_part(r, &mut key)?;
    }
    let mut payload = vec![0u8; len];
    read_part(r, &mut payload)?;
    if masked {
        apply_mask(&mut payload, key);
    }
    Ok(Frame {
        fin,
        opcode,
        payload,
    })
}

/// Connexion WebSocket, poignée de main faite.
pub struct WebSocket<S: Read + Write> {
    io: BufReader<S>,
    role: Role,
    max_message: usize,
    noise: Noise,
    closed: bool,
}

impl<S: Read + Write> WebSocket<S> {
    /// Enveloppe un flux dont la poignée de main est déjà faite.
    pub fn new(stream: S, role: Role) -> Self {
        WebSocket {
            io: BufReader::new(stream),
            role,
            max_message: MAX_MESSAGE_LEN,
            noise: Noise::new(),
            closed: false,
        }
    }

    /// Change le plafond de taille d'un message reçu.
    pub fn with_max_message(mut self, max: usize) -> Self {
        self.max_message = max;
        self
    }

    /// Envoie un message texte en une seule trame.
    pub fn send_text(&mut self, text: &str) -> Result<(), Error> {
        if self.closed {
            return Err(protocol("connexion WebSocket déjà fermée"));
        }
        self.send_frame(OpCode::Text, text.as_bytes())
    }

    /// Reçoit le prochain message texte, fragments réunis, en répondant aux
    /// `ping` en chemin. `None` : le pair a fermé la connexion (trame close,
    /// à laquelle on a répondu).
    pub fn recv_text(&mut self) -> Result<Option<String>, Error> {
        let mut partial: Option<Vec<u8>> = None;
        while !self.closed {
            let limit = self.max_message - partial.as_ref().map_or(0, Vec::len);
            let frame = read_frame(&mut self.io, self.role, limit)?;
            match (frame.opcode, partial.as_mut()) {
                (OpCode::Ping, _) => self.send_frame(OpCode::Pong, &frame.payload)?,
                (OpCode::Pong, _) => {}
                (OpCode::Close, _) => {
                    if frame.payload.len() == 1 {
                        return Err(protocol("trame close de 1 octet"));
                    }
                    self.closed = true;
                    // Écho du code d'état ; le pair a pu déjà couper : erreur ignorée.
                    let code = &frame.payload[..frame.payload.len().min(2)];
                    let _ = self.send_frame(OpCode::Close, code);
                }
                (OpCode::Binary, _) => return Err(protocol("message binaire inattendu")),
                (OpCode::Text, Some(_)) => {
                    return Err(protocol("nouveau message avant la fin du précédent"));
                }
                (OpCode::Text, None) if frame.fin => return utf8(frame.payload).map(Some),
                (OpCode::Text, None) => partial = Some(frame.payload),
                (OpCode::Continuation, None) => {
                    return Err(protocol("trame de continuation sans début de message"));
                }
                (OpCode::Continuation, Some(buf)) => {
                    buf.extend_from_slice(&frame.payload);
                    if frame.fin {
                        return utf8(std::mem::take(buf)).map(Some);
                    }
                }
            }
        }
        Ok(None)
    }

    /// Ferme proprement : envoie close (1000) puis attend l'écho du pair en
    /// ignorant les messages en vol. Sans écho (coupure, délai), rien à faire.
    pub fn close(&mut self) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.send_frame(OpCode::Close, &1000u16.to_be_bytes())?;
        for _ in 0..64 {
            match read_frame(&mut self.io, self.role, self.max_message) {
                Ok(f) if f.opcode != OpCode::Close => {}
                _ => break,
            }
        }
        Ok(())
    }

    fn send_frame(&mut self, opcode: OpCode, payload: &[u8]) -> Result<(), Error> {
        let mask = match self.role {
            Role::Client => Some(self.noise.mask()),
            Role::Server => None,
        };
        let bytes = encode_frame(true, opcode, payload, mask);
        let w = self.io.get_mut();
        w.write_all(&bytes)
            .and_then(|()| w.flush())
            .map_err(io_error)
    }

    /// Poignée de main côté client, avec une clé donnée.
    fn handshake(&mut self, host: &str, path: &str, key: &str) -> Result<(), Error> {
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        let w = self.io.get_mut();
        w.write_all(request.as_bytes())
            .and_then(|()| w.flush())
            .map_err(io_error)?;
        // Octet par octet depuis le tampon : la première trame peut suivre.
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() >= MAX_HANDSHAKE_LEN {
                return Err(protocol("réponse de poignée de main trop longue"));
            }
            match self.io.read(&mut byte) {
                Ok(0) => return Err(protocol("connexion fermée pendant la poignée de main")),
                Ok(_) => head.push(byte[0]),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(io_error(e)),
            }
        }
        check_handshake(&String::from_utf8_lossy(&head), key)
    }

    #[cfg(test)]
    fn stream(&self) -> &S {
        self.io.get_ref()
    }
}

/// Ouvre une connexion WebSocket vers `ws://hôte[:port][/chemin]`.
pub fn connect(url: &str, timeouts: Timeouts) -> Result<WebSocket<TcpStream>, Error> {
    let target = Target::new(Url::ws(url)?);
    let addrs = (target.socket_host(), target.port())
        .to_socket_addrs()
        .map_err(|e| Error::Io(format!("résolution de {} : {e}", target.socket_host())))?;
    let mut last_error = String::from("aucune adresse");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeouts.connect) {
            Ok(stream) => {
                stream.set_read_timeout(Some(timeouts.read))?;
                stream.set_write_timeout(Some(timeouts.write))?;
                // Petites trames : inutile d'attendre l'algorithme de Nagle.
                let _ = stream.set_nodelay(true);
                let mut ws = WebSocket::new(stream, Role::Client);
                let nonce = [ws.noise.next_u64(), ws.noise.next_u64()].map(u64::to_le_bytes);
                ws.handshake(
                    &target.0.authority(),
                    &target.0.path_and_query(),
                    &base64(&nonce.concat()),
                )?;
                return Ok(ws);
            }
            Err(e) => last_error = io_error(e).to_string(),
        }
    }
    Err(Error::Io(format!(
        "connexion à {}:{} impossible : {last_error}",
        target.socket_host(),
        target.port()
    )))
}

/// Valeur de `Sec-WebSocket-Accept` attendue pour une clé (RFC 6455 §4.2.2).
pub fn accept_key(key: &str) -> String {
    base64(&sha1(format!("{key}{ACCEPT_GUID}").as_bytes()))
}

/// Vue de connexion d'une adresse `ws://` analysée par [`crate::url`] :
/// hôte pour le système (sans crochets), port (80 par défaut).
struct Target(Url);

impl Target {
    fn new(url: Url) -> Target {
        Target(url)
    }

    /// Hôte à résoudre : une adresse IPv6 perd ses crochets.
    fn socket_host(&self) -> &str {
        let host = self.0.host();
        host.strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host)
    }

    fn port(&self) -> u16 {
        self.0.port_or_default()
    }
}

/// Vérifie l'en-tête complet de la réponse à la poignée de main.
fn check_handshake(head: &str, key: &str) -> Result<(), Error> {
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    if !status.starts_with("HTTP/1.1 101") {
        let shown: String = status.chars().take(80).collect();
        return Err(protocol(format!("poignée de main refusée : {shown:?}")));
    }
    let header = |name: &str| {
        lines.clone().find_map(|l| {
            let (n, v) = l.split_once(':')?;
            n.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
    };
    let upgrade = header("upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let connection = header("connection").is_some_and(|v| {
        v.split(',')
            .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
    });
    if !upgrade || !connection {
        return Err(protocol(
            "poignée de main sans en-têtes Upgrade/Connection WebSocket",
        ));
    }
    if header("sec-websocket-accept") != Some(accept_key(key).as_str()) {
        return Err(protocol("Sec-WebSocket-Accept absent ou incorrect"));
    }
    Ok(())
}

/// Générateur non cryptographique (voir le commentaire du module).
struct Noise {
    keys: RandomState,
    counter: u64,
}

impl Noise {
    fn new() -> Self {
        Noise {
            keys: RandomState::new(),
            counter: 0,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.counter = self.counter.wrapping_add(1);
        let mut h = self.keys.build_hasher();
        h.write_u64(self.counter);
        h.finish()
    }

    fn mask(&mut self) -> [u8; 4] {
        let [a, b, c, d, ..] = self.next_u64().to_le_bytes();
        [a, b, c, d]
    }
}

fn apply_mask(data: &mut [u8], key: [u8; 4]) {
    for (byte, k) in data.iter_mut().zip(key.iter().cycle()) {
        *byte ^= k;
    }
}

/// Lit le premier octet d'une trame ; `false` si le flux finit avant.
fn read_first_byte<R: Read>(r: &mut R, byte: &mut u8) -> Result<bool, Error> {
    let mut buf = [0u8; 1];
    loop {
        match r.read(&mut buf) {
            Ok(0) => return Ok(false),
            Ok(_) => {
                *byte = buf[0];
                return Ok(true);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(io_error(e)),
        }
    }
}

/// Lit la suite d'une trame commencée : une fin de flux la rend tronquée.
fn read_part<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<(), Error> {
    r.read_exact(buf).map_err(|e| match e.kind() {
        ErrorKind::UnexpectedEof => protocol("trame tronquée"),
        _ => io_error(e),
    })
}

fn io_error(e: io::Error) -> Error {
    match e.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => {
            Error::Io("délai dépassé sur la connexion WebSocket".into())
        }
        _ => Error::Io(format!("connexion WebSocket : {e}")),
    }
}

fn protocol(message: impl Into<String>) -> Error {
    Error::Protocol(format!("WebSocket : {}", message.into()))
}

fn utf8(bytes: Vec<u8>) -> Result<String, Error> {
    String::from_utf8(bytes).map_err(|_| protocol("message texte en UTF-8 invalide"))
}

/// Base64 standard avec remplissage (RFC 4648 §4), encodage seul.
fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let byte = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = byte(0) << 16 | byte(1) << 8 | byte(2);
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63])
            } else {
                '='
            });
        }
    }
    out
}

/// SHA-1 (FIPS 180-4), utilisé uniquement pour `Sec-WebSocket-Accept`.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (hv, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *hv = hv.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (bytes, v) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *bytes = v.to_be_bytes();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::TcpListener;

    /// Flux en mémoire : lit `input`, accumule ce qui est écrit dans `output`.
    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn duplex(input: Vec<u8>) -> Duplex {
        Duplex {
            input: Cursor::new(input),
            output: Vec::new(),
        }
    }

    /// Client qui reçoit `input` (trames du serveur).
    fn client(input: Vec<u8>) -> WebSocket<Duplex> {
        WebSocket::new(duplex(input), Role::Client)
    }

    /// Trame telle que l'envoie un serveur (non masquée).
    fn server_frame(fin: bool, opcode: OpCode, payload: &[u8]) -> Vec<u8> {
        encode_frame(fin, opcode, payload, None)
    }

    fn frame(opcode: OpCode, payload: &[u8]) -> Frame {
        Frame {
            fin: true,
            opcode,
            payload: payload.to_vec(),
        }
    }

    /// Trames écrites par le client, démasquées.
    fn sent_frames(ws: &WebSocket<Duplex>) -> Vec<Frame> {
        let out = &ws.stream().output;
        let mut r = Cursor::new(out.as_slice());
        let mut frames = Vec::new();
        while (r.position() as usize) < out.len() {
            frames.push(read_frame(&mut r, Role::Server, usize::MAX).unwrap());
        }
        frames
    }

    fn read_as_client(bytes: &[u8]) -> Result<Frame, Error> {
        read_frame(&mut Cursor::new(bytes), Role::Client, MAX_MESSAGE_LEN)
    }

    #[track_caller]
    fn assert_protocol<T: std::fmt::Debug>(result: Result<T, Error>, needle: &str) {
        match result {
            Err(Error::Protocol(m)) => assert!(m.contains(needle), "{m:?} sans {needle:?}"),
            other => panic!("erreur de protocole attendue ({needle}), reçu {other:?}"),
        }
    }

    // --- Exemples de la RFC 6455 §5.7 ---

    #[test]
    fn rfc_single_frame_unmasked_text() {
        let f = read_as_client(&[0x81, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f]).unwrap();
        assert_eq!(f, frame(OpCode::Text, b"Hello"));
    }

    #[test]
    fn rfc_single_frame_masked_text() {
        let bytes = [
            0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
        ];
        let key = Some([0x37, 0xfa, 0x21, 0x3d]);
        assert_eq!(encode_frame(true, OpCode::Text, b"Hello", key), bytes);
        let f = read_frame(&mut &bytes[..], Role::Server, 100).unwrap();
        assert_eq!(f, frame(OpCode::Text, b"Hello"));
    }

    #[test]
    fn rfc_fragmented_unmasked_text() {
        let input = [
            &[0x01, 0x03, 0x48, 0x65, 0x6c][..],
            &[0x80, 0x02, 0x6c, 0x6f],
        ]
        .concat();
        assert_eq!(client(input).recv_text().unwrap().as_deref(), Some("Hello"));
    }

    #[test]
    fn rfc_ping_gets_masked_pong_with_same_payload() {
        let ping = [0x89, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f];
        let input = [&ping[..], &server_frame(true, OpCode::Text, b"ok")].concat();
        let mut ws = client(input);
        assert_eq!(ws.recv_text().unwrap().as_deref(), Some("ok"));
        // Pong final, masqué, 5 octets.
        assert_eq!(ws.stream().output[..2], [0x8A, 0x85]);
        assert_eq!(sent_frames(&ws), [frame(OpCode::Pong, b"Hello")]);
    }

    #[test]
    fn rfc_length_headers_256_and_65536() {
        let f = encode_frame(true, OpCode::Binary, &[0; 256], None);
        assert_eq!(f[..4], [0x82, 0x7E, 0x01, 0x00]);
        let f = encode_frame(true, OpCode::Binary, &vec![0; 65536], None);
        assert_eq!(f[..10], [0x82, 0x7F, 0, 0, 0, 0, 0, 1, 0, 0]);
    }

    // --- Longueurs, masquage, fragmentation ---

    #[test]
    fn length_boundaries_round_trip() {
        for (len, header) in [
            (0, 2),
            (125, 2),
            (126, 4),
            (65535, 4),
            (65536, 10),
            (70_000, 10),
        ] {
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let plain = encode_frame(true, OpCode::Text, &payload, None);
            assert_eq!(plain.len(), header + len, "{len}");
            assert_eq!(read_as_client(&plain).unwrap().payload, payload, "{len}");
            let masked = encode_frame(true, OpCode::Text, &payload, Some([1, 2, 3, 4]));
            assert_eq!(masked.len(), header + 4 + len, "{len}");
            let back = read_frame(&mut &masked[..], Role::Server, len).unwrap();
            assert_eq!(back.payload, payload, "{len}");
        }
    }

    #[test]
    fn fragmented_message_over_64_kib_with_ping_in_the_middle() {
        let text: String = (0..100_000u32)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let b = text.as_bytes();
        let input = [
            server_frame(false, OpCode::Text, &b[..30_000]),
            server_frame(false, OpCode::Continuation, &b[30_000..70_000]),
            server_frame(true, OpCode::Ping, b"p"),
            server_frame(true, OpCode::Continuation, &b[70_000..]),
        ]
        .concat();
        let mut ws = client(input);
        assert_eq!(ws.recv_text().unwrap(), Some(text));
        assert_eq!(sent_frames(&ws), [frame(OpCode::Pong, b"p")]);
    }

    #[test]
    fn utf8_split_across_fragments() {
        let input = [
            server_frame(false, OpCode::Text, &[b'h', 0xC3]),
            server_frame(true, OpCode::Continuation, &[0xA9]),
        ]
        .concat();
        assert_eq!(client(input).recv_text().unwrap().as_deref(), Some("hé"));
    }

    #[test]
    fn pong_is_ignored() {
        let input = [
            server_frame(true, OpCode::Pong, b"x"),
            server_frame(true, OpCode::Text, b"ok"),
        ]
        .concat();
        let mut ws = client(input);
        assert_eq!(ws.recv_text().unwrap().as_deref(), Some("ok"));
        assert!(ws.stream().output.is_empty());
    }

    #[test]
    fn client_masks_every_frame_with_a_fresh_key() {
        let mut ws = client(Vec::new());
        ws.send_text("bonjour").unwrap();
        ws.send_text("bonjour").unwrap();
        let out = ws.stream().output.clone();
        assert_eq!(out.len(), 2 * (2 + 4 + 7));
        let (first, second) = out.split_at(13);
        assert_eq!(first[..2], [0x81, 0x80 | 7]);
        assert_ne!(first[2..6], second[2..6], "clé de masquage réutilisée");
        assert_eq!(
            sent_frames(&ws),
            [
                frame(OpCode::Text, b"bonjour"),
                frame(OpCode::Text, b"bonjour")
            ]
        );
    }

    #[test]
    fn server_role_sends_unmasked_and_requires_masked() {
        let input = encode_frame(true, OpCode::Text, b"hi", Some([9, 8, 7, 6]));
        let mut ws = WebSocket::new(duplex(input), Role::Server);
        assert_eq!(ws.recv_text().unwrap().as_deref(), Some("hi"));
        ws.send_text("x").unwrap();
        assert_eq!(ws.stream().output, [0x81, 0x01, b'x']);
        let unmasked = [0x81, 0x00];
        assert_protocol(
            read_frame(&mut &unmasked[..], Role::Server, 10),
            "non masquée reçue du client",
        );
    }

    // --- Fermeture ---

    #[test]
    fn close_is_answered_then_reported() {
        let code = 1001u16.to_be_bytes();
        let mut ws = client(server_frame(true, OpCode::Close, &code));
        assert_eq!(ws.recv_text().unwrap(), None);
        assert_eq!(ws.recv_text().unwrap(), None);
        assert_eq!(sent_frames(&ws), [frame(OpCode::Close, &code)]);
        assert_protocol(ws.send_text("x"), "déjà fermée");
        ws.close().unwrap();
        assert_eq!(sent_frames(&ws).len(), 1, "rien de plus après la fermeture");
    }

    #[test]
    fn close_without_status_and_invalid_close() {
        let mut ws = client(server_frame(true, OpCode::Close, &[]));
        assert_eq!(ws.recv_text().unwrap(), None);
        assert_eq!(sent_frames(&ws), [frame(OpCode::Close, &[])]);
        let mut ws = client(server_frame(true, OpCode::Close, &[3]));
        assert_protocol(ws.recv_text(), "close de 1 octet");
    }

    #[test]
    fn close_sends_1000_and_waits_for_echo() {
        let input = [
            server_frame(true, OpCode::Text, b"en vol"),
            server_frame(true, OpCode::Close, &1000u16.to_be_bytes()),
            server_frame(true, OpCode::Text, b"apres"),
        ]
        .concat();
        let mut ws = client(input);
        ws.close().unwrap();
        assert_eq!(
            sent_frames(&ws),
            [frame(OpCode::Close, &1000u16.to_be_bytes())]
        );
        // Lecture arrêtée à l'écho : la trame suivante n'a pas été consommée.
        let next = read_frame(&mut ws.io, Role::Client, 100).unwrap();
        assert_eq!(next, frame(OpCode::Text, b"apres"));
        assert_protocol(ws.send_text("x"), "déjà fermée");
    }

    #[test]
    fn close_tolerates_missing_echo() {
        let mut ws = client(Vec::new());
        ws.close().unwrap();
        assert_eq!(sent_frames(&ws).len(), 1);
    }

    // --- Trames invalides ---

    #[test]
    fn eof_between_frames_is_io_error() {
        assert!(matches!(client(Vec::new()).recv_text(), Err(Error::Io(_))));
    }

    #[test]
    fn truncated_frames_are_protocol_errors() {
        for bytes in [
            &[0x81][..],
            &[0x81, 0x7E, 0x01],
            &[0x81, 0x05, b'H', b'e'],
            &[0x81, 0x7F, 0, 0, 0],
        ] {
            assert_protocol(read_as_client(bytes), "tronquée");
        }
        let short_mask = [0x81, 0x85, 1, 2];
        assert_protocol(
            read_frame(&mut &short_mask[..], Role::Server, 10),
            "tronquée",
        );
    }

    #[test]
    fn size_limit_is_checked_before_allocation() {
        let input = server_frame(true, OpCode::Text, &[b'a'; 11]);
        assert_protocol(
            client(input).with_max_message(10).recv_text(),
            "plus de 10 octets",
        );
        // Le plafond porte sur le message entier, fragments réunis.
        let input = [
            server_frame(false, OpCode::Text, &[b'a'; 6]),
            server_frame(true, OpCode::Continuation, &[b'a'; 6]),
        ]
        .concat();
        assert_protocol(
            client(input).with_max_message(10).recv_text(),
            "plus de 4 octets",
        );
        // 2^62 octets annoncés sans charge utile : refusé sans rien allouer.
        assert_protocol(
            read_as_client(&[0x81, 0x7F, 0x40, 0, 0, 0, 0, 0, 0, 0]),
            "plus de",
        );
        assert_protocol(
            read_as_client(&[0x81, 0x7F, 0x80, 0, 0, 0, 0, 0, 0, 1]),
            "bit de poids fort",
        );
    }

    #[test]
    fn invalid_frames_are_refused() {
        let long_ping = [&[0x89, 0x7E, 0x00, 0x7E][..], &[0; 126]].concat();
        let cases: [(&[u8], &str); 6] = [
            (&[0xC1, 0x00], "réservés"),
            (&[0x91, 0x00], "réservés"),
            (&[0x83, 0x00], "inconnu"),
            (&[0x81, 0x80, 0, 0, 0, 0], "masquée reçue du serveur"),
            (&[0x09, 0x00], "contrôle"),
            (&long_ping, "contrôle"),
        ];
        for (bytes, needle) in cases {
            assert_protocol(read_as_client(bytes), needle);
        }
    }

    #[test]
    fn message_sequence_errors() {
        let input = server_frame(true, OpCode::Continuation, b"x");
        assert_protocol(client(input).recv_text(), "continuation sans début");
        let input = [
            server_frame(false, OpCode::Text, b"a"),
            server_frame(true, OpCode::Text, b"b"),
        ]
        .concat();
        assert_protocol(client(input).recv_text(), "avant la fin");
        let input = server_frame(true, OpCode::Binary, b"x");
        assert_protocol(client(input).recv_text(), "binaire");
        let input = server_frame(true, OpCode::Text, &[0xC3, 0x28]);
        assert_protocol(client(input).recv_text(), "UTF-8");
    }

    // --- Poignée de main ---

    const RFC_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
    const RFC_ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

    fn response(accept: &str) -> String {
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )
    }

    #[test]
    fn handshake_request_and_first_frame_kept() {
        let auth_required = br#"{"type":"auth_required"}"#;
        let input = [
            response(RFC_ACCEPT).into_bytes(),
            server_frame(true, OpCode::Text, auth_required),
        ]
        .concat();
        let mut ws = client(input);
        ws.handshake("ha.local:8123", "/api/websocket", RFC_KEY)
            .unwrap();
        assert_eq!(
            String::from_utf8(ws.stream().output.clone()).unwrap(),
            "GET /api/websocket HTTP/1.1\r\nHost: ha.local:8123\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        // La trame qui suit immédiatement la réponse n'est pas perdue.
        assert_eq!(
            ws.recv_text().unwrap().as_deref(),
            Some(r#"{"type":"auth_required"}"#)
        );
    }

    #[test]
    fn handshake_headers_are_case_insensitive() {
        let head = format!(
            "HTTP/1.1 101 OK\r\nupgrade: WebSocket\r\nCONNECTION: keep-alive, upgrade\r\n\
             sec-websocket-accept:{RFC_ACCEPT}\r\n\r\n"
        );
        check_handshake(&head, RFC_KEY).unwrap();
    }

    #[test]
    fn handshake_refusals() {
        let ok = response(RFC_ACCEPT);
        let cases = [
            ("HTTP/1.1 404 Not Found\r\n\r\n".to_owned(), "404"),
            (ok.replace("HTTP/1.1", "HTTP/1.0"), "refusée"),
            (ok.replace("Upgrade: websocket\r\n", ""), "Upgrade"),
            (
                ok.replace("Connection: Upgrade", "Connection: close"),
                "Upgrade",
            ),
            (response("AAAAAAAAAAAAAAAAAAAAAAAAAAA="), "Accept"),
            (
                ok.replace(&format!("Sec-WebSocket-Accept: {RFC_ACCEPT}\r\n"), ""),
                "Accept",
            ),
        ];
        for (head, needle) in cases {
            assert_protocol(check_handshake(&head, RFC_KEY), needle);
        }
    }

    #[test]
    fn handshake_cut_or_too_long() {
        let mut ws = client(b"HTTP/1.1 101 Switching".to_vec());
        assert_protocol(ws.handshake("h", "/", RFC_KEY), "fermée pendant");
        let mut ws = client(vec![b'a'; MAX_HANDSHAKE_LEN + 10]);
        assert_protocol(ws.handshake("h", "/", RFC_KEY), "trop longue");
    }

    #[test]
    fn target_is_a_view_of_the_address() {
        let t = |url: &str| Target::new(Url::ws(url).unwrap());
        let ha = t("ws://192.168.0.88:8123/api/websocket");
        assert_eq!((ha.socket_host(), ha.port()), ("192.168.0.88", 8123));
        assert_eq!(ha.0.authority(), "192.168.0.88:8123");
        assert_eq!(ha.0.path_and_query(), "/api/websocket");
        let plain = t("ws://ha.local");
        assert_eq!((plain.socket_host(), plain.port()), ("ha.local", 80));
        assert_eq!(plain.0.authority(), "ha.local");
        assert_eq!(plain.0.path_and_query(), "/");
        assert_eq!(t("ws://ha.local?x=1#frag").0.path_and_query(), "/?x=1");
        assert_eq!(t("ws://ha.local:80/x").0.authority(), "ha.local");
        let v6 = t("ws://[::1]:8124/api/websocket");
        assert_eq!((v6.socket_host(), v6.port()), ("::1", 8124));
        assert_eq!(v6.0.authority(), "[::1]:8124");
    }

    #[test]
    fn connect_refuses_invalid_addresses_without_echo() {
        for bad in [
            "wss://h/",
            "http://h/",
            "ws://",
            "ws://:8123/",
            "ws://h:0/",
            "ws://h:99999/",
            "ws://h:x/",
            "ws://h:/",
            "ws://secret@h/",
            "ws://h/a b",
            "ws://[::1/",
            "ws://h/\r\nX: y",
        ] {
            match connect(bad, quick()) {
                Err(Error::Protocol(m)) => {
                    assert!(m.starts_with("adresse invalide : "), "{bad:?} : {m}");
                    assert!(!m.contains("secret"), "{m}");
                }
                Err(e) => panic!("{bad:?} : {e:?}"),
                Ok(_) => panic!("{bad:?} aurait dû être refusée"),
            }
        }
    }

    // --- SHA-1, base64, aléa ---

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha1_fips_vectors() {
        let cases: [(&[u8], &str); 4] = [
            (b"", "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (b"abc", "a9993e364706816aba3e25717850c26c9cd0d89d"),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "84983e441c3bd26ebaae4aa1f95129e5e54670f1",
            ),
            (
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmno\
                  ijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
                "a49b2446a02c645bf419f995b67091253a04a259",
            ),
        ];
        for (data, digest) in cases {
            assert_eq!(hex(&sha1(data)), digest);
        }
        assert_eq!(
            hex(&sha1(&vec![b'a'; 1_000_000])),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }

    #[test]
    fn sha1_padding_boundaries() {
        // Valeurs de référence calculées avec sha1sum.
        for (len, digest) in [
            (55, "c1c8bbdc22796e28c0e15163d20899b65621d65a"),
            (56, "c2db330f6083854c99d4b5bfb6e8f29f201be699"),
            (63, "03f09f5b158a7a8cdad920bddc29b81c18a551f5"),
            (64, "0098ba824b5c16427bd7a1122a5a442a25ec644d"),
            (65, "11655326c708d70319be2610e8a57d9a5b959d3b"),
        ] {
            assert_eq!(hex(&sha1(&vec![b'a'; len])), digest, "{len}");
        }
    }

    #[test]
    fn base64_rfc4648_vectors() {
        for (data, text) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(data.as_bytes()), text);
        }
        assert_eq!(base64(&[0xFB, 0xFF]), "+/8=");
    }

    #[test]
    fn accept_key_rfc_example() {
        assert_eq!(accept_key(RFC_KEY), RFC_ACCEPT);
    }

    #[test]
    fn noise_does_not_repeat() {
        let mut n = Noise::new();
        assert_ne!(n.next_u64(), n.next_u64());
        assert_ne!(Noise::new().next_u64(), Noise::new().next_u64());
    }

    // --- Réseau local (127.0.0.1) ---

    fn quick() -> Timeouts {
        Timeouts {
            connect: Duration::from_secs(2),
            read: Duration::from_millis(200),
            write: Duration::from_secs(2),
        }
    }

    #[test]
    fn connect_times_out_on_silent_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/api/websocket", listener.local_addr().unwrap());
        match connect(&url, quick()) {
            Err(e) => assert_eq!(
                e,
                Error::Io("délai dépassé sur la connexion WebSocket".into())
            ),
            Ok(_) => panic!("connexion inattendue"),
        }
    }

    #[test]
    fn connect_refused_is_io_error() {
        let addr = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        match connect(&format!("ws://{addr}/"), quick()) {
            Err(Error::Io(m)) => assert!(m.contains("impossible"), "{m}"),
            Err(e) => panic!("{e}"),
            Ok(_) => panic!("connexion inattendue"),
        }
    }
}

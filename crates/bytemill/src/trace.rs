//! Session-aware stream decoding for one Minecraft Java connection.
//!
//! A capture is a sequence of direction-tagged slices of the two TCP byte
//! streams. Frame boundaries and chunk boundaries are independent: a frame may
//! span chunks, and one chunk may hold many frames. [`Tracer`] reassembles each
//! direction, unwraps the compression envelope once it has been negotiated, and
//! decrypts with AES-128-CFB8 once login encryption begins. Packet names drive
//! a hand-written state machine — the schema language itself does not change.
//!
//! Both the hand-written 26.3 root and the ProtoDef-imported version roots are
//! recognised. A transition whose packet the schema does not declare never
//! fires, and a state with no routes (configuration before 1.20.2) is never
//! entered.

use std::fmt::Write as _;

use crate::ResolvedSchema;
use crate::compress::{ensure_minecraft_frame, unwrap_minecraft_frame};
use crate::crypto::Cfb8Decryptor;
use crate::errors::BmlResult;
use crate::interp::decode_frame_payload;
use crate::json::{self, Value};
use crate::ordmap::OrderedMap;

const ENCRYPTED_STREAM: &str = "stream is encrypted; pass a shared secret";
const HEX_CAP: usize = 64;

/// Which TCP stream a captured slice belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Serverbound,
    Clientbound,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Serverbound => "serverbound",
            Self::Clientbound => "clientbound",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Serverbound => 0,
            Self::Clientbound => 1,
        }
    }

    fn both() -> [Self; 2] {
        [Self::Serverbound, Self::Clientbound]
    }
}

/// One slice of a direction's byte stream, in capture order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub direction: Direction,
    pub bytes: Vec<u8>,
}

/// Options for [`Tracer::new`]. `secret` is the 16-byte shared secret; without
/// it an encrypted stream yields one error per direction and then stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceOptions {
    pub secret: Option<[u8; 16]>,
    pub initial_state: String,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            secret: None,
            initial_state: "handshaking".to_string(),
        }
    }
}

/// One thing the tracer observed. `seq` is the 1-based index over every event
/// a session emits, including errors and the trailing [`Event::Incomplete`]s.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Packet {
        seq: u64,
        direction: Direction,
        state: String,
        id: i128,
        packet: String,
        fields: Value,
    },
    State {
        seq: u64,
        from: String,
        to: String,
        cause: String,
    },
    Compression {
        seq: u64,
        threshold: i128,
    },
    Encryption {
        seq: u64,
    },
    Error {
        seq: u64,
        direction: Direction,
        state: String,
        message: String,
        hex: String,
    },
    Incomplete {
        seq: u64,
        direction: Direction,
        pending_bytes: usize,
    },
}

struct Decoded {
    id: i128,
    name: String,
    fields: Value,
}

enum SideEffect {
    None,
    State(&'static str),
    Compression(i128),
    Encrypt,
}

struct Stream {
    buf: Vec<u8>,
    dead: bool,
    decryptor: Option<Cfb8Decryptor>,
}

impl Stream {
    fn fresh() -> Self {
        Self {
            buf: Vec::new(),
            dead: false,
            decryptor: None,
        }
    }
}

enum Peek {
    NeedMore,
    Invalid,
    Frame(usize),
}

/// Replays one connection. Each direction has its own buffer; an undecodable
/// frame kills only that direction.
pub struct Tracer<'a> {
    resolved: &'a ResolvedSchema,
    secret: Option<[u8; 16]>,
    state: String,
    /// Compression envelope is on for every later frame in both directions.
    envelope: bool,
    encrypted: bool,
    has_configuration: bool,
    seq: u64,
    streams: [Stream; 2],
}

impl<'a> Tracer<'a> {
    /// Reject a schema whose routed frames are not Minecraft-style
    /// (`length: varint`, `id: varint`, `payload: rest`). The error is the one
    /// [`ensure_minecraft_frame`] returns.
    pub fn new(resolved: &'a ResolvedSchema, options: TraceOptions) -> BmlResult<Self> {
        ensure_routed_frames(resolved, &options.initial_state)?;
        let has_configuration = resolved
            .routes
            .iter()
            .any(|(route, _)| route.state == "configuration");
        Ok(Self {
            resolved,
            secret: options.secret,
            state: options.initial_state,
            envelope: false,
            encrypted: false,
            has_configuration,
            seq: 0,
            streams: [Stream::fresh(), Stream::fresh()],
        })
    }

    /// Append one capture slice and return every event it completed.
    ///
    /// A direction that has already failed is dropped with no further events.
    pub fn feed(&mut self, chunk: &Chunk) -> Vec<Event> {
        let mut events = Vec::new();
        let dir = chunk.direction;
        let idx = dir.index();
        if self.streams[idx].dead || chunk.bytes.is_empty() {
            return events;
        }
        if self.encrypted && self.secret.is_none() {
            self.fail(dir, ENCRYPTED_STREAM, &chunk.bytes, &mut events);
            return events;
        }
        if self.encrypted {
            self.decrypt_incoming(idx, &chunk.bytes);
        } else {
            self.streams[idx].buf.extend_from_slice(&chunk.bytes);
        }
        self.drain(dir, &mut events);
        events
    }

    /// Report a partial frame left in either direction. A direction that
    /// already failed is not reported: it cannot resync, so the remainder was
    /// dropped.
    pub fn finish(mut self) -> Vec<Event> {
        let pending: Vec<(Direction, usize)> = Direction::both()
            .into_iter()
            .filter_map(|dir| {
                let stream = &self.streams[dir.index()];
                if stream.dead || stream.buf.is_empty() {
                    None
                } else {
                    Some((dir, stream.buf.len()))
                }
            })
            .collect();
        let mut events = Vec::new();
        for (dir, pending_bytes) in pending {
            let seq = self.next_seq();
            events.push(Event::Incomplete {
                seq,
                direction: dir,
                pending_bytes,
            });
        }
        events
    }

    /// Decrypt `bytes` onto the end of the direction buffer. Called only when
    /// a secret is present, so the decryptor was installed at encryption start.
    fn decrypt_incoming(&mut self, idx: usize, bytes: &[u8]) {
        let start = self.streams[idx].buf.len();
        self.streams[idx].buf.extend_from_slice(bytes);
        let Stream { buf, decryptor, .. } = &mut self.streams[idx];
        if let Some(decryptor) = decryptor.as_mut() {
            decryptor.decrypt(&mut buf[start..]);
        }
    }

    fn drain(&mut self, dir: Direction, events: &mut Vec<Event>) {
        loop {
            if self.streams[dir.index()].dead {
                return;
            }
            let total = match peek_frame(&self.streams[dir.index()].buf) {
                Peek::NeedMore => return,
                Peek::Invalid => {
                    let bytes = self.streams[dir.index()].buf.clone();
                    self.fail(dir, "invalid frame length", &bytes, events);
                    return;
                }
                Peek::Frame(total) => total,
            };
            let rest = self.streams[dir.index()].buf.split_off(total);
            let frame = std::mem::replace(&mut self.streams[dir.index()].buf, rest);
            match self.decode_one(dir, &frame) {
                Ok(decoded) => {
                    self.push_packet(dir, &decoded, events);
                    self.apply(dir, &decoded, events);
                }
                Err(message) => {
                    self.fail(dir, message, &frame, events);
                    return;
                }
            }
        }
    }

    fn decode_one(&self, dir: Direction, wire: &[u8]) -> Result<Decoded, String> {
        let logical = if self.envelope {
            unwrap_minecraft_frame(wire).map_err(|err| err.to_string())?
        } else {
            wire.to_vec()
        };
        let (id, mut env) =
            decode_frame_payload(self.resolved, &self.state, dir.as_str(), &logical)
                .map_err(|err| err.to_string())?;
        let name = match env.remove("packet") {
            Some(Value::Str(text)) => text.to_string(),
            _ => return Err("decoded frame has no packet name".to_string()),
        };
        let fields = env
            .remove("fields")
            .unwrap_or_else(|| Value::map(OrderedMap::new()));
        Ok(Decoded { id, name, fields })
    }

    fn push_packet(&mut self, dir: Direction, decoded: &Decoded, events: &mut Vec<Event>) {
        let seq = self.next_seq();
        events.push(Event::Packet {
            seq,
            direction: dir,
            state: self.state.clone(),
            id: decoded.id,
            packet: decoded.name.clone(),
            fields: decoded.fields.clone(),
        });
    }

    fn apply(&mut self, dir: Direction, decoded: &Decoded, events: &mut Vec<Event>) {
        let effect = side_effect(
            &self.state,
            dir,
            &decoded.name,
            &decoded.fields,
            self.has_configuration,
        );
        match effect {
            SideEffect::None => {}
            SideEffect::State(to) => self.change_state(&decoded.name, to, events),
            SideEffect::Compression(threshold) => self.note_compression(threshold, events),
            SideEffect::Encrypt => self.begin_encryption(dir, events),
        }
    }

    fn change_state(&mut self, cause: &str, to: &str, events: &mut Vec<Event>) {
        let from = std::mem::replace(&mut self.state, to.to_string());
        if from == to {
            return;
        }
        let seq = self.next_seq();
        events.push(Event::State {
            seq,
            from,
            to: to.to_string(),
            cause: cause.to_string(),
        });
    }

    fn note_compression(&mut self, threshold: i128, events: &mut Vec<Event>) {
        let seq = self.next_seq();
        events.push(Event::Compression { seq, threshold });
        // A negative threshold disables the envelope; non-negative turns it on
        // for every later frame, in both directions. This frame already used
        // the previous framing.
        self.envelope = threshold >= 0;
    }

    /// Encryption starts at the byte after the serverbound response. Bytes
    /// already buffered on the clientbound side were captured before that
    /// frame completed, so they stay plaintext; the next clientbound chunk is
    /// the first encrypted one.
    fn begin_encryption(&mut self, dir: Direction, events: &mut Vec<Event>) {
        if self.encrypted {
            return;
        }
        let seq = self.next_seq();
        events.push(Event::Encryption { seq });
        self.encrypted = true;
        let idx = dir.index();
        if let Some(secret) = self.secret {
            for stream in &mut self.streams {
                stream.decryptor = Some(Cfb8Decryptor::new(&secret, &secret));
            }
            let mut pending = std::mem::take(&mut self.streams[idx].buf);
            if !pending.is_empty()
                && let Some(decryptor) = self.streams[idx].decryptor.as_mut()
            {
                decryptor.decrypt(&mut pending);
            }
            self.streams[idx].buf = pending;
        } else {
            let pending = std::mem::take(&mut self.streams[idx].buf);
            if !pending.is_empty() {
                self.fail(dir, ENCRYPTED_STREAM, &pending, events);
            }
        }
    }

    fn fail(
        &mut self,
        dir: Direction,
        message: impl Into<String>,
        frame: &[u8],
        events: &mut Vec<Event>,
    ) {
        let seq = self.next_seq();
        events.push(Event::Error {
            seq,
            direction: dir,
            state: self.state.clone(),
            message: message.into(),
            hex: hex_capped(frame),
        });
        let stream = &mut self.streams[dir.index()];
        stream.dead = true;
        stream.buf.clear();
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }
}

/// Feed every chunk, then [`Tracer::finish`].
pub fn trace(
    resolved: &ResolvedSchema,
    options: TraceOptions,
    chunks: &[Chunk],
) -> BmlResult<Vec<Event>> {
    let mut tracer = Tracer::new(resolved, options)?;
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(tracer.feed(chunk));
    }
    events.extend(tracer.finish());
    Ok(events)
}

/// One JSON object, keys sorted, no trailing newline.
pub fn event_json(event: &Event) -> String {
    let mut map = OrderedMap::new();
    match event {
        Event::Packet {
            seq,
            direction,
            state,
            id,
            packet,
            fields,
        } => {
            map.insert("seq", Value::Int(i128::from(*seq)));
            map.insert("direction", Value::str(direction.as_str()));
            map.insert("state", Value::str(state));
            map.insert("id", Value::Int(*id));
            map.insert("packet", Value::str(packet));
            map.insert("fields", fields.clone());
        }
        Event::State {
            seq,
            from,
            to,
            cause,
        } => {
            map.insert("seq", Value::Int(i128::from(*seq)));
            map.insert("event", Value::str("state"));
            map.insert("from", Value::str(from));
            map.insert("to", Value::str(to));
            map.insert("cause", Value::str(cause));
        }
        Event::Compression { seq, threshold } => {
            map.insert("seq", Value::Int(i128::from(*seq)));
            map.insert("event", Value::str("compression"));
            map.insert("threshold", Value::Int(*threshold));
        }
        Event::Encryption { seq } => {
            map.insert("seq", Value::Int(i128::from(*seq)));
            map.insert("event", Value::str("encryption"));
        }
        Event::Error {
            seq,
            direction,
            state,
            message,
            hex,
        } => {
            map.insert("seq", Value::Int(i128::from(*seq)));
            map.insert("event", Value::str("error"));
            map.insert("direction", Value::str(direction.as_str()));
            map.insert("state", Value::str(state));
            map.insert("message", Value::str(message));
            map.insert("hex", Value::str(hex));
        }
        Event::Incomplete {
            seq,
            direction,
            pending_bytes,
        } => {
            map.insert("seq", Value::Int(i128::from(*seq)));
            map.insert("event", Value::str("incomplete"));
            map.insert("direction", Value::str(direction.as_str()));
            map.insert(
                "pending_bytes",
                Value::Int(i128::try_from(*pending_bytes).unwrap_or(i128::MAX)),
            );
        }
    }
    json::dumps_sorted(&Value::map(map))
}

/// Parse a JSONL capture. Blank lines are ignored; extra object keys are
/// ignored; whitespace inside `hex` is ignored. `toServer` / `toClient` are
/// the same directions as `serverbound` / `clientbound`. A bad line's error
/// names that line, 1-based.
pub fn parse_capture(text: &str) -> Result<Vec<Chunk>, String> {
    let mut chunks = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line_no = index + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value = json::loads(trimmed).map_err(|err| format!("line {line_no}: {err}"))?;
        let Value::Map(obj) = value else {
            return Err(format!("line {line_no}: expected a JSON object"));
        };
        let direction = match obj.get("direction") {
            Some(Value::Str(text)) => parse_direction(text)
                .ok_or_else(|| format!("line {line_no}: unknown direction {text:?}"))?,
            Some(_) => return Err(format!("line {line_no}: direction must be a string")),
            None => return Err(format!("line {line_no}: missing direction")),
        };
        let hex = match obj.get("hex") {
            Some(Value::Str(text)) => text.as_ref(),
            Some(_) => return Err(format!("line {line_no}: hex must be a string")),
            None => return Err(format!("line {line_no}: missing hex")),
        };
        let bytes = parse_hex(hex).map_err(|err| format!("line {line_no}: {err}"))?;
        chunks.push(Chunk { direction, bytes });
    }
    Ok(chunks)
}

fn parse_direction(text: &str) -> Option<Direction> {
    match text {
        "serverbound" | "toServer" => Some(Direction::Serverbound),
        "clientbound" | "toClient" => Some(Direction::Clientbound),
        _ => None,
    }
}

fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let mut digits = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            continue;
        }
        if !ch.is_ascii_hexdigit() {
            return Err(format!("invalid hex digit {ch:?}"));
        }
        digits.push(ch);
    }
    if !digits.len().is_multiple_of(2) {
        return Err("odd number of hex digits".to_string());
    }
    let mut out = Vec::with_capacity(digits.len() / 2);
    let raw = digits.as_bytes();
    for index in (0..raw.len()).step_by(2) {
        let pair =
            std::str::from_utf8(&raw[index..index + 2]).map_err(|_| "invalid hex".to_string())?;
        let byte = u8::from_str_radix(pair, 16).map_err(|_| "invalid hex".to_string())?;
        out.push(byte);
    }
    Ok(out)
}

fn ensure_routed_frames(resolved: &ResolvedSchema, initial_state: &str) -> BmlResult<()> {
    let mut pairs = Vec::new();
    for (route, _) in &resolved.routes {
        let pair = (route.state.as_str(), route.direction.as_str());
        if !pairs.contains(&pair) {
            pairs.push(pair);
        }
    }
    if pairs.is_empty() {
        return ensure_minecraft_frame(resolved, initial_state, "serverbound");
    }
    for (state, direction) in pairs {
        ensure_minecraft_frame(resolved, state, direction)?;
    }
    Ok(())
}

/// Packet-name transitions. Names absent from a schema never arrive here, so
/// the rule never fires. `login_acknowledged` enters `configuration` only when
/// that state has routes; otherwise login success goes straight to `play`.
fn side_effect(
    state: &str,
    dir: Direction,
    name: &str,
    fields: &Value,
    has_configuration: bool,
) -> SideEffect {
    match (state, dir, name) {
        ("handshaking", Direction::Serverbound, "handshake" | "set_protocol") => {
            handshake_target(name, fields)
                .map(SideEffect::State)
                .unwrap_or(SideEffect::None)
        }
        ("login", Direction::Clientbound, "login_compression" | "compress") => {
            int_field(fields, "threshold")
                .map(SideEffect::Compression)
                .unwrap_or(SideEffect::None)
        }
        ("login", Direction::Clientbound, "login_finished" | "success") => {
            if has_configuration {
                SideEffect::None
            } else {
                SideEffect::State("play")
            }
        }
        ("login", Direction::Serverbound, "login_acknowledged") if has_configuration => {
            SideEffect::State("configuration")
        }
        (
            "login",
            Direction::Serverbound,
            "encryption_response" | "encryption_begin_login_serverbound",
        ) => SideEffect::Encrypt,
        (
            "configuration",
            Direction::Serverbound,
            "finish_configuration" | "finish_configuration_configuration_serverbound",
        ) => SideEffect::State("play"),
        ("play", Direction::Serverbound, "configuration_acknowledged") => {
            SideEffect::State("configuration")
        }
        _ => SideEffect::None,
    }
}

/// `intent` (26.3): 1 status, 2 login, 3 transfer (login). `next_state`
/// (version roots): 1 status, 2 login.
fn handshake_target(name: &str, fields: &Value) -> Option<&'static str> {
    let (key, transfer_is_login) = match name {
        "handshake" => ("intent", true),
        "set_protocol" => ("next_state", false),
        _ => return None,
    };
    match int_field(fields, key)? {
        1 => Some("status"),
        2 => Some("login"),
        3 if transfer_is_login => Some("login"),
        _ => None,
    }
}

fn int_field(fields: &Value, key: &str) -> Option<i128> {
    fields.get(key)?.as_int().ok()
}

/// Outer frame: a Minecraft VarInt length, then that many bytes. A short
/// continuation is incomplete, not an error — the rest may arrive later.
fn peek_frame(buf: &[u8]) -> Peek {
    match read_length(buf) {
        LengthRead::NeedMore => Peek::NeedMore,
        LengthRead::Invalid => Peek::Invalid,
        LengthRead::Ready { header, body } => match header.checked_add(body) {
            Some(total) if buf.len() >= total => Peek::Frame(total),
            Some(_) => Peek::NeedMore,
            None => Peek::Invalid,
        },
    }
}

enum LengthRead {
    NeedMore,
    Invalid,
    Ready { header: usize, body: usize },
}

fn read_length(data: &[u8]) -> LengthRead {
    if data.is_empty() {
        return LengthRead::NeedMore;
    }
    let mut raw: u32 = 0;
    for index in 0..5 {
        if index == data.len() {
            return LengthRead::NeedMore;
        }
        let byte = data[index];
        if index == 4 {
            if byte & 0x80 != 0 {
                return LengthRead::Invalid;
            }
            raw |= u32::from(byte & 0x0f) << 28;
            if raw & 0x8000_0000 != 0 {
                return LengthRead::Invalid;
            }
            return LengthRead::Ready {
                header: 5,
                body: usize::try_from(raw).unwrap_or(usize::MAX),
            };
        }
        raw |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return LengthRead::Ready {
                header: index + 1,
                body: usize::try_from(raw).unwrap_or(usize::MAX),
            };
        }
    }
    LengthRead::Invalid
}

fn hex_capped(bytes: &[u8]) -> String {
    let count = bytes.len().min(HEX_CAP);
    let mut out = String::with_capacity(count * 2);
    for byte in &bytes[..count] {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

//! Session replay: state transitions, compression, encryption, and chunk boundaries.

mod common;

use bytemill::compress::{CompressionSpec, wrap_minecraft_frame};
use bytemill::crypto::Cfb8Encryptor;
use bytemill::interp::encode_packet;
use bytemill::trace::{
    Chunk, Direction, Event, TraceOptions, Tracer, event_json, parse_capture, trace,
};
use bytemill::{Env, ResolvedSchema, Value};

const SECRET: [u8; 16] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
];

fn load_26() -> ResolvedSchema {
    common::load_minecraft().expect("26.3 schema")
}

fn load_1165() -> ResolvedSchema {
    let path = common::schema_path("versions/v1_16_5.bml");
    bytemill::analyze(&bytemill::load_schema(&[path]).expect("load 1.16.5"))
        .expect("analyze 1.16.5")
}

macro_rules! fields {
    ($($key:expr => $value:expr),* $(,)?) => {{
        #[allow(unused_mut, reason = "an empty invocation inserts nothing")]
        let mut env = Env::new();
        $(env.insert($key, $value);)*
        env
    }};
}

fn uuid(msb: i128, lsb: i128) -> Value {
    let mut map = Env::new();
    map.insert("msb", Value::Int(msb));
    map.insert("lsb", Value::Int(lsb));
    Value::map(map)
}

fn profile(name: &str) -> Value {
    let mut map = Env::new();
    map.insert("profile_uuid", uuid(1, 2));
    map.insert("username", Value::str(name));
    map.insert("properties_count", Value::Int(0));
    map.insert("properties", Value::list(Vec::new()));
    Value::map(map)
}

fn route_id(resolved: &ResolvedSchema, state: &str, direction: &str, name: &str) -> i128 {
    resolved
        .routes
        .iter()
        .find(|(route, pkt)| {
            route.state == state && route.direction == direction && pkt.name == name
        })
        .map(|(route, _)| route.packet_id)
        .unwrap_or_else(|| panic!("no route {state}.{direction} {name}"))
}

fn write_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return;
        }
    }
}

fn read_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let byte = *data.get(*pos)?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

fn logical_frame(id: i128, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint(u64::try_from(id).expect("id"), &mut body);
    body.extend_from_slice(payload);
    let mut out = Vec::new();
    write_varint(u64::try_from(body.len()).expect("length"), &mut out);
    out.extend_from_slice(&body);
    out
}

fn encoded_frame(
    resolved: &ResolvedSchema,
    state: &str,
    dir: Direction,
    name: &str,
    fields: &Env,
) -> Vec<u8> {
    let id = route_id(resolved, state, dir.as_str(), name);
    let payload =
        encode_packet(resolved, name, fields).unwrap_or_else(|err| panic!("encode {name}: {err}"));
    logical_frame(id, &payload)
}

struct Step {
    direction: Direction,
    bytes: Vec<u8>,
}

struct Builder<'a> {
    resolved: &'a ResolvedSchema,
    steps: Vec<Step>,
    envelope: bool,
}

impl<'a> Builder<'a> {
    fn new(resolved: &'a ResolvedSchema) -> Self {
        Self {
            resolved,
            steps: Vec::new(),
            envelope: false,
        }
    }

    fn push(&mut self, state: &str, dir: Direction, name: &str, fields: &Env) {
        let logical = encoded_frame(self.resolved, state, dir, name, fields);
        let bytes = if self.envelope {
            wrap_minecraft_frame(&logical, CompressionSpec { threshold: 256 })
                .unwrap_or_else(|err| panic!("wrap {name}: {err}"))
        } else {
            logical
        };
        self.steps.push(Step {
            direction: dir,
            bytes,
        });
    }
}

fn byte_chunks(steps: &[Step]) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    for step in steps {
        for byte in &step.bytes {
            chunks.push(Chunk {
                direction: step.direction,
                bytes: vec![*byte],
            });
        }
    }
    chunks
}

/// Consecutive frames of one direction become one chunk. Cross-direction order
/// is preserved, which is what makes the decoded events identical.
fn run_chunks(steps: &[Step]) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    for step in steps {
        if let Some(last) = chunks.last_mut()
            && last.direction == step.direction
        {
            last.bytes.extend_from_slice(&step.bytes);
        } else {
            chunks.push(Chunk {
                direction: step.direction,
                bytes: step.bytes.clone(),
            });
        }
    }
    chunks
}

/// Split every frame after its first byte, so a multi-byte length varint is cut
/// in the middle.
fn split_chunks(steps: &[Step]) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    for step in steps {
        if step.bytes.is_empty() {
            continue;
        }
        chunks.push(Chunk {
            direction: step.direction,
            bytes: step.bytes[..1].to_vec(),
        });
        if step.bytes.len() > 1 {
            chunks.push(Chunk {
                direction: step.direction,
                bytes: step.bytes[1..].to_vec(),
            });
        }
    }
    chunks
}

fn frame_count(bytes: &[u8]) -> usize {
    let mut pos = 0;
    let mut count = 0;
    while pos < bytes.len() {
        let start = pos;
        let Some(body) = read_varint(bytes, &mut pos) else {
            break;
        };
        let total = pos - start + usize::try_from(body).unwrap_or(usize::MAX);
        if pos - start + usize::try_from(body).unwrap_or(0) > bytes.len() - start {
            break;
        }
        pos = start + total;
        count += 1;
    }
    count
}

fn envelope_data_length(frame: &[u8]) -> u64 {
    let mut pos = 0;
    let _outer = read_varint(frame, &mut pos).expect("outer length");
    read_varint(frame, &mut pos).expect("data_length")
}

fn replay(resolved: &ResolvedSchema, options: TraceOptions, chunks: &[Chunk]) -> Vec<Event> {
    trace(resolved, options, chunks).unwrap_or_else(|err| panic!("trace: {err}"))
}

fn outline(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event {
            Event::Packet {
                direction,
                state,
                packet,
                id,
                ..
            } => format!("packet {} {state} {packet} id={id}", direction.as_str()),
            Event::State {
                from, to, cause, ..
            } => format!("state {from}->{to} ({cause})"),
            Event::Compression { threshold, .. } => format!("compression {threshold}"),
            Event::Encryption { .. } => "encryption".to_string(),
            Event::Error {
                direction, message, ..
            } => format!("error {} {message}", direction.as_str()),
            Event::Incomplete {
                direction,
                pending_bytes,
                ..
            } => format!("incomplete {} {pending_bytes}", direction.as_str()),
        })
        .collect()
}

fn packet<'a>(events: &'a [Event], name: &str) -> &'a Event {
    events
        .iter()
        .find(|event| matches!(event, Event::Packet { packet, .. } if packet == name))
        .unwrap_or_else(|| panic!("missing packet {name}"))
}

fn fields_of(event: &Event) -> &Value {
    match event {
        Event::Packet { fields, .. } => fields,
        other => panic!("not a packet: {other:?}"),
    }
}

fn session_26_3(resolved: &ResolvedSchema) -> Vec<Step> {
    let mut builder = Builder::new(resolved);
    let address = "a".repeat(200);
    builder.push(
        "handshaking",
        Direction::Serverbound,
        "handshake",
        &fields!(
            "protocol_version" => Value::Int(777),
            "server_address" => Value::str(&address),
            "server_port" => Value::Int(25565),
            "intent" => Value::Int(2),
        ),
    );
    builder.push(
        "login",
        Direction::Serverbound,
        "login_start",
        &fields!(
            "name" => Value::str("Steve"),
            "player_uuid" => uuid(7, 8),
        ),
    );
    builder.push(
        "login",
        Direction::Clientbound,
        "login_compression",
        &fields!("threshold" => Value::Int(256)),
    );
    builder.envelope = true;
    builder.push(
        "login",
        Direction::Clientbound,
        "login_finished",
        &fields!(
            "profile" => profile("Steve"),
            "session_id" => uuid(9, 10),
        ),
    );
    builder.push(
        "login",
        Direction::Serverbound,
        "login_acknowledged",
        &fields!(),
    );
    builder.push(
        "configuration",
        Direction::Clientbound,
        "finish_configuration",
        &fields!(),
    );
    builder.push(
        "configuration",
        Direction::Serverbound,
        "finish_configuration",
        &fields!(),
    );
    builder.push(
        "play",
        Direction::Clientbound,
        "keep_alive",
        &fields!("keep_alive_id" => Value::Int(42)),
    );
    let command: String = (0..300)
        .map(|i| char::from(b'!' + (i % 90) as u8))
        .collect();
    builder.push(
        "play",
        Direction::Serverbound,
        "chat_command",
        &fields!("command" => Value::str(&command)),
    );
    builder.steps
}

#[test]
fn options_default_to_handshaking() {
    let options = TraceOptions::default();
    assert!(options.secret.is_none());
    assert_eq!(options.initial_state, "handshaking");
}

#[test]
fn session_26_3_is_stable_across_chunk_boundaries() {
    let resolved = load_26();
    let steps = session_26_3(&resolved);
    assert!(
        steps[0].bytes[0] & 0x80 != 0,
        "handshake length varint must span a chunk boundary in the split delivery"
    );
    // After login_compression: login_finished, ack, two finish_configuration, keep_alive, chat.
    assert_eq!(
        envelope_data_length(&steps[7].bytes),
        0,
        "keep_alive is below the threshold"
    );
    assert!(
        envelope_data_length(&steps[8].bytes) > 0,
        "chat_command is zlib, not a raw envelope"
    );
    let runs = run_chunks(&steps);
    assert!(
        runs.iter().any(|chunk| frame_count(&chunk.bytes) > 1),
        "a direction-run must hold more than one frame"
    );

    let options = TraceOptions::default();
    let by_byte = replay(&resolved, options.clone(), &byte_chunks(&steps));
    let by_run = replay(&resolved, options.clone(), &runs);
    let by_split = replay(&resolved, options, &split_chunks(&steps));

    let expect = [
        "packet serverbound handshaking handshake id=0",
        "state handshaking->login (handshake)",
        "packet serverbound login login_start id=0",
        "packet clientbound login login_compression id=3",
        "compression 256",
        "packet clientbound login login_finished id=2",
        "packet serverbound login login_acknowledged id=3",
        "state login->configuration (login_acknowledged)",
        "packet clientbound configuration finish_configuration id=3",
        "packet serverbound configuration finish_configuration id=3",
        "state configuration->play (finish_configuration)",
        "packet clientbound play keep_alive id=45",
        "packet serverbound play chat_command id=7",
    ];
    assert_eq!(outline(&by_byte), expect, "one byte per chunk");
    assert_eq!(
        outline(&by_run),
        expect,
        "frames coalesced per direction-run"
    );
    assert_eq!(outline(&by_split), expect, "frames split mid-varint");
    assert_eq!(by_byte, by_run);
    assert_eq!(by_byte, by_split);

    assert_eq!(
        fields_of(packet(&by_byte, "handshake")).get("intent"),
        Some(&Value::Int(2))
    );
    assert_eq!(
        fields_of(packet(&by_byte, "handshake")).get("server_address"),
        Some(&Value::str("a".repeat(200)))
    );
    assert_eq!(
        fields_of(packet(&by_byte, "login_start")).get("name"),
        Some(&Value::str("Steve"))
    );
    assert_eq!(
        fields_of(packet(&by_byte, "keep_alive")).get("keep_alive_id"),
        Some(&Value::Int(42))
    );
    match fields_of(packet(&by_byte, "chat_command")).get("command") {
        Some(Value::Str(text)) => assert_eq!(text.len(), 300),
        other => panic!("command field: {other:?}"),
    }
    assert!(
        by_byte
            .iter()
            .all(|event| !matches!(event, Event::Error { .. } | Event::Incomplete { .. }))
    );
}

#[test]
fn transfer_intent_enters_login() {
    let resolved = load_26();
    let mut builder = Builder::new(&resolved);
    builder.push(
        "handshaking",
        Direction::Serverbound,
        "handshake",
        &fields!(
            "protocol_version" => Value::Int(777),
            "server_address" => Value::str("localhost"),
            "server_port" => Value::Int(25565),
            "intent" => Value::Int(3),
        ),
    );
    builder.push(
        "login",
        Direction::Serverbound,
        "login_start",
        &fields!(
            "name" => Value::str("Alex"),
            "player_uuid" => uuid(3, 4),
        ),
    );
    let events = replay(
        &resolved,
        TraceOptions::default(),
        &run_chunks(&builder.steps),
    );
    assert_eq!(
        outline(&events),
        [
            "packet serverbound handshaking handshake id=0",
            "state handshaking->login (handshake)",
            "packet serverbound login login_start id=0",
        ]
    );
}

#[test]
fn version_1_16_5_goes_straight_to_play() {
    let resolved = load_1165();
    let mut builder = Builder::new(&resolved);
    builder.push(
        "handshaking",
        Direction::Serverbound,
        "set_protocol",
        &fields!(
            "protocol_version" => Value::Int(754),
            "server_host" => Value::str("localhost"),
            "server_port" => Value::Int(25565),
            "next_state" => Value::Int(2),
        ),
    );
    builder.push(
        "login",
        Direction::Serverbound,
        "login_start",
        &fields!("username" => Value::str("Steve")),
    );
    builder.push(
        "login",
        Direction::Clientbound,
        "success",
        &fields!(
            "uuid" => uuid(1, 2),
            "username" => Value::str("Steve"),
        ),
    );
    builder.push(
        "play",
        Direction::Clientbound,
        "keep_alive_play_clientbound",
        &fields!("keep_alive_id" => Value::Int(7)),
    );
    let events = replay(
        &resolved,
        TraceOptions::default(),
        &byte_chunks(&builder.steps),
    );
    assert_eq!(
        outline(&events),
        [
            "packet serverbound handshaking set_protocol id=0",
            "state handshaking->login (set_protocol)",
            "packet serverbound login login_start id=0",
            "packet clientbound login success id=2",
            "state login->play (success)",
            "packet clientbound play keep_alive_play_clientbound id=31",
        ]
    );
    assert!(events.iter().all(|event| !matches!(
        event,
        Event::State { to, .. } if to == "configuration"
    )));
}

#[test]
fn status_flow_has_no_errors_and_stable_json() {
    let resolved = load_26();
    let mut builder = Builder::new(&resolved);
    builder.push(
        "handshaking",
        Direction::Serverbound,
        "handshake",
        &fields!(
            "protocol_version" => Value::Int(777),
            "server_address" => Value::str("localhost"),
            "server_port" => Value::Int(25565),
            "intent" => Value::Int(1),
        ),
    );
    builder.push(
        "status",
        Direction::Serverbound,
        "status_request",
        &fields!(),
    );
    builder.push(
        "status",
        Direction::Clientbound,
        "status_response",
        &fields!("json" => Value::str("hi")),
    );
    builder.push(
        "status",
        Direction::Serverbound,
        "ping_request",
        &fields!("timestamp" => Value::Int(99)),
    );
    builder.push(
        "status",
        Direction::Clientbound,
        "pong_response",
        &fields!("timestamp" => Value::Int(99)),
    );
    let events = replay(
        &resolved,
        TraceOptions::default(),
        &run_chunks(&builder.steps),
    );
    assert_eq!(
        outline(&events),
        [
            "packet serverbound handshaking handshake id=0",
            "state handshaking->status (handshake)",
            "packet serverbound status status_request id=0",
            "packet clientbound status status_response id=0",
            "packet serverbound status ping_request id=1",
            "packet clientbound status pong_response id=1",
        ]
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, Event::Error { .. }))
    );
    assert_eq!(
        event_json(&events[1]),
        r#"{"cause": "handshake", "event": "state", "from": "handshaking", "seq": 2, "to": "status"}"#
    );
    assert_eq!(
        event_json(packet(&events, "status_request")),
        r#"{"direction": "serverbound", "fields": {}, "id": 0, "packet": "status_request", "seq": 3, "state": "status"}"#
    );
    assert_eq!(
        event_json(packet(&events, "status_request")),
        event_json(packet(&events, "status_request"))
    );
    assert_eq!(
        fields_of(packet(&events, "ping_request")).get("timestamp"),
        Some(&Value::Int(99))
    );
}

#[test]
fn encryption_with_secret_decrypts_across_the_frame_boundary() {
    let resolved = load_26();
    let chunks = encryption_chunks(&resolved, &SECRET);
    // The fourth chunk is the tail of plaintext encryption_response plus the
    // first encrypted bytes of the following serverbound packet.
    assert!(chunks[3].bytes.len() > 2);
    assert_eq!(chunks[3].direction, Direction::Serverbound);

    let options = TraceOptions {
        secret: Some(SECRET),
        initial_state: "handshaking".to_string(),
    };
    let events = replay(&resolved, options, &chunks);
    assert_eq!(
        outline(&events),
        [
            "packet serverbound handshaking handshake id=0",
            "state handshaking->login (handshake)",
            "packet clientbound login hello id=1",
            "packet serverbound login encryption_response id=1",
            "encryption",
            "packet serverbound login custom_query_answer id=2",
            "packet clientbound login login_disconnect id=0",
        ]
    );
    assert_eq!(
        fields_of(packet(&events, "custom_query_answer")).get("message_id"),
        Some(&Value::Int(7))
    );
    assert_eq!(
        fields_of(packet(&events, "login_disconnect")).get("reason"),
        Some(&Value::str("bye"))
    );
}

#[test]
fn encryption_without_secret_errors_once_per_direction() {
    let resolved = load_26();
    let chunks = encryption_chunks(&resolved, &SECRET);
    let events = replay(&resolved, TraceOptions::default(), &chunks);
    let errors: Vec<&Event> = events
        .iter()
        .filter(|event| matches!(event, Event::Error { .. }))
        .collect();
    assert_eq!(errors.len(), 2, "{}", outline(&events).join(" | "));
    for (event, direction) in errors
        .iter()
        .zip([Direction::Serverbound, Direction::Clientbound])
    {
        match event {
            Event::Error {
                direction: got,
                message,
                state,
                ..
            } => {
                assert_eq!(*got, direction);
                assert_eq!(message, "stream is encrypted; pass a shared secret");
                assert_eq!(state, "login");
            }
            other => panic!("{other:?}"),
        }
    }
    let names: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            Event::Packet { packet, .. } => Some(packet.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["handshake", "hello", "encryption_response"]);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Encryption { .. }))
    );
}

#[test]
fn version_encryption_names_enable_decryption() {
    let resolved = load_1165();
    let handshake = encoded_frame(
        &resolved,
        "handshaking",
        Direction::Serverbound,
        "set_protocol",
        &fields!(
            "protocol_version" => Value::Int(754),
            "server_host" => Value::str("localhost"),
            "server_port" => Value::Int(25565),
            "next_state" => Value::Int(2),
        ),
    );
    let hello = encoded_frame(
        &resolved,
        "login",
        Direction::Clientbound,
        "encryption_begin_login_clientbound",
        &fields!(
            "server_id" => Value::str(""),
            "public_key_len" => Value::Int(0),
            "public_key" => Value::bytes(Vec::new()),
            "verify_token_len" => Value::Int(0),
            "verify_token" => Value::bytes(Vec::new()),
        ),
    );
    let response = encoded_frame(
        &resolved,
        "login",
        Direction::Serverbound,
        "encryption_begin_login_serverbound",
        &fields!(
            "shared_secret_len" => Value::Int(0),
            "shared_secret" => Value::bytes(Vec::new()),
            "verify_token_len" => Value::Int(0),
            "verify_token" => Value::bytes(Vec::new()),
        ),
    );
    let success = encoded_frame(
        &resolved,
        "login",
        Direction::Clientbound,
        "success",
        &fields!(
            "uuid" => uuid(4, 5),
            "username" => Value::str("Steve"),
        ),
    );
    let mut encrypted = success.clone();
    Cfb8Encryptor::new(&SECRET, &SECRET).encrypt(&mut encrypted);
    let chunks = vec![
        Chunk {
            direction: Direction::Serverbound,
            bytes: handshake,
        },
        Chunk {
            direction: Direction::Clientbound,
            bytes: hello,
        },
        Chunk {
            direction: Direction::Serverbound,
            bytes: response,
        },
        Chunk {
            direction: Direction::Clientbound,
            bytes: encrypted[..1].to_vec(),
        },
        Chunk {
            direction: Direction::Clientbound,
            bytes: encrypted[1..].to_vec(),
        },
    ];
    let events = replay(
        &resolved,
        TraceOptions {
            secret: Some(SECRET),
            ..TraceOptions::default()
        },
        &chunks,
    );
    assert_eq!(
        outline(&events),
        [
            "packet serverbound handshaking set_protocol id=0",
            "state handshaking->login (set_protocol)",
            "packet clientbound login encryption_begin_login_clientbound id=1",
            "packet serverbound login encryption_begin_login_serverbound id=1",
            "encryption",
            "packet clientbound login success id=2",
            "state login->play (success)",
        ]
    );
}

#[test]
fn unknown_id_stops_only_that_direction() {
    let resolved = load_26();
    let handshake = encoded_frame(
        &resolved,
        "handshaking",
        Direction::Serverbound,
        "handshake",
        &fields!(
            "protocol_version" => Value::Int(777),
            "server_address" => Value::str("localhost"),
            "server_port" => Value::Int(25565),
            "intent" => Value::Int(1),
        ),
    );
    let bad = logical_frame(0x2a, &[0u8; 70]);
    let request = encoded_frame(
        &resolved,
        "status",
        Direction::Serverbound,
        "status_request",
        &fields!(),
    );
    let ping = encoded_frame(
        &resolved,
        "status",
        Direction::Serverbound,
        "ping_request",
        &fields!("timestamp" => Value::Int(1)),
    );
    let chunks = vec![
        Chunk {
            direction: Direction::Serverbound,
            bytes: handshake,
        },
        Chunk {
            direction: Direction::Clientbound,
            bytes: bad.clone(),
        },
        Chunk {
            direction: Direction::Serverbound,
            bytes: request,
        },
        Chunk {
            direction: Direction::Clientbound,
            bytes: vec![0x00],
        },
        Chunk {
            direction: Direction::Serverbound,
            bytes: ping,
        },
    ];
    let events = replay(&resolved, TraceOptions::default(), &chunks);
    let errors: Vec<&Event> = events
        .iter()
        .filter(|event| matches!(event, Event::Error { .. }))
        .collect();
    assert_eq!(errors.len(), 1, "{}", outline(&events).join(" | "));
    match errors[0] {
        Event::Error {
            direction,
            state,
            message,
            hex,
            ..
        } => {
            assert_eq!(*direction, Direction::Clientbound);
            assert_eq!(state, "status");
            assert_eq!(message, "no route for id 0x2a in status.clientbound");
            assert_eq!(hex.len(), 128);
            assert_eq!(hex, &hex_encode(&bad)[..128]);
        }
        other => panic!("{other:?}"),
    }
    let names: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            Event::Packet { packet, .. } => Some(packet.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["handshake", "status_request", "ping_request"]);
}

#[test]
fn truncated_frame_reports_pending_bytes() {
    let resolved = load_26();
    let handshake = encoded_frame(
        &resolved,
        "handshaking",
        Direction::Serverbound,
        "handshake",
        &fields!(
            "protocol_version" => Value::Int(777),
            "server_address" => Value::str("localhost"),
            "server_port" => Value::Int(25565),
            "intent" => Value::Int(1),
        ),
    );
    let chunks = vec![
        Chunk {
            direction: Direction::Serverbound,
            bytes: handshake,
        },
        Chunk {
            direction: Direction::Serverbound,
            bytes: vec![0x05, 0xaa, 0xbb],
        },
        Chunk {
            direction: Direction::Clientbound,
            bytes: vec![0x80],
        },
    ];
    let events = replay(&resolved, TraceOptions::default(), &chunks);
    let pending: Vec<(Direction, usize)> = events
        .iter()
        .filter_map(|event| match event {
            Event::Incomplete {
                direction,
                pending_bytes,
                ..
            } => Some((*direction, *pending_bytes)),
            _ => None,
        })
        .collect();
    assert_eq!(
        pending,
        [(Direction::Serverbound, 3), (Direction::Clientbound, 1)]
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::State { to, .. } if to == "status"
    )));
}

#[test]
fn parse_capture_accepts_both_spellings() {
    let text = "\n  \n{\"direction\":\"toServer\",\"hex\":\"0a 0B\"}\n{\"extra\":1,\"direction\":\"clientbound\",\"hex\":\"ff\"}\n{\"direction\":\"toClient\",\"hex\":\"01\"}\n{\"direction\":\"serverbound\",\"hex\":\"\"}\n";
    let chunks = parse_capture(text).expect("capture");
    assert_eq!(chunks.len(), 4);
    assert_eq!(chunks[0].direction, Direction::Serverbound);
    assert_eq!(chunks[0].bytes, vec![0x0a, 0x0b]);
    assert_eq!(chunks[1].direction, Direction::Clientbound);
    assert_eq!(chunks[1].bytes, vec![0xff]);
    assert_eq!(chunks[2].direction, Direction::Clientbound);
    assert_eq!(chunks[2].bytes, vec![0x01]);
    assert_eq!(chunks[3].direction, Direction::Serverbound);
    assert!(chunks[3].bytes.is_empty());

    let bad_json = parse_capture("\n\n{").unwrap_err();
    assert!(bad_json.contains("line 3"), "{bad_json}");
    let bad_dir = parse_capture("{\"direction\":\"sideways\",\"hex\":\"00\"}\n").unwrap_err();
    assert!(bad_dir.contains("line 1"), "{bad_dir}");
    let odd = parse_capture("{\"direction\":\"serverbound\",\"hex\":\"abc\"}\n").unwrap_err();
    assert!(odd.contains("line 1"), "{odd}");
}

#[test]
fn configuration_acknowledged_returns_to_configuration() {
    let resolved = load_26();
    let mut builder = Builder::new(&resolved);
    builder.push(
        "play",
        Direction::Clientbound,
        "start_configuration",
        &fields!(),
    );
    builder.push(
        "play",
        Direction::Serverbound,
        "configuration_acknowledged",
        &fields!(),
    );
    builder.push(
        "configuration",
        Direction::Clientbound,
        "finish_configuration",
        &fields!(),
    );
    let options = TraceOptions {
        initial_state: "play".to_string(),
        secret: None,
    };
    let events = replay(&resolved, options, &run_chunks(&builder.steps));
    assert_eq!(
        outline(&events),
        [
            "packet clientbound play start_configuration id=120",
            "packet serverbound play configuration_acknowledged id=16",
            "state play->configuration (configuration_acknowledged)",
            "packet clientbound configuration finish_configuration id=3",
        ]
    );
}

#[test]
fn non_minecraft_frame_is_rejected() {
    let source = "\
protocol probe @ v1 {
    frame default {
        length: u8
        id: u8
        payload: rest
    }
    route play.serverbound 0x00 => ping
}
packet ping {
}
";
    let resolved = bytemill::analyze(&bytemill::parse(source).expect("parse")).expect("analyze");
    let err = Tracer::new(&resolved, TraceOptions::default())
        .err()
        .expect("frame");
    assert!(err.to_string().contains("Minecraft-style frames"), "{err}");
}

fn encryption_chunks(resolved: &ResolvedSchema, secret: &[u8; 16]) -> Vec<Chunk> {
    let handshake = encoded_frame(
        resolved,
        "handshaking",
        Direction::Serverbound,
        "handshake",
        &fields!(
            "protocol_version" => Value::Int(777),
            "server_address" => Value::str("localhost"),
            "server_port" => Value::Int(25565),
            "intent" => Value::Int(2),
        ),
    );
    let hello = encoded_frame(
        resolved,
        "login",
        Direction::Clientbound,
        "hello",
        &fields!(
            "server_id" => Value::str(""),
            "public_key_length" => Value::Int(0),
            "public_key" => Value::list(Vec::new()),
            "verify_token_length" => Value::Int(0),
            "verify_token" => Value::list(Vec::new()),
            "should_authenticate" => Value::Bool(false),
        ),
    );
    let response = encoded_frame(
        resolved,
        "login",
        Direction::Serverbound,
        "encryption_response",
        &fields!(
            "secret_length" => Value::Int(0),
            "secret" => Value::list(Vec::new()),
            "verify_token_length" => Value::Int(0),
            "verify_token" => Value::list(Vec::new()),
        ),
    );
    let follow = encoded_frame(
        resolved,
        "login",
        Direction::Serverbound,
        "custom_query_answer",
        &fields!(
            "message_id" => Value::Int(7),
            "has_data" => Value::Bool(false),
        ),
    );
    let disconnect = encoded_frame(
        resolved,
        "login",
        Direction::Clientbound,
        "login_disconnect",
        &fields!("reason" => Value::str("bye")),
    );
    let encrypted_follow = encrypt(secret, &follow);
    let encrypted_disconnect = encrypt(secret, &disconnect);
    let tail = 2.min(response.len());
    let take = 2.min(encrypted_follow.len());
    let mut mixed = response[response.len() - tail..].to_vec();
    mixed.extend_from_slice(&encrypted_follow[..take]);
    vec![
        Chunk {
            direction: Direction::Serverbound,
            bytes: handshake,
        },
        Chunk {
            direction: Direction::Clientbound,
            bytes: hello,
        },
        Chunk {
            direction: Direction::Serverbound,
            bytes: response[..response.len() - tail].to_vec(),
        },
        Chunk {
            direction: Direction::Serverbound,
            bytes: mixed,
        },
        Chunk {
            direction: Direction::Serverbound,
            bytes: encrypted_follow[take..].to_vec(),
        },
        Chunk {
            direction: Direction::Clientbound,
            bytes: encrypted_disconnect,
        },
    ]
}

fn encrypt(secret: &[u8; 16], frame: &[u8]) -> Vec<u8> {
    let mut out = frame.to_vec();
    let mut encryptor = Cfb8Encryptor::new(secret, secret);
    encryptor.encrypt(&mut out);
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

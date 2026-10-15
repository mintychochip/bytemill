//! Hytale connection payloads from the 2026.02.17 decompile.
//!
//! Bytes are built the way `serialize` writes them (little-endian integers,
//! unsigned varint string lengths, big-endian UUID longs, packed variable
//! tails). They are not copied from a capture.

mod common;

use bps::interp::{decode_packet, encode_packet};
use bps::{analyze, load_schema, Value};

fn load() -> bps::ResolvedSchema {
    analyze(&load_schema(&[common::schema_path("hytale/2026_02_17.bps")]).unwrap()).unwrap()
}

fn roundtrip(resolved: &bps::ResolvedSchema, packet: &str, raw: &[u8]) -> bps::Env {
    let decoded = decode_packet(resolved, packet, raw, true).unwrap_or_else(|e| {
        panic!("decode {packet}: {e}");
    });
    let again = encode_packet(resolved, packet, &decoded).unwrap_or_else(|e| {
        panic!("encode {packet}: {e}");
    });
    assert_eq!(again, raw, "{packet} re-encode");
    decoded
}

fn i32le(n: i32) -> [u8; 4] {
    n.to_le_bytes()
}

#[test]
fn routes_match_the_decompiled_ids() {
    let resolved = load();
    let mut routes: Vec<(String, i128, String)> = resolved
        .routes
        .iter()
        .map(|(route, packet)| {
            (
                format!("{}.{}", route.state, route.direction),
                route.packet_id,
                packet.name.clone(),
            )
        })
        .collect();
    routes.sort();
    assert_eq!(
        routes,
        vec![
            ("default.clientbound".into(), 1, "disconnect".into()),
            ("default.clientbound".into(), 2, "ping".into()),
            ("default.serverbound".into(), 0, "connect".into()),
            ("default.serverbound".into(), 1, "disconnect".into()),
            ("default.serverbound".into(), 3, "pong".into()),
        ]
    );
}

#[test]
fn disconnect_reason_is_a_varint_utf8_string() {
    let resolved = load();
    // null bit 0 set, DisconnectType.Disconnect = 0, "hi"
    let raw = [0x01, 0x00, 0x02, b'h', b'i'];
    let decoded = roundtrip(&resolved, "disconnect", &raw);
    assert_eq!(decoded.get("type"), Some(&Value::Int(0)));
    assert_eq!(decoded.get("reason"), Some(&Value::str("hi")));

    let absent = roundtrip(&resolved, "disconnect", &[0x00, 0x01]);
    assert_eq!(absent.get("type"), Some(&Value::Int(1)));
    assert_eq!(absent.get("reason"), None);
}

#[test]
fn ping_and_pong_keep_the_twelve_byte_instant_slot() {
    let resolved = load();
    let mut ping = vec![0x00];
    ping.extend(i32le(7));
    ping.extend([0u8; 12]);
    ping.extend(i32le(1));
    ping.extend(i32le(2));
    ping.extend(i32le(3));
    assert_eq!(ping.len(), 29);
    let decoded = roundtrip(&resolved, "ping", &ping);
    assert_eq!(decoded.get("time_present"), Some(&Value::Bool(false)));
    assert_eq!(decoded.get("time_seconds"), Some(&Value::Int(0)));
    assert_eq!(decoded.get("id"), Some(&Value::Int(7)));
    assert_eq!(decoded.get("last_ping_value_tick"), Some(&Value::Int(3)));

    let mut present = vec![0x01];
    present.extend(i32le(7));
    present.extend(1i64.to_le_bytes());
    present.extend(i32le(2));
    present.extend(i32le(1));
    present.extend(i32le(2));
    present.extend(i32le(3));
    let decoded = roundtrip(&resolved, "ping", &present);
    assert_eq!(decoded.get("time_present"), Some(&Value::Bool(true)));
    assert_eq!(decoded.get("time_seconds"), Some(&Value::Int(1)));
    assert_eq!(decoded.get("time_nanos"), Some(&Value::Int(2)));

    // PongType.Tick = 2, packetQueueSize = -1, instant absent.
    let mut pong = vec![0x00];
    pong.extend(i32le(4));
    pong.extend([0u8; 12]);
    pong.push(2);
    pong.extend((-1i16).to_le_bytes());
    assert_eq!(pong.len(), 20);
    let decoded = roundtrip(&resolved, "pong", &pong);
    assert_eq!(decoded.get("type"), Some(&Value::Int(2)));
    assert_eq!(decoded.get("packet_queue_size"), Some(&Value::Int(-1)));
    assert_eq!(decoded.get("time_present"), Some(&Value::Bool(false)));
}

fn fixed_ascii(text: &str) -> Vec<u8> {
    let mut out = vec![0u8; 20];
    out[..text.len()].copy_from_slice(text.as_bytes());
    out
}

fn var_string(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = vec![bytes.len() as u8];
    out.extend(bytes);
    out
}

/// Canonical Connect.serialize: 66-byte fixed block, then packed variable fields.
fn connect_payload(identity: Option<&str>, referral: Option<(&str, i16)>) -> Vec<u8> {
    let username = var_string("a");
    let identity_bytes = identity.map(var_string);
    let language = var_string("en");
    let mut null_bits = 0u8;
    if identity.is_some() {
        null_bits |= 1;
    }
    if referral.is_some() {
        null_bits |= 4;
    }
    let mut variable = Vec::new();
    let username_offset = 0i32;
    variable.extend(&username);
    let identity_offset = if let Some(raw) = &identity_bytes {
        let at = variable.len() as i32;
        variable.extend(raw);
        at
    } else {
        -1
    };
    let language_offset = variable.len() as i32;
    variable.extend(&language);
    let source_offset = if let Some((host, port)) = referral {
        let at = variable.len() as i32;
        variable.extend(port.to_le_bytes());
        variable.extend(var_string(host));
        at
    } else {
        -1
    };

    let mut raw = Vec::new();
    raw.push(null_bits);
    raw.extend(i32le(0x11));
    raw.extend(i32le(0x22));
    raw.extend(fixed_ascii("0"));
    raw.push(0); // ClientType.Game
    raw.extend(0i64.to_be_bytes());
    raw.extend(1i64.to_be_bytes());
    raw.extend(i32le(username_offset));
    raw.extend(i32le(identity_offset));
    raw.extend(i32le(language_offset));
    raw.extend(i32le(-1)); // referralData absent
    raw.extend(i32le(source_offset));
    assert_eq!(raw.len(), 66, "fixed block");
    raw.extend(variable);
    raw
}

#[test]
fn connect_packs_the_variable_tail_in_serialize_order() {
    let resolved = load();
    let plain = connect_payload(None, None);
    let decoded = roundtrip(&resolved, "connect", &plain);
    assert_eq!(decoded.get("protocol_crc"), Some(&Value::Int(0x11)));
    assert_eq!(decoded.get("protocol_build_number"), Some(&Value::Int(0x22)));
    assert_eq!(decoded.get("client_type"), Some(&Value::Int(0)));
    assert_eq!(decoded.get("uuid_msb"), Some(&Value::Int(0)));
    assert_eq!(decoded.get("uuid_lsb"), Some(&Value::Int(1)));
    assert_eq!(decoded.get("username"), Some(&Value::str("a")));
    assert_eq!(decoded.get("language"), Some(&Value::str("en")));
    assert_eq!(decoded.get("identity_token"), None);
    assert_eq!(decoded.get("referral_source"), None);
    assert_eq!(decoded.get("identity_token_offset"), Some(&Value::Int(-1)));
    assert_eq!(
        decoded.get("client_version"),
        Some(&Value::bytes(fixed_ascii("0")))
    );

    let with_tail = connect_payload(Some("xy"), Some(("localhost", 25565)));
    let decoded = roundtrip(&resolved, "connect", &with_tail);
    assert_eq!(decoded.get("null_bits"), Some(&Value::Int(5)));
    assert_eq!(decoded.get("identity_token"), Some(&Value::str("xy")));
    assert_eq!(decoded.get("username_offset"), Some(&Value::Int(0)));
    assert_eq!(decoded.get("identity_token_offset"), Some(&Value::Int(2)));
    assert_eq!(decoded.get("language_offset"), Some(&Value::Int(5)));
    let source = decoded
        .get("referral_source")
        .and_then(|v| match v {
            Value::Map(m) => Some(m.clone()),
            _ => None,
        })
        .expect("referral source");
    assert_eq!(source.get("port"), Some(&Value::Int(25565)));
    assert_eq!(source.get("host"), Some(&Value::str("localhost")));
}

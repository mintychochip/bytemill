//! Ports tests/test_play_packets.py.

use bps::ast::TypeRef;
use bps::interp::encode_bare;
use bps::interp::{decode_frame_payload};
use bps::Value;

mod common;

/// Length-prefix a frame whose length and id use the imported `varint` schema.
fn frame(resolved: &bps::ResolvedSchema, pid: i128, payload: &[u8]) -> Vec<u8> {
    fn vint(resolved: &bps::ResolvedSchema, n: i128) -> Vec<u8> {
        let varint = TypeRef { name: "varint".to_string(), args: Vec::new(), transforms: Vec::new() };
        encode_bare(&varint, &Value::Int(n), resolved, &bps::Env::new())
            .unwrap()
            .unwrap()
    }
    let mut body = vint(resolved, pid);
    body.extend_from_slice(payload);
    let mut out = vint(resolved, body.len() as i128);
    out.extend_from_slice(&body);
    out
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn block_update() {
    let resolved = common::load_minecraft().unwrap();
    let mut payload = hex("4607632c15b4833f");
    payload.extend_from_slice(&hex("01"));
    let raw = frame(&resolved, 0x08, &payload);
    let (pid, out) = decode_frame_payload(&resolved, "play", "clientbound", &raw).unwrap();
    assert_eq!(pid, 0x08);
    let fields = out.get("fields").expect("fields");
    let location = fields.get("location").expect("location");
    assert_eq!(location.get("x"), Some(&Value::Int(18357644)));
    assert_eq!(location.get("y"), Some(&Value::Int(831)));
    assert_eq!(fields.get("block_id"), Some(&Value::Int(1)));
}

#[test]
fn animate() {
    let resolved = common::load_minecraft().unwrap();
    let raw = frame(&resolved, 0x02, &hex("ac0202"));
    let (_, out) = decode_frame_payload(&resolved, "play", "clientbound", &raw).unwrap();
    assert_eq!(out.get("packet"), Some(&Value::str("animate")));
    let fields = out.get("fields").expect("fields");
    assert_eq!(fields.get("entity_id"), Some(&Value::Int(300)));
    assert_eq!(fields.get("animation"), Some(&Value::Int(2)));
}

#[test]
fn game_event() {
    let resolved = common::load_minecraft().unwrap();
    let mut payload = vec![1u8];
    payload.extend_from_slice(&1.5f32.to_be_bytes());
    let (_, out) = decode_frame_payload(&resolved, "play", "clientbound", &frame(&resolved, 0x27, &payload)).unwrap();
    let fields = out.get("fields").expect("fields");
    assert_eq!(fields.get("event"), Some(&Value::Int(1)));
    assert_eq!(fields.get("value"), Some(&Value::Float(1.5)));
}

#[test]
fn bundle_delimiter_is_empty() {
    let resolved = common::load_minecraft().unwrap();
    let (_, out) = decode_frame_payload(&resolved, "play", "clientbound", &frame(&resolved, 0x00, b"")).unwrap();
    assert_eq!(out.get("packet"), Some(&Value::str("bundle_delimiter")));
    match out.get("fields") {
        Some(Value::Map(m)) => assert!(m.is_empty()),
        other => panic!("expected empty fields, got {other:?}"),
    }
}

#[test]
fn set_time_clock_array() {
    let resolved = common::load_minecraft().unwrap();
    // world_age i64be, clocks_count varint, clocks[] of
    // { clock_id varint, time varlong, fractional_time f32be, rate f32be }
    let mut payload = 100i64.to_be_bytes().to_vec();
    payload.extend_from_slice(&[1]);
    payload.extend_from_slice(&[7]);
    payload.extend_from_slice(&hex("c801"));
    payload.extend_from_slice(&0.5f32.to_be_bytes());
    payload.extend_from_slice(&1.0f32.to_be_bytes());
    let (_, out) = decode_frame_payload(&resolved, "play", "clientbound", &frame(&resolved, 0x73, &payload)).unwrap();
    let fields = out.get("fields").expect("fields");
    assert_eq!(fields.get("world_age"), Some(&Value::Int(100)));
    assert_eq!(fields.get("clocks_count"), Some(&Value::Int(1)));
    let Value::List(clocks) = fields.get("clocks").expect("clocks") else {
        panic!("expected list")
    };
    let clock = &clocks[0];
    assert_eq!(clock.get("clock_id"), Some(&Value::Int(7)));
    assert_eq!(clock.get("time"), Some(&Value::Int(200)));
    assert_eq!(clock.get("fractional_time"), Some(&Value::Float(0.5)));
    assert_eq!(clock.get("rate"), Some(&Value::Float(1.0)));
}

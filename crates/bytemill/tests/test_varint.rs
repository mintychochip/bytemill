//! Ports tests/test_varint.py.

mod common;

use bytemill::interp::{decode_frame_payload, decode_packet, encode_packet};
use bytemill::Value;

#[test]
fn varint_25565_roundtrip() {
    let merged = common::load_with_extra("packet varint_probe { n: varint }").unwrap();
    let mut value = bytemill::Env::new();
    value.insert("n", Value::Int(25565));
    let enc = encode_packet(&merged, "varint_probe", &value).unwrap();
    assert_eq!(enc, hex_bytes("ddc701"));
    let dec = decode_packet(&merged, "varint_probe", &enc, false).unwrap();
    assert_eq!(dec.get("n"), Some(&Value::Int(25565)));
}

#[test]
fn handshake_frame() {
    let resolved = common::load_minecraft().unwrap();
    let frame = hex_bytes("10008906096c6f63616c686f737463dd01");
    let (pid, out) = decode_frame_payload(&resolved, "handshaking", "serverbound", &frame).unwrap();
    assert_eq!(pid, 0);
    assert_eq!(out.get("packet"), Some(&Value::str("handshake")));
    let fields = match out.get("fields") {
        Some(Value::Map(m)) => m,
        _ => panic!("expected fields"),
    };
    assert_eq!(fields.get("protocol_version"), Some(&Value::Int(777)));
    assert_eq!(fields.get("server_address"), Some(&Value::str("localhost")));
    assert_eq!(fields.get("server_port"), Some(&Value::Int(25565)));
    assert_eq!(fields.get("intent"), Some(&Value::Int(1)));
}

#[test]
fn position_decode() {
    use bytemill::ast::Schema;
    let core = bytemill::load_schema(&[common::schema_path("minecraft_core.bml")]).unwrap();
    let extra = bytemill::parse("packet pos { p: position }").unwrap();
    let merged = bytemill::analyze(&Schema {
        encodings: core.encodings,
        enums: Vec::new(),
        packets: extra.packets,
        protocols: Vec::new(),
        imports: Vec::new(),
    })
    .unwrap();
    let raw = hex_bytes("4607632c15b4833f");
    let dec = decode_packet(&merged, "pos", &raw, false).unwrap();
    let p = match dec.get("p") {
        Some(Value::Map(m)) => m,
        _ => panic!("expected position map"),
    };
    assert_eq!(p.get("x"), Some(&Value::Int(18357644)));
    assert_eq!(p.get("z"), Some(&Value::Int(-20882616)));
    assert_eq!(p.get("y"), Some(&Value::Int(831)));
}

#[test]
fn entity_metadata_sample() {
    use bytemill::ast::Schema;
    let core = bytemill::load_schema(&[common::schema_path("minecraft_core.bml")]).unwrap();
    let extra = bytemill::parse("packet md { m: entity_metadata }").unwrap();
    let merged = bytemill::analyze(&Schema {
        encodings: core.encodings,
        enums: Vec::new(),
        packets: extra.packets,
        protocols: Vec::new(),
        imports: Vec::new(),
    })
    .unwrap();
    let raw = hex_bytes("0000010201ac02ff");
    let dec = decode_packet(&merged, "md", &raw, false).unwrap();
    let entries = match dec.get("m").and_then(|v| v.get("entries")) {
        Some(Value::List(l)) => l,
        _ => panic!("expected entries list"),
    };
    assert_eq!(entries.len(), 2);
    match &entries[1] {
        Value::Map(m) => {
            assert_eq!(m.get("kind"), Some(&Value::Int(1)));
            assert_eq!(m.get("value"), Some(&Value::Int(300)));
        }
        other => panic!("expected entry map, got {other:?}"),
    }
}

pub fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

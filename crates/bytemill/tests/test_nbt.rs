//! Ports tests/test_nbt.py.

use bytemill::ast::Schema;
use bytemill::interp::{decode_packet, encode_packet};
use bytemill::{analyze, load_schema, parse, Value};

fn load_nbt() -> bytemill::ResolvedSchema {
    let core = load_schema(&[concat!(env!("CARGO_MANIFEST_DIR"), "/../../schemas/minecraft_core.bps")])
        .unwrap();
    let probe = parse("packet nb { t: nbt }").unwrap();
    analyze(&Schema {
        encodings: core.encodings,
        enums: Vec::new(),
        packets: probe.packets,
        protocols: Vec::new(),
        imports: Vec::new(),
    })
    .unwrap()
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn network_nbt_roundtrip() {
    let resolved = load_nbt();
    // { "a": "hi" (string), "n": 7 (int), "l": [1, 2] (int list) }
    let raw = hex(concat!(
        "0a",
        "0800016100026869",
        "0300016e00000007",
        "0900016c",
        "0300000002",
        "0000000100000002",
        "00",
    ));
    let out = decode_packet(&resolved, "nb", &raw, true).unwrap();
    let t = out.get("t").and_then(|v| v.get("tag_type"));
    assert_eq!(t, Some(&Value::Int(10)));
    let entries = out
        .get("t")
        .and_then(|v| v.get("value"))
        .and_then(|v| v.get("entries"));
    let Value::List(entries) = entries.expect("entries") else {
        panic!("expected list")
    };
    assert_eq!(entries.len(), 3);
    match &entries[0] {
        Value::Map(m) => {
            assert_eq!(m.get("tag_type"), Some(&Value::Int(8)));
            assert_eq!(m.get("name"), Some(&Value::str("a")));
            assert_eq!(m.get("value"), Some(&Value::str("hi")));
        }
        other => panic!("expected map, got {other:?}"),
    }
    match &entries[1] {
        Value::Map(m) => {
            assert_eq!(m.get("tag_type"), Some(&Value::Int(3)));
            assert_eq!(m.get("name"), Some(&Value::str("n")));
            assert_eq!(m.get("value"), Some(&Value::Int(7)));
        }
        other => panic!("expected map, got {other:?}"),
    }
    match &entries[2] {
        Value::Map(m) => {
            assert_eq!(m.get("tag_type"), Some(&Value::Int(9)));
            let items = m.get("value").and_then(|v| v.get("items"));
            let Value::List(items) = items.expect("items") else {
                panic!("expected list")
            };
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].get("v"), Some(&Value::Int(1)));
            assert_eq!(items[1].get("v"), Some(&Value::Int(2)));
        }
        other => panic!("expected map, got {other:?}"),
    }
    let back = encode_packet(&resolved, "nb", &out).unwrap();
    assert_eq!(back, raw);
}

#[test]
fn an_absent_tag_is_a_lone_tag_end() {
    let resolved = load_nbt();
    let out = decode_packet(&resolved, "nb", &[0x00], true).unwrap();
    assert_eq!(out.get("t").and_then(|v| v.get("tag_type")), Some(&Value::Int(0)));
    assert_eq!(encode_packet(&resolved, "nb", &out).unwrap(), vec![0x00]);
}

#[test]
fn an_empty_compound_is_its_id_then_its_end() {
    let resolved = load_nbt();
    let out = decode_packet(&resolved, "nb", &hex("0a00"), true).unwrap();
    let t = out.get("t").expect("t");
    assert_eq!(t.get("tag_type"), Some(&Value::Int(10)));
    let value = t.get("value").expect("value");
    let Value::Map(m) = value else {
        panic!("expected map")
    };
    assert_eq!(m.len(), 1);
    match m.get("entries") {
        Some(Value::List(l)) => assert!(l.is_empty()),
        other => panic!("expected empty list, got {other:?}"),
    }
    assert_eq!(encode_packet(&resolved, "nb", &out).unwrap(), hex("0a00"));
}

//! Ports tests/test_std_types.py.

mod common;

use bytemill::ast::Schema;
use bytemill::interp::{decode_packet, encode_packet};
use bytemill::{analyze, load_schema, parse, Value};

fn hex(s: &str) -> Vec<u8> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..cleaned.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).unwrap())
        .collect()
}

fn repo_file(rel: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../").to_string() + rel;
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn u16be_is_two_bytes_built_from_u8() {
    let resolved = analyze(&parse(
        r#"
        encoding u16be = {
            hi: u8 = (value >> 8) & 0xFF
            lo: u8 = value & 0xFF
        } = (hi << 8) | lo
        packet probe { n: u16be }
        "#,
    )
    .unwrap())
    .unwrap();
    let mut value = bytemill::Env::new();
    value.insert("n", Value::Int(0x1234));
    let encoded = encode_packet(&resolved, "probe", &value).unwrap();
    assert_eq!(encoded, hex("1234"));
    let decoded = decode_packet(&resolved, "probe", &encoded, false).unwrap();
    assert_eq!(decoded.get("n"), Some(&Value::Int(0x1234)));
}

#[test]
fn signed_widths_roundtrip_negative_one() {
    let source = repo_file("schemas/std/integers.bml")
        + "\npacket probe { a: i8  b: i16be  c: i32be  d: i64be  e: u16le }\n";
    let resolved = analyze(&parse(&source).unwrap()).unwrap();
    let mut value = bytemill::Env::new();
    for k in ["a", "b", "c", "d"] {
        value.insert(k, Value::Int(-1));
    }
    value.insert("e", Value::Int(0x1234));
    let encoded = encode_packet(&resolved, "probe", &value).unwrap();
    assert_eq!(
        encoded,
        hex("ff ffff ffffffff ffffffffffffffff 3412")
    );
    let decoded = decode_packet(&resolved, "probe", &encoded, false).unwrap();
    assert_eq!(decoded, value);
}

#[test]
fn varint_matches_wiki_samples() {
    let source = repo_file("schemas/std/varint.bml") + "packet probe { n: varint  m: varlong }\n";
    let resolved = analyze(&parse(&source).unwrap()).unwrap();
    let mut v = bytemill::Env::new();
    v.insert("n", Value::Int(25565));
    v.insert("m", Value::Int(0));
    let enc = encode_packet(&resolved, "probe", &v).unwrap();
    assert_eq!(&enc[..3], &hex("ddc701")[..]);
    let decoded = decode_packet(&resolved, "probe", &enc, false).unwrap();
    assert_eq!(decoded.get("n"), Some(&Value::Int(25565)));

    let mut neg = bytemill::Env::new();
    neg.insert("n", Value::Int(-1));
    neg.insert("m", Value::Int(-1));
    let neg = encode_packet(&resolved, "probe", &neg).unwrap();
    assert_eq!(&neg[..5], &hex("ffffffff0f")[..]);
    assert_eq!(&neg[5..], &hex("ffffffffffffffffff01")[..]);
    let decoded = decode_packet(&resolved, "probe", &neg, false).unwrap();
    assert_eq!(decoded.get("n"), Some(&Value::Int(-1)));
    assert_eq!(decoded.get("m"), Some(&Value::Int(-1)));
}

#[test]
fn overlong_varint_is_rejected() {
    let source = repo_file("schemas/std/varint.bml") + "packet probe { n: varint }\n";
    let resolved = analyze(&parse(&source).unwrap()).unwrap();
    let err = decode_packet(&resolved, "probe", &hex("ffffffffff"), false).unwrap_err();
    assert_eq!(err.kind, bytemill::errors::ErrorKind::Decode, "{err}");
}

#[test]
fn minecraft_core_imports_std_schemas() {
    let loaded = load_schema(&[concat!(env!("CARGO_MANIFEST_DIR"), "/../../schemas/minecraft_core.bml")])
        .unwrap();
    let names: std::collections::HashSet<&str> =
        loaded.encodings.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains("varint"));
    assert!(names.contains("u16be"));
    assert!(names.contains("boolean"));
    assert!(!names.contains("leb128"));
}

#[test]
fn leb128_is_not_a_type() {
    let schema = parse("encoding bad = leb128(32, signed)\n").unwrap();
    let err = analyze(&schema).unwrap_err();
    assert_eq!(err.kind, bytemill::errors::ErrorKind::Resolve, "{err}");
}

#[test]
fn imported_varint_feeds_the_handshake_port() {
    let loaded = load_schema(&[concat!(env!("CARGO_MANIFEST_DIR"), "/../../schemas/minecraft_core.bml")])
        .unwrap();
    let extra = parse("packet probe { n: varint }").unwrap();
    let resolved = analyze(&Schema {
        encodings: loaded.encodings.iter().cloned().chain(extra.encodings).collect(),
        enums: loaded.enums,
        packets: loaded.packets.iter().cloned().chain(extra.packets).collect(),
        protocols: loaded.protocols,
        imports: Vec::new(),
    })
    .unwrap();
    let mut value = bytemill::Env::new();
    value.insert("n", Value::Int(25565));
    let encoded = encode_packet(&resolved, "probe", &value).unwrap();
    assert_eq!(encoded, hex("ddc701"));
    let decoded = decode_packet(&resolved, "probe", &encoded, false).unwrap();
    assert_eq!(decoded.get("n"), Some(&Value::Int(25565)));
}

static TEMP_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "bml-std-{tag}-{}-{n}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const U24_PROBE: &str = "
    encoding u24be = {
        b0: u8 = (value >> 16) & 0xFF
        b1: u8 = (value >> 8) & 0xFF
        b2: u8 = value & 0xFF
    } = (b0 << 16) | (b1 << 8) | b2
    packet probe { x: u24be }
";

fn std_resolved(source: &str) -> bytemill::ResolvedSchema {
    let std = repo_file("schemas/std/integers.bml") + &repo_file("schemas/std/varint.bml");
    analyze(&parse(&(std + source)).unwrap()).unwrap()
}

#[test]
fn rust_emitter_rejects_unlowerable_scalar() {
    let resolved = std_resolved(U24_PROBE);
    let dir = temp_dir("rust-reject");
    let err = bytemill::emit_rust::emit_rust(&resolved, &dir.join("crate")).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(err.kind, bytemill::errors::ErrorKind::Emit, "{err}");
    assert!(err.to_string().contains("u24be"), "{err}");
}

// Deviation from the Python test: the Rust `emit_rstom` takes an extra
// `rstom_path` argument (it only affects the generated `Cargo.toml` text),
// so `"../rstom"` is passed; rejection happens before the table is used.
#[test]
fn rstom_emitter_rejects_unlowerable_scalar() {
    let resolved = std_resolved(U24_PROBE);
    let dir = temp_dir("rstom-reject");
    let table = common::rstom_tables::probe_table();
    let err =
        bytemill::emit_rstom::emit_rstom(&resolved, &dir.join("crate"), &table, "../rstom").unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(err.kind, bytemill::errors::ErrorKind::Emit, "{err}");
    assert!(err.to_string().contains("u24be"), "{err}");
}

#[test]
fn a_scalar_reached_through_another_encoding_is_rejected() {
    let source = U24_PROBE.replace(
        "packet probe { x: u24be }",
        "encoding wrapper = { inner: u24be }\n    packet probe { w: wrapper }",
    );
    let resolved = std_resolved(&source);
    let dir = temp_dir("rust-nested");
    let err = bytemill::emit_rust::emit_rust(&resolved, &dir.join("crate")).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(err.kind, bytemill::errors::ErrorKind::Emit, "rust: {err}");
    assert!(err.to_string().contains("u24be"), "rust: {err}");
    let dir = temp_dir("rstom-nested");
    let table = common::rstom_tables::probe_table();
    let err =
        bytemill::emit_rstom::emit_rstom(&resolved, &dir.join("crate"), &table, "../rstom").unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(err.kind, bytemill::errors::ErrorKind::Emit, "rstom: {err}");
    assert!(err.to_string().contains("u24be"), "rstom: {err}");
}

#[test]
fn rstom_keeps_std_scalar_default_arm_as_one_value() {
    let body = "packet probe { kind: varint  value: match kind { 0 => i8, _ => varint } }";
    let source = common::rstom_tables::probe_source(&common::repo_root(), body);
    let dir = temp_dir("default-arm");
    let schema = dir.join("probe.bml");
    std::fs::write(&schema, &source).unwrap();
    let resolved = analyze(&load_schema(&[schema]).unwrap()).unwrap();
    let table = common::rstom_tables::probe_table();
    bytemill::emit_rstom::emit_rstom(&resolved, &dir.join("crate"), &table, "../rstom").unwrap();
    let text =
        std::fs::read_to_string(dir.join("crate/src/server/play/probe_packet.rs")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!text.contains("pub b0"), "{text}");
}

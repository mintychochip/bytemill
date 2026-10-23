//! Interpreter semantics the typed printers already assumed.
//!
//! `sign_extend` is the low N bits read as two's complement. A block encodes
//! through its own fields: a string-shaped length uses that field's type, and
//! a `uuid` uses the halves it declares, not a name.

use bytemill::interp::{decode_packet, encode_packet};
use bytemill::{Env, Value, analyze, load_schema, parse};

fn resolved(source: &str) -> bytemill::ResolvedSchema {
    analyze(&parse(source).unwrap()).unwrap()
}

fn hex(s: &str) -> Vec<u8> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..cleaned.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).unwrap())
        .collect()
}

fn one(name: &str, value: Value) -> Env {
    let mut env = Env::new();
    env.insert(name, value);
    env
}

fn int_field(resolved: &bytemill::ResolvedSchema, packet: &str, raw: &[u8]) -> i128 {
    let decoded = decode_packet(resolved, packet, raw, true).unwrap();
    match decoded.get("n") {
        Some(Value::Int(n)) => *n,
        other => panic!("{packet} decoded {other:?}"),
    }
}

const ZIGZAG: &str = r#"
encoding varuint32 = {
    b0: u8 = (value & 0x7F) | (((value >> 7) != 0) ? 0x80 : 0)
    b1: u8 if (b0 & 0x80) != 0 = ((value >> 7) & 0x7F) | (((value >> 14) != 0) ? 0x80 : 0)
    b2: u8 if (b1 & 0x80) != 0 = ((value >> 14) & 0x7F) | (((value >> 21) != 0) ? 0x80 : 0)
    b3: u8 if (b2 & 0x80) != 0 = ((value >> 21) & 0x7F) | (((value >> 28) != 0) ? 0x80 : 0)
    b4: u8 if (b3 & 0x80) != 0 = (value >> 28) & 0x0F
    require (b4 & 0x80) == 0
} = (b0 & 0x7F) | ((b1 & 0x7F) << 7) | ((b2 & 0x7F) << 14) | ((b3 & 0x7F) << 21) | ((b4 & 0x0F) << 28)

encoding varint32 = {
    raw: varuint32 = ((value << 1) ^ (value >> 31)) & 0xFFFFFFFF
} = sign_extend((raw >> 1) ^ (0 - (raw & 1)), 32)

packet probe { n: varint32 }
"#;

#[test]
fn zigzag_varint32_round_trips_the_i32_range() {
    let resolved = resolved(ZIGZAG);
    let cases: &[(&str, i128)] = &[
        ("00", 0),
        ("01", -1),
        ("02", 1),
        ("03", -2),
        ("feffffff0f", 2_147_483_647),
        ("ffffffff0f", -2_147_483_648),
    ];
    for (raw, n) in cases {
        let bytes = hex(raw);
        assert_eq!(int_field(&resolved, "probe", &bytes), *n, "decode {raw}");
        let encoded = encode_packet(&resolved, "probe", &one("n", Value::Int(*n))).unwrap();
        assert_eq!(encoded, bytes, "encode {n}");
    }
}

fn sext(args: &str) -> i128 {
    let source = format!(
        "encoding sext(v = 0, bits = 1) {{\n}} = sign_extend(v, bits)\npacket probe {{ n: sext({args}) }}\n"
    );
    int_field(&resolved(&source), "probe", &[])
}

fn sext_err(args: &str) -> String {
    let source = format!(
        "encoding sext(v = 0, bits = 1) {{\n}} = sign_extend(v, bits)\npacket probe {{ n: sext({args}) }}\n"
    );
    decode_packet(&resolved(&source), "probe", &[], true)
        .unwrap_err()
        .to_string()
}

#[test]
fn sign_extend_truncates_to_the_low_bits() {
    // Negative, and already inside the signed range: low 32 bits of -2 are -2,
    // not -2 - 2^32.
    assert_eq!(sext("0 - 2, 32"), -2);
    // Negative whose magnitude does not fit: low 8 bits of -130 are 0x7E.
    assert_eq!(sext("0 - 130, 8"), 126);
    // At or above 2^bits the high bits are discarded.
    assert_eq!(sext("1 << 8, 8"), 0);
    assert_eq!(sext("(1 << 32) + 5, 32"), 5);
    // Exactly the sign bit.
    assert_eq!(sext("1 << 7, 8"), -128);
    assert_eq!(sext("1 << 31, 32"), -2_147_483_648);
    // One bit: 0 stays 0, a set low bit is -1.
    assert_eq!(sext("0, 1"), 0);
    assert_eq!(sext("1, 1"), -1);
    assert_eq!(sext("2, 1"), 0);
    assert_eq!(sext("0 - 1, 1"), -1);
    // 128 must not shift by 128 (that does not fit in `u128`).
    assert_eq!(sext("0 - 1, 128"), -1);
    assert_eq!(sext("1, 128"), 1);
    assert_eq!(sext("1 << 100, 128"), 1i128 << 100);
    assert_eq!(sext_err("1, 0"), "sign_extend bits out of range: 0");
    assert_eq!(sext_err("1, 129"), "sign_extend bits out of range: 129");
    assert_eq!(sext_err("1, 0 - 1"), "sign_extend bits out of range: -1");
}

const BEDROCK: &str = r#"
encoding varuint32 = {
    b0: u8 = (value & 0x7F) | (((value >> 7) != 0) ? 0x80 : 0)
    b1: u8 if (b0 & 0x80) != 0 = ((value >> 7) & 0x7F) | (((value >> 14) != 0) ? 0x80 : 0)
    b2: u8 if (b1 & 0x80) != 0 = ((value >> 14) & 0x7F) | (((value >> 21) != 0) ? 0x80 : 0)
    b3: u8 if (b2 & 0x80) != 0 = ((value >> 21) & 0x7F) | (((value >> 28) != 0) ? 0x80 : 0)
    b4: u8 if (b3 & 0x80) != 0 = (value >> 28) & 0x0F
    require (b4 & 0x80) == 0
} = (b0 & 0x7F) | ((b1 & 0x7F) << 7) | ((b2 & 0x7F) << 14) | ((b3 & 0x7F) << 21) | ((b4 & 0x0F) << 28)

encoding string = {
    byte_len: varuint32
    value: bytes(byte_len) as utf8
}
encoding label = { ...string }
packet named { name: string }
packet labeled { name: label }
"#;

fn text_of(resolved: &bytemill::ResolvedSchema, packet: &str, raw: &[u8]) -> String {
    let decoded = decode_packet(resolved, packet, raw, true).unwrap();
    match decoded.get("name") {
        Some(Value::Str(s)) => s.to_string(),
        other => panic!("{packet} decoded {other:?}"),
    }
}

#[test]
fn a_varuint32_string_encodes_its_own_length() {
    let resolved = resolved(BEDROCK);
    let hi = one("name", Value::str("hi"));
    assert_eq!(
        encode_packet(&resolved, "named", &hi).unwrap(),
        hex("026869")
    );
    assert_eq!(text_of(&resolved, "named", &hex("026869")), "hi");
    // A spread of that string is the same wire, not a missing `varint`.
    assert_eq!(
        encode_packet(&resolved, "labeled", &hi).unwrap(),
        hex("026869")
    );
    assert_eq!(text_of(&resolved, "labeled", &hex("026869")), "hi");

    let long = "a".repeat(200);
    let mut expect = vec![0xc8, 0x01];
    expect.extend(long.as_bytes());
    let encoded =
        encode_packet(&resolved, "named", &one("name", Value::str(long.clone()))).unwrap();
    assert_eq!(encoded, expect);
    assert_eq!(text_of(&resolved, "named", &encoded), long);
}

const PREFIXES: &str = r#"
encoding u16be = {
    b0: u8 = (value >> 8) & 0xFF
    b1: u8 = value & 0xFF
} = (b0 << 8) | b1

encoding wide_text = {
    byte_len: u16be
    value: bytes(byte_len) as utf8
}
encoding short_text = {
    byte_len: u8
    value: bytes(byte_len) as utf8
}
encoding wide_label = { ...wide_text }
packet wide { name: wide_text }
packet short { name: short_text }
packet labeled { name: wide_label }
"#;

#[test]
fn a_string_length_uses_the_declared_prefix() {
    let resolved = resolved(PREFIXES);
    let hi = one("name", Value::str("hi"));
    assert_eq!(
        encode_packet(&resolved, "wide", &hi).unwrap(),
        hex("00026869")
    );
    assert_eq!(
        encode_packet(&resolved, "short", &hi).unwrap(),
        hex("026869")
    );
    assert_eq!(
        encode_packet(&resolved, "labeled", &hi).unwrap(),
        hex("00026869")
    );
    assert_eq!(text_of(&resolved, "wide", &hex("00026869")), "hi");
    assert_eq!(text_of(&resolved, "short", &hex("026869")), "hi");
}

const LE_UUID: &str = r#"
encoding u64le = {
    b0: u8 = value & 0xFF
    b1: u8 = (value >> 8) & 0xFF
    b2: u8 = (value >> 16) & 0xFF
    b3: u8 = (value >> 24) & 0xFF
    b4: u8 = (value >> 32) & 0xFF
    b5: u8 = (value >> 40) & 0xFF
    b6: u8 = (value >> 48) & 0xFF
    b7: u8 = (value >> 56) & 0xFF
} = (b7 << 56) | (b6 << 48) | (b5 << 40) | (b4 << 32) | (b3 << 24) | (b2 << 16) | (b1 << 8) | b0

encoding uuid = {
    msb: u64le
    lsb: u64le
}
packet probe { id: uuid }
"#;

#[test]
fn a_uuid_encodes_through_its_declared_halves() {
    let resolved = resolved(LE_UUID);
    let mut halves = Env::new();
    halves.insert("msb", Value::Int(1));
    halves.insert("lsb", Value::Int(2));
    let encoded = encode_packet(&resolved, "probe", &one("id", Value::map(halves))).unwrap();
    assert_eq!(encoded, hex("0100000000000000 0200000000000000"));
    let decoded = decode_packet(&resolved, "probe", &encoded, true).unwrap();
    let id = decoded.get("id").unwrap();
    assert_eq!(id.get("msb"), Some(&Value::Int(1)));
    assert_eq!(id.get("lsb"), Some(&Value::Int(2)));
}

#[test]
fn minecraft_login_packets_match_the_released_encoder() {
    // Hex from the released `bml encode` (0.2.0). A string plus a uuid, and an
    // identifier (a spread of `string`), must stay byte-identical.
    let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../"))
        .join("schemas/minecraft_26_3_login.bml");
    let resolved = analyze(&load_schema(&[path]).unwrap()).unwrap();

    let mut uuid = Env::new();
    uuid.insert("msb", Value::Int(1));
    uuid.insert("lsb", Value::Int(2));
    let mut push = Env::new();
    push.insert("uuid", Value::map(uuid));
    push.insert("url", Value::str("hi"));
    push.insert("hash", Value::str("ab"));
    push.insert("forced", Value::Bool(false));
    push.insert("has_prompt", Value::Bool(false));
    let encoded = encode_packet(&resolved, "resource_pack_push", &push).unwrap();
    assert_eq!(
        encoded,
        hex("000000000000000100000000000000020268690261620000")
    );

    let key = encode_packet(
        &resolved,
        "cookie_request",
        &one("key", Value::str("minecraft:stone")),
    )
    .unwrap();
    assert_eq!(key, hex("0f6d696e6563726166743a73746f6e65"));
    let decoded = decode_packet(&resolved, "cookie_request", &key, true).unwrap();
    assert_eq!(decoded.get("key"), Some(&Value::str("minecraft:stone")));
}

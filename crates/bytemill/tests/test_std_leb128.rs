//! Tests for the std LEB128 encodings (varuint* unsigned, varint* zigzag).
//! Uses import "std/leb128.bml" via a temp tree so resolution exercises the
//! import machinery the same way other std tests and tree tests do.

mod common;

use bytemill::interp::{decode_packet, encode_packet};
use bytemill::{Value, analyze, load_schema};
use std::collections::HashSet;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn one(name: &str, v: Value) -> bytemill::Env {
    let mut e = bytemill::Env::new();
    e.insert(name, v);
    e
}

/// Set up a temp tree containing std/leb128.bml and a probe.bml that imports
/// it relatively, then load and analyze. Cleanup is best-effort.
fn leb_resolved() -> bytemill::ResolvedSchema {
    let dir = common::temp_tree("leb128");
    std::fs::create_dir_all(dir.join("std")).unwrap();
    let src = common::schema_path("std/leb128.bml");
    std::fs::copy(src, dir.join("std/leb128.bml")).unwrap();
    let main = common::write_file(
        &dir,
        "probe.bml",
        r#"
import "std/leb128.bml"
packet u32p { v: varuint32 }
packet u64p { v: varuint64 }
packet i32p { v: varint32 }
packet i64p { v: varint64 }
"#,
    );
    let schema = load_schema(&[main]).unwrap();
    let resolved = analyze(&schema).unwrap();
    common::cleanup(&dir);
    resolved
}

#[test]
fn leb128_declares_the_four_names_and_no_leb128() {
    let dir = common::temp_tree("leb128-names");
    std::fs::create_dir_all(dir.join("std")).unwrap();
    let src = common::schema_path("std/leb128.bml");
    std::fs::copy(src, dir.join("std/leb128.bml")).unwrap();
    let main = common::write_file(
        &dir,
        "p.bml",
        r#"
import "std/leb128.bml"
packet p { a: varuint32 }
"#,
    );
    let loaded = load_schema(&[main]).unwrap();
    let names: HashSet<&str> = loaded.encodings.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains("varuint32"));
    assert!(names.contains("varuint64"));
    assert!(names.contains("varint32"));
    assert!(names.contains("varint64"));
    assert!(!names.contains("leb128"));
    common::cleanup(&dir);
}

#[test]
fn varuint32_roundtrips_listed_values_and_rejects_overlong_or_truncated() {
    let r = leb_resolved();
    let cases: &[(&str, i128)] = &[
        ("00", 0),
        ("7f", 127),
        ("8001", 128),
        ("ac02", 300),
        ("808001", 16384),
        ("ffffffff0f", 4_294_967_295),
    ];
    for (raw, n) in cases {
        let b = hex(raw);
        let dec = decode_packet(&r, "u32p", &b, false).unwrap();
        assert_eq!(dec.get("v"), Some(&Value::Int(*n)), "decode {raw}");
        let enc = encode_packet(&r, "u32p", &one("v", Value::Int(*n))).unwrap();
        assert_eq!(enc, b, "encode {n}");
    }
    // 6th continuation byte (b4 has 0x80 so would require b5) -> decode error
    let over = hex("808080808080");
    let err = decode_packet(&r, "u32p", &over, false).unwrap_err();
    assert_eq!(err.kind, bytemill::errors::ErrorKind::Decode, "{err}");
    // truncated (needs the b1 but only one byte)
    let trunc = hex("80");
    let err = decode_packet(&r, "u32p", &trunc, false).unwrap_err();
    assert_eq!(err.kind, bytemill::errors::ErrorKind::Decode, "{err}");
}

#[test]
fn varuint32_accepts_noncanonical_on_decode_but_encodes_canonically() {
    let r = leb_resolved();
    // 8000 is 0 with an unnecessary continuation byte (b0=80, b1=00)
    let noncan = hex("8000");
    let dec = decode_packet(&r, "u32p", &noncan, false).unwrap();
    assert_eq!(dec.get("v"), Some(&Value::Int(0)));
    // encode of 0 must be the canonical single-byte form
    let enc = encode_packet(&r, "u32p", &one("v", Value::Int(0))).unwrap();
    assert_eq!(enc, hex("00"));
}

#[test]
fn varuint64_roundtrips_and_rejects_11th_byte_or_truncated() {
    let r = leb_resolved();
    // 2^64-1
    let max = hex("ffffffffffffffffff01");
    let dec = decode_packet(&r, "u64p", &max, false).unwrap();
    assert_eq!(dec.get("v"), Some(&Value::Int(u64::MAX as i128)));
    let enc = encode_packet(&r, "u64p", &one("v", Value::Int(u64::MAX as i128))).unwrap();
    assert_eq!(enc, max);

    // 11th byte would be b9 having 0x80, requiring b10
    let over = hex("8080808080808080808080");
    let err = decode_packet(&r, "u64p", &over, false).unwrap_err();
    assert_eq!(err.kind, bytemill::errors::ErrorKind::Decode, "{err}");

    // truncated for a 2-byte value
    let trunc = hex("80");
    let err = decode_packet(&r, "u64p", &trunc, false).unwrap_err();
    assert_eq!(err.kind, bytemill::errors::ErrorKind::Decode, "{err}");
}

#[test]
fn varint32_zigzag_roundtrips_listed_and_symmetry() {
    let r = leb_resolved();
    let cases: &[(&str, i128)] = &[
        ("00", 0),
        ("01", -1),
        ("02", 1),
        ("03", -2),
        ("feffffff0f", 2_147_483_647),
        ("ffffffff0f", -2_147_483_648),
    ];
    for (raw, n) in cases {
        let b = hex(raw);
        let dec = decode_packet(&r, "i32p", &b, false).unwrap();
        assert_eq!(dec.get("v"), Some(&Value::Int(*n)), "dec {raw}");
        let enc = encode_packet(&r, "i32p", &one("v", Value::Int(*n))).unwrap();
        assert_eq!(enc, b, "enc {n}");
    }
    // symmetry sweep + extremes
    for n in -300i128..=300 {
        let enc = encode_packet(&r, "i32p", &one("v", Value::Int(n))).unwrap();
        let dec = decode_packet(&r, "i32p", &enc, false).unwrap();
        assert_eq!(dec.get("v"), Some(&Value::Int(n)));
    }
    for n in [i32::MIN as i128, i32::MAX as i128] {
        let enc = encode_packet(&r, "i32p", &one("v", Value::Int(n))).unwrap();
        let dec = decode_packet(&r, "i32p", &enc, false).unwrap();
        assert_eq!(dec.get("v"), Some(&Value::Int(n)));
    }
}

#[test]
fn varint64_zigzag_roundtrips_listed_and_symmetry() {
    let r = leb_resolved();
    let max = i64::MAX as i128;
    let min = i64::MIN as i128;
    let cases: &[(&str, i128)] = &[
        ("00", 0),
        ("01", -1),
        ("feffffffffffffffff01", max),
        ("ffffffffffffffffff01", min),
    ];
    for (raw, n) in cases {
        let b = hex(raw);
        let dec = decode_packet(&r, "i64p", &b, false).unwrap();
        assert_eq!(dec.get("v"), Some(&Value::Int(*n)), "dec {raw}");
        let enc = encode_packet(&r, "i64p", &one("v", Value::Int(*n))).unwrap();
        assert_eq!(enc, b, "enc {n}");
    }
    // symmetry
    for n in -300i128..=300 {
        let enc = encode_packet(&r, "i64p", &one("v", Value::Int(n))).unwrap();
        let dec = decode_packet(&r, "i64p", &enc, false).unwrap();
        assert_eq!(dec.get("v"), Some(&Value::Int(n)));
    }
    for &n in &[min, max] {
        let enc = encode_packet(&r, "i64p", &one("v", Value::Int(n))).unwrap();
        let dec = decode_packet(&r, "i64p", &enc, false).unwrap();
        assert_eq!(dec.get("v"), Some(&Value::Int(n)));
    }
}

//! Tests for wiregen on any scalar encoding that declares a yield (le ints, user scalars,
//! unrolled varuint with requires and conditionals). Named scalars are untouched.

mod common;

use bytemill::interp::{decode_packet, encode_packet};
use bytemill::wiregen::generate;
use bytemill::{Value, analyze, load_schema, parse};

fn schema(src: &str) -> bytemill::ResolvedSchema {
    analyze(&parse(src).unwrap()).unwrap()
}

fn with_integers(extra: &str) -> bytemill::ResolvedSchema {
    let base = std::fs::read_to_string(common::schema_path("std/integers.bml")).unwrap();
    analyze(&parse(&(base + "\n" + extra)).unwrap()).unwrap()
}

#[test]
fn le_integers_generate_decode_strict_and_roundtrip() {
    for &ty in &["u16le", "i16le", "u32le", "i32le", "u64le", "i64le"] {
        let resolved = with_integers(&format!("packet probe {{ x: {} }}", ty));
        for seed in 0..20 {
            let bytes = generate(&resolved, "probe", seed)
                .unwrap_or_else(|_| panic!("generate {} seed {}", ty, seed));
            let dec = decode_packet(&resolved, "probe", &bytes, true)
                .unwrap_or_else(|_| panic!("strict decode {} seed {}", ty, seed));
            let re = encode_packet(&resolved, "probe", &dec)
                .unwrap_or_else(|_| panic!("reencode {} seed {}", ty, seed));
            assert_eq!(bytes, re, "roundtrip {} seed {}", ty, seed);
            if let Some(Value::Int(v)) = dec.get("x") {
                if ty.starts_with('u') {
                    assert!(*v >= 0, "{} produced negative", ty);
                }
            } else {
                panic!("{} did not decode to int", ty);
            }
        }
    }
}

#[test]
fn user_defined_u24le_scalar() {
    let src = r#"
    encoding u24le = {
        b0: u8 = value & 0xFF
        b1: u8 = (value >> 8) & 0xFF
        b2: u8 = (value >> 16) & 0xFF
    } = (b2 << 16) | (b1 << 8) | b0
    packet probe { x: u24le }
    "#;
    let resolved = schema(src);
    for seed in 0..10 {
        let bytes =
            generate(&resolved, "probe", seed).unwrap_or_else(|_| panic!("u24le seed {}", seed));
        assert_eq!(bytes.len(), 3, "u24le must be 3 bytes");
        let dec = decode_packet(&resolved, "probe", &bytes, true).expect("strict");
        let re = encode_packet(&resolved, "probe", &dec).expect("reenc");
        assert_eq!(bytes, re);
        if let Some(Value::Int(v)) = dec.get("x") {
            assert!(*v >= 0 && *v < (1 << 24), "u24 out of range: {}", v);
        }
    }
}

#[test]
fn le_scalar_as_count_draws_small() {
    let resolved = with_integers(
        r#"
        packet probe {
            n: u16le
            items: u8[] count = n
        }
        "#,
    );
    for seed in 0..10 {
        let bytes =
            generate(&resolved, "probe", seed).unwrap_or_else(|_| panic!("count seed {}", seed));
        let dec = decode_packet(&resolved, "probe", &bytes, true).expect("strict");
        let n = match dec.get("n") {
            Some(Value::Int(k)) => *k as usize,
            _ => panic!("n not int"),
        };
        assert!(n <= 3, "count must be small like named: got {}", n);
        let items = match dec.get("items") {
            Some(Value::List(l)) => l,
            _ => panic!("items not list"),
        };
        assert_eq!(items.len(), n, "list len must match count");
    }
}

#[test]
fn le_scalars_honour_exact_match_tags_and_const_comparisons() {
    let resolved = with_integers(
        r#"
        packet probe {
            t: u16le
            match t {
                100 => { a: u8 }
                200 => { b: u8 }
            }
            c: u16le
            if c == 42 {
                d: u8
            }
        }
        "#,
    );
    let mut saw_tag = false;
    let mut saw_const = false;
    for seed in 0..30 {
        let bytes = generate(&resolved, "probe", seed)
            .unwrap_or_else(|_| panic!("match/const seed {}", seed));
        let dec = decode_packet(&resolved, "probe", &bytes, true).expect("strict");
        if let Some(Value::Int(t)) = dec.get("t")
            && (*t == 100 || *t == 200)
        {
            saw_tag = true;
        }
        if let Some(Value::Int(c)) = dec.get("c")
            && *c == 42
        {
            saw_const = true;
        }
    }
    assert!(saw_tag, "match tag scalar must draw from arms (exact)");
    assert!(
        saw_const,
        "compared scalar must draw the constant (consts hint)"
    );
}

#[test]
fn hytale_packets_generate_with_le_and_roundtrip_nonstring_packets() {
    let resolved =
        analyze(&load_schema(&[common::schema_path("hytale/2026_02_17.bml")]).unwrap()).unwrap();
    let mut generated = 0;
    let mut roundtripped = 0;
    for name in resolved.packets.keys() {
        for seed in 0..5 {
            let bytes = generate(&resolved, name, seed)
                .unwrap_or_else(|_| panic!("hytale generate {} seed {}", name, seed));
            generated += 1;
            if name == "ping" || name == "pong" {
                let dec = decode_packet(&resolved, name, &bytes, true)
                    .unwrap_or_else(|_| panic!("strict {}", name));
                let reenc = encode_packet(&resolved, name, &dec)
                    .unwrap_or_else(|_| panic!("reenc {}", name));
                assert_eq!(bytes, reenc, "roundtrip {} seed {}", name, seed);
                roundtripped += 1;
            }
            // connect/disconnect use string( ) which wiregen special-cases to leb128 length
            // while hytale's string decl uses varint; generate now succeeds (le fix) but
            // we do not assert roundtrip for them here.
        }
    }
    assert!(
        generated >= 4 * 5,
        "all 4 hytale packets must generate for 5 seeds"
    );
    assert!(roundtripped >= 2 * 5, "ping/pong must roundtrip");
}

#[test]
fn unrolled_varuint32_with_ifs_and_require_generates_and_roundtrips() {
    let src = r#"
    encoding varuint32 = {
        b0: u8 = (value & 0x7F) | (((value >> 7) != 0) ? 0x80 : 0)
        b1: u8 if (b0 & 0x80) != 0 = ((value >> 7) & 0x7F) | (((value >> 14) != 0) ? 0x80 : 0)
        b2: u8 if (b1 & 0x80) != 0 = ((value >> 14) & 0x7F) | (((value >> 21) != 0) ? 0x80 : 0)
        b3: u8 if (b2 & 0x80) != 0 = ((value >> 21) & 0x7F) | (((value >> 28) != 0) ? 0x80 : 0)
        b4: u8 if (b3 & 0x80) != 0 = (value >> 28) & 0x0F
        require (b4 & 0x80) == 0
    } = (b0 & 0x7F) | ((b1 & 0x7F) << 7) | ((b2 & 0x7F) << 14) | ((b3 & 0x7F) << 21) | ((b4 & 0x0F) << 28)
    packet probe { v: varuint32 }
    "#;
    let resolved = schema(src);
    for seed in 0..50 {
        let bytes = generate(&resolved, "probe", seed)
            .unwrap_or_else(|_| panic!("varuint32 seed {}", seed));
        let dec = decode_packet(&resolved, "probe", &bytes, true).expect("strict");
        let re = encode_packet(&resolved, "probe", &dec).expect("reenc");
        assert_eq!(bytes, re, "varuint32 roundtrip seed {}", seed);
    }
}

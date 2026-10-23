//! Dedicated tests for the `f32le`/`f64le` primitives (F7 front-end addition).
//! Uses inline schemas, public interp/wiregen/synth APIs, and direct byte checks.

use bytemill::interp::{decode_packet, encode_packet};
use bytemill::synth::synthesize_packet;
use bytemill::wiregen::generate;
use bytemill::{Env, Value, analyze, parse};

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

#[test]
fn f32le_decodes_and_roundtrips() {
    let r = resolved("packet probe { x: f32le }");
    // 0000c03f is le bytes for f32 1.5
    let bytes = hex("00 00 c0 3f");
    let dec = decode_packet(&r, "probe", &bytes, true).unwrap();
    match dec.get("x") {
        Some(Value::Float(f)) => assert_eq!(*f, 1.5),
        other => panic!("expected Float(1.5), got {other:?}"),
    }
    let enc = encode_packet(&r, "probe", &one("x", Value::Float(1.5))).unwrap();
    assert_eq!(enc, bytes, "encode roundtrip");
}

#[test]
fn f64le_decodes_and_roundtrips() {
    let r = resolved("packet probe { x: f64le }");
    // 0000000000000240 is le for f64 2.25
    let bytes = hex("00 00 00 00 00 00 02 40");
    let dec = decode_packet(&r, "probe", &bytes, true).unwrap();
    match dec.get("x") {
        Some(Value::Float(f)) => assert_eq!(*f, 2.25),
        other => panic!("expected Float(2.25), got {other:?}"),
    }
    let enc = encode_packet(&r, "probe", &one("x", Value::Float(2.25))).unwrap();
    assert_eq!(enc, bytes, "encode roundtrip");
}

#[test]
fn le_bytes_decode_as_be_on_reversed() {
    let r32 = resolved("packet probe { x: f32be }");
    let r64 = resolved("packet probe { x: f64be }");
    // reversed le bytes for 1.5/2.25 should give same values via be
    let be32 = hex("3f c0 00 00");
    let dec32 = decode_packet(&r32, "probe", &be32, true).unwrap();
    assert_eq!(dec32.get("x"), Some(&Value::Float(1.5)));

    let be64 = hex("40 02 00 00 00 00 00 00");
    let dec64 = decode_packet(&r64, "probe", &be64, true).unwrap();
    assert_eq!(dec64.get("x"), Some(&Value::Float(2.25)));
}

#[test]
fn float_le_truncated_is_error() {
    let r = resolved("packet probe { x: f32le }");
    // only 3 bytes for f32le
    let short = hex("00 00 c0");
    assert!(
        decode_packet(&r, "probe", &short, true).is_err(),
        "3-byte f32le input must be decode error"
    );
}

#[test]
fn wiregen_le_roundtrips_for_seeds() {
    let src = "packet probe { a: f32le  b: f64le  n: u8 }";
    let r = resolved(src);
    let mut checked = 0usize;
    for seed in 0..20 {
        let bytes =
            generate(&r, "probe", seed).unwrap_or_else(|e| panic!("generate seed {seed}: {e}"));
        let dec = decode_packet(&r, "probe", &bytes, true).expect("strict decode");
        let enc = encode_packet(&r, "probe", &dec).expect("re-encode");
        assert_eq!(enc, bytes, "seed {seed} roundtrip");
        checked += 1;
    }
    assert!(checked >= 20, "checked floor");
}

#[test]
fn synth_le_roundtrips() {
    let r = resolved("packet probe { a: f32le  b: f64le }");
    let pkt = r.packets.get("probe").unwrap();
    let val = synthesize_packet(&r, pkt).unwrap();
    // must contain the float keys
    assert!(val.contains_key("a"));
    assert!(val.contains_key("b"));
    let encoded = encode_packet(&r, "probe", &val).unwrap();
    let decoded = decode_packet(&r, "probe", &encoded, true).unwrap();
    // equal as maps
    assert_eq!(decoded, val, "synth value roundtrips via encode/decode");
}

#[test]
fn float_le_nonfinite_free_sweep_roundtrips_byte_exact() {
    // 0.0, -0.0 (bits), small, max etc. No NaN/Inf.
    let cases_f32: [(f64, [u8; 4]); 4] = [
        (0.0, 0.0f32.to_le_bytes()),
        (-0.0, (-0.0f32).to_le_bytes()),
        (1e-3, 1e-3f32.to_le_bytes()),
        (f32::MAX as f64, f32::MAX.to_le_bytes()),
    ];
    let cases_f64: [(f64, [u8; 8]); 4] = [
        (0.0, 0.0f64.to_le_bytes()),
        (-0.0, (-0.0f64).to_le_bytes()),
        (1e-3, 1e-3f64.to_le_bytes()),
        (f64::MAX, f64::MAX.to_le_bytes()),
    ];
    let mut checked = 0usize;

    // f32le
    let r32 = resolved("packet probe { x: f32le }");
    for (val, want_bytes) in cases_f32 {
        let enc = encode_packet(&r32, "probe", &one("x", Value::Float(val))).unwrap();
        assert_eq!(enc, want_bytes.as_slice(), "f32le encode {val}");
        let dec = decode_packet(&r32, "probe", &enc, true).unwrap();
        let re = encode_packet(&r32, "probe", &dec).unwrap();
        assert_eq!(re, enc, "f32le decode then re-encode bytes");
        checked += 1;
    }

    // f64le
    let r64 = resolved("packet probe { x: f64le }");
    for (val, want_bytes) in cases_f64 {
        let enc = encode_packet(&r64, "probe", &one("x", Value::Float(val))).unwrap();
        assert_eq!(enc, want_bytes.as_slice(), "f64le encode {val}");
        let dec = decode_packet(&r64, "probe", &enc, true).unwrap();
        match dec.get("x") {
            Some(Value::Float(f)) => assert_eq!(f.to_bits(), val.to_bits()),
            o => panic!("bad decode {o:?}"),
        }
        checked += 1;
    }
    assert!(checked >= 8, "sweep cases checked");
}

#[test]
fn f32le_is_bare_primitive_no_decl_needed() {
    let r = analyze(&parse("packet p { x: f32le }").unwrap());
    assert!(
        r.is_ok(),
        "f32le use as bare primitive succeeds without import/decl"
    );
}

#[test]
fn declaring_encoding_f32le_behaves_as_f32be() {
    // Mirror of f32be: declaring encoding with a primitive name is accepted
    // (shadows in encode/decode lookup; synth/wiregen hit arm first for prim path).
    // No rejection at analyze time (same as f32be today).
    let src = r#"
encoding f32le = u8
packet p { x: f32le }
"#;
    let r = analyze(&parse(src).unwrap());
    assert!(
        r.is_ok(),
        "decl shadowing f32le accepted (mirrors f32be behaviour)"
    );
}

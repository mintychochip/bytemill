//! Integer coercion of encoder input. Python's `int()` raises on NaN and infinity (so the
//! Python `encode` command dies with a traceback); this port reports a clean error. The
//! saturating `as` cast it used before turned NaN into 0 and infinity into all-ones, and those
//! reached the wire as ordinary bytes. Python's `int()` is also unbounded: a float beyond
//! `i128` keeps its low bits instead of saturating.

mod common;

use bytemill::interp::encode_packet;
use bytemill::ordmap::OrderedMap;
use bytemill::{Value, analyze, parse};

fn encode_n(value: Value) -> Result<Vec<u8>, String> {
    let resolved = analyze(&parse("packet probe { n: u8 }\n").unwrap()).unwrap();
    let mut fields = OrderedMap::new();
    fields.insert("n", value);
    encode_packet(&resolved, "probe", &fields).map_err(|e| e.to_string())
}

#[test]
fn nan_and_infinity_are_not_integers() {
    let cases = [
        (f64::NAN, "cannot convert float NaN to integer"),
        (f64::INFINITY, "cannot convert float infinity to integer"),
        (f64::NEG_INFINITY, "cannot convert float infinity to integer"),
    ];
    for (value, message) in cases {
        assert_eq!(encode_n(Value::Float(value)), Err(message.to_string()), "{value}");
    }
}

#[test]
fn a_finite_float_truncates_toward_zero() {
    assert_eq!(encode_n(Value::Float(7.9)), Ok(vec![7]));
    // int(-7.9) is -7, and the byte is its two's complement.
    assert_eq!(encode_n(Value::Float(-7.9)), Ok(vec![0xf9]));
}

#[test]
fn a_float_beyond_i128_wraps_like_pythons_unbounded_int() {
    // Expected values are Python's `int(f) & (2**64 - 1)`: zero for every double at or above
    // 2**127 (the integer is a multiple of 2**75), 0x4675000000000000 for 1e30 and 0x1000 for
    // 2**64 + 2**12. Saturating to i128::MAX gave all-ones for the positive ones.
    let resolved = common::load_minecraft().unwrap();
    let encode = |f: f64| {
        let mut fields = OrderedMap::new();
        fields.insert("keep_alive_id", Value::Float(f));
        encode_packet(&resolved, "keep_alive", &fields).unwrap()
    };
    let two_127 = (1u128 << 127) as f64;
    let cases: [(f64, u64); 9] = [
        (1e300, 0),
        (-1e300, 0),
        (1.5 * two_127, 0),
        (-1.5 * two_127, 0),
        (two_127, 0),
        (-two_127, 0),
        (f64::MAX, 0),
        (1e30, 0x4675_0000_0000_0000),
        ((1u128 << 64) as f64 + 4096.0, 0x1000),
    ];
    for (value, want) in cases {
        assert_eq!(encode(value), want.to_be_bytes(), "{value:e}");
    }
}

#[test]
fn a_json_integer_beyond_i128_keeps_its_low_bits() {
    // Python's `json.loads` keeps the whole integer and a width-masked encode sees its low bits.
    // Each row is (literal, Python's `int % 2**128` as a signed value, the `keep_alive` bytes).
    // Rounding the literal to a float, as the parser once did, encoded zeros for all of them.
    let cases: [(&str, i128, u64); 8] = [
        // 2**200 + 5
        ("1606938044258990275541962092341162602522202993782792835301381", 5, 5),
        ("-1606938044258990275541962092341162602522202993782792835301381", -5, 0xffff_ffff_ffff_fffb),
        // 2**128 and 2**128 + 7
        ("340282366920938463463374607431768211456", 0, 0),
        ("340282366920938463463374607431768211463", 7, 7),
        // 10**40
        ("10000000000000000000000000000000000000000", 131811359292784559562136384478721867776, 0xb9f5_6100_0000_0000),
        ("-10000000000000000000000000000000000000000", -131811359292784559562136384478721867776, 0x460a_9f00_0000_0000),
        // 2**127, the first integer that does not fit
        ("170141183460469231731687303715884105728", i128::MIN, 0),
        // 2**255 + 1
        ("57896044618658097711785492504343953926634992332820282019728792003956564819969", 1, 1),
    ];
    let resolved = common::load_minecraft().unwrap();
    for (literal, want_value, want_bytes) in cases {
        assert_eq!(bytemill::json::loads(literal).unwrap(), Value::Int(want_value), "{literal}");
        let object = bytemill::json::loads(&format!("{{\"keep_alive_id\": {literal}}}")).unwrap();
        let Value::Map(fields) = object else { panic!("not an object: {object:?}") };
        let bytes = encode_packet(&resolved, "keep_alive", &fields).unwrap();
        assert_eq!(bytes, want_bytes.to_be_bytes(), "{literal}");
    }
}

//! Integer coercion of encoder input. Python's `int()` raises on NaN and infinity (so the
//! Python `encode` command dies with a traceback); this port reports a clean error. The
//! saturating `as` cast it used before turned NaN into 0 and infinity into all-ones, and those
//! reached the wire as ordinary bytes. Python's `int()` is also unbounded: a float beyond
//! `i128` keeps its low bits instead of saturating.

mod common;

use bps::interp::encode_packet;
use bps::ordmap::OrderedMap;
use bps::{Value, analyze, parse};

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

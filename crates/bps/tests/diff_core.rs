//! Differential interpreter check against Python-generated fixtures.
//!
//! Dev-time verification tool: `BPS_FIXTURES=/tmp/difftest/fixtures.json cargo
//! test -p bps --test diff_core`. The fixtures come from the Python reference
//! (synth value, Python-encoded bytes, decoded shape, canonicalized to JSON).

mod common;

use bps::interp::{decode_packet, encode_packet};
use bps::json::{dumps_sorted, loads};
use bps::ordmap::OrderedMap;
use bps::{Value};

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn tagged(tag: &str, inner: Value) -> Value {
    let mut m = OrderedMap::new();
    m.insert(tag, inner);
    Value::map(m)
}

fn canon(v: &Value) -> Value {
    match v {
        Value::Int(i) => tagged("$int", Value::Int(*i)),
        Value::Float(f) => tagged("$float", Value::str(bps::json::float_repr(*f))),
        Value::Bool(b) => tagged("$bool", Value::Bool(*b)),
        Value::Str(s) => tagged("$str", Value::str(s.to_string())),
        Value::Bytes(b) => tagged("$bytes", Value::str(hex_lower(b))),
        Value::Null => tagged("$null", Value::Bool(true)),
        Value::List(items) => {
            tagged("$list", Value::list(items.iter().map(canon).collect()))
        }
        Value::Map(m) => {
            let pairs: Vec<Value> = m
                .iter()
                .map(|(k, v)| Value::list(vec![Value::str(k.to_string()), canon(v)]))
                .collect();
            tagged("$map", Value::list(pairs))
        }
    }
}

/// Rebuild a `Value` from the canonical fixture form.
fn from_canon(v: &Value) -> Value {
    if let Some(inner) = v.get("$int") {
        return inner.clone();
    }
    if let Some(inner) = v.get("$str") {
        return inner.clone();
    }
    if let Some(inner) = v.get("$bool") {
        return inner.clone();
    }
    if let Some(inner) = v.get("$float") {
        // repr round-trips exactly.
        return match inner {
            Value::Str(s) => Value::Float(s.parse().unwrap()),
            other => other.clone(),
        };
    }
    if let Some(Value::Str(s)) = v.get("$bytes") {
        let bytes: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect();
        return Value::bytes(bytes);
    }
    if v.get("$null").is_some() {
        return Value::Null;
    }
    if let Some(Value::List(items)) = v.get("$list") {
        return Value::list(items.iter().map(from_canon).collect());
    }
    if let Some(Value::List(pairs)) = v.get("$map") {
        let mut m = OrderedMap::new();
        for pair in pairs.iter() {
            let Value::List(kv) = pair else { panic!("bad pair") };
            let key = match &kv[0] {
                Value::Str(s) => s.to_string(),
                _ => panic!("bad key"),
            };
            m.insert(&key, from_canon(&kv[1]));
        }
        return Value::map(m);
    }
    panic!("unknown canonical form: {v:?}");
}

#[test]
fn interpreter_matches_python_fixtures() {
    let path = match std::env::var("BPS_FIXTURES") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("BPS_FIXTURES not set; skipping differential check");
            return;
        }
    };
    let text = std::fs::read_to_string(&path).unwrap();
    let fixtures = loads(&text).unwrap();
    let Value::List(entries) = fixtures else {
        panic!("fixtures must be a list");
    };
    let resolved = common::load_minecraft().unwrap();
    let mut failures = Vec::new();
    for entry in entries.iter() {
        let name = match entry.get("packet") {
            Some(Value::Str(s)) => s.to_string(),
            _ => continue,
        };
        if entry.get("error").is_some() {
            continue; // Python could not synthesize it; nothing to compare.
        }
        let expect_bytes = match entry.get("bytes") {
            Some(Value::Str(s)) => s.to_string(),
            _ => continue,
        };
        let bytes: Vec<u8> = (0..expect_bytes.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&expect_bytes[i..i + 2], 16).unwrap())
            .collect();

        // 1. Decode the Python-encoded bytes; the shape must match Python's.
        let decoded = match decode_packet(&resolved, &name, &bytes, false) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{name}: rust decode failed: {e}"));
                continue;
            }
        };
        let want_decoded = canon(&from_canon(entry.get("decoded").unwrap()));
        let got_decoded = canon(&Value::map(decoded.clone()));
        if got_decoded != want_decoded {
            failures.push(format!(
                "{name}: decoded differs\n  py:  {}\n  rust:{}",
                dumps_sorted(&want_decoded),
                dumps_sorted(&got_decoded)
            ));
            continue;
        }

        // 2. Re-encode the decoded shape; the bytes must match.
        match encode_packet(&resolved, &name, &decoded) {
            Ok(out) => {
                if hex_lower(&out) != expect_bytes {
                    failures.push(format!(
                        "{name}: re-encode differs\n  py:  {expect_bytes}\n  rust:{}",
                        hex_lower(&out)
                    ));
                }
            }
            Err(e) => failures.push(format!("{name}: rust re-encode failed: {e}")),
        }

        // 3. Encode the Python-synthesized value directly.
        let value = from_canon(entry.get("value").unwrap());
        let Value::Map(m) = value else {
            failures.push(format!("{name}: fixture value is not a map"));
            continue;
        };
        match encode_packet(&resolved, &name, &m) {
            Ok(out) => {
                if hex_lower(&out) != expect_bytes {
                    failures.push(format!(
                        "{name}: encode-from-value differs\n  py:  {expect_bytes}\n  rust:{}",
                        hex_lower(&out)
                    ));
                }
            }
            Err(e) => failures.push(format!("{name}: rust encode failed: {e}")),
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} of {} packets differ:\n{}",
            failures.len(),
            entries.len(),
            failures.join("\n")
        );
    }
}

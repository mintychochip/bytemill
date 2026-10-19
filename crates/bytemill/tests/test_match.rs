//! Ports tests/test_match.py.

use bytemill::errors::ErrorKind;
use bytemill::interp::{decode_packet, encode_packet};
use bytemill::{analyze, parse, Value};

fn resolved(source: &str) -> bytemill::ResolvedSchema {
    analyze(&parse(source).unwrap()).unwrap()
}

fn env_of(pairs: &[(&str, Value)]) -> bytemill::Env {
    let mut env = bytemill::Env::new();
    for (k, v) in pairs {
        env.insert(k, v.clone());
    }
    env
}

const STATEMENT_SOURCE: &str = "
    packet probe {
        action: u8
        match (action) {
            0 => { x: u8 }
            1 | 2 => { y: u8 }
            _ => { z: u8 }
        }
    }
";

/// A raw payload and the decoded fields it must produce.
type Case<'a> = (Vec<u8>, &'a [(&'a str, Value)]);

#[test]
fn match_splices_the_selected_arm() {
    let r = resolved(STATEMENT_SOURCE);
    let cases: [Case<'_>; 4] = [
        (vec![0, 0x11], &[("action", Value::Int(0)), ("x", Value::Int(0x11))]),
        (vec![1, 0x22], &[("action", Value::Int(1)), ("y", Value::Int(0x22))]),
        (vec![2, 0x22], &[("action", Value::Int(2)), ("y", Value::Int(0x22))]),
        (vec![9, 0x33], &[("action", Value::Int(9)), ("z", Value::Int(0x33))]),
    ];
    for (raw, pairs) in cases {
        let value = env_of(pairs);
        let decoded = decode_packet(&r, "probe", &raw, false).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(encode_packet(&r, "probe", &value).unwrap(), raw);
    }
}

#[test]
fn switch_is_the_same_statement() {
    let r = resolved(
        "
        packet probe {
            action: u8
            switch (action) {
                0 => { title: u8  health: u8 }
                _ => { }
            }
        }
        ",
    );
    let raw = vec![0, 0x01, 0x02];
    let value = env_of(&[
        ("action", Value::Int(0)),
        ("title", Value::Int(1)),
        ("health", Value::Int(2)),
    ]);
    assert_eq!(decode_packet(&r, "probe", &raw, false).unwrap(), value);
    assert_eq!(encode_packet(&r, "probe", &value).unwrap(), raw);
    let other = vec![4u8];
    let only = env_of(&[("action", Value::Int(4))]);
    assert_eq!(decode_packet(&r, "probe", &other, false).unwrap(), only);
    assert_eq!(encode_packet(&r, "probe", &only).unwrap(), other);
}

#[test]
fn unmatched_tag_is_an_error() {
    let r = resolved(
        "
        packet probe {
            action: u8
            match (action) {
                0 => { x: u8 }
            }
        }
        ",
    );
    let err = decode_packet(&r, "probe", &[1], false).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Decode, "{err}");
    let value = env_of(&[("action", Value::Int(1))]);
    let err = encode_packet(&r, "probe", &value).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Encode, "{err}");
}

#[test]
fn statement_arm_must_be_a_block() {
    let err = parse(
        "
        packet probe {
            action: u8
            match (action) { 0 => u8 }
        }
        ",
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Parse, "{err}");
}

#[test]
fn field_named_match() {
    let r = resolved(
        "
        packet probe {
            matches: { match: u8 }
        }
        ",
    );
    let raw = vec![7u8];
    let value = env_of(&[("matches", Value::map({
        let mut m = bytemill::ordmap::OrderedMap::new();
        m.insert("match", Value::Int(7));
        m
    }))]);
    assert_eq!(decode_packet(&r, "probe", &raw, false).unwrap(), value);
    assert_eq!(encode_packet(&r, "probe", &value).unwrap(), raw);
}

#[test]
fn or_tags_share_one_arm() {
    let r = resolved(
        "
        packet probe {
            kind: u8
            value: match kind {
                0 | 1 => u8
                _ => u8
            }
        }
        ",
    );
    for (raw, pairs) in [
        (vec![0u8, 0x2A], [("kind", 0), ("value", 0x2A)]),
        (vec![1u8, 0x2A], [("kind", 1), ("value", 0x2A)]),
        (vec![7u8, 0x03], [("kind", 7), ("value", 0x03)]),
    ] {
        let value = env_of(&[
            ("kind", Value::Int(pairs[0].1)),
            ("value", Value::Int(pairs[1].1)),
        ]);
        assert_eq!(decode_packet(&r, "probe", &raw, false).unwrap(), value);
        assert_eq!(encode_packet(&r, "probe", &value).unwrap(), raw);
    }
}

static TEMP_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "bps-match-{tag}-{}-{n}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn expr_ints(expr: Option<&bytemill::ast::Expr>, out: &mut std::collections::BTreeSet<i128>) {
    let Some(expr) = expr else { return };
    match expr {
        bytemill::ast::Expr::Int(v) => {
            out.insert(*v);
        }
        bytemill::ast::Expr::Unop(_, inner) => expr_ints(Some(&**inner), out),
        bytemill::ast::Expr::Binop(_, left, right) => {
            expr_ints(Some(&**left), out);
            expr_ints(Some(&**right), out);
        }
        bytemill::ast::Expr::Ternary(cond, yes, no) => {
            expr_ints(Some(&**cond), out);
            expr_ints(Some(&**yes), out);
            expr_ints(Some(&**no), out);
        }
        bytemill::ast::Expr::Call(_, args) => {
            for arg in args {
                expr_ints(Some(arg), out);
            }
        }
        _ => {}
    }
}

#[test]
fn synthesized_value_round_trips() {
    let r = resolved(STATEMENT_SOURCE);
    let pkt = r.packets.get("probe").unwrap();
    let value = bytemill::synth::synthesize_packet(&r, pkt).unwrap();
    let raw = encode_packet(&r, "probe", &value).unwrap();
    assert_eq!(decode_packet(&r, "probe", &raw, false).unwrap(), value);
}

// Deviation from the Python test: `Walker`/`packet_fn` are private in the Rust
// port, so the check runs the public `emit_rust` entry point and reads the
// generated sources instead of rendering one packet in memory. The decoder
// lives in its route-category module, not the `packets.rs` index. The
// assertion's meaning — the decoder emits a `1 | 2` or-pattern — is unchanged.
#[test]
fn rust_decoder_emits_or_pattern() {
    let r = resolved(STATEMENT_SOURCE);
    let dir = temp_dir("or-pattern");
    bytemill::emit_rust::emit_rust(&r, &dir.join("crate")).unwrap();
    let src = dir.join("crate/src");
    let mut text = String::new();
    for entry in walkdir_rs(&src) {
        text.push_str(&std::fs::read_to_string(&entry).unwrap());
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(text.contains("1 | 2"), "{text}");
}

fn walkdir_rs(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walkdir_rs(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    out
}

// Deviation from the Python test: `_flatten` is private in the Rust port, so
// the check uses the public `packet_fields` wrapper over the same `flatten`
// and inspects the guard of shape `y`. The assertion's meaning — the `1 | 2`
// arm guards exactly the tags 1 and 2 — is unchanged.
#[test]
fn rstom_shape_ors_alternative_tags() {
    let r = resolved(STATEMENT_SOURCE);
    let pkt = r.packets.get("probe").unwrap();
    let (shapes, _) = bytemill::emit_rstom::packet_fields(pkt, &r).unwrap();
    let y = shapes.iter().find(|s| s.ident == "y").expect("shape y");
    let mut found = std::collections::BTreeSet::new();
    expr_ints(y.guard.as_ref(), &mut found);
    assert_eq!(found, [1i128, 2].into_iter().collect());
}

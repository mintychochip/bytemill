//! Lazy evaluation of ternary / short-circuit operands that read optional fields (B7a, B8).

mod harness {
    include!("fixtures/typed_exec_harness.inc.rs");
}

use std::path::{Path, PathBuf};
use std::process::Command;

use bytemill::interp::{decode_packet, encode_packet};
use bytemill::ir::typed::typed;
use bytemill::{GoVersion, analyze, parse};

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn std_prefix() -> String {
    std::fs::read_to_string(repo("schemas/std/varint.bml")).unwrap()
        + r#"
encoding boolean = {
    raw: u8 = value ? 1 : 0
} = raw != 0
"#
}

fn lazy_ternary_schema() -> String {
    format!(
        "{}{}",
        std_prefix(),
        r#"
packet lazy_ternary {
    has: boolean
    if has {
        n: varint
    }
    s: match has ? n : -1 {
        1 | 2 => u8
        _ => { }
    }
}
"#
    )
}

fn lazy_logical_schema() -> String {
    format!(
        "{}{}",
        std_prefix(),
        r#"
packet lazy_logical {
    has: boolean
    if has {
        n: varint
    }
    s: match has && n > 0 ? 1 : 0 {
        1 => u8
        _ => { }
    }
}
"#
    )
}

fn eof_skip_schema() -> String {
    format!(
        "{}{}",
        std_prefix(),
        r#"
packet eof_skip {
    tag: u8
    has: boolean
    if has {
        v: match tag {
            1 => { x: u8 }
            2 => { }
        }
    }
}
"#
    )
}

fn interp_roundtrip(resolved: &bytemill::ResolvedSchema, packet: &str, bytes: &[u8]) {
    let dec = decode_packet(resolved, packet, bytes, true).expect("interp decode");
    let enc = encode_packet(resolved, packet, &dec).expect("interp encode");
    assert_eq!(enc, bytes, "interpreter re-encode");
}

#[test]
fn lazy_ternary_has_false_roundtrips() {
    let resolved = analyze(&parse(&lazy_ternary_schema()).unwrap()).unwrap();
    let prog = typed(&resolved).unwrap();
    // has=0, scrutinee -1, default arm: single boolean byte.
    let bytes = [0u8];
    interp_roundtrip(&resolved, "lazy_ternary", &bytes);
    harness::packet_roundtrip(&prog, "lazy_ternary", &bytes).expect("typed roundtrip");
}

#[test]
fn lazy_ternary_has_true_arms() {
    let resolved = analyze(&parse(&lazy_ternary_schema()).unwrap()).unwrap();
    let prog = typed(&resolved).unwrap();
    // has=1, n=1, arm 1 => u8(42)
    let bytes = [1, 1, 42];
    interp_roundtrip(&resolved, "lazy_ternary", &bytes);
    harness::packet_roundtrip(&prog, "lazy_ternary", &bytes).expect("typed arm 1");
    // n=2, arm 2
    let bytes2 = [1, 2, 7];
    interp_roundtrip(&resolved, "lazy_ternary", &bytes2);
    harness::packet_roundtrip(&prog, "lazy_ternary", &bytes2).expect("typed arm 2");
}

#[test]
fn lazy_logical_short_circuit() {
    let resolved = analyze(&parse(&lazy_logical_schema()).unwrap()).unwrap();
    let prog = typed(&resolved).unwrap();
    // has=0: typed lowering must not unwrap `n` (interpreter evaluates `&&` eagerly).
    let bytes = [0u8];
    harness::packet_roundtrip(&prog, "lazy_logical", &bytes).expect("typed && short-circuit");
    // has=1, n=5 => true && (5>0) => arm 1 (typed lowering; scrutinee `&&` is Stmt-only).
    let bytes2 = [1, 5, 9];
    harness::packet_roundtrip(&prog, "lazy_logical", &bytes2).expect("typed && true arm");
}

#[test]
fn lazy_ternary_go_native_roundtrip() {
    if Command::new("go").arg("version").output().is_err() {
        eprintln!("lazy_ternary_go_native_roundtrip: no go; skipping");
        return;
    }
    let source = lazy_ternary_schema();
    let resolved = analyze(&parse(&source).unwrap()).unwrap();
    let vectors = vec![
        bytemill::vectors::Vector {
            packet: "lazy_ternary".into(),
            route: None,
            hex: "00".into(),
        },
        bytemill::vectors::Vector {
            packet: "lazy_ternary".into(),
            route: None,
            hex: "01012a".into(),
        },
    ];
    let dir = std::env::temp_dir().join(format!("bml-lazy-go-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let tr =
        bytemill::emit_go::emit_go_with_vectors(&resolved, &dir, GoVersion::default(), &vectors)
            .unwrap()
            .unwrap();
    assert_eq!(tr.vectors, 2);
    let out = Command::new("go")
        .args(["test", "-count=1", "./..."])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "go vectors\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn eof_skipped_group_typed_roundtrip() {
    let resolved = analyze(&parse(&eof_skip_schema()).unwrap()).unwrap();
    let prog = typed(&resolved).unwrap();
    // tag=2, has=1, no bytes for inner group (interpreter EOF-skips).
    let bytes = [2, 1];
    harness::packet_roundtrip(&prog, "eof_skip", &bytes).expect("typed eof-skip encode");
}

#[test]
fn debug_subscription_update_eof_wire_typed() {
    let resolved =
        analyze(&bytemill::load_schema(&[repo("schemas/minecraft_26_3_wiki.bml")]).unwrap())
            .unwrap();
    let prog = typed(&resolved).unwrap();
    // entity_id=0, subscription_id=10, has_value=1, EOF before optional value (id 10 arm).
    let bytes = [0x00, 0x0a, 0x01];
    harness::packet_roundtrip(&prog, "debug_entity_value", &bytes)
        .expect("debug_entity_value eof subscription update wire");
}

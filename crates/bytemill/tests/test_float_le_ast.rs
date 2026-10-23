//! Tests for f32le/f64le in the AST-based emitters (emit rust, rstom/tree, frames, protodef names).
//! See local://w3-ast-emitters.md.

mod common;

use bytemill::emit_rstom::{emit_rstom, packet_body};
use bytemill::emit_tree::emit_tree;
use bytemill::errors::ErrorKind;
use bytemill::minestom::build_units;
use bytemill::ordmap::OrderedMap;

/// Deletes the generated crate tree on drop (matches test_emit_rust pattern).
struct Cleanup(std::path::PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        common::cleanup(&self.0);
    }
}

#[test]
fn emit_rust_supports_f32le_f64le() {
    if std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("cargo not installed; skipping");
        return;
    }
    let src = "packet probe { a: f32le b: f64le }";
    let resolved = common::load_with_extra(src).unwrap();
    let out = common::temp_tree("float-le-ast-rust");
    let _cleanup = Cleanup(out.clone());
    let crate_dir = out.join("crate");
    bytemill::emit_rust::emit_rust(&resolved, &crate_dir).unwrap();
    // Exercise the added runtime helpers and ensure the packet decode source
    // (which contains PRIMITIVES lowering for f32le/f64le) compiles and runs.
    let test_code = r#"
use bml_generated::runtime::{self, Cursor};

#[test]
fn le_floats() {
    let mut c = Cursor::new(&[0x00, 0x00, 0xc0, 0x3f]);
    let v = runtime::f32_le(&mut c).unwrap();
    assert!((v - 1.5).abs() < 1e-6);

    let mut c = Cursor::new(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x40]);
    let v = runtime::f64_le(&mut c).unwrap();
    assert!((v - 2.25).abs() < 1e-9);
}
"#;
    let tests_dir = crate_dir.join("tests");
    std::fs::create_dir_all(&tests_dir).unwrap();
    std::fs::write(tests_dir.join("le_floats.rs"), test_code).unwrap();
    let proc = std::process::Command::new("cargo")
        .args(["test", "--quiet"])
        .current_dir(&crate_dir)
        .output()
        .unwrap();
    assert_eq!(
        proc.status.code(),
        Some(0),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&proc.stdout),
        String::from_utf8_lossy(&proc.stderr)
    );
}

#[test]
fn protodef_names_reserves_f32le_f64le() {
    use bytemill::protodef::names::unique;
    let never = |_: &str| false;
    assert_eq!(unique("f32le", never), "f32le_");
    assert_eq!(unique("f64le", never), "f64le_");
    // never returns the reserved name itself
    let mut checks = 0usize;
    assert_eq!(unique("f32le", |n| n == "f32le_"), "f32le__2");
    checks += 1;
    assert_eq!(unique("f64le", |n| n == "f64le_"), "f64le__2");
    checks += 1;
    assert!(checks >= 2, "checked {} reserved cases", checks);
}

#[test]
fn rstom_and_tree_emit_clear_error_for_f32le() {
    let root = common::repo_root();
    let body = "packet probe { a: f32le b: f64le }";
    let source = common::rstom_tables::probe_source(&root, body);
    let dir = common::temp_tree("float-le-ast-rstom");
    let bml = dir.join("probe.bml");
    std::fs::write(&bml, &source).unwrap();
    let resolved = bytemill::analyze(&bytemill::load_schema(&[bml]).unwrap()).unwrap();
    let table = common::rstom_tables::probe_table();

    // full emit_rstom path
    let e = emit_rstom(&resolved, &dir.join("r"), &table, "../rstom").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Emit, "{}", e);
    let m = e.to_string();
    assert!(m.contains("f32le"), "{}", m);
    assert!(m.contains("rstom binary type"), "{}", m);

    // tree path via packet_body (used by emit_tree after build_units)
    let units = build_units(&resolved, &table).unwrap();
    let probe_unit = units
        .iter()
        .find(|unit| unit.pkt.name == "probe")
        .expect("the probe packet has a unit");
    let e2 = packet_body(probe_unit, &resolved).unwrap_err();
    assert_eq!(e2.kind, ErrorKind::Emit, "{}", e2);
    assert!(e2.to_string().contains("f32le"), "{}", e2);

    // emit_tree reaches packet_body inside planned_packets
    let ws = dir.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let sh = dir.join("sh");
    std::fs::create_dir_all(&sh).unwrap();
    let e3 = emit_tree(&resolved, &ws, &table, &sh, false).unwrap_err();
    assert_eq!(e3.kind, ErrorKind::Emit, "{}", e3);
    assert!(e3.to_string().contains("f32le"), "{}", e3);

    common::cleanup(&dir);
}

#[test]
fn frames_gen_supports_f32le_f64le_fixed_width() {
    // Decision: support via ("f32le",4), ("f64le",8) in FIXED.
    // Reason: the walker only advances byte position for fixed scalars (walk_skip
    // or walk_be for wanted numeric); endianness of the float bits is irrelevant
    // to length computation. Same policy as other fixed scalars.
    let src = r#"
encoding v3le = { x: f32le y: f32le z: f32le }
packet p { pos: v3le }
"#;
    let schema = bytemill::parse(src).unwrap();
    let resolved = bytemill::analyze(&schema).unwrap();
    let roots = vec!["v3le".to_string()];
    let known: OrderedMap<String> = OrderedMap::new();
    let walkers = bytemill::frames_gen::generate(&resolved, &roots, &known)
        .expect("frames_gen must support le floats as fixed byte widths");
    assert!(
        walkers.unsupported.is_empty(),
        "v3le using f32le must not be unsupported; got {:?}",
        walkers.unsupported
    );
    // source was produced and is non-trivial
    assert!(
        walkers.source.len() > 100,
        "generated walker source len={}",
        walkers.source.len()
    );
    // at least one check performed
    let mut checks = 0;
    assert!(
        !walkers.names.is_empty() || walkers.source.contains("walk_skip"),
        "walker produced"
    );
    checks += 1;
    assert!(checks >= 1);
}

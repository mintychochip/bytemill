//! Ports tests/test_emit_tree_real_tree.py: emit every shaped packet of the
//! committed ledger into a copied rstom tree.
//!
//! Deviation: scratch dirs use a Drop guard (unique per call: pid + counter +
//! nanos) instead of `tempfile.TemporaryDirectory`; the recursive copy and the
//! `*.json` class-set scan are hand-rolled with std only (no new
//! dependencies); elapsed time goes to stderr.

mod common;

use bytemill::emit_tree::emit_tree;
use bytemill::minestom::build_units;
use bytemill::shapes::{module_path, shape_to_json};
use std::path::{Path, PathBuf};

/// Unique scratch dir, removed on drop (even on assertion failure).
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "bml-tree-real-{tag}-{}-{n}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempRoot(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path);
        } else {
            std::fs::copy(entry.path(), &dst_path).unwrap();
        }
    }
}

fn has_rustfmt() -> bool {
    std::process::Command::new("rustfmt")
        .arg("--version")
        .output()
        .is_ok()
}

#[test]
fn emit_every_committed_packet() {
    let t0 = std::time::Instant::now();
    let rstom = common::rstom_tables::rstom_dir();
    let packet_src = bytemill::shapes::packet_root(&rstom);
    if !packet_src.is_dir() {
        eprintln!("skipped: rstom worktree packet sources not present");
        return;
    }
    let Some(table) = common::rstom_tables::real_table() else {
        eprintln!("skipped: rstom packets.json not present");
        return;
    };
    if !common::committed_shapes_present() {
        eprintln!("{}", common::MISSING_SHAPES_MSG);
        return;
    }
    let resolved = common::load_minecraft().unwrap();
    let units = build_units(&resolved, &table).unwrap();
    let ledger =
        bytemill::shapes::load_ledger(&common::repo_root().join("targets/rstom/shapes")).unwrap();
    // The hand files are gone: each unit's shape is the committed one, not an extraction.
    let shaped: Vec<_> = units
        .iter()
        .filter(|unit| ledger.iter().any(|(m, _)| *m == module_path(unit)))
        .collect();
    assert_eq!(
        shaped.len(),
        ledger.len(),
        "every committed shape names a packet of the protocol"
    );

    let scratch = TempRoot::new("realtree");
    let ws = scratch.path().join("rstom");
    copy_dir_recursive(
        &packet_src,
        &ws.join("crates/netherstar/src/network/packet"),
    );
    // `write` creates parents, but `copy` does not: make them first.
    std::fs::create_dir_all(ws.join("tools/packets_gen/schema")).unwrap();
    std::fs::copy(
        common::rstom_tables::packets_gen_schema(),
        ws.join("tools/packets_gen/schema/packets.json"),
    )
    .unwrap();

    // The pin gate files the check compares come along, like the real workspace has them.
    let tests_src = bytemill::shapes::crate_root(&rstom).join("tests");
    let tests_dst = bytemill::shapes::crate_root(&ws).join("tests");
    std::fs::create_dir_all(&tests_dst).unwrap();
    std::fs::copy(
        tests_src.join("bml_wire_pins.rs"),
        tests_dst.join("bml_wire_pins.rs"),
    )
    .unwrap();
    copy_dir_recursive(&tests_src.join("bml"), &tests_dst.join("bml"));

    // The committed ledger shapes, with their bindings, hooks and accepted constructs.
    let shapes_dir = scratch.path().join("shapes");
    let mut shaped_modules = std::collections::BTreeSet::new();
    for unit in &shaped {
        let module = module_path(unit);
        let (_, shape) = ledger
            .iter()
            .find(|(m, _)| *m == module)
            .unwrap_or_else(|| panic!("no committed shape for {module}"));
        shaped_modules.insert(module.clone());
        write(
            &shapes_dir.join(format!("{module}.json")),
            &shape_to_json(shape),
        );
    }
    // The bindings table sits beside the shapes directory, as it does in the ledger.
    std::fs::copy(
        common::repo_root().join("targets/rstom/bindings.json"),
        scratch.path().join("bindings.json"),
    )
    .unwrap();

    let drift = emit_tree(&resolved, &ws, &table, &shapes_dir, true).unwrap();
    assert!(
        drift.is_empty(),
        "the committed ledger and the generated packets in the tree have drifted:\n{}",
        drift.join("\n")
    );
    emit_tree(&resolved, &ws, &table, &shapes_dir, false).unwrap();
    let pkt_root = ws.join("crates/netherstar/src/network/packet");
    let mut generated = Vec::new();
    for module in &shaped_modules {
        let path = pkt_root.join(format!("{module}.rs"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("//! Generated by `bml emit rust`"),
            "{module}"
        );
        generated.push(path);
    }
    assert_eq!(generated.len(), ledger.len());

    assert!(has_rustfmt(), "rustfmt not installed");
    for path in &generated {
        let text = std::fs::read_to_string(path).unwrap();
        let lines = text.matches('\n').count() + usize::from(!text.ends_with('\n'));
        assert!(lines <= 400, "{} has {lines} lines", path.display());
        // The generated file only: its hand-owned `mod ext;`/`mod tests;` children are not the
        // emitter's to format (the emitter itself passes the same flags).
        let mut cmd = std::process::Command::new("rustfmt");
        if text.contains("mod ext;") || text.contains("mod tests;") {
            cmd.args(["--unstable-features", "--skip-children"]);
        }
        let out = cmd
            .args(["--edition", "2024", "--check"])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}: {}{}",
            path.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let first_bytes: Vec<(PathBuf, Vec<u8>)> = generated
        .iter()
        .map(|p| (p.clone(), std::fs::read(p).unwrap()))
        .collect();
    emit_tree(&resolved, &ws, &table, &shapes_dir, false).unwrap();
    assert!(
        emit_tree(&resolved, &ws, &table, &shapes_dir, true)
            .unwrap()
            .is_empty()
    );
    for (path, data) in &first_bytes {
        assert_eq!(&std::fs::read(path).unwrap(), data, "{}", path.display());
    }

    eprintln!(
        "real-tree emit test elapsed {:.1}s",
        t0.elapsed().as_secs_f64()
    );
}

//! Ports tests/test_emit_tree_safety.py.

mod common;

use bps::emit_rstom::{packet_body, PacketBody};
use bps::emit_tree::{
    assert_hand_content_preserved, emit_tree, missing_hand_markers,
};
use bps::minestom::ClassTable;
use bps::ordmap::OrderedMap;
use bps::shapes::{
    extract_shape, shape_to_json, FieldShape, NewShape, SerializerShape, Shape,
};
use bps::tree_packet::{tree_packet, validate_shape};
use bps::Value;
use std::path::{Path, PathBuf};

// Reuse the keep-alive fixtures from the emit-tree test module.
include!("fixtures/mod.rs");

fn workspace_with_table(tag: &str) -> PathBuf {
    let ws = temp_dir(tag).join("rstom");
    write(
        &ws.join("tools/packets_gen/schema/packets.json"),
        &keep_alive_table_json(),
    );
    ws
}

fn ghost_shape() -> Shape {
    Shape {
        cls: "GhostPacket".into(),
        module: "client/common/ghost_packet".into(),
        module_docs: Vec::new(),
        struct_docs: Vec::new(),
        derives: Vec::new(),
        form: "unit".into(),
        fields: Vec::new(),
        new: NewShape {
            present: false,
            const_: false,
            must_use: false,
            attrs: Vec::new(),
            docs: Vec::new(),
            body: Vec::new(),
            params: None,
        },
        serializer_fn_docs: Vec::new(),
        serializer_fn_must_use: false,
        serializer: SerializerShape {
            style: "constant".into(),
            name: "GhostPacket".into(),
            public: false,
            docs: Vec::new(),
            read_via_new: false,
        },
        extras: None,
        ..Shape::default()
    }
}

#[test]
fn missing_doc_in_generated_raises() {
    let hand = "//! top\n/// keep me\npub struct X;\n";
    let generated = "//! top\npub struct X;\n";
    assert_eq!(missing_hand_markers(hand, generated, ""), vec!["/// keep me"]);
}

#[test]
fn doc_only_in_ext_is_accepted() {
    let hand = "/// only in ext\npub struct X;\n";
    let generated = "pub struct X;\n";
    let ext = "/// only in ext\n";
    assert!(missing_hand_markers(hand, generated, ext).is_empty());
}

#[test]
fn cfg_test_doc_is_ignored() {
    let hand = "/// real\n#[cfg(test)]\n/// test only\nmod t {}\n";
    let generated = "/// real\n";
    assert!(missing_hand_markers(hand, generated, "").is_empty());
}

#[test]
fn assert_hand_content_preserved_names_module() {
    let err = assert_hand_content_preserved("m/a/x", "/// lost\n", "", None).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("m/a/x"), "{text}");
    assert!(text.contains("hand content lost"), "{text}");
}

#[test]
fn stale_ledger_module_raises() {
    let (resolved, _) = analyze_keep_alive();
    let shapes = temp_dir("stale");
    write(&shapes.join("client/common/ghost_packet.json"), &shape_to_json(&ghost_shape()));
    let ws = workspace_with_table("stale");
    let err = emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, false).unwrap_err();
    assert!(err.to_string().contains("stale shape"), "{err}");
}

#[test]
fn changed_hand_file_raises() {
    let (resolved, _unit) = analyze_keep_alive();
    let shapes = temp_dir("changed");
    write(
        &shapes.join("client/common/client_keep_alive_packet.json"),
        &shape_to_json(&keep_alive_shape()),
    );
    let ws = workspace_with_table("changed");
    let path = ws.join("crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs");
    write(&path, "// hand but not matching shape\n");
    let err = emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, false).unwrap_err();
    assert!(
        err.to_string()
            .contains("hand file changed since its shape was extracted"),
        "{err}"
    );
}

#[test]
fn unit_shape_with_fields_raises() {
    let (resolved, unit) = analyze_keep_alive();
    let mut shape = keep_alive_shape();
    shape.form = "unit".into();
    shape.fields = vec![FieldShape {
        name: "id".into(),
        type_: "i64".into(),
        docs: Vec::new(),
        wire: None,
    }];
    let err = tree_packet(&unit, &shape, &packet_body(&unit, &resolved).unwrap()).unwrap_err();
    assert!(err.to_string().contains("unit shape must have no fields"), "{err}");
}

#[test]
fn constant_serializer_on_non_unit_raises() {
    let (resolved, unit) = analyze_keep_alive();
    let mut shape = keep_alive_shape();
    shape.serializer = SerializerShape {
        style: "constant".into(),
        name: "Self".into(),
        public: false,
        docs: Vec::new(),
        read_via_new: false,
    };
    let err = tree_packet(&unit, &shape, &packet_body(&unit, &resolved).unwrap()).unwrap_err();
    assert!(
        err.to_string().contains("constant serializer requires unit form"),
        "{err}"
    );
}

#[test]
fn mismatch_raises_with_module_and_field() {
    let (resolved, _) = analyze_keep_alive();
    let shapes = temp_dir("mismatch2");
    let mut bad = keep_alive_shape();
    bad.fields[0] = FieldShape {
        name: "id".into(),
        type_: "i32".into(),
        docs: vec!["Java: `id()`.".into()],
        wire: None,
    };
    write(
        &shapes.join("client/common/client_keep_alive_packet.json"),
        &shape_to_json(&bad),
    );
    let ws = workspace_with_table("mismatch2");
    let err = emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, false).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("client/common/client_keep_alive_packet"), "{msg}");
    assert!(msg.contains("id"), "{msg}");
}

#[test]
fn missing_ext_raises_create_first() {
    let (resolved, _unit) = analyze_keep_alive();
    let mut shape = keep_alive_shape();
    shape.extras = Some("client_keep_alive_packet.ext.rs".into());
    let shapes = temp_dir("missingext");
    write(
        &shapes.join("client/common/client_keep_alive_packet.json"),
        &shape_to_json(&shape),
    );
    let ws = workspace_with_table("missingext");
    let err = emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, false).unwrap_err();
    assert!(
        err.to_string().contains(
            "create crates/rstom/src/network/packet/client/common/client_keep_alive_packet.ext.rs first"
        ),
        "{err}"
    );
}

#[test]
fn over_400_lines_raises() {
    let (resolved, unit) = analyze_keep_alive();
    let mut shape = keep_alive_shape();
    shape.module_docs = (0..380).map(|i| format!("//! pad {i}")).collect();
    let err = tree_packet(&unit, &shape, &packet_body(&unit, &resolved).unwrap()).unwrap_err();
    assert!(err.to_string().contains("exceeds 400 lines"), "{err}");
}

#[test]
fn exactly_400_lines_accepted() {
    let (resolved, unit) = analyze_keep_alive();
    let base_lines = tree_packet(&unit, &keep_alive_shape(), &packet_body(&unit, &resolved).unwrap())
        .unwrap()
        .lines()
        .count();
    let need = 400 - base_lines;
    assert!(need > 0);
    let mut found = false;
    for extra in need.saturating_sub(3)..need + 4 {
        let mut shape = keep_alive_shape();
        shape.module_docs = (0..extra).map(|i| format!("pad {i}")).collect();
        if let Ok(text) = tree_packet(&unit, &shape, &packet_body(&unit, &resolved).unwrap()) {
            let lines = text.lines().count();
            if lines == 400 {
                found = true;
                break;
            }
        }
    }
    assert!(found, "expected some padding to hit exactly 400 lines");
}

#[test]
fn check_never_writes_and_flags_drift_orphan_and_hand_written() {
    let (resolved, _unit) = analyze_keep_alive();
    let shapes = temp_dir("checkro");
    write(
        &shapes.join("client/common/client_keep_alive_packet.json"),
        &shape_to_json(&keep_alive_shape()),
    );
    let ws = workspace_with_table("checkro");
    emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, false).unwrap();
    let pkt = ws.join("crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs");
    let before = std::fs::read(&pkt).unwrap();

    // --check never writes.
    emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, true).unwrap();
    assert_eq!(std::fs::read(&pkt).unwrap(), before);

    // A drifted file is reported.
    let text = std::fs::read_to_string(&pkt).unwrap();
    write(&pkt, &text.replacen("i64", "i64 /* drift */", 1));
    let drift = emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, true).unwrap();
    assert!(drift.iter().any(|m| m.contains("drift")), "{drift:?}");

    // Re-emit, then a hand-written file is reported.
    emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, false).unwrap();
    write(&pkt, "// hand\n");
    let hand = emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, true).unwrap();
    assert!(hand.iter().any(|m| m.contains("hand-written")), "{hand:?}");

    // A generated file with no shape is an orphan.
    let orphan = ws.join("crates/rstom/src/network/packet/server/play/orphan_packet.rs");
    write(&orphan, "//! Generated by `bps emit rust` orphan\n");
    let msgs = emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, true).unwrap();
    assert!(msgs.iter().any(|m| m.contains("orphan")), "{msgs:?}");
}

#[test]
fn check_on_empty_workspace_creates_no_files() {
    let (resolved, _) = analyze_keep_alive();
    let shapes = temp_dir("emptyws");
    write(
        &shapes.join("client/common/client_keep_alive_packet.json"),
        &shape_to_json(&keep_alive_shape()),
    );
    let ws = temp_dir("emptyws").join("rstom");
    std::fs::create_dir_all(&ws).unwrap();
    write(
        &ws.join("tools/packets_gen/schema/packets.json"),
        &keep_alive_table_json(),
    );
    let before = workspace_snapshot(&ws);
    emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, true).unwrap();
    let after = workspace_snapshot(&ws);
    assert_eq!(before, after, "check=True modified workspace entries");
}

fn workspace_snapshot(ws: &Path) -> Vec<(String, bool, std::time::SystemTime)> {
    let mut out = Vec::new();
    fn walk(dir: &Path, ws: &Path, out: &mut Vec<(String, bool, std::time::SystemTime)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(ws).unwrap().to_string_lossy().replace('\\', "/");
            let meta = std::fs::metadata(&path).unwrap();
            let mtime = meta.modified().unwrap();
            out.push((rel.clone(), meta.is_dir(), mtime));
            if meta.is_dir() {
                walk(&path, ws, out);
            }
        }
    }
    walk(ws, ws, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn emit_refuses_when_ext_would_be_orphaned() {
    let (resolved, _) = analyze_keep_alive();
    let mut shape = keep_alive_shape();
    shape.extras = None;
    let shapes = temp_dir("orphanext");
    write(
        &shapes.join("client/common/client_keep_alive_packet.json"),
        &shape_to_json(&shape),
    );
    let ws = workspace_with_table("orphanext");
    let pkt_dir = ws.join("crates/rstom/src/network/packet/client/common");
    write(
        &pkt_dir.join("client_keep_alive_packet.rs"),
        "//! Generated by `bps emit rust` from schemas/minecraft_26_3*.bps: keep_alive\n#[path = \"client_keep_alive_packet.ext.rs\"]\nmod ext;\n",
    );
    write(&pkt_dir.join("client_keep_alive_packet.ext.rs"), "//! orphan\n");
    let err = emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, false).unwrap_err();
    assert!(err.to_string().contains("would be orphaned"), "{err}");
    assert!(
        err.to_string().contains("client_keep_alive_packet.ext.rs"),
        "{err}"
    );
}

#[test]
fn rejects_scan_body_using_frames_module() {
    let (_resolved, unit) = analyze_keep_alive();
    let body = PacketBody {
        fields: Vec::new(),
        field_types: OrderedMap::new(),
        writes: String::new(),
        reads: "        let raw = crate::frames::nbt(&buf) as i32;".into(),
        ctor: String::new(),
        init: String::new(),
        used_wires: Vec::new(),
        needs_fixed_raw_bytes: false,
        rtypes: std::collections::BTreeSet::new(),
    };
    let err = tree_packet(&unit, &keep_alive_shape(), &body).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("frames walker"), "{text}");
    assert!(text.contains("nbt"), "{text}");
    let _ = validate_shape(&keep_alive_shape());
}

#[test]
fn extras_emit_check_idempotent() {
    let shape = Shape {
        cls: "ProbePacket".into(),
        module: "server/play/probe_packet".into(),
        module_docs: Vec::new(),
        struct_docs: Vec::new(),
        derives: vec!["Clone".into()],
        form: "braced".into(),
        fields: vec![FieldShape {
            name: "x".into(),
            type_: "i32".into(),
            docs: Vec::new(),
            wire: None,
        }],
        new: NewShape {
            present: true,
            const_: false,
            must_use: false,
            attrs: Vec::new(),
            docs: Vec::new(),
            body: Vec::new(),
            params: None,
        },
        serializer_fn_docs: Vec::new(),
        serializer_fn_must_use: false,
        serializer: SerializerShape {
            style: "struct".into(),
            name: "ProbePacketSerializer".into(),
            public: false,
            docs: Vec::new(),
            read_via_new: false,
        },
        extras: Some("probe_packet.ext.rs".into()),
        ..Shape::default()
    };
    let source = probe_source("packet probe { x: i32be }");
    let dir = temp_dir("extrasidem");
    let schema = dir.join("p.bps");
    write(&schema, &source);
    let resolved = bps::analyze(&bps::load_schema(&[schema]).unwrap()).unwrap();
    let shapes = temp_dir("extrasidem-shapes");
    write(
        &shapes.join("server/play/probe_packet.json"),
        &shape_to_json(&shape),
    );
    let ws = temp_dir("extrasidem-ws").join("rstom");
    write(
        &ws.join("tools/packets_gen/schema/packets.json"),
        &common::rstom_tables::probe_schema_json(),
    );
    write(
        &ws.join("crates/rstom/src/network/packet/server/play/probe_packet.ext.rs"),
        "//! scratch\n",
    );
    emit_tree(&resolved, &ws, &common::rstom_tables::probe_table(), &shapes, false).unwrap();
    let first = emit_tree(&resolved, &ws, &common::rstom_tables::probe_table(), &shapes, true).unwrap();
    assert!(first.is_empty(), "{first:?}");
    let mut before: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    fn collect(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.to_string_lossy().contains("crates/rstom/src/network/packet") {
                out.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
    }
    collect(&ws, &mut before);
    emit_tree(&resolved, &ws, &common::rstom_tables::probe_table(), &shapes, false).unwrap();
    for (path, data) in &before {
        assert_eq!(&std::fs::read(path).unwrap(), data, "{}", path.display());
    }
}

#[test]
fn unit_with_new_extracted() {
    // The rstom worktree must be present for the hand files.
    let rstom = common::rstom_tables::rstom_dir();
    let packet_root = rstom.join("crates/rstom/src/network/packet");
    if !packet_root.is_dir() {
        eprintln!("rstom worktree packet sources not present; skipping");
        return;
    }
    let cases: [(&str, &str, &str, &str); 2] = [
        (
            "EnterCombatEventPacket",
            "server/play/enter_combat_event_packet",
            "enter_combat_event",
            "packet enter_combat_event {}\nprotocol minecraft @ v1 {\n    id = 1\n    frame default { length: varint id: varint payload: rest }\n    route play.clientbound 0x00 => enter_combat_event\n}\n",
        ),
        (
            "LowDiskSpaceWarningPacket",
            "server/play/low_disk_space_warning_packet",
            "low_disk_space_warning",
            "packet low_disk_space_warning {}\nprotocol minecraft @ v1 {\n    id = 1\n    frame default { length: varint id: varint payload: rest }\n    route play.clientbound 0x00 => low_disk_space_warning\n}\n",
        ),
    ];
    for (cls, module, pkt_name, body_src) in cases {
        let path = packet_root.join(format!("{module}.rs"));
        let text = std::fs::read_to_string(&path).unwrap();
        let shape = extract_shape(&text, cls, module, Some(&path)).unwrap();
        let source = format!(
            "import \"{}\"\n{body_src}",
            common::repo_root().join("schemas/std/varint.bps").display()
        );
        let dir = temp_dir("unitnew");
        let schema = dir.join("x.bps");
        write(&schema, &source);
        let resolved = bps::analyze(&bps::load_schema(&[schema]).unwrap()).unwrap();
        let table = single_class_table("server", "play", cls);
        let unit = bps::minestom::build_units(&resolved, &table)
            .unwrap()
            .into_iter()
            .find(|u| u.pkt.name == pkt_name)
            .unwrap();
        let text = tree_packet(&unit, &shape, &packet_body(&unit, &resolved).unwrap()).unwrap();
        assert!(text.contains("pub fn new() -> Self"), "{cls}: {text}");
        assert!(text.contains("ConstantSerializer::new(Self)"), "{cls}: {text}");
        assert!(text.contains(&format!("pub struct {cls};")), "{cls}: {text}");
    }
}

fn single_class_table(group: &str, package: &str, name: &str) -> ClassTable {
    let mut entry = OrderedMap::new();
    entry.insert("name", Value::str(name));
    entry.insert(
        "java",
        Value::str(format!(
            "net.minestom.server.network.packet.{group}.{package}.{name}"
        )),
    );
    entry.insert("group", Value::str(group));
    entry.insert("state", Value::str(package));
    let mut table = OrderedMap::new();
    table.insert("state", Value::str("play"));
    table.insert("side", Value::str("SERVER"));
    table.insert("packets", Value::list(vec![Value::str(name)]));
    let mut root = OrderedMap::new();
    root.insert("packets", Value::list(vec![Value::map(entry)]));
    root.insert("stateTables", Value::list(vec![Value::map(table)]));
    ClassTable::from_schema(&Value::map(root)).unwrap()
}
// ---------------------------------------------------------------------------
// Remaining tests/test_emit_tree_safety.py cases.
// `check_flags_drift_orphan_and_hand_written` is already covered by
// `check_never_writes_and_flags_drift_orphan_and_hand_written` above, and both
// `enter_combat...`/`low_disk_space...` cases by `unit_with_new_extracted`;
// neither is duplicated here.
// Deviations: scratch dirs use a Drop guard (unique per call: pid + counter +
// nanos) instead of `tempfile.TemporaryDirectory`. Python patches
// `subprocess.run` to fail rustfmt and re-runs the golden test under pytest;
// Rust has no patchable seam, so both tests run a filtered child copy of this
// test binary with a rigged environment — the parent only waits on the child
// and never mutates its own `PATH`/`BPS_RSTOM_DIR`, which parallel siblings
// also read.
// ---------------------------------------------------------------------------

/// Unique scratch dir, removed on drop (even on assertion failure).
struct TempRoot(std::path::PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "bps-tree-safety-{tag}-{}-{n}-{:?}",
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

#[test]
fn r11_matching_hand_file_is_swapped() {
    let hand_src = common::rstom_tables::rstom_dir()
        .join("crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs");
    if !hand_src.is_file() {
        eprintln!("skipped: rstom hand keep-alive not present");
        return;
    }
    let (resolved, _unit) = analyze_keep_alive();
    let scratch = TempRoot::new("r11swap");
    let shapes = scratch.path().join("shapes");
    write(
        &shapes.join("client/common/client_keep_alive_packet.json"),
        &shape_to_json(&keep_alive_shape()),
    );
    let ws = scratch.path().join("rstom");
    write(
        &ws.join("tools/packets_gen/schema/packets.json"),
        &keep_alive_table_json(),
    );
    let path =
        ws.join("crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::copy(&hand_src, &path).unwrap();
    emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, false).unwrap();
    let out = std::fs::read_to_string(&path).unwrap();
    assert!(
        out.lines().next().unwrap_or("").contains("Generated by `bps emit rust`"),
        "{out}"
    );
}

/// This process's `bps-emit-tree-*.rs` temporaries, sorted. Scoped to our pid:
/// sibling tests on other threads share the system temp dir.
fn leaked_tree_tmps() -> Vec<PathBuf> {
    let prefix = format!("bps-emit-tree-{}-", std::process::id());
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && name.ends_with(".rs") {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

#[test]
fn rustfmt_failure_does_not_leak_temp_files() {
    let scratch = TempRoot::new("rustfmtleak");
    // A fake `rustfmt` that always fails, leading `PATH` in the child only.
    let bin = scratch.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("rustfmt");
    std::fs::write(&fake, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    let path = std::env::join_paths(dirs).unwrap();
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(&exe)
        .args(["--exact", "rustfmt_failure_worker"])
        .env("PATH", path)
        .env("BPS_EMIT_TREE_RUSTFMT_WORKER", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "worker failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run by the test above with a failing `rustfmt` first on `PATH`; a no-op on its own.
#[test]
fn rustfmt_failure_worker() {
    if std::env::var_os("BPS_EMIT_TREE_RUSTFMT_WORKER").is_none() {
        return;
    }
    let scratch = TempRoot::new("rustfmtworker");
    let before = leaked_tree_tmps();
    let (resolved, _unit) = analyze_keep_alive();
    let shapes = scratch.path().join("shapes");
    write(
        &shapes.join("client/common/client_keep_alive_packet.json"),
        &shape_to_json(&keep_alive_shape()),
    );
    let ws = scratch.path().join("rstom");
    write(
        &ws.join("tools/packets_gen/schema/packets.json"),
        &keep_alive_table_json(),
    );
    let err = emit_tree(&resolved, &ws, &keep_alive_table(), &shapes, false).unwrap_err();
    assert!(err.to_string().contains("rustfmt"), "{err}");
    assert_eq!(
        leaked_tree_tmps(),
        before,
        "temp file leaked after rustfmt failure"
    );
}

/// Format through a temporary `.rs` file, like `test_emit_tree.rs`' helper.
fn rustfmt_file(text: &str) -> String {
    let scratch = TempRoot::new("fmtfile");
    let path = scratch.path().join("x.rs");
    write(&path, text);
    let _ = std::process::Command::new("rustfmt")
        .args(["--edition", "2024", path.to_str().unwrap()])
        .output();
    std::fs::read_to_string(&path).unwrap_or_else(|_| text.to_string())
}

#[test]
fn golden_keep_alive_without_rstom_worktree() {
    let scratch = TempRoot::new("goldenwt");
    // Never created: the child must render the golden packet without it.
    let missing = scratch.path().join("missing-rstom-worktree");
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(&exe)
        .args(["--exact", "golden_keep_alive_worker"])
        .env("BPS_RSTOM_DIR", &missing)
        .env("BPS_EMIT_TREE_GOLDEN_WORKER", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "worker failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn golden_keep_alive_worker() {
    if std::env::var_os("BPS_EMIT_TREE_GOLDEN_WORKER").is_none() {
        return;
    }
    let (resolved, unit) = analyze_keep_alive();
    let shape = keep_alive_shape();
    let body = packet_body(&unit, &resolved).unwrap();
    let got = rustfmt_file(&tree_packet(&unit, &shape, &body).unwrap());
    let golden =
        common::repo_root().join("crates/bps/tests/fixtures/rstom_tree/client_keep_alive_packet.rs");
    let want = rustfmt_file(&std::fs::read_to_string(&golden).unwrap());
    assert_eq!(got, want);
}

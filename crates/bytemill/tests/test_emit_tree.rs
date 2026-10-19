//! Ports tests/test_emit_tree.py.

mod common;

use bytemill::ast::ResolvedSchema;
use bytemill::emit_rstom::packet_body;
use bytemill::emit_tree::emit_tree;
use bytemill::errors::ErrorKind;
use bytemill::minestom::{build_units, ClassTable};
use bytemill::ordmap::OrderedMap;
use bytemill::shapes::{
    extract_shape, hand_packet_path, shape_to_json, FieldShape, NewShape, SerializerShape, Shape,
};
use bytemill::tree_packet::{tree_packet, validate_shape};
use bytemill::Value;
use std::path::{Path, PathBuf};

include!("fixtures/mod.rs");

/// The workspace/schema a probe emit needs: packets.json plus a schema file.
struct ProbeWorkspace {
    pub root: PathBuf,
    pub resolved: ResolvedSchema,
}

fn probe_schema_json() -> String {
    common::rstom_tables::probe_schema_json()
}

fn probe_workspace(tag: &str) -> ProbeWorkspace {
    let root = temp_dir(tag);
    let workspace = root.join("rstom");
    write(
        &workspace.join("tools/packets_gen/schema/packets.json"),
        &probe_schema_json(),
    );
    let schema = root.join("probe.bps");
    write(&schema, &probe_source("packet probe { a: u8 }"));
    let resolved = bytemill::analyze(
        &bytemill::load_schema(&[schema]).unwrap(),
    )
    .unwrap();
    ProbeWorkspace { root, resolved }
}

fn paths(workspace: &Path) -> [(&'static str, PathBuf); 3] {
    let base = workspace.join("crates/rstom/tests");
    [
        ("gate", base.join("bps_wire_pins.rs")),
        ("pins", base.join("bps/wire_pins.json")),
        ("known", base.join("bps/wire_pins_known.json")),
    ]
}

#[test]
fn writes_the_pin_gate_and_nothing_else_without_shapes() {
    let ws = probe_workspace("gate");
    let shapes = ws.root.join("shapes");
    std::fs::create_dir_all(&shapes).unwrap();
    let table = common::rstom_tables::probe_table();
    let written = emit_tree(&ws.resolved, &ws.root.join("rstom"), &table, &shapes, false).unwrap();
    let rel: std::collections::HashSet<String> = written.into_iter().collect();
    let expected: std::collections::HashSet<String> = [
        "crates/rstom/tests/bps_wire_pins.rs",
        "crates/rstom/tests/bps/wire_pins.json",
        "crates/rstom/tests/bps/wire_pins_known.json",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(rel, expected);
    for (_, path) in paths(&ws.root.join("rstom")) {
        assert!(path.is_file(), "{}", path.display());
    }
    let known = paths(&ws.root.join("rstom"))[2].1.clone();
    assert_eq!(std::fs::read_to_string(&known).unwrap(), "[]\n");
    let src = ws.root.join("rstom/crates/rstom/src");
    assert!(!src.exists());
}

#[test]
fn known_file_is_never_overwritten() {
    let ws = probe_workspace("known");
    let p = paths(&ws.root.join("rstom"));
    write(&p[2].1, "[{\"side\":\"CLIENT\",\"state\":\"PLAY\",\"id\":0,\"category\":\"x\",\"reason\":\"y\"}]\n");
    let table = common::rstom_tables::probe_table();
    emit_tree(&ws.resolved, &ws.root.join("rstom"), &table, &ws.root.join("shapes"), false).unwrap();
    let text = std::fs::read_to_string(&p[2].1).unwrap();
    assert!(text.contains("CLIENT"), "{text}");
}

#[test]
fn check_reports_a_drifted_pin_file() {
    let ws = probe_workspace("drift");
    let table = common::rstom_tables::probe_table();
    let p = paths(&ws.root.join("rstom"));
    emit_tree(&ws.resolved, &ws.root.join("rstom"), &table, &ws.root.join("shapes"), false).unwrap();
    assert!(
        emit_tree(&ws.resolved, &ws.root.join("rstom"), &table, &ws.root.join("shapes"), true)
            .unwrap()
            .is_empty()
    );
    std::fs::write(&p[1].1, "{}\n").unwrap();
    let drift =
        emit_tree(&ws.resolved, &ws.root.join("rstom"), &table, &ws.root.join("shapes"), true).unwrap();
    assert_eq!(drift.len(), 1);
    assert!(drift[0].contains("wire_pins.json"), "{}", drift[0]);
}

#[test]
fn a_second_emit_is_a_no_op() {
    let ws = probe_workspace("noop");
    let p = paths(&ws.root.join("rstom"));
    write(&p[2].1, "[{\"side\":\"CLIENT\",\"state\":\"PLAY\",\"id\":0,\"category\":\"x\",\"reason\":\"y\"}]\n");
    let table = common::rstom_tables::probe_table();
    let shapes = ws.root.join("shapes");
    emit_tree(&ws.resolved, &ws.root.join("rstom"), &table, &shapes, false).unwrap();
    let first: Vec<Vec<u8>> =
        paths(&ws.root.join("rstom")).iter().map(|(_, p)| std::fs::read(p).unwrap()).collect();
    emit_tree(&ws.resolved, &ws.root.join("rstom"), &table, &shapes, false).unwrap();
    for (i, (_, path)) in paths(&ws.root.join("rstom")).iter().enumerate() {
        assert_eq!(&std::fs::read(path).unwrap(), &first[i], "{}", path.display());
    }
}

#[test]
fn a_refused_emit_writes_nothing() {
    let ws = probe_workspace("refuse");
    let p = paths(&ws.root.join("rstom"));
    write(&p[0].1, "// hand-maintained gate\n");
    write(&p[1].1, "{\"rows\":[]}\n");
    let before: Vec<Vec<u8>> = paths(&ws.root.join("rstom"))
        .iter()
        .filter(|(_, p)| p.is_file())
        .map(|(_, p)| std::fs::read(p).unwrap())
        .collect();
    let table = common::rstom_tables::probe_table();
    let err = emit_tree(&ws.resolved, &ws.root.join("rstom"), &table, &ws.root.join("shapes"), false)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Emit, "{err}");
    let mut i = 0;
    for (_, path) in paths(&ws.root.join("rstom")) {
        if path.is_file() && i < before.len() {
            assert_eq!(&std::fs::read(&path).unwrap(), &before[i]);
            i += 1;
        }
    }
    assert!(!p[2].1.exists());
}

#[test]
fn cli_check_on_rust_exits_3() {
    let ws = probe_workspace("clicheck");
    let code = bytemill::cli::main(&[
        "emit".into(),
        "rust".into(),
        ws.root.join("probe.bps").to_string_lossy().into_owned(),
        "--out".into(),
        ws.root.join("rstom/out").to_string_lossy().into_owned(),
        "--check".into(),
    ])
    .unwrap();
    assert_eq!(code, 3);
}

#[test]
fn cli_without_rstom_exits_3() {
    let dir = temp_dir("clino");
    let schema = dir.join("probe.bps");
    write(&schema, &probe_source("packet probe { a: u8 }"));
    let code = bytemill::cli::main(&[
        "emit".into(),
        "rstom-tree".into(),
        schema.to_string_lossy().into_owned(),
    ])
    .unwrap();
    assert_eq!(code, 3);
}

// ---------------------------------------------------------------------------
// tree_packet rendering
// ---------------------------------------------------------------------------

fn rustfmt(text: &str) -> String {
    use std::io::Write;
    let mut path = std::env::temp_dir();
    path.push(format!(
        "bps-fmt-{}-{:?}.rs",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, text).unwrap();
    let _ = std::process::Command::new("rustfmt")
        .args(["--edition", "2024", path.to_str().unwrap()])
        .output();
    let out = std::fs::read_to_string(&path).unwrap_or_else(|_| text.to_string());
    let _ = std::fs::remove_file(&path);
    let _ = std::io::stdout().flush();
    out
}

#[test]
fn golden_keep_alive() {
    let (resolved, unit) = analyze_keep_alive();
    let shape = keep_alive_shape();
    let body = packet_body(&unit, &resolved).unwrap();
    let got = rustfmt(&tree_packet(&unit, &shape, &body).unwrap());
    let golden = common::repo_root().join("crates/bytemill/tests/fixtures/rstom_tree/client_keep_alive_packet.rs");
    let want = rustfmt(&std::fs::read_to_string(&golden).unwrap());
    assert_eq!(got, want);
}

#[test]
fn unit_packet_uses_constant_serializer_and_has_no_new() {
    let clear = common::repo_root().join("targets/rstom/shapes/server/common/clear_dialog_packet.json");
    if !clear.is_file() && !common::committed_shapes_present() {
        eprintln!("{}", common::MISSING_SHAPES_MSG);
        return;
    }
    let shape: Shape = if clear.is_file() {
        let text = std::fs::read_to_string(&clear).unwrap();
        bytemill::shapes::shape_from_json(&text).unwrap()
    } else {
        // The ClearDialogPacket hand file is not in this checkout; render from
        // the shape the ledger records for it.
        let ledger = bytemill::shapes::load_ledger(&common::repo_root().join("targets/rstom/shapes")).unwrap();
        ledger
            .into_iter()
            .find(|(_, s)| s.cls == "ClearDialogPacket")
            .map(|(_, s)| s)
            .expect("ClearDialogPacket in the ledger")
    };
    let source = format!(
        "import \"{}\"\npacket clear_dialog {{}}\nprotocol minecraft @ v1 {{\n    id = 1\n    frame default {{ length: varint id: varint payload: rest }}\n    route play.clientbound 0x00 => clear_dialog\n}}\n",
        common::repo_root().join("schemas/std/varint.bps").display()
    );
    let dir = temp_dir("cleardialog");
    let schema = dir.join("x.bps");
    write(&schema, &source);
    let resolved = bytemill::analyze(&bytemill::load_schema(&[schema]).unwrap()).unwrap();
    let mut root = OrderedMap::new();
    let mut entry = OrderedMap::new();
    entry.insert("name", Value::str("ClearDialogPacket"));
    entry.insert(
        "java",
        Value::str("net.minestom.server.network.packet.server.common.ClearDialogPacket"),
    );
    entry.insert("group", Value::str("server"));
    entry.insert("state", Value::str("common"));
    root.insert("packets", Value::list(vec![Value::map(entry)]));
    let mut table = OrderedMap::new();
    table.insert("state", Value::str("play"));
    table.insert("side", Value::str("SERVER"));
    table.insert("packets", Value::list(vec![Value::str("ClearDialogPacket")]));
    root.insert("stateTables", Value::list(vec![Value::map(table)]));
    let table = ClassTable::from_schema(&Value::map(root)).unwrap();
    let unit = build_units(&resolved, &table)
        .unwrap()
        .into_iter()
        .find(|u| u.pkt.name == "clear_dialog")
        .unwrap();
    let body = packet_body(&unit, &resolved).unwrap();
    let text = tree_packet(&unit, &shape, &body).unwrap();
    assert!(!text.contains("pub fn new"), "{text}");
    assert!(text.contains("ConstantSerializer::new(ClearDialogPacket)"), "{text}");
    assert!(text.contains("pub struct ClearDialogPacket;"), "{text}");
}

#[test]
fn copy_fields_are_not_cloned() {
    let (resolved, unit) = analyze_keep_alive();
    let shape = keep_alive_shape();
    let body = packet_body(&unit, &resolved).unwrap();
    let text = tree_packet(&unit, &shape, &body).unwrap();
    assert!(text.contains("buffer.write(LONG, value.id);"), "{text}");
    assert!(!text.contains("value.id.clone()"), "{text}");

    let probe_shape = Shape {
        cls: "ProbePacket".into(),
        module: "server/play/probe_packet".into(),
        module_docs: Vec::new(),
        struct_docs: Vec::new(),
        derives: vec!["Clone".into(), "Debug".into(), "PartialEq".into()],
        form: "braced".into(),
        fields: vec![
            FieldShape { name: "n".into(), type_: "i64".into(), docs: Vec::new(), wire: None },
            FieldShape {
                name: "label".into(),
                type_: "String".into(),
                docs: Vec::new(),
                wire: None,
            },
        ],
        new: NewShape {
            present: true,
            const_: false,
            must_use: true,
            attrs: Vec::new(),
            docs: Vec::new(),
            body: Vec::new(),
            params: None,
        },
        serializer_fn_docs: Vec::new(),
        serializer_fn_must_use: true,
        serializer: SerializerShape {
            style: "struct".into(),
            name: "ProbePacketSerializer".into(),
            public: false,
            docs: Vec::new(),
            read_via_new: false,
        },
        extras: None,
        ..Shape::default()
    };
    let source = format!(
        "import \"{}\"\n{}",
        common::repo_root().join("schemas/minecraft_core.bps").display(),
        probe_source("packet probe { n: i64be label: string }")
    );
    let dir = temp_dir("probecopy");
    let schema = dir.join("p.bps");
    write(&schema, &source);
    let resolved2 = bytemill::analyze(&bytemill::load_schema(&[schema]).unwrap()).unwrap();
    let table = common::rstom_tables::probe_table();
    let unit2 = build_units(&resolved2, &table)
        .unwrap()
        .into_iter()
        .find(|u| u.pkt.name == "probe")
        .unwrap();
    let body2 = packet_body(&unit2, &resolved2).unwrap();
    let text2 = tree_packet(&unit2, &probe_shape, &body2).unwrap();
    assert!(text2.contains("value.label.clone()"), "{text2}");
}

#[test]
fn no_rstom_paths_in_a_tree_file() {
    let (resolved, unit) = analyze_keep_alive();
    let shape = keep_alive_shape();
    let body = packet_body(&unit, &resolved).unwrap();
    let text = tree_packet(&unit, &shape, &body).unwrap();
    assert!(!text.contains("rstom::"), "{text}");
    assert!(text.contains("crate::network::"), "{text}");
}

#[test]
fn new_body_from_the_shape_is_emitted() {
    let (resolved, unit) = analyze_keep_alive();
    let mut shape = keep_alive_shape();
    shape.new.body = vec!["        panic!(\"validated\");".into(), "        Self { id }".into()];
    let body = packet_body(&unit, &resolved).unwrap();
    let text = tree_packet(&unit, &shape, &body).unwrap();
    assert!(text.contains("panic!(\"validated\");"), "{text}");
}

#[test]
fn read_via_new_emits_constructor_call() {
    let (resolved, unit) = analyze_keep_alive();
    let mut shape = keep_alive_shape();
    shape.serializer.read_via_new = true;
    let body = packet_body(&unit, &resolved).unwrap();
    let text = tree_packet(&unit, &shape, &body).unwrap();
    assert!(text.contains("Box::new(ClientKeepAlivePacket::new(id))"), "{text}");
    assert!(!text.contains("Box::new(ClientKeepAlivePacket {"), "{text}");
}

#[test]
fn unit_read_via_new_is_rejected() {
    let mut shape = keep_alive_shape();
    shape.form = "unit".into();
    shape.fields.clear();
    shape.serializer.read_via_new = true;
    let err = validate_shape(&shape).unwrap_err();
    assert!(err.to_string().contains("unit packet cannot use read_via_new"), "{err}");
}

#[test]
fn read_without_via_new_uses_struct_literal() {
    let (resolved, unit) = analyze_keep_alive();
    let mut shape = keep_alive_shape();
    shape.serializer.read_via_new = false;
    let body = packet_body(&unit, &resolved).unwrap();
    let text = tree_packet(&unit, &shape, &body).unwrap();
    assert!(text.contains("Box::new(ClientKeepAlivePacket {"), "{text}");
    assert!(!text.contains("::new(id)"), "{text}");
}

#[test]
fn extras_adds_the_path_mod_and_reexports_it() {
    let shape = Shape {
        cls: "ProbePacket".into(),
        module: "server/play/probe_packet".into(),
        module_docs: Vec::new(),
        struct_docs: Vec::new(),
        derives: vec!["Clone".into()],
        form: "braced".into(),
        fields: vec![FieldShape { name: "x".into(), type_: "i32".into(), docs: Vec::new(), wire: None }],
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
    let dir = temp_dir("extras");
    let schema = dir.join("p.bps");
    write(&schema, &source);
    let resolved = bytemill::analyze(&bytemill::load_schema(&[schema]).unwrap()).unwrap();
    let table = common::rstom_tables::probe_table();
    let unit = build_units(&resolved, &table)
        .unwrap()
        .into_iter()
        .find(|u| u.pkt.name == "probe")
        .unwrap();
    let body = packet_body(&unit, &resolved).unwrap();
    let text = tree_packet(&unit, &shape, &body).unwrap();
    assert!(text.contains("#[path = \"probe_packet.ext.rs\"]"), "{text}");
    assert!(text.contains("mod ext;"), "{text}");
    assert!(text.contains("#[allow(unused_imports)]"), "{text}");
    assert!(text.contains("pub use ext::*;"), "{text}");
}

#[test]
fn shape_that_disagrees_with_the_wire_is_an_error() {
    let (resolved, _unit) = analyze_keep_alive();
    let mut shape = keep_alive_shape();
    shape.fields[0].type_ = "i32".into();
    let dir = temp_dir("mismatch");
    let shapes = dir.join("shapes");
    write(&shapes.join("client/common/client_keep_alive_packet.json"), &shape_to_json(&shape));
    let err = emit_tree(
        &resolved,
        &dir.join("ws"),
        &keep_alive_table(),
        &shapes,
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("id"), "{err}");
}

#[test]
fn emit_writes_shaped_packet() {
    let (resolved, unit) = analyze_keep_alive();
    let shape = keep_alive_shape();
    let dir = temp_dir("emitpkt");
    let workspace = dir.join("rstom");
    write(
        &workspace.join("tools/packets_gen/schema/packets.json"),
        &keep_alive_table_json(),
    );
    let shapes = dir.join("shapes");
    write(
        &shapes.join("client/common/client_keep_alive_packet.json"),
        &shape_to_json(&shape),
    );
    // The keep-alive table, not the probe table, so the unit routes.
    let written = emit_tree(&resolved, &workspace, &keep_alive_table(), &shapes, false).unwrap();
    let pkt = "crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs";
    assert!(written.iter().any(|w| w == pkt), "{written:?}");
    let text = std::fs::read_to_string(hand_packet_path(&workspace, &unit)).unwrap();
    assert!(text.contains("Generated by `bps emit rust`"), "{text}");
}
// ---------------------------------------------------------------------------
// Remaining tests/test_emit_tree.py cases.
// Deviation: scratch dirs use a Drop guard (unique per call: pid + counter +
// nanos) instead of `tempfile.TemporaryDirectory`; nothing here needs a regex.
// ---------------------------------------------------------------------------

/// Unique scratch dir, removed on drop (even on assertion failure).
struct TempRoot(std::path::PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "bps-tree-{tag}-{}-{n}-{:?}",
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

/// One Minestom class routed as play clientbound/serverbound, like the Python
/// `ClassTable` literals in `TreePacketExtractedShapeTest`.
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

#[test]
fn check_flags_a_shaped_packet_whose_file_is_hand_written() {
    let scratch = TempRoot::new("flaghand");
    let workspace = scratch.path().join("rstom");
    write(
        &workspace.join("tools/packets_gen/schema/packets.json"),
        &keep_alive_table_json(),
    );
    let (resolved, unit) = analyze_keep_alive();
    let shapes = scratch.path().join("shapes");
    write(
        &shapes.join("client/common/client_keep_alive_packet.json"),
        &shape_to_json(&keep_alive_shape()),
    );
    emit_tree(&resolved, &workspace, &keep_alive_table(), &shapes, false).unwrap();
    write(&hand_packet_path(&workspace, &unit), "// hand\n");
    let msgs = emit_tree(&resolved, &workspace, &keep_alive_table(), &shapes, true).unwrap();
    assert!(
        msgs.iter().any(|m| m.contains("hand-written")),
        "{msgs:?}"
    );
}

#[test]
fn check_flags_a_generated_file_with_no_shape() {
    let scratch = TempRoot::new("flagorphan");
    let workspace = scratch.path().join("rstom");
    write(
        &workspace.join("tools/packets_gen/schema/packets.json"),
        &keep_alive_table_json(),
    );
    let (resolved, _unit) = analyze_keep_alive();
    let shapes = scratch.path().join("shapes");
    write(
        &shapes.join("client/common/client_keep_alive_packet.json"),
        &shape_to_json(&keep_alive_shape()),
    );
    emit_tree(&resolved, &workspace, &keep_alive_table(), &shapes, false).unwrap();
    write(
        &workspace.join("crates/rstom/src/network/packet/server/play/orphan_packet.rs"),
        "//! Generated by `bps emit rust` orphan\n",
    );
    let msgs = emit_tree(&resolved, &workspace, &keep_alive_table(), &shapes, true).unwrap();
    assert!(msgs.iter().any(|m| m.contains("orphan")), "{msgs:?}");
}

#[test]
fn recipe_book_settings_new_attributes() {
    let packet_root =
        common::rstom_tables::rstom_dir().join("crates/rstom/src/network/packet");
    if !packet_root.is_dir() {
        eprintln!("skipped: rstom worktree packet sources not present");
        return;
    }
    let path = packet_root.join("server/play/recipe_book_settings_packet.rs");
    let shape = extract_shape(
        &std::fs::read_to_string(&path).unwrap(),
        "RecipeBookSettingsPacket",
        "server/play/recipe_book_settings_packet",
        None,
    )
    .unwrap();
    let root = common::repo_root();
    let source = format!(
        "import \"{}\"\nimport \"{}\"\npacket recipe_book_settings {{\n    crafting_recipe_book_open: boolean\n    crafting_recipe_book_filter_active: boolean\n    smelting_recipe_book_open: boolean\n    smelting_recipe_book_filter_active: boolean\n    blast_furnace_recipe_book_open: boolean\n    blast_furnace_recipe_book_filter_active: boolean\n    smoker_recipe_book_open: boolean\n    smoker_recipe_book_filter_active: boolean\n}}\nprotocol minecraft @ v1 {{\n    id = 1\n    frame default {{ length: varint id: varint payload: rest }}\n    route play.clientbound 0x00 => recipe_book_settings\n}}\n",
        root.join("schemas/std/integers.bps").display(),
        root.join("schemas/std/varint.bps").display(),
    );
    let scratch = TempRoot::new("recipebook");
    let schema = scratch.path().join("r.bps");
    write(&schema, &source);
    let resolved = bytemill::analyze(&bytemill::load_schema(&[schema]).unwrap()).unwrap();
    let table = single_class_table("server", "play", "RecipeBookSettingsPacket");
    let unit = build_units(&resolved, &table)
        .unwrap()
        .into_iter()
        .find(|u| u.pkt.name == "recipe_book_settings")
        .unwrap();
    let text = tree_packet(&unit, &shape, &packet_body(&unit, &resolved).unwrap()).unwrap();
    assert!(text.contains("fn_params_excessive_bools"), "{text}");
    assert!(text.contains("too_many_arguments"), "{text}");
    assert!(text.contains("#[must_use]"), "{text}");
}

#[test]
fn plugin_message_string_and_vec_fields() {
    let packet_root =
        common::rstom_tables::rstom_dir().join("crates/rstom/src/network/packet");
    if !packet_root.is_dir() {
        eprintln!("skipped: rstom worktree packet sources not present");
        return;
    }
    let path = packet_root.join("server/common/plugin_message_packet.rs");
    let shape = extract_shape(
        &std::fs::read_to_string(&path).unwrap(),
        "PluginMessagePacket",
        "server/common/plugin_message_packet",
        Some(&path),
    )
    .unwrap();
    assert_eq!(
        shape.extras.as_deref(),
        Some("plugin_message_packet.ext.rs")
    );
    let root = common::repo_root();
    let source = format!(
        "import \"{}\"\nimport \"{}\"\npacket plugin_message {{ channel: string data: rest }}\nprotocol minecraft @ v1 {{\n    id = 1\n    frame default {{ length: varint id: varint payload: rest }}\n    route play.clientbound 0x00 => plugin_message\n}}\n",
        root.join("schemas/std/varint.bps").display(),
        root.join("schemas/minecraft_core.bps").display(),
    );
    let scratch = TempRoot::new("pluginmsg");
    let schema = scratch.path().join("p.bps");
    write(&schema, &source);
    let resolved = bytemill::analyze(&bytemill::load_schema(&[schema]).unwrap()).unwrap();
    let table = single_class_table("server", "common", "PluginMessagePacket");
    let unit = build_units(&resolved, &table)
        .unwrap()
        .into_iter()
        .find(|u| u.pkt.name == "plugin_message")
        .unwrap();
    let text = tree_packet(&unit, &shape, &packet_body(&unit, &resolved).unwrap()).unwrap();
    assert!(text.contains("pub channel: String"), "{text}");
    assert!(text.contains("pub data: Vec<u8>"), "{text}");
    assert!(text.contains("value.channel.clone()"), "{text}");
}

#[test]
fn const_fn_new_from_extracted_shape() {
    let packet_root =
        common::rstom_tables::rstom_dir().join("crates/rstom/src/network/packet");
    if !packet_root.is_dir() {
        eprintln!("skipped: rstom worktree packet sources not present");
        return;
    }
    let path = packet_root.join("server/play/acknowledge_block_change_packet.rs");
    let shape = extract_shape(
        &std::fs::read_to_string(&path).unwrap(),
        "AcknowledgeBlockChangePacket",
        "server/play/acknowledge_block_change_packet",
        None,
    )
    .unwrap();
    assert!(shape.new.const_);
    let root = common::repo_root();
    let source = format!(
        "import \"{}\"\npacket acknowledge_block_change {{ sequence: varint }}\nprotocol minecraft @ v1 {{\n    id = 1\n    frame default {{ length: varint id: varint payload: rest }}\n    route play.clientbound 0x00 => acknowledge_block_change\n}}\n",
        root.join("schemas/std/varint.bps").display(),
    );
    let scratch = TempRoot::new("constnew");
    let schema = scratch.path().join("a.bps");
    write(&schema, &source);
    let resolved = bytemill::analyze(&bytemill::load_schema(&[schema]).unwrap()).unwrap();
    let table = single_class_table("server", "play", "AcknowledgeBlockChangePacket");
    let unit = build_units(&resolved, &table)
        .unwrap()
        .into_iter()
        .find(|u| u.pkt.name == "acknowledge_block_change")
        .unwrap();
    let text = tree_packet(&unit, &shape, &packet_body(&unit, &resolved).unwrap()).unwrap();
    assert!(text.contains("pub const fn new("), "{text}");
    assert!(
        text.contains("pub struct AcknowledgeBlockChangeSerializer"),
        "{text}"
    );
}

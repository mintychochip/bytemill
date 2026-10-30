//! Swapping a hand packet for a generated one: what moves to the siblings, what the swap refuses
//! to lose, how hooks render, and that nothing is written unless every packet passes.

mod common;

use bytemill::emit_rstom::packet_body;
use bytemill::emit_tree::emit_tree;
use bytemill::minestom::{Unit, build_units};
use bytemill::shapes::model::{Accepted, FieldShape, SerializerShape, Shape};
use bytemill::shapes::{extract_shape, packet_report, shape_to_json};
use bytemill::tree_packet::tree_packet;
use bytemill::tree_split::{conservation_gaps, split_hand, uncovered_constructs};
use std::path::{Path, PathBuf};

const MODULE: &str = "server/play/probe_packet";

/// A hand packet with everything a swap has to place: a constant, an extra method, a helper the
/// method uses, a helper only the old serializer used, and inline tests.
const HAND: &str = r#"//! Port of `net.minestom.server.network.packet.server.play.ProbePacket`.

use std::sync::Arc;

use crate::network::buffer::NetworkBuffer;
use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::packet::server::server_packet::{ServerPacket, ServerPacketGroup};
use crate::network::types::STRING;

/// Java: `ProbePacket.MAX`.
pub const MAX: usize = 3;

/// Java: `record ProbePacket(String name)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbePacket {
    /// Java: `name()`.
    pub name: String,
}

impl ProbePacket {
    /// Java: `new ProbePacket(String)`.
    #[must_use]
    pub fn new(name: String) -> Self {
        Self { name }
    }

    /// Java: `shout()`.
    #[must_use]
    pub fn shout(&self) -> String {
        loud(&self.name)
    }

    /// Java: `ProbePacket.SERIALIZER`.
    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(ProbeSerializer)
    }
}

impl Packet for ProbePacket {
    const CLASS_NAME: &'static str = "net.minestom.server.network.packet.server.play.ProbePacket";
}

impl ServerPacket for ProbePacket {}
impl ServerPacketGroup for ProbePacket {}

fn loud(text: &str) -> String {
    text.to_uppercase()
}

/// Writes the name.
fn write_name(buffer: &mut NetworkBuffer, name: &str) {
    buffer.write(STRING, name);
}

/// Java: the template.
struct ProbeSerializer;

impl PacketSerializer for ProbeSerializer {
    fn write(&self, buffer: &mut NetworkBuffer, value: &PacketValue) {
        let packet = downcast::<ProbePacket>(value);
        write_name(buffer, &packet.name);
    }

    fn read(&self, buffer: &mut NetworkBuffer) -> PacketValue {
        Box::new(ProbePacket {
            name: buffer.read(STRING),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shouts() {
        assert_eq!(ProbePacket::new("a".into()).shout(), "A");
    }
}
"#;

/// The same packet with a serializer that checks, clamps and calls a helper.
fn hand_with_limits() -> String {
    HAND.replace(
        "        write_name(buffer, &packet.name);",
        "        assert!(packet.name.len() < 100);\n        write_name(buffer, &packet.name);",
    )
    .replace(
        "        Box::new(ProbePacket {\n            name: buffer.read(STRING),\n        })",
        "        let size = read_size(buffer, 10);\n        let mut name = String::with_capacity(size.min(4));\n        name.push_str(&buffer.read(STRING));\n        Box::new(ProbePacket { name })",
    )
}

/// A second, trivially swappable packet (`start_configuration { a: u8 }` in the probe schema).
const SECOND: &str = r#"//! Port of `StartConfigurationPacket`.

use std::sync::Arc;

use crate::network::buffer::NetworkBuffer;
use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::packet::server::server_packet::{ServerPacket, ServerPacketGroup};
use crate::network::types::BYTE;

/// Java: `StartConfigurationPacket.LIMIT`.
pub const LIMIT: i8 = 9;

/// Java: `record StartConfigurationPacket(byte a)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartConfigurationPacket {
    /// Java: `a()`.
    pub a: i8,
}

impl StartConfigurationPacket {
    /// Java: `new StartConfigurationPacket(byte)`.
    #[must_use]
    pub fn new(a: i8) -> Self {
        Self { a }
    }

    /// Java: `StartConfigurationPacket.SERIALIZER`.
    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(StartConfigurationSerializer)
    }
}

impl Packet for StartConfigurationPacket {
    const CLASS_NAME: &'static str =
        "net.minestom.server.network.packet.server.play.StartConfigurationPacket";
}

impl ServerPacket for StartConfigurationPacket {}
impl ServerPacketGroup for StartConfigurationPacket {}

/// Java: the template.
struct StartConfigurationSerializer;

impl PacketSerializer for StartConfigurationSerializer {
    fn write(&self, buffer: &mut NetworkBuffer, value: &PacketValue) {
        let packet = downcast::<StartConfigurationPacket>(value);
        buffer.write(BYTE, packet.a);
    }

    fn read(&self, buffer: &mut NetworkBuffer) -> PacketValue {
        Box::new(StartConfigurationPacket {
            a: buffer.read(BYTE),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_is_nine() {
        assert_eq!(LIMIT, 9);
    }
}
"#;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bml-split-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn packet_dir(ws: &Path) -> PathBuf {
    ws.join("crates/netherstar/src/network/packet")
}

fn analyze(body: &str, tag: &str) -> bytemill::ResolvedSchema {
    let dir = temp_dir(tag);
    let schema = dir.join("probe.bml");
    write(
        &schema,
        &format!(
            "import \"{}\"\n{}",
            common::repo_root()
                .join("schemas/minecraft_core.bml")
                .display(),
            common::rstom_tables::probe_source(&common::repo_root(), body)
        ),
    );
    bytemill::analyze(&bytemill::load_schema(&[schema]).unwrap()).unwrap()
}

fn probe_unit(resolved: &bytemill::ResolvedSchema) -> Unit {
    build_units(resolved, &common::rstom_tables::probe_table())
        .unwrap()
        .into_iter()
        .find(|u| u.pkt.name == "probe")
        .unwrap()
}

#[test]
fn a_swap_moves_everything_the_generated_file_does_not_render() {
    let resolved = analyze("packet probe { name: string }", "split");
    let unit = probe_unit(&resolved);
    let shape = extract_shape(HAND, "ProbePacket", MODULE, None).unwrap();
    assert_eq!(shape.extras.as_deref(), Some("probe_packet.ext.rs"));
    assert_eq!(shape.tests.as_deref(), Some("probe_packet.tests.rs"));
    let generated = tree_packet(&unit, &shape, &packet_body(&unit, &resolved).unwrap()).unwrap();

    let split = split_hand(&shape, HAND, &generated).unwrap();
    let ext = split.ext.clone().unwrap();
    assert!(ext.contains("pub const MAX: usize = 3;"), "{ext}");
    assert!(ext.contains("pub fn shout(&self)"), "{ext}");
    assert!(ext.contains("fn loud(text: &str)"), "{ext}");
    assert!(
        ext.contains("use super::*;"),
        "the method names the packet: {ext}"
    );
    assert!(
        !ext.contains("fn write_name"),
        "a helper only the old serializer called is dead after the swap: {ext}"
    );
    assert_eq!(split.dropped, vec!["fn write_name".to_string()]);
    assert!(
        ext.contains("Dropped with the swap") && ext.contains("write_name"),
        "{ext}"
    );
    let tests = split.tests.clone().unwrap();
    assert!(tests.starts_with("use super::*;"), "{tests}");
    assert!(tests.contains("fn shouts()"), "{tests}");

    // Nothing is lost: every hand item is regenerated, moved or dropped on purpose.
    let gaps = conservation_gaps(&shape, HAND, &generated, &ext, &split.dropped).unwrap();
    assert!(gaps.is_empty(), "{gaps:?}");

    // Without the ext the lost items are named, so a swap that forgot them is refused.
    let lost = conservation_gaps(&shape, HAND, &generated, "", &split.dropped).unwrap();
    for key in ["const MAX", "fn loud", "impl ProbePacket::fn shout"] {
        assert!(lost.contains(&key.to_string()), "{key} in {lost:?}");
    }
}

#[test]
fn a_trait_impl_with_more_than_the_generated_one_is_a_gap_not_a_silent_drop() {
    let resolved = analyze("packet probe { name: string }", "traitgap");
    let unit = probe_unit(&resolved);
    let shape = extract_shape(HAND, "ProbePacket", MODULE, None).unwrap();
    let generated = tree_packet(&unit, &shape, &packet_body(&unit, &resolved).unwrap()).unwrap();
    let split = split_hand(&shape, HAND, &generated).unwrap();
    let hand = HAND.replace(
        "    const CLASS_NAME: &'static str = \"net.minestom.server.network.packet.server.play.ProbePacket\";\n}",
        "    const CLASS_NAME: &'static str = \"net.minestom.server.network.packet.server.play.ProbePacket\";\n\n    fn extra(&self) {}\n}",
    );
    assert_ne!(hand, HAND, "the replacement must hit");
    let gaps = conservation_gaps(
        &shape,
        &hand,
        &generated,
        split.ext.as_deref().unwrap(),
        &split.dropped,
    )
    .unwrap();
    assert_eq!(
        gaps,
        vec!["impl Packet for ProbePacket::fn extra".to_string()]
    );
}

#[test]
fn the_audit_names_constructs_the_generated_wire_code_lacks() {
    let hand = hand_with_limits();
    let shape = extract_shape(&hand, "ProbePacket", MODULE, None).unwrap();
    let found = uncovered_constructs(&shape, &hand, "").unwrap();
    assert_eq!(
        found,
        vec![
            ".min(".to_string(),
            "assert!".to_string(),
            "read_size".to_string(),
            "write_name".to_string()
        ],
    );

    // A hook that contains the construct reproduces it; `accepted` records the rest.
    let mut covered = shape.clone();
    covered.accepted = vec![
        Accepted {
            construct: ".min(".into(),
            reason: "capacity hint only".into(),
        },
        Accepted {
            construct: "read_size".into(),
            reason: "reproduced by the hook".into(),
        },
        Accepted {
            construct: "write_name".into(),
            reason: "wire-equivalent to the STRING write".into(),
        },
    ];
    covered.hooks.insert(
        "read.after:name_len",
        vec!["assert!(name_len >= 0);".to_string()],
    );
    assert_eq!(
        uncovered_constructs(&covered, &hand, "").unwrap(),
        Vec::<String>::new()
    );

    // A packet with nothing beyond the wire layout is clean.
    let clean = extract_shape(HAND, "ProbePacket", MODULE, None).unwrap();
    assert_eq!(
        uncovered_constructs(&clean, HAND, "").unwrap(),
        vec!["write_name".to_string()],
        "a call to a helper the generated code does not know is a construct too"
    );
}

fn count_shape() -> Shape {
    Shape {
        cls: "ProbePacket".into(),
        module: MODULE.into(),
        derives: vec!["Clone".into(), "Debug".into(), "PartialEq".into()],
        form: "braced".into(),
        fields: vec![FieldShape {
            name: "ids".into(),
            type_: "Vec<i32>".into(),
            ..Default::default()
        }],
        serializer: SerializerShape {
            style: "struct".into(),
            name: "ProbeSerializer".into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn hooks_render_at_the_named_points_and_bad_hooks_are_refused() {
    let resolved = analyze(
        "packet probe { ids_count: varint ids: varint[] count = ids_count }",
        "hooks",
    );
    let unit = probe_unit(&resolved);
    let body = packet_body(&unit, &resolved).unwrap();

    let mut shape = count_shape();
    shape
        .hooks
        .insert("write.start", vec!["// hook-write".into()]);
    shape
        .hooks
        .insert("read.after:ids_count", vec!["// hook-after".into()]);
    shape.hooks.insert("read.end", vec!["// hook-end".into()]);
    let text = tree_packet(&unit, &shape, &body).unwrap();
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle} missing in\n{text}"))
    };
    assert!(at("let value = downcast") < at("// hook-write"));
    assert!(at("// hook-write") < at("buffer.write(VAR_INT"));
    assert!(at("let ids_count = buffer.read(VAR_INT);") < at("// hook-after"));
    assert!(at("// hook-after") < at("let mut ids"));
    assert!(at("ids.push(") < at("// hook-end"));
    assert!(at("// hook-end") < at("Box::new(ProbePacket"));

    let mut unknown = count_shape();
    unknown.hooks.insert("sideways", vec!["// x".into()]);
    let err = tree_packet(&unit, &unknown, &body).unwrap_err().to_string();
    assert!(err.contains("unknown hook"), "{err}");

    let mut missing = count_shape();
    missing
        .hooks
        .insert("read.after:no_such_local", vec!["// x".into()]);
    let err = tree_packet(&unit, &missing, &body).unwrap_err().to_string();
    assert!(err.contains("names no statement"), "{err}");
}

/// A workspace whose packet tree holds `ProbePacket` (hand text `probe`) and
/// `StartConfigurationPacket`, with the ledger shapes of both.
struct Swap {
    ws: PathBuf,
    shapes: PathBuf,
    resolved: bytemill::ResolvedSchema,
}

fn swap_workspace(tag: &str, probe: &str, second: Option<&str>) -> Swap {
    let root = temp_dir(tag);
    let ws = root.join("ws");
    let shapes = root.join("shapes");
    write(&packet_dir(&ws).join("server/play/probe_packet.rs"), probe);
    let mut probe_shape = extract_shape(probe, "ProbePacket", MODULE, None).unwrap();
    probe_shape.accepted = vec![Accepted {
        construct: "write_name".into(),
        reason: "wire-equivalent to the STRING write".into(),
    }];
    write(
        &shapes.join(format!("{MODULE}.json")),
        &shape_to_json(&probe_shape),
    );
    if let Some(text) = second {
        let module = "server/play/start_configuration_packet";
        write(&packet_dir(&ws).join(format!("{module}.rs")), text);
        let shape = extract_shape(text, "StartConfigurationPacket", module, None).unwrap();
        write(
            &shapes.join(format!("{module}.json")),
            &shape_to_json(&shape),
        );
    }
    Swap {
        ws,
        shapes,
        resolved: analyze("packet probe { name: string }", &format!("{tag}-schema")),
    }
}

fn emit(swap: &Swap, check: bool) -> bytemill::errors::BmlResult<Vec<String>> {
    emit_tree(
        &swap.resolved,
        &swap.ws,
        &common::rstom_tables::probe_table(),
        &swap.shapes,
        check,
    )
}

#[test]
fn a_refused_swap_writes_no_siblings_for_any_packet() {
    // The second packet is fine; the probe's serializer carries an unrecorded assert.
    let swap = swap_workspace("atomic", &hand_with_limits(), Some(SECOND));
    let err = emit(&swap, false).unwrap_err().to_string();
    assert!(
        err.contains("probe_packet") && err.contains("assert!"),
        "{err}"
    );

    let dir = packet_dir(&swap.ws).join("server/play");
    for stem in ["start_configuration_packet", "probe_packet"] {
        for ext in ["ext.rs", "tests.rs"] {
            assert!(
                !dir.join(format!("{stem}.{ext}")).exists(),
                "{stem}.{ext} was written although the run was refused"
            );
        }
    }
    let second = std::fs::read_to_string(dir.join("start_configuration_packet.rs")).unwrap();
    assert_eq!(second, SECOND, "the hand file is untouched");
}

#[test]
fn a_swap_creates_siblings_once_and_never_overwrites_them() {
    let swap = swap_workspace("siblings", HAND, None);
    emit(&swap, false).unwrap();
    let dir = packet_dir(&swap.ws).join("server/play");
    let packet = std::fs::read_to_string(dir.join("probe_packet.rs")).unwrap();
    assert!(
        packet.starts_with("//! Generated by `bml emit rust`"),
        "{packet}"
    );
    assert!(
        packet.contains("mod ext;") && packet.contains("mod tests;"),
        "{packet}"
    );
    let ext = std::fs::read_to_string(dir.join("probe_packet.ext.rs")).unwrap();
    assert!(ext.contains("pub const MAX: usize = 3;"), "{ext}");
    let tests = std::fs::read_to_string(dir.join("probe_packet.tests.rs")).unwrap();
    assert!(tests.contains("fn shouts()"), "{tests}");

    // The siblings are hand-owned from here on: an edit survives a re-emit and is no drift.
    write(
        &dir.join("probe_packet.ext.rs"),
        &format!("{ext}\n// edited by hand\n"),
    );
    emit(&swap, false).unwrap();
    let after = std::fs::read_to_string(dir.join("probe_packet.ext.rs")).unwrap();
    assert!(after.contains("// edited by hand"), "{after}");
    assert!(emit(&swap, true).unwrap().is_empty());
}

#[test]
fn a_known_wire_deviation_is_neither_eligible_nor_swappable() {
    let swap = swap_workspace("known", HAND, None);
    write(
        &swap
            .ws
            .join("crates/netherstar/tests/bml/wire_pins_known.json"),
        "[{\"class\":\"ProbePacket\",\"side\":\"SERVER\",\"state\":\"PLAY\",\"id\":1,\
         \"category\":\"minestom-deviation\",\"reason\":\"keys are ids\"}]\n",
    );
    let err = emit(&swap, false).unwrap_err().to_string();
    assert!(err.contains("known wire deviation"), "{err}");
    assert!(
        emit(&swap, true)
            .unwrap_err()
            .to_string()
            .contains("known wire deviation")
    );

    let unit = probe_unit(&swap.resolved);
    let report = packet_report(&unit, &swap.resolved, &swap.ws).unwrap();
    assert!(
        report.starts_with("known wire deviation (minestom-deviation)"),
        "{report}"
    );
}

/// `hand` split for the one-field probe packet: the ext sibling text and the swap's other texts.
fn split_probe(hand: &str, tag: &str) -> (Shape, bytemill::tree_split::Split) {
    let resolved = analyze("packet probe { name: string }", tag);
    let unit = probe_unit(&resolved);
    let shape = extract_shape(hand, "ProbePacket", MODULE, None).unwrap();
    let generated = tree_packet(&unit, &shape, &packet_body(&unit, &resolved).unwrap()).unwrap();
    let split = split_hand(&shape, hand, &generated).unwrap();
    (shape, split)
}

#[test]
fn the_coordinate_vec_is_renamed_in_code_and_never_in_comments() {
    let hand = HAND
        .replace(
            "use std::sync::Arc;\n",
            "use std::sync::Arc;\n\nuse crate::coordinate::Vec;\n",
        )
        .replace(
            "        loud(&self.name)",
            "        let _ = origin();\n        loud(&self.name)",
        )
        .replace(
            "fn loud(text: &str)",
            "/// The `Vec` of the origin.\nfn origin() -> Vec {\n    Vec::new(0.0, 0.0, 0.0)\n}\n\nfn loud(text: &str)",
        );
    let (_, split) = split_probe(&hand, "coordvec");
    let ext = split.ext.unwrap();
    assert!(ext.contains("/// The `Vec` of the origin."), "{ext}");
    assert!(ext.contains("fn origin() -> CoordVec {"), "{ext}");
    assert!(ext.contains("CoordVec::new(0.0, 0.0, 0.0)"), "{ext}");
    assert!(
        ext.contains("use crate::coordinate::Vec as CoordVec;"),
        "{ext}"
    );
}

#[test]
fn a_trait_import_stays_with_the_helper_that_calls_its_methods() {
    let with_call = HAND
        .replace(
            "use std::sync::Arc;\n",
            "use std::sync::Arc;\n\nuse crate::registry::store::{Registry, block_entries};\n",
        )
        .replace(
            "fn loud(text: &str) -> String {\n    text.to_uppercase()\n}",
            "fn loud(text: &str) -> String {\n    let _ = block_entries().size();\n    text.to_uppercase()\n}",
        );
    assert_ne!(with_call, HAND, "the replacement must hit");
    let (_, split) = split_probe(&with_call, "traitimport");
    let ext = split.ext.unwrap();
    // `size` is a method of the `Registry` trait: the import is what makes the call compile
    assert!(ext.contains("Registry"), "{ext}");
    assert!(ext.contains("block_entries"), "{ext}");

    // without a call of one of its methods the trait import is not carried over
    let without = with_call.replace("    let _ = block_entries().size();\n", "");
    let (_, split) = split_probe(&without, "traitnoimport");
    let ext = split.ext.unwrap();
    assert!(!ext.contains("Registry"), "{ext}");
}

#[test]
fn a_struct_with_hand_written_impls_and_no_derive_swaps_with_its_impls_kept() {
    let hand = HAND
        .replace("#[derive(Clone, Debug, PartialEq, Eq)]\n", "")
        .replace(
            "impl ServerPacketGroup for ProbePacket {}\n",
            "impl ServerPacketGroup for ProbePacket {}\n\nimpl PartialEq for ProbePacket {\n    fn eq(&self, other: &Self) -> bool {\n        self.name == other.name\n    }\n}\n\nimpl Clone for ProbePacket {\n    fn clone(&self) -> Self {\n        Self { name: self.name.clone() }\n    }\n}\n",
        );
    assert_ne!(hand, HAND, "the replacement must hit");
    let (shape, split) = split_probe(&hand, "noderive");
    assert!(shape.derives.is_empty(), "{:?}", shape.derives);
    let ext = split.ext.unwrap();
    assert!(ext.contains("impl PartialEq for ProbePacket"), "{ext}");
    assert!(ext.contains("impl Clone for ProbePacket"), "{ext}");
}

#[test]
fn a_domain_limit_known_entry_records_a_limit_and_does_not_block_the_swap() {
    let swap = swap_workspace("limit", HAND, None);
    let known = swap
        .ws
        .join("crates/netherstar/tests/bml/wire_pins_known.json");
    let entry = "[{\"class\":\"ProbePacket\",\"side\":\"SERVER\",\"state\":\"PLAY\",\"id\":1,\
         \"category\":\"domain-limit\",\"reason\":\"the hand type cannot hold id zero\"}]\n";
    write(&known, entry);
    emit(&swap, false).unwrap();
    assert!(emit(&swap, true).unwrap().is_empty());
    // the entry stays as the record of the limit
    assert_eq!(std::fs::read_to_string(&known).unwrap(), entry);

    // a swapped packet is no longer on the worklist
    let unit = probe_unit(&swap.resolved);
    assert_eq!(
        packet_report(&unit, &swap.resolved, &swap.ws).unwrap(),
        "generated"
    );
}

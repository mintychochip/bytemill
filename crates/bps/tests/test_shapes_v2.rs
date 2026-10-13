//! Ports tests/test_shapes_v2.py.

mod common;

use bps::shapes::{
    extract_shape, shape_from_json, shape_to_json, Accepted, FieldShape, NewShape, SerializerShape,
    Shape,
};
use std::path::{Path, PathBuf};

const PACKET_PATH: &str = "server/play/probe_packet";

const STRUCT: &str = r#"#[derive(Clone, Debug)]
pub struct ProbePacket {
    pub name: String,
}
"#;

const SERIALIZER: &str = r#"struct ProbePacketSerializer;

impl PacketSerializer for ProbePacketSerializer {
    fn write(&self, buffer: &mut NetworkBuffer, value: &PacketValue) {
        let packet = downcast::<ProbePacket>(value);
        buffer.write(STRING, &packet.name);
    }

    fn read(&self, buffer: &mut NetworkBuffer) -> PacketValue {
        Box::new(ProbePacket {
            name: buffer.read(STRING),
        })
    }
}
"#;

const HEADER: &str = r#"//! Port.

use std::sync::Arc;

use crate::network::buffer::NetworkBuffer;
use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::types::STRING;
"#;

const MODE_ITEMS: &str = r#"/// A mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    On,
    Off,
}

impl Mode {
    pub fn id(&self) -> u8 {
        match self {
            Mode::On => 1,
            Mode::Off => 0,
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{{}}")
    }
}

impl PartialEq for ProbePacket {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

pub const MAX: usize = 4;

pub fn helper() -> usize {
    MAX
}
"#;

const TESTS_MODULE: &str = r#"#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_is_max() {
        assert_eq!(helper(), MAX);
    }
}
"#;

const GENERATED_TAIL: &str = "\nmod ext;\npub use ext::*;\n\n#[cfg(test)]\nmod tests;\n";

#[allow(clippy::too_many_arguments)]
fn packet(
    struct_decl: &str,
    head: &str,
    new_sig: &str,
    new_body: &str,
    items: &str,
    tail: &str,
) -> String {
    [
        HEADER,
        head,
        struct_decl,
        "impl ProbePacket {",
        "    #[must_use]",
        &format!("    pub fn new({new_sig}) -> Self {{"),
        &format!("        {new_body}"),
        "    }",
        "",
        "    #[must_use]",
        "    pub fn serializer() -> Arc<dyn PacketSerializer> {",
        "        Arc::new(ProbePacketSerializer)",
        "    }",
        "}",
        "",
        "impl Packet for ProbePacket {}",
        "",
        SERIALIZER,
        items,
        tail,
    ]
    .join("\n")
}

fn extract(source: &str, packet_path: Option<&Path>) -> bps::BpsResult<Shape> {
    extract_shape(source, "ProbePacket", PACKET_PATH, packet_path)
}

fn base_shape() -> Shape {
    Shape {
        cls: "ProbePacket".into(),
        module: PACKET_PATH.into(),
        module_docs: Vec::new(),
        struct_docs: Vec::new(),
        derives: vec!["Clone".into()],
        form: "braced".into(),
        fields: vec![FieldShape { name: "id".into(), type_: "i32".into(), docs: Vec::new(), wire: None }],
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
        extras: None,
        ..Shape::default()
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bps-v2-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn every_committed_ledger_file_reserialises_byte_for_byte() {
    let ledger =
        bps::shapes::load_ledger(&common::repo_root().join("targets/rstom/shapes")).unwrap();
    assert_eq!(ledger.len(), 64);
    for (module, shape) in &ledger {
        let path = common::repo_root().join("targets/rstom/shapes").join(format!("{module}.json"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(shape_to_json(shape), text, "{module}");
    }
}

#[test]
fn v2_attributes_round_trip_through_json() {
    let mut shape = base_shape();
    shape.fields[0].wire = Some("ident".into());
    shape.new.params = Some("id: i32, label: impl Into<String>".into());
    shape.imports = vec!["use uuid::Uuid;".into()];
    shape.tests = Some("probe_packet.tests.rs".into());
    let mut hooks = bps::ordmap::OrderedMap::new();
    hooks.insert("write.start", vec!["let x = 1;".into()]);
    hooks.insert("read.after:ident", vec!["let y = 2;".into()]);
    shape.hooks = hooks;
    shape.accepted = vec![Accepted { construct: "pub enum Mode".into(), reason: "kept in extras".into() }];
    shape.drops = vec!["use crate::old::Thing;".into()];
    assert_eq!(shape_from_json(&shape_to_json(&shape)).unwrap(), shape);
}

#[test]
fn non_packet_items_become_extras_and_inline_tests_are_recorded() {
    let source = packet(STRUCT, "", "name: String", "Self { name }", MODE_ITEMS, &format!("\n{TESTS_MODULE}"));
    let shape = extract(&source, None).unwrap();
    assert_eq!(shape.extras.as_deref(), Some("probe_packet.ext.rs"));
    assert_eq!(shape.tests.as_deref(), Some("probe_packet.tests.rs"));
    let names: Vec<&str> = shape.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["name"]);
    assert_eq!(shape.serializer.name, "ProbePacketSerializer");
}

#[test]
fn inline_tests_alone_do_not_make_extras() {
    let shape = extract(&packet(STRUCT, "", "name: String", "Self { name }", "", &format!("\n{TESTS_MODULE}")), None)
        .unwrap();
    assert_eq!(shape.extras, None);
    assert_eq!(shape.tests.as_deref(), Some("probe_packet.tests.rs"));
}

#[test]
fn generated_tail_is_not_an_extra_until_a_sibling_ext_file_exists() {
    let source = packet(STRUCT, "", "name: String", "Self { name }", "", GENERATED_TAIL);
    let tmp = temp_dir("gentail");
    let path = tmp.join("probe_packet.rs");
    std::fs::write(&path, &source).unwrap();
    let shape = extract(&source, Some(&path)).unwrap();
    assert_eq!(shape.extras, None);
    assert_eq!(shape.tests, None);
    std::fs::write(tmp.join("probe_packet.ext.rs"), "").unwrap();
    let shape = extract(&source, Some(&path)).unwrap();
    assert_eq!(shape.extras.as_deref(), Some("probe_packet.ext.rs"));
}

#[test]
fn generated_tail_is_not_a_hand_import() {
    let shape = extract(&packet(STRUCT, "", "name: String", "Self { name }", "", GENERATED_TAIL), None).unwrap();
    assert!(shape.imports.is_empty());
}

#[test]
fn params_are_recorded_verbatim_when_they_differ_from_fields() {
    let struct_decl = "#[derive(Clone, Debug)]\npub struct ProbePacket {\n    pub position: i8,\n    pub score_name: String,\n}\n";
    let source = packet(
        struct_decl,
        "",
        "position: i8, score_name: impl Into<String>",
        "let score_name = score_name.into();\n        Self { position, score_name }",
        "",
        "",
    );
    assert_eq!(
        extract(&source, None).unwrap().new.params.as_deref(),
        Some("position: i8, score_name: impl Into<String>")
    );
}

#[test]
fn params_are_none_when_they_match_the_fields() {
    let shape = extract(&packet(STRUCT, "", "name: String", "Self { name }", "", ""), None).unwrap();
    assert_eq!(shape.new.params, None);
}

#[test]
fn bare_vec_from_coordinate_becomes_coord_vec_and_std_vec_loses_its_path() {
    let struct_decl = "#[derive(Clone, Debug)]\npub struct ProbePacket {\n    pub location: Vec,\n    pub history: std::vec::Vec<String>,\n}\n";
    let source = packet(
        struct_decl,
        "use crate::coordinate::Vec;\n",
        "location: Vec, history: std::vec::Vec<String>",
        "Self { location, history }",
        "",
        "",
    );
    let shape = extract(&source, None).unwrap();
    let pairs: Vec<(&str, &str)> =
        shape.fields.iter().map(|f| (f.name.as_str(), f.type_.as_str())).collect();
    assert_eq!(pairs, vec![("location", "CoordVec"), ("history", "Vec<String>")]);
    assert!(shape.imports.is_empty());
}

#[test]
fn every_coordinate_vec_spelling_becomes_coord_vec() {
    let cases = [
        ("use crate::coordinate::{Point, Vec};", "Vec"),
        ("use crate::coordinate::{Pos, Vec};", "Vec"),
        ("use crate::coordinate::vec::Vec;", "Vec"),
        ("use crate::coordinate::{Pos, Vec as Location};", "Location"),
    ];
    for (head, spelling) in cases {
        let struct_decl = format!(
            "#[derive(Clone, Debug)]\npub struct ProbePacket {{\n    pub location: {spelling},\n}}\n"
        );
        let source = packet(
            &struct_decl,
            &format!("{head}\n"),
            &format!("location: {spelling}"),
            "Self { location }",
            "",
            "",
        );
        let shape = extract(&source, None).unwrap();
        let types: Vec<&str> = shape.fields.iter().map(|f| f.type_.as_str()).collect();
        assert_eq!(types, vec!["CoordVec"], "{head}");
        assert!(shape.imports.is_empty());
    }
}

#[test]
fn coordinate_vec_is_normalised_inside_generic_types() {
    let struct_decl = "#[derive(Clone, Debug)]\npub struct ProbePacket {\n    pub around: Option<Vec>,\n    pub pairs: Vec<(Vec, i32)>,\n}\n";
    let source = packet(
        struct_decl,
        "use crate::coordinate::{Point, Vec};\n",
        "around: Option<Vec>, pairs: Vec<(Vec, i32)>",
        "Self { around, pairs }",
        "",
        "",
    );
    let shape = extract(&source, None).unwrap();
    let types: Vec<&str> = shape.fields.iter().map(|f| f.type_.as_str()).collect();
    assert_eq!(types, vec!["Option<CoordVec>", "Vec<(CoordVec, i32)>"]);
}

#[test]
fn params_use_the_same_normalised_types() {
    let struct_decl = "#[derive(Clone, Debug)]\npub struct ProbePacket {\n    pub location: Vec,\n    pub history: Vec<String>,\n    pub label: String,\n}\n";
    let source = packet(
        struct_decl,
        "use crate::coordinate::Vec;\n",
        "location: Vec, history: std::vec::Vec<String>, label: impl Into<String>",
        "Self { location, history, label: label.into() }",
        "",
        "",
    );
    let shape = extract(&source, None).unwrap();
    assert_eq!(
        shape.new.params.as_deref(),
        Some("location: CoordVec, history: Vec<String>, label: impl Into<String>")
    );
}

#[test]
fn bare_vec_without_coordinate_import_is_unchanged() {
    let struct_decl = "#[derive(Clone, Debug)]\npub struct ProbePacket {\n    pub name: Vec<u8>,\n}\n";
    let shape = extract(&packet(struct_decl, "", "name: Vec<u8>", "Self { name }", "", ""), None).unwrap();
    assert_eq!(shape.fields[0].type_, "Vec<u8>");
}

#[test]
fn imports_keep_only_names_used_by_the_hand_packet() {
    let head = "use crate::network::types::BOOLEAN;\nuse crate::coordinate::{Point, Pos};\nuse crate::text::Component;\nuse uuid::Uuid;\n";
    let struct_decl = "#[derive(Clone, Debug)]\npub struct ProbePacket {\n    pub id: Uuid,\n    pub text: Component,\n}\n";
    let source = packet(struct_decl, head, "id: Uuid, text: Component", "Self { id, text }", "", "");
    let shape = extract(&source, None).unwrap();
    assert_eq!(shape.imports, vec!["use crate::text::Component;"]);
}

#[test]
fn grouped_use_keeps_only_the_names_the_hand_packet_mentions() {
    let head = "use crate::network::types::{BOOLEAN, BitSet, message_format_int};\n";
    let struct_decl = "#[derive(Clone, Debug)]\npub struct ProbePacket {\n    pub bits: BitSet,\n}\n";
    let source = packet(struct_decl, head, "bits: BitSet", "Self { bits }", "", "");
    let shape = extract(&source, None).unwrap();
    assert_eq!(shape.imports, vec!["use crate::network::types::BitSet;"]);
}

#[test]
fn aliases_keep_their_alias_and_generator_owned_names_are_dropped() {
    let head = "use crate::text::{Component as Text, Style};\nuse crate::network::uuid::Uuid;\n";
    let struct_decl = "#[derive(Clone, Debug)]\npub struct ProbePacket {\n    pub text: Text,\n    pub id: Uuid,\n}\n";
    let source = packet(struct_decl, head, "text: Text, id: Uuid", "Self { text, id }", "", "");
    let shape = extract(&source, None).unwrap();
    assert_eq!(shape.imports, vec!["use crate::text::Component as Text;"]);
}

#[test]
fn names_used_only_by_the_sibling_tests_file_are_kept() {
    let source = packet(STRUCT, "use crate::text::Component;\n", "name: String", "Self { name }", "", "");
    let tmp = temp_dir("testsib");
    std::fs::write(tmp.join("probe_packet.tests.rs"), "fn check(_: Component) {}\n").unwrap();
    let shape = extract(&source, Some(&tmp.join("probe_packet.rs"))).unwrap();
    assert_eq!(shape.imports, vec!["use crate::text::Component;"]);
}

#[test]
fn pub_reexports_are_kept_verbatim_even_when_unused() {
    let source = packet(STRUCT, "pub use super::data::WorldPos;\n", "name: String", "Self { name }", "", "");
    let shape = extract(&source, None).unwrap();
    assert_eq!(shape.imports, vec!["pub use super::data::WorldPos;"]);
}

#[test]
fn the_serializer_is_the_struct_named_in_the_serializer_fn() {
    let source = packet(STRUCT, "pub struct HelperSerializer;\n", "name: String", "Self { name }", "", "");
    let shape = extract(&source, None).unwrap();
    assert_eq!(shape.serializer.name, "ProbePacketSerializer");
}

#[test]
fn a_serializer_fn_naming_no_declared_struct_is_refused() {
    let source = packet(STRUCT, "", "name: String", "Self { name }", "", "")
        .replace("Arc::new(ProbePacketSerializer)", "Arc::new(MissingSerializer)");
    assert!(extract(&source, None).is_err());
}

#[test]
fn generics_lifetimes_and_tuple_structs_are_refused() {
    let cases = [
        (
            "generic",
            "#[derive(Clone, Debug)]\npub struct ProbePacket<T> {\n    pub name: T,\n}\n",
        ),
        (
            "lifetime",
            "#[derive(Clone, Debug)]\npub struct ProbePacket<'a> {\n    pub name: &'a str,\n}\n",
        ),
        ("tuple", "#[derive(Clone, Debug)]\npub struct ProbePacket(pub String);\n"),
    ];
    for (label, struct_decl) in cases {
        let source = packet(struct_decl, "", "name: String", "Self { name }", "", "");
        assert!(extract(&source, None).is_err(), "{label}");
    }
}

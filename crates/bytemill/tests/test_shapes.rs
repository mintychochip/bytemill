//! Ports tests/test_shapes.py: shape extraction from hand-written packet files.

mod common;

use bytemill::shapes::{
    advance_item_start, docs_and_attrs_before, extract_shape, hand_packet_path, mismatch,
    packet_report, shape_from_json, shape_to_json,
};
use std::collections::BTreeMap;

const KEEP_ALIVE: &str = r#"//! Port of `net.minestom.server.network.packet.server.common.KeepAlivePacket`.

use std::sync::Arc;

use crate::network::buffer::NetworkBuffer;
use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::packet::server::server_packet::{ServerPacket, ServerPacketGroup};
use crate::network::types::LONG;

/// Java: `record KeepAlivePacket(long id) implements ServerPacket.Configuration, Play`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KeepAlivePacket {
    /// Java: `id()`.
    pub id: i64,
}

impl KeepAlivePacket {
    /// Java: `KeepAlivePacket(long id)`.
    #[must_use]
    pub fn new(id: i64) -> Self {
        Self { id }
    }

    /// Java: `KeepAlivePacket.SERIALIZER`.
    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(KeepAlivePacketSerializer)
    }
}

impl Packet for KeepAlivePacket {
    const CLASS_NAME: &'static str =
        "net.minestom.server.network.packet.server.common.KeepAlivePacket";
}

impl ServerPacket for KeepAlivePacket {}
impl ServerPacketGroup for KeepAlivePacket {}

/// Java: the `template(LONG, KeepAlivePacket::id, …)` template.
struct KeepAlivePacketSerializer;

impl PacketSerializer for KeepAlivePacketSerializer {
    fn write(&self, buffer: &mut NetworkBuffer, value: &PacketValue) {
        buffer.write(LONG, downcast::<KeepAlivePacket>(value).id);
    }

    fn read(&self, buffer: &mut NetworkBuffer) -> PacketValue {
        Box::new(KeepAlivePacket {
            id: buffer.read(LONG),
        })
    }
}
"#;

const CLEAR_DIALOG: &str = r#"//! Port of `net.minestom.server.network.packet.server.common.ClearDialogPacket`.

use std::sync::Arc;

use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{ConstantSerializer, PacketSerializer};
use crate::network::packet::server::server_packet::{ServerPacket, ServerPacketGroup};

/// Java: `record ClearDialogPacket() implements ServerPacket.Configuration, Play`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClearDialogPacket;

impl Packet for ClearDialogPacket {
    const CLASS_NAME: &'static str =
        "net.minestom.server.network.packet.server.common.ClearDialogPacket";
}

impl ServerPacket for ClearDialogPacket {}
impl ServerPacketGroup for ClearDialogPacket {}

impl ClearDialogPacket {
    /// Java: `ClearDialogPacket.SERIALIZER`.
    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(ConstantSerializer::new(ClearDialogPacket))
    }
}
"#;

const MODULE_FN_BEFORE_SERIALIZER: &str = r#"//! Port of `net.minestom.server.network.packet.server.play.ProbePacket`.

use std::sync::Arc;

use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::packet::server::server_packet::{ServerPacket, ServerPacketGroup};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ProbePacket {
    pub id: i32,
}

impl ProbePacket {
    #[must_use]
    pub fn new(id: i32) -> Self {
        Self { id }
    }

    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(ProbePacketSerializer)
    }
}

impl Packet for ProbePacket {}
impl ServerPacket for ProbePacket {}
impl ServerPacketGroup for ProbePacket {}

fn helper(x: i32) -> i8 {
    x as i8
}

const fn k() -> i32 {
    1
}

struct ProbePacketSerializer;

impl PacketSerializer for ProbePacketSerializer {
    fn write(&self, _b: &mut (), _v: &PacketValue) {}
    fn read(&self, _b: &mut ()) -> PacketValue {
        Box::new(ProbePacket { id: 0 })
    }
}
"#;

const FORWARD_EXTRA: &str = r#"//! Port of `net.minestom.server.network.packet.server.common.ProbePacket`.

use std::sync::Arc;

use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::packet::server::server_packet::{ServerPacket, ServerPacketGroup};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ProbePacket {
    pub id: i64,
}

impl ProbePacket {
    #[must_use]
    pub fn new(id: i64) -> Self {
        Self { id }
    }

    #[must_use]
    pub fn forward(&self) -> bool {
        true
    }

    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(ProbePacketSerializer)
    }
}

impl Packet for ProbePacket {}
impl ServerPacket for ProbePacket {}
impl ServerPacketGroup for ProbePacket {}

struct ProbePacketSerializer;

impl PacketSerializer for ProbePacketSerializer {
    fn write(&self, _buffer: &mut (), _value: &PacketValue) {}
    fn read(&self, _buffer: &mut ()) -> PacketValue {
        Box::new(ProbePacket { id: 0 })
    }
}
"#;

const NAME_ITEM_VALIDATION: &str = r#"//! Port of `net.minestom.server.network.packet.client.play.ClientNameItemPacket`.

use std::sync::Arc;

use crate::network::buffer::NetworkBuffer;
use crate::network::packet::client::client_packet::{ClientPacket, ClientPacketGroup};
use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::types::STRING;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientNameItemPacket {
    pub item_name: String,
}

impl ClientNameItemPacket {
    #[must_use]
    pub fn new(item_name: String) -> Self {
        if item_name.chars().count() > i16::MAX as usize {
            panic!("Item name cannot be longer than Short.MAX_SIZE");
        }
        Self { item_name }
    }

    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(ClientNameItemSerializer)
    }
}

impl Packet for ClientNameItemPacket {}
impl ClientPacket for ClientNameItemPacket {}
impl ClientPacketGroup for ClientNameItemPacket {}

struct ClientNameItemSerializer;

impl PacketSerializer for ClientNameItemSerializer {
    fn write(&self, buffer: &mut NetworkBuffer, value: &PacketValue) {
        let packet = downcast::<ClientNameItemPacket>(value);
        buffer.write(STRING, packet.item_name.clone());
    }

    fn read(&self, buffer: &mut NetworkBuffer) -> PacketValue {
        let item_name = buffer.read(STRING);
        Box::new(ClientNameItemPacket { item_name })
    }
}
"#;

const PLUGIN_MESSAGE_ASSERT_NEW: &str = r#"//! Port of `net.minestom.server.network.packet.client.common.ClientPluginMessagePacket`.

use std::sync::Arc;

use crate::network::buffer::NetworkBuffer;
use crate::network::packet::client::client_packet::{ClientPacket, ClientPacketGroup};
use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::types::{RAW_BYTES, STRING};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientPluginMessagePacket {
    pub channel: String,
    pub data: Vec<u8>,
}

impl ClientPluginMessagePacket {
    #[must_use]
    pub fn new(channel: String, data: Vec<u8>) -> Self {
        assert!(
            channel.chars().count() <= 256,
            "Channel cannot be more than 256 characters long"
        );
        assert!(
            data.len() <= i16::MAX as usize,
            "Data cannot be longer than Short.MAX_VALUE bytes"
        );
        Self { channel, data }
    }

    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(ClientPluginMessageSerializer)
    }
}

impl Packet for ClientPluginMessagePacket {}
impl ClientPacket for ClientPluginMessagePacket {}
impl ClientPacketGroup for ClientPluginMessagePacket {}

struct ClientPluginMessageSerializer;

impl PacketSerializer for ClientPluginMessageSerializer {
    fn write(&self, buffer: &mut NetworkBuffer, value: &PacketValue) {
        let packet = downcast::<ClientPluginMessagePacket>(value);
        buffer.write(STRING, &packet.channel);
        buffer.write(RAW_BYTES, packet.data.as_slice());
    }

    fn read(&self, buffer: &mut NetworkBuffer) -> PacketValue {
        Box::new(ClientPluginMessagePacket {
            channel: buffer.read(STRING),
            data: buffer.read(RAW_BYTES),
        })
    }
}
"#;

const VIEW_DISTANCE_CLAMP_NEW: &str = r#"//! Port.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct UpdateViewDistancePacket {
    pub view_distance: i32,
}

impl UpdateViewDistancePacket {
    pub fn new(view_distance: i32) -> Self {
        Self {
            view_distance: view_distance.clamp(2, 32),
        }
    }
}

impl Packet for UpdateViewDistancePacket {}
"#;

const FOR_SCAN_NEW: &str = r#"//! Port.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbePacket {
    pub total: usize,
    pub pages: Vec<String>,
}

impl ProbePacket {
    pub fn new(pages: Vec<String>) -> Self {
        let mut total = 0;
        for page in &pages {
            total += page.len();
        }
        Self { total, pages }
    }
}

impl Packet for ProbePacket {}
"#;

const NAME_ITEM_PANICS_DOC: &str = r#"//! Port.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientNameItemPacket {
    pub item_name: String,
}

impl ClientNameItemPacket {
    /// Java: `ClientNameItemPacket(String itemName)`.
    ///
    /// # Panics
    ///
    /// Panics with Java's `IllegalArgumentException` message when the name is
    /// longer than [`i16::MAX`].
    pub fn new(item_name: String) -> Self {
        if item_name.chars().count() > i16::MAX as usize {
            panic!("Item name cannot be longer than Short.MAX_SIZE");
        }
        Self { item_name }
    }
}

impl Packet for ClientNameItemPacket {}
"#;

const WRAPPED_NEW_DOC: &str = r#"//! Port.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ClientPunchPacket {
    pub entity_id: i32,
}

impl ClientPunchPacket {
    /// Java: `record ClientPunchPacket(int entityId) implements
    /// ClientPacket.Play`.
    /// Java: `entityId()`.
    pub fn new(entity_id: i32) -> Self {
        Self { entity_id }
    }
}

impl Packet for ClientPunchPacket {}
"#;

const READ_VIA_NEW_SNIPPET: &str = r#"//! Port.

use crate::network::buffer::NetworkBuffer;
use crate::network::packet::serializer::{PacketSerializer, PacketValue};
use crate::network::types::STRING;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientNameItemPacket {
    pub item_name: String,
}

impl ClientNameItemPacket {
    pub fn new(item_name: String) -> Self {
        Self { item_name }
    }
}

struct ClientNameItemSerializer;

impl PacketSerializer for ClientNameItemSerializer {
    fn write(&self, _buffer: &mut NetworkBuffer, _value: &PacketValue) {}

    fn read(&self, buffer: &mut NetworkBuffer) -> PacketValue {
        Box::new(ClientNameItemPacket::new(buffer.read(STRING)))
    }
}
"#;

const RECIPE_BOOK_SNIPPET: &str = r#"//! Port of `net.minestom.server.network.packet.server.play.RecipeBookSettingsPacket`.

use std::sync::Arc;

use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::packet::server::server_packet::{ServerPacket, ServerPacketGroup};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RecipeBookSettingsPacket {
    pub a: bool,
    pub b: bool,
}

impl RecipeBookSettingsPacket {
    /// Java: `new RecipeBookSettingsPacket(boolean × 8)`.
    #[must_use]
    #[allow(clippy::fn_params_excessive_bools)]
    #[allow(
        clippy::too_many_arguments,
        reason = "1:1 port of Java's RecipeBookSettingsPacket(boolean x 8)"
    )]
    pub fn new(a: bool, b: bool) -> Self {
        Self { a, b }
    }

    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(RecipeBookSettingsSerializer)
    }
}

impl Packet for RecipeBookSettingsPacket {}
impl ServerPacket for RecipeBookSettingsPacket {}
impl ServerPacketGroup for RecipeBookSettingsPacket {}

struct RecipeBookSettingsSerializer;

impl PacketSerializer for RecipeBookSettingsSerializer {
    fn write(&self, _b: &mut (), _v: &PacketValue) {}
    fn read(&self, _b: &mut ()) -> PacketValue {
        Box::new(RecipeBookSettingsPacket { a: false, b: false })
    }
}
"#;

const TAB_COMPLETE_CONST_EXTRA: &str = r#"//! Port of `net.minestom.server.network.packet.client.play.ClientTabCompletePacket`.

use std::sync::Arc;

use crate::network::packet::client::client_packet::{ClientPacket, ClientPacketGroup};
use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};

pub const MAX_TEXT_LENGTH: i32 = 32500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientTabCompletePacket {
    pub transaction_id: i32,
    pub text: String,
}

impl ClientTabCompletePacket {
    #[must_use]
    pub fn new(transaction_id: i32, text: String) -> Self {
        Self { transaction_id, text }
    }

    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(ClientTabCompletePacketSerializer)
    }
}

impl Packet for ClientTabCompletePacket {}
impl ClientPacket for ClientTabCompletePacket {}
impl ClientPacketGroup for ClientTabCompletePacket {}

struct ClientTabCompletePacketSerializer;

impl PacketSerializer for ClientTabCompletePacketSerializer {
    fn write(&self, _b: &mut (), _v: &PacketValue) {}
    fn read(&self, _b: &mut ()) -> PacketValue {
        Box::new(ClientTabCompletePacket { transaction_id: 0, text: String::new() })
    }
}
"#;

#[test]
fn keep_alive_style_packet() {
    let shape =
        extract_shape(KEEP_ALIVE, "KeepAlivePacket", "server/common/keep_alive_packet", None)
            .unwrap();
    assert_eq!(shape.derives, vec!["Copy", "Clone", "Debug", "PartialEq", "Eq"]);
    assert_eq!(shape.form, "braced");
    assert_eq!(shape.fields.len(), 1);
    assert_eq!(shape.fields[0].name, "id");
    assert_eq!(shape.fields[0].type_, "i64");
    assert_eq!(shape.fields[0].docs, vec!["Java: `id()`."]);
    assert!(shape.new.present);
    assert!(!shape.new.const_);
    assert!(shape.new.must_use);
    assert_eq!(shape.serializer.style, "struct");
    assert_eq!(shape.serializer.name, "KeepAlivePacketSerializer");
    assert!(!shape.serializer.public);
    assert_eq!(shape.extras, None);
}

#[test]
fn unit_struct_constant_serializer() {
    let shape = extract_shape(
        CLEAR_DIALOG,
        "ClearDialogPacket",
        "server/common/clear_dialog_packet",
        None,
    )
    .unwrap();
    assert_eq!(shape.form, "unit");
    assert!(!shape.new.present);
    assert_eq!(shape.serializer.style, "constant");
    assert!(shape.fields.is_empty());
}

#[test]
fn module_fn_with_return_type_before_serializer() {
    let shape = extract_shape(
        MODULE_FN_BEFORE_SERIALIZER,
        "ProbePacket",
        "server/play/probe_packet",
        None,
    )
    .unwrap();
    assert_eq!(shape.serializer.name, "ProbePacketSerializer");
    assert_eq!(shape.extras.as_deref(), Some("probe_packet.ext.rs"));
}

#[test]
fn inherent_helper_sets_extras() {
    let shape =
        extract_shape(FORWARD_EXTRA, "ProbePacket", "server/common/probe_packet", None).unwrap();
    assert_eq!(shape.extras.as_deref(), Some("probe_packet.ext.rs"));
}

#[test]
fn default_impl_becomes_extra() {
    let src = CLEAR_DIALOG.replace(
        "impl ServerPacket for ClearDialogPacket {}",
        "impl Default for ClearDialogPacket {}\nimpl ServerPacket for ClearDialogPacket {}",
    );
    let shape =
        extract_shape(&src, "ClearDialogPacket", "server/common/clear_dialog_packet", None).unwrap();
    assert_eq!(shape.extras.as_deref(), Some("clear_dialog_packet.ext.rs"));
}

#[test]
fn free_function_becomes_extra() {
    let src = format!("//! x\n\npub fn free_helper() {{}}\n{CLEAR_DIALOG}");
    let shape =
        extract_shape(&src, "ClearDialogPacket", "server/common/clear_dialog_packet", None).unwrap();
    assert_eq!(shape.extras.as_deref(), Some("clear_dialog_packet.ext.rs"));
}

#[test]
fn struct_allow_raises() {
    let src = CLEAR_DIALOG.replace(
        "#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]",
        "#[allow(dead_code)]\n#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]",
    );
    assert!(
        extract_shape(&src, "ClearDialogPacket", "server/common/clear_dialog_packet", None).is_err()
    );
}

#[test]
fn json_round_trip() {
    let shape =
        extract_shape(KEEP_ALIVE, "KeepAlivePacket", "server/common/keep_alive_packet", None).unwrap();
    let text = shape_to_json(&shape);
    let back = shape_from_json(&text).unwrap();
    assert_eq!(back, shape);
    let doc = bytemill::json::loads(&text).unwrap();
    assert!(doc.get("struct").is_some());
    assert!(doc.get("serializer_fn").is_some());
    assert!(text.ends_with('\n'));
}

#[test]
fn validating_new_body_is_captured() {
    let shape = extract_shape(
        NAME_ITEM_VALIDATION,
        "ClientNameItemPacket",
        "client/play/client_name_item_packet",
        None,
    )
    .unwrap();
    let body = shape.new.body.join("\n");
    assert!(body.contains("panic!("), "{body}");
    assert!(body.contains("Self { item_name }"), "{body}");
}

#[test]
fn assert_validating_new_body_is_captured() {
    let shape = extract_shape(
        PLUGIN_MESSAGE_ASSERT_NEW,
        "ClientPluginMessagePacket",
        "client/common/client_plugin_message_packet",
        None,
    )
    .unwrap();
    let body = shape.new.body.join("\n");
    assert!(body.contains("assert!("), "{body}");
    assert!(body.contains("channel.chars().count()"), "{body}");
}

#[test]
fn clamp_transform_new_body_is_captured() {
    let shape = extract_shape(
        VIEW_DISTANCE_CLAMP_NEW,
        "UpdateViewDistancePacket",
        "server/play/update_view_distance_packet",
        None,
    )
    .unwrap();
    let body = shape.new.body.join("\n");
    assert!(body.contains(".clamp(2, 32)"), "{body}");
}

#[test]
fn for_loop_without_panic_new_body_is_captured() {
    let shape =
        extract_shape(FOR_SCAN_NEW, "ProbePacket", "server/play/probe_packet", None).unwrap();
    let body = shape.new.body.join("\n");
    assert!(body.contains("let mut total"), "{body}");
    assert!(body.contains("for page in &pages"), "{body}");
    assert!(!shape.new.body.is_empty());
}

#[test]
fn trivial_new_body_stays_empty() {
    let shape =
        extract_shape(KEEP_ALIVE, "KeepAlivePacket", "server/common/keep_alive_packet", None).unwrap();
    assert!(shape.new.body.is_empty());
}

#[test]
fn multiline_new_docs_and_panics_section() {
    let shape = extract_shape(
        NAME_ITEM_PANICS_DOC,
        "ClientNameItemPacket",
        "client/play/client_name_item_packet",
        None,
    )
    .unwrap();
    let joined = shape.new.docs.join("\n");
    assert!(joined.contains("# Panics"), "{joined}");
    assert!(joined.contains("IllegalArgumentException"), "{joined}");
    assert!(joined.contains("longer than"), "{joined}");
}

#[test]
fn wrapped_java_doc_on_new() {
    let shape = extract_shape(
        WRAPPED_NEW_DOC,
        "ClientPunchPacket",
        "client/play/client_punch_packet",
        None,
    )
    .unwrap();
    assert_eq!(
        shape.new.docs,
        vec![
            "Java: `record ClientPunchPacket(int entityId) implements",
            "ClientPacket.Play`.",
            "Java: `entityId()`.",
        ]
    );
}

#[test]
fn read_via_new_detected_from_serializer_read() {
    let shape = extract_shape(
        READ_VIA_NEW_SNIPPET,
        "ClientNameItemPacket",
        "client/play/client_name_item_packet",
        None,
    )
    .unwrap();
    assert!(shape.serializer.read_via_new);
}

#[test]
fn read_struct_literal_does_not_set_read_via_new() {
    let shape = extract_shape(
        PLUGIN_MESSAGE_ASSERT_NEW,
        "ClientPluginMessagePacket",
        "client/common/client_plugin_message_packet",
        None,
    )
    .unwrap();
    assert!(!shape.serializer.read_via_new);
}

#[test]
fn multiline_new_attributes() {
    let shape = extract_shape(
        RECIPE_BOOK_SNIPPET,
        "RecipeBookSettingsPacket",
        "server/play/recipe_book_settings_packet",
        None,
    )
    .unwrap();
    assert!(
        shape.new.docs.iter().any(|d| d == "Java: `new RecipeBookSettingsPacket(boolean × 8)`."),
        "{:?}",
        shape.new.docs
    );
    assert!(shape.new.must_use);
    assert_eq!(
        shape.new.attrs,
        vec![
            "#[allow(clippy::fn_params_excessive_bools)]",
            "#[allow(\n        clippy::too_many_arguments,\n        reason = \"1:1 port of Java's RecipeBookSettingsPacket(boolean x 8)\"\n    )]",
        ]
    );
}

#[test]
fn attr_reason_with_bracket_does_not_truncate_docs() {
    let body = "    /// above attr\n    #[allow(foo, reason = \"has ] bracket\")]\n    pub fn new() -> Self { Self }\n";
    let item = advance_item_start(body, 0).unwrap();
    let (docs, _) = docs_and_attrs_before(body, item).unwrap();
    assert!(docs.iter().any(|d| d == "above attr"), "{docs:?}");
}

#[test]
fn doc_between_attributes_is_captured() {
    let body = "    #[allow(a)]\n    /// between attrs\n    #[must_use]\n    pub fn new() -> Self { Self }\n";
    let item = advance_item_start(body, 0).unwrap();
    let (docs, _) = docs_and_attrs_before(body, item).unwrap();
    assert_eq!(docs, vec!["between attrs"]);
}

#[test]
fn doc_line_preserves_markdown_indent() {
    let src = "//! Port of `x`.\n\n/// Line one\n///  indented second line\n#[derive(Clone)]\npub struct X;\n";
    let shape = extract_shape(src, "X", "server/common/x", None).unwrap();
    assert_eq!(shape.struct_docs[1], " indented second line");
}

#[test]
fn module_pub_const_sets_extras() {
    let shape = extract_shape(
        TAB_COMPLETE_CONST_EXTRA,
        "ClientTabCompletePacket",
        "client/play/client_tab_complete_packet",
        None,
    )
    .unwrap();
    assert_eq!(shape.extras.as_deref(), Some("client_tab_complete_packet.ext.rs"));
}

#[test]
fn mismatch_equal_and_differing() {
    let shape =
        extract_shape(KEEP_ALIVE, "KeepAlivePacket", "server/common/keep_alive_packet", None).unwrap();
    assert!(mismatch(&shape, &[("id".to_string(), "i64".to_string())]).is_empty());
    let bad = mismatch(&shape, &[("id".to_string(), "i32".to_string())]);
    assert_eq!(bad.len(), 1);
    assert!(bad[0].contains("id"), "{}", bad[0]);
    assert!(bad[0].contains("i64"), "{}", bad[0]);
    assert!(bad[0].contains("i32"), "{}", bad[0]);
}

// ---------------------------------------------------------------------------
// The real worktree
// ---------------------------------------------------------------------------

fn worktree_present() -> bool {
    common::rstom_tables::rstom_dir()
        .join("crates/rstom/src/network/packet")
        .is_dir()
}

#[test]
fn eligible_packets_extract_and_match_bps() {
    if !worktree_present() {
        eprintln!("rstom worktree packet sources not present; skipping");
        return;
    }
    let rstom = common::rstom_tables::rstom_dir();
    let resolved = common::load_minecraft().unwrap();
    let table = common::rstom_tables::real_table().unwrap();
    let units = bytemill::minestom::build_units(&resolved, &table).unwrap();

    let mut eligible: Vec<String> = Vec::new();
    let mut ineligible: BTreeMap<String, String> = BTreeMap::new();
    for unit in &units {
        let path = hand_packet_path(&rstom, unit);
        if !path.is_file() {
            ineligible.insert(unit.cls.name.clone(), format!("missing hand file {}", path.display()));
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let module = bytemill::shapes::module_path(unit);
        let shape = match extract_shape(&source, &unit.cls.name, &module, Some(&path)) {
            Ok(s) => s,
            Err(err) => {
                ineligible.insert(unit.cls.name.clone(), err.to_string());
                continue;
            }
        };
        let bps = bytemill::shapes::bps_field_pairs(&unit.pkt, &resolved).unwrap();
        let reasons = mismatch(&shape, &bps);
        if !reasons.is_empty() {
            ineligible.insert(unit.cls.name.clone(), reasons.join("; "));
            continue;
        }
        eligible.push(unit.cls.name.clone());
        assert_eq!(shape_from_json(&shape_to_json(&shape)).unwrap(), shape);
    }

    // The committed ledger is a subset of the eligible set.
    let committed: std::collections::BTreeSet<String> =
        bytemill::shapes::load_ledger(&common::repo_root().join("targets/rstom/shapes"))
            .unwrap()
            .into_iter()
            .map(|(_, s)| s.cls)
            .collect();
    assert_eq!(committed.len(), 64);
    let eligible_set: std::collections::BTreeSet<String> = eligible.iter().cloned().collect();
    let lost: Vec<&String> = committed.difference(&eligible_set).collect();
    assert!(lost.is_empty(), "lost: {lost:?}");

    // The worklist summary matches the committed one.
    let mut summary: BTreeMap<String, usize> = BTreeMap::new();
    for unit in &units {
        let status = packet_report(unit, &resolved, &rstom).unwrap();
        let category = if status == "eligible" {
            "eligible".to_string()
        } else if status.starts_with("field count") {
            "field count mismatch".to_string()
        } else if status.starts_with("field ") {
            "field name/type mismatch".to_string()
        } else if status == "unexpected public type" {
            "extract: extra public type in hand file".to_string()
        } else if status.starts_with("unexpected public type") {
            "extract: nested public type".to_string()
        } else if status.starts_with("missing hand file") {
            "missing hand file".to_string()
        } else if status.starts_with("impl ") {
            "extract: unsupported trait impl".to_string()
        } else if status == "free function" {
            "extract: free function".to_string()
        } else if status == "missing derive on struct" {
            "extract: missing derive on struct".to_string()
        } else if status.starts_with("missing serializer struct") {
            "extract: missing serializer struct".to_string()
        } else {
            status
        };
        *summary.entry(category).or_insert(0) += 1;
    }
    let expected: BTreeMap<String, usize> = [
        ("eligible", 65),
        ("field count mismatch", 91),
        ("field name/type mismatch", 78),
        ("extract: missing derive on struct", 1),
        ("missing hand file", 1),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), *v))
    .collect();
    assert_eq!(summary, expected);
}

#[test]
fn module_level_fn_packets_extract() {
    if !worktree_present() {
        eprintln!("rstom worktree packet sources not present; skipping");
        return;
    }
    let rstom = common::rstom_tables::rstom_dir();
    for (cls, module) in [
        ("EntityRotationPacket", "server/play/entity_rotation_packet"),
        (
            "EntityPositionAndRotationPacket",
            "server/play/entity_position_and_rotation_packet",
        ),
        ("EntityHeadLookPacket", "server/play/entity_head_look_packet"),
        (
            "ClientAdvancementTabPacket",
            "client/play/client_advancement_tab_packet",
        ),
    ] {
        let path = rstom.join("crates/rstom/src/network/packet").join(format!("{module}.rs"));
        let text = std::fs::read_to_string(&path).unwrap();
        let shape = extract_shape(&text, cls, module, Some(&path)).unwrap();
        assert!(shape.serializer.name.ends_with("Serializer"), "{cls}");
    }
}

/// Run the compiled `bml` exactly like the Python tests spawn the CLI.
fn run_cli(args: &[String]) -> (i32, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bml"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn shapes_extract_eligible_keeps_committed_packets_with_worktree() {
    if !worktree_present() {
        eprintln!("rstom worktree absent; skipping");
        return;
    }
    let out = std::env::temp_dir().join(format!("bps-shapes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let (code, _) = run_cli(&[
        "shapes".into(),
        "extract".into(),
        common::schema_path("minecraft_26_3_wiki.bps").to_string_lossy().into_owned(),
        "--rstom".into(),
        common::rstom_tables::rstom_dir().to_string_lossy().into_owned(),
        "--eligible".into(),
        "--out".into(),
        out.to_string_lossy().into_owned(),
    ]);
    assert_eq!(code, 0);
    let committed: std::collections::BTreeSet<String> =
        bytemill::shapes::load_ledger(&common::repo_root().join("targets/rstom/shapes"))
            .unwrap()
            .into_iter()
            .map(|(_, s)| s.cls)
            .collect();
    let written: std::collections::BTreeSet<String> =
        bytemill::shapes::load_ledger(&out)
            .unwrap()
            .into_iter()
            .map(|(_, s)| s.cls)
            .collect();
    assert!(committed.is_subset(&written), "lost: {:?}", committed.difference(&written));
}

#[test]
fn shapes_report_runs() {
    if !worktree_present() {
        eprintln!("rstom worktree absent; skipping");
        return;
    }
    let (code, out) = run_cli(&[
        "shapes".into(),
        "report".into(),
        common::schema_path("minecraft_26_3_wiki.bps").to_string_lossy().into_owned(),
        "--rstom".into(),
        common::rstom_tables::rstom_dir().to_string_lossy().into_owned(),
    ]);
    assert_eq!(code, 0);
    assert!(out.contains("eligible"), "{out}");
}

// Ports tests/test_shapes.py `test_shapes_extract_eligible_skips_unrepresentable`.
// The Python test copies the real packets.json with no skip; without the
// rstom worktree there is nothing to copy, so this skips like the other
// worktree tests.
#[test]
fn shapes_extract_eligible_skips_unrepresentable() {
    let packets_json = common::rstom_tables::packets_gen_schema();
    if !packets_json.is_file() {
        eprintln!("skipped: rstom packets_gen schema not present");
        return;
    }
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!(
        "bps-shapes-unrepresentable-{}-{n}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    struct Guard(std::path::PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _guard = Guard(tmp.clone());
    let write = |rel: &str, text: &str| {
        let path = tmp.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    };
    write(
        "ws/crates/rstom/src/network/packet/server/common/clear_dialog_packet.rs",
        CLEAR_DIALOG,
    );
    write(
        "ws/crates/rstom/src/network/packet/server/play/recipe_book_settings_packet.rs",
        &format!("//! x\npub fn bad() {{}}\n{RECIPE_BOOK_SNIPPET}"),
    );
    std::fs::write(tmp.join("packets.json"), std::fs::read(&packets_json).unwrap()).unwrap();
    let (code, _) = run_cli(&[
        "shapes".into(),
        "extract".into(),
        common::schema_path("minecraft_26_3_wiki.bps")
            .to_string_lossy()
            .into_owned(),
        "--rstom".into(),
        tmp.join("ws").to_string_lossy().into_owned(),
        "--packets-json".into(),
        tmp.join("packets.json").to_string_lossy().into_owned(),
        "--eligible".into(),
        "--out".into(),
        tmp.join("shapes").to_string_lossy().into_owned(),
    ]);
    assert_eq!(code, 0);
    let mut written = Vec::new();
    let mut stack = vec![tmp.join("shapes")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "json") {
                written.push(path);
            }
        }
    }
    assert!(!written.is_empty(), "no shapes written");
    assert!(
        written
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n == "clear_dialog_packet.json")),
        "clear_dialog_packet.json missing: {written:?}"
    );
}

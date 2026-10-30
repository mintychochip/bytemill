//! Bound shapes: `emit rstom-tree` bodies written over hand fields that do not mirror the wire.

mod common;

use bytemill::errors::BmlResult;
use bytemill::minestom::build_units;
use bytemill::shapes::{Shape, shape_from_json, shape_to_json};
use bytemill::tree_bind::Bindings;
use bytemill::tree_bound::bound_body;
use bytemill::tree_packet::tree_packet_bound;

include!("fixtures/mod.rs");

/// A bound `ProbePacket` shape: the hand `fields` (JSON objects) and any `extra` top-level keys.
fn shape(fields: &str, extra: &str) -> Shape {
    let text = format!(
        r#"{{
  "class": "ProbePacket", "module": "server/play/probe_packet", "module_docs": [],
  "struct": {{"docs": [], "derives": ["Clone", "Debug", "PartialEq"], "form": "braced"}},
  "fields": [{fields}],
  "new": {{"present": true, "must_use": true, "const": false, "attrs": [], "docs": [], "body": []}},
  "serializer_fn": {{"docs": [], "must_use": true}},
  "serializer_struct": {{"style": "struct", "name": "ProbePacketSerializer", "pub": false, "docs": [], "read_via_new": false}},
  "bound": true{extra}
}}"#
    );
    shape_from_json(&text).unwrap_or_else(|e| panic!("shape: {e:?}\n{text}"))
}

/// What `emit rstom-tree` renders for `shape` over the BML packet `probe { body }`, whitespace
/// collapsed so an assertion does not depend on formatting.
fn render(body: &str, shape: &Shape, bindings: &Bindings) -> BmlResult<String> {
    let source = format!(
        "import \"{}\"\n{}",
        common::repo_root()
            .join("schemas/minecraft_core.bml")
            .display(),
        probe_source(&format!("packet probe {{ {body} }}"))
    );
    let dir = temp_dir("bound");
    let schema = dir.join("p.bml");
    write(&schema, &source);
    let resolved = bytemill::analyze(&bytemill::load_schema(&[schema]).unwrap()).unwrap();
    let table = common::rstom_tables::probe_table();
    let unit = build_units(&resolved, &table)
        .unwrap()
        .into_iter()
        .find(|u| u.pkt.name == "probe")
        .unwrap();
    let bound = bound_body(&unit, &resolved, shape, bindings)?;
    let text = tree_packet_bound(&unit, shape, &bound)?;
    Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn render_ok(body: &str, fields: &str, extra: &str) -> String {
    render(body, &shape(fields, extra), &Bindings::empty()).unwrap_or_else(|e| panic!("{e:?}"))
}

fn render_err(body: &str, fields: &str, extra: &str) -> String {
    match render(body, &shape(fields, extra), &Bindings::empty()) {
        Ok(text) => panic!("expected an error, got:\n{text}"),
        Err(e) => e.message,
    }
}

fn has(text: &str, wanted: &str) {
    assert!(text.contains(wanted), "missing `{wanted}` in:\n{text}");
}

#[test]
fn a_hand_field_reads_and_writes_under_its_wire_name() {
    let text = render_ok(
        "entity_id: varint kind_id: varint",
        r#"{"name": "target", "type": "i32", "docs": [], "wire": "entity_id"},
           {"name": "kind", "type": "Kind", "docs": [], "wire": "kind_id",
            "write": "$.ordinal()", "read": "Kind::from_ordinal($)"}"#,
        "",
    );
    // a plain field is written from the hand name and read into the wire name
    has(&text, "buffer.write(VAR_INT, value.target);");
    has(&text, "let entity_id = buffer.read(VAR_INT);");
    has(&text, "target: entity_id,");
    // templates convert the hand value on the way out and back
    has(&text, "buffer.write(VAR_INT, value.kind.ordinal());");
    has(&text, "let kind_val = Kind::from_ordinal(kind_id);");
    has(&text, "kind: kind_val,");
}

#[test]
fn a_codec_carries_the_wire_field_and_a_hand_only_field_is_off_the_wire() {
    let text = render_ok(
        "mode_id: u8",
        r#"{"name": "mode", "type": "Mode", "docs": [], "wire": "mode_id", "codec": "enum_type::<Mode>()"},
           {"name": "locked", "type": "bool", "docs": [], "wire": [], "read": "false"}"#,
        "",
    );
    has(
        &text,
        "buffer.write(enum_type::<Mode>(), value.mode.clone());",
    );
    has(&text, "let mode_id = buffer.read(enum_type::<Mode>());");
    has(&text, "locked: false,");
    assert!(!text.contains("value.locked"), "{text}");
}

#[test]
fn a_counted_list_writes_its_length_and_reads_that_many_elements() {
    let text = render_ok(
        "ids_count: varint ids: varint[] count = ids_count",
        r#"{"name": "ids", "type": "Vec<i32>", "docs": [], "wire": "ids"}"#,
        "",
    );
    has(&text, "buffer.write(VAR_INT, value.ids.len() as i32);");
    has(
        &text,
        "for it0 in &value.ids { buffer.write(VAR_INT, *it0); }",
    );
    has(&text, "let ids_count = buffer.read(VAR_INT);");
    has(&text, "for _ in 0..ids_count {");
    has(&text, "ids.push(");
}

#[test]
fn a_list_of_records_binds_each_element_field() {
    let text = render_ok(
        "entries_count: varint entries: { id: varint name: string }[] count = entries_count",
        r#"{"name": "entries", "type": "Vec<Entry>", "docs": [], "wire": "entries", "item": {"fields": [
            {"name": "id", "type": "i32", "docs": []},
            {"name": "name", "type": "String", "docs": []}]}}"#,
        "",
    );
    has(
        &text,
        "buffer.write(VAR_INT, it0.id); buffer.write(STRING, it0.name.clone());",
    );
    has(
        &text,
        "let id = buffer.read(VAR_INT); let name = buffer.read(STRING); entries.push(Entry { id, name });",
    );
}

#[test]
fn an_option_derives_the_flag_that_guards_it() {
    let text = render_ok(
        "has_title: boolean if has_title { title: string }",
        r#"{"name": "title", "type": "Option<String>", "docs": [], "wire": "title"}"#,
        "",
    );
    has(
        &text,
        "let has_title = value.title.is_some(); buffer.write(BOOLEAN, has_title);",
    );
    has(
        &text,
        "if let Some(v) = &value.title { buffer.write(STRING, v.clone()); }",
    );
    has(
        &text,
        "let title = if has_title { Some(buffer.read(STRING)) } else { None };",
    );
}

#[test]
fn a_nested_hand_struct_is_flattened_over_the_wire_fields() {
    let text = render_ok(
        "a: varint b: string",
        r#"{"name": "holder", "type": "Holder", "docs": [], "nest": {"fields": [
            {"name": "id", "type": "i32", "docs": [], "wire": "a"},
            {"name": "data", "type": "String", "docs": [], "wire": "b"}], "ctor": "Holder { id, data }"}}"#,
        "",
    );
    has(
        &text,
        "buffer.write(VAR_INT, value.holder.id); buffer.write(STRING, value.holder.data.clone());",
    );
    has(&text, "holder: Holder { id, data },");
}

#[test]
fn a_guarded_field_without_a_flag_is_written_under_its_value_guard_and_defaults_when_absent() {
    let text = render_ok(
        "kind: varint if kind == 0 { data: string }",
        r#"{"name": "kind", "type": "i32", "docs": []},
           {"name": "data", "type": "String", "docs": [], "wire": "data", "absent": "String::new()"}"#,
        "",
    );
    // the guard reads the value that was just written, and a read that skips the field defaults it
    has(
        &text,
        "let kind = value.kind; buffer.write(VAR_INT, kind); if kind == 0 { buffer.write(STRING, value.data.clone()); }",
    );
    has(
        &text,
        "let data = if kind == 0 { Some(buffer.read(STRING)) } else { None };",
    );
    has(
        &text,
        "let data_val = data.unwrap_or_else(|| String::new());",
    );
}

#[test]
fn constants_give_an_uncarried_wire_field_its_value() {
    let text = render_ok(
        "a: varint b: varint",
        r#"{"name": "a", "type": "i32", "docs": [], "wire": "a"}"#,
        r#", "constants": {"b": "7"}"#,
    );
    has(
        &text,
        "buffer.write(VAR_INT, value.a); buffer.write(VAR_INT, 7);",
    );
    // a read consumes the constant's bytes and drops them
    has(
        &text,
        "let a = buffer.read(VAR_INT); let _ = buffer.read(VAR_INT);",
    );
}

#[test]
fn a_wire_field_no_hand_field_carries_is_refused() {
    let message = render_err(
        "a: varint b: varint",
        r#"{"name": "a", "type": "i32", "docs": [], "wire": "a"}"#,
        "",
    );
    has(&message, "wire field b is carried by no hand field");
}

#[test]
fn a_constant_that_names_no_uncarried_wire_field_is_refused() {
    let message = render_err(
        "a: varint",
        r#"{"name": "a", "type": "i32", "docs": [], "wire": "a"}"#,
        r#", "constants": {"a2": "1", "a": "2"}"#,
    );
    has(&message, "constant a2 is not the value of any wire field");
}

#[test]
fn a_field_over_several_wire_fields_needs_templates_or_functions() {
    let message = render_err(
        "a: varint b: varint",
        r#"{"name": "both", "type": "Pair", "docs": [], "wire": "a..b"}"#,
        "",
    );
    has(&message, "field both carries 2 wire fields");
    has(&message, "give it \"write\" and \"read\" templates");
}

#[test]
fn put_and_take_hand_a_span_to_buffer_functions() {
    let text = render_ok(
        "a: varint b: string",
        r#"{"name": "both", "type": "Pair", "docs": [], "wire": "a..b",
            "put": "write_pair(buffer, &$);", "take": "read_pair(buffer)"}"#,
        "",
    );
    has(&text, "write_pair(buffer, &value.both);");
    has(&text, "let a = read_pair(buffer);");
    has(&text, "both: a,");
}

#[test]
fn the_bindings_table_converts_a_hand_type_unless_the_field_says_otherwise() {
    let table = Bindings::parse(
        r#"{"types": {"Mode": [{"wire": ["varint"], "write": "$.ordinal()",
            "read": "Mode::from_ordinal($)", "imports": ["use crate::entity::Mode;"]}]}}"#,
    )
    .unwrap();
    let bare = shape(
        r#"{"name": "mode", "type": "Mode", "docs": [], "wire": "a"}"#,
        "",
    );
    let text = render("a: varint", &bare, &table).unwrap();
    has(&text, "buffer.write(VAR_INT, value.mode.ordinal());");
    has(&text, "Mode::from_ordinal(a)");
    has(&text, "use crate::entity::Mode;");

    // a template on the field wins over the table
    let own = shape(
        r#"{"name": "mode", "type": "Mode", "docs": [], "wire": "a",
            "write": "$.id()", "read": "Mode::from_id($)"}"#,
        "",
    );
    let text = render("a: varint", &own, &table).unwrap();
    has(&text, "buffer.write(VAR_INT, value.mode.id());");
    assert!(!text.contains("ordinal"), "{text}");

    // a binding over other wire types does not apply
    let text = render("a: string", &bare, &table).unwrap_err().message;
    has(&text, "field mode");
}

#[test]
fn hooks_land_after_the_local_they_name() {
    let text = render_ok(
        "a: varint b: varint",
        r#"{"name": "a", "type": "i32", "docs": []}, {"name": "b", "type": "i32", "docs": []}"#,
        r#", "hooks": {"read.after:a": ["check(a);"], "write.start": ["audit();"]}"#,
    );
    has(
        &text,
        "let a = buffer.read(VAR_INT); check(a); let b = buffer.read(VAR_INT);",
    );
    has(
        &text,
        "let value = downcast::<ProbePacket>(value); audit(); buffer.write(VAR_INT, value.a);",
    );
}

#[test]
fn an_unknown_hook_is_refused() {
    let message = render_err(
        "a: varint",
        r#"{"name": "a", "type": "i32", "docs": []}"#,
        r#", "hooks": {"write.before:a": ["audit();"]}"#,
    );
    has(&message, "unknown hook 'write.before:a'");
}

#[test]
fn an_alias_shape_adds_a_serializer_to_a_type_another_module_defines() {
    let text = render_ok(
        "a: varint",
        r#"{"name": "a", "type": "i32", "docs": []}"#,
        r#", "alias": {"path": "crate::network::packet::server::common::probe_packet::ProbePacket",
            "serializer_fn": "inline_serializer"}"#,
    );
    has(
        &text,
        "use crate::network::packet::server::common::probe_packet::ProbePacket;",
    );
    has(&text, "impl ProbePacket {");
    has(
        &text,
        "pub fn inline_serializer() -> Arc<dyn PacketSerializer> { Arc::new(ProbePacketSerializer) }",
    );
    has(&text, "impl PacketSerializer for ProbePacketSerializer {");
    // the type is the other module's: no struct, no packet impls of its own
    assert!(!text.contains("pub struct ProbePacket"), "{text}");
    assert!(!text.contains("impl Packet for"), "{text}");
}

#[test]
fn a_path_qualified_copy_type_is_not_cloned() {
    let text = render_ok(
        "id: uuid",
        r#"{"name": "id", "type": "crate::network::uuid::Uuid", "docs": []}"#,
        "",
    );
    has(&text, "buffer.write(UUID, value.id);");
    assert!(!text.contains("value.id.clone()"), "{text}");
}

#[test]
fn a_bound_shape_round_trips_through_its_json() {
    let s = shape(
        r#"{"name": "ids", "type": "Vec<Entry>", "docs": [], "wire": "ids", "item": {"fields": [
              {"name": "id", "type": "i32", "docs": [], "write": "$.id()", "read": "Id::new($)"}],
              "ctor": "Entry::new(id)"}},
           {"name": "holder", "type": "Holder", "docs": [], "nest": {"fields": [
              {"name": "data", "type": "String", "docs": [], "wire": ["b", "c"], "put": "put(&$);", "take": "take()",
               "absent": "String::new()", "guarded_default": true, "write_whole": true, "read_flat": true}]}},
           {"name": "locked", "type": "bool", "docs": [], "wire": [], "read": "false"}"#,
        r##", "constants": {"z": "1"}, "hooks": {"read.end": ["done();"]},
            "uses": ["use crate::x::Y;"], "serializer_attrs": ["#[allow(clippy::x)]"]"##,
    );
    let again = shape_from_json(&shape_to_json(&s)).unwrap();
    assert_eq!(again, s);
    assert!(s.has_bindings());
}

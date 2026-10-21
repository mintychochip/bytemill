//! The protodef importer, one construct at a time: a small `protocol.json` with one packet is
//! imported, loaded through the real parser and analyzer, and real bytes are decoded strictly
//! and encoded back.

use bytemill::ast::Schema;
use bytemill::interp::{decode_packet, encode_packet};
use bytemill::protodef::lint::lint;
use bytemill::protodef::model::Protocol;
use bytemill::protodef::{FileSpec, import};
use bytemill::{Env, ResolvedSchema, Value, analyze, load_schema, parse};

const STRING: &str = r#""string": ["pstring", {"countType": "varint"}]"#;

/// A protocol with one packet, `t`, in play.clientbound.
fn document(types: &str, body: &str) -> String {
    format!(
        r#"{{"types": {{{types}}},
  "play": {{"toClient": {{"types": {{
    "packet": ["container", [
      {{"name": "name", "type": ["mapper", {{"type": "varint", "mappings": {{"0x00": "t"}}}}]}},
      {{"name": "params", "type": ["switch", {{"compareTo": "name", "fields": {{"t": "packet_t"}}, "default": "void"}}]}}]],
    "packet_t": {body}
  }}}}}}}}"#
    )
}

fn generate(types: &str, body: &str) -> Result<String, String> {
    let proto = Protocol::from_json(&document(types, body))?;
    import(
        &proto,
        &FileSpec {
            protocol: 1,
            release: "0.0",
            also: &[],
            source: "test",
            patches: &[],
            note: None,
        },
    )
}

fn resolve(source: &str) -> ResolvedSchema {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../schemas/std/");
    let std = load_schema(&[format!("{root}integers.bml"), format!("{root}varint.bml")]).unwrap();
    let generated = parse(source).unwrap_or_else(|e| panic!("{e}\n{source}"));
    analyze(&Schema {
        encodings: std
            .encodings
            .into_iter()
            .chain(generated.encodings)
            .collect(),
        enums: Vec::new(),
        packets: generated.packets,
        protocols: generated.protocols,
        imports: Vec::new(),
    })
    .unwrap_or_else(|e| panic!("{e}\n{source}"))
}

fn schema(types: &str, body: &str) -> ResolvedSchema {
    let source = generate(types, body).unwrap_or_else(|e| panic!("import failed: {e}"));
    resolve(&source)
}

fn hex(s: &str) -> Vec<u8> {
    let s: String = s.split_whitespace().collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Strict decode, then encode of the decoded value must give the input back.
fn round_trip(r: &ResolvedSchema, bytes: &str) -> Env {
    let raw = hex(bytes);
    let decoded =
        decode_packet(r, "t", &raw, true).unwrap_or_else(|e| panic!("decode {bytes}: {e}"));
    let again = encode_packet(r, "t", &decoded).unwrap_or_else(|e| panic!("encode {bytes}: {e}"));
    assert_eq!(again, raw, "re-encoded bytes of {bytes}");
    decoded
}

fn get<'a>(v: &'a Env, key: &str) -> &'a Value {
    v.get(key)
        .unwrap_or_else(|| panic!("no {key:?} in {}", bytemill::json::dumps(&Value::map(v.clone()))))
}

fn int(v: &Env, key: &str) -> i128 {
    match get(v, key) {
        Value::Int(i) => *i,
        other => panic!("{key} is {other:?}"),
    }
}

fn sub<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key)
        .unwrap_or_else(|| panic!("no {key:?} in {}", bytemill::json::dumps(v)))
}

fn as_int(v: &Value) -> i128 {
    match v {
        Value::Int(i) => *i,
        Value::Bool(b) => i128::from(*b),
        other => panic!("{other:?} is not an integer"),
    }
}

#[test]
fn scalars_strings_and_uuids() {
    let r = schema(
        STRING,
        r#"["container", [
          {"name": "a", "type": "u8"}, {"name": "b", "type": "i16"}, {"name": "c", "type": "string"},
          {"name": "d", "type": "UUID"}, {"name": "e", "type": "f32"}, {"name": "f", "type": "varlong"}]]"#,
    );
    let v = round_trip(
        &r,
        "01 FFFE 02 6869 0001020304050607 08090a0b0c0d0e0f 3FC00000 AC02",
    );
    assert_eq!(int(&v, "a"), 1);
    assert_eq!(int(&v, "b"), -2);
    assert_eq!(get(&v, "c"), &Value::str("hi"));
    assert_eq!(as_int(sub(get(&v, "d"), "msb")), 0x0001_0203_0405_0607);
    assert_eq!(get(&v, "e"), &Value::Float(1.5));
    assert_eq!(int(&v, "f"), 300);
}

#[test]
fn arrays_options_buffers_and_the_rest_of_the_packet() {
    let r = schema(
        "",
        r#"["container", [
          {"name": "n", "type": ["array", {"countType": "varint", "type": "u8"}]},
          {"name": "o", "type": ["option", "varint"]},
          {"name": "b", "type": ["buffer", {"countType": "u8"}]},
          {"name": "k", "type": "u8"},
          {"name": "items", "type": ["array", {"count": "k", "type": "u8"}]},
          {"name": "fixed", "type": ["buffer", {"count": 3}]},
          {"name": "tail", "type": "restBuffer"}]]"#,
    );
    let v = round_trip(&r, "02 0A0B  01 05  01 FF  02 0102  090807  AABB");
    assert_eq!(int(&v, "n_count"), 2);
    assert_eq!(get(&v, "has_o"), &Value::Bool(true));
    assert_eq!(int(&v, "o"), 5);
    assert_eq!(int(&v, "b_len"), 1);
    assert_eq!(get(&v, "tail"), &Value::bytes(vec![0xAA, 0xBB]));
    // An absent option has no value field at all.
    let absent = round_trip(&r, "00  00  00  00  000000  0000 00");
    assert_eq!(get(&absent, "has_o"), &Value::Bool(false));
    assert!(absent.get("o").is_none());
}

#[test]
fn bitfields_and_bitflags() {
    let r = schema(
        "",
        r#"["container", [
          {"name": "loc", "type": ["bitfield", [
            {"name": "x", "size": 26, "signed": true}, {"name": "z", "size": 26, "signed": true},
            {"name": "y", "size": 12, "signed": true}]]},
          {"name": "flags", "type": ["bitflags", {"type": "u8", "flags": ["a", "b", "c"]}]},
          {"anon": true, "type": ["bitfield", [{"name": "hi", "size": 3, "signed": false}, {"name": "lo", "size": 5, "signed": false}]]}]]"#,
    );
    // x = -1, z = 2, y = -3
    let v = round_trip(&r, "FFFFFFC000002FFD  05  B5");
    assert_eq!(int(&v, "loc_x"), -1);
    assert_eq!(int(&v, "loc_z"), 2);
    assert_eq!(int(&v, "loc_y"), -3);
    assert_eq!(get(&v, "flags_a"), &Value::Bool(true));
    assert_eq!(get(&v, "flags_b"), &Value::Bool(false));
    assert_eq!(get(&v, "flags_c"), &Value::Bool(true));
    assert_eq!(int(&v, "hi"), 5);
    assert_eq!(int(&v, "lo"), 0x15);
}

#[test]
fn a_switch_on_a_mapper_selects_by_name_and_falls_back_to_default() {
    let r = schema(
        "",
        r#"["container", [
          {"name": "kind", "type": ["mapper", {"type": "varint", "mappings": {"0": "circle", "1": "square"}}]},
          {"name": "shape", "type": ["switch", {"compareTo": "kind", "fields": {
            "circle": ["container", [{"name": "r", "type": "f32"}]],
            "square": ["container", [{"name": "w", "type": "u8"}, {"name": "h", "type": "u8"}]]},
            "default": "void"}]}]]"#,
    );
    let square = round_trip(&r, "01 03 04");
    assert_eq!(as_int(sub(get(&square, "shape"), "w")), 3);
    assert_eq!(as_int(sub(get(&square, "shape"), "h")), 4);
    let circle = round_trip(&r, "00 3FC00000");
    assert_eq!(sub(get(&circle, "shape"), "r"), &Value::Float(1.5));
    // An unmapped value reaches the default arm: nothing follows.
    round_trip(&r, "05");
}

#[test]
fn a_switch_on_a_string_field_compares_strings() {
    let r = schema(
        STRING,
        r#"["container", [
          {"name": "channel", "type": "string"},
          {"name": "data", "type": ["switch", {"compareTo": "channel", "fields": {"brand": "string"}, "default": "restBuffer"}]}]]"#,
    );
    let brand = round_trip(&r, "05 6272616e64 01 78");
    assert_eq!(sub(get(&brand, "data"), "value"), &Value::str("x"));
    let other = round_trip(&r, "05 6f74686572 0909");
    assert_eq!(sub(get(&other, "data"), "value"), &Value::bytes(vec![9, 9]));
}

#[test]
fn negative_keys_and_anonymous_switches_splice_their_fields() {
    // The 1.13 slot: an item id of -1 is an empty slot, anything else carries a count and NBT.
    let r = schema(
        "",
        r#"["container", [{"name": "s", "type": ["container", [
          {"name": "itemId", "type": "i16"},
          {"anon": true, "type": ["switch", {"compareTo": "itemId", "fields": {"-1": "void"},
            "default": ["container", [{"name": "itemCount", "type": "i8"}, {"name": "nbtData", "type": "optionalNbt"}]]}]}]]}]]"#,
    );
    let empty = round_trip(&r, "FFFF");
    assert_eq!(as_int(sub(get(&empty, "s"), "item_id")), -1);
    assert!(get(&empty, "s").get("item_count").is_none());
    let full = round_trip(&r, "0005 02 00");
    assert_eq!(as_int(sub(get(&full, "s"), "item_id")), 5);
    // The anonymous arm's fields sit beside `item_id`, not under a name of their own.
    assert_eq!(as_int(sub(get(&full, "s"), "item_count")), 2);
}

#[test]
fn a_switch_on_a_later_read_option_sees_it_as_absent() {
    // scoreboard_objective: `number_format` is an option inside a switch, and `styling` switches on it.
    let r = schema(
        "",
        r#"["container", [
          {"name": "action", "type": "i8"},
          {"name": "number_format", "type": ["switch", {"compareTo": "action", "fields": {"0": ["option", "varint"], "2": ["option", "varint"]}, "default": "void"}]},
          {"name": "styling", "type": ["switch", {"compareTo": "number_format", "fields": {"1": "u8", "2": "u16"}, "default": "void"}]}]]"#,
    );
    let present = round_trip(&r, "00 01 02 0007");
    assert_eq!(int(&present, "number_format"), 2);
    assert_eq!(int(&present, "styling"), 7);
    // Option absent, then an action that has no option at all: `styling` is void both times.
    round_trip(&r, "00 00");
    round_trip(&r, "01");
}

#[test]
fn anonymous_containers_flatten_into_their_parent() {
    let r = schema(
        "",
        r#"["container", [{"name": "x", "type": "u8"},
          {"anon": true, "type": ["container", [{"name": "y", "type": "u8"}, {"name": "z", "type": "u8"}]]}]]"#,
    );
    let v = round_trip(&r, "01 02 03");
    assert_eq!((int(&v, "x"), int(&v, "y"), int(&v, "z")), (1, 2, 3));
}

#[test]
fn metadata_loops_stop_at_the_end_byte_and_top_bit_arrays_chain() {
    let r = schema(
        "",
        r#"["container", [
          {"name": "meta", "type": ["entityMetadataLoop", {"endVal": 255, "type": ["container", [{"name": "key", "type": "u8"}, {"name": "value", "type": "varint"}]]}]},
          {"name": "eq", "type": ["topBitSetTerminatedArray", {"type": ["container", [{"name": "slot", "type": "i8"}, {"name": "item", "type": "u8"}]]}]}]]"#,
    );
    let v = round_trip(&r, "00 05  01 06  FF  81 0A  02 0B");
    let Value::List(entries) = get(&v, "meta") else {
        panic!("meta is not a list")
    };
    assert_eq!(entries.len(), 2);
    let first = get(&v, "eq");
    assert_eq!(as_int(sub(first, "slot")), 1);
    assert_eq!(as_int(sub(first, "item")), 10);
    let next = sub(first, "next");
    assert_eq!(as_int(sub(next, "slot")), 2);
    assert!(
        next.get("next").is_none(),
        "the last element has its top bit clear"
    );
}

#[test]
fn registry_holders_are_an_id_or_an_inline_value() {
    let r = schema(
        STRING,
        r#"["container", [
          {"name": "h", "type": ["registryEntryHolder", {"baseName": "id", "otherwise": {"name": "inline", "type": "string"}}]},
          {"name": "s", "type": ["registryEntryHolderSet", {"base": {"name": "tag", "type": "string"}, "otherwise": {"name": "ids", "type": "varint"}}]}]]"#,
    );
    let inline = round_trip(&r, "00 01 61  00 01 62");
    assert_eq!(sub(get(&inline, "h"), "inline"), &Value::str("a"));
    assert_eq!(sub(get(&inline, "s"), "tag"), &Value::str("b"));
    let by_id = round_trip(&r, "05  03 07 08");
    assert_eq!(as_int(sub(get(&by_id, "h"), "id")), 5);
    let Value::List(ids) = sub(get(&by_id, "s"), "ids") else {
        panic!("ids is not a list")
    };
    assert_eq!(ids.iter().map(as_int).collect::<Vec<_>>(), vec![7, 8]);
}

#[test]
fn the_four_nbt_flavours() {
    let r = schema(
        "",
        r#"["container", [
          {"name": "named", "type": "nbt"}, {"name": "maybe", "type": "optionalNbt"},
          {"name": "anon", "type": "anonymousNbt"}, {"name": "anon_maybe", "type": "anonOptionalNbt"}]]"#,
    );
    // named: compound with an empty name; maybe: absent; anon: {"a": "hi"}; anon_maybe: absent.
    round_trip(&r, "0a 0000 00   00   0a 08 0001 61 0002 6869 00   00");
    // A present optional named root, and a list of int arrays (a compound holding one).
    round_trip(
        &r,
        "0a 0000 00   0a 0001 6e 00   0a 09 0001 6c 0b 00000001 00000001 00000007 00   00",
    );
}

#[test]
fn parametrised_types_are_expanded_at_the_use_site() {
    let r = schema(
        r#""particleData": ["switch", {"compareTo": "$compareTo", "fields": {"1": ["container", [{"name": "x", "type": "u8"}]]}, "default": "void"}],
           "particle": ["container", [{"name": "id", "type": "varint"}, {"name": "data", "type": ["particleData", {"compareTo": "id"}]}]]"#,
        r#"["container", [{"name": "p", "type": "particle"}]]"#,
    );
    let with_data = round_trip(&r, "01 07");
    assert_eq!(as_int(sub(sub(get(&with_data, "p"), "data"), "x")), 7);
    round_trip(&r, "02");
}

#[test]
fn an_inner_field_may_not_shadow_a_name_read_from_outside() {
    // `extra` switches on the outer `type`; the element also has a field called `type`.
    let types = "";
    let body = r#"["container", [
      {"name": "type", "type": "varint"},
      {"name": "items", "type": ["array", {"countType": "u8", "type": ["container", [
        {"name": "type", "type": "u8"},
        {"name": "extra", "type": ["switch", {"compareTo": "../type", "fields": {"1": "u8"}, "default": "void"}]}]]}]}]]"#;
    let source = generate(types, body).unwrap();
    let r = resolve(&source);
    // outer type 1, one element: its own type 9, then the arm for the OUTER type 1.
    let v = round_trip(&r, "01 01 09 0A");
    let Value::List(items) = get(&v, "items") else {
        panic!("items is not a list")
    };
    assert_eq!(items.len(), 1);
    assert_eq!(as_int(sub(&items[0], "extra")), 10);
}

#[test]
fn a_reference_that_resolves_nowhere_is_refused() {
    let err = generate("", r#"["container", [{"name": "s", "type": ["switch", {"compareTo": "missing", "fields": {"1": "u8"}}]}]]"#)
        .unwrap_err();
    assert!(err.contains("no field named"), "{err}");
}

#[test]
fn the_lint_reports_an_identifier_nothing_binds() {
    let schema = parse("packet p { a: u8\n match b { 0 => { c: u8 } } }").unwrap();
    let problems = lint(&schema);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("\"b\""), "{problems:?}");
    // The same shape with the tag declared first is clean.
    let ok = parse("packet p { b: u8\n match b { 0 => { c: u8 } }\n d: u8 if c == 1 }").unwrap();
    assert_eq!(lint(&ok), Vec::<String>::new());
}

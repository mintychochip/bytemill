//! Derived count/presence pairing: a list's count and an `Option`'s flag are
//! not hand fields; the renderer writes them from the hand field.

mod common;

use bytemill::emit_rstom::packet_body;
use bytemill::minestom::build_units;
use bytemill::shapes::ledger::{mismatch_fields, pair_body_fields};
use bytemill::shapes::model::{FieldShape, SerializerShape, Shape};
use bytemill::tree_packet::tree_packet;
use std::path::PathBuf;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bml-derived-{tag}-{}-{:?}",
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
fn count_and_flag_are_derived_from_hand_fields() {
    let body = "packet probe {\n\
        ids_count: varint\n\
        ids: varint[] count = ids_count\n\
        has_name: boolean\n\
        name: u8 if has_name\n\
    }";
    let root = common::repo_root();
    let source = common::rstom_tables::probe_source(&root, body);
    let dir = temp_dir("pair");
    let schema = dir.join("probe.bml");
    std::fs::write(&schema, source).unwrap();
    let resolved = bytemill::analyze(&bytemill::load_schema(&[schema]).unwrap()).unwrap();
    let table = common::rstom_tables::probe_table();
    let unit = build_units(&resolved, &table)
        .unwrap()
        .into_iter()
        .find(|u| u.pkt.name == "probe")
        .unwrap();
    let fields = packet_body(&unit, &resolved).unwrap();

    let shape = Shape {
        cls: "ProbePacket".into(),
        module: "server/play/probe_packet".into(),
        form: "braced".into(),
        fields: vec![
            FieldShape {
                name: "ids".into(),
                type_: "Vec<i32>".into(),
                ..Default::default()
            },
            FieldShape {
                name: "name".into(),
                type_: "Option<i8>".into(),
                ..Default::default()
            },
        ],
        serializer: SerializerShape {
            style: "struct".into(),
            name: "ProbeSerializer".into(),
            ..Default::default()
        },
        ..Default::default()
    };

    let plan = pair_body_fields(&shape, &fields.fields).expect("derived fields pair");
    assert_eq!(plan.derived.len(), 2, "{plan:?}");
    assert!(mismatch_fields(&shape, &fields.fields).is_empty());

    let text = tree_packet(&unit, &shape, &fields).unwrap();
    assert!(
        text.contains("buffer.write(VAR_INT, value.ids.len() as i32);"),
        "{text}"
    );
    assert!(
        text.contains("buffer.write(BOOLEAN, value.name.is_some());"),
        "{text}"
    );
    assert!(text.contains("if value.name.is_some() {"), "{text}");
    assert!(
        text.contains("let ids_count = buffer.read(VAR_INT);"),
        "{text}"
    );
    assert!(
        text.contains("let has_name = buffer.read(BOOLEAN);"),
        "{text}"
    );
    assert!(
        text.contains("Box::new(ProbePacket {\n            ids,\n            name,\n        })"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_plain_int_is_not_silently_bound_to_a_count_field() {
    let body = "packet probe {\n\
        ids_count: varint\n\
        ids: varint[] count = ids_count\n\
    }";
    let root = common::repo_root();
    let source = common::rstom_tables::probe_source(&root, body);
    let dir = temp_dir("ambiguous");
    let schema = dir.join("probe.bml");
    std::fs::write(&schema, source).unwrap();
    let resolved = bytemill::analyze(&bytemill::load_schema(&[schema]).unwrap()).unwrap();
    let table = common::rstom_tables::probe_table();
    let unit = build_units(&resolved, &table)
        .unwrap()
        .into_iter()
        .find(|u| u.pkt.name == "probe")
        .unwrap();
    let fields = packet_body(&unit, &resolved).unwrap();

    let shape = Shape {
        cls: "ProbePacket".into(),
        module: "server/play/probe_packet".into(),
        form: "braced".into(),
        fields: vec![
            FieldShape {
                name: "size".into(),
                type_: "i32".into(),
                ..Default::default()
            },
            FieldShape {
                name: "ids".into(),
                type_: "Vec<i32>".into(),
                ..Default::default()
            },
        ],
        serializer: SerializerShape {
            style: "struct".into(),
            name: "ProbeSerializer".into(),
            ..Default::default()
        },
        ..Default::default()
    };

    assert!(
        pair_body_fields(&shape, &fields.fields).is_none(),
        "an unrelated i32 must not bind to ids_count"
    );
    assert!(!mismatch_fields(&shape, &fields.fields).is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

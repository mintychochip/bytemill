//! The explicit compiler phases stay inspectable and agree on names.

use bytemill::ast::Schema;
use bytemill::errors::ErrorKind;
use bytemill::ir::{stages, stages_json};
use bytemill::json::{dumps, loads};
use bytemill::parse;
use bytemill::value::Value;
use std::process::Command;

fn schema_source() -> String {
    format!(
        "{}\n\
         packet ping {{\n\
             value: varint\n\
         }}\n\
         protocol demo @ v1 {{\n\
             frame default {{\n\
                 length: varint\n\
                 id: varint\n\
                 payload: rest\n\
             }}\n\
             route play.clientbound 0x01 => ping\n\
         }}\n",
        include_str!("../../../schemas/std/varint.bps")
    )
}

fn schema() -> Schema {
    parse(&schema_source()).expect("test schema parses")
}

#[test]
fn phases_expose_syntax_semantics_lowered_and_typed() {
    let report = stages(&schema()).expect("stages lower and type");

    assert!(report.syntax.imports.is_empty());
    assert!(
        report
            .syntax
            .declarations
            .iter()
            .any(|decl| decl.kind == "encoding" && decl.name == "varint")
    );
    assert!(
        report
            .syntax
            .declarations
            .iter()
            .any(|decl| decl.kind == "packet" && decl.name == "ping")
    );

    assert!(
        report
            .semantic
            .encodings
            .iter()
            .any(|name| name == "varint")
    );
    assert!(report.semantic.packets.iter().any(|name| name == "ping"));
    assert_eq!(report.semantic.routes.len(), 1);
    assert_eq!(report.semantic.routes[0].packet, "ping");

    assert!(
        report
            .lowered
            .types
            .iter()
            .any(|ty| ty.name == "varint" && ty.kind == "scalar")
    );
    assert!(
        report
            .lowered
            .decode
            .iter()
            .any(|proc_| proc_.name == "varint")
    );
    let frame = report
        .lowered
        .frames
        .iter()
        .find(|frame| frame.name == "default")
        .expect("default frame");
    assert_eq!(frame.type_id, "frame_default");
    assert_eq!(frame.decode, "frame_default");
    assert_eq!(frame.encode, "frame_default");
    assert_eq!(report.lowered.routes[0].frame.as_deref(), Some("default"));

    assert!(
        report
            .typed
            .types
            .iter()
            .any(|ty| ty.name == "ping" && ty.kind == "record")
    );
    let ping = report
        .typed
        .codecs
        .iter()
        .find(|codec| codec.name == "ping")
        .expect("ping codec");
    assert_eq!(ping.kind, "record");
    assert!(ping.statements > 0);
    assert_eq!(report.typed.routes[0].packet, "ping");
}

#[test]
fn stages_json_keeps_every_phase() {
    let json = dumps(&stages_json(&stages(&schema()).expect("stages")));
    let value = loads(&json).expect("stage JSON parses");

    let syntax = value.get("syntax").expect("syntax phase");
    let declarations = match syntax.get("declarations") {
        Some(Value::List(items)) => items,
        other => panic!("declarations is not a list: {other:?}"),
    };
    assert!(declarations.iter().any(|decl| matches!(
        decl.get("name"),
        Some(Value::Str(name)) if &**name == "ping"
    )));

    let semantic = value.get("semantic").expect("semantic phase");
    assert!(matches!(
        semantic.get("routes"),
        Some(Value::List(routes)) if routes.len() == 1
    ));

    let lowered = value.get("lowered").expect("lowered phase");
    assert!(matches!(
        lowered.get("frames"),
        Some(Value::List(frames)) if frames.iter().any(|frame| matches!(
            frame.get("type_id"),
            Some(Value::Str(name)) if &**name == "frame_default"
        ))
    ));

    let typed = value.get("typed").expect("typed phase");
    assert!(matches!(
        typed.get("codecs"),
        Some(Value::List(codecs)) if codecs.iter().any(|codec| matches!(
            codec.get("name"),
            Some(Value::Str(name)) if &**name == "ping"
        ))
    ));
}

#[test]
fn bml_stages_prints_every_phase() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Command::new(env!("CARGO_BIN_EXE_bml"))
        .args(["stages", "schemas/std/varint.bps"])
        .current_dir(root)
        .output()
        .expect("run bml stages");
    assert!(
        out.status.success(),
        "bml stages failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = loads(&String::from_utf8_lossy(&out.stdout)).expect("stage JSON");
    for phase in ["syntax", "semantic", "lowered", "typed"] {
        assert!(report.get(phase).is_some(), "missing phase {phase}");
    }
}

#[test]
fn stages_stop_at_the_failing_phase() {
    let schema = parse("packet broken { value: nowhere }").expect("parse");
    let error = stages(&schema).expect_err("unknown type must fail");
    assert_eq!(error.kind, ErrorKind::Resolve);
}

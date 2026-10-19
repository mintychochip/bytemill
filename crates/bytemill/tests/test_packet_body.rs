//! Ports tests/test_packet_body.py: the template-independent wire body seam.

mod common;

use bytemill::emit_rstom::packet_body;
use bytemill::minestom::build_units;

fn probe_unit(body: &str) -> (bytemill::minestom::Unit, bytemill::ResolvedSchema) {
    let source = format!(
        "import \"{}\"\n{}",
        common::repo_root().join("schemas/minecraft_core.bps").display(),
        common::rstom_tables::probe_source(&common::repo_root(), body)
    );
    let dir = std::env::temp_dir().join(format!("bps-body-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let schema = dir.join("probe.bps");
    std::fs::write(&schema, &source).unwrap();
    let resolved = bytemill::analyze(&bytemill::load_schema(&[schema]).unwrap()).unwrap();
    let units = build_units(&resolved, &common::rstom_tables::probe_table()).unwrap();
    let unit = units.into_iter().find(|u| u.pkt.name == "probe").unwrap();
    (unit, resolved)
}

#[test]
fn probe_packet_wire_body() {
    let (unit, resolved) = probe_unit("packet probe { a: i32be b: string c: u8 }");
    let body = packet_body(&unit, &resolved).unwrap();

    let idents: Vec<&str> = body.fields.iter().map(|f| f.ident.as_str()).collect();
    assert_eq!(idents, vec!["a", "b", "c"]);
    assert_eq!(body.field_types.get("a"), Some(&"i32".to_string()));
    assert_eq!(body.field_types.get("b"), Some(&"String".to_string()));
    assert_eq!(body.field_types.get("c"), Some(&"i8".to_string()));

    assert!(body.writes.contains("buffer.write(INT, value.a.clone());"), "{}", body.writes);
    assert!(body.writes.contains("buffer.write(STRING, value.b.clone());"), "{}", body.writes);
    assert!(body.writes.contains("buffer.write(BYTE, value.c.clone());"), "{}", body.writes);

    assert!(body.reads.contains("let a = buffer.read(INT);"), "{}", body.reads);
    assert!(body.reads.contains("let b = buffer.read(STRING);"), "{}", body.reads);
    assert!(body.reads.contains("let c = buffer.read(BYTE);"), "{}", body.reads);

    assert_eq!(body.ctor, "a: i32,\n        b: String,\n        c: i8");
    assert_eq!(body.init, "a,\n            b,\n            c");
    assert_eq!(body.used_wires, vec!["BYTE", "INT", "STRING"]);
    assert!(!body.needs_fixed_raw_bytes);
    let rtypes: std::collections::BTreeSet<&str> =
        body.rtypes.iter().map(|s| s.as_str()).collect();
    assert_eq!(rtypes, ["String", "i32", "i8"].into_iter().collect());
}

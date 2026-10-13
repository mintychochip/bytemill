//! Ports tests/test_imports.py: file-scoped names.

mod common;

use bps::errors::ErrorKind;
use bps::interp::decode_packet;
use bps::{analyze, load_schema, Value};

fn load_error(paths: &[std::path::PathBuf]) -> String {
    let err = load_schema(paths).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Resolve, "{err}");
    err.to_string()
}

#[test]
fn a_name_declared_in_another_file_needs_an_import() {
    let dir = common::temp_tree("imports");
    let lib = common::write_file(&dir, "lib.bps", "encoding ident = { value: u8 }\n");
    let main = common::write_file(&dir, "main.bps", "packet probe { x: ident }\n");
    let message = load_error(&[lib, main]);
    assert!(message.contains("main.bps"), "{message}");
    assert!(message.contains("ident"), "{message}");
    assert!(message.contains("import \"lib.bps\""), "{message}");
    common::cleanup(&dir);
}

#[test]
fn importing_the_file_makes_its_names_visible() {
    let dir = common::temp_tree("imports");
    common::write_file(&dir, "lib.bps", "encoding ident = { value: u8 }\n");
    let main = common::write_file(
        &dir,
        "main.bps",
        "import \"lib.bps\"\npacket probe { x: ident }\n",
    );
    let resolved = analyze(&load_schema(&[main]).unwrap()).unwrap();
    let dec = decode_packet(&resolved, "probe", &[0x07], false).unwrap();
    match dec.get("x") {
        Some(Value::Map(m)) => assert_eq!(m.get("value"), Some(&Value::Int(7))),
        other => panic!("expected nested map, got {other:?}"),
    }
    common::cleanup(&dir);
}

#[test]
fn imports_are_not_transitive() {
    let dir = common::temp_tree("imports");
    common::write_file(&dir, "c.bps", "encoding thing = { n: u8 }\n");
    common::write_file(
        &dir,
        "b.bps",
        "import \"c.bps\"\nencoding wrapped = { t: thing }\n",
    );
    let a = common::write_file(
        &dir,
        "a.bps",
        "import \"b.bps\"\npacket probe { w: wrapped  t: thing }\n",
    );
    let message = load_error(&[a]);
    assert!(message.contains("import \"c.bps\""), "{message}");
    assert!(!message.contains("wrapped"), "{message}");
    common::cleanup(&dir);
}

#[test]
fn the_import_to_add_is_relative_to_the_importing_file() {
    let dir = common::temp_tree("imports");
    let lib = common::write_file(&dir, "sub/lib.bps", "encoding ident = { value: u8 }\n");
    let user = common::write_file(&dir, "other/deep/user.bps", "packet probe { x: ident }\n");
    let message = load_error(&[lib, user]);
    assert!(
        message.contains("import \"../../sub/lib.bps\""),
        "{message}"
    );
    common::cleanup(&dir);
}

#[test]
fn every_missing_import_is_reported_at_once() {
    let dir = common::temp_tree("imports");
    let one = common::write_file(&dir, "one.bps", "encoding aa = { n: u8 }\n");
    let two = common::write_file(&dir, "two.bps", "encoding bb = { n: u8 }\n");
    let user = common::write_file(&dir, "user.bps", "packet probe { a: aa  b: bb }\n");
    let message = load_error(&[one, two, user]);
    assert!(message.contains("import \"one.bps\""), "{message}");
    assert!(message.contains("import \"two.bps\""), "{message}");
    common::cleanup(&dir);
}

#[test]
fn a_name_declared_nowhere_is_not_blamed_on_a_missing_import() {
    let dir = common::temp_tree("imports");
    let main = common::write_file(&dir, "main.bps", "packet probe { x: ghost }\n");
    let message = load_error(&[main]);
    assert!(message.contains("ghost"), "{message}");
    assert!(!message.contains("import \""), "{message}");
    common::cleanup(&dir);
}

#[test]
fn a_nearby_file_that_nothing_loaded_is_still_suggested() {
    let dir = common::temp_tree("imports");
    common::write_file(&dir, "std/num.bps", "encoding num = u8\n");
    let main = common::write_file(&dir, "main.bps", "packet probe { x: num }\n");
    let message = load_error(&[main]);
    assert!(message.contains("import \"std/num.bps\""), "{message}");
    common::cleanup(&dir);
}

#[test]
fn an_unreadable_neighbor_does_not_hide_the_real_error() {
    let dir = common::temp_tree("imports");
    common::write_file(&dir, "broken.bps", "this is not a schema\n");
    let main = common::write_file(&dir, "main.bps", "packet probe { x: ghost }\n");
    let message = load_error(&[main]);
    assert!(message.contains("ghost"), "{message}");
    common::cleanup(&dir);
}

#[test]
fn a_route_needs_the_packet_file_imported() {
    let dir = common::temp_tree("imports");
    let packets = common::write_file(&dir, "packets.bps", "packet ping { }\n");
    let routes = common::write_file(
        &dir,
        "routes.bps",
        "protocol p @ v1 {\n    id = 1\n    route play.clientbound 0x00 => ping\n}\n",
    );
    let message = load_error(&[packets.clone(), routes.clone()]);
    assert!(message.contains("import \"packets.bps\""), "{message}");
    let text = std::fs::read_to_string(&routes).unwrap();
    std::fs::write(&routes, format!("import \"packets.bps\"\n{text}")).unwrap();
    let resolved = analyze(&load_schema(&[routes]).unwrap()).unwrap();
    let names: Vec<String> = resolved.routes.iter().map(|(_, p)| p.name.clone()).collect();
    assert_eq!(names, vec!["ping"]);
    common::cleanup(&dir);
}

#[test]
fn a_frame_field_needs_its_type_imported() {
    let dir = common::temp_tree("imports");
    let lib = common::write_file(&dir, "lib.bps", "encoding num = u8\n");
    let proto = common::write_file(
        &dir,
        "proto.bps",
        "protocol p @ v1 {\n    id = 1\n    frame default {\n        length: num\n        payload: rest\n    }\n}\n",
    );
    let message = load_error(&[lib, proto]);
    assert!(message.contains("import \"lib.bps\""), "{message}");
    common::cleanup(&dir);
}

#[test]
fn an_enum_base_needs_its_type_imported() {
    let dir = common::temp_tree("imports");
    let lib = common::write_file(&dir, "lib.bps", "encoding num = u8\n");
    let enum_file = common::write_file(&dir, "kinds.bps", "enum kind : num {\n    a = 1\n}\n");
    let message = load_error(&[lib, enum_file]);
    assert!(message.contains("import \"lib.bps\""), "{message}");
    common::cleanup(&dir);
}

#[test]
fn a_spread_needs_its_encoding_imported() {
    let dir = common::temp_tree("imports");
    let lib = common::write_file(&dir, "lib.bps", "encoding ident = { value: u8 }\n");
    let user = common::write_file(
        &dir,
        "user.bps",
        "encoding wrapper = {\n    ...ident\n}\n",
    );
    let message = load_error(&[lib, user]);
    assert!(message.contains("import \"lib.bps\""), "{message}");
    common::cleanup(&dir);
}

#[test]
fn a_missing_import_target_names_the_file_that_asked_for_it() {
    let dir = common::temp_tree("imports");
    let main = common::write_file(&dir, "main.bps", "import \"nope.bps\"\n");
    let err = load_schema(&[main]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Parse, "{err}");
    let text = err.to_string();
    assert!(text.contains("nope.bps"), "{text}");
    assert!(text.contains("main.bps"), "{text}");
    common::cleanup(&dir);
}

#[test]
fn unknown_types_are_rejected_wherever_they_appear() {
    let sources = [
        ("packet field", "packet probe { x: ghost }"),
        ("enum base", "enum kind : ghost { a = 1 }"),
        (
            "frame field",
            "protocol p @ v1 {\n id = 1\n frame default { id: ghost }\n}",
        ),
    ];
    for (where_, source) in sources {
        let schema = bps::parse(source).unwrap();
        let err = analyze(&schema).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Resolve, "{where_}: {err}");
    }
}

#[test]
fn each_shipped_schema_loads_on_its_own() {
    let mut paths = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "bps") {
                out.push(path);
            }
        }
    }
    walk(&common::repo_root().join("schemas"), &mut paths);
    paths.sort();
    for path in paths {
        let schema = load_schema(&[path.clone()]).unwrap_or_else(|e| {
            panic!("{}: {e}", path.display());
        });
        analyze(&schema).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
}

#[test]
fn the_route_file_is_one_root_for_the_whole_protocol() {
    let resolved = common::load_minecraft().unwrap();
    assert_eq!(resolved.routes.len(), 261);
    assert_eq!(resolved.packets.len(), 232);
    assert!(resolved.encodings.contains_key("varint"));
}

#[test]
fn generated_route_file_loads_with_the_imports_it_is_given() {
    let dir = common::temp_tree("routes");
    std::fs::create_dir_all(dir.join("std")).unwrap();
    std::fs::copy(common::schema_path("std/varint.bps"), dir.join("std/varint.bps")).unwrap();
    common::write_file(&dir, "packets.bps", "packet handshake { }\n");
    let rows = [bps::wiki_routes::RouteRow {
        state: "handshaking".into(),
        direction: "serverbound".into(),
        packet_id: 0,
        official_name: "intention".into(),
    }];
    let text = bps::wiki_routes::emit_bps(
        &rows,
        &[],
        &std::collections::BTreeSet::new(),
        &["std/varint.bps".to_string(), "packets.bps".to_string()],
    );
    let routes = common::write_file(&dir, "routes.bps", &text);
    let resolved = analyze(&load_schema(&[routes]).unwrap()).unwrap();
    let names: Vec<&str> = resolved.routes.iter().map(|(_, pkt)| pkt.name.as_str()).collect();
    assert_eq!(names, ["handshake"]);
    common::cleanup(&dir);
}

#[test]
fn a_path_outside_the_cwd_prints_as_one_absolute_path() {
    // `Path.as_posix()` of an absolute path has a single leading slash.
    let dir = common::temp_tree("abspath");
    let main = common::write_file(&dir, "main.bps", "import \"missing.bps\"\n");
    let message = load_schema(&[main]).unwrap_err().to_string();
    let dir = dir.canonicalize().unwrap();
    let want = format!(
        "schema not found: {0}/missing.bps (imported by {0}/main.bps)",
        dir.display()
    );
    assert_eq!(message, want);
    common::cleanup(&dir);
}

#[test]
fn a_nearby_file_declaring_a_routed_packet_is_suggested() {
    // Nothing loaded `packets.bps`, so the nearby search finds the route's packet, and it must
    // look among packets (a packet is not a type) to name the import to add.
    let dir = common::temp_tree("imports");
    common::write_file(&dir, "packets.bps", "packet ping { }\n");
    let routes = common::write_file(
        &dir,
        "routes.bps",
        "protocol p @ v1 {\n    id = 1\n    route play.clientbound 0x00 => ping\n}\n",
    );
    let message = load_error(&[routes]);
    assert!(message.ends_with("  add  import \"packets.bps\"  for ping"), "{message}");
    common::cleanup(&dir);
}

#[test]
fn the_same_name_in_two_nearby_files_lists_them_in_path_order() {
    // Created in reverse order: a directory listing is unlikely to come out sorted by itself.
    let dir = common::temp_tree("imports");
    common::write_file(&dir, "b.bps", "encoding thing = u8\n");
    common::write_file(&dir, "a.bps", "encoding thing = u8\n");
    let main = common::write_file(&dir, "main.bps", "packet probe { x: thing }\n");
    let message = load_error(&[main]);
    let lines: Vec<&str> = message.lines().collect();
    assert_eq!(
        lines[1..],
        ["  add  import \"a.bps\"  for thing", "  add  import \"b.bps\"  for thing"],
        "{message}"
    );
    common::cleanup(&dir);
}

#[test]
fn a_hidden_nearby_file_is_still_suggested() {
    // pathlib's glob matches dotfiles, unlike `glob.glob`.
    let dir = common::temp_tree("imports");
    common::write_file(&dir, ".hidden.bps", "encoding thing = u8\n");
    let main = common::write_file(&dir, "main.bps", "packet probe { x: thing }\n");
    let message = load_error(&[main]);
    assert!(message.ends_with("  add  import \".hidden.bps\"  for thing"), "{message}");
    common::cleanup(&dir);
}

#[test]
fn source_files_are_read_with_universal_newlines() {
    // Python's `read_text` turns `\r\n` into `\n`, so an error offset counts a CRLF as one
    // character: the `#` is character 7 of `// one\n# two`, where raw bytes would say 8.
    let dir = common::temp_tree("imports");
    let main = common::write_file(&dir, "main.bps", "// one\r\n# two\r\n");
    let message = load_schema(&[main]).unwrap_err().to_string();
    assert_eq!(message, "unexpected character '#' (offset 7)");
    common::cleanup(&dir);
}

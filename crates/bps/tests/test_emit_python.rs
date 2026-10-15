//! `emit python` produces a module whose varint matches the known bytes.

fn repo(rel: &str) -> String {
    let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel);
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn python_varint_roundtrip() {
    let source = repo("schemas/std/varint.bps") + "packet probe { n: varint }\n";
    let resolved = bps::analyze(&bps::parse(&source).unwrap()).unwrap();
    let dir = std::env::temp_dir().join(format!("bps-py-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    bps::emit_python::emit_python(&resolved, &dir).unwrap();
    std::fs::write(
        dir.join("check.py"),
        r#"import bpsgen
w = bpsgen.Writer()
bpsgen.encode_varint(w, 300)
assert bytes(w.buf) == bytes([0xAC, 0x02]), w.buf
v = bpsgen.decode_varint(bpsgen.Cursor(bytes(w.buf)))
assert v == 300, v
w = bpsgen.Writer()
bpsgen.encode_varint(w, -1)
assert bytes(w.buf) == bytes([0xFF, 0xFF, 0xFF, 0xFF, 0x0F]), list(w.buf)
v = bpsgen.decode_varint(bpsgen.Cursor(bytes(w.buf)))
assert v == -1, v
print("ok")
"#,
    )
    .unwrap();
    let out = std::process::Command::new("python3")
        .arg("check.py")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "python failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

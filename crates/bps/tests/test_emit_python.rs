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

#[test]
fn python_26_3_packets_roundtrip() {
    use bps::interp::encode_packet;
    use bps::synth::synthesize_packet;
    let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../"))
        .join("schemas/minecraft_26_3_wiki.bps");
    let resolved = bps::analyze(&bps::load_schema(&[path]).unwrap()).unwrap();
    let dir = std::env::temp_dir().join(format!("bps-py-all-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    bps::emit_python::emit_python(&resolved, &dir).unwrap();
    let generated = std::fs::read_to_string(dir.join("bpsgen.py")).unwrap();
    let mut body = String::from(
        "import bpsgen\nfails = []\ndef check(name, raw, dec, enc):\n    try:\n        v = dec(bpsgen.Cursor(raw))\n        w = bpsgen.Writer()\n        enc(w, v)\n        if bytes(w.buf) != raw:\n            fails.append(\"%s bytes got %s want %s\" % (name, list(w.buf), list(raw)))\n    except Exception as e:\n        fails.append(\"%s %s: %s\" % (name, type(e).__name__, e))\n",
    );
    let mut n = 0;
    for packet in resolved.packets.values() {
        let value = match synthesize_packet(&resolved, packet) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let bytes = match encode_packet(&resolved, &packet.name, &value) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let alt = format!("packet_{}", packet.name);
        let raw_name = if generated.contains(&format!("def decode_{alt}(")) {
            alt
        } else {
            packet.name.clone()
        };
        let pyname = raw_name.replace(':', "_");
        let list = bytes.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(",");
        body.push_str(&format!(
            "check({name:?}, bytes([{list}]), bpsgen.decode_{pyname}, bpsgen.encode_{pyname})\n",
            name = packet.name
        ));
        n += 1;
    }
    body.push_str("print(\"\\n\".join(fails[:30]))\nprint(\"count\", len(fails))\nraise SystemExit(1 if fails else 0)\n");
    assert!(n > 100, "only {n} packets");
    std::fs::write(dir.join("check.py"), body).unwrap();
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

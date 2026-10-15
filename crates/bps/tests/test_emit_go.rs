//! `emit go` produces a package `go test` can round-trip.

use std::process::Command;

fn repo(rel: &str) -> String {
    let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel);
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn go_varint_roundtrip() {
    let source = repo("schemas/std/varint.bps") + "packet probe { n: varint }\n";
    let resolved = bps::analyze(&bps::parse(&source).unwrap()).unwrap();
    let dir = std::env::temp_dir().join(format!("bps-go-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    bps::emit_go::emit_go(&resolved, &dir).unwrap();
    std::fs::write(
        dir.join("varint_test.go"),
        r#"package bpsgen

import "testing"

func TestVarint(t *testing.T) {
    w := &Writer{}
    if err := Encode_varint(w, 300); err != nil {
        t.Fatal(err)
    }
    if got, want := w.Buf, []byte{0xac, 0x02}; string(got) != string(want) {
        t.Fatalf("300: %x", got)
    }
    v, err := Decode_varint(&Cursor{Data: w.Buf})
    if err != nil {
        t.Fatal(err)
    }
    if v != 300 {
        t.Fatalf("decode 300: %d", v)
    }
    w = &Writer{}
    if err := Encode_varint(w, -1); err != nil {
        t.Fatal(err)
    }
    if got, want := w.Buf, []byte{0xff, 0xff, 0xff, 0xff, 0x0f}; string(got) != string(want) {
        t.Fatalf("-1: %x", got)
    }
    v, err = Decode_varint(&Cursor{Data: w.Buf})
    if err != nil {
        t.Fatal(err)
    }
    if v != -1 {
        t.Fatalf("decode -1: %d", v)
    }
}
"#,
    )
    .unwrap();
    let out = Command::new("go")
        .args(["test"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "go test failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn go_26_3_packets_roundtrip() {
    use bps::interp::encode_packet;
    use bps::synth::synthesize_packet;
    let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../"))
        .join("schemas/minecraft_26_3_wiki.bps");
    let resolved = bps::analyze(&bps::load_schema(&[path]).unwrap()).unwrap();
    let dir = std::env::temp_dir().join(format!("bps-go-all-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    bps::emit_go::emit_go(&resolved, &dir).unwrap();
    let mut body = String::from(
        "package bpsgen\nimport \"bytes\"\nimport \"testing\"\nfunc TestPackets(t *testing.T) {\n",
    );
    let generated = std::fs::read_to_string(dir.join("packets.go")).unwrap();
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
        let goname = if generated.contains(&format!("func Decode_{alt}(")) {
            alt
        } else {
            packet.name.clone()
        };
        let list = bytes.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(",");
        let dec = format!("{} decode %v", packet.name);
        let enc = format!("{} encode %v", packet.name);
        let msg = format!("{} bytes", packet.name);
        body.push_str(&format!(
            "\tfunc() {{\n\t\traw := []byte{{{list}}}\n\t\tv, err := Decode_{goname}(&Cursor{{Data: raw}})\n\t\tif err != nil {{ t.Errorf({dec:?}, err); return }}\n\t\tw := &Writer{{}}\n\t\tif err = Encode_{goname}(w, v); err != nil {{ t.Errorf({enc:?}, err); return }}\n\t\tif !bytes.Equal(w.Buf, raw) {{ t.Errorf({msg:?}) }}\n\t}}()\n"
        ));
        n += 1;
    }
    body.push_str("}\n");
    assert!(n > 100, "only {n} packets");
    std::fs::write(dir.join("round_test.go"), body).unwrap();
    let out = Command::new("go").args(["test", "-count=1"]).current_dir(&dir).output().unwrap();
    assert!(
        out.status.success(),
        "go test failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

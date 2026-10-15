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

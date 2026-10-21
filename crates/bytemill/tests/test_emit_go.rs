//! `emit go` produces a gofmt-clean package whose codecs agree with the
//! interpreter byte-for-byte.

use std::path::{Path, PathBuf};
use std::process::Command;

use bytemill::GoVersion;
use bytemill::ir::typed::{CodecIdx, Program};

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

/// The codec for a schema packet, trying the packet, encoding, and
/// `packet:`-prefixed IR keys.
fn packet_codec(program: &Program, name: &str) -> Option<CodecIdx> {
    program
        .packet_codec(name)
        .or_else(|| program.codec_by_ir(name))
        .or_else(|| program.codec_by_ir(&format!("packet:{name}")))
}

fn run(dir: &Path, tool: &str, args: &[&str]) -> std::process::Output {
    Command::new(tool)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("{tool} not installed: {e}"))
}

/// Every `.go` file of an emitted package, concatenated, so assertions see the
/// whole tree.
fn generated(dir: &Path) -> String {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("go"))
        .collect();
    files.sort();
    let mut out = String::new();
    for file in files {
        out.push_str(&std::fs::read_to_string(&file).unwrap());
        out.push('\n');
    }
    out
}

/// The guide's local gates: gofmt-canonical output and a clean `go vet`.
fn assert_go_gates(dir: &Path) {
    let out = run(dir, "gofmt", &["-l", "."]);
    assert!(
        out.status.success(),
        "gofmt failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let flagged = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        flagged.trim().is_empty(),
        "gofmt would reformat:\n{flagged}"
    );
    let out = run(dir, "go", &["vet", "./..."]);
    assert!(
        out.status.success(),
        "go vet failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bps-go-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn go_unit_codecs() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + "packet probe { n: varint }\npacket floats { a: f32be, b: f64be }\npacket gated { v: varint\n require v == 777 }\npacket holder { n: varint\n ids: varint[] count = n }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let dir = scratch("unit");
    bytemill::emit_go::emit_go(&resolved, &dir).unwrap();
    assert_go_gates(&dir);
    let text = generated(&dir);
    assert!(text.contains("// Package bmlgen"), "missing package doc");
    assert!(text.contains("IDs []int32"), "initialism plural:\n{text}");
    assert!(
        text.contains("make([]int32, 0, int(n))"),
        "counted list must be preallocated:\n{text}"
    );
    std::fs::write(
        dir.join("unit_test.go"),
        r#"package bmlgen

import "errors"
import "testing"

func TestVarint(t *testing.T) {
	w := &Writer{}
	if err := EncodeVarint(w, 300); err != nil {
		t.Fatal(err)
	}
	if got, want := w.Bytes(), []byte{0xac, 0x02}; string(got) != string(want) {
		t.Fatalf("300: %x", got)
	}
	v, err := DecodeVarint(NewReader(w.Bytes()))
	if err != nil {
		t.Fatal(err)
	}
	if v != 300 {
		t.Fatalf("decode 300: %d", v)
	}
	w = &Writer{}
	if err := EncodeVarint(w, -1); err != nil {
		t.Fatal(err)
	}
	if got, want := w.Bytes(), []byte{0xff, 0xff, 0xff, 0xff, 0x0f}; string(got) != string(want) {
		t.Fatalf("-1: %x", got)
	}
	v, err = DecodeVarint(NewReader(w.Bytes()))
	if err != nil {
		t.Fatal(err)
	}
	if v != -1 {
		t.Fatalf("decode -1: %d", v)
	}
}

func TestFloats(t *testing.T) {
	w := &Writer{}
	if err := EncodeFloats(w, Floats{A: 1.5, B: -2.25}); err != nil {
		t.Fatal(err)
	}
	want := []byte{0x3f, 0xc0, 0x00, 0x00, 0xc0, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00}
	if got := w.Bytes(); string(got) != string(want) {
		t.Fatalf("floats: %x", got)
	}
	v, err := DecodeFloats(NewReader(w.Bytes()))
	if err != nil {
		t.Fatal(err)
	}
	if v.A != 1.5 || v.B != -2.25 {
		t.Fatalf("decode floats: %+v", v)
	}
}

func TestShortBuffer(t *testing.T) {
	_, err := DecodeProbe(NewReader([]byte{0xac}))
	if !errors.Is(err, ErrShortBuffer) {
		t.Fatalf("expected short buffer, got %v", err)
	}
}

func TestRequire(t *testing.T) {
	_, err := DecodeGated(NewReader([]byte{0x8a, 0x06}))
	if !errors.Is(err, ErrRequire) {
		t.Fatalf("expected require failure, got %v", err)
	}
	v, err := DecodeGated(NewReader([]byte{0x89, 0x06}))
	if err != nil {
		t.Fatal(err)
	}
	if v.V != 777 {
		t.Fatalf("decode gated: %+v", v)
	}
}
"#,
    )
    .unwrap();
    let out = run(&dir, "go", &["test", "-count=1", "."]);
    assert!(
        out.status.success(),
        "go test failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Round-trip every synthesizable packet of `schema_rel`: interpreter bytes
/// into generated decode, back through generated encode, byte-compare.
fn roundtrip_schema(schema_rel: &str, tag: &str, min_packets: usize) -> usize {
    let resolved = bytemill::analyze(&bytemill::load_schema(&[repo(schema_rel)]).unwrap()).unwrap();
    let program = bytemill::ir::typed(&resolved).unwrap();
    let dir = scratch(tag);
    bytemill::emit_go::emit_go(&resolved, &dir).unwrap();
    assert!(
        !dir.join("packets.go").is_file(),
        "packets.go should be split by route"
    );
    assert_go_gates(&dir);
    let mut body = String::from(
        "package bmlgen\n\nimport (\n\t\"bytes\"\n\t\"testing\"\n)\n\nfunc TestPackets(t *testing.T) {\n",
    );
    let mut n = 0;
    for packet in resolved.packets.values() {
        let Some(codec) = packet_codec(&program, &packet.name) else {
            continue;
        };
        let value = match bytemill::synth::synthesize_packet(&resolved, packet) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let bytes = match bytemill::interp::encode_packet(&resolved, &packet.name, &value) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let decode = bytemill::emit_go::decode_fn(&program, codec);
        let encode = bytemill::emit_go::encode_fn(&program, codec);
        let list = bytes
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let dec = format!("{} decode %v", packet.name);
        let enc = format!("{} encode %v", packet.name);
        let msg = format!("{} bytes differ", packet.name);
        body.push_str(&format!(
            "\tfunc() {{\n\t\traw := []byte{{{list}}}\n\t\tv, err := {decode}(NewReader(raw))\n\t\tif err != nil {{ t.Errorf({dec:?}, err); return }}\n\t\tw := &Writer{{}}\n\t\tif err = {encode}(w, v); err != nil {{ t.Errorf({enc:?}, err); return }}\n\t\tif !bytes.Equal(w.Bytes(), raw) {{ t.Errorf({msg:?}) }}\n\t}}()\n"
        ));
        n += 1;
    }
    body.push_str("}\n");
    assert!(
        n >= min_packets,
        "only {n} packets round-tripped for {schema_rel}"
    );
    if schema_rel.contains("26_3") {
        assert!(
            dir.join("play_clientbound.go").is_file(),
            "missing play_clientbound.go"
        );
        assert!(dir.join("encoding.go").is_file(), "missing encoding.go");
        assert!(dir.join("shared.go").is_file(), "missing shared.go");
    }
    std::fs::write(dir.join("round_test.go"), body).unwrap();
    let out = run(&dir, "go", &["test", "-count=1", "."]);
    assert!(
        out.status.success(),
        "go test failed for {schema_rel}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    n
}

#[test]
fn go_26_3_packets_roundtrip() {
    roundtrip_schema("schemas/minecraft_26_3_wiki.bps", "26-3", 100);
}

#[test]
fn go_v1_20_1_packets_roundtrip() {
    roundtrip_schema("schemas/versions/v1_20_1.bps", "v1-20-1", 100);
}

#[test]
fn go_hytale_packets_roundtrip() {
    roundtrip_schema("schemas/hytale/2026_02_17.bps", "hytale", 1);
}

/// Emit a package containing a counted list under `ver`, returning its scratch dir.
fn emit_versioned(ver: GoVersion, tag: &str) -> PathBuf {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + "packet probe { n: varint }\npacket holder { n: varint\n ids: varint[] count = n }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let dir = scratch(&format!("ver-{tag}"));
    bytemill::emit_go::emit_go_with_version(&resolved, &dir, ver).unwrap();
    dir
}

#[test]
fn go_version_go_mod_directives() {
    for (ver, directive) in [
        (GoVersion::V1_18, "go 1.18"),
        (GoVersion::V1_21, "go 1.21"),
        (GoVersion::V1_23, "go 1.23"),
    ] {
        let dir = emit_versioned(ver, &format!("mod-{}", ver.mod_str()));
        let gomod = std::fs::read_to_string(dir.join("go.mod")).unwrap();
        assert!(
            gomod.contains(directive),
            "go.mod for {ver} should contain {directive:?}:\n{gomod}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn go_version_counted_loop_form() {
    let dir = emit_versioned(GoVersion::V1_18, "loop-1-18");
    let text = generated(&dir);
    assert!(
        text.contains("for i := 0; i < int(n); i++"),
        "Go 1.18 cannot range over an integer:\n{text}"
    );
    assert!(
        !text.contains("for range"),
        "no integer range on Go 1.18:\n{text}"
    );
    assert!(
        text.contains("through any layer of wrapping"),
        "comments must survive the pre-1.18 any downgrade:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let dir = emit_versioned(GoVersion::V1_23, "loop-1-23");
    let text = generated(&dir);
    assert!(
        text.contains("for range n"),
        "Go 1.23 keeps the integer range:\n{text}"
    );
    assert!(
        !text.contains("i < int(n); i++"),
        "no legacy loop on Go 1.23:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn go_version_vet_old_and_new() {
    for (ver, tag) in [
        (GoVersion::V1_18, "vet-1-18"),
        (GoVersion::V1_23, "vet-1-23"),
    ] {
        let dir = emit_versioned(ver, tag);
        assert_go_gates(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

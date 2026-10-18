//! `emit python` on the typed program: typed dataclasses round-trip bytes.

use std::path::PathBuf;
use std::process::Command;

fn repo(rel: &str) -> PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bps-py-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn py_compile(dir: &std::path::Path) {
    let out = Command::new("python3")
        .args(["-m", "compileall", "-q", "bpsgen"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "py_compile failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_check(dir: &std::path::Path) {
    let out = Command::new("python3")
        .arg("check.py")
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "check.py failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn python_varint_values() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + "packet probe { n: varint }\npacket probe_guarded { n: varint require n == 300 }\n";
    let resolved = bps::analyze(&bps::parse(&source).unwrap()).unwrap();
    let program = bps::ir::typed::typed(&resolved).unwrap();
    let dir = scratch("values");
    bps::emit_python::emit_python(&resolved, &dir).unwrap();
    py_compile(&dir);

    let varint = program.codec_by_ir("varint").unwrap();
    let varint_dec = bps::emit_python::decode_fn(&program, varint);
    let varint_enc = bps::emit_python::encode_fn(&program, varint);
    assert_eq!(varint_dec, "decode_varint");
    assert_eq!(varint_enc, "encode_varint");
    let probe = program.packet_codec("probe").unwrap();
    let probe_dec = bps::emit_python::decode_fn(&program, probe);
    assert_eq!(probe_dec, "Probe.decode");
    let guarded = program.packet_codec("probe_guarded").unwrap();
    let guarded_dec = bps::emit_python::decode_fn(&program, guarded);
    assert_eq!(guarded_dec, "ProbeGuarded.decode");

    std::fs::write(
        dir.join("check.py"),
        r#"import struct

import bpsgen


def eq(got, want, msg):
    assert got == want, f"{msg}: {got!r} != {want!r}"


# varint 300 <-> ac 02
r = bpsgen.Reader(bytes([0xAC, 0x02]))
eq(bpsgen.decode_varint(r), 300, "decode 300")
eq(r.pos, 2, "pos after 300")
w = bpsgen.Writer()
bpsgen.encode_varint(w, 300)
eq(w.get_bytes(), bytes([0xAC, 0x02]), "encode 300")
# varint -1 <-> ff ff ff ff 0f
r = bpsgen.Reader(bytes([0xFF, 0xFF, 0xFF, 0xFF, 0x0F]))
eq(bpsgen.decode_varint(r), -1, "decode -1")
w = bpsgen.Writer()
bpsgen.encode_varint(w, -1)
eq(w.get_bytes(), bytes([0xFF, 0xFF, 0xFF, 0xFF, 0x0F]), "encode -1")
# probe packet round trip
v = bpsgen.Probe.decode(bpsgen.Reader(bytes([0xAC, 0x02])))
eq(v.n, 300, "probe field")
w = bpsgen.Writer()
v.encode(w)
eq(w.get_bytes(), bytes([0xAC, 0x02]), "probe bytes")
# floats through the runtime
r = bpsgen.Reader(struct.pack(">f", 1.5))
eq(r.read_f32(), 1.5, "f32")
r = bpsgen.Reader(struct.pack(">d", -2.25))
eq(r.read_f64(), -2.25, "f64")
w = bpsgen.Writer()
w.write_f32(1.5)
w.write_f64(-2.25)
eq(w.get_bytes(), struct.pack(">f", 1.5) + struct.pack(">d", -2.25), "float bytes")
# a short buffer carries the offset and the inner-to-outer codec path
try:
    bpsgen.Probe.decode(bpsgen.Reader(b""))
    raise AssertionError("expected ShortBuffer")
except bpsgen.ShortBuffer as exc:
    assert "offset 0" in str(exc), f"offset missing: {exc}"
    assert "need 1, have 0" in str(exc), f"counts missing: {exc}"
    assert "[in varint <- probe]" in str(exc), f"codec path missing: {exc}"
# a require failure names the condition and the codec
try:
    bpsgen.ProbeGuarded.decode(bpsgen.Reader(bytes([0x00])))
    raise AssertionError("expected RequireFailed")
except bpsgen.RequireFailed as exc:
    assert "n == 300" in str(exc), f"condition missing: {exc}"
    assert "probe_guarded" in str(exc), f"codec path missing: {exc}"
w = bpsgen.Writer()
# the typed program checks requires on decode only: encode writes as-is
bpsgen.ProbeGuarded(n=0).encode(w)
eq(w.get_bytes(), bytes([0x00]), "unguarded encode bytes")
print("values ok")
"#,
    )
    .unwrap();
    run_check(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Decode the interpreter's bytes with the generated code, encode again, and
/// compare: every synthesizable packet of one schema.
fn packets_roundtrip(schema_rel: &str, dir_name: &str, min_packets: usize) {
    use bps::interp::encode_packet;
    use bps::synth::synthesize_packet;
    let resolved = bps::analyze(&bps::load_schema(&[repo(schema_rel)]).unwrap()).unwrap();
    let program = bps::ir::typed::typed(&resolved).unwrap();
    let dir = scratch(dir_name);
    bps::emit_python::emit_python(&resolved, &dir).unwrap();
    py_compile(&dir);
    let mut body = String::from(
        "import bpsgen\nfails = []\n\n\ndef check(name, raw, cls):\n    try:\n        value = cls.decode(bpsgen.Reader(raw))\n    except Exception as e:\n        fails.append(f\"{name} decode {type(e).__name__}: {e}\")\n        return\n    try:\n        w = bpsgen.Writer()\n        value.encode(w)\n    except Exception as e:\n        fails.append(f\"{name} encode {type(e).__name__}: {e}\")\n        return\n    if w.get_bytes() != raw:\n        fails.append(f\"{name} bytes got {w.get_bytes().hex()} want {raw.hex()}\")\n\n\n",
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
        let Some(codec) = program.packet_codec(&packet.name) else {
            continue;
        };
        let dec = bps::emit_python::decode_fn(&program, codec);
        let Some(cls) = dec.strip_suffix(".decode") else {
            panic!("packet {} decodes via {dec}, not a record", packet.name);
        };
        let list = bytes
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        body.push_str(&format!(
            "check({:?}, bytes([{list}]), bpsgen.{cls})\n",
            packet.name
        ));
        n += 1;
    }
    assert!(n >= min_packets, "only {n} packets");
    body.push_str(
        "print(\"\\n\".join(fails[:30]))\nprint(\"count\", len(fails))\nraise SystemExit(1 if fails else 0)\n",
    );
    std::fs::write(dir.join("check.py"), body).unwrap();
    run_check(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn python_26_3_packets_roundtrip() {
    packets_roundtrip("schemas/minecraft_26_3_wiki.bps", "26-3", 100);
}

#[test]
fn python_hytale_packets_roundtrip() {
    packets_roundtrip("schemas/hytale/2026_02_17.bps", "hytale", 1);
}

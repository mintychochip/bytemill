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
        + "enum intent : varint { status = 1 login = 2 }\npacket probe { n: varint }\npacket probe_guarded { n: varint require n == 300 }\npacket routed { i: intent }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let program = bytemill::ir::typed::typed(&resolved).unwrap();
    let dir = scratch("values");
    bytemill::emit_python::emit_python(&resolved, &dir).unwrap();
    py_compile(&dir);

    let varint = program.codec_by_ir("varint").unwrap();
    let varint_dec = bytemill::emit_python::decode_fn(&program, varint);
    let varint_enc = bytemill::emit_python::encode_fn(&program, varint);
    assert_eq!(varint_dec, "decode_varint");
    assert_eq!(varint_enc, "encode_varint");
    let probe = program.packet_codec("probe").unwrap();
    let probe_dec = bytemill::emit_python::decode_fn(&program, probe);
    assert_eq!(probe_dec, "Probe.decode");
    let guarded = program.packet_codec("probe_guarded").unwrap();
    let guarded_dec = bytemill::emit_python::decode_fn(&program, guarded);
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
# a failure carries the offset as data, not only inside the message
try:
    bpsgen.Probe.decode(bpsgen.Reader(b""))
    raise AssertionError("expected ShortBuffer")
except bpsgen.ShortBuffer as exc:
    eq(exc.offset, 0, "exc.offset")
# an open enum keeps an unknown value as a real member
unknown = bpsgen.Intent(7)
eq(int(unknown), 7, "unknown enum int")
eq(unknown.name, "UNKNOWN_7", "unknown enum name")
assert repr(unknown), "unknown enum repr"
eq(bpsgen.Intent(1), bpsgen.Intent.STATUS, "known enum member")
r = bpsgen.Reader(bytes([0x07]))
eq(int(bpsgen.decode_intent(r)), 7, "decode unknown enum")
w = bpsgen.Writer()
bpsgen.encode_intent(w, unknown)
eq(w.get_bytes(), bytes([0x07]), "encode unknown enum")
# the package surface is explicit: every public name is in __all__
missing = [
    name
    for name in ("Reader", "Writer", "BpsError", "Intent", "Probe", "Routed")
    if name not in bpsgen.__all__
]
assert not missing, f"__all__ is missing {missing}"
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
    use bytemill::interp::encode_packet;
    use bytemill::synth::synthesize_packet;
    let resolved = bytemill::analyze(&bytemill::load_schema(&[repo(schema_rel)]).unwrap()).unwrap();
    let program = bytemill::ir::typed::typed(&resolved).unwrap();
    let dir = scratch(dir_name);
    bytemill::emit_python::emit_python(&resolved, &dir).unwrap();
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
        let dec = bytemill::emit_python::decode_fn(&program, codec);
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

/// Version-gated emission: one schema exercising dataclasses, an open enum,
/// a sum union, `match` tag dispatch, and an optional field.
fn version_schema() -> bytemill::ResolvedSchema {
    let source = std::fs::read_to_string(repo("schemas/std/integers.bps")).unwrap()
        + &std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + "enum intent : varint { status = 1 login = 2 }\npacket probe { kind: u8 v: match kind { 0 => u8 1 | 2 => { a: u8, b: u8 } _ => boolean } mode: u8 if mode == 0 { extra: u8 } }\npacket routed { i: intent }\n";
    bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap()
}

fn py_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.join("bpsgen")];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "py") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn all_py(dir: &std::path::Path) -> String {
    let files = py_files(dir);
    assert!(!files.is_empty(), "no .py emitted in {}", dir.display());
    let mut all = String::new();
    for f in &files {
        all.push_str(&std::fs::read_to_string(f).unwrap());
        all.push('\n');
    }
    all
}

/// The emitted `if`/`elif`/`else` fallback must decode exactly what the
/// `match` version decodes: every tag arm, the default arm, and the
/// optional field both absent and present.
const VERSION_SMOKE: &str = r#"import bpsgen


def roundtrip(raw):
    value = bpsgen.Probe.decode(bpsgen.Reader(bytes(raw)))
    w = bpsgen.Writer()
    value.encode(w)
    assert w.get_bytes() == bytes(raw), (w.get_bytes(), bytes(raw))
    return value


# case 0: scalar payload, no optional field
v = roundtrip([0, 5, 1])
assert (v.kind, v.mode) == (0, 1), vars(v)
assert v.extra is None, vars(v)
# case 1 | 2: record payload
v = roundtrip([1, 9, 8, 1])
assert (v.v.a, v.v.b) == (9, 8), vars(v)
# default arm: boolean payload round-trips
roundtrip([7, 1, 1])
# optional field present
v = roundtrip([0, 5, 0, 7])
assert v.extra == 7, vars(v)
# the open enum still fabricates unknown members
assert bpsgen.Intent(7).name == "UNKNOWN_7"
print("version smoke ok")
"#;

#[test]
fn python_version_py38() {
    let resolved = version_schema();
    let dir = scratch("py38");
    bytemill::emit_python::emit_python_with_version(&resolved, &dir, bytemill::PythonVersion::Py38).unwrap();
    py_compile(&dir);
    let all = all_py(&dir);
    // No 3.10+ syntax: every `match` became `if`/`elif`/`else`.
    for line in all.lines() {
        let trimmed = line.trim_start();
        assert!(
            !trimmed.starts_with("match "),
            "py38 line uses match: {line}"
        );
        assert!(!trimmed.starts_with("case "), "py38 line uses case: {line}");
    }
    assert!(!all.contains("| None"), "py38 uses `| None`");
    assert!(!all.contains("slots=True"), "py38 uses slots");
    assert!(
        !all.contains("from typing import Self"),
        "py38 imports Self"
    );
    assert!(all.contains("@dataclasses.dataclass\n"), "py38 dataclass");
    assert!(all.contains("Optional["), "py38 Optional");
    assert!(all.contains("Union["), "py38 Union");
    assert!(all.contains("elif "), "py38 elif chain");
    assert!(all.contains("isinstance("), "py38 isinstance");
    std::fs::write(dir.join("check.py"), VERSION_SMOKE).unwrap();
    run_check(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn python_version_py311() {
    let resolved = version_schema();
    let dir = scratch("py311");
    bytemill::emit_python::emit_python_with_version(&resolved, &dir, bytemill::PythonVersion::Py311).unwrap();
    py_compile(&dir);
    let all = all_py(&dir);
    assert!(
        all.lines().any(|l| l.trim_start().starts_with("match ")),
        "py311 match"
    );
    assert!(
        all.lines().any(|l| l.trim_start().starts_with("case ")),
        "py311 case"
    );
    assert!(
        all.contains("slots=True, kw_only=True"),
        "py311 dataclass args"
    );
    assert!(all.contains("| None"), "py311 optional pipe");
    assert!(all.contains(" | "), "py311 union pipe");
    assert!(all.contains("from typing import Self"), "py311 Self import");
    std::fs::write(dir.join("check.py"), VERSION_SMOKE).unwrap();
    run_check(&dir);
    // The default emitter targets Python 3.11: byte-for-byte identical.
    let default_dir = scratch("py311-default");
    bytemill::emit_python::emit_python(&resolved, &default_dir).unwrap();
    let emitted = py_files(&dir);
    let baseline = py_files(&default_dir);
    assert_eq!(emitted.len(), baseline.len(), "file count drift");
    for (new, old) in emitted.iter().zip(baseline.iter()) {
        assert_eq!(new.file_name(), old.file_name(), "file name drift");
        assert_eq!(
            std::fs::read(new).unwrap(),
            std::fs::read(old).unwrap(),
            "drift in {}",
            new.display()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&default_dir);
}

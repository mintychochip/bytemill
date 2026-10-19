//! `emit typescript` produces a `bpsgen.ts` index over per-category modules
//! that node can round-trip.
//!
//! Function names resolve through the printer (`decode_fn`/`encode_fn`),
//! never by string munging in the test.

use bytemill::ir::typed::Program;
use std::path::Path;

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn node(dir: &Path, script: &str) -> std::process::Output {
    std::process::Command::new("node")
        .arg("--experimental-strip-types")
        .arg(script)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bps-ts-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Concatenated generated modules, so text assertions see the whole tree.
fn generated(dir: &Path) -> String {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("ts")
                && p.file_name().and_then(|n| n.to_str()) != Some("check.ts")
        })
        .collect();
    files.sort();
    let mut out = String::new();
    for file in files {
        out.push_str(&format!(
            "// ---- {} ----\n",
            file.file_name().unwrap().to_string_lossy()
        ));
        out.push_str(&std::fs::read_to_string(&file).unwrap());
        out.push('\n');
    }
    out
}

/// The public entry point re-exports every generated module.
fn index(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("bpsgen.ts")).unwrap()
}

fn check_ok(dir: &Path, main: &str) {
    let src = format!("import * as bps from \"./bpsgen.ts\";\n{main}");
    std::fs::write(dir.join("check.ts"), &src).unwrap();
    let out = node(dir, "check.ts");
    assert!(
        out.status.success(),
        "node failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ts_varint_roundtrip() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + "packet probe { n: varint }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let dir = scratch("varint");
    bytemill::emit_ts::emit_typescript(&resolved, &dir).unwrap();
    let src = generated(&dir);
    // 32-bit-or-narrower integers are plain `number`, not `bigint`.
    assert!(src.contains("export function decodeVarint(r: Reader): number"));
    assert!(
        index(&dir).contains("export * from \"./encoding.ts\";"),
        "bpsgen.ts must re-export the encoding module\n{}",
        index(&dir)
    );
    assert!(dir.join("encoding.ts").is_file(), "missing encoding.ts");
    check_ok(
        &dir,
        r#"
const w = new bps.Writer();
bps.encodeVarint(w, 300);
if (w.toBytes().join(",") !== "172,2") throw new Error("300 " + w.toBytes());
if (bps.decodeVarint(new bps.Reader(w.toBytes())) !== 300) throw new Error("dec 300");
const w2 = new bps.Writer();
bps.encodeVarint(w2, -1);
if (w2.toBytes().join(",") !== "255,255,255,255,15") throw new Error("-1 " + w2.toBytes());
if (bps.decodeVarint(new bps.Reader(w2.toBytes())) !== -1) throw new Error("dec -1");
console.log("ok");
"#,
    );
}

#[test]
fn ts_scalars_floats_and_strings() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + &std::fs::read_to_string(repo("schemas/std/integers.bps")).unwrap()
        + &std::fs::read_to_string(repo("schemas/minecraft_core.bps")).unwrap()
        + r#"
packet probe {
  a: boolean
  b: i8
  c: u16be
  d: f32be
  e: f64be
  f: string(10)
}
"#;
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let program = bytemill::ir::typed(&resolved).unwrap();
    let dir = scratch("scalars");
    bytemill::emit_ts::emit_typescript(&resolved, &dir).unwrap();
    let c = program.packet_codec("probe").unwrap();
    let (dec, enc) = (
        bytemill::emit_ts::decode_fn(&program, c),
        bytemill::emit_ts::encode_fn(&program, c),
    );
    check_ok(
        &dir,
        &format!(
            r#"
const v = {{ a: true, b: -5, c: 0xcafe, d: 1.5, e: -123.456, f: "héllo" }};
const w = new bps.Writer();
bps.{enc}(w, v);
const back = bps.{dec}(new bps.Reader(w.toBytes()));
const w2 = new bps.Writer();
bps.{enc}(w2, back);
if (w2.toBytes().join(",") !== w.toBytes().join(",")) throw new Error("re-encode");
if (back.a !== true || back.b !== -5 || back.c !== 0xcafe) throw new Error("ints " + JSON.stringify(back));
if (back.d !== 1.5 || back.e !== -123.456 || back.f !== "héllo") throw new Error("rest " + JSON.stringify(back));
console.log("ok");
"#
        ),
    );
}

#[test]
fn ts_short_buffer_and_require_failures() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + r#"
packet probe {
  n: varint
  require n == 300
}
"#;
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let program: Program = bytemill::ir::typed(&resolved).unwrap();
    let dir = scratch("errors");
    bytemill::emit_ts::emit_typescript(&resolved, &dir).unwrap();
    let c = program.packet_codec("probe").unwrap();
    let dec = bytemill::emit_ts::decode_fn(&program, c);
    check_ok(
        &dir,
        &format!(
            r#"
function kindOf(f) {{
  try {{
    f();
    return "no-throw";
  }} catch (e) {{
    return e instanceof bps.BpsError ? e.kind + "@" + e.codec : "foreign:" + e;
  }}
}}
if (kindOf(() => bps.decodeVarint(new bps.Reader(new Uint8Array(0)))) !== "shortBuffer@varint")
  throw new Error("empty varint");
if (kindOf(() => bps.decodeVarint(new bps.Reader(Uint8Array.from([0xac])))) !== "shortBuffer@varint")
  throw new Error("truncated varint");
const w = new bps.Writer();
bps.encodeVarint(w, 299);
if (kindOf(() => bps.{dec}(new bps.Reader(w.toBytes()))) !== "requireFailed@probe")
  throw new Error("require should fail");
const w2 = new bps.Writer();
bps.encodeVarint(w2, 300);
const v = bps.{dec}(new bps.Reader(w2.toBytes()));
if (v.n !== 300) throw new Error("require should pass");
console.log("ok");
"#
        ),
    );
}

/// Decode the interpreter's bytes with the generated decoder, encode again
/// with the generated encoder, and compare bytes exactly.
fn roundtrip_schema(schema_rel: &str, tag: &str, min_packets: usize) {
    use bytemill::interp::encode_packet;
    use bytemill::synth::synthesize_packet;
    let resolved = bytemill::analyze(&bytemill::load_schema(&[repo(schema_rel)]).unwrap()).unwrap();
    let program: Program = bytemill::ir::typed(&resolved).unwrap();
    let dir = scratch(tag);
    bytemill::emit_ts::emit_typescript(&resolved, &dir).unwrap();
    assert!(
        index(&dir).contains("export * from \"./encoding.ts\";"),
        "missing index re-export for {schema_rel}"
    );
    assert!(dir.join("encoding.ts").is_file(), "missing encoding.ts");
    if schema_rel.contains("26_3") {
        assert!(
            dir.join("play_clientbound.ts").is_file(),
            "missing play_clientbound.ts"
        );
        assert!(dir.join("shared.ts").is_file(), "missing shared.ts");
    }
    let mut body = String::from(
        "const fails: string[] = [];\nfunction check(name: string, raw: number[], v: unknown, enc: (w: bps.Writer, v: never) => void) {\n  try {\n    const w = new bps.Writer();\n    enc(w, v as never);\n    const back = w.toBytes();\n    if (back.length !== raw.length || back.some((b, i) => b !== raw[i])) fails.push(name + \" bytes \" + back.length + \" \" + raw.length);\n  } catch (e) { fails.push(name + \" \" + e); }\n}\n",
    );
    let mut n = 0;
    let mut packets: Vec<String> = resolved.packets.values().map(|p| p.name.clone()).collect();
    packets.sort();
    for name in &packets {
        let packet = resolved.packets.values().find(|p| &p.name == name).unwrap();
        let value = match synthesize_packet(&resolved, packet) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let bytes = match encode_packet(&resolved, &packet.name, &value) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let Some(c) = program.packet_codec(&packet.name) else {
            continue;
        };
        let (dec, enc) = (
            bytemill::emit_ts::decode_fn(&program, c),
            bytemill::emit_ts::encode_fn(&program, c),
        );
        let list = bytes
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(",");
        body.push_str(&format!(
            "check({name:?}, [{list}], bps.{dec}(new bps.Reader(Uint8Array.from([{list}]))), bps.{enc});\n",
            name = packet.name
        ));
        n += 1;
    }
    body.push_str(
        "if (fails.length) { console.log(fails.slice(0, 30).join(\"\\n\")); throw new Error(\"fail \" + fails.length); }\n",
    );
    body.push_str(&format!("console.log(\"packets\", {n});\n"));
    assert!(n >= min_packets, "only {n} packets for {schema_rel}");
    std::fs::write(
        dir.join("check.ts"),
        format!("import * as bps from \"./bpsgen.ts\";\n{body}"),
    )
    .unwrap();
    let out = node(&dir, "check.ts");
    assert!(
        out.status.success(),
        "node failed for {schema_rel}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ts_26_3_packets_roundtrip() {
    roundtrip_schema("schemas/minecraft_26_3_wiki.bps", "all", 100);
}

/// Interface and import idioms a reviewer would rewrite:
/// records are `readonly` (the codec never mutates a decoded value), the
/// catch-all arm's payload is required so encode skips its `!` assertion,
/// and an open enum's `const` member keys never drag in a type-only import.
#[test]
fn ts_type_and_import_idioms() {
    let resolved =
        bytemill::analyze(&bytemill::load_schema(&[repo("schemas/minecraft_26_3_wiki.bps")]).unwrap())
            .unwrap();
    let dir = scratch("idioms");
    bytemill::emit_ts::emit_typescript(&resolved, &dir).unwrap();
    let src = generated(&dir);
    let enc = std::fs::read_to_string(dir.join("encoding.ts")).unwrap();
    // Record/union fields are immutable.
    assert!(
        src.contains("export interface ParticleData {\n  readonly particleType: number;"),
        "record fields must be readonly"
    );
    assert!(
        src.contains("  readonly kind: \"other\";\n  readonly tag: number;"),
        "catch-all arm discriminant must be readonly"
    );
    // The catch-all payload is always decoded, so encode reads it directly.
    assert!(
        enc.contains("readonly value: NbtNothing;"),
        "catch-all arm payload must be required"
    );
    assert!(
        !src.contains(".value!"),
        "emitted code must never use a non-null assertion"
    );
    // `Intent = { Status, Login, Transfer }`: `Login`/`Transfer` are member
    // keys, not references to the types of those names.
    assert!(
        !enc.contains("import type { Login }") && !enc.contains("Login } from"),
        "enum member keys must not create type imports"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ts_hytale_packets_roundtrip() {
    roundtrip_schema("schemas/hytale/2026_02_17.bps", "hytale", 1);
}

/// `emit_typescript_with_target` writes a `tsconfig.json` with the requested
/// target, `ESNext` modules, and strict mode. Text-only: no node run here.
#[test]
fn ts_target_option() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + "packet probe { n: varint }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    for (tag, target) in [
        ("es2020", bytemill::TsTarget::ES2020),
        ("es2022", bytemill::TsTarget::ES2022),
    ] {
        let dir = scratch(&format!("target-{tag}"));
        bytemill::emit_ts::emit_typescript_with_target(&resolved, &dir, target).unwrap();
        let tsconfig = std::fs::read_to_string(dir.join("tsconfig.json")).unwrap();
        assert!(
            tsconfig.contains(&format!("\"target\": \"{tag}\"")),
            "tsconfig.json must request target {tag}:\n{tsconfig}"
        );
        assert!(
            tsconfig.contains("\"module\": \"ESNext\""),
            "tsconfig.json must use ESNext modules:\n{tsconfig}"
        );
        assert!(
            tsconfig.contains("\"strict\": true"),
            "tsconfig.json must enable strict mode:\n{tsconfig}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

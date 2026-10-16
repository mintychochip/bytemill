//! `emit typescript` produces a module node can round-trip.
//!
//! Function names resolve through the printer (`decode_fn`/`encode_fn`),
//! never by string munging in the test.

use bps::ir::typed::Program;

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn node(dir: &std::path::Path, script: &str) -> std::process::Output {
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

fn check_ok(dir: &std::path::Path, source: &str, main: &str) {
    let mut src = source.to_string();
    src.push_str(main);
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
    let resolved = bps::analyze(&bps::parse(&source).unwrap()).unwrap();
    let dir = scratch("varint");
    bps::emit_ts::emit_typescript(&resolved, &dir).unwrap();
    let src = std::fs::read_to_string(dir.join("bpsgen.ts")).unwrap();
    // 32-bit-or-narrower integers are plain `number`, not `bigint`.
    assert!(src.contains("export function decodeVarint(r: Reader): number"));
    check_ok(
        &dir,
        &src,
        r#"
const w = new Writer();
encodeVarint(w, 300);
if (w.toBytes().join(",") !== "172,2") throw new Error("300 " + w.toBytes());
if (decodeVarint(new Reader(w.toBytes())) !== 300) throw new Error("dec 300");
const w2 = new Writer();
encodeVarint(w2, -1);
if (w2.toBytes().join(",") !== "255,255,255,255,15") throw new Error("-1 " + w2.toBytes());
if (decodeVarint(new Reader(w2.toBytes())) !== -1) throw new Error("dec -1");
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
    let resolved = bps::analyze(&bps::parse(&source).unwrap()).unwrap();
    let program = bps::ir::typed(&resolved).unwrap();
    let dir = scratch("scalars");
    bps::emit_ts::emit_typescript(&resolved, &dir).unwrap();
    let src = std::fs::read_to_string(dir.join("bpsgen.ts")).unwrap();
    let c = program.packet_codec("probe").unwrap();
    let (dec, enc) = (
        bps::emit_ts::decode_fn(&program, c),
        bps::emit_ts::encode_fn(&program, c),
    );
    check_ok(
        &dir,
        &src,
        &format!(
            r#"
const v = {{ a: true, b: -5, c: 0xcafe, d: 1.5, e: -123.456, f: "héllo" }};
const w = new Writer();
{enc}(w, v);
const back = {dec}(new Reader(w.toBytes()));
const w2 = new Writer();
{enc}(w2, back);
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
    let resolved = bps::analyze(&bps::parse(&source).unwrap()).unwrap();
    let program: Program = bps::ir::typed(&resolved).unwrap();
    let dir = scratch("errors");
    bps::emit_ts::emit_typescript(&resolved, &dir).unwrap();
    let src = std::fs::read_to_string(dir.join("bpsgen.ts")).unwrap();
    let c = program.packet_codec("probe").unwrap();
    let dec = bps::emit_ts::decode_fn(&program, c);
    check_ok(
        &dir,
        &src,
        &format!(
            r#"
function kindOf(f) {{
  try {{
    f();
    return "no-throw";
  }} catch (e) {{
    return e instanceof BpsError ? e.kind + "@" + e.codec : "foreign:" + e;
  }}
}}
if (kindOf(() => decodeVarint(new Reader(new Uint8Array(0)))) !== "shortBuffer@varint")
  throw new Error("empty varint");
if (kindOf(() => decodeVarint(new Reader(Uint8Array.from([0xac])))) !== "shortBuffer@varint")
  throw new Error("truncated varint");
const w = new Writer();
encodeVarint(w, 299);
if (kindOf(() => {dec}(new Reader(w.toBytes()))) !== "requireFailed@probe")
  throw new Error("require should fail");
const w2 = new Writer();
encodeVarint(w2, 300);
const v = {dec}(new Reader(w2.toBytes()));
if (v.n !== 300) throw new Error("require should pass");
console.log("ok");
"#
        ),
    );
}

/// Decode the interpreter's bytes with the generated decoder, encode again
/// with the generated encoder, and compare bytes exactly.
fn roundtrip_schema(schema_rel: &str, tag: &str, min_packets: usize) {
    use bps::interp::encode_packet;
    use bps::synth::synthesize_packet;
    let resolved = bps::analyze(&bps::load_schema(&[repo(schema_rel)]).unwrap()).unwrap();
    let program: Program = bps::ir::typed(&resolved).unwrap();
    let dir = scratch(tag);
    bps::emit_ts::emit_typescript(&resolved, &dir).unwrap();
    let generated = std::fs::read_to_string(dir.join("bpsgen.ts")).unwrap();
    let mut body = generated.clone();
    body.push_str(
        "const fails: string[] = [];\nfunction check(name: string, raw: number[], v: unknown, enc: (w: Writer, v: never) => void) {\n  try {\n    const w = new Writer();\n    enc(w, v as never);\n    const back = w.toBytes();\n    if (back.length !== raw.length || back.some((b, i) => b !== raw[i])) fails.push(name + \" bytes \" + back.length + \" \" + raw.length);\n  } catch (e) { fails.push(name + \" \" + e); }\n}\n",
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
            bps::emit_ts::decode_fn(&program, c),
            bps::emit_ts::encode_fn(&program, c),
        );
        let list = bytes
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(",");
        body.push_str(&format!(
            "check({name:?}, [{list}], {dec}(new Reader(Uint8Array.from([{list}]))), {enc});\n",
            name = packet.name
        ));
        n += 1;
    }
    body.push_str(
        "if (fails.length) { console.log(fails.slice(0, 30).join(\"\\n\")); throw new Error(\"fail \" + fails.length); }\n",
    );
    body.push_str(&format!("console.log(\"packets\", {n});\n"));
    assert!(n >= min_packets, "only {n} packets for {schema_rel}");
    std::fs::write(dir.join("check.ts"), &body).unwrap();
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

#[test]
fn ts_hytale_packets_roundtrip() {
    roundtrip_schema("schemas/hytale/2026_02_17.bps", "hytale", 1);
}

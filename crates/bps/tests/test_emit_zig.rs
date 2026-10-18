//! `emit zig` produces a `bpsgen.zig` index over per-category modules that
//! `zig fmt` accepts and `zig test` round-trips with
//! `std.testing.allocator` (leaks fail the suite).

use bps::ir::typed::Program;
use std::path::Path;

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn zig() -> String {
    std::env::var("ZIG").unwrap_or_else(|_| "/tmp/zig-x86_64-linux-0.14.1/zig".into())
}

/// Concatenated generated modules, so text assertions see the whole tree.
fn generated(dir: &Path) -> String {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("zig"))
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

fn emit(resolved: &bps::ResolvedSchema, dir: &Path) -> String {
    let _ = std::fs::remove_dir_all(dir);
    bps::emit_zig::emit_zig(resolved, dir).unwrap();
    generated(dir)
}

/// The public entry point re-exports every generated module.
fn index(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("bpsgen.zig")).unwrap()
}

fn run_zig(dir: &Path, tool_args: &[&str], file: &str) {
    let run = std::process::Command::new(zig())
        .args(tool_args)
        .arg(file)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{file} failed\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

/// `zig fmt --check` over every generated module.
fn fmt_check(dir: &Path) {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("zig"))
        .collect();
    files.sort();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy();
        run_zig(dir, &["fmt", "--check"], &name);
    }
}

/// Decode call through the printer (`decodeVarint(&r)` or
/// `Probe.decode(a, &r)`), so tests never grep emitted names.
fn decode_call(program: &Program, codec: usize, reader: &str) -> String {
    let path = bps::emit_zig::decode_fn(program, codec);
    if bps::emit_zig::has_deinit(program, codec) {
        format!("{path}(a, &{reader})")
    } else {
        format!("{path}(&{reader})")
    }
}

/// Encode method tail through the printer (`encode`, never grepped).
fn encode_tail(program: &Program, codec: usize) -> String {
    bps::emit_zig::encode_fn(program, codec)
        .rsplit('.')
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn zig_unit_vectors() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + "packet probe { n: varint\n  x: f32be\n  y: f64be\n  require n == 300 }\n";
    let resolved = bps::analyze(&bps::parse(&source).unwrap()).unwrap();
    let program = bps::ir::typed(&resolved).unwrap();
    let dir = std::env::temp_dir().join(format!("bps-zig-unit-{}", std::process::id()));
    let generated = emit(&resolved, &dir);
    assert!(
        generated.contains("pub fn decodeVarint("),
        "expected decodeVarint in a generated module"
    );
    assert!(
        index(&dir).contains("pub const Error = @import(\"runtime.zig\").Error;"),
        "bpsgen.zig must re-export the runtime\n{}",
        index(&dir)
    );
    assert!(dir.join("encoding.zig").is_file(), "missing encoding.zig");
    fmt_check(&dir);
    let varint = program.codec_by_ir("varint").unwrap();
    let probe = program.packet_codec("probe").unwrap();
    let varint_encode = bps::emit_zig::encode_fn(&program, varint);
    let probe_tail = encode_tail(&program, probe);
    let mut src =
        String::from("const std = @import(\"std\");\nconst bps = @import(\"bpsgen.zig\");\n");
    src.push_str(&format!(
        r#"
test "varint vectors" {{
    const a = std.testing.allocator;
    var b300 = [_]u8{{ 0xac, 0x02 }};
    var r300 = bps.Reader{{ .data = &b300 }};
    try std.testing.expectEqual(@as(i32, 300), try bps.{d300});
    var bneg = [_]u8{{ 0xff, 0xff, 0xff, 0xff, 0x0f }};
    var rneg = bps.Reader{{ .data = &bneg }};
    try std.testing.expectEqual(@as(i32, -1), try bps.{dneg});
    var w = bps.Writer.init(a);
    defer w.buf.deinit();
    try bps.{varint_encode}(&w, 300);
    try bps.{varint_encode}(&w, -1);
    try std.testing.expectEqualSlices(u8, &[_]u8{{ 0xac, 0x02, 0xff, 0xff, 0xff, 0xff, 0x0f }}, w.buf.items);
    var bad = [_]u8{{ 0x80, 0x80, 0x80, 0x80, 0x80 }};
    var rbad = bps.Reader{{ .data = &bad }};
    try std.testing.expectError(error.RequireFailed, bps.{dbad});
}}
"#,
        d300 = decode_call(&program, varint, "r300"),
        dneg = decode_call(&program, varint, "rneg"),
        dbad = decode_call(&program, varint, "rbad"),
    ));
    let probe_deinit = if bps::emit_zig::has_deinit(&program, probe) {
        "\n    defer packet.deinit(a);"
    } else {
        ""
    };
    src.push_str(&format!(
        r#"
test "probe floats and require" {{
    const a = std.testing.allocator;
    var bytes = [_]u8{{ 0xac, 0x02, 0x3f, 0x80, 0x00, 0x00, 0x40, 0x09, 0x21, 0xfb, 0x54, 0x44, 0x2d, 0x18 }};
    var r = bps.Reader{{ .data = &bytes }};
    var packet = try bps.{dprobe};{probe_deinit}
    try std.testing.expectEqual(@as(i32, 300), packet.n);
    try std.testing.expectEqual(@as(f32, 1.0), packet.x);
    try std.testing.expectEqual(@as(f64, 3.141592653589793), packet.y);
    var w = bps.Writer.init(a);
    defer w.buf.deinit();
    try packet.{probe_tail}(&w);
    try std.testing.expectEqualSlices(u8, &bytes, w.buf.items);
    var wrong = [_]u8{{ 0x05, 0x3f, 0x80, 0x00, 0x00, 0x40, 0x09, 0x21, 0xfb, 0x54, 0x44, 0x2d, 0x18 }};
    var rwrong = bps.Reader{{ .data = &wrong }};
    try std.testing.expectError(error.RequireFailed, bps.{dwrong});
    var short = [_]u8{{ 0xac }};
    var rshort = bps.Reader{{ .data = &short }};
    try std.testing.expectError(error.Short, bps.{dshort});
}}
"#,
        dprobe = decode_call(&program, probe, "r"),
        probe_deinit = probe_deinit,
        dwrong = decode_call(&program, probe, "rwrong"),
        dshort = decode_call(&program, probe, "rshort"),
    ));
    std::fs::write(dir.join("unit.zig"), &src).unwrap();
    run_zig(&dir, &["test"], "unit.zig");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Round-trip harness shared by the 26.3 and Hytale suites: the interpreter's
/// bytes decode through the generated code and encode back to identical bytes.
fn roundtrip_case(
    program: &Program,
    out: &mut String,
    packets: &mut usize,
    name: &str,
    codec: usize,
    bytes: &[u8],
) {
    let decode = decode_call(program, codec, "reader");
    let tail = encode_tail(program, codec);
    let deinit = if bps::emit_zig::has_deinit(program, codec) {
        "\n    defer packet.deinit(a);"
    } else {
        ""
    };
    let list = bytes
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!(
        r#"
test "packet {packets} {name}" {{
    const a = std.testing.allocator;
    const raw = [_]u8{{ {list} }};
    var reader = bps.Reader{{ .data = &raw }};
    var packet = try bps.{decode};{deinit}
    try std.testing.expectEqual(raw.len, reader.pos);
    var w = bps.Writer.init(a);
    defer w.buf.deinit();
    try packet.{tail}(&w);
    try std.testing.expectEqualSlices(u8, &raw, w.buf.items);
}}
"#,
    ));
    *packets += 1;
}

fn roundtrip_suite(schema: &str, dir_name: &str, minimum: usize) {
    use bps::interp::encode_packet;
    use bps::synth::synthesize_packet;
    let resolved = bps::analyze(&bps::load_schema(&[repo(schema)]).unwrap()).unwrap();
    let program: Program = bps::ir::typed(&resolved).unwrap();
    let dir = std::env::temp_dir().join(format!("{dir_name}-{}", std::process::id()));
    emit(&resolved, &dir);
    assert!(
        index(&dir).contains("pub const Error = @import(\"runtime.zig\").Error;"),
        "missing index re-export for {schema}"
    );
    assert!(dir.join("encoding.zig").is_file(), "missing encoding.zig");
    if schema.contains("26_3") {
        assert!(
            dir.join("play_clientbound.zig").is_file(),
            "missing play_clientbound.zig"
        );
        assert!(dir.join("shared.zig").is_file(), "missing shared.zig");
    }
    fmt_check(&dir);
    let mut src =
        String::from("const std = @import(\"std\");\nconst bps = @import(\"bpsgen.zig\");\n");
    let mut n = 0usize;
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
        roundtrip_case(&program, &mut src, &mut n, &packet.name, codec, &bytes);
    }
    assert!(n > minimum, "only {n} packets in {schema}");
    std::fs::write(dir.join("roundtrip.zig"), &src).unwrap();
    run_zig(&dir, &["test"], "roundtrip.zig");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zig_26_3_packets_roundtrip() {
    roundtrip_suite("schemas/minecraft_26_3_wiki.bps", "bps-zig-263", 100);
}

#[test]
fn zig_hytale_packets_roundtrip() {
    roundtrip_suite("schemas/hytale/2026_02_17.bps", "bps-zig-hytale", 1);
}

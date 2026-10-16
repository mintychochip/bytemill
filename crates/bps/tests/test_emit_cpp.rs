//! `emit cpp` produces `bpsgen.hpp` + `bpsgen.cpp` that g++ round-trips:
//! unit checks on `varint` + a probe packet (exact bytes, floats, a
//! short-buffer error, a `require` failure), a UTF-8 rejection check on
//! `string`, then every synthesizable 26.3 and Hytale packet through
//! decode/encode comparison under the guide's gates.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bps-cpp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn has_tool(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn clang_format_gate(dir: &Path) {
    if !has_tool("clang-format") {
        eprintln!("skipping clang-format gate: not installed");
        return;
    }
    for file in ["bpsgen.hpp", "bpsgen.cpp"] {
        let out = Command::new("clang-format")
            .args(["--dry-run", "--Werror"])
            .arg(dir.join(file))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "clang-format failed for {file}\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn compile_strict(dir: &Path, extra: &[&str]) {
    let mut cmd = Command::new("g++");
    cmd.args([
        "-std=c++20",
        "-Wall",
        "-Wextra",
        "-Wpedantic",
        "-Werror",
        "-O1",
        "-o",
    ])
    .arg(dir.join("check"))
    .arg(dir.join("check.cpp"))
    .arg(dir.join("bpsgen.cpp"));
    for a in extra {
        cmd.arg(a);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "g++ failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_check(dir: &Path) {
    let run = Command::new(dir.join("check")).output().unwrap();
    assert!(
        run.status.success(),
        "c++ check failed\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

/// Sanitize a packet name for use inside a C++ string literal.
fn display_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[test]
fn cpp_varint_roundtrip() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + "packet probe { n: varint, f: f32be, d: f64be }\n";
    let resolved = bps::analyze(&bps::parse(&source).unwrap()).unwrap();
    let program = bps::ir::typed::typed(&resolved).unwrap();
    let dir = temp("varint");
    bps::emit_cpp::emit_cpp(&resolved, &dir).unwrap();
    clang_format_gate(&dir);
    let probe = program.packet_codec("probe").expect("probe codec");
    assert!(program.codecs[probe].params.is_empty());
    let decode = bps::emit_cpp::decode_fn(&program, probe);
    let encode = bps::emit_cpp::encode_fn(&program, probe);
    let check = format!(
        r#"#include "bpsgen.hpp"
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>
static int fails = 0;
static void check(bool ok, const char *msg) {{
  if (!ok) {{
    std::printf("fail: %s\n", msg);
    ++fails;
  }}
}}
int main() {{
  using namespace bps;
  {{
    Writer w;
    encode_varint(w, 300);
    check(w.buf == std::vector<std::uint8_t>{{0xac, 0x02}}, "encode 300");
    Reader r(w.buf.data(), w.buf.size());
    check(decode_varint(r) == 300, "decode 300");
  }}
  {{
    Writer w;
    encode_varint(w, -1);
    check(w.buf == std::vector<std::uint8_t>{{0xff, 0xff, 0xff, 0xff, 0x0f}}, "encode -1");
    Reader r(w.buf.data(), w.buf.size());
    check(decode_varint(r) == -1, "decode -1");
  }}
  {{
    // 300, 1.5f, -1.0: exact bits in, exact bits out.
    const std::uint8_t raw[] = {{0xac, 0x02, 0x3f, 0xc0, 0x00, 0x00, 0xbf, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00}};
    Reader r(raw, sizeof(raw));
    auto v = {decode}(r);
    check(v.n == 300, "probe n");
    float want_f = 1.5f;
    check(std::memcmp(&v.f, &want_f, 4) == 0, "probe f");
    double want_d = -1.0;
    check(std::memcmp(&v.d, &want_d, 8) == 0, "probe d");
    Writer w;
    {encode}(w, v);
    std::vector<std::uint8_t> want(raw, raw + sizeof(raw));
    check(w.buf == want, "probe round trip");
  }}
  {{
    Reader r(nullptr, 0);
    bool threw = false;
    try {{
      decode_varint(r);
    }} catch (const DecodeError &e) {{
      threw = true;
      check(e.offset == 0, "short offset");
      check(std::string(e.what()).find("varint") != std::string::npos, "short codec");
    }}
    check(threw, "short throws");
  }}
  {{
    const std::uint8_t bad[] = {{0x80, 0x80, 0x80, 0x80, 0x80}};
    Reader r(bad, sizeof(bad));
    bool threw = false;
    try {{
      decode_varint(r);
    }} catch (const DecodeError &e) {{
      threw = true;
      check(std::string(e.what()).find("require failed") != std::string::npos, "require text");
    }}
    check(threw, "require throws");
  }}
  return fails != 0 ? 1 : 0;
}}
"#
    );
    std::fs::write(dir.join("check.cpp"), check).unwrap();
    compile_strict(&dir, &[]);
    run_check(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cpp_string_rejects_bad_utf8() {
    let path = repo("schemas/minecraft_core.bps");
    let resolved = bps::analyze(&bps::load_schema(&[path]).unwrap()).unwrap();
    let program = bps::ir::typed::typed(&resolved).unwrap();
    let dir = temp("string");
    bps::emit_cpp::emit_cpp(&resolved, &dir).unwrap();
    clang_format_gate(&dir);
    let string = program.codec_by_ir("string").expect("string codec");
    assert_eq!(bps::emit_cpp::decode_fn(&program, string), "decode_string");
    assert_eq!(bps::emit_cpp::encode_fn(&program, string), "encode_string");
    let check = r#"#include "bpsgen.hpp"
#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>
static int fails = 0;
static void check(bool ok, const char *msg) {
  if (!ok) {
    std::printf("fail: %s\n", msg);
    ++fails;
  }
}
int main() {
  using namespace bps;
  {
    const std::uint8_t raw[] = {0x02, 0x68, 0x69};
    Reader r(raw, sizeof(raw));
    std::string s = decode_string(r, 32767);
    check(s == "hi", "decode string");
    Writer w;
    encode_string(w, s, 32767);
    std::vector<std::uint8_t> want(raw, raw + sizeof(raw));
    check(w.buf == want, "encode string");
  }
  {
    const std::uint8_t bad[] = {0x01, 0xff};
    Reader r(bad, sizeof(bad));
    bool threw = false;
    try {
      decode_string(r, 32767);
    } catch (const DecodeError &e) {
      threw = true;
      check(std::string(e.what()).find("invalid UTF-8") != std::string::npos, "utf8 text");
    }
    check(threw, "utf8 throws");
  }
  {
    const std::uint8_t raw[] = {0x02, 0x68, 0x69};
    Reader r(raw, sizeof(raw));
    bool threw = false;
    try {
      decode_string(r, 1);
    } catch (const DecodeError &e) {
      threw = true;
      check(std::string(e.what()).find("require failed") != std::string::npos, "max text");
    }
    check(threw, "max chars throws");
  }
  return fails != 0 ? 1 : 0;
}
"#;
    std::fs::write(dir.join("check.cpp"), check).unwrap();
    compile_strict(&dir, &[]);
    run_check(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

fn roundtrip_suite(schema_rel: &str, dir_name: &str, min_packets: usize) {
    use bps::interp::encode_packet;
    use bps::synth::synthesize_packet;
    let path = repo(schema_rel);
    let resolved = bps::analyze(&bps::load_schema(&[path]).unwrap()).unwrap();
    let program = bps::ir::typed::typed(&resolved).unwrap();
    let dir = temp(dir_name);
    bps::emit_cpp::emit_cpp(&resolved, &dir).unwrap();
    clang_format_gate(&dir);
    let mut body = String::from(
        "#include \"bpsgen.hpp\"\n#include <cstdint>\n#include <cstdio>\n#include <vector>\nusing namespace bps;\nstatic int fails = 0;\n",
    );
    let mut calls = String::new();
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
        let ci = match program.packet_codec(&packet.name) {
            Some(c) => c,
            None => continue,
        };
        if !program.codecs[ci].params.is_empty() {
            continue;
        }
        let decode = bps::emit_cpp::decode_fn(&program, ci);
        let encode = bps::emit_cpp::encode_fn(&program, ci);
        let name = display_name(&packet.name);
        if bytes.is_empty() {
            body.push_str(&format!(
                "static void check_{n}() {{\n  Reader r(nullptr, 0);\n  try {{\n    auto v = {decode}(r);\n    Writer w;\n    {encode}(w, v);\n    if (!w.buf.empty()) {{\n      std::printf(\"bytes {name}\\n\");\n      ++fails;\n    }}\n  }} catch (const std::exception &e) {{\n    std::printf(\"error {name}: %s\\n\", e.what());\n    ++fails;\n  }}\n}}\n"
            ));
        } else {
            let list = bytes
                .iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            body.push_str(&format!(
                "static void check_{n}() {{\n  const std::uint8_t raw_{n}[] = {{{list}}};\n  Reader r(raw_{n}, sizeof(raw_{n}));\n  try {{\n    auto v = {decode}(r);\n    Writer w;\n    {encode}(w, v);\n    std::vector<std::uint8_t> want(raw_{n}, raw_{n} + sizeof(raw_{n}));\n    if (w.buf != want) {{\n      std::printf(\"bytes {name}\\n\");\n      ++fails;\n    }}\n  }} catch (const std::exception &e) {{\n    std::printf(\"error {name}: %s\\n\", e.what());\n    ++fails;\n  }}\n}}\n"
            ));
        }
        calls.push_str(&format!("check_{n}(); "));
        n += 1;
    }
    assert!(n > min_packets, "only {n} packets");
    body.push_str(&format!(
        "int main() {{ {calls}return fails != 0 ? 1 : 0; }}\n"
    ));
    std::fs::write(dir.join("check.cpp"), body).unwrap();
    compile_strict(&dir, &[]);
    run_check(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cpp_26_3_packets_roundtrip() {
    roundtrip_suite("schemas/minecraft_26_3_wiki.bps", "all-26-3", 100);
}

#[test]
fn cpp_hytale_packets_roundtrip() {
    // Connection carries 4 packets; every synthesizable one must survive.
    roundtrip_suite("schemas/hytale/2026_02_17.bps", "hytale", 3);
}

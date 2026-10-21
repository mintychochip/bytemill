//! `emit cpp` produces `bmlgen.hpp` + `bmlgen.cpp` that g++ round-trips:
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
    let dir = std::env::temp_dir().join(format!("bml-cpp-{name}-{}", std::process::id()));
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
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            matches!(p.extension().and_then(|e| e.to_str()), Some("hpp" | "cpp"))
                && p.file_name().and_then(|n| n.to_str()) != Some("check.cpp")
        })
        .collect();
    files.sort();
    for file in files {
        let out = Command::new("clang-format")
            .args(["--dry-run", "--Werror"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "clang-format failed for {}\n{}\n{}",
            file.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn compile_strict(dir: &Path, extra: &[&str]) {
    let mut sources: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("cpp")
                && p.file_name().and_then(|n| n.to_str()) != Some("check.cpp")
        })
        .collect();
    sources.sort();
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
    .arg(dir.join("check.cpp"));
    cmd.args(&sources);
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
    let source = std::fs::read_to_string(repo("schemas/std/varint.bml")).unwrap()
        + "packet probe { n: varint, f: f32be, d: f64be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let program = bytemill::ir::typed::typed(&resolved).unwrap();
    let dir = temp("varint");
    bytemill::emit_cpp::emit_cpp(&resolved, &dir).unwrap();
    clang_format_gate(&dir);
    // The unit's shape: decoders are [[nodiscard]], errors share a base, the
    // writer borrows byte buffers, and the integer helpers are constexpr.
    let header = std::fs::read_to_string(dir.join("bmlgen.hpp")).unwrap();
    assert!(
        header.contains("[[nodiscard]]"),
        "decoders must be [[nodiscard]]"
    );
    assert!(
        header.contains("class CodecError : public std::runtime_error"),
        "both error types share a base"
    );
    assert!(
        header.contains("std::span<const std::uint8_t>"),
        "byte buffers are borrowed as a span"
    );
    assert!(
        header.contains("constexpr std::int64_t add_wrap("),
        "integer helpers are constexpr"
    );
    let probe = program.packet_codec("probe").expect("probe codec");
    assert!(program.codecs[probe].params.is_empty());
    let decode = bytemill::emit_cpp::decode_fn(&program, probe);
    let encode = bytemill::emit_cpp::encode_fn(&program, probe);
    let check = format!(
        r#"#include "bmlgen.hpp"
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
  using namespace bml;
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
      (void)decode_varint(r);
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
      (void)decode_varint(r);
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
    let path = repo("schemas/minecraft_core.bml");
    let resolved = bytemill::analyze(&bytemill::load_schema(&[path]).unwrap()).unwrap();
    let program = bytemill::ir::typed::typed(&resolved).unwrap();
    let dir = temp("string");
    bytemill::emit_cpp::emit_cpp(&resolved, &dir).unwrap();
    clang_format_gate(&dir);
    let string = program.codec_by_ir("string").expect("string codec");
    assert_eq!(bytemill::emit_cpp::decode_fn(&program, string), "decode_string");
    assert_eq!(bytemill::emit_cpp::encode_fn(&program, string), "encode_string");
    let check = r#"#include "bmlgen.hpp"
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
  using namespace bml;
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
      (void)decode_string(r, 32767);
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
      (void)decode_string(r, 1);
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
    use bytemill::interp::encode_packet;
    use bytemill::synth::synthesize_packet;
    let path = repo(schema_rel);
    let resolved = bytemill::analyze(&bytemill::load_schema(&[path]).unwrap()).unwrap();
    let program = bytemill::ir::typed::typed(&resolved).unwrap();
    let dir = temp(dir_name);
    bytemill::emit_cpp::emit_cpp(&resolved, &dir).unwrap();
    clang_format_gate(&dir);
    let mut body = String::from(
        "#include \"bmlgen.hpp\"\n#include <cstdint>\n#include <cstdio>\n#include <vector>\nusing namespace bml;\nstatic int fails = 0;\n",
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
        let decode = bytemill::emit_cpp::decode_fn(&program, ci);
        let encode = bytemill::emit_cpp::encode_fn(&program, ci);
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
    roundtrip_suite("schemas/minecraft_26_3_wiki.bml", "all-26-3", 100);
}

#[test]
fn cpp_hytale_packets_roundtrip() {
    // Connection carries 4 packets; every synthesizable one must survive.
    roundtrip_suite("schemas/hytale/2026_02_17.bml", "hytale", 3);
}

/// Probe round-trip under one `g++ -std=...` flag, emitted with the matching
/// `CppStandard`. The check program is written in C++98 (no `auto`,
/// `nullptr`, braced init, or range-for), so the same source proves every
/// standard from C++98 to C++23.
fn probe_roundtrip_std(std_ver: bytemill::CppStandard, flag: &str, dir_name: &str) {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bml")).unwrap()
        + "packet probe { n: varint, f: f32be, d: f64be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let program = bytemill::ir::typed::typed(&resolved).unwrap();
    let dir = temp(dir_name);
    bytemill::emit_cpp::emit_cpp_with_standard(&resolved, &dir, std_ver).unwrap();
    let header = std::fs::read_to_string(dir.join("bmlgen.hpp")).unwrap();
    if std_ver.supports_std_span() {
        assert!(
            header.contains("std::span<const std::uint8_t>"),
            "{flag}: byte buffers are borrowed as a span"
        );
    } else {
        assert!(
            header.contains("class Span"),
            "{flag}: pre-C++20 headers polyfill Span"
        );
        assert!(
            !header.contains("std::span"),
            "{flag}: pre-C++20 headers must not use std::span"
        );
    }
    if std_ver.supports_std_optional() {
        assert!(
            header.contains("#include <optional>"),
            "{flag}: C++17+ headers use std::optional"
        );
    } else {
        assert!(
            header.contains("class Optional"),
            "{flag}: pre-C++17 headers polyfill Optional"
        );
        assert!(
            !header.contains("#include <optional>"),
            "{flag}: pre-C++17 headers must not include <optional>"
        );
    }
    if std_ver.supports_nodiscard() {
        assert!(
            header.contains("[[nodiscard]] Probe decode_probe"),
            "{flag}: decoders must be [[nodiscard]]"
        );
    } else {
        // (The `[[nodiscard]]` spelling still appears inside the
        // `BML_NODISCARD` macro definition; the declarations themselves
        // must use the macro.)
        assert!(
            header.contains("BML_NODISCARD Probe decode_probe"),
            "{flag}: pre-C++17 declarations use the BML_NODISCARD macro"
        );
        assert!(
            !header.contains("[[nodiscard]] Probe decode_probe"),
            "{flag}: pre-C++17 declarations must not use [[nodiscard]]"
        );
    }
    if std_ver.supports_cstdint() {
        assert!(
            header.contains("#include <cstdint>"),
            "{flag}: C++11+ headers use <cstdint>"
        );
    } else {
        assert!(
            header.contains("#include <stdint.h>"),
            "{flag}: pre-C++11 headers use <stdint.h>"
        );
        assert!(
            !header.contains("#include <cstdint>"),
            "{flag}: pre-C++11 headers must not include <cstdint>"
        );
    }
    let probe = program.packet_codec("probe").expect("probe codec");
    assert_eq!(bytemill::emit_cpp::decode_fn(&program, probe), "decode_probe");
    assert_eq!(bytemill::emit_cpp::encode_fn(&program, probe), "encode_probe");
    let check = r#"#include "bmlgen.hpp"
#include <cstdio>
#include <cstring>
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
  using namespace bml;
  {
    Writer w;
    encode_varint(w, 300);
    std::vector<std::uint8_t> want;
    want.push_back(0xac);
    want.push_back(0x02);
    check(w.buf == want, "encode 300");
    Reader r(&w.buf[0], w.buf.size());
    check(decode_varint(r) == 300, "decode 300");
  }
  {
    const std::uint8_t raw[] = {0xac, 0x02, 0x3f, 0xc0, 0x00, 0x00, 0xbf,
                                0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00};
    Reader r(raw, sizeof(raw) / sizeof(raw[0]));
    Probe v = decode_probe(r);
    check(v.n == 300, "probe n");
    float want_f = 1.5f;
    check(std::memcmp(&v.f, &want_f, 4) == 0, "probe f");
    double want_d = -1.0;
    check(std::memcmp(&v.d, &want_d, 8) == 0, "probe d");
    Writer w;
    encode_probe(w, v);
    std::vector<std::uint8_t> want(raw, raw + sizeof(raw) / sizeof(raw[0]));
    check(w.buf == want, "probe round trip");
  }
  {
    Reader r(0, 0);
    bool threw = false;
    try {
      (void)decode_varint(r);
    } catch (const DecodeError &e) {
      threw = true;
      check(e.offset == 0, "short offset");
      check(std::string(e.what()).find("varint") != std::string::npos,
            "short codec");
    }
    check(threw, "short throws");
  }
  return fails != 0 ? 1 : 0;
}
"#;
    std::fs::write(dir.join("check.cpp"), check).unwrap();
    compile_with_std(&dir, flag);
    run_check(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `g++ -std=<flag>` without `-Werror`/`-Wpedantic`: pre-C++11 code may use
/// `long long` literals, which are an extension there, so the gate is
/// compilation plus a passing round-trip, not warning-freedom.
fn compile_with_std(dir: &Path, flag: &str) {
    let mut sources: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("cpp")
                && p.file_name().and_then(|n| n.to_str()) != Some("check.cpp")
        })
        .collect();
    sources.sort();
    let mut cmd = Command::new("g++");
    cmd.arg(format!("-std={flag}"));
    cmd.args(["-Wall", "-O1", "-o"])
        .arg(dir.join("check"))
        .arg(dir.join("check.cpp"));
    cmd.args(&sources);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "g++ -std={flag} failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn cpp_std98_probe_roundtrip() {
    probe_roundtrip_std(bytemill::CppStandard::Cpp98, "c++98", "probe-98");
}

#[test]
fn cpp_std11_probe_roundtrip() {
    probe_roundtrip_std(bytemill::CppStandard::Cpp11, "c++11", "probe-11");
}

#[test]
fn cpp_std17_probe_roundtrip() {
    probe_roundtrip_std(bytemill::CppStandard::Cpp17, "c++17", "probe-17");
}

#[test]
fn cpp_std20_probe_roundtrip() {
    probe_roundtrip_std(bytemill::CppStandard::Cpp20, "c++20", "probe-20");
}

#[test]
fn cpp_std23_probe_roundtrip() {
    probe_roundtrip_std(bytemill::CppStandard::Cpp23, "c++23", "probe-23");
}

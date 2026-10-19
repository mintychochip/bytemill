//! `emit c` output round-trips through `cc` under the strict gate.
//!
//! Unit checks pin varint values, floats, short-buffer and `require`
//! failures (codec, offset, and detail through the reader). The packet
//! harnesses decode the interpreter's bytes for every synthesizable packet,
//! encode again, compare, and prove a counting allocator balances to zero
//! after decode, encode, and free.

use bps::ir::typed::{IntTy, Program, Ty, typed};

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn analyzed(rel: &str) -> bps::ResolvedSchema {
    bps::analyze(&bps::load_schema(&[repo(rel)]).unwrap()).unwrap()
}

fn program_of(resolved: &bps::ResolvedSchema) -> Program {
    typed(resolved).unwrap()
}

fn probe_program(text: &str) -> (bps::ResolvedSchema, Program) {
    let std = std::fs::read_to_string(repo("schemas/std/integers.bps")).unwrap()
        + &std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap();
    let resolved = bps::analyze(&bps::parse(&(std + text)).unwrap()).unwrap();
    let program = typed(&resolved).unwrap();
    (resolved, program)
}

fn emit_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bps-c-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn emit(resolved: &bps::ResolvedSchema, dir: &std::path::Path) {
    bps::emit_c::emit_c(resolved, dir).unwrap();
}

fn has_tool(tool: &str) -> bool {
    std::process::Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The emitted tree must already be `clang-format`-canonical (LLVM style,
/// 2-space indent, 80 columns): the printer formats in place, so a dry run
/// over its own output must find nothing left to change.
fn clang_format_gate(dir: &std::path::Path) {
    if !has_tool("clang-format") {
        eprintln!("skipping clang-format gate: not installed");
        return;
    }
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            matches!(p.extension().and_then(|e| e.to_str()), Some("h" | "c"))
                && p.file_name().and_then(|n| n.to_str()) != Some("check.c")
        })
        .collect();
    files.sort();
    for file in files {
        let out = std::process::Command::new("clang-format")
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

/// Compile `check.c` against `bpsgen.c` with the strict gate, run it, and
/// return its stdout. `extra` adds flags such as `-O2`.
fn compile_and_run(dir: &std::path::Path, extra: &[&str]) -> String {
    let mut sources: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("c")
                && p.file_name().and_then(|n| n.to_str()) != Some("check.c")
        })
        .collect();
    sources.sort();
    let mut cmd = std::process::Command::new("cc");
    cmd.args(["-std=c11", "-Wall", "-Wextra", "-Wpedantic", "-Werror"])
        .args(extra)
        .arg("-o")
        .arg(dir.join("check"))
        .arg(dir.join("check.c"));
    cmd.args(&sources);
    let compile = cmd.output().unwrap();
    assert!(
        compile.status.success(),
        "cc failed\n{}\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = std::process::Command::new(dir.join("check"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "c check failed\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8_lossy(&run.stdout).into_owned()
}

/// The C spelling of a scalar codec value for hand-written checks.
fn scalar_ty(ty: &Ty) -> &'static str {
    match ty {
        Ty::Int(t) => match t {
            IntTy::I8 => "int8_t",
            IntTy::I16 => "int16_t",
            IntTy::I32 => "int32_t",
            IntTy::I64 => "int64_t",
            IntTy::I128 => panic!("no C scalar"),
            IntTy::U8 => "uint8_t",
            IntTy::U16 => "uint16_t",
            IntTy::U32 => "uint32_t",
            IntTy::U64 => "uint64_t",
        },
        Ty::Bool => "bool",
        Ty::F32 => "float",
        Ty::F64 => "double",
        _ => panic!("not a scalar"),
    }
}

#[test]
fn c_varint_values() {
    let text = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap();
    let resolved = bps::analyze(&bps::parse(&text).unwrap()).unwrap();
    let program = program_of(&resolved);
    let dir = emit_dir("varint");
    emit(&resolved, &dir);
    clang_format_gate(&dir);
    let ci = program.codec_by_ir("varint").expect("varint codec");
    let dec = bps::emit_c::decode_fn(&program, ci);
    let enc = bps::emit_c::encode_fn(&program, ci);
    let vt = scalar_ty(&program.codecs[ci].value);
    assert!(
        !bps::emit_c::decode_takes_alloc(&program, ci),
        "varint decode takes no allocator"
    );
    let body = format!(
        r#"#include "bpsgen.h"
#include <stdio.h>
#include <string.h>
int main(void) {{
    {{ uint8_t raw[] = {{0xac, 0x02}}; bps_reader r = {{raw, sizeof raw, 0, NULL, NULL}}; {vt} v = 0;
      if ({dec}(&r, &v) != BPS_OK || v != 300) {{ printf("decode 300 got %d\n", (int)v); return 1; }} }}
    {{ bps_writer w = {{NULL, 0, 0, NULL, NULL}}; bps_alloc al = {{NULL, NULL, NULL}};
      if ({enc}(&w, &al, 300) != BPS_OK || w.len != 2 || w.data[0] != 0xac || w.data[1] != 0x02) {{ printf("encode 300\n"); return 1; }}
      bps_free(&al, w.data); }}
    {{ uint8_t raw[] = {{0xff, 0xff, 0xff, 0xff, 0x0f}}; bps_reader r = {{raw, sizeof raw, 0, NULL, NULL}}; {vt} v = 0;
      if ({dec}(&r, &v) != BPS_OK || v != -1) {{ printf("decode -1\n"); return 1; }} }}
    {{ bps_writer w = {{NULL, 0, 0, NULL, NULL}}; bps_alloc al = {{NULL, NULL, NULL}};
      if ({enc}(&w, &al, -1) != BPS_OK || w.len != 5 || w.data[0] != 0xff || w.data[4] != 0x0f) {{ printf("encode -1\n"); return 1; }}
      bps_free(&al, w.data); }}
    {{ uint8_t raw[] = {{0xac}}; bps_reader r = {{raw, sizeof raw, 0, NULL, NULL}}; {vt} v = 0;
      bps_status s = {dec}(&r, &v);
      if (s != BPS_ERR_SHORT || r.error_codec == NULL || strcmp(r.error_codec, "{dec}") != 0 || r.pos != 1) {{
          printf("short %s %d\n", bps_status_str(s), (int)r.pos); return 1; }} }}
    {{ uint8_t raw[] = {{0x80, 0x80, 0x80, 0x80, 0x80}}; bps_reader r = {{raw, sizeof raw, 0, NULL, NULL}}; {vt} v = 0;
      bps_status s = {dec}(&r, &v);
      if (s != BPS_ERR_REQUIRE || r.error_detail == NULL
          || strcmp(r.error_detail, "require failed: (b4 & 0x80) == 0") != 0) {{
          printf("require %s\n", bps_status_str(s)); return 1; }} }}
    printf("varint ok\n");
    return 0;
}}
"#,
    );
    std::fs::write(dir.join("check.c"), body).unwrap();
    let out = compile_and_run(&dir, &[]);
    assert!(out.contains("varint ok"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn c_float_and_require_probe() {
    let (resolved, program) = probe_program(
        r#"
packet probe_floats {
    a: f32be
    b: f64be
}
packet probe_req {
    v: varint
    require v == 777
}
"#,
    );
    let dir = emit_dir("probe");
    emit(&resolved, &dir);
    clang_format_gate(&dir);
    let ci = program
        .packet_codec("probe_floats")
        .expect("probe_floats codec");
    let dec = bps::emit_c::decode_fn(&program, ci);
    let enc = bps::emit_c::encode_fn(&program, ci);
    let Ty::Named(t) = &program.codecs[ci].value else {
        panic!("probe_floats is a record")
    };
    let tn = bps::emit_c::type_name(&program, *t);
    let free = bps::emit_c::free_fn(&program, *t);
    let dcall = if bps::emit_c::decode_takes_alloc(&program, ci) {
        format!("{dec}(&r, &al, &v)")
    } else {
        format!("{dec}(&r, &v)")
    };
    let ri = program.packet_codec("probe_req").expect("probe_req codec");
    let rdec = bps::emit_c::decode_fn(&program, ri);
    let Ty::Named(rt) = &program.codecs[ri].value else {
        panic!("probe_req is a record")
    };
    let rtn = bps::emit_c::type_name(&program, *rt);
    let rfree = bps::emit_c::free_fn(&program, *rt);
    let rdcall = if bps::emit_c::decode_takes_alloc(&program, ri) {
        format!("{rdec}(&r, &al, &q)")
    } else {
        format!("{rdec}(&r, &q)")
    };
    let body = format!(
        r#"#include "bpsgen.h"
#include <stdio.h>
#include <string.h>
int main(void) {{
    bps_alloc al = {{NULL, NULL, NULL}};
    {{ uint8_t raw[] = {{0x3f, 0x80, 0x00, 0x00, 0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00}};
      bps_reader r = {{raw, sizeof raw, 0, NULL, NULL}}; {tn} v; memset(&v, 0, sizeof v);
      if ({dcall} != BPS_OK || v.a != 1.0f || v.b != 1.0) {{ printf("decode floats\n"); return 1; }}
      bps_writer w = {{NULL, 0, 0, NULL, NULL}};
      if ({enc}(&w, &al, &v) != BPS_OK || w.len != sizeof raw || memcmp(w.data, raw, sizeof raw) != 0) {{
          printf("encode floats\n"); return 1; }}
      bps_free(&al, w.data);
      {free}(&al, &v); }}
    {{ uint8_t raw[] = {{0x3f, 0x80, 0x00}}; bps_reader r = {{raw, sizeof raw, 0, NULL, NULL}}; {tn} v; memset(&v, 0, sizeof v);
      if ({dcall} != BPS_ERR_SHORT) {{ printf("short floats\n"); return 1; }}
      {free}(&al, &v); }}
    {{ uint8_t raw[] = {{0x01}}; bps_reader r = {{raw, sizeof raw, 0, NULL, NULL}}; {rtn} q; memset(&q, 0, sizeof q);
      bps_status s = {rdcall};
      if (s != BPS_ERR_REQUIRE || r.error_codec == NULL || strcmp(r.error_codec, "{rdec}") != 0
          || r.error_detail == NULL || strcmp(r.error_detail, "require failed: v == 777") != 0) {{
          printf("require %s\n", bps_status_str(s)); return 1; }}
      {rfree}(&al, &q); }}
    printf("probe ok\n");
    return 0;
}}
"#,
    );
    std::fs::write(dir.join("check.c"), body).unwrap();
    let out = compile_and_run(&dir, &[]);
    assert!(out.contains("probe ok"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

const COUNTING_ALLOC: &str = r#"#include "bpsgen.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static size_t live_bytes = 0;
static size_t live_count = 0;
static int fails = 0;
static void *cnt_malloc(size_t n, void *ctx) {
    size_t *p;
    (void)ctx;
    p = malloc(n + sizeof(size_t));
    if (p == NULL) { return NULL; }
    *p = n;
    live_bytes += n;
    live_count += 1;
    return p + 1;
}
static void cnt_free(void *p, void *ctx) {
    size_t *q;
    (void)ctx;
    if (p == NULL) { return; }
    q = (size_t *)p - 1;
    live_bytes -= *q;
    live_count -= 1;
    free(q);
}
static void check_live(const char *what) {
    if (live_bytes != 0 || live_count != 0) {
        printf("live %s %lu %lu\n", what, (unsigned long)live_bytes, (unsigned long)live_count);
        fails += 1;
        live_bytes = 0;
        live_count = 0;
    }
}
 "#;

/// One harness function per packet: interpreter bytes -> decode -> encode ->
/// compare, then free everything and prove the allocator balances to zero.
fn packet_harness(resolved: &bps::ResolvedSchema, program: &Program) -> (String, usize) {
    use bps::interp::encode_packet;
    use bps::synth::synthesize_packet;
    let mut body = String::from(COUNTING_ALLOC);
    let mut names: Vec<&str> = resolved.packets.keys().collect();
    names.sort();
    let mut n = 0;
    for name in names {
        let packet = resolved.packets.get(name).unwrap();
        let value = match synthesize_packet(resolved, packet) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let bytes = match encode_packet(resolved, &packet.name, &value) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let ci = program
            .packet_codec(&packet.name)
            .unwrap_or_else(|| panic!("no codec {}", packet.name));
        let Ty::Named(t) = &program.codecs[ci].value else {
            panic!("packet {} is a record", packet.name)
        };
        let tn = bps::emit_c::type_name(program, *t);
        let dec = bps::emit_c::decode_fn(program, ci);
        let enc = bps::emit_c::encode_fn(program, ci);
        let free = bps::emit_c::free_fn(program, *t);
        let dcall = if bps::emit_c::decode_takes_alloc(program, ci) {
            format!("{dec}(&r, &al, &v)")
        } else {
            format!("{dec}(&r, &v)")
        };
        let raw = if bytes.is_empty() {
            "0".to_string()
        } else {
            bytes
                .iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        let len = bytes.len();
        body.push_str(&format!(
            "static void check_{n}(void) {{\n    static const uint8_t raw[] = {{{raw}}};\n    bps_reader r = {{raw, {len}, 0, NULL, NULL}};\n    bps_alloc al = {{cnt_malloc, cnt_free, NULL}};\n    {tn} v; memset(&v, 0, sizeof v);\n    bps_writer w = {{NULL, 0, 0, NULL, NULL}};\n    if ({dcall} != BPS_OK) {{ printf(\"decode {name}: %s %s\\n\", r.error_codec ? r.error_codec : \"?\", r.error_detail ? r.error_detail : \"?\"); fails += 1; check_live(\"{name}-decode\"); return; }}\n    if ({enc}(&w, &al, &v) != BPS_OK) {{ printf(\"encode {name}: %s %s\\n\", w.error_codec ? w.error_codec : \"?\", w.error_detail ? w.error_detail : \"?\"); {free}(&al, &v); bps_free(&al, w.data); w.data = NULL; fails += 1; check_live(\"{name}-encode\"); return; }}\n    if (w.len != {len} || ({len} != 0 && memcmp(w.data, raw, {len}) != 0)) {{ printf(\"bytes {name} %lu\\n\", (unsigned long)w.len); fails += 1; }}\n    bps_free(&al, w.data);\n    {free}(&al, &v);\n    check_live(\"{name}\");\n}}\n",
        ));
        n += 1;
    }
    body.push_str("int main(void) {\n");
    for i in 0..n {
        body.push_str(&format!("    check_{i}();\n"));
    }
    body.push_str(&format!(
        "    if (fails != 0) {{ printf(\"%d failures\\n\", fails); return 1; }}\n    printf(\"packets ok {n}\\n\");\n    return 0;\n}}\n"
    ));
    (body, n)
}

#[test]
fn c_26_3_packets_roundtrip() {
    let resolved = analyzed("schemas/minecraft_26_3_wiki.bps");
    let program = program_of(&resolved);
    let dir = emit_dir("all");
    emit(&resolved, &dir);
    clang_format_gate(&dir);
    let (body, n) = packet_harness(&resolved, &program);
    std::fs::write(dir.join("check.c"), body).unwrap();
    let out = compile_and_run(&dir, &[]);
    assert!(out.contains(&format!("packets ok {n}")), "{out}");
    // And again at -O2.
    let out = compile_and_run(&dir, &["-O2"]);
    assert!(out.contains(&format!("packets ok {n}")), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn c_hytale_packets_roundtrip() {
    let resolved = analyzed("schemas/hytale/2026_02_17.bps");
    let program = program_of(&resolved);
    let dir = emit_dir("hytale");
    emit(&resolved, &dir);
    clang_format_gate(&dir);
    let (body, n) = packet_harness(&resolved, &program);
    std::fs::write(dir.join("check.c"), body).unwrap();
    let out = compile_and_run(&dir, &["-O2"]);
    assert!(out.contains(&format!("packets ok {n}")), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A schema field named after a codec's fixed local (`s` for the status,
/// `r`/`a`/`out` on decode, `w`/`a`/`value` on encode) must not collide with
/// it: the body renames the local, and the strict compile gate proves it.
#[test]
fn c_field_names_never_collide_with_codec_locals() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + "packet clash { s: varint\n r: varint\n a: varint\n out: varint\n w: varint\n value: varint }\n";
    let resolved = bps::analyze(&bps::parse(&source).unwrap()).unwrap();
    let dir = emit_dir("clash");
    emit(&resolved, &dir);
    clang_format_gate(&dir);
    std::fs::write(
        dir.join("check.c"),
        r#"#include "bpsgen.h"
#include <stdio.h>
#include <string.h>
int main(void) {
    uint8_t raw[] = {1, 2, 3, 4, 5, 6};
    bps_reader r = {raw, sizeof raw, 0, NULL, NULL};
    bps_clash v;
    if (bps_decode_clash(&r, &v) != BPS_OK) { printf("decode\n"); return 1; }
    if (v.s != 1 || v.r != 2 || v.a != 3 || v.out != 4 || v.w != 5 || v.value != 6) {
        printf("fields\n"); return 1;
    }
    uint8_t buf[16];
    bps_writer w = {buf, 0, sizeof buf, NULL, NULL};
    if (bps_encode_clash(&w, NULL, &v) != BPS_OK) { printf("encode\n"); return 1; }
    if (w.len != 6 || memcmp(w.data, raw, 6) != 0) { printf("bytes\n"); return 1; }
    printf("clash ok\n");
    return 0;
}
"#,
    )
    .unwrap();
    let out = compile_and_run(&dir, &[]);
    assert!(out.contains("clash ok"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

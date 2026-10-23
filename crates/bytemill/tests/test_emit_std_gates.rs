//! Strict formatter/lint gates on `schemas/std/leb128.bml` for every typed printer.
//! Regression for parenthesization and other emitter issues that only show up under
//! `-Wparentheses` / full language gates.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use bytemill::ir::typed::{IntTy, Program, Ty};
use bytemill::vectors::{TestRun, Vector, VectorSpec};
use bytemill::{
    CSharpVersion, CStandard, CppStandard, GoVersion, JavaVersion, PythonVersion, RustEdition,
    TsTarget, ZigVersion,
};

fn zig() -> String {
    std::env::var("ZIG").unwrap_or_else(|_| "/tmp/zig-x86_64-linux-0.14.1/zig".into())
}

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bml-std-gates-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or(());
    dir
}

fn probe_source() -> String {
    std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap()
        + &std::fs::read_to_string(repo("schemas/std/leb128.bml")).unwrap()
        + r"
packet p_vu32 { v: varuint32 }
packet p_vu64 { v: varuint64 }
packet p_vi32 { v: varint32 }
packet p_vi64 { v: varint64 }
"
}

fn resolved_probe() -> (bytemill::ResolvedSchema, Program) {
    let resolved = bytemill::analyze(&bytemill::parse(&probe_source()).unwrap()).unwrap();
    let program = bytemill::ir::typed::typed(&resolved).unwrap();
    (resolved, program)
}

fn has_tool(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn assert_ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fresh_rust_target(name: &str) -> PathBuf {
    let dir = Path::new(&std::env::var("HOME").unwrap()).join(format!(
        ".cache/bml-scratch/std-gates-rust-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn rust_cargo(dir: &Path, target: &Path, args: &[&str]) -> Output {
    Command::new("cargo")
        .args(args)
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .expect("cargo")
}

fn gate_rust_typed(dir: &Path, target: &Path, tests: &str) {
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("tests/leb.rs"), tests).unwrap();
    if has_tool("rustfmt") {
        let _ = Command::new("rustfmt")
            .args(["--edition", "2024"])
            .arg(dir.join("tests/leb.rs"))
            .status();
    }
    if Command::new("cargo")
        .args(["fmt", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        assert_ok(
            &rust_cargo(dir, target, &["fmt", "--check"]),
            "rust fmt --check",
        );
    }
    if Command::new("cargo")
        .args(["clippy", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        assert_ok(
            &rust_cargo(
                dir,
                target,
                &["clippy", "--all-targets", "--", "-D", "warnings"],
            ),
            "rust clippy",
        );
    }
    assert_ok(&rust_cargo(dir, target, &["test"]), "rust cargo test");
    let _ = std::fs::remove_dir_all(target);
}

#[test]
fn std_gates_rust_typed_leb128() {
    let (resolved, program) = resolved_probe();
    let vu32 = program.codec_by_ir("varuint32").expect("varuint32");
    let vi32 = program.codec_by_ir("varint32").expect("varint32");
    let dec_vu32 = bytemill::emit_rust_typed::decode_fn(&program, vu32);
    let enc_vu32 = bytemill::emit_rust_typed::encode_fn(&program, vu32);
    let dec_vi32 = bytemill::emit_rust_typed::decode_fn(&program, vi32);
    let enc_vi32 = bytemill::emit_rust_typed::encode_fn(&program, vi32);
    let dir = scratch("rust");
    let target = fresh_rust_target("leb");
    bytemill::emit_rust_typed::emit_rust_typed(&resolved, &dir).unwrap();
    let tests = format!(
        "use bml_generated::*;\n\
#[test]\nfn varuint32_300() {{\n    let mut r = Reader::new(&[0xac, 0x02]);\n    assert_eq!({dec_vu32}(&mut r).unwrap(), 300);\n    let mut w = Writer::new();\n    {enc_vu32}(&mut w, 300).unwrap();\n    assert_eq!(w.into_bytes(), vec![0xac, 0x02]);\n}}\n\
#[test]\nfn varint32_neg2() {{\n    let mut r = Reader::new(&[0x03]);\n    assert_eq!({dec_vi32}(&mut r).unwrap(), -2);\n    let mut w = Writer::new();\n    {enc_vi32}(&mut w, -2).unwrap();\n    assert_eq!(w.into_bytes(), vec![0x03]);\n}}\n"
    );
    gate_rust_typed(&dir, &target, &tests);
    let _ = std::fs::remove_dir_all(&dir);
}

fn assert_go_gates(dir: &Path) {
    let out = Command::new("gofmt")
        .args(["-l", "."])
        .current_dir(dir)
        .output()
        .unwrap();
    assert_ok(&out, "gofmt");
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "gofmt would reformat:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_ok(
        &Command::new("go")
            .args(["vet", "./..."])
            .current_dir(dir)
            .output()
            .unwrap(),
        "go vet",
    );
}

#[test]
fn std_gates_go_leb128() {
    let (resolved, program) = resolved_probe();
    let vu32 = program.codec_by_ir("varuint32").unwrap();
    let vi32 = program.codec_by_ir("varint32").unwrap();
    let dec_vu32 = bytemill::emit_go::decode_fn(&program, vu32);
    let enc_vu32 = bytemill::emit_go::encode_fn(&program, vu32);
    let dec_vi32 = bytemill::emit_go::decode_fn(&program, vi32);
    let enc_vi32 = bytemill::emit_go::encode_fn(&program, vi32);
    let dir = scratch("go");
    bytemill::emit_go::emit_go(&resolved, &dir).unwrap();
    assert_go_gates(&dir);
    std::fs::write(
        dir.join("leb_test.go"),
        format!(
            r#"package bmlgen

import "testing"

func TestLeb128(t *testing.T) {{
	w := &Writer{{}}
	if err := {enc_vu32}(w, 300); err != nil {{
		t.Fatal(err)
	}}
	if got, want := w.Bytes(), []byte{{0xac, 0x02}}; string(got) != string(want) {{
		t.Fatalf("300: %x", got)
	}}
	vu, err := {dec_vu32}(NewReader(w.Bytes()))
	if err != nil || vu != 300 {{
		t.Fatalf("decode 300: %v %d", err, vu)
	}}
	w = &Writer{{}}
	if err := {enc_vi32}(w, -2); err != nil {{
		t.Fatal(err)
	}}
	if got, want := w.Bytes(), []byte{{0x03}}; string(got) != string(want) {{
		t.Fatalf("-2: %x", got)
	}}
	vi, err := {dec_vi32}(NewReader(w.Bytes()))
	if err != nil || vi != -2 {{
		t.Fatalf("decode -2: %v %d", err, vi)
	}}
}}
"#,
        ),
    )
    .unwrap();
    assert_ok(
        &Command::new("go")
            .args(["test", "./..."])
            .current_dir(&dir)
            .output()
            .unwrap(),
        "go test",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn py_gates(dir: &Path, extra: &[&str]) {
    assert_ok(
        &Command::new("python3")
            .args(["-m", "compileall", "-q", "bmlgen"])
            .current_dir(dir)
            .output()
            .unwrap(),
        "compileall",
    );
    if has_tool("ruff") {
        assert_ok(
            &Command::new("ruff")
                .args(["format", "--check", "bmlgen"])
                .args(extra)
                .current_dir(dir)
                .output()
                .unwrap(),
            "ruff format --check",
        );
        assert_ok(
            &Command::new("ruff")
                .args(["check", "bmlgen"])
                .args(extra)
                .current_dir(dir)
                .output()
                .unwrap(),
            "ruff check",
        );
    }
    if Command::new("python3")
        .args(["-m", "mypy", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        let mut args = vec!["-m", "mypy", "--strict", "bmlgen"];
        args.extend(extra);
        assert_ok(
            &Command::new("python3")
                .args(&args)
                .current_dir(dir)
                .output()
                .unwrap(),
            "mypy --strict",
        );
    }
}

#[test]
fn std_gates_python_leb128() {
    let (resolved, program) = resolved_probe();
    let vu32 = program.codec_by_ir("varuint32").unwrap();
    let vi32 = program.codec_by_ir("varint32").unwrap();
    let dir = scratch("py");
    bytemill::emit_python::emit_python(&resolved, &dir).unwrap();
    let dec_vu32 = bytemill::emit_python::decode_fn(&program, vu32);
    let enc_vu32 = bytemill::emit_python::encode_fn(&program, vu32);
    let dec_vi32 = bytemill::emit_python::decode_fn(&program, vi32);
    let enc_vi32 = bytemill::emit_python::encode_fn(&program, vi32);
    std::fs::write(
        dir.join("check.py"),
        format!(
            r#"from bmlgen import encoding as enc


def main() -> None:
    w = enc.Writer()
    enc.{enc_vu32}(w, 300)
    assert w.get_bytes() == bytes([0xAC, 0x02])
    r = enc.Reader(w.get_bytes())
    assert enc.{dec_vu32}(r) == 300
    w = enc.Writer()
    enc.{enc_vi32}(w, -2)
    assert w.get_bytes() == bytes([0x03])
    r = enc.Reader(w.get_bytes())
    assert enc.{dec_vi32}(r) == -2


if __name__ == "__main__":
    main()
"#,
        ),
    )
    .unwrap();
    py_gates(&dir, &["check.py"]);
    assert_ok(
        &Command::new("python3")
            .arg("check.py")
            .current_dir(&dir)
            .output()
            .unwrap(),
        "check.py",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn java_run_leb(pkg: &Path, dec_vu32: &str, enc_vu32: &str, dec_vi32: &str, enc_vi32: &str) {
    let src = format!(
        r#"class Check {{
    static void expect(boolean ok, String msg) {{
        if (!ok) throw new AssertionError(msg);
    }}
    public static void main(String[] args) throws Exception {{
        Writer w = new Writer();
        Encoding.{enc_vu32}(w, 300);
        expect(java.util.Arrays.equals(w.toByteArray(), new byte[]{{(byte)0xac, 0x02}}), "300 enc");
        expect(Encoding.{dec_vu32}(new Reader(w.toByteArray())) == 300L, "300 dec");
        w = new Writer();
        Encoding.{enc_vi32}(w, -2);
        expect(java.util.Arrays.equals(w.toByteArray(), new byte[]{{0x03}}), "-2 enc");
        expect(Encoding.{dec_vi32}(new Reader(w.toByteArray())) == -2L, "-2 dec");
        System.out.println("ok");
    }}
}}
"#,
    );
    std::fs::write(pkg.join("Check.java"), src).unwrap();
    let sources: Vec<PathBuf> = std::fs::read_dir(pkg)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "java"))
        .collect();
    let java = which("java");
    let javac = which("javac");
    match (javac, java) {
        (Some(javac), Some(java)) => {
            let _ = std::fs::create_dir_all(pkg.join("classes"));
            let compile = Command::new(&javac)
                .args(["-d", "classes"])
                .args(&sources)
                .current_dir(pkg)
                .output()
                .unwrap();
            assert_ok(&compile, "javac");
            let run = Command::new(&java)
                .args(["-cp", "classes", "Check"])
                .current_dir(pkg)
                .output()
                .unwrap();
            assert_ok(&run, "java Check");
            assert!(
                String::from_utf8_lossy(&run.stdout).contains("ok"),
                "stdout:\n{}",
                String::from_utf8_lossy(&run.stdout)
            );
        }
        (None, Some(java)) => {
            let major = Command::new(&java)
                .arg("-version")
                .output()
                .ok()
                .and_then(|o| {
                    let text = String::from_utf8_lossy(&o.stderr);
                    text.split('"').nth(1).and_then(|q| {
                        let first = q.split('.').next()?.parse::<u32>().ok()?;
                        if first == 1 {
                            q.split('.').nth(1)?.parse().ok()
                        } else {
                            Some(first)
                        }
                    })
                })
                .unwrap_or(0);
            if major < 22 {
                eprintln!("skipping java source launch: need Java 22+ without javac");
                return;
            }
            let run = Command::new(&java)
                .arg("Check.java")
                .current_dir(pkg)
                .output()
                .unwrap();
            assert_ok(&run, "java Check.java");
        }
        _ => eprintln!("skipping java execution: no java on PATH"),
    }
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

#[test]
fn std_gates_java_leb128() {
    let (resolved, program) = resolved_probe();
    let vu32 = program.codec_by_ir("varuint32").unwrap();
    let vi32 = program.codec_by_ir("varint32").unwrap();
    let dir = scratch("java");
    bytemill::emit_java::emit_java_with_version(&resolved, &dir, JavaVersion::Java21).unwrap();
    let pkg = dir.join("bmlgen");
    let dec_vu32 = java_codec_fn(&program, vu32, true);
    let enc_vu32 = java_codec_fn(&program, vu32, false);
    let dec_vi32 = java_codec_fn(&program, vi32, true);
    let enc_vi32 = java_codec_fn(&program, vi32, false);
    java_run_leb(&pkg, &dec_vu32, &enc_vu32, &dec_vi32, &enc_vi32);
    let _ = std::fs::remove_dir_all(&dir);
}

fn java_codec_fn(program: &Program, ci: usize, decode: bool) -> String {
    let name = program.codecs[ci].name.pascal();
    if decode {
        format!("decode{name}")
    } else {
        format!("encode{name}")
    }
}

fn dotnet() -> Option<PathBuf> {
    std::env::var_os("HOME").and_then(|h| {
        let p = Path::new(&h).join(".dotnet/dotnet");
        p.is_file().then_some(p)
    })
}

fn csharp_build_gate(dir: &Path) {
    let Some(dotnet) = dotnet() else {
        eprintln!("skipping dotnet build: ~/.dotnet/dotnet not found");
        return;
    };
    let csproj = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <LangVersion>12</LangVersion>
    <OutputType>Library</OutputType>
    <Nullable>disable</Nullable>
    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="BmlGen/**/*.cs" />
  </ItemGroup>
</Project>
"#;
    std::fs::write(dir.join("bmlgen.csproj"), csproj).unwrap();
    assert_ok(
        &Command::new(&dotnet)
            .args(["build", "bmlgen.csproj", "--nologo", "-v", "q"])
            .current_dir(dir)
            .output()
            .unwrap(),
        "dotnet build",
    );
}

#[test]
fn std_gates_csharp_leb128() {
    let (resolved, _program) = resolved_probe();
    let dir = scratch("cs");
    bytemill::emit_csharp::emit_csharp_with_version(
        &resolved,
        &dir,
        bytemill::CSharpVersion::CSharp12,
    )
    .unwrap();
    csharp_build_gate(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

fn clang_format_gate(dir: &Path, exts: &[&str]) {
    if !has_tool("clang-format") {
        eprintln!("skipping clang-format gate: not installed");
        return;
    }
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| exts.contains(&e))
                && p.file_name().and_then(|n| n.to_str()) != Some("check.c")
                && p.file_name().and_then(|n| n.to_str()) != Some("check.cpp")
        })
        .collect();
    files.sort();
    for file in files {
        assert_ok(
            &Command::new("clang-format")
                .args(["--dry-run", "--Werror"])
                .arg(&file)
                .output()
                .unwrap(),
            &format!("clang-format {}", file.display()),
        );
    }
}

fn c_scalar_ty(ty: &Ty) -> &'static str {
    match ty {
        Ty::Int(t) => match t {
            IntTy::I32 => "int32_t",
            IntTy::I64 => "int64_t",
            IntTy::U32 => "uint32_t",
            IntTy::U64 => "uint64_t",
            _ => "int32_t",
        },
        _ => panic!("not scalar"),
    }
}

fn c_leb_check(program: &Program) -> String {
    let vu32 = program.codec_by_ir("varuint32").unwrap();
    let vi32 = program.codec_by_ir("varint32").unwrap();
    let dec_vu32 = bytemill::emit_c::decode_fn(program, vu32);
    let enc_vu32 = bytemill::emit_c::encode_fn(program, vu32);
    let dec_vi32 = bytemill::emit_c::decode_fn(program, vi32);
    let enc_vi32 = bytemill::emit_c::encode_fn(program, vi32);
    let vt_u = c_scalar_ty(&program.codecs[vu32].value);
    let vt_i = c_scalar_ty(&program.codecs[vi32].value);
    format!(
        r#"#include "bmlgen.h"
#include <stdio.h>
#include <string.h>
int main(void) {{
    bml_alloc al = {{NULL, NULL, NULL}};
    {{ uint8_t raw[] = {{0xac, 0x02}}; bml_reader r = {{raw, sizeof raw, 0, NULL, NULL}}; {vt_u} v = 0;
      if ({dec_vu32}(&r, &v) != BML_OK || v != 300) {{ printf("vu32 dec\n"); return 1; }}
      bml_writer w = {{NULL, 0, 0, NULL, NULL}};
      if ({enc_vu32}(&w, &al, 300) != BML_OK || w.len != 2) {{ printf("vu32 enc\n"); return 1; }}
      bml_free(&al, w.data); }}
    {{ uint8_t raw[] = {{0x03}}; bml_reader r = {{raw, sizeof raw, 0, NULL, NULL}}; {vt_i} v = 0;
      if ({dec_vi32}(&r, &v) != BML_OK || v != -2) {{ printf("vi32 dec\n"); return 1; }}
      bml_writer w = {{NULL, 0, 0, NULL, NULL}};
      if ({enc_vi32}(&w, &al, -2) != BML_OK || w.len != 1) {{ printf("vi32 enc\n"); return 1; }}
      bml_free(&al, w.data); }}
    printf("leb ok\n");
    return 0;
}}
"#,
    )
}

fn compile_c_std(dir: &Path, flag: &str) {
    let mut sources: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("c")
                && p.file_name().and_then(|n| n.to_str()) != Some("check.c")
        })
        .collect();
    sources.sort();
    let mut cmd = Command::new("cc");
    cmd.arg(flag).args(["-Wall", "-Wextra", "-Werror"]);
    if flag != "-std=c89" {
        cmd.arg("-Wpedantic");
    }
    cmd.arg("-o")
        .arg(dir.join("check"))
        .arg(dir.join("check.c"));
    cmd.args(&sources);
    assert_ok(&cmd.output().unwrap(), &format!("cc {flag}"));
    assert_ok(
        &Command::new(dir.join("check")).output().unwrap(),
        &format!("run check ({flag})"),
    );
}

#[test]
fn std_gates_c_leb128_all_standards() {
    let (resolved, program) = resolved_probe();
    let cases = [
        (bytemill::CStandard::C89, "-std=c89"),
        (bytemill::CStandard::C99, "-std=c99"),
        (bytemill::CStandard::C11, "-std=c11"),
        (bytemill::CStandard::C17, "-std=c17"),
        (bytemill::CStandard::C23, "-std=c23"),
    ];
    for (std, flag) in cases {
        let dir = scratch(&format!("c-{}", std.name()));
        bytemill::emit_c::emit_c_with_standard(&resolved, &dir, std).unwrap();
        clang_format_gate(&dir, &["h", "c"]);
        std::fs::write(dir.join("check.c"), c_leb_check(&program)).unwrap();
        compile_c_std(&dir, flag);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn cpp_leb_check(program: &Program) -> String {
    let vu32 = program.codec_by_ir("varuint32").unwrap();
    let vi32 = program.codec_by_ir("varint32").unwrap();
    let dec_vu32 = bytemill::emit_cpp::decode_fn(program, vu32);
    let enc_vu32 = bytemill::emit_cpp::encode_fn(program, vu32);
    let dec_vi32 = bytemill::emit_cpp::decode_fn(program, vi32);
    let enc_vi32 = bytemill::emit_cpp::encode_fn(program, vi32);
    format!(
        r#"#include "bmlgen.hpp"
#include <cstdio>
#include <vector>
int main() {{
  using namespace bml;
  {{
    Writer w;
    {enc_vu32}(w, 300);
    std::vector<std::uint8_t> want;
    want.push_back(0xac);
    want.push_back(0x02);
    if (w.buf != want) {{ std::printf("vu32 enc\n"); return 1; }}
    Reader r(&w.buf[0], w.buf.size());
    if ({dec_vu32}(r) != 300) {{ std::printf("vu32 dec\n"); return 1; }}
  }}
  {{
    Writer w;
    {enc_vi32}(w, -2);
    std::vector<std::uint8_t> want;
    want.push_back(0x03);
    if (w.buf != want) {{ std::printf("vi32 enc\n"); return 1; }}
    Reader r(&w.buf[0], w.buf.size());
    if ({dec_vi32}(r) != -2) {{ std::printf("vi32 dec\n"); return 1; }}
  }}
  std::printf("leb ok\n");
  return 0;
}}
"#,
    )
}

fn compile_cpp_std(dir: &Path, flag: &str, strict: bool) {
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
    cmd.arg(format!("-std={flag}"))
        .args(["-Wall", "-Wextra", "-O1", "-o"])
        .arg(dir.join("check"))
        .arg(dir.join("check.cpp"));
    if strict {
        cmd.args(["-Wpedantic", "-Werror"]);
    }
    cmd.args(&sources);
    assert_ok(&cmd.output().unwrap(), &format!("g++ -std={flag}"));
    assert_ok(
        &Command::new(dir.join("check")).output().unwrap(),
        &format!("run cpp check -std={flag}"),
    );
}

#[test]
fn std_gates_cpp_leb128_all_standards() {
    let (resolved, program) = resolved_probe();
    let cases = [
        (bytemill::CppStandard::Cpp98, "c++98", false),
        (bytemill::CppStandard::Cpp11, "c++11", true),
        (bytemill::CppStandard::Cpp17, "c++17", true),
        (bytemill::CppStandard::Cpp20, "c++20", true),
        (bytemill::CppStandard::Cpp23, "c++23", true),
    ];
    for (std, flag, strict) in cases {
        let dir = scratch(&format!("cpp-{}", std.name()));
        bytemill::emit_cpp::emit_cpp_with_standard(&resolved, &dir, std).unwrap();
        // C++98 polyfill headers are not clang-format-canonical (see test_emit_cpp).
        if std != bytemill::CppStandard::Cpp98 {
            clang_format_gate(&dir, &["hpp", "cpp"]);
        }
        std::fs::write(dir.join("check.cpp"), cpp_leb_check(&program)).unwrap();
        compile_cpp_std(&dir, flag, strict);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A list counted by a `u64` can hold a count above `i64::MAX`. The C++ printer casts the count to
/// `std::int64_t` for its loop, so such a count turns negative: the decode must reject it with the
/// printer's negative-length check, not decode an empty list and carry on. Counts of 32 bits or
/// fewer cannot be negative after the cast and carry no check (g++ `-Wtype-limits` rejects the
/// comparison); the Bedrock-style test covers that side.
#[test]
fn cpp_list_counted_by_u64_rejects_a_count_above_i64_max() {
    let source = std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap()
        + &std::fs::read_to_string(repo("schemas/std/leb128.bml")).unwrap()
        + r"
packet big_count {
    n:     varuint64
    items: u16be[] count = n
    tail:  u8
}
";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let program = bytemill::ir::typed::typed(&resolved).unwrap();
    let codec = program.packet_codec("big_count").expect("big_count codec");
    let decode = bytemill::emit_cpp::decode_fn(&program, codec);
    let check = format!(
        r#"#include "bmlgen.hpp"
#include <cstdio>
int main() {{
  using namespace bml;
  {{
    // count 2, two u16be items, tail 7: decodes and consumes every byte
    const std::uint8_t ok[] = {{0x02, 0x00, 0x01, 0x00, 0x02, 0x07}};
    Reader r(ok, sizeof ok);
    (void){decode}(r);
    if (r.remaining() != 0) {{ std::printf("small count left bytes\n"); return 1; }}
  }}
  {{
    // count 2^63 (nine 0x80 bytes, then 0x01) and a tail byte: the count is above i64::MAX
    const std::uint8_t big[] = {{0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01, 0x07}};
    Reader r(big, sizeof big);
    try {{
      (void){decode}(r);
    }} catch (const DecodeError &) {{
      std::printf("rejected\n");
      return 0;
    }}
    std::printf("accepted a count above i64::MAX as an empty list\n");
    return 1;
  }}
}}
"#
    );
    for (std, flag, strict) in [
        (bytemill::CppStandard::Cpp98, "c++98", false),
        (bytemill::CppStandard::Cpp11, "c++11", true),
        (bytemill::CppStandard::Cpp20, "c++20", true),
    ] {
        let dir = scratch(&format!("cpp-big-count-{}", std.name()));
        bytemill::emit_cpp::emit_cpp_with_standard(&resolved, &dir, std).unwrap();
        std::fs::write(dir.join("check.cpp"), &check).unwrap();
        compile_cpp_std(&dir, flag, strict);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn tsc_gate(dir: &Path) {
    if !has_tool("tsc") {
        eprintln!("skipping tsc: not installed");
        return;
    }
    assert_ok(
        &Command::new("tsc")
            .args(["-p", "."])
            .current_dir(dir)
            .output()
            .unwrap(),
        "tsc -p .",
    );
}

fn prettier_gate_ts(dir: &Path) {
    if !has_tool("prettier") {
        eprintln!("skipping prettier: not installed");
        return;
    }
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ts"))
        .collect();
    files.sort();
    for file in files {
        assert_ok(
            &Command::new("prettier")
                .args(["--check"])
                .arg(&file)
                .current_dir(dir)
                .output()
                .unwrap(),
            &format!(
                "prettier --check {}",
                file.file_name().unwrap().to_string_lossy()
            ),
        );
    }
}

#[test]
fn std_gates_typescript_leb128() {
    let (resolved, program) = resolved_probe();
    let vu32 = program.codec_by_ir("varuint32").unwrap();
    let vi32 = program.codec_by_ir("varint32").unwrap();
    let dir = scratch("ts");
    bytemill::emit_ts::emit_typescript(&resolved, &dir).unwrap();
    tsc_gate(&dir);
    prettier_gate_ts(&dir);
    std::fs::write(
        dir.join("check.ts"),
        format!(
            r#"import * as bml from "./bmlgen.ts";
const w = new bml.Writer();
bml.{enc_vu32}(w, 300);
if (w.toBytes().join(",") !== "172,2") throw new Error("300");
if (bml.{dec_vu32}(new bml.Reader(w.toBytes())) !== 300)
  throw new Error("dec 300");
const w2 = new bml.Writer();
bml.{enc_vi32}(w2, -2);
if (w2.toBytes().join(",") !== "3") throw new Error("-2");
if (bml.{dec_vi32}(new bml.Reader(w2.toBytes())) !== -2)
  throw new Error("dec -2");
console.log("ok");
"#,
            enc_vu32 = bytemill::emit_ts::encode_fn(&program, vu32),
            dec_vu32 = bytemill::emit_ts::decode_fn(&program, vu32),
            enc_vi32 = bytemill::emit_ts::encode_fn(&program, vi32),
            dec_vi32 = bytemill::emit_ts::decode_fn(&program, vi32),
        ),
    )
    .unwrap();
    if has_tool("prettier") {
        assert_ok(
            &Command::new("prettier")
                .args(["--check", "check.ts"])
                .current_dir(&dir)
                .output()
                .unwrap(),
            "prettier --check check.ts",
        );
    }
    assert_ok(
        &Command::new("node")
            .args(["--experimental-strip-types", "check.ts"])
            .current_dir(&dir)
            .output()
            .unwrap(),
        "node check.ts",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn zig_fmt_check(dir: &Path) {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("zig"))
        .collect();
    files.sort();
    for file in files {
        assert_ok(
            &Command::new(zig())
                .args(["fmt", "--check"])
                .arg(file.file_name().unwrap().to_string_lossy().as_ref())
                .current_dir(dir)
                .output()
                .unwrap(),
            "zig fmt --check",
        );
    }
}

#[test]
fn std_gates_zig_leb128() {
    let (resolved, program) = resolved_probe();
    let vu32 = program.codec_by_ir("varuint32").unwrap();
    let vi32 = program.codec_by_ir("varint32").unwrap();
    let dir = scratch("zig");
    bytemill::emit_zig::emit_zig(&resolved, &dir).unwrap();
    zig_fmt_check(&dir);
    let dec_vu32 = bytemill::emit_zig::decode_fn(&program, vu32);
    let enc_vu32 = bytemill::emit_zig::encode_fn(&program, vu32);
    let dec_vi32 = bytemill::emit_zig::decode_fn(&program, vi32);
    let enc_vi32 = bytemill::emit_zig::encode_fn(&program, vi32);
    let test_src = format!(
        r#"const std = @import("std");
const bml = @import("bmlgen.zig");
test "leb128" {{
    const a = std.testing.allocator;
    var b300 = [_]u8{{ 0xac, 0x02 }};
    var r300 = bml.Reader{{ .data = &b300 }};
    try std.testing.expectEqual(@as(u32, 300), try bml.{dec_vu32}(&r300));
    var bneg = [_]u8{{ 0x03 }};
    var rneg = bml.Reader{{ .data = &bneg }};
    try std.testing.expectEqual(@as(i32, -2), try bml.{dec_vi32}(&rneg));
    var w = bml.Writer.init(a);
    defer w.deinit();
    try bml.{enc_vu32}(&w, 300);
    try bml.{enc_vi32}(&w, -2);
    try std.testing.expectEqualSlices(u8, &[_]u8{{ 0xac, 0x02, 0x03 }}, w.buf.items);
}}
"#,
    );
    std::fs::write(dir.join("leb_test.zig"), test_src).unwrap();
    assert_ok(
        &Command::new(zig())
            .args(["test", "leb_test.zig"])
            .current_dir(&dir)
            .output()
            .unwrap(),
        "zig test",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// Wire hex verified with `bml encode --packet … /tmp/leb-probe/probe.bml '<json>'`.
fn leb_vector(packet: &str, hex: &str) -> Vector {
    Vector {
        packet: packet.to_string(),
        route: None,
        hex: hex.to_string(),
    }
}

fn leb_boundary_vectors() -> Vec<Vector> {
    vec![
        leb_vector("p_vu32", "00"),
        leb_vector("p_vu32", "7f"),
        leb_vector("p_vu32", "8001"),
        leb_vector("p_vu32", "ac02"),
        leb_vector("p_vu32", "808001"),
        leb_vector("p_vu32", "8080808008"),
        leb_vector("p_vu32", "ffffffff0f"),
        leb_vector("p_vu64", "00"),
        leb_vector("p_vu64", "8080808010"),
        leb_vector("p_vu64", "80808080808080808001"),
        leb_vector("p_vu64", "ffffffffffffffffff01"),
        leb_vector("p_vi32", "00"),
        leb_vector("p_vi32", "01"),
        leb_vector("p_vi32", "02"),
        leb_vector("p_vi32", "03"),
        leb_vector("p_vi32", "feffffff0f"),
        leb_vector("p_vi32", "ffffffff0f"),
        leb_vector("p_vi64", "00"),
        leb_vector("p_vi64", "01"),
        leb_vector("p_vi64", "02"),
        leb_vector("p_vi64", "03"),
        leb_vector("p_vi64", "feffffffffffffffff01"),
        leb_vector("p_vi64", "ffffffffffffffffff01"),
    ]
}

fn resolved_leb_probe() -> bytemill::ResolvedSchema {
    bytemill::analyze(&bytemill::parse(&probe_source()).unwrap()).unwrap()
}

fn assert_tr_count(tr: &TestRun, vectors: &[Vector]) {
    assert_eq!(tr.vectors, vectors.len(), "embedded vector count");
}

fn run_sh(dir: &Path, cmd: &str, what: &str) {
    assert_ok(
        &Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(dir)
            .output()
            .expect("sh"),
        what,
    );
}

fn run_rust_vectors(dir: &Path, target: &Path, tr: &TestRun) {
    let args: Vec<&str> = tr.command.split_whitespace().skip(1).collect();
    assert_ok(&rust_cargo(dir, target, &args), &tr.command);
}

fn run_python_vectors(dir: &Path, tr: &TestRun) {
    let args: Vec<&str> = tr.command.split_whitespace().skip(1).collect();
    assert_ok(
        &Command::new("python3")
            .args(&args)
            .current_dir(dir)
            .output()
            .unwrap(),
        &tr.command,
    );
}

fn run_go_vectors(dir: &Path, tr: &TestRun) {
    let parts: Vec<&str> = tr.command.split_whitespace().collect();
    assert_eq!(parts[0], "go");
    assert_ok(
        &Command::new("go")
            .args(&parts[1..])
            .current_dir(dir)
            .output()
            .unwrap(),
        &tr.command,
    );
}

fn run_java_vectors(dir: &Path, tr: &TestRun) -> bool {
    if which("java").is_none() {
        return false;
    }
    run_sh(dir, &tr.command, &tr.command);
    true
}

fn run_csharp_vectors(dir: &Path, tr: &TestRun) -> bool {
    let Some(dotnet) = dotnet() else {
        return false;
    };
    let parts: Vec<&str> = tr.command.split_whitespace().collect();
    assert_eq!(parts[0], "dotnet");
    assert_ok(
        &Command::new(&dotnet)
            .args(&parts[1..])
            .current_dir(dir)
            .output()
            .unwrap(),
        &tr.command,
    );
    true
}

fn run_ts_vectors(dir: &Path, tr: &TestRun) {
    if has_tool("tsc") {
        assert_ok(
            &Command::new("tsc")
                .args(["-p", "."])
                .current_dir(dir)
                .output()
                .unwrap(),
            "tsc -p .",
        );
    }
    if has_tool("prettier") {
        assert_ok(
            &Command::new("prettier")
                .args(["--check", &tr.file])
                .current_dir(dir)
                .output()
                .unwrap(),
            "prettier --check",
        );
    }
    let parts: Vec<&str> = tr.command.split_whitespace().collect();
    assert_eq!(parts[0], "node");
    assert_ok(
        &Command::new(parts[0])
            .args(&parts[1..])
            .current_dir(dir)
            .output()
            .unwrap(),
        &tr.command,
    );
}

fn run_zig_vectors(dir: &Path, tr: &TestRun) {
    let parts: Vec<&str> = tr.command.split_whitespace().collect();
    assert_eq!(parts[0], "zig");
    assert_ok(
        &Command::new(zig())
            .args(&parts[1..])
            .current_dir(dir)
            .output()
            .unwrap(),
        &tr.command,
    );
}

fn run_c_vectors(dir: &Path, tr: &TestRun) {
    if !has_tool("cc") {
        eprintln!("run_c_vectors: no cc; skipping");
        return;
    }
    run_sh(dir, &tr.command, &tr.command);
}

fn run_cpp_vectors(dir: &Path, tr: &TestRun) {
    if !has_tool("g++") {
        eprintln!("run_cpp_vectors: no g++; skipping");
        return;
    }
    run_sh(dir, &tr.command, &tr.command);
}

#[test]
fn std_boundary_vectors_rust_typed() {
    let resolved = resolved_leb_probe();
    let vectors = leb_boundary_vectors();
    let dir = scratch("boundary-rust");
    let target = fresh_rust_target("boundary-rust");
    let tr = bytemill::emit_rust_typed::emit_rust_typed_with_vectors(
        &resolved,
        &dir,
        RustEdition::default(),
        &vectors,
    )
    .unwrap()
    .expect("leb boundary vectors");
    assert_tr_count(&tr, &vectors);
    run_rust_vectors(&dir, &target, &tr);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&target);
}

#[test]
fn std_boundary_vectors_go() {
    // `go` has no `--version` flag; its version subcommand is `go version`.
    let go_installed = Command::new("go")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !go_installed {
        eprintln!("std_boundary_vectors_go: no go; skipping");
        return;
    }
    let resolved = resolved_leb_probe();
    let vectors = leb_boundary_vectors();
    let dir = scratch("boundary-go");
    let tr =
        bytemill::emit_go::emit_go_with_vectors(&resolved, &dir, GoVersion::default(), &vectors)
            .unwrap()
            .expect("vectors");
    assert_tr_count(&tr, &vectors);
    run_go_vectors(&dir, &tr);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn std_boundary_vectors_python() {
    let resolved = resolved_leb_probe();
    let vectors = leb_boundary_vectors();
    let dir = scratch("boundary-py");
    let tr = bytemill::emit_python::emit_python_with_vectors(
        &resolved,
        &dir,
        PythonVersion::default(),
        &vectors,
    )
    .unwrap()
    .expect("vectors");
    assert_tr_count(&tr, &vectors);
    run_python_vectors(&dir, &tr);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn std_boundary_vectors_java() {
    let resolved = resolved_leb_probe();
    let vectors = leb_boundary_vectors();
    let dir = scratch("boundary-java");
    let tr = bytemill::emit_java::emit_java_with_vectors(
        &resolved,
        &dir,
        JavaVersion::default(),
        &vectors,
    )
    .unwrap()
    .expect("vectors");
    assert_tr_count(&tr, &vectors);
    if !run_java_vectors(&dir, &tr) {
        eprintln!("std_boundary_vectors_java: no java; skipping run");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn std_boundary_vectors_csharp() {
    let resolved = resolved_leb_probe();
    let vectors = leb_boundary_vectors();
    let dir = scratch("boundary-cs");
    let tr = bytemill::emit_csharp::emit_csharp_with_vectors(
        &resolved,
        &dir,
        CSharpVersion::default(),
        &vectors,
    )
    .unwrap()
    .expect("vectors");
    assert_tr_count(&tr, &vectors);
    if !run_csharp_vectors(&dir, &tr) {
        eprintln!("std_boundary_vectors_csharp: no dotnet; skipping run");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn std_boundary_vectors_c() {
    let resolved = resolved_leb_probe();
    let vectors = leb_boundary_vectors();
    for (std, tag) in [
        (CStandard::default(), "c11"),
        (CStandard::C89, "c89"),
        (CStandard::C23, "c23"),
    ] {
        let dir = scratch(&format!("boundary-{tag}"));
        let tr = bytemill::emit_c::emit_c_with_vectors(&resolved, &dir, std, &vectors)
            .unwrap()
            .expect("vectors");
        assert_tr_count(&tr, &vectors);
        run_c_vectors(&dir, &tr);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn std_boundary_vectors_cpp() {
    let resolved = resolved_leb_probe();
    let vectors = leb_boundary_vectors();
    for (std, tag) in [
        (CppStandard::default(), "cpp20"),
        (CppStandard::Cpp98, "cpp98"),
        (CppStandard::Cpp23, "cpp23"),
    ] {
        let dir = scratch(&format!("boundary-{tag}"));
        let tr = bytemill::emit_cpp::emit_cpp_with_vectors(&resolved, &dir, std, &vectors)
            .unwrap()
            .expect("vectors");
        assert_tr_count(&tr, &vectors);
        run_cpp_vectors(&dir, &tr);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn std_boundary_vectors_typescript() {
    if !has_tool("node") {
        eprintln!("std_boundary_vectors_typescript: no node; skipping");
        return;
    }
    let resolved = resolved_leb_probe();
    let vectors = leb_boundary_vectors();
    let dir = scratch("boundary-ts");
    let tr = bytemill::emit_ts::emit_typescript_with_vectors(
        &resolved,
        &dir,
        TsTarget::default(),
        &vectors,
    )
    .unwrap()
    .expect("vectors");
    assert_tr_count(&tr, &vectors);
    run_ts_vectors(&dir, &tr);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn std_boundary_vectors_zig() {
    if !std::path::Path::new(&zig()).is_file() {
        eprintln!("std_boundary_vectors_zig: zig not at {}; skipping", zig());
        return;
    }
    let resolved = resolved_leb_probe();
    let vectors = leb_boundary_vectors();
    let dir = scratch("boundary-zig");
    let tr =
        bytemill::emit_zig::emit_zig_with_vectors(&resolved, &dir, ZigVersion::default(), &vectors)
            .unwrap()
            .expect("vectors");
    assert_tr_count(&tr, &vectors);
    run_zig_vectors(&dir, &tr);
    let _ = std::fs::remove_dir_all(&dir);
}

fn bedrock_source() -> String {
    std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap()
        + &std::fs::read_to_string(repo("schemas/std/leb128.bml")).unwrap()
        + r"
encoding bstring {
    byte_len: varuint32
    value:    bytes(byte_len) as utf8
}
encoding vec3f = {
    x: f32le
    y: f32le
    z: f32le
}
packet move_player {
    runtime_id: varuint64
    position:   vec3f
    pitch:      f32le
    yaw:        f32le
    mode:       u8
    on_ground:  boolean
    tick:       varuint64
}
packet text {
    kind:        u8
    source:      bstring
    message:     bstring
    param_count: varuint32
    params:      bstring[] count = param_count
    xuid:        bstring
}
packet set_score {
    entry_id:  varint64
    objective: bstring
    score:     i32le
    delta:     varint32
    ratio:     f64le
}
"
}

fn resolved_bedrock() -> bytemill::ResolvedSchema {
    bytemill::analyze(&bytemill::parse(&bedrock_source()).unwrap()).unwrap()
}

/// Generated vectors (3 packets × 6) plus hand `text` / `set_score` wires verified with `bml encode`.
fn bedrock_vectors(resolved: &bytemill::ResolvedSchema) -> Vec<Vector> {
    let set = bytemill::vectors::generate(
        resolved,
        &VectorSpec {
            per_packet: 6,
            seed: 1,
        },
    )
    .unwrap();
    assert!(
        set.skipped.is_empty(),
        "bedrock vector generator skipped: {:?}",
        set.skipped
    );
    assert_eq!(
        set.vectors.len(),
        18,
        "expected 18 generated bedrock vectors"
    );
    let mut vectors = set.vectors;
    // text: kind 1, source "Steve", message "hi", one param "x", empty xuid
    vectors.push(leb_vector("text", "0105537465766502686901017800"));
    // set_score: entry_id -2, objective "obj", score -1, delta i32::MIN, ratio 1.5
    vectors.push(leb_vector(
        "set_score",
        "03036f626affffffffffffffff0f000000000000f83f",
    ));
    vectors
}

#[derive(Clone, Copy)]
enum BedrockPrinter {
    RustTyped,
    Go,
    Python,
    Java,
    CSharp,
    C,
    Cpp,
    TypeScript,
    Zig,
}

impl BedrockPrinter {
    const ALL: [Self; 9] = [
        Self::RustTyped,
        Self::Go,
        Self::Python,
        Self::Java,
        Self::CSharp,
        Self::C,
        Self::Cpp,
        Self::TypeScript,
        Self::Zig,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::RustTyped => "rust-typed",
            Self::Go => "go",
            Self::Python => "python",
            Self::Java => "java",
            Self::CSharp => "csharp",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::TypeScript => "typescript",
            Self::Zig => "zig",
        }
    }
}

#[test]
fn bedrock_style_schema_round_trips_natively_in_all_nine_printers() {
    let resolved = resolved_bedrock();
    let vectors = bedrock_vectors(&resolved);
    assert_eq!(vectors.len(), 20);

    for printer in BedrockPrinter::ALL {
        let lang = printer.label();
        match printer {
            BedrockPrinter::RustTyped => {
                let dir = scratch(&format!("bedrock-{lang}"));
                let target = fresh_rust_target("bedrock-rust");
                let tr = bytemill::emit_rust_typed::emit_rust_typed_with_vectors(
                    &resolved,
                    &dir,
                    RustEdition::default(),
                    &vectors,
                )
                .unwrap()
                .expect("bedrock vectors");
                assert_tr_count(&tr, &vectors);
                run_rust_vectors(&dir, &target, &tr);
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_dir_all(&target);
            }
            BedrockPrinter::Go => {
                let go_installed = Command::new("go")
                    .arg("version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if !go_installed {
                    eprintln!("bedrock {lang}: no go; skipping");
                    continue;
                }
                let dir = scratch(&format!("bedrock-{lang}"));
                let tr = bytemill::emit_go::emit_go_with_vectors(
                    &resolved,
                    &dir,
                    GoVersion::default(),
                    &vectors,
                )
                .unwrap()
                .expect("bedrock vectors");
                assert_tr_count(&tr, &vectors);
                run_go_vectors(&dir, &tr);
                let _ = std::fs::remove_dir_all(&dir);
            }
            BedrockPrinter::Python => {
                let dir = scratch(&format!("bedrock-{lang}"));
                let tr = bytemill::emit_python::emit_python_with_vectors(
                    &resolved,
                    &dir,
                    PythonVersion::default(),
                    &vectors,
                )
                .unwrap()
                .expect("bedrock vectors");
                assert_tr_count(&tr, &vectors);
                run_python_vectors(&dir, &tr);
                let _ = std::fs::remove_dir_all(&dir);
            }
            BedrockPrinter::Java => {
                let dir = scratch(&format!("bedrock-{lang}"));
                let tr = bytemill::emit_java::emit_java_with_vectors(
                    &resolved,
                    &dir,
                    JavaVersion::default(),
                    &vectors,
                )
                .unwrap()
                .expect("bedrock vectors");
                assert_tr_count(&tr, &vectors);
                if !run_java_vectors(&dir, &tr) {
                    eprintln!("bedrock {lang}: no java; skipping run");
                }
                let _ = std::fs::remove_dir_all(&dir);
            }
            BedrockPrinter::CSharp => {
                let dir = scratch(&format!("bedrock-{lang}"));
                let tr = bytemill::emit_csharp::emit_csharp_with_vectors(
                    &resolved,
                    &dir,
                    CSharpVersion::default(),
                    &vectors,
                )
                .unwrap()
                .expect("bedrock vectors");
                assert_tr_count(&tr, &vectors);
                if !run_csharp_vectors(&dir, &tr) {
                    eprintln!("bedrock {lang}: no dotnet; skipping run");
                }
                let _ = std::fs::remove_dir_all(&dir);
            }
            BedrockPrinter::C => {
                for (std, tag) in [
                    (CStandard::default(), "c11"),
                    (CStandard::C89, "c89"),
                    (CStandard::C23, "c23"),
                ] {
                    let dir = scratch(&format!("bedrock-{lang}-{tag}"));
                    let tr = bytemill::emit_c::emit_c_with_vectors(&resolved, &dir, std, &vectors)
                        .unwrap()
                        .expect("bedrock vectors");
                    assert_tr_count(&tr, &vectors);
                    run_c_vectors(&dir, &tr);
                    let _ = std::fs::remove_dir_all(&dir);
                }
            }
            BedrockPrinter::Cpp => {
                for (std, tag) in [
                    (CppStandard::default(), "cpp20"),
                    (CppStandard::Cpp98, "cpp98"),
                    (CppStandard::Cpp23, "cpp23"),
                ] {
                    let dir = scratch(&format!("bedrock-{lang}-{tag}"));
                    let tr =
                        bytemill::emit_cpp::emit_cpp_with_vectors(&resolved, &dir, std, &vectors)
                            .unwrap()
                            .expect("bedrock vectors");
                    assert_tr_count(&tr, &vectors);
                    run_cpp_vectors(&dir, &tr);
                    let _ = std::fs::remove_dir_all(&dir);
                }
            }
            BedrockPrinter::TypeScript => {
                if !has_tool("node") {
                    eprintln!("bedrock {lang}: no node; skipping");
                    continue;
                }
                let dir = scratch(&format!("bedrock-{lang}"));
                let tr = bytemill::emit_ts::emit_typescript_with_vectors(
                    &resolved,
                    &dir,
                    TsTarget::default(),
                    &vectors,
                )
                .unwrap()
                .expect("bedrock vectors");
                assert_tr_count(&tr, &vectors);
                run_ts_vectors(&dir, &tr);
                let _ = std::fs::remove_dir_all(&dir);
            }
            BedrockPrinter::Zig => {
                if !std::path::Path::new(&zig()).is_file() {
                    eprintln!("bedrock {lang}: zig not at {}; skipping", zig());
                    continue;
                }
                let dir = scratch(&format!("bedrock-{lang}"));
                let tr = bytemill::emit_zig::emit_zig_with_vectors(
                    &resolved,
                    &dir,
                    ZigVersion::default(),
                    &vectors,
                )
                .unwrap()
                .expect("bedrock vectors");
                assert_tr_count(&tr, &vectors);
                run_zig_vectors(&dir, &tr);
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
}

//! The `emit_<lang>_with_vectors` entry points of the nine typed printers share one contract:
//! vectors for packets the schema has produce a test that embeds exactly those vectors, and
//! vectors that name no packet of the schema produce `Ok(None)` and no test file at all (a test
//! that embeds nothing would pass vacuously and `bml emit --vectors` would print a run hint for
//! it).

use std::path::{Path, PathBuf};

use bytemill::errors::BmlResult;
use bytemill::vectors::{TestRun, Vector};
use bytemill::{
    CSharpVersion, CStandard, CppStandard, GoVersion, JavaVersion, PythonVersion, ResolvedSchema,
    RustEdition, TsTarget, ZigVersion,
};

type Emit = Box<dyn Fn(&ResolvedSchema, &Path, &[Vector]) -> BmlResult<Option<TestRun>>>;

fn printers() -> Vec<(&'static str, Emit)> {
    vec![
        (
            "rust-typed",
            Box::new(|r, d, v| {
                bytemill::emit_rust_typed::emit_rust_typed_with_vectors(
                    r,
                    d,
                    RustEdition::default(),
                    v,
                )
            }),
        ),
        (
            "go",
            Box::new(|r, d, v| {
                bytemill::emit_go::emit_go_with_vectors(r, d, GoVersion::default(), v)
            }),
        ),
        (
            "python",
            Box::new(|r, d, v| {
                bytemill::emit_python::emit_python_with_vectors(r, d, PythonVersion::default(), v)
            }),
        ),
        (
            "java",
            Box::new(|r, d, v| {
                bytemill::emit_java::emit_java_with_vectors(r, d, JavaVersion::default(), v)
            }),
        ),
        (
            "csharp",
            Box::new(|r, d, v| {
                bytemill::emit_csharp::emit_csharp_with_vectors(r, d, CSharpVersion::default(), v)
            }),
        ),
        (
            "c",
            Box::new(|r, d, v| {
                bytemill::emit_c::emit_c_with_vectors(r, d, CStandard::default(), v)
            }),
        ),
        (
            "cpp",
            Box::new(|r, d, v| {
                bytemill::emit_cpp::emit_cpp_with_vectors(r, d, CppStandard::default(), v)
            }),
        ),
        (
            "typescript",
            Box::new(|r, d, v| {
                bytemill::emit_ts::emit_typescript_with_vectors(r, d, TsTarget::default(), v)
            }),
        ),
        (
            "zig",
            Box::new(|r, d, v| {
                bytemill::emit_zig::emit_zig_with_vectors(r, d, ZigVersion::default(), v)
            }),
        ),
    ]
}

fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("bml-vectors-contract-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn schema() -> ResolvedSchema {
    bytemill::analyze(&bytemill::parse("packet p { a: u8 }").unwrap()).unwrap()
}

fn vector(packet: &str) -> Vector {
    Vector {
        packet: packet.to_string(),
        route: None,
        hex: "07".to_string(),
    }
}

/// Every file under `dir`, relative, with `/` separators.
fn files_under(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap();
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn vectors_naming_no_packet_give_none_and_no_test_file() {
    let resolved = schema();
    let mut checked = 0usize;
    for (name, emit) in printers() {
        let dir = scratch(&format!("none-{name}"));
        let got = emit(&resolved, &dir, &[vector("nosuch")]).unwrap();
        assert!(
            got.is_none(),
            "{name}: vectors naming no packet must give None, got {got:?}"
        );
        let test_like: Vec<String> = files_under(&dir)
            .into_iter()
            .filter(|f| {
                let lower = f.to_lowercase();
                lower.contains("vector") || lower.ends_with("check.cs")
            })
            .collect();
        assert!(
            test_like.is_empty(),
            "{name}: no test file may be written, found {test_like:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        checked += 1;
    }
    assert_eq!(checked, 9, "all nine typed printers are covered");
}

#[test]
fn vectors_naming_a_packet_give_a_test_embedding_exactly_those() {
    let resolved = schema();
    let mut checked = 0usize;
    for (name, emit) in printers() {
        let dir = scratch(&format!("some-{name}"));
        // Two of the three name a packet of the schema; the unknown one is not embedded.
        let vectors = [vector("p"), vector("nosuch"), vector("p")];
        let run = emit(&resolved, &dir, &vectors)
            .unwrap()
            .unwrap_or_else(|| panic!("{name}: vectors for a real packet must give a test"));
        assert_eq!(run.vectors, 2, "{name}: embedded vector count");
        assert!(
            dir.join(&run.file).is_file(),
            "{name}: the reported test file {} must exist",
            run.file
        );
        assert!(!run.command.is_empty(), "{name}: a run command is reported");
        let _ = std::fs::remove_dir_all(&dir);
        checked += 1;
    }
    assert_eq!(checked, 9, "all nine typed printers are covered");
}

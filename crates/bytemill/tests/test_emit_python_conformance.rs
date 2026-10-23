//! Python 3.8 / 3.9 emission: dataclass field order and legacy syntax.

use std::path::PathBuf;
use std::process::Command;

fn repo(rel: &str) -> PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bml-py-conf-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn py_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.join("bmlgen")];
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

fn py_compile(dir: &std::path::Path) {
    let out = Command::new("python3")
        .args(["-m", "compileall", "-q", "bmlgen"])
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

fn import_bmlgen(dir: &std::path::Path) {
    let out = Command::new("python3")
        .args(["-c", "import bmlgen"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "import bmlgen failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_vectors(dir: &std::path::Path, tr: &bytemill::vectors::TestRun) {
    let args: Vec<&str> = tr.command.split_whitespace().collect();
    assert!(!args.is_empty(), "empty command");
    let prog = args[0];
    let rest = &args[1..];
    let out = Command::new(prog)
        .args(rest)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "test_vectors failed\ncmd: {}\n{}\n{}",
        tr.command,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `ast.parse(..., feature_version=(3, minor))` on every emitted module.
fn ast_parse_tree(dir: &std::path::Path, minor: u8) {
    let files = py_files(dir);
    assert!(
        files.len() >= 3,
        "expected at least 3 .py files under bmlgen, got {}",
        files.len()
    );
    const SCRIPT: &str = r"
import ast
import pathlib
import sys

minor = int(sys.argv[1])
root = pathlib.Path('bmlgen')
paths = sorted(root.rglob('*.py'))
if len(paths) < 3:
    raise SystemExit(f'too few files: {len(paths)}')
for path in paths:
    ast.parse(path.read_text(encoding='utf-8'), feature_version=(3, minor))
print(len(paths))
";
    let minor_s = minor.to_string();
    let out = Command::new("python3")
        .args(["-c", SCRIPT, &minor_s])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ast.parse feature_version 3.{} failed\n{}\n{}",
        minor,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let n = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<usize>()
        .unwrap_or(0);
    assert!(
        n >= 3,
        "ast.parse must check at least 3 files, parsed {}",
        n
    );
}

fn hytale_resolved() -> bytemill::ResolvedSchema {
    bytemill::analyze(&bytemill::load_schema(&[repo("schemas/hytale/2026_02_17.bml")]).unwrap())
        .unwrap()
}

fn hytale_vector_list(resolved: &bytemill::ResolvedSchema) -> Vec<bytemill::vectors::Vector> {
    let vs =
        bytemill::vectors::generate(resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    assert!(
        !vs.vectors.is_empty(),
        "hytale must produce at least one vector"
    );
    vs.vectors
}

#[test]
fn python_hytale_vectors_py38_import_and_roundtrip() {
    let resolved = hytale_resolved();
    let vectors = hytale_vector_list(&resolved);
    let dir = scratch("hytale-py38");
    let some = bytemill::emit_python::emit_python_with_vectors(
        &resolved,
        &dir,
        bytemill::PythonVersion::Py38,
        &vectors,
    )
    .unwrap();
    let tr = some.expect("vectors present");
    assert!(tr.vectors >= 1, "must embed at least one vector");
    py_compile(&dir);
    import_bmlgen(&dir);
    run_vectors(&dir, &tr);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn python_hytale_vectors_py39_import_and_roundtrip() {
    let resolved = hytale_resolved();
    let vectors = hytale_vector_list(&resolved);
    let dir = scratch("hytale-py39");
    let some = bytemill::emit_python::emit_python_with_vectors(
        &resolved,
        &dir,
        bytemill::PythonVersion::Py39,
        &vectors,
    )
    .unwrap();
    let tr = some.expect("vectors present");
    assert!(tr.vectors >= 1, "must embed at least one vector");
    py_compile(&dir);
    import_bmlgen(&dir);
    run_vectors(&dir, &tr);
    let _ = std::fs::remove_dir_all(&dir);
}

const LEGACY_SCHEMAS: &[(&str, &str)] = &[
    ("hytale", "schemas/hytale/2026_02_17.bml"),
    ("mc26", "schemas/minecraft_26_3_wiki.bml"),
    ("v1_13", "schemas/versions/v1_13.bml"),
];

fn emit_schema(dir: &std::path::Path, schema: &str, ver: bytemill::PythonVersion) {
    let resolved = bytemill::analyze(&bytemill::load_schema(&[repo(schema)]).unwrap()).unwrap();
    bytemill::emit_python::emit_python_with_version(&resolved, dir, ver).unwrap();
}

#[test]
fn python_legacy_ast_parse_py38_on_key_schemas() {
    for (name, schema) in LEGACY_SCHEMAS {
        let dir = scratch(&format!("ast38-{name}"));
        emit_schema(&dir, schema, bytemill::PythonVersion::Py38);
        ast_parse_tree(&dir, 8);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn python_legacy_ast_parse_py39_on_key_schemas() {
    for (name, schema) in LEGACY_SCHEMAS {
        let dir = scratch(&format!("ast39-{name}"));
        emit_schema(&dir, schema, bytemill::PythonVersion::Py39);
        ast_parse_tree(&dir, 9);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

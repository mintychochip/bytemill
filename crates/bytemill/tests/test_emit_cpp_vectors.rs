//! Vectors integration for C++: roundtrips on generated+hand vectors (incl. f32le/f64le),
//! strict consume checks, negative (truncated/trailing) cases that exit non-zero, and
//! empty-vectors produces no test file and identical package to without.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bml-cpp-vec-{name}-{}", std::process::id()));
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

fn run_from(dir: &Path, cmd: &str) -> std::process::ExitStatus {
    Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .status()
        .unwrap()
}

fn file_map(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut m = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file()
                && let Some(n) = p.file_name().and_then(|s| s.to_str())
                && let Ok(b) = std::fs::read(&p)
            {
                m.insert(n.to_string(), b);
            }
        }
    }
    m
}

#[test]
fn cpp_hytale_vectors() {
    let path = repo("schemas/hytale/2026_02_17.bml");
    let resolved = bytemill::analyze(&bytemill::load_schema(&[path]).unwrap()).unwrap();
    let dir = temp("hytale");
    let set =
        bytemill::vectors::generate(&resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    let tr = bytemill::emit_cpp::emit_cpp_with_vectors(
        &resolved,
        &dir,
        bytemill::CppStandard::default(),
        &set.vectors,
    )
    .unwrap()
    .expect("hytale produced vectors");
    assert!(tr.vectors >= 1, "at least one vector embedded");
    if has_tool("g++") {
        let st = run_from(&dir, &tr.command);
        assert!(st.success(), "hytale vectors roundtrip failed");
    } else {
        eprintln!("skipping cpp vectors run (no g++)");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cpp_mc26_vectors() {
    let path = repo("schemas/minecraft_26_3_wiki.bml");
    let resolved = bytemill::analyze(&bytemill::load_schema(&[path]).unwrap()).unwrap();
    let dir = temp("mc26");
    let set =
        bytemill::vectors::generate(&resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    let tr = bytemill::emit_cpp::emit_cpp_with_vectors(
        &resolved,
        &dir,
        bytemill::CppStandard::default(),
        &set.vectors,
    )
    .unwrap()
    .expect("mc produced vectors");
    assert!(tr.vectors >= 664, "many vectors for mc");
    if has_tool("g++") {
        let st = run_from(&dir, &tr.command);
        assert!(st.success(), "mc26 vectors roundtrip failed");
    } else {
        eprintln!("skipping cpp vectors run (no g++)");
    }
}

#[test]
fn cpp_le_be_vectors() {
    let source = "packet probe { a: f32le b: f64le c: f32be d: f64be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(source).unwrap()).unwrap();
    let dir = temp("probe-le");
    let vectors = vec![bytemill::vectors::Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "0000c03f00000000000002403fc000004002000000000000".to_string(),
    }];
    let tr = bytemill::emit_cpp::emit_cpp_with_vectors(
        &resolved,
        &dir,
        bytemill::CppStandard::Cpp98,
        &vectors,
    )
    .unwrap()
    .expect("probe le/be vectors");
    assert_eq!(tr.vectors, 1);
    if has_tool("g++") {
        let st = run_from(&dir, &tr.command);
        assert!(st.success(), "le/be probe vectors failed");
    } else {
        eprintln!("skipping cpp vectors run (no g++)");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cpp_negative_trailing() {
    let source = std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap()
        + "packet probe { n: u16be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let dir = temp("neg");
    let vectors = vec![
        bytemill::vectors::Vector {
            packet: "probe".to_string(),
            route: None,
            hex: "0102".to_string(),
        },
        bytemill::vectors::Vector {
            packet: "probe".to_string(),
            route: None,
            hex: "01".to_string(),
        },
        bytemill::vectors::Vector {
            packet: "probe".to_string(),
            route: None,
            hex: "010203".to_string(),
        },
    ];
    let tr = bytemill::emit_cpp::emit_cpp_with_vectors(
        &resolved,
        &dir,
        bytemill::CppStandard::Cpp98,
        &vectors,
    )
    .unwrap()
    .expect("negative vectors");
    assert_eq!(tr.vectors, 3);
    if has_tool("g++") {
        let st = run_from(&dir, &tr.command);
        assert!(!st.success(), "negative case must exit non-zero");
    } else {
        eprintln!("skipping cpp vectors run (no g++)");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cpp_empty_vectors_none_and_identical() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bml")).unwrap()
        + "packet probe { n: varint, f: f32be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let dir1 = temp("empty-a");
    bytemill::emit_cpp::emit_cpp_with_standard(&resolved, &dir1, bytemill::CppStandard::Cpp98)
        .unwrap();
    let dir2 = temp("empty-b");
    let res = bytemill::emit_cpp::emit_cpp_with_vectors(
        &resolved,
        &dir2,
        bytemill::CppStandard::Cpp98,
        &[],
    )
    .unwrap();
    assert!(res.is_none(), "empty vectors yields None");
    let f1 = file_map(&dir1);
    let f2 = file_map(&dir2);
    assert_eq!(f1, f2, "package bytes identical for empty vectors");
    let _ = std::fs::remove_dir_all(&dir1);
    let _ = std::fs::remove_dir_all(&dir2);
}

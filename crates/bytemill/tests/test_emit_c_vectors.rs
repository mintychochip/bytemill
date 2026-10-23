//! Vectors roundtrips for `emit c` (F3): the printer emits vectors_test.c
//! that bakes the vectors and runs native decode/encode/compare with strict
//! consumption check. The test file and command are std-correct (c89 uses no
//! pedantic, block decls, /* */).

use std::path::Path;
use std::path::PathBuf;

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn analyzed(rel: &str) -> bytemill::ResolvedSchema {
    bytemill::analyze(&bytemill::load_schema(&[repo(rel)]).unwrap()).unwrap()
}

fn emit_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bml-c-vec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn has_tool(tool: &str) -> bool {
    std::process::Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_cmd(dir: &Path, cmd: &str) -> std::process::ExitStatus {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .status()
        .unwrap()
}

fn probe_program(text: &str) -> (bytemill::ResolvedSchema, bytemill::ir::typed::Program) {
    let std = std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap()
        + &std::fs::read_to_string(repo("schemas/std/varint.bml")).unwrap();
    let resolved = bytemill::analyze(&bytemill::parse(&(std + text)).unwrap()).unwrap();
    let program = bytemill::ir::typed(&resolved).unwrap();
    (resolved, program)
}

#[test]
fn c_vectors_hytale() {
    if !has_tool("cc") {
        eprintln!("c_vectors_hytale: no cc; skipping execution");
        return;
    }
    let resolved = analyzed("schemas/hytale/2026_02_17.bml");
    let set =
        bytemill::vectors::generate(&resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    let dir = emit_dir("hytale");
    let tr = bytemill::emit_c::emit_c_with_vectors(
        &resolved,
        &dir,
        bytemill::CStandard::C11,
        &set.vectors,
    )
    .unwrap()
    .unwrap();
    assert!(tr.vectors >= 1);
    let st = run_cmd(&dir, &tr.command);
    assert!(st.success());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn c_vectors_minecraft() {
    if !has_tool("cc") {
        eprintln!("c_vectors_minecraft: no cc; skipping execution");
        return;
    }
    let resolved = analyzed("schemas/minecraft_26_3_wiki.bml");
    let set =
        bytemill::vectors::generate(&resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    let dir = emit_dir("mc");
    let tr = bytemill::emit_c::emit_c_with_vectors(
        &resolved,
        &dir,
        bytemill::CStandard::C11,
        &set.vectors,
    )
    .unwrap()
    .unwrap();
    assert!(tr.vectors >= 600);
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&tr.command)
        .current_dir(&dir)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("vectors ok"), "{}", out);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn c_vectors_le_be_probe() {
    if !has_tool("cc") {
        eprintln!("c_vectors_le_be_probe: no cc; skipping execution");
        return;
    }
    let (resolved, _p) = probe_program(
        r#"
packet probe {
    a: f32le
    b: f64le
    c: f32be
    d: f64be
}
"#,
    );
    let vectors = vec![bytemill::vectors::Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "0000c03f00000000000002403fc000004002000000000000".to_string(),
    }];
    let dir = emit_dir("probele");
    let tr =
        bytemill::emit_c::emit_c_with_vectors(&resolved, &dir, bytemill::CStandard::C11, &vectors)
            .unwrap()
            .unwrap();
    let st = run_cmd(&dir, &tr.command);
    assert!(st.success());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn c_vectors_negative_trunc_trail() {
    if !has_tool("cc") {
        eprintln!("c_vectors_negative_trunc_trail: no cc; skipping execution");
        return;
    }
    let (resolved, _p) = probe_program(
        r#"
packet probe {
    n: u16be
}
"#,
    );
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
    let dir = emit_dir("neg");
    let tr =
        bytemill::emit_c::emit_c_with_vectors(&resolved, &dir, bytemill::CStandard::C11, &vectors)
            .unwrap()
            .unwrap();
    let st = run_cmd(&dir, &tr.command);
    assert!(!st.success());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn c_vectors_empty_none_and_identical_files() {
    let resolved = analyzed("schemas/std/varint.bml");
    let dir1 = emit_dir("e1");
    bytemill::emit_c::emit_c_with_standard(&resolved, &dir1, bytemill::CStandard::C11).unwrap();
    let mut fs1: Vec<String> = std::fs::read_dir(&dir1)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    fs1.sort();
    let dir2 = emit_dir("e2");
    let ret =
        bytemill::emit_c::emit_c_with_vectors(&resolved, &dir2, bytemill::CStandard::C11, &[])
            .unwrap();
    assert!(ret.is_none());
    let mut fs2: Vec<String> = std::fs::read_dir(&dir2)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    fs2.sort();
    assert_eq!(fs1, fs2);
    for name in &fs1 {
        let b1 = std::fs::read(dir1.join(name)).unwrap();
        let b2 = std::fs::read(dir2.join(name)).unwrap();
        assert_eq!(b1, b2, "{}", name);
    }
    let _ = std::fs::remove_dir_all(&dir1);
    let _ = std::fs::remove_dir_all(&dir2);
}

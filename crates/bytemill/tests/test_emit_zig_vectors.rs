//! `emit zig --vectors` ships `vectors_test.zig` (roundtrips with embedded
//! vectors as literals). Only packets that have a codec participate. Decode is
//! strict: after decode, reader.pos must equal payload len. Re-encode must
//! match input bytes. Works for Zig 0.11-0.14; emitted source is zig-fmt clean.

use std::path::Path;
use std::process::Command;

use bytemill::vectors::{Vector, VectorSpec};

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn zig() -> String {
    std::env::var("ZIG").unwrap_or_else(|_| "/tmp/zig-x86_64-linux-0.14.1/zig".into())
}

fn run_zig(dir: &Path, tool_args: &[&str], file: &str) {
    let cache_dir = dir.join("zig-cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let run = Command::new(zig())
        .args(tool_args)
        .arg(file)
        .current_dir(dir)
        .env("ZIG_LOCAL_CACHE_DIR", &cache_dir)
        .env("ZIG_GLOBAL_CACHE_DIR", &cache_dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{file} failed\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

fn run_zig_fail(dir: &Path, tool_args: &[&str], file: &str) {
    let cache_dir = dir.join("zig-cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let run = Command::new(zig())
        .args(tool_args)
        .arg(file)
        .current_dir(dir)
        .env("ZIG_LOCAL_CACHE_DIR", &cache_dir)
        .env("ZIG_GLOBAL_CACHE_DIR", &cache_dir)
        .output()
        .unwrap();
    assert!(
        !run.status.success(),
        "{file} unexpectedly passed\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

fn written(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("zig"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            let content = std::fs::read(&p).unwrap();
            (name, content)
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn emit_vectors(
    resolved: &bytemill::ResolvedSchema,
    dir: &Path,
    vectors: &[Vector],
) -> Option<bytemill::vectors::TestRun> {
    let _ = std::fs::remove_dir_all(dir);
    bytemill::emit_zig::emit_zig_with_vectors(
        resolved,
        dir,
        bytemill::ZigVersion::default(),
        vectors,
    )
    .unwrap()
}

#[test]
fn zig_vectors_empty_is_identical_to_with_version() {
    let source = "packet p { x: u8 }\n";
    let resolved = bytemill::analyze(&bytemill::parse(source).unwrap()).unwrap();
    let dir = std::env::temp_dir().join(format!("bml-zig-vec-id-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    bytemill::emit_zig::emit_zig_with_version(&resolved, &dir, bytemill::ZigVersion::V0_14)
        .unwrap();
    let before = written(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    let res = bytemill::emit_zig::emit_zig_with_vectors(
        &resolved,
        &dir,
        bytemill::ZigVersion::V0_14,
        &[],
    )
    .unwrap();
    assert!(res.is_none());
    let after = written(&dir);
    assert_eq!(before, after);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zig_vectors_floats_roundtrip() {
    let source = "packet probe { a: f32le b: f64le c: f32be d: f64be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(source).unwrap()).unwrap();
    // f32le 1.5, f64le 2.25, f32be 1.5, f64be 2.25
    let hex = "0000c03f00000000000002403fc000004002000000000000";
    let v = Vector {
        packet: "probe".into(),
        route: None,
        hex: hex.into(),
    };
    let dir = std::env::temp_dir().join(format!("bml-zig-vec-float-{}", std::process::id()));
    let tr = emit_vectors(&resolved, &dir, &[v]).unwrap();
    assert_eq!(tr.vectors, 1);
    run_zig(&dir, &["fmt", "--check"], "vectors_test.zig");
    run_zig(&dir, &["test"], "vectors_test.zig");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zig_vectors_u16be_negatives() {
    let integers = std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap();
    let source = integers + "packet probe { n: u16be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let good = Vector {
        packet: "probe".into(),
        route: None,
        hex: "0102".into(),
    };
    let short = Vector {
        packet: "probe".into(),
        route: None,
        hex: "01".into(),
    };
    let trail = Vector {
        packet: "probe".into(),
        route: None,
        hex: "010203".into(),
    };
    // good passes
    let dirg = std::env::temp_dir().join(format!("bml-zig-vec-u16g-{}", std::process::id()));
    let trg = emit_vectors(&resolved, &dirg, &[good]).unwrap();
    assert_eq!(trg.vectors, 1);
    run_zig(&dirg, &["fmt", "--check"], "vectors_test.zig");
    run_zig(&dirg, &["test"], "vectors_test.zig");
    let _ = std::fs::remove_dir_all(&dirg);
    // truncated fails
    let dirs = std::env::temp_dir().join(format!("bml-zig-vec-u16s-{}", std::process::id()));
    let _ = emit_vectors(&resolved, &dirs, &[short]).unwrap();
    run_zig_fail(&dirs, &["test"], "vectors_test.zig");
    let _ = std::fs::remove_dir_all(&dirs);
    // trailing fails strict
    let dirt = std::env::temp_dir().join(format!("bml-zig-vec-u16t-{}", std::process::id()));
    let _ = emit_vectors(&resolved, &dirt, &[trail]).unwrap();
    run_zig_fail(&dirt, &["test"], "vectors_test.zig");
    let _ = std::fs::remove_dir_all(&dirt);
}

#[test]
fn zig_hytale_vectors() {
    let resolved = bytemill::analyze(
        &bytemill::load_schema(&[repo("schemas/hytale/2026_02_17.bml")]).unwrap(),
    )
    .unwrap();
    let set = bytemill::vectors::generate(&resolved, &VectorSpec::default()).unwrap();
    assert!(!set.vectors.is_empty(), "at least one vector for hytale");
    let dir = std::env::temp_dir().join(format!("bml-zig-vec-hytale-{}", std::process::id()));
    let tr = emit_vectors(&resolved, &dir, &set.vectors).unwrap();
    assert!(tr.vectors >= 1);
    run_zig(&dir, &["fmt", "--check"], "vectors_test.zig");
    run_zig(&dir, &["test"], "vectors_test.zig");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zig_minecraft_vectors() {
    let resolved = bytemill::analyze(
        &bytemill::load_schema(&[repo("schemas/minecraft_26_3_wiki.bml")]).unwrap(),
    )
    .unwrap();
    let set = bytemill::vectors::generate(&resolved, &VectorSpec::default()).unwrap();
    let dir = std::env::temp_dir().join(format!("bml-zig-vec-mc-{}", std::process::id()));
    let tr = emit_vectors(&resolved, &dir, &set.vectors).unwrap();
    assert!(tr.vectors >= 50);
    run_zig(&dir, &["fmt", "--check"], "vectors_test.zig");
    run_zig(&dir, &["test"], "vectors_test.zig");
    let _ = std::fs::remove_dir_all(&dir);
}

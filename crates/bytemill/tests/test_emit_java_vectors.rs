//! `emit java --vectors` round-trip tests (new file per W3 contract).
//! Copies minimal harness helpers; follows toolchain-skip.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bytemill::JavaVersion;
use bytemill::vectors::{TestRun, Vector, VectorSpec, generate};

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bml-java-vec-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|exe| exe.is_file())
}

fn java_major(java: &Path) -> Option<u32> {
    let out = std::process::Command::new(java)
        .arg("-version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    let quote = text.split('"').nth(1)?;
    let mut parts = quote.split('.');
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

fn run_command(dir: &Path, tr: &TestRun) -> Option<bool> {
    let java = find_on_path("java")?;
    if java_major(&java).unwrap_or(0) < 22 {
        return None;
    }
    let shell = "sh";
    let output = std::process::Command::new(shell)
        .arg("-c")
        .arg(&tr.command)
        .current_dir(dir)
        .output()
        .ok()?;
    Some(output.status.success())
}
#[test]
fn java_vectors_hytale() {
    let resolved = bytemill::analyze(
        &bytemill::load_schema(&[repo("schemas/hytale/2026_02_17.bml")]).unwrap(),
    )
    .unwrap();
    let set = generate(&resolved, &VectorSpec::default()).unwrap();
    assert!(!set.vectors.is_empty());
    let dir = scratch("hytale");
    let tr = bytemill::emit_java::emit_java_with_vectors(
        &resolved,
        &dir,
        JavaVersion::Java21,
        &set.vectors,
    )
    .unwrap()
    .expect("some vectors");
    assert!(tr.vectors >= 1);
    if let Some(ok) = run_command(&dir, &tr) {
        assert!(ok, "hytale vectors run");
    } else {
        eprintln!("java_vectors_hytale: no suitable java; skipping");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn java_vectors_minecraft() {
    let resolved = bytemill::analyze(
        &bytemill::load_schema(&[repo("schemas/minecraft_26_3_wiki.bml")]).unwrap(),
    )
    .unwrap();
    let set = generate(&resolved, &VectorSpec::default()).unwrap();
    let dir = scratch("mc26");
    let tr = bytemill::emit_java::emit_java_with_vectors(
        &resolved,
        &dir,
        JavaVersion::Java21,
        &set.vectors,
    )
    .unwrap()
    .expect("some");
    assert!(tr.vectors >= 50);
    if let Some(ok) = run_command(&dir, &tr) {
        assert!(ok, "mc vectors run");
    } else {
        eprintln!("java_vectors_minecraft: no suitable java; skipping");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn java_vectors_le_be_floats() {
    let source = "packet probe { a: f32le b: f64le c: f32be d: f64be }";
    let resolved = bytemill::analyze(&bytemill::parse(source).unwrap()).unwrap();
    let vecs = vec![Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "0000c03f00000000000002403fc000004002000000000000".to_string(),
    }];
    let dir = scratch("probe-lebe");
    let tr =
        bytemill::emit_java::emit_java_with_vectors(&resolved, &dir, JavaVersion::Java21, &vecs)
            .unwrap()
            .expect("one");
    assert_eq!(tr.vectors, 1);
    if let Some(ok) = run_command(&dir, &tr) {
        assert!(ok);
    } else {
        eprintln!("java_vectors_le_be_floats: no suitable java; skipping");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn java_vectors_negative_strict() {
    let source = std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap()
        + r#"packet probe { n: u16be }"#;
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    {
        let goods = vec![Vector {
            packet: "probe".to_string(),
            route: None,
            hex: "0102".to_string(),
        }];
        let dir = scratch("neg-ok");
        let tr = bytemill::emit_java::emit_java_with_vectors(
            &resolved,
            &dir,
            JavaVersion::Java21,
            &goods,
        )
        .unwrap()
        .expect("one");
        if let Some(ok) = run_command(&dir, &tr) {
            assert!(ok);
        } else {
            eprintln!("neg ok: skip");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    // trunc
    {
        let bads = vec![Vector {
            packet: "probe".to_string(),
            route: None,
            hex: "01".to_string(),
        }];
        let dir = scratch("neg-trunc");
        let tr = bytemill::emit_java::emit_java_with_vectors(
            &resolved,
            &dir,
            JavaVersion::Java21,
            &bads,
        )
        .unwrap()
        .expect("one");
        if let Some(ok) = run_command(&dir, &tr) {
            assert!(!ok, "trunc must fail");
        } else {
            eprintln!("neg trunc: skip");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    // trail
    {
        let bads = vec![Vector {
            packet: "probe".to_string(),
            route: None,
            hex: "010203".to_string(),
        }];
        let dir = scratch("neg-trail");
        let tr = bytemill::emit_java::emit_java_with_vectors(
            &resolved,
            &dir,
            JavaVersion::Java21,
            &bads,
        )
        .unwrap()
        .expect("one");
        if let Some(ok) = run_command(&dir, &tr) {
            assert!(!ok, "trail must fail");
        } else {
            eprintln!("neg trail: skip");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn java_vectors_empty_matches_version() {
    let source = "packet p { x: u8 }";
    let resolved = bytemill::analyze(&bytemill::parse(source).unwrap()).unwrap();
    let d1 = scratch("empty1");
    let r = bytemill::emit_java::emit_java_with_vectors(&resolved, &d1, JavaVersion::Java8, &[])
        .unwrap();
    assert!(r.is_none());
    let d2 = scratch("empty2");
    bytemill::emit_java::emit_java_with_version(&resolved, &d2, JavaVersion::Java8).unwrap();
    fn bml_files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut m = BTreeMap::new();
        let pkg = dir.join("bmlgen");
        if let Ok(rd) = std::fs::read_dir(&pkg) {
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.extension().is_some_and(|e| e == "java") {
                    let nm = p.file_name().unwrap().to_string_lossy().to_string();
                    m.insert(nm, std::fs::read(&p).unwrap());
                }
            }
        }
        m
    }
    assert_eq!(bml_files(&d1), bml_files(&d2));
    let _ = std::fs::remove_dir_all(&d1);
    let _ = std::fs::remove_dir_all(&d2);
}

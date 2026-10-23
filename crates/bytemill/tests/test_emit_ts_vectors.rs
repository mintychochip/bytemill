//! `emit typescript --vectors` integration: F7 LE floats plus the emitted
//! vectors.test.ts roundtrip harness. Tests run the generated node script via
//! --experimental-strip-types. Do not edit test_emit_ts.rs.

use std::path::Path;

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn node(dir: &Path, script: &str) -> std::process::Output {
    std::process::Command::new("node")
        .arg("--experimental-strip-types")
        .arg(script)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bml-ts-vec-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn tsc(dir: &Path) -> std::process::Output {
    std::process::Command::new("tsc")
        .args(["--strict", "--erasableSyntaxOnly", "--noEmit"])
        .current_dir(dir)
        .output()
        .unwrap()
}

fn prettier_check_vectors(dir: &Path) {
    let out = std::process::Command::new("prettier")
        .arg("--check")
        .arg("vectors.test.ts")
        .current_dir(dir)
        .output();
    match out {
        Ok(pr) => {
            assert!(
                pr.status.success(),
                "prettier --check vectors.test.ts failed\n{}",
                String::from_utf8_lossy(&pr.stderr)
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("prettier: {e}"),
    }
}

#[test]
fn ts_vectors_hytale_at_least_one() {
    let resolved = bytemill::analyze(
        &bytemill::load_schema(&[repo("schemas/hytale/2026_02_17.bml")]).unwrap(),
    )
    .unwrap();
    let set =
        bytemill::vectors::generate(&resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    let dir = scratch("hytale");
    let res = bytemill::emit_ts::emit_typescript_with_vectors(
        &resolved,
        &dir,
        bytemill::TsTarget::default(),
        &set.vectors,
    )
    .unwrap();
    let tc = tsc(&dir);
    assert!(
        tc.status.success(),
        "tsc failed\n{}",
        String::from_utf8_lossy(&tc.stderr)
    );
    prettier_check_vectors(&dir);
    let tr = res.expect("hytale vectors must yield TestRun");
    assert_eq!(tr.file, "vectors.test.ts");
    let out = node(&dir, &tr.file);
    assert!(
        out.status.success(),
        "hytale vectors failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ts_vectors_minecraft_50_plus() {
    let resolved = bytemill::analyze(
        &bytemill::load_schema(&[repo("schemas/minecraft_26_3_wiki.bml")]).unwrap(),
    )
    .unwrap();
    let set =
        bytemill::vectors::generate(&resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    let dir = scratch("mc26");
    let res = bytemill::emit_ts::emit_typescript_with_vectors(
        &resolved,
        &dir,
        bytemill::TsTarget::default(),
        &set.vectors,
    )
    .unwrap();
    let tc = tsc(&dir);
    assert!(
        tc.status.success(),
        "tsc failed\n{}",
        String::from_utf8_lossy(&tc.stderr)
    );
    prettier_check_vectors(&dir);
    let tr = res.expect("mc vectors must yield TestRun");
    assert!(tr.vectors >= 661);
    let out = node(&dir, &tr.file);
    assert!(
        out.status.success(),
        "mc vectors failed (should be 0 fails now that source bug removed)\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("debug_subscription_update"),
        "no failing entry expected"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ts_vectors_le_be_floats() {
    let source = "packet probe { a: f32le b: f64le c: f32be d: f64be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(source).unwrap()).unwrap();
    let vecs = vec![bytemill::vectors::Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "0000c03f00000000000002403fc000004002000000000000".to_string(),
    }];
    let dir = scratch("lebe");
    let res = bytemill::emit_ts::emit_typescript_with_vectors(
        &resolved,
        &dir,
        bytemill::TsTarget::default(),
        &vecs,
    )
    .unwrap();
    let tc = tsc(&dir);
    assert!(
        tc.status.success(),
        "tsc failed\n{}",
        String::from_utf8_lossy(&tc.stderr)
    );
    prettier_check_vectors(&dir);
    let tr = res.expect("lebe must produce run");
    let out = node(&dir, &tr.file);
    assert!(
        out.status.success(),
        "lebe vectors failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ts_vectors_negative_trailing_and_short() {
    let source = std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap()
        + "packet probe { n: u16be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let vecs = vec![
        bytemill::vectors::Vector {
            packet: "probe".into(),
            route: None,
            hex: "0102".into(),
        },
        bytemill::vectors::Vector {
            packet: "probe".into(),
            route: None,
            hex: "01".into(),
        },
        bytemill::vectors::Vector {
            packet: "probe".into(),
            route: None,
            hex: "010203".into(),
        },
    ];
    let dir = scratch("neg");
    let res = bytemill::emit_ts::emit_typescript_with_vectors(
        &resolved,
        &dir,
        bytemill::TsTarget::default(),
        &vecs,
    )
    .unwrap();
    let tc = tsc(&dir);
    assert!(
        tc.status.success(),
        "tsc failed\n{}",
        String::from_utf8_lossy(&tc.stderr)
    );
    prettier_check_vectors(&dir);
    let tr = res.expect("neg must produce run");
    let out = node(&dir, &tr.file);
    assert!(
        !out.status.success(),
        "negative vectors must make the script exit non-zero"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ts_vectors_empty_slice_is_none_and_identical_files() {
    let source = "packet probe { x: u8 }\n";
    let resolved = bytemill::analyze(&bytemill::parse(source).unwrap()).unwrap();
    let dir_v = scratch("ev");
    let none = bytemill::emit_ts::emit_typescript_with_vectors(
        &resolved,
        &dir_v,
        bytemill::TsTarget::default(),
        &[],
    )
    .unwrap();
    let tc_v = tsc(&dir_v);
    assert!(
        tc_v.status.success(),
        "tsc failed on empty-v\n{}",
        String::from_utf8_lossy(&tc_v.stderr)
    );
    assert!(none.is_none());
    let dir_b = scratch("eb");
    bytemill::emit_ts::emit_typescript_with_vectors(
        &resolved,
        &dir_b,
        bytemill::TsTarget::default(),
        &[],
    )
    .unwrap();
    let tc_b = tsc(&dir_b);
    assert!(
        tc_b.status.success(),
        "tsc failed on empty-base\n{}",
        String::from_utf8_lossy(&tc_b.stderr)
    );
    fn list_ts(d: &Path) -> Vec<String> {
        let mut v: Vec<_> = std::fs::read_dir(d)
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                if p.extension().and_then(|e| e.to_str()) == Some("ts") {
                    p.file_name().map(|n| n.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
            .collect();
        v.sort();
        v
    }
    let fs_v = list_ts(&dir_v);
    let fs_b = list_ts(&dir_b);
    assert_eq!(fs_v, fs_b);
    for f in &fs_v {
        let c1 = std::fs::read_to_string(dir_v.join(f)).unwrap();
        let c2 = std::fs::read_to_string(dir_b.join(f)).unwrap();
        assert_eq!(
            c1, c2,
            "file {} must be byte-identical with empty vectors",
            f
        );
    }
    let _ = std::fs::remove_dir_all(&dir_v);
    let _ = std::fs::remove_dir_all(&dir_b);
}

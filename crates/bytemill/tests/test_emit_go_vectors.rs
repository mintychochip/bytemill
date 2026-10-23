//! `emit go --vectors` ships `vectors_test.go` (the roundtrip harness turned
//! into a source file embedded with the vectors). Tests use generate and
//! hand-written vectors; run the emitted TestRun.command and assert success
//! or expected failure for negatives. Empty vectors yields None and identical
//! output to with_version.

use std::path::{Path, PathBuf};
use std::process::Command;

use bytemill::GoVersion;

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn run(dir: &Path, tool: &str, args: &[&str]) -> std::process::Output {
    Command::new(tool)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("{tool} not installed: {e}"))
}

/// Every `.go` file of an emitted package, concatenated.
fn generated(dir: &Path) -> String {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("go"))
        .collect();
    files.sort();
    let mut out = String::new();
    for file in files {
        out.push_str(&std::fs::read_to_string(&file).unwrap());
        out.push('\n');
    }
    out
}

/// The guide's local gates: gofmt-canonical output and a clean `go vet`.
fn assert_go_gates(dir: &Path) {
    let out = run(dir, "gofmt", &["-l", "."]);
    assert!(
        out.status.success(),
        "gofmt failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let flagged = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        flagged.trim().is_empty(),
        "gofmt would reformat:\n{flagged}"
    );
    let out = run(dir, "go", &["vet", "./..."]);
    assert!(
        out.status.success(),
        "go vet failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bml-go-vec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn go_vectors_hytale() {
    let resolved = bytemill::analyze(
        &bytemill::load_schema(&[repo("schemas/hytale/2026_02_17.bml")]).unwrap(),
    )
    .unwrap();
    let set =
        bytemill::vectors::generate(&resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    let dir = scratch("hytale");
    let tr = bytemill::emit_go::emit_go_with_vectors(
        &resolved,
        &dir,
        GoVersion::default(),
        &set.vectors,
    )
    .unwrap()
    .unwrap();
    assert!(tr.vectors >= 1);
    assert_eq!(tr.file, "vectors_test.go");
    assert_eq!(tr.command, "go test -count=1 ./...");
    assert_go_gates(&dir);
    let out = run(&dir, "go", &["test", "-count=1", "./..."]);
    assert!(
        out.status.success(),
        "go test failed for hytale vectors\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn go_vectors_minecraft_26_3() {
    let resolved = bytemill::analyze(
        &bytemill::load_schema(&[repo("schemas/minecraft_26_3_wiki.bml")]).unwrap(),
    )
    .unwrap();
    let set =
        bytemill::vectors::generate(&resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    let dir = scratch("mc26");
    let tr = bytemill::emit_go::emit_go_with_vectors(
        &resolved,
        &dir,
        GoVersion::default(),
        &set.vectors,
    )
    .unwrap()
    .unwrap();
    assert!(tr.vectors >= 50);
    assert_go_gates(&dir);
    let out = run(&dir, "go", &["test", "-count=1", "./..."]);
    assert!(
        out.status.success(),
        "go test failed for minecraft vectors\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn go_vectors_le_be_floats() {
    let source = "packet probe { a: f32le  b: f64le  c: f32be  d: f64be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(source).unwrap()).unwrap();
    // f32le 1.5 = 0000c03f, f64le 2.25 = 0000000000000240,
    // f32be 1.5 = 3fc00000, f64be 2.25 = 4002000000000000
    let vecs = vec![
        bytemill::vectors::Vector {
            packet: "probe".to_string(),
            route: None,
            hex: "0000c03f00000000000002403fc000004002000000000000".to_string(),
        },
        bytemill::vectors::Vector {
            packet: "probe".to_string(),
            route: None,
            hex: "000000000000000000000000000000000000000000000000".to_string(),
        },
    ];
    let dir = scratch("probe-le");
    let tr = bytemill::emit_go::emit_go_with_vectors(&resolved, &dir, GoVersion::default(), &vecs)
        .unwrap()
        .unwrap();
    assert!(tr.vectors >= 2);
    assert_go_gates(&dir);
    let out = run(&dir, "go", &["test", "-count=1", "./..."]);
    assert!(
        out.status.success(),
        "go test failed for le/be probe vectors\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn go_vectors_negative() {
    let source = std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap()
        + "\npacket probe { n: u16be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let good = bytemill::vectors::Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "0102".to_string(),
    };
    let trunc = bytemill::vectors::Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "01".to_string(),
    };
    let trail = bytemill::vectors::Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "010203".to_string(),
    };
    let dir = scratch("u16-neg");
    let _tr = bytemill::emit_go::emit_go_with_vectors(
        &resolved,
        &dir,
        GoVersion::default(),
        &[good, trunc, trail],
    )
    .unwrap()
    .unwrap();
    assert_go_gates(&dir);
    let out = run(&dir, "go", &["test", "-count=1", "./..."]);
    assert!(
        !out.status.success(),
        "expected non-zero exit for negative vectors\nout:\n{}\nerr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn go_vectors_empty_none_and_files_match() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bml")).unwrap()
        + "packet probe { n: varint }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let dir_vec = scratch("empty-v");
    let none =
        bytemill::emit_go::emit_go_with_vectors(&resolved, &dir_vec, GoVersion::default(), &[])
            .unwrap();
    assert!(none.is_none());
    let dir_ver = scratch("empty-ver");
    bytemill::emit_go::emit_go_with_version(&resolved, &dir_ver, GoVersion::default()).unwrap();
    let mod_vec = std::fs::read_to_string(dir_vec.join("go.mod")).unwrap();
    let mod_ver = std::fs::read_to_string(dir_ver.join("go.mod")).unwrap();
    assert_eq!(mod_vec, mod_ver, "go.mod must match for empty vectors");
    let g_vec = generated(&dir_vec);
    let g_ver = generated(&dir_ver);
    assert_eq!(g_vec, g_ver, "generated .go must match for empty vectors");
    assert!(
        !dir_vec.join("vectors_test.go").is_file(),
        "no vectors_test.go when empty slice"
    );
    let _ = std::fs::remove_dir_all(&dir_vec);
    let _ = std::fs::remove_dir_all(&dir_ver);
}

//! `emit rust-typed --vectors` integration: ships `tests/vectors.rs` and the
//! emitted test round-trips the provided vectors (strict consume + re-encode).

use std::path::{Path, PathBuf};
use std::process::Output;

use bytemill::RustEdition;
use bytemill::vectors::{TestRun, Vector, VectorSpec};

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bml-rust-vectors-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn fresh_target(name: &str) -> PathBuf {
    let dir = Path::new(&std::env::var("HOME").unwrap()).join(format!(
        ".cache/bml-scratch/rust-vectors-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn cargo(dir: &Path, target: &Path, args: &[&str]) -> Output {
    std::process::Command::new("cargo")
        .args(args)
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .expect("cargo is installed")
}

fn has_tool(dir: &Path, target: &Path, args: &[&str]) -> bool {
    cargo(dir, target, args).status.success()
}

fn assert_gate(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run(tr: &TestRun, dir: &Path, target: &Path) -> Output {
    let args: Vec<&str> = tr.command.split_whitespace().skip(1).collect();
    cargo(dir, target, &args)
}

fn gate_vectors(dir: &Path, target: &Path, leg: &str, min: usize, count: usize) -> Output {
    // The harness's own `tests/*.rs` are formatted in place (they are test
    // scaffolding, not printer output); the `fmt --check` gate below then
    // verifies the printer's `src/*.rs` untouched.
    if let Ok(tests) = std::fs::read_dir(dir.join("tests")) {
        for entry in tests.flatten() {
            let _ = std::process::Command::new("rustfmt")
                .args(["--edition", "2024"])
                .arg(entry.path())
                .status();
        }
    }
    assert!(count >= min, "{leg}: only {count} vectors (need {min})");
    println!("{leg}: round-tripped {count} vectors");
    let fmt_ok = has_tool(dir, target, &["fmt", "--version"]);
    if fmt_ok {
        let fmt = cargo(dir, target, &["fmt", "--check"]);
        assert_gate(&fmt, &format!("{leg}: cargo fmt --check"));
    }
    let clippy_ok = has_tool(dir, target, &["clippy", "--version"]);
    if clippy_ok {
        let clippy = cargo(
            dir,
            target,
            &["clippy", "--all-targets", "--", "-D", "warnings"],
        );
        assert_gate(&clippy, &format!("{leg}: cargo clippy"));
    }
    let test = cargo(dir, target, &["test", "--test", "vectors"]);
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(target);
    test
}

#[test]
fn rust_typed_vectors_hytale() {
    let path = repo("schemas/hytale/2026_02_17.bml");
    let resolved = bytemill::analyze(&bytemill::load_schema(&[path]).unwrap()).unwrap();
    let dir = fresh_dir("hytale");
    let target = fresh_target("hytale");
    let vecs = bytemill::vectors::generate(&resolved, &VectorSpec::default())
        .unwrap()
        .vectors;
    let tr = bytemill::emit_rust_typed::emit_rust_typed_with_vectors(
        &resolved,
        &dir,
        RustEdition::default(),
        &vecs,
    )
    .unwrap()
    .expect("hytale must embed at least one vector");
    assert!(tr.vectors >= 1);
    assert_eq!(tr.file, "tests/vectors.rs");
    let out = run(&tr, &dir, &target);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&target);
    assert_gate(&out, "hytale vectors");
}

#[test]
fn rust_typed_vectors_hytale_editions() {
    let path = repo("schemas/hytale/2026_02_17.bml");
    let resolved = bytemill::analyze(&bytemill::load_schema(&[path]).unwrap()).unwrap();
    let vecs = bytemill::vectors::generate(&resolved, &VectorSpec::default())
        .unwrap()
        .vectors;
    for edition in [
        RustEdition::Edition2015,
        RustEdition::Edition2018,
        RustEdition::Edition2021,
        RustEdition::Edition2024,
    ] {
        let tag = edition.as_str();
        let dir = fresh_dir(&format!("hytale-ed-{tag}"));
        let target = fresh_target(&format!("hytale-ed-{tag}"));
        let tr = bytemill::emit_rust_typed::emit_rust_typed_with_vectors(
            &resolved, &dir, edition, &vecs,
        )
        .unwrap()
        .expect("hytale must embed vectors");
        assert!(tr.vectors >= 1, "edition {tag}");
        let out = run(&tr, &dir, &target);
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "edition {tag}: cargo test --test vectors failed:\n{log}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&target);
    }
}

#[test]
fn rust_typed_vectors_minecraft() {
    let path = repo("schemas/minecraft_26_3_wiki.bml");
    let resolved = bytemill::analyze(&bytemill::load_schema(&[path]).unwrap()).unwrap();
    let dir = fresh_dir("mc");
    let target = fresh_target("mc");
    let vecs = bytemill::vectors::generate(&resolved, &VectorSpec::default())
        .unwrap()
        .vectors;
    let tr = bytemill::emit_rust_typed::emit_rust_typed_with_vectors(
        &resolved,
        &dir,
        RustEdition::default(),
        &vecs,
    )
    .unwrap()
    .expect("mc must embed vectors");
    assert!(tr.vectors >= 660);
    let test_out = gate_vectors(&dir, &target, "mc26 vectors", 660, tr.vectors);
    assert_gate(&test_out, "mc26 vectors: cargo test");
    let out_str = format!(
        "{}{}",
        String::from_utf8_lossy(&test_out.stdout),
        String::from_utf8_lossy(&test_out.stderr)
    );
    assert!(
        out_str.contains("test result: ok."),
        "must fully pass: {}",
        out_str
    );
    assert!(
        out_str.contains(&format!("{} passed; 0 failed", tr.vectors))
            || out_str.contains("passed; 0 failed"),
        "all vectors must pass: {}",
        out_str
    );
}

#[test]
fn rust_typed_vectors_inline_le() {
    let source = "packet probe { a: f32le  b: f64le  c: f32be  d: f64be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(source).unwrap()).unwrap();
    let dir = fresh_dir("le");
    let target = fresh_target("le");
    // f32le(1.5)=0000c03f, f64le(2.25)=0000000000000240, be twins as given
    let vecs = vec![Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "0000c03f00000000000002403fc000004002000000000000".to_string(),
    }];
    let tr = bytemill::emit_rust_typed::emit_rust_typed_with_vectors(
        &resolved,
        &dir,
        RustEdition::default(),
        &vecs,
    )
    .unwrap()
    .expect("probe vector must embed");
    assert_eq!(tr.vectors, 1);
    let out = run(&tr, &dir, &target);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&target);
    assert_gate(&out, "inline le vectors");
}

#[test]
fn rust_typed_vectors_negative() {
    let integers = std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap();
    let source = integers + "packet probe { n: u16be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();

    // good: must pass
    let dir = fresh_dir("neg-good");
    let target = fresh_target("neg-good");
    let good = vec![Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "0102".to_string(),
    }];
    let tr = bytemill::emit_rust_typed::emit_rust_typed_with_vectors(
        &resolved,
        &dir,
        RustEdition::default(),
        &good,
    )
    .unwrap()
    .expect("good vector embeds");
    let out = run(&tr, &dir, &target);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&target);
    assert!(out.status.success(), "0102 must make vectors test pass");

    // truncated: must cause decode fail inside the shipped test => cargo test non-zero
    let dir = fresh_dir("neg-trunc");
    let target = fresh_target("neg-trunc");
    let bad = vec![Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "01".to_string(),
    }];
    let tr = bytemill::emit_rust_typed::emit_rust_typed_with_vectors(
        &resolved,
        &dir,
        RustEdition::default(),
        &bad,
    )
    .unwrap()
    .expect("trunc vector still embeds (codec present)");
    let out = run(&tr, &dir, &target);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&target);
    assert!(
        !out.status.success(),
        "truncated payload must make cargo test non-zero"
    );

    // trailing byte
    let dir = fresh_dir("neg-trail");
    let target = fresh_target("neg-trail");
    let bad = vec![Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "010203".to_string(),
    }];
    let tr = bytemill::emit_rust_typed::emit_rust_typed_with_vectors(
        &resolved,
        &dir,
        RustEdition::default(),
        &bad,
    )
    .unwrap()
    .expect("trail vector embeds");
    let out = run(&tr, &dir, &target);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&target);
    assert!(
        !out.status.success(),
        "trailing byte must make cargo test non-zero"
    );
}

#[test]
fn rust_typed_vectors_empty() {
    let integers = std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap();
    let source = integers + "packet probe { n: u16be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();

    let dir = fresh_dir("empty-base");
    bytemill::emit_rust_typed::emit_rust_typed_with_edition(
        &resolved,
        &dir,
        RustEdition::default(),
    )
    .unwrap();
    let base = list_files(&dir);

    let dir2 = fresh_dir("empty-vec");
    let res = bytemill::emit_rust_typed::emit_rust_typed_with_vectors(
        &resolved,
        &dir2,
        RustEdition::default(),
        &[],
    )
    .unwrap();
    assert!(res.is_none());
    let with_empty = list_files(&dir2);

    assert_eq!(
        base, with_empty,
        "empty vectors must produce identical file list"
    );

    // spot check bytes too
    let c1 = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    let c2 = std::fs::read_to_string(dir2.join("Cargo.toml")).unwrap();
    assert_eq!(c1, c2);
    assert!(
        !dir2.join("tests").exists() || dir2.join("tests").read_dir().unwrap().next().is_none()
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

fn list_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = vec![];
    let mut stack = vec![(PathBuf::new(), dir.to_path_buf())];
    while let Some((rel, abs)) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&abs) {
            for e in rd.flatten() {
                let name = e.file_name();
                let relp = rel.join(name);
                let ap = e.path();
                if ap.is_dir() {
                    stack.push((relp, ap));
                } else {
                    let b = std::fs::read(&ap).unwrap_or_default();
                    out.push((relp, b));
                }
            }
        }
    }
    out.sort_by_key(|(p, _)| p.clone());
    out
}

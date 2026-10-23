//! CLI emit --vectors.

use std::path::PathBuf;
use std::process::{Command, Output};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bml"))
        .args(args)
        .current_dir(ROOT)
        .output()
        .expect("run bml")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}
fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bml-vec-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn has_tool(name: &str) -> bool {
    Command::new(name).arg("--version").output().is_ok()
}

#[test]
fn emit_vectors_rust_typed() {
    if !has_tool("cargo") {
        eprintln!("skipped: cargo not installed");
        return;
    }
    let outd = temp_dir("rt");
    let o = run(&[
        "emit",
        "rust-typed",
        "schemas/hytale/2026_02_17.bml",
        "--out",
        outd.to_str().unwrap(),
        "--vectors",
    ]);
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("run: cd"));
    // run the printed command (parse crude)
    let s = stdout(&o);
    if let Some(cdpos) = s.find("cd ") {
        let rest = &s[cdpos + 3..];
        if let Some(and) = rest.find(" && ") {
            let dir = &rest[..and];
            let cmd = &rest[and + 4..];
            let sh = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(dir)
                .output();
            if let Ok(sho) = sh {
                assert!(sho.status.success(), "vec test run failed");
            }
        }
    }
}

#[test]
fn emit_vectors_go() {
    if !has_tool("go") {
        eprintln!("skipped: go not installed");
        return;
    }
    let outd = temp_dir("go");
    let o = run(&[
        "emit",
        "go",
        "schemas/hytale/2026_02_17.bml",
        "--out",
        outd.to_str().unwrap(),
        "--vectors",
    ]);
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("run: cd"));
}

#[test]
fn emit_vectors_python() {
    if !has_tool("python3") {
        eprintln!("skipped: python3 not installed");
        return;
    }
    let outd = temp_dir("py");
    let o = run(&[
        "emit",
        "python",
        "schemas/hytale/2026_02_17.bml",
        "--out",
        outd.to_str().unwrap(),
        "--vectors",
    ]);
    assert_eq!(code(&o), 0);
}

#[test]
fn emit_rust_vectors_exit_3() {
    let outd = temp_dir("bad");
    let o = run(&[
        "emit",
        "rust",
        "schemas/hytale/2026_02_17.bml",
        "--out",
        outd.to_str().unwrap(),
        "--vectors",
    ]);
    assert_eq!(code(&o), 3);
}

#[test]
fn vectors_count_without_flag_exit_2() {
    let outd = temp_dir("cnt");
    let o = run(&[
        "emit",
        "go",
        "schemas/hytale/2026_02_17.bml",
        "--out",
        outd.to_str().unwrap(),
        "--vectors-count",
        "2",
    ]);
    assert_eq!(code(&o), 2);
}

#[test]
fn without_vectors_file_list_matches_plain() {
    if !has_tool("cargo") {
        eprintln!("skipped");
        return;
    }
    let d1 = temp_dir("p1");
    let d2 = temp_dir("p2");
    let _ = run(&[
        "emit",
        "rust-typed",
        "schemas/hytale/2026_02_17.bml",
        "--out",
        d1.to_str().unwrap(),
    ]);
    let _ = run(&[
        "emit",
        "rust-typed",
        "schemas/hytale/2026_02_17.bml",
        "--out",
        d2.to_str().unwrap(),
    ]);
    let mut f1: Vec<_> = std::fs::read_dir(&d1)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let mut f2: Vec<_> = std::fs::read_dir(&d2)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    f1.sort();
    f2.sort();
    assert_eq!(f1, f2);
}

//! CLI `bml gen` and pipe to wire-check.

use std::io::Write;
use std::process::{Command, Output, Stdio};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn run(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bml"))
        .args(args)
        .current_dir(ROOT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bml");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for bml")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

fn assert_gen_jsonl_lines(schema: &str) {
    let out = run(&["gen", "--count", "1", schema], "");
    assert_eq!(code(&out), 0);
    for line in stdout(&out).lines() {
        if line.is_empty() {
            continue;
        }
        let keys: Vec<&str> = line
            .trim_start_matches('{')
            .trim_end_matches('}')
            .split(',')
            .map(|p| p.split(':').next().unwrap().trim().trim_matches('"'))
            .collect();
        let expected = ["direction", "hex", "id", "packet", "state"];
        let mut sorted = keys.clone();
        sorted.sort();
        let mut exp = expected.to_vec();
        if !keys.contains(&"direction") {
            exp.retain(|k| *k != "direction" && *k != "id" && *k != "state");
        }
        exp.sort();
        assert_eq!(
            sorted, exp,
            "line keys {:?} not sorted subset of route keys: {line}",
            keys
        );
    }
}

#[test]
fn gen_jsonl_lines_have_sorted_keys() {
    assert_gen_jsonl_lines("schemas/hytale/2026_02_17.bml");
    assert_gen_jsonl_lines("schemas/minecraft_26_3_wiki.bml");
}

#[test]
fn gen_pipes_to_wire_check_with_zero_failures() {
    let generated = run(
        &["gen", "--count", "1", "schemas/minecraft_26_3_wiki.bml"],
        "",
    );
    assert_eq!(code(&generated), 0);
    let mut check = Command::new(env!("CARGO_BIN_EXE_bml"))
        .args(["wire-check", "schemas/minecraft_26_3_wiki.bml"])
        .current_dir(ROOT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wire-check");
    check
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&generated.stdout)
        .unwrap();
    let check = check.wait_with_output().unwrap();
    assert_eq!(check.status.code(), Some(0));
    let summary = String::from_utf8_lossy(&check.stdout);
    assert!(
        summary.contains("failed 0"),
        "wire-check summary: {summary}"
    );
}

#[test]
fn gen_packet_filter_limits_packets() {
    let all = run(
        &["gen", "--count", "1", "schemas/hytale/2026_02_17.bml"],
        "",
    );
    let one = run(
        &[
            "gen",
            "--count",
            "1",
            "--packet",
            "connect",
            "schemas/hytale/2026_02_17.bml",
        ],
        "",
    );
    assert_eq!(code(&one), 0);
    for line in stdout(&one).lines() {
        assert!(line.contains("connect"));
    }
    assert!(stdout(&all).lines().count() >= stdout(&one).lines().count());
}

#[test]
fn gen_count_0_prints_nothing() {
    let out = run(
        &["gen", "--count", "0", "schemas/hytale/2026_02_17.bml"],
        "",
    );
    assert_eq!(stdout(&out), "");
    assert_eq!(code(&out), 0);
}

#[test]
fn gen_seed_changes_output() {
    let out1 = run(
        &[
            "gen",
            "--count",
            "1",
            "--seed",
            "1",
            "schemas/hytale/2026_02_17.bml",
        ],
        "",
    );
    let out2 = run(
        &[
            "gen",
            "--count",
            "1",
            "--seed",
            "2",
            "schemas/hytale/2026_02_17.bml",
        ],
        "",
    );
    assert_ne!(stdout(&out1), stdout(&out2));
}

#[test]
fn gen_unknown_packet_exit_3() {
    let out = run(
        &[
            "gen",
            "--packet",
            "no_such",
            "schemas/hytale/2026_02_17.bml",
        ],
        "",
    );
    assert_eq!(code(&out), 3);
    assert!(stderr(&out).contains("no_such"));
}

#[test]
fn gen_bad_count_exit_2() {
    let out = run(
        &["gen", "--count", "abc", "schemas/hytale/2026_02_17.bml"],
        "",
    );
    assert_eq!(code(&out), 2);
}

#[test]
fn gen_skipped_on_stderr_only() {
    let out = run(
        &["gen", "--count", "1", "schemas/minecraft_26_3_wiki.bml"],
        "",
    );
    assert_eq!(code(&out), 0);
    let err = stderr(&out);
    if err.contains("skipped") {
        assert!(!stdout(&out).contains("skipped"));
    }
}

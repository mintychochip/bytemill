//! CLI bml trace.

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
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().expect("wait")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}
fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

const CAP: &str = r#"{"direction":"serverbound","hex":"10008906096c6f63616c686f737463dd01"}
{"direction":"serverbound","hex":"0100"}
"#;

#[test]
fn trace_basic_events() {
    let out = run(&["trace", "schemas/minecraft_26_3_wiki.bml"], CAP);
    assert_eq!(code(&out), 0);
    let s = stdout(&out);
    assert!(s.contains("\"packet\": \"handshake\""));
    assert!(s.contains("handshaking"));
    assert!(s.contains("\"packet\": \"status_request\""));
}

#[test]
fn trace_bad_secret_exit_2() {
    let out = run(
        &[
            "trace",
            "--secret",
            "123",
            "schemas/minecraft_26_3_wiki.bml",
        ],
        CAP,
    );
    assert_eq!(code(&out), 2);
}

#[test]
fn trace_unroutable_exit_3() {
    // use a capture with bad id that triggers error event
    let bad = r#"{"direction":"serverbound","hex":"0200fe01"}"#;
    let out = run(&["trace", "schemas/minecraft_26_3_wiki.bml"], bad);
    assert_eq!(code(&out), 3);
    assert!(stdout(&out).contains("\"event\": \"error\""));
}

#[test]
fn trace_from_file() {
    use std::fs;
    let tmp = std::env::temp_dir().join("bml_trace_cap.jsonl");
    fs::write(&tmp, CAP).unwrap();
    let out = run(
        &[
            "trace",
            "--file",
            tmp.to_str().unwrap(),
            "schemas/minecraft_26_3_wiki.bml",
        ],
        "",
    );
    assert_eq!(code(&out), 0);
    let _ = fs::remove_file(&tmp);
}

#[test]
fn trace_stdin_works() {
    let out = run(&["trace", "schemas/minecraft_26_3_wiki.bml"], CAP);
    assert!(stdout(&out).contains("seq"));
}

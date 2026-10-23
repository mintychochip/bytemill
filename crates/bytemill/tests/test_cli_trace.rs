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

#[test]
fn trace_capture_ending_inside_a_frame_exits_3() {
    // The second chunk claims a one-byte frame and ends: the capture is damaged. The events are
    // still printed (the handshake before it), and the exit status says the capture is not clean.
    let cap = r#"{"direction":"serverbound","hex":"10008906096c6f63616c686f737463dd01"}
{"direction":"serverbound","hex":"01"}
"#;
    let out = run(&["trace", "schemas/minecraft_26_3_wiki.bml"], cap);
    assert_eq!(
        code(&out),
        3,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = stdout(&out);
    assert!(s.contains("\"packet\": \"handshake\""), "{s}");
    assert!(s.contains("\"event\": \"incomplete\""), "{s}");
    assert!(s.contains("\"pending_bytes\": 1"), "{s}");
}

/// `login_finished` (id 0x02, profile "alice") framed with Python 3 `zlib.compress(body, level)`,
/// so the test does not depend on this crate's own compressor. Frame: `varint(len(inner)) +
/// inner`, `inner = varint(len(body)) + zlib`, `body = varint(2) + payload`. The levels 1, 6 and 9
/// frames carry one 100-byte property value (151 body bytes); the `78 01` / `78 9c` / `78 da`
/// headers differ by level. The level 0 frame is a stored block with no properties (40 body
/// bytes).
const PY_ZLIB_FRAMES: [(&str, &str); 4] = [
    (
        "level 0 (stored block)",
        "34287801012800d7ff020000000000000001000000000000000205616c696365000000000000000003000000000000000429330210",
    ),
    (
        "level 1",
        "2c97017801636280004628cdc49a9893999ccac851925a51525a945a9ce2480700b59c8119ca6001009ec51f65",
    ),
    (
        "level 6",
        "2c9701789c636280004628cdc49a9893999ccac851925a51525a945a9ce2480700b59c81194ab300009ec51f65",
    ),
    (
        "level 9",
        "2c970178da636280004628cdc49a9893999ccac851925a51525a945a9ce2480700b59c81194ab300009ec51f65",
    ),
];

#[test]
fn trace_and_decode_accept_zlib_frames_from_another_deflate_implementation() {
    let handshake = "10008906096c6f63616c686f737463dd02"; // intent 2: login
    let compression = "020340"; // login_compression, threshold 64
    for (label, frame) in PY_ZLIB_FRAMES {
        let cap = format!(
            "{{\"direction\":\"serverbound\",\"hex\":\"{handshake}\"}}\n\
             {{\"direction\":\"clientbound\",\"hex\":\"{compression}\"}}\n\
             {{\"direction\":\"clientbound\",\"hex\":\"{frame}\"}}\n"
        );
        let traced = run(&["trace", "schemas/minecraft_26_3_wiki.bml"], &cap);
        assert_eq!(
            code(&traced),
            0,
            "{label}: {}",
            String::from_utf8_lossy(&traced.stderr)
        );
        let s = stdout(&traced);
        assert!(s.contains("\"event\": \"compression\""), "{label}: {s}");
        assert!(s.contains("\"packet\": \"login_finished\""), "{label}: {s}");
        assert!(s.contains("\"username\": \"alice\""), "{label}: {s}");

        let decoded = run(
            &[
                "decode",
                "schemas/minecraft_26_3_wiki.bml",
                "--state",
                "login",
                "--direction",
                "clientbound",
                "--compression",
                "zlib",
                "--hex",
            ],
            &format!("{frame}\n"),
        );
        assert_eq!(
            code(&decoded),
            0,
            "{label}: {}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        assert!(
            stdout(&decoded).contains("\"username\": \"alice\""),
            "{label}: {}",
            stdout(&decoded)
        );
    }
}

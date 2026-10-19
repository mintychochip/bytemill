//! `bml decode/encode --compression zlib[:threshold]`.

use std::io::Write;
use std::process::{Command, Output, Stdio};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const SCHEMA: &str = "schemas/minecraft_26_3_wiki.bps";

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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

const HANDSHAKE: &str =
    r#"{"protocol_version":777,"server_address":"localhost","server_port":25565,"intent":1}"#;

#[test]
fn decodes_a_minecraft_compressed_handshake() {
    // `packet_length, data_length, zlib([id][payload])` for the README handshake.
    let compressed = "1910789c63e864e3ccc94f4eccc9c82f2e49becb080029c105a3";
    let output = run(
        &[
            "decode",
            SCHEMA,
            "--state",
            "handshaking",
            "--direction",
            "serverbound",
            "--compression",
            "zlib",
            "--hex",
        ],
        compressed,
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let json = stdout(&output);
    assert!(json.contains(r#""packet": "handshake""#), "{json}");
    assert!(json.contains(r#""protocol_version": 777"#), "{json}");
}

#[test]
fn encodes_and_round_trips_a_compressed_frame() {
    let encode = run(
        &[
            "encode",
            SCHEMA,
            "--packet",
            "handshake",
            "--state",
            "handshaking",
            "--direction",
            "serverbound",
            "--compression",
            "zlib:0",
            HANDSHAKE,
        ],
        "",
    );
    assert!(encode.status.success(), "{}", stderr(&encode));
    let compressed = stdout(&encode);
    assert!(!compressed.is_empty());

    let decode = run(
        &[
            "decode",
            SCHEMA,
            "--state",
            "handshaking",
            "--direction",
            "serverbound",
            "--compression",
            "zlib",
            "--hex",
        ],
        &compressed,
    );
    assert!(decode.status.success(), "{}", stderr(&decode));
    assert!(stdout(&decode).contains(r#""packet": "handshake""#));
}

#[test]
fn a_long_threshold_writes_the_raw_branch() {
    let encode = run(
        &[
            "encode",
            SCHEMA,
            "--packet",
            "handshake",
            "--state",
            "handshaking",
            "--direction",
            "serverbound",
            "--compression",
            "zlib:1000",
            HANDSHAKE,
        ],
        "",
    );
    assert!(encode.status.success(), "{}", stderr(&encode));
    let frame = stdout(&encode);
    assert_eq!(frame, "1100008906096c6f63616c686f737463dd01");

    let decode = run(
        &[
            "decode",
            SCHEMA,
            "--state",
            "handshaking",
            "--direction",
            "serverbound",
            "--compression",
            "zlib",
            "--hex",
        ],
        &frame,
    );
    assert!(decode.status.success(), "{}", stderr(&decode));
    assert!(stdout(&decode).contains(r#""packet": "handshake""#));
}

#[test]
fn rejects_non_minecraft_frames() {
    let output = run(
        &[
            "decode",
            "schemas/hytale/2026_02_17.bps",
            "--state",
            "default",
            "--direction",
            "serverbound",
            "--compression",
            "zlib",
            "--hex",
        ],
        "00",
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("Minecraft-style frames"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn compression_needs_a_frame() {
    let decode = run(
        &[
            "decode",
            SCHEMA,
            "--packet",
            "handshake",
            "--compression",
            "zlib",
            "--hex",
        ],
        "00",
    );
    assert!(!decode.status.success());
    assert!(stderr(&decode).contains("applies to frames"));

    let encode = run(
        &[
            "encode",
            SCHEMA,
            "--packet",
            "handshake",
            "--compression",
            "zlib",
            HANDSHAKE,
        ],
        "",
    );
    assert!(!encode.status.success());
    assert!(stderr(&encode).contains("needs --state and --direction"));
}

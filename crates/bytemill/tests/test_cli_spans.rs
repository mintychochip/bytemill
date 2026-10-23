//! CLI decode --spans --hexdump.

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

const HANDSHAKE_FRAME: &str = "10008906096c6f63616c686f737463dd01";
const WIKI: &str = "schemas/minecraft_26_3_wiki.bml";

fn decode_frame(flag: &str) -> Output {
    run(
        &[
            "decode",
            WIKI,
            "--state",
            "handshaking",
            "--direction",
            "serverbound",
            flag,
        ],
        HANDSHAKE_FRAME,
    )
}

#[test]
fn decode_spans_packet() {
    let out = run(
        &[
            "decode",
            "schemas/minecraft_26_3_play.bml",
            "--packet",
            "animate",
            "--spans",
        ],
        "ac0202",
    );
    assert_eq!(code(&out), 0);
    // entity_id is the two-byte VarInt 0xac 0x02 at offset 0, animation the byte after it.
    assert_eq!(
        stdout(&out),
        r#"{"fields": {"animation": 2, "entity_id": 300}, "spans": {"animation": {"len": 1, "offset": 2}, "entity_id": {"len": 2, "offset": 0}}}"#
    );
}

#[test]
fn decode_hexdump_prints_one_row_per_field() {
    let out = run(
        &[
            "decode",
            "schemas/minecraft_26_3_play.bml",
            "--packet",
            "animate",
            "--hexdump",
        ],
        "ac0202",
    );
    assert_eq!(code(&out), 0);
    assert_eq!(
        stdout(&out),
        "0000    ac 02                  entity_id = 300\n\
         0002    02                     animation = 2"
    );
}

#[test]
fn decode_frame_spans_cover_the_frame_header_and_the_payload() {
    let out = decode_frame("--spans");
    assert_eq!(code(&out), 0);
    // length at 0, id at 1, then the handshake fields in order; the header values travel too.
    assert_eq!(
        stdout(&out),
        r#"{"fields": {"intent": 1, "protocol_version": 777, "server_address": "localhost", "server_port": 25565}, "frame": {"id": 0, "length": 16}, "id": 0, "packet": "handshake", "spans": {"id": {"len": 1, "offset": 1}, "intent": {"len": 1, "offset": 16}, "length": {"len": 1, "offset": 0}, "protocol_version": {"len": 2, "offset": 2}, "server_address": {"len": 10, "offset": 4}, "server_port": {"len": 2, "offset": 14}}}"#
    );
}

#[test]
fn decode_frame_hexdump_shows_the_header_values() {
    let out = decode_frame("--hexdump");
    assert_eq!(code(&out), 0);
    assert_eq!(
        stdout(&out),
        "0000    10                     length = 16\n\
         0001    00                     id = 0\n\
         0002    89 06                  protocol_version = 777\n\
         0004    09 6c 6f 63 61 6c 68 6f ... server_address = \"localhost\"\n\
         000e    63 dd                  server_port = 25565\n\
         0010    01                     intent = 1"
    );
}

#[test]
fn plain_frame_decode_has_no_span_keys() {
    let out = run(
        &[
            "decode",
            WIKI,
            "--state",
            "handshaking",
            "--direction",
            "serverbound",
        ],
        HANDSHAKE_FRAME,
    );
    assert_eq!(code(&out), 0);
    assert_eq!(
        stdout(&out),
        r#"{"fields": {"intent": 1, "protocol_version": 777, "server_address": "localhost", "server_port": 25565}, "id": 0, "packet": "handshake"}"#
    );
}

#[test]
fn decode_spans_and_hexdump_exit_2() {
    let out = run(
        &[
            "decode",
            "schemas/minecraft_26_3_play.bml",
            "--packet",
            "animate",
            "--spans",
            "--hexdump",
        ],
        "ac0202",
    );
    assert_eq!(code(&out), 2);
}

#[test]
fn decode_no_flag_same_as_before() {
    let out = run(
        &[
            "decode",
            "schemas/minecraft_26_3_play.bml",
            "--packet",
            "animate",
        ],
        "ac0202",
    );
    assert_eq!(code(&out), 0);
    let s = stdout(&out);
    assert!(!s.contains("\"spans\""));
    assert!(s.contains("entity_id"));
}

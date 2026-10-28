//! Java emitter: direct sum payloads lifted from shared encodings must be
//! referenced in the packet wrapper class, not `Encoding.<Type>`.

use std::path::{Path, PathBuf};

use bytemill::JavaVersion;
use bytemill::vectors::{Vector, VectorRoute};

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bml-java-conform-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn direct_encoding_schema() -> String {
    let mut s = String::new();
    for rel in ["schemas/std/integers.bml", "schemas/std/varint.bml"] {
        s.push_str(&std::fs::read_to_string(repo(rel)).unwrap());
    }
    s.push_str(
        r#"
encoding string(max_chars = 32767) {
    byte_len: varint
    value:    bytes(byte_len) as utf8
    require   utf16_units(value) <= max_chars
}
"#,
    );
    s.push_str(
        &std::fs::read_to_string(repo(
            "crates/bytemill/tests/fixtures/java_direct_encoding/shared.bml",
        ))
        .unwrap(),
    );
    s.push_str(
        &std::fs::read_to_string(repo(
            "crates/bytemill/tests/fixtures/java_direct_encoding/play.bml",
        ))
        .unwrap(),
    );
    s
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

fn run_vectors(dir: &Path, command: &str) -> bool {
    let Some(java) = find_on_path("java") else {
        return false;
    };
    if java_major(&java).unwrap_or(0) < 22 {
        return false;
    }
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "vectors run failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

#[test]
fn java_direct_encoding_payload_owner_matches_packet_wrapper() {
    let resolved = bytemill::analyze(&bytemill::parse(&direct_encoding_schema()).unwrap()).unwrap();
    let dir = scratch("direct-encoding");
    let vecs = vec![Vector {
        packet: "direct_profile_probe".to_string(),
        route: Some(VectorRoute {
            state: "play".to_string(),
            direction: "clientbound".to_string(),
            id: 0,
        }),
        // tag=1 (game_profile): empty name, zero properties.
        hex: "010000".to_string(),
    }];
    let tr =
        bytemill::emit_java::emit_java_with_vectors(&resolved, &dir, JavaVersion::Java21, &vecs)
            .unwrap()
            .expect("vectors emitted");
    assert_eq!(tr.vectors, 1);
    let play = std::fs::read_to_string(dir.join("bmlgen/PlayClientbound.java")).unwrap();
    assert!(
        play.contains("permits GameProfile,"),
        "expected unqualified direct payload in permits clause"
    );
    assert!(
        !play.contains("Encoding.GameProfile.decode")
            && !play.contains("instanceof Encoding.GameProfile")
            && !play.contains("permits Encoding.GameProfile"),
        "direct payload must not be qualified as Encoding.GameProfile"
    );
    assert!(
        play.contains("public record GameProfile("),
        "GameProfile record should live in PlayClientbound"
    );
    if run_vectors(&dir, &tr.command) {
        // round-tripped via the source launcher
    } else {
        eprintln!(
            "java_direct_encoding_payload_owner_matches_packet_wrapper: no java 22+; skipping run"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

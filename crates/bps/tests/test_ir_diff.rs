//! The IR interpreter must reproduce the reference interpreter's bytes.

use bps::interp::encode_packet;
use bps::ir::{exec_decode, exec_encode, lower};
use bps::synth::synthesize_packet;

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn first_diff(a: &[u8], b: &[u8]) -> String {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(i) => format!(
            "byte {i}: {:02x} vs {:02x} (lens {} vs {})",
            a[i], b[i], a.len(), b.len()
        ),
        None => format!("lens {} vs {}", a.len(), b.len()),
    }
}

fn proc_name(ir: &bps::ir::IrSchema, packet: &str) -> String {
    let prefixed = format!("packet:{packet}");
    if ir.decode.contains_key(&prefixed) {
        prefixed
    } else {
        packet.to_string()
    }
}

fn check(path: &str) {
    let resolved = bps::analyze(&bps::load_schema(&[repo(path)]).unwrap()).unwrap();
    let ir = lower(&resolved).expect(path);
    let mut failed = Vec::new();
    for packet in resolved.packets.values() {
        let key = proc_name(&ir, &packet.name);
        let value = match synthesize_packet(&resolved, packet) {
            Ok(v) => v,
            Err(err) => {
                failed.push(format!("{}: synth {err}", packet.name));
                continue;
            }
        };
        let bytes = match encode_packet(&resolved, &packet.name, &value) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let decoded = match exec_decode(&ir, &key, &[], &bytes) {
            Ok(v) => v,
            Err(err) => {
                failed.push(format!("{}: ir decode {err}", packet.name));
                continue;
            }
        };
        match exec_encode(&ir, &key, &[], &decoded) {
            Ok(again) if again == bytes => {}
            Ok(again) => failed.push(format!("{}: {}", packet.name, first_diff(&bytes, &again))),
            Err(err) => failed.push(format!("{}: ir encode {err}", packet.name)),
        }
    }
    assert!(
        failed.is_empty(),
        "{path}: {} packets disagree\n{}",
        failed.len(),
        failed.iter().take(12).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn wiki_26_3_matches_the_interpreter() {
    check("schemas/minecraft_26_3_wiki.bps");
}

#[test]
fn version_1_20_1_matches_the_interpreter() {
    check("schemas/versions/v1_20_1.bps");
}

//! Ports tests/test_roundtrip_all.py.

mod common;

use bytemill::interp::{decode_packet, encode_packet};
use bytemill::synth::synthesize_packet;

#[test]
fn every_packet_round_trips() {
    let resolved = common::load_minecraft().unwrap();
    let mut failures = Vec::new();
    let mut checked = 0usize;
    let mut names: Vec<&str> = resolved.packets.keys().collect();
    names.sort();
    for name in names {
        if name.ends_with("_probe") {
            continue;
        }
        let pkt = resolved.packets.get(name).unwrap();
        let value = synthesize_packet(&resolved, pkt).unwrap();
        let encoded = match encode_packet(&resolved, name, &value) {
            Ok(e) => e,
            Err(e) => {
                failures.push(format!("{name}: encode {e}"));
                continue;
            }
        };
        let decoded = match decode_packet(&resolved, name, &encoded, false) {
            Ok(d) => d,
            Err(e) => {
                // `require` assertions are schema contracts, not round-trip
                // failures: a synthetic value that violates one is expected to
                // be rejected (handshake pins protocol_version == 777).
                if e.to_string().contains("require failed") {
                    continue;
                }
                failures.push(format!("{name}: decode {e}"));
                continue;
            }
        };
        match encode_packet(&resolved, name, &decoded) {
            Ok(re) => {
                if re != encoded {
                    failures.push(format!("{name}: re-encode differs"));
                }
            }
            Err(e) => failures.push(format!("{name}: re-encode {e}")),
        }
        checked += 1;
    }
    assert_eq!(failures.into_iter().take(10).collect::<Vec<_>>(), Vec::<String>::new());
    assert!(checked >= 230, "checked {checked}");
}

#[test]
fn every_route_decodes_its_packet() {
    let resolved = common::load_minecraft().unwrap();
    let payload = vec![0u8; 8192];
    let mut bad = Vec::new();
    for (route, pkt) in &resolved.routes {
        match decode_packet(&resolved, &pkt.name, &payload, false) {
            Ok(_) => {}
            // A clean BmlError is a pass; a crash is not.
            Err(e) => {
                if e.kind != bytemill::errors::ErrorKind::Decode
                    && e.kind != bytemill::errors::ErrorKind::Encode
                {
                    bad.push(format!(
                        "{}.{} 0x{:x}: {e:?}",
                        route.state, route.direction, route.packet_id
                    ));
                }
            }
        }
    }
    assert_eq!(bad, Vec::<String>::new());
}

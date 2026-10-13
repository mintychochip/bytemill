//! Ports tests/test_java_ground_truth.py: BPS layouts against real Minestom bytes.

mod common;

/// packet -> how Minestom's bytes differ from vanilla's wire format, which
/// BPS follows.
const KNOWN_MISMATCHES: &[(&str, &str)] = &[
    (
        "delete_chat",
        "vanilla writes MessageSignature.Packed (varint id + 1, then the 256-byte signature only when that is 0); Minestom writes just the 256 bytes",
    ),
    (
        "move_minecart_along_track",
        "vanilla's MinecartStep writes yaw and pitch as ROTATION_BYTE (one byte); Minestom writes them as 4-byte floats",
    ),
];

/// Java classes that serve two BPS packets because vanilla writes them
/// differently by state.
const SPLIT_CLASSES: &[(&str, &[&str])] =
    &[("ShowDialogPacket", &["show_dialog", "show_dialog_configuration"])];

/// Packets with at least one recorded sample.
const COVERED_PACKETS: usize = 136;

#[test]
fn java_and_bps_register_the_same_ids() {
    if !common::java_probes::available() {
        eprintln!("rstom probe fixtures not present; skipping");
        return;
    }
    let resolved = common::load_minecraft().unwrap();
    let route = common::java_probes::route_packets(&resolved);
    let java_slots: std::collections::BTreeSet<(String, String, i128)> = common::java_probes::slots()
        .into_iter()
        .map(|s| (s.state, s.direction, s.packet_id))
        .collect();
    let route_keys: std::collections::BTreeSet<(String, String, i128)> =
        route.into_iter().map(|(s, d, i, _)| (s, d, i)).collect();
    let java_only: Vec<_> = java_slots.difference(&route_keys).collect();
    assert!(java_only.is_empty(), "ids Java has and BPS lacks: {java_only:?}");
    let bps_only: Vec<_> = route_keys.difference(&java_slots).collect();
    assert_eq!(
        bps_only,
        vec![&("handshaking".to_string(), "serverbound".to_string(), 0xFE)],
        "ids BPS has and Java lacks"
    );
}

#[test]
fn each_java_class_is_one_bps_packet() {
    if !common::java_probes::available() {
        eprintln!("rstom probe fixtures not present; skipping");
        return;
    }
    let resolved = common::load_minecraft().unwrap();
    let by_class = common::java_probes::packets_by_class(&resolved);
    let mut many = Vec::new();
    for (class, packets) in &by_class {
        if packets.len() != 1 && !SPLIT_CLASSES.iter().any(|(c, _)| c == class) {
            many.push((class.clone(), packets.clone()));
        }
    }
    assert!(many.is_empty(), "{many:?}");
    for (class, expected) in SPLIT_CLASSES {
        let mut expected_sorted: Vec<String> =
            expected.iter().map(|s| s.to_string()).collect();
        expected_sorted.sort_unstable();
        assert_eq!(
            by_class.iter().find(|(c, _)| c == class).map(|(_, p)| p.clone()),
            Some(expected_sorted)
        );
    }
}

#[test]
fn every_recorded_sample_belongs_to_a_bps_packet() {
    if !common::java_probes::available() {
        eprintln!("rstom probe fixtures not present; skipping");
        return;
    }
    let resolved = common::load_minecraft().unwrap();
    let by_class = common::java_probes::packets_by_class(&resolved);
    let mut orphans: Vec<String> = common::java_probes::samples()
        .into_iter()
        .map(|s| s.java_class)
        .filter(|c| !by_class.iter().any(|(k, _)| k == c))
        .collect();
    orphans.sort();
    orphans.dedup();
    assert!(orphans.is_empty(), "{orphans:?}");
}

#[test]
fn recorded_samples_still_cover_the_packets_they_did() {
    if !common::java_probes::available() {
        eprintln!("rstom probe fixtures not present; skipping");
        return;
    }
    let resolved = common::load_minecraft().unwrap();
    let by_class = common::java_probes::packets_by_class(&resolved);
    let mut covered: Vec<String> = Vec::new();
    for sample in common::java_probes::samples() {
        if let Some((_, packets)) = by_class.iter().find(|(c, _)| *c == sample.java_class) {
            for p in packets {
                if !covered.contains(p) {
                    covered.push(p.clone());
                }
            }
        }
    }
    assert!(covered.len() >= COVERED_PACKETS, "covered {}", covered.len());
}

#[test]
fn recorded_packets_round_trip_through_the_schema() {
    if !common::java_probes::available() {
        eprintln!("rstom probe fixtures not present; skipping");
        return;
    }
    let resolved = common::load_minecraft().unwrap();
    let failed = common::java_probes::failures(&resolved);
    let mut lines = Vec::new();
    for (packet, items) in &failed {
        if KNOWN_MISMATCHES.iter().any(|(p, _)| p == packet) {
            continue;
        }
        let first = &items[0];
        lines.push(format!(
            "{packet}: {} sample(s) differ, e.g. {}: {}\n    bytes {}",
            items.len(),
            first.0.label(),
            first.1,
            hex(&first.0.data)
        ));
    }
    assert!(lines.is_empty(), "\n{}", lines.join("\n"));
    // A listed packet that starts passing fails the test, so the list cannot go stale.
    let fixed: Vec<&str> = KNOWN_MISMATCHES
        .iter()
        .map(|(p, _)| *p)
        .filter(|p| !failed.iter().any(|(f, _)| f == p))
        .collect();
    assert!(fixed.is_empty(), "listed in KNOWN_MISMATCHES but now round-trip: {fixed:?}");
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

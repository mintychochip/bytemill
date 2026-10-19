//! Ports tests/test_ledger_wave1.py: committed wave-1 shape ledger invariants.

mod common;

const WAVE1_NEW_BODY_PACKETS: &[&str] = &["ClientNameItemPacket", "ClientPluginMessagePacket"];

const WAVE1_READ_VIA_NEW_PACKETS: &[&str] = &[
    "ClientClickWindowButtonPacket",
    "ClientCloseWindowPacket",
    "ClientHeldItemChangePacket",
    "ClientLockDifficultyPacket",
    "ClientNameItemPacket",
    "ClientPickItemFromEntityPacket",
    "UpdateHealthPacket",
    "UpdateSimulationDistancePacket",
    "UpdateViewDistancePacket",
    "UpdateViewPositionPacket",
    "WindowPropertyPacket",
    "WorldBorderCenterPacket",
    "WorldBorderLerpSizePacket",
    "WorldBorderSizePacket",
    "WorldBorderWarningDelayPacket",
    "WorldBorderWarningReachPacket",
];

fn ledger_present() -> bool {
    if common::committed_shapes_present() {
        return true;
    }
    eprintln!("{}", common::MISSING_SHAPES_MSG);
    false
}

fn ledger() -> Vec<(String, bytemill::shapes::Shape)> {
    bytemill::shapes::load_ledger(&common::repo_root().join("targets/rstom/shapes")).unwrap()
}

#[test]
fn non_trivial_new_body_packets() {
    if !ledger_present() {
        return;
    }
    let mut found: Vec<String> = ledger()
        .into_iter()
        .filter(|(_, s)| !s.new.body.is_empty())
        .map(|(_, s)| s.cls)
        .collect();
    found.sort();
    let mut expected = WAVE1_NEW_BODY_PACKETS.to_vec();
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn read_via_new_packets() {
    if !ledger_present() {
        return;
    }
    let mut found: Vec<String> = ledger()
        .into_iter()
        .filter(|(_, s)| s.serializer.read_via_new)
        .map(|(_, s)| s.cls)
        .collect();
    found.sort();
    let mut expected = WAVE1_READ_VIA_NEW_PACKETS.to_vec();
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn exactly_sixty_four_shapes() {
    if !ledger_present() {
        return;
    }
    assert_eq!(ledger().len(), 64);
}

//! Shape ledger invariants: every committed ledger file reserialises and
//! the special-render sets stay exactly known.

mod common;

const SHAPED_NEW_BODY_PACKETS: &[&str] = &[
    "ClientCommandChatPacket",
    "ClientNameItemPacket",
    "ClientPluginMessagePacket",
];

const SHAPED_READ_VIA_NEW_PACKETS: &[&str] = &[
    "ClientAttackPacket",
    "ClientChatAckPacket",
    "ClientChunkBatchReceivedPacket",
    "ClientClickWindowButtonPacket",
    "ClientCloseWindowPacket",
    "ClientCommandChatPacket",
    "ClientGenerateStructurePacket",
    "ClientHeldItemChangePacket",
    "ClientLockDifficultyPacket",
    "ClientNameItemPacket",
    "ClientPickItemFromBlockPacket",
    "ClientPickItemFromEntityPacket",
    "TickStepPacket",
    "UnloadChunkPacket",
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
    "WorldEventPacket",
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
    let mut expected = SHAPED_NEW_BODY_PACKETS.to_vec();
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
    let mut expected = SHAPED_READ_VIA_NEW_PACKETS.to_vec();
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn exactly_ninety_two_shapes() {
    if !ledger_present() {
        return;
    }
    assert_eq!(ledger().len(), 92);
}

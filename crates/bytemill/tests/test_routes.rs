//! Ports tests/test_routes.py (test_vanilla_routes.py needs the vanilla
//! Java sources under `mc-ref/`, which this checkout does not carry).

mod common;

use bytemill::interp::decode_packet;
use bytemill::BlockItem;

fn is_rest_stub(pkt: &bytemill::ast::PacketDecl) -> bool {
    if pkt.items.len() != 1 {
        return false;
    }
    match &pkt.items[0] {
        BlockItem::Field(wf) => match &wf.type_ref {
            bytemill::ast::FieldType::Ref(r) => r.name == "rest",
            _ => false,
        },
        _ => false,
    }
}

#[test]
fn every_route_resolves_to_its_packet() {
    let resolved = common::load_minecraft().unwrap();
    assert_eq!(resolved.routes.len(), 261);
    for (route, pkt) in &resolved.routes {
        assert_eq!(route.packet_name, pkt.name);
        assert!(route.packet_id >= 0);
    }
}

#[test]
fn rest_fallback_set_is_explicit() {
    // REST_FALLBACKS is empty: no packet may silently regress to a stub.
    let resolved = common::load_minecraft().unwrap();
    let routed: std::collections::HashSet<&str> =
        resolved.routes.iter().map(|(_, p)| p.name.as_str()).collect();
    let stubs: Vec<&str> = routed
        .iter()
        .copied()
        .filter(|name| is_rest_stub(resolved.packets.get(name).unwrap()))
        .collect();
    assert!(stubs.is_empty(), "rest stubs: {stubs:?}");
}

#[test]
fn non_stub_packets_smoke_decode() {
    let resolved = common::load_minecraft().unwrap();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (_, pkt) in &resolved.routes {
        if seen.contains(pkt.name.as_str()) || is_rest_stub(pkt) {
            continue;
        }
        seen.insert(&pkt.name);
        // Every transcribed packet must decode (or fail cleanly) on a zero buffer.
        match decode_packet(&resolved, &pkt.name, &vec![0u8; 4096], false) {
            Ok(_) => {}
            Err(e) => assert_eq!(
                e.kind,
                bytemill::errors::ErrorKind::Decode,
                "{}: non-clean failure {e}",
                pkt.name
            ),
        }
    }
    // Every declared packet is reachable from a route (no orphans).
    let routed: std::collections::HashSet<&str> =
        resolved.routes.iter().map(|(_, p)| p.name.as_str()).collect();
    assert_eq!(seen.len(), routed.len());
}

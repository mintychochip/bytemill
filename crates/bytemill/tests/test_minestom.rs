//! Ports tests/test_minestom.py: BPS packets get the Minestom class
//! `rstom-packets` names them by.

mod common;

use bytemill::errors::ErrorKind;
use bytemill::minestom::{build_units, ClassTable, Unit};
use std::collections::{BTreeSet, HashMap, HashSet};

/// `protocol minecraft @ v1` with `routes`, like the Python `_protocol`.
fn protocol(routes: &[&str]) -> String {
    let body: String = routes.iter().map(|r| format!("    route {r}\n")).collect();
    let frame = "    frame default {\n        length: varint\n        id: varint\n        payload: rest\n    }\n";
    format!("protocol minecraft @ v1 {{\n    id = 1\n{frame}{body}}}\n")
}

/// `build_units` over the std scalars plus `source`, like the Python `_units`.
fn units(source: &str, table: &ClassTable) -> bytemill::BpsResult<Vec<Unit>> {
    let root = common::repo_root();
    let mut std = String::new();
    for name in ["integers", "varint"] {
        std.push_str(
            &std::fs::read_to_string(root.join("schemas/std").join(format!("{name}.bps"))).unwrap(),
        );
    }
    let schema = bytemill::parse(&format!("{std}{source}")).expect("test source parses");
    let resolved = bytemill::analyze(&schema).expect("test source resolves");
    build_units(&resolved, table)
}

/// The whole protocol's units, or `None` without rstom (the Python
/// `setUpClass` raising `SkipTest`).
fn minecraft_units() -> Option<(bytemill::ResolvedSchema, Vec<Unit>)> {
    let table = common::rstom_tables::real_table()?;
    let resolved = common::load_minecraft().unwrap();
    let units = build_units(&resolved, &table).unwrap();
    Some((resolved, units))
}

fn missing_table() {
    eprintln!("skipped: rstom packets_gen schema not present");
}

fn minestom_java(tail: &str) -> String {
    format!("net.minestom.server.network.packet.{tail}")
}

/// `assertRaisesRegex` on an `EmitError`: the Python patterns here are
/// literals joined by `.*` on one line, so ordered substring search matches
/// exactly what `re.search` would.
fn assert_emit_in_order(result: bytemill::BpsResult<Vec<Unit>>, needles: &[&str]) {
    let err = result.expect_err("expected an EmitError");
    assert_eq!(err.kind, ErrorKind::Emit, "wrong error kind: {}", err.message);
    let mut from = 0;
    for needle in needles {
        let at = err.message[from..].find(needle).unwrap_or_else(|| {
            panic!("message {:?} lacks {needle:?} after {:?}", err.message, &err.message[..from])
        });
        from += at + needle.len();
    }
}

#[test]
fn the_class_at_an_id_is_the_class_minestom_registers_there() {
    if !common::java_probes::available() {
        eprintln!("skipped: rstom probe fixtures not present");
        return;
    }
    let Some((_, units)) = minecraft_units() else {
        missing_table();
        return;
    };
    let mut routed: HashMap<(String, String, i128), &str> = HashMap::new();
    for u in &units {
        for r in &u.routes {
            routed.insert((r.state.clone(), r.direction.clone(), r.packet_id), u.cls.name.as_str());
        }
    }
    let recorded = common::java_probes::slots();
    for slot in &recorded {
        let key = (slot.state.clone(), slot.direction.clone(), slot.packet_id);
        assert_eq!(routed.get(&key), Some(&slot.java_class.as_str()), "class at {key:?}");
    }
    // The one route Minestom's id tables do not list is the pre-1.7 ping.
    let recorded_keys: HashSet<_> = recorded
        .iter()
        .map(|s| (s.state.clone(), s.direction.clone(), s.packet_id))
        .collect();
    let mut extra: Vec<(String, String, i128)> =
        routed.keys().filter(|k| !recorded_keys.contains(*k)).cloned().collect();
    extra.sort();
    assert_eq!(extra, vec![("handshaking".to_string(), "serverbound".to_string(), 0xFE)]);
}

#[test]
fn every_packet_has_a_struct_for_each_direction_it_travels() {
    let Some((resolved, units)) = minecraft_units() else {
        missing_table();
        return;
    };
    let serverbound: HashSet<&str> = units
        .iter()
        .filter(|u| u.direction == "serverbound")
        .map(|u| u.pkt.name.as_str())
        .collect();
    let clientbound: HashSet<&str> = units
        .iter()
        .filter(|u| u.direction == "clientbound")
        .map(|u| u.pkt.name.as_str())
        .collect();
    let mut both: Vec<&str> = serverbound.intersection(&clientbound).copied().collect();
    both.sort();
    assert_eq!(both, ["custom_payload", "finish_configuration", "keep_alive", "select_known_packs"]);
    let named: HashSet<&str> = units.iter().map(|u| u.pkt.name.as_str()).collect();
    let declared: HashSet<&str> = resolved.packets.keys().collect();
    assert_eq!(named, declared);
    assert_eq!(units.len(), resolved.packets.len() + both.len());
}

#[test]
fn a_packet_in_both_directions_is_a_client_and_a_server_class() {
    let Some((_, units)) = minecraft_units() else {
        missing_table();
        return;
    };
    let mut classes: HashMap<&str, &str> = HashMap::new();
    for u in &units {
        if u.pkt.name == "keep_alive" {
            classes.insert(u.direction.as_str(), u.cls.java.as_str());
        }
    }
    assert_eq!(classes["serverbound"], minestom_java("client.common.ClientKeepAlivePacket"));
    assert_eq!(classes["clientbound"], minestom_java("server.common.KeepAlivePacket"));
}

#[test]
fn a_route_outside_the_id_table_is_found_by_the_packets_name() {
    let Some((_, units)) = minecraft_units() else {
        missing_table();
        return;
    };
    let legacy = units
        .iter()
        .find(|u| u.pkt.name == "legacy_server_list_ping")
        .expect("legacy_server_list_ping has a unit");
    // Minestom files it with status.
    assert_eq!(
        (legacy.cls.group.as_str(), legacy.cls.package.as_str(), legacy.cls.name.as_str()),
        ("client", "status", "LegacyServerListPingPacket")
    );
}

#[test]
fn a_class_with_two_wire_forms_is_one_module_per_form() {
    let Some((_, units)) = minecraft_units() else {
        missing_table();
        return;
    };
    // Minestom keeps `ShowDialogPacket`'s two serializers on one class; a struct has one.
    let mut forms: HashMap<&str, &bytemill::minestom::MinestomClass> = HashMap::new();
    for u in &units {
        if u.cls.name == "ShowDialogPacket" {
            forms.insert(u.pkt.name.as_str(), &u.cls);
        }
    }
    let mut names: Vec<&str> = forms.keys().copied().collect();
    names.sort();
    assert_eq!(names, ["show_dialog", "show_dialog_configuration"]);
    assert_eq!(forms["show_dialog"].java, minestom_java("server.common.ShowDialogPacket"));
    assert_eq!(
        forms["show_dialog_configuration"].java,
        minestom_java("server.configuration.ShowDialogPacket")
    );
    assert_eq!(forms["show_dialog_configuration"].package, "configuration");
}

#[test]
fn no_two_structs_share_a_java_class_name() {
    let Some((_, units)) = minecraft_units() else {
        missing_table();
        return;
    };
    // `PacketEntry::resolve` swaps a serializer in by class name, so a repeat would win.
    let mut seen = BTreeSet::new();
    let mut dupes = BTreeSet::new();
    for name in units.iter().map(|u| u.cls.java.as_str()) {
        if !seen.insert(name) {
            dupes.insert(name);
        }
    }
    let dupes: Vec<&str> = dupes.into_iter().collect();
    assert!(dupes.is_empty(), "java class named by two structs: {dupes:?}");
}

#[test]
fn a_route_with_no_class_is_an_error() {
    let ids: &[&str] = &["ProbePacket"];
    let table = common::rstom_tables::small_table(&[("play", "SERVER", ids)]);
    assert_emit_in_order(
        units(
            &format!(
                "packet probe {{ a: u8 }} packet other {{ b: u8 }}\n{}",
                protocol(&["play.clientbound 0x00 => probe", "play.clientbound 0x01 => other"])
            ),
            &table,
        ),
        &["route play.clientbound 0x01 => other"],
    );
}

#[test]
fn a_packet_with_no_route_is_an_error() {
    let ids: &[&str] = &["ProbePacket"];
    let table = common::rstom_tables::small_table(&[("play", "SERVER", ids)]);
    assert_emit_in_order(
        units(&format!("packet probe {{ a: u8 }}\n{}", protocol(&[])), &table),
        &["packet 'probe' has no route"],
    );
}

#[test]
fn one_packet_cannot_be_two_classes_in_one_direction() {
    let a: &[&str] = &["APacket"];
    let b: &[&str] = &["BPacket"];
    let table = common::rstom_tables::small_table(&[
        ("configuration", "SERVER", a),
        ("play", "SERVER", b),
    ]);
    assert_emit_in_order(
        units(
            &format!(
                "packet probe {{ a: u8 }}\n{}",
                protocol(&[
                    "configuration.clientbound 0x00 => probe",
                    "play.clientbound 0x00 => probe"
                ])
            ),
            &table,
        ),
        &["probe", "APacket", "BPacket"],
    );
}

#[test]
fn two_packets_on_one_class_must_be_declared() {
    let a: &[&str] = &["APacket"];
    let table = common::rstom_tables::small_table(&[
        ("configuration", "SERVER", a),
        ("play", "SERVER", a),
    ]);
    assert_emit_in_order(
        units(
            &format!(
                "packet first {{ a: u8 }} packet second {{ b: u8 }}\n{}",
                protocol(&[
                    "configuration.clientbound 0x00 => first",
                    "play.clientbound 0x00 => second"
                ])
            ),
            &table,
        ),
        &["APacket", "first", "second"],
    );
}

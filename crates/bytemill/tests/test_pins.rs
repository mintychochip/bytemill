//! Ports tests/test_pins.py.

mod common;

use bytemill::interp::{decode_packet, encode_packet};
use bytemill::minestom::build_units;
use bytemill::pins::{build_pins, pins_json};

fn side_of(direction: &str) -> &'static str {
    match direction {
        "serverbound" => "CLIENT",
        _ => "SERVER",
    }
}

fn state_of(state: &str) -> &'static str {
    match state {
        "handshaking" => "HANDSHAKE",
        "status" => "STATUS",
        "login" => "LOGIN",
        "configuration" => "CONFIGURATION",
        "play" => "PLAY",
        _ => panic!("state"),
    }
}

fn state_to_route(state: &str) -> &'static str {
    match state {
        "HANDSHAKE" => "handshaking",
        "STATUS" => "status",
        "LOGIN" => "login",
        "CONFIGURATION" => "configuration",
        "PLAY" => "play",
        _ => panic!("state"),
    }
}

fn side_to_direction(side: &str) -> &'static str {
    if side == "CLIENT" {
        "serverbound"
    } else {
        "clientbound"
    }
}

#[test]
fn pin_rows_match_the_id_tables_and_round_trip() {
    let Some(table) = common::rstom_tables::real_table() else {
        eprintln!("rstom packets_gen schema not present; skipping");
        return;
    };
    let resolved = common::load_minecraft().unwrap();
    let units = build_units(&resolved, &table).unwrap();
    let rows = build_pins(&resolved, &units).unwrap();

    // One row per id-table entry, ids contiguous per (side, state).
    assert_eq!(rows.len(), 260);
    let mut groups: Vec<(String, String, Vec<i128>)> = Vec::new();
    for row in &rows {
        match groups.iter_mut().find(|(s, st, _)| *s == row.side && *st == row.state) {
            Some((_, _, ids)) => ids.push(row.id),
            None => groups.push((row.side.clone(), row.state.clone(), vec![row.id])),
        }
    }
    for (_, _, ids) in &groups {
        let expected: Vec<i128> = (0..ids.len() as i128).collect();
        assert_eq!(ids, &expected);
    }

    // The packet at each row, from the registry entries.
    let mut packet_by_row: Vec<((String, String, i128), String)> = Vec::new();
    for (_key, entries) in bytemill::emit_rstom::registry_entries(&units).iter() {
        for (route, unit) in entries {
            packet_by_row.push((
                (side_of(&route.direction).to_string(), state_of(&route.state).to_string(), route.packet_id),
                unit.pkt.name.clone(),
            ));
        }
    }

    for row in &rows {
        let packet = packet_by_row
            .iter()
            .find(|((s, st, id), _)| *s == row.side && *st == row.state && *id == row.id)
            .map(|(_, p)| p.clone())
            .expect("row packet");
        let data: Vec<u8> = (0..row.hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&row.hex[i..i + 2], 16).unwrap())
            .collect();
        let value = decode_packet(&resolved, &packet, &data, true).unwrap();
        let again = encode_packet(&resolved, &packet, &value).unwrap();
        assert_eq!(again, data, "{}/{}/{}", row.side, row.state, row.id);
    }

    // Class names come from the class table.
    for row in &rows {
        let state = state_to_route(&row.state);
        let direction = side_to_direction(&row.side);
        let cls = table.at(state, direction, row.id).expect("class at id");
        assert_eq!(row.cls, cls.name);
    }

    // JSON is stable and one row per line.
    let forward = pins_json(&rows);
    let mut reversed = rows.clone();
    reversed.reverse();
    assert_eq!(pins_json(&reversed), forward);
    let doc = bytemill::json::loads(&forward).unwrap();
    assert!(doc.get("_generated").is_some());
    let json_rows = doc.get("rows").and_then(|v| match v {
        bytemill::Value::List(l) => Some(l.len()),
        _ => None,
    });
    assert_eq!(json_rows, Some(rows.len()));
}

//! Generated wire vectors round-trip in the reference interpreter.

mod common;

use std::collections::{HashMap, HashSet};

use bytemill::errors::ErrorKind;
use bytemill::vectors::{self, Vector, VectorRoute, VectorSet, VectorSpec};
use bytemill::wire::check_vectors;
use bytemill::{analyze, load_schema, parse};

fn schema(source: &str) -> bytemill::ResolvedSchema {
    analyze(&parse(source).unwrap()).unwrap()
}

fn spec(per_packet: usize, seed: u64) -> VectorSpec {
    VectorSpec { per_packet, seed }
}

fn hexes(set: &VectorSet) -> Vec<&str> {
    set.vectors
        .iter()
        .map(|vector| vector.hex.as_str())
        .collect()
}

#[test]
fn same_spec_is_deterministic_and_a_new_seed_changes_hex() {
    let resolved = schema("packet probe { a: u8 b: u8 c: u8 d: u8 }\n");
    let spec = VectorSpec::default();
    let once = vectors::generate(&resolved, &spec).unwrap();
    let twice = vectors::generate(&resolved, &spec).unwrap();
    assert_eq!(once, twice);
    assert!(once.skipped.is_empty(), "{:?}", once.skipped);
    assert!(!once.vectors.is_empty());
    let mut distinct = hexes(&once);
    let n = distinct.len();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), n);
    assert!(n <= spec.per_packet);

    let other = vectors::generate(
        &resolved,
        &VectorSpec {
            per_packet: spec.per_packet,
            seed: 2,
        },
    )
    .unwrap();
    assert_ne!(hexes(&once), hexes(&other));
}

#[test]
fn zero_per_packet_yields_nothing() {
    let resolved = schema("packet probe { x: u8 }\n");
    let set = vectors::generate(&resolved, &spec(0, 1)).unwrap();
    assert!(set.vectors.is_empty());
    assert!(set.skipped.is_empty());
}

#[test]
fn hytale_vectors_round_trip() {
    let resolved =
        analyze(&load_schema(&[common::schema_path("hytale/2026_02_17.bml")]).unwrap()).unwrap();
    let set = vectors::generate(&resolved, &VectorSpec::default()).unwrap();
    for skipped in &set.skipped {
        assert!(!skipped.reason.is_empty(), "{}", skipped.packet);
    }
    assert!(set.vectors.len() >= 12);
    let report = check_vectors(&resolved, &vectors::to_jsonl(&set.vectors), false).unwrap();
    assert!(
        report.failures.is_empty(),
        "{} failures, first: {:?}",
        report.failures.len(),
        report.failures.first()
    );
    assert_eq!(report.checked, set.vectors.len());
}

#[test]
fn minecraft_packets_are_covered_and_round_trip() {
    let resolved = common::load_minecraft().unwrap();
    let spec = spec(1, 1);
    let set = vectors::generate(&resolved, &spec).unwrap();

    let mut by_packet: HashMap<&str, Vec<&str>> = HashMap::new();
    for vector in &set.vectors {
        by_packet
            .entry(vector.packet.as_str())
            .or_default()
            .push(vector.hex.as_str());
    }
    let skipped: HashSet<&str> = set
        .skipped
        .iter()
        .map(|item| item.packet.as_str())
        .collect();
    for item in &set.skipped {
        assert!(!item.reason.is_empty(), "{}", item.packet);
    }
    assert!(set.vectors.len() >= 231);

    for packet in &resolved.schema.packets {
        let name = packet.name.as_str();
        let hexes = by_packet.get(name);
        assert_eq!(
            hexes.is_some(),
            !skipped.contains(name),
            "{name}: vector={} skipped={}",
            hexes.is_some(),
            skipped.contains(name)
        );
        if let Some(hexes) = hexes {
            assert!(hexes.len() <= spec.per_packet, "{name}: {}", hexes.len());
            let mut uniq = hexes.clone();
            uniq.sort_unstable();
            uniq.dedup();
            assert_eq!(uniq.len(), hexes.len(), "{name} repeats a payload");
        }
    }
    assert_eq!(
        by_packet.len() + skipped.len(),
        resolved.schema.packets.len()
    );

    let report = check_vectors(&resolved, &vectors::to_jsonl(&set.vectors), false).unwrap();
    assert!(
        report.failures.is_empty(),
        "{} failures, first: {:?}",
        report.failures.len(),
        report.failures.first()
    );
    assert_eq!(report.checked, set.vectors.len());
}

#[test]
fn empty_packet_is_one_empty_vector_and_until_end_is_skipped() {
    let empty = schema("packet empty { }\n");
    let set = vectors::generate(&empty, &VectorSpec::default()).unwrap();
    assert!(set.skipped.is_empty(), "{:?}", set.skipped);
    assert_eq!(set.vectors.len(), 1);
    assert_eq!(set.vectors[0].packet, "empty");
    assert_eq!(set.vectors[0].hex, "");
    assert!(set.vectors[0].route.is_none());
    let report = check_vectors(&empty, &vectors::to_jsonl(&set.vectors), false).unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.checked, 1);

    let tail = schema("packet tail { entries: u8[] until_end }\n");
    let set = vectors::generate(&tail, &spec(3, 1)).unwrap();
    assert!(set.vectors.is_empty(), "{:?}", set.vectors);
    assert_eq!(set.skipped.len(), 1);
    assert_eq!(set.skipped[0].packet, "tail");
    assert!(
        set.skipped[0]
            .reason
            .contains("encode not supported for repeat mode until_end"),
        "{}",
        set.skipped[0].reason
    );
}

#[test]
fn first_route_in_schema_order_is_attached() {
    let resolved = schema(
        "packet probe { }\nprotocol demo @ v1 {\n    id = 1\n    route play.serverbound 2 => probe\n    route play.clientbound 1 => probe\n}\n",
    );
    let set = vectors::generate(&resolved, &spec(1, 1)).unwrap();
    assert_eq!(set.vectors.len(), 1);
    let route = set.vectors[0].route.as_ref().expect("probe is routed");
    assert_eq!(route.state, "play");
    assert_eq!(route.direction, "serverbound");
    assert_eq!(route.id, 2);
}

#[test]
fn generate_for_rejects_an_unknown_name_and_emits_only_the_subset() {
    let resolved = schema("packet a { x: u8 }\npacket b { y: u8 }\npacket c { }\n");
    let err = vectors::generate_for(&resolved, &["missing".into()], &spec(1, 1)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Resolve);
    assert!(err.to_string().contains("missing"), "{err}");

    let spec = spec(1, 1);
    let all = vectors::generate(&resolved, &spec).unwrap();
    let sub = vectors::generate_for(&resolved, &["b".into(), "a".into()], &spec).unwrap();
    assert!(sub.skipped.is_empty(), "{:?}", sub.skipped);
    assert_eq!(
        sub.vectors
            .iter()
            .map(|vector| vector.packet.as_str())
            .collect::<Vec<_>>(),
        ["b", "a"]
    );
    fn of<'a>(set: &'a VectorSet, name: &str) -> Vec<&'a str> {
        set.vectors
            .iter()
            .filter(|vector| vector.packet == name)
            .map(|vector| vector.hex.as_str())
            .collect()
    }
    assert_eq!(of(&all, "a"), of(&sub, "a"));
    assert_eq!(of(&all, "b"), of(&sub, "b"));
    assert!(sub.vectors.iter().all(|vector| vector.packet != "c"));
}

#[test]
fn jsonl_keys_are_sorted_and_route_keys_are_omitted_when_absent() {
    let routed = Vector {
        packet: "probe".into(),
        route: Some(VectorRoute {
            state: "play".into(),
            direction: "clientbound".into(),
            id: 1,
        }),
        hex: "07".into(),
    };
    assert_eq!(
        vectors::to_jsonl(&[routed]),
        "{\"direction\": \"clientbound\", \"hex\": \"07\", \"id\": 1, \"packet\": \"probe\", \"state\": \"play\"}\n"
    );
    let bare = Vector {
        packet: "empty".into(),
        route: None,
        hex: String::new(),
    };
    assert_eq!(
        vectors::to_jsonl(&[bare]),
        "{\"hex\": \"\", \"packet\": \"empty\"}\n"
    );
    assert_eq!(vectors::to_jsonl(&[]), "");
}

#[test]
fn packet_keyed_lines_round_trip_and_a_mismatched_name_is_rejected() {
    let resolved = schema(
        "packet probe { x: u8 }\npacket other { y: u8 }\nprotocol demo @ v1 {\n    id = 1\n    route play.clientbound 1 => probe\n}\n",
    );

    let keyed = check_vectors(
        &resolved,
        "{\"packet\": \"probe\", \"hex\": \"07\"}\n",
        false,
    )
    .unwrap();
    assert!(keyed.failures.is_empty(), "{:?}", keyed.failures);
    assert_eq!(keyed.checked, 1);

    let routed = check_vectors(
        &resolved,
        "{\"direction\": \"clientbound\", \"hex\": \"07\", \"id\": 1, \"state\": \"play\"}\n",
        false,
    )
    .unwrap();
    assert!(routed.failures.is_empty(), "{:?}", routed.failures);
    assert_eq!(routed.checked, 1);

    let matching = check_vectors(
        &resolved,
        "{\"direction\": \"clientbound\", \"hex\": \"07\", \"id\": 1, \"packet\": \"probe\", \"state\": \"play\"}\n",
        false,
    )
    .unwrap();
    assert!(matching.failures.is_empty(), "{:?}", matching.failures);
    assert_eq!(matching.checked, 1);

    let err = check_vectors(
        &resolved,
        "{\"direction\": \"clientbound\", \"hex\": \"07\", \"id\": 1, \"packet\": \"other\", \"state\": \"play\"}\n",
        false,
    )
    .unwrap_err();
    assert_eq!(
        err,
        "line 1: packet \"other\" is not the route's packet \"probe\""
    );

    let malformed = check_vectors(&resolved, "{\"hex\": \"00\"}\n", false).unwrap_err();
    assert_eq!(malformed, "line 1: vector has no \"state\"");
}
#[test]
fn eof_group_skips_counter_and_vectors_avoid_ambiguous() {
    let src = r#"
encoding boolean = {
    raw: u8 = value ? 1 : 0
} = raw != 0

packet p {
    tag: u8
    has: boolean
    if has {
        v: match tag {
            1 => { x: u8 }
            2 => { }
        }
    }
}
"#;
    let resolved = schema(src);

    // tag=2 has=true : group skipped
    let data_amb = [2, 1];
    let (_, skips) = bytemill::interp::decode_packet_eof_skips(&resolved, "p", &data_amb).unwrap();
    assert_eq!(skips, 1);

    // tag=1 has=true + x present
    let data_good = [1, 1, 99];
    let (_, skips) = bytemill::interp::decode_packet_eof_skips(&resolved, "p", &data_good).unwrap();
    assert_eq!(skips, 0);

    // has=false
    let data_no = [5, 0];
    let (_, skips) = bytemill::interp::decode_packet_eof_skips(&resolved, "p", &data_no).unwrap();
    assert_eq!(skips, 0);

    // vectors::generate never emits ambig (all vectors have 0 skips)
    let set = vectors::generate(&resolved, &spec(5, 42)).unwrap();
    assert!(
        !set.vectors.is_empty(),
        "the unambiguous payloads are still generated"
    );
    for v in &set.vectors {
        let b = v.bytes().unwrap();
        let (_, s) = bytemill::interp::decode_packet_eof_skips(&resolved, "p", &b).unwrap();
        assert_eq!(s, 0, "ambiguous vector emitted");
    }

    // schema whose only possible payload is ambig -> skipped with reason
    let only_src = r#"
packet onlyamb {
    if 1 {
        v: { }
    }
}
"#;
    let only_res = schema(only_src);
    let set2 = vectors::generate(&only_res, &spec(1, 1)).unwrap();
    assert!(set2.vectors.is_empty());
    assert_eq!(set2.skipped.len(), 1);
    assert!(
        set2.skipped[0]
            .reason
            .contains("group whose condition holds was skipped at end of input"),
        "reason: {}",
        set2.skipped[0].reason
    );
}

#[test]
fn a_require_equality_pins_the_field_in_either_operand_order() {
    let resolved =
        schema("packet a { v: u8 require v == 177 }\npacket b { w: u8 require 5 == w }\n");
    let set = vectors::generate(&resolved, &VectorSpec::default()).unwrap();
    assert!(set.skipped.is_empty(), "{:?}", set.skipped);
    let by_packet = |name: &str| -> Vec<&str> {
        set.vectors
            .iter()
            .filter(|vector| vector.packet == name)
            .map(|vector| vector.hex.as_str())
            .collect()
    };
    assert_eq!(by_packet("a"), ["b1"]);
    assert_eq!(by_packet("b"), ["05"]);
}

#[test]
fn a_require_that_is_not_a_plain_equality_pins_nothing() {
    // Only a top-level `field == constant` leaves one valid value. A disjunction or a `!=` leaves
    // the field free: the generator must still produce several vectors that satisfy the require.
    let resolved =
        schema("packet a { v: u8 require v == 7 || v == 9 }\npacket b { w: u8 require w != 3 }\n");
    let set = vectors::generate(&resolved, &spec(6, 1)).unwrap();
    assert!(set.skipped.is_empty(), "{:?}", set.skipped);
    let by_packet = |name: &str| -> Vec<&str> {
        set.vectors
            .iter()
            .filter(|vector| vector.packet == name)
            .map(|vector| vector.hex.as_str())
            .collect()
    };
    let a = by_packet("a");
    assert!(
        a.contains(&"07") && a.contains(&"09"),
        "both disjuncts occur: {a:?}"
    );
    assert!(a.iter().all(|hex| *hex == "07" || *hex == "09"), "{a:?}");
    let b = by_packet("b");
    assert!(
        b.len() >= 2,
        "a `!=` must not pin the field to one value: {b:?}"
    );
    assert!(b.iter().all(|hex| *hex != "03"), "{b:?}");
}

#[test]
fn the_minecraft_handshake_is_generated_because_its_require_pins_the_protocol_version() {
    let resolved = common::load_minecraft().unwrap();
    let set = vectors::generate_for(&resolved, &["handshake".to_string()], &spec(3, 1)).unwrap();
    assert!(set.skipped.is_empty(), "{:?}", set.skipped);
    assert!(!set.vectors.is_empty());
    for vector in &set.vectors {
        let decoded =
            bytemill::interp::decode_packet(&resolved, "handshake", &vector.bytes().unwrap(), true)
                .unwrap();
        assert_eq!(
            decoded.get("protocol_version"),
            Some(&bytemill::Value::Int(777))
        );
    }
}

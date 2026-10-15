//! 26.3 twice: the hand-written, vanilla-derived schema (`schemas/minecraft_26_3*.bps`) and the
//! one imported from minecraft-data (`schemas/versions/v26_3.bps`). Bytes the hand-written schema
//! encodes for a synthetic packet must decode strictly under the imported one, and re-encode
//! to the same bytes.

mod common;

use bps::interp::{decode_packet, encode_packet};
use bps::wiregen::generate;
use bps::{ResolvedSchema, analyze, load_schema};

fn imported() -> ResolvedSchema {
    analyze(&load_schema(&[common::schema_path("versions/v26_3.bps")]).unwrap()).unwrap()
}

fn seeds() -> u64 {
    std::env::var("BPS_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24)
}

/// Bytes drawn from the hand-written schema's packets, run through the imported one.
fn disagreements() -> (usize, Vec<(String, String)>) {
    let hand = common::load_minecraft().unwrap();
    let theirs = imported();
    let mut bad = Vec::new();
    let mut checked = 0;
    for (route, pkt) in &hand.routes {
        let Some((_, other)) = theirs.routes.iter().find(|(r, _)| {
            r.state == route.state
                && r.direction == route.direction
                && r.packet_id == route.packet_id
        }) else {
            bad.push((
                pkt.name.clone(),
                format!(
                    "{}.{} 0x{:02x} {}: no such route in the imported schema",
                    route.state, route.direction, route.packet_id, pkt.name
                ),
            ));
            continue;
        };
        for seed in 0..seeds() {
            let tag = format!(
                "{}.{} 0x{:02x} {} -> {} seed {seed}",
                route.state, route.direction, route.packet_id, pkt.name, other.name
            );
            let Ok(bytes) = generate(&hand, &pkt.name, seed) else {
                continue;
            };
            // A schema `require` the random bytes break is a contract, not a disagreement.
            if decode_packet(&hand, &pkt.name, &bytes, true).is_err() {
                continue;
            }
            let decoded = match decode_packet(&theirs, &other.name, &bytes, true) {
                Ok(d) => d,
                Err(e) => {
                    bad.push((pkt.name.clone(), format!("{tag}: imported decode {e}")));
                    continue;
                }
            };
            match encode_packet(&theirs, &other.name, &decoded) {
                Ok(re) if re == bytes => checked += 1,
                Ok(_) => bad.push((
                    pkt.name.clone(),
                    format!("{tag}: imported re-encode differs"),
                )),
                Err(e) => bad.push((pkt.name.clone(), format!("{tag}: imported re-encode {e}"))),
            }
        }
    }
    (checked, bad)
}

#[test]
#[ignore = "diagnostic: lists every disagreement"]
fn list_cross_disagreements() {
    let (checked, bad) = disagreements();
    eprintln!("agree on {checked}, disagree on {}", bad.len());
    for (_, line) in &bad {
        eprintln!("{line}");
    }
}

/// Packets the hand-written schema is deliberately looser about than the vanilla-checked import:
/// it stops at a registry boundary, so bytes it draws for a non-empty item stack carry component
/// counts that the imported schema then reads component payloads for. Every other packet must agree.
const LOOSER_HAND_WRITTEN: &[(&str, &str)] = &[
    (
        "container_set_content",
        "Slot stops at the component counts",
    ),
    ("container_set_slot", "Slot stops at the component counts"),
    ("set_cursor_item", "Slot stops at the component counts"),
    ("set_equipment", "Slot stops at the component counts"),
    ("set_player_inventory", "Slot stops at the component counts"),
    (
        "set_creative_mode_slot",
        "Slot stops at the component counts",
    ),
    (
        "merchant_offers",
        "item_cost stops at the predicate count; slots at the component counts",
    ),
    (
        "place_ghost_recipe",
        "recipe_display is opaque past its type",
    ),
    (
        "recipe_book_add",
        "slot displays carry item templates that stop at the component counts",
    ),
    (
        "update_recipes",
        "slot displays carry item templates that stop at the component counts",
    ),
    (
        "update_advancements",
        "advancement icons are item templates that stop at the component counts",
    ),
    (
        "explode",
        "the `item` particle is an item template that stops at the component counts",
    ),
    (
        "update_attributes",
        "only for out-of-range operation ids, which the hand-written i8 accepts and the mapper does not",
    ),
];

#[test]
fn imported_26_3_agrees_with_the_hand_written_schema_except_where_that_is_looser() {
    let (checked, bad) = disagreements();
    let unexplained: Vec<&str> = bad
        .iter()
        .filter(|(packet, _)| !LOOSER_HAND_WRITTEN.iter().any(|(p, _)| p == packet))
        .map(|(_, line)| line.as_str())
        .collect();
    assert!(
        unexplained.is_empty(),
        "{} unexplained disagreements, first:\n{}",
        unexplained.len(),
        unexplained
            .iter()
            .take(10)
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    );
    // An exclusion that no longer disagrees is stale: remove it so the packet is checked.
    for (packet, why) in LOOSER_HAND_WRITTEN {
        assert!(
            bad.iter().any(|(p, _)| p == packet),
            "{packet} ({why}) now agrees: drop it from LOOSER_HAND_WRITTEN"
        );
    }
    assert!(checked > 5000, "only {checked} packets compared");
}

//! Ports tests/test_synth_domains.py.

mod common;

use bps::ordmap::OrderedMap;
use bps::synth::{synthesize_packet, validate_all_synth_overrides, with_synth_overrides};
use bps::Value;

fn extra_env(pairs: &[(&str, Value)]) -> bps::Env {
    let mut env = bps::Env::new();
    for (k, v) in pairs {
        env.insert(k, v.clone());
    }
    env
}

fn synth(r: &bps::ResolvedSchema, name: &str) -> bps::Env {
    let pkt = r.packets.get(name).unwrap();
    synthesize_packet(r, pkt).unwrap()
}

#[test]
fn scalars_default_to_the_smallest_valid_id() {
    let r = common::load_with_extra(
        "
        packet probe {
            a: varint
            b: varlong
            c: i8
            d: u8
        }
        ",
    )
    .unwrap();
    let value = synth(&r, "probe");
    assert_eq!(value, extra_env(&[
        ("a", Value::Int(1)),
        ("b", Value::Int(1)),
        ("c", Value::Int(1)),
        ("d", Value::Int(1)),
    ]));
}

#[test]
fn require_equality_pins_the_field() {
    let r = common::load_with_extra(
        "
        packet p {
            n: varint
            require n == 777
        }
        ",
    )
    .unwrap();
    assert_eq!(synth(&r, "p").get("n"), Some(&Value::Int(777)));
}

#[test]
fn handshake_protocol_version_from_require_not_special_case() {
    let r = common::load_with_extra("").unwrap();
    assert_eq!(
        synth(&r, "handshake").get("protocol_version"),
        Some(&Value::Int(777))
    );
}

#[test]
fn nbt_defaults_are_valid_for_vanilla() {
    let r = common::load_with_extra(
        "
        packet probe {
            root: nbt
            chat: text_component
        }
        ",
    )
    .unwrap();
    let value = synth(&r, "probe");
    assert_eq!(
        value.get("root"),
        Some(&Value::map({
            let mut m = OrderedMap::new();
            m.insert("tag_type", Value::Int(10));
            let mut inner = OrderedMap::new();
            inner.insert("entries", Value::list(vec![]));
            m.insert("value", Value::map(inner));
            m
        }))
    );
    assert_eq!(
        value.get("chat"),
        Some(&Value::map({
            let mut m = OrderedMap::new();
            m.insert("tag_type", Value::Int(8));
            m.insert("value", Value::str("abc"));
            m
        }))
    );
}

#[test]
fn overrides_apply_by_packet_and_field() {
    let r = common::load_with_extra("packet probe { x: varint }").unwrap();
    with_synth_overrides(|o| {
        o.insert("probe/x", Value::Int(99));
    });
    let value = synth(&r, "probe");
    // Restore the production table.
    with_synth_overrides(|o| {
        o.remove("probe/x");
    });
    assert_eq!(value.get("x"), Some(&Value::Int(99)));
}

#[test]
fn override_for_missing_packet_is_an_error() {
    let r = common::load_minecraft().unwrap();
    with_synth_overrides(|o| {
        o.insert("no_such_packet/x", Value::Int(1));
    });
    let err = validate_all_synth_overrides(&r).unwrap_err();
    with_synth_overrides(|o| {
        o.remove("no_such_packet/x");
    });
    assert!(
        err.to_string().contains("SYNTH_OVERRIDES references unknown packet 'no_such_packet'"),
        "{err}"
    );
}

#[test]
fn override_for_missing_field_is_an_error() {
    let r = common::load_with_extra("packet probe { x: varint }").unwrap();
    with_synth_overrides(|o| {
        o.insert("probe/no_such_field", Value::Int(1));
    });
    let pkt = r.packets.get("probe").unwrap();
    let err = synthesize_packet(&r, pkt).unwrap_err();
    with_synth_overrides(|o| {
        o.remove("probe/no_such_field");
    });
    assert!(err.to_string().contains("no_such_field"), "{err}");
}

#[test]
fn item_stack_template_default_has_zero_component_counts() {
    let r = common::load_with_extra("packet probe { stack: item_stack_template }").unwrap();
    assert_eq!(
        synth(&r, "probe").get("stack"),
        Some(&Value::map({
            let mut m = OrderedMap::new();
            for (k, v) in [
                ("item_id", 1),
                ("item_count", 1),
                ("component_added", 0),
                ("component_removed", 0),
            ] {
                m.insert(k, Value::Int(v));
            }
            m
        }))
    );
}

#[test]
fn item_cost_default_has_zero_predicate_count() {
    let r = common::load_with_extra("packet probe { cost: item_cost }").unwrap();
    assert_eq!(
        synth(&r, "probe").get("cost"),
        Some(&Value::map({
            let mut m = OrderedMap::new();
            for (k, v) in [("item_id", 1), ("count", 1), ("predicate_count", 0)] {
                m.insert(k, Value::Int(v));
            }
            m
        }))
    );
}

#[test]
fn production_synth_overrides_are_non_empty() {
    let n = with_synth_overrides(|o| o.len());
    assert!(n > 0);
}

#[test]
fn production_synth_overrides_validate_against_minecraft_schema() {
    let r = common::load_minecraft().unwrap();
    validate_all_synth_overrides(&r).unwrap();
    let names = with_synth_overrides(|o| {
        let mut seen: Vec<String> = Vec::new();
        for key in o.keys() {
            if let Some((pkt, _)) = key.split_once('/')
                && !seen.iter().any(|s| s == pkt)
            {
                seen.push(pkt.to_string());
            }
        }
        seen
    });
    for name in names {
        let pkt = r.packets.get(&name).unwrap();
        synthesize_packet(&r, pkt).unwrap();
    }
}

#[test]
fn client_information_view_distance_is_valid() {
    let r = common::load_minecraft().unwrap();
    assert_eq!(synth(&r, "client_information").get("view_distance"), Some(&Value::Int(8)));
}

#[test]
fn login_disconnect_reason_is_json_string_component() {
    let r = common::load_minecraft().unwrap();
    assert_eq!(synth(&r, "login_disconnect").get("reason"), Some(&Value::str("\"abc\"")));
}

#[test]
fn debug_sample_type_is_the_only_enum_constant() {
    let r = common::load_minecraft().unwrap();
    assert_eq!(synth(&r, "debug_sample").get("sample_type"), Some(&Value::Int(0)));
}

fn particle_default(r: &bps::ResolvedSchema, name: &str) {
    assert_eq!(
        synth(r, name).get("particle"),
        Some(&Value::map({
            let mut m = OrderedMap::new();
            m.insert("particle_type", Value::Int(0));
            m.insert("value", Value::map(OrderedMap::new()));
            m
        }))
    );
}

#[test]
fn explode_particle_uses_registry_id_zero() {
    particle_default(&common::load_minecraft().unwrap(), "explode");
}

#[test]
fn level_particles_particle_uses_registry_id_zero() {
    particle_default(&common::load_minecraft().unwrap(), "level_particles");
}

#[test]
fn update_recipes_property_set_uses_vanilla_namespaced_key() {
    let r = common::load_minecraft().unwrap();
    let expected = Value::list(vec![Value::map({
        let mut m = OrderedMap::new();
        m.insert("property_set_id", Value::str("minecraft:smithing_base"));
        m.insert("items_count", Value::Int(1));
        m.insert("items", Value::list(vec![Value::Int(1)]));
        m
    })]);
    assert_eq!(synth(&r, "update_recipes").get("property_sets"), Some(&expected));
}

#[test]
fn custom_click_action_payload_size_matches_nbt_bytes() {
    let r = common::load_minecraft().unwrap();
    let value = synth(&r, "custom_click_action");
    assert_eq!(value.get("payload_size"), Some(&Value::Int(2)));
    assert_eq!(
        value.get("payload"),
        Some(&Value::map({
            let mut m = OrderedMap::new();
            m.insert("tag_type", Value::Int(10));
            let mut inner = OrderedMap::new();
            inner.insert("entries", Value::list(vec![]));
            m.insert("value", Value::map(inner));
            m
        }))
    );
}

#[test]
fn update_advancements_override_omits_display_only() {
    let r = common::load_minecraft().unwrap();
    // The production override replaces `advancement_mappings`; drop it to
    // synthesize the schema default, then restore it for the real value.
    let removed = with_synth_overrides(|o| o.remove("update_advancements/advancement_mappings"))
        .expect("production update_advancements override");
    let default = synth(&r, "update_advancements");
    with_synth_overrides(|o| {
        o.insert("update_advancements/advancement_mappings", removed);
    });
    let value = synth(&r, "update_advancements");

    let count = |env: &bps::Env, key: &str| -> i128 {
        match env.get(key) {
            Some(Value::Int(n)) => *n,
            other => panic!("{key}: {other:?}"),
        }
    };
    assert!(count(&value, "identifiers_to_remove_count") > 0);
    assert!(count(&value, "progress_mappings_count") > 0);
    let list_len = |env: &bps::Env, key: &str| -> usize {
        match env.get(key) {
            Some(Value::List(items)) => items.len(),
            other => panic!("{key}: {other:?}"),
        }
    };
    assert!(list_len(&value, "identifiers_to_remove") > 0);
    assert!(list_len(&value, "progress_mappings") > 0);
    assert_eq!(
        value.get("identifiers_to_remove"),
        default.get("identifiers_to_remove")
    );
    assert_eq!(
        value.get("progress_mappings"),
        default.get("progress_mappings")
    );

    let first_of = |env: &bps::Env, key: &str| -> Value {
        match env.get(key) {
            Some(Value::List(items)) => items
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("{key} is empty")),
            other => panic!("{key}: {other:?}"),
        }
    };
    let actual = first_of(&value, "advancement_mappings");
    let expected = first_of(&default, "advancement_mappings");
    for key in ["key", "x", "y"] {
        assert_eq!(actual.get(key), expected.get(key), "{key}");
    }
    let actual_value = match actual.get("value") {
        Some(Value::Map(m)) => m,
        other => panic!("value: {other:?}"),
    };
    let default_value = match expected.get("value") {
        Some(Value::Map(m)) => m,
        other => panic!("default value: {other:?}"),
    };
    assert!(
        !actual_value.contains_key("display_data"),
        "{actual_value:?}"
    );
    let mut without_display = (**default_value).clone();
    without_display.remove("display_data");
    without_display.insert("has_display_data", Value::Bool(false));
    assert_eq!(**actual_value, without_display);
}

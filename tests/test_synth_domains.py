"""Synthetic value domains: small valid scalars, require pins, NBT shapes, overrides."""

import unittest

from bps.errors import BpsError
from bps.synth import SYNTH_OVERRIDES, synthesize_packet, validate_all_synth_overrides

from tests.test_varint import load_minecraft, load_with_extra


def _resolved(source: str):
    return load_with_extra(source)


class SynthScalarsTest(unittest.TestCase):
    def test_scalars_default_to_the_smallest_valid_id(self) -> None:
        resolved = _resolved(
            """
            packet probe {
                a: varint
                b: varlong
                c: i8
                d: u8
            }
            """
        )
        value = synthesize_packet(resolved, resolved.packets["probe"])
        self.assertEqual(value, {"a": 1, "b": 1, "c": 1, "d": 1})


class SynthRequireTest(unittest.TestCase):
    def test_require_equality_pins_the_field(self) -> None:
        resolved = _resolved(
            """
            packet p {
                n: varint
                require n == 777
            }
            """
        )
        value = synthesize_packet(resolved, resolved.packets["p"])
        self.assertEqual(value["n"], 777)

    def test_handshake_protocol_version_from_require_not_special_case(self) -> None:
        resolved = load_with_extra("")
        value = synthesize_packet(resolved, resolved.packets["handshake"])
        self.assertEqual(value["protocol_version"], 777)


class SynthNbtTest(unittest.TestCase):
    def test_nbt_defaults_are_valid_for_vanilla(self) -> None:
        resolved = _resolved(
            """
            packet probe {
                root: nbt
                chat: text_component
            }
            """
        )
        value = synthesize_packet(resolved, resolved.packets["probe"])
        self.assertEqual(value["root"], {"tag_type": 10, "value": {"entries": []}})
        self.assertEqual(value["chat"], {"tag_type": 8, "value": "abc"})


class SynthOverridesTest(unittest.TestCase):
    def test_overrides_apply_by_packet_and_field(self) -> None:
        resolved = _resolved("packet probe { x: varint }")
        original = dict(SYNTH_OVERRIDES)
        try:
            SYNTH_OVERRIDES[("probe", "x")] = 99
            value = synthesize_packet(resolved, resolved.packets["probe"])
            self.assertEqual(value["x"], 99)
        finally:
            SYNTH_OVERRIDES.clear()
            SYNTH_OVERRIDES.update(original)

    def test_override_for_missing_packet_is_an_error(self) -> None:
        resolved = load_minecraft()
        original = dict(SYNTH_OVERRIDES)
        try:
            SYNTH_OVERRIDES[("no_such_packet", "x")] = 1
            with self.assertRaises(BpsError):
                validate_all_synth_overrides(resolved)
        finally:
            SYNTH_OVERRIDES.clear()
            SYNTH_OVERRIDES.update(original)

    def test_override_for_missing_field_is_an_error(self) -> None:
        resolved = _resolved("packet probe { x: varint }")
        original = dict(SYNTH_OVERRIDES)
        try:
            SYNTH_OVERRIDES[("probe", "no_such_field")] = 1
            with self.assertRaises(BpsError):
                synthesize_packet(resolved, resolved.packets["probe"])
        finally:
            SYNTH_OVERRIDES.clear()
            SYNTH_OVERRIDES.update(original)


class SynthEncodingDefaultsTest(unittest.TestCase):
    def test_item_stack_template_default_has_zero_component_counts(self) -> None:
        resolved = load_with_extra("packet probe { stack: item_stack_template }")
        value = synthesize_packet(resolved, resolved.packets["probe"])
        self.assertEqual(
            value["stack"],
            {
                "item_id": 1,
                "item_count": 1,
                "component_added": 0,
                "component_removed": 0,
            },
        )

    def test_item_cost_default_has_zero_predicate_count(self) -> None:
        resolved = load_with_extra("packet probe { cost: item_cost }")
        value = synthesize_packet(resolved, resolved.packets["probe"])
        self.assertEqual(
            value["cost"],
            {"item_id": 1, "count": 1, "predicate_count": 0},
        )


class SynthProductionOverridesTest(unittest.TestCase):
    def test_production_synth_overrides_are_non_empty(self) -> None:
        self.assertGreater(len(SYNTH_OVERRIDES), 0)

    def test_production_synth_overrides_validate_against_minecraft_schema(self) -> None:
        resolved = load_minecraft()
        validate_all_synth_overrides(resolved)
        seen: set[str] = set()
        for pkt_name, _field in SYNTH_OVERRIDES:
            if pkt_name in seen:
                continue
            seen.add(pkt_name)
            synthesize_packet(resolved, resolved.packets[pkt_name])

    def test_client_information_view_distance_is_valid(self) -> None:
        resolved = load_minecraft()
        value = synthesize_packet(resolved, resolved.packets["client_information"])
        self.assertEqual(value["view_distance"], 8)

    def test_login_disconnect_reason_is_json_string_component(self) -> None:
        resolved = load_minecraft()
        value = synthesize_packet(resolved, resolved.packets["login_disconnect"])
        self.assertEqual(value["reason"], '"abc"')

    def test_debug_sample_type_is_the_only_enum_constant(self) -> None:
        resolved = load_minecraft()
        value = synthesize_packet(resolved, resolved.packets["debug_sample"])
        self.assertEqual(value["sample_type"], 0)

    def test_explode_particle_uses_registry_id_zero(self) -> None:
        resolved = load_minecraft()
        value = synthesize_packet(resolved, resolved.packets["explode"])
        self.assertEqual(value["particle"], {"particle_type": 0, "value": {}})

    def test_level_particles_particle_uses_registry_id_zero(self) -> None:
        resolved = load_minecraft()
        value = synthesize_packet(resolved, resolved.packets["level_particles"])
        self.assertEqual(value["particle"], {"particle_type": 0, "value": {}})

    def test_update_recipes_property_set_uses_hand_known_key(self) -> None:
        resolved = load_minecraft()
        value = synthesize_packet(resolved, resolved.packets["update_recipes"])
        self.assertEqual(
            value["property_sets"],
            [{"property_set_id": "smithing_base", "items_count": 1, "items": [1]}],
        )

    def test_custom_click_action_payload_size_matches_nbt_bytes(self) -> None:
        resolved = load_minecraft()
        value = synthesize_packet(resolved, resolved.packets["custom_click_action"])
        self.assertEqual(value["payload_size"], 2)
        self.assertEqual(value["payload"], {"tag_type": 10, "value": {"entries": []}})


if __name__ == "__main__":
    unittest.main()

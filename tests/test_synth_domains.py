"""Synthetic value domains: small valid scalars, require pins, NBT shapes, overrides."""

import unittest

from bps.errors import BpsError
from bps.synth import SYNTH_OVERRIDES, synthesize_packet

from tests.test_varint import load_with_extra


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
        resolved = _resolved("packet probe { x: varint }")
        original = dict(SYNTH_OVERRIDES)
        try:
            SYNTH_OVERRIDES[("no_such_packet", "x")] = 1
            with self.assertRaises(BpsError):
                synthesize_packet(resolved, resolved.packets["probe"])
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


if __name__ == "__main__":
    unittest.main()

"""Statement match/switch, and alternative tags on field match.

A statement arm is a block whose fields land on the parent value, the same way
an `if` group does. `match` and `switch` are the same statement. `0 | 2 =>`
selects that arm for either tag. `_` is the arm used when nothing else matches.
"""

import unittest

from bps import ast
from bps.analyze import analyze
from bps.errors import DecodeError, EncodeError, ParseError
from bps.interp.engine import decode_packet, encode_packet
from bps.parser import parse


def _resolved(source: str):
    return analyze(parse(source))


def _expr_ints(expr: ast.Expr | None) -> set[int]:
    if expr is None:
        return set()
    found: set[int] = set()
    if expr.kind == "int":
        found.add(int(expr.value))
    for child in (expr.left, expr.right, *expr.args):
        if child is not None:
            found |= _expr_ints(child)
    return found


class StatementMatchTest(unittest.TestCase):
    SOURCE = """
    packet probe {
        action: u8
        match (action) {
            0 => { x: u8 }
            1 | 2 => { y: u8 }
            _ => { z: u8 }
        }
    }
    """

    def test_match_splices_the_selected_arm(self) -> None:
        resolved = _resolved(self.SOURCE)
        cases = [
            (bytes([0, 0x11]), {"action": 0, "x": 0x11}),
            (bytes([1, 0x22]), {"action": 1, "y": 0x22}),
            (bytes([2, 0x22]), {"action": 2, "y": 0x22}),
            (bytes([9, 0x33]), {"action": 9, "z": 0x33}),
        ]
        for raw, value in cases:
            decoded = decode_packet(resolved, "probe", raw)
            self.assertEqual(decoded, value)
            self.assertEqual(encode_packet(resolved, "probe", value), raw)

    def test_switch_is_the_same_statement(self) -> None:
        resolved = _resolved(
            """
            packet probe {
                action: u8
                switch (action) {
                    0 => { title: u8  health: u8 }
                    _ => { }
                }
            }
            """
        )
        raw = bytes([0, 0x01, 0x02])
        value = {"action": 0, "title": 1, "health": 2}
        self.assertEqual(decode_packet(resolved, "probe", raw), value)
        self.assertEqual(encode_packet(resolved, "probe", value), raw)
        other = bytes([4])
        self.assertEqual(decode_packet(resolved, "probe", other), {"action": 4})
        self.assertEqual(encode_packet(resolved, "probe", {"action": 4}), other)

    def test_unmatched_tag_is_an_error(self) -> None:
        resolved = _resolved(
            """
            packet probe {
                action: u8
                match (action) {
                    0 => { x: u8 }
                }
            }
            """
        )
        with self.assertRaises(DecodeError):
            decode_packet(resolved, "probe", bytes([1]))
        with self.assertRaises(EncodeError):
            encode_packet(resolved, "probe", {"action": 1})

    def test_statement_arm_must_be_a_block(self) -> None:
        with self.assertRaises(ParseError):
            parse(
                """
                packet probe {
                    action: u8
                    match (action) { 0 => u8 }
                }
                """
            )

    def test_synthesized_value_round_trips(self) -> None:
        from bps.synth import synthesize_packet

        resolved = _resolved(self.SOURCE)
        value = synthesize_packet(resolved, resolved.packets["probe"])
        raw = encode_packet(resolved, "probe", value)
        self.assertEqual(decode_packet(resolved, "probe", raw), value)

    def test_rust_decoder_emits_or_pattern(self) -> None:
        from bps.emit_rust import Walker

        resolved = _resolved(self.SOURCE)
        text = Walker(resolved).packet_fn(resolved.packets["probe"])
        self.assertIn("1 | 2", text)

    def test_rstom_shape_ors_alternative_tags(self) -> None:
        from bps.emit_rstom import _flatten

        resolved = _resolved(self.SOURCE)
        shapes: list = []
        _flatten(resolved.packets["probe"].items, shapes, resolved)
        y = next(s for s in shapes if s.ident == "y")
        self.assertEqual(_expr_ints(y.guard), {1, 2})


class FieldNamedMatchTest(unittest.TestCase):
    def test_match_before_a_colon_is_a_field_name(self) -> None:
        resolved = _resolved(
            """
            packet probe {
                matches: { match: u8 }
            }
            """
        )
        raw = bytes([7])
        self.assertEqual(decode_packet(resolved, "probe", raw), {"matches": {"match": 7}})
        self.assertEqual(encode_packet(resolved, "probe", {"matches": {"match": 7}}), raw)


class FieldMatchAlternativesTest(unittest.TestCase):
    def test_or_tags_share_one_arm(self) -> None:
        resolved = _resolved(
            """
            packet probe {
                kind: u8
                value: match kind {
                    0 | 1 => u8
                    _ => u8
                }
            }
            """
        )
        for raw, value in (
            (bytes([0, 0x2A]), {"kind": 0, "value": 0x2A}),
            (bytes([1, 0x2A]), {"kind": 1, "value": 0x2A}),
            (bytes([7, 0x03]), {"kind": 7, "value": 0x03}),
        ):
            self.assertEqual(decode_packet(resolved, "probe", raw), value)
            self.assertEqual(encode_packet(resolved, "probe", value), raw)

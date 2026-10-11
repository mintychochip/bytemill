import struct
import unittest

from bps.ast import TypeRef
from bps.interp.cursor import WriteCursor
from bps.interp.engine import _encode_type_ref, decode_frame_payload

from tests.test_varint import load_minecraft


def frame(resolved, pid: int, payload: bytes) -> bytes:
    """Length-prefixed frame whose length and id are the imported `varint` schema."""

    def vint(n: int) -> bytes:
        w = WriteCursor()
        _encode_type_ref(TypeRef("varint"), w, resolved, {}, n)
        return w.result()

    body = vint(pid) + payload
    return vint(len(body)) + body


class PlayGolden(unittest.TestCase):
    def test_block_update(self) -> None:
        resolved = load_minecraft()
        raw = frame(resolved, 0x08, bytes.fromhex("4607632c15b4833f") + bytes.fromhex("01"))
        pid, out = decode_frame_payload(resolved, "play", "clientbound", raw)
        self.assertEqual(pid, 0x08)
        fields = out["fields"]
        self.assertEqual(fields["location"]["x"], 18357644)
        self.assertEqual(fields["location"]["y"], 831)
        self.assertEqual(fields["block_id"], 1)

    def test_animate(self) -> None:
        resolved = load_minecraft()
        raw = frame(resolved, 0x02, bytes.fromhex("ac02") + bytes([2]))
        _, out = decode_frame_payload(resolved, "play", "clientbound", raw)
        self.assertEqual(out["packet"], "animate")
        self.assertEqual(out["fields"]["entity_id"], 300)
        self.assertEqual(out["fields"]["animation"], 2)

    def test_game_event(self) -> None:
        resolved = load_minecraft()
        raw = frame(resolved, 0x27, bytes([1]) + struct.pack(">f", 1.5))
        _, out = decode_frame_payload(resolved, "play", "clientbound", raw)
        self.assertEqual(out["fields"]["event"], 1)
        self.assertAlmostEqual(out["fields"]["value"], 1.5)

    def test_bundle_delimiter_is_empty(self) -> None:
        resolved = load_minecraft()
        raw = frame(resolved, 0x00, b"")
        _, out = decode_frame_payload(resolved, "play", "clientbound", raw)
        self.assertEqual(out["packet"], "bundle_delimiter")
        self.assertEqual(out["fields"], {})

    def test_set_time_clock_array(self) -> None:
        # set_time: world_age i64be, clocks_count varint, clocks[] of
        # { clock_id varint, time varlong, fractional_time f32be, rate f32be }
        resolved = load_minecraft()
        payload = (
            struct.pack(">q", 100)
            + bytes([1])
            + bytes([7])
            + bytes([0xC8, 0x01])
            + struct.pack(">f", 0.5)
            + struct.pack(">f", 1.0)
        )
        _, out = decode_frame_payload(resolved, "play", "clientbound", frame(resolved, 0x73, payload))
        fields = out["fields"]
        self.assertEqual(fields["world_age"], 100)
        self.assertEqual(fields["clocks_count"], 1)
        clock = fields["clocks"][0]
        self.assertEqual(clock["clock_id"], 7)
        self.assertEqual(clock["time"], 200)
        self.assertAlmostEqual(clock["fractional_time"], 0.5)
        self.assertAlmostEqual(clock["rate"], 1.0)


if __name__ == "__main__":
    unittest.main()

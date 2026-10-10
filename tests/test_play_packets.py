import struct
import unittest

from bps.interp.cursor import WriteCursor
from bps.interp.engine import _write_leb128, decode_frame_payload

from tests.test_varint import load_minecraft


def frame(pid: int, payload: bytes) -> bytes:
    body = WriteCursor()
    _write_leb128(body, pid, 32)
    body.emit(payload)
    raw = body.result()
    out = WriteCursor()
    _write_leb128(out, len(raw), 32)
    out.emit(raw)
    return out.result()


class PlayGolden(unittest.TestCase):
    def test_block_update(self) -> None:
        resolved = load_minecraft()
        raw = frame(0x08, bytes.fromhex("4607632c15b4833f") + bytes.fromhex("01"))
        pid, out = decode_frame_payload(resolved, "play", "clientbound", raw)
        self.assertEqual(pid, 0x08)
        fields = out["fields"]
        self.assertEqual(fields["location"]["x"], 18357644)
        self.assertEqual(fields["location"]["y"], 831)
        self.assertEqual(fields["block_id"], 1)

    def test_animate(self) -> None:
        resolved = load_minecraft()
        raw = frame(0x02, bytes.fromhex("ac02") + bytes([2]))
        _, out = decode_frame_payload(resolved, "play", "clientbound", raw)
        self.assertEqual(out["packet"], "animate")
        self.assertEqual(out["fields"]["entity_id"], 300)
        self.assertEqual(out["fields"]["animation"], 2)

    def test_game_event(self) -> None:
        resolved = load_minecraft()
        raw = frame(0x27, bytes([1]) + struct.pack(">f", 1.5))
        _, out = decode_frame_payload(resolved, "play", "clientbound", raw)
        self.assertEqual(out["fields"]["event"], 1)
        self.assertAlmostEqual(out["fields"]["value"], 1.5)

    def test_bundle_delimiter_is_empty(self) -> None:
        resolved = load_minecraft()
        raw = frame(0x00, b"")
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
        _, out = decode_frame_payload(resolved, "play", "clientbound", frame(0x73, payload))
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

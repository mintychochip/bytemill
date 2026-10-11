import unittest
from pathlib import Path

from bps.analyze import analyze
from bps.ast import Schema
from bps.interp.engine import decode_packet, encode_packet
from bps.load import load_schema
from bps.parser import parse

ROOT = Path(__file__).resolve().parents[1]


def load_nbt():
    core = load_schema([ROOT / "schemas" / "minecraft_core.bps"])
    probe = parse("packet nb { t: nbt }")
    return analyze(Schema(core.encodings, (), probe.packets, ()))


class NbtGolden(unittest.TestCase):
    def test_network_nbt_roundtrip(self) -> None:
        """Network NBT is the root tag id, then its payload with no root name."""
        resolved = load_nbt()
        # { "a": "hi" (string), "n": 7 (int), "l": [1, 2] (int list) }
        raw = bytes.fromhex(
            "0a"                # root tag id: TAG_Compound
            "0800016100026869"  # string "a" = "hi"
            "0300016e00000007"  # int "n" = 7
            "0900016c"          # list "l"
            "0300000002"        # elem type int, count 2
            "0000000100000002"  # 1, 2
            "00"                # TAG_End
        )
        out = decode_packet(resolved, "nb", raw, strict=True)
        self.assertEqual(out["t"]["tag_type"], 10)
        entries = out["t"]["value"]["entries"]
        self.assertEqual(len(entries), 3)
        self.assertEqual(entries[0], {"tag_type": 8, "name": "a", "value": "hi"})
        self.assertEqual(entries[1], {"tag_type": 3, "name": "n", "value": 7})
        self.assertEqual(entries[2]["tag_type"], 9)
        self.assertEqual(entries[2]["value"]["items"], [{"v": 1}, {"v": 2}])
        back = encode_packet(resolved, "nb", out)
        self.assertEqual(back, raw)

    def test_an_absent_tag_is_a_lone_tag_end(self) -> None:
        """Java writes TAG_End (id 0, no payload) for a null tag, e.g. an empty optional."""
        resolved = load_nbt()
        out = decode_packet(resolved, "nb", b"\x00", strict=True)
        self.assertEqual(out["t"]["tag_type"], 0)
        self.assertEqual(encode_packet(resolved, "nb", out), b"\x00")

    def test_an_empty_compound_is_its_id_then_its_end(self) -> None:
        """The bytes Minestom writes for an empty compound: `0a 00`."""
        resolved = load_nbt()
        out = decode_packet(resolved, "nb", bytes.fromhex("0a00"), strict=True)
        self.assertEqual(out["t"], {"tag_type": 10, "value": {"entries": []}})
        self.assertEqual(encode_packet(resolved, "nb", out), bytes.fromhex("0a00"))


if __name__ == "__main__":
    unittest.main()

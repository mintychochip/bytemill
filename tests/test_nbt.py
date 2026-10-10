import unittest
from pathlib import Path

from bps.analyze import analyze
from bps.ast import Schema
from bps.interp.engine import decode_packet, encode_packet
from bps.parser import parse

ROOT = Path(__file__).resolve().parents[1]


def load_nbt():
    core = parse((ROOT / "schemas" / "minecraft_core.bps").read_text(encoding="utf-8"))
    probe = parse("packet nb { t: nbt }")
    return analyze(Schema(core.encodings, (), probe.packets, ()))


class NbtGolden(unittest.TestCase):
    def test_network_nbt_roundtrip(self) -> None:
        """Network NBT: root compound without name, records until TAG_End (0x00)."""
        resolved = load_nbt()
        # { "a": "hi" (string), "n": 7 (int), "l": [1, 2] (int list) }
        raw = bytes.fromhex(
            "0800016100026869"  # string "a" = "hi"
            "0300016e00000007"  # int "n" = 7
            "0900016c"          # list "l"
            "0300000002"        # elem type int, count 2
            "0000000100000002"  # 1, 2
            "00"                # TAG_End
        )
        out = decode_packet(resolved, "nb", raw)
        entries = out["t"]["entries"]
        self.assertEqual(len(entries), 3)
        self.assertEqual(entries[0], {"tag_type": 8, "name": "a", "value": "hi"})
        self.assertEqual(entries[1], {"tag_type": 3, "name": "n", "value": 7})
        self.assertEqual(entries[2]["tag_type"], 9)
        self.assertEqual(entries[2]["value"]["items"], [{"v": 1}, {"v": 2}])
        back = encode_packet(resolved, "nb", out)
        self.assertEqual(back, raw)


if __name__ == "__main__":
    unittest.main()

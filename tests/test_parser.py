import unittest
from pathlib import Path

from bps.parser import parse

ROOT = Path(__file__).resolve().parents[1]


class ParserSmoke(unittest.TestCase):
    def test_parse_core(self) -> None:
        src = (ROOT / "schemas" / "minecraft_core.bps").read_text(encoding="utf-8")
        s = parse(src)
        self.assertEqual(s.imports, ("std/integers.bps", "std/varint.bps"))
        self.assertFalse(any(e.name == "varint" for e in s.encodings))

    def test_parse_all_schemas(self) -> None:
        for path in sorted((ROOT / "schemas").rglob("*.bps")):
            parse(path.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()

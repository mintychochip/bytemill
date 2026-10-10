import unittest

from bps.parser import parse


class ParserSmoke(unittest.TestCase):
    def test_parse_core(self) -> None:
        src = open("schemas/minecraft_core.bps", encoding="utf-8").read()
        s = parse(src)
        self.assertTrue(any(e.name == "varint" for e in s.encodings))


if __name__ == "__main__":
    unittest.main()

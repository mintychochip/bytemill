import unittest

from bps.errors import DecodeError, ParseError


class ErrorsTest(unittest.TestCase):
    def test_offset_optional(self) -> None:
        e = DecodeError("bad", offset=3)
        self.assertIn("3", str(e))
        self.assertEqual(e.offset, 3)

    def test_parse_error(self) -> None:
        e = ParseError("syntax")
        self.assertIsInstance(e, Exception)


if __name__ == "__main__":
    unittest.main()

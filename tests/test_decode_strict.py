"""`decode_packet(strict=True)` fails when a packet's layout stops short of its bytes.

By default trailing bytes are ignored. A layout that under-reads real packets is
exactly the bug that hides there, so checks against captured bytes decode strictly.
"""

import unittest

from bps.analyze import analyze
from bps.errors import DecodeError
from bps.interp.engine import decode_packet
from bps.parser import parse


class StrictDecodeTest(unittest.TestCase):
    def setUp(self) -> None:
        self.resolved = analyze(parse("packet probe { x: u8 }"))

    def test_trailing_bytes_are_an_error(self) -> None:
        with self.assertRaisesRegex(DecodeError, "1 of 2 bytes"):
            decode_packet(self.resolved, "probe", b"\x01\x02", strict=True)

    def test_a_packet_that_uses_every_byte_decodes(self) -> None:
        self.assertEqual(decode_packet(self.resolved, "probe", b"\x07", strict=True), {"x": 7})


if __name__ == "__main__":
    unittest.main()

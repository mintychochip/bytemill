"""Unit tests for the template-independent packet wire body seam."""

import tempfile
import unittest
from pathlib import Path

from bps.analyze import analyze
from bps.emit_rstom import _packet_body
from bps.load import load_schema
from bps.minestom import build_units

from tests.rstom_tables import PROBE_TABLE, probe_source

ROOT = Path(__file__).resolve().parents[1]


def _probe_unit(body: str):
    source = (
        f'import "{ROOT / "schemas" / "minecraft_core.bps"}"\n'
        + probe_source(ROOT, body)
    )
    with tempfile.TemporaryDirectory() as td:
        schema = Path(td) / "probe.bps"
        schema.write_text(source, encoding="utf-8")
        resolved = analyze(load_schema([schema]))
    units = build_units(resolved, PROBE_TABLE)
    return next(u for u in units if u.pkt.name == "probe"), resolved


class PacketBodyTest(unittest.TestCase):
    def test_probe_packet_wire_body(self) -> None:
        unit, resolved = _probe_unit("packet probe { a: i32be b: string c: u8 }")
        body = _packet_body(unit, resolved)

        self.assertEqual([f.ident for f in body.fields], ["a", "b", "c"])
        self.assertEqual(body.field_types["a"], "i32")
        self.assertEqual(body.field_types["b"], "String")
        self.assertEqual(body.field_types["c"], "i8")

        self.assertIn("buffer.write(INT, value.a.clone());", body.writes)
        self.assertIn("buffer.write(STRING, value.b.clone());", body.writes)
        self.assertIn("buffer.write(BYTE, value.c.clone());", body.writes)

        self.assertIn("let a = buffer.read(INT);", body.reads)
        self.assertIn("let b = buffer.read(STRING);", body.reads)
        self.assertIn("let c = buffer.read(BYTE);", body.reads)

        self.assertEqual(
            body.ctor,
            "a: i32,\n        b: String,\n        c: i8",
        )
        self.assertEqual(body.init, "a,\n            b,\n            c")
        self.assertEqual(body.used_wires, ["BYTE", "INT", "STRING"])
        self.assertFalse(body.needs_fixed_raw_bytes)
        self.assertEqual(body.rtypes, {"String", "i32", "i8"})

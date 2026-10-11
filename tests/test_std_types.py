"""Scalar schemas built from the bare primitives, and import of those schemas.

The compiler's primitive set is `u8`, `bytes`, `rest`, `f32be`, and `f64be`.
Wider integers and LEB128 integers are ordinary encodings: a block reads and
writes bytes, and a trailing `= expr` is the scalar value of the encoding.
"""

import tempfile
import unittest
from pathlib import Path

from bps.analyze import analyze
from bps.ast import Schema
from bps.emit_rstom import emit_rstom
from bps.emit_rust import emit_rust
from bps.errors import BpsError, DecodeError, ResolveError
from bps.interp.engine import decode_packet, encode_packet
from bps.parser import parse

ROOT = Path(__file__).resolve().parents[1]


def _roundtrip(source: str, packet: str, value: dict) -> tuple[bytes, dict]:
    schema = parse(source)
    resolved = analyze(schema)
    encoded = encode_packet(resolved, packet, value)
    decoded = decode_packet(resolved, packet, encoded)
    return encoded, decoded


class ScalarIntegerTest(unittest.TestCase):
    def test_u16be_is_two_bytes_built_from_u8(self) -> None:
        encoded, decoded = _roundtrip(
            """
            encoding u16be = {
                hi: u8 = (value >> 8) & 0xFF
                lo: u8 = value & 0xFF
            } = (hi << 8) | lo
            packet probe { n: u16be }
            """,
            "probe",
            {"n": 0x1234},
        )
        self.assertEqual(encoded, bytes.fromhex("1234"))
        self.assertEqual(decoded["n"], 0x1234)

    def test_signed_widths_roundtrip_negative_one(self) -> None:
        source = (ROOT / "schemas" / "std" / "integers.bps").read_text(encoding="utf-8")
        source += """
        packet probe { a: i8  b: i16be  c: i32be  d: i64be  e: u16le }
        """
        resolved = analyze(parse(source))
        value = {"a": -1, "b": -1, "c": -1, "d": -1, "e": 0x1234}
        encoded = encode_packet(resolved, "probe", value)
        self.assertEqual(
            encoded,
            bytes.fromhex("ff ffff ffffffff ffffffffffffffff 3412"),
        )
        self.assertEqual(decode_packet(resolved, "probe", encoded), value)


class VarintSchemaTest(unittest.TestCase):
    def test_varint_matches_wiki_samples(self) -> None:
        source = (ROOT / "schemas" / "std" / "varint.bps").read_text(encoding="utf-8")
        source += "packet probe { n: varint  m: varlong }\n"
        resolved = analyze(parse(source))
        enc = encode_packet(resolved, "probe", {"n": 25565, "m": 0})
        self.assertEqual(enc[:3], bytes.fromhex("ddc701"))
        self.assertEqual(decode_packet(resolved, "probe", enc)["n"], 25565)
        neg = encode_packet(resolved, "probe", {"n": -1, "m": -1})
        self.assertEqual(neg[:5], bytes.fromhex("ffffffff0f"))
        self.assertEqual(neg[5:], bytes.fromhex("ffffffffffffffffff01"))
        decoded = decode_packet(resolved, "probe", neg)
        self.assertEqual(decoded["n"], -1)
        self.assertEqual(decoded["m"], -1)

    def test_overlong_varint_is_rejected(self) -> None:
        source = (ROOT / "schemas" / "std" / "varint.bps").read_text(encoding="utf-8")
        source += "packet probe { n: varint }\n"
        resolved = analyze(parse(source))
        with self.assertRaises(DecodeError):
            decode_packet(resolved, "probe", bytes.fromhex("ffffffffff"))


class ImportTest(unittest.TestCase):
    def test_minecraft_core_imports_std_schemas(self) -> None:
        from bps.load import load_schema

        loaded = load_schema([ROOT / "schemas" / "minecraft_core.bps"])
        names = {e.name for e in loaded.encodings}
        self.assertIn("varint", names)
        self.assertIn("u16be", names)
        self.assertIn("boolean", names)
        self.assertNotIn("leb128", names)

    def test_leb128_is_not_a_type(self) -> None:
        schema = parse("encoding bad = leb128(32, signed)\n")
        with self.assertRaises(ResolveError):
            analyze(schema)

    def test_imported_varint_feeds_the_handshake_port(self) -> None:
        from bps.load import load_schema

        loaded = load_schema([ROOT / "schemas" / "minecraft_core.bps"])
        extra = parse("packet probe { n: varint }")
        resolved = analyze(
            Schema(
                loaded.encodings + extra.encodings,
                loaded.enums,
                loaded.packets + extra.packets,
                loaded.protocols,
            )
        )
        encoded = encode_packet(resolved, "probe", {"n": 25565})
        self.assertEqual(encoded, bytes.fromhex("ddc701"))
        self.assertEqual(decode_packet(resolved, "probe", encoded)["n"], 25565)


class EmitterScalarTest(unittest.TestCase):
    """The Rust emitters lower the std scalars by name and nothing else.

    A scalar they cannot lower would come out as its wire bytes (`b0`, `b1`,
    `b2`) instead of one value, so emission refuses it.
    """

    U24_PROBE = """
    encoding u24be = {
        b0: u8 = (value >> 16) & 0xFF
        b1: u8 = (value >> 8) & 0xFF
        b2: u8 = value & 0xFF
    } = (b0 << 16) | (b1 << 8) | b2
    packet probe { x: u24be }
    """

    @staticmethod
    def _resolved(source: str):
        std = "".join(
            (ROOT / "schemas" / "std" / f"{name}.bps").read_text(encoding="utf-8")
            for name in ("integers", "varint")
        )
        return analyze(parse(std + source))

    def test_rust_emitter_rejects_unlowerable_scalar(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            with self.assertRaisesRegex(BpsError, "u24be"):
                emit_rust(self._resolved(self.U24_PROBE), Path(td))

    def test_rstom_emitter_rejects_unlowerable_scalar(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            with self.assertRaisesRegex(BpsError, "u24be"):
                emit_rstom(self._resolved(self.U24_PROBE), Path(td))

    def test_rstom_keeps_std_scalar_default_arm_as_one_value(self) -> None:
        source = "packet probe { kind: varint  value: match kind { 0 => i8, _ => varint } }"
        with tempfile.TemporaryDirectory() as td:
            emit_rstom(self._resolved(source), Path(td))
            text = (Path(td) / "src" / "packets" / "probe.rs").read_text(encoding="utf-8")
        self.assertNotIn("pub b0", text)


if __name__ == "__main__":
    unittest.main()

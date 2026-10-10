import unittest
from pathlib import Path

from bps.analyze import analyze
from bps.ast import Schema
from bps.interp.engine import decode_frame_payload, decode_packet, encode_packet
from bps.parser import parse

ROOT = Path(__file__).resolve().parents[1]


def load_minecraft():
    enc, enu, pkt, proto = [], [], [], []
    for name in (
        "minecraft_core.bps",
        "minecraft_26_3.bps",
        "minecraft_26_3_login.bps",
        "minecraft_26_3_play.bps",
        "minecraft_26_3_play_serverbound.bps",
        "minecraft_26_3_wiki.bps",
    ):
        s = parse((ROOT / "schemas" / name).read_text(encoding="utf-8"))
        enc.extend(s.encodings)
        enu.extend(s.enums)
        pkt.extend(s.packets)
        proto.extend(s.protocols)
    return analyze(Schema(tuple(enc), tuple(enu), tuple(pkt), tuple(proto)))


def load_with_extra(source: str):
    base = load_minecraft()
    extra = parse(source)
    return analyze(
        Schema(
            base.schema.encodings + extra.encodings,
            base.schema.enums + extra.enums,
            base.schema.packets + extra.packets,
            base.schema.protocols + extra.protocols,
        )
    )


class VarintGolden(unittest.TestCase):
    def test_varint_25565_roundtrip(self) -> None:
        merged = load_with_extra("packet varint_probe { n: varint }")
        enc = encode_packet(merged, "varint_probe", {"n": 25565})
        self.assertEqual(enc, bytes.fromhex("ddc701"))
        dec = decode_packet(merged, "varint_probe", enc)
        self.assertEqual(dec["n"], 25565)


class HandshakeGolden(unittest.TestCase):
    def test_handshake_frame(self) -> None:
        resolved = load_minecraft()
        frame = bytes.fromhex("10008906096c6f63616c686f737463dd01")
        pid, out = decode_frame_payload(resolved, "handshaking", "serverbound", frame)
        self.assertEqual(pid, 0)
        self.assertEqual(out["packet"], "handshake")
        fields = out["fields"]
        self.assertEqual(fields["protocol_version"], 777)
        self.assertEqual(fields["server_address"], "localhost")
        self.assertEqual(fields["server_port"], 25565)
        self.assertEqual(fields["intent"], 1)


class PositionGolden(unittest.TestCase):
    def test_position_decode(self) -> None:
        core = parse((ROOT / "schemas" / "minecraft_core.bps").read_text(encoding="utf-8"))
        merged = analyze(
            Schema(
                core.encodings,
                (),
                parse("packet pos { p: position }").packets,
                (),
            )
        )
        raw = bytes.fromhex("4607632c15b4833f")
        dec = decode_packet(merged, "pos", raw)
        self.assertEqual(dec["p"]["x"], 18357644)
        self.assertEqual(dec["p"]["z"], -20882616)
        self.assertEqual(dec["p"]["y"], 831)


class MetadataGolden(unittest.TestCase):
    def test_entity_metadata_sample(self) -> None:
        core = parse((ROOT / "schemas" / "minecraft_core.bps").read_text(encoding="utf-8"))
        merged = analyze(
            Schema(
                core.encodings,
                (),
                parse("packet md { m: entity_metadata }").packets,
                (),
            )
        )
        raw = bytes.fromhex("0000010201ac02ff")
        dec = decode_packet(merged, "md", raw)
        entries = dec["m"]["entries"]
        self.assertEqual(len(entries), 2)
        self.assertEqual(entries[1]["kind"], 1)
        self.assertEqual(entries[1]["value"], 300)


if __name__ == "__main__":
    unittest.main()

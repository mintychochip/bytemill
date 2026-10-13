"""Real-tree import invariants: kept hand `use` lines bind names the packet needs, none owned by the generator."""

from __future__ import annotations

import re
import unittest

from bps.emit_rstom import WIRE_TYPES
from bps.minestom import Unit, build_units
from bps.shapes import Shape, ShapeError, extract_shape, hand_packet_path, module_path

from tests.rstom_tables import RSTOM, real_table
from tests.test_varint import load_minecraft

RSTOM_PACKET = RSTOM / "crates" / "rstom" / "src" / "network" / "packet"
_KNOWN_UNEXTRACTABLE = frozenset({"ClientSignedCommandChatPacket", "ShowDialogPacket"})
GENERATOR_OWNED = frozenset(
    {
        "Arc",
        "ClientPacket",
        "ClientPacketGroup",
        "ConstantSerializer",
        "NetworkBuffer",
        "Packet",
        "PacketSerializer",
        "PacketValue",
        "ServerPacket",
        "ServerPacketGroup",
        "Uuid",
        "downcast",
        "fixed_raw_bytes",
        "FIXED_BYTES",
        "RAW_BYTES",
        "VAR_INT",
    }
    | {wire for wire, _ in WIRE_TYPES.values()}
)
_USE = re.compile(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t][^;]*;")
_COORD_VEC_IMPORT = re.compile(r"crate::coordinate::(?:vec::)?(?:Vec\b|\{[^}]*\bVec\b)")
_BARE_VEC = re.compile(r"(?<![\w:])Vec(?!\s*<)")


def _bound_names(statement: str) -> list[str]:
    inner = re.sub(r"^use\s+", "", statement).rstrip(";")
    names: list[str] = []
    for part in re.split(r"[{},]", inner):
        part = part.strip()
        if not part or part.endswith("::"):
            continue
        name = part.rsplit(" as ", 1)[1] if " as " in part else part.rsplit("::", 1)[-1]
        names.append(name.strip())
    return [n for n in names if n not in ("", "_", "*", "self")]


@unittest.skipUnless(RSTOM_PACKET.is_dir(), "rstom worktree packet sources not present")
class RealTreeImportsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        resolved = load_minecraft()
        units = build_units(resolved, real_table())
        cls.pairs: list[tuple[Unit, Shape]] = []
        cls.skipped: list[str] = []
        for unit in units:
            path = hand_packet_path(RSTOM, unit)
            if not path.is_file():
                cls.skipped.append(unit.cls.name)
                continue
            try:
                shape = extract_shape(
                    path.read_text(encoding="utf-8"),
                    unit.cls.name,
                    module_path(unit),
                    packet_path=path,
                )
            except ShapeError:
                cls.skipped.append(unit.cls.name)
                continue
            cls.pairs.append((unit, shape))

    @staticmethod
    def _hand_and_siblings(unit) -> tuple[str, str]:
        path = hand_packet_path(RSTOM, unit)
        siblings = "\n".join(
            p.read_text(encoding="utf-8") for p in path.parent.glob(f"{unit.cls.stem}.*.rs")
        )
        return path.read_text(encoding="utf-8"), siblings

    def test_every_kept_import_binds_a_name_the_packet_uses_and_the_generator_does_not_own(
        self,
    ) -> None:
        for unit, shape in self.pairs:
            hand, siblings = self._hand_and_siblings(unit)
            needed = _USE.sub("", hand) + siblings
            for statement in shape.imports:
                if statement.startswith("pub "):
                    continue
                for name in _bound_names(statement):
                    with self.subTest(cls=shape.cls, statement=statement, name=name):
                        self.assertNotIn(name, GENERATOR_OWNED)
                        self.assertRegex(needed, rf"\b{re.escape(name)}\b")

    def test_a_packet_importing_the_coordinate_vec_has_no_bare_vec_field(self) -> None:
        for unit, shape in self.pairs:
            hand, _ = self._hand_and_siblings(unit)
            if not _COORD_VEC_IMPORT.search(hand):
                continue
            for field in shape.fields:
                with self.subTest(cls=shape.cls, field=field.name):
                    self.assertIsNone(_BARE_VEC.search(field.type))

    def test_only_known_unextractable_packets_are_skipped(self) -> None:
        unexpected = sorted(set(self.skipped) - _KNOWN_UNEXTRACTABLE)
        self.assertEqual(unexpected, [])

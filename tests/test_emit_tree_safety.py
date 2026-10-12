"""Safety and validation rules for ``emit_tree`` / ``tree_packet``."""

from __future__ import annotations

import json
import shutil
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

from bps.analyze import analyze
from bps.emit_rstom import _PacketBody, _packet_body
from bps.emit_tree import _assert_hand_content_preserved, _missing_hand_markers, emit_tree
from bps.errors import EmitError
from bps.load import load_schema
from bps.minestom import ClassTable, build_units
from bps.shapes import (
    FieldShape,
    NewShape,
    SerializerShape,
    Shape,
    extract_shape,
    shape_to_json,
)
from bps.tree_packet import _validate_shape, tree_packet

from tests.rstom_tables import PROBE_SCHEMA, PROBE_TABLE, RSTOM, class_entry, probe_source
from tests.test_emit_tree import (
    KEEP_ALIVE_TABLE,
    KEEP_ALIVE_TABLE_DATA,
    ROOT,
    _analyze_keep_alive,
    _keep_alive_shape,
    _write_shape,
)

RSTOM_PACKET = RSTOM / "crates" / "rstom" / "src" / "network" / "packet"


def _workspace_snapshot(ws: Path) -> dict[str, tuple[bool, int]]:
    """Every path under ``ws`` (files and directories), relative to ``ws``."""
    snap: dict[str, tuple[bool, int]] = {}
    for path in sorted(ws.rglob("*"), key=lambda p: p.as_posix()):
        rel = path.relative_to(ws).as_posix()
        st = path.stat()
        snap[rel] = (path.is_dir(), st.st_mtime_ns)
    return snap


class HandContentGuardTest(unittest.TestCase):
    def test_missing_doc_in_generated_raises(self) -> None:
        hand = "//! top\n/// keep me\npub struct X;\n"
        generated = "//! top\npub struct X;\n"
        self.assertEqual(_missing_hand_markers(hand, generated, ""), ["/// keep me"])

    def test_doc_only_in_ext_is_accepted(self) -> None:
        hand = "/// only in ext\npub struct X;\n"
        generated = "pub struct X;\n"
        ext = "/// only in ext\n"
        self.assertEqual(_missing_hand_markers(hand, generated, ext), [])

    def test_cfg_test_doc_is_ignored(self) -> None:
        hand = "/// real\n#[cfg(test)]\n/// test only\nmod t {}\n"
        generated = "/// real\n"
        self.assertEqual(_missing_hand_markers(hand, generated, ""), [])

    def test_assert_hand_content_preserved_names_module(self) -> None:
        with self.assertRaises(EmitError) as ctx:
            _assert_hand_content_preserved("m/a/x", "/// lost\n", "", None)
        self.assertIn("m/a/x", str(ctx.exception))
        self.assertIn("hand content lost", str(ctx.exception))


class EmitTreeSafetyTest(unittest.TestCase):
    def test_stale_ledger_module_raises(self) -> None:
        resolved, _ = _analyze_keep_alive()
        shapes = Path(tempfile.mkdtemp())
        orphan = Shape(
            cls="GhostPacket",
            module="client/common/ghost_packet",
            module_docs=(),
            struct_docs=(),
            derives=(),
            form="unit",
            fields=(),
            new=NewShape(False, False, False, (), ()),
            serializer_fn_docs=(),
            serializer_fn_must_use=False,
            serializer=SerializerShape("constant", "GhostPacket", False, ()),
            extras=None,
        )
        _write_shape(shapes, orphan)
        ws = Path(tempfile.mkdtemp()) / "rstom"
        ws.mkdir()
        (ws / "tools" / "packets_gen" / "schema").mkdir(parents=True)
        (ws / "tools" / "packets_gen" / "schema" / "packets.json").write_text(
            json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8"
        )
        with self.assertRaises(EmitError) as ctx:
            emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes)
        self.assertIn("stale shape", str(ctx.exception))

    def test_changed_hand_file_raises(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shapes = Path(tempfile.mkdtemp())
        _write_shape(shapes, _keep_alive_shape())
        ws = Path(tempfile.mkdtemp()) / "rstom"
        ws.mkdir()
        (ws / "tools" / "packets_gen" / "schema").mkdir(parents=True)
        (ws / "tools" / "packets_gen" / "schema" / "packets.json").write_text(
            json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8"
        )
        path = ws / "crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs"
        path.parent.mkdir(parents=True)
        path.write_text("// hand but not matching shape\n", encoding="utf-8")
        with self.assertRaises(EmitError) as ctx:
            emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes)
        self.assertIn("hand file changed since its shape was extracted", str(ctx.exception))

    def test_unit_shape_with_fields_raises(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shape = replace(
            _keep_alive_shape(),
            form="unit",
            fields=(FieldShape("id", "i64", ()),),
        )
        with self.assertRaises(EmitError) as ctx:
            tree_packet(unit, shape, _packet_body(unit, resolved))
        self.assertIn("unit shape must have no fields", str(ctx.exception))

    def test_constant_serializer_on_non_unit_raises(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shape = replace(
            _keep_alive_shape(),
            serializer=SerializerShape("constant", "Self", False, ()),
        )
        with self.assertRaises(EmitError) as ctx:
            tree_packet(unit, shape, _packet_body(unit, resolved))
        self.assertIn("constant serializer requires unit form", str(ctx.exception))

    def test_mismatch_raises_with_module_and_field(self) -> None:
        resolved, _ = _analyze_keep_alive()
        shapes = Path(tempfile.mkdtemp())
        bad = replace(
            _keep_alive_shape(),
            fields=(FieldShape("id", "i32", ("Java: `id()`.",)),),
        )
        _write_shape(shapes, bad)
        ws = Path(tempfile.mkdtemp()) / "rstom"
        ws.mkdir()
        (ws / "tools" / "packets_gen" / "schema").mkdir(parents=True)
        (ws / "tools" / "packets_gen" / "schema" / "packets.json").write_text(
            json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8"
        )
        with self.assertRaises(EmitError) as ctx:
            emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes)
        msg = str(ctx.exception)
        self.assertIn("client/common/client_keep_alive_packet", msg)
        self.assertIn("id", msg)

    def test_missing_ext_raises_create_first(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shape = replace(_keep_alive_shape(), extras="client_keep_alive_packet.ext.rs")
        shapes = Path(tempfile.mkdtemp())
        _write_shape(shapes, shape)
        ws = Path(tempfile.mkdtemp()) / "rstom"
        ws.mkdir()
        (ws / "tools" / "packets_gen" / "schema").mkdir(parents=True)
        (ws / "tools" / "packets_gen" / "schema" / "packets.json").write_text(
            json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8"
        )
        with self.assertRaises(EmitError) as ctx:
            emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes)
        self.assertIn("create crates/rstom/src/network/packet/client/common/client_keep_alive_packet.ext.rs first", str(ctx.exception))

    def test_over_400_lines_raises(self) -> None:
        resolved, unit = _analyze_keep_alive()
        pad = tuple(f"//! pad {i}" for i in range(380))
        shape = replace(_keep_alive_shape(), module_docs=pad)
        with self.assertRaises(EmitError) as ctx:
            tree_packet(unit, shape, _packet_body(unit, resolved))
        self.assertIn("exceeds 400 lines", str(ctx.exception))

    def test_exactly_400_lines_accepted(self) -> None:
        resolved, unit = _analyze_keep_alive()
        base_lines = (
            tree_packet(unit, _keep_alive_shape(), _packet_body(unit, resolved)).count("\n") + 1
        )
        need = 400 - base_lines
        self.assertGreater(need, 0)
        found = False
        for extra in range(max(0, need - 3), need + 4):
            shape = replace(
                _keep_alive_shape(),
                module_docs=tuple(f"pad {i}" for i in range(extra)),
            )
            try:
                text = tree_packet(unit, shape, _packet_body(unit, resolved))
            except EmitError:
                continue
            lines = text.count("\n") + (0 if text.endswith("\n") else 1)
            if lines == 400:
                found = True
                break
        self.assertTrue(found, "expected some padding to hit exactly 400 lines")

    def test_r11_matching_hand_file_is_swapped(self) -> None:
        if not (RSTOM / "crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs").is_file():
            self.skipTest("rstom hand keep-alive not present")
        resolved, unit = _analyze_keep_alive()
        shapes = Path(tempfile.mkdtemp())
        _write_shape(shapes, _keep_alive_shape())
        ws = Path(tempfile.mkdtemp()) / "rstom"
        ws.mkdir()
        (ws / "tools" / "packets_gen" / "schema").mkdir(parents=True)
        (ws / "tools" / "packets_gen" / "schema" / "packets.json").write_text(
            json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8"
        )
        hand_src = RSTOM / "crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs"
        path = ws / "crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs"
        path.parent.mkdir(parents=True)
        shutil.copyfile(hand_src, path)
        emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes)
        out = path.read_text(encoding="utf-8")
        self.assertIn("Generated by `bps emit rust`", out.splitlines()[0])

    def test_check_never_writes(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shapes = Path(tempfile.mkdtemp())
        _write_shape(shapes, _keep_alive_shape())
        ws = Path(tempfile.mkdtemp()) / "rstom"
        ws.mkdir()
        (ws / "tools" / "packets_gen" / "schema").mkdir(parents=True)
        (ws / "tools" / "packets_gen" / "schema" / "packets.json").write_text(
            json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8"
        )
        emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes)
        pkt = ws / "crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs"
        before = pkt.read_bytes()
        emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes, check=True)
        self.assertEqual(pkt.read_bytes(), before)

    def test_check_flags_drift_orphan_and_hand_written(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shapes = Path(tempfile.mkdtemp())
        _write_shape(shapes, _keep_alive_shape())
        ws = Path(tempfile.mkdtemp()) / "rstom"
        ws.mkdir()
        (ws / "tools" / "packets_gen" / "schema").mkdir(parents=True)
        (ws / "tools" / "packets_gen" / "schema" / "packets.json").write_text(
            json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8"
        )
        emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes)
        pkt = ws / "crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs"
        pkt.write_text(pkt.read_text().replace("i64", "i64 /* drift */", 1), encoding="utf-8")
        drift = emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes, check=True)
        self.assertTrue(any("drift" in m for m in drift))
        emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes)
        pkt.write_text("// hand\n", encoding="utf-8")
        hand = emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes, check=True)
        self.assertTrue(any("hand-written" in m for m in hand))
        rstom_hand = (
            RSTOM / "crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs"
        )
        if rstom_hand.is_file():
            shutil.copyfile(rstom_hand, pkt)
            emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes)
        orphan = ws / "crates/rstom/src/network/packet/server/play/orphan_packet.rs"
        orphan.parent.mkdir(parents=True)
        orphan.write_text("//! Generated by `bps emit rust` orphan\n", encoding="utf-8")
        msgs = emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes, check=True)
        self.assertTrue(any("orphan" in m for m in msgs))


class EmitTreeOrphanExtTest(unittest.TestCase):
    def test_emit_refuses_when_ext_would_be_orphaned(self) -> None:
        resolved, _ = _analyze_keep_alive()
        shape = replace(_keep_alive_shape(), extras=None)
        shapes = Path(tempfile.mkdtemp())
        _write_shape(shapes, shape)
        ws = Path(tempfile.mkdtemp()) / "rstom"
        ws.mkdir()
        (ws / "tools/packets_gen/schema").mkdir(parents=True)
        (ws / "tools/packets_gen/schema/packets.json").write_text(
            json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8"
        )
        pkt_dir = ws / "crates/rstom/src/network/packet/client/common"
        pkt_dir.mkdir(parents=True)
        pkt = pkt_dir / "client_keep_alive_packet.rs"
        pkt.write_text(
            "//! Generated by `bps emit rust` from schemas/minecraft_26_3*.bps: keep_alive\n"
            "#[path = \"client_keep_alive_packet.ext.rs\"]\nmod ext;\n",
            encoding="utf-8",
        )
        ext = pkt_dir / "client_keep_alive_packet.ext.rs"
        ext.write_text("//! orphan\n", encoding="utf-8")
        with self.assertRaises(EmitError) as ctx:
            emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes)
        self.assertIn("would be orphaned", str(ctx.exception))
        self.assertIn("client_keep_alive_packet.ext.rs", str(ctx.exception))


class EmitTreeCheckReadOnlyTest(unittest.TestCase):
    def test_check_on_empty_workspace_creates_no_files(self) -> None:
        resolved, _ = _analyze_keep_alive()
        shapes = Path(tempfile.mkdtemp())
        _write_shape(shapes, _keep_alive_shape())
        ws = Path(tempfile.mkdtemp()) / "rstom"
        ws.mkdir()
        (ws / "tools/packets_gen/schema").mkdir(parents=True)
        (ws / "tools/packets_gen/schema/packets.json").write_text(
            json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8"
        )
        before = _workspace_snapshot(ws)
        emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes, check=True)
        after = _workspace_snapshot(ws)
        self.assertEqual(before, after, msg="check=True modified workspace entries")

    def test_rustfmt_failure_does_not_leak_temp_files(self) -> None:
        resolved, _ = _analyze_keep_alive()
        shapes = Path(tempfile.mkdtemp())
        _write_shape(shapes, _keep_alive_shape())
        ws = Path(tempfile.mkdtemp()) / "rstom"
        ws.mkdir()
        (ws / "tools/packets_gen/schema").mkdir(parents=True)
        (ws / "tools/packets_gen/schema/packets.json").write_text(
            json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8"
        )
        tmp_before = set(Path(tempfile.gettempdir()).glob("tmp*.rs"))
        with patch("bps.emit_tree.subprocess.run", side_effect=OSError("mock")):
            with self.assertRaises(OSError):
                emit_tree(resolved, ws, KEEP_ALIVE_TABLE, shapes)
        tmp_after = set(Path(tempfile.gettempdir()).glob("tmp*.rs"))
        self.assertEqual(tmp_before, tmp_after, msg="temp file leaked after OSError")


class TreePacketFramesWalkerTest(unittest.TestCase):
    def test_rejects_scan_body_using_frames_module(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shape = _keep_alive_shape()
        body = _PacketBody(
            fields=[],
            field_types={},
            writes="",
            reads="        let raw = crate::frames::nbt(&buf) as i32;",
            ctor="",
            init="",
            used_wires=[],
            needs_fixed_raw_bytes=False,
            rtypes=set(),
        )
        with self.assertRaises(EmitError) as ctx:
            tree_packet(unit, shape, body)
        self.assertIn("frames walker", str(ctx.exception))
        self.assertIn("nbt", str(ctx.exception))


@unittest.skipUnless(shutil.which("rustfmt"), "rustfmt not installed")
class EmitTreeExtrasRustfmtTest(unittest.TestCase):
    def test_extras_emit_check_idempotent(self) -> None:
        shape = Shape(
            cls="ProbePacket",
            module="server/play/probe_packet",
            module_docs=(),
            struct_docs=(),
            derives=("Clone",),
            form="braced",
            fields=(FieldShape("x", "i32", ()),),
            new=NewShape(True, False, False, (), ()),
            serializer_fn_docs=(),
            serializer_fn_must_use=False,
            serializer=SerializerShape("struct", "ProbePacketSerializer", False, ()),
            extras="probe_packet.ext.rs",
        )
        source = probe_source(ROOT, "packet probe { x: i32be }")
        with tempfile.TemporaryDirectory() as td:
            schema = Path(td) / "p.bps"
            schema.write_text(source, encoding="utf-8")
            resolved = analyze(load_schema([str(schema)]))
        shapes = Path(tempfile.mkdtemp())
        _write_shape(shapes, shape)
        ws = Path(tempfile.mkdtemp()) / "rstom"
        ws.mkdir()
        (ws / "tools" / "packets_gen" / "schema").mkdir(parents=True)
        (ws / "tools" / "packets_gen" / "schema" / "packets.json").write_text(
            json.dumps(PROBE_SCHEMA),
            encoding="utf-8",
        )
        ext = ws / "crates/rstom/src/network/packet/server/play/probe_packet.ext.rs"
        ext.parent.mkdir(parents=True)
        ext.write_text("//! scratch\n", encoding="utf-8")
        emit_tree(resolved, ws, PROBE_TABLE, shapes)
        first = emit_tree(resolved, ws, PROBE_TABLE, shapes, check=True)
        self.assertEqual(first, [])
        before = {
            p: p.read_bytes()
            for p in ws.rglob("*")
            if p.is_file() and "crates/rstom/src/network/packet" in p.as_posix()
        }
        emit_tree(resolved, ws, PROBE_TABLE, shapes)
        for path, data in before.items():
            self.assertEqual(path.read_bytes(), data, msg=path)


class GoldenWithoutWorktreeTest(unittest.TestCase):
    def test_golden_keep_alive_without_rstom_worktree(self) -> None:
        import os
        import subprocess

        env = {**os.environ, "BPS_RSTOM_DIR": str(ROOT / ".missing-rstom-worktree")}
        proc = subprocess.run(
            [
                "python",
                "-m",
                "pytest",
                "tests/test_emit_tree.py::TreePacketGoldenTest::test_golden_keep_alive",
                "-q",
                "-p",
                "no:cacheprovider",
            ],
            cwd=ROOT,
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stdout + proc.stderr)


@unittest.skipUnless(RSTOM_PACKET.is_dir(), "rstom worktree packet sources not present")
class UnitWithNewExtractedTest(unittest.TestCase):
    def test_enter_combat_and_low_disk_space_unit_new(self) -> None:
        from bps.tree_packet import tree_packet

        cases = (
            (
                "EnterCombatEventPacket",
                "server/play/enter_combat_event_packet",
                "enter_combat_event",
                ClassTable(
                    {
                        "packets": [class_entry("server", "play", "EnterCombatEventPacket")],
                        "stateTables": [
                            {
                                "state": "play",
                                "side": "SERVER",
                                "packets": ["EnterCombatEventPacket"],
                            },
                        ],
                    }
                ),
                f'import "{ROOT / "schemas" / "std" / "varint.bps"}"\n'
                "packet enter_combat_event {}\n"
                "protocol minecraft @ v1 {\n"
                "    id = 1\n"
                "    frame default { length: varint id: varint payload: rest }\n"
                "    route play.clientbound 0x00 => enter_combat_event\n"
                "}\n",
            ),
            (
                "LowDiskSpaceWarningPacket",
                "server/play/low_disk_space_warning_packet",
                "low_disk_space_warning",
                ClassTable(
                    {
                        "packets": [class_entry("server", "play", "LowDiskSpaceWarningPacket")],
                        "stateTables": [
                            {
                                "state": "play",
                                "side": "SERVER",
                                "packets": ["LowDiskSpaceWarningPacket"],
                            },
                        ],
                    }
                ),
                f'import "{ROOT / "schemas" / "std" / "varint.bps"}"\n'
                "packet low_disk_space_warning {}\n"
                "protocol minecraft @ v1 {\n"
                "    id = 1\n"
                "    frame default { length: varint id: varint payload: rest }\n"
                "    route play.clientbound 0x00 => low_disk_space_warning\n"
                "}\n",
            ),
        )
        for cls, module, pkt_name, table, schema_src in cases:
            path = RSTOM_PACKET / f"{module}.rs"
            shape = extract_shape(path.read_text(encoding="utf-8"), cls, module)
            with self.subTest(cls=cls):
                with tempfile.TemporaryDirectory() as td:
                    schema = Path(td) / "x.bps"
                    schema.write_text(schema_src, encoding="utf-8")
                    resolved = analyze(load_schema([str(schema)]))
                unit = next(u for u in build_units(resolved, table) if u.pkt.name == pkt_name)
                text = tree_packet(unit, shape, _packet_body(unit, resolved))
                self.assertIn("pub fn new() -> Self", text)
                self.assertIn("ConstantSerializer::new(Self)", text)
                self.assertIn(f"pub struct {cls};", text)


if __name__ == "__main__":
    unittest.main()

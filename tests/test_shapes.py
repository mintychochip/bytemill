"""Shape extraction from hand-written rstom packet files."""

from __future__ import annotations

import json
import os
import tempfile
import unittest
from collections import Counter
from pathlib import Path

from bps.analyze import analyze
from bps.emit_rstom import _packet_fields
from bps.errors import EmitError
from bps.load import load_schema
from bps.minestom import build_units
from bps.shapes import (
    ShapeError,
    extract_shape,
    hand_packet_path,
    mismatch,
    packet_report,
    shape_from_json,
    shape_to_json,
)
from tests.rstom_tables import RSTOM, real_table
from tests.test_varint import load_minecraft

ROOT = Path(__file__).resolve().parents[1]


def _bps_field_pairs(pkt, resolved) -> list[tuple[str, str]]:
    fields, _ = _packet_fields(pkt, resolved)
    out: list[tuple[str, str]] = []
    for f in fields:
        base = f"Vec<{f.rtype}>" if f.repeat_mode is not None else f.rtype
        mapped = f"Option<{base}>" if f.guard is not None else base
        out.append((f.ident, mapped))
    return out


KEEP_ALIVE = """\
//! Port of `net.minestom.server.network.packet.server.common.KeepAlivePacket`.

use std::sync::Arc;

use crate::network::buffer::NetworkBuffer;
use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::packet::server::server_packet::{ServerPacket, ServerPacketGroup};
use crate::network::types::LONG;

/// Java: `record KeepAlivePacket(long id) implements ServerPacket.Configuration, Play`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KeepAlivePacket {
    /// Java: `id()`.
    pub id: i64,
}

impl KeepAlivePacket {
    /// Java: `KeepAlivePacket(long id)`.
    #[must_use]
    pub fn new(id: i64) -> Self {
        Self { id }
    }

    /// Java: `KeepAlivePacket.SERIALIZER`.
    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(KeepAlivePacketSerializer)
    }
}

impl Packet for KeepAlivePacket {
    const CLASS_NAME: &'static str =
        "net.minestom.server.network.packet.server.common.KeepAlivePacket";
}

impl ServerPacket for KeepAlivePacket {}
impl ServerPacketGroup for KeepAlivePacket {}

/// Java: the `template(LONG, KeepAlivePacket::id, …)` template.
struct KeepAlivePacketSerializer;

impl PacketSerializer for KeepAlivePacketSerializer {
    fn write(&self, buffer: &mut NetworkBuffer, value: &PacketValue) {
        buffer.write(LONG, downcast::<KeepAlivePacket>(value).id);
    }

    fn read(&self, buffer: &mut NetworkBuffer) -> PacketValue {
        Box::new(KeepAlivePacket {
            id: buffer.read(LONG),
        })
    }
}
"""

CLEAR_DIALOG = """\
//! Port of `net.minestom.server.network.packet.server.common.ClearDialogPacket`.

use std::sync::Arc;

use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{ConstantSerializer, PacketSerializer};
use crate::network::packet::server::server_packet::{ServerPacket, ServerPacketGroup};

/// Java: `record ClearDialogPacket() implements ServerPacket.Configuration, Play`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClearDialogPacket;

impl Packet for ClearDialogPacket {
    const CLASS_NAME: &'static str =
        "net.minestom.server.network.packet.server.common.ClearDialogPacket";
}

impl ServerPacket for ClearDialogPacket {}
impl ServerPacketGroup for ClearDialogPacket {}

impl ClearDialogPacket {
    /// Java: `ClearDialogPacket.SERIALIZER`.
    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(ConstantSerializer::new(ClearDialogPacket))
    }
}
"""

FORWARD_EXTRA = """\
//! Port of `net.minestom.server.network.packet.server.common.ProbePacket`.

use std::sync::Arc;

use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::packet::server::server_packet::{ServerPacket, ServerPacketGroup};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ProbePacket {
    pub id: i64,
}

impl ProbePacket {
    #[must_use]
    pub fn new(id: i64) -> Self {
        Self { id }
    }

    #[must_use]
    pub fn forward(&self) -> bool {
        true
    }

    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(ProbePacketSerializer)
    }
}

impl Packet for ProbePacket {}
impl ServerPacket for ProbePacket {}
impl ServerPacketGroup for ProbePacket {}

struct ProbePacketSerializer;

impl PacketSerializer for ProbePacketSerializer {
    fn write(&self, _buffer: &mut (), _value: &PacketValue) {}
    fn read(&self, _buffer: &mut ()) -> PacketValue {
        Box::new(ProbePacket { id: 0 })
    }
}
"""


class ShapeSnippetTest(unittest.TestCase):
    def test_keep_alive_style_packet(self) -> None:
        shape = extract_shape(KEEP_ALIVE, "KeepAlivePacket", "server/common/keep_alive_packet")
        self.assertEqual(shape.derives, ("Copy", "Clone", "Debug", "PartialEq", "Eq"))
        self.assertEqual(shape.form, "braced")
        self.assertEqual(len(shape.fields), 1)
        self.assertEqual(shape.fields[0].name, "id")
        self.assertEqual(shape.fields[0].type, "i64")
        self.assertEqual(shape.fields[0].docs, ("Java: `id()`.",))
        self.assertTrue(shape.new.present)
        self.assertFalse(shape.new.const)
        self.assertTrue(shape.new.must_use)
        self.assertEqual(shape.serializer.style, "struct")
        self.assertEqual(shape.serializer.name, "KeepAlivePacketSerializer")
        self.assertFalse(shape.serializer.public)
        self.assertIsNone(shape.extras)

    def test_unit_struct_constant_serializer(self) -> None:
        shape = extract_shape(CLEAR_DIALOG, "ClearDialogPacket", "server/common/clear_dialog_packet")
        self.assertEqual(shape.form, "unit")
        self.assertFalse(shape.new.present)
        self.assertEqual(shape.serializer.style, "constant")
        self.assertEqual(shape.fields, ())

    def test_inherent_helper_sets_extras(self) -> None:
        shape = extract_shape(FORWARD_EXTRA, "ProbePacket", "server/common/probe_packet")
        self.assertEqual(shape.extras, "probe_packet.ext.rs")

    def test_default_impl_raises(self) -> None:
        src = CLEAR_DIALOG.replace(
            "impl ServerPacket for ClearDialogPacket {}",
            "impl Default for ClearDialogPacket {}\nimpl ServerPacket for ClearDialogPacket {}",
        )
        with self.assertRaises(ShapeError):
            extract_shape(src, "ClearDialogPacket", "server/common/clear_dialog_packet")

    def test_free_function_raises(self) -> None:
        src = "//! x\n\npub fn free_helper() {}\n" + CLEAR_DIALOG
        with self.assertRaises(ShapeError):
            extract_shape(src, "ClearDialogPacket", "server/common/clear_dialog_packet")

    def test_struct_allow_raises(self) -> None:
        src = CLEAR_DIALOG.replace(
            "#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]",
            "#[allow(dead_code)]\n#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]",
        )
        with self.assertRaises(ShapeError):
            extract_shape(src, "ClearDialogPacket", "server/common/clear_dialog_packet")

    def test_json_round_trip(self) -> None:
        shape = extract_shape(KEEP_ALIVE, "KeepAlivePacket", "server/common/keep_alive_packet")
        text = shape_to_json(shape)
        back = shape_from_json(text)
        self.assertEqual(back, shape)
        parsed = json.loads(text)
        self.assertIn("struct", parsed)
        self.assertIn("serializer_fn", parsed)
        self.assertTrue(text.endswith("\n"))

    def test_mismatch_equal_and_differing(self) -> None:
        shape = extract_shape(KEEP_ALIVE, "KeepAlivePacket", "server/common/keep_alive_packet")
        self.assertEqual(mismatch(shape, [("id", "i64")]), [])
        bad = mismatch(shape, [("id", "i32")])
        self.assertEqual(len(bad), 1)
        self.assertIn("id", bad[0])
        self.assertIn("i64", bad[0])
        self.assertIn("i32", bad[0])


def _worktree_present() -> bool:
    path = RSTOM / "crates" / "rstom" / "src" / "network" / "packet"
    return path.is_dir()


class ShapeRealTreeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not _worktree_present():
            raise unittest.SkipTest("rstom worktree packet sources not present")
        cls.resolved = load_minecraft()
        cls.table = real_table()
        cls.units = build_units(cls.resolved, cls.table)

    def test_eligible_packets_extract_and_match_bps(self) -> None:
        eligible: list[str] = []
        ineligible: dict[str, str] = {}
        for unit in self.units:
            path = hand_packet_path(RSTOM, unit)
            label = f"{unit.cls.name} ({unit.cls.group}/{unit.cls.package}/{unit.cls.stem})"
            if not path.is_file():
                ineligible[unit.cls.name] = f"missing hand file {path}"
                continue
            try:
                source = path.read_text(encoding="utf-8")
                module = f"{unit.cls.group}/{unit.cls.package}/{unit.cls.stem}"
                shape = extract_shape(source, unit.cls.name, module)
            except ShapeError as err:
                ineligible[unit.cls.name] = str(err)
                continue
            bps = _bps_field_pairs(unit.pkt, self.resolved)
            reasons = mismatch(shape, bps)
            if reasons:
                ineligible[unit.cls.name] = "; ".join(reasons)
                continue
            eligible.append(unit.cls.name)
            self.assertEqual(shape_from_json(shape_to_json(shape)), shape)

        self.assertEqual(
            len(eligible),
            64,
            f"eligible count {len(eligible)} != 64; eligible={sorted(eligible)}",
        )
        if ineligible:
            summary = Counter(ineligible.values())
            # Printed for the report; failure above is only on count.
            self._ineligible_summary = dict(summary)


class ShapeCliTest(unittest.TestCase):
    def test_shapes_report_runs(self) -> None:
        import subprocess

        if not _worktree_present():
            self.skipTest("rstom worktree absent")
        proc = subprocess.run(
            [
                "python",
                "-m",
                "bps.cli",
                "shapes",
                "report",
                str(ROOT / "schemas" / "minecraft_26_3_wiki.bps"),
                "--rstom",
                str(RSTOM),
            ],
            capture_output=True,
            text=True,
            cwd=ROOT,
            env={**os.environ, "BPS_RSTOM_DIR": str(RSTOM)},
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("eligible", proc.stdout)


if __name__ == "__main__":
    unittest.main()

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

WORKLIST_SUMMARY = {
    "eligible": 64,
    "field count mismatch": 47,
    "field name/type mismatch": 60,
    "extract: extra public type in hand file": 32,
    "extract: nested public type": 25,
    "extract: missing derive on struct": 5,
    "missing hand file": 1,
    "extract: unsupported trait impl": 1,
    "extract: free function": 1,
}

def _worklist_category(status: str) -> str:
    if status.startswith("field count"):
        return "field count mismatch"
    if status.startswith("field "):
        return "field name/type mismatch"
    if status == "unexpected public type":
        return "extract: extra public type in hand file"
    if status.startswith("unexpected public type"):
        return "extract: nested public type"
    if status.startswith("missing hand file"):
        return "missing hand file"
    if status.startswith("impl "):
        return "extract: unsupported trait impl"
    if status == "free function":
        return "extract: free function"
    if status.startswith("missing"):
        return "extract: missing derive on struct"
    return status


EXTRAS_PACKETS = frozenset(
    {
        "ClientInputPacket",
        "ClientPlayerPositionStatusPacket",
        "ClientPlayerRotationPacket",
        "PluginMessagePacket",
        "ClientTabCompletePacket",
    }
)


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

RECIPE_NEW_ATTRS = """\
    #[must_use]
    #[allow(clippy::fn_params_excessive_bools)]
    #[allow(
        clippy::too_many_arguments,
        reason = "1:1 port of Java's RecipeBookSettingsPacket(boolean x 8)"
    )]
    pub fn new(a: bool, b: bool) -> Self {
        Self { a, b }
    }
"""

RECIPE_BOOK_SNIPPET = """\
//! Port of `net.minestom.server.network.packet.server.play.RecipeBookSettingsPacket`.

use std::sync::Arc;

use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::packet::server::server_packet::{ServerPacket, ServerPacketGroup};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RecipeBookSettingsPacket {
    pub a: bool,
    pub b: bool,
}

impl RecipeBookSettingsPacket {
%s
    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(RecipeBookSettingsSerializer)
    }
}

impl Packet for RecipeBookSettingsPacket {}
impl ServerPacket for RecipeBookSettingsPacket {}
impl ServerPacketGroup for RecipeBookSettingsPacket {}

struct RecipeBookSettingsSerializer;

impl PacketSerializer for RecipeBookSettingsSerializer {
    fn write(&self, _b: &mut (), _v: &PacketValue) {}
    fn read(&self, _b: &mut ()) -> PacketValue {
        Box::new(RecipeBookSettingsPacket { a: false, b: false })
    }
}
""" % RECIPE_NEW_ATTRS

TAB_COMPLETE_CONST_EXTRA = """\
//! Port of `net.minestom.server.network.packet.client.play.ClientTabCompletePacket`.

use std::sync::Arc;

use crate::network::packet::client::client_packet::{ClientPacket, ClientPacketGroup};
use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};

pub const MAX_TEXT_LENGTH: i32 = 32500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientTabCompletePacket {
    pub transaction_id: i32,
    pub text: String,
}

impl ClientTabCompletePacket {
    #[must_use]
    pub fn new(transaction_id: i32, text: String) -> Self {
        Self { transaction_id, text }
    }

    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(ClientTabCompletePacketSerializer)
    }
}

impl Packet for ClientTabCompletePacket {}
impl ClientPacket for ClientTabCompletePacket {}
impl ClientPacketGroup for ClientTabCompletePacket {}

struct ClientTabCompletePacketSerializer;

impl PacketSerializer for ClientTabCompletePacketSerializer {
    fn write(&self, _b: &mut (), _v: &PacketValue) {}
    fn read(&self, _b: &mut ()) -> PacketValue {
        Box::new(ClientTabCompletePacket { transaction_id: 0, text: String::new() })
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

    def test_multiline_new_attributes(self) -> None:
        shape = extract_shape(
            RECIPE_BOOK_SNIPPET,
            "RecipeBookSettingsPacket",
            "server/play/recipe_book_settings_packet",
        )
        self.assertTrue(shape.new.must_use)
        self.assertEqual(
            shape.new.attrs,
            (
                "#[allow(clippy::fn_params_excessive_bools)]",
                "#[allow(\n        clippy::too_many_arguments,\n        reason = \"1:1 port of Java's RecipeBookSettingsPacket(boolean x 8)\"\n    )]",
            ),
        )

    def test_doc_line_preserves_markdown_indent(self) -> None:
        src = (
            "//! Port of `x`.\n\n"
            "/// Line one\n"
            "///  indented second line\n"
            "#[derive(Clone)]\n"
            "pub struct X;\n"
        )
        shape = extract_shape(src, "X", "server/common/x")
        self.assertEqual(shape.struct_docs[1], " indented second line")

    def test_module_pub_const_sets_extras(self) -> None:
        shape = extract_shape(
            TAB_COMPLETE_CONST_EXTRA,
            "ClientTabCompletePacket",
            "client/play/client_tab_complete_packet",
        )
        self.assertEqual(shape.extras, "client_tab_complete_packet.ext.rs")

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

        self.assertEqual(len(eligible), 64, f"eligible={sorted(eligible)}")

        summary = Counter()
        for unit in self.units:
            status = packet_report(unit, self.resolved, RSTOM)
            if status == "eligible":
                summary["eligible"] += 1
            else:
                summary[_worklist_category(status)] += 1
        self.assertEqual(dict(summary), WORKLIST_SUMMARY)

        shapes_by_class: dict[str, object] = {}
        for unit in self.units:
            if unit.cls.name not in eligible:
                continue
            path = hand_packet_path(RSTOM, unit)
            mod = f"{unit.cls.group}/{unit.cls.package}/{unit.cls.stem}"
            shapes_by_class[unit.cls.name] = extract_shape(
                path.read_text(encoding="utf-8"), unit.cls.name, mod
            )

        recipe = shapes_by_class["RecipeBookSettingsPacket"]
        self.assertTrue(recipe.new.must_use)
        self.assertEqual(len(recipe.new.attrs), 2)
        self.assertIn("fn_params_excessive_bools", recipe.new.attrs[0])
        self.assertIn("too_many_arguments", recipe.new.attrs[1])

        clear = shapes_by_class["ClearDialogPacket"]
        self.assertEqual(clear.serializer.style, "constant")
        self.assertEqual(clear.serializer.name, "ClearDialogPacket")

        pong = shapes_by_class["ClientPongPacket"]
        self.assertEqual(pong.derives, ("Copy", "Clone", "Debug", "PartialEq", "Eq"))
        self.assertEqual(pong.fields[0].name, "id")
        self.assertEqual(pong.fields[0].type, "i32")

        keep_path = hand_packet_path(RSTOM, next(u for u in self.units if u.cls.name == "ClientKeepAlivePacket"))
        keep = extract_shape(
            keep_path.read_text(encoding="utf-8"),
            "ClientKeepAlivePacket",
            "client/common/client_keep_alive_packet",
        )
        self.assertEqual(keep.derives, ("Copy", "Clone", "Debug", "PartialEq", "Eq"))
        self.assertEqual(keep.fields[0].docs, ("Java: `id()`.",))
        self.assertIn("KeepAlivePacket", keep.struct_docs[0])

        for name in EXTRAS_PACKETS:
            self.assertEqual(
                shapes_by_class[name].extras,
                f"{shapes_by_class[name].module.rsplit('/', 1)[-1]}.ext.rs",
            )
        for name, shape in shapes_by_class.items():
            if name not in EXTRAS_PACKETS:
                self.assertIsNone(shape.extras, msg=name)


class ShapeCliTest(unittest.TestCase):
    def test_shapes_extract_eligible_skips_unrepresentable(self) -> None:
        import shutil
        import subprocess

        with tempfile.TemporaryDirectory() as tmp:
            ws = Path(tmp) / "ws"
            pkt_root = (
                ws
                / "crates"
                / "rstom"
                / "src"
                / "network"
                / "packet"
            )
            good = pkt_root / "server" / "common" / "clear_dialog_packet.rs"
            bad = pkt_root / "server" / "play" / "recipe_book_settings_packet.rs"
            good.parent.mkdir(parents=True)
            bad.parent.mkdir(parents=True)
            good.write_text(CLEAR_DIALOG, encoding="utf-8")
            bad.write_text(
                "//! x\npub fn bad() {}\n" + RECIPE_BOOK_SNIPPET,
                encoding="utf-8",
            )
            schema = Path(tmp) / "packets.json"
            shutil.copy(
                RSTOM / "tools" / "packets_gen" / "schema" / "packets.json",
                schema,
            )
            out = Path(tmp) / "shapes"
            proc = subprocess.run(
                [
                    "python",
                    "-m",
                    "bps.cli",
                    "shapes",
                    "extract",
                    str(ROOT / "schemas" / "minecraft_26_3_wiki.bps"),
                    "--rstom",
                    str(ws),
                    "--packets-json",
                    str(schema),
                    "--eligible",
                    "--out",
                    str(out),
                ],
                capture_output=True,
                text=True,
                cwd=ROOT,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            written = list(out.rglob("*.json"))
            self.assertGreaterEqual(len(written), 1)
            self.assertTrue(any(p.name == "clear_dialog_packet.json" for p in written))

    def test_shapes_extract_eligible_writes_64_with_worktree(self) -> None:
        import subprocess

        if not _worktree_present():
            self.skipTest("rstom worktree absent")
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "shapes"
            proc = subprocess.run(
                [
                    "python",
                    "-m",
                    "bps.cli",
                    "shapes",
                    "extract",
                    str(ROOT / "schemas" / "minecraft_26_3_wiki.bps"),
                    "--rstom",
                    str(RSTOM),
                    "--eligible",
                    "--out",
                    str(out),
                ],
                capture_output=True,
                text=True,
                cwd=ROOT,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            files = sorted(out.rglob("*.json"))
            self.assertEqual(len(files), 64)

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

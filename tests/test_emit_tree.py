"""`bps emit rstom-tree` writes the wire-pin gate into an rstom workspace."""

import contextlib
import io
import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from bps.analyze import analyze
from bps.cli import main
from bps.emit_rstom import _packet_body
from bps.emit_tree import _tree_packet, emit_tree
from bps.errors import EmitError
from bps.load import load_schema
from bps.minestom import ClassTable, build_units
from dataclasses import replace

from bps.shapes import (
    FieldShape,
    NewShape,
    SerializerShape,
    Shape,
    extract_shape,
    hand_packet_path,
    shape_to_json,
)

from tests.rstom_tables import PROBE_SCHEMA, PROBE_TABLE, RSTOM, class_entry, probe_source
from tests.test_shapes import CLEAR_DIALOG

ROOT = Path(__file__).resolve().parents[1]
GOLDEN_KEEP_ALIVE = ROOT / "tests" / "fixtures" / "rstom_tree" / "client_keep_alive_packet.rs"


def _rustfmt(text: str) -> str:
    rustfmt = shutil.which("rustfmt")
    if not rustfmt:
        return text
    with tempfile.NamedTemporaryFile(mode="w", suffix=".rs", delete=False, encoding="utf-8") as f:
        f.write(text)
        path = Path(f.name)
    try:
        subprocess.run(
            [rustfmt, "--edition", "2024", str(path)],
            check=False,
            capture_output=True,
        )
        return path.read_text(encoding="utf-8")
    finally:
        path.unlink(missing_ok=True)


def _keep_alive_schema() -> str:
    std = "".join(
        f'import "{ROOT / "schemas" / "std" / f"{n}.bps"}"\n' for n in ("integers", "varint")
    )
    return (
        f"{std}\n"
        "packet keep_alive { id: i64be }\n"
        "protocol minecraft @ v1 {\n"
        "    id = 1\n"
        "    frame default { length: varint id: varint payload: rest }\n"
        "    route play.serverbound 0x00 => keep_alive\n"
        "}\n"
    )


KEEP_ALIVE_TABLE_DATA = {
    "packets": [class_entry("client", "common", "ClientKeepAlivePacket")],
    "stateTables": [
        {"state": "play", "side": "CLIENT", "packets": ["ClientKeepAlivePacket"]},
    ],
}
KEEP_ALIVE_TABLE = ClassTable(KEEP_ALIVE_TABLE_DATA)


def _analyze_keep_alive():
    with tempfile.TemporaryDirectory() as td:
        schema = Path(td) / "keep.bps"
        schema.write_text(_keep_alive_schema(), encoding="utf-8")
        resolved = analyze(load_schema([str(schema)]))
    units = build_units(resolved, KEEP_ALIVE_TABLE)
    return resolved, next(u for u in units if u.pkt.name == "keep_alive")


def _struct_docs_from_golden() -> tuple[str, ...]:
    docs: list[str] = []
    for line in GOLDEN_KEEP_ALIVE.read_text(encoding="utf-8").splitlines():
        if line.startswith("/// "):
            docs.append(line[4:])
        elif line.startswith("#[derive"):
            break
    return tuple(docs)


def _keep_alive_shape() -> Shape:
    hand = RSTOM / "crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs"
    if hand.is_file():
        return extract_shape(
            hand.read_text(encoding="utf-8"),
            "ClientKeepAlivePacket",
            "client/common/client_keep_alive_packet",
        )
    return Shape(
        cls="ClientKeepAlivePacket",
        module="client/common/client_keep_alive_packet",
        module_docs=(
            "Port of `net.minestom.server.network.packet.client.common.ClientKeepAlivePacket`.",
        ),
        struct_docs=_struct_docs_from_golden(),
        derives=("Copy", "Clone", "Debug", "PartialEq", "Eq"),
        form="braced",
        fields=(FieldShape("id", "i64", ("Java: `id()`.",)),),
        new=NewShape(True, False, True, (), ("Java: `new ClientKeepAlivePacket(long)`.",)),
        serializer_fn_docs=("Java: `ClientKeepAlivePacket.SERIALIZER`.",),
        serializer_fn_must_use=True,
        serializer=SerializerShape(
            "struct",
            "ClientKeepAliveSerializer",
            False,
            ("Java: the `template(LONG, ClientKeepAlivePacket::id, …)` template.",),
        ),
        extras=None,
    )


def _write_shape(shapes_dir: Path, shape: Shape) -> None:
    path = shapes_dir / f"{shape.module}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(shape_to_json(shape), encoding="utf-8")


def _run(*argv: str) -> tuple[int, str]:
    err = io.StringIO()
    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
        code = main(list(argv))
    return code, err.getvalue()


class EmitTreeTest(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.workspace = Path(tmp.name) / "rstom"
        self.workspace.mkdir()
        table_path = self.workspace / "tools" / "packets_gen" / "schema" / "packets.json"
        table_path.parent.mkdir(parents=True, exist_ok=True)
        table_path.write_text(json.dumps(PROBE_SCHEMA), encoding="utf-8")
        schema_path = Path(tmp.name) / "probe.bps"
        schema_path.write_text(probe_source(ROOT), encoding="utf-8")
        self.resolved = analyze(load_schema([str(schema_path)]))
        self.shapes = Path(tmp.name) / "shapes"
        self.shapes.mkdir()

    def _paths(self) -> dict[str, Path]:
        base = self.workspace / "crates" / "rstom" / "tests"
        return {
            "gate": base / "bps_wire_pins.rs",
            "pins": base / "bps" / "wire_pins.json",
            "known": base / "bps" / "wire_pins_known.json",
        }

    def test_writes_the_pin_gate_and_nothing_else_without_shapes(self) -> None:
        written = emit_tree(self.resolved, self.workspace, PROBE_TABLE, self.shapes)
        paths = self._paths()
        rel = set(written)
        self.assertEqual(
            rel,
            {
                "crates/rstom/tests/bps_wire_pins.rs",
                "crates/rstom/tests/bps/wire_pins.json",
                "crates/rstom/tests/bps/wire_pins_known.json",
            },
        )
        self.assertTrue(paths["gate"].is_file())
        self.assertTrue(paths["pins"].is_file())
        self.assertEqual(paths["known"].read_text(encoding="utf-8"), "[]\n")
        src = self.workspace / "crates" / "rstom" / "src"
        self.assertFalse(src.exists() or any(src.rglob("*")))

    def test_known_file_is_never_overwritten(self) -> None:
        paths = self._paths()
        paths["known"].parent.mkdir(parents=True, exist_ok=True)
        paths["known"].write_text('[{"side":"CLIENT","state":"PLAY","id":0,"category":"x","reason":"y"}]\n')
        emit_tree(self.resolved, self.workspace, PROBE_TABLE, self.shapes)
        self.assertIn("CLIENT", paths["known"].read_text(encoding="utf-8"))

    def test_check_reports_a_drifted_pin_file(self) -> None:
        emit_tree(self.resolved, self.workspace, PROBE_TABLE, self.shapes)
        paths = self._paths()
        self.assertEqual(emit_tree(self.resolved, self.workspace, PROBE_TABLE, self.shapes, check=True), [])
        paths["pins"].write_text("{}\n", encoding="utf-8")
        drift = emit_tree(self.resolved, self.workspace, PROBE_TABLE, self.shapes, check=True)
        self.assertEqual(len(drift), 1)
        self.assertIn("wire_pins.json", drift[0])

    def test_a_second_emit_is_a_no_op(self) -> None:
        paths = self._paths()
        paths["known"].parent.mkdir(parents=True, exist_ok=True)
        paths["known"].write_text('[{"side":"CLIENT","state":"PLAY","id":0,"category":"x","reason":"y"}]\n')
        emit_tree(self.resolved, self.workspace, PROBE_TABLE, self.shapes)
        first = {k: p.read_bytes() for k, p in paths.items()}
        emit_tree(self.resolved, self.workspace, PROBE_TABLE, self.shapes)
        for key, path in paths.items():
            self.assertEqual(path.read_bytes(), first[key], msg=key)

    def test_a_refused_emit_writes_nothing(self) -> None:
        paths = self._paths()
        paths["gate"].parent.mkdir(parents=True, exist_ok=True)
        paths["gate"].write_text("// hand-maintained gate\n", encoding="utf-8")
        paths["pins"].parent.mkdir(parents=True, exist_ok=True)
        paths["pins"].write_text('{"rows":[]}\n', encoding="utf-8")
        before = {k: p.read_bytes() for k, p in paths.items() if p.is_file()}
        with self.assertRaises(EmitError):
            emit_tree(self.resolved, self.workspace, PROBE_TABLE, self.shapes)
        for key, data in before.items():
            self.assertEqual(paths[key].read_bytes(), data, msg=key)
        self.assertFalse(paths["known"].exists())

    def test_cli_check_on_rust_exits_3(self) -> None:
        schema = self.workspace.parent / "probe.bps"
        code, err = _run(
            "emit",
            "rust",
            str(schema),
            "--out",
            str(self.workspace / "out"),
            "--check",
        )
        self.assertEqual(code, 3)
        self.assertIn("--check", err)

    def test_cli_without_rstom_exits_3(self) -> None:
        schema = Path(tempfile.mkdtemp()) / "probe.bps"
        schema.write_text(probe_source(ROOT), encoding="utf-8")
        code, err = _run("emit", "rstom-tree", str(schema))
        self.assertEqual(code, 3)
        self.assertIn("--rstom", err)


@unittest.skipUnless(shutil.which("rustfmt"), "rustfmt not installed")
class TreePacketGoldenTest(unittest.TestCase):
    def test_golden_keep_alive(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shape = _keep_alive_shape()
        body = _packet_body(unit, resolved)
        got = _rustfmt(_tree_packet(unit, shape, body))
        want = _rustfmt(GOLDEN_KEEP_ALIVE.read_text(encoding="utf-8"))
        self.assertEqual(got, want)


class TreePacketRenderTest(unittest.TestCase):
    def test_unit_packet_uses_constant_serializer_and_has_no_new(self) -> None:
        shape = extract_shape(CLEAR_DIALOG, "ClearDialogPacket", "server/common/clear_dialog_packet")
        source = (
            f'import "{ROOT / "schemas" / "std" / "varint.bps"}"\n'
            "packet clear_dialog {}\n"
            "protocol minecraft @ v1 {\n"
            "    id = 1\n"
            "    frame default { length: varint id: varint payload: rest }\n"
            "    route play.clientbound 0x00 => clear_dialog\n"
            "}\n"
        )
        with tempfile.TemporaryDirectory() as td:
            schema = Path(td) / "x.bps"
            schema.write_text(source, encoding="utf-8")
            resolved = analyze(load_schema([str(schema)]))
        table = ClassTable(
            {
                "packets": [class_entry("server", "common", "ClearDialogPacket")],
                "stateTables": [
                    {"state": "play", "side": "SERVER", "packets": ["ClearDialogPacket"]},
                ],
            }
        )
        unit = next(u for u in build_units(resolved, table) if u.pkt.name == "clear_dialog")
        text = _tree_packet(unit, shape, _packet_body(unit, resolved))
        self.assertNotIn("pub fn new", text)
        self.assertIn("ConstantSerializer::new(ClearDialogPacket)", text)
        self.assertIn("pub struct ClearDialogPacket;", text)

    def test_copy_fields_are_not_cloned(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shape = _keep_alive_shape()
        text = _tree_packet(unit, shape, _packet_body(unit, resolved))
        self.assertIn("buffer.write(LONG, value.id);", text)
        self.assertNotIn("value.id.clone()", text)

        probe_shape = Shape(
            cls="ProbePacket",
            module="server/play/probe_packet",
            module_docs=(),
            struct_docs=(),
            derives=("Clone", "Debug", "PartialEq"),
            form="braced",
            fields=(
                FieldShape("n", "i64", ()),
                FieldShape("label", "String", ()),
            ),
            new=NewShape(True, False, True, (), ()),
            serializer_fn_docs=(),
            serializer_fn_must_use=True,
            serializer=SerializerShape("struct", "ProbePacketSerializer", False, ()),
            extras=None,
        )
        source = (
            f'import "{ROOT / "schemas" / "minecraft_core.bps"}"\n'
            + probe_source(ROOT, "packet probe { n: i64be label: string }")
        )
        with tempfile.TemporaryDirectory() as td:
            schema = Path(td) / "p.bps"
            schema.write_text(source, encoding="utf-8")
            resolved = analyze(load_schema([str(schema)]))
        unit = next(u for u in build_units(resolved, PROBE_TABLE) if u.pkt.name == "probe")
        text = _tree_packet(unit, probe_shape, _packet_body(unit, resolved))
        self.assertIn("value.label.clone()", text)

    def test_no_rstom_paths_in_a_tree_file(self) -> None:
        resolved, unit = _analyze_keep_alive()
        text = _tree_packet(unit, _keep_alive_shape(), _packet_body(unit, resolved))
        self.assertNotIn("rstom::", text)
        self.assertIn("crate::network::", text)

    def test_new_body_from_the_shape_is_emitted(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shape = replace(
            _keep_alive_shape(),
            new=NewShape(
                True,
                False,
                True,
                _keep_alive_shape().new.docs,
                (),
                ('        panic!("validated");', "        Self { id }"),
            ),
        )
        text = _tree_packet(unit, shape, _packet_body(unit, resolved))
        self.assertIn('panic!("validated");', text)

    def test_read_via_new_emits_constructor_call(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shape = replace(
            _keep_alive_shape(),
            fields=(FieldShape("id", "i64", ()),),
            serializer=replace(_keep_alive_shape().serializer, read_via_new=True),
        )
        text = _tree_packet(unit, shape, _packet_body(unit, resolved))
        self.assertIn("Box::new(ClientKeepAlivePacket::new(id))", text)
        self.assertNotIn("Box::new(ClientKeepAlivePacket {", text)

    def test_read_without_via_new_uses_struct_literal(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shape = replace(_keep_alive_shape(), serializer=replace(_keep_alive_shape().serializer, read_via_new=False))
        text = _tree_packet(unit, shape, _packet_body(unit, resolved))
        self.assertIn("Box::new(ClientKeepAlivePacket {", text)
        self.assertNotIn("::new(id)", text)

    def test_extras_adds_the_path_mod_and_reexports_it(self) -> None:
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
        unit = next(u for u in build_units(resolved, PROBE_TABLE) if u.pkt.name == "probe")
        text = _tree_packet(unit, shape, _packet_body(unit, resolved))
        self.assertIn('#[path = "probe_packet.ext.rs"]', text)
        self.assertIn("mod ext;", text)
        self.assertIn("#[allow(unused_imports)]", text)
        self.assertIn("pub use ext::*;", text)

    def test_shape_that_disagrees_with_the_wire_is_an_error(self) -> None:
        resolved, unit = _analyze_keep_alive()
        shape = _keep_alive_shape()
        bad = replace(
            shape,
            fields=(FieldShape("id", "i32", shape.fields[0].docs),),
        )
        with self.assertRaises(EmitError) as ctx:
            emit_tree(
                resolved,
                Path(tempfile.mkdtemp()) / "ws",
                KEEP_ALIVE_TABLE,
                self._shapes_dir(bad),
            )
        self.assertIn("id", str(ctx.exception))

    def _shapes_dir(self, shape: Shape) -> Path:
        td = tempfile.mkdtemp()
        self.addCleanup(lambda: shutil.rmtree(td, ignore_errors=True))
        shapes = Path(td)
        _write_shape(shapes, shape)
        return shapes


class TreePacketEmitTest(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.workspace = Path(tmp.name) / "rstom"
        self.workspace.mkdir()
        self.resolved, self.unit = _analyze_keep_alive()
        self.shape = _keep_alive_shape()
        self.shapes = Path(tmp.name) / "shapes"
        _write_shape(self.shapes, self.shape)

    def test_emit_writes_shaped_packet(self) -> None:
        table_path = self.workspace / "tools" / "packets_gen" / "schema" / "packets.json"
        table_path.parent.mkdir(parents=True, exist_ok=True)
        table_path.write_text(json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8")
        written = emit_tree(self.resolved, self.workspace, KEEP_ALIVE_TABLE, self.shapes)
        pkt = "crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs"
        self.assertIn(pkt, written)
        self.assertIn("Generated by `bps emit rust`", (self.workspace / pkt).read_text(encoding="utf-8"))

    def test_check_flags_a_shaped_packet_whose_file_is_hand_written(self) -> None:
        table_path = self.workspace / "tools" / "packets_gen" / "schema" / "packets.json"
        table_path.parent.mkdir(parents=True, exist_ok=True)
        table_path.write_text(json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8")
        path = hand_packet_path(self.workspace, self.unit)
        emit_tree(self.resolved, self.workspace, KEEP_ALIVE_TABLE, self.shapes)
        path.write_text("// hand\n", encoding="utf-8")
        msgs = emit_tree(self.resolved, self.workspace, KEEP_ALIVE_TABLE, self.shapes, check=True)
        self.assertTrue(any("hand-written" in m for m in msgs))

    def test_check_flags_a_generated_file_with_no_shape(self) -> None:
        table_path = self.workspace / "tools" / "packets_gen" / "schema" / "packets.json"
        table_path.parent.mkdir(parents=True, exist_ok=True)
        table_path.write_text(json.dumps(KEEP_ALIVE_TABLE_DATA), encoding="utf-8")
        emit_tree(self.resolved, self.workspace, KEEP_ALIVE_TABLE, self.shapes)
        orphan = (
            self.workspace
            / "crates/rstom/src/network/packet/server/play/orphan_packet.rs"
        )
        orphan.parent.mkdir(parents=True, exist_ok=True)
        orphan.write_text("//! Generated by `bps emit rust` orphan\n", encoding="utf-8")
        msgs = emit_tree(self.resolved, self.workspace, KEEP_ALIVE_TABLE, self.shapes, check=True)
        self.assertTrue(any("orphan" in m for m in msgs))


@unittest.skipUnless(
    (RSTOM / "crates/rstom/src/network/packet").is_dir(),
    "rstom worktree packet sources not present",
)
class TreePacketExtractedShapeTest(unittest.TestCase):
    def test_recipe_book_settings_new_attributes(self) -> None:
        path = RSTOM / "crates/rstom/src/network/packet/server/play/recipe_book_settings_packet.rs"
        shape = extract_shape(
            path.read_text(encoding="utf-8"),
            "RecipeBookSettingsPacket",
            "server/play/recipe_book_settings_packet",
        )
        source = (
            f'import "{ROOT / "schemas" / "std" / "integers.bps"}"\n'
            f'import "{ROOT / "schemas" / "std" / "varint.bps"}"\n'
            "packet recipe_book_settings {\n"
            "    crafting_recipe_book_open: boolean\n"
            "    crafting_recipe_book_filter_active: boolean\n"
            "    smelting_recipe_book_open: boolean\n"
            "    smelting_recipe_book_filter_active: boolean\n"
            "    blast_furnace_recipe_book_open: boolean\n"
            "    blast_furnace_recipe_book_filter_active: boolean\n"
            "    smoker_recipe_book_open: boolean\n"
            "    smoker_recipe_book_filter_active: boolean\n"
            "}\n"
            "protocol minecraft @ v1 {\n"
            "    id = 1\n"
            "    frame default { length: varint id: varint payload: rest }\n"
            "    route play.clientbound 0x00 => recipe_book_settings\n"
            "}\n"
        )
        with tempfile.TemporaryDirectory() as td:
            schema = Path(td) / "r.bps"
            schema.write_text(source, encoding="utf-8")
            resolved = analyze(load_schema([str(schema)]))
        table = ClassTable(
            {
                "packets": [class_entry("server", "play", "RecipeBookSettingsPacket")],
                "stateTables": [
                    {"state": "play", "side": "SERVER", "packets": ["RecipeBookSettingsPacket"]},
                ],
            }
        )
        unit = next(u for u in build_units(resolved, table) if u.pkt.name == "recipe_book_settings")
        text = _tree_packet(unit, shape, _packet_body(unit, resolved))
        self.assertIn("fn_params_excessive_bools", text)
        self.assertIn("too_many_arguments", text)
        self.assertIn("#[must_use]", text)

    def test_plugin_message_string_and_vec_fields(self) -> None:
        path = RSTOM / "crates/rstom/src/network/packet/server/common/plugin_message_packet.rs"
        shape = extract_shape(
            path.read_text(encoding="utf-8"),
            "PluginMessagePacket",
            "server/common/plugin_message_packet",
            packet_path=path,
        )
        self.assertEqual(shape.extras, "plugin_message_packet.ext.rs")
        source = (
            f'import "{ROOT / "schemas" / "std" / "varint.bps"}"\n'
            f'import "{ROOT / "schemas" / "minecraft_core.bps"}"\n'
            "packet plugin_message { channel: string data: rest }\n"
            "protocol minecraft @ v1 {\n"
            "    id = 1\n"
            "    frame default { length: varint id: varint payload: rest }\n"
            "    route play.clientbound 0x00 => plugin_message\n"
            "}\n"
        )
        with tempfile.TemporaryDirectory() as td:
            schema = Path(td) / "p.bps"
            schema.write_text(source, encoding="utf-8")
            resolved = analyze(load_schema([str(schema)]))
        table = ClassTable(
            {
                "packets": [class_entry("server", "common", "PluginMessagePacket")],
                "stateTables": [
                    {"state": "play", "side": "SERVER", "packets": ["PluginMessagePacket"]},
                ],
            }
        )
        unit = next(u for u in build_units(resolved, table) if u.pkt.name == "plugin_message")
        text = _tree_packet(unit, shape, _packet_body(unit, resolved))
        self.assertIn("pub channel: String", text)
        self.assertIn("pub data: Vec<u8>", text)
        self.assertIn("value.channel.clone()", text)

    def test_const_fn_new_from_extracted_shape(self) -> None:
        path = RSTOM / "crates/rstom/src/network/packet/server/play/acknowledge_block_change_packet.rs"
        shape = extract_shape(
            path.read_text(encoding="utf-8"),
            "AcknowledgeBlockChangePacket",
            "server/play/acknowledge_block_change_packet",
        )
        self.assertTrue(shape.new.const)
        source = (
            f'import "{ROOT / "schemas" / "std" / "varint.bps"}"\n'
            "packet acknowledge_block_change { sequence: varint }\n"
            "protocol minecraft @ v1 {\n"
            "    id = 1\n"
            "    frame default { length: varint id: varint payload: rest }\n"
            "    route play.clientbound 0x00 => acknowledge_block_change\n"
            "}\n"
        )
        with tempfile.TemporaryDirectory() as td:
            schema = Path(td) / "a.bps"
            schema.write_text(source, encoding="utf-8")
            resolved = analyze(load_schema([str(schema)]))
        table = ClassTable(
            {
                "packets": [class_entry("server", "play", "AcknowledgeBlockChangePacket")],
                "stateTables": [
                    {
                        "state": "play",
                        "side": "SERVER",
                        "packets": ["AcknowledgeBlockChangePacket"],
                    },
                ],
            }
        )
        unit = next(
            u for u in build_units(resolved, table) if u.pkt.name == "acknowledge_block_change"
        )
        text = _tree_packet(unit, shape, _packet_body(unit, resolved))
        self.assertIn("pub const fn new(", text)
        self.assertIn("pub struct AcknowledgeBlockChangeSerializer", text)


if __name__ == "__main__":
    unittest.main()
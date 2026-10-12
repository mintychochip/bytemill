"""`bps emit rstom-tree` writes the wire-pin gate into an rstom workspace."""

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

from bps.analyze import analyze
from bps.cli import main
from bps.emit_tree import emit_tree
from bps.errors import EmitError
from bps.load import load_schema

from tests.rstom_tables import PROBE_SCHEMA, PROBE_TABLE, probe_source

ROOT = Path(__file__).resolve().parents[1]


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
        self.shapes = ROOT / "targets" / "rstom" / "shapes"

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


if __name__ == "__main__":
    unittest.main()

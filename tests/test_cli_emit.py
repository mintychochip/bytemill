"""`bps emit rstom` finds rstom's class table: by flag, or beside the crate it writes."""

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

from bps.cli import main

from tests.rstom_tables import PROBE_SCHEMA, probe_source

ROOT = Path(__file__).resolve().parents[1]


def _run(*argv: str) -> tuple[int, str]:
    err = io.StringIO()
    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
        code = main(list(argv))
    return code, err.getvalue()


class EmitRstomCliTest(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)
        self.schema = self.dir / "probe.bps"
        self.schema.write_text(probe_source(ROOT), encoding="utf-8")

    def write_table(self, path: Path) -> Path:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(PROBE_SCHEMA), encoding="utf-8")
        return path

    def test_the_table_is_read_from_the_workspace_the_crate_is_written_into(self) -> None:
        workspace = self.dir / "rstom"
        self.write_table(workspace / "tools" / "packets_gen" / "schema" / "packets.json")
        out = workspace / "crates" / "bps_rstom"
        code, err = _run("emit", "rstom", str(self.schema), "--out", str(out))
        self.assertEqual((code, err), (0, ""))
        self.assertTrue((out / "src" / "server" / "play" / "probe_packet.rs").is_file())

    def test_a_flag_names_the_table_for_a_crate_outside_a_workspace(self) -> None:
        table = self.write_table(self.dir / "elsewhere" / "packets.json")
        out = self.dir / "scratch"
        code, err = _run("emit", "rstom", str(self.schema), "--out", str(out), "--packets-json", str(table))
        self.assertEqual((code, err), (0, ""))
        self.assertTrue((out / "src" / "server" / "play" / "probe_packet.rs").is_file())

    def test_without_a_table_the_emit_says_where_it_looked(self) -> None:
        out = self.dir / "scratch"
        code, err = _run("emit", "rstom", str(self.schema), "--out", str(out))
        self.assertEqual(code, 3)
        self.assertIn("--packets-json", err)
        self.assertIn("packets.json", err)
        self.assertFalse(out.exists(), "nothing is written when the table is missing")


if __name__ == "__main__":
    unittest.main()

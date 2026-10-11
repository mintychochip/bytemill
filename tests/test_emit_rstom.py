"""Tests for `bps emit rstom`: the `rstom-packets` crate shape, generated from BPS."""

import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from bps.analyze import analyze
from bps.emit_rstom import emit_rstom
from bps.errors import EmitError
from bps.load import load_schema
from bps.minestom import build_units

from tests import java_probes as java
from tests.rstom_tables import (
    PACKETS_GEN_SCHEMA,
    PROBE_TABLE,
    RSTOM,
    RSTOM_PACKETS,
    probe_source,
    real_table,
)
from tests.test_varint import load_minecraft

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = "net.minestom.server.network.packet"


class EmitRstomTest(unittest.TestCase):
    """One emit of the whole protocol, shared: it is the slow part."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.resolved = load_minecraft()
        cls.table = real_table()
        cls.units = build_units(cls.resolved, cls.table)
        tmp = tempfile.TemporaryDirectory()
        cls.addClassCleanup(tmp.cleanup)
        cls.out = Path(tmp.name) / "bps_rstom"
        # Absolute, so the crate also builds from here: the test never writes into rstom.
        emit_rstom(cls.resolved, cls.out, cls.table, rstom_path=str(RSTOM / "crates" / "rstom"))
        cls.src = cls.out / "src"

    def modules(self) -> set[str]:
        """The packet modules of the emitted crate, as `client/play/foo_packet`."""
        return {
            p.relative_to(self.src).with_suffix("").as_posix()
            for group in ("client", "server")
            for p in (self.src / group).rglob("*.rs")
            if p.name != "mod.rs"
        }

    @unittest.skipUnless(RSTOM_PACKETS.is_dir(), "rstom-packets not present")
    def test_every_packet_module_of_rstom_packets_exists_at_the_same_path(self) -> None:
        """The drop-in claim: a module path that works for `rstom-packets` works here."""
        data = json.loads(PACKETS_GEN_SCHEMA.read_text(encoding="utf-8"))
        shared = {f"{s['module']}" for s in data["shared"]}
        theirs = {
            p.relative_to(RSTOM_PACKETS / "src").with_suffix("").as_posix()
            for group in ("client", "server")
            for p in (RSTOM_PACKETS / "src" / group).rglob("*.rs")
            if p.name != "mod.rs" and not p.name.endswith(".tests.rs")
        } - shared
        self.assertEqual(theirs - self.modules(), set(), "modules rstom-packets has and BPS lacks")
        # The one extra: the configuration form of a class Minestom gives two serializers.
        self.assertEqual(self.modules() - theirs, {"server/configuration/show_dialog_packet"})

    def test_each_module_declares_the_struct_and_class_name_of_its_unit(self) -> None:
        for unit in self.units:
            cls = unit.cls
            text = (self.src / cls.group / cls.package / f"{cls.stem}.rs").read_text(encoding="utf-8")
            self.assertIn(f"pub struct {cls.name} {{", text)
            self.assertRegex(text, rf'const CLASS_NAME: &\'static str =\s*"{re.escape(cls.java)}";')
            marker = "ClientPacket" if cls.group == "client" else "ServerPacket"
            self.assertIn(f"impl {marker} for {cls.name} {{}}", text)

    def test_each_package_lists_its_modules_and_serializers(self) -> None:
        for group in ("client", "server"):
            packages = sorted(p.name for p in (self.src / group).iterdir() if p.is_dir())
            declared = re.findall(r"pub mod (\w+);", (self.src / f"{group}.rs").read_text(encoding="utf-8"))
            self.assertEqual(declared, packages)
            for package in packages:
                stems = sorted(p.stem for p in (self.src / group / package).glob("*.rs") if p.name != "mod.rs")
                text = (self.src / group / package / "mod.rs").read_text(encoding="utf-8")
                self.assertEqual(re.findall(r"^pub mod (\w+);", text, re.M), stems)
                self.assertEqual(len(re.findall(r"::CLASS_NAME,", text)), len(stems))
        ported = (self.src / "ported.rs").read_text(encoding="utf-8")
        self.assertEqual(len(re.findall(r"ported\.extend\(crate::", ported)), 11)

    def test_the_old_flat_layout_is_gone(self) -> None:
        self.assertFalse((self.src / "packets").exists())

    def test_the_registry_functions_are_the_ones_rstom_packets_exports(self) -> None:
        text = (self.src / "packet_vanilla" / "mod.rs").read_text(encoding="utf-8")
        exported = set(re.findall(r"pub use \w+::(\w+_registry)", text))
        self.assertEqual(
            exported,
            {f"{side}_{state}_registry" for side in ("client", "server")
             for state in ("handshake", "status", "login", "configuration", "play")},
        )

    @unittest.skipUnless(java.available(), "rstom probe fixtures not present")
    def test_the_registry_test_expects_the_tables_minestom_recorded(self) -> None:
        """Python closes the loop: the Rust test's expectations are Minestom's recorded tables."""
        text = (self.out / "tests" / "registry_tables.rs").read_text(encoding="utf-8")
        expected = {}
        rows = re.findall(r'\(\s*"(\w+)",\s*"(\w+)",\s*&\[(.*?)\],?\s*\)', text, re.S)
        for side, state, body in rows:
            expected[(side, state)] = re.findall(r'"(\w+)"', body)
        recorded: dict[tuple[str, str], list[str]] = {}
        for case in java._cases("packetprobe"):
            if case["op"] == "registryTable":
                recorded[(case["side"], case["state"])] = case["packets"]
        self.assertEqual(len(recorded), 10)
        self.assertEqual(expected, recorded)

    def test_emitted_crate_compiles_and_passes_its_tests(self) -> None:
        """The crate builds against real rstom; the round-trips and registry tables pass.

        The emit is outside the rstom checkout, with the workspace's target dir
        reused for the build cache, so running this suite never writes into
        another repository: refreshing the deployed crate is the CLI's job.
        """
        if shutil.which("cargo") is None:
            self.skipTest("cargo not installed")
        if not (RSTOM / "Cargo.toml").is_file():
            self.skipTest("rstom workspace not present")
        env = dict(os.environ, CARGO_TARGET_DIR=str(RSTOM / "target"))
        proc = subprocess.run(
            ["cargo", "test", "--manifest-path", str(self.out / "Cargo.toml"), "--quiet"],
            env=env,
            capture_output=True,
            text=True,
            timeout=900,
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stdout + proc.stderr)

    def test_every_packet_pins_the_rust_encoder_to_the_python_reference(self) -> None:
        """The pin is what makes a round-trip meaningful.

        Without it the encode/decode pair is only checked for self-consistency. A
        regression that renders a packet as consistency-only (wrong wire types,
        unframed payloads, dropped fields) must not pass silently.
        """
        tests = sorted((self.out / "tests").glob("roundtrip_*.rs"))
        unpinned = [
            t.stem.removeprefix("roundtrip_")
            for t in tests
            if "Pinned by the Python interpreter" not in t.read_text(encoding="utf-8")
        ]
        self.assertEqual(len(tests), len(self.units), "one test per struct")
        self.assertEqual(unpinned, [], "packets that only check consistency")


class EmitSchemaErrorTest(unittest.TestCase):
    def test_an_encoding_reused_in_one_packet_is_rejected(self) -> None:
        """The generated struct has one namespace per packet.

        Two uses of an encoding put the same field names in it twice. Merging
        them would drop the second use from the wire silently, so the emit must
        refuse it and ask for distinct names.
        """
        source = probe_source(
            ROOT,
            """
            encoding pair = {
                n: varint
                items: varint[] count = n
            }
            packet probe { first: pair  second: pair }
            """,
        )
        with tempfile.TemporaryDirectory() as td:
            schema = Path(td) / "probe.bps"
            schema.write_text(source, encoding="utf-8")
            resolved = analyze(load_schema([schema]))
            with self.assertRaisesRegex(EmitError, "'n'.*first.*second"):
                emit_rstom(resolved, Path(td) / "crate", PROBE_TABLE)


class EmitSweepTest(unittest.TestCase):
    """The crate is generated, so a re-emit drops what the schema no longer declares."""

    def test_a_re_emit_drops_stale_generated_files_and_keeps_hand_written_ones(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            schema = Path(td) / "probe.bps"
            schema.write_text(probe_source(ROOT), encoding="utf-8")
            resolved = analyze(load_schema([schema]))
            out = Path(td) / "crate"
            generated = "//! Generated by `bps emit rust`: left over from an earlier emit.\n"
            stale = [
                out / "src" / "packets" / "old.rs",
                out / "src" / "client" / "play" / "gone_packet.rs",
                out / "tests" / "roundtrip_gone.rs",
            ]
            hand = [out / "src" / "util.rs", out / "tests" / "mine.rs"]
            for path in stale + hand:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(generated if path in stale else "// by hand\n", encoding="utf-8")

            emit_rstom(resolved, out, PROBE_TABLE)

            self.assertEqual([p for p in stale if p.exists()], [])
            self.assertFalse((out / "src" / "packets").exists(), "an emptied directory is removed")
            self.assertEqual([p for p in hand if not p.exists()], [])
            self.assertTrue((out / "src" / "server" / "play" / "probe_packet.rs").is_file())


if __name__ == "__main__":
    unittest.main()

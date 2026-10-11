"""Tests for `bps emit rstom` (rstom `PacketSerializer` crate shape)."""

import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from bps.emit_rstom import _routes_by_registry, emit_rstom

from tests.test_varint import load_minecraft

ROOT = Path(__file__).resolve().parents[1]
PACKETS_GEN_SCHEMA = (
    ROOT.parent / "rstom" / "tools" / "packets_gen" / "schema" / "packets.json"
)

class EmitRstomTest(unittest.TestCase):
    def test_registry_route_ids_are_contiguous(self) -> None:
        resolved = load_minecraft()
        by_reg = _routes_by_registry(resolved)
        for key, entries in by_reg.items():
            ids = [route.packet_id for route, _pkt in entries]
            self.assertEqual(
                ids,
                list(range(len(ids))),
                msg=f"{key}: registry order must follow positional ids 0..n-1",
            )

    def test_registry_table_sizes_match_packets_gen(self) -> None:
        if not PACKETS_GEN_SCHEMA.is_file():
            self.skipTest("rstom packets_gen schema not present")
        resolved = load_minecraft()
        by_reg = _routes_by_registry(resolved)
        gen = json.loads(PACKETS_GEN_SCHEMA.read_text(encoding="utf-8"))
        state_alias = {"handshake": "handshaking"}
        for table in gen["stateTables"]:
            side = table["side"].lower()
            group = "client" if side == "client" else "server"
            state = state_alias.get(table["state"], table["state"])
            key = f"{group}_{state}"
            gen_n = len(table.get("packets") or [])
            bps_n = len(by_reg.get(key) or [])
            self.assertEqual(
                bps_n,
                gen_n,
                msg=f"{key}: BPS route count vs packets_gen stateTables",
            )

    def test_emit_writes_vanilla_and_ported(self) -> None:
        resolved = load_minecraft()
        with tempfile.TemporaryDirectory() as td:
            out = Path(td) / "crate"
            emit_rstom(resolved, out)
            src = out / "src"
            self.assertTrue((src / "ported.rs").is_file())
            self.assertTrue((src / "packet_vanilla" / "mod.rs").is_file())
            self.assertTrue((src / "packets" / "list.rs").is_file())
            handshake = (src / "packets" / "handshake.rs").read_text(encoding="utf-8")
            self.assertIn("net.minestom.server.network.packet.", handshake)
            self.assertRegex(
                handshake,
                re.compile(
                    r'const CLASS_NAME.*HandshakePacket', re.DOTALL
                ),
            )

    def test_emitted_crate_compiles_and_passes_roundtrips(self) -> None:
        """The emitted crate compiles against real rstom and its tests pass.

        The emit goes to a scratch directory outside the rstom checkout (which
        points at rstom by absolute path, with the workspace's target dir
        reused for the build cache), so running this suite never mutates
        another repository — refreshing the deployed crate is the CLI's job.
        """
        if shutil.which("cargo") is None:
            self.skipTest("cargo not installed")
        rstom_ws = ROOT.parent / "rstom"
        if not (rstom_ws / "Cargo.toml").is_file():
            self.skipTest("rstom workspace not present")
        resolved = load_minecraft()
        with tempfile.TemporaryDirectory() as td:
            out = Path(td) / "bps_rstom"
            emit_rstom(resolved, out, rstom_path=str(rstom_ws / "crates" / "rstom"))
            env = dict(os.environ, CARGO_TARGET_DIR=str(rstom_ws / "target"))
            proc = subprocess.run(
                ["cargo", "test", "--manifest-path", str(out / "Cargo.toml"), "--quiet"],
                env=env,
                capture_output=True,
                text=True,
                timeout=600,
            )
            self.assertEqual(
                proc.returncode,
                0,
                msg=proc.stdout + proc.stderr,
            )

    def test_pinned_reference_coverage(self) -> None:
        """Most packets must pin the Rust encoder to the Python reference.

        The pin is what makes the round-trip meaningful; without it the
        encode/decode pair is only checked for self-consistency. A regression
        that renders more packets as consistency-only (wrong wire types,
        unframed payloads, dropped fields) must not pass silently.
        """
        resolved = load_minecraft()
        with tempfile.TemporaryDirectory() as td:
            out = Path(td) / "crate"
            emit_rstom(resolved, out)
            test_files = list((out / "tests").glob("roundtrip_*.rs"))
            pinned = sum(
                "Pinned by the Python interpreter" in f.read_text(encoding="utf-8")
                for f in test_files
            )
            self.assertGreaterEqual(
                len(test_files), 231, msg="one roundtrip test per declared packet"
            )
            self.assertGreaterEqual(
                pinned,
                180,
                msg=f"only {pinned} of {len(test_files)} packets pin reference bytes",
            )


if __name__ == "__main__":
    unittest.main()

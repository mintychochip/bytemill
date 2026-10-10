import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from bps.emit_rust import emit_rust

from tests.test_varint import load_minecraft

ROOT = Path(__file__).resolve().parents[1]


class EmitRustTest(unittest.TestCase):
    def test_emit_and_cargo_test(self) -> None:
        if shutil.which("cargo") is None:
            self.skipTest("cargo not installed")
        resolved = load_minecraft()
        with tempfile.TemporaryDirectory() as td:
            out = Path(td) / "crate"
            emit_rust(resolved, out)
            proc = subprocess.run(
                ["cargo", "test", "--quiet"],
                cwd=out,
                capture_output=True,
                text=True,
                timeout=300,
            )
            self.assertEqual(
                proc.returncode,
                0,
                msg=proc.stdout + proc.stderr,
            )


if __name__ == "__main__":
    unittest.main()

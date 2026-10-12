"""Tests for ``tests.rstom_tables`` path defaults used by integration tests."""

from __future__ import annotations

import importlib
import os
import unittest
from pathlib import Path
from unittest.mock import patch


class RstomPathDefaultTest(unittest.TestCase):
    """``RSTOM`` resolves from env, then rstom-bps worktree, then live checkout."""

    def test_bps_rstom_dir_overrides_default(self) -> None:
        with patch.dict(os.environ, {"BPS_RSTOM_DIR": "/tmp/custom"}):
            import tests.rstom_tables as rt

            importlib.reload(rt)
            self.assertEqual(str(rt.RSTOM), "/tmp/custom")

    def test_prefers_rstom_bps_sibling_when_present(self) -> None:
        without_env = {k: v for k, v in os.environ.items() if k != "BPS_RSTOM_DIR"}
        with patch.dict(os.environ, without_env, clear=True):
            import tests.rstom_tables as rt

            importlib.reload(rt)
            protoc = Path(rt.__file__).resolve().parents[1]
            if (protoc.parent / "rstom-bps").is_dir():
                self.assertEqual(rt.RSTOM, protoc.parent / "rstom-bps")
            else:
                self.assertEqual(rt.RSTOM, protoc.parent / "rstom")


if __name__ == "__main__":
    unittest.main()

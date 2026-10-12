"""Emit shaped packets and the wire-pin gate into an rstom workspace."""

from __future__ import annotations

import shutil
import subprocess
import tempfile
from pathlib import Path

from bps.emit_rstom import _is_generated
from bps.minestom import ClassTable, build_units
from bps.pins import PINS_TEST_RS, build_pins, pins_json
from bps.ast import ResolvedSchema
from bps.errors import EmitError


def _rustfmt_text(text: str) -> str:
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


def _rustfmt_paths(paths: list[Path]) -> None:
    rustfmt = shutil.which("rustfmt")
    if not rustfmt:
        return
    for f in paths:
        if f.suffix == ".rs" and f.is_file():
            subprocess.run(
                [rustfmt, "--edition", "2024", str(f)],
                check=False,
                capture_output=True,
            )


def emit_tree(
    resolved: ResolvedSchema,
    workspace: Path,
    classes: ClassTable,
    shapes_dir: Path,
    check: bool = False,
) -> list[str]:
    """Write the pin gate (and later, shaped packets). Returns paths written or drift messages."""
    units = build_units(resolved, classes)
    rows = build_pins(resolved, units)
    tests = workspace / "crates" / "rstom" / "tests"
    rust_path = tests / "bps_wire_pins.rs"
    json_path = tests / "bps" / "wire_pins.json"
    known_path = tests / "bps" / "wire_pins_known.json"

    planned: dict[Path, str] = {
        rust_path: _rustfmt_text(PINS_TEST_RS),
        json_path: pins_json(rows) + "\n",
    }

    if check:
        messages: list[str] = []
        for path, want in planned.items():
            if not path.is_file():
                messages.append(f"{path}: missing")
                continue
            have = path.read_text(encoding="utf-8")
            if have != want:
                messages.append(f"{path}: drift")
        return messages

    written: list[Path] = []
    for path, text in planned.items():
        if path.is_file() and not _is_generated(path):
            raise EmitError(f"refusing to overwrite hand file {path}")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        written.append(path)

    if not known_path.is_file():
        known_path.parent.mkdir(parents=True, exist_ok=True)
        known_path.write_text("[]\n", encoding="utf-8")
        written.append(known_path)

    _rustfmt_paths(written)
    return [p.relative_to(workspace).as_posix() for p in written]

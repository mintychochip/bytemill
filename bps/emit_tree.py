"""Emit shaped packets and the wire-pin gate into an rstom workspace."""

from __future__ import annotations

import shutil
import subprocess
import tempfile
from pathlib import Path

from bps.ast import ResolvedSchema
from bps.emit_rstom import _is_generated
from bps.errors import EmitError
from bps.minestom import ClassTable, build_units
from bps.pins import PINS_TEST_RS, build_pins, pins_json, pins_json_is_owned


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


def _is_owned_output(path: Path) -> bool:
    """Whether the emitter may replace an existing file at ``path``."""
    if not path.is_file():
        return True
    if path.suffix == ".rs":
        return _is_generated(path)
    if path.name == "wire_pins.json":
        return pins_json_is_owned(path.read_text(encoding="utf-8"))
    return False


def _refuse_hand_files(planned: dict[Path, str]) -> None:
    for path in planned:
        if not _is_owned_output(path):
            raise EmitError(f"refusing to overwrite hand file {path}")


def emit_tree(
    resolved: ResolvedSchema,
    workspace: Path,
    classes: ClassTable,
    shapes_dir: Path,
    check: bool = False,
) -> list[str]:
    """Write the pin gate (and later, shaped packets). Returns paths written or drift messages."""
    from bps.synth import SYNTH_OVERRIDES, validate_all_synth_overrides

    if {name for name, _ in SYNTH_OVERRIDES} <= resolved.packets.keys():
        validate_all_synth_overrides(resolved)
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

    _refuse_hand_files(planned)

    written: list[Path] = []
    for path, text in planned.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        written.append(path)

    if not known_path.is_file():
        known_path.parent.mkdir(parents=True, exist_ok=True)
        known_path.write_text("[]\n", encoding="utf-8")
        written.append(known_path)

    _rustfmt_paths(written)
    return [p.relative_to(workspace).as_posix() for p in written]

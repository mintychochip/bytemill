"""Emit shaped packets and the wire-pin gate into an rstom workspace."""

from __future__ import annotations

import shutil
import subprocess
import tempfile
from pathlib import Path

from bps.ast import ResolvedSchema
from bps.emit_rstom import _is_generated, _packet_body
from bps.errors import EmitError
from bps.minestom import ClassTable, Unit, build_units
from bps.pins import PINS_TEST_RS, build_pins, pins_json, pins_json_is_owned
from bps.shapes import (
    Shape,
    ShapeError,
    bps_field_pairs,
    extract_shape,
    hand_packet_path,
    load_ledger,
    mismatch,
    module_path,
)
from bps.tree_packet import _validate_shape, tree_packet

_PACKET_SCAN_ROOT = Path("crates") / "rstom" / "src" / "network" / "packet"


def _rustfmt_text(
    text: str,
    *,
    parent_dir: Path | None = None,
    skip_children: bool = False,
) -> str:
    rustfmt = shutil.which("rustfmt")
    if not rustfmt:
        return text
    parent = parent_dir or Path(tempfile.gettempdir())
    parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        suffix=".rs",
        delete=False,
        dir=parent,
        encoding="utf-8",
    ) as f:
        f.write(text)
        path = Path(f.name)
    args = [rustfmt, "--edition", "2024", str(path)]
    if skip_children:
        args[1:1] = ["--unstable-features", "--skip-children"]
    proc = subprocess.run(args, capture_output=True, text=True)
    try:
        if proc.returncode != 0:
            err = proc.stderr.strip() or proc.stdout.strip() or f"exit {proc.returncode}"
            raise EmitError(f"rustfmt failed: {err}")
        return path.read_text(encoding="utf-8")
    finally:
        path.unlink(missing_ok=True)


def _rustfmt_paths(paths: list[Path]) -> None:
    rustfmt = shutil.which("rustfmt")
    if not rustfmt:
        return
    for f in paths:
        if f.suffix != ".rs" or not f.is_file():
            continue
        skip = "mod ext;" in f.read_text(encoding="utf-8")
        args = [rustfmt, "--edition", "2024", str(f)]
        if skip:
            args[1:1] = ["--unstable-features", "--skip-children"]
        proc = subprocess.run(args, capture_output=True, text=True)
        if proc.returncode != 0:
            err = proc.stderr.strip() or proc.stdout.strip() or f"exit {proc.returncode}"
            raise EmitError(f"rustfmt failed on {f}: {err}")


def _is_owned_output(path: Path) -> bool:
    """Whether the emitter may replace an existing file at ``path``."""
    if not path.is_file():
        return True
    if path.suffix == ".rs":
        return _is_generated(path)
    if path.name == "wire_pins.json":
        return pins_json_is_owned(path.read_text(encoding="utf-8"))
    return False


def _refuse_hand_files(planned: dict[Path, str], hand_swaps: set[Path]) -> None:
    for path in planned:
        if path in hand_swaps:
            continue
        if not _is_owned_output(path):
            raise EmitError(f"refusing to overwrite hand file {path}")


def _packet_output_path(workspace: Path, unit: Unit) -> Path:
    return workspace / _PACKET_SCAN_ROOT / f"{module_path(unit)}.rs"


def _require_extras(workspace: Path, shape: Shape, packet_dir: Path) -> None:
    if not shape.extras:
        return
    ext = packet_dir / shape.extras
    if not ext.is_file():
        raise EmitError(f"create {ext.relative_to(workspace).as_posix()} first")


def _hand_matches_ledger(path: Path, shape: Shape) -> bool:
    try:
        hand = extract_shape(
            path.read_text(encoding="utf-8"), shape.cls, shape.module, packet_path=path
        )
    except ShapeError:
        return False
    return hand == shape


def _orphan_generated_packets(workspace: Path, shaped_modules: set[str]) -> list[str]:
    root = workspace / _PACKET_SCAN_ROOT
    if not root.is_dir():
        return []
    out: list[str] = []
    for path in sorted(root.rglob("*.rs")):
        if not _is_generated(path):
            continue
        rel = path.relative_to(root).with_suffix("").as_posix()
        if rel not in shaped_modules:
            out.append(f"{path}: orphan")
    return out


def _planned_packets(
    resolved: ResolvedSchema,
    workspace: Path,
    classes: ClassTable,
    shapes: dict[str, Shape],
    *,
    swap_hand: bool,
) -> tuple[dict[Path, str], set[Path]]:
    units = build_units(resolved, classes)
    by_module = {module_path(u): u for u in units}
    for module in shapes:
        if module not in by_module:
            raise EmitError(f"stale shape: {module}")
    planned: dict[Path, str] = {}
    hand_swaps: set[Path] = set()
    for unit in units:
        module = module_path(unit)
        shape = shapes.get(module)
        if shape is None:
            continue
        _validate_shape(shape)
        reasons = mismatch(shape, bps_field_pairs(unit.pkt, resolved))
        if reasons:
            raise EmitError(f"{module}: {'; '.join(reasons)}")
        body = _packet_body(unit, resolved)
        path = _packet_output_path(workspace, unit)
        packet_dir = path.parent
        _require_extras(workspace, shape, packet_dir)
        raw = tree_packet(unit, shape, body)
        text = _rustfmt_text(
            raw,
            parent_dir=packet_dir,
            skip_children=shape.extras is not None,
        )
        if swap_hand and path.is_file() and not _is_generated(path):
            if not _hand_matches_ledger(path, shape):
                raise EmitError(f"hand file changed since its shape was extracted: {module}")
            hand_swaps.add(path)
        planned[path] = text
    return planned, hand_swaps


def emit_tree(
    resolved: ResolvedSchema,
    workspace: Path,
    classes: ClassTable,
    shapes_dir: Path,
    check: bool = False,
) -> list[str]:
    """Write the pin gate and shaped packets. Returns paths written or drift messages."""
    shapes = load_ledger(shapes_dir) if shapes_dir.is_dir() else {}
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
    packet_planned, hand_swaps = _planned_packets(
        resolved, workspace, classes, shapes, swap_hand=not check
    )
    planned.update(packet_planned)

    if check:
        messages: list[str] = []
        shaped_modules = set(shapes)
        for path, want in planned.items():
            if not path.is_file():
                messages.append(f"{path}: missing")
                continue
            have = path.read_text(encoding="utf-8")
            if have != want:
                messages.append(f"{path}: drift")
        for module, shape in shapes.items():
            unit = next((u for u in units if module_path(u) == module), None)
            if unit is None:
                continue
            path = _packet_output_path(workspace, unit)
            if path.is_file() and not _is_generated(path):
                messages.append(f"{path}: hand-written")
        messages.extend(_orphan_generated_packets(workspace, shaped_modules))
        return messages

    _refuse_hand_files(planned, hand_swaps)

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


def _tree_packet(unit: Unit, shape: Shape, body) -> str:
    """Task-6 seam; delegates to :func:`bps.tree_packet.tree_packet`."""
    return tree_packet(unit, shape, body)


__all__ = ["emit_tree", "_tree_packet"]

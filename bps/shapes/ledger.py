"""Map BPS units to hand packet files and compare them with the shape ledger."""

from __future__ import annotations

from pathlib import Path

from bps.ast import PacketDecl, ResolvedSchema
from bps.emit_rstom import _packet_fields
from bps.minestom import Unit
from bps.shapes.extract import _PACKET_ROOT, extract_shape
from bps.shapes.model import Shape, ShapeError, shape_from_json


def hand_packet_path(workspace: Path, unit: Unit) -> Path:
    return (
        workspace
        / _PACKET_ROOT
        / unit.cls.group
        / unit.cls.package
        / f"{unit.cls.stem}.rs"
    )


def module_path(unit: Unit) -> str:
    return f"{unit.cls.group}/{unit.cls.package}/{unit.cls.stem}"


def bps_field_pairs(pkt: PacketDecl, resolved: ResolvedSchema) -> list[tuple[str, str]]:
    fields, _ = _packet_fields(pkt, resolved)
    out: list[tuple[str, str]] = []
    for f in fields:
        base = f"Vec<{f.rtype}>" if f.repeat_mode is not None else f.rtype
        mapped = f"Option<{base}>" if f.guard is not None else base
        out.append((f.ident, mapped))
    return out


def mismatch(shape: Shape, bps_fields: list[tuple[str, str]]) -> list[str]:
    hand = [(f.name, f.type) for f in shape.fields]
    if len(hand) != len(bps_fields):
        return [f"field count: hand has {len(hand)}, BPS has {len(bps_fields)}"]
    for (hn, ht), (bn, bt) in zip(hand, bps_fields):
        if hn != bn:
            return [f"field {hn}: hand name {hn!r}, BPS name {bn!r}"]
        if ht != bt:
            return [f"field {hn}: hand type {ht!r}, BPS type {bt!r}"]
    return []


def load_ledger(dir: Path) -> dict[str, Shape]:
    out: dict[str, Shape] = {}
    for path in sorted(dir.rglob("*.json")):
        rel = path.relative_to(dir)
        module = rel.with_suffix("").as_posix()
        out[module] = shape_from_json(path.read_text(encoding="utf-8"))
    return out


def packet_report(
    unit: Unit,
    resolved: ResolvedSchema,
    workspace: Path,
) -> str:
    path = hand_packet_path(workspace, unit)
    if not path.is_file():
        return f"missing hand file {path}"
    try:
        shape = extract_shape(
            path.read_text(encoding="utf-8"),
            unit.cls.name,
            module_path(unit),
            packet_path=path,
        )
    except ShapeError as err:
        return str(err)
    reasons = mismatch(shape, bps_field_pairs(unit.pkt, resolved))
    if reasons:
        return "; ".join(reasons)
    return "eligible"


def extract_shapes_for_units(
    resolved: ResolvedSchema,
    workspace: Path,
    units: list[Unit],
    *,
    eligible_only: bool = False,
    class_names: set[str] | None = None,
) -> list[tuple[Unit, Shape]]:
    out: list[tuple[Unit, Shape]] = []
    for unit in units:
        if class_names is not None and unit.cls.name not in class_names:
            continue
        path = hand_packet_path(workspace, unit)
        if not path.is_file():
            continue
        try:
            shape = extract_shape(
                path.read_text(encoding="utf-8"),
                unit.cls.name,
                module_path(unit),
                packet_path=path,
            )
        except ShapeError:
            if eligible_only:
                continue
            raise
        if eligible_only and mismatch(shape, bps_field_pairs(unit.pkt, resolved)):
            continue
        out.append((unit, shape))
    return out

from __future__ import annotations

from collections.abc import Collection, Iterable

from bps import ast
from bps.errors import EmitError, ResolveError
from bps.primitives import BARE_PRIMITIVES
from bps.refs import Ref, item_refs, references, type_refs


def _dedupe(kind: str, decls, name_of) -> dict:
    table: dict = {}
    for d in decls:
        name = name_of(d)
        if name in table:
            raise ResolveError(f"duplicate {kind} {name!r}")
        table[name] = d
    return table


def analyze(schema: ast.Schema) -> ast.ResolvedSchema:
    encodings = _dedupe("encoding", schema.encodings, lambda e: e.name)
    enums = _dedupe("enum", schema.enums, lambda e: e.name)
    packets = _dedupe("packet", schema.packets, lambda p: p.name)
    protocols = _dedupe("protocol", schema.protocols, lambda p: p.name)

    for ref in references(schema):
        _resolve(ref, encodings, enums, packets)

    routes = [
        (route, packets[route.packet_name])
        for proto in schema.protocols
        for route in proto.routes
    ]

    return ast.ResolvedSchema(schema, encodings, enums, packets, protocols, routes)


def reject_unlowered_scalars(
    resolved: ast.ResolvedSchema, lowered: Collection[str], target: str
) -> None:
    """Raise `EmitError` when a packet reaches a scalar encoding `target` cannot lower.

    A scalar encoding is a block with a trailing `= expr`: its value is the
    expression, not the block's fields. An emitter that lowers scalars by name
    (`lowered`) would otherwise emit the block's wire bytes as fields. Scalars
    that are declared but never reached from a packet do not matter, and a
    lowered name is never entered, because the emitter never looks inside it.
    """
    seen: set[str] = set()
    unlowered: set[str] = set()

    def visit(refs: Iterable[Ref]) -> None:
        for ref in refs:
            enc = resolved.encodings.get(ref.name)
            if enc is None or ref.name in seen or ref.name in lowered:
                continue
            seen.add(ref.name)
            if enc.yield_expr is not None:
                unlowered.add(ref.name)
            visit(type_refs(enc.body, f"encoding {ref.name}"))

    for packet in resolved.packets.values():
        visit(item_refs(packet.items, f"packet {packet.name}"))
    if unlowered:
        names = ", ".join(repr(n) for n in sorted(unlowered))
        raise EmitError(
            f"{target} emitter has no lowering for scalar encoding {names}; "
            f"only the std scalars map to {target} types"
        )


def _resolve(
    ref: Ref,
    encodings: dict[str, ast.EncodingDecl],
    enums: dict[str, ast.EnumDecl],
    packets: dict[str, ast.PacketDecl],
) -> None:
    if ref.kind == "packet":
        if ref.name not in packets:
            raise ResolveError(f"{ref.where} references unknown packet {ref.name!r}")
    elif ref.kind == "spread":
        if ref.name not in encodings:
            raise ResolveError(f"unknown spread encoding {ref.name!r} in {ref.where}")
    elif (
        ref.name not in encodings
        and ref.name not in enums
        and ref.name not in BARE_PRIMITIVES
    ):
        raise ResolveError(f"unknown type {ref.name!r} in {ref.where}")

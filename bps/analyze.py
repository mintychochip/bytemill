from __future__ import annotations

from collections.abc import Collection

from bps import ast
from bps.errors import EmitError, ResolveError
from bps.primitives import BARE_PRIMITIVES


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

    for name, enc in encodings.items():
        _check_encoding(enc, encodings, enums, name)

    routes: list[tuple[ast.RouteDecl, ast.PacketDecl]] = []
    for proto in schema.protocols:
        for route in proto.routes:
            if route.packet_name not in packets:
                raise ResolveError(
                    f"route {route.state}.{route.direction} 0x{route.packet_id:x} "
                    f"references unknown packet {route.packet_name!r}"
                )
            routes.append((route, packets[route.packet_name]))

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

    def visit_name(name: str) -> None:
        enc = resolved.encodings.get(name)
        if enc is None or name in seen or name in lowered:
            return
        seen.add(name)
        if enc.yield_expr is not None:
            unlowered.add(name)
        visit_type(enc.body)

    def visit_type(t: ast.TypeRef | ast.BlockType | ast.MatchType) -> None:
        if isinstance(t, ast.TypeRef):
            visit_name(t.name)
        elif isinstance(t, ast.BlockType):
            visit_items(t.items)
        else:
            for arm in t.arms:
                if arm.type_ref is not None:
                    visit_type(arm.type_ref)

    def visit_items(items: tuple[ast.BlockItem, ...]) -> None:
        for item in items:
            if item.kind == "field" and item.wire is not None:
                visit_type(item.wire.type_ref)
            elif item.kind == "group":
                visit_items(item.group_items)
            elif item.kind == "match" and item.match is not None:
                visit_type(item.match)
            elif item.kind == "spread":
                visit_name(item.spread_name)

    for packet in resolved.packets.values():
        visit_items(packet.items)
    if unlowered:
        names = ", ".join(repr(n) for n in sorted(unlowered))
        raise EmitError(
            f"{target} emitter has no lowering for scalar encoding {names}; "
            f"only the std scalars map to {target} types"
        )


def _check_encoding(
    enc: ast.EncodingDecl,
    encodings: dict[str, ast.EncodingDecl],
    enums: dict[str, ast.EnumDecl],
    path: str,
) -> None:
    if isinstance(enc.body, ast.TypeRef):
        _check_type_ref(enc.body, encodings, enums, path)
    elif isinstance(enc.body, ast.BlockType):
        _check_block(enc.body, encodings, enums, path, enc.params)


def _check_type_ref(
    ref: ast.TypeRef,
    encodings: dict[str, ast.EncodingDecl],
    enums: dict[str, ast.EnumDecl],
    path: str,
) -> None:
    if ref.name in encodings or ref.name in BARE_PRIMITIVES:
        return
    if ref.name in enums:
        return
    raise ResolveError(f"unknown type {ref.name!r} in {path}")


def _check_block(
    block: ast.BlockType,
    encodings: dict[str, ast.EncodingDecl],
    enums: dict[str, ast.EnumDecl],
    path: str,
    params: tuple[ast.Param, ...],
) -> None:
    param_names = {p.name for p in params}
    for item in block.items:
        if item.kind == "field" and item.wire:
            _check_field_type(item.wire.type_ref, encodings, enums, path, param_names)
        if item.kind == "spread" and item.spread_name:
            if item.spread_name not in encodings:
                raise ResolveError(f"unknown spread encoding {item.spread_name!r} in {path}")
        if item.kind == "group":
            _check_block(ast.BlockType(item.group_items), encodings, enums, path, params)
        if item.kind == "match" and item.match is not None:
            for arm in item.match.arms:
                if isinstance(arm.type_ref, ast.BlockType):
                    _check_block(arm.type_ref, encodings, enums, path, params)
                elif arm.type_ref is not None:
                    _check_field_type(arm.type_ref, encodings, enums, path, param_names)


def _check_field_type(
    t: ast.TypeRef | ast.BlockType | ast.MatchType,
    encodings: dict[str, ast.EncodingDecl],
    enums: dict[str, ast.EnumDecl],
    path: str,
    param_names: set[str],
) -> None:
    if isinstance(t, ast.TypeRef):
        if t.name in param_names:
            return
        _check_type_ref(t, encodings, enums, path)
    elif isinstance(t, ast.BlockType):
        _check_block(t, encodings, enums, path, ())
    elif isinstance(t, ast.MatchType):
        for arm in t.arms:
            if arm.type_ref is not None:
                _check_field_type(arm.type_ref, encodings, enums, path, param_names)

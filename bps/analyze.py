from __future__ import annotations

from bps import ast
from bps.errors import ResolveError


def analyze(schema: ast.Schema) -> ast.ResolvedSchema:
    encodings = {e.name: e for e in schema.encodings}
    enums = {e.name: e for e in schema.enums}
    packets = {p.name: p for p in schema.packets}
    protocols = {p.name: p for p in schema.protocols}

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
    if ref.name in encodings or ref.name in _PRIMITIVES:
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
            if arm.type_ref and isinstance(arm.type_ref, ast.TypeRef):
                _check_type_ref(arm.type_ref, encodings, enums, path)


_PRIMITIVES = frozenset(
    {
        "u8",
        "i8",
        "u16be",
        "u16le",
        "u32be",
        "u32le",
        "u64be",
        "u64le",
        "i16be",
        "i16le",
        "i32be",
        "i32le",
        "i64be",
        "i64le",
        "f32be",
        "f32le",
        "f64be",
        "f64le",
        "bool",
        "bytes",
        "rest",
        "utf8",
        "mutf8",
        "leb128",
    }
)

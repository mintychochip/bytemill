from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass(frozen=True)
class Param:
    name: str
    default: int | None = None


@dataclass(frozen=True)
class Expr:
    """Expression AST node; kind discriminates shape."""

    kind: str
    value: Any = None
    left: Expr | None = None
    right: Expr | None = None
    args: tuple[Expr, ...] = ()
    name: str | None = None


@dataclass(frozen=True)
class TypeRef:
    name: str
    args: tuple[Expr, ...] = ()
    transforms: tuple[str, ...] = ()  # e.g. utf8, fixed(1/32) stored as str


@dataclass(frozen=True)
class MatchArm:
    tag: int | str | None  # None = default arm '_'
    type_ref: TypeRef | BlockType | None


@dataclass(frozen=True)
class BlockType:
    items: tuple[BlockItem, ...]


@dataclass(frozen=True)
class Repeat:
    mode: str  # count | until | until_end
    expr: Expr | None = None


@dataclass(frozen=True)
class WireField:
    name: str
    type_ref: TypeRef | BlockType | MatchType
    if_expr: Expr | None = None
    repeat: Repeat | None = None


@dataclass(frozen=True)
class MatchType:
    tag_expr: Expr
    arms: tuple[MatchArm, ...]


@dataclass(frozen=True)
class BlockItem:
    kind: str  # field | computed | require | spread | group
    wire: WireField | None = None
    computed_name: str | None = None
    computed_expr: Expr | None = None
    require_expr: Expr | None = None
    spread_name: str | None = None
    spread_args: tuple[Expr, ...] = ()
    group_if: Expr | None = None
    group_items: tuple["BlockItem", ...] = ()
    _param_env: dict | None = None


@dataclass(frozen=True)
class EncodingDecl:
    name: str
    params: tuple[Param, ...]
    body: TypeRef | BlockType


@dataclass(frozen=True)
class EnumMember:
    name: str
    value: int


@dataclass(frozen=True)
class EnumDecl:
    name: str
    base: str
    members: tuple[EnumMember, ...]


@dataclass(frozen=True)
class PacketDecl:
    name: str
    items: tuple[BlockItem, ...]


@dataclass(frozen=True)
class FrameDecl:
    name: str
    items: tuple[BlockItem, ...]


@dataclass(frozen=True)
class RouteDecl:
    state: str
    direction: str
    packet_id: int
    packet_name: str
    frame: str | None = None


@dataclass(frozen=True)
class ProtocolDecl:
    name: str
    version: str
    protocol_id: int
    frames: tuple[FrameDecl, ...]
    routes: tuple[RouteDecl, ...]


@dataclass(frozen=True)
class Schema:
    encodings: tuple[EncodingDecl, ...] = ()
    enums: tuple[EnumDecl, ...] = ()
    packets: tuple[PacketDecl, ...] = ()
    protocols: tuple[ProtocolDecl, ...] = ()


@dataclass
class ResolvedSchema:
    schema: Schema
    encodings: dict[str, EncodingDecl]
    enums: dict[str, EnumDecl]
    packets: dict[str, PacketDecl]
    protocols: dict[str, ProtocolDecl]
    routes: list[tuple[RouteDecl, PacketDecl]]

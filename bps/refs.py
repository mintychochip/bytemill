"""Every name a schema's declarations mention.

A declaration names another one in four places: a field's type, a spread
(`...name`), an enum's base type, and a route's packet. `references` walks all
four over a whole schema, so resolution (`analyze`) and import checking (`load`)
cannot disagree about what counts as a use. `type_refs` and `item_refs` walk one
type or one block, for a caller that follows the names itself (for example, to
see what a packet reaches through its encodings).
"""

from __future__ import annotations

from collections.abc import Iterator
from dataclasses import dataclass

from bps import ast


@dataclass(frozen=True)
class Ref:
    kind: str  # "type", "spread" (an encoding), or "packet"
    name: str
    where: str  # the declaration that mentions it, e.g. "packet keep_alive"


def references(schema: ast.Schema) -> Iterator[Ref]:
    for enc in schema.encodings:
        yield from type_refs(enc.body, f"encoding {enc.name}")
    for enum in schema.enums:
        yield Ref("type", enum.base, f"enum {enum.name}")
    for pkt in schema.packets:
        yield from item_refs(pkt.items, f"packet {pkt.name}")
    for proto in schema.protocols:
        for frame in proto.frames:
            yield from item_refs(
                frame.items, f"protocol {proto.name} frame {frame.name}"
            )
        for route in proto.routes:
            where = f"route {route.state}.{route.direction} 0x{route.packet_id:x}"
            yield Ref("packet", route.packet_name, where)


def type_refs(
    t: ast.TypeRef | ast.BlockType | ast.MatchType, where: str
) -> Iterator[Ref]:
    if isinstance(t, ast.TypeRef):
        yield Ref("type", t.name, where)
    elif isinstance(t, ast.BlockType):
        yield from item_refs(t.items, where)
    else:
        for arm in t.arms:
            if arm.type_ref is not None:
                yield from type_refs(arm.type_ref, where)


def item_refs(items: tuple[ast.BlockItem, ...], where: str) -> Iterator[Ref]:
    for item in items:
        if item.kind == "field" and item.wire is not None:
            yield from type_refs(item.wire.type_ref, where)
        elif item.kind == "spread" and item.spread_name:
            yield Ref("spread", item.spread_name, where)
        elif item.kind == "group":
            yield from item_refs(item.group_items, where)
        elif item.kind == "match" and item.match is not None:
            yield from type_refs(item.match, where)

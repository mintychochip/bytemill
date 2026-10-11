"""Minestom classes for BPS packets: which struct, in which module, under which name.

`rstom-packets` names a packet by its Minestom class and files it under the
Minestom package; BPS names it by what vanilla calls it (`add_entity`). The two
meet at the id tables: the class at position N of a state's table is the class
of the route with id N. The tables, and each class's package and module name,
come from rstom's `packets_gen` schema (`packets.json`), the file that crate is
generated from.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, replace
from pathlib import Path

from bps.ast import PacketDecl, ResolvedSchema, RouteDecl
from bps.errors import EmitError

# BPS spells the first state `handshaking`; Minestom's packages and tables say `handshake`.
_PACKAGE = {"handshaking": "handshake"}

# Minestom keeps two wire forms on one class and picks the serializer per id table
# (`ShowDialogPacket`: `SERIALIZER` in play, `INLINE_SERIALIZER` in configuration). A BPS
# packet is one wire form and a Rust struct has one, so such a class is one struct per BPS
# packet: the packet named here keeps the class, and each other one is an extra module in
# the package of the state it routes in, still named `ShowDialogPacket`.
SHARED_CLASSES = {"ShowDialogPacket": "show_dialog"}

PACKAGE_PREFIX = "net.minestom.server.network.packet"


def _module_name(entry: dict) -> str:
    """`SpawnEntityPacket` -> `spawn_entity_packet`, as the generator names the file.

    Two entries carry their fields in the schema and name no source file, so the
    name is derived; where a file is named, the two must agree.
    """
    name = entry["name"]
    stem = re.sub(r"(?<=[a-z0-9])(?=[A-Z])|(?<=[A-Z])(?=[A-Z][a-z])", "_", name).lower()
    if "source" in entry and Path(entry["source"]).stem != stem:
        raise EmitError(f"{name}: module {Path(entry['source']).stem}, not the {stem} its name gives")
    return stem


def package_of(state: str) -> str:
    """The Minestom package (and `rstom-packets` module) of a BPS state."""
    return _PACKAGE.get(state, state)


@dataclass(frozen=True)
class MinestomClass:
    name: str  # `SpawnEntityPacket`
    java: str  # the fully qualified name, which is the struct's `CLASS_NAME`
    group: str  # `client` (the client sends it: serverbound) or `server`
    package: str  # `common`, `configuration`, `handshake`, `login`, `play` or `status`
    stem: str  # the module's file name: `spawn_entity_packet`


class ClassTable:
    """The classes of `packets_gen`'s schema, and the id tables that order them."""

    def __init__(self, schema: dict) -> None:
        self._classes = {
            entry["name"]: MinestomClass(
                name=entry["name"],
                java=entry["java"],
                group=entry["group"],
                package=entry["state"],
                stem=_module_name(entry),
            )
            for entry in schema["packets"]
        }
        self._tables = {(t["state"], t["side"]): list(t["packets"] or []) for t in schema["stateTables"]}
        for (state, side), names in self._tables.items():
            for name in names:
                if name not in self._classes:
                    raise EmitError(f"{state} {side} table lists {name}, which the schema does not declare")

    @classmethod
    def load(cls, path: Path) -> ClassTable:
        return cls(json.loads(Path(path).read_text(encoding="utf-8")))

    def at(self, state: str, direction: str, packet_id: int) -> MinestomClass | None:
        """The class Minestom registers at an id, or None past the end of that table."""
        side = "CLIENT" if direction == "serverbound" else "SERVER"
        table = self._tables.get((package_of(state), side), [])
        return self._classes[table[packet_id]] if 0 <= packet_id < len(table) else None

    def named(self, name: str) -> MinestomClass | None:
        return self._classes.get(name)


@dataclass(frozen=True)
class Unit:
    """One generated struct: a BPS packet as the Minestom class of one direction."""

    pkt: PacketDecl
    direction: str
    cls: MinestomClass
    routes: tuple[RouteDecl, ...]


def _class_name(packet: str) -> str:
    return "".join(p[:1].upper() + p[1:] for p in re.split(r"[^a-zA-Z0-9]+", packet) if p) + "Packet"


def build_units(resolved: ResolvedSchema, classes: ClassTable) -> list[Unit]:
    """A struct for each direction each packet travels, ordered by module path."""
    found: dict[tuple[str, str], MinestomClass] = {}
    routed: dict[tuple[str, str], list[RouteDecl]] = {}
    for route, pkt in resolved.routes:
        # A route past its table's end is one Minestom does not register (the pre-1.7
        # ping); the class can only be found by the packet's own name.
        cls = classes.at(route.state, route.direction, route.packet_id) or classes.named(
            _class_name(pkt.name)
        )
        if cls is None:
            raise EmitError(
                f"route {route.state}.{route.direction} {route.packet_id:#04x} => {pkt.name}: "
                f"no Minestom class at that id, and none named {_class_name(pkt.name)}"
            )
        key = (pkt.name, route.direction)
        if found.setdefault(key, cls) != cls:
            raise EmitError(
                f"packet {pkt.name!r} is routed {route.direction} to two Minestom classes: "
                f"{found[key].name}, {cls.name}"
            )
        routed.setdefault(key, []).append(route)
    for name in resolved.packets:
        if all(key[0] != name for key in found):
            raise EmitError(f"packet {name!r} has no route, so no Minestom class")

    claims: dict[tuple[str, str], list[str]] = {}
    for (name, _direction), cls in found.items():
        claim = claims.setdefault((cls.group, cls.name), [])
        if name not in claim:
            claim.append(name)
    for (_group, class_name), packets in claims.items():
        if len(packets) > 1 and SHARED_CLASSES.get(class_name) not in packets:
            raise EmitError(
                f"class {class_name} is claimed by {' and '.join(repr(p) for p in packets)}; "
                "name the one that keeps it in SHARED_CLASSES"
            )

    units = []
    for (name, direction), cls in found.items():
        routes = tuple(routed[(name, direction)])
        if len(claims[(cls.group, cls.name)]) > 1 and SHARED_CLASSES[cls.name] != name:
            cls = _extra_form(cls, name, routes)
        units.append(Unit(resolved.packets[name], direction, cls, routes))
    units.sort(key=lambda u: (u.cls.group, u.cls.package, u.cls.stem))
    for a, b in zip(units, units[1:]):
        if (a.cls.group, a.cls.package, a.cls.stem) == (b.cls.group, b.cls.package, b.cls.stem):
            raise EmitError(f"{a.pkt.name!r} and {b.pkt.name!r} are both {a.cls.java}")
    return units


def _extra_form(cls: MinestomClass, packet: str, routes: tuple[RouteDecl, ...]) -> MinestomClass:
    """The class an extra wire form of a shared class becomes: same name, its own state's package."""
    states = {package_of(r.state) for r in routes}
    if len(states) != 1:
        raise EmitError(f"{packet!r} shares {cls.name} but routes in {sorted(states)}, not one state")
    (package,) = states
    return replace(cls, package=package, java=f"{PACKAGE_PREFIX}.{cls.group}.{package}.{cls.name}")

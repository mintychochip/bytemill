"""rstom's Minestom class table, as the emitter tests need it."""

from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

from bps.minestom import ClassTable

def _default_rstom() -> Path:
    env = os.environ.get("BPS_RSTOM_DIR")
    if env:
        return Path(env)
    protoc = Path(__file__).resolve().parents[1]
    worktree = protoc.parent / "rstom-bps"
    if worktree.is_dir():
        return worktree
    return protoc.parent / "rstom"


RSTOM = _default_rstom()
_shm = Path("/dev/shm")
_default_cargo_target = (
    _shm / "rstom-bps-target" if _shm.is_dir() else Path(tempfile.gettempdir()) / "bps-rstom-target"
)
CARGO_TARGET = Path(os.environ.get("BPS_CARGO_TARGET_DIR") or _default_cargo_target)
PACKETS_GEN_SCHEMA = RSTOM / "tools" / "packets_gen" / "schema" / "packets.json"
RSTOM_PACKETS = RSTOM / "crates" / "rstom-packets"


def real_table() -> ClassTable:
    """The table `rstom-packets` is generated from; the test is skipped without rstom."""
    if not PACKETS_GEN_SCHEMA.is_file():
        raise unittest.SkipTest("rstom packets_gen schema not present")
    return ClassTable.load(PACKETS_GEN_SCHEMA)


def class_entry(group: str, package: str, name: str) -> dict:
    return {
        "name": name,
        "java": f"net.minestom.server.network.packet.{group}.{package}.{name}",
        "dir": f"{group}/{package}",
        "group": group,
        "state": package,
        "fields": [],
    }


def small_table(*tables: tuple[str, str, list[str]]) -> ClassTable:
    """A table of `(state, side, [class names by id])`, every class a `server/play` one.

    For the tests that only map routes to classes.
    """
    names = dict.fromkeys(name for _, _, packets in tables for name in packets)
    return ClassTable(
        {
            "packets": [class_entry("server", "play", name) for name in names],
            "stateTables": [
                {"state": state, "side": side, "packets": packets}
                for state, side, packets in tables
            ],
        }
    )


# The smallest protocol the emitter accepts: the seven packets that move a connection
# between states (the generated state machine matches on them), and one more to test with.
# (BPS packet, route, Minestom group, package, class).
_PROTOCOL = (
    ("handshake", "handshaking.serverbound 0x00", "client", "handshake", "ClientHandshakePacket"),
    ("login_acknowledged", "login.serverbound 0x00", "client", "login", "ClientLoginAcknowledgedPacket"),
    ("configuration_acknowledged", "play.serverbound 0x00", "client", "play", "ClientConfigurationAckPacket"),
    ("finish_configuration", "configuration.serverbound 0x00", "client", "configuration", "ClientFinishConfigurationPacket"),
    ("finish_configuration", "configuration.clientbound 0x00", "server", "configuration", "FinishConfigurationPacket"),
    ("login_finished", "login.clientbound 0x00", "server", "login", "LoginSuccessPacket"),
    ("start_configuration", "play.clientbound 0x00", "server", "play", "StartConfigurationPacket"),
    ("probe", "play.clientbound 0x01", "server", "play", "ProbePacket"),
)


def _table_data() -> dict:
    tables: dict[tuple[str, str], list[str]] = {}
    for _packet, route, group, _package, name in _PROTOCOL:
        state = route.split(".")[0].replace("handshaking", "handshake")
        tables.setdefault((state, "CLIENT" if group == "client" else "SERVER"), []).append(name)
    return {
        "packets": [class_entry(group, package, name) for _, _, group, package, name in _PROTOCOL],
        "stateTables": [
            {"state": state, "side": side, "packets": names}
            for (state, side), names in tables.items()
        ],
    }


PROBE_SCHEMA = _table_data()
PROBE_TABLE = ClassTable(PROBE_SCHEMA)


def probe_source(root: Path, body: str = "packet probe { a: u8 }") -> str:
    """A minimal protocol whose `probe` packet is `body`, importing the std scalars."""
    std = "".join(f'import "{root / "schemas" / "std" / f"{n}.bps"}"\n' for n in ("integers", "varint"))
    others = "\n".join(
        f"packet {name} {{ {'intent: varint' if name == 'handshake' else 'a: u8'} }}"
        for name in dict.fromkeys(p for p, *_ in _PROTOCOL if p != "probe")
    )
    routes = "\n".join(f"    route {route} => {packet}" for packet, route, *_ in _PROTOCOL)
    return (
        f"{std}\n{body}\n{others}\n"
        "protocol minecraft @ v1 {\n"
        "    id = 1\n"
        "    frame default {\n        length: varint\n        id: varint\n        payload: rest\n    }\n"
        f"{routes}\n"
        "}\n"
    )

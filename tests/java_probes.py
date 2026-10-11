"""Java ground truth for packet layouts: what real Minestom writes.

rstom's `probes/expected/*.json` are recorded from the Java server. Two kinds of
fact are used here, and neither comes from BPS or from the wiki: the registry
tables (which class sits at which id, per state and side) and the serialized
bytes of sample packets.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

from bps import ast
from bps.interp.engine import decode_packet, encode_packet

PROBES = Path(__file__).resolve().parents[2] / "rstom" / "probes" / "expected"

STATE = {
    "HANDSHAKE": "handshaking",
    "STATUS": "status",
    "LOGIN": "login",
    "CONFIGURATION": "configuration",
    "PLAY": "play",
}
# Minestom names a packet by who sends it: a CLIENT packet is serverbound.
DIRECTION = {"CLIENT": "serverbound", "SERVER": "clientbound"}

# Fixtures whose cases are `{"kind", "value", "bytes"}`: a packet and the bytes Java wrote.
KIND_FIXTURES = (
    "serverplaybpacketprobe",
    "serverplayspacketprobe",
    "serverplayscoreprobe",
    "serverplayworldprobe",
    "serveradvancementprobe",
)

# `kind` names that are not the class name minus "Packet" with a capital first letter.
KIND_CLASS = {"selectTab": "SelectAdvancementTabPacket"}

# Classes the registry tables do not list, and the BPS packet that carries each.
UNREGISTERED = {"LegacyServerListPingPacket": "legacy_server_list_ping"}


@dataclass(frozen=True)
class Slot:
    state: str
    direction: str
    packet_id: int
    java_class: str


@dataclass(frozen=True)
class Sample:
    fixture: str
    index: int
    java_class: str
    data: bytes
    value: object = None  # the field values Java built the packet from, when recorded

    @property
    def label(self) -> str:
        return f"{self.fixture}[{self.index}]"


@dataclass(frozen=True)
class Failure:
    sample: Sample
    reason: str


def available() -> bool:
    return PROBES.is_dir()


def _cases(fixture: str) -> list[dict]:
    return json.loads((PROBES / f"{fixture}.json").read_text(encoding="utf-8"))["cases"]


def slots() -> list[Slot]:
    """Every `(state, direction, id)` Java registers, with the class at that id."""
    out = []
    for case in _cases("packetprobe"):
        if case["op"] != "registryTable":
            continue
        for packet_id, java_class in enumerate(case["packets"]):
            out.append(
                Slot(STATE[case["state"]], DIRECTION[case["side"]], packet_id, java_class)
            )
    return out


def samples() -> list[Sample]:
    """The recorded packet bytes. Cases where Java itself rejected the input are skipped."""
    out = []
    for index, case in enumerate(_cases("corepacketsprobe")):
        if case.get("write") is None:
            continue
        java_class = case["name"].rsplit(".", 1)[-1]
        out.append(Sample("corepacketsprobe", index, java_class, bytes.fromhex(case["write"])))
    for index, case in enumerate(_cases("playernetworkprobe")):
        if case.get("op") == "clientSettings" and case.get("hex"):
            out.append(
                Sample(
                    "playernetworkprobe",
                    index,
                    "ClientSettingsPacket",
                    bytes.fromhex(case["hex"]),
                    case.get("input"),
                )
            )
    for fixture in KIND_FIXTURES:
        for index, case in enumerate(_cases(fixture)):
            if case.get("bytes") is None or "errorClass" in case:
                continue
            value = case.get("value")
            java_class = (
                (value.get("packetClass") if isinstance(value, dict) else None)
                or KIND_CLASS.get(case["kind"])
                or case["kind"][0].upper() + case["kind"][1:] + "Packet"
            )
            out.append(Sample(fixture, index, java_class, bytes.fromhex(case["bytes"]), value))
    return out


def route_packets(resolved: ast.ResolvedSchema) -> dict[tuple[str, str, int], str]:
    return {(r.state, r.direction, r.packet_id): pkt.name for r, pkt in resolved.routes}


def packets_by_class(resolved: ast.ResolvedSchema) -> dict[str, list[str]]:
    """The BPS packet(s) at the ids where Java registers each class."""
    route = route_packets(resolved)
    out: dict[str, set[str]] = {}
    for slot in slots():
        packet = route.get((slot.state, slot.direction, slot.packet_id))
        if packet is not None:
            out.setdefault(slot.java_class, set()).add(packet)
    for java_class, packet in UNREGISTERED.items():
        out.setdefault(java_class, set()).add(packet)
    return {java_class: sorted(packets) for java_class, packets in out.items()}


def check(resolved: ast.ResolvedSchema, packet: str, data: bytes) -> str | None:
    """Why the schema cannot reproduce `data` for `packet`, or None when it does.

    The decode is strict, so a layout that stops short fails; the re-encode must
    give the same bytes back, so one that reads them as the wrong shape fails too.
    """
    try:
        value = decode_packet(resolved, packet, data, strict=True)
    except Exception as exc:  # any failure to read the bytes is the finding
        return f"decode: {exc}"
    try:
        again = encode_packet(resolved, packet, value)
    except Exception as exc:
        return f"re-encode: {type(exc).__name__}: {exc}"
    if again != data:
        return f"re-encode differs: {again.hex()}"
    return None


def failures(resolved: ast.ResolvedSchema) -> dict[str, list[Failure]]:
    """Samples the schema cannot reproduce, by BPS packet."""
    by_class = packets_by_class(resolved)
    out: dict[str, list[Failure]] = {}
    for sample in samples():
        for packet in by_class.get(sample.java_class, []):
            reason = check(resolved, packet, sample.data)
            if reason is not None:
                out.setdefault(packet, []).append(Failure(sample, reason))
    return out

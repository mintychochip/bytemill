#!/usr/bin/env python3
"""Scrape minecraft.wiki packet ID tables for Java Edition protocol/Packets page."""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

WIKI_URL = "https://minecraft.wiki/w/Java_Edition_protocol/Packets"

STATE_MAP = {
    "Handshaking": "handshaking",
    "Status": "status",
    "Login": "login",
    "Configuration": "configuration",
    "Play": "play",
}

# Official Mojang names -> hand-maintained BPS packet names (richer field layouts).
PACKET_ALIASES: dict[str, str] = {
    "intention": "handshake",
}

# Packets defined with full layouts in minecraft_26_3.bps (no stub generated).
MANUAL_PACKETS = frozenset(
    {
        "handshake",
        "legacy_server_list_ping",
        "status_request",
        "status_response",
        "ping_request",
        "pong_response",
        "login_start",
        "encryption_response",
        "keep_alive",
    }
)


@dataclass(frozen=True)
class RouteRow:
    state: str
    direction: str
    packet_id: int
    official_name: str


def fetch_html() -> str:
    return subprocess.check_output(["curl", "-sL", WIKI_URL], text=True)


def parse_tables(html: str) -> list[RouteRow]:
    rows: list[RouteRow] = []
    header_re = re.compile(
        r'<th colspan="4">(\w+) (clientbound|serverbound) packets',
        re.IGNORECASE,
    )
    tr_re = re.compile(r"<tr[^>]*>(.*?)</tr>", re.IGNORECASE | re.DOTALL)
    row_re = re.compile(
        r"<td>(\d+)</td>\s*<td>0x([0-9A-Fa-f]+)\s*</td>.*?<td><code>([^<]+)</code>",
        re.IGNORECASE | re.DOTALL,
    )
    pos = 0
    while True:
        m = header_re.search(html, pos)
        if not m:
            break
        state = STATE_MAP.get(m.group(1), m.group(1).lower())
        direction = m.group(2).lower()
        start = m.end()
        next_m = header_re.search(html, start)
        end = next_m.start() if next_m else len(html)
        chunk = html[start:end]
        for tr in tr_re.finditer(chunk):
            rm = row_re.search(tr.group(1))
            if not rm:
                continue
            pid = int(rm.group(1))
            official = rm.group(3).strip()
            rows.append(RouteRow(state, direction, pid, official))
        pos = end
    return rows


# Routes whose official name needs a different BPS packet: a serverbound packet that
# collides with a clientbound one (the hand-transcribed declarations use a
# `_serverbound` suffix), and packets whose wire form differs between states.
SERVERBOUND_ALIASES: dict[tuple[str, str, str], str] = {
    ("play", "serverbound", "change_difficulty"): "change_difficulty_serverbound",
    ("play", "serverbound", "container_close"): "container_close_serverbound",
    ("play", "serverbound", "move_vehicle"): "move_vehicle_serverbound",
    ("play", "serverbound", "player_abilities"): "player_abilities_serverbound",
    # Vanilla's configuration codec writes the dialog without the holder id play uses.
    ("configuration", "clientbound", "show_dialog"): "show_dialog_configuration",
}


def packet_name(official: str, state: str, direction: str, pid: int) -> str:
    key = (state, direction, official)
    if key in SERVERBOUND_ALIASES:
        return SERVERBOUND_ALIASES[key]
    if key == ("login", "serverbound", "hello"):
        return "login_start"
    if key == ("login", "serverbound", "key"):
        return "encryption_response"
    if key == ("login", "clientbound", "hello"):
        return "hello"
    if key == ("handshaking", "serverbound", "intention"):
        return "handshake"
    if official in PACKET_ALIASES:
        return PACKET_ALIASES[official]
    name = official.replace("-", "_")
    if not name or name[0].isdigit():
        return f"pkt_{state}_{direction}_{pid:02x}"
    return name


def emit_bps(
    rows: list[RouteRow],
    extra_routes: list[str],
    manual: set[str] = frozenset(),
    imports: tuple[str, ...] | list[str] = (),
) -> str:
    """The route file. `imports` are paths relative to the output file: the file
    declaring `varint` (the frame uses it) and each file declaring packets that
    routes bind to. A name the output uses without importing it will not load."""
    needed_packets: dict[str, None] = {}
    routes: list[str] = []
    for r in rows:
        pname = packet_name(r.official_name, r.state, r.direction, r.packet_id)
        needed_packets[pname] = None
        routes.append(f"    route {r.state}.{r.direction} 0x{r.packet_id:02x} => {pname}")
    routes.extend(extra_routes)

    lines = [
        "// Packet stubs + routes transcribed from minecraft.wiki (protocol 777 / 26.3).",
        "// Regenerate: python scripts/scrape_wiki_routes.py -o schemas/minecraft_26_3_wiki.bps",
        "",
    ]
    if imports:
        lines.extend(f'import "{path}"' for path in imports)
        lines.append("")
    for pname in sorted(needed_packets):
        if pname in manual or pname in MANUAL_PACKETS:
            continue
        lines.append(f"packet {pname} {{")
        lines.append("    body: rest")
        lines.append("}\n")
    lines.append("protocol minecraft @ v26_3 {")
    lines.append("    id = 777")
    lines.append("    frame default {")
    lines.append("        length: varint")
    lines.append("        id: varint")
    lines.append("        payload: rest")
    lines.append("    }\n")
    lines.extend(sorted(set(routes), key=lambda s: s))
    lines.append("}")
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("-o", "--out", type=Path, help="write .bps fragment")
    ap.add_argument(
        "--manual-replace",
        type=Path,
        nargs="*",
        default=[],
        help="schema files whose packet declarations replace generated stubs",
    )
    args = ap.parse_args()
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    html = fetch_html()
    rows = parse_tables(html)
    if not rows:
        print("no routes parsed", file=sys.stderr)
        return 1
    manual: set[str] = set()
    packet_files: list[Path] = []
    for path in args.manual_replace:
        from bps.parser import parse as _parse

        declared = _parse(path.read_text(encoding="utf-8")).packets
        manual.update(pkt.name for pkt in declared)
        if declared:
            packet_files.append(path)
    extra = ["    route handshaking.serverbound 0xfe => legacy_server_list_ping"]
    out_dir = (args.out.parent if args.out else Path.cwd()).resolve()
    std_varint = Path(__file__).resolve().parents[1] / "schemas" / "std" / "varint.bps"
    imports = [
        Path(os.path.relpath(path.resolve(), out_dir)).as_posix()
        for path in [*packet_files, std_varint]
    ]
    text = emit_bps(rows, extra, manual, imports)
    print(f"parsed {len(rows)} wiki routes", file=sys.stderr)
    if args.out:
        args.out.write_text(text, encoding="utf-8")
        print(f"wrote {args.out}", file=sys.stderr)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

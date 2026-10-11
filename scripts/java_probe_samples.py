#!/usr/bin/env python3
"""Compare BPS packet layouts with the bytes real Minestom writes.

    python scripts/java_probe_samples.py                # packets the schema cannot reproduce
    python scripts/java_probe_samples.py set_objective  # every recorded sample for one packet

The samples come from rstom's `probes/expected/*.json`. For a packet it shows the
bytes Java wrote, the field values Java built it from (when recorded), and either
how the schema decoded them or why it could not.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from bps.analyze import analyze  # noqa: E402
from bps.interp.engine import decode_packet  # noqa: E402
from bps.load import load_schema  # noqa: E402
from tests import java_probes as java  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("packets", nargs="*", help="BPS packet names; none lists the failing ones")
    ap.add_argument(
        "--schema",
        type=Path,
        default=ROOT / "schemas" / "minecraft_26_3_wiki.bps",
        help="root schema file (default: the whole protocol)",
    )
    args = ap.parse_args()
    if not java.available():
        print(f"rstom probe fixtures not found at {java.PROBES}", file=sys.stderr)
        return 2
    resolved = analyze(load_schema([args.schema]))

    if not args.packets:
        failed = java.failures(resolved)
        for packet, items in sorted(failed.items()):
            first = items[0]
            print(f"{packet}: {len(items)} sample(s) differ; {first.sample.label}: {first.reason}")
        print(f"{len(failed)} packet(s) differ")
        return 0

    by_class = java.packets_by_class(resolved)
    shown = 0
    for sample in java.samples():
        for packet in by_class.get(sample.java_class, []):
            if packet not in args.packets:
                continue
            shown += 1
            print(f"{packet} <- {sample.java_class}  {sample.label}  ({len(sample.data)} bytes)")
            print(f"  bytes  {sample.data.hex()}")
            if sample.value is not None:
                print(f"  java   {json.dumps(sample.value)[:1500]}")
            reason = java.check(resolved, packet, sample.data)
            if reason is None:
                print(f"  ok     {decode_packet(resolved, packet, sample.data, strict=True)}")
            else:
                print(f"  FAILS  {reason}")
    if not shown:
        print("no recorded samples for", ", ".join(args.packets), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

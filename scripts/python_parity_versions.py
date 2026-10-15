#!/usr/bin/env python3
"""The Python reference implementation against the Rust one, on every generated protocol schema.

For each entry of schemas/versions/manifest.json:

  1. `bpsc dump-json` and `python -m bps.cli dump-json` must print the same bytes (parse + resolve);
  2. random packets drawn from the schema (the `write_generated_vectors` cargo test) are decoded by
     `bpsc wire-check --dump` (Rust) and by the Python interpreter in this process; the JSON must be equal.

    scripts/python_parity_versions.py [--seeds 6] [--only 1.20.1]

Needs a built `target/debug/bpsc`. Cargo and Python run under the memory cap of scripts/bps_cargo.sh.
"""
import argparse
import atexit
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BPSC = ROOT / "target/debug/bpsc"
sys.path.insert(0, str(ROOT))

from bps.analyze import analyze  # noqa: E402
from bps.interp.engine import decode_packet  # noqa: E402
from bps.load import load_schema  # noqa: E402


def norm(v):
    """Rust prints bytes as arrays of ints (Python's json cannot print bytes at all)."""
    if isinstance(v, (bytes, bytearray)):
        return list(v)
    if isinstance(v, dict):
        return {k: norm(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [norm(x) for x in v]
    return v


def capped(cmd):
    if shutil.which("systemd-run") is None:
        return cmd
    mem = os.environ.get("BPS_MEM_MAX", "8G")
    return ["systemd-run", "--user", "--scope", "-q", "-p", f"MemoryMax={mem}", "-p", "MemorySwapMax=0"] + cmd


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seeds", type=int, default=6, help="random packets per packet type")
    ap.add_argument("--only", help="a release from the manifest")
    args = ap.parse_args()
    entries = [e for e in json.loads((ROOT / "schemas/versions/manifest.json").read_text())["versions"]
               if args.only in (None, e["release"])]
    work = Path(tempfile.mkdtemp(prefix="bps-parity-"))
    atexit.register(shutil.rmtree, work, ignore_errors=True)
    env = dict(os.environ, BPS_VECTORS_OUT=str(work), BPS_SEEDS=str(args.seeds))
    if args.only:
        env["BPS_ONLY"] = args.only
    r = subprocess.run([str(ROOT / "scripts/bps_cargo.sh"), "test", "-p", "bps", "--test", "test_versions", "--",
                        "--ignored", "write_generated_vectors"], capture_output=True, text=True, env=env, cwd=ROOT)
    if r.returncode != 0:
        sys.exit(r.stdout + r.stderr)

    bad = 0
    total = 0
    for e in entries:
        rel = e["release"]
        schema = ROOT / "schemas/versions" / f"v{rel.replace('.', '_')}.bps"
        rs = subprocess.run([str(BPSC), "dump-json", str(schema)], capture_output=True, text=True)
        py = subprocess.run(capped([sys.executable, "-m", "bps.cli", "dump-json", str(schema)]), capture_output=True,
                            text=True, cwd=ROOT)
        same_schema = rs.returncode == py.returncode and rs.stdout == py.stdout

        vectors = work / f"{rel}.jsonl"
        dump = work / f"{rel}.dump.jsonl"
        subprocess.run([str(BPSC), "wire-check", str(schema), "--file", str(vectors), "--dump", str(dump)],
                       capture_output=True, text=True, check=True)
        rust = {}
        for line in dump.read_text().splitlines():
            o = json.loads(line)
            rust[o["line"]] = o["value"]
        resolved = analyze(load_schema([str(schema)]))
        routes = {(r.state, r.direction, r.packet_id): p.name for r, p in resolved.routes}
        differ = 0
        lines = vectors.read_text().splitlines()
        for i, line in enumerate(lines, 1):
            v = json.loads(line)
            name = routes[(v["state"], "clientbound" if v["direction"] == "toClient" else "serverbound", v["id"])]
            try:
                got = norm(decode_packet(resolved, name, bytes.fromhex(v["hex"])))
            except Exception as ex:  # a Python crash is a difference too
                got = f"{type(ex).__name__}: {ex}"
            differ += got != rust.get(i)
        total += len(lines)
        bad += differ + (not same_schema)
        print(f"{rel:9s} dump-json {'identical' if same_schema else 'DIFFERENT':9s} decode: {len(lines)} packets, {differ} differ")
    print(f"{total} packets, {bad} differences")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()

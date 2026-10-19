#!/usr/bin/env python3
"""Check every generated protocol schema against the ProtoDef oracle (tools/protodef-oracle).

Both directions, for every entry of schemas/versions/manifest.json:

  JS -> BPS   the oracle compiles the version's protocol.json (with the manifest's patches) with
              ProtoDef and generates random packets; `bml wire-check` must decode each strictly
              and encode it back to the same bytes.
  BPS -> JS   bytes drawn from the generated schema itself (the `write_generated_vectors` test)
              must decode in ProtoDef with every byte consumed and re-serialise identically.

    scripts/verify_versions.py [--count 20] [--seeds 20] [--only 1.20.1] [--keep DIR]
    scripts/verify_versions.py --pin     # rewrite crates/bytemill/tests/fixtures/protodef_vectors/

`--pin` writes one oracle-generated packet per packet type and version (seed 1) as JSON Lines. The
cargo test `oracle_vectors_round_trip` decodes and re-encodes them with no Node needed.

Needs `npm ci` in tools/protodef-oracle and a built `target/debug/bml` (cargo build -p bytemill). Node and
cargo run under the same memory cap as scripts/bytemill_cargo.sh (BPS_MEM_MAX, default 8G).
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
ORACLE = ROOT / "tools/protodef-oracle/oracle.js"
DATA = ROOT / "data/minecraft-data/pc"
if not DATA.is_dir():
    sys.exit("minecraft-data not fetched; run scripts/fetch_minecraft_data.py")
BPSC = ROOT / "target/debug/bml"

# BPS -> JS: random bytes drawn from a BPS schema are valid wire bytes, but ProtoDef's JS value model
# is lossy for a few shapes, so JS cannot always write back what it read. A packet with one of these
# shapes is still required to DECODE in ProtoDef with every byte consumed; only a difference in the JS
# re-serialisation is put down to the oracle, and only for the causes below (each was confirmed by
# reading the vectors). Anything else is a failure.
LPVEC3_PACKETS = {"entity_velocity", "spawn_entity", "use_entity"}
VOID_OPTION_PACKETS = {"debug_block_value", "debug_chunk_value", "debug_entity_value"}


def js_lossy_cause(f):
    reason, detail, name = f.get("reason"), f.get("detail", ""), f.get("name")
    if reason == "reserialise" and "out of range" in detail:
        return "u32 bitflags with bit 31 set overflow ProtoDef's `1 << 31`"
    if reason == "reserialise" and "registryEntryHolder" in detail:
        return "a registryEntryHolder whose inline value is falsy (0, empty string) cannot be written by ProtoDef"
    if reason == "mismatch" and name in LPVEC3_PACKETS:
        return "lpVec3 bytes that are not a canonical quantisation are rewritten by JS"
    if reason == "mismatch" and name in VOID_OPTION_PACKETS:
        return "an option flag followed by a void payload reads back as absent in JS"
    return None


def capped(cmd):
    if shutil.which("systemd-run") is None:
        return cmd
    mem = os.environ.get("BPS_MEM_MAX", "8G")
    return ["systemd-run", "--user", "--scope", "-q", "-p", f"MemoryMax={mem}", "-p", "MemorySwapMax=0"] + cmd


def run(cmd, **kw):
    return subprocess.run(capped(cmd), capture_output=True, text=True, **kw)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--count", type=int, default=20, help="JS-generated packets per packet type")
    ap.add_argument("--seeds", type=int, default=20, help="BPS-generated packets per packet type")
    ap.add_argument("--only", help="a release from the manifest, e.g. 1.20.1")
    ap.add_argument("--keep", help="write vectors here instead of a temporary directory")
    ap.add_argument("--pin", action="store_true", help="refresh the committed oracle vectors and stop")
    args = ap.parse_args()

    manifest = json.loads((ROOT / "schemas/versions/manifest.json").read_text())["versions"]
    entries = [e for e in manifest if args.only in (None, e["release"])]
    if not entries:
        sys.exit(f"no manifest entry for {args.only!r}")
    if args.pin:
        pin_dir = ROOT / "crates/bytemill/tests/fixtures/protodef_vectors"
        pin_dir.mkdir(parents=True, exist_ok=True)
        for e in entries:
            proto = ROOT / "data/minecraft-data" / e["data"] / "protocol.json"
            patches = ["--patches", str(ROOT / "schemas/versions" / e["patches"])] if e.get("patches") else []
            g = run(["node", str(ORACLE), "gen", "--protocol", str(proto), *patches, "--count", "1", "--seed", "1"])
            if g.returncode != 0:
                sys.exit(f"oracle gen failed for {e['release']}: {g.stderr.strip()[:300]}")
            (pin_dir / f"{e['release']}.jsonl").write_text(g.stdout)
            print(f"{e['release']}: {len(g.stdout.splitlines())} vectors")
        return
    if args.keep:
        work = Path(args.keep)
    else:
        work = Path(tempfile.mkdtemp(prefix="bps-verify-"))
        atexit.register(shutil.rmtree, work, ignore_errors=True)
    (work / "bps").mkdir(parents=True, exist_ok=True)

    # BPS -> JS vectors, drawn once for all versions by the cargo test.
    env = dict(os.environ, BPS_VECTORS_OUT=str(work / "bps"), BPS_SEEDS=str(args.seeds))
    if args.only:
        env["BPS_ONLY"] = args.only
    r = subprocess.run(
        [str(ROOT / "scripts/bytemill_cargo.sh"), "test", "-p", "bps", "--test", "test_versions", "--",
         "--ignored", "write_generated_vectors"], capture_output=True, text=True, env=env, cwd=ROOT)
    if r.returncode != 0:
        sys.exit(r.stdout + r.stderr)

    failed = 0
    causes = {}
    print(f"{'release':9s} {'protocol':8s} {'JS->BPS':>22s}   {'BPS->JS':>34s}")
    for e in entries:
        rel = e["release"]
        schema = ROOT / "schemas/versions" / f"v{rel.replace('.', '_')}.bps"
        proto = ROOT / "data/minecraft-data" / e["data"] / "protocol.json"
        patches = ["--patches", str(ROOT / "schemas/versions" / e["patches"])] if e.get("patches") else []

        js_out = work / f"{rel}.js.jsonl"
        g = run(["node", str(ORACLE), "gen", "--protocol", str(proto), *patches, "--count", str(args.count)])
        if g.returncode != 0:
            print(f"{rel:9s} {e['protocol']:<8d} oracle gen failed: {g.stderr.strip()[:200]}")
            failed += 1
            continue
        js_out.write_text(g.stdout)
        w = subprocess.run([str(BPSC), "wire-check", str(schema), "--file", str(js_out)], capture_output=True, text=True)
        a = w.stdout.strip() or w.stderr.strip()[:60]

        c = run(["node", str(ORACLE), "check", "--protocol", str(proto), *patches, "--file", str(work / "bps" / f"{rel}.jsonl")])
        total = sum(1 for _ in open(work / "bps" / f"{rel}.jsonl"))
        lossy, real = 0, []
        for line in c.stdout.splitlines():
            f = json.loads(line)
            cause = js_lossy_cause(f)
            if cause:
                lossy += 1
                causes[cause] = causes.get(cause, 0) + 1
            else:
                real.append(line)
        b = f"checked {total}, js-lossy {lossy}, failed {len(real)}"
        bad = w.returncode != 0 or bool(real) or (c.returncode not in (0, 1))
        failed += bad
        print(f"{rel:9s} {e['protocol']:<8d} {a:>22s}   {b:>34s}{'   <-- FAIL' if bad else ''}")
        if bad:
            for line in w.stderr.strip().splitlines()[:3]:
                print("    JS->BPS:", line[:180])
            for line in real[:3]:
                print("    BPS->JS:", line[:180])
    for cause, n in sorted(causes.items(), key=lambda kv: -kv[1]):
        print(f"js-lossy x{n}: {cause}")
    print(f"vectors kept in {work}" if args.keep else "")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()

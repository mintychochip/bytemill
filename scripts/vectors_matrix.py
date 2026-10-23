#!/usr/bin/env python3
"""Run every typed printer's generated vectors test on schemas, per target and standard.

For each (schema, target spec) it runs

    bml emit <target[:standard]> <schema> --out <dir> --vectors

and executes the command the CLI prints after "run:" (the printer's native test). A pair
passes when that command exits 0. The toolchains must be on PATH already (go, python3, java,
dotnet, cc, g++, node, zig, cargo); a missing one shows up as a failure with its error line.

    scripts/vectors_matrix.py                         # every version root, nine targets
    scripts/vectors_matrix.py --roots v1_13 v26_3 --targets go cpp
    scripts/vectors_matrix.py --schemas schemas/hytale/2026_02_17.bml --standards extremes
    scripts/vectors_matrix.py --json /tmp/matrix.json --jobs 4

--standards picks which language standards each target is emitted at, read from `bml targets`:
`default` (the target's default), `extremes` (its oldest and newest) or `all`.

Exit status is 0 when every pair passes, 1 otherwise. Output directories of failing pairs are
kept (under --scratch) so the failure can be reproduced; passing ones are removed.
"""

import argparse
import concurrent.futures
import json
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

TARGETS = ["rust-typed", "go", "python", "java", "csharp", "c", "cpp", "typescript", "zig"]
REPO = Path(__file__).resolve().parent.parent
ERROR_LINE = re.compile(r"error|fail|panic|mismatch|missing|exception|cannot|undefined", re.I)


def excerpt(text: str, width: int = 200) -> str:
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    hits = [line for line in lines if ERROR_LINE.search(line)]
    chosen = hits[:2] if hits else lines[-1:]
    return " | ".join(chosen)[:width]


def target_specs(bml: str, targets: list[str], standards: str) -> list[str]:
    """`target` or `target:standard` strings, in the order `bml targets` lists the standards."""
    info = {t["target"]: t for t in json.loads(subprocess.run(
        [bml, "targets"], capture_output=True, text=True, check=True).stdout)}
    specs = []
    for target in targets:
        versions = info[target]["versions"]
        if standards == "default":
            specs.append(target)
        elif standards == "extremes":
            specs += [f"{target}:{v}" for v in dict.fromkeys([versions[0], versions[-1]])]
        else:
            specs += [f"{target}:{v}" for v in versions]
    return specs


def run_pair(bml: str, schema: Path, label: str, spec: str, scratch: Path, count: int | None,
             timeout: int) -> dict:
    out = scratch / re.sub(r"[^A-Za-z0-9_.-]+", "_", f"{spec}_{label}")
    shutil.rmtree(out, ignore_errors=True)
    started = time.time()
    result = {"root": label, "target": spec, "status": "", "vectors": 0, "detail": ""}
    cmd = [bml, "emit", spec, str(schema), "--out", str(out), "--vectors"]
    if count is not None:
        cmd += ["--vectors-count", str(count)]
    emitted = subprocess.run(cmd, capture_output=True, text=True)
    if emitted.returncode != 0:
        result.update(status="EMIT-FAIL", detail=excerpt(emitted.stderr or emitted.stdout))
    else:
        run = re.search(r"\(([0-9]+) vectors\); run: (.*)$", emitted.stdout, re.M)
        if not run:
            result.update(status="NO-TEST", detail=excerpt(emitted.stdout))
        else:
            result["vectors"] = int(run.group(1))
            try:
                ran = subprocess.run(["bash", "-c", run.group(2)], capture_output=True,
                                     text=True, timeout=timeout)
                if ran.returncode == 0:
                    result["status"] = "PASS"
                else:
                    result.update(status="FAIL", detail=excerpt(ran.stdout + "\n" + ran.stderr))
            except subprocess.TimeoutExpired:
                result.update(status="TIMEOUT", detail=f"no result after {timeout}s")
    result["secs"] = round(time.time() - started, 1)
    if result["status"] == "PASS":
        shutil.rmtree(out, ignore_errors=True)
    return result


def version_key(name: str) -> list[int]:
    return [int(x) for x in re.findall(r"\d+", name)]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--bml", default=str(REPO / "target/debug/bml"))
    parser.add_argument("--roots", nargs="*", help="version names such as v1_20_6 (default: all)")
    parser.add_argument("--schemas", nargs="*", help="schema files to run instead of the roots")
    parser.add_argument("--targets", nargs="*", default=TARGETS, choices=TARGETS)
    parser.add_argument("--standards", choices=["default", "extremes", "all"], default="default")
    parser.add_argument("--jobs", type=int, default=3)
    parser.add_argument("--count", type=int, help="vectors per packet (default: the CLI's)")
    parser.add_argument("--timeout", type=int, default=900, help="seconds per native test run")
    parser.add_argument("--scratch", default=str(Path.home() / ".cache/bytemill-scratch/matrix"))
    parser.add_argument("--json", help="write every result to this file")
    args = parser.parse_args()

    if args.schemas:
        schemas = [(Path(s).resolve(), Path(s).stem) for s in args.schemas]
    else:
        roots = sorted((REPO / "schemas/versions").glob("v*.bml"), key=lambda p: version_key(p.stem))
        if args.roots:
            wanted = set(args.roots)
            roots = [r for r in roots if r.stem in wanted]
            missing = wanted - {r.stem for r in roots}
            if missing:
                print(f"unknown roots: {sorted(missing)}", file=sys.stderr)
                return 2
        schemas = [(r, r.stem) for r in roots]
    specs = target_specs(args.bml, args.targets, args.standards)
    scratch = Path(args.scratch)
    scratch.mkdir(parents=True, exist_ok=True)

    results = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = [pool.submit(run_pair, args.bml, path, label, spec, scratch, args.count,
                               args.timeout)
                   for path, label in schemas for spec in specs]
        for future in concurrent.futures.as_completed(futures):
            r = future.result()
            results.append(r)
            note = f"  {r['detail']}" if r["status"] != "PASS" else ""
            print(f"{r['root']:>8} {r['target']:<16} {r['status']:<9} {r['vectors']:>5} vectors "
                  f"{r['secs']:>6}s{note}", flush=True)

    print("\nschema     " + " ".join(f"{s[:11]:>11}" for s in specs))
    for _, label in schemas:
        cells = []
        for spec in specs:
            r = next(x for x in results if x["root"] == label and x["target"] == spec)
            cells.append("         ok" if r["status"] == "PASS" else "       FAIL")
        print(f"{label:<10} " + " ".join(cells))
    failed = [r for r in results if r["status"] != "PASS"]
    print(f"\n{len(results) - len(failed)}/{len(results)} pairs pass")
    if args.json:
        results.sort(key=lambda r: (version_key(r["root"]), r["target"]))
        Path(args.json).write_text(json.dumps(results, indent=1))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

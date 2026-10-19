#!/usr/bin/env python3
"""Fetch the pinned minecraft-data `protocol.json` files.

    scripts/fetch_minecraft_data.py [--check] [--force] [--root DIR]

Reads `data/minecraft-data/sources.json` (a `files` list with `path`, `repo`,
`commit`, `upstream`, `sha256`) and downloads each file from
`https://raw.githubusercontent.com/{repo}/{commit}/{upstream}` to
`data/minecraft-data/{path}`, verifying the SHA-256 of every file. Files
already present with the right hash are skipped unless `--force` is given.

`--check` verifies only (no network): it exits 1 when anything is missing or
stale. `--root` defaults to the repository root (the script's parent's
parent). A `summary: fetched=N verified=M missing=K` line is always printed.
On an HTTP/network failure the URL is printed and the exit status is 1.
"""
import argparse
import hashlib
import http.client
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path

TIMEOUT = 60  # seconds without bytes before a download is given up


def sha256_of(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--check", action="store_true",
                    help="verify only: exit 1 if anything is missing or stale")
    ap.add_argument("--force", action="store_true",
                    help="re-download even files already present with the right hash")
    ap.add_argument("--root", default=str(Path(__file__).resolve().parent.parent),
                    help="repository root (default: the script's parent's parent)")
    args = ap.parse_args()

    root = Path(args.root)
    try:
        sources = json.loads((root / "data/minecraft-data/sources.json").read_text())
    except OSError as e:
        sys.exit(f"sources.json: {e}")
    files = sources["files"]

    fetched = 0
    verified = 0
    bad = []
    for entry in files:
        dest = root / "data/minecraft-data" / entry["path"]
        want = entry["sha256"]
        if dest.is_file() and not args.force:
            if sha256_of(dest) == want:
                verified += 1
                continue
            detail = "stale (sha256 mismatch)"
        elif dest.is_file():
            detail = "stale (sha256 mismatch)"
        else:
            detail = "missing"
        if args.check:
            print(f"{detail}: {dest}")
            bad.append(entry["path"])
            continue
        url = "https://raw.githubusercontent.com/{repo}/{commit}/{upstream}".format(**entry)
        print(f"download {url}")
        try:
            with urllib.request.urlopen(url, timeout=TIMEOUT) as response:
                data = response.read()
        except (urllib.error.URLError, http.client.HTTPException, OSError) as e:
            sys.exit(f"download failed: {url}: {e}")
        if hashlib.sha256(data).hexdigest() != want:
            sys.exit(f"sha256 mismatch for {url} (expected {want})")
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(data)
        fetched += 1

    print(f"summary: fetched={fetched} verified={verified} missing={len(bad)}")
    if bad:
        sys.exit(1)


if __name__ == "__main__":
    main()

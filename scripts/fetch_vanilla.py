#!/usr/bin/env python3
"""Decompile a vanilla Minecraft Java server: the source the version patches cite.

    scripts/fetch_vanilla.py 26.2 [26.1 26.3 ...] [--root DIR]

Each `schemas/versions/patches/*.json` entry names the vanilla source file that decides it (for
example `net/minecraft/network/protocol/game/ClientboundSectionBlocksUpdatePacket.java`). This
produces that tree: it downloads the server jar named by Mojang's launcher metadata (SHA-1 checked),
unpacks the bundled server jar, and decompiles `net/minecraft` and `com/mojang` with Vineflower 1.12.0
(Maven Central, SHA-1 checked) into `<root>/server-<version>/` (default `~/.cache/minecraft-vanilla`).

Only unobfuscated releases (26.1 and later) decompile to readable names; older server jars ship
obfuscated and are refused. Nothing here is part of a test: the tree is for reading.
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import urllib.request
import zipfile
from pathlib import Path

MANIFEST = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
VINEFLOWER = "1.12.0"
TIMEOUT = 120  # seconds without bytes before a download is given up
VINEFLOWER_URL = f"https://repo1.maven.org/maven2/org/vineflower/vineflower/{VINEFLOWER}/vineflower-{VINEFLOWER}.jar"
# present in an unobfuscated jar, mangled into a short name otherwise
READABLE = "net/minecraft/network/protocol/game/GameProtocols.class"


def fetch(url, dest, sha1=None):
    if dest.exists() and (sha1 is None or hashlib.sha1(dest.read_bytes()).hexdigest() == sha1):
        return
    print(f"download {url}")
    data = urllib.request.urlopen(url, timeout=TIMEOUT).read()
    if sha1 and hashlib.sha1(data).hexdigest() != sha1:
        sys.exit(f"sha1 mismatch for {url}")
    dest.write_bytes(data)


def capped(cmd):
    if shutil.which("systemd-run") is None:
        return cmd
    mem = os.environ.get("BPS_MEM_MAX", "4G")
    return ["systemd-run", "--user", "--scope", "-q", "-p", f"MemoryMax={mem}", "-p", "MemorySwapMax=0"] + cmd


def decompile(version, root):
    work = root / "work"
    work.mkdir(parents=True, exist_ok=True)
    manifest = json.load(urllib.request.urlopen(MANIFEST, timeout=TIMEOUT))
    entry = next((v for v in manifest["versions"] if v["id"] == version), None)
    if entry is None:
        sys.exit(f"{version}: not in Mojang's version manifest")
    server = json.load(urllib.request.urlopen(entry["url"], timeout=TIMEOUT)).get("downloads", {}).get("server")
    if not server:
        sys.exit(f"{version}: no dedicated server download")

    bundler = work / f"server-{version}-bundler.jar"
    fetch(server["url"], bundler, server["sha1"])
    with zipfile.ZipFile(bundler) as z:
        inner = [n for n in z.namelist() if n.startswith("META-INF/versions/") and n.endswith(".jar")]
        if not inner:
            sys.exit(f"{version}: the server jar has no bundled META-INF/versions/*/server-*.jar")
        pick = f"META-INF/versions/{version}/server-{version}.jar"
        jar = work / f"server-{version}.jar"
        jar.write_bytes(z.read(pick if pick in inner else inner[0]))
    with zipfile.ZipFile(jar) as z:
        if READABLE not in z.namelist():
            sys.exit(f"{version}: the server jar is obfuscated; only 26.1 and later decompile to readable names")

    vine = work / f"vineflower-{VINEFLOWER}.jar"
    if not vine.exists():
        expected = urllib.request.urlopen(VINEFLOWER_URL + ".sha1", timeout=TIMEOUT).read().decode().split()[0]
        fetch(VINEFLOWER_URL, vine, expected)

    out = root / f"server-{version}"
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    print(f"decompile {jar} -> {out}")
    cmd = ["java", "-Xmx3g", "-jar", str(vine), "--folder", "--silent", "--only=net/minecraft", "--only=com/mojang",
           str(jar), str(out)]
    subprocess.run(capped(cmd), check=True)
    (out / "VERSION.txt").write_text(f"version={version}\nvanilla_sha1={hashlib.sha1(jar.read_bytes()).hexdigest()}\n")
    print(f"done: {out}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("versions", nargs="+", help="26.1, 26.2, 26.3, ...")
    ap.add_argument("--root", default=str(Path.home() / ".cache/minecraft-vanilla"))
    args = ap.parse_args()
    for v in args.versions:
        decompile(v, Path(args.root))


if __name__ == "__main__":
    main()

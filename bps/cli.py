from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from bps.analyze import analyze
from bps.emit_json import schema_to_json
from bps.emit_rust import emit_rust
from bps.emit_rstom import emit_rstom
from bps.emit_tree import emit_tree
from bps.errors import BpsError, EmitError
from bps.interp.engine import decode_frame_payload, decode_packet, encode_packet
from bps.load import load_schema
from bps.minestom import ClassTable

# Where `packets_gen`'s schema is in an rstom workspace, relative to the workspace.
PACKETS_GEN_SCHEMA = Path("tools") / "packets_gen" / "schema" / "packets.json"
PROTOC_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_SHAPES = PROTOC_ROOT / "targets" / "rstom" / "shapes"


def _load_schema(paths: list[str]):
    return analyze(load_schema(paths))


def _default_packets_json_hint(target: str, anchor: Path) -> str:
    resolved = anchor.resolve()
    if target == "rstom-tree":
        return f"<workspace>/{PACKETS_GEN_SCHEMA.as_posix()}"
    if (resolved / PACKETS_GEN_SCHEMA).is_file():
        return f"<workspace>/{PACKETS_GEN_SCHEMA.as_posix()}"
    return f"<out>/../../{PACKETS_GEN_SCHEMA.as_posix()}"


def _class_table(given: str | None, anchor: Path, target: str) -> ClassTable:
    """rstom's class table: the one named, or the one beside an rstom workspace / crate."""
    if given is not None:
        path = Path(given)
    else:
        resolved = anchor.resolve()
        path = (
            resolved / PACKETS_GEN_SCHEMA
            if (resolved / PACKETS_GEN_SCHEMA).is_file()
            else resolved.parent.parent / PACKETS_GEN_SCHEMA
        )
    if not path.is_file():
        default = _default_packets_json_hint(target, anchor)
        raise EmitError(
            f"emit {target} needs rstom's packets_gen schema, which names each packet's Minestom "
            f"class: {path} is not a file (pass --packets-json; by default it is {default})"
        )
    return ClassTable.load(path)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="bpsc")
    sub = parser.add_subparsers(dest="cmd", required=True)

    check_p = sub.add_parser("check")
    check_p.add_argument("schemas", nargs="+")

    dump_p = sub.add_parser("dump-json")
    dump_p.add_argument("schemas", nargs="+")

    routes_p = sub.add_parser("routes")
    routes_p.add_argument("schemas", nargs="+")

    dec_p = sub.add_parser("decode")
    dec_p.add_argument("schemas", nargs="+")
    dec_p.add_argument("--packet", help="packet name for raw payload decode")
    dec_p.add_argument("--state")
    dec_p.add_argument("--direction")
    dec_p.add_argument("--hex", action="store_true")

    enc_p = sub.add_parser("encode")
    enc_p.add_argument("--packet", required=True)
    enc_p.add_argument("schemas", nargs="*")
    enc_p.add_argument("json_value")

    emit_p = sub.add_parser("emit")
    emit_p.add_argument("target", choices=["rust", "rstom", "rstom-tree"])
    emit_p.add_argument("schemas", nargs="+")
    emit_p.add_argument("--out")
    emit_p.add_argument(
        "--rstom",
        help="rstom workspace root (emit rstom-tree)",
    )
    emit_p.add_argument(
        "--shapes",
        help=f"shape ledger directory (emit rstom-tree), default: {DEFAULT_SHAPES.as_posix()}",
    )
    emit_p.add_argument(
        "--check",
        action="store_true",
        help="report drift instead of writing (emit rstom-tree)",
    )
    emit_p.add_argument(
        "--packets-json",
        help="rstom's packets_gen schema (emit rstom), default: the one of the rstom "
        "workspace the crate is written into",
    )

    args = parser.parse_args(argv)
    try:
        if args.cmd == "check":
            _load_schema(args.schemas)
            print("ok")
            return 0
        if args.cmd == "dump-json":
            resolved = _load_schema(args.schemas)
            print(schema_to_json(resolved.schema))
            return 0
        if args.cmd == "routes":
            resolved = _load_schema(args.schemas)
            for route, pkt in resolved.routes:
                print(
                    f"{route.state}.{route.direction} 0x{route.packet_id:02x} -> {pkt.name}"
                )
            return 0
        if args.cmd == "decode":
            resolved = _load_schema(args.schemas)
            raw = sys.stdin.read().strip()
            if args.hex:
                data = bytes.fromhex(raw.replace(" ", ""))
            else:
                data = bytes.fromhex(raw.replace(" ", ""))
            if args.packet:
                out = decode_packet(resolved, args.packet, data)
            elif args.state and args.direction:
                pid, out = decode_frame_payload(resolved, args.state, args.direction, data)
                out = {"id": pid, **out}
            else:
                print("need --packet or --state and --direction", file=sys.stderr)
                return 1
            print(json.dumps(out, sort_keys=True))
            return 0
        if args.cmd == "encode":
            resolved = _load_schema(args.schemas)
            val = json.loads(args.json_value)
            out = encode_packet(resolved, args.packet, val)
            print(out.hex())
            return 0
        if args.cmd == "emit":
            resolved = _load_schema(args.schemas)
            if args.target == "rstom-tree":
                if args.rstom is None:
                    raise EmitError("emit rstom-tree needs --rstom <workspace>")
                workspace = Path(args.rstom)
                shapes = Path(args.shapes) if args.shapes else DEFAULT_SHAPES
                classes = _class_table(args.packets_json, workspace, "rstom-tree")
                drift = emit_tree(resolved, workspace, classes, shapes, check=args.check)
                if args.check:
                    if drift:
                        print("\n".join(drift), file=sys.stderr)
                        return 3
                    print("ok")
                    return 0
                print(f"emitted rstom-tree into {workspace}")
                return 0
            if args.check:
                raise EmitError(f"emit {args.target} does not support --check (only emit rstom-tree does)")
            if args.out is None:
                raise EmitError(f"emit {args.target} needs --out <directory>")
            if args.target == "rust":
                emit_rust(resolved, Path(args.out))
            else:
                emit_rstom(
                    resolved,
                    Path(args.out),
                    _class_table(args.packets_json, Path(args.out), "rstom"),
                )
            print(f"emitted {args.target} crate to {args.out}")
            return 0
    except BpsError as e:
        print(str(e), file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
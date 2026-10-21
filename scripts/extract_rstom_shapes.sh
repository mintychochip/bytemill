#!/usr/bin/env bash
# Regenerate the git-ignored rstom shape ledger at targets/rstom/shapes.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/extract_rstom_shapes.sh [--rstom <dir>] [--packets-json <file>] [--schema <file>]

Regenerates the shape ledger at targets/rstom/shapes from an rstom worktree.

  --rstom <dir>          rstom worktree (or BML_RSTOM_DIR)
  --packets-json <file>  packets_gen schema (or BML_CLASS_TABLE); defaults to
                         <rstom>/tools/packets_gen/schema/packets.json
  --schema <file>        schema to extract against
                         (default: schemas/minecraft_26_3_wiki.bml)
  -h, --help             this text

Requires a built CLI (BML, default target/debug/bml); run
`cargo build --bin bml` first if it is missing.
EOF
}

RSTOM="${BML_RSTOM_DIR:-}"
PACKETS_JSON="${BML_CLASS_TABLE:-}"
SCHEMA="schemas/minecraft_26_3_wiki.bml"

while [ $# -gt 0 ]; do
    case "$1" in
        --rstom) RSTOM="${2:?--rstom needs a directory}"; shift 2 ;;
        --packets-json) PACKETS_JSON="${2:?--packets-json needs a file}"; shift 2 ;;
        --schema) SCHEMA="${2:?--schema needs a file}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ -z "$RSTOM" ]; then
    echo "error: rstom worktree not given (pass --rstom or set BML_RSTOM_DIR)" >&2
    usage >&2
    exit 2
fi

BML="${BML:-target/debug/bml}"
if [ ! -x "$BML" ]; then
    echo "error: CLI not found at $BML; run: cargo build --bin bml" >&2
    exit 1
fi

EXTRA=()
if [ -n "$PACKETS_JSON" ]; then
    EXTRA+=(--packets-json "$PACKETS_JSON")
fi

"$BML" shapes extract "$SCHEMA" --rstom "$RSTOM" --eligible --out targets/rstom/shapes "${EXTRA[@]}"

echo "wrote targets/rstom/shapes (git-ignored; regenerable with this script)"

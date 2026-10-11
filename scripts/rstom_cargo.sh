#!/usr/bin/env bash
# cargo against the rstom worktree, politely: the machine also runs a benchmark loop.
set -euo pipefail
here="$(cd "$(dirname "$0")/../.." && pwd)"
cd "${BPS_RSTOM_DIR:-$here/rstom-bps}"
exec nice -n 19 ionice -c3 env \
  CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
  CARGO_TARGET_DIR="${BPS_CARGO_TARGET_DIR:-/dev/shm/rstom-bps-target}" \
  CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 cargo "$@"

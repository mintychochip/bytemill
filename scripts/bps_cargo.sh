#!/usr/bin/env bash
# cargo for the Rust port, capped so a runaway test cannot freeze the machine.
#
# cargo runs in a transient user scope with a hard memory cap and swap off: a runaway
# allocation is killed by the kernel inside the scope, instead of pushing the whole desktop
# into reclaim until systemd-oomd kills the terminal. It also runs nice/ionice with 4 jobs and
# 4 test threads, and keeps scratch files on disk, outside the repo (a tmpfs /tmp is RAM the cap
# would count; a crate under the workspace root is taken for a workspace member).
#
#   scripts/bps_cargo.sh test --test test_nbt
#   BPS_MEM_MAX=3G scripts/bps_cargo.sh test    # tighter cap: a runaway dies sooner
#
# Prints the scope's peak memory on exit, so the cap can be tuned from evidence.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
scratch="${BPS_SCRATCH_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/bps-scratch}"
mkdir -p "$scratch"
exec systemd-run --user --scope --quiet \
  -p "MemoryMax=${BPS_MEM_MAX:-8G}" -p MemorySwapMax=0 \
  -p "CPUQuota=${BPS_CPU_QUOTA:-400%}" -p "TasksMax=${BPS_TASKS_MAX:-1024}" \
  nice -n 19 ionice -c3 env \
    CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
    RUST_TEST_THREADS="${RUST_TEST_THREADS:-4}" \
    TMPDIR="$scratch" \
    bash -c 'cargo "$@"; rc=$?; peak=$(cat "/sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup)/memory.peak" 2>/dev/null || echo 0); echo "bps_cargo: peak $((peak / 1048576)) MiB, exit $rc" >&2; exit $rc' bps_cargo "$@"

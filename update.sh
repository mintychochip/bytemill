#!/usr/bin/env bash
# Update an installed bpsc to the latest GitHub release (or a pinned tag).
#
#   curl -fsSL https://raw.githubusercontent.com/mintychochip/wirecraft/master/update.sh | bash
#   curl -fsSL .../update.sh | bash -s -- --version v1.2.3
#
# The install location is taken from --dir, $BPS_INSTALL_DIR, the bpsc found on
# PATH, or ~/.local/bin. The installer records the version in bpsc.version
# next to the binary; if that file matches the target, nothing is downloaded.
set -euo pipefail

REPO="${BPS_REPO:-mintychochip/wirecraft}"
DIR="${BPS_INSTALL_DIR:-}"
VERSION="${BPS_VERSION:-latest}"
TARGET=""

usage() {
  cat <<'EOF'
usage: update.sh [options]

options:
  --version <tag>   release tag to install (default: the latest release)
  --dir <path>      install directory (default: $BPS_INSTALL_DIR, the bpsc on
                    PATH, or ~/.local/bin)
  --target <triple> release target to install (passed to install.sh)
  -h, --help        show this help

environment:
  BPS_REPO         GitHub repository (default: mintychochip/wirecraft)
  BPS_INSTALL_DIR  default install directory
  BPS_VERSION      default release tag
  BPS_INSTALL_SH   installer path or URL (default: next to this script, else
                   install.sh from master)
EOF
}

die() {
  printf 'update.sh: %s\n' "$*" >&2
  exit 1
}

note() {
  printf 'update.sh: %s\n' "$*"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      [ $# -ge 2 ] || die "--version needs a value"
      VERSION="$2"
      shift 2
      ;;
    --version=*)
      VERSION="${1#*=}"
      shift
      ;;
    --dir)
      [ $# -ge 2 ] || die "--dir needs a value"
      DIR="$2"
      shift 2
      ;;
    --dir=*)
      DIR="${1#*=}"
      shift
      ;;
    --target)
      [ $# -ge 2 ] || die "--target needs a value"
      TARGET="$2"
      shift 2
      ;;
    --target=*)
      TARGET="${1#*=}"
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1 (try --help)"
      ;;
  esac
done

fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 --retry-delay 2 "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- "$1"
  else
    die "needs curl or wget"
  fi
}

latest_tag() {
  local url="https://github.com/${REPO}/releases/latest"
  local effective tag
  if command -v curl >/dev/null 2>&1; then
    effective=$(curl -fsSIL -o /dev/null -w '%{url_effective}' "$url" 2>/dev/null) ||
      die "cannot reach $url (no published release yet?)"
    tag="${effective##*/tag/}"
    if [ "$tag" = "$effective" ] || [ "${tag#v}" = "$tag" ]; then
      die "no published release found at $url"
    fi
    printf '%s' "$tag"
    return
  fi
  effective=$(fetch "https://api.github.com/repos/${REPO}/releases/latest") ||
    die "cannot query releases for ${REPO}"
  tag=$(printf '%s\n' "$effective" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
  [ -n "$tag" ] || die "no published release found for ${REPO}"
  printf '%s' "$tag"
}

if [ -z "$DIR" ]; then
  if on_path=$(command -v bpsc 2>/dev/null); then
    DIR=$(cd "$(dirname "$on_path")" && pwd)
  else
    DIR="$HOME/.local/bin"
  fi
fi

current=""
if [ -f "${DIR}/bpsc.version" ]; then
  current=$(tr -d '[:space:]' <"${DIR}/bpsc.version")
fi

if [ "$VERSION" = latest ]; then
  VERSION=$(latest_tag)
  [ -n "$VERSION" ] || die "could not resolve the latest release"
fi
case "$VERSION" in
  v*) ;;
  *) VERSION="v${VERSION}" ;;
esac

if [ "$current" = "$VERSION" ] && [ -x "${DIR}/bpsc" ]; then
  note "bpsc ${VERSION} is already installed in ${DIR}"
  exit 0
fi

if [ -n "$current" ]; then
  note "updating bpsc ${current} -> ${VERSION}"
else
  note "installing bpsc ${VERSION}"
fi

installer="${BPS_INSTALL_SH:-}"
if [ -z "$installer" ]; then
  src="${BASH_SOURCE[0]:-}"
  if [ -n "$src" ] && [ -f "$src" ]; then
    here=$(cd "$(dirname "$src")" && pwd)
    if [ -f "${here}/install.sh" ]; then
      installer="${here}/install.sh"
    fi
  fi
fi

tmp=""
if [ -z "$installer" ]; then
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/bpsc-update.XXXXXX")
  trap 'rm -rf "$tmp"' EXIT
  installer="${tmp}/install.sh"
  fetch "https://raw.githubusercontent.com/${REPO}/master/install.sh" >"$installer" ||
    die "could not download install.sh"
fi

args=(--version "$VERSION" --dir "$DIR")
if [ -n "$TARGET" ]; then
  args+=(--target "$TARGET")
fi
bash "$installer" "${args[@]}"

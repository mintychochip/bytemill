#!/usr/bin/env bash
# Install the Bytemill CLI (`bml`) from this repository's GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/mintychochip/wirecraft/master/install.sh | bash
#   curl -fsSL .../install.sh | bash -s -- --version v1.2.3 --dir /usr/local/bin
#
# The archive and SHA256SUMS are downloaded from the same release; the binary
# is verified before it replaces anything. Linux x86_64 defaults to the static
# musl build; pass --libc gnu if you need the glibc-linked one.
set -euo pipefail

REPO="${BML_REPO:-${BPS_REPO:-mintychochip/wirecraft}}"
DIR="${BML_INSTALL_DIR:-${BPS_INSTALL_DIR:-$HOME/.local/bin}}"
VERSION="${BML_VERSION:-${BPS_VERSION:-latest}}"
TARGET=""
LIBC=""

usage() {
  cat <<'EOF'
usage: install.sh [options]

options:
  --version <tag>   release tag to install (default: the latest release)
  --dir <path>      install directory (default: $BML_INSTALL_DIR or ~/.local/bin)
  --target <triple> release target to install (e.g. x86_64-unknown-linux-musl)
  --libc <gnu|musl> Linux x86_64 variant (default: musl, the static build)
  -h, --help        show this help

environment:
  BML_REPO         GitHub repository (default: mintychochip/wirecraft)
  BML_INSTALL_DIR  default install directory
  BML_VERSION      default release tag

The legacy BPS_REPO/BPS_INSTALL_DIR/BPS_VERSION names are still honored.

Windows is not supported by this script; download the .zip from the release.
EOF
}

die() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

note() {
  printf 'install.sh: %s\n' "$*"
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
    --libc)
      [ $# -ge 2 ] || die "--libc needs a value"
      LIBC="$2"
      shift 2
      ;;
    --libc=*)
      LIBC="${1#*=}"
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

[ -n "$DIR" ] || die "install directory is empty"
[ -n "$VERSION" ] || die "version is empty"
case "$LIBC" in
  "" | gnu | musl) ;;
  *) die "--libc must be 'gnu' or 'musl'" ;;
esac

fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 --retry-delay 2 "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- "$1"
  else
    die "needs curl or wget"
  fi
}

download() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 --retry-delay 2 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$2" "$1"
  else
    die "needs curl or wget"
  fi
}

# Resolve the latest stable tag. `releases/latest` redirects to
# .../releases/tag/<tag>; the API is the fallback for wget-only systems.
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

if [ "$VERSION" = latest ]; then
  VERSION=$(latest_tag)
  [ -n "$VERSION" ] || die "could not resolve the latest release"
fi
case "$VERSION" in
  v*) ;;
  *) VERSION="v${VERSION}" ;;
esac

if [ -z "$TARGET" ]; then
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Linux) os=linux ;;
    Darwin) os=darwin ;;
    *)
      die "unsupported OS '$os'; Windows users: download bml-x86_64-pc-windows-msvc.zip from https://github.com/${REPO}/releases"
      ;;
  esac
  case "$arch" in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) die "unsupported architecture '$arch'" ;;
  esac
  case "${os}/${arch}" in
    linux/x86_64) TARGET="x86_64-unknown-linux-${LIBC:-musl}" ;;
    linux/aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
    darwin/x86_64) TARGET="x86_64-apple-darwin" ;;
    darwin/aarch64) TARGET="aarch64-apple-darwin" ;;
    *) die "no prebuilt binary for ${os}/${arch}" ;;
  esac
fi

ASSET="bml-${TARGET}.tar.gz"
BASE="https://github.com/${REPO}/releases/download/${VERSION}"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/bml-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

note "downloading ${ASSET} (${VERSION})"
download "${BASE}/${ASSET}" "${tmp}/${ASSET}" ||
  die "download failed: ${BASE}/${ASSET}"
download "${BASE}/SHA256SUMS" "${tmp}/SHA256SUMS" ||
  die "download failed: ${BASE}/SHA256SUMS"

line=$(grep -E "[[:space:]]${ASSET}\$" "${tmp}/SHA256SUMS" || true)
[ -n "$line" ] || die "SHA256SUMS has no entry for ${ASSET}"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmp" && printf '%s\n' "$line" | sha256sum -c - >/dev/null) ||
    die "checksum verification failed for ${ASSET}"
elif command -v shasum >/dev/null 2>&1; then
  (cd "$tmp" && printf '%s\n' "$line" | shasum -a 256 -c - >/dev/null) ||
    die "checksum verification failed for ${ASSET}"
else
  die "needs sha256sum or shasum to verify the download"
fi
note "checksum ok"

tar -xzf "${tmp}/${ASSET}" -C "$tmp" bml ||
  die "archive does not contain bml"

mkdir -p "$DIR"
staged="${DIR}/.bml.new.$$"
cp "${tmp}/bml" "$staged"
chmod 0755 "$staged"
mv -f "$staged" "${DIR}/bml"
printf '%s\n' "$VERSION" >"${DIR}/bml.version"

note "installed bml ${VERSION} to ${DIR}/bml"
if [ -e "${DIR}/bpsc" ]; then
  note "legacy ${DIR}/bpsc is still present; bml is the supported command"
fi
case ":${PATH}:" in
  *":${DIR}:"*) ;;
  *) note "add ${DIR} to PATH to run 'bml' directly" ;;
esac

#!/usr/bin/env bash
# Download the `kubectl` executable the admin service drives clusters with.
#
#   ./local-infra/applications/kubectl/fetch.sh                for this machine
#   ./local-infra/applications/kubectl/fetch.sh linux arm64    for an image
#
# The admin shells out to kubectl for every host operation, so its image has to
# carry one. It is a single static Go binary, which is what makes it something
# crane can append as a layer rather than something a Dockerfile has to install.
#
# Fetched per target architecture, into kubectl-<os>-<arch>, because the image
# being built is often not for this machine.
set -euo pipefail

VERSION="${VERSION:-v1.34.4}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

case "${1:-$(uname -s)}" in
    Darwin|darwin) OS=darwin ;;
    Linux|linux)   OS=linux ;;
    *) echo "unsupported OS '${1:-$(uname -s)}'"; exit 1 ;;
esac
case "${2:-$(uname -m)}" in
    arm64|aarch64) MACH=arm64 ;;
    x86_64|amd64)  MACH=amd64 ;;
    *) echo "unsupported architecture '${2:-$(uname -m)}'"; exit 1 ;;
esac

OUT="$DIR/kubectl-$OS-$MACH"
BASE="https://dl.k8s.io/release/${VERSION}/bin/${OS}/${MACH}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "==> kubectl $VERSION for $OS/$MACH"
curl -fsSL -o "$tmp/kubectl" "$BASE/kubectl"
# Every release publishes a checksum beside the binary; a control-plane tool is
# not something to take on trust from a redirect.
curl -fsSL -o "$tmp/kubectl.sha256" "$BASE/kubectl.sha256"
want="$(cat "$tmp/kubectl.sha256" | tr -d '[:space:]')"
got="$(shasum -a 256 "$tmp/kubectl" | awk '{print $1}')"
[ "$want" = "$got" ] || { echo "checksum mismatch for kubectl"; exit 1; }
echo "    checksum ok"

install -m 0755 "$tmp/kubectl" "$OUT"
echo "    -> $OUT"

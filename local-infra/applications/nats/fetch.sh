#!/usr/bin/env bash
# Download the `nats-server` executable the services talk to.
#
#   ./local-infra/applications/nats/fetch.sh                 for this machine
#   ./local-infra/applications/nats/fetch.sh linux amd64     for a server
#
# NATS is the event bus: every service publishes what it did and subscribes to
# what it cares about. The executable lives beside the other vendored tools
# rather than being installed system-wide, so a fresh checkout has a bus with
# nothing else set up — no brew, no apt, no container.
#
# One static Go binary, ~20 MB, no dependencies and no runtime. That is why it
# can be vendored at all where Postgres is not.
#
# Version is pinned. A bus that silently changes underneath a deployment is a
# way to have two environments disagree about what a subject means.
set -euo pipefail

VERSION="${VERSION:-2.14.6}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The release assets use Go's names, not uname's.
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

NAME="nats-server-v${VERSION}-${OS}-${MACH}"
ARCHIVE="$NAME.tar.gz"
BASE="https://github.com/nats-io/nats-server/releases/download/v${VERSION}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "==> nats-server $VERSION for $OS/$MACH"
curl -fsSL -o "$tmp/$ARCHIVE" "$BASE/$ARCHIVE"

# The checksum file covers every asset in the release; check ours against it
# rather than trusting the download.
if curl -fsSL -o "$tmp/SHA256SUMS" "$BASE/SHA256SUMS" 2>/dev/null; then
    want="$(grep -F "$ARCHIVE" "$tmp/SHA256SUMS" | awk '{print $1}' | head -1)"
    if [ -n "$want" ]; then
        got="$(shasum -a 256 "$tmp/$ARCHIVE" | awk '{print $1}')"
        [ "$want" = "$got" ] || { echo "checksum mismatch for $ARCHIVE"; exit 1; }
        echo "    checksum ok"
    fi
fi

# Only the server. The archive also carries a README and a LICENSE, and an
# unused file in a repository is a thing people later wonder about.
tar -xzf "$tmp/$ARCHIVE" -C "$tmp"
install -m 0755 "$tmp/$NAME/nats-server" "$DIR/nats-server"
echo "    $("$DIR/nats-server" --version) -> $DIR/nats-server"

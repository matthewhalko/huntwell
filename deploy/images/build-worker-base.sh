#!/usr/bin/env bash
# Build the worker TOOLCHAIN base — Node, the Playwright MCP server and the
# Cursor CLI. No Huntwell code goes in it.
#
#   ./deploy/images/build-worker-base.sh                    build it locally
#   ARCH=amd64 ./deploy/images/build-worker-base.sh         for a prod node
#   PUSH=ghcr.io/you ./deploy/images/build-worker-base.sh   and publish it
#
# This is the only step in the whole build that needs a builder, because
# installing packages means running commands and crane only appends layers.
# Run it when a tool version changes — not when the product does. Everything
# else, including the worker image itself, is assembled by crane from bin/.
#
# Once it is published, a machine with no Docker at all can build workers:
#   WORKER_BASE=ghcr.io/you/huntwell-worker-base:1 ./build.sh --images
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$DIR/../.." && pwd)"
TAG="${TAG:-1}"
PUSH="${PUSH:-}"; PUSH="${PUSH%/}"

case "${ARCH:-$(uname -m)}" in
    arm64|aarch64) ARCH=arm64; PLATFORM="linux/arm64"; NODE_ARCH=arm64 ;;
    amd64|x86_64)  ARCH=amd64; PLATFORM="linux/amd64"; NODE_ARCH=x64 ;;
    *) echo "unknown ARCH '${ARCH}' — expected arm64 or amd64"; exit 1 ;;
esac

# crane, which builds every other image, can only read a base from a registry —
# not from the Docker daemon and not from a tarball. So this base has to be
# pushed somewhere even to be used locally. With no PUSH given that somewhere is
# a throwaway registry container on this machine, started if it is not already
# running, which keeps `build-worker-base.sh && build.sh --images` a two-command
# sequence instead of a manual dance with a registry.
LOCAL_REGISTRY="${LOCAL_REGISTRY:-localhost:5111}"
LOCAL_REGISTRY_CONTAINER="huntwell-registry"
if [ -z "$PUSH" ]; then
    PUSH="$LOCAL_REGISTRY"
    USING_LOCAL=1
fi
IMAGE="$PUSH/huntwell-worker-base:$TAG"

command -v docker >/dev/null 2>&1 || {
    echo "docker not found — this one step needs a builder (see the header)." >&2
    echo "Everything else builds without one." >&2
    exit 1
}
docker info >/dev/null 2>&1 || { echo "docker is installed but the daemon is not responding" >&2; exit 1; }

# The throwaway registry, if that is what we are using.
if [ "${USING_LOCAL:-0}" = 1 ]; then
    if ! curl -fsS -m 2 "http://$LOCAL_REGISTRY/v2/" >/dev/null 2>&1; then
        echo "==> Starting the local registry ($LOCAL_REGISTRY)"
        docker rm -f "$LOCAL_REGISTRY_CONTAINER" >/dev/null 2>&1 || true
        docker run -d --restart unless-stopped \
            -p "${LOCAL_REGISTRY##*:}:5000" \
            --name "$LOCAL_REGISTRY_CONTAINER" registry:2 >/dev/null
        for _ in $(seq 1 30); do
            curl -fsS -m 1 "http://$LOCAL_REGISTRY/v2/" >/dev/null 2>&1 && break
            sleep 0.3
        done
        curl -fsS -m 2 "http://$LOCAL_REGISTRY/v2/" >/dev/null 2>&1 \
            || { echo "the local registry did not come up" >&2; exit 1; }
    fi
fi

echo "==> Building $IMAGE ($PLATFORM)"
docker build \
    -f "$DIR/Dockerfile.worker-base" \
    -t "$IMAGE" \
    --platform "$PLATFORM" \
    --build-arg "NODE_ARCH=$NODE_ARCH" \
    "$REPO"

# `image ls`, not `image inspect .Size`: with the containerd store the latter
# reports the compressed size, which reads as suspiciously small.
echo "    $IMAGE  $(docker image ls --format '{{.Size}}' "$IMAGE")"

docker push -q "$IMAGE" >/dev/null
echo
if [ "${USING_LOCAL:-0}" = 1 ]; then
    echo "Pushed to the local registry. Build the images with:"
    # No WORKER_BASE needed: this is where build-images.sh looks by default.
    echo "  ./build.sh --images"
    echo
    echo "That registry is this machine's only. For a real deployment publish it"
    echo "somewhere the build server can reach:"
    echo "  PUSH=ghcr.io/you $0"
else
    echo "Pushed. Point the worker build at it — anywhere, with or without Docker:"
    echo "  WORKER_BASE=$IMAGE ./build.sh --images"
fi

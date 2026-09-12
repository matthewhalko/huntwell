#!/usr/bin/env bash
# Package the Huntwell executable into the two pod images. No Docker.
#
#   ./build.sh --images                     build the binary, then package it
#   ./deploy/images/build-images.sh         package whatever bin/ already holds
#
#   IMAGES=worker ./deploy/images/build-images.sh    just one of them
#   TAG=v3        ./deploy/images/build-images.sh    a different tag
#   ARCH=amd64    ./deploy/images/build-images.sh    package the amd64 build
#
#   PUSH=ghcr.io/you ./deploy/images/build-images.sh   push them as well
#
# Output is bin/images/<name>.tar — a docker-style archive that `docker load`,
# `skopeo copy docker-archive:…`, `k3d image import` and `crane push` all
# accept. The artifact lands beside the binaries it wraps, the same way the
# parkriver build works.
#
# Built with `crane`, which assembles an image by appending one layer to a base
# it pulls straight from the registry. No daemon, no VM, nothing privileged —
# the same property ./build.sh already has for compiling.
#
# One image per service, and the base is decided by one thing: whether that
# service runs the agent.
#
#   website       ubuntu:24.04 + one executable. Nothing is installed — the
#   scheduling    binaries carry their own TLS roots through webpki-roots, so
#   notification  nothing reads /etc/ssl/certs and ca-certificates is not needed.
#
#   planning      the toolchain base, which carries Node, the Playwright MCP
#   worker        server and the Cursor CLI. That base DOES need a builder,
#                 because installing things means running commands — build it
#                 with deploy/images/build-worker-base.sh once, and after that
#                 every code change is a crane append like the others.
#
# There is no admin image: the control plane runs as a systemd unit on its own
# server (deploy/huntwell-admin.service.in), because it drives the clusters
# rather than living in one.
#
# A remote k3d/k3s host cannot see an image built here — there is no
# `k3d image import` across the network — so a fleet of hosts needs a registry.
# Point Host.image in the admin dashboard at the pushed name.
#
# ARCH defaults to this machine's, so images built here run in the k3d cluster
# here. A production node is almost certainly amd64 — pass ARCH=amd64 for it,
# on Apple silicon as elsewhere. Getting this wrong is not subtle: the pod
# starts, the kernel cannot run the binary, and the log reads "exec format
# error".
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$DIR/../.." && pwd)"
TAG="${TAG:-dev}"
PUSH="${PUSH:-}"; PUSH="${PUSH%/}"
OUT="$REPO/bin/images"
SERVICES_BASE="${SERVICES_BASE:-ubuntu:24.04}"
# The toolchain image build-worker-base.sh produces. Defaulted to the throwaway
# local registry that script pushes to, because crane can only read a base from
# a registry — a bare name like `huntwell-worker-base:1` is one it can never
# resolve, so defaulting to that was a default that could not work. Override to
# use one you have already published, which is how a machine with no Docker
# builds workers.
WORKER_BASE="${WORKER_BASE:-${LOCAL_REGISTRY:-localhost:5111}/huntwell-worker-base:1}"

# Which architecture, and therefore which build directory and which variant of
# each base to pull.
case "${ARCH:-$(uname -m)}" in
    arm64|aarch64) ARCH=arm64; BUILD_TARGET=arm64;  BINDIR="bin/ubuntu-arm64" ;;
    amd64|x86_64)  ARCH=amd64; BUILD_TARGET=ubuntu; BINDIR="bin/ubuntu" ;;
    *) echo "unknown ARCH '${ARCH}' — expected arm64 or amd64"; exit 1 ;;
esac
PLATFORM="linux/$ARCH"
BIN="$REPO/$BINDIR"

SELECT="${IMAGES:-}"

# name : which base : which port it listens on ('-' for the services that
# serve no HTTP). Keep in step with build.sh's BINARIES and k8s/base.
IMAGE_SET="website:plain:8611 admin:plain:8710 planning:agent:- worker:agent:- scheduling:plain:- notification:plain:-"

wanted() {
    [ -z "$SELECT" ] && return 0
    echo " $SELECT " | grep -q " $1 "
}

# Where crane is, in order of how deliberate the choice was: an explicit path,
# beside this script (a copy deployed to a server on its own), a repository
# checkout, then $PATH.
if [ -n "${CRANE:-}" ] && [ ! -x "$CRANE" ]; then
    echo "CRANE=$CRANE is not executable" >&2; exit 1
fi
for candidate in "${CRANE:-}" "$DIR/crane" "$REPO/local-infra/applications/crane/crane"; do
    if [ -n "$candidate" ] && [ -x "$candidate" ]; then CRANE="$candidate"; break; fi
    CRANE=""
done
[ -n "$CRANE" ] || CRANE="$(command -v crane 2>/dev/null || true)"
if [ -z "$CRANE" ]; then
    cat >&2 <<'HELP'
crane not found. It is what builds and pushes these archives — one Go binary,
no daemon.

  brew install crane

  Or on a Linux server:
    curl -fsSL https://github.com/google/go-containerregistry/releases/download/v0.22.1/go-containerregistry_Linux_x86_64.tar.gz \
      | tar -xzf - crane && chmod +x crane

  Or point at one you already have:
    CRANE=/path/to/crane deploy/images/build-images.sh
HELP
    exit 1
fi

# Say what is missing and how to get it, rather than letting the layer step
# fail on a path the reader can see on disk for the other architecture.
missing=""
for entry in $IMAGE_SET; do
    name="${entry%%:*}"
    wanted "$name" || continue
    [ -f "$BIN/$name" ] || missing="$missing $name"
done
if [ -n "$missing" ]; then
    echo "no $ARCH build of:$missing" >&2
    echo "" >&2
    echo "  Build them first:" >&2
    echo "    ./build.sh $BUILD_TARGET" >&2
    exit 1
fi

mkdir -p "$OUT"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Ownership is set numerically rather than left as whoever ran the build: a
# layer carrying a macOS uid would put files in the image owned by an account
# that does not exist in it. bsdtar (macOS) and GNU tar (Linux) spell numeric
# ownership differently and each rejects the other's flag, so it is resolved
# once here rather than guessed per call.
if tar --version 2>&1 | grep -qi bsdtar; then
    tar_owned() { tar --uid "$1" --gid "$2" "${@:3}"; }
else
    tar_owned() { tar --owner="$1" --group="$2" --numeric-owner "${@:3}"; }
fi

# One layer per service: that service's executable, alone. Nothing shares an
# image, so nothing carries a binary it never runs.
layer() {
    local name="$1" root="$work/root"
    rm -rf "$root"
    mkdir -p "$root/usr/local/bin"
    cp "$BIN/$name" "$root/usr/local/bin/$name"
    chmod 0755 "$root/usr/local/bin/$name"
    # The admin drives clusters by shelling out to kubectl, so its image needs
    # one. A single static binary, which is why it can ride in this layer
    # instead of requiring a Dockerfile that installs it.
    if [ "$name" = admin ]; then
        local kube="$REPO/local-infra/applications/kubectl/kubectl-linux-$ARCH"
        # Fetched rather than demanded. This script already knows the command,
        # and stopping a six-image build to have someone paste it back is a
        # prerequisite pretending to be an error.
        if [ ! -x "$kube" ]; then
            echo "    fetching kubectl for linux/$ARCH (the admin image carries one)"
            if ! "$REPO/local-infra/applications/kubectl/fetch.sh" linux "$ARCH" >/dev/null; then
                echo "    could not fetch kubectl for linux/$ARCH" >&2
                echo "" >&2
                echo "  Fetch it by hand, then re-run:" >&2
                echo "    ./local-infra/applications/kubectl/fetch.sh linux $ARCH" >&2
                exit 1
            fi
        fi
        cp "$kube" "$root/usr/local/bin/kubectl"
        chmod 0755 "$root/usr/local/bin/kubectl"
    fi
    ( cd "$root" && tar_owned 0 0 -cf "$work/layer.tar" usr )
}

echo
echo "==> Packaging $ARCH images from $BINDIR (tag :$TAG) with crane"

PACKAGED=""
package() {
    # Separate statements: bash expands every argument to `local` before it
    # assigns any of them, so `file="$OUT/$image.tar"` on the same line would
    # read $image while it is still unset.
    local image="$1"
    local base="$2"
    local file="$OUT/$image.tar"
    shift 2
    "$CRANE" mutate "$base" \
        --platform "$PLATFORM" \
        --append "$work/layer.tar" \
        "$@" \
        -t "$image:$TAG" \
        -o "$file" >/dev/null
    PACKAGED="$PACKAGED $image"
    printf '    %-24s %s\n' "$image:$TAG" "$(du -h "$file" | cut -f1)"
}

# The agent-base check happens once, and only if something actually needs it.
agent_base_ready=""
need_agent_base() {
    [ -n "$agent_base_ready" ] && return 0
    # `config --platform` rather than `manifest`: a multi-arch index built for
    # this Mac exists under the same name while carrying no linux/amd64 child,
    # and an existence check passes on it — then the layer append fails with
    # crane's own wording several images later.
    if "$CRANE" manifest "$WORKER_BASE" >/dev/null 2>&1 \
        && ! "$CRANE" config --platform "$PLATFORM" "$WORKER_BASE" >/dev/null 2>&1; then
        echo "    agent base $WORKER_BASE exists, but not for $PLATFORM" >&2
        echo "" >&2
        echo "  It was built for a different architecture — most likely this Mac's." >&2
        echo "  Build the one this image needs:" >&2
        echo "    ARCH=$ARCH ./deploy/images/build-worker-base.sh" >&2
        exit 1
    fi
    if ! "$CRANE" manifest "$WORKER_BASE" >/dev/null 2>&1; then
        echo "    agent base $WORKER_BASE not found" >&2
        echo "" >&2
        echo "  It carries Node, the Playwright MCP server and the Cursor CLI, so" >&2
        echo "  building it runs commands and needs a builder. Build it once:" >&2
        echo "    ARCH=$ARCH ./deploy/images/build-worker-base.sh" >&2
        echo "  or point at one already published:" >&2
        echo "    WORKER_BASE=ghcr.io/you/huntwell-worker-base:1 ./build.sh --images" >&2
        echo "" >&2
        echo "  The four that need no base can be built now — only planning and" >&2
        echo "  worker carry the agent:" >&2
        echo "    IMAGES=\"website admin scheduling notification\" ./build.sh --images" >&2
        exit 1
    fi
    agent_base_ready=1
}

for entry in $IMAGE_SET; do
    name="${entry%%:*}"
    rest="${entry#*:}"
    base_kind="${rest%%:*}"
    port="${rest##*:}"
    wanted "$name" || continue

    case "$base_kind" in
        agent) need_agent_base; base="$WORKER_BASE" ;;
        *)     base="$SERVICES_BASE" ;;
    esac

    layer "$name"
    set -- --entrypoint "$name"
    # Only the services that listen get an address and a port. The rest are
    # queue readers: nothing connects to them, so an exposed port would be a
    # claim the image does not honour.
    if [ "$port" != "-" ]; then
        set -- "$@" --env "HUNTWELL_ADDR=0.0.0.0:$port" --exposed-ports "$port/tcp"
    fi
    # The two that run the agent write a workspace per execution and drive a
    # remote browser; both are properties of the image, not of the deployment.
    if [ "$base_kind" = agent ]; then
        set -- "$@" --env HUNTWELL_DATA_DIR=/tmp/huntwell-data --env HUNTWELL_BROWSER=browserbase
    fi
    package "huntwell-$name" "$base" "$@"
done

[ -n "$PACKAGED" ] || { echo "    nothing selected by IMAGES=$SELECT"; exit 1; }
echo
echo "==> Wrote $OUT ($(du -sh "$OUT" | cut -f1) total)"

if [ -n "$PUSH" ]; then
    echo
    echo "==> Pushing to $PUSH"
    for name in $PACKAGED; do
        # --insecure: a registry on a private network serves plain HTTP.
        "$CRANE" push --insecure "$OUT/$name.tar" "$PUSH/$name:$TAG" >/dev/null
        echo "    $PUSH/$name:$TAG"
    done
    for name in $PACKAGED; do
        [ "$name" = huntwell-worker ] || continue
        echo
        echo "Point the pool at it — admin dashboard > the host > Image:"
        echo "  $PUSH/$name:$TAG"
    done
else
    echo
    echo "Load them into the local cluster with:"
    for name in $PACKAGED; do echo "  k3d image import $OUT/$name.tar -c huntwell"; done
    echo
    echo "A remote host cannot use these — push them:"
    echo "  PUSH=ghcr.io/you TAG=$TAG $0"
fi

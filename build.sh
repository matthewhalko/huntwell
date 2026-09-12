#!/usr/bin/env bash
# Build the Huntwell executable.
#
#   ./build.sh            Linux x86_64, cross-compiled with zig  -> bin/ubuntu/
#   ./build.sh arm64      Linux arm64,  cross-compiled with zig  -> bin/ubuntu-arm64/
#   ./build.sh windows    Windows x86_64, cross-compiled with zig -> bin/windows/
#   ./build.sh host       native build for this machine          -> bin/macos/ (or bin/linux/)
#
#   ./build.sh --images         ...and package it as the Huntwell pod images
#   ./build.sh arm64 --images   the same, for an arm64 cluster or node
#   REGISTRY=ghcr.io/you ./build.sh --images --push    ...and publish them
#
# Every OS builds on this machine, at native speed: the cross targets are a
# plain cargo build with zig as the C compiler and linker. No Docker, no VM, no
# emulation. Same mechanism as the parkriver and atech projects.
#
# Seven executables come out of one library — six services plus the operator
# multi-tool:
#
#   website       UI + the public API        planning   drafts plans
#   admin         the control plane          worker     executes plans
#   scheduling    fires due plans            notification  sends queued mail
#   huntwell    migrate, doctor, config, account, run, mcp
#
# The UI is embedded in `website` at compile time, so the bundle is built first
# on every path — there is no "build the UI first" step to forget.
#
# --images then wraps that one binary in the two images the k3d/k8s deployment
# runs: huntwell-services (the five services + the migrate Jobs) and
# huntwell-worker (one Job per execution). Packaging is handed off to
# deploy/images/build-images.sh, which compiles nothing and uses crane, not
# Docker — the images land beside the binaries as bin/images/<name>.tar.
#
# --push publishes them to $REGISTRY. A remote k3d/k3s host has no way to see an
# image built here, so a fleet of worker hosts needs a registry; a single local
# cluster does not (k3d image import reads the tar directly).
#
# One-time setup for the cross builds:
#
#   brew install zig
#   cargo install cargo-zigbuild
#   rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
#                     x86_64-pc-windows-gnu
#
# The binary reads its settings from a `global` file beside it (or the one
# local-infra/global on a dev checkout); see local-infra/global.example.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TARGET=""
WANT_IMAGES=0
WANT_PUSH=0
for arg in "$@"; do
    case "$arg" in
        --images)  WANT_IMAGES=1 ;;
        --push)    WANT_IMAGES=1; WANT_PUSH=1 ;;
        -h|--help) sed -n '2,38p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        --*) echo "unknown flag '$arg' — expected: --images | --push"; exit 1 ;;
        *)
            [ -z "$TARGET" ] || { echo "give one target, not '$TARGET' and '$arg'"; exit 1; }
            TARGET="$arg" ;;
    esac
done
TARGET="${TARGET:-ubuntu}"

# TRIPLE empty means "build for this machine"; anything else is a zig cross.
# EXPECT is what `file` should say afterwards — a wrong-architecture binary is
# silent until it reaches the server, where it reads as "exec format error".
# EXE is the suffix the target's executables carry.
EXE=""
case "$TARGET" in
    ubuntu|linux|amd64)
        TRIPLE="x86_64-unknown-linux-gnu";  OUT="$ROOT/bin/ubuntu"
        IMAGE_ARCH="amd64"; EXPECT="ELF 64-bit.*x86-64" ;;
    arm64|aarch64)
        TRIPLE="aarch64-unknown-linux-gnu"; OUT="$ROOT/bin/ubuntu-arm64"
        IMAGE_ARCH="arm64"; EXPECT="ELF 64-bit.*aarch64" ;;
    windows|win)
        TRIPLE="x86_64-pc-windows-gnu";     OUT="$ROOT/bin/windows"
        IMAGE_ARCH="";      EXPECT="PE32\+ executable"; EXE=".exe" ;;
    host|native|macos|mac|darwin)
        TRIPLE="";          IMAGE_ARCH="";  EXPECT=""
        case "$(uname -s)" in Darwin) OUT="$ROOT/bin/macos" ;; *) OUT="$ROOT/bin/linux" ;; esac ;;
    *) echo "unknown target '$TARGET' — expected: ubuntu | arm64 | windows | host"; exit 1 ;;
esac

# Fail before compiling, not after. A host binary cannot go in a Linux image,
# and finding that out at the end of a long build is a waste.
# Pods run Linux, so only the two Linux targets can be packaged.
if [ "$WANT_IMAGES" = 1 ] && [ -z "$IMAGE_ARCH" ]; then
    echo "--images needs a Linux target: ./build.sh --images  or  ./build.sh arm64 --images" >&2
    exit 1
fi
if [ "$WANT_PUSH" = 1 ] && [ -z "${REGISTRY:-}" ]; then
    echo "--push needs REGISTRY — e.g. REGISTRY=ghcr.io/you ./build.sh --push" >&2
    exit 1
fi


# --- preflight ---------------------------------------------------------------
# Every missing tool at once, each with the command that installs it, rather
# than one failure per re-run.
missing=0
command -v cargo >/dev/null 2>&1 || { echo "cargo not found — install Rust from https://rustup.rs" >&2; missing=1; }
command -v npm   >/dev/null 2>&1 || { echo "npm not found — install Node" >&2; missing=1; }
if [ -n "$TRIPLE" ]; then
    command -v zig            >/dev/null 2>&1 || { echo "zig not found — brew install zig" >&2; missing=1; }
    command -v cargo-zigbuild >/dev/null 2>&1 || { echo "cargo-zigbuild not found — cargo install cargo-zigbuild" >&2; missing=1; }
    if command -v rustup >/dev/null 2>&1 && ! rustup target list --installed 2>/dev/null | grep -qx "$TRIPLE"; then
        echo "rust target $TRIPLE not installed — rustup target add $TRIPLE" >&2; missing=1
    fi
fi
[ "$missing" = 0 ] || exit 1

# --- the UI ------------------------------------------------------------------
# Embedded in the binary by rust-embed at compile time, so it is built on every
# path, cross or native. `npm ci` only when the lockfile is newer than the
# installed tree, so a rebuild is not a fresh install every time.
echo "==> Building the UI"
if [ ! -d "$ROOT/UI/web/node_modules" ] || [ "$ROOT/UI/web/package-lock.json" -nt "$ROOT/UI/web/node_modules" ]; then
    (cd "$ROOT/UI/web" && npm ci --ignore-scripts)
fi
(cd "$ROOT/UI/web" && npm run build)

# --- compile -----------------------------------------------------------------
# Every executable the crate defines. Keep in step with the [[bin]] list in
# cmd/Cargo.toml and with deploy/images/build-images.sh, which packages them.
BINARIES="huntwell website admin planning worker scheduling notification"

if [ -n "$TRIPLE" ]; then
    echo "==> Cross-compiling for $TRIPLE (zig)"
    (cd "$ROOT/cmd" && cargo zigbuild --release --locked --target "$TRIPLE" --bins)
    BUILT_DIR="$ROOT/cmd/target/$TRIPLE/release"
else
    echo "==> Building natively for $(uname -m)"
    (cd "$ROOT/cmd" && cargo build --release --locked --bins)
    BUILT_DIR="$ROOT/cmd/target/release"
fi

# --- collect and check -------------------------------------------------------
mkdir -p "$OUT"
for b in $BINARIES; do
    [ -f "$BUILT_DIR/$b$EXE" ] || { echo "build produced no $b at $BUILT_DIR/$b$EXE"; exit 1; }
    cp "$BUILT_DIR/$b$EXE" "$OUT/$b$EXE"
    chmod +x "$OUT/$b$EXE"
    # A wrong-architecture artifact is silent until it reaches the server, where
    # it reads as "exec format error". Checked per binary, not just once.
    if [ -n "$EXPECT" ]; then
        desc="$(file "$OUT/$b$EXE")"
        echo "$desc" | grep -qE "$EXPECT" || { echo "wrong output for $TARGET:"; echo "  $desc"; exit 1; }
    fi
done

# The glibc floor these impose on whatever runs them — the number that decides
# which base image works. They all come from one compile, so one is
# representative. objdump is not installed everywhere: information, not a gate.
if [ -n "$IMAGE_ARCH" ] && command -v objdump >/dev/null 2>&1; then
    glibc="$(objdump -T "$OUT/website" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
    [ -n "$glibc" ] && echo "    needs $glibc or newer"
fi

cp "$ROOT/local-infra/global.example" "$OUT/"
echo
echo "==> $OUT"
ls -lh "$OUT" | awk 'NR>1 {print "    "$5"\t"$9}'

# The packaging hand-off. The architecture is passed explicitly so the images
# wrap the binary just built, not whatever the packaging script would have
# defaulted to on this machine.
if [ "$WANT_IMAGES" = 1 ]; then
    # PUSH carries the registry, and is empty unless --push was actually given:
    # an exported REGISTRY must never be enough on its own to publish.
    push_to=""
    [ "$WANT_PUSH" = 1 ] && push_to="${REGISTRY:-}"
    ARCH="$IMAGE_ARCH" PUSH="$push_to" "$ROOT/deploy/images/build-images.sh"
fi

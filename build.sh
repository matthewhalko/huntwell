#!/usr/bin/env bash
# Build the Huntwell executable.
#
#   ./build.sh            Linux x86_64, cross-compiled with zig  -> bin/ubuntu/
#   ./build.sh arm64      Linux arm64,  cross-compiled with zig  -> bin/ubuntu-arm64/
#   ./build.sh windows    Windows x86_64, cross-compiled with zig -> bin/windows/
#   ./build.sh host       native build for this machine          -> bin/macos/ (or bin/linux/)
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
# To deploy, copy bin/ubuntu (or bin/ubuntu-arm64) into the admin's build
# folder and press Deploy: the admin pushes these executables into the VMs
# itself. There is nothing to package.
#
# One-time setup for the cross builds:
#
#   brew install zig
#   cargo install cargo-zigbuild
#   rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
#                     x86_64-pc-windows-gnu
#
# Which environment the binaries are for — as in Park River:
#
#   --prod   (default)  embeds genesis_prod, reads the Huntwell_Production secret
#   --local             embeds genesis_local, reads the Huntwell_Local secret
#
# The genesis key is COMPILED IN, so this is the one place the choice is made.
# HUNTWELL_GENESIS_DIR moves the key files out of the repo:
#
#   HUNTWELL_GENESIS_DIR=~/keys ./build.sh
#
# The binary reads the nearest encrypted `global` at or above its own folder
# (/yaksoft/bin/admin reads /yaksoft/global); see local-infra/global.example.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TARGET=""
GENESIS_VARIANT="prod"
for arg in "$@"; do
    case "$arg" in
        -h|--help) sed -n '2,46p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        --prod)  GENESIS_VARIANT="prod" ;;
        --local|--test) GENESIS_VARIANT="local" ;;
        --*) echo "unknown flag '$arg'"; exit 1 ;;
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
        LINUX=1; EXPECT="ELF 64-bit.*x86-64" ;;
    arm64|aarch64)
        TRIPLE="aarch64-unknown-linux-gnu"; OUT="$ROOT/bin/ubuntu-arm64"
        LINUX=1; EXPECT="ELF 64-bit.*aarch64" ;;
    windows|win)
        TRIPLE="x86_64-pc-windows-gnu";     OUT="$ROOT/bin/windows"
        LINUX="";  EXPECT="PE32\+ executable"; EXE=".exe" ;;
    host|native|macos|mac|darwin)
        TRIPLE="";          LINUX="";  EXPECT=""
        case "$(uname -s)" in Darwin) OUT="$ROOT/bin/macos" ;; *) OUT="$ROOT/bin/linux" ;; esac ;;
    *) echo "unknown target '$TARGET' — expected: ubuntu | arm64 | windows | host"; exit 1 ;;
esac

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

# --- genesis key ---------------------------------------------------------------
# A binary that cannot open its `global` fails on the server with "database URL
# is not set" — a long way from the cause. Refuse to build it instead.
GENESIS_DIR="${HUNTWELL_GENESIS_DIR:-$ROOT/local-infra}"
GENESIS_KEY="$GENESIS_DIR/genesis_$GENESIS_VARIANT"
[ -f "$GENESIS_KEY" ] || GENESIS_KEY="$GENESIS_DIR/genesis_$GENESIS_VARIANT.txt"
if [ -f "$GENESIS_KEY" ] || [ -n "${HUNTWELL_GENESIS_KEY:-}" ]; then
    echo "==> genesis: embedding the $(echo "$GENESIS_VARIANT" | tr a-z A-Z) key${HUNTWELL_GENESIS_KEY:+ from HUNTWELL_GENESIS_KEY}${HUNTWELL_GENESIS_KEY:-" from $GENESIS_KEY"}"
elif [ "${ALLOW_NO_GENESIS:-0}" = "1" ]; then
    echo "==> genesis: no key, and ALLOW_NO_GENESIS=1 — these binaries need a genesis_$GENESIS_VARIANT file beside them on the host."
else
    cat >&2 <<EOF

error: no genesis_$GENESIS_VARIANT in $GENESIS_DIR

  A --$GENESIS_VARIANT build embeds that genesis key. Without it the binary cannot
  decrypt its global. Point at the directory holding your keys:
    HUNTWELL_GENESIS_DIR=~/keys ./build.sh $TARGET --$GENESIS_VARIANT
  or, to build without a key on purpose (the host must then carry it):
    ALLOW_NO_GENESIS=1 ./build.sh $TARGET --$GENESIS_VARIANT

EOF
    exit 1
fi
export HUNTWELL_GENESIS_VARIANT="$GENESIS_VARIANT" HUNTWELL_GENESIS_DIR="$GENESIS_DIR"

# --- compile -----------------------------------------------------------------
# Every executable the crate defines. Keep in step with the [[bin]] list in
# cmd/Cargo.toml and with the roles in cmd/src/admin/incus_driver.rs, which
# decide which of them each VM is given.
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
# which VM image works. They all come from one compile, so one is
# representative. objdump is not installed everywhere: information, not a gate.
if [ -n "$LINUX" ] && command -v objdump >/dev/null 2>&1; then
    glibc="$(objdump -T "$OUT/website" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
    [ -n "$glibc" ] && echo "    needs $glibc or newer"
fi

cp "$ROOT/local-infra/global.example" "$OUT/"
echo
echo "==> $OUT"
ls -lh "$OUT" | awk 'NR>1 {print "    "$5"\t"$9}'

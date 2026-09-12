#!/usr/bin/env bash
# Start the Huntwell dev stack: the Rust server (debug build, --dev) and
# Vite with HMR proxying /api to it.
#
# Always stops a previous ./dev.sh for this checkout first (server, admin,
# pool workers, leftover runs, Vite), then builds and starts a new one.
# Postgres is left alone — start that with ./local-infra/start.sh.
#
#   ./dev.sh                  build, then server + Vite
#   ./dev.sh --setup-account  first: create the first sign-in account (prompts)
#   ./dev.sh --no-ui          server only (serves the last-built embedded UI)
#   INSTANCE=pra ./dev.sh     the instance ./local-infra/start.sh made under that name
#   UI_HOST=127.0.0.1 ./dev.sh   loopback-only (default binds 0.0.0.0 for LAN devices)
#   ADMIN_HOST=127.0.0.1 ./dev.sh   keep the admin control plane off the LAN
#
# Start the database first:
#   ./local-infra/start.sh
#
# LAN access: the listeners bind 0.0.0.0 by default, and macOS is asked to
# allow node/huntwell through the Application Firewall (best effort, needs
# cached sudo). Set UI_HOST=127.0.0.1 to keep everything on loopback.
#
# Open the Vite URL for development — it proxies /api (and the SSE log stream)
# to the server. The server also serves its embedded UI on its own port, but
# that page does not hot-reload.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

WITH_UI=1
SETUP_ACCOUNT=0
for arg in "$@"; do
  case "$arg" in
    --no-ui) WITH_UI=0 ;;
    --setup-account|--setup-admin|--bootstrap) SETUP_ACCOUNT=1 ;;
    -h|--help) sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  esac
done
UI_HOST="${UI_HOST:-0.0.0.0}"

INSTANCE="${INSTANCE:-default}"
export HUNTWELL_INSTANCE="$INSTANCE"
if [ "$INSTANCE" = "default" ]; then GLOBAL="$ROOT/local-infra/global"; else GLOBAL="$ROOT/local-infra/global-$INSTANCE"; fi
if [ ! -f "$GLOBAL" ]; then
  echo "error: $GLOBAL not found — run:  INSTANCE=$INSTANCE ./local-infra/start.sh" >&2
  exit 1
fi
# Only the values the scripts need; the server reads the file itself.
API_ADDR="$(grep -E '^HUNTWELL_ADDR=' "$GLOBAL" | cut -d= -f2-)"
UI_PORT="$(grep -E '^HUNTWELL_UI_PORT=' "$GLOBAL" | cut -d= -f2-)"
API_PORT="${API_ADDR##*:}"
# Admin control plane address: from global (start.sh writes it), else derived
# so an older global file still works (admin = API - 1000, i.e. the 7611 family).
ADMIN_ADDR="$(grep -E '^HUNTWELL_ADMIN_ADDR=' "$GLOBAL" | cut -d= -f2- || true)"
[ -n "$ADMIN_ADDR" ] || ADMIN_ADDR="127.0.0.1:$(( API_PORT - 1000 ))"
ADMIN_PORT="${ADMIN_ADDR##*:}"
# Like the server, the admin binds where the UI does so LAN devices reach it.
# ADMIN_HOST=127.0.0.1 keeps just the privileged control plane on loopback
# while the app stays on the LAN; UI_HOST=127.0.0.1 pulls everything back.
ADMIN_HOST="${ADMIN_HOST:-$UI_HOST}"
ADMIN_ADDR="$ADMIN_HOST:$ADMIN_PORT"
# The server binds where the UI does, so LAN devices reach both.
API_ADDR="$UI_HOST:$API_PORT"
SERVER_BIN="$ROOT/cmd/target/debug/huntwell"
UI_DIR="$ROOT/UI/web"
# Same temp dir local-infra/start.sh uses for postgres/minio/nats pid files.
if [ "$INSTANCE" = "default" ]; then DATA_DIR="$ROOT/local-infra/data"; else DATA_DIR="$ROOT/local-infra/data-$INSTANCE"; fi
TMP_DIR="$DATA_DIR/temp"
DEV_PID_FILE="$TMP_DIR/dev.pid"
ADMIN_PID_FILE="$TMP_DIR/admin.pid"
SERVER_PID_FILE="$TMP_DIR/server.pid"
VITE_PID_FILE="$TMP_DIR/vite.pid"

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33m!! %s\033[0m\n' "$*" >&2; }
port_in_use() { lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1; }

write_pid() {
  mkdir -p "$TMP_DIR"
  printf '%s\n' "$2" > "$1"
}

# Same recycled-pid guard as local-infra/stop.sh: only kill if this pid is
# still the process we started ($expect matches comm or the command line).
stop_by_pidfile() {
  local label="$1" pid_file="$2" expect="$3" pid comm args
  [[ -f "$pid_file" ]] || return 0
  pid="$(tr -d '[:space:]' < "$pid_file")"
  [[ -n "$pid" && "$pid" != "$$" ]] || { rm -f "$pid_file"; return 0; }
  comm="$(ps -p "$pid" -o comm= 2>/dev/null || true)"
  args="$(ps -p "$pid" -o args= 2>/dev/null || true)"
  if [[ -z "$comm" ]]; then
    rm -f "$pid_file"
    return 0
  fi
  if [[ "$comm" != *"$expect"* && "$args" != *"$expect"* ]]; then
    warn "$label: pid $pid is now '$comm', not $expect — leaving it, removing stale pid file"
    rm -f "$pid_file"
    return 0
  fi
  printf '    stopping %s (pid %s)\n' "$label" "$pid"
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 20); do
    kill -0 "$pid" 2>/dev/null || { rm -f "$pid_file"; return 0; }
    sleep 0.1
  done
  if kill -0 "$pid" 2>/dev/null; then
    warn "Force-killing $label (pid $pid)"
    kill -9 "$pid" 2>/dev/null || true
  fi
  rm -f "$pid_file"
}

# Kill whatever is listening on a TCP port, then force-kill stragglers so a
# second ./dev.sh always gets a clean slate.
kill_port() {
  local port="$1" label="${2:-port $1}" pids
  pids="$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null | sort -u || true)"
  [[ -n "$pids" ]] || return 0
  printf '    stopping %s on :%s (pids %s)\n' "$label" "$port" "$(echo "$pids" | tr '\n' ' ')"
  # shellcheck disable=SC2086
  kill $pids 2>/dev/null || true
  for _ in $(seq 1 20); do port_in_use "$port" || return 0; sleep 0.1; done
  pids="$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null | sort -u || true)"
  [[ -n "$pids" ]] || return 0
  warn "Force-killing $label on :$port"
  # shellcheck disable=SC2086
  kill -9 $pids 2>/dev/null || true
  sleep 0.1
}

# Tear down a leftover ./dev.sh stack for this instance before we build or bind.
# Pid files live next to postgres.pid. Postgres / MinIO / NATS stay up.
stop_previous_stack() {
  step "Stopping any previous stack (instance $INSTANCE)"
  stop_by_pidfile "previous ./dev.sh" "$DEV_PID_FILE" "dev.sh"
  stop_by_pidfile "admin" "$ADMIN_PID_FILE" "huntwell"
  stop_by_pidfile "server" "$SERVER_PID_FILE" "huntwell"
  stop_by_pidfile "vite" "$VITE_PID_FILE" "vite"
  # Pool workers and leftover `run` children are not in a pid file — they
  # are spawned by admin and would double-claim if they survived it.
  # pgrep exits 1 when nothing matches; with pipefail that used to abort ./dev.sh.
  local leftovers="" pat
  pat="$(printf '%s' "$SERVER_BIN" | sed 's/[][().^$*+?{|}\\]/\\&/g')"
  leftovers="$(pgrep -f "$pat" 2>/dev/null | awk -v me="$$" '$1 != me { print }' | tr '\n' ' ' || true)"
  leftovers="${leftovers% }"
  if [[ -n "$leftovers" ]]; then
    printf '    stopping leftover huntwell (pids %s)\n' "$leftovers"
    # shellcheck disable=SC2086
    kill $leftovers 2>/dev/null || true
    sleep 0.2
    # shellcheck disable=SC2086
    kill -9 $leftovers 2>/dev/null || true
  fi
  kill_port "$API_PORT" "huntwell server"
  kill_port "$UI_PORT" "vite"
  kill_port "$ADMIN_PORT" "admin control plane"
}

lan_ipv4_addrs() {
  if command -v ip >/dev/null 2>&1; then
    ip -4 -o addr show scope global 2>/dev/null | awk '{print $4}' | cut -d/ -f1
  else
    ifconfig 2>/dev/null | awk '/inet / && $2 != "127.0.0.1" { print $2 }' | sed 's/addr://'
  fi
}

# Best-effort firewall open (never blocks startup). macOS Application
# Firewall only: allow the binaries that actually listen.
open_lan_firewall() {
  [[ "$UI_HOST" == "0.0.0.0" ]] || return 0
  case "$(uname -s)" in
    Darwin)
      local fw=/usr/libexec/ApplicationFirewall/socketfilterfw
      [[ -x "$fw" ]] || return 0
      if ! sudo -n true 2>/dev/null; then
        warn "No cached sudo — skipping the firewall allow-list (run 'sudo -v', then restart ./dev.sh)"
        return 0
      fi
      step "Allowing Node / huntwell through macOS Application Firewall"
      local node_bin; node_bin="$(command -v node || true)"
      for bin in "$node_bin" "$SERVER_BIN"; do
        [[ -n "$bin" && -x "$bin" ]] || continue
        if sudo -n "$fw" --add "$bin" >/dev/null && sudo -n "$fw" --unblockapp "$bin" >/dev/null; then
          printf '    allowed: %s\n' "$bin"
        else
          warn "Could not update Application Firewall for $bin"
        fi
      done
      ;;
  esac
}

wait_for() {
  local port="$1" label="$2" pid="$3"
  for _ in $(seq 1 600); do
    port_in_use "$port" && return 0
    if ! kill -0 "$pid" 2>/dev/null; then
      wait "$pid" || true
      echo "error: $label exited before binding port $port" >&2
      exit 1
    fi
    sleep 0.25
  done
  echo "error: timed out waiting for $label on port $port" >&2
  exit 1
}

stop_previous_stack
write_pid "$DEV_PID_FILE" "$$"

PG_PORT="$(grep -E '^HUNTWELL_DATABASE_URL=' "$GLOBAL" | sed -E 's/.*:([0-9]+)\/.*/\1/')"
if ! port_in_use "$PG_PORT"; then
  echo "error: no Postgres on port $PG_PORT — start it with:  INSTANCE=$INSTANCE ./local-infra/start.sh" >&2
  exit 1
fi

if [[ ! -d "$UI_DIR/node_modules" ]]; then
  step "Installing UI dependencies"
  (cd "$UI_DIR" && if [[ -f package-lock.json ]]; then npm ci; else npm install; fi)
fi

step "Building UI (UI/web/dist — embedded into the binary)"
(cd "$UI_DIR" && npm run build)

step "Building server"
cargo build --manifest-path "$ROOT/cmd/Cargo.toml"

# --- first account (only with --setup-account) -------------------------------
if [[ "$SETUP_ACCOUNT" == "1" ]]; then
  step "Creating the first sign-in account"
  read -r -p "  email: " ACCT_EMAIL
  read -r -s -p "  password (10+ chars): " ACCT_PW; echo
  if "$SERVER_BIN" account create --email "$ACCT_EMAIL" --password "$ACCT_PW"; then
    echo "  You can now sign in; set HUNTWELL_OPEN_SIGNUP=0 in $GLOBAL to close sign-up."
  else
    warn "account setup did not complete — continuing to start the stack anyway"
  fi
fi

cleanup() {
  trap - INT TERM EXIT
  for pid in "${UI_PID:-}" "${SERVER_PID:-}" "${ADMIN_PID:-}"; do
    [[ -n "$pid" ]] || continue
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
  pkill -f "huntwell worker-pool" 2>/dev/null || true
  rm -f "$DEV_PID_FILE" "$ADMIN_PID_FILE" "$SERVER_PID_FILE" "$VITE_PID_FILE"
}
trap cleanup INT TERM EXIT

# --- admin control plane -----------------------------------------------------
# Operator credentials live in the global file so they survive restarts; a
# fresh checkout gets a generated password (printed below).
ADMIN_EMAIL="$(grep -E '^HUNTWELL_ADMIN_EMAIL=' "$GLOBAL" | cut -d= -f2- || true)"
if [[ -z "$ADMIN_EMAIL" ]]; then
  ADMIN_EMAIL="admin@local.test"
  ADMIN_PASS="$(LC_ALL=C head -c 24 /dev/urandom | base64 | tr -d '\n=/+' | head -c 16)"
  {
    echo "HUNTWELL_ADMIN_EMAIL=$ADMIN_EMAIL"
    echo "HUNTWELL_ADMIN_PASSWORD=$ADMIN_PASS"
  } >> "$GLOBAL"
fi
ADMIN_PASS="$(grep -E '^HUNTWELL_ADMIN_PASSWORD=' "$GLOBAL" | cut -d= -f2- || true)"

step "Starting admin control plane on http://$ADMIN_ADDR (local pool of ${LOCAL_POOL:-2} workers)"
HUNTWELL_LOCAL_POOL="${LOCAL_POOL:-2}" "$SERVER_BIN" admin --addr "$ADMIN_ADDR" &
ADMIN_PID=$!
write_pid "$ADMIN_PID_FILE" "$ADMIN_PID"
wait_for "$ADMIN_PORT" "admin" "$ADMIN_PID"

# Runs route through the pool (admin + local workers) by default, exercising
# the same placement path production uses. RUN_DISPATCH=local ./dev.sh opts out.
RUN_DISPATCH="${RUN_DISPATCH:-pool}"
export RUN_DISPATCH

step "Starting server on http://$API_ADDR (RUN_DISPATCH=$RUN_DISPATCH)"
"$SERVER_BIN" serve --dev --addr "$API_ADDR" &
SERVER_PID=$!
write_pid "$SERVER_PID_FILE" "$SERVER_PID"
wait_for "$API_PORT" "server" "$SERVER_PID"

if [[ "$WITH_UI" == "1" ]]; then
  step "Starting Vite on http://127.0.0.1:$UI_PORT"
  (cd "$UI_DIR" && HUNTWELL_API_PORT="$API_PORT" HUNTWELL_UI_PORT="$UI_PORT" npm run dev -- --host "$UI_HOST" --port "$UI_PORT" --strictPort) &
  UI_PID=$!
  wait_for "$UI_PORT" "vite" "$UI_PID"
  # The job pid is the bash wrapper; the pid file records the listener so a
  # later ./dev.sh kills Vite itself, not an unrelated recycled bash.
  listen_pid="$(lsof -nP -iTCP:"$UI_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1 || true)"
  [[ -n "$listen_pid" ]] && write_pid "$VITE_PID_FILE" "$listen_pid"
fi

open_lan_firewall

printf '\n  Ready — open these:\n\n'
[[ "$WITH_UI" == "1" ]] && printf '    App (HMR)      http://127.0.0.1:%s\n' "$UI_PORT"
printf '    Server         http://127.0.0.1:%s   (embedded UI, no HMR)\n' "$API_PORT"
printf '    Admin          http://127.0.0.1:%s   (%s / %s)\n' "$ADMIN_PORT" "$ADMIN_EMAIL" "$ADMIN_PASS"
printf '    Health         http://127.0.0.1:%s/healthz\n' "$API_PORT"
if [[ "$UI_HOST" == "0.0.0.0" ]]; then
  printf '\n'
  while IFS= read -r addr; do
    [[ -n "$addr" ]] || continue
    [[ "$WITH_UI" == "1" ]] && printf '    LAN app        http://%s:%s\n' "$addr" "$UI_PORT"
    printf '    LAN server     http://%s:%s\n' "$addr" "$API_PORT"
    [[ "$ADMIN_HOST" == "0.0.0.0" ]] && printf '    LAN admin      http://%s:%s\n' "$addr" "$ADMIN_PORT"
  done < <(lan_ipv4_addrs)
  printf '    tip: if LAN is blocked, check macOS Settings > Network > Firewall\n'
fi
printf '\n    Ctrl-C stops all.\n\n'

# Exit as soon as any one of them dies, so a crashed process is not left
# hiding behind the other. (Bash 3.2 on macOS has no `wait -n`.)
EXIT_CODE=0
while true; do
  for pid in ${UI_PID:+"$UI_PID"} "$SERVER_PID" "$ADMIN_PID"; do
    if ! kill -0 "$pid" 2>/dev/null; then
      wait "$pid" 2>/dev/null || EXIT_CODE=$?
      cleanup
      exit "$EXIT_CODE"
    fi
  done
  sleep 0.5
done

#!/usr/bin/env bash
# Start the Huntwell admin control plane in the background, detached from this
# shell, so it keeps running when your SSH session ends.
#
#   ./start-admin.sh                                  admin beside this script (or in bin/), on 10.121.17.152:8710
#   ADDR=10.121.17.152:8710 ./start-admin.sh          another address
#   ADMIN_BIN=/opt/huntwell/admin ./start-admin.sh    the executable somewhere else
#
# It runs from the executable's own directory, where `global`, `genesis` and
# build/ are looked for. Output is appended to admin.log and the process id
# kept in admin.pid, both beside the executable. ./stop-admin.sh stops it.
#
# This does not bring it back after a crash or a reboot; the systemd unit in
# deploy/huntwell-admin.service.in does (Restart=always), if you want that.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
# The executable: ADMIN_BIN, else `admin` beside this script, else in bin/
# beside it (e.g. scripts in /yaksoft, the binary in /yaksoft/bin).
if [ -n "${ADMIN_BIN:-}" ]; then
  BIN="$ADMIN_BIN"
elif [ -x "$here/admin" ]; then
  BIN="$here/admin"
else
  BIN="$here/bin/admin"
fi
ADDR="${ADDR:-10.121.17.152:8710}"

[ -x "$BIN" ] || { echo "error: no admin executable at $BIN or $here/admin (set ADMIN_BIN=/path/to/admin)" >&2; exit 1; }
DIR="$(cd "$(dirname "$BIN")" && pwd)"
BIN="$DIR/$(basename "$BIN")"
PID_FILE="${PID_FILE:-$DIR/admin.pid}"
LOG_FILE="${LOG_FILE:-$DIR/admin.log}"

if [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
  echo "admin is already running (pid $(cat "$PID_FILE")) — ./stop-admin.sh first" >&2
  exit 1
fi
# The same thing started by hand, without this script's pid file.
if running="$(pgrep -f "$BIN serve" | head -n1)" && [ -n "$running" ]; then
  echo "admin is already running (pid $running, not started by this script) — stop it first" >&2
  exit 1
fi

cd "$DIR"
# setsid: its own session, so the SSH hangup never reaches it. nohup: ignore
# a hangup anyway. stdin from /dev/null: never waits on a terminal.
setsid nohup "$BIN" serve --addr "$ADDR" >>"$LOG_FILE" 2>&1 </dev/null &

# setsid may fork, so the pid is looked up rather than taken from $!.
pid=""
for _ in $(seq 1 20); do
  pid="$(pgrep -n -f "$BIN serve --addr $ADDR" || true)"
  [ -n "$pid" ] && break
  sleep 0.1
done
if [ -z "$pid" ]; then
  echo "error: admin did not start — last lines of $LOG_FILE:" >&2
  tail -n 20 "$LOG_FILE" >&2 || true
  exit 1
fi
echo "$pid" >"$PID_FILE"

# A bad setting or a taken port ends it within a moment: say so now.
sleep 2
if ! kill -0 "$pid" 2>/dev/null; then
  rm -f "$PID_FILE"
  echo "error: admin exited right after starting — last lines of $LOG_FILE:" >&2
  tail -n 20 "$LOG_FILE" >&2 || true
  exit 1
fi
echo "admin started (pid $pid) on http://$ADDR"
echo "  log:  tail -f $LOG_FILE"
echo "  stop: $here/stop-admin.sh"

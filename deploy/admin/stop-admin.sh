#!/usr/bin/env bash
# Stop the admin control plane ./start-admin.sh started: asks it to shut down
# (SIGTERM), waits up to 20 seconds, then forces it (SIGKILL).
#
#   ./stop-admin.sh
#   ADMIN_BIN=/opt/huntwell/admin ./stop-admin.sh     the executable somewhere else
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
DIR="$(cd "$(dirname "$BIN")" 2>/dev/null && pwd || echo "$here")"
BIN="$DIR/$(basename "$BIN")"
PID_FILE="${PID_FILE:-$DIR/admin.pid}"

pid=""
if [ -f "$PID_FILE" ]; then
  pid="$(cat "$PID_FILE")"
  kill -0 "$pid" 2>/dev/null || pid=""
fi
# No pid file (or a stale one): find it by its command line instead.
[ -n "$pid" ] || pid="$(pgrep -n -f "$BIN serve" || true)"
if [ -z "$pid" ]; then
  echo "admin is not running"
  rm -f "$PID_FILE"
  exit 0
fi

kill -TERM "$pid" 2>/dev/null || true
for _ in $(seq 1 40); do
  kill -0 "$pid" 2>/dev/null || { rm -f "$PID_FILE"; echo "admin stopped (pid $pid)"; exit 0; }
  sleep 0.5
done
echo "admin did not stop within 20s — forcing it (pid $pid)" >&2
kill -KILL "$pid" 2>/dev/null || true
rm -f "$PID_FILE"
echo "admin stopped (pid $pid)"

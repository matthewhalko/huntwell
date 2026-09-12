#!/usr/bin/env bash
# Stop one Huntwell instance's services (Postgres, MinIO). INSTANCE picks
# which (default: default).
set -uo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ -f "$DIR/config" ] && { INSTANCE_FROM_ENV="${INSTANCE:-}"; source "$DIR/config"; [ -n "$INSTANCE_FROM_ENV" ] && INSTANCE="$INSTANCE_FROM_ENV"; }
INSTANCE="${INSTANCE:-default}"
INSTANCE="$(printf '%s' "$INSTANCE" | tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9]+/-/g; s/^-+//; s/-+$//')"
[ -n "$INSTANCE" ] || INSTANCE=default
if [ "$INSTANCE" = "default" ]; then DATA_DIR="$DIR/data"; else DATA_DIR="$DIR/data-$INSTANCE"; fi
TMP_DIR="$DATA_DIR/temp"
PG_PID_FILE="$TMP_DIR/postgres.pid"
NATS_PID_FILE="$TMP_DIR/nats.pid"
MINIO_PID_FILE="$TMP_DIR/minio.pid"
MINIO_CONTAINER_FILE="$TMP_DIR/minio.container"

# stop_by_pidfile <label> <pidfile> <expected process name> <signal>
# Only kills the stored pid if it is alive AND still the process we started,
# so a recycled pid can never take down an unrelated process.
stop_by_pidfile() {
    local label="$1" pid_file="$2" expect="$3" signal="$4" pid comm

    if [ ! -f "$pid_file" ]; then
        echo "$label: not running (no pid file)"
        return
    fi

    pid="$(cat "$pid_file")"
    comm="$(ps -p "$pid" -o comm= 2>/dev/null || true)"

    if [ -z "$comm" ]; then
        echo "$label: not running (stale pid $pid)"
        rm -f "$pid_file"
        return
    fi

    if [[ "$comm" != *"$expect"* ]]; then
        echo "$label: pid $pid is now '$comm', not $expect — refusing to kill, removing stale pid file"
        rm -f "$pid_file"
        return
    fi

    kill "-$signal" "$pid"
    for _ in $(seq 1 40); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.25
    done

    if kill -0 "$pid" 2>/dev/null; then
        echo "$label: pid $pid did not exit within 10s"
    else
        echo "$label: stopped (pid $pid)"
        rm -f "$pid_file"
    fi
}

# SIGINT = postgres "fast" shutdown (same as pg_ctl -m fast)
stop_by_pidfile "postgres" "$PG_PID_FILE" "postgres" "INT"
stop_by_pidfile "nats" "$NATS_PID_FILE" "nats-server" "TERM"
stop_by_pidfile "minio" "$MINIO_PID_FILE" "minio" "TERM"
# The Docker fallback (see start.sh) is stopped by container name instead.
if [ -f "$MINIO_CONTAINER_FILE" ]; then
    name="$(cat "$MINIO_CONTAINER_FILE")"
    if docker rm -f "$name" >/dev/null 2>&1; then
        echo "minio:    container $name removed"
    fi
    rm -f "$MINIO_CONTAINER_FILE"
fi

# Reset by default: clear the instance's data so every start/stop is a clean
# slate (Postgres cluster, Chrome profiles and agent workspaces all live under
# DATA_DIR). The env file with the API keys lives outside DATA_DIR and is kept.
# KEEP=1 ./stop.sh stops without wiping, to preserve accounts/plans across a
# restart. start.sh forwards KEEP, so `KEEP=1 ./start.sh` also preserves.
if [ "${KEEP:-0}" = "1" ]; then
    echo "data:     kept $DATA_DIR (KEEP=1)"
elif [ -d "$DATA_DIR" ]; then
    rm -rf "$DATA_DIR"
    echo "data:     cleared $DATA_DIR (KEEP=1 to preserve)"
fi

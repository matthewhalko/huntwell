#!/usr/bin/env bash
# Bring up one isolated Huntwell infrastructure: a repo-local Postgres
# cluster with the schema applied, plus the env file the server reads.
#
#   ./local-infra/start.sh                 fresh start — resets Postgres (clean slate)
#   KEEP=1 ./local-infra/start.sh          start but preserve existing data
#   INSTANCE=pra ./local-infra/start.sh    a second, fully separate instance
#   ./local-infra/stop.sh                  stop and clear (KEEP=1 to preserve)
#
# By default every start and stop resets the instance's data, so you always come
# up on an empty database. The gitignored `global` env file (API keys, session
# secret) lives outside data/ and is preserved.
#
# Nothing here touches /usr/local, needs root, or shares state with another
# checkout: the cluster, its logs, pid files and Chrome profiles all live under
# local-infra/data-<INSTANCE>/ (data/ for the default instance). Settings come
# from local-infra/config (see config.example); the environment overrides it.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG_FILE="$DIR/config"
if [ -f "$CONFIG_FILE" ]; then
    # shellcheck source=/dev/null
    source "$CONFIG_FILE"
fi

# --- instance and ports -------------------------------------------------------
INSTANCE="${INSTANCE:-default}"
INSTANCE="$(printf '%s' "$INSTANCE" | tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9]+/-/g; s/^-+//; s/-+$//')"
[ -n "$INSTANCE" ] || INSTANCE=default

# A stable offset per name so a second instance never has to be given ports
# by hand. Same formula as the original huntwell's HUNTWELL_INSTANCE.
instance_offset() {
    local name="$1" h=0 i c
    [ "$name" = "default" ] && { echo 0; return; }
    for ((i = 0; i < ${#name}; i++)); do
        c=$(printf '%d' "'${name:$i:1}")
        h=$(( (h * 31 + c) % 2147483647 ))
    done
    echo $(( (1 + h % 40) * 100 ))
}
PORT_OFFSET="${PORT_OFFSET:-$(instance_offset "$INSTANCE")}"

BIND_ADDR="${BIND_ADDR:-127.0.0.1}"
PG_PORT=$(( ${PG_PORT:-6511} + PORT_OFFSET ))
API_PORT=$(( ${API_PORT:-8611} + PORT_OFFSET ))
UI_PORT=$(( ${UI_PORT:-5611} + PORT_OFFSET ))
ADMIN_PORT=$(( ${ADMIN_PORT:-7611} + PORT_OFFSET ))
S3_PORT=$(( ${S3_PORT:-9611} + PORT_OFFSET ))
S3_CONSOLE_PORT=$(( ${S3_CONSOLE_PORT:-9612} + PORT_OFFSET ))
NATS_PORT=$(( ${NATS_PORT:-4611} + PORT_OFFSET ))
NATS_MONITOR_PORT=$(( ${NATS_MONITOR_PORT:-4612} + PORT_OFFSET ))
CDP_PORT_BASE=$(( ${CDP_PORT_BASE:-20611} + PORT_OFFSET ))
S3_BUCKET="${S3_BUCKET:-huntwell}"
S3_ACCESS_KEY="${S3_ACCESS_KEY:-huntwell}"
S3_SECRET_KEY="${S3_SECRET_KEY:-huntwell-dev-secret}"
PG_USER="${PG_USER:-postgres}"
DB_NAME="${DB_NAME:-huntwell}"

if [ "$INSTANCE" = "default" ]; then
    DATA_DIR="$DIR/data"
    ENV_FILE="$DIR/global"
else
    DATA_DIR="$DIR/data-$INSTANCE"
    ENV_FILE="$DIR/global-$INSTANCE"
fi
PG_DATA="$DATA_DIR/postgres"
LOG_DIR="$DATA_DIR/logs"
APP_DATA="$DATA_DIR/huntwell"          # Chrome profiles, agent workspaces
TMP_DIR="$DATA_DIR/temp"
PG_PID_FILE="$TMP_DIR/postgres.pid"

# --- postgres binaries --------------------------------------------------------
# Not vendored: Homebrew or apt ship the same major, and 43 MB of binaries in
# git is a lot to ask for a `pg_ctl`. Set PG_BIN in config to pin one.
find_pg_bin() {
    local candidates=(
        "${PG_BIN:-}"
        "$DIR/applications/postgres/bin"
        /opt/homebrew/opt/postgresql@18/bin
        /opt/homebrew/bin
        /usr/local/opt/postgresql@18/bin
        /usr/lib/postgresql/18/bin
        /usr/lib/postgresql/17/bin
        /usr/lib/postgresql/16/bin
    )
    local c
    for c in "${candidates[@]}"; do
        # libpq installs ship initdb/pg_ctl without the server itself, so the
        # test is for `postgres`, not `pg_ctl`.
        [ -n "$c" ] && [ -x "$c/pg_ctl" ] && [ -x "$c/postgres" ] && { echo "$c"; return; }
    done
    if command -v postgres >/dev/null 2>&1; then
        dirname "$(command -v postgres)"; return
    fi
    return 1
}
PG_BIN="$(find_pg_bin)" || {
    echo "postgres: no pg_ctl found — install PostgreSQL 16+ (brew install postgresql@18) or set PG_BIN in local-infra/config"
    exit 1
}
# A vendored copy needs to find its own libs.
if [ -d "$DIR/applications/postgres/lib" ]; then
    export DYLD_LIBRARY_PATH="$DIR/applications/postgres/lib${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
    export LD_LIBRARY_PATH="$DIR/applications/postgres/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi

echo "instance: $INSTANCE  (offset $PORT_OFFSET)  pg $PG_PORT · api $API_PORT · ui $UI_PORT · admin $ADMIN_PORT · s3 $S3_PORT · cdp $CDP_PORT_BASE+"
echo "postgres: using $PG_BIN ($("$PG_BIN/pg_ctl" --version | awk '{print $NF}'))"

# --- stop the running instance, and (by default) clear its data --------------
# stop.sh does the wipe so a bare `./stop.sh` also resets; KEEP=1 is forwarded so
# `KEEP=1 ./start.sh` preserves the existing cluster instead.
INSTANCE="$INSTANCE" KEEP="${KEEP:-0}" "$DIR/stop.sh"
mkdir -p "$LOG_DIR" "$TMP_DIR" "$APP_DATA"

FAILED=0

# --- postgres -----------------------------------------------------------------
if [ ! -d "$PG_DATA" ]; then
    echo "postgres: initialising new cluster at $PG_DATA"
    if ! "$PG_BIN/initdb" -D "$PG_DATA" -U "$PG_USER" -A trust -E UTF8 --no-instructions \
        > "$LOG_DIR/initdb.log" 2>&1; then
        echo "postgres: FAILED initdb — last lines of $LOG_DIR/initdb.log:"
        tail -5 "$LOG_DIR/initdb.log" | sed 's/^/          /'
        exit 1
    fi
fi

if "$PG_BIN/pg_ctl" -D "$PG_DATA" -l "$LOG_DIR/postgres.log" \
        -o "-p $PG_PORT -c listen_addresses=$BIND_ADDR -c unix_socket_directories=$TMP_DIR" -w start > /dev/null 2>&1; then
    head -1 "$PG_DATA/postmaster.pid" > "$PG_PID_FILE"
    echo "postgres: OK  $BIND_ADDR:$PG_PORT  pid $(cat "$PG_PID_FILE")"

    DB_LOG="$LOG_DIR/db-init.log"
    : > "$DB_LOG"
    psql_admin() {
        "$PG_BIN/psql" -h "$BIND_ADDR" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>>"$DB_LOG"
    }
    if [ "$(psql_admin "SELECT 1 FROM pg_database WHERE datname = '$DB_NAME'" || true)" = "1" ]; then
        echo "postgres: database '$DB_NAME' already present"
    elif "$PG_BIN/createdb" -h "$BIND_ADDR" -p "$PG_PORT" -U "$PG_USER" "$DB_NAME" >> "$DB_LOG" 2>&1; then
        echo "postgres: database '$DB_NAME' created"
    else
        echo "postgres: FAILED creating database '$DB_NAME' — see $DB_LOG"
        FAILED=1
    fi

    # --- schema from db/ ----------------------------------------------------
    # Every file is idempotent (CREATE ... IF NOT EXISTS), so this is safe on a
    # cluster that already has data. The server applies the same files at
    # startup; this pass just means `psql` works before the server is built.
    psql_db() {
        "$PG_BIN/psql" -v ON_ERROR_STOP=1 -q -h "$BIND_ADDR" -p "$PG_PORT" \
            -U "$PG_USER" -d "$DB_NAME" "$@" >> "$DB_LOG" 2>&1
    }
    DDL_COUNT=0
    DB_OK=1
    while IFS= read -r f; do
        case "$f" in ''|\#*) continue ;; esac
        if psql_db -f "$DIR/db/public/$f"; then
            DDL_COUNT=$((DDL_COUNT + 1))
        else
            echo "db:       FAILED applying $f — see $DB_LOG"
            DB_OK=0
        fi
    done < "$DIR/db/schema.order"

    CSV_COUNT=0
    # Filename is schema.table, snake_case, matching the SQL files
    # (public.account, public.account_browser).
    for csv in "$DIR"/db/data/*.csv; do
        [ -e "$csv" ] || continue
        base="$(basename "$csv" .csv)"      # e.g. public.account
        schema="${base%%.*}"
        table="${base#*.}"
        # Seeds only ever go into an empty table; a cluster with data keeps it.
        rows="$("$PG_BIN/psql" -h "$BIND_ADDR" -p "$PG_PORT" -U "$PG_USER" -d "$DB_NAME" -tAc "SELECT count(*) FROM \"$schema\".\"$table\"" 2>>"$DB_LOG" || echo x)"
        [ "$rows" = "0" ] || continue
        if sed $'1s/^\xef\xbb\xbf//' "$csv" | \
           psql_db -c "\\copy \"$schema\".\"$table\" FROM STDIN WITH (FORMAT csv, NULL 'NULL')"; then
            # advance identity sequences past the seeded ids
            psql_db -c "DO \$\$
                DECLARE col text; seq text; maxid bigint;
                BEGIN
                  FOR col IN SELECT attname FROM pg_attribute
                    WHERE attrelid = '\"$schema\".\"$table\"'::regclass AND attidentity <> ''
                  LOOP
                    seq := pg_get_serial_sequence('\"$schema\".\"$table\"', col);
                    IF seq IS NOT NULL THEN
                      EXECUTE format('SELECT coalesce(max(%I), 0) FROM %s', col, '\"$schema\".\"$table\"') INTO maxid;
                      IF maxid > 0 THEN PERFORM setval(seq, maxid); END IF;
                    END IF;
                  END LOOP;
                END \$\$;" || DB_OK=0
            CSV_COUNT=$((CSV_COUNT + 1))
        else
            echo "db:       FAILED loading $csv — see $DB_LOG"
            DB_OK=0
        fi
    done

    if [ "$DB_OK" -eq 1 ]; then
        echo "db:       OK  applied $DDL_COUNT ddl files, loaded $CSV_COUNT csv files"
    else
        FAILED=1
    fi
    echo "postgres: postgres://$PG_USER@$BIND_ADDR:$PG_PORT/$DB_NAME"
else
    rm -f "$PG_PID_FILE"
    echo "postgres: FAILED to start — last lines of $LOG_DIR/postgres.log:"
    tail -5 "$LOG_DIR/postgres.log" 2>/dev/null | sed 's/^/          /'
    FAILED=1
fi

# --- minio (object store for collected files) --------------------------------
# Assets plans keep the files they download here. Not vendored, same reasoning
# as Postgres: `brew install minio` (or apt) ships it. Without it huntwell
# falls back to a directory under the data dir, which works on one machine but
# not for pool runs on other hosts.
MINIO_PID_FILE="$TMP_DIR/minio.pid"
MINIO_CONTAINER_FILE="$TMP_DIR/minio.container"
MINIO_CONTAINER="huntwell-minio-$INSTANCE"
S3_ENDPOINT=""

s3_live() { curl -fsS -m 1 "http://$BIND_ADDR:$S3_PORT/minio/health/live" >/dev/null 2>&1; }
wait_for_s3() { for _ in $(seq 1 "$1"); do s3_live && return 0; sleep 0.4; done; return 1; }

mkdir -p "$DATA_DIR/minio"
# Native binary first. Homebrew's arm64 build segfaults in a cgo CPU probe on
# some macOS versions, so the health check decides — never the mere presence
# of the binary.
if command -v minio >/dev/null 2>&1; then
    MINIO_ROOT_USER="$S3_ACCESS_KEY" MINIO_ROOT_PASSWORD="$S3_SECRET_KEY" \
        nohup minio server "$DATA_DIR/minio" \
            --address "$BIND_ADDR:$S3_PORT" \
            --console-address "$BIND_ADDR:$S3_CONSOLE_PORT" \
            > "$LOG_DIR/minio.log" 2>&1 &
    echo $! > "$MINIO_PID_FILE"
    if wait_for_s3 20; then
        S3_ENDPOINT="http://$BIND_ADDR:$S3_PORT"
        echo "minio:    OK  $S3_ENDPOINT  (console :$S3_CONSOLE_PORT)  bucket '$S3_BUCKET'  pid $(cat "$MINIO_PID_FILE")"
    else
        kill "$(cat "$MINIO_PID_FILE")" 2>/dev/null || true
        rm -f "$MINIO_PID_FILE"
        echo "minio:    the local binary did not start (see $LOG_DIR/minio.log) — trying Docker"
    fi
fi
# Docker fallback: the Linux image works where the native build does not.
if [ -z "$S3_ENDPOINT" ] && command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    docker rm -f "$MINIO_CONTAINER" >/dev/null 2>&1 || true
    if docker run -d --name "$MINIO_CONTAINER" \
            -p "$BIND_ADDR:$S3_PORT:9000" -p "$BIND_ADDR:$S3_CONSOLE_PORT:9001" \
            -e "MINIO_ROOT_USER=$S3_ACCESS_KEY" -e "MINIO_ROOT_PASSWORD=$S3_SECRET_KEY" \
            -v "$DATA_DIR/minio:/data" \
            minio/minio server /data --console-address ":9001" \
            >> "$LOG_DIR/minio.log" 2>&1; then
        if wait_for_s3 40; then
            echo "$MINIO_CONTAINER" > "$MINIO_CONTAINER_FILE"
            S3_ENDPOINT="http://$BIND_ADDR:$S3_PORT"
            echo "minio:    OK  $S3_ENDPOINT  (console :$S3_CONSOLE_PORT)  bucket '$S3_BUCKET'  container $MINIO_CONTAINER"
        else
            docker rm -f "$MINIO_CONTAINER" >/dev/null 2>&1 || true
            echo "minio:    the container did not become healthy — see $LOG_DIR/minio.log"
        fi
    else
        echo "minio:    could not start the container — see $LOG_DIR/minio.log"
    fi
fi
if [ -z "$S3_ENDPOINT" ]; then
    echo "minio:    unavailable — collected files fall back to $APP_DATA/objects"
    echo "          (brew install minio, or start Docker) — the fallback is single-machine only"
fi

# --- write the env file, preserving anything that isn't ours -----------------
OWNED='^(HUNTWELL_DATABASE_URL|HUNTWELL_ADDR|HUNTWELL_INSTANCE|HUNTWELL_CDP_PORT_BASE|HUNTWELL_DATA_DIR|HUNTWELL_UI_PORT|HUNTWELL_ADMIN_ADDR|HUNTWELL_S3_[A-Z_]+)='
PRESERVED=""
if [ -f "$ENV_FILE" ]; then
    PRESERVED="$(grep -vE "$OWNED" "$ENV_FILE" || true)"
fi
# --- nats (the event bus) ----------------------------------------------------
# Every service publishes what it did and subscribes to what it cares about.
# Vendored, unlike Postgres and MinIO: one static Go binary with no runtime and
# no dependencies, so `applications/nats/fetch.sh` is the whole install.
#
# JetStream is on. Core NATS drops a message with no listener, which is right
# for "something happened" but wrong for the moment a service is restarting;
# the stream holds those until it comes back.
NATS_BIN="${NATS_BIN:-$DIR/applications/nats/nats-server}"
[ -x "$NATS_BIN" ] || NATS_BIN="$(command -v nats-server 2>/dev/null || true)"
NATS_PID_FILE="$TMP_DIR/nats.pid"
NATS_URL=""

nats_live() { curl -fsS -m 1 "http://$BIND_ADDR:$NATS_MONITOR_PORT/healthz" >/dev/null 2>&1; }

if [ -z "$NATS_BIN" ]; then
    echo "nats:     not found — fetch it once with:"
    echo "          ./local-infra/applications/nats/fetch.sh"
    echo "          (without it the services run with no bus: events are dropped, nothing else changes)"
elif nats_live; then
    NATS_URL="nats://$BIND_ADDR:$NATS_PORT"
    echo "nats:     already running  $NATS_URL"
else
    mkdir -p "$DATA_DIR/nats"
    nohup "$NATS_BIN" \
        --addr "$BIND_ADDR" --port "$NATS_PORT" \
        --http_port "$NATS_MONITOR_PORT" \
        --jetstream --store_dir "$DATA_DIR/nats" \
        --name "huntwell-$INSTANCE" \
        > "$LOG_DIR/nats.log" 2>&1 &
    echo $! > "$NATS_PID_FILE"
    for _ in $(seq 1 25); do nats_live && break; sleep 0.2; done
    if nats_live; then
        NATS_URL="nats://$BIND_ADDR:$NATS_PORT"
        echo "nats:     OK  $NATS_URL  (monitor :$NATS_MONITOR_PORT)  pid $(cat "$NATS_PID_FILE")"
    else
        rm -f "$NATS_PID_FILE"
        echo "nats:     FAILED to start — last lines of $LOG_DIR/nats.log:"
        tail -5 "$LOG_DIR/nats.log" 2>/dev/null | sed 's/^/          /'
        echo "          the services will run without a bus rather than not at all"
    fi
fi

# A session secret is minted once and then preserved with everything else.
if ! printf '%s\n' "$PRESERVED" | grep -qE '^HUNTWELL_SESSION_SECRET='; then
    # Fixed-size read, then trim in the shell: a trailing `head -c` in a pipe
    # kills its upstream with SIGPIPE, which `set -o pipefail` turns into a
    # failed start. (start.sh does not set pipefail today; this is here so it
    # stays correct if it ever does.)
    secret="$(LC_ALL=C od -An -tx1 -N24 /dev/urandom | tr -d ' \n')"
    PRESERVED="$(printf '%s\n%s' "$PRESERVED" "HUNTWELL_SESSION_SECRET=$secret")"
fi
# CURSOR_API_KEY from the shell is handed through once so a fresh checkout
# does not need a manual edit to run its first scrape.
if [ -n "${CURSOR_API_KEY:-}" ] && ! printf '%s\n' "$PRESERVED" | grep -qE '^CURSOR_API_KEY='; then
    PRESERVED="$(printf '%s\n%s' "$PRESERVED" "CURSOR_API_KEY=$CURSOR_API_KEY")"
fi
{
    echo "HUNTWELL_DATABASE_URL=postgres://$PG_USER@$BIND_ADDR:$PG_PORT/$DB_NAME"
    echo "HUNTWELL_ADDR=$BIND_ADDR:$API_PORT"
    echo "HUNTWELL_UI_PORT=$UI_PORT"
    echo "HUNTWELL_ADMIN_ADDR=127.0.0.1:$ADMIN_PORT"
    # Only written when MinIO actually came up; otherwise huntwell uses its
    # local-filesystem fallback.
    if [ -n "$S3_ENDPOINT" ]; then
        echo "HUNTWELL_S3_ENDPOINT=$S3_ENDPOINT"
        echo "HUNTWELL_S3_BUCKET=$S3_BUCKET"
        echo "HUNTWELL_S3_ACCESS_KEY=$S3_ACCESS_KEY"
        echo "HUNTWELL_S3_SECRET_KEY=$S3_SECRET_KEY"
    fi
    # Only written when NATS actually came up. Unset means no bus, which the
    # services treat as "publish nowhere" rather than as a failure.
    if [ -n "$NATS_URL" ]; then
        echo "HUNTWELL_NATS_URL=$NATS_URL"
    fi
    echo "HUNTWELL_INSTANCE=$INSTANCE"
    echo "HUNTWELL_CDP_PORT_BASE=$CDP_PORT_BASE"
    echo "HUNTWELL_DATA_DIR=$APP_DATA"
    if [ -n "$PRESERVED" ]; then
        printf '%s\n' "$PRESERVED" | sed '/^$/d'
    fi
} > "$ENV_FILE"
chmod 600 "$ENV_FILE"
echo "env:      wrote $ENV_FILE"

exit $FAILED

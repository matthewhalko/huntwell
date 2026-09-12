#!/usr/bin/env bash
# Bring up the whole Huntwell stack on a local k3d cluster: build the two
# images, import them, apply the manifests, wait for the schema Jobs, and print
# the URL. Re-runnable — it reconciles an existing cluster.
#
#   ./k3d/up.sh            build + import + apply
#   SKIP_BUILD=1 ./k3d/up.sh   re-apply manifests only (fast iteration)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SECRETS="$ROOT/k8s/overlays/k3d/secrets.env"
cd "$ROOT"

# Isolation, matching the local-infra "isolated instance" scheme: the cluster
# name and its host port come from the instance (default, or HUNTWELL_INSTANCE
# / local-infra/config's INSTANCE). Two checkouts — or a sibling project's k3d
# cluster — never share a name or a port.
INSTANCE="${HUNTWELL_INSTANCE:-$(sed -n 's/^INSTANCE=//p' local-infra/config 2>/dev/null | head -1)}"
INSTANCE="${INSTANCE:-default}"
if [ -z "${CLUSTER:-}" ]; then
  [ "$INSTANCE" = default ] && CLUSTER="huntwell" || CLUSTER="huntwell-$INSTANCE"
fi

# Ports already mapped by ANY k3d cluster's loadbalancer — running OR stopped —
# so we never reuse a stopped cluster's port and collide when it restarts.
k3d_mapped_ports() {
  docker ps -a --format '{{.Names}}' 2>/dev/null | grep -E '^k3d-.*-serverlb$' | while read -r n; do
    docker port "$n" 2>/dev/null | sed -nE 's#.*:([0-9]+)$#\1#p'
  done | sort -u
}
port_free() {  # free = nothing listening AND not mapped by another k3d cluster
  lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1 && return 1
  k3d_mapped_ports | grep -qx "$1" && return 1
  return 0
}
# A stable base derived from this checkout's path, so the port is consistent
# across restarts and distinct from a sibling checkout's.
_hash="$(printf '%s|%s' "$ROOT" "$INSTANCE" | cksum | cut -d' ' -f1)"
BASE_PORT=$(( 8700 + (_hash % 800) ))   # 8700..9499
# A pinned preference wins over the hashed base — env HOST_PORT, or K3D_HOST_PORT
# in the environment or local-infra/config. Handy for a memorable LAN port; it
# is still de-conflicted against other clusters. The cluster binds 0.0.0.0, so
# it is reachable over the LAN at http://<this-host-ip>:<port>.
_pref="${HOST_PORT:-${K3D_HOST_PORT:-$(sed -n 's/^K3D_HOST_PORT=//p' local-infra/config 2>/dev/null | head -1)}}"
[ -n "$_pref" ] && BASE_PORT="$_pref"

say() { printf '\n\033[1;33m==>\033[0m %s\n' "$*"; }
need() { command -v "$1" >/dev/null 2>&1 || { echo "missing prerequisite: $1"; exit 1; }; }
need docker; need k3d; need kubectl

# ---- 1. secrets ------------------------------------------------------------
if [ ! -f "$SECRETS" ]; then
  say "generating k8s/overlays/k3d/secrets.env from local-infra/global"
  # shellcheck disable=SC1091
  [ -f local-infra/global ] && set -a && . local-infra/global && set +a || true
  # `od -N` reads a fixed number of bytes and stops. The obvious
  # `tr -dc … </dev/urandom | head -c N` cannot be used under `set -o pipefail`:
  # head closes the pipe at N, tr dies of SIGPIPE, and the script exits 141
  # having written nothing.
  PW="$(LC_ALL=C od -An -tx1 -N16 /dev/urandom | tr -d ' \n')"
  SESSION="${HUNTWELL_SESSION_SECRET:-$(LC_ALL=C od -An -tx1 -N24 /dev/urandom | tr -d ' \n')}"
  cat > "$SECRETS" <<EOF
POSTGRES_PASSWORD=$PW
HUNTWELL_DATABASE_URL=postgres://huntwell:$PW@postgres:5432/huntwell
HUNTWELL_SESSION_SECRET=$SESSION
CURSOR_API_KEY=${CURSOR_API_KEY:-}
BROWSERBASE_API_KEY=${BROWSERBASE_API_KEY:-}
BROWSERBASE_PROJECT_ID=${BROWSERBASE_PROJECT_ID:-}
HUNTWELL_ADMIN_EMAIL=${HUNTWELL_ADMIN_EMAIL:-admin@local.test}
HUNTWELL_ADMIN_PASSWORD=${HUNTWELL_ADMIN_PASSWORD:-admin}
EOF
  echo "wrote $SECRETS (edit it to add any missing keys)"
fi

# ---- 2. cluster ------------------------------------------------------------
if k3d cluster list "$CLUSTER" >/dev/null 2>&1; then
  # Existing cluster: use the port it was actually created with, not a new pick.
  HOST_PORT="$(docker port "k3d-$CLUSTER-serverlb" 2>/dev/null | sed -nE 's#.*:([0-9]+)$#\1#p' | head -1)"
  HOST_PORT="${HOST_PORT:-$BASE_PORT}"
  say "cluster '$CLUSTER' already exists (host :$HOST_PORT → ingress :80)"
else
  # New cluster: first free port at/after the stable base, isolated from every
  # other k3d cluster. HOST_PORT=<n> overrides the base if you want a specific one.
  HOST_PORT="${HOST_PORT:-$BASE_PORT}"
  while ! port_free "$HOST_PORT"; do HOST_PORT=$((HOST_PORT + 1)); done
  say "creating k3d cluster '$CLUSTER' (host :$HOST_PORT → ingress :80)"
  k3d cluster create "$CLUSTER" --agents 1 --port "${HOST_PORT}:80@loadbalancer" --wait
fi
kubectl config use-context "k3d-$CLUSTER" >/dev/null

# ---- 3. images -------------------------------------------------------------
# The cluster runs on this machine, so the images must be this machine's
# architecture; ./build.sh names the target, not the host.
case "$(uname -m)" in arm64|aarch64) IMG_TARGET=arm64 ;; *) IMG_TARGET=ubuntu ;; esac
if [ "${SKIP_BUILD:-0}" != "1" ]; then
  say "building images (this is slow the first time — Rust + the worker toolchain base)"
  ./build.sh "$IMG_TARGET" --images
fi
# Always import — a freshly (re)created cluster has no images even when the
# build was skipped. The archives, not daemon image names: build-images.sh
# assembles them with crane and writes them to bin/images/, so there is no
# local Docker image for k3d to look up by name.
say "importing images into the cluster"
TARS=""
for name in website admin planning worker scheduling notification; do
  tar="bin/images/huntwell-$name.tar"
  [ -f "$tar" ] || { echo "missing $tar — run ./build.sh $IMG_TARGET --images"; exit 1; }
  TARS="$TARS $tar"
done
# shellcheck disable=SC2086
k3d image import $TARS -c "$CLUSTER"

# ---- 4. apply --------------------------------------------------------------
say "applying manifests"
kubectl apply -k k8s/overlays/k3d

say "waiting for Postgres and the schema Jobs"
kubectl -n huntwell rollout status statefulset/postgres --timeout=180s
kubectl -n huntwell wait --for=condition=complete job/migrate --timeout=240s

say "waiting for the bus"
kubectl -n huntwell rollout status statefulset/nats --timeout=120s

say "waiting for services"
for d in website admin planning scheduling notification; do
  kubectl -n huntwell rollout status "deployment/$d" --timeout=180s
done

say "Huntwell is up"
cat <<EOF

  Open:   http://huntwell.localhost:${HOST_PORT}
          (huntwell.localhost resolves to 127.0.0.1 automatically)

  Watch:  kubectl -n huntwell get pods,jobs -w
  Runs:   kubectl -n huntwell get jobs -l component=run-worker -w
  Admin:  kubectl -n huntwell port-forward svc/admin 8710:80
          then http://127.0.0.1:8710  (not exposed at the edge, on purpose)

  Logs:   kubectl -n huntwell logs deploy/website -f
  Bus:    kubectl -n huntwell exec -it statefulset/nats -- nats-server --help >/dev/null && \
          kubectl -n huntwell port-forward svc/nats 8222:8222   # then /varz
  Down:   ./k3d/down.sh
EOF

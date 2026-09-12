#!/usr/bin/env bash
# Tear down the local Huntwell cluster. Data lives in the cluster's PVC, so this
# deletes it too; secrets.env is kept for the next `up.sh`.
set -euo pipefail
cd "$(dirname "$0")/.."
# Same instance → cluster-name derivation as up.sh, so `down.sh` targets the
# cluster this checkout/instance created.
INSTANCE="${HUNTWELL_INSTANCE:-$(sed -n 's/^INSTANCE=//p' local-infra/config 2>/dev/null | head -1)}"
INSTANCE="${INSTANCE:-default}"
if [ -z "${CLUSTER:-}" ]; then
  [ "$INSTANCE" = default ] && CLUSTER="huntwell" || CLUSTER="huntwell-$INSTANCE"
fi
if k3d cluster list "$CLUSTER" >/dev/null 2>&1; then
  echo "==> deleting k3d cluster '$CLUSTER'"
  k3d cluster delete "$CLUSTER"
else
  echo "cluster '$CLUSTER' does not exist"
fi

#!/usr/bin/env bash
# Refuse to deploy a production overlay that is not finished.
#
#   ./deploy/preflight.sh                 checks k8s/overlays/prod
#   ./deploy/preflight.sh k8s/overlays/x  checks another one
#
# Every check here is a mistake that deploys cleanly and fails later, somewhere
# that does not mention the cause: a CHANGEME image nothing will ever pull, an
# empty mail key that turns every verification email into a log line, dev mode
# left on so session cookies are not Secure.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OVERLAY="${1:-k8s/overlays/prod}"
DIR="$ROOT/$OVERLAY"
[ -d "$DIR" ] || { echo "no such overlay: $OVERLAY" >&2; exit 1; }

fail=0
bad() { printf '  \033[1;31m✗\033[0m %s\n' "$*"; fail=1; }
ok()  { printf '  \033[1;32m✓\033[0m %s\n' "$*"; }

echo
echo "==> $OVERLAY"

# --- secrets -----------------------------------------------------------------
SECRETS="$DIR/secrets.env"
if [ ! -f "$SECRETS" ]; then
    bad "secrets.env is missing — cp secrets.example.env secrets.env and fill it in"
else
    # Anything with an empty value. Kustomize is happy to build a Secret full of
    # empty strings, and each one fails somewhere different at run time.
    empty=""
    while IFS= read -r line; do
        case "$line" in ''|'#'*) continue ;; esac
        key="${line%%=*}"; val="${line#*=}"
        [ -n "$val" ] || empty="$empty $key"
    done < "$SECRETS"
    if [ -n "$empty" ]; then
        bad "secrets.env has empty values:$empty"
    else
        ok "secrets.env has a value for every key"
    fi

    # The database password appears twice and they have to agree; a mismatch is
    # a Postgres that starts and an application that cannot sign in to it.
    pw="$(grep -E '^POSTGRES_PASSWORD=' "$SECRETS" | cut -d= -f2- || true)"
    url="$(grep -E '^HUNTWELL_DATABASE_URL=' "$SECRETS" | cut -d= -f2- || true)"
    if [ -n "$pw" ] && [ -n "$url" ]; then
        case "$url" in
            *":$pw@"*) ok "the database password matches the URL" ;;
            *) bad "POSTGRES_PASSWORD does not appear in HUNTWELL_DATABASE_URL" ;;
        esac
    fi
fi

# --- placeholders ------------------------------------------------------------
# Rendered, not grepped from the files: a placeholder can arrive through a patch
# or a base, and what matters is what would actually be applied.
rendered="$(KUBECONFIG=/dev/null kubectl kustomize "$DIR" 2>/dev/null || true)"
if [ -z "$rendered" ]; then
    bad "the overlay does not render — run: kubectl kustomize $OVERLAY"
else
    if echo "$rendered" | grep -q "CHANGEME"; then
        bad "CHANGEME is still in the rendered output:"
        echo "$rendered" | grep -n "CHANGEME" | sed 's/^/      /' | head -8
    else
        ok "no CHANGEME placeholders"
    fi

    # A tag you can push again means a node keeps whatever it pulled first.
    moving="$(echo "$rendered" | grep -oE 'image: \S+:(dev|latest)' || true)"
    if [ -n "$moving" ]; then
        bad "moving image tags — a node that has one already will never update:"
        echo "$moving" | sed 's/^/      /'
    else
        ok "every image tag is immutable"
    fi

    # An image with no registry resolves to docker.io and will not exist.
    bare="$(echo "$rendered" | grep -oE 'image: huntwell-\S+' || true)"
    if [ -n "$bare" ]; then
        bad "unqualified image names — these resolve to docker.io and cannot pull:"
        echo "$bare" | sed 's/^/      /'
    else
        ok "every Huntwell image is registry-qualified"
    fi

    for pair in 'HUNTWELL_DEV: "0"|dev mode is off (session cookies get the Secure flag)' \
                'HUNTWELL_OPEN_SIGNUP: "0"|signups are closed'; do
        want="${pair%%|*}"; what="${pair##*|}"
        if echo "$rendered" | grep -q "$want"; then ok "$what"; else bad "$what — expected $want"; fi
    done
fi

echo
if [ "$fail" = 0 ]; then
    echo "Ready. Apply with:"
    echo "  kubectl apply -k $OVERLAY"
else
    echo "Not ready — fix the above first." >&2
    exit 1
fi

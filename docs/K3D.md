# Huntwell on k3d (microservices)

The hosted shape of Huntwell: the single `huntwell` binary, deployed as
independent services on a local [k3d](https://k3d.io) Kubernetes cluster, with
runs executed as one Kubernetes Job each. This is the prod-like environment;
`./dev.sh` (single process, single Postgres) is still the fast local loop.

## Topology

```
Traefik ingress (huntwell.localhost)
  ├─ /                → gateway        (embedded React UI)
  ├─ /api/auth/*      → auth-svc       (cookies; owns the `auth` database)
  ├─ /api/plans/*     → plans-svc  ┐
  ├─ /api/prospects/* → prospects-svc ├ forward-auth → auth-svc /introspect,
  ├─ /api/keys/*      → prospects-svc │  which injects a trusted X-Account-Id;
  ├─ /api/executions/*      → runs-svc   ┘  services share the `core` database
  ├─ /api/overview    → runs-svc       (dashboard aggregate)
  └─ /dl/*            → prospects-svc  (token-authenticated download API)

runs-svc ──creates──▶ run-<id> Job ──▶ huntwell run-worker --execution-id N
                                        (Browserbase + Cursor agent; no Chrome)

Postgres StatefulSet · two databases: auth, core
```

Why this split and where the databases land is in
[ARCHITECTURE.md](ARCHITECTURE.md#hosted-k3d-microservices).

## Prerequisites

- Docker (running), [`k3d`](https://k3d.io) ≥ 5, `kubectl`. `kubectl` has
  kustomize built in — nothing else to install.
- Your `local-infra/global` with `CURSOR_API_KEY` and the Browserbase keys
  (`BROWSERBASE_API_KEY`, `BROWSERBASE_PROJECT_ID`). `up.sh` copies them into
  the cluster Secret; runs need them.

## Bring it up

```sh
./k3d/up.sh            # build images, create the cluster, apply, wait
```

Then open **http://huntwell.localhost:8080** (the `.localhost` name resolves to
127.0.0.1 automatically). Sign up, create a plan, and run it.

- First run is slow: it builds the Rust binary and the worker image (Node +
  Cursor CLI). Re-runs are cached.
- Iterating on manifests only (no rebuild): `SKIP_BUILD=1 ./k3d/up.sh`.
- Tear down (deletes the cluster and its data): `./k3d/down.sh`.

**Isolation.** The cluster name and host port follow the local-infra "isolated
instance" scheme, so this stack never collides with another k3d cluster (a
sibling project, or a second checkout):

- Name: `huntwell` by default, `huntwell-<INSTANCE>` when `HUNTWELL_INSTANCE`
  (or `local-infra/config`'s `INSTANCE`) is set.
- Host port: a stable base derived from the checkout path (8700–9499), then the
  first port at/after it that is **not mapped by any other k3d cluster (running
  or stopped) and not otherwise listening**. An existing cluster keeps the port
  it was created with (read back from Docker). Override with `HOST_PORT=<n>`.

## Secrets

`up.sh` writes `k8s/overlays/k3d/secrets.env` (gitignored) from
`local-infra/global` plus a random Postgres password, and kustomize turns it
into the `huntwell-secrets` Secret. Keys:

| Key | Used by |
|---|---|
| `POSTGRES_PASSWORD` | postgres, and the two DB URLs below |
| `HUNTWELL_DATABASE_URL` | core services + run-worker (the `core` database) |
| `AUTH_DATABASE_URL` | auth-svc + its migrate Job (the `auth` database) |
| `HUNTWELL_SESSION_SECRET` | auth-svc (session cookies) |
| `CURSOR_API_KEY` | plans-svc (AI plan authoring) + run-worker |
| `BROWSERBASE_API_KEY`, `BROWSERBASE_PROJECT_ID` | run-worker |

The Cursor agent in the worker authenticates **non-interactively with
`CURSOR_API_KEY`** — there is no `agent login` and no host agent.

## Watching a run

```sh
kubectl -n huntwell get pods,jobs -w              # everything
kubectl -n huntwell get jobs -l component=run-worker -w   # runs as they spawn
kubectl -n huntwell logs job/run-42 -f            # one run's worker log
kubectl -n huntwell logs deploy/runs-svc -f       # dispatcher + scheduler
```

The runs service streams each run pod's log into `execution_log`, so the live log in
the UI works exactly as in the single-process app.

## How it maps to the code

| Piece | Code |
|---|---|
| Service selection | `huntwell service --role <auth\|plans\|prospects\|runs\|gateway>` (`cmd/src/main.rs`, `web::serve_service`) |
| Header auth | `HUNTWELL_TRUST_HEADER_AUTH=1` → `AuthUser` reads `X-Account-Id` (`web/auth.rs`) |
| Forward-auth endpoint | `GET /api/internal/introspect` (`web/auth.rs::introspect`) |
| Run dispatch | `RUN_DISPATCH=k8s` → `web/dispatch.rs` creates a Job via the in-cluster API |
| Schema per database | `huntwell migrate --schema <auth\|core>` (`store::migrate_schema`), Jobs in `k8s/base/migrate.yaml` |
| Manifests | `k8s/base/` (+ `k8s/overlays/k3d`) |
| Images | `./build.sh --images` → `deploy/images/build-images.sh` (crane, no Docker) → `bin/images/*.tar`. The worker's toolchain base comes from `deploy/images/build-worker-base.sh`, built rarely. |

## Multi-host pool dispatch (the admin control plane)

`huntwell admin` (default `:8710`, env `HUNTWELL_ADMIN_ADDR`) is a separate
control plane that manages **many** k3d/k3s hosts. Register a host by pasting
its kubeconfig in the dashboard; the admin keeps a warm StatefulSet pool of
`worker-pool` pods on it (`hw-pool-0..N-1`, size + CPU/mem from the host row)
and routes queued runs to pods — **round-robin**, **random**, or **pinned** to
one pod. One run per pod is the isolation contract.

- Central runs-svc switches with `RUN_DISPATCH=pool`: runs are created queued
  and the admin's placement loop assigns them; the pod supervisor claims the
  run through the shared core database (no network path to pods needed) and
  executes the same `huntwell run` child as every other mode.
- Admin env: `HUNTWELL_DATABASE_URL` (core DB), `HUNTWELL_ADMIN_EMAIL` /
  `_PASSWORD` (operator seed), `HUNTWELL_POOL_DATABASE_URL` (DB URL as the
  *pods* reach it, when the admin's own URL is loopback), plus
  `CURSOR_API_KEY` / `BROWSERBASE_*`, which it materializes into each host's
  `huntwell-secrets`. Needs `kubectl` on the admin server.
- Cancel sets a flag the pod's 10s heartbeat picks up; a pod that dies mid-run
  is failed by the heartbeat reaper (default 90s, `HUNTWELL_POOL_STALE_SECS`).
  Killing a pod from the dashboard fails its run cleanly and the StatefulSet
  recreates the same pod name, so pins survive.

## Routing, and the gap that used to be here

Earlier versions of this overlay split `/api` five ways at the edge, one prefix
per service, with a forward-auth middleware in front. That had a failure mode
worth remembering: a path with no rule fell through to the SPA fallback and
answered 200 with the index page, so a missing route looked to a caller like an
API returning HTML, and four paths were in that state at once.

`k8s/base/ingress.yaml` is now one rule to one backend. The website serves the
UI, `/api`, the versioned `/v1` and the token-authenticated `/dl` in one
process, so there is no per-prefix routing to drift from the code — and the
session is checked in-process, so there is no middleware to keep in step
either.

For a production deployment, do not apply this overlay by hand: register the
cluster in the admin with **Run the application here** ticked and let the
reconcile loop own it — [PRODUCTION.md](PRODUCTION.md). This overlay is for
reading, and for k3d.

## Not yet (later phases)

- **Strict DB isolation** — plans/prospects/runs share the `core` database in
  Phase 1; Phase 2 splits ownership with per-service DB roles, then physically.
- **Autoscaling** the run-workers (KEDA on the queue) and the API replicas.
- A **prod overlay** against a managed Postgres, for operators who would
  rather hold the manifests themselves than let the admin reconcile them.

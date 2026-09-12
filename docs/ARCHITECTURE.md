# Architecture

## Processes

| Process | Started by | Does |
|---|---|---|
| `huntwell serve` | you / systemd | API, embedded UI, download API, scheduler (20 s tick), run spawner |
| `huntwell run --execution-id N` | `serve` (or by hand) | one run: Chrome, agent workspace, pipeline, writes `execution`/`prospect` |
| `huntwell mcp-prospects --plan-id N` | the Cursor agent | JSON-RPC over stdio; read-only Postgres session pinned to one plan |
| Chrome | `run` | one per **account** (profile `data/huntwell/accounts/<id>/chrome`, DevTools port `CDP_PORT_BASE + account_id % 2000`) |

Process-per-run is deliberate: the browser, guard, trail and agent workspace
keep process-wide state that assumes one run per process, exactly as the CLI
did. The server never runs an agent in-process except the plan wizard (no
plan in scope, browser-less).

## Modules (`cmd/src`)

| File | Origin | Role |
|---|---|---|
| `main.rs` | new | CLI: serve / run / mcp-prospects / account / doctor / config |
| `config.rs` | new | env → `global` file resolution, data dir, ports |
| `store.rs` | rewritten | all Postgres access; every fn takes `account_id` |
| `pipeline.rs` | ported from `main.rs` | SCRAPE → DEDUPE → ENRICH → STORE, async over sqlx, agent calls on blocking threads |
| `web/{mod,api,auth,runner,scheduler,download}.rs` | new | axum app |
| `plan_chat.rs` | ported | AI plan drafting prompts + defaults |
| `agent.rs`, `browser.rs`, `sandbox.rs`, `guard.rs`, `trail.rs`, `progress.rs` | ported, lightly adapted | Cursor agent driver, Chrome lifecycle, tool-policy workspace, tripwire, search rotation, log narration |
| `prospect.rs`, `normalize.rs`, `csv.rs`, `cidr.rs` | verbatim | templates + mapping, cleansing, CSV, CIDR |
| `mcp.rs` | rewritten | the plan-memory MCP server on Postgres |

## Data model

All tables in `local-infra/db/public/`, Postgres-standard snake_case
identifiers. Every tenant table carries `account_id`; children key on
`plan_id` with cascade deletes. `(plan_id, source_key)` on `prospect` is the
dedupe mechanism. `meta_data` (jsonb) keeps the full raw scraped row so
display columns can be re-projected without re-scraping.

## Isolation between accounts

- SQL: every query filters by `account_id` (plans, runs, prospects, keys, audit).
- Browser: profile and DevTools port per account; a run of another account
  cannot adopt this account's Chrome because the port differs.
- Agent workspace: `data/huntwell/agent-workspace/plan<N>-…`, one per plan+port.
- MCP: pinned to a plan id on the command line; the session is
  `default_transaction_read_only = on`; tools return verdicts, not rows.
- Download API: the key's account (and optional plan) decides what is exported;
  a `plan` query parameter is ignored for a plan-scoped key.

## Isolation between instances

`INSTANCE=<name>` in `local-infra/config` (or the environment) picks a data
directory (`data-<name>/`), an env file (`global-<name>`) and a port offset
(a stable hash of the name), so a second checkout with a different name runs
beside the first with nothing shared — including Chrome profiles, because the
data dir is what `HUNTWELL_DATA_DIR` points at.

## Hosted (k3d) microservices

The same binary also runs as a set of independent services on Kubernetes — see
[K3D.md](K3D.md) for how to bring it up. The seams:

- **One binary, a role per deployment.** `huntwell service --role
  <auth|plans|prospects|runs|gateway>` mounts only that slice of the `web/`
  router (`web::serve_service`); `serve` is still the all-in-one for `dev.sh`.
  The gateway serves the embedded UI; Traefik routes `/api/*` to the services.
- **Edge auth.** Only auth-svc handles cookies. Traefik forward-auth calls its
  `/api/internal/introspect`, which returns `X-Account-Id`; the other services
  run with `HUNTWELL_TRUST_HEADER_AUTH=1` and read that header instead of a
  session table (`web/auth.rs`). The ingress strips any client-supplied copy.
- **Two databases.** `auth` (account, session) is isolated; `plans`, `prospects`
  and `runs` share a `core` database in this phase, so the cross-cutting run
  pipeline keeps one connection and stays a minimal-diff port. The two
  `auth`↔`core` foreign keys were dropped; account scoping is still enforced in
  every query. Schema is applied per database by `huntwell migrate --schema
  <auth|core>` Jobs. Splitting `core` ownership (DB roles, then physically) is
  the next phase.
- **A run is a Job, not a child process.** With `RUN_DISPATCH=k8s`, runs-svc
  creates one `run-<id>` Job per run through the in-cluster API (`web/dispatch.rs`)
  and streams its pod log into `execution_log`; cancel deletes the Job. One run per pod
  preserves the pipeline's process-wide state. Runs drive Browserbase, so no pod
  needs Chrome. The scheduler runs only in the single-replica runs-svc.

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

## In production: VMs

Production runs in Incus VMs driven by the admin control plane — see
[PRODUCTION.md](PRODUCTION.md). The seams:

- **One app VM, many worker VMs.** The app VM runs `website` (the UI and the
  whole API, in one process), `planning`, `scheduling`, `notification` and
  NATS under systemd. Every worker VM connects to that NATS — at the app host's
  private IP, forwarded by its edge container, with the NATS user and password
  from the secret. Worker VMs run `huntwell-worker@1..N`, one slot
  per concurrent plan. The admin pushes the executables in; nothing is pulled
  from a registry.
- **One database, outside every VM.** Every VM reaches Postgres over the
  network; account scoping is enforced in every query.
- **A run is claimed, not dispatched.** With `RUN_DISPATCH=pool` the website
  queues a run, the admin's placement loop assigns it to a free slot, and that
  slot claims it from Postgres and heartbeats while it executes
  (`worker_pool.rs`). Workers need no route to the app VM. One run per slot
  preserves the pipeline's process-wide state, and runs drive Browserbase, so
  no VM needs Chrome.
- **Credentials by role.** A worker VM's settings file carries the database,
  Cursor and Browserbase keys and nothing else; identity, mail, billing and the
  session secret exist only on the app VM (`admin/incus_driver.rs`).

# Huntwell — working notes for agents

Hosted, multi-account port of `../huntwell` (Rust CLI). Rust backend (axum +
sqlx) on Postgres, React UI embedded in the binary, one process per run.

## Commands

- `./local-infra/start.sh` — Postgres up (repo-local, :6511 default), schema applied, `local-infra/global` written.
  **`stop.sh` clears `local-infra/data` (the whole dev DB) by default; `KEEP=1` preserves it**, and `start.sh` forwards KEEP.
  To apply a schema change you do *not* need either: `store::migrate` runs the embedded SQL at server startup, so
  `cargo build` + restart is enough and cannot lose data
- `./dev.sh` — build UI + server, run server `:8611` (`--dev`) and Vite `:5611`
- `cd cmd && cargo test` — unit tests (134); `cargo build` needs `UI/web/dist` to exist (any file)
- `cd UI/web && npm run build` — typecheck + bundle
- `cmd/target/debug/huntwell doctor|config list|account create --email … --password …`

## Rules

- **Schema = SQL files** in `local-infra/db/public/*.sql`, order in `db/schema.order`.
  Idempotent only (`IF NOT EXISTS`; new columns via `ALTER TABLE … ADD COLUMN IF NOT EXISTS`).
  No `CREATE TABLE` in Rust. `build.rs` embeds them; `store::migrate` applies them.
- **Every store function takes `account_id`** and filters by it. Never expose a
  query that trusts a bare `plan_id`/`execution_id` from a request.
- **Ported modules** (`agent`, `browser`, `sandbox`, `guard`, `trail`, `prospect`,
  `normalize`, `csv`, `progress`, `plan_chat`) track the original CLI; prefer
  minimal diffs so fixes can be carried across.
- The security preamble lives in `agent.rs`, prepended in code — never make it
  part of a user-editable prompt.
- Secrets live in `local-infra/global` (gitignored). Never commit `global*`
  except `global.example`. Hook: `git config core.hooksPath .githooks`.
- UI theme: tokens on `:root` and `:root[data-theme='dark']` in `UI/web/src/styles.css`;
  never hard-code a colour in a component.

## Model providers (`cmd/src/llm/`) and our own agent loop (`cmd/src/direct/`)

A stage model id of `provider:model` runs in Huntwell's loop; a bare id is the
Cursor CLI, unchanged. `pipeline::agent_call` routes on `direct::handles`.

- `llm/` — one `Provider` trait, one file per provider (`gemini`, `anthropic`,
  then the Chat Completions family in `chat.rs`: `openai`, `deepseek`, `groq`,
  `mistral`, `xai`). Shared HTTP, retries, concurrency cap and error
  classification live in `kit.rs` — never in an adapter. `usage.rs` normalises
  every provider's token counts to **fresh input** (cache reads separate), the
  convention `TokenUsage::billable` has always meant.
- `llm/conformance.rs` — the contract every adapter must pass. Adding a
  provider = copy the closest file, change auth/URL/field names, fill the price
  table, register it, pass conformance. See `docs/plans/direct-providers.md` §1.4.
- `direct/` — the loop. `tools.rs` offers six browser verbs over CDP and
  nothing else (no shell/file tools at all, so `sandbox.rs` is not needed on
  this path); `snapshot.js` produces Playwright-format page outlines;
  `context.rs` keeps only the last two pages, which is the whole point;
  `cdp.rs` attaches to `browser::cdp_endpoint()` — **Browserbase is unchanged**.
- The guard judges via `judge_before` — before a page is fetched, not after.
- **Model ids are listed live** (`Provider::list_models`) — a provider's own
  catalogue endpoint. The `models()` list in each adapter is a *fallback* for
  when that call fails, and a price table keyed by **family** (`flash-lite`,
  `haiku`, `nano`) so a new version prices without an edit. Hard-coding ids
  from memory is what once shipped a retired `gemini-2.5-flash` and killed
  every run at its first call.
- Prices themselves cannot be fetched and are **unverified**; check the
  provider's page before trusting the admin's margin column. A model from no
  known family shows "price unknown" rather than a wrong number.

## Spending less (`cmd/src/thrift/`)

Code that does work an agent would otherwise be paid to do. One rule for every
piece: **when in doubt, do what the run did before** — a page that will not
fetch, data that does not parse, a match that is not certain all fall through to
the agent. Nothing here may be site-specific; rules are about standards
(schema.org), tree structure, or evidence the plan itself produced.

- `structured` — a row's page is read over HTTP; schema.org/Open Graph values fill blank
  columns and are handed to the enrich agent as a head start (sanitised by `guard::sanitize_replayed`).
- `trim` — a thread in the run process watches the browser tools' snapshot directory
  (`browser::output_dir()`, per process) and rewrites each `.yml` before the agent's next
  turn reads it. Nothing sits between the agent and its tools. Never cuts `main` content
  or a clickable ref.
- `watch` — a *scheduled* artifacts run is skipped when its proven listing pages show no new
  link; never more than `HUNTWELL_WATCH_MAX_SKIPS` (2) in a row. `execution.skipped_reason`.
- `twins` / `filters` — before enrich: the same listing on another site, and rows outside a
  column's `min`/`max` (set at draft time only from limits the brief states).
- `rounds` — the search is one agent call per named site instead of one long conversation
  (`pipeline::run_search`). The page budget comes from the plan's `effort`
  (`store::effort_pages`), and the prompt block is written as work to get through, not as
  limits to avoid — the first version was all brakes and a fast model did one page and
  stopped. `direct::context::Ground` carries a line-each record of searches run and pages
  opened, which survives compaction so a round does not re-tread itself.
- `HUNTWELL_THRIFT_OFF=trim,watch,…|all` switches pieces off. Our own fetches obey
  `guard::host_may_be_fetched` and the download SSRF guards, and identify as HuntwellBot.

## Ports (default instance; `INSTANCE=name` adds a stable offset)

PG 6511 · API 8611 · Admin 7611 · UI 5611 · MinIO 9611 (console 9612) · Chrome DevTools 20611 + account_id

## Plan kinds

`Plan.Kind` decides what a run produces; `store::PlanKind` + `store::plan_ready`
are the single source of truth (API, runner and pipeline all call the latter —
do not re-derive the rules):

- `prospects` (default) — people/companies via the `*Tmpl` mapping → `Prospect`
- `artifacts` — custom-schema rows from `FieldsSchemaJson` → `Artifact`
- `report` — one Markdown document about `Subject` → `Report`; read in-app,
  PDF via the browser's own print (`/api/reports/{id}/print`). No server-side
  PDF renderer, deliberately: worker VMs have no Chrome.
- `assets` — files found about `Subject` → `Asset` rows + bytes in the object
  store (`objstore.rs`: S3/MinIO, falling back to a data-dir directory that
  only works on one machine). Downloads go through `assets.rs`, which is where
  the SSRF and size guards live.

`./dev.sh` also starts the admin control plane (`huntwell admin`) with a
process-backed local worker pool (`HUNTWELL_LOCAL_POOL`, default 2) and runs
the server with `RUN_DISPATCH=pool`, so run routing works locally with no VMs.
`RUN_DISPATCH=local ./dev.sh` opts out. Operator creds live in `local-infra/global`.

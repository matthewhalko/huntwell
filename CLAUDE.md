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

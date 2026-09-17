# Huntwell

**Huntwell** (formerly branded Huntwell) is a hosted, multi-account version of
the `huntwell` CLI: users sign up, describe
who they are looking for, and run **prospecting plans** — SCRAPE → DEDUPE →
ENRICH → STORE — driven by the Cursor `agent` CLI and a real Chrome. Results
land in Postgres, deduped per plan, exportable as CSV or over a token-gated API.

```
Rust (axum + sqlx)  ──►  Postgres
   │  serves the embedded React UI, the JSON API, the download API,
   │  the scheduler — and spawns one `huntwell run` process per run
   └─► huntwell run ──► Cursor agent ──► @playwright/mcp ──► Chrome
                       └─► huntwell mcp-prospects (read-only plan memory)
```

## Quick start

```bash
./local-infra/start.sh      # repo-local Postgres 18 on :6511, schema applied, global written
./dev.sh                    # builds UI + server, starts server :8611 and Vite :5611
open http://127.0.0.1:5611  # sign up, create a plan, run it
```

Runs need the Cursor `agent` CLI on PATH with `CURSOR_API_KEY` (put it in
`local-infra/global`; `start.sh` copies it from your shell the first time),
Node/npm (for `npx @playwright/mcp`), and Google Chrome. `cmd/target/debug/huntwell doctor`
checks all of it.

A second, fully isolated stack — own database, ports, Chrome profiles, env file:

```bash
INSTANCE=pra ./local-infra/start.sh
INSTANCE=pra ./dev.sh
```

## Layout

```
local-infra/      the isolated infrastructure: start/stop, config, schema (db/public/*.sql), data-<instance>/
cmd/              the Rust binaries: website, admin, planning, worker, scheduling, notification, huntwell
UI/web/           React + Vite app (Mailchimp-flavoured theme, light + dark), embedded into the binary
dev.sh            dev stack;  build.sh   release binaries (zig cross-compile → bin/ubuntu, or host)
deploy/           systemd units, Caddyfile example
docs/             ARCHITECTURE.md, OPERATIONS.md, PRODUCTION.md (Incus VMs), SECRETS.md
```

## What a plan is

A plan (`Plan` table, the original `SourceConfig`) holds:

- **Prompts** — scrape (how to search, what JSON to return), enrich (per new row:
  contact, published email, drafted outreach), planner (learn mode: where to
  look next).
- **Field mapping** — `{{.key}}` templates over the scraped row that become
  Name, Email, Company, … `SourceKeyTmpl` is the dedupe identity.
- **Seed vars** — `{"city": "Mexico City"}`, the starting point the planner varies.
- **Behaviour** — learn mode, iterations, target, min value, free agent, allowed hosts, model.
- **Schedule** — local wall-clock time + weekdays, in the account's timezone.

The **AI wizard** (`POST /api/plans/draft`, then `/api/plans/chat`) drafts all of
it from a plain-language description, using the same agent.

## Runs

`POST /api/executions` inserts an `Execution` row and spawns `huntwell run --execution-id N` in
its own process group. The child:

1. launches (or adopts) a Chrome for that **account** — one profile per account,
   one DevTools port per account, so accounts never share cookies;
2. writes a locked-down agent workspace (`.cursor/cli.json`, `mcp.json`,
   `hooks.json`) with two MCP servers: the browser, and
   `huntwell mcp-prospects --plan-id N` — a **read-only** Postgres session
   pinned to that plan that answers *is this company known?*, *what was
   searched?*, never returning a row;
3. runs the pipeline, printing progress that the server tails into `execution_log`
   and streams to the browser over SSE;
4. writes its final status to `Run`.

Everything that made the CLI safe against prompt injection is kept: the
security contract prefixed in code, the tool-policy workspace, the guard that
kills a run on a forbidden tool call, the sanitised replay of scraped values,
formula-defused CSV.

## Download API

```
GET /dl/prospects.csv?token=<key>[&plan=<name>][&excludeDraft=1][&bom=0]
GET /dl/plans?token=<key>
```

Keys are minted under **API access** (shown once; only a SHA-256 is stored),
scoped to one plan or all, with an expiry and an IP/CIDR allow-list. Every
failure is the same 401; every attempt is audited; per-client rate limiting and
lockouts apply. Behind a TLS proxy set `HUNTWELL_TRUST_PROXY=1`.

## Browser backend (local Chrome or Browserbase)

Runs drive a browser over CDP. By default that's a local Chrome (one profile
per account). Set `HUNTWELL_BROWSER=browserbase` to drive a hosted
[Browserbase](https://www.browserbase.com) session instead — stealth
fingerprinting, residential proxies and captcha handling, which beat the
search-engine captchas that wall a datacenter Chrome, plus no "headed Chrome
needs a display" server setup.

```
HUNTWELL_BROWSER=browserbase
BROWSERBASE_API_KEY=bb_...
BROWSERBASE_PROJECT_ID=...
# optional
BROWSERBASE_PROXIES=1
BROWSERBASE_REGION=us-east-1
BROWSERBASE_CONTEXT_ID=...     # a persistent Context = the cloud per-account profile; logins persist
```

The swap is transport-only: the agent, the injection guard, the search trail
and the pipeline are unchanged, because they read the tool-call stream, not
where the browser runs. A run creates a session, drives it over its
`connectUrl`, prints a live-view URL to watch, and releases it at the end.
Selecting `browserbase` without the two keys fails a run loudly rather than
silently using local Chrome. Trade-off: the session (and any logged-in
Context cookies) lives in Browserbase's cloud, not on your box. `huntwell
doctor` and Settings → This server show which backend is active.

## Configuration

Resolution order: process environment → `local-infra/global` (or
`HUNTWELL_GLOBAL`, or `global` beside a release binary). Only `HUNTWELL_*`
and `CURSOR_API_KEY` are read from the file. See `local-infra/global.example`.

## Schema changes

SQL files in `local-infra/db/public/`, applied in `db/schema.order`, are the
only source of truth — `start.sh` and the server both apply them, and they must
stay idempotent. A change to an existing table is an
`ALTER TABLE … ADD COLUMN IF NOT EXISTS` appended to that table's file. There is
no `CREATE TABLE` in Rust.

## Security notes

- Sessions: random cookie token, SHA-256 stored, argon2id passwords, same
  message for unknown email and wrong password.
- Every store function takes the account id; a bare `PlanId` is never trusted.
- The UI's CSP allows only self + Google Fonts; no CDN scripts.
- Enable the pre-commit secret scan once per clone: `git config core.hooksPath .githooks`.

Things worth knowing:

- **A headed Chrome needs a display.** On a server without one, install `xvfb`
  (the runner uses `xvfb-run` automatically) or set
  `HUNTWELL_CHROME_DISPLAY=headless` — headless is easier for sites to block.
- **Sign-up is open by default.** Set `HUNTWELL_OPEN_SIGNUP=0` once your
  account exists, and mint others with `huntwell account create`.
- **Runs left running when the server dies** are marked failed at the next
  start; a Chrome an old run left behind is adopted by the next run of that account.

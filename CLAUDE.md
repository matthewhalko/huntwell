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

## Public API auth (`cmd/src/web/signing.rs`)

A key issued today carries a **secret**. The key identifies and travels; the
secret only signs and never leaves either end.
`X-HW-KEY` / `X-HW-TS` / `X-HW-NONCE` / `X-HW-SIGN`, signing
`METHOD\nPATH\nQUERY\nTIMESTAMP\nNONCE\nSHA256(BODY)` with HMAC-SHA256, 30s
window. **Signing is the only way in** — bearer tokens and `?token=` are
refused, including on `/dl/`.

- **The body is signed.** `download::stamp_signed_parts` (on `/v1` and `/dl`)
  reads the body once (1 MB cap → 413), hashes it, and puts the digest and the
  raw query in internal headers it strips from the caller first; the handler
  gets the same bytes. What is verified is exactly what the handler reads.
- **A nonce is used once.** `signing::claim_nonce` remembers (key, nonce) for
  the window, after the signature is good; a repeat is `Refused::Replayed`.
  In memory — the website is one process.
- Park River signs the same way (`shared/huntwell.rs`, `advisor/src/prospecting.rs`
  in that repo); both repos pin the same vectors (`operator_tests::the_shared_vectors_verify`).
  Change the scheme in both, together.
- The secret is **encrypted, not hashed** (`signing::seal_secret`): verifying an
  HMAC means computing it, so it has to be recoverable. Key from
  `HUNTWELL_API_SIGNING_KEY`, falling back to `HUNTWELL_SESSION_SECRET`.
- A key predating signing has no secret and is refused, told to make a new one.
- Signature comparison is constant-time; the signed query is the raw string as
  sent, never re-encoded; nothing the caller sends separately (the old
  `X-HW-Query`) is trusted.
- A key records `created_by`; a teammate's key stops authenticating when they
  leave the workspace, and `remove_member` revokes it. Only its maker or an
  admin may revoke, delete or re-pin a key. A plan-pinned key's plan always
  wins over a `plan_id` in the request.
- A key never does more than its maker: `Caller.can_plans` comes from the
  maker's workspace permissions, and every `/v1` write calls `needs_plans`.
  `/v1/stream` re-checks its key (`store::api_key_live`) every 10s and closes
  when it is revoked, expired, or its maker leaves the team.
- Two-factor sign-in: the account comes from the server's record of which
  password earned the challenge (`identity::mfa_challenge_account`), never
  from the request, and the Cognito subject must match the bound one.

## Invite-only sign-up (`web/auth.rs`, `store` waitlist fns, `waitlist.sql`)

Closed by default: `HUNTWELL_OPEN_SIGNUP=1` opens it; the first account on an
empty database never needs an invitation. Without one, `/signup` is a waitlist
request (`POST /api/auth/waitlist`, answers the same whether or not the address
is known). Two invitations let someone in, resolved by `find_invitation`:

- **Platform** — admin Users → Waitlist & invites (`/admin/api/waitlist/*`).
  Token `hwi_…`, only its SHA-256 stored, 14 days, single-use, bound to the
  address; link `{HUNTWELL_PUBLIC_URL}/signup?invite=…` (the admin needs that
  setting). It was emailed, so sign-up marks the address verified — no OTP.
- **Team** — a workspace's existing `invite`; still lets someone sign up while
  closed. It can be copied, so the OTP is still required.

Each waitlist row has a "…" menu (Approve / Decline / Delete). Delete on a
joined row is `store::purge_account` — one transaction over every table that
names the account (most do not cascade from `account`; add a new
account-keyed table to its list), refused while a run is queued or running,
then the asset bytes and the Cognito user. The operator must type the address;
the server checks it too. `purge_tests` (ignored; dev DB) fails if any
`account_id`-like column still points at a deleted account.

## Public site and SEO (`cmd/src/web/seo.rs`, `UI/web/src/components/Site.tsx`)

`seo::PAGES` is the one list of public pages: `/sitemap.xml`, `/robots.txt` and
the per-route `<head>` (title, description, canonical, Open Graph/Twitter,
JSON-LD) come from it. The server writes the head into `index.html` between
`<!--seo-->` markers, so crawlers and link previews see it without running JS;
the app, sign-in pages and unknown URLs get `noindex`. `PAGE_META` in
`Site.tsx` mirrors it for client-side navigation — a test fails if they drift,
or if a listed page is not routed in `App.tsx`. Absolute URLs use
`HUNTWELL_PUBLIC_URL`. Share image: `public/og.png`, source `brand/og.html`.
Unknown URLs are served with the app but as **404** (`seo::route`), and
`/page/` 301s to `/page` — a test fails if a route in `App.tsx` would 404.
Icons for Google's result favicon (48px multiple, plus `/favicon.ico`) and the
JSON-LD logo (`icon-512.png`) are rendered from `brand/favicon.html`; see its
comment. `<lastmod>` is the UI bundle's build date (`build.rs`).

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

## Outreach (`cmd/src/outreach.rs`, `cmd/src/web/outreach.rs`, `pages/Outreach.tsx`)

Cold emails drafted to a prospect or a hand-entered person — drafted, never
sent; the person copies them into their own mail. One `llm` call per draft or
revision (no tools), model = admin stage **outreach** (`model_outreach`), else
the newest Claude Sonnet from Anthropic's live model list — never the plan
drafter's model. Cursor ids cannot serve it. Current Claude models reject
`temperature` (400); the Anthropic adapter drops it for them (`accepts_sampling`).
Billed to the workspace via `charge_account_usage`, after a credit check.
Product description and rules are per workspace (`outreach_profile`) — the
default. On top of it, a draft can be written to a *campaign*
(`store::Campaign`, `outreach::prompt`'s CAMPAIGN block: a brief, an optional
product that replaces the workspace's, rules that win where they differ):
- a saved profile (`outreach_design`, Outreach → Profiles) — belongs to no
  plan, picked for any draft (`design_id`), prospect or hand-entered;
- a plan's own design (`plan.outreach_*`, Edit tab), or the saved profile the
  plan uses (`plan.outreach_design_id`).
`store::outreach_campaign` is the one resolver: picked profile → plan's own →
plan's profile → workspace only. `outreach.design_id`/`plan_id` pin a draft so
revisions stay on it. The
footer is per person (`account.outreach_footer`) and appended by code, never
by the model. Every text change is a row in `outreach_version`. Scraped
prospect fields go through `outreach::scraped` (flattened, bounded, dropped if
`guard::reads_like_an_attack`) and are marked as data in the prompt.
Also on the signed API (`/v1/outreach*`, `public_api.rs`): the handlers call
the same `pub(crate)` functions in `web/outreach.rs` (`draft_new`,
`revise_draft`, `edit_draft`, `restore_draft`) with a `Writer` — the key's
maker (`api_key.created_by`), else the workspace owner. Plan-pinned keys are
refused. Keep app and API on those functions; never copy the logic.

## Slack (`cmd/src/slack.rs`, `components/SlackSettings.tsx`, `slack_outbox.sql`)

Per plan: an incoming webhook posts each run's **new** rows to a channel.
Layout `auto` = up to `slack_limit` rows in one post, past it a summary + a
"View all" link to the plan; `all` = every row over several messages (capped).
Webhooks cannot attach files, so there is no CSV in Slack by design.

- The run only queues (`store::queue_slack_post`, in `pipeline::announce_new`);
  the **notification service** renders from rows first seen in
  `[since, created_at]` and sends. Workers hold neither the seal key nor
  `HUNTWELL_PUBLIC_URL`, so they cannot send it themselves. A multi-message
  post resumes from `parts_sent` on retry.
- The webhook is sealed (`signing::seal_secret`); the browser only ever gets
  `slack::hint`. `slack::validate_webhook` (https, exactly `hooks.slack.com`,
  `/services/…`, no port/userinfo/query) is the SSRF defence, and it runs on
  save *and* on open. No redirects are followed.
- Everything posted is scraped data: `slack::escape` everything, and a URL
  becomes a link only if `safe_url`. No unfurling.
- A permanent Slack refusal is written to `plan.slack_last_error` and shown in
  the plan's settings; transient ones retry with the mail backoff.

## Billing rate

Customers pay per million billable (input + output) tokens. The rate is the
workspace's own `account.sell_usd_per_mtoken` when an operator set one (admin →
Users → Rate), else `HUNTWELL_SELL_USD_PER_MTOKEN` (default $5). Always price
through `store::sell_rate` / `effective_sell_rate` — never read the config rate
directly — so the wallet debit, the live limit, run costs and the admin's
Charged column agree. A change applies to tokens billed from then on.

Credit lands through `store::apply_credit` only: `apply_credit_purchase`
(Stripe / local mock) or `apply_credit_grant` (admin → Users → Credits → Add;
`credit_purchase.kind='grant'`, never revenue). It returns `false` for a
`payment_ref` already applied; only a `true` calls
`billing::announce_credit`, which queues the owner's "credit added" email —
so a retried payment or repeated webhook never credits or emails twice.

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

**Runs.** A workspace may run many plans at once; only the same plan twice is
refused (runner check + `execution_one_active_per_plan_idx`). With no room a
run waits `queued`: pool dispatch → the admin's placement loop starts it when a
slot frees; local dispatch → `runner::spawn_local_queue` starts it when a run
ends (`HUNTWELL_LOCAL_MAX_RUNS`, default 4; an account's runs take turns when
they share a local Chrome, i.e. without Browserbase). Queued runs can be
cancelled. Credits stay safe: debits are atomic and capped at the wallet.

`./dev.sh` starts the notification service (mail is logged without a
provider; Slack posts are delivered). When the operators' Cognito pool is set
(`ADMIN_COGNITO_USER_POOL_ID`/`_CLIENT_ID`) it also starts the admin control
plane with a process-backed local worker pool (`HUNTWELL_LOCAL_POOL`, default
2) and runs the server with `RUN_DISPATCH=pool`; without that pool the admin
is skipped and runs are `RUN_DISPATCH=local`.

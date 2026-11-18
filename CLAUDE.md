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
- **No inline `<script>` in `index.html`**: the CSP is `script-src 'self'`, so
  an inline script silently never runs (the theme script once didn't, and
  dark-mode phones flashed white). Put it in `UI/web/public/*.js` — `theme.js`
  (before paint) and `skeleton.js` (pre-script skeleton) live there.
- Tables are `components/Grid.tsx`: AG Grid on a wide screen, **cards on a
  phone** (`useNarrow`, ≤ 860px). Cards reuse each column's own
  valueGetter/field, valueFormatter and cellRenderer — first column the title,
  titled columns as label/value (empty left out), untitled ones (links,
  delete) in the corner — so a page describes its columns once. Don't drop
  columns at phone width any more; the cards have room for them.
- Loading states are skeletons (`components/Skeleton.tsx`, `.skel*` in
  styles.css), never a blank page or "Loading…". `Loading` (block) is one.
  **Gate every skeleton with `useLoadView(ready)`** so it never flickers: a
  load under 150ms shows no skeleton, and a shown one stays ≥ 700ms
  (`SKELETON_DELAY_MS` / `SKELETON_MIN_MS`). `FirstLoadHold` (main.tsx) keeps
  the first paint's skeleton until 700ms after page start while the app mounts
  hidden underneath.

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

## MCP for the user's own Claude (`cmd/src/web/mcp_api.rs`)

`/mcp` is an MCP server (Streamable HTTP, JSON-RPC POST, plain-JSON answers;
GET is 405) that a workspace adds to its own Claude as a custom connector, so
Claude — the Chrome side panel included — reads results and drafts while it
works in their browser. Not to be confused with `cmd/src/mcp.rs`, the
read-only server for Huntwell's own scraping agent.

- **Its own key kind**: `api_key.kind='mcp'` (`hwm_…`, no secret), made under
  API access → "Claude (MCP)". Claude cannot sign, so it is a bearer token —
  `Authorization: Bearer` or in the URL (`/mcp/{key}`, for Claudes without
  header support). Refused on `/v1`/`/dl`; a signed key is refused on `/mcp`.
  Otherwise a normal key: workspace, plan pin, maker's caps, audit, revocation.
- `/mcp` is merged **after** the TraceLayer in `serve_app` so a key in a URL is
  never logged; the audit records `/mcp <tool>`, never the path. JSON bodies
  only (415 otherwise).
- Tools: `list_plans`, `search_results`, `list_outreach_profiles`,
  `draft_outreach` (paid; needs the maker's plans cap, like `/v1`),
  `list_drafts`, `get_draft` (Gmail/Outlook compose links, LinkedIn profile).
  Outreach tools refuse plan-pinned keys. Nothing sends: the user does.
  Results are scraped — every tool and the server instructions say "data".
- Claude on the web connects from Anthropic's cloud: a local server needs a
  tunnel (or use Claude Code). The dev server proxies `/mcp` (vite.config.ts).

## Huntwell for Chrome (`extension/`, `cmd/src/web/extension.rs`)

An MV3 side-panel extension that sends queued outreach on LinkedIn and Reddit:
it opens the person's page, fills the message in and points at Send — **the
person presses Send; nothing in it ever clicks Send, submits or presses Enter**
(LinkedIn's terms, Reddit's spam rules, the Web Store's policy). Keep it so.

- **Sign-in** is authorization code + PKCE via `chrome.identity.launchWebAuthFlow`:
  `/app/extension/authorize` (consent page) → `POST /api/extension/authorize`
  (redirect must be `https://<id>.chromiumapp.org/…` of an allowed id:
  `HUNTWELL_EXTENSION_IDS`, default the manifest key's `BUILT_IN_ID`; any id
  in dev) → a 5-minute single-use code (`extension_grant`, hash only, claimed
  before the verifier is checked) → `POST /ext/v1/token` → its own key,
  `api_key.kind='ext'` (`hwx_…`, 180 days, its approver as maker, listed and
  revocable under API access). Works only at `/ext/v1`.
- **Queue** = `outreach.send_channel` (`linkedin`|`reddit`) + `queued_at`; sent
  = `sent_at`/`sent_via`, by the extension or "Mark sent" in the draft dialog,
  and a CRM activity when the module is on. `recipient_reddit` via
  `store::reddit_username`.
- `/ext/v1`: `me`, `queue`, `outreach/{id}/sent`, `outreach/{id}/unqueue`,
  `signout` (revokes the key). Writes need the maker's plans cap.
- Content scripts fill via a paste event (then `insertText`), detect a send by
  the box emptying after the person's click/Enter, and fall back to the
  banner's Copy / Mark sent / Skip. Branded Chrome ignores `--load-extension`,
  so test them against mock pages with a stubbed `chrome.*`.
- Branding mirrors the app: `panel.css` and the banner in `content/common.js`
  copy `styles.css`'s tokens (keep them in step); Lato is bundled in `fonts/`.

## CRM sync (`cmd/src/crm/`, `cmd/src/web/crm.rs`)

A workspace connects a CRM (`crm_connection`, Settings → CRM integrations; the
credential sealed with `signing::seal_secret`); a plan sends its results to it
(`plan_crm`, the plan's Edit tab). The **website's** sync loop
(`web::crm::spawn_sync`, 45s or woken) pushes every row a plan has that the
ledger (`crm_push`, unique per connection+row) does not — so runs on workers
know nothing of CRMs and a missed pass loses nothing. A row becomes a
`crm::Contact` (from a prospect, or a custom row read by column names), then
an adapter's records. Errors: `Auth` pauses the connection, `Retry` leaves the
row for the next pass, `Rejected` records it failed (retried 3× after 10 min).

- **Adding a CRM** = a file beside `crm/pipedrive.rs` (`connect`, `push`), a
  `crm::Provider` variant, and its credential's name in the providers list.
- **Pipedrive**: personal API token as `api_token`; the company domain comes
  from `/v1/users/me`, so only the token is pasted. Organization by exact name
  (else created), person matched by exact email (else created, v2 API), an
  escaped HTML note with everything else, an optional lead (v1). Hosts are
  only `*.pipedrive.com`; `HUNTWELL_PIPEDRIVE_API_BASE` points it at a stand-in,
  honoured only with `HUNTWELL_DEV`.

## Modules and the built-in CRM (`web/contacts.rs`, `pages/Crm.tsx`)

`account.modules` (comma-separated, `store::MODULES`) is what an operator
switches on per workspace — admin → Users → Modules, saved on tick
(`PUT /admin/api/accounts/{id}/modules`). Unknown ids are dropped; off is the
default. `/api/me` carries `modules`; the UI gates with `hasModule(me, …)`, the
API with `store::has_module` on **every** route, reads included. Add a module
= a `MODULES` entry + its gate.

`crm` is a deliberately simple CRM (modelled on Park River's, minus everything
advisor-specific): `contact` (fixed stages `CONTACT_STAGES`: lead → contacted →
qualified → customer / lost, a follow-up date, `last_touch_at`) and
`contact_activity` (note/email/call/meeting/linkedin, `occurred_at`; logged by
people, nothing is sent). Contacts come by hand, from a whole plan
(`/api/contacts/import {plan_id}`, ≤ 2000) or one result's "Add to CRM". A
result goes through `crm::Contact::from_prospect`/`from_artifact` — the same
reading the CRM *integrations* use — and imports once: same result again =
`Already` (unique `(account, source_kind, source_id)`), same email =
`Linked` (blanks filled, nothing doubled). Columns with no field land in
`notes`. Not to be confused with `crm_connection`/`crm_push`, which push to
someone else's CRM.

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

**Dev sign-in**: a dev server (`--dev`, never production) signs email `test`
with an empty password in as `test@huntwell.test` (made on first use;
`web::auth::dev_login`), because a dev box has no Cognito pool. The sign-in
form relaxes its fields when `/api/auth/config` says `dev_login`. The admin does the same
(`admin::api::dev_login_allowed`, `HUNTWELL_DEV`, as `test@huntwell.test`), so `dev.sh` starts the
console even without an operators' Cognito pool (then with no worker pool, and runs stay local).

## Password reset (`web/auth.rs` `issue_password_reset`, `pages/Auth.tsx` `Forgot`)

"Forgot your password?" and admin → Users → **Send password reset** send the
same email: a one-click link (`/forgot?token=hwr_…`, its own 192-bit token,
hash in `account.reset_link_hash`) and a six-digit code for another device,
both good for `store::RESET_VALID_HOURS` (24). Using either spends both; a new
reset replaces the last. The link page strips the token from the URL, shows
whose account it is, and asks only for the new password (twice). The password
is checked against the pool's rule **before** the link or code is spent, and a
claimed link is restored if Cognito then refuses — a failed reset never costs
the person their email. Never put the code in a link.

## Support chat (`web/support.rs`, `admin/api.rs` support, `crate::support`, `components/SupportChat.tsx`)

A "Help" bubble on every page (public site, sign-in, the app) opens a chat;
operators answer in admin → **Support** (inbox, the chat, who the person is in
Huntwell: workspace, credit, recent runs). Tables `support_conversation` /
`support_message`; not tenant data, like `waitlist`.

- **Ways in, never a bare id**: signed in, the session (`/api/support/mine`,
  one current chat per person); a visitor, a `hwc_` token from `/support/start`, kept
  in localStorage, sent in JSON bodies only. A visitor's **first step is their
  name (required), email and Cloudflare Turnstile**; it opens an empty chat,
  and only the first message posts to Slack (an empty chat is not in the
  inbox). **No Turnstile secret, no public chat**: `/support/start` refuses and
  `/api/auth/config` says `support_chat: false` (dev excepted). The row holds its SHA-256 and a
  sealed copy so a reply email can link back: `/#support=<token>` (a fragment,
  so no server log sees it); signed-in people get `/app#support`.
- **Like Alpha Tech's terminal** (`~/Desktop/projects/atech`, `servers/ui_support.rs`): a person has one
  chat at a time, can **end** it (status `ended`: read-only to them, in "Earlier chats"; an answer still
  arrives and does not reopen it; not the operator's to reopen) and start a new one. Signed in: the current
  chat is the newest not ended (`/support/mine`, `?id=` for an earlier one, checked against the account).
  A visitor: one `hwc_` token per chat; the bubble keeps earlier tokens (`huntwell.support.past`) and
  `/support/history` lists those. An operator's "Close" is different: writing again reopens it.
- **Screenshots, and only screenshots** (as in Alpha Tech): the camera button captures the tab
  (`supportShots.ts`, `getDisplayMedia`; https or localhost, not phones) and `SnapshotRegion` drags out
  an area. No file upload, no paste. ≤ 4 per message, ≤ 4 MB, type sniffed from the bytes
  (`store::support_image_type`), kept in `support_attachment`. The bubble fetches one by POST
  (`/api/support/image`, the visitor's token in the body, never a URL); the admin by
  `/admin/api/support/image/{id}`. Each is uploaded into the Slack thread (`files:write`), best effort;
  images posted in Slack do not come back.
- **Every chat is a Slack thread, both ways** (`support/slack.rs`): a Slack app, set on the Support page
  (bot token `xoxb-` and the app's **signing secret**, both **sealed** in `control_setting`, and a
  channel id; the page shows the app manifest with the exact scopes and the Request URL). The parent is
  "*Name (email)* needs support:" with their words quoted; each later message of theirs is quoted into the
  thread **and broadcast to the channel**; an answer typed in the admin goes into the thread too
  (`support_message.slack_ts` empty = still owed; the notification service drains every 2s with the mail
  backoff). Ending a chat posts "_Name ended this conversation._". A **reply in the thread** is the
  operator answering: Slack's **Events API** calls `POST /slack/events` on the website
  (`HUNTWELL_PUBLIC_URL` + that path; Vite proxies `/slack` for a tunnel), `slack::verify_signature`
  checks it (5-minute window) before anything is read, and it is answered at once and delivered after. It
  is recorded once per Slack ts, reaches the bubble, is emailed if they are away, and gets a ✅ (plus 📧
  when emailed). Replies starting `//` stay in Slack; bots, edits and other threads are ignored. Anyone
  in the channel can answer customers. Everything the person typed is `slack::escape`d; Slack's markup
  is turned back into plain text (`support::slack::plain`). `HUNTWELL_SLACK_API_BASE` points it at a
  stand-in, honoured only with `HUNTWELL_DEV` (`support::slack::two_way_tests`).
- A reply is emailed (`mail::support_reply`) only when the person has not had
  the chat open for 2 minutes. Starting is `SUPPORT_CHATS` (5/h per address);
  messages `SUPPORT_MESSAGES` (20 per 10 min per chat). `purge_account` takes a
  person's chats. Test: `admin::api::support_tests` (ignored; dev DB).

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

Cold emails drafted to a prospect, a custom-columns result (`artifact_id`:
`outreach::recipient_from_record` reads it by column names — an Email-like
column, else any address in it, fills the To) or a hand-entered person —
drafted, never sent by Huntwell: the person sends them. The draft dialog's
buttons hand a draft over ready to send — Gmail and Outlook on the web
(compose links), the mail app (mailto), and LinkedIn (no compose link exists:
the body is copied and their profile, `outreach.recipient_linkedin` from
`outreach::linkedin_url`, or a people search opens). No mailbox is connected
and nothing is automated on LinkedIn — deliberately (its terms forbid it). One `llm` call per draft or
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

## Mail (`cmd/src/mail.rs`, `notification.rs`, `mail_outbox`)

Everything is queued (`store::queue_mail`) and the **notification service**
sends it through Resend (`RESEND_API_KEY`, `HUNTWELL_MAIL_FROM` on a domain
verified in Resend); without a key it is logged, not sent. A link is built
with `mail::link_base()`: `HUNTWELL_PUBLIC_URL` where the process has it, else
`mail::PUBLIC_URL_MARK`, which the service fills in at send — workers never
have the URL (a run's "new results" alert used to be dropped there, silently).
No URL on the service either → the mail is held with that reason on its row.
`dev.sh` sets `http://localhost:$UI_PORT` when global has none.
A run's alert (`mail::new_records`) shows its new rows as cards
(`mail::AlertCard`, ≤ 10, ≤ 8 columns each, labels over values so a phone
never scrolls sideways); everything in a card is scraped, so escaped, and a
value is a link only if `safe_url`/`safe_email`. `cargo test dump_mail_samples
-- --ignored` writes every message to `cmd/target/mail-samples/` to look at.

## Billing rate

Customers pay per million billable (input + output) tokens. The rate is the
workspace's own `account.sell_usd_per_mtoken` when an operator set one (admin →
Users → Rate), else `HUNTWELL_SELL_USD_PER_MTOKEN` (default $1, the price the Pricing page quotes from `/api/auth/config`). Always price
through `store::sell_rate` / `effective_sell_rate` — never read the config rate
directly — so the wallet debit, the live limit, run costs and the admin's
Charged column agree. A change applies to tokens billed from then on.

Credit lands through `store::apply_credit` only: `apply_credit_purchase`
(Stripe / local mock) or `apply_credit_grant` (admin → Users → Credits → Add;
`credit_purchase.kind='grant'`, never revenue). It returns `false` for a
`payment_ref` already applied; only a `true` calls
`billing::announce_credit`, which queues the owner's "credit added" email —
so a retried payment or repeated webhook never credits or emails twice.

**Free credit runs without a card.** A workspace an operator granted credit
to (`kind='grant'`) may run with no card while it has credit left
(`store::spends_free_credit`, checked in `runner::start`); once spent, the
card is asked for, with "your free credit is used up". `/api/billing` says
`card_required` / `free_credit` / `free_credit_spent` — the UI gates on
`card_required`, never on `has_card`.

Auto-reload (`billing.rs`, "auto-reload"; `AutoReloadCard.tsx`): when spendable
credit falls below `auto_reload_below_micros`, the **website's** loop
(`spawn_auto_reload`, 20s, the only process with the Stripe key) charges the
saved card off-session and credits it as `kind='auto'`. Only with a recorded
agreement (`auto_reload_agreed_at/by`, `auto_reload_terms` = `AUTO_RELOAD_TERMS`;
the wording is in `AutoReloadCard` — change both, bump the version); any change
to the numbers asks again. Claims are atomic with a lease; the Stripe
idempotency key is `huntwell-auto-{account}-{n}`, so a retry is the same
charge. At most `AUTO_RELOAD_MAX_PER_DAY` (3) per 24h. A card error or a
bank wanting 3-D Secure turns it off and emails the owner; removing the card
turns it off.

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

## Editing a plan's columns (`web/api.rs` `column_edit`, `PlanSettings.tsx`)

Each column keeps what the person said goes in it (`FieldSpec.note`, set by
`columns_to_schema`); the planner reads it, a rebuild reuses it, and the Edit
tab shows the same `ColumnsEditor` as the home page (drag to reorder,
multi-line descriptions). Saving: the same columns in a new order are applied
in place, keeping each column's spec (no rebuild); added, removed, renamed or
redescribed columns are refused unless `rebuild` is on, and rebuild from them
(and mark the plan's columns the person's own). Results are keyed by column
name, so a renamed column is empty for rows found before.

## Admin → Schedule (`admin/api.rs` `schedule` / `schedule_view`)

Every scheduled plan in the installation and its runs in the next 24h / 3d /
7d, as a list and a Gantt timeline. Run times come from
`store::fires_between`, which steps `next_fire` — the scheduler's own rule
(account timezone, chosen days) — so the page cannot disagree with the
scheduler. A bar's length is the plan's average over its last ten finished
runs, else a default for its effort (quick 5m, normal 15, thorough 30,
exhaustive 60). The "runs at once" strip and the peak are held against
`store::run_capacity` (local pool children + running worker VMs' slots).

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
still starts (dev sign-in: email `test`, no password) and runs are `RUN_DISPATCH=local`.

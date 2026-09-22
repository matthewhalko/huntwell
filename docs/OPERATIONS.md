# Operations

## Local

```bash
./local-infra/start.sh                 # Postgres up, schema applied, global written
./dev.sh                               # server (--dev) + Vite
./dev.sh --no-ui                       # server only
cmd/target/debug/huntwell doctor     # agent / npx / Chrome / database check
./local-infra/stop.sh
FRESH=1 ./local-infra/start.sh         # wipe this instance's data
```

Logs: `local-infra/data/logs/*.log` (Postgres), the server's stdout, and per
run the `execution_log` table (visible live in the UI).

## Release

```bash
./build.sh            # zig cross-compile, linux/amd64 -> bin/ubuntu/
./build.sh arm64      # -> bin/ubuntu-arm64/
./build.sh host       # native
```

**Production is [PRODUCTION.md](PRODUCTION.md)**: Incus VMs on bare-metal
servers, driven by the admin. What follows is the single-server alternative —
one binary, runs as child processes — the simplest thing that works, with no
control plane to operate.

On the server:

1. Postgres 16+ with a database and a role; put `HUNTWELL_DATABASE_URL` in
   `/opt/huntwell/global` (copy `global.example`), plus `HUNTWELL_ADDR`,
   `HUNTWELL_DATA_DIR`, `HUNTWELL_SESSION_SECRET`, `CURSOR_API_KEY`.
2. Install the Cursor `agent` CLI, Node 20+, Google Chrome, and `xvfb`
   (`HUNTWELL_CHROME_DISPLAY=xvfb`, or `headless` if you accept the
   detectability trade-off).
3. Install `deploy/huntwell.service.in` with `@APP_DIR@`/`@APP_USER@`
   substituted; `systemctl enable --now huntwell`.
4. Put Caddy (or any TLS terminator) in front — `deploy/Caddyfile.example` —
   and set `HUNTWELL_TRUST_PROXY=1` (the *last* `X-Forwarded-For` entry, the
   one the proxy appends, is taken; `cloudflare` reads `CF-Connecting-IP`).
5. Sign up, then set `HUNTWELL_OPEN_SIGNUP=0` and restart.

To run executions in worker VMs instead of as child processes of this server,
use the admin control plane — see [PRODUCTION.md](PRODUCTION.md).

The binary applies the schema at startup (idempotent, under an advisory lock),
so a deploy is: copy binary, restart.

## Reading a failed run

The admin's **Logs** page lists the last 25 executions; click one to see
everything it printed — the friendly feed a customer sees hides the raw lines,
and the reason a run failed is nearly always in those.

- It opens filtered to **problems only** when the run has any, since that is
  usually why you clicked. Untick it for the whole log.
- `stderr` is not treated as "a problem" by itself: the guard and the trail
  both narrate there. What is highlighted is the wording the pipeline uses when
  something actually broke — `! `, `✖`, `error`, `failed`, `panicked`,
  `refused`, `could not`, `unavailable`, `exhausted`.
- **Copy** puts what is on screen on the clipboard, filter included.

The same thing straight from the database, when you are already on the box:

```
sudo -u postgres psql huntwell -At -F' | ' -c \
"select seq, stream, line from execution_log where execution_id=N order by seq"
```

### When a run finds less than it should

The run log explains itself before you have to ask:

- **An empty search says why.** A scrape that returns no rows logs how many turns
  and pages it used and *what the model said* — "came back empty after 1 turn(s) and
  0 page(s). It said: …". A model that answered from memory looks nothing like one
  that searched and found nothing, and this tells them apart.
- **A stage that answered without opening a page is sent back once**, told that every
  row must come off a page it actually read. If it does it twice, that is in the log.
- **A reply that is not the JSON the task asked for** gets one more try before the
  round is written off.
- `HUNTWELL_AGENT_TRACE=1` narrates every turn: the stop reason, how many tool calls,
  tokens in and out, and the first 400 characters of what the model said. Verbose —
  turn it on to diagnose a plan, off again afterwards. The admin pushes it to VMs on
  Deploy, so it can be set without a rebuild.

All of it lands in `execution_log`, so the admin's **Logs** page (click a run) is where
to read it.

## Watching a run's browser

"Watch the browser" opens **Huntwell's own viewer**: the app polls
`/api/executions/{id}/browser/frame`, which takes a JPEG off the browser over
CDP and returns it. The provider's own live-view URL never reaches the client,
and is no longer printed into the run log either — that log is customer-visible
and the URL is a bearer capability, so whoever held it could *drive* a browser
carrying that workspace's logged-in sessions.

The viewer is read-only by design. Watching is what it is for, and a view that
could click would be a way to take the browser away from a running plan.

Frames are about one every 1.5s; each is its own short CDP connection, so a
watcher costs a little and an unwatched run costs nothing.

## Cost controls

`cmd/src/thrift/` does, in code, work a run would otherwise pay an agent for. All
of it is on by default and each piece falls back to the agent when unsure.

| Piece | What it does | In the run log |
|---|---|---|
| `structured` | Reads a row's page over HTTP; published schema.org values fill blank columns and give enrich a head start | `page data: …` under each enrich |
| `trim` | Trims the page snapshots the agent reads (wrappers, icon glyphs, repeated headers/footers) | `trim  page snapshots … (N% smaller)` at the end of the run |
| `watch` | Skips a **scheduled** run when its listing pages show no new link | `[skip] nothing new on …` — the run succeeds with 0 new and 0 tokens |
| `twins` | The same listing on another site is not enriched twice | `same listing as … on another site` |
| `filters` | Rows outside a column's `min`/`max` are dropped before enrich | `outside the plan's limits` |
| `rounds` | The search runs as one short call per named site (up to 4 a round), each told a page budget and to take rows from listing pages only, and stops once the target is met | `rounds N call(s), one per site` · `→ scrape <site>: N row(s)` |

- Turn pieces off: `HUNTWELL_THRIFT_OFF=watch,trim` (or `all`). Set it in the `setting`
  table or Secrets Manager; the admin passes it to worker VMs on Deploy.
- **How hard a plan tries is the plan's own `effort` setting**, which the owner picks:
  quick / normal / thorough / exhaustive. It decides both how many rounds a run does *and*
  how many pages each round may read (5 / 10 / 20 / 35). On the direct loop the page budget
  is enforced — past it `browser_navigate` refuses and the model answers with what it has.
  `HUNTWELL_SCRAPE_PAGE_BUDGET` overrides it install-wide, for a deployment that wants a
  firmer ceiling than any effort level.
- A search call also carries a short **record of ground already covered** — the searches it
  has run and the pages it has opened in this call, a line each. The pages themselves are
  compacted away; without the record the model re-runs its own searches and a round treads
  the same ground until it runs out of turns.
- `HUNTWELL_WATCH_MAX_SKIPS` (default 2): consecutive scheduled runs a plan may skip.
  The next one always runs for real. Runs started by hand are never skipped.
- Large sites (cars.com, eBay, Zillow, Indeed…) refuse plain HTTP. For those,
  `structured` and `watch` do nothing and the run behaves as before; `trim`, `twins`
  and `filters` still apply.

## Choosing a model provider

A stage's model id says which engine runs it:

| Setting | Engine |
|---|---|
| `gemini:gemini-2.5-flash` | Huntwell's own agent loop, talking to Google directly |
| `anthropic:claude-haiku-4-5` | the same loop, talking to Anthropic |
| `composer-2.5`, `auto`, or empty | the Cursor CLI, as before |

Set them on the admin's **Models** page. The dropdown asks each configured
provider what it currently offers, so a retired model is never listed, and
saving one the provider no longer has is refused with a reason. Prices come
from a table keyed by model family; anything from an unknown family shows
"price unknown". A **Model providers** card underneath shows which keys are
present.

Providers: `gemini`, `anthropic`, `openai`, `deepseek`, `groq`, `mistral`,
`xai`. Each needs `<PROVIDER>_API_KEY` in Secrets Manager and takes an optional
`<PROVIDER>_BASE_URL` for a proxy or a regional endpoint. The admin pushes them
to the app VM and to worker VMs on Deploy.

What changes when a stage runs on the direct loop:

- **Only the last two pages stay in the conversation.** Older ones become a
  one-line note. That is what stops a long search costing the square of its
  pages; the run log ends with `context  N KB of page text dropped`.
- **The page budget is enforced**, not suggested: past it `browser_navigate`
  refuses and the model returns what it has.
- **The model is offered six browser tools and the plan's memory, and nothing
  else** — no shell, no file tools, so nothing to refuse and no sandbox to
  maintain.
- **The guard answers before a page is fetched** rather than after.
- The browser is unchanged: the same Browserbase session, Context, proxies and
  live view.

`HUNTWELL_LLM_CONCURRENCY` (default 4) caps in-flight calls per provider per
worker VM. `HUNTWELL_LLM_ATTEMPTS` (default 4) is how many times a rate-limited
or failed call is retried.

DeepSeek is a Chinese company: page content and briefs leave the US. Enable it
only deliberately.

## Things that will bite

| Symptom | Cause |
|---|---|
| Runs fail immediately with "no browser to scrape with" | No display and no Xvfb; set `HUNTWELL_CHROME_DISPLAY` or install `xvfb` |
| Run log shows the agent guessing at `prospects` server names | `huntwell mcp-prospects` could not start — usually `HUNTWELL_DATABASE_URL` not reaching the child; check `huntwell config get HUNTWELL_DATABASE_URL` as the service user |
| "plan is already running" but nothing is | The previous server died mid-run; restart the server (it marks stale runs failed) or cancel from the UI |
| Everyone signed out after a restart | `HUNTWELL_SESSION_SECRET` missing — sessions are stored server-side, but cookies are not `Secure` in `--dev`; in production make sure the file is readable by the service user |
| API rate limits or sign-in lockouts hit everyone at once | `HUNTWELL_TRUST_PROXY` unset behind a proxy, so every caller is the proxy's address (the admin sets it on the app VM automatically) |
| A new account never gets its confirmation code | Mail is not sending: check the `notification` service's log in the app VM — Resend's own reason is in it ("domain is not verified", "API key is invalid"). Check `RESEND_API_KEY` in the secret and that `HUNTWELL_MAIL_FROM` is on a verified domain. Codes sit in `mail_outbox` until it does |
| Sign-in says "verification failed" for everyone | `TURNSTILE_SECRET_KEY` does not match the widget's site key — the admin log has Cloudflare's error code |
| UI 404 "UI bundle not built" | The binary was built without `UI/web/dist`; run `npm run build` in `UI/web` and rebuild |

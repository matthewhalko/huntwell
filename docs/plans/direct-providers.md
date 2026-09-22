# Plan: run the agent ourselves, on any model provider

Status: **phases 1–3 and 5 built** (2026-09-21); phase 0 (baseline) and phase 4
(parity runs) are measurements that need a provider key and production traffic.
Phase 6 (the five extra adapters) is built too — all seven providers pass the
conformance suite. What remains is switching the defaults, which is phase 4's
answer to give.

Built: `cmd/src/llm/` (the provider layer), `cmd/src/direct/` (the loop, the
CDP browser tools, context compaction), routing in `pipeline::agent_call`, and
the admin's Models dropdown and Providers card.

**Not yet measured:** every price in this document is from memory and every
saving is an estimate. No run has yet gone through the direct loop on a real
provider.

## Why

Every agent call today goes through the Cursor CLI. Cursor decides the model
(unless told), keeps every page the agent has read in the conversation forever,
re-sends its own system prompt and coding-tool catalogue on every step, and
bills us at (roughly) provider list price. Three costs follow:

1. **Quadratic re-reading.** Step *n* of a search re-reads pages 1..n-1. We
   can't drop a page once its rows are out, because we don't own the conversation.
2. **Harness overhead.** Cursor's system prompt and tool definitions (Shell,
   Glob, Read, Write, …) ride along on every step; run #5 shows the model
   reaching for `Glob` and `Shell` — tools it should never see.
3. **No provider choice.** Cheap models with good tool use (Gemini Flash-Lite,
   DeepSeek, Groq-hosted open models) are unavailable or priced by Cursor.

Owning the loop fixes all three, and makes the guard stronger: today it judges a
tool call *after* Cursor ran it; in our loop it judges *before*.

Expected effect (estimate, measured in phases 0 and 4): 3–5× fewer tokens per
search than Cursor with the same model, on top of `thrift` (rounds, trim).

## Non-goals

- Not a rewrite of the pipeline. `pipeline.rs`, `plan_chat.rs`, `guard`,
  `trail`, `meter`, `progress` keep their shapes and their event formats.
- Not dropping Cursor on day one. It becomes one adapter (`cursor`) and stays
  until the direct loop beats it on the same plan.
- No aggregator. Every provider is talked to directly, on its own API, with
  its own key — one adapter per provider, all behind one interface.

## What exists today (the seams)

| Piece | Where | What it needs from the agent |
|---|---|---|
| `ask_agent(label, prompt, opts) -> Value` | `agent.rs:1115` | The only entry point. 12 call sites: `pipeline.rs` (via `agent_call`) and `plan_chat.rs` (5). Returns the JSON the reply carried. |
| Stream reader | `agent.rs:737 read_stream` | Consumes Cursor's `stream-json` events; feeds `progress::parse_tool_call`, `guard.inspect_call`, `trail::note_call`, `accumulate_usage`. |
| Guard | `guard.rs` | `inspect_call(tool, args, result, call_id)` — host allowlist, restricted platforms, denied tools, injection markers. |
| Trail | `trail.rs::note_call(args, result)` | Reads URLs out of tool args/results. |
| Meter | `meter.rs`, `agent.rs::parse_usage` | `TokenUsage {input, output, cache_read, cache_write}` + provider cost per call. |
| Progress | `progress.rs` | Narrates tool calls (`describe_tool`), heartbeats, warnings. |
| Browser | `browser.rs` (CDP helpers, `TabScope`, watchdog), `browserbase.rs` | The Chrome is ours already; Playwright MCP only *attaches* to it. |
| Plan memory tools | `mcp.rs` | Tool definitions + handlers, today served over MCP stdio. |
| Sandbox | `sandbox.rs` | Exists only to fence Cursor's own tools. Unnecessary for the direct loop. |
| Security preamble | `agent.rs::GUARD_PREAMBLE`, `compose_prompt` | Prepended in code. Becomes the system prompt, unchanged. |
| Model settings | `agent.rs::stage_model`, admin Models page, `control_setting` | Per-stage model id string. |
| Prices | `model_catalog.rs::cursor_rate` | Rates by model id fragment. |

## Target design

```
ask_agent(label, prompt, opts)                      opts.model = "<provider>:<model>"
   │
   └─ agent::loop::run(label, prompt, provider, model)
         │
         │  system  = GUARD_PREAMBLE (+ memory note)          ← unchanged text
         │  tools   = browser tools + plan-memory tools        ← agent/tools.rs
         │
         │  loop:
         │    reply = provider.complete(&Request)              ← llm::Provider (one adapter per provider)
         │    meter ← reply.usage                              ← same TokenUsage as today
         │    for call in reply.tool_calls:
         │        guard.judge(tool, args)      BEFORE running  ← refusal = tool error, run continues
         │        result = tools::run(tool, args)              ← CDP; trail.note_call
         │        guard.inspect(tool, args, result)
         │        messages.push(ToolResult)
         │    context::compact(&mut messages)                  ← drop old page snapshots, keep rows
         │  until reply.text → extract_json
```

### 1. `llm/` — the provider layer

One interface, one file per provider, and a shared kit so each adapter is
mostly "translate this shape to that shape".

```
cmd/src/llm/
  mod.rs          Provider trait, Request/Reply/Message types, registry, parse_model_id
  kit.rs          shared: reqwest client, retry/backoff, concurrency cap, SSE reader,
                  error classification (rate limit / auth / provider down / bad request)
  usage.rs        usage → store::TokenUsage (input, output, cache_read, cache_write)
  conformance.rs  the test suite every adapter must pass (see §1.3)
  gemini.rs       Google AI Studio API  (generateContent, functionDeclarations, cachedContents)
  anthropic.rs    Messages API           (tool_use/tool_result blocks, cache_control)
  openai.rs       Chat Completions / Responses API (tools, tool_choice, cached_tokens)
  deepseek.rs     DeepSeek API           (Chat Completions shape; prompt-cache-hit usage fields)
  groq.rs         Groq API               (Chat Completions shape; x-ratelimit headers)
  mistral.rs      Mistral API            (Chat Completions shape)
  xai.rs          xAI API                (Chat Completions shape)
  cursor.rs       today's CLI path wrapped as an adapter, for parity runs and fallback
```

#### 1.1 The interface

```rust
pub struct Request {
    pub model: String,
    pub system: String,                 // GUARD_PREAMBLE + notes; adapters may mark it cacheable
    pub messages: Vec<Message>,         // User(text) | Assistant{text, tool_calls} | ToolResult{call_id, name, content}
    pub tools: Vec<ToolDef>,            // name, description, JSON-schema parameters
    pub max_output_tokens: u32,
    pub temperature: Option<f32>,
}

pub struct Reply {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,      // id, name, args: serde_json::Value
    pub usage: TokenUsage,              // normalised by usage.rs
    pub cost_micros: Option<i64>,       // only if the provider states it
    pub stop: Stop,                     // EndTurn | ToolUse | MaxTokens | Refused
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;                        // "gemini", "anthropic", …
    fn configured(&self) -> bool;                        // key present
    async fn complete(&self, req: &Request) -> Result<Reply, LlmError>;
    fn rate(&self, model: &str) -> Option<Rate>;         // list price for the admin estimate
    fn models(&self) -> Vec<ModelInfo>;                  // static catalogue for the Models page
    fn caps(&self, model: &str) -> Caps;                 // context window, tool use, caching, json mode
}
```

`LlmError` has exactly the classes the loop reacts to: `RateLimited{retry_after}`,
`Unauthorized`, `Unavailable`, `BadRequest(String)`, `ContextTooLong`. Each
adapter maps its provider's status codes and error bodies onto these; the loop
never sees provider-specific text (and neither does a customer — the same rule
as `cognito::UNAVAILABLE`).

**Registry.** `llm::registry()` builds every adapter once from settings;
`llm::for_model("gemini:gemini-2.5-flash") -> (&dyn Provider, model)`. A bare
id (today's settings) resolves to `cursor`. A provider whose key is missing is
listed as *not configured* and refused at draft time with a clear message, not
mid-run.

**Secrets.** `GEMINI_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`,
`DEEPSEEK_API_KEY`, `GROQ_API_KEY`, `MISTRAL_API_KEY`, `XAI_API_KEY`
(+ optional `<PROVIDER>_BASE_URL` for a proxy or a regional endpoint). Added
to `config::settable`, `incus_driver::SHARED_SETTINGS` (workers call models),
`docs/SECRETS.md`, `global.example`.

#### 1.2 What each adapter has to do

The differences between providers are small and known; each adapter is
100–250 lines and does only these five things:

1. **Auth + endpoint.** Header name and URL. (`x-api-key` + `anthropic-version`
   for Anthropic; `Authorization: Bearer` for the rest; Gemini takes
   `x-goog-api-key`.)
2. **Request shape.** System prompt placement (top-level `system` for
   Anthropic, `systemInstruction` for Gemini, a `system` message for the
   Chat-Completions family); tools (`tools[].function` vs `tools[].input_schema`
   vs `functionDeclarations`); tool results (`role: tool` vs `tool_result`
   block vs `functionResponse` part).
3. **Reply shape.** Where text and tool calls live; `finish_reason` →
   `Stop`. Streaming is *not* required in v1 — the loop is turn-based and the
   progress feed is driven by tool calls, not tokens. Adapters may add SSE
   later behind the same trait.
4. **Usage.** Field names differ everywhere (`usage.prompt_tokens` +
   `prompt_tokens_details.cached_tokens`; `usage.input_tokens` +
   `cache_read_input_tokens`; `usageMetadata.promptTokenCount` +
   `cachedContentTokenCount`; DeepSeek's `prompt_cache_hit_tokens`).
   `usage.rs` has one normaliser per shape; adapters pick one.
5. **Caching hints.** Anthropic: `cache_control: {type: "ephemeral"}` on the
   system prompt and the tool list. Gemini: implicit on repeated prefixes;
   explicit `cachedContents` later. OpenAI/DeepSeek: automatic prefix caching.
   The adapter's job is to keep the fixed prefix byte-identical across calls,
   which `Request` already guarantees (system + tools first, in a fixed order).

Rate limits and concurrency are in `kit.rs`, not in adapters: a per-provider
semaphore (default 4 in-flight per worker VM, configurable) and exponential
backoff honouring `Retry-After`.

#### 1.3 Conformance suite — how an adapter proves itself

`llm/conformance.rs` is a set of tests that take a `Box<dyn Provider>` plus a
stand-in HTTP server (the pattern `turnstile.rs` and `mail.rs` already use) and
check the same things for every adapter:

- sends the system prompt, the tools and the messages in the provider's shape (golden request JSON per adapter, checked into `cmd/tests/llm/<provider>/`);
- parses a text reply, a tool-call reply, a mixed reply, and a `max_tokens` stop;
- round-trips a tool result and gets the follow-up reply;
- normalises usage including cached tokens;
- maps 401 → `Unauthorized`, 429 (+ `Retry-After`) → `RateLimited`, 5xx → `Unavailable`, an oversized request → `ContextTooLong`;
- never puts provider error text in the `Display` of an error a customer could see.

An adapter is "done" when it passes the suite; an `#[ignore]`d live test per
adapter (`<PROVIDER>_API_KEY=… cargo test live_gemini -- --ignored`) runs one
real tool-use exchange for a sanity check before a release.

#### 1.4 Adding a provider later (the checklist)

1. Copy the closest adapter (most new providers use the Chat Completions shape — copy `openai.rs`).
2. Change auth, URL, and any field names; pick the usage normaliser.
3. Fill `models()` and `rate()` from the provider's pricing page (date the entry).
4. Add the key name to `settable`, `SHARED_SETTINGS`, `docs/SECRETS.md`.
5. Register it in `llm::registry()`.
6. Pass the conformance suite; add the golden request files; run the live test once.

Half a day per provider.

### 2. `agent/tools.rs` — the tools the model sees

Exactly these, and nothing else:

| Tool | Does | Over |
|---|---|---|
| `browser_navigate(url)` | go to URL, return snapshot | CDP `Page.navigate` + load wait |
| `browser_snapshot()` | accessibility tree of the current page, trimmed by `thrift::trim` **in memory** — no file | injected script (ported from Playwright's) via `Runtime.evaluate` |
| `browser_click(ref)` | click element | `DOM.resolveNode` + `Input.dispatchMouseEvent` |
| `browser_type(ref, text, submit?)` | fill a field | `Input.insertText` / key events |
| `browser_press_key(key)` | Enter / PageDown / Escape | `Input.dispatchKeyEvent` |
| `browser_back()` | history back | `Page.navigateToHistoryEntry` |
| `prospect_known(…)` and the other plan-memory tools | plan memory | `mcp.rs` handlers, called in-process |

No `evaluate`, no `run_code`, no files, no shell. `DENIED_BROWSER_TOOLS` and
`sandbox.rs` reduce to "the list above". `guard::judge_browser_host` runs
**before** `browser_navigate` executes; a refusal is returned to the model as a
tool error and the run goes on.

**Browserbase is unchanged.** The tools speak CDP to whatever
`browser::cdp_endpoint()` returns — today that is `browserbase::connect_url()`
on the workers (the cloud session with the account's Context, proxies, live
view) and the local Chrome in dev. Playwright MCP is only an *attachment* to
that endpoint; removing it removes nothing Browserbase does. Session start,
keep-alive, release on every exit path, the persistent Context per account and
the watch-live link all stay in `browserbase.rs` exactly as they are. The one
difference is that the CDP websocket is opened by our process instead of by a
Node child, which is a smaller footprint on the worker VM (no `npx`).

Snapshots keep Playwright's format (role, name, `[ref=eN]`, `- /url:`) so
`trim`, `trail`, `guard::collect_hosts` and the prompts keep working. Refs map
to CDP backend node ids held per page.

### 3. `agent/loop.rs` and `agent/context.rs` — the conversation

- Builds the `Request` from `compose_prompt` (unchanged text) + tools.
- Emits the **same event shapes** `read_stream` produces today
  (`tool_call started/completed`, `usage`, `result`) into the existing
  consumers; `progress`, `guard`, `trail`, `meter` need no changes.
- **Compaction** — the reason for the plan. A page snapshot stays verbatim only
  while it is one of the last *K* (2) tool results; older ones are replaced by
  `"[page <url> — read earlier; N rows taken from it]"`. The system prompt
  says so, so the model extracts rows as it goes (rounds already ask for that).
  Hard cap on conversation size; at the cap the loop asks for the final answer.
- **Page budget enforced.** The loop counts navigations; at the budget,
  `browser_navigate` returns a tool error saying the budget is spent and the
  model returns its rows. (Today the budget is only an instruction.)
- Timeouts, `CreditsExhausted`, `Unavailable`, `ContainmentBreach` raised the
  same way `raw_ask_agent` raises them.

### 4. Cost accounting and the admin

- `Reply.usage` → `meter::record`, per label/stage, unchanged downstream.
- `model_catalog.rs` asks each adapter for `rate(model)`; `estimate_cost_micros`
  uses it. "Our cost" is *reported* when a provider states cost, *estimated*
  otherwise — same column, same tooltips as now.
- Admin Models page: a dropdown per stage grouped by provider, from
  `registry()` → `models()`, showing only configured providers; a Providers
  card on Home showing which keys are set (never the keys) and each
  provider's last error class, so a bad key is seen before a run fails.

### 5. Configuration and rollout switches

- Stage model ids become `provider:model`. Existing bare ids read as
  `cursor:<id>`; nothing breaks.
- `HUNTWELL_AGENT_SHADOW=1`: on a hand-started run, the scrape runs on the
  direct loop *and* on Cursor; both token and row counts go in the run log;
  Cursor's rows are used. This is the measurement for phase 4.

## Providers to write adapters for

Prices are **from memory as of mid-2026, per million tokens; check each
provider's page before deciding.** "Tool use" is required and all of these
have it. Order = the order to build.

| # | Provider | Cheap models to try | ≈ $ in / out | Context | Why / notes |
|---|---|---|---|---|---|
| 1 | **Google Gemini** | `gemini-2.5-flash`, `gemini-2.5-flash-lite` | Flash ≈ 0.30 / 2.50 · Lite ≈ 0.10 / 0.40 | 1M | Best value for page reading; long context; implicit caching ≈ 75–90% off repeated prefixes. |
| 2 | **Anthropic** | `claude-haiku-4-5`; Sonnet 5 for judgment stages | Haiku ≈ 1 / 5 | 200k | Strongest tool use per dollar in class; explicit caching at 10% of input; Batch API −50% for scheduled runs. |
| 3 | **OpenAI** | `gpt-5-mini`, `gpt-5-nano` | mini ≈ 0.25 / 2 · nano ≈ 0.05 / 0.40 | 400k | Nano is the cheapest capable extractor; cached input −90%. |
| 4 | **DeepSeek** | `deepseek-chat` | ≈ 0.27 / 1.10 (cache hit ≈ 0.07) | 128k | Cheapest overall; Chinese company — customer page content leaves the US; **opt-in only**. |
| 5 | **Groq** | Llama 4 Scout/Maverick, Llama 3.3 70B, Qwen | Scout ≈ 0.11 / 0.34 · 70B ≈ 0.59 / 0.79 | 128k | Fastest inference; open-weight; weaker on very long pages. |
| 6 | **Mistral** | `mistral-small`, `ministral` | small ≈ 0.10 / 0.30 | 128k | EU-hosted, cheap, adequate tool use. |
| 7 | **xAI** | `grok-4-fast` / mini | ≈ 0.20 / 0.50 | 2M | Long context, cheap. |
| — | **Cursor** | whatever it lists | Cursor's rates | — | Today's path, kept as an adapter for parity and fallback. |

Deliberately not on the list: self-hosted models on the worker VMs (CPU-only,
too slow for 100k-token pages), consumer plans (Claude Max, ChatGPT — terms
forbid product use), aggregators (one more company in the path; a direct
adapter is half a day).

**Starting set:** Gemini Flash for scrape and enrich, Haiku 4.5 or Sonnet 5
for draft and planner. Everything else is a stage setting away once its
adapter exists.

## Phases

| # | Phase | Deliverable | Est. |
|---|---|---|---|
| 0 | **Baseline** | `tokens` per stage for 5 real runs of the Crosstrek plan on the current build (`rounds` + `trim` on). Nothing to build. | 0 |
| 1 | **Provider layer** | `llm/` core + conformance suite + `gemini.rs` + `anthropic.rs`; registry; secrets plumbed. `plan_chat` drafting (no tools) switched to it first — lowest risk, measurable at once. | 3 days |
| 2 | **Browser tools over CDP** | `agent/tools.rs`; Playwright-compatible snapshot; `TabScope`/watchdog reused. Test: the two-page HN check used for `trim`, through our tools; snapshot diffed against Playwright's on 20 real pages. | 3 days |
| 3 | **The loop + compaction** | `agent/loop.rs`, `context.rs`, page budget enforced, events into guard/trail/meter/progress; `cursor.rs` adapter; shadow mode. | 3 days |
| 4 | **Parity run** | Same plan, Cursor vs direct, 5 runs each: rows found, tokens, $ (admin Logs). Fix what differs. | 1–2 days |
| 5 | **Switch + catalogue** | Defaults to `gemini:` / `anthropic:`; Models dropdown by provider; Providers card; rates from adapters; docs. | 1 day |
| 6 | **More adapters** | `openai.rs`, `deepseek.rs`, `groq.rs`, `mistral.rs`, `xai.rs` — each ½ day against the conformance suite. Independent; any order; can be done by anyone following §1.4. | 2–3 days |
| 7 | **Later** | Gemini explicit caching; Anthropic Batch API for scheduled runs; streaming adapters for live token display; remove `sandbox.rs` and the `mcp.rs` stdio server once Cursor is unused. | — |

≈ 2 weeks to phase 5; phases 1 and 2 are independent and can run in parallel.

## Risks and how the plan handles them

- **Snapshot fidelity.** Port Playwright's snapshot script verbatim; diff on 20 real pages before phase 3.
- **Tool-use quality of cheap models.** Phase 4 measures rows per dollar, not tokens; keep Haiku/Sonnet where judgment matters.
- **Provider drift.** APIs change field names. The conformance suite with golden request files catches it in CI; the live `#[ignore]` test catches it before a release.
- **Rate limits with 10–30 slots.** Per-provider semaphore + `Retry-After` backoff in `kit.rs`; spread stages across providers.
- **Data location / terms.** Page content and briefs go to the provider. Gemini, Anthropic and OpenAI offer zero-retention API terms; DeepSeek is opt-in.
- **The CLI port stops tracking upstream.** `agent.rs` and `sandbox.rs` diverge; noted in CLAUDE.md when it happens.
- **Billing.** Customers pay per input+output token; compaction and caching cut their bill as much as ours. Per-result pricing is a separate decision.

## Open decisions (yours)

1. Confirm the starting pair: Gemini + Anthropic.
2. Whether DeepSeek (non-US data) is acceptable at all.
3. Pricing model once cost per run drops (per token vs per result).

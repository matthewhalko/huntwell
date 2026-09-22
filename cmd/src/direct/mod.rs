//! Running the agent ourselves, on any provider.
//!
//! The Cursor CLI's job, done in this process: send the task and the tools to a
//! model, run the tool calls it asks for, feed the answers back, and stop when
//! it replies with the rows. What that buys, in the order it matters:
//!
//!   - **The conversation is ours**, so [`context`] can drop a page once its
//!     rows are out. With the CLI every page is re-read on every later step,
//!     which is where a search's cost actually goes.
//!   - **The tools are ours**, so the model is offered six browser verbs and
//!     nothing else, and the guard judges a navigation *before* it happens
//!     rather than after ([`tools`]).
//!   - **The model is ours to choose**, per stage, on any provider
//!     ([`crate::llm`]).
//!
//! Everything downstream is unchanged: the same security contract is the system
//! prompt, the same events reach `progress`, `guard`, `trail` and `meter`, and
//! the answer is the same JSON `pipeline` already expects.
//!
//! The browser is unchanged too — [`cdp`] attaches to whatever
//! `browser::cdp_endpoint()` names, which on a worker is the Browserbase
//! session with the account's logged-in Context.

use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::llm::{LlmError, Message, Provider, Reply, Request, Stop};
use crate::progress::{Level, Reporter};

pub mod cdp;
pub mod context;
pub mod memory;
pub mod tools;

/// Turns — a model reply and the tool calls it asked for — before the loop
/// insists on an answer. A page budget usually bites first; this is the stop
/// for a model going in circles.
const MAX_TURNS: usize = 40;

/// Room for the answer. A scrape returns rows, which is a long reply.
const MAX_OUTPUT: u32 = 16_384;

/// Whether to narrate every turn: what the model said, what it asked for, what
/// it spent.
///
/// Off by default because it is a lot of lines, and worth having because the
/// interesting failures are all "the model did something odd and the run log
/// only showed the consequence" — a search that returned nothing, a stage that
/// answered without looking. `HUNTWELL_AGENT_TRACE=1`.
fn tracing_turns() -> bool {
    matches!(crate::config::get("HUNTWELL_AGENT_TRACE").as_deref(), Some("1" | "true" | "on"))
}

/// What the model said, safe for a run log: one line, redacted, bounded.
fn said(text: &str, max: usize) -> String {
    let flat = crate::progress::one_line(text, max);
    crate::guard::safe_for_log(&flat, max)
}

/// The page budget for the call about to be made, set by `pipeline::run_search`
/// just before it. A process-global for the same reason `sandbox::run_scope` is
/// one: it is a property of the run, and threading it through `AgentOpts` would
/// change a struct the ported CLI path owns.
static PAGE_BUDGET: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn set_page_budget(pages: usize) {
    PAGE_BUDGET.store(pages, std::sync::atomic::Ordering::Relaxed);
}

pub fn page_budget() -> usize {
    PAGE_BUDGET.load(std::sync::atomic::Ordering::Relaxed)
}

/// The trail this call should start from, set alongside the budget. Same
/// reason it is a global: a property of the run that would otherwise have to
/// be threaded through `AgentOpts`, which the ported CLI path owns.
static COVERED: std::sync::Mutex<Option<context::Ground>> = std::sync::Mutex::new(None);

pub fn set_covered(ground: context::Ground) {
    if let Ok(mut g) = COVERED.lock() {
        *g = Some(ground);
    }
}

pub fn covered() -> context::Ground {
    COVERED.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

/// The plan's own records, opened once per process.
///
/// The same server the CLI path starts as a child over stdio; here it is
/// called in place. `None` when there is no plan in scope (a draft) or the
/// database is not reachable — the tools are simply not offered then.
///
/// **Async, and it has to be.** `mcp::Server::open` builds a runtime of its own
/// and blocks on it, which panics if it happens on a thread already driving
/// one. Opening it on a blocking thread is the whole reason this is not a
/// plain function, and making it `async` is what stops a caller getting that
/// wrong again.
pub async fn memory() -> Option<memory::Memory> {
    static MEMORY: tokio::sync::OnceCell<Option<memory::Memory>> = tokio::sync::OnceCell::const_new();
    MEMORY
        .get_or_init(|| async {
            let (_, plan_id) = crate::sandbox::run_scope()?.clone();
            let url = crate::config::database_url().ok()?;
            memory::Memory::start(url, plan_id).await
        })
        .await
        .clone()
}

/// Whether this model id runs here rather than through the Cursor CLI.
pub fn handles(model: Option<&str>) -> Option<(&'static dyn Provider, String)> {
    let raw = model?;
    match crate::llm::parse_model_id(raw) {
        Ok(crate::llm::Engine::Direct { provider, model }) => crate::llm::provider(&provider).map(|p| (p, model)),
        _ => None,
    }
}

pub struct Options {
    /// The stage, for the log and the meter: "scrape", "enrich 3/14", …
    pub label: String,
    pub provider: &'static dyn Provider,
    pub model: String,
    /// Pages this call may open. 0 for no limit.
    pub budget: usize,
    pub progress: Level,
    /// The plan's own records, when the run has a plan behind it.
    pub memory: Option<memory::Memory>,
    /// Where this plan has already been, from its trail. The call adds to it.
    pub covered: context::Ground,
    /// Whether this stage reads pages. Drafting a plan does not — it is one
    /// text call, and on the app VM there is no browser to attach to at all.
    pub needs_browser: bool,
}

/// Ask a model to do one stage, with a browser.
///
/// Returns the JSON its final reply carried — the same thing
/// `agent::ask_agent` returns, so `pipeline` cannot tell which engine ran.
pub async fn run(opts: Options, task: &str) -> Result<Value> {
    let rep = Reporter::new(&opts.label, opts.progress);
    let guard = crate::guard::Guard::configured();
    let with_memory = opts.memory.is_some() && crate::agent::memory_useful(&opts.label);

    let mut tools = match opts.needs_browser {
        true => Some(tools::Tools::open(guard.clone(), opts.memory.clone(), opts.budget).await.context("attach to the browser")?),
        false => None,
    };

    // The same security contract the CLI path prepends, plus — only when there
    // are pages to read — the one thing that is true here and not there: old
    // pages go away.
    let mut system = crate::agent::compose_prompt("", with_memory && opts.needs_browser);
    if opts.needs_browser {
        system.push_str(context::NOTE);
    }
    let mut req = Request::new(&opts.model, system);
    req.tools = tools.as_ref().map(|t| t.definitions()).unwrap_or_default();
    req.max_output_tokens = MAX_OUTPUT;
    let started = Instant::now();
    let mut compacted = 0usize;
    let mut turns = 0usize;
    // Where this plan has been in *earlier* rounds. What this call itself
    // covers needs no note: compaction leaves every page's URL behind in its
    // stub, in order, which is the same record for free.
    let ground = opts.covered.clone();
    // Context first, task last. A model does what the *end* of its input asks,
    // and with the trail going in after the task the last thing a later round
    // read was a list of things not to do — so it did nothing, and answered in
    // under a second having opened no page at all. Seeded once and never
    // touched again, so the conversation stays append-only.
    context::seed_note(&mut req.messages, &ground);
    req.messages.push(Message::User(task.to_string()));
    // Whether the model has already been asked to re-send its answer as JSON.
    let mut asked_again = false;
    // …and whether it has already been sent back for answering without looking.
    let mut pushed_back = false;
    let trace = tracing_turns();

    loop {
        turns += 1;
        if turns > MAX_TURNS {
            rep.warn(&format!("{} turns without an answer — asking for what it has", MAX_TURNS));
            req.messages.push(Message::User(
                "Stop searching now and reply with the rows you have, in the format the task asked for.".into(),
            ));
        }

        let reply = complete(opts.provider, &mut req, &rep).await?;
        if trace {
            rep.emit(
                '·',
                &format!(
                    "turn {turns}: {:?}, {} tool call(s), {} in · {} out{}",
                    reply.stop,
                    reply.tool_calls.len(),
                    reply.usage.input,
                    reply.usage.output,
                    match reply.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                        Some(t) => format!(" — said: {}", said(t, 400)),
                        None => String::new(),
                    }
                ),
            );
        }
        // Our cost, from this provider's own price for this model and the
        // tokens it just reported — cache reads at the cache rate. Exact,
        // unlike the rate-card estimate the admin falls back to.
        let cost = reply
            .cost_micros
            .or_else(|| opts.provider.price(&opts.model).map(|p| p.cost_micros(reply.usage)))
            .unwrap_or(0);
        crate::agent::note_usage(reply.usage, cost);

        if reply.stop == Stop::Refused {
            bail!("{} declined the task", opts.provider.label());
        }

        // Nothing more to run: the reply is the answer.
        if reply.tool_calls.is_empty() {
            let text = reply.text.unwrap_or_default();
            crate::agent::report_injection_attempts(&text, &rep);
            if guard.tripped() {
                return Err(guard.breach().into());
            }
            // A search that never searched. Rows in such an answer came out of
            // the model rather than off the web, which is worse than no rows.
            let said_it = text.clone();
            if opts.needs_browser && tools.as_ref().is_some_and(|t| t.opened() == 0) && !pushed_back {
                pushed_back = true;
                rep.warn("answered without opening a page — asking it to actually search");
                req.messages.push(Message::Assistant { text, tool_calls: vec![] });
                req.messages.push(Message::User(
                    "You have not opened a single page. Do not answer from memory — every row must come off a page you \
                     actually read. Start now: run a web search with browser_navigate, open the listing pages it returns, \
                     and take the rows from them."
                        .into(),
                ));
                continue;
            }
            match crate::agent::extract_json(&text) {
                Ok(v) => {
                    // An empty answer is the one outcome the run log cannot
                    // explain by itself — "0 rows" says nothing about why. The
                    // model's own words usually do, so they are kept whether
                    // or not anyone asked for a trace.
                    let empty = v.as_array().is_some_and(|a| a.is_empty());
                    if empty && opts.needs_browser {
                        let pages = tools.as_ref().map(|t| t.opened()).unwrap_or(0);
                        rep.warn(&format!(
                            "came back empty after {turns} turn(s) and {pages} page(s). It said: {}",
                            said(&said_it, 500)
                        ));
                    }
                    if compacted > 0 && opts.progress != Level::Off {
                        println!("  context    {} of page text dropped once its rows were out", crate::progress::human_bytes(compacted));
                    }
                    rep.emit(
                        '✓',
                        &format!("{} {} answered in {} turn(s)", opts.provider.label(), opts.model, turns),
                    );
                    let _ = started;
                    return Ok(v);
                }
                // A model that finished in prose — "I could not find any
                // matching firms" — has done the work and mis-delivered it.
                // Losing the round over formatting is the expensive answer;
                // asking once more costs one short turn. A second miss is a
                // real failure.
                Err(_) if !asked_again => {
                    asked_again = true;
                    rep.emit('⚠', "the reply was not in the format the task asked for — asking once more");
                    req.messages.push(Message::Assistant { text, tool_calls: vec![] });
                    req.messages.push(Message::User(
                        "Reply again with ONLY the fenced ```json``` block the task asked for, and nothing outside it. \
                         If you found nothing, that is a valid answer: return an empty array []."
                            .into(),
                    ));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        // A text-only call has no tools; a model inventing one is told so
        // rather than left waiting.
        let Some(tools) = tools.as_mut() else {
            bail!("{} called a tool on a text-only stage", opts.provider.label());
        };

        // Run what it asked for, in the order it asked.
        let mut results: Vec<Message> = Vec::with_capacity(reply.tool_calls.len());
        for call in &reply.tool_calls {
            rep.emit('→', &crate::progress::describe_tool(&call.name, &call.args));
            let outcome = tools.run(&call.name, &call.args).await;
            // The same two consumers the CLI stream fed.
            crate::trail::note_call(&call.args, &outcome.observed);
            if outcome.is_error {
                rep.emit('⚠', &crate::guard::safe_for_log(&outcome.content, 160));
            }
            results.push(Message::ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content: outcome.content,
                is_error: outcome.is_error,
            });
        }

        // A fatal violation ends the run, after the model has been told —
        // `judge_before` records it and the result says why.
        if guard.tripped() {
            return Err(guard.breach().into());
        }

        req.messages.push(Message::Assistant {
            text: reply.text.unwrap_or_default(),
            tool_calls: reply.tool_calls,
        });
        req.messages.extend(results);
        compacted += context::compact(&mut req.messages);

        if crate::agent::credits_exhausted() {
            return Err(crate::agent::CreditsExhausted.into());
        }
    }
}

/// One model call, with the one recovery worth making: a conversation that no
/// longer fits is compacted harder and asked once more.
async fn complete(provider: &'static dyn Provider, req: &mut Request, rep: &Reporter) -> Result<Reply> {
    match provider.complete(req).await {
        Ok(r) => Ok(r),
        Err(LlmError::ContextTooLong) => {
            rep.warn("the conversation outgrew the model — dropping the older pages and asking again");
            // Everything but the task and the last exchange.
            squeeze(&mut req.messages);
            provider.complete(req).await.map_err(|e| anyhow!(e))
        }
        Err(e) => Err(anyhow!(e)),
    }
}

/// The last resort when even compaction was not enough: every page result
/// becomes a stub, whatever its age.
fn squeeze(messages: &mut Vec<Message>) {
    for m in messages.iter_mut() {
        if let Message::ToolResult { content, .. } = m {
            if content.len() > 1500 {
                *content = "[a page read earlier; its text was dropped to make room]".into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The whole loop, end to end, against a **real browser** and a real page:
    /// tool definitions out, a tool call back, CDP drives Chrome, the page is
    /// snapshotted and trimmed, the conversation is compacted, and the model's
    /// last reply becomes the rows the pipeline gets.
    ///
    /// The model is a stand-in that plays a scripted scrape, because what is
    /// being checked here is our half of the exchange; the providers' halves
    /// are `llm::conformance`'s job. Needs Chrome:
    ///
    /// ```text
    /// google-chrome --headless=new --remote-debugging-port=9222 &
    /// HUNTWELL_CDP_PORT=9222 cargo test a_whole_scrape -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore]
    async fn a_whole_scrape_runs_from_tool_definitions_to_rows() {
        use axum::{extract::State, response::Html, routing::{get, post}, Json, Router};
        use std::sync::{Arc, Mutex};

        let _env = crate::llm::test_env();

        // --- the site being scraped ----------------------------------------
        let site = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let site_addr = site.local_addr().unwrap();
        tokio::spawn(async move {
            // A listing page of a realistic size: compaction leaves small
            // results alone, so a two-row page would prove nothing.
            let rows: String = (3..40)
                .map(|i| format!("<li><a href=\"/car/{i}\">20{} Subaru Impreza Sport {i}</a> <span>${i},500</span></li>", 10 + i % 12))
                .collect();
            let listing = Html(format!(
                r#"<html><head><title>Cars for sale</title></head><body>
                   <header><nav><a href="/">Home</a><a href="/sell">Sell</a></nav></header>
                   <main><ul>
                     <li><a href="/car/1">2022 Subaru Crosstrek Limited</a> <span>$27,995</span></li>
                     <li><a href="/car/2">2021 Subaru Outback Premium</a> <span>$24,500</span></li>
                     {rows}
                   </ul>
                   <input type="search" placeholder="Search cars">
                   </main></body></html>"#
            ));
            let page2 = Html(
                r#"<html><head><title>More cars</title></head><body>
                   <header><nav><a href="/">Home</a><a href="/sell">Sell</a></nav></header>
                   <main><ul><li><a href="/car/3">2020 Subaru Forester Sport</a> <span>$21,000</span></li></ul></main>
                   </body></html>"#,
            );
            let page3 = Html(
                r#"<html><head><title>Last page</title></head><body>
                   <main><ul><li><a href="/car/4">2019 Subaru Ascent Touring</a> <span>$19,000</span></li></ul></main>
                   </body></html>"#,
            );
            let app = Router::new()
                .route("/", get(move || async move { listing.clone() }))
                .route("/page2", get(move || async move { page2.clone() }))
                .route("/page3", get(move || async move { page3.clone() }));
            axum::serve(site, app).await.unwrap();
        });
        let site_url = format!("http://127.0.0.1:{}", site_addr.port());

        // --- the model, playing a scrape -----------------------------------
        #[derive(Default)]
        struct Script {
            turn: usize,
            seen: Vec<Value>,
        }
        let script: Arc<Mutex<Script>> = Arc::default();
        let model = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let model_addr = model.local_addr().unwrap();
        let recorder = script.clone();
        let site_for_model = site_url.clone();
        tokio::spawn(async move {
            async fn turn(State((s, site)): State<(Arc<Mutex<Script>>, String)>, Json(body): Json<Value>) -> Json<Value> {
                let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
                s.seen.push(body);
                s.turn += 1;
                let call = |url: &str| {
                    json!({ "choices": [{ "message": { "content": null, "tool_calls": [{
                        "id": format!("c{}", url.len()), "type": "function",
                        "function": { "name": "browser_navigate", "arguments": json!({ "url": url }).to_string() }
                    }] }, "finish_reason": "tool_calls" }],
                    "usage": { "prompt_tokens": 1000, "completion_tokens": 50 } })
                };
                Json(match s.turn {
                    1 => call(&site),
                    2 => call(&format!("{site}/page2")),
                    3 => call(&format!("{site}/page3")),
                    _ => json!({ "choices": [{ "message": { "content":
                        "Here are the rows:\n```json\n[{\"title\":\"2022 Subaru Crosstrek Limited\",\"price\":\"$27,995\"},\
                         {\"title\":\"2020 Subaru Forester Sport\",\"price\":\"$21,000\"}]\n```" },
                        "finish_reason": "stop" }],
                        "usage": { "prompt_tokens": 1200, "completion_tokens": 90, "prompt_tokens_details": { "cached_tokens": 900 } } }),
                })
            }
            let app = Router::new().route("/chat/completions", post(turn)).with_state((recorder, site_for_model));
            axum::serve(model, app).await.unwrap();
        });
        std::env::set_var("OPENAI_BASE_URL", format!("http://127.0.0.1:{}", model_addr.port()));
        std::env::set_var("OPENAI_API_KEY", "test");
        std::env::set_var("HUNTWELL_LLM_ATTEMPTS", "1");

        // --- run it --------------------------------------------------------
        let provider = crate::llm::provider("openai").unwrap();
        let rows = run(
            Options {
                label: "scrape".into(),
                provider,
                model: "gpt-5-mini".into(),
                budget: 8,
                progress: Level::Off,
                memory: None,
                covered: Default::default(),
                needs_browser: true,
            },
            "Find Subaru listings. Return a JSON array of {title, price}.",
        )
        .await
        .expect("the loop should answer");

        // --- what the pipeline gets ----------------------------------------
        assert_eq!(rows.as_array().map(|a| a.len()), Some(2), "{rows}");
        assert_eq!(rows[0]["title"], "2022 Subaru Crosstrek Limited");

        // --- what the model was actually sent -------------------------------
        let seen = script.lock().unwrap_or_else(|e| e.into_inner()).seen.clone();
        assert_eq!(seen.len(), 4, "three pages, then the answer");
        let first = seen[0].to_string();
        assert!(first.contains("SECURITY CONTRACT"), "the contract is the system prompt");
        // The tool list is the boundary, so it is checked as a list — the
        // contract's prose names the forbidden tools on purpose.
        let offered: Vec<String> = seen[0]["tools"]
            .as_array()
            .expect("tools went with the request")
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(offered.contains(&"browser_navigate".to_string()) && offered.contains(&"browser_click".to_string()));
        for forbidden in ["shell", "glob", "read", "write", "evaluate", "websearch"] {
            assert!(!offered.iter().any(|n| n.to_lowercase().contains(forbidden)), "{forbidden} was offered: {offered:?}");
        }
        assert!(first.contains("last two pages"), "the model is told how the conversation works");

        // The second turn carries the real page, read out of real Chrome.
        let second = seen[1].to_string();
        assert!(second.contains("2022 Subaru Crosstrek Limited"), "the listing reached the model:\n{second}");
        assert!(second.contains("Cars for sale"), "with the page title");
        assert!(second.contains("searchbox") || second.contains("Search cars"), "and its search box, to act on");

        // The last turn has the two newest pages whole — and the first page
        // replaced by a stub, which is the whole point of the exercise.
        let last = seen[3].to_string();
        assert!(last.contains("2019 Subaru Ascent Touring"), "the newest page is whole");
        assert!(last.contains("2020 Subaru Forester Sport"), "and the one before it");
        assert!(last.contains("read earlier"), "the oldest page was compacted away");
        assert!(!last.contains("2021 Subaru Outback Premium"), "its text is really gone");
        assert!(last.contains("Cars for sale") || last.contains(&site_url), "but the model is still told where it was");
        // And the saving is real: the stub is a line where a page was.
        assert!(last.len() < seen[2].to_string().len() + 2000, "the conversation stopped growing with the pages");

        std::env::remove_var("OPENAI_BASE_URL");
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("HUNTWELL_LLM_ATTEMPTS");
    }

    /// Plan drafting: one text call, no browser, on the app VM where there is
    /// no browser to attach to. The bug this guards made every draft fail the
    /// moment a stage was pointed at a provider, because the id went to the
    /// Cursor CLI, which does not know it.
    #[tokio::test]
    async fn a_text_only_stage_needs_no_browser_and_is_offered_no_tools() {
        use axum::{extract::State, routing::post, Json, Router};
        use std::sync::{Arc, Mutex};

        let _env = crate::llm::test_env();
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let recorder = seen.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            async fn reply(State(s): State<Arc<Mutex<Vec<Value>>>>, Json(b): Json<Value>) -> Json<Value> {
                s.lock().unwrap_or_else(|e| e.into_inner()).push(b);
                Json(json!({ "choices": [{ "message": { "content": "```json\n{\"Source\":\"Reno advisors\"}\n```" },
                    "finish_reason": "stop" }], "usage": { "prompt_tokens": 400, "completion_tokens": 20 } }))
            }
            axum::serve(listener, Router::new().route("/chat/completions", post(reply)).with_state(recorder)).await.unwrap()
        });
        std::env::set_var("OPENAI_BASE_URL", format!("http://127.0.0.1:{}", addr.port()));
        std::env::set_var("OPENAI_API_KEY", "k");
        std::env::set_var("HUNTWELL_LLM_ATTEMPTS", "1");

        // No browser is running, and none is needed.
        let out = run(
            Options {
                label: "artifact plan draft".into(),
                provider: crate::llm::provider("openai").unwrap(),
                model: "gpt-5-mini".into(),
                budget: 0,
                progress: Level::Off,
                memory: None,
                covered: Default::default(),
                needs_browser: false,
            },
            "Draft a plan for Reno financial advisors.",
        )
        .await
        .expect("a draft should not need a browser");
        assert_eq!(out["Source"], "Reno advisors");

        let sent = seen.lock().unwrap_or_else(|e| e.into_inner())[0].clone();
        assert!(sent.get("tools").is_none(), "a text call is offered no tools: {sent}");
        assert!(sent.to_string().contains("SECURITY CONTRACT"), "the contract still applies");
        assert!(!sent.to_string().contains("last two pages"), "and the page rule does not, since there are none");

        std::env::remove_var("OPENAI_BASE_URL");
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("HUNTWELL_LLM_ATTEMPTS");
    }

    /// The crash that killed runs 10, 11 and 12: opening the plan-memory
    /// server from inside the runtime, where the runtime it builds for itself
    /// panics. It is now refused with a sentence instead, and `memory()` is
    /// async so the shape of the call is what stops it happening.
    #[tokio::test]
    async fn opening_plan_memory_inside_a_runtime_is_refused_not_a_panic() {
        let e = match crate::mcp::Server::open("postgres://nobody@127.0.0.1:1/none", 1) {
            Err(e) => e,
            Ok(_) => panic!("it must refuse rather than open from inside a runtime"),
        };
        assert!(e.to_string().contains("spawn_blocking"), "{e}");
    }

    /// And the way the loop actually reaches it works from async.
    #[tokio::test]
    async fn memory_is_none_when_there_is_no_plan_rather_than_a_crash() {
        assert!(memory().await.is_none(), "no run scope in a test process");
    }

    /// The order the model reads things in, which is not a detail: a model
    /// does what the *end* of its input asks. With the plan's trail going in
    /// after the task, the last thing a later round read was a list of ground
    /// already covered — and it answered in under a second, having opened no
    /// page at all. The task goes last.
    #[test]
    fn the_task_is_the_last_thing_the_model_reads() {
        let ground = context::Ground::from_history(vec!["chicago RIA".into()], vec![]);
        let mut messages: Vec<Message> = Vec::new();
        context::seed_note(&mut messages, &ground);
        messages.push(Message::User("Find boutique advisors in Chicago.".into()));

        match messages.last().unwrap() {
            Message::User(t) => assert!(t.starts_with("Find boutique"), "the task must be last, not {t:.60}"),
            other => panic!("{other:?}"),
        }
        match &messages[0] {
            Message::User(t) => assert!(t.starts_with("GROUND ALREADY COVERED")),
            other => panic!("{other:?}"),
        }
        // And with nothing inherited there is no preamble at all — a first
        // round should not open by being told what it has already done.
        let mut fresh: Vec<Message> = Vec::new();
        context::seed_note(&mut fresh, &context::Ground::default());
        assert!(fresh.is_empty());
    }

    #[test]
    fn a_squeeze_leaves_the_conversations_shape_alone() {
        let mut msgs = vec![
            Message::User("task".into()),
            Message::Assistant { text: String::new(), tool_calls: vec![] },
            Message::ToolResult { call_id: "a".into(), name: "browser_navigate".into(), content: "x".repeat(9000), is_error: false },
            Message::ToolResult { call_id: "b".into(), name: "prospect_known".into(), content: "{}".into(), is_error: false },
        ];
        squeeze(&mut msgs);
        assert_eq!(msgs.len(), 4, "a provider refuses a call whose results went missing");
        match &msgs[2] {
            Message::ToolResult { content, .. } => assert!(content.len() < 200),
            _ => panic!(),
        }
        match &msgs[3] {
            Message::ToolResult { content, .. } => assert_eq!(content, "{}"),
            _ => panic!(),
        }
    }
}

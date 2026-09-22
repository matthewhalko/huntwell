//! The tools the model is given, and nothing else.
//!
//! Six browser verbs and the plan's own memory. No shell, no files, no
//! `evaluate`, no way to run code of the model's choosing — which is the whole
//! of what `sandbox.rs` exists to fence off when the Cursor CLI is driving.
//! Here the fence is the list: a tool that is not below cannot be called
//! because the model is never told it exists.
//!
//! The guard judges a call **before** it runs. With the CLI it could only
//! judge afterwards, from the stream, by which time the page had been fetched;
//! a refusal here means the request never leaves the process. A refused call is
//! returned to the model as a failed tool result, not as the end of the run:
//! being told "not that host" is something it can act on.
//!
//! Every browser tool answers with the page in the same shape Playwright's
//! server used, because the scrape prompts, `thrift::trim`, `trail` and the
//! guard's host scan were all written against it.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use super::cdp::Cdp;
use crate::guard::Guard;
use crate::llm::ToolDef;

/// The page snapshot script, run in the page.
const SNAPSHOT_JS: &str = include_str!("snapshot.js");

/// How long to let a page settle after a navigation.
const SETTLE: Duration = Duration::from_secs(12);

/// What a tool call did.
pub struct Outcome {
    /// What the model is told.
    pub content: String,
    pub is_error: bool,
    /// What the trail and the guard see. Deliberately *not* the snapshot: a
    /// listing page carries hundreds of links, and recording them all as
    /// "pages this plan visited" would drown the record it is meant to keep.
    pub observed: Value,
}

impl Outcome {
    fn ok(content: String, observed: Value) -> Self {
        Self { content, is_error: false, observed }
    }
    fn failed(why: impl std::fmt::Display) -> Self {
        Self { content: why.to_string(), is_error: true, observed: Value::Null }
    }
}

/// The tools the model is offered. A free function, and the order is fixed:
/// this list is part of the prefix every provider caches on.
pub fn definitions(with_memory: bool) -> Vec<ToolDef> {
    let mut out = vec![
        ToolDef {
            name: "browser_navigate".into(),
            description: "Open a web page and return what is on it. Use this for search-engine \
                          queries and for listing pages. Returns the page as an outline: every \
                          line is an element, and `[ref=e12]` is how you name one to click or type in."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": { "url": { "type": "string", "description": "The full URL, including https://" } },
                "required": ["url"]
            }),
        },
        ToolDef {
            name: "browser_snapshot".into(),
            description: "Read the current page again. Use after something on the page changed \
                          — a filter applied, more results loaded — and you need to see the result."
                .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: "browser_click".into(),
            description: "Click an element, named by the `ref` the page outline gave it.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "ref": { "type": "string", "description": "A ref from the outline, e.g. e12" } },
                "required": ["ref"]
            }),
        },
        ToolDef {
            name: "browser_type".into(),
            description: "Type into a field, named by its `ref`. Set `submit` to press Enter afterwards.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "ref": { "type": "string", "description": "A ref from the outline" },
                    "text": { "type": "string" },
                    "submit": { "type": "boolean", "description": "Press Enter after typing" }
                },
                "required": ["ref", "text"]
            }),
        },
        ToolDef {
            name: "browser_press_key".into(),
            description: "Press a key on the page: Enter, Tab, Escape, PageDown, PageUp, Home, End or an arrow key.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "key": { "type": "string" } },
                "required": ["key"]
            }),
        },
        ToolDef {
            name: "browser_back".into(),
            description: "Go back to the previous page. Cheaper than opening a listing page again by URL.".into(),
            parameters: json!({ "type": "object", "properties": {} }),
        },
    ];
    // The plan's own memory: what it has already stored and already
    // searched. Descriptions are the ones written for the MCP server.
    if with_memory {
        for t in crate::mcp::tool_definitions() {
            out.push(ToolDef {
                name: t["name"].as_str().unwrap_or_default().to_string(),
                description: t["description"].as_str().unwrap_or_default().to_string(),
                parameters: t["inputSchema"].clone(),
            });
        }
    }
    out
}

/// A `document.querySelector` argument for the ref the model named, as a JSON
/// string so nothing it sends can end the selector early and reach another
/// element.
fn selector(args: &Value) -> Result<String> {
    let r = args.get("ref").and_then(Value::as_str).unwrap_or_default().trim();
    if r.is_empty() || !r.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(anyhow!("'{}' is not a ref from the page outline", crate::guard::safe_for_log(r, 40)));
    }
    Ok(json!(format!("[data-hw-ref=\"{r}\"]")).to_string())
}

pub struct Tools {
    cdp: Cdp,
    guard: Guard,
    memory: Option<super::memory::Memory>,
    /// Pages opened so far, against the budget.
    opened: usize,
    budget: usize,
    /// Site furniture already shown, so the trimmer can drop a repeat.
    seen: crate::thrift::trim::Seen,
}

impl Tools {
    pub async fn open(guard: Guard, memory: Option<super::memory::Memory>, budget: usize) -> Result<Self> {
        let cdp = Cdp::connect(&crate::browser::cdp_endpoint()).await?;
        Ok(Self { cdp, guard, memory, opened: 0, budget, seen: crate::thrift::trim::Seen::default() })
    }

    /// Pages opened so far in this call. Zero after an answer means the model
    /// replied out of its own memory rather than off the web.
    pub fn opened(&self) -> usize {
        self.opened
    }

    /// What the model is told it can do.
    pub fn definitions(&self) -> Vec<ToolDef> {
        definitions(self.memory.is_some())
    }


    /// Run one call the model asked for.
    pub async fn run(&mut self, name: &str, args: &Value) -> Outcome {
        // Before anything happens. `judge_before` sees the arguments only,
        // which for a navigation is the URL — the one thing worth stopping.
        if let Some(v) = self.guard.judge_before(name, args) {
            return Outcome::failed(format!(
                "refused: {}. {}",
                v.detail,
                "Stay on the sites the task named and their search pages."
            ));
        }
        let result = match name {
            "browser_navigate" => self.navigate(args).await,
            "browser_snapshot" => self.page("").await,
            "browser_click" => self.click(args).await,
            "browser_type" => self.type_into(args).await,
            "browser_press_key" => self.press(args).await,
            "browser_back" => self.back().await,
            other => self.memory_tool(other, args).await,
        };
        match result {
            Ok(o) => o,
            // A page that would not load is a fact about the page, so the
            // model is told and the run goes on.
            Err(e) => Outcome::failed(crate::guard::safe_for_log(&format!("{e:#}"), 300)),
        }
    }

    async fn navigate(&mut self, args: &Value) -> Result<Outcome> {
        let url = args.get("url").and_then(Value::as_str).unwrap_or_default().trim().to_string();
        if url.is_empty() {
            return Ok(Outcome::failed("browser_navigate needs a url"));
        }
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Ok(Outcome::failed(format!("'{}' is not a web address — give a full https:// URL", crate::guard::safe_for_log(&url, 80))));
        }
        // The budget is real here, not advice: beyond it a page costs more in
        // re-reading than it can add, so the tool says no and the model
        // answers with what it has.
        if self.budget > 0 && self.opened >= self.budget {
            return Ok(Outcome::failed(format!(
                "page budget spent ({} of {} opened). Do not open more pages — return the rows you have now, in the format the task asked for.",
                self.opened, self.budget
            )));
        }
        self.opened += 1;
        self.cdp.navigate(&url, SETTLE).await?;
        self.page(&url).await
    }

    async fn click(&mut self, args: &Value) -> Result<Outcome> {
        let target = selector(args)?;
        // Scrolled into view first: a click on an element off-screen is the
        // commonest reason a page ignores one.
        let script = format!(
            "(() => {{ const el = document.querySelector({target}); if (!el) return 'gone'; \
              el.scrollIntoView({{block:'center'}}); el.click(); return 'ok'; }})()"
        );
        match self.cdp.eval(&script).await?.as_str() {
            Some("ok") => {
                // A click may navigate; let it.
                tokio::time::sleep(Duration::from_millis(900)).await;
                self.page("").await
            }
            _ => Ok(Outcome::failed("that ref is not on the page any more — take a fresh snapshot first")),
        }
    }

    async fn type_into(&mut self, args: &Value) -> Result<Outcome> {
        let target = selector(args)?;
        let text = args.get("text").and_then(Value::as_str).unwrap_or_default();
        let script = format!(
            "(() => {{ const el = document.querySelector({target}); if (!el) return 'gone'; \
              el.focus(); el.value = {}; \
              el.dispatchEvent(new Event('input', {{bubbles:true}})); \
              el.dispatchEvent(new Event('change', {{bubbles:true}})); return 'ok'; }})()",
            json!(text)
        );
        if self.cdp.eval(&script).await?.as_str() != Some("ok") {
            return Ok(Outcome::failed("that ref is not on the page any more — take a fresh snapshot first"));
        }
        if args.get("submit").and_then(Value::as_bool).unwrap_or(false) {
            self.cdp.press("Enter").await?;
            tokio::time::sleep(Duration::from_millis(1200)).await;
        }
        self.page("").await
    }

    async fn press(&mut self, args: &Value) -> Result<Outcome> {
        let key = args.get("key").and_then(Value::as_str).unwrap_or_default();
        self.cdp.press(key).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.page("").await
    }

    async fn back(&mut self) -> Result<Outcome> {
        self.cdp.eval("history.back()").await?;
        tokio::time::sleep(Duration::from_millis(1200)).await;
        self.page("").await
    }


    /// The current page, in the shape the prompts expect.
    async fn page(&mut self, requested: &str) -> Result<Outcome> {
        let url = self.cdp.url().await;
        let title = self.cdp.title().await;
        let yaml = self.cdp.eval(SNAPSHOT_JS).await?.as_str().unwrap_or_default().to_string();
        let host = super::super::thrift::page::host_of(&url).unwrap_or_default();
        // Trimmed in memory, before the model ever sees it — no file, and no
        // second process to go wrong.
        let yaml = crate::thrift::trim::trim_snapshot(&yaml, &host, &mut self.seen).unwrap_or(yaml);

        let content = format!("### Page\n- Page URL: {url}\n- Page Title: {title}\n### Snapshot\n{yaml}");
        // Only the addresses, never the snapshot: the trail records where a
        // plan has been, not every link it saw.
        let observed = json!({ "requested": requested, "url": url });
        Ok(Outcome::ok(content, observed))
    }

    async fn memory_tool(&mut self, name: &str, args: &Value) -> Result<Outcome> {
        let Some(memory) = self.memory.clone() else {
            return Ok(Outcome::failed(format!("there is no tool called {name}")));
        };
        // Over a channel to the thread that owns the records — see
        // `direct::memory` for why it cannot simply be called here.
        match memory.call(name, args).await {
            Ok(text) => Ok(Outcome::ok(text, Value::Null)),
            Err(e) => Ok(Outcome::failed(format!("{e:#}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tool list is the security boundary, so what is *not* on it matters
    /// as much as what is.
    #[test]
    fn the_model_is_offered_browsing_and_nothing_else() {
        let names: Vec<String> = definitions(false).iter().map(|t| t.name.clone()).collect();
        assert_eq!(
            names,
            vec!["browser_navigate", "browser_snapshot", "browser_click", "browser_type", "browser_press_key", "browser_back"]
        );
        for forbidden in ["shell", "bash", "read", "write", "glob", "evaluate", "run_code", "websearch", "fetch"] {
            assert!(!names.iter().any(|n| n.to_lowercase().contains(forbidden)), "{forbidden} must not be offered");
        }
        // Plan memory is added only when the run has a plan behind it.
        let with = definitions(true);
        assert!(with.len() > names.len());
        assert!(with.iter().any(|t| t.name == "prospect_known"));
        // Every tool must carry a schema a provider will accept.
        for t in with {
            assert_eq!(t.parameters["type"], "object", "{}", t.name);
            assert!(!t.description.trim().is_empty(), "{}", t.name);
        }
    }

    #[test]
    fn a_ref_from_the_model_cannot_become_a_selector_of_its_own() {
        assert_eq!(selector(&json!({ "ref": "e12" })).unwrap(), r#""[data-hw-ref=\"e12\"]""#);
        for bad in ["e12\"], script", "*", "", "e12 a", "e1\']", "e1\\"] {
            assert!(selector(&json!({ "ref": bad })).is_err(), "{bad:?} must be refused");
        }
    }

    /// Against a real browser, for comparing with Playwright's own snapshot of
    /// the same page. Start Chrome with `--remote-debugging-port=9222`, then:
    ///
    /// ```text
    /// HUNTWELL_CDP_PORT=9222 HUNTWELL_PAGE=https://… \
    ///   cargo test live_snapshot -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore]
    async fn live_snapshot() {
        let url = std::env::var("HUNTWELL_PAGE").expect("set HUNTWELL_PAGE");
        let mut tools = Tools::open(Guard::new(vec![]), None, 0).await.expect("a browser");
        let out = tools.run("browser_navigate", &json!({ "url": url })).await;
        assert!(!out.is_error, "{}", out.content);
        if let Ok(dest) = std::env::var("HUNTWELL_SNAPSHOT_OUT") {
            std::fs::write(dest, &out.content).unwrap();
        }
        println!("{}", out.content);
    }

    #[test]
    fn the_guard_answers_before_a_page_is_fetched() {
        let guard = Guard::new(vec!["cars.test".into()]);
        assert!(guard.judge_before("browser_navigate", &json!({ "url": "https://cars.test/x" })).is_none());
        let refused = guard.judge_before("browser_navigate", &json!({ "url": "https://elsewhere.test/x" }));
        assert!(refused.is_some(), "an off-allowlist host must be stopped before the request");
        // And a tool with no host in it is not the host check's business.
        assert!(guard.judge_before("browser_snapshot", &json!({})).is_none());
    }
}

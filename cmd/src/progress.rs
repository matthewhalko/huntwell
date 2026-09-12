//! Live progress reporting for the long-running Cursor `agent` calls that
//! drive the pipeline.
//!
//! A single scrape can keep `agent` and Chrome busy for minutes while it
//! searches, opens pages and reads them. With `--output-format json` none of
//! that is visible: the CLI buffers everything and prints one blob at the end.
//! We instead ask for `--output-format stream-json`, which emits one NDJSON
//! event per assistant message / tool call / tool result, and turn those
//! events into a running commentary:
//!
//! ```text
//!     0.4s  ▸ session started (model Composer)
//!     2.1s  · I'll start with the payment processors, then remittance…
//!     4.8s  → WebSearch "Mexico PSP stablecoin treasury USDC"
//!    12.0s  → browser.navigate https://bitso.com/business
//!    31.5s  … still working (no events for 30s)
//!   180.2s  ✓ agent finished — 26 tool calls, 3m00s
//! ```
//!
//! Everything here is display-only; nothing influences what gets stored.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

/// How much of the agent conversation to narrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Only the pipeline's own step lines — the pre-streaming behavior.
    Off,
    /// Assistant prose, every tool call, tool errors, and a closing summary.
    Normal,
    /// Adds session/model info, thinking blocks, tool-result sizes, and the
    /// rendered prompts.
    Verbose,
}

impl Level {
    pub fn parse(s: &str) -> Result<Level, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "quiet" | "none" | "false" => Ok(Level::Off),
            "normal" | "on" | "true" => Ok(Level::Normal),
            "verbose" | "debug" | "all" => Ok(Level::Verbose),
            other => Err(format!(
                "bad --progress {other:?} (want off, normal, or verbose)"
            )),
        }
    }

    pub fn is_verbose(self) -> bool {
        self == Level::Verbose
    }
}

/// Narrates one `agent` invocation. Cheap to clone — the clone shares the
/// same start time, counters, and last-event clock, so the reader thread and
/// the heartbeat thread stay in sync.
#[derive(Clone)]
pub struct Reporter {
    label: Arc<String>,
    level: Level,
    start: Instant,
    /// Elapsed millis at the last emitted line — the heartbeat's idle clock.
    last_ms: Arc<AtomicU64>,
    tool_calls: Arc<AtomicU64>,
}

impl Reporter {
    pub fn new(label: &str, level: Level) -> Self {
        Self {
            label: Arc::new(label.to_string()),
            level,
            start: Instant::now(),
            last_ms: Arc::new(AtomicU64::new(0)),
            tool_calls: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn level(&self) -> Level {
        self.level
    }

    pub fn tool_calls(&self) -> u64 {
        self.tool_calls.load(Ordering::Relaxed)
    }

    /// Prints one commentary line, timestamped with elapsed time since the
    /// call started. Also resets the heartbeat's idle clock.
    pub fn emit(&self, marker: char, msg: &str) {
        let elapsed = self.start.elapsed();
        self.last_ms
            .store(elapsed.as_millis() as u64, Ordering::Relaxed);
        if self.level == Level::Off {
            return;
        }
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "  {:>8}  {marker} {msg}", fmt_elapsed(elapsed));
        let _ = out.flush();
    }

    /// Prints a line that must be seen even when this reporter is silent, and
    /// on stderr so it survives a pipeline that only keeps the JSON. Used for
    /// containment events: the tab-cleanup call runs at `Level::Off`, and a
    /// page steering the agent during cleanup is exactly as interesting as one
    /// steering it during a scrape.
    pub fn warn(&self, msg: &str) {
        let elapsed = self.start.elapsed();
        self.last_ms
            .store(elapsed.as_millis() as u64, Ordering::Relaxed);
        let label = if self.label.is_empty() {
            String::new()
        } else {
            format!("[{}] ", self.label)
        };
        eprintln!("  {:>8}  ✖ {label}{msg}", fmt_elapsed(elapsed));
    }

    /// Starts a background thread that prints a "still working" line whenever
    /// `every` passes with no events. Long page loads and long model turns
    /// otherwise look identical to a hang. The heartbeat stops when the
    /// returned guard is dropped.
    pub fn heartbeat(&self, every: Duration) -> Heartbeat {
        if self.level == Level::Off {
            return Heartbeat { done: None };
        }
        let done = Arc::new(AtomicBool::new(false));
        let rep = self.clone();
        let flag = done.clone();
        let handle = std::thread::spawn(move || {
            // Poll in short slices so dropping the guard returns promptly.
            const SLICE: Duration = Duration::from_millis(250);
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(SLICE);
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                let idle_ms = rep.start.elapsed().as_millis() as u64
                    - rep.last_ms.load(Ordering::Relaxed);
                if idle_ms >= every.as_millis() as u64 {
                    let what = if rep.label.is_empty() {
                        "still working".to_string()
                    } else {
                        format!("still working on {}", rep.label)
                    };
                    rep.emit(
                        '…',
                        &format!("{what} (quiet for {})", fmt_elapsed(Duration::from_millis(idle_ms))),
                    );
                }
            }
        });
        Heartbeat {
            done: Some((done, handle)),
        }
    }

    /// Formats one `stream-json` event as a commentary line (or several, for
    /// a message carrying multiple content blocks).
    pub fn event(&self, ev: &Value) {
        if self.level == Level::Off {
            return;
        }
        match ev.get("type").and_then(Value::as_str).unwrap_or("") {
            // Only the `init` system event describes the session; the others
            // (compaction boundaries and the like) carry no model/tool info.
            "system" if ev.get("subtype").and_then(Value::as_str) == Some("init") => {
                if self.level.is_verbose() {
                    let model = ev.get("model").and_then(Value::as_str).unwrap_or("?");
                    let tools = ev.get("tools").and_then(Value::as_array).map_or(0, Vec::len);
                    if tools > 0 {
                        self.emit('▸', &format!("session started (model {model}, {tools} tools)"));
                    } else {
                        self.emit('▸', &format!("session started (model {model})"));
                    }
                }
            }
            "assistant" => self.blocks(ev, true),
            "user" => self.blocks(ev, false),
            // Cursor agent: tool calls are top-level events, not content blocks.
            "tool_call" => self.tool_call_event(ev),
            // Older / alternate Cursor MCP event shape.
            "mcpToolCall" => {
                let name = ev
                    .get("toolName")
                    .or_else(|| ev.get("tool_name"))
                    .and_then(Value::as_str)
                    .unwrap_or("mcp");
                let provider = ev
                    .get("providerIdentifier")
                    .or_else(|| ev.get("provider"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let args = ev
                    .get("arguments")
                    .or_else(|| ev.get("args"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let display = if provider.is_empty() {
                    name.to_string()
                } else {
                    format!("{provider}.{name}")
                };
                self.tool_calls.fetch_add(1, Ordering::Relaxed);
                self.emit('→', &describe_tool(&display, &args));
            }
            "result" => {
                let ms = ev.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
                let turns = ev.get("num_turns").and_then(Value::as_u64);
                let cost = ev.get("total_cost_usd").and_then(Value::as_f64);
                let calls = self.tool_calls();
                let mut msg = if let Some(t) = turns {
                    format!(
                        "agent finished — {t} turns, {calls} tool calls, {}",
                        fmt_elapsed(Duration::from_millis(ms))
                    )
                } else {
                    format!(
                        "agent finished — {calls} tool calls, {}",
                        fmt_elapsed(Duration::from_millis(ms))
                    )
                };
                if let Some(c) = cost {
                    msg.push_str(&format!(", ${c:.2}"));
                }
                let ok = !ev.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                self.emit(if ok { '✓' } else { '⚠' }, &msg);
            }
            _ => {}
        }
    }

    fn tool_call_event(&self, ev: &Value) {
        let subtype = ev.get("subtype").and_then(Value::as_str).unwrap_or("");
        let Some((name, args, result)) = parse_tool_call(ev) else {
            return;
        };
        match subtype {
            "started" => {
                self.tool_calls.fetch_add(1, Ordering::Relaxed);
                self.emit('→', &describe_tool(&name, &args));
            }
            "completed" => {
                if crate::browser::result_looks_disconnected(&result) {
                    self.warn(
                        "the browser is gone — Chrome exited or was closed mid-run; check `huntwell doctor`",
                    );
                } else if crate::browser::looks_like_mcp_timeout(&tool_result_text(&result)) {
                    self.emit(
                        '⚠',
                        "browser call timed out — usually a slow or hung page; retrying in the browser beats falling back to WebSearch",
                    );
                } else if tool_result_is_error(&result) {
                    // A missing tool is the model inventing a name (browser_scroll
                    // and friends). It is not a broken run, and the next call is
                    // the recovery — so this stays a quiet note, not a warning.
                    let (marker, msg) = explain_tool_error(&result);
                    self.emit(marker, &msg);
                } else if self.level.is_verbose() {
                    let text = tool_result_text(&result);
                    let size = if text.is_empty() {
                        result.to_string().len()
                    } else {
                        text.len()
                    };
                    self.emit('↩', &format!("result, {}", human_bytes(size)));
                }
            }
            _ => {}
        }
    }

    fn blocks(&self, ev: &Value, assistant: bool) {
        let Some(blocks) = ev.pointer("/message/content").and_then(Value::as_array) else {
            return;
        };
        for b in blocks {
            match b.get("type").and_then(Value::as_str).unwrap_or("") {
                "text" if assistant => {
                    let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                    if t.trim().is_empty() {
                        continue;
                    }
                    if is_payload(t) {
                        self.emit('·', &format!("returning JSON payload ({} chars)", t.len()));
                    } else {
                        self.emit('·', &one_line(t, 110));
                    }
                }
                "thinking" if assistant && self.level.is_verbose() => {
                    let t = b.get("thinking").and_then(Value::as_str).unwrap_or("");
                    if !t.trim().is_empty() {
                        self.emit('~', &one_line(t, 110));
                    }
                }
                // Legacy Claude-style content blocks (kept for robustness).
                "tool_use" if assistant => {
                    self.tool_calls.fetch_add(1, Ordering::Relaxed);
                    let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let input = b.get("input").unwrap_or(&Value::Null);
                    self.emit('→', &describe_tool(name, input));
                }
                "tool_result" if !assistant => {
                    let text = result_text(b);
                    if b.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                        let (marker, msg) = explain_tool_error_text(&text);
                        self.emit(marker, &msg);
                    } else if self.level.is_verbose() {
                        self.emit('↩', &format!("result, {}", human_bytes(result_size(b, &text))));
                    }
                }
                _ => {}
            }
        }
    }
}

/// Stops the heartbeat thread when dropped.
pub struct Heartbeat {
    done: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        if let Some((flag, handle)) = self.done.take() {
            flag.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
    }
}

/// Pulls `(display_name, args, result)` out of a Cursor `tool_call` event.
///
/// Public to the crate because [`crate::guard`] screens the same events, and it
/// has to see every one of them — including during silent calls, where this
/// reporter prints nothing.
pub(crate) fn parse_tool_call(ev: &Value) -> Option<(String, Value, Value)> {
    let tc = ev.get("tool_call")?.as_object()?;

    if let Some(func) = tc.get("function") {
        let name = func
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("function")
            .to_string();
        let args = parse_maybe_json_args(func.get("arguments").or_else(|| func.get("args")));
        let result = func.get("result").cloned().unwrap_or(Value::Null);
        return Some((name, args, result));
    }

    if let Some(mcp) = tc.get("mcpToolCall") {
        let name = mcp
            .get("toolName")
            .or_else(|| mcp.get("tool_name"))
            .or_else(|| mcp.pointer("/args/toolName"))
            .or_else(|| mcp.pointer("/args/name"))
            .and_then(Value::as_str)
            .unwrap_or("mcp");
        let provider = mcp
            .get("providerIdentifier")
            .or_else(|| mcp.get("provider"))
            .or_else(|| mcp.pointer("/args/providerIdentifier"))
            .and_then(Value::as_str)
            // The browser is the only server a scrape can reach, so an event
            // that omits the provider came from it. It must be named the same
            // way `guard` names it, or every browser call an event under-reports
            // reads as a forbidden server and kills the run on the first
            // navigate.
            .unwrap_or(crate::sandbox::BROWSER_SERVER);
        let args = mcp
            .get("arguments")
            .or_else(|| mcp.pointer("/args/arguments"))
            .or_else(|| mcp.get("args").and_then(|a| a.get("arguments")))
            .or_else(|| mcp.get("args"))
            .cloned()
            .unwrap_or(Value::Null);
        let result = mcp.get("result").cloned().unwrap_or(Value::Null);
        return Some((format!("{provider}.{name}"), args, result));
    }

    for (key, val) in tc {
        if let Some(base) = key.strip_suffix("ToolCall") {
            let args = val.get("args").cloned().unwrap_or(Value::Null);
            let result = val.get("result").cloned().unwrap_or(Value::Null);
            return Some((pretty_builtin_tool(base), args, result));
        }
    }
    None
}

fn parse_maybe_json_args(v: Option<&Value>) -> Value {
    match v {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::String(s.clone())),
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

fn pretty_builtin_tool(base: &str) -> String {
    // readToolCall → Read, shellToolCall → Shell, webSearchToolCall → WebSearch
    if base.is_empty() {
        return "tool".into();
    }
    let mut chars = base.chars();
    let Some(first) = chars.next() else {
        return "tool".into();
    };
    let mut out = String::new();
    out.push(first.to_ascii_uppercase());
    for c in chars {
        out.push(c);
    }
    out
}

fn tool_result_is_error(result: &Value) -> bool {
    if result.is_null() {
        return false;
    }
    if result.get("error").is_some() {
        return true;
    }
    if result.get("failure").is_some() {
        return true;
    }
    if result.get("is_error").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    false
}

fn tool_result_text(result: &Value) -> String {
    if result.is_null() {
        return String::new();
    }
    if let Some(s) = result.as_str() {
        return s.to_string();
    }
    for path in [
        "/success/content",
        "/error/message",
        "/error/error",
        "/failure/message",
        "/message",
        "/content",
    ] {
        if let Some(s) = result.pointer(path).and_then(Value::as_str) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    if let Some(s) = nested_error_string(result) {
        return s;
    }
    result.to_string()
}

/// Cursor nests failures as `{error: {error: "…"}}`. Walk that without
/// dumping the object at the reader.
fn nested_error_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Object(m) => {
            for k in ["message", "error", "errorMessage"] {
                if let Some(found) = m.get(k).and_then(nested_error_string) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Turns a tool failure into something a person can read. The marker is `·`
/// when nothing actually broke (the model named a tool that is not there);
/// `⚠` only when the action itself failed.
fn explain_tool_error(result: &Value) -> (char, String) {
    explain_tool_error_text(&tool_result_text(result))
}

fn explain_tool_error_text(text: &str) -> (char, String) {
    if let Some(msg) = unknown_tool_message(text) {
        return ('·', msg);
    }
    let inner = innermost_error_line(text);
    if let Some(msg) = unknown_tool_message(&inner) {
        return ('·', msg);
    }
    if inner.starts_with('{') || inner.starts_with('[') {
        return (
            '⚠',
            "a browser action failed; it will try another way".into(),
        );
    }
    ('⚠', format!("tool error: {}", one_line(&inner, 100)))
}

/// `Tool "browser_scroll" not found in namespace "browser".` — Cursor's
/// wording when the model invents an MCP name. Playwright never shipped
/// `browser_scroll`; paging is Page Down or a click on the snapshot.
fn unknown_tool_message(text: &str) -> Option<String> {
    let t = text.replace('\\', "");
    if !t.to_ascii_lowercase().contains("not found in namespace") {
        return None;
    }
    let name = tool_name_in_not_found(&t).unwrap_or("");
    let short = name
        .rsplit(['.', '_'])
        .find(|s| !s.is_empty())
        .unwrap_or(name);
    Some(match short {
        "scroll" => "needed more of the page — paging down another way".into(),
        "" => "skipped an action the browser does not have".into(),
        other => format!("skipped a {other} action the browser does not have"),
    })
}

fn tool_name_in_not_found(text: &str) -> Option<&str> {
    let start = text.find("Tool \"")?;
    let rest = &text[start + 6..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn innermost_error_line(text: &str) -> String {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        if let Some(s) = nested_error_string(&v) {
            return s;
        }
    }
    trimmed.to_string()
}

/// Turns a tool call into a one-line human description. Browser tools get
/// a `browser.` prefix; Claude-in-Chrome leftovers keep a `chrome.` prefix.
/// The most telling argument is picked by key, falling back to the first short
/// scalar so unfamiliar tools still say something useful.
fn describe_tool(name: &str, input: &Value) -> String {
    let short = shorten_tool_name(name);
    match telling_detail(input, 0) {
        Some(detail) => format!("{short} {detail}"),
        None => short,
    }
}

/// Keys that usually carry the "what is it doing" detail, best first.
const TELLING_KEYS: [&str; 11] = [
    "url", "query", "command", "pattern", "prompt", "file_path", "path", "text", "action",
    "selector", "description",
];

/// List arguments worth showing: for a batch tool the list *is* the detail.
const TELLING_LISTS: [&str; 5] = ["names", "companies", "domains", "keys", "urls"];

/// Keys that identify the tool rather than describe the call.
///
/// Cursor puts the server-qualified tool name in `name`, so without this the
/// log reads `browser.navigate name=browser-browser_navigate` — the tool's own
/// identity, twice, instead of the page it opened.
const IDENTIFIER_KEYS: [&str; 5] =
    ["name", "toolname", "provideridentifier", "provider", "server"];

/// Finds the most telling argument, wherever it sits.
///
/// Searched recursively because the argument object is nested differently
/// across CLI versions — sometimes `arguments`, sometimes wrapped a level
/// deeper — and a display that only checks the top level silently degrades to
/// showing nothing useful. Depth-limited so a huge tool result can't turn a log
/// line into a tree walk.
fn telling_detail(input: &Value, depth: usize) -> Option<String> {
    const MAX_DEPTH: usize = 4;
    let obj = input.as_object()?;

    for k in TELLING_KEYS {
        if let Some(s) = obj.get(k).and_then(Value::as_str) {
            if !s.trim().is_empty() {
                return Some(one_line(s, 90));
            }
        }
    }
    for k in TELLING_LISTS {
        if let Some(items) = obj.get(k).and_then(Value::as_array) {
            if let Some(rendered) = render_list(items) {
                return Some(rendered);
            }
        }
    }
    if depth < MAX_DEPTH {
        for (_, v) in obj {
            if v.is_object() {
                if let Some(found) = telling_detail(v, depth + 1) {
                    return Some(found);
                }
            }
        }
    }
    // Unfamiliar schema: the first short scalar that is not the tool's own name.
    for (k, v) in obj {
        if IDENTIFIER_KEYS.contains(&k.to_ascii_lowercase().as_str()) {
            continue;
        }
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => continue,
        };
        if !s.is_empty() && s.len() <= 60 {
            return Some(format!("{k}={s}"));
        }
    }
    None
}

fn render_list(items: &[Value]) -> Option<String> {
    let shown: Vec<String> = items
        .iter()
        .filter_map(|v| v.as_str())
        .take(3)
        .map(|s| one_line(s, 28))
        .collect();
    if shown.is_empty() {
        return None;
    }
    let extra = items.len().saturating_sub(shown.len());
    let mut out = shown.join(", ");
    if extra > 0 {
        out.push_str(&format!(" +{extra} more"));
    }
    Some(out)
}

fn shorten_tool_name(name: &str) -> String {
    // mcp__browsermcp__browser_navigate → browser.navigate
    // browsermcp.browser_navigate → browser.navigate
    // browser_navigate → browser.navigate
    // mcp__claude-in-chrome__navigate → chrome.navigate
    if let Some((prefix, tail)) = name.rsplit_once("__") {
        if prefix.contains("claude-in-chrome") {
            return format!("chrome.{tail}");
        }
        if prefix.contains("browsermcp") || tail.starts_with("browser_") {
            let tool = tail.strip_prefix("browser_").unwrap_or(tail);
            return format!("browser.{tool}");
        }
        return tail.to_string();
    }
    if let Some((provider, tool)) = name.split_once('.') {
        if provider.contains("browsermcp") || tool.starts_with("browser_") {
            let tool = tool.strip_prefix("browser_").unwrap_or(tool);
            return format!("browser.{tool}");
        }
        // The MCP server is still named `prospects` internally; the log says
        // artifacts, which is what a run collects.
        if provider == "prospects" || provider == "mcp-prospects" {
            let tool = tool.strip_prefix("prospect_").unwrap_or(tool);
            return format!("artifacts.{tool}");
        }
        return format!("{provider}.{tool}");
    }
    if let Some(tool) = name.strip_prefix("browser_") {
        return format!("browser.{tool}");
    }
    name.to_string()
}

/// Extracts displayable text from a tool_result block, whose `content` is
/// either a string or a list of `{type: "text", text: ...}` blocks.
fn result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Size of a tool result for display. Some tools return structured content
/// with no text blocks, so fall back to the encoded `content` rather than
/// reporting a misleading zero.
fn result_size(block: &Value, text: &str) -> usize {
    if !text.is_empty() {
        return text.len();
    }
    block.get("content").map_or(0, |c| c.to_string().len())
}

/// `4.2 KB` / `812 B`, for prompt and tool-result sizes.
pub fn human_bytes(n: usize) -> String {
    if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

/// True when assistant text is really the JSON result blob rather than prose,
/// so we summarize it instead of dumping 110 characters of `[{"company_dom…`.
fn is_payload(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("```") || t.starts_with('[') || t.starts_with('{')
}

/// Collapses whitespace and truncates to `max` chars with an ellipsis.
pub fn one_line(s: &str, max: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = collapsed.chars().collect();
    if chars.len() <= max {
        return collapsed;
    }
    let mut out: String = chars[..max.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

/// `12.4s` under a minute, `3m04s` above it.
pub fn fmt_elapsed(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let whole = d.as_secs();
        format!("{}m{:02}s", whole / 60, whole % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn level_parsing() {
        assert_eq!(Level::parse("off").unwrap(), Level::Off);
        assert_eq!(Level::parse(" Normal ").unwrap(), Level::Normal);
        assert_eq!(Level::parse("VERBOSE").unwrap(), Level::Verbose);
        assert!(Level::parse("loud").is_err());
    }

    #[test]
    fn browser_tools_get_short_names() {
        let d = describe_tool(
            "browser_navigate",
            &json!({"url": "https://bitso.com"}),
        );
        assert_eq!(d, "browser.navigate https://bitso.com");

        let d = describe_tool(
            "mcp__browsermcp__browser_navigate",
            &json!({"url": "https://bitso.com"}),
        );
        assert_eq!(d, "browser.navigate https://bitso.com");

        let d = describe_tool(
            "browsermcp.browser_navigate",
            &json!({"url": "https://bitso.com"}),
        );
        assert_eq!(d, "browser.navigate https://bitso.com");
    }

    #[test]
    fn a_dedupe_hit_is_visible_in_the_run_log() {
        // The whole path a real event takes: Cursor's NDJSON -> parse_tool_call
        // -> describe_tool -> the line printed at --progress normal. If this
        // renders nothing, the agent is skipping work and you cannot tell.
        let ev = json!({
            "type": "tool_call",
            "subtype": "started",
            "tool_call": {"mcpToolCall": {
                "toolName": "prospect_known",
                "providerIdentifier": "prospects",
                "args": {
                    "name": "prospects-prospect_known",
                    "arguments": {"names": ["Bitso", "clip.mx", "Kapital", "Stori"]}
                }
            }}
        });
        let (name, args, _) = parse_tool_call(&ev).expect("an mcp call must parse");
        assert_eq!(describe_tool(&name, &args), "artifacts.known Bitso, clip.mx, Kapital +1 more");

        // And the guard must wave it through, or the first check kills the run.
        let provider = name.split_once('.').unwrap().0;
        assert!(crate::guard::mcp_provider_allowed(provider));
    }

    #[test]
    fn a_nested_argument_object_still_shows_the_page() {
        // Shape taken from a real run, where the log read
        // "browser.navigate name=browser-browser_navigate" — the tool naming
        // itself instead of the URL, because the arguments sit a level down
        // and `name` won the fallback.
        let d = describe_tool(
            "browser.browser_navigate",
            &json!({
                "name": "browser-browser_navigate",
                "arguments": {"url": "https://bitso.com"}
            }),
        );
        assert_eq!(d, "browser.navigate https://bitso.com");
    }

    #[test]
    fn a_tools_own_identity_is_never_the_detail() {
        // With nothing else to show, the line stays bare rather than repeating
        // the tool name back at the reader.
        let d = describe_tool("browser.browser_snapshot", &json!({"name": "browser-browser_snapshot"}));
        assert_eq!(d, "browser.snapshot");
    }

    #[test]
    fn the_dedupe_check_says_what_it_checked() {
        // Watching a run, this line is how you see the agent skip work it
        // already did — an empty "artifacts.known" would say nothing.
        let d = describe_tool(
            "prospects.prospect_known",
            &json!({"names": ["Bitso", "clip.mx", "Kapital", "Stori", "Konfio"]}),
        );
        assert_eq!(d, "artifacts.known Bitso, clip.mx, Kapital +2 more");

        let d = describe_tool("prospects.searches_done", &json!({}));
        assert_eq!(d, "artifacts.searches_done");
    }

    #[test]
    fn chrome_tools_still_shorten() {
        let d = describe_tool(
            "mcp__claude-in-chrome__navigate",
            &json!({"url": "https://bitso.com", "tabId": 7}),
        );
        assert_eq!(d, "chrome.navigate https://bitso.com");
    }

    #[test]
    fn picks_the_telling_argument() {
        assert_eq!(
            describe_tool("WebSearch", &json!({"query": "Mexico PSP stablecoin"})),
            "WebSearch Mexico PSP stablecoin"
        );
        // No known key: falls back to the first short scalar.
        assert_eq!(
            describe_tool("Odd", &json!({"tabId": 3, "blob": "x".repeat(200)})),
            "Odd tabId=3"
        );
        // Nothing usable at all: just the tool name.
        assert_eq!(describe_tool("Bare", &json!({})), "Bare");
    }

    #[test]
    fn cursor_tool_call_events_are_counted() {
        let r = Reporter::new("scrape", Level::Normal);
        r.event(&json!({
            "type": "tool_call",
            "subtype": "started",
            "tool_call": {
                "mcpToolCall": {
                    "args": {
                        "toolName": "browser_navigate",
                        "providerIdentifier": "browsermcp",
                        "arguments": {"url": "https://bitso.com"}
                    }
                }
            }
        }));
        assert_eq!(r.tool_calls(), 1);
    }

    #[test]
    fn an_event_without_a_provider_is_still_named_the_browser() {
        // The guard judges by the provider half of "provider.tool". When Cursor
        // omits providerIdentifier we fill it in — and if we fill in a name the
        // guard does not allow, the very first navigate reads as a forbidden
        // MCP server and kills the run. Silent, fatal, and only at runtime.
        let (name, _, _) = parse_tool_call(&json!({
            "type": "tool_call",
            "subtype": "started",
            "tool_call": {"mcpToolCall": {"args": {
                "toolName": "browser_navigate",
                "arguments": {"url": "https://example.com"}
            }}}
        }))
        .expect("an mcp tool call event must parse");
        let provider = name.split_once('.').expect("provider.tool").0;
        assert!(
            crate::guard::mcp_provider_allowed(provider),
            "{provider} is not a server the guard allows — every browser call would be fatal"
        );
    }

    #[test]
    fn payload_text_is_summarized_not_dumped() {
        assert!(is_payload("```json\n[{\"a\":1}]"));
        assert!(is_payload("  [{\"a\": 1}]"));
        assert!(!is_payload("I'll search for Mexican PSPs first."));
    }

    #[test]
    fn one_line_collapses_and_truncates() {
        assert_eq!(one_line("a\n  b\tc", 40), "a b c");
        assert_eq!(one_line("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn heartbeat_starts_and_stops_promptly() {
        let r = Reporter::new("probe", Level::Normal);
        let beat = r.heartbeat(Duration::from_millis(20));
        std::thread::sleep(Duration::from_millis(600)); // several idle windows
        let t = Instant::now();
        drop(beat); // joins the thread
        assert!(t.elapsed() < Duration::from_secs(2), "heartbeat guard hung on drop");
    }

    #[test]
    fn elapsed_formatting() {
        assert_eq!(fmt_elapsed(Duration::from_millis(1500)), "1.5s");
        assert_eq!(fmt_elapsed(Duration::from_secs(184)), "3m04s");
    }

    #[test]
    fn tool_uses_are_counted() {
        let r = Reporter::new("scrape", Level::Off); // Off still counts, just prints nothing
        r.event(&json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "name": "WebSearch", "input": {"query": "x"}},
                {"type": "tool_use", "name": "WebFetch", "input": {"url": "y"}}
            ]}
        }));
        // Level::Off returns before counting — verify the Normal path instead.
        assert_eq!(r.tool_calls(), 0);

        let r = Reporter::new("scrape", Level::Normal);
        r.event(&json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "name": "WebSearch", "input": {"query": "x"}}
            ]}
        }));
        assert_eq!(r.tool_calls(), 1);
    }

    #[test]
    fn an_invented_browser_tool_is_not_dumped_as_json() {
        // The exact Cursor payload that used to print
        // `tool error: {"error":{"error":"Tool \"browser_scroll\" not found…"}}`
        // into the execution log.
        let result = json!({
            "error": {"error": "Tool \"browser_scroll\" not found in namespace \"browser\"."}
        });
        assert_eq!(
            tool_result_text(&result),
            "Tool \"browser_scroll\" not found in namespace \"browser\"."
        );
        let (marker, msg) = explain_tool_error(&result);
        assert_eq!(marker, '·');
        assert_eq!(msg, "needed more of the page — paging down another way");
        assert!(!msg.contains('{'), "the reader must never see the JSON envelope");
    }

    #[test]
    fn a_raw_json_tool_error_stays_a_sentence() {
        let (marker, msg) = explain_tool_error_text(
            r#"{"error":{"error":"Tool \"browser_hover\" not found in namespace \"browser\"."}}"#,
        );
        assert_eq!(marker, '·');
        assert_eq!(msg, "skipped a hover action the browser does not have");
    }
}

//! huntwell's own MCP server: what the scraping agent is allowed to ask
//! about its own past work.
//!
//! The agent gets this beside the browser, exposing read-only questions: *do
//! you already have these?*, *what has been searched?*, *how is this plan
//! doing?* It is the one door from an agent reading attacker-controlled pages
//! into the database, so it is built so an injected instruction gains nothing
//! by walking through it:
//!
//!   - Every connection is opened with `default_transaction_read_only = on`
//!     (see [`crate::store::connect_read_only`]). Not "we only write SELECTs"
//!     — the session cannot write.
//!   - It is pinned to one plan id, passed on the command line by the run. No
//!     tool takes a plan argument, so other plans — and other accounts — are
//!     unreachable.
//!   - `prospect_known` answers *yes or no*. It never returns a row: no email,
//!     phone, title or contact name is reachable through any tool here.

use std::io::{self, BufRead, BufReader, Write};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sqlx::Row;

use crate::store::Db;

/// The MCP protocol version echoed back at `initialize`.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Cap on how many names one `prospect_known` call may ask about, and on how
/// many searches `searches_done` returns.
const MAX_BATCH: usize = 200;

pub struct Server {
    db: Db,
    rt: tokio::runtime::Runtime,
    plan_id: i64,
    source: String,
    individual: bool,
}

impl Server {
    /// Opens the store read-only and pins the server to one plan.
    pub fn open(database_url: &str, plan_id: i64) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let db = rt.block_on(crate::store::connect_read_only(database_url))?;
        let row = rt
            .block_on(
                sqlx::query(r#"SELECT source,plan_type FROM plan WHERE plan_id = $1"#)
                    .bind(plan_id)
                    .fetch_optional(&db),
            )
            .context("look up plan")?
            .ok_or_else(|| anyhow::anyhow!("plan {plan_id} does not exist"))?;
        let source: String = row.get(0);
        let plan_type: String = row.get(1);
        Ok(Self { db, rt, plan_id, source, individual: crate::store::is_individual(&plan_type) })
    }

    /// Reads MCP requests from stdin and writes replies to stdout until the
    /// client goes away.
    pub fn serve_stdio(&self) -> Result<()> {
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        let stdout = io::stdout();
        let mut out = stdout.lock();
        while let Some(frame) = read_frame(&mut reader)? {
            let Ok(req) = serde_json::from_slice::<Value>(body_of(&frame)) else {
                continue;
            };
            let Some(id) = req.get("id").cloned() else {
                continue;
            };
            let method = req.get("method").and_then(Value::as_str).unwrap_or("");
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            let reply = match self.dispatch(method, &params) {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32603, "message": format!("{e:#}")}
                }),
            };
            let framed = encode_frame(&reply, uses_content_length(&frame));
            out.write_all(&framed)?;
            out.flush()?;
        }
        Ok(())
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Value> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {
                    "name": crate::sandbox::PROSPECTS_SERVER,
                    "version": env!("CARGO_PKG_VERSION"),
                },
            })),
            "tools/list" => Ok(json!({"tools": tool_definitions()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let text = self.call_tool(name, &args)?;
                Ok(json!({"content": [{"type": "text", "text": text}]}))
            }
            "ping" => Ok(json!({})),
            other => anyhow::bail!("unknown method {other}"),
        }
    }

    fn call_tool(&self, name: &str, args: &Value) -> Result<String> {
        let v = match name {
            "prospect_known" => self.prospect_known(args)?,
            "searches_done" => self.searches_done(args)?,
            "queries_done" => self.queries_done(args)?,
            "pages_seen" => self.pages_seen(args)?,
            "plan_status" => self.plan_status()?,
            other => anyhow::bail!("unknown tool {other}"),
        };
        Ok(serde_json::to_string_pretty(&v)?)
    }

    fn prospect_known(&self, args: &Value) -> Result<Value> {
        let names = string_list(args, &["names", "companies", "keys", "domains", "people"]);
        if names.is_empty() {
            if self.individual {
                anyhow::bail!("pass `names`: a list of people to check");
            }
            anyhow::bail!("pass `names`: a list of company names or domains to check");
        }
        let mut known = Vec::new();
        let mut fresh = Vec::new();
        for name in names.into_iter().take(MAX_BATCH) {
            if self.is_known(&name)? {
                known.push(name);
            } else {
                fresh.push(name);
            }
        }
        Ok(json!({
            "known": known,
            "new": fresh,
            "note": "`known` are already stored for this plan — do not spend time on them. \
                     Gather details for the ones in `new`.",
        }))
    }

    fn is_known(&self, needle: &str) -> Result<bool> {
        let key = crate::normalize::cleanse_key(needle).to_lowercase();
        if key.is_empty() {
            return Ok(false);
        }
        let bare = key
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.")
            .trim_end_matches('/')
            .to_string();
        // Spliced into the SQL below as an identifier, so it has to be the
        // column's real (lowercase) name — a bind parameter cannot name a column.
        let identity = if self.individual { "name" } else { "company" };
        let n: i64 = self.rt.block_on(
            sqlx::query_scalar(&format!(
                r#"SELECT count(*) FROM prospect
                   WHERE plan_id = $1 AND (
                       lower(source_key) = $2 OR lower(source_key) = $3
                       OR lower({identity}) = $2 OR lower({identity}) = $3
                       OR lower(website) LIKE '%' || $3 || '%' )"#
            ))
            .bind(self.plan_id)
            .bind(&key)
            .bind(&bare)
            .fetch_one(&self.db),
        )?;
        Ok(n > 0)
    }

    fn searches_done(&self, args: &Value) -> Result<Value> {
        let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(50).clamp(1, MAX_BATCH as i64);
        let explored = self.seeds("explored", r#"explored_at DESC NULLS LAST"#, limit)?;
        let queued = self.seeds("pending", r#"queued_at ASC NULLS LAST"#, limit)?;
        Ok(json!({
            "already_searched": explored,
            "queued_next": queued,
            "note": "Propose a search that is not in `already_searched` or `queued_next`. \
                     A reworded repeat of one of these finds the same companies again.",
        }))
    }

    fn seeds(&self, status: &str, order: &str, limit: i64) -> Result<Vec<Value>> {
        let rows: Vec<String> = self.rt.block_on(
            sqlx::query_scalar(&format!(
                r#"SELECT seed_json FROM search_frontier WHERE plan_id = $1 AND status = $2 ORDER BY {order} LIMIT $3"#
            ))
            .bind(self.plan_id)
            .bind(status)
            .bind(limit)
            .fetch_all(&self.db),
        )?;
        Ok(rows.into_iter().map(parse_seed).collect())
    }

    fn queries_done(&self, args: &Value) -> Result<Value> {
        let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(40).clamp(1, MAX_BATCH as i64);
        let rows = self.rt.block_on(crate::store::list_search_queries(&self.db, self.plan_id, limit))?;
        let queries: Vec<Value> = rows
            .iter()
            .map(|q| {
                json!({
                    "query": q.query,
                    "engine": q.engine,
                    "times_searched": q.hits,
                    "deepest_page": q.max_depth,
                    "prospects_found": q.new_prospects,
                    "last_run": q.last_used_at,
                })
            })
            .collect();
        Ok(json!({
            "queries": queries,
            "note": "These have been searched before. Prefer a query that is not in this list. \
                     If you reuse one, start past its `deepest_page` — the pages above it have \
                     already been read.",
        }))
    }

    fn pages_seen(&self, args: &Value) -> Result<Value> {
        let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(40).clamp(1, MAX_BATCH as i64);
        let rows = self.rt.block_on(crate::store::list_visited_pages(&self.db, self.plan_id, limit))?;
        let pages: Vec<Value> = rows
            .iter()
            .map(|p| json!({"url": p.url, "host": p.host, "visits": p.visits, "last_seen": p.last_seen_at}))
            .collect();
        Ok(json!({
            "pages": pages,
            "note": "Opened before, least recently first. A directory or list here may have \
                     had more companies on it than were taken the first time.",
        }))
    }

    fn plan_status(&self) -> Result<Value> {
        let row = self.rt.block_on(
            sqlx::query(
                r#"SELECT (SELECT count(*) FROM prospect WHERE plan_id = $1),
                          (SELECT count(*) FROM search_frontier WHERE plan_id = $1 AND status = 'explored'),
                          (SELECT count(*) FROM search_frontier WHERE plan_id = $1 AND status = 'pending')"#,
            )
            .bind(self.plan_id)
            .fetch_one(&self.db),
        )?;
        Ok(json!({
            "plan": self.source,
            "prospects_stored": row.get::<i64, _>(0),
            "searches_done": row.get::<i64, _>(1),
            "searches_queued": row.get::<i64, _>(2),
        }))
    }
}

fn parse_seed(raw: String) -> Value {
    serde_json::from_str::<Value>(&raw).unwrap_or(Value::String(raw))
}

/// Advertised tool surface.
///
/// Descriptions are written for the model: they say when to reach for the tool,
/// because a tool the model does not think to call is the same as no tool.
fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "prospect_known",
            "description":
                "Check which prospects this plan has ALREADY stored — companies on a \
                 business plan, people on an individual one. Call this on every page \
                 of search results, BEFORE opening any of the links, passing every candidate \
                 from that page in a single call — names, bare domains and full URLs all \
                 match. Returns `known` (already stored, skip them) and `new` (worth opening). \
                 Opening a site to discover it was already stored costs a page load and a \
                 snapshot and tells you nothing; this call costs neither.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "names": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Company names or domains on a business plan, e.g. \
                                        [\"Bitso\", \"clip.mx\"]; people's names on an \
                                        individual plan, e.g. [\"Jane Doe\"].",
                    }
                },
                "required": ["names"],
            },
        }),
        json!({
            "name": "searches_done",
            "description":
                "List the searches this plan has already run, and the ones queued next. Call \
                 this before deciding what to search for, so you pick an angle that has not \
                 been tried instead of rewording one that has.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "How many of each to return (default 50)."}
                },
            },
        }),
        json!({
            "name": "queries_done",
            "description":
                "List the exact search queries this plan has already typed, how many times each \
                 was run, and the deepest page of results anyone read for it. Call this before \
                 your first search. Pick a query that is not on the list; if you do reuse one, \
                 start past its `deepest_page` rather than at page 1 — those pages have been \
                 read and their companies are already stored.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "How many to return (default 40)."}
                },
            },
        }),
        json!({
            "name": "pages_seen",
            "description":
                "List pages this plan has opened before, least recently seen first. Useful when \
                 search results are going stale: a directory or listicle here may still have \
                 companies on it that were never taken.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "How many to return (default 40)."}
                },
            },
        }),
        json!({
            "name": "plan_status",
            "description":
                "How this plan stands: prospects stored so far, searches done, searches queued.",
            "inputSchema": {"type": "object", "properties": {}},
        }),
    ]
}

/// Pulls a list of strings out of whichever argument name the model used.
///
/// Models reach for `companies` or `domains` as readily as the documented
/// `names`, and a rejected call costs a whole retry round-trip. A bare string
/// is accepted as a one-element list for the same reason.
fn string_list(args: &Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        match args.get(*key) {
            Some(Value::Array(a)) => {
                let v: Vec<String> = a
                    .iter()
                    .filter_map(|x| x.as_str().map(str::trim).filter(|s| !s.is_empty()))
                    .map(str::to_string)
                    .collect();
                if !v.is_empty() {
                    return v;
                }
            }
            Some(Value::String(s)) if !s.trim().is_empty() => {
                return vec![s.trim().to_string()];
            }
            _ => {}
        }
    }
    Vec::new()
}

// -----------------------------------------------------------------------
// MCP stdio framing
// -----------------------------------------------------------------------

fn uses_content_length(frame: &[u8]) -> bool {
    frame.len() > 14 && frame[..14].eq_ignore_ascii_case(b"content-length")
}

fn body_of(frame: &[u8]) -> &[u8] {
    if let Some(i) = frame
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| frame.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
    {
        return &frame[i..];
    }
    frame
}

fn encode_frame(v: &Value, content_length: bool) -> Vec<u8> {
    let body = serde_json::to_vec(v).unwrap_or_default();
    if content_length {
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend(body);
        out
    } else {
        let mut out = body;
        out.push(b'\n');
        out
    }
}

/// Reads one MCP frame: either newline-delimited JSON or a `Content-Length`
/// header block. Clients use both, so the server accepts both and answers in
/// whichever the caller used.
fn read_frame<R: BufRead>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut first = [0u8; 1];
    match r.read_exact(&mut first) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
        Ok(()) => {}
    }
    if first[0] == b'{' {
        let mut rest = Vec::new();
        r.read_until(b'\n', &mut rest)?;
        let mut frame = Vec::from(first);
        frame.append(&mut rest);
        return Ok(Some(frame));
    }
    let mut headers = Vec::from(first);
    loop {
        let mut line = Vec::new();
        if r.read_until(b'\n', &mut line)? == 0 {
            return Ok(None);
        }
        let blank = line == b"\r\n" || line == b"\n";
        headers.append(&mut line);
        if blank {
            break;
        }
        if headers.len() > 64 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "mcp headers too large"));
        }
    }
    let len = String::from_utf8_lossy(&headers)
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0usize);
    let mut body = vec![0u8; len];
    if len > 0 {
        r.read_exact(&mut body)?;
    }
    headers.append(&mut body);
    Ok(Some(headers))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_in_both_dialects() {
        let v = json!({"jsonrpc": "2.0", "id": 1, "result": {}});
        let nd = encode_frame(&v, false);
        assert!(nd.ends_with(b"\n"));
        let mut r = io::Cursor::new(nd.clone());
        let frame = read_frame(&mut r).unwrap().unwrap();
        assert!(!uses_content_length(&frame));
        assert_eq!(serde_json::from_slice::<Value>(body_of(&frame)).unwrap(), v);

        let cl = encode_frame(&v, true);
        assert!(cl.starts_with(b"Content-Length: "));
        let mut r = io::Cursor::new(cl);
        let frame = read_frame(&mut r).unwrap().unwrap();
        assert!(uses_content_length(&frame));
        assert_eq!(serde_json::from_slice::<Value>(body_of(&frame)).unwrap(), v);
    }

    #[test]
    fn argument_names_the_model_actually_uses_are_accepted() {
        let keys = ["names", "companies", "keys", "domains", "people"];
        assert_eq!(string_list(&json!({"companies": ["A", " B "]}), &keys), vec!["A", "B"]);
        assert_eq!(string_list(&json!({"names": "Solo"}), &keys), vec!["Solo"]);
        assert!(string_list(&json!({"other": ["x"]}), &keys).is_empty());
    }
}

//! Anthropic, on the Messages API.
//!
//! Two things here are not in the Chat Completions family and are the reason
//! this adapter is worth its own file:
//!
//!   - **Caching is asked for, not assumed.** A `cache_control` marker on the
//!     end of the system prompt and on the last tool tells Anthropic to keep
//!     that prefix; a hit then costs a tenth of fresh input. For an agent loop,
//!     where the same preamble and tool list are re-sent every turn, that is
//!     most of the bill.
//!   - **Tool results are user turns.** Every result for one assistant turn
//!     must arrive together, in a single user message, or the API refuses the
//!     conversation. Consecutive results are merged here to make that so.

use serde_json::{json, Map, Value};

use super::{kit, usage, LlmError, Message, ModelInfo, Price, Provider, Reply, Request, Stop, ToolCall};

pub struct Anthropic;

const KEY: &str = "ANTHROPIC_API_KEY";
const BASE: &str = "ANTHROPIC_BASE_URL";
const DEFAULT_BASE: &str = "https://api.anthropic.com/v1";
/// The dated API contract. Bumping it is a deliberate act: a newer version can
/// change reply shapes, which is what `conformance` would catch.
const VERSION: &str = "2023-06-01";

#[async_trait::async_trait]
impl Provider for Anthropic {
    fn id(&self) -> &'static str {
        "anthropic"
    }
    fn label(&self) -> &'static str {
        "Anthropic"
    }
    fn key_setting(&self) -> &'static str {
        KEY
    }

    async fn complete(&self, req: &Request) -> Result<Reply, LlmError> {
        let key = kit::api_key(KEY)?;
        let url = format!("{}/messages", kit::base_url(BASE, DEFAULT_BASE));
        let headers = vec![("x-api-key", key), ("anthropic-version", VERSION.to_string())];
        let v = kit::post_json("anthropic", &url, headers, &build(req)).await?;
        parse(&v)
    }

    fn models(&self) -> Vec<ModelInfo> {
        // $/M input · cached read · output. Cached reads are a tenth of input
        // across the range. Named by family, so a new Haiku is priced as one.
        vec![
            ModelInfo {
                id: "claude-haiku-4-5".into(),
                label: "Claude Haiku".into(),
                family: "haiku".into(),
                price: Price::new(1.0, 0.10, 5.0),
                context: 200_000,
                good_for_tools: true,
            },
            ModelInfo {
                id: "claude-sonnet-5".into(),
                label: "Claude Sonnet".into(),
                family: "sonnet".into(),
                // Sonnet 5's list price (Anthropic's model table, 2026-06).
                price: Price::new(2.0, 0.20, 10.0),
                context: 200_000,
                good_for_tools: true,
            },
            ModelInfo {
                id: "claude-opus-5".into(),
                label: "Claude Opus".into(),
                family: "opus".into(),
                // Opus 5's list price (Anthropic's model table, 2026-06).
                price: Price::new(5.0, 0.50, 25.0),
                context: 200_000,
                good_for_tools: true,
            },
        ]
    }

    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        let key = kit::api_key(KEY)?;
        let url = format!("{}/models?limit=200", kit::base_url(BASE, DEFAULT_BASE));
        let headers = vec![("x-api-key", key), ("anthropic-version", VERSION.to_string())];
        let v = kit::get_json("anthropic", &url, headers).await?;
        Ok(v["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str()).map(str::to_string).collect())
    }
}

/// Marks a block as the end of the cacheable prefix.
fn cacheable() -> Value {
    json!({ "type": "ephemeral" })
}

pub fn build(req: &Request) -> Value {
    let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len());
    for m in &req.messages {
        match m {
            Message::User(text) => messages.push(json!({ "role": "user", "content": text })),
            Message::Assistant { text, tool_calls } => {
                let mut blocks: Vec<Value> = Vec::new();
                if !text.trim().is_empty() {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                for c in tool_calls {
                    blocks.push(json!({ "type": "tool_use", "id": c.id, "name": c.name, "input": c.args }));
                }
                // An assistant turn may not be empty; a bare space is the
                // smallest thing that is not.
                if blocks.is_empty() {
                    blocks.push(json!({ "type": "text", "text": "." }));
                }
                messages.push(json!({ "role": "assistant", "content": blocks }));
            }
            Message::ToolResult { call_id, content, is_error, .. } => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": content,
                    "is_error": is_error,
                });
                // All results for one assistant turn go in one user message.
                match messages.last_mut() {
                    Some(last) if is_results(last) => {
                        if let Some(a) = last["content"].as_array_mut() {
                            a.push(block);
                        }
                    }
                    _ => messages.push(json!({ "role": "user", "content": [block] })),
                }
            }
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("max_tokens".into(), json!(req.max_output_tokens));
    if !req.system.trim().is_empty() {
        body.insert(
            "system".into(),
            json!([{ "type": "text", "text": req.system, "cache_control": cacheable() }]),
        );
    }
    if !req.tools.is_empty() {
        let last = req.tools.len() - 1;
        body.insert(
            "tools".into(),
            Value::Array(
                req.tools
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let mut tool = json!({ "name": t.name, "description": t.description, "input_schema": kit::plain_schema(&t.parameters) });
                        // One marker, on the last tool: it caches everything
                        // before it, which is the whole fixed prefix.
                        if i == last {
                            tool["cache_control"] = cacheable();
                        }
                        tool
                    })
                    .collect(),
            ),
        );
    }
    body.insert("messages".into(), Value::Array(messages));
    if let Some(t) = req.temperature.filter(|_| accepts_sampling(&req.model)) {
        body.insert("temperature".into(), json!(t));
    }
    Value::Object(body)
}

/// Whether a model still takes `temperature`. The current generation — Sonnet
/// 5, Opus 4.7 and later, Fable, Mythos — refuses sampling parameters with a
/// 400, so a caller's preference is dropped for them rather than failing the
/// call. Older models (Haiku 4.5, the 4.6 family and before) accept it.
fn accepts_sampling(model: &str) -> bool {
    const NO_SAMPLING: [&str; 6] = ["claude-sonnet-5", "claude-opus-5", "claude-opus-4-7", "claude-opus-4-8", "claude-fable", "claude-mythos"];
    !NO_SAMPLING.iter().any(|p| model.starts_with(p))
}

/// Whether this message is a user turn made only of tool results, and so the
/// one the next result belongs in.
fn is_results(m: &Value) -> bool {
    m["role"] == "user"
        && m["content"]
            .as_array()
            .is_some_and(|a| !a.is_empty() && a.iter().all(|b| b["type"] == "tool_result"))
}

pub fn parse(v: &Value) -> Result<Reply, LlmError> {
    let blocks = v.get("content").and_then(Value::as_array).ok_or_else(|| {
        tracing::error!("anthropic: reply carried no content: {}", kit::snippet(&v.to_string()));
        LlmError::Unavailable
    })?;
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str).unwrap_or_default() {
            "text" => text.push_str(b.get("text").and_then(Value::as_str).unwrap_or_default()),
            "tool_use" => tool_calls.push(ToolCall::new(
                b.get("id").and_then(Value::as_str).unwrap_or_default(),
                b.get("name").and_then(Value::as_str).unwrap_or_default(),
                b.get("input").cloned().unwrap_or_else(|| json!({})),
            )),
            // "thinking" blocks and anything newer: not the answer.
            _ => {}
        }
    }
    let stop = match v.get("stop_reason").and_then(Value::as_str).unwrap_or("end_turn") {
        "tool_use" => Stop::ToolUse,
        "max_tokens" => Stop::MaxTokens,
        "refusal" => Stop::Refused,
        _ if !tool_calls.is_empty() => Stop::ToolUse,
        _ => Stop::EndTurn,
    };
    Ok(Reply {
        text: (!text.trim().is_empty()).then_some(text),
        tool_calls,
        usage: usage::anthropic(v.get("usage").unwrap_or(&Value::Null)),
        cost_micros: None,
        stop,
    })
}

#[cfg(test)]
mod tests {

    #[test]
    fn temperature_is_not_sent_to_models_that_refuse_it() {
        let mut req = Request::new("claude-sonnet-5", "sys").user("hi");
        req.temperature = Some(0.5);
        assert!(build(&req).get("temperature").is_none());
        req.model = "claude-haiku-4-5".into();
        assert_eq!(build(&req)["temperature"], json!(0.5));
    }

    use super::*;
    use crate::llm::ToolDef;

    fn tools() -> Vec<ToolDef> {
        vec![
            ToolDef { name: "a".into(), description: "first".into(), parameters: json!({ "type": "object" }) },
            ToolDef { name: "b".into(), description: "second".into(), parameters: json!({ "type": "object" }) },
        ]
    }

    #[test]
    fn the_prefix_is_marked_for_caching_exactly_once_at_its_end() {
        let mut req = Request::new("claude-haiku-4-5", "BE CAREFUL").user("go");
        req.tools = tools();
        let b = build(&req);
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(b["tools"][0].get("cache_control").is_none());
        assert_eq!(b["tools"][1]["cache_control"]["type"], "ephemeral", "the marker goes on the last tool");
        assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
    }

    #[test]
    fn results_for_one_turn_arrive_together_as_the_api_insists() {
        let mut req = Request::new("m", "s").user("go");
        req.messages.push(Message::Assistant {
            text: String::new(),
            tool_calls: vec![
                ToolCall::new("t1", "a", json!({})),
                ToolCall::new("t2", "b", json!({})),
            ],
        });
        req.messages.push(Message::ToolResult { call_id: "t1".into(), name: "a".into(), content: "one".into(), is_error: false });
        req.messages.push(Message::ToolResult { call_id: "t2".into(), name: "b".into(), content: "two".into(), is_error: true });
        let b = build(&req);
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3, "user, assistant, and ONE user turn of results: {msgs:#?}");
        assert_eq!(msgs[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "t1");
        assert_eq!(msgs[2]["content"][1]["is_error"], true);
        // A new user turn after results starts a new message.
        let mut later = req.clone();
        later.messages.push(Message::User("next".into()));
        assert_eq!(build(&later)["messages"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn an_assistant_turn_is_never_empty() {
        let mut req = Request::new("m", "s").user("go");
        req.messages.push(Message::Assistant { text: String::new(), tool_calls: vec![] });
        let b = build(&req);
        assert!(!b["messages"][1]["content"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_reply_becomes_text_or_tool_calls_and_counts_its_cache() {
        let r = parse(&json!({
            "content": [
                { "type": "thinking", "thinking": "hmm" },
                { "type": "text", "text": "here you are" },
                { "type": "tool_use", "id": "toolu_1", "name": "browser_navigate", "input": { "url": "https://x.test" } }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 12, "output_tokens": 80, "cache_read_input_tokens": 4000, "cache_creation_input_tokens": 0 }
        })).unwrap();
        assert_eq!(r.stop, Stop::ToolUse);
        assert_eq!(r.text.as_deref(), Some("here you are"));
        assert_eq!(r.tool_calls[0].args["url"], "https://x.test");
        assert_eq!(r.usage.cache_read, 4000);
        assert_eq!(r.usage.billable(), 92, "a cache hit is not the customer's to pay");
    }

    #[test]
    fn a_shapeless_reply_is_the_provider_being_unavailable() {
        assert!(matches!(parse(&json!({ "type": "error" })), Err(LlmError::Unavailable)));
    }
}

//! The Chat Completions wire format, which most providers speak.
//!
//! OpenAI defined it and DeepSeek, Groq, Mistral and xAI all copy it, so they
//! share this one implementation and differ only in a [`Spec`] — an endpoint, a
//! key setting, and the couple of flags where they disagree. Their own files
//! hold what is genuinely theirs: the model catalogue and the prices.
//!
//! Gemini and Anthropic do not speak it and have full adapters of their own.

use serde_json::{json, Map, Value};

use super::{kit, usage, LlmError, Message, Reply, Request, Stop, ToolCall};

/// What one Chat Completions provider needs beyond the shared format.
pub struct Spec {
    pub id: &'static str,
    pub key_setting: &'static str,
    pub base_setting: &'static str,
    pub default_base: &'static str,
    /// Newer OpenAI models refuse `max_tokens` and want
    /// `max_completion_tokens`; everyone else still takes the old name.
    pub max_completion_tokens: bool,
}

/// `GET /models`, which every provider in this family serves.
pub async fn list_models(spec: &Spec) -> Result<Vec<String>, LlmError> {
    let key = kit::api_key(spec.key_setting)?;
    let url = format!("{}/models", kit::base_url(spec.base_setting, spec.default_base));
    let v = kit::get_json(spec.id, &url, vec![("authorization", format!("Bearer {key}"))]).await?;
    Ok(v["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str()).map(str::to_string).collect())
}

pub async fn complete(spec: &Spec, req: &Request) -> Result<Reply, LlmError> {
    let key = kit::api_key(spec.key_setting)?;
    let url = format!("{}/chat/completions", kit::base_url(spec.base_setting, spec.default_base));
    let body = build(spec, req);
    let v = kit::post_json(spec.id, &url, vec![("authorization", format!("Bearer {key}"))], &body).await?;
    parse(spec.id, &v)
}

/// The request body. The system prompt and the tools come first and in a fixed
/// order, because every provider here caches on a matching prefix and charges
/// a fraction for a hit — a reordering would silently multiply the bill.
pub fn build(spec: &Spec, req: &Request) -> Value {
    let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
    if !req.system.trim().is_empty() {
        messages.push(json!({ "role": "system", "content": req.system }));
    }
    for m in &req.messages {
        match m {
            Message::User(text) => messages.push(json!({ "role": "user", "content": text })),
            Message::Assistant { text, tool_calls } => {
                let mut msg = Map::new();
                msg.insert("role".into(), json!("assistant"));
                // Content must be present even when the turn was only tool
                // calls; null is what the format expects there.
                msg.insert("content".into(), if text.is_empty() { Value::Null } else { json!(text) });
                if !tool_calls.is_empty() {
                    msg.insert(
                        "tool_calls".into(),
                        Value::Array(
                            tool_calls
                                .iter()
                                .map(|c| {
                                    json!({
                                        "id": c.id,
                                        "type": "function",
                                        "function": { "name": c.name, "arguments": c.args.to_string() }
                                    })
                                })
                                .collect(),
                        ),
                    );
                }
                messages.push(Value::Object(msg));
            }
            // The format has no way to mark a tool result as a failure, so the
            // text says so. Models follow it as well as a flag.
            Message::ToolResult { call_id, content, is_error, .. } => messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": if *is_error { format!("ERROR: {content}") } else { content.clone() },
            })),
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("messages".into(), Value::Array(messages));
    if !req.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": { "name": t.name, "description": t.description, "parameters": kit::plain_schema(&t.parameters) }
                        })
                    })
                    .collect(),
            ),
        );
    }
    let limit = if spec.max_completion_tokens { "max_completion_tokens" } else { "max_tokens" };
    body.insert(limit.into(), json!(req.max_output_tokens));
    if let Some(t) = req.temperature {
        body.insert("temperature".into(), json!(t));
    }
    Value::Object(body)
}

pub fn parse(provider: &str, v: &Value) -> Result<Reply, LlmError> {
    let choice = v.pointer("/choices/0").ok_or_else(|| {
        tracing::error!("{provider}: reply carried no choices: {}", kit::snippet(&v.to_string()));
        LlmError::Unavailable(format!("{provider} sent a reply with no answer in it"))
    })?;
    let message = choice.get("message").unwrap_or(&Value::Null);
    let text = message.get("content").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(str::to_string);

    let mut tool_calls = Vec::new();
    for (i, c) in message.get("tool_calls").and_then(Value::as_array).into_iter().flatten().enumerate() {
        let name = c.pointer("/function/name").and_then(Value::as_str).unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        // Arguments arrive as a JSON *string*. A model can produce one that
        // does not parse; that is the model's mistake to be told about, not a
        // reason to end the run, so it becomes an empty argument set and the
        // tool refuses it by its own rules.
        let raw = c.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("{}");
        let args = serde_json::from_str(raw).unwrap_or_else(|_| {
            tracing::warn!("{provider}: unparseable arguments for {name}: {}", kit::snippet(raw));
            json!({})
        });
        tool_calls.push(ToolCall::new(
            c.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| format!("call_{i}")),
            name,
            args,
        ));
    }

    let stop = match choice.get("finish_reason").and_then(Value::as_str).unwrap_or("stop") {
        "tool_calls" | "function_call" => Stop::ToolUse,
        "length" => Stop::MaxTokens,
        "content_filter" => Stop::Refused,
        // A model can ask for tools and still say "stop"; what it asked for
        // decides, not what it called the ending.
        _ if !tool_calls.is_empty() => Stop::ToolUse,
        _ => Stop::EndTurn,
    };

    Ok(Reply {
        text,
        tool_calls,
        usage: usage::chat_completions(v.get("usage").unwrap_or(&Value::Null)),
        cost_micros: None,
        stop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ToolDef;

    const SPEC: Spec = Spec {
        id: "test",
        key_setting: "TEST_API_KEY",
        base_setting: "TEST_BASE_URL",
        default_base: "https://api.test/v1",
        max_completion_tokens: false,
    };

    fn a_request() -> Request {
        let mut r = Request::new("m-1", "BE CAREFUL").user("find cars");
        r.tools = vec![ToolDef {
            name: "browser_navigate".into(),
            description: "go to a page".into(),
            parameters: json!({ "type": "object", "additionalProperties": false, "properties": { "url": { "type": "string" } }, "required": ["url"] }),
        }];
        r.messages.push(Message::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall::new("call_1", "browser_navigate", json!({ "url": "https://x.test" }))],
        });
        r.messages.push(Message::ToolResult { call_id: "call_1".into(), name: "browser_navigate".into(), content: "- page".into(), is_error: false });
        r
    }

    #[test]
    fn the_request_carries_the_conversation_in_the_providers_shape() {
        let b = build(&SPEC, &a_request());
        assert_eq!(b["model"], "m-1");
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][0]["content"], "BE CAREFUL");
        assert_eq!(b["messages"][1]["content"], "find cars");
        // A tool-call turn has no text and its arguments are a JSON string.
        assert_eq!(b["messages"][2]["content"], Value::Null);
        assert_eq!(b["messages"][2]["tool_calls"][0]["function"]["arguments"], r#"{"url":"https://x.test"}"#);
        assert_eq!(b["messages"][3]["role"], "tool");
        assert_eq!(b["messages"][3]["tool_call_id"], "call_1");
        assert_eq!(b["tools"][0]["function"]["name"], "browser_navigate");
        assert!(b["tools"][0]["function"]["parameters"].get("additionalProperties").is_none(), "providers refuse it");
        assert_eq!(b["max_tokens"], 8192);
    }

    #[test]
    fn the_cacheable_prefix_is_identical_between_calls() {
        // Same system prompt and tools, different conversation: the bytes a
        // provider matches its cache on must not move.
        let mut later = a_request();
        later.messages.push(Message::User("more".into()));
        let (a, b) = (build(&SPEC, &a_request()), build(&SPEC, &later));
        assert_eq!(a["messages"][0], b["messages"][0]);
        assert_eq!(a["tools"], b["tools"]);
    }

    #[test]
    fn a_newer_model_gets_the_name_it_insists_on() {
        let spec = Spec { max_completion_tokens: true, ..SPEC };
        let b = build(&spec, &a_request());
        assert_eq!(b["max_completion_tokens"], 8192);
        assert!(b.get("max_tokens").is_none());
    }

    #[test]
    fn a_failed_tool_result_says_so_in_the_only_place_the_format_has() {
        let mut r = Request::new("m", "s");
        r.messages.push(Message::ToolResult { call_id: "c".into(), name: "t".into(), content: "not allowed".into(), is_error: true });
        assert_eq!(build(&SPEC, &r)["messages"][1]["content"], "ERROR: not allowed");
    }

    #[test]
    fn a_reply_becomes_text_or_tool_calls() {
        let text = parse("test", &json!({
            "choices": [{ "message": { "content": "```json\n[]\n```" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 100, "completion_tokens": 10 }
        })).unwrap();
        assert_eq!(text.stop, Stop::EndTurn);
        assert!(text.text.unwrap().contains("json"));
        assert_eq!(text.usage.input, 100);

        let calls = parse("test", &json!({
            "choices": [{ "message": { "content": null, "tool_calls": [
                { "id": "call_9", "type": "function", "function": { "name": "browser_navigate", "arguments": "{\"url\":\"https://x.test\"}" } }
            ] }, "finish_reason": "tool_calls" }]
        })).unwrap();
        assert_eq!(calls.stop, Stop::ToolUse);
        assert_eq!(calls.tool_calls[0].name, "browser_navigate");
        assert_eq!(calls.tool_calls[0].args["url"], "https://x.test");
    }

    #[test]
    fn a_model_that_asks_for_tools_is_believed_over_its_finish_reason() {
        let r = parse("test", &json!({
            "choices": [{ "message": { "tool_calls": [{ "id": "c", "function": { "name": "t", "arguments": "{}" } }] }, "finish_reason": "stop" }]
        })).unwrap();
        assert_eq!(r.stop, Stop::ToolUse);
    }

    #[test]
    fn broken_arguments_are_an_empty_call_rather_than_a_dead_run() {
        let r = parse("test", &json!({
            "choices": [{ "message": { "tool_calls": [
                { "id": "c", "function": { "name": "browser_navigate", "arguments": "{\"url\": " } }
            ] }, "finish_reason": "tool_calls" }]
        })).unwrap();
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].args, json!({}));
    }

    #[test]
    fn a_reply_with_no_choices_is_the_provider_being_unavailable() {
        assert!(matches!(parse("test", &json!({ "id": "x" })), Err(LlmError::Unavailable(_))));
    }

    #[test]
    fn running_out_of_room_mid_answer_is_its_own_ending() {
        let r = parse("test", &json!({ "choices": [{ "message": { "content": "half" }, "finish_reason": "length" }] })).unwrap();
        assert_eq!(r.stop, Stop::MaxTokens);
    }
}

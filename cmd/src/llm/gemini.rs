//! Google Gemini, on the Generative Language API.
//!
//! The shape differs from everyone else's in three ways worth knowing:
//!
//!   - the model is in the URL, not the body;
//!   - an assistant turn is a `model` turn, and a tool call is a
//!     `functionCall` part rather than a field of its own;
//!   - **there are no tool-call ids.** A result is matched to its call by
//!     name, so this adapter carries our ids only as far as the wire and drops
//!     them, keeping the order the loop produced.
//!
//! Caching is implicit: Gemini matches a repeated prefix by itself and bills
//! the hit at a quarter of input. Keeping the system prompt and tools first
//! and unchanged is all that is required, which [`Request`] already does.

use serde_json::{json, Map, Value};

use super::{kit, usage, LlmError, Message, ModelInfo, Price, Provider, Reply, Request, Stop, ToolCall};

pub struct Gemini;

const KEY: &str = "GEMINI_API_KEY";
const BASE: &str = "GEMINI_BASE_URL";
const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

#[async_trait::async_trait]
impl Provider for Gemini {
    fn id(&self) -> &'static str {
        "gemini"
    }
    fn label(&self) -> &'static str {
        "Google Gemini"
    }
    fn key_setting(&self) -> &'static str {
        KEY
    }

    async fn complete(&self, req: &Request) -> Result<Reply, LlmError> {
        let key = kit::api_key(KEY)?;
        // The model names the endpoint. A model id with a slash or a query in
        // it would reach a different one, so it is refused rather than escaped.
        if req.model.contains(['/', '?', '#', ':']) || req.model.trim().is_empty() {
            return Err(LlmError::BadRequest(format!("'{}' is not a Gemini model id", req.model)));
        }
        let url = format!("{}/models/{}:generateContent", kit::base_url(BASE, DEFAULT_BASE), req.model);
        let v = kit::post_json("gemini", &url, vec![("x-goog-api-key", key)], &build(req)).await?;
        parse(&v)
    }

    fn models(&self) -> Vec<ModelInfo> {
        // Prices are $/M input · cached read · output. Named by *family*: a
        // new Flash version is priced by this entry without an edit.
        vec![
            ModelInfo {
                id: "gemini-3.6-flash-lite".into(),
                label: "Gemini Flash-Lite".into(),
                family: "flash-lite".into(),
                price: Price::new(0.10, 0.025, 0.40),
                context: 1_048_576,
                good_for_tools: true,
            },
            ModelInfo {
                id: "gemini-3.6-flash".into(),
                label: "Gemini Flash".into(),
                family: "flash".into(),
                price: Price::new(0.30, 0.075, 2.50),
                context: 1_048_576,
                good_for_tools: true,
            },
            ModelInfo {
                id: "gemini-3.6-pro".into(),
                label: "Gemini Pro".into(),
                family: "pro".into(),
                price: Price::new(1.25, 0.3125, 10.0),
                context: 1_048_576,
                good_for_tools: true,
            },
        ]
    }

    /// Google's own catalogue, so a retired name is never offered.
    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        let key = kit::api_key(KEY)?;
        let url = format!("{}/models?pageSize=200", kit::base_url(BASE, DEFAULT_BASE));
        let v = kit::get_json("gemini", &url, vec![("x-goog-api-key", key)]).await?;
        Ok(v["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| {
                // Only what this loop can actually drive.
                m["supportedGenerationMethods"]
                    .as_array()
                    .is_none_or(|a| a.iter().any(|x| x.as_str() == Some("generateContent")))
            })
            .filter_map(|m| m["name"].as_str())
            .map(|n| n.trim_start_matches("models/").to_string())
            .filter(|n| !n.contains("embedding") && !n.contains("aqa") && !n.contains("imagen") && !n.contains("veo") && !n.contains("tts"))
            .collect())
    }
}

pub fn build(req: &Request) -> Value {
    let mut contents: Vec<Value> = Vec::with_capacity(req.messages.len());
    for m in &req.messages {
        match m {
            Message::User(text) => contents.push(json!({ "role": "user", "parts": [{ "text": text }] })),
            Message::Assistant { text, tool_calls } => {
                let mut parts: Vec<Value> = Vec::new();
                if !text.trim().is_empty() {
                    parts.push(json!({ "text": text }));
                }
                for c in tool_calls {
                    let mut part = json!({ "functionCall": { "name": c.name, "args": c.args } });
                    // Gemini's thinking models sign each call and refuse the
                    // next request if the signature does not come back.
                    if let Some(sig) = &c.opaque {
                        part["thoughtSignature"] = json!(sig);
                    }
                    parts.push(part);
                }
                if parts.is_empty() {
                    parts.push(json!({ "text": "." }));
                }
                contents.push(json!({ "role": "model", "parts": parts }));
            }
            Message::ToolResult { name, content, is_error, .. } => {
                // `response` has to be an object, so a tool's text is wrapped.
                // The error case is named rather than flagged: the API has no
                // flag, and a model reads the word as well as it would one.
                let response = if *is_error { json!({ "error": content }) } else { json!({ "result": content }) };
                let part = json!({ "functionResponse": { "name": name, "response": response } });
                match contents.last_mut() {
                    Some(last) if is_responses(last) => {
                        if let Some(a) = last["parts"].as_array_mut() {
                            a.push(part);
                        }
                    }
                    _ => contents.push(json!({ "role": "user", "parts": [part] })),
                }
            }
        }
    }

    let mut body = Map::new();
    if !req.system.trim().is_empty() {
        body.insert("systemInstruction".into(), json!({ "parts": [{ "text": req.system }] }));
    }
    if !req.tools.is_empty() {
        body.insert(
            "tools".into(),
            json!([{
                "functionDeclarations": req.tools.iter().map(|t| json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": kit::plain_schema(&t.parameters),
                })).collect::<Vec<_>>()
            }]),
        );
    }
    body.insert("contents".into(), Value::Array(contents));
    let mut generation = Map::new();
    generation.insert("maxOutputTokens".into(), json!(req.max_output_tokens));
    if let Some(t) = req.temperature {
        generation.insert("temperature".into(), json!(t));
    }
    body.insert("generationConfig".into(), Value::Object(generation));
    Value::Object(body)
}

fn is_responses(m: &Value) -> bool {
    m["role"] == "user"
        && m["parts"]
            .as_array()
            .is_some_and(|a| !a.is_empty() && a.iter().all(|p| p.get("functionResponse").is_some()))
}

pub fn parse(v: &Value) -> Result<Reply, LlmError> {
    let usage = usage::gemini(v.get("usageMetadata").unwrap_or(&Value::Null));
    let candidate = v.pointer("/candidates/0").ok_or_else(|| {
        // A prompt Google refuses comes back with no candidate at all, and
        // that is a refusal rather than an outage — but the run should see the
        // difference, so the reason is logged.
        tracing::error!("gemini: no candidate in reply: {}", kit::snippet(&v.to_string()));
        LlmError::Unavailable("gemini sent a reply with no answer in it".into())
    })?;

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for (i, part) in candidate.pointer("/content/parts").and_then(Value::as_array).into_iter().flatten().enumerate() {
        if let Some(t) = part.get("text").and_then(Value::as_str) {
            text.push_str(t);
        }
        if let Some(call) = part.get("functionCall") {
            tool_calls.push(ToolCall {
                // Gemini issues no id; one is made so the loop, the guard and
                // the log can refer to this call.
                id: format!("call_{i}"),
                name: call.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                args: call.get("args").cloned().unwrap_or_else(|| json!({})),
                // Carried back on the next turn, unread. Without it a thinking
                // model rejects the whole conversation.
                opaque: part.get("thoughtSignature").and_then(Value::as_str).map(str::to_string),
            });
        }
    }

    let stop = match candidate.get("finishReason").and_then(Value::as_str).unwrap_or("STOP") {
        "MAX_TOKENS" => Stop::MaxTokens,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" => Stop::Refused,
        _ if !tool_calls.is_empty() => Stop::ToolUse,
        _ => Stop::EndTurn,
    };

    Ok(Reply { text: (!text.trim().is_empty()).then_some(text), tool_calls, usage, cost_micros: None, stop })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ToolDef;

    #[test]
    fn the_request_uses_googles_names_for_everything() {
        let mut req = Request::new("gemini-2.5-flash", "BE CAREFUL").user("find cars");
        req.tools = vec![ToolDef {
            name: "browser_navigate".into(),
            description: "go".into(),
            parameters: json!({ "type": "object", "additionalProperties": false, "properties": { "url": { "type": "string" } } }),
        }];
        req.messages.push(Message::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall::new("call_0", "browser_navigate", json!({ "url": "https://x.test" }))],
        });
        req.messages.push(Message::ToolResult { call_id: "call_0".into(), name: "browser_navigate".into(), content: "- page".into(), is_error: false });
        let b = build(&req);
        assert_eq!(b["systemInstruction"]["parts"][0]["text"], "BE CAREFUL");
        assert_eq!(b["tools"][0]["functionDeclarations"][0]["name"], "browser_navigate");
        assert!(b["tools"][0]["functionDeclarations"][0]["parameters"].get("additionalProperties").is_none());
        assert_eq!(b["contents"][1]["role"], "model");
        assert_eq!(b["contents"][1]["parts"][0]["functionCall"]["args"]["url"], "https://x.test");
        // A result is a user turn, and its payload is an object.
        assert_eq!(b["contents"][2]["role"], "user");
        assert_eq!(b["contents"][2]["parts"][0]["functionResponse"]["response"]["result"], "- page");
        assert_eq!(b["generationConfig"]["maxOutputTokens"], 8192);
    }

    #[test]
    fn a_failed_tool_is_named_as_an_error_since_there_is_no_flag_for_it() {
        let mut req = Request::new("m", "s");
        req.messages.push(Message::ToolResult { call_id: "c".into(), name: "t".into(), content: "not allowed".into(), is_error: true });
        assert_eq!(build(&req)["contents"][0]["parts"][0]["functionResponse"]["response"]["error"], "not allowed");
    }

    #[test]
    fn results_for_one_turn_stay_in_one_user_turn() {
        let mut req = Request::new("m", "s");
        req.messages.push(Message::ToolResult { call_id: "a".into(), name: "a".into(), content: "1".into(), is_error: false });
        req.messages.push(Message::ToolResult { call_id: "b".into(), name: "b".into(), content: "2".into(), is_error: false });
        let b = build(&req);
        assert_eq!(b["contents"].as_array().unwrap().len(), 1);
        assert_eq!(b["contents"][0]["parts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_reply_becomes_text_and_calls_with_ids_of_our_own() {
        let r = parse(&json!({
            "candidates": [{ "content": { "role": "model", "parts": [
                { "text": "looking" },
                { "functionCall": { "name": "browser_navigate", "args": { "url": "https://x.test" } } }
            ] }, "finishReason": "STOP" }],
            "usageMetadata": { "promptTokenCount": 1200, "candidatesTokenCount": 80, "cachedContentTokenCount": 1000, "thoughtsTokenCount": 20 }
        })).unwrap();
        assert_eq!(r.stop, Stop::ToolUse, "asking for a tool outranks a STOP");
        assert_eq!(r.text.as_deref(), Some("looking"));
        assert!(!r.tool_calls[0].id.is_empty(), "the loop needs something to call it");
        assert_eq!(r.usage.input, 200);
        assert_eq!(r.usage.output, 100, "thinking is billed as output");
        assert_eq!(r.usage.cache_read, 1000);
    }

    /// Gemini's thinking models sign every function call and **refuse the next
    /// request** if the signature does not come back: "Function call is
    /// missing a thought_signature in functionCall parts". Not a degradation —
    /// a 400, and the run is over. So what comes back must go back out.
    #[test]
    fn a_thought_signature_comes_back_and_goes_back_out() {
        let reply = parse(&json!({
            "candidates": [{ "content": { "role": "model", "parts": [
                { "functionCall": { "name": "queries_done", "args": {} }, "thoughtSignature": "Cr4BAdHtim8abc==" },
                { "functionCall": { "name": "plan_status", "args": {} } }
            ] }, "finishReason": "STOP" }]
        })).unwrap();
        assert_eq!(reply.tool_calls[0].opaque.as_deref(), Some("Cr4BAdHtim8abc=="));
        assert_eq!(reply.tool_calls[1].opaque, None, "a call without one carries none");

        let mut req = Request::new("gemini-3.6-flash", "s").user("go");
        req.messages.push(Message::Assistant { text: String::new(), tool_calls: reply.tool_calls });
        let parts = build(&req)["contents"][1]["parts"].clone();
        assert_eq!(parts[0]["thoughtSignature"], "Cr4BAdHtim8abc==", "it must be echoed verbatim");
        assert_eq!(parts[0]["functionCall"]["name"], "queries_done");
        // And one that never had a signature does not gain an empty field,
        // which the API would also reject.
        assert!(parts[1].get("thoughtSignature").is_none());
    }

    #[test]
    fn a_refusal_is_not_an_outage() {
        let r = parse(&json!({ "candidates": [{ "content": { "parts": [] }, "finishReason": "SAFETY" }] })).unwrap();
        assert_eq!(r.stop, Stop::Refused);
    }

    #[tokio::test]
    async fn a_model_id_that_would_change_the_endpoint_is_refused() {
        let _env = crate::llm::test_env();
        std::env::set_var(KEY, "k");
        let bad = Gemini.complete(&Request::new("../../other:generateContent", "s")).await;
        assert!(matches!(bad, Err(LlmError::BadRequest(_))), "{bad:?}");
        std::env::remove_var(KEY);
    }
}

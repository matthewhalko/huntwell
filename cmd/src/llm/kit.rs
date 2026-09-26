//! What every adapter needs and none of them should write twice: the HTTP
//! client, retries, the per-provider concurrency cap, and turning an HTTP
//! failure into one of [`LlmError`]'s few classes.
//!
//! An adapter's job is the shape of a request and the shape of a reply. If it
//! is reaching for a timeout, a backoff or a status code, it belongs here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;

use super::LlmError;

/// Long, because a model reading a large page thinks for a while and a timeout
/// throws away everything the call had done — but not so long that a hung
/// request looks like a working one. Five minutes, times four attempts, meant
/// a stage could sit for twenty with nothing on screen.
const TIMEOUT: Duration = Duration::from_secs(150);

/// In-flight calls per provider, per process. A worker VM runs several plan
/// slots; without a cap they arrive at one provider together and all get 429.
const DEFAULT_CONCURRENCY: usize = 4;

/// Attempts, counting the first. Beyond this a rate limit is the answer.
const ATTEMPTS: usize = 4;

/// How many times to try. Settable so a deployment behind a flaky link can
/// ask for more, and so the conformance tests can ask for one.
fn attempts() -> usize {
    crate::config::get("HUNTWELL_LLM_ATTEMPTS").and_then(|v| v.trim().parse().ok()).unwrap_or(ATTEMPTS).clamp(1, 10)
}

pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("huntwell/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("build the model provider HTTP client")
    })
}

fn concurrency() -> usize {
    crate::config::get("HUNTWELL_LLM_CONCURRENCY")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_CONCURRENCY)
        .clamp(1, 64)
}

/// One semaphore per provider, made on first use.
fn gate(provider: &str) -> Arc<tokio::sync::Semaphore> {
    static GATES: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>> = OnceLock::new();
    let gates = GATES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut gates = gates.lock().unwrap_or_else(|e| e.into_inner());
    gates
        .entry(provider.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(concurrency())))
        .clone()
}

/// The base URL for a provider, letting an operator point one at a proxy or a
/// regional endpoint without a rebuild.
pub fn base_url(setting: &str, default: &str) -> String {
    crate::config::get(setting)
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub fn api_key(setting: &str) -> Result<String, LlmError> {
    crate::config::get(setting)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or(LlmError::Unauthorized)
}

/// POST `body` as JSON and return the parsed reply, retrying what is worth
/// retrying.
///
/// `provider` names the caller for the concurrency gate and the log.
/// `headers` carries the provider's auth, since no two agree on it.
pub async fn post_json(
    provider: &'static str,
    url: &str,
    headers: Vec<(&'static str, String)>,
    body: &Value,
) -> Result<Value, LlmError> {
    let gate = gate(provider);
    let _permit = gate.acquire().await.map_err(|_| LlmError::Unavailable)?;

    let attempts = attempts();
    let mut wait = Duration::from_millis(500);
    let mut last = LlmError::Unavailable;
    for attempt in 1..=attempts {
        match try_once(provider, url, &headers, body).await {
            Ok(v) => return Ok(v),
            Err(e) if e.retryable() && attempt < attempts => {
                // A provider that says how long to wait is believed, within
                // reason: a 10-minute Retry-After is a run-ending wait, so it
                // is capped and the call fails instead.
                let hinted = match &e {
                    LlmError::RateLimited { retry_after: Some(d) } => *d,
                    _ => wait,
                };
                let sleep = hinted.min(Duration::from_secs(30)).max(wait);
                tracing::warn!("{provider}: attempt {attempt} failed ({e}) — retrying in {:.1}s", sleep.as_secs_f32());
                tokio::time::sleep(sleep).await;
                wait = (wait * 2).min(Duration::from_secs(16));
                last = e;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

async fn try_once(provider: &'static str, url: &str, headers: &[(&'static str, String)], body: &Value) -> Result<Value, LlmError> {
    let started = std::time::Instant::now();
    let mut req = client().post(url).json(body);
    for (name, value) in headers {
        req = req.header(*name, value);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("{provider}: {e}");
            return Err(if e.is_timeout() { LlmError::Unavailable } else { LlmError::Unavailable });
        }
    };
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let text = resp.text().await.unwrap_or_default();
    if started.elapsed() > Duration::from_secs(60) {
        tracing::warn!("{provider}: that call took {:.0}s", started.elapsed().as_secs_f32());
    }
    if status.is_success() {
        return serde_json::from_str(&text).map_err(|e| {
            tracing::error!("{provider}: unparseable reply ({e}): {}", snippet(&text));
            LlmError::Unavailable
        });
    }
    Err(classify(provider, status.as_u16(), &text, retry_after))
}

/// GET JSON, for a provider's list-of-models endpoint. One attempt: a
/// catalogue that will not load falls back to the built-in list, and making
/// the admin wait through a backoff for that is worse than answering at once.
pub async fn get_json(provider: &'static str, url: &str, headers: Vec<(&'static str, String)>) -> Result<Value, LlmError> {
    let mut req = client().get(url).timeout(Duration::from_secs(15));
    for (name, value) in &headers {
        req = req.header(*name, value);
    }
    let resp = req.send().await.map_err(|e| {
        tracing::warn!("{provider}: listing models: {e}");
        LlmError::Unavailable
    })?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(classify(provider, status.as_u16(), &text, None));
    }
    serde_json::from_str(&text).map_err(|_| LlmError::Unavailable)
}

/// An HTTP failure as one of the classes the loop reacts to.
///
/// The provider's own words are logged and dropped: they name projects,
/// quotas, organisation ids and sometimes the key itself.
pub fn classify(provider: &str, status: u16, body: &str, retry_after: Option<Duration>) -> LlmError {
    let lower = body.to_ascii_lowercase();
    // Context-length failures arrive as 400s, and are the one 400 the loop can
    // do something about, so they are recognised by what they say.
    let too_long = lower.contains("context length")
        || lower.contains("context_length")
        || lower.contains("too many tokens")
        || lower.contains("maximum context")
        || lower.contains("prompt is too long")
        || lower.contains("exceeds the maximum")
        || lower.contains("input token count");
    match status {
        429 => {
            tracing::warn!("{provider}: rate limited: {}", snippet(body));
            LlmError::RateLimited { retry_after }
        }
        401 | 403 => {
            tracing::error!("{provider}: credentials refused ({status}): {}", snippet(body));
            LlmError::Unauthorized
        }
        400 | 413 | 422 if too_long => LlmError::ContextTooLong,
        400 | 404 | 405 | 413 | 415 | 422 => {
            tracing::error!("{provider}: rejected our request ({status}): {}", snippet(body));
            // Ours to fix, so the detail is kept — but only ours: the
            // provider's text stays in the log.
            LlmError::BadRequest(format!("HTTP {status} from {provider}"))
        }
        _ => {
            tracing::warn!("{provider}: HTTP {status}: {}", snippet(body));
            LlmError::Unavailable
        }
    }
}

/// Enough of a body to diagnose from, bounded so a provider cannot fill the
/// log with one reply.
pub fn snippet(body: &str) -> String {
    let flat: String = body.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 400 {
        format!("{}…", flat.chars().take(400).collect::<String>())
    } else {
        flat
    }
}

/// A JSON Schema with the fields providers disagree about removed.
///
/// Gemini and several others reject `additionalProperties`, `$schema` and
/// `definitions` in a function declaration. The tool definitions are ours, so
/// the simplest thing is not to send what anybody refuses.
pub fn plain_schema(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "additionalProperties" | "$schema" | "definitions" | "$defs" | "examples" | "default"))
                .map(|(k, v)| (k.clone(), plain_schema(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(plain_schema).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_is_classified_by_what_the_loop_can_do_about_it() {
        assert!(matches!(classify("p", 429, "slow down", Some(Duration::from_secs(7))), LlmError::RateLimited { retry_after: Some(d) } if d.as_secs() == 7));
        assert!(matches!(classify("p", 401, "bad key", None), LlmError::Unauthorized));
        assert!(matches!(classify("p", 403, "no access", None), LlmError::Unauthorized));
        assert!(matches!(classify("p", 500, "oops", None), LlmError::Unavailable));
        assert!(matches!(classify("p", 503, "", None), LlmError::Unavailable));
        assert!(matches!(classify("p", 400, r#"{"error":{"message":"bad tool schema"}}"#, None), LlmError::BadRequest(_)));
        for body in ["maximum context length exceeded", "prompt is too long: 250000 tokens", "The input token count exceeds"] {
            assert!(matches!(classify("p", 400, body, None), LlmError::ContextTooLong), "{body}");
        }
    }

    #[test]
    fn what_a_provider_says_never_rides_along_in_the_error() {
        let e = classify("p", 401, "key sk-live-abcdef is not valid for project 12345", None);
        assert!(!e.to_string().contains("sk-live"), "{e}");
        let e = classify("p", 400, r#"{"error":"organisation org-99 is over quota"}"#, None);
        assert!(!e.to_string().contains("org-99"), "{e}");
    }

    #[test]
    fn a_long_body_is_cut_for_the_log() {
        let s = snippet(&"x".repeat(5000));
        assert!(s.chars().count() <= 401 && s.ends_with('…'));
        assert_eq!(snippet("two\n\tlines"), "two lines");
    }

    #[test]
    fn a_schema_loses_only_what_providers_refuse() {
        let schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "url": { "type": "string", "description": "where to go", "default": "x" } },
            "required": ["url"]
        });
        let plain = plain_schema(&schema);
        assert_eq!(plain["type"], "object");
        assert_eq!(plain["properties"]["url"]["description"], "where to go");
        assert_eq!(plain["required"][0], "url");
        assert!(plain.get("additionalProperties").is_none());
        assert!(plain["properties"]["url"].get("default").is_none());
    }

    #[test]
    fn a_base_url_can_be_pointed_elsewhere_and_loses_its_trailing_slash() {
        let _env = crate::llm::test_env();
        std::env::set_var("HUNTWELL_TEST_BASE", "https://proxy.test/v1/");
        assert_eq!(base_url("HUNTWELL_TEST_BASE", "https://real.test"), "https://proxy.test/v1");
        std::env::remove_var("HUNTWELL_TEST_BASE");
        assert_eq!(base_url("HUNTWELL_TEST_BASE", "https://real.test"), "https://real.test");
    }
}

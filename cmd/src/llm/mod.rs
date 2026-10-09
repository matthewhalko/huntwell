//! Talking to a model, on whichever provider a stage is set to.
//!
//! One interface ([`Provider`]), one file per provider. What differs between
//! providers is small and known — where the system prompt goes, what a tool
//! call is called, which field carries the cached-token count — so an adapter
//! is a translation of [`Request`] into that provider's shape and its reply
//! back into [`Reply`]. Everything shared (the HTTP client, retries, the
//! concurrency cap, error classification) is in [`kit`], never in an adapter.
//!
//! Three rules hold for every adapter, and [`conformance`] is the test suite
//! that proves them:
//!
//!   - **The fixed prefix is byte-identical between calls.** Providers cache on
//!     matching prefixes and charge a fraction for a hit, so the system prompt
//!     and the tool list are built once and never reordered.
//!   - **A provider's own words never reach a person.** Errors collapse to the
//!     handful of classes in [`LlmError`]; the detail goes to the log, the way
//!     `cognito::UNAVAILABLE` does for sign-in.
//!   - **Usage is normalised**, including cached reads, so the meter and the
//!     admin's cost column mean the same thing whoever answered.
//!
//! Model ids are `provider:model` — `gemini:gemini-2.5-flash`. A bare id is
//! Cursor's, which is what every existing setting holds, so nothing that was
//! configured before this module existed changes meaning.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use anyhow::Result;
use serde_json::Value;

use crate::store::TokenUsage;

pub mod anthropic;
pub mod chat;
pub mod gemini;
pub mod kit;
pub mod usage;

// The Chat Completions family: one file each, holding that provider's
// endpoint, catalogue and prices. The wire format lives in `chat`.
pub mod deepseek;
pub mod groq;
pub mod mistral;
pub mod openai;
pub mod xai;

#[cfg(test)]
pub mod conformance;

/// API keys and base URLs are process-global, and these tests set them. Any
/// test in this module that touches one takes this first, so two of them
/// cannot unset each other's key halfway through a call.
#[cfg(test)]
pub fn test_env() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// What a call looks like
// ---------------------------------------------------------------------------

/// One request to a model. Provider-neutral: the adapter shapes it.
#[derive(Debug, Clone)]
pub struct Request {
    pub model: String,
    /// The security contract and the notes that go with it. Held apart from
    /// the messages because every provider puts it somewhere of its own, and
    /// because it is the part worth caching.
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub max_output_tokens: u32,
    pub temperature: Option<f32>,
}

impl Request {
    pub fn new(model: impl Into<String>, system: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: system.into(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_output_tokens: 8192,
            temperature: None,
        }
    }
    pub fn user(mut self, text: impl Into<String>) -> Self {
        self.messages.push(Message::User(text.into()));
        self
    }
    pub fn tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = tools;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    User(String),
    /// What the model said and asked for, kept so the next turn has the
    /// conversation. `text` may be empty when it only called tools.
    Assistant { text: String, tool_calls: Vec<ToolCall> },
    /// The answer to one tool call. `is_error` is how a refusal or a failure
    /// is told to the model without ending the run.
    ToolResult { call_id: String, name: String, content: String, is_error: bool },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolCall {
    /// The provider's id for this call, echoed back on the result. Some
    /// providers do not issue one; the loop assigns a stable id in that case.
    pub id: String,
    pub name: String,
    pub args: Value,
    /// Something the provider attached to this call and expects back verbatim
    /// when the conversation continues. Opaque to us — it is not read, not
    /// logged and not shown, only carried.
    ///
    /// Gemini's thinking models put a `thoughtSignature` here and **refuse the
    /// next request without it**: "Function call is missing a
    /// thought_signature in functionCall parts". Dropping it does not degrade
    /// the answer, it ends the run.
    pub opaque: Option<String>,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, args: Value) -> Self {
        Self { id: id.into(), name: name.into(), args, opaque: None }
    }
}

/// A tool as the model is told about it: a name, what it is for, and a JSON
/// Schema for its arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone)]
pub struct Reply {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
    /// What the provider said this cost, in µUSD. `None` from providers that
    /// do not say — the admin then shows an estimate and marks it as one.
    pub cost_micros: Option<i64>,
    pub stop: Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The model is done and `text` is the answer.
    EndTurn,
    /// The model wants its tool calls run and the conversation continued.
    ToolUse,
    /// It ran out of output budget mid-answer.
    MaxTokens,
    /// It declined. Rare, and not something to retry.
    Refused,
}

// ---------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------

/// The only failure classes the loop reacts to.
///
/// An adapter maps its provider's status codes and error bodies onto these.
/// What the provider actually said is logged by the adapter, never carried in
/// the error: those messages name projects, quotas and keys.
#[derive(Debug, Clone)]
pub enum LlmError {
    /// Out of quota for now. `retry_after` is the provider's own hint.
    RateLimited { retry_after: Option<std::time::Duration> },
    /// The key is missing, wrong, or not allowed to use this model. An
    /// operator problem: retrying will not fix it.
    Unauthorized,
    /// The provider is down, timed out, or answered with something
    /// unparseable. Worth retrying. The text is *our* description of what
    /// happened ("HTTP 529 from anthropic", "anthropic timed out") — never the
    /// provider's own words, which stay in the log.
    Unavailable(String),
    /// We built a request the provider will not accept. A bug in an adapter or
    /// an unknown model id, so the detail is kept — it is ours, not theirs.
    BadRequest(String),
    /// The conversation no longer fits. The loop answers by compacting and
    /// trying once more rather than by failing.
    ContextTooLong,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::RateLimited { .. } => write!(f, "the model provider is rate-limiting this account — try again shortly"),
            LlmError::Unauthorized => write!(f, "the model provider refused our credentials — check the API key for this provider"),
            LlmError::Unavailable(why) => write!(f, "the model provider is unavailable ({why}) — try again in a few minutes"),
            LlmError::BadRequest(why) => write!(f, "the model provider rejected the request: {why}"),
            LlmError::ContextTooLong => write!(f, "the conversation grew past what this model can hold"),
        }
    }
}

impl std::error::Error for LlmError {}

impl LlmError {
    /// Whether waiting and asking again could work.
    pub fn retryable(&self) -> bool {
        matches!(self, LlmError::RateLimited { .. } | LlmError::Unavailable(_))
    }
}

// ---------------------------------------------------------------------------
// Prices and catalogue
// ---------------------------------------------------------------------------

/// What a provider charges, in US dollars per million tokens.
///
/// `cached_input` is what a prefix cache *hit* costs. It is the number that
/// decides whether an agent loop is affordable: the conversation is re-read
/// every turn, so nearly all input is a cache hit once a run is going.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub cached_input: f64,
    pub output: f64,
}

impl Price {
    pub const fn new(input: f64, cached_input: f64, output: f64) -> Self {
        Self { input, cached_input, output }
    }

    /// µUSD for one call's usage.
    ///
    /// `usage.input` is *fresh* input — [`usage`] normalises every provider to
    /// that convention, because it is the one `TokenUsage::billable` has always
    /// meant and it is what a customer is charged for. Cache reads are ours to
    /// pay and theirs to have free, so they are priced separately here and
    /// counted nowhere in the customer's bill.
    pub fn cost_micros(&self, u: TokenUsage) -> i64 {
        let dollars = u.input.max(0) as f64 * self.input
            + u.cache_read.max(0) as f64 * self.cached_input
            // A cache *write* is charged at more than input by some providers
            // (Anthropic: 1.25×). Priced at input here: the difference is a few
            // percent of one call, and pretending to a precision the price
            // table does not have would be worse than the rounding.
            + u.cache_write.max(0) as f64 * self.input
            + u.output.max(0) as f64 * self.output;
        dollars.round() as i64
    }
}

/// A model a stage can be set to.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    /// Without the provider prefix.
    pub id: String,
    pub label: String,
    /// The part of the id that decides the price — `flash-lite`, `haiku`,
    /// `nano`. Matched against whatever the provider lists, so a new version
    /// of a known family is priced without an edit here.
    pub family: String,
    pub price: Price,
    /// Total tokens the model can hold.
    pub context: u32,
    /// Worth offering for the browsing stages. A model that cannot call tools
    /// reliably is listed but not recommended.
    pub good_for_tools: bool,
}

// ---------------------------------------------------------------------------
// The interface
// ---------------------------------------------------------------------------

/// One model provider.
///
/// Implementations live one to a file. See `docs/plans/direct-providers.md`
/// §1.4 for the checklist to add another; it is half a day's work, most of it
/// filling in the price table.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// The prefix in a model id: `gemini`, `anthropic`, …
    fn id(&self) -> &'static str;

    /// Human name for the admin's Providers card.
    fn label(&self) -> &'static str;

    /// The setting that holds this provider's API key, for the operator to be
    /// told which one is missing.
    fn key_setting(&self) -> &'static str;

    /// Whether the key is set. A provider that is not configured is refused
    /// when a plan is saved, not in the middle of a run.
    fn configured(&self) -> bool {
        crate::config::get(self.key_setting()).is_some_and(|v| !v.trim().is_empty())
    }

    async fn complete(&self, req: &Request) -> Result<Reply, LlmError>;

    /// The models Huntwell suggests, used when the provider cannot be asked.
    ///
    /// A fallback, not the truth: providers retire model names, and a list
    /// written from memory goes stale — which is exactly how a run once died
    /// on "this model is no longer available". [`Provider::list_models`] is
    /// what the admin actually offers.
    fn models(&self) -> Vec<ModelInfo>;

    /// What this provider says it has, right now.
    ///
    /// The default is the built-in list, for a provider with no catalogue
    /// endpoint. An adapter that can ask, asks.
    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        Ok(self.models().into_iter().map(|m| m.id).collect())
    }

    /// The price of one model.
    ///
    /// By **family**, not by exact id: `gemini-3.6-flash` and
    /// `gemini-3.8-flash` cost the same and a table keyed on exact names is
    /// wrong the day a version lands. Exact match first, so a specifically
    /// priced model still wins.
    fn price(&self, model: &str) -> Option<Price> {
        let m = model.trim().to_ascii_lowercase();
        let listed = self.models();
        if let Some(exact) = listed.iter().find(|x| x.id.eq_ignore_ascii_case(&m)) {
            return Some(exact.price);
        }
        // The longest family fragment that appears in the id wins, so
        // "flash-lite" is not mistaken for "flash".
        listed
            .iter()
            .filter(|x| m.contains(&x.family))
            .max_by_key(|x| x.family.len())
            .map(|x| x.price)
    }
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// Built once. Adapters hold no state beyond their settings, and settings are
/// read per call, so one of each is enough for the process.
fn registry() -> &'static Vec<Box<dyn Provider>> {
    static REGISTRY: OnceLock<Vec<Box<dyn Provider>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        vec![
            Box::new(gemini::Gemini),
            Box::new(anthropic::Anthropic),
            Box::new(openai::OpenAi),
            Box::new(deepseek::DeepSeek),
            Box::new(groq::Groq),
            Box::new(mistral::Mistral),
            Box::new(xai::XAi),
        ]
    })
}

/// Every provider Huntwell can talk to, in the order the admin lists them.
pub fn providers() -> &'static [Box<dyn Provider>] {
    registry()
}

pub fn provider(id: &str) -> Option<&'static dyn Provider> {
    registry().iter().find(|p| p.id() == id).map(|p| p.as_ref())
}

/// What a stage's model setting names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Engine {
    /// `provider:model` — our own loop, this provider.
    Direct { provider: String, model: String },
    /// A bare id, or `cursor:…` — the Cursor CLI, as before.
    Cursor { model: Option<String> },
}

/// Read a stage's model setting.
///
/// A colon means the part before it names a provider, and an unknown one is an
/// error rather than a silent fall back to Cursor: a typo in a setting should
/// be visible when the plan is saved, not as a surprising bill.
pub fn parse_model_id(raw: &str) -> Result<Engine, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("auto") {
        return Ok(Engine::Cursor { model: None });
    }
    let Some((prefix, model)) = raw.split_once(':') else {
        return Ok(Engine::Cursor { model: Some(raw.to_string()) });
    };
    let prefix = prefix.trim().to_ascii_lowercase();
    let model = model.trim();
    if model.is_empty() {
        return Err(format!("'{raw}' names a provider but no model"));
    }
    if prefix == "cursor" {
        return Ok(Engine::Cursor { model: Some(model.to_string()) });
    }
    if provider(&prefix).is_some() {
        return Ok(Engine::Direct { provider: prefix, model: model.to_string() });
    }
    Err(format!(
        "'{prefix}' is not a model provider Huntwell knows — try one of: {}",
        registry().iter().map(|p| p.id()).collect::<Vec<_>>().join(", ")
    ))
}

/// The provider and model a stage setting names, ready to call.
pub fn for_model(raw: &str) -> Result<(&'static dyn Provider, String), String> {
    match parse_model_id(raw)? {
        Engine::Direct { provider: id, model } => {
            let p = provider(&id).ok_or_else(|| format!("no provider '{id}'"))?;
            if !p.configured() {
                return Err(format!("{} is not configured — set {} to use {raw}", p.label(), p.key_setting()));
            }
            Ok((p, model))
        }
        Engine::Cursor { .. } => Err(format!("'{raw}' is a Cursor model, not a direct provider")),
    }
}

/// The price of any model id, whichever engine it names. Used by the admin's
/// cost estimate, which sees ids from both worlds.
pub fn price_of(raw: &str) -> Option<Price> {
    match parse_model_id(raw).ok()? {
        Engine::Direct { provider: id, model } => provider(&id)?.price(&model),
        Engine::Cursor { model } => {
            let rate = crate::model_catalog::cursor_rate(model.as_deref().unwrap_or("auto"))?;
            // Cursor does not publish a cached-read rate; the providers behind
            // it charge about a tenth of input, which is what the admin's
            // estimate has always assumed.
            Some(Price::new(rate.input, rate.input * 0.1, rate.output))
        }
    }
}

/// Every model the Models page can offer, grouped by provider, with the
/// Cursor list last. Only configured providers appear.
pub fn catalogue() -> Vec<(String, Vec<ModelInfo>)> {
    let mut out: Vec<(String, Vec<ModelInfo>)> = Vec::new();
    for p in registry() {
        if p.configured() {
            out.push((p.label().to_string(), p.models()));
        }
    }
    out
}

/// What the admin shows on its Providers card: which are configured, and how
/// many models each offers. Never the keys.
pub fn status() -> Vec<BTreeMap<String, Value>> {
    registry()
        .iter()
        .map(|p| {
            BTreeMap::from([
                ("id".to_string(), Value::from(p.id())),
                ("label".to_string(), Value::from(p.label())),
                ("setting".to_string(), Value::from(p.key_setting())),
                ("configured".to_string(), Value::from(p.configured())),
                ("models".to_string(), Value::from(p.models().len())),
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_id_is_still_cursors() {
        assert_eq!(parse_model_id("claude-sonnet-5-thinking-high").unwrap(), Engine::Cursor { model: Some("claude-sonnet-5-thinking-high".into()) });
        assert_eq!(parse_model_id("").unwrap(), Engine::Cursor { model: None });
        assert_eq!(parse_model_id("auto").unwrap(), Engine::Cursor { model: None });
        assert_eq!(parse_model_id("cursor:composer-2.5").unwrap(), Engine::Cursor { model: Some("composer-2.5".into()) });
    }

    #[test]
    fn a_provider_prefix_picks_the_adapter() {
        assert_eq!(
            parse_model_id("gemini:gemini-2.5-flash").unwrap(),
            Engine::Direct { provider: "gemini".into(), model: "gemini-2.5-flash".into() }
        );
        // Case and spacing are the operator's, not ours.
        assert_eq!(
            parse_model_id(" Anthropic : claude-haiku-4-5 ").unwrap(),
            Engine::Direct { provider: "anthropic".into(), model: "claude-haiku-4-5".into() }
        );
    }

    #[test]
    fn a_typo_is_an_error_rather_than_a_silent_fallback() {
        let e = parse_model_id("gemni:gemini-2.5-flash").unwrap_err();
        assert!(e.contains("gemni") && e.contains("gemini"), "{e}");
        assert!(parse_model_id("gemini:").is_err(), "a provider with no model says so");
    }

    #[test]
    fn every_registered_provider_is_distinct_and_prices_what_it_lists() {
        let ids: Vec<&str> = registry().iter().map(|p| p.id()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "two providers share an id: {ids:?}");
        for p in registry() {
            assert!(!p.models().is_empty(), "{} lists no models", p.id());
            for m in p.models() {
                assert!(m.price.input > 0.0 && m.price.output > 0.0, "{}:{} has no price", p.id(), m.id);
                assert!(m.price.cached_input <= m.price.input, "{}:{} cached input costs more than fresh", p.id(), m.id);
                assert!(p.price(&m.id).is_some());
                assert!(!m.id.contains(':'), "{}:{} — a model id may not carry a provider prefix", p.id(), m.id);
            }
        }
    }

    #[test]
    fn a_new_version_of_a_known_family_is_still_priced() {
        // The failure this guards: the catalogue was written from memory,
        // `gemini-2.5-flash` had been retired, and a run died on its first
        // call. Models are now listed live, so pricing has to work for names
        // that were never typed into this codebase.
        let gemini = provider("gemini").unwrap();
        let flash = gemini.price("gemini-3.6-flash").expect("a Flash is a Flash");
        assert_eq!(gemini.price("gemini-9.9-flash"), Some(flash), "a future Flash prices as Flash");
        // And the longer family wins, so Lite is not priced as full Flash.
        let lite = gemini.price("gemini-9.9-flash-lite").expect("priced");
        assert!(lite.input < flash.input, "flash-lite {lite:?} must not be priced as flash {flash:?}");

        let anthropic = provider("anthropic").unwrap();
        let haiku = anthropic.price("claude-haiku-9-9").expect("a Haiku is a Haiku");
        let sonnet = anthropic.price("claude-sonnet-9").expect("a Sonnet is a Sonnet");
        assert!(haiku.input < sonnet.input);

        // Something from no known family has no price, rather than a wrong one.
        assert!(gemini.price("some-new-thing").is_none());
    }

    #[test]
    fn a_price_charges_cache_hits_at_the_cache_rate() {
        let p = Price::new(1.0, 0.1, 5.0);
        // 100k fresh input, 900k read from cache, 100k output:
        // 100k × $1 + 900k × $0.10 + 100k × $5 = $0.10 + $0.09 + $0.50 = $0.69
        let u = TokenUsage { input: 100_000, output: 100_000, cache_read: 900_000, cache_write: 0 };
        assert_eq!(p.cost_micros(u), 690_000);
        // And the customer is billed for the fresh input and the output only —
        // the cache read is our cost, not theirs.
        assert_eq!(u.billable(), 200_000);
    }

    #[test]
    fn an_unconfigured_provider_is_refused_by_name() {
        let _env = test_env();
        // No key is set in a test process, so this is the unconfigured path.
        let e = for_model("gemini:gemini-2.5-flash").err().expect("no key is set in a test process");
        assert!(e.contains("GEMINI_API_KEY"), "{e}");
    }
}

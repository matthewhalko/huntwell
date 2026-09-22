//! Groq, on the Chat Completions format ([`super::chat`]).
//!
//! Open-weight models on Groq's own hardware: far faster than anyone else,
//! which matters for a loop that takes a turn per page. There is no prefix
//! cache, so a cached read is priced the same as a fresh one — on a long
//! search that makes Groq dearer than its headline rate suggests.
//!
//! Prices are $/M input · cached read · output, taken from Groq's pricing
//! page and dated in `docs/plans/direct-providers.md`. They feed the admin's
//! cost estimate only — what a customer is charged is the sell rate — but a
//! stale number here makes the margin column lie, so check them when a model
//! is added.

use super::chat::{self, Spec};
use super::{LlmError, ModelInfo, Price, Provider, Reply, Request};

pub struct Groq;

const SPEC: Spec = Spec {
    id: "groq",
    key_setting: "GROQ_API_KEY",
    base_setting: "GROQ_BASE_URL",
    default_base: "https://api.groq.com/openai/v1",
    max_completion_tokens: false,
};

#[async_trait::async_trait]
impl Provider for Groq {
    fn id(&self) -> &'static str {
        SPEC.id
    }
    fn label(&self) -> &'static str {
        "Groq"
    }
    fn key_setting(&self) -> &'static str {
        SPEC.key_setting
    }
    async fn complete(&self, req: &Request) -> Result<Reply, LlmError> {
        chat::complete(&SPEC, req).await
    }
    /// What this provider says it has, so a retired name is never offered.
    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        chat::list_models(&SPEC).await
    }
    fn models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "llama-3.3-70b-versatile".into(),
                family: "70b".into(),
                label: "Llama 3.3 70B".into(),
                price: Price::new(0.59, 0.59, 0.79),
                context: 128_000,
                good_for_tools: true,
            },
            ModelInfo {
                id: "meta-llama/llama-4-scout-17b-16e-instruct".into(),
                family: "scout".into(),
                label: "Llama 4 Scout".into(),
                price: Price::new(0.11, 0.11, 0.34),
                context: 128_000,
                good_for_tools: true,
            },
            ModelInfo {
                id: "meta-llama/llama-4-maverick-17b-128e-instruct".into(),
                family: "maverick".into(),
                label: "Llama 4 Maverick".into(),
                price: Price::new(0.2, 0.2, 0.6),
                context: 128_000,
                good_for_tools: true,
            },
        ]
    }
}

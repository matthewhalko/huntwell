//! OpenAI, on the Chat Completions format ([`super::chat`]).
//!
//! The one provider that renamed the output limit: its current models
//! refuse `max_tokens` and want `max_completion_tokens`, which the spec sets.
//! Prefix caching is automatic and discounts a hit by about 90%.
//!
//! Prices are $/M input · cached read · output, taken from OpenAI's pricing
//! page and dated in `docs/plans/direct-providers.md`. They feed the admin's
//! cost estimate only — what a customer is charged is the sell rate — but a
//! stale number here makes the margin column lie, so check them when a model
//! is added.

use super::chat::{self, Spec};
use super::{LlmError, ModelInfo, Price, Provider, Reply, Request};

pub struct OpenAi;

const SPEC: Spec = Spec {
    id: "openai",
    key_setting: "OPENAI_API_KEY",
    base_setting: "OPENAI_BASE_URL",
    default_base: "https://api.openai.com/v1",
    max_completion_tokens: true,
};

#[async_trait::async_trait]
impl Provider for OpenAi {
    fn id(&self) -> &'static str {
        SPEC.id
    }
    fn label(&self) -> &'static str {
        "OpenAI"
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
                id: "gpt-5-nano".into(),
                family: "nano".into(),
                label: "GPT-5 nano".into(),
                price: Price::new(0.05, 0.005, 0.4),
                context: 400_000,
                good_for_tools: true,
            },
            ModelInfo {
                id: "gpt-5-mini".into(),
                family: "mini".into(),
                label: "GPT-5 mini".into(),
                price: Price::new(0.25, 0.025, 2.0),
                context: 400_000,
                good_for_tools: true,
            },
            ModelInfo {
                id: "gpt-5".into(),
                family: "gpt-5".into(),
                label: "GPT-5".into(),
                price: Price::new(1.25, 0.125, 10.0),
                context: 400_000,
                good_for_tools: true,
            },
        ]
    }
}

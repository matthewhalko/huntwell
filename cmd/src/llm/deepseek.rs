//! DeepSeek, on the Chat Completions format ([`super::chat`]).
//!
//! The cheapest capable option, and the one to think about before enabling:
//! DeepSeek is a Chinese company, so a page a plan reads and the brief that
//! sent it there both leave the US. Treat it as opt-in per deployment.
//!
//! Its cache reporting is its own — `prompt_cache_hit_tokens` rather than
//! `prompt_tokens_details` — which [`super::usage`] already reads.
//!
//! Prices are $/M input · cached read · output, taken from DeepSeek's pricing
//! page and dated in `docs/plans/direct-providers.md`. They feed the admin's
//! cost estimate only — what a customer is charged is the sell rate — but a
//! stale number here makes the margin column lie, so check them when a model
//! is added.

use super::chat::{self, Spec};
use super::{LlmError, ModelInfo, Price, Provider, Reply, Request};

pub struct DeepSeek;

const SPEC: Spec = Spec {
    id: "deepseek",
    key_setting: "DEEPSEEK_API_KEY",
    base_setting: "DEEPSEEK_BASE_URL",
    default_base: "https://api.deepseek.com/v1",
    max_completion_tokens: false,
};

#[async_trait::async_trait]
impl Provider for DeepSeek {
    fn id(&self) -> &'static str {
        SPEC.id
    }
    fn label(&self) -> &'static str {
        "DeepSeek"
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
                id: "deepseek-chat".into(),
                family: "chat".into(),
                label: "DeepSeek V3".into(),
                price: Price::new(0.27, 0.07, 1.1),
                context: 128_000,
                good_for_tools: true,
            },
            ModelInfo {
                id: "deepseek-reasoner".into(),
                family: "reasoner".into(),
                label: "DeepSeek R1".into(),
                price: Price::new(0.55, 0.14, 2.19),
                context: 128_000,
                good_for_tools: true,
            },
        ]
    }
}

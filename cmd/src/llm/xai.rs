//! xAI, on the Chat Completions format ([`super::chat`]).
//!
//! Long context at a low price — the Fast models hold two million tokens,
//! which is more than any page a scrape will meet.
//!
//! Prices are $/M input · cached read · output, taken from xAI's pricing
//! page and dated in `docs/plans/direct-providers.md`. They feed the admin's
//! cost estimate only — what a customer is charged is the sell rate — but a
//! stale number here makes the margin column lie, so check them when a model
//! is added.

use super::chat::{self, Spec};
use super::{LlmError, ModelInfo, Price, Provider, Reply, Request};

pub struct XAi;

const SPEC: Spec = Spec {
    id: "xai",
    key_setting: "XAI_API_KEY",
    base_setting: "XAI_BASE_URL",
    default_base: "https://api.x.ai/v1",
    max_completion_tokens: false,
};

#[async_trait::async_trait]
impl Provider for XAi {
    fn id(&self) -> &'static str {
        SPEC.id
    }
    fn label(&self) -> &'static str {
        "xAI"
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
                id: "grok-4-fast-non-reasoning".into(),
                family: "fast".into(),
                label: "Grok 4 Fast".into(),
                price: Price::new(0.2, 0.05, 0.5),
                context: 2_000_000,
                good_for_tools: true,
            },
            ModelInfo {
                id: "grok-4-fast-reasoning".into(),
                family: "fast-reasoning".into(),
                label: "Grok 4 Fast (reasoning)".into(),
                price: Price::new(0.2, 0.05, 0.5),
                context: 2_000_000,
                good_for_tools: true,
            },
            ModelInfo {
                id: "grok-4".into(),
                family: "grok-4".into(),
                label: "Grok 4".into(),
                price: Price::new(3.0, 0.75, 15.0),
                context: 256_000,
                good_for_tools: true,
            },
        ]
    }
}

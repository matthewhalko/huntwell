//! Mistral, on the Chat Completions format ([`super::chat`]).
//!
//! EU-hosted and cheap. Worth having when where the data goes is the
//! deciding question rather than the price.
//!
//! Prices are $/M input · cached read · output, taken from Mistral's pricing
//! page and dated in `docs/plans/direct-providers.md`. They feed the admin's
//! cost estimate only — what a customer is charged is the sell rate — but a
//! stale number here makes the margin column lie, so check them when a model
//! is added.

use super::chat::{self, Spec};
use super::{LlmError, ModelInfo, Price, Provider, Reply, Request};

pub struct Mistral;

const SPEC: Spec = Spec {
    id: "mistral",
    key_setting: "MISTRAL_API_KEY",
    base_setting: "MISTRAL_BASE_URL",
    default_base: "https://api.mistral.ai/v1",
    max_completion_tokens: false,
};

#[async_trait::async_trait]
impl Provider for Mistral {
    fn id(&self) -> &'static str {
        SPEC.id
    }
    fn label(&self) -> &'static str {
        "Mistral"
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
                id: "mistral-small-latest".into(),
                family: "small".into(),
                label: "Mistral Small".into(),
                price: Price::new(0.1, 0.1, 0.3),
                context: 128_000,
                good_for_tools: true,
            },
            ModelInfo {
                id: "mistral-medium-latest".into(),
                family: "medium".into(),
                label: "Mistral Medium".into(),
                price: Price::new(0.4, 0.4, 2.0),
                context: 128_000,
                good_for_tools: true,
            },
        ]
    }
}

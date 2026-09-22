//! Token counts, in one shape, whoever answered.
//!
//! Every provider reports usage differently, and the difference that matters
//! is not the field names — it is whether the prompt count *includes* the
//! tokens that were served from cache:
//!
//!   - **Anthropic** reports them apart: `input_tokens` is fresh, and
//!     `cache_read_input_tokens` is on top of it.
//!   - **OpenAI, Gemini, and the Chat Completions family** fold them in:
//!     `prompt_tokens` is the whole prompt, of which `cached_tokens` was a hit.
//!
//! Huntwell's convention is Anthropic's: [`crate::store::TokenUsage::input`]
//! is **fresh input only**. That is what `billable()` has always summed, and
//! it is what a customer pays for — a cache hit is our cost and their
//! discount. So the folded-in shapes are subtracted here, once, rather than in
//! five adapters where one could quietly forget and bill a run ten times over.

use serde_json::Value;

use crate::store::TokenUsage;

fn num(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// OpenAI's Chat Completions shape, and everything that copies it (Groq,
/// Mistral, xAI, and DeepSeek's totals).
///
/// ```text
/// "usage": { "prompt_tokens": 1200, "completion_tokens": 80,
///            "prompt_tokens_details": { "cached_tokens": 1024 } }
/// ```
pub fn chat_completions(usage: &Value) -> TokenUsage {
    let prompt = num(usage, "prompt_tokens");
    let cached = usage
        .get("prompt_tokens_details")
        .map(|d| num(d, "cached_tokens"))
        // DeepSeek puts the same number at the top level under its own name.
        .filter(|n| *n > 0)
        .unwrap_or_else(|| num(usage, "prompt_cache_hit_tokens"));
    TokenUsage {
        input: (prompt - cached).max(0),
        output: num(usage, "completion_tokens"),
        cache_read: cached,
        cache_write: 0,
    }
}

/// Anthropic's Messages shape — already the convention, so nothing is
/// subtracted.
///
/// ```text
/// "usage": { "input_tokens": 12, "output_tokens": 80,
///            "cache_read_input_tokens": 1024, "cache_creation_input_tokens": 0 }
/// ```
pub fn anthropic(usage: &Value) -> TokenUsage {
    TokenUsage {
        input: num(usage, "input_tokens"),
        output: num(usage, "output_tokens"),
        cache_read: num(usage, "cache_read_input_tokens"),
        cache_write: num(usage, "cache_creation_input_tokens"),
    }
}

/// Gemini's `usageMetadata`.
///
/// ```text
/// "usageMetadata": { "promptTokenCount": 1200, "candidatesTokenCount": 80,
///                    "cachedContentTokenCount": 1024, "thoughtsTokenCount": 40 }
/// ```
///
/// Thinking tokens are billed as output and are not in `candidatesTokenCount`,
/// so they are added: a thinking model would otherwise look free.
pub fn gemini(usage: &Value) -> TokenUsage {
    let prompt = num(usage, "promptTokenCount");
    let cached = num(usage, "cachedContentTokenCount");
    TokenUsage {
        input: (prompt - cached).max(0),
        output: num(usage, "candidatesTokenCount") + num(usage, "thoughtsTokenCount"),
        cache_read: cached,
        cache_write: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_folded_in_cache_count_is_taken_out_of_the_billed_input() {
        let u = chat_completions(&json!({
            "prompt_tokens": 1200, "completion_tokens": 80,
            "prompt_tokens_details": { "cached_tokens": 1024 }
        }));
        assert_eq!(u, TokenUsage { input: 176, output: 80, cache_read: 1024, cache_write: 0 });
        // The customer pays for what was actually new work.
        assert_eq!(u.billable(), 256);
    }

    #[test]
    fn deepseek_names_the_same_number_its_own_way() {
        let u = chat_completions(&json!({
            "prompt_tokens": 1200, "completion_tokens": 80,
            "prompt_cache_hit_tokens": 1024, "prompt_cache_miss_tokens": 176
        }));
        assert_eq!(u, TokenUsage { input: 176, output: 80, cache_read: 1024, cache_write: 0 });
    }

    #[test]
    fn anthropic_already_reports_fresh_input() {
        let u = anthropic(&json!({
            "input_tokens": 176, "output_tokens": 80,
            "cache_read_input_tokens": 1024, "cache_creation_input_tokens": 300
        }));
        assert_eq!(u, TokenUsage { input: 176, output: 80, cache_read: 1024, cache_write: 300 });
        assert_eq!(u.billable(), 256, "a cache write is our cost too, not the customer's");
    }

    #[test]
    fn gemini_counts_thinking_as_output() {
        let u = gemini(&json!({
            "promptTokenCount": 1200, "candidatesTokenCount": 80,
            "cachedContentTokenCount": 1024, "thoughtsTokenCount": 40
        }));
        assert_eq!(u, TokenUsage { input: 176, output: 120, cache_read: 1024, cache_write: 0 });
    }

    #[test]
    fn a_reply_with_no_usage_at_all_is_zero_rather_than_a_panic() {
        assert!(chat_completions(&Value::Null).is_zero());
        assert!(anthropic(&json!({})).is_zero());
        assert!(gemini(&json!({ "promptTokenCount": null })).is_zero());
    }

    #[test]
    fn a_cache_count_larger_than_the_prompt_never_goes_negative() {
        // Not expected, but a provider bug must not credit a customer tokens.
        let u = chat_completions(&json!({ "prompt_tokens": 10, "prompt_tokens_details": { "cached_tokens": 999 } }));
        assert_eq!(u.input, 0);
    }
}

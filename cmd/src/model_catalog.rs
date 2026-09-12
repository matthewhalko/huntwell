//! Models a customer can pick for a plan stage, with Cursor list prices and
//! our markup.
//!
//! The ids come from `agent --list-models` (cached), so the picker only offers
//! what this install can actually run. Prices are ours: Cursor publishes
//! input/output rates per million tokens, and we multiply by
//! [`crate::config::model_markup`]. A model we have not priced yet is still
//! listed — the UI just has nothing to put next to it.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::agent;

const LIST_TTL: Duration = Duration::from_secs(10 * 60);

/// When the CLI is missing we still need something to put in the picker, so
/// these are the ids Cursor has been listing lately. A live `--list-models`
/// replaces them the moment one succeeds.
const FALLBACK: &[(&str, &str)] = &[
    ("composer-2.5", "Composer 2.5"),
    ("composer-2.5-fast", "Composer 2.5 Fast"),
    ("cursor-grok-4.6", "Grok 4.6"),
    ("cursor-grok-4.6-high", "Grok 4.6 High"),
    ("cursor-grok-4.6-fast", "Grok 4.6 Fast"),
    ("claude-sonnet-5-thinking-high", "Claude Sonnet 5"),
    ("claude-opus-5-thinking-high", "Claude Opus 5"),
    ("gemini-3.8-flash-medium", "Gemini 3.8 Flash"),
    ("gemini-3.1-pro", "Gemini 3.1 Pro"),
    ("gpt-5.4", "GPT-5.4"),
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate {
    pub input: f64,
    pub output: f64,
}

static LISTED: Mutex<Option<(Instant, Vec<Value>)>> = Mutex::new(None);

/// Every model this install can run, newest-listed first. Empty only if the
/// CLI has never answered and we have already given the fallback.
pub fn listed_models() -> Vec<Value> {
    {
        let guard = LISTED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, rows)) = guard.as_ref() {
            if at.elapsed() < LIST_TTL && !rows.is_empty() {
                return rows.clone();
            }
        }
    }
    let live = agent::available_models();
    let rows = if live.is_empty() {
        FALLBACK
            .iter()
            .map(|(id, label)| json!({ "id": id, "label": label }))
            .collect()
    } else {
        live
    };
    if let Ok(mut guard) = LISTED.lock() {
        *guard = Some((Instant::now(), rows.clone()));
    }
    rows
}

/// Cursor's published $/M input and output for a CLI model id, if we know it.
///
/// Matching is by fragment so `claude-sonnet-5-thinking-high` and
/// `claude-sonnet-5` share a row. More specific needles are checked first.
pub fn cursor_rate(id: &str) -> Option<Rate> {
    let s = id.trim().to_ascii_lowercase().replace('_', "-");
    if s.is_empty() {
        return None;
    }
    let fast = s.contains("-fast");

    // Composer / Grok — Cursor's own pool.
    if has(&s, "composer-2.5") || has(&s, "composer-2") {
        return Some(if fast { Rate { input: 3.0, output: 15.0 } } else { Rate { input: 0.5, output: 2.5 } });
    }
    if has(&s, "grok-4.6") {
        return Some(if fast { Rate { input: 4.0, output: 12.0 } } else { Rate { input: 2.0, output: 6.0 } });
    }
    if has(&s, "grok-4.5") {
        return Some(if fast { Rate { input: 4.0, output: 18.0 } } else { Rate { input: 2.0, output: 6.0 } });
    }

    // Anthropic. Fable and the expensive Opus fast tiers before the family.
    if has(&s, "fable-5.1") || has(&s, "fable-5-1") {
        return Some(Rate { input: 10.0, output: 50.0 });
    }
    if has(&s, "fable-5") {
        return Some(Rate { input: 10.0, output: 50.0 });
    }
    if has(&s, "opus-4.7") && fast {
        return Some(Rate { input: 30.0, output: 150.0 });
    }
    if (has(&s, "opus-5") || has(&s, "opus-4.8")) && fast {
        return Some(Rate { input: 10.0, output: 50.0 });
    }
    if has(&s, "opus-") {
        return Some(Rate { input: 5.0, output: 25.0 });
    }
    if has(&s, "sonnet-5") {
        return Some(Rate { input: 2.0, output: 10.0 });
    }
    if has(&s, "sonnet-4") {
        return Some(Rate { input: 3.0, output: 15.0 });
    }
    if has(&s, "haiku-4.5") || has(&s, "haiku-4") {
        return Some(Rate { input: 1.0, output: 5.0 });
    }

    // Gemini.
    if has(&s, "gemini-3.8-flash") || has(&s, "gemini-3.7-flash") {
        return Some(Rate { input: 0.75, output: 3.5 });
    }
    if has(&s, "gemini-3.6-flash") {
        return Some(Rate { input: 1.5, output: 7.5 });
    }
    if has(&s, "gemini-3.5-flash") {
        return Some(Rate { input: 1.5, output: 9.0 });
    }
    if has(&s, "gemini-3-flash") || has(&s, "gemini-3.flash") {
        return Some(Rate { input: 0.5, output: 3.0 });
    }
    if has(&s, "gemini-3.1-pro") || has(&s, "gemini-3-pro") || has(&s, "gemini-3.pro") {
        return Some(Rate { input: 2.0, output: 12.0 });
    }
    if has(&s, "gemini-2.5-flash") {
        return Some(Rate { input: 0.3, output: 2.5 });
    }

    // OpenAI — longest / most specific first.
    if has(&s, "gpt-5.6-luna") {
        return Some(if fast { Rate { input: 0.4, output: 2.4 } } else { Rate { input: 0.2, output: 1.2 } });
    }
    if has(&s, "gpt-5.6-sol") {
        return Some(if fast { Rate { input: 8.0, output: 40.0 } } else { Rate { input: 4.0, output: 20.0 } });
    }
    if has(&s, "gpt-5.6-terra") {
        return Some(if fast { Rate { input: 4.0, output: 24.0 } } else { Rate { input: 2.0, output: 12.0 } });
    }
    if has(&s, "gpt-5.5") {
        return Some(if fast { Rate { input: 10.0, output: 60.0 } } else { Rate { input: 5.0, output: 30.0 } });
    }
    if has(&s, "gpt-5.4-nano") {
        return Some(Rate { input: 0.2, output: 1.25 });
    }
    if has(&s, "gpt-5.4-mini") {
        return Some(Rate { input: 0.75, output: 4.5 });
    }
    if has(&s, "gpt-5.4") {
        return Some(if fast { Rate { input: 5.0, output: 30.0 } } else { Rate { input: 2.5, output: 15.0 } });
    }
    if has(&s, "gpt-5.3") || has(&s, "gpt-5.2") {
        return Some(Rate { input: 1.75, output: 14.0 });
    }
    if has(&s, "gpt-5.1-codex-mini") || has(&s, "gpt-5-mini") {
        return Some(Rate { input: 0.25, output: 2.0 });
    }
    if has(&s, "gpt-5-fast") || (has(&s, "gpt-5") && fast) {
        return Some(Rate { input: 2.5, output: 20.0 });
    }
    if has(&s, "gpt-5") {
        return Some(Rate { input: 1.25, output: 10.0 });
    }

    if has(&s, "glm-5.2") || has(&s, "glm-5") {
        return Some(Rate { input: 1.4, output: 4.4 });
    }
    if has(&s, "kimi-k2.7") || has(&s, "kimi-k2") {
        return Some(Rate { input: 0.95, output: 4.0 });
    }
    if has(&s, "kimi-k3") {
        return Some(Rate { input: 3.0, output: 15.0 });
    }
    if has(&s, "muse-spark") {
        return Some(Rate { input: 1.25, output: 4.25 });
    }
    None
}

fn has(id: &str, needle: &str) -> bool {
    let a = needle.replace('.', "-");
    let b = needle.replace('-', ".");
    id.contains(needle) || id.contains(&a) || (b != needle && id.contains(&b))
}

fn sell(rate: Rate, markup: f64) -> Rate {
    Rate {
        input: rate.input * markup,
        output: rate.output * markup,
    }
}

/// What `/api/models` returns: the live list plus our sell prices.
pub fn catalog() -> Value {
    let markup = crate::config::model_markup();
    let models: Vec<Value> = listed_models()
        .into_iter()
        .map(|m| {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let label = m.get("label").and_then(|v| v.as_str()).unwrap_or(&id).to_string();
            let (cursor, priced) = match cursor_rate(&id) {
                Some(r) => (Some(r), Some(sell(r, markup))),
                None => (None, None),
            };
            json!({
                "id": id,
                "label": label,
                "cursor_input_per_m": cursor.map(|r| r.input),
                "cursor_output_per_m": cursor.map(|r| r.output),
                "sell_input_per_m": priced.map(|r| r.input),
                "sell_output_per_m": priced.map(|r| r.output),
            })
        })
        .collect();
    json!({
        "markup": markup,
        "models": models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_and_composer_have_cursor_pool_rates() {
        let g = cursor_rate("cursor-grok-4.6-high").unwrap();
        assert_eq!(g, Rate { input: 2.0, output: 6.0 });
        let gf = cursor_rate("cursor-grok-4.6-fast").unwrap();
        assert_eq!(gf, Rate { input: 4.0, output: 12.0 });
        let c = cursor_rate("composer-2.5").unwrap();
        assert_eq!(c, Rate { input: 0.5, output: 2.5 });
    }

    #[test]
    fn claude_variants_share_a_family_rate() {
        let s = cursor_rate("claude-sonnet-5-thinking-high").unwrap();
        assert_eq!(s, Rate { input: 2.0, output: 10.0 });
        let o = cursor_rate("claude-opus-5-thinking-high").unwrap();
        assert_eq!(o, Rate { input: 5.0, output: 25.0 });
        let of = cursor_rate("claude-opus-5-fast").unwrap();
        assert_eq!(of, Rate { input: 10.0, output: 50.0 });
    }

    #[test]
    fn gemini_flash_is_the_cheap_search_model() {
        let g = cursor_rate("gemini-3.8-flash-medium").unwrap();
        assert_eq!(g, Rate { input: 0.75, output: 3.5 });
    }

    #[test]
    fn junk_and_auto_have_no_rate() {
        assert!(cursor_rate("").is_none());
        assert!(cursor_rate("auto").is_none());
        assert!(cursor_rate("not-a-real-model").is_none());
    }

    #[test]
    fn markup_is_applied_to_both_sides() {
        let r = cursor_rate("composer-2.5").unwrap();
        let s = sell(r, 1.30);
        assert!((s.input - 0.65).abs() < 1e-9);
        assert!((s.output - 3.25).abs() < 1e-9);
    }
}

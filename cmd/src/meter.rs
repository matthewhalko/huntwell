//! Where a run's tokens actually went.
//!
//! [`crate::store::add_execution_tokens`] folds every agent call into one pair of
//! totals on the `Execution` row, which answers "what did this run cost" and nothing
//! else. The interesting question is which *stage* spent it: a run whose
//! enrichment costs eight times its scrape wants batching, and a run whose
//! scrape costs eight times its enrichment wants a tighter page budget. Those
//! are opposite fixes, and the `Execution` row cannot tell them apart.
//!
//! So this module keeps a per-stage tally in the run process and prints it at
//! the end, alongside the two ratios that say whether a run was efficient:
//! billable tokens per new row, and how much of what the scrape found was
//! already stored. Server-side that goes to stdout, which the runner tails into
//! `ExecutionLog`, so the breakdown is as durable as the rest of the run's output
//! without a schema change.
//!
//! One run per process (as with the browser, guard and trail state), so process
//! globals are the natural scope.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use crate::store::TokenUsage;

/// Per-stage tally. Cost is Cursor's own micro-USD, reported only on
/// usage-based billing.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stage {
    pub calls: i64,
    pub usage: TokenUsage,
    pub cost_micros: i64,
}

/// What the row stages dropped on the way to the store, summed over the run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Funnel {
    pub rows: i64,
    pub duplicate: i64,
    pub rejected: i64,
    pub stored: i64,
}

#[derive(Default)]
struct Tally {
    stages: BTreeMap<&'static str, Stage>,
    funnel: Funnel,
}

fn tally() -> &'static Mutex<Tally> {
    static T: OnceLock<Mutex<Tally>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(Tally::default()))
}

/// The stage a call label belongs to. Labels carry per-row counters
/// ("enrich 3/14"), so the leading word is the stage.
pub fn stage_of(label: &str) -> &'static str {
    let l = label.trim().to_ascii_lowercase();
    for stage in ["scrape", "enrich", "planner", "research", "find files"] {
        if l.starts_with(stage) {
            return stage;
        }
    }
    "other"
}

/// What the most recent agent call cost, for attributing a scrape's spend to
/// the angles it searched. Process-global like the rest of the tally.
static LAST_CALL: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Billable tokens of the last call booked. Read straight after a stage's call
/// to attribute that stage.
pub fn last_call_tokens() -> i64 {
    LAST_CALL.load(std::sync::atomic::Ordering::Relaxed)
}

/// Books one agent call against its stage.
pub fn record(label: &str, usage: TokenUsage, cost_micros: i64) {
    if usage.is_zero() && cost_micros == 0 {
        return;
    }
    LAST_CALL.store(usage.billable(), std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut t) = tally().lock() {
        let s = t.stages.entry(stage_of(label)).or_default();
        s.calls += 1;
        s.usage.add(usage);
        s.cost_micros += cost_micros;
    }
}

/// Books what one iteration scraped, dropped and stored. `rejected` is
/// everything discarded for a reason other than already being stored —
/// unmappable, below the plan's minimum value, no name, a refused URL.
pub fn note_rows(rows: i64, duplicate: i64, rejected: i64, stored: i64) {
    if let Ok(mut t) = tally().lock() {
        t.funnel.rows += rows;
        t.funnel.duplicate += duplicate;
        t.funnel.rejected += rejected;
        t.funnel.stored += stored;
    }
}

/// One line for the call that just finished, for the run log.
pub fn call_line(usage: TokenUsage, cost_micros: i64) -> String {
    let mut s = format!("{} in · {} out", tokens(usage.input), tokens(usage.output));
    if usage.cache_read > 0 {
        s.push_str(&format!(" · {} cached", tokens(usage.cache_read)));
    }
    if cost_micros > 0 {
        s.push_str(&format!(" · {}", money(cost_micros)));
    }
    s
}

/// The end-of-run breakdown. Empty when no call reported usage — Cursor omits
/// it on some plans, and half a table is worse than none.
pub fn summary(new_rows: i64) -> Vec<String> {
    let t = match tally().lock() {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    if t.stages.is_empty() {
        return Vec::new();
    }
    let mut out = vec!["[tokens] by stage".to_string()];
    let mut total = Stage::default();
    for (stage, s) in &t.stages {
        out.push(format!(
            "  {:<10} {:>3} call(s)  {}",
            stage,
            s.calls,
            call_line(s.usage, s.cost_micros)
        ));
        total.calls += s.calls;
        total.usage.add(s.usage);
        total.cost_micros += s.cost_micros;
    }
    out.push(format!(
        "  {:<10} {:>3} call(s)  {}",
        "total",
        total.calls,
        call_line(total.usage, total.cost_micros)
    ));

    let billable = total.usage.billable();
    if new_rows > 0 && billable > 0 {
        out.push(format!(
            "[tokens] {} billable per new row ({} new)",
            tokens(billable / new_rows.max(1)),
            new_rows
        ));
    } else if billable > 0 {
        out.push(format!("[tokens] {} billable for no new rows", tokens(billable)));
    }

    let f = t.funnel;
    if f.rows > 0 {
        out.push(format!(
            "[funnel] {} row(s) scraped · {} already stored ({}%) · {} rejected · {} stored",
            f.rows,
            f.duplicate,
            (f.duplicate * 100) / f.rows,
            f.rejected,
            f.stored
        ));
    }
    out
}

/// Tokens, at the scale a run produces them.
fn tokens(n: i64) -> String {
    let f = n as f64;
    if f >= 1e6 {
        format!("{:.1}M", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.1}k", f / 1e3)
    } else {
        format!("{n}")
    }
}

fn money(micros: i64) -> String {
    format!("${:.2}", micros as f64 / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_row_labels_fold_into_one_stage() {
        assert_eq!(stage_of("enrich 3/14"), "enrich");
        assert_eq!(stage_of("scrape"), "scrape");
        assert_eq!(stage_of("find files"), "find files");
        assert_eq!(stage_of("Planner"), "planner");
        assert_eq!(stage_of("tidy up"), "other");
    }

    #[test]
    fn a_call_line_hides_what_was_not_reported() {
        let u = TokenUsage { input: 12_345, output: 1_100, cache_read: 0, cache_write: 0 };
        assert_eq!(call_line(u, 0), "12.3k in · 1.1k out");
        let u = TokenUsage { input: 2_000_000, output: 500, cache_read: 8_000, cache_write: 0 };
        assert_eq!(call_line(u, 2_410_000), "2.0M in · 500 out · 8.0k cached · $2.41");
    }

    /// The tally is process-global and this is the only test that writes to
    /// it, so the summary it produces is deterministic.
    #[test]
    fn the_summary_says_which_stage_spent_it() {
        let scrape = TokenUsage { input: 400_000, output: 8_000, cache_read: 0, cache_write: 0 };
        record("scrape", scrape, 0);
        let one_row = TokenUsage { input: 50_000, output: 2_000, cache_read: 0, cache_write: 0 };
        record("enrich 1/2", one_row, 0);
        record("enrich 2/2", one_row, 0);
        note_rows(14, 7, 1, 6);

        let lines = summary(6);
        let text = lines.join("\n");
        assert!(text.contains("scrape       1 call(s)  400.0k in"), "{text}");
        assert!(text.contains("enrich       2 call(s)  100.0k in"), "{text}");
        assert!(text.contains("total        3 call(s)  500.0k in"), "{text}");
        // 512k billable over 6 new rows.
        assert!(text.contains("85.3k billable per new row (6 new)"), "{text}");
        assert!(text.contains("14 row(s) scraped · 7 already stored (50%)"), "{text}");
    }
}

//! The search, in short calls instead of one long one.
//!
//! An agent call re-reads its whole conversation on every step, pages
//! included, so a search that opens twenty pages pays for the first page
//! twenty times. Cost grows with the square of the pages. Three things here
//! make it grow with the pages instead:
//!
//!   - **one call per site.** When the plan names sites, each gets its own
//!     conversation, opened with only a count of what earlier sites found. The
//!     pages of one site are never re-read while working another.
//!   - **a page budget per call**, from the plan's own effort setting
//!     (`store::effort_pages`) — quick reads 5 pages a call, exhaustive 35.
//!     Effort used to decide only how many *rounds* a run did, so a thorough
//!     plan made six shallow passes over the same first page of results.
//!   - **listing pages only.** A listing page carries twenty rows; a detail
//!     page carries one, and every detail page opened is re-read on every step
//!     after it. Details are the enrich step's job, one short call per row.
//!
//! The budget is what the agent is told; the guard sees where it went, and the
//! run log says when it overran. Ending a call by force would lose the rows it
//! had found, so it is not done.

use serde_json::Value;

use crate::prospect::Ctx;

/// Sites given their own call in one round. More would be a long round; the
/// rest wait for the next iteration, where the rotation starts elsewhere.
const SITES_PER_ROUND: usize = 4;

/// The calls one search is split into: `(label, site, prompt suffix)`.
/// One entry when the plan names no sites (or the piece is off).
pub fn plan(sites: &[String], iteration: i64, pages: usize) -> Vec<Call> {
    // What the plan's effort asks for, unless an operator has overridden it.
    let per_site = crate::config::get("HUNTWELL_SCRAPE_PAGE_BUDGET")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(pages.max(2))
        .clamp(2, 100);
    if sites.len() < 2 || !super::on("rounds") {
        // One call covering everything gets the whole allowance, not one
        // site's worth of it.
        let budget = if sites.is_empty() { (per_site * 2).clamp(2, 100) } else { per_site };
        return vec![Call { label: "scrape".into(), site: None, budget }];
    }
    // Later iterations start from a different site, so a plan with more
    // sites than fit in a round works through all of them over time.
    let start = ((iteration.max(1) - 1) as usize * SITES_PER_ROUND) % sites.len();
    sites
        .iter()
        .cycle()
        .skip(start)
        .take(SITES_PER_ROUND.min(sites.len()))
        .map(|s| Call { label: format!("scrape {s}"), site: Some(s.clone()), budget: per_site })
        .collect()
}

pub struct Call {
    pub label: String,
    pub site: Option<String>,
    pub budget: usize,
}

impl Call {
    /// What this call is told about its scope, appended to the scrape prompt.
    pub fn block(&self, found_so_far: usize, sites_done: &[String], remaining: Option<i64>) -> String {
        let mut s = String::from("\n\nHOW TO SEARCH — the shape of this call:\n");
        match &self.site {
            Some(site) => {
                s.push_str(&format!("  • This call covers {site} and nothing else: its own search and listing pages, sorted so the most recent or most relevant come first.\n"));
                if !sites_done.is_empty() {
                    s.push_str(&format!("  • {} already covered in separate calls: {} — {found_so_far} row(s) found there. Do not open them.\n", if sites_done.len() == 1 { "One site was" } else { "Sites" }, sites_done.join(", ")));
                }
            }
            None => s.push_str("  • Work through the sites above in order, their own search and listing pages first.\n"),
        }
        // Written as work to get through, not as limits to avoid. The first
        // version of this block was all brakes — "at most N pages", "stop when
        // you have enough", "do NOT open detail pages" — and a fast model read
        // it exactly as written: two searches, one page, done. A budget is a
        // allowance to spend, and it has to say so.
        s.push_str(&format!(
            "  • You have {} page(s) to spend here. Use them: run several different searches, and open several \
             listing pages. One page is not a search.\n",
            self.budget
        ));
        match remaining.filter(|n| *n > 0) {
            Some(n) => s.push_str(&format!(
                "  • {n} more row(s) are wanted. Keep searching until you have them or your pages are spent — \
                 whichever comes first. Do not stop early with a short list because the first page was thin.\n"
            )),
            None => s.push_str("  • Keep searching until your pages are spent.\n"),
        }
        s.push_str("  • Take rows from LISTING pages (search results, directories, category and member pages that show many \
             items at once). Every listing entry carries a name, a link and the headline facts — that is a row. Take them \
             as you go.\n");
        s.push_str("  • Prefer listing pages to individual detail pages: a listing gives twenty rows for one page load, and a \
             later step opens each row's own page to fill in the rest. Open a detail page only when the task needs a field \
             the listing does not carry at all.\n");
        s.push_str("  • If a search returns nothing useful, try a different wording, a different engine or a different \
             directory rather than giving up — that is what the remaining pages are for.\n");
        s.push_str("  • Return the rows in the format above. An empty list is only the right answer if you spent your pages \
             and genuinely found nothing.\n");
        s
    }
}

/// Rows from one call's reply, in the shape the pipeline works with.
pub fn rows_of(v: &Value) -> Vec<Ctx> {
    v.as_array()
        .map(|a| a.iter().filter_map(|r| r.as_object()).map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sites(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("site{i}.test")).collect()
    }

    #[test]
    fn named_sites_each_get_a_call_and_later_rounds_start_elsewhere() {
        let calls = plan(&sites(6), 1, 8);
        assert_eq!(calls.iter().map(|c| c.site.clone().unwrap()).collect::<Vec<_>>(), sites(4));
        let round2: Vec<String> = plan(&sites(6), 2, 8).iter().map(|c| c.site.clone().unwrap()).collect();
        assert_eq!(round2, vec!["site5.test", "site6.test", "site1.test", "site2.test"]);
        assert!(calls.iter().all(|c| c.budget == 8 && c.label.starts_with("scrape ")));
    }

    #[test]
    fn no_sites_or_one_site_is_one_call() {
        let one = plan(&[], 1, 8);
        assert_eq!(one.len(), 1);
        assert!(one[0].site.is_none() && one[0].budget == 16, "one call for the whole web gets the whole allowance");
        let single = plan(&sites(1), 3, 8);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].budget, 8);
    }

    #[test]
    fn the_call_is_told_its_scope_budget_and_what_not_to_open() {
        let c = &plan(&sites(2), 1, 8)[1];
        let b = c.block(7, &["site1.test".into()], Some(3));
        for want in ["covers site2.test", "site1.test", "7 row(s)", "8 page(s) to spend", "3 more row(s)"] {
            assert!(b.contains(want), "{want}:\n{b}");
        }
    }

    /// The failure this guards: the block was written entirely as limits, and
    /// a fast model obeyed them — two searches, one page, an answer. A budget
    /// has to read as work to get through.
    #[test]
    fn a_call_is_told_to_use_its_budget_not_to_avoid_it() {
        let b = plan(&[], 1, 8)[0].block(0, &[], Some(10));
        let lower = b.to_lowercase();
        for want in ["use them", "several different searches", "one page is not a search", "keep searching", "do not stop early"] {
            assert!(lower.contains(want), "missing {want:?}:\n{b}");
        }
        // And the one real prohibition softened to a preference: a prospects
        // plan often needs a name that only the firm's own page carries.
        assert!(lower.contains("prefer listing pages"), "{b}");
        assert!(!lower.contains("do not open individual"), "a hard ban costs rows: {b}");
    }
}

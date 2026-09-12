//! What a plan has already searched for, and where it has already been.
//!
//! A plan's scrape prompt lists a handful of Google query templates. Left to
//! itself the model works down that list from the top on every run, opens page
//! one, and follows the same first few results — so a plan converges on the same
//! companies and stops finding anyone new long before the web runs out.
//!
//! Two halves fix that:
//!
//!   - **Capture.** Every browser navigation is already streamed past
//!     [`crate::guard`]; this module reads the same events for the search
//!     queries and page URLs behind them, and the run writes them to the plan's
//!     own record ([`crate::store::SearchQuery`] / `VisitedPage`).
//!
//!   - **Rotation.** Before a scrape, the record is turned into a short block of
//!     instructions appended to the prompt: which angles are worn out, which
//!     page of results to start on, and — chosen at random, so runs differ — an
//!     old query to re-mine deeper and a page seen once and never returned to.
//!
//! The randomness is seeded per run and printed, so a surprising run can be
//! replayed rather than guessed at.

use std::sync::Mutex;
use std::sync::OnceLock;

use serde_json::Value;

use crate::store::{SearchQueryRow, VisitedPageRow};

/// Query-string keys that carry the search terms, by engine family.
const QUERY_KEYS: [&str; 5] = ["q", "query", "p", "text", "wd"];

/// Hosts treated as search engines. A navigation to one of these is a *search*;
/// anything else is a *page*.
const SEARCH_HOSTS: [&str; 10] = [
    "google.",
    "bing.com",
    "duckduckgo.com",
    "search.brave.com",
    "ecosia.org",
    "startpage.com",
    "search.yahoo.com",
    "mojeek.com",
    "searx.",
    "baidu.com",
];

/// Per-run caps. A scrape that navigates in a loop must not be able to grow the
/// database without bound before the guard notices.
const MAX_QUERIES_PER_RUN: usize = 200;
const MAX_PAGES_PER_RUN: usize = 1_000;

/// A search seen in the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Identity: lowercased, whitespace-collapsed.
    pub key: String,
    pub query: String,
    pub engine: String,
    /// 1-based page of results.
    pub depth: i64,
}

/// A non-search page seen in the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHit {
    pub key: String,
    pub url: String,
    pub host: String,
    /// The search that was on screen before this page was opened, when there
    /// was one. Browsing order is the only evidence of what led where, and it
    /// is enough: a page opened after a search is a result of that search.
    pub from_query: Option<String>,
}

#[derive(Default)]
struct Trail {
    searches: Vec<SearchHit>,
    pages: Vec<PageHit>,
    /// The most recent search this run navigated to, which is what the next
    /// page opened will have come from.
    current_query: Option<String>,
}

static TRAIL: OnceLock<Mutex<Trail>> = OnceLock::new();

fn trail() -> &'static Mutex<Trail> {
    TRAIL.get_or_init(|| Mutex::new(Trail::default()))
}

/// Records whatever a tool call reveals about where the agent went.
///
/// Called for every tool call alongside the guard's own inspection, so a URL
/// that only shows up in a result payload (a redirect, the final page URL) is
/// caught as well as one passed as an argument.
pub fn note_call(args: &Value, result: &Value) {
    let mut urls = Vec::new();
    collect_urls(args, &mut urls);
    collect_urls(result, &mut urls);
    if urls.is_empty() {
        return;
    }
    let Ok(mut t) = trail().lock() else { return };
    for url in urls {
        match classify(&url) {
            Some(Hit::Search(s)) => {
                t.current_query = Some(s.key.clone());
                if t.searches.len() < MAX_QUERIES_PER_RUN
                    && !t.searches.iter().any(|e| e.key == s.key && e.depth == s.depth)
                {
                    t.searches.push(s);
                }
            }
            Some(Hit::Page(mut p)) => {
                p.from_query = t.current_query.clone();
                if t.pages.len() < MAX_PAGES_PER_RUN && !t.pages.iter().any(|e| e.key == p.key) {
                    t.pages.push(p);
                }
            }
            None => {}
        }
    }
}

/// Takes everything recorded so far and clears the buffer, so each iteration
/// persists only what that iteration did.
pub fn drain() -> (Vec<SearchHit>, Vec<PageHit>) {
    let Ok(mut t) = trail().lock() else {
        return (Vec::new(), Vec::new());
    };
    // The current search does not survive the drain: the next iteration starts
    // its own browsing.
    t.current_query = None;
    (std::mem::take(&mut t.searches), std::mem::take(&mut t.pages))
}

enum Hit {
    Search(SearchHit),
    Page(PageHit),
}

fn classify(url: &str) -> Option<Hit> {
    let host = host_of(url);
    if host.is_empty() {
        return None;
    }
    if is_search_host(&host) {
        let (query, depth) = search_terms(url)?;
        let key = normalize_query(&query);
        if key.is_empty() {
            return None;
        }
        return Some(Hit::Search(SearchHit {
            key,
            query,
            engine: engine_name(&host),
            depth,
        }));
    }
    Some(Hit::Page(PageHit {
        key: normalize_url(url),
        url: url.trim().to_string(),
        host,
        // Filled in by note_call, which is the only place that knows what was
        // on screen before this.
        from_query: None,
    }))
}

fn is_search_host(host: &str) -> bool {
    SEARCH_HOSTS.iter().any(|h| host_matches(h, host))
}

/// One entry of [`SEARCH_HOSTS`] against a host.
fn host_matches(pattern: &str, host: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('.') {
        // "google." matches google.com, google.com.mx, www.google.es.
        host == prefix
            || host.starts_with(&format!("{prefix}."))
            || host.contains(&format!(".{prefix}."))
    } else {
        host == pattern || host.ends_with(&format!(".{pattern}"))
    }
}

fn engine_name(host: &str) -> String {
    host.trim_start_matches("www.").to_string()
}

/// The search terms and 1-based result page from a search URL.
fn search_terms(url: &str) -> Option<(String, i64)> {
    let params = query_params(url);
    let query = QUERY_KEYS
        .iter()
        .find_map(|k| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone()))?;
    let query = query.trim().to_string();
    if query.is_empty() {
        return None;
    }
    Some((query, result_page(&params)))
}

/// Every engine paginates differently, and all of them do it with an offset
/// rather than a page number. Anything unrecognised is page one.
fn result_page(params: &[(String, String)]) -> i64 {
    let num = |key: &str| -> Option<i64> {
        params
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.parse::<i64>().ok())
    };
    // Google/Startpage: start=0,10,20. Bing: first=1,11,21. DuckDuckGo: s=0,30.
    if let Some(start) = num("start") {
        return start / 10 + 1;
    }
    if let Some(first) = num("first") {
        return (first.max(1) - 1) / 10 + 1;
    }
    if let Some(s) = num("s") {
        return s / 30 + 1;
    }
    if let Some(page) = num("page").or_else(|| num("pn")) {
        return page.max(1);
    }
    // Brave: offset=0,1,2. Ecosia: p=0,1,2. Both are 0-based page indexes.
    // `p` carries the *query* on Yahoo, where it does not parse as a number and
    // so never reaches this.
    if let Some(idx) = num("offset").or_else(|| num("p")) {
        return idx.max(0) + 1;
    }
    1
}

/// Splits a URL's query string into decoded key/value pairs.
fn query_params(url: &str) -> Vec<(String, String)> {
    let Some((_, tail)) = url.split_once('?') else {
        return Vec::new();
    };
    let tail = tail.split('#').next().unwrap_or("");
    tail.split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((k.trim().to_ascii_lowercase(), percent_decode(v)))
        })
        .collect()
}

/// `+` and `%XX` only — enough for a query string, and it cannot fail. Invalid
/// escapes are left as written rather than dropped, since the point is to
/// recognise a repeat, not to reconstruct the URL.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Identity for a query: case and spacing differences are the same search.
pub fn normalize_query(q: &str) -> String {
    q.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Identity for a page: scheme, `www.`, trailing slash and fragment dropped, so
/// the same page reached two ways counts once.
pub fn normalize_url(url: &str) -> String {
    let no_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let no_frag = no_scheme.split('#').next().unwrap_or(no_scheme);
    no_frag
        .trim_start_matches("www.")
        .trim_end_matches('/')
        .to_lowercase()
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or("");
    let host = match host.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => h,
        _ => host,
    };
    host.trim().to_ascii_lowercase()
}

fn looks_like_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Pulls every http(s) string out of a tool-call payload, wherever it sits.
fn collect_urls(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if looks_like_url(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|i| collect_urls(i, out)),
        Value::Object(map) => map.values().for_each(|c| collect_urls(c, out)),
        _ => {}
    }
}

// ---------------- Rotation ----------------

/// How many of the most-used queries a run is told to open with something other
/// than. Small on purpose: this steers the opening move, it does not ban work.
const AVOID_TOP_N: usize = 3;
/// A query is only "worn out" once it has actually been used more than once.
const WORN_OUT_AFTER: i64 = 1;
/// Ceiling on how deep a reused query is sent. Past this, results stop being
/// about the query.
const MAX_START_PAGE: i64 = 8;
const MAX_REVISIT_QUERIES: usize = 2;
const MAX_REVISIT_PAGES: usize = 3;

/// An engine a run can be pointed at.
///
/// Rotating between them is the cheapest way to break out of one index's idea
/// of the web: the same words put to Bing and to DuckDuckGo return different
/// companies, so a plan that only ever asks Google converges on Google's answer
/// long before it has found everyone.
///
/// Only engines whose result pages this module can already count are listed —
/// pagination is what [`Rotation::start_page`] rides on, and an engine whose
/// page number cannot be read would silently restart every run at page one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Engine {
    pub name: &'static str,
    /// Matched against the host recorded in the trail, like [`SEARCH_HOSTS`].
    pattern: &'static str,
    /// Prefix a query is appended to.
    search_url: &'static str,
    /// Query-string key this engine paginates with, and the arithmetic behind
    /// it: page N is `base + (N - 1) * step`.
    page_key: &'static str,
    page_base: i64,
    page_step: i64,
}

pub const ENGINES: [Engine; 5] = [
    Engine {
        name: "Google",
        pattern: "google.",
        search_url: "https://www.google.com/search?q=",
        page_key: "start",
        page_base: 0,
        page_step: 10,
    },
    Engine {
        name: "Bing",
        pattern: "bing.com",
        search_url: "https://www.bing.com/search?q=",
        page_key: "first",
        page_base: 1,
        page_step: 10,
    },
    Engine {
        name: "DuckDuckGo",
        pattern: "duckduckgo.com",
        search_url: "https://duckduckgo.com/?q=",
        page_key: "s",
        page_base: 0,
        page_step: 30,
    },
    Engine {
        name: "Brave Search",
        pattern: "search.brave.com",
        search_url: "https://search.brave.com/search?q=",
        page_key: "offset",
        page_base: 0,
        page_step: 1,
    },
    Engine {
        name: "Ecosia",
        pattern: "ecosia.org",
        search_url: "https://www.ecosia.org/search?q=",
        page_key: "p",
        page_base: 0,
        page_step: 1,
    },
];

impl Engine {
    /// Whether a host recorded in the trail is this engine.
    fn owns(&self, recorded_host: &str) -> bool {
        host_matches(self.pattern, recorded_host)
    }

    /// The URL a search on this engine starts from.
    pub fn search_url(&self) -> &'static str {
        self.search_url
    }

    /// What to add to a search URL to land on result page `page`, e.g.
    /// `&start=20` on Google or `&first=21` on Bing.
    pub fn page_query(&self, page: i64) -> String {
        let n = self.page_base + (page.max(1) - 1) * self.page_step;
        format!("&{}={}", self.page_key, n)
    }
}

/// The engine this run should use: the one this plan has leaned on least,
/// oldest first, with ties broken by the run's seed so a plan with several
/// untouched engines does not always reach for the same one.
fn pick_engine(queries: &[SearchQueryRow], rng: &mut Rng) -> Engine {
    let mut scored: Vec<(usize, i64, &str)> = ENGINES
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let used = queries.iter().filter(|q| e.owns(&q.engine));
            let hits = used.clone().map(|q| q.hits).sum();
            let last = used.map(|q| q.last_used_at.as_str()).max().unwrap_or("");
            (i, hits, last)
        })
        .collect();
    scored.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)));

    let front = (scored[0].1, scored[0].2);
    let tied: Vec<usize> = scored
        .iter()
        .filter(|s| (s.1, s.2) == front)
        .map(|s| s.0)
        .collect();
    ENGINES[tied[(rng.next() % tied.len() as u64) as usize]]
}

/// An old query worth re-mining, and where to pick it up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revisit {
    pub query: String,
    pub start_page: i64,
}

/// What this run should do differently from the last one.
#[derive(Debug, Clone, Default)]
pub struct Rotation {
    pub seed: u64,
    /// Queries used often enough that opening with them again is wasted effort.
    pub avoid: Vec<String>,
    /// Where to pick up any query that has been run before.
    pub start_page: i64,
    pub revisit_queries: Vec<Revisit>,
    pub revisit_pages: Vec<String>,
    /// Which search engine this run should put its queries to. `None` on a
    /// first run, where there is no history to rotate away from yet.
    pub engine: Option<Engine>,
    /// Nothing on record yet — this run establishes the baseline.
    pub first_run: bool,
}

/// Builds the plan for this run from its record.
///
/// `seed` makes the random picks reproducible: the run prints it, so a run that
/// went somewhere odd can be rebuilt exactly.
pub fn plan_rotation(queries: &[SearchQueryRow], pages: &[VisitedPageRow], seed: u64) -> Rotation {
    let mut rng = Rng::new(seed);
    if queries.is_empty() && pages.is_empty() {
        return Rotation { seed, first_run: true, start_page: 1, ..Rotation::default() };
    }

    let avoid: Vec<String> = queries
        .iter()
        .filter(|q| q.hits > WORN_OUT_AFTER)
        .take(AVOID_TOP_N)
        .map(|q| q.query.clone())
        .collect();

    // Start past the deepest page the *repeated* queries have reached. Those
    // are the ones a run is at risk of reusing; measuring against every query
    // would let a single one-off that someone took to page 8 push everything
    // else past the point where results still relate to the search. The extra
    // page, a third of the time, is what keeps successive runs from lining up.
    let deepest = queries
        .iter()
        .filter(|q| q.hits > WORN_OUT_AFTER)
        .map(|q| q.max_depth)
        .max()
        .or_else(|| queries.iter().map(|q| q.max_depth).max())
        .unwrap_or(0);
    let start_page = (deepest + 1 + if rng.chance(3) { 1 } else { 0 }).clamp(1, MAX_START_PAGE);

    // Re-mining favours angles that have produced something but were never
    // taken deep — the ones abandoned early, not the ones exhausted.
    let mut candidates: Vec<&SearchQueryRow> = queries
        .iter()
        .filter(|q| q.max_depth < MAX_START_PAGE)
        .collect();
    candidates.sort_by(|a, b| {
        b.new_prospects
            .cmp(&a.new_prospects)
            .then_with(|| a.max_depth.cmp(&b.max_depth))
            .then_with(|| a.last_used_at.cmp(&b.last_used_at))
    });
    let revisit_queries = rng
        .sample(&candidates, MAX_REVISIT_QUERIES)
        .into_iter()
        .map(|q| Revisit {
            query: q.query.clone(),
            start_page: (q.max_depth + 1).clamp(2, MAX_START_PAGE),
        })
        .collect();

    // Pages seen once and not since: the ones most likely to have been skimmed
    // for the first few names and abandoned. `pages` arrives oldest-first.
    let stale: Vec<&VisitedPageRow> = pages.iter().filter(|p| p.visits <= 1).collect();
    let pool = if stale.is_empty() { pages.iter().collect() } else { stale };
    let revisit_pages = rng
        .sample(&pool, MAX_REVISIT_PAGES)
        .into_iter()
        .map(|p| p.url.clone())
        .collect();

    Rotation {
        seed,
        avoid,
        start_page,
        revisit_queries,
        revisit_pages,
        engine: Some(pick_engine(queries, &mut rng)),
        first_run: false,
    }
}

impl Rotation {
    /// The block appended to the scrape prompt. Empty when there is nothing
    /// useful to say, so a first run's prompt is not padded with "no history".
    pub fn render(&self) -> String {
        if self.first_run {
            return String::new();
        }
        let mut s = String::from(
            "\nWHERE THIS PLAN HAS ALREADY BEEN — vary from it.\n\n\
             This plan has run before. Its own record of what it searched and \
             opened is below.\nRepeating the opening move repeats the results, \
             so:\n",
        );
        if !self.avoid.is_empty() {
            s.push_str(
                "\n  - Do NOT open with these. They have been run repeatedly and \
                 their first pages\n    are mined out:\n",
            );
            for q in &self.avoid {
                s.push_str(&format!("      · {q}\n"));
            }
            s.push_str(
                "    Search a different angle first: a different wording, a \
                 different segment,\n    a different city, a different language, \
                 a directory rather than a search.\n",
            );
        }
        if let Some(e) = self.engine {
            s.push_str(&format!(
                "\n  - Put this run's searches to {}, not the engine the last run \
                 used.\n    A different index returns different companies for the \
                 same words:\n      · {}<your query>\n    Directories, registries \
                 and company sites are unaffected — this is where\n    you search, \
                 not what you may open.\n",
                e.name,
                e.search_url()
            ));
        }
        let page_hint = self
            .engine
            .map(|e| format!("{}: add {}", e.name, e.page_query(self.start_page)))
            .unwrap_or_else(|| format!("Google: add &start={}", (self.start_page - 1) * 10));
        s.push_str(&format!(
            "\n  - If you do reuse a query from this plan's history, do not start \
             at page 1.\n    Start at result page {} ({page_hint} to the search \
             URL).\n",
            self.start_page,
        ));
        if !self.revisit_queries.is_empty() {
            s.push_str("\n  - Take these further than they were taken before:\n");
            for r in &self.revisit_queries {
                s.push_str(&format!(
                    "      · {} — from result page {} on\n",
                    r.query, r.start_page
                ));
            }
        }
        if !self.revisit_pages.is_empty() {
            s.push_str(
                "\n  - These pages were opened once and not returned to. If one is \
                 a directory,\n    list or article with more companies on it than \
                 were taken, mine the rest:\n",
            );
            for p in &self.revisit_pages {
                s.push_str(&format!("      · {p}\n"));
            }
        }
        // This block is appended after the task, which on most plans ends with
        // the output contract — so it says plainly that it does not touch it.
        s.push_str(
            "\nNone of this overrides the TASK. It is where to look, not what to \
             collect: every\ncompany still has to fit the plan's target profile, \
             and the JSON you return is\nexactly the shape the TASK specified.\n",
        );
        s
    }

    /// One line for the run log.
    pub fn summary(&self) -> String {
        if self.first_run {
            return "no search history yet — this run sets the baseline".into();
        }
        format!(
            "seed {} · {} · start at result page {} · {} worn-out angle(s) to \
             avoid · {} query re-mine(s) · {} page revisit(s)",
            self.seed,
            self.engine.map(|e| e.name).unwrap_or("any engine"),
            self.start_page,
            self.avoid.len(),
            self.revisit_queries.len(),
            self.revisit_pages.len()
        )
    }
}

/// xorshift64*. A dependency-free PRNG is enough here: the requirement is that
/// two runs of the same plan make different choices, not that the sequence is
/// unpredictable to anyone.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // 0 is a fixed point of xorshift; anything else is fine.
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// True one time in `n`.
    fn chance(&mut self, n: u64) -> bool {
        n > 0 && self.next() % n == 0
    }

    /// Up to `n` distinct items, biased toward the front of the list: the
    /// candidates are already in preference order, so a fair shuffle would
    /// throw that ordering away.
    fn sample<'a, T>(&mut self, items: &[&'a T], n: usize) -> Vec<&'a T> {
        if items.is_empty() || n == 0 {
            return Vec::new();
        }
        let mut picked: Vec<usize> = Vec::new();
        let window = items.len().min(n * 3).max(1);
        for _ in 0..n.min(items.len()) {
            for _ in 0..8 {
                let i = (self.next() % window as u64) as usize;
                if !picked.contains(&i) {
                    picked.push(i);
                    break;
                }
            }
        }
        picked.into_iter().map(|i| items[i]).collect()
    }
}

/// A seed for this run's rotation: the run id when the UI set one, otherwise
/// the clock. Either way it is printed, so the run can be reproduced.
pub fn run_seed(iteration: i64) -> u64 {
    let base = std::env::var("HUNTWELL_EXECUTION_ID")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(1)
        });
    base.wrapping_mul(1_000_003).wrapping_add(iteration as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn q(query: &str, hits: i64, depth: i64, yielded: i64) -> SearchQueryRow {
        SearchQueryRow {
            query_key: normalize_query(query),
            query: query.into(),
            engine: "google.com".into(),
            hits,
            max_depth: depth,
            new_prospects: yielded,
            first_used_at: "2026-08-01 00:00:00".into(),
            last_used_at: "2026-08-10 00:00:00".into(),
            tokens: 0,
        }
    }

    fn page(url: &str, visits: i64) -> VisitedPageRow {
        VisitedPageRow {
            url_key: url.into(),
            url: url.into(),
            host: host_of(url),
            visits,
            first_seen_at: "2026-08-01 00:00:00".into(),
            last_seen_at: "2026-08-02 00:00:00".into(),
        }
    }

    #[test]
    fn a_google_search_is_recorded_with_its_result_page() {
        let hit = match classify(
            "https://www.google.com/search?q=remesas+M%C3%A9xico+fintech&start=20&hl=es",
        ) {
            Some(Hit::Search(s)) => s,
            _ => panic!("a google search url must read as a search"),
        };
        assert_eq!(hit.query, "remesas México fintech");
        assert_eq!(hit.key, "remesas méxico fintech");
        assert_eq!(hit.depth, 3, "start=20 is the third page");
        assert_eq!(hit.engine, "google.com");
    }

    #[test]
    fn every_engine_reports_the_same_page_number() {
        let page_of = |url: &str| match classify(url) {
            Some(Hit::Search(s)) => s.depth,
            _ => panic!("{url} must read as a search"),
        };
        assert_eq!(page_of("https://google.com.mx/search?q=a"), 1);
        assert_eq!(page_of("https://www.bing.com/search?q=a&first=11"), 2);
        assert_eq!(page_of("https://duckduckgo.com/?q=a&s=30"), 2);
        assert_eq!(page_of("https://search.brave.com/search?q=a&offset=2&page=3"), 3);
    }

    #[test]
    fn wording_and_case_do_not_make_a_new_search() {
        let a = classify("https://google.com/search?q=Stablecoin++Mexico");
        let b = classify("https://www.google.com.mx/search?q=stablecoin+mexico&start=10");
        match (a, b) {
            (Some(Hit::Search(a)), Some(Hit::Search(b))) => {
                assert_eq!(a.key, b.key, "the same search typed twice is one row");
                assert_ne!(a.depth, b.depth, "but the pages reached are different");
            }
            _ => panic!("both must read as searches"),
        }
    }

    #[test]
    fn anything_that_is_not_a_search_engine_is_a_page() {
        let hit = match classify("https://www.contxto.com/es/fintech/?utm=x#top") {
            Some(Hit::Page(p)) => p,
            _ => panic!("a company or directory url must read as a page"),
        };
        assert_eq!(hit.host, "www.contxto.com");
        assert_eq!(hit.key, "contxto.com/es/fintech/?utm=x");
        // The same page reached with a fragment or a trailing slash is one row.
        assert_eq!(
            normalize_url("https://contxto.com/es/fintech/"),
            normalize_url("http://www.contxto.com/es/fintech#section")
        );
    }

    #[test]
    fn urls_are_found_wherever_the_tool_payload_puts_them() {
        note_call(
            &json!({"url": "https://google.com/search?q=neobanco+peru"}),
            &json!({"pages": [{"finalUrl": "https://rappi.com/about"}]}),
        );
        let (searches, pages) = drain();
        assert_eq!(searches.len(), 1);
        assert_eq!(searches[0].key, "neobanco peru");
        assert_eq!(pages.len(), 1, "the redirect target counts as a visit");
        assert_eq!(pages[0].host, "rappi.com");
        // Draining is what makes each iteration record only its own work.
        let (s, p) = drain();
        assert!(s.is_empty() && p.is_empty());
    }

    #[test]
    fn the_first_run_of_a_plan_is_told_nothing() {
        let r = plan_rotation(&[], &[], 7);
        assert!(r.first_run);
        assert!(r.render().is_empty(), "an empty history is not worth prompt space");
    }

    #[test]
    fn worn_out_angles_are_named_and_reuse_starts_past_the_deepest_page() {
        let queries = vec![
            q("remesas mexico fintech", 12, 2, 30),
            q("neobanco mexico", 8, 3, 12),
            q("psp mexico pagos", 4, 1, 3),
            q("stablecoin tesoreria", 1, 1, 0),
            // Run once, but taken to the bottom. It must not drag every other
            // reused query down to page 8 with it.
            q("one deep detour", 1, 7, 1),
        ];
        let r = plan_rotation(&queries, &[page("https://contxto.com/list", 1)], 42);

        assert_eq!(r.avoid.len(), 3, "the top three are what a run opens with");
        assert!(r.avoid.contains(&"remesas mexico fintech".to_string()));
        assert!(
            !r.avoid.contains(&"stablecoin tesoreria".to_string()),
            "an angle used once is not worn out"
        );
        assert!(
            (4..=5).contains(&r.start_page),
            "the repeated queries reached page 3, so start just past that (got {})",
            r.start_page
        );

        let text = r.render();
        assert!(text.contains("remesas mexico fintech"));
        // The jump-in point is spelled in the parameter the run's own engine
        // paginates with, not always Google's.
        let engine = r.engine.expect("a run with history is sent to an engine");
        assert!(text.contains(&engine.page_query(r.start_page)));
        assert!(text.contains("contxto.com/list"), "the stale page is offered back");
    }

    #[test]
    fn two_runs_of_the_same_plan_do_not_make_the_same_picks() {
        let queries: Vec<SearchQueryRow> = (0..12)
            .map(|i| q(&format!("angle {i}"), 3, 1, 12 - i))
            .collect();
        let pages: Vec<VisitedPageRow> = (0..12)
            .map(|i| page(&format!("https://site{i}.com/list"), 1))
            .collect();

        let a = plan_rotation(&queries, &pages, run_seed(0));
        let b = plan_rotation(&queries, &pages, run_seed(1));
        assert_ne!(
            (a.revisit_queries.clone(), a.revisit_pages.clone()),
            (b.revisit_queries.clone(), b.revisit_pages.clone()),
            "successive runs must not revisit the same things"
        );

        // But a given seed always rebuilds the same run.
        let again = plan_rotation(&queries, &pages, a.seed);
        assert_eq!(again.revisit_pages, a.revisit_pages);
        assert_eq!(again.revisit_queries, a.revisit_queries);
    }

    fn q_on(engine: &str, query: &str, hits: i64, last_used: &str) -> SearchQueryRow {
        SearchQueryRow {
            engine: engine.into(),
            last_used_at: last_used.into(),
            ..q(query, hits, 1, 1)
        }
    }

    #[test]
    fn a_plan_that_has_only_used_one_engine_is_sent_to_another() {
        let queries = vec![
            q_on("google.com", "remesas mexico", 9, "2026-08-10 00:00:00"),
            q_on("google.com.mx", "neobanco mexico", 4, "2026-08-11 00:00:00"),
        ];
        // Whatever the seed, the answer is never the engine it has worn out.
        for seed in 0..25 {
            let e = plan_rotation(&queries, &[], seed)
                .engine
                .expect("a run with history is sent to an engine");
            assert_ne!(e.name, "Google", "seed {seed} sent it back to Google");
        }
    }

    #[test]
    fn engines_come_up_in_turn_rather_than_the_same_one_twice() {
        // Four of the five used once; the fifth never. The untouched engine is
        // the least-used, so that is where the next run goes — and once it has
        // been used too, the rotation moves on rather than repeating it.
        let mut queries = vec![
            q_on("google.com", "a", 1, "2026-08-01 00:00:00"),
            q_on("bing.com", "b", 1, "2026-08-02 00:00:00"),
            q_on("duckduckgo.com", "c", 1, "2026-08-03 00:00:00"),
            q_on("search.brave.com", "d", 1, "2026-08-04 00:00:00"),
        ];
        let first = plan_rotation(&queries, &[], 11).engine.unwrap();
        assert_eq!(first.name, "Ecosia", "the untouched engine is the least used");

        queries.push(q_on("ecosia.org", "e", 1, "2026-08-05 00:00:00"));
        let second = plan_rotation(&queries, &[], 12).engine.unwrap();
        assert_ne!(
            second.name, "Ecosia",
            "the engine just used is the most recent, so it goes to the back"
        );
        assert_eq!(
            second.name, "Google",
            "with every engine used once, the oldest comes up again"
        );
    }

    #[test]
    fn each_engine_is_paginated_the_way_it_actually_paginates() {
        // Page 3 on each engine, put back through the parser that reads the
        // trail: what the prompt tells the agent to type has to be what this
        // module later counts as page 3.
        for e in ENGINES {
            let url = format!("{}widgets{}", e.search_url(), e.page_query(3));
            match classify(&url) {
                Some(Hit::Search(s)) => {
                    assert_eq!(s.depth, 3, "{} paginates wrong: {url}", e.name);
                    assert_eq!(s.query, "widgets", "{} lost the query: {url}", e.name);
                    assert!(e.owns(&s.engine), "{} did not recognise {}", e.name, s.engine);
                }
                _ => panic!("{} produced a url that is not a search: {url}", e.name),
            }
            assert_eq!(e.page_query(1), format!("&{}={}", e.page_key, e.page_base));
        }
    }

    #[test]
    fn re_mining_prefers_angles_that_paid_and_were_left_shallow() {
        let queries = vec![
            q("exhausted but rich", 9, MAX_START_PAGE + 2, 40),
            q("shallow and rich", 2, 1, 25),
            q("shallow and empty", 2, 1, 0),
        ];
        let r = plan_rotation(&queries, &[], 3);
        let picked: Vec<&str> = r.revisit_queries.iter().map(|x| x.query.as_str()).collect();
        assert!(
            !picked.contains(&"exhausted but rich"),
            "a query already taken to the bottom has nothing left"
        );
        assert!(picked.contains(&"shallow and rich"));
        for r in &r.revisit_queries {
            assert!(r.start_page >= 2, "re-mining never restarts at page 1");
        }
    }
}

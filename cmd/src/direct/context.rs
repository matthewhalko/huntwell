//! Keeping the conversation from growing faster than the work.
//!
//! This is the reason the direct loop exists. An agent call re-reads its whole
//! conversation on every turn, and a page snapshot is tens of kilobytes, so
//! with the Cursor CLI — which keeps every page forever — step *n* pays for
//! pages 1..n-1 again. Cost grows with the square of the pages read.
//!
//! Here a page is kept verbatim only while it is one of the last [`KEEP`] tool
//! results. Older ones become a line saying which page it was and that it has
//! been read. The model is told this rule in its instructions, so it takes the
//! rows off a page while it is looking at it rather than expecting to come
//! back — which is what a scrape should do anyway.
//!
//! Nothing is ever dropped: the shape of the conversation is unchanged, so
//! every tool call still has its result and no provider refuses it. Only the
//! bulk goes.

use serde_json::Value;

use crate::llm::Message;

/// Page snapshots kept in full. Two: the one the model is working from, and
/// the one before it, so it can compare a listing page with the page it just
/// came back from.
const KEEP: usize = 2;

/// Shorter than this and replacing it saves nothing worth the confusion.
const WORTH_STUBBING: usize = 1500;

/// Replace the body of every page snapshot but the last [`KEEP`].
///
/// Returns how many bytes went, for the run log.
pub fn compact(messages: &mut [Message]) -> usize {
    let mut pages: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, Message::ToolResult { content, .. } if is_page(content)))
        .map(|(i, _)| i)
        .collect();
    if pages.len() <= KEEP {
        return 0;
    }
    pages.truncate(pages.len() - KEEP);
    let mut saved = 0;
    for i in pages {
        let Message::ToolResult { content, .. } = &mut messages[i] else { continue };
        if content.len() < WORTH_STUBBING {
            continue;
        }
        let stub = stub_for(content);
        saved += content.len() - stub.len();
        *content = stub;
    }
    saved
}

/// Whether a tool result is a page — the shape [`super::tools`] returns, and
/// already stubbed results are not stubbed twice.
fn is_page(content: &str) -> bool {
    content.starts_with("### Page\n") && content.contains("### Snapshot")
}

fn stub_for(content: &str) -> String {
    let url = content
        .lines()
        .find_map(|l| l.trim().strip_prefix("- Page URL: "))
        .unwrap_or("a page")
        .to_string();
    let title = content.lines().find_map(|l| l.trim().strip_prefix("- Page Title: ")).unwrap_or_default();
    format!(
        "[This page was read earlier and its text is no longer in the conversation.\n\
         URL: {url}\n\
         Title: {title}\n\
         Anything you needed from it should already be in the rows you are collecting. \
         Open it again only if you truly need it.]"
    )
}

// ---------------------------------------------------------------------------
// Ground covered
// ---------------------------------------------------------------------------

/// Where this call has already been, in a line each.
///
/// Compaction takes the *pages* away, and without this it takes the memory of
/// them too: the model re-runs a search it already ran, re-opens a directory it
/// already read, and a round ends having covered two pages three times. The
/// pages are tens of thousands of tokens; knowing you have seen them is fifty.
///
/// Kept as one message that is rewritten each turn rather than appended to, so
/// the record of the run does not itself become the thing that fills the
/// conversation.
#[derive(Default, Clone)]
pub struct Ground {
    searches: Vec<String>,
    pages: Vec<(String, String)>,
    /// How many of the above came from earlier rounds rather than this call.
    /// Only for the wording: "already covered in this call" is wrong when the
    /// ground was covered yesterday.
    inherited: usize,
}

/// How much ground to name before summarising the rest. Enough to steer away
/// from repeats, small enough to be free.
const MOST: usize = 40;

impl Ground {
    /// Note one browser call and what came back.
    pub fn note(&mut self, tool: &str, args: &Value, content: &str) {
        if tool != "browser_navigate" {
            return;
        }
        let url = args.get("url").and_then(Value::as_str).unwrap_or_default().trim();
        if url.is_empty() {
            return;
        }
        if let Some(terms) = search_terms(url) {
            if !self.searches.iter().any(|s| s == &terms) {
                self.searches.push(terms);
            }
            return;
        }
        let title = content
            .lines()
            .find_map(|l| l.trim().strip_prefix("- Page Title: "))
            .unwrap_or_default()
            .chars()
            .take(70)
            .collect::<String>();
        let short = super::super::thrift::page::link_key(url);
        if !self.pages.iter().any(|(u, _)| u == &short) {
            self.pages.push((short, title));
        }
    }

    /// Start from what this plan has already searched and opened, in earlier
    /// rounds and earlier runs.
    ///
    /// Without this a round only knows its own call: round two re-runs round
    /// one's searches and re-reads its pages, and a six-round plan covers two
    /// pages six times. The trail has held this all along — it was simply
    /// never put in front of the model as "you have been here".
    pub fn from_history(searches: Vec<String>, pages: Vec<(String, String)>) -> Self {
        let mut g = Ground::default();
        for q in searches {
            let q = q.trim().to_string();
            if !q.is_empty() && !g.searches.contains(&q) {
                g.searches.push(q);
            }
        }
        for (url, title) in pages {
            let key = super::super::thrift::page::link_key(&url);
            if !g.pages.iter().any(|(u, _)| u == &key) {
                g.pages.push((key, title.chars().take(70).collect()));
            }
        }
        g.inherited = g.searches.len() + g.pages.len();
        g
    }

    pub fn is_empty(&self) -> bool {
        self.searches.is_empty() && self.pages.is_empty()
    }

    /// The block the model is shown. Written as something to build on, not as
    /// a list of things it is forbidden to revisit.
    pub fn block(&self) -> String {
        let mut s = String::from(
            "GROUND ALREADY COVERED — this plan's earlier rounds have searched and read the following. \
             You do not need to repeat them:\n",
        );
        if !self.searches.is_empty() {
            s.push_str("  Searches run:\n");
            for q in self.searches.iter().take(MOST) {
                s.push_str(&format!("    · {q}\n"));
            }
            if self.searches.len() > MOST {
                s.push_str(&format!("    · …and {} more\n", self.searches.len() - MOST));
            }
        }
        if !self.pages.is_empty() {
            s.push_str("  Pages opened:\n");
            for (url, title) in self.pages.iter().take(MOST) {
                if title.is_empty() {
                    s.push_str(&format!("    · {url}\n"));
                } else {
                    s.push_str(&format!("    · {url} — {title}\n"));
                }
            }
            if self.pages.len() > MOST {
                s.push_str(&format!("    · …and {} more\n", self.pages.len() - MOST));
            }
        }
        // Phrased as a head start, not a prohibition. The first version was a
        // list of don'ts and a later round read it as "there is nothing left",
        // answered in under a second and opened no page at all.
        s.push_str(
            "That ground is covered, so this round starts from further on — there is plenty left. Search for the same \
             thing in different words, on a different engine, in a different directory, in a neighbouring town, or on \
             page two and three of results. You are expected to come back with rows.\n",
        );
        s
    }
}

/// The terms behind a search URL, or `None` if it is an ordinary page.
fn search_terms(url: &str) -> Option<String> {
    let host = super::super::thrift::page::host_of(url)?;
    const ENGINES: [&str; 8] = ["google.", "bing.com", "duckduckgo.com", "search.brave.com", "ecosia.org", "startpage.com", "search.yahoo.com", "mojeek.com"];
    if !ENGINES.iter().any(|e| host.starts_with(e) || host.contains(e)) {
        return None;
    }
    let query = url.split_once('?')?.1;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if matches!(k, "q" | "query" | "p" | "text") && !v.is_empty() {
            let decoded = v.replace('+', " ");
            // Percent-decoding, enough for a line the model reads.
            let bytes = decoded.as_bytes();
            let mut out = String::with_capacity(decoded.len());
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'%' && i + 2 < bytes.len() {
                    if let Ok(b) = u8::from_str_radix(&decoded[i + 1..i + 3], 16) {
                        out.push(b as char);
                        i += 3;
                        continue;
                    }
                }
                out.push(bytes[i] as char);
                i += 1;
            }
            return Some(out.chars().take(120).collect());
        }
    }
    None
}

/// Put the plan's inherited trail in once, at the start.
///
/// Once — never rewritten. Everything a provider caches depends on the
/// conversation being **append-only**: a message removed or edited in the
/// middle shifts everything after it, the prefix stops matching, and the whole
/// conversation is re-read as *fresh* input on every turn. That is billed to
/// the customer and it is what made a run reach a million tokens in minutes.
///
/// What this call itself covers is added by [`Ground::block`] onto the newest
/// tool result, which is appended anyway — see `direct::run`.
pub fn seed_note(messages: &mut Vec<Message>, ground: &Ground) {
    if !ground.is_empty() {
        messages.push(Message::User(ground.block()));
    }
}

/// What the model is told about the rule, appended to its instructions. Says
/// it plainly, because a model that expects to scroll back will collect
/// nothing and then go looking.
pub const NOTE: &str = "\n\nHOW THIS CONVERSATION WORKS — read this carefully:\n\
    Only the last two pages you opened stay readable. An older page is replaced by a short note \
    giving its URL. So take every row you want off a page WHILE YOU ARE LOOKING AT IT, and keep \
    them in your reply as you go. Do not plan to come back to a page later.\n";

#[cfg(test)]
mod tests {
    use super::*;

    fn page(url: &str, size: usize) -> Message {
        Message::ToolResult {
            call_id: url.into(),
            name: "browser_navigate".into(),
            content: format!("### Page\n- Page URL: {url}\n- Page Title: T\n### Snapshot\n{}", "- link \"x\"\n".repeat(size)),
            is_error: false,
        }
    }
    fn body(m: &Message) -> &str {
        match m {
            Message::ToolResult { content, .. } => content,
            _ => "",
        }
    }

    #[test]
    fn only_the_last_two_pages_keep_their_text() {
        let mut msgs = vec![Message::User("find cars".into())];
        for host in ["a", "b", "c", "d"] {
            msgs.push(page(&format!("https://{host}.test"), 400));
        }
        let saved = compact(&mut msgs);
        assert!(saved > 0);
        // The two oldest are stubs that still say where they were.
        assert!(body(&msgs[1]).contains("read earlier") && body(&msgs[1]).contains("https://a.test"));
        assert!(body(&msgs[2]).contains("https://b.test"));
        assert!(body(&msgs[1]).len() < 400 && body(&msgs[2]).len() < 400, "a stub is a line, not a page");
        // The two newest are untouched.
        assert!(body(&msgs[3]).contains("### Snapshot"));
        assert!(body(&msgs[4]).contains("### Snapshot"));
        // The conversation's shape is unchanged: every result is still there.
        assert_eq!(msgs.len(), 5);
    }

    /// The property that matters: what a conversation costs stops growing with
    /// the pages read. Two pages of text, however many pages were opened.
    #[test]
    fn a_long_search_costs_what_a_short_one_does() {
        let size = |pages: usize| {
            let mut msgs = vec![Message::User("find cars".into())];
            let mut total = 0;
            for i in 0..pages {
                msgs.push(page(&format!("https://p{i}.test"), 400));
                compact(&mut msgs);
                // Every turn re-reads everything: that sum is the real bill.
                total += msgs.iter().map(|m| body(m).len()).sum::<usize>();
            }
            total
        };
        let (five, twenty) = (size(5), size(20));
        // Four times the pages, and nothing like four times the cost — the
        // growth is linear in the pages rather than in their square.
        assert!(twenty < five * 6, "5 pages: {five}, 20 pages: {twenty}");
        // Without compaction 20 pages would be about 16× the cost of 5.
        assert!(twenty > five * 3, "sanity: it should still grow: {five} -> {twenty}");
    }

    #[test]
    fn compacting_twice_changes_nothing_the_second_time() {
        let mut msgs = vec![page("https://a.test", 400), page("https://b.test", 400), page("https://c.test", 400)];
        assert!(compact(&mut msgs) > 0);
        assert_eq!(compact(&mut msgs), 0, "a stub is not a page and is not stubbed again");
    }

    #[test]
    fn what_is_not_a_page_is_left_alone() {
        // Plan-memory answers and refusals are short and worth keeping whole.
        let mut msgs = vec![
            Message::ToolResult { call_id: "1".into(), name: "prospect_known".into(), content: "{\"known\":[]}".into(), is_error: false },
            Message::ToolResult { call_id: "2".into(), name: "browser_navigate".into(), content: "refused: host x".into(), is_error: true },
            page("https://a.test", 400),
            page("https://b.test", 400),
            page("https://c.test", 400),
        ];
        compact(&mut msgs);
        assert_eq!(body(&msgs[0]), "{\"known\":[]}");
        assert_eq!(body(&msgs[1]), "refused: host x");
    }

    #[test]
    fn a_short_page_is_not_worth_replacing() {
        let mut msgs = vec![page("https://a.test", 1), page("https://b.test", 400), page("https://c.test", 400)];
        assert_eq!(compact(&mut msgs), 0);
        assert!(body(&msgs[0]).contains("### Snapshot"));
    }

    #[test]
    fn where_the_call_has_been_survives_the_pages_going_away() {
        let mut g = Ground::default();
        let page = |title: &str| format!("### Page\n- Page URL: x\n- Page Title: {title}\n### Snapshot\n- list");
        g.note("browser_navigate", &serde_json::json!({ "url": "https://www.ecosia.org/search?q=fee-only+advisor+Reno" }), "");
        g.note("browser_navigate", &serde_json::json!({ "url": "https://duckduckgo.com/?q=RIA%20Sparks%20Nevada" }), "");
        // The same search twice is one line, not two.
        g.note("browser_navigate", &serde_json::json!({ "url": "https://www.ecosia.org/search?q=fee-only+advisor+Reno" }), "");
        g.note("browser_navigate", &serde_json::json!({ "url": "https://napfa.org/find-an-advisor" }), &page("Find an Advisor"));
        g.note("browser_navigate", &serde_json::json!({ "url": "https://napfa.org/find-an-advisor?utm_source=x" }), &page("Find an Advisor"));

        let b = g.block();
        assert!(b.contains("fee-only advisor Reno"), "a search is remembered by its words:\n{b}");
        assert!(b.contains("RIA Sparks Nevada"), "percent-encoding and all:\n{b}");
        assert_eq!(b.matches("fee-only advisor Reno").count(), 1, "and only once");
        assert!(b.contains("napfa.org/find-an-advisor — Find an Advisor"), "{b}");
        assert_eq!(b.matches("napfa.org/find-an-advisor").count(), 1, "tracking parameters are not a new page");
        assert!(b.to_lowercase().contains("there is plenty left"), "it must point forward, not just forbid:\n{b}");
        assert!(!b.to_lowercase().contains("do not run these"), "a list of don'ts reads as 'nothing left':\n{b}");
        // Fifty tokens, not fifty thousand.
        assert!(b.len() < 700, "the record must not become the thing that fills the conversation: {}", b.len());
    }

    /// The gap this closes: `Ground` knew only its own call, so round two
    /// re-ran round one's searches and re-read its pages. The trail has held
    /// this all along; it was never put in front of the model.
    #[test]
    fn a_later_round_starts_knowing_what_earlier_rounds_covered() {
        let mut g = Ground::from_history(
            vec!["fee-only advisor Reno".into(), "  ".into(), "fee-only advisor Reno".into()],
            vec![
                ("https://www.napfa.org/find-an-advisor?utm_source=x".into(), "Find an Advisor".into()),
                ("https://napfa.org/find-an-advisor/".into(), "Find an Advisor".into()),
            ],
        );
        let b = g.block();
        assert!(b.contains("earlier rounds"), "{b}");
        assert_eq!(b.matches("fee-only advisor Reno").count(), 1, "duplicates and blanks collapse");
        assert_eq!(b.matches("napfa.org/find-an-advisor").count(), 1, "one page, whatever the tracking parameters");

        // And this call's own work joins it.
        g.note("browser_navigate", &serde_json::json!({ "url": "https://www.bing.com/search?q=RIA+Sparks" }), "");
        let b = g.block();
        assert!(b.contains("RIA Sparks") && b.contains("fee-only advisor Reno"));

        // A first round inherits nothing, so there is no note at all.
        assert!(Ground::default().is_empty());
    }

    /// The bug this guards, which cost a real run a million tokens in minutes:
    /// the note used to be removed from the middle of the conversation and
    /// re-appended each turn. Providers cache on a matching **prefix**, so
    /// shifting the middle makes every turn fresh input — and fresh input is
    /// what the customer is billed for.
    ///
    /// Everything before the newest turn must therefore be byte-identical from
    /// one request to the next.
    #[test]
    fn the_conversation_only_ever_grows_at_the_end() {
        let mut g = Ground::from_history(vec!["advisors reno".into()], vec![]);
        let mut msgs = vec![Message::User("find advisors".into())];
        seed_note(&mut msgs, &g);
        let mut snapshots: Vec<Vec<Message>> = vec![msgs.clone()];

        // Four turns of a page each, the way `direct::run` builds them.
        for i in 0..4 {
            g.note("browser_navigate", &serde_json::json!({ "url": format!("https://site{i}.test/a") }), "");
            let mut results = vec![Message::ToolResult {
                call_id: format!("c{i}"),
                name: "browser_navigate".into(),
                content: format!("### Page\n- Page URL: https://site{i}.test/a\n- Page Title: T\n### Snapshot\n{}", "- link\n".repeat(400)),
                is_error: false,
            }];
            if let Some(Message::ToolResult { content, .. }) = results.last_mut() {
                content.push_str("\n\n");
                content.push_str(&g.block());
            }
            msgs.push(Message::Assistant { text: String::new(), tool_calls: vec![] });
            msgs.extend(results);
            compact(&mut msgs);
            snapshots.push(msgs.clone());
        }

        // Compaction rewrites an ageing page, so the guarantee is: nothing
        // changes except at or after the message compaction touched — never
        // the opening, which is the expensive part to re-read.
        for pair in snapshots.windows(2) {
            let (before, after) = (&pair[0], &pair[1]);
            assert!(after.len() > before.len(), "a turn only adds");
            // The task and the inherited trail are the cacheable head and must
            // never move or change.
            assert_eq!(before[0], after[0], "the task moved");
            assert_eq!(before[1], after[1], "the inherited trail moved — the whole prefix stops matching");
        }

        // And the current ground is still in front of the model, on the newest
        // result rather than in a message that shuffles.
        match msgs.last().unwrap() {
            Message::ToolResult { content, .. } => {
                assert!(content.contains("site3.test"), "the newest page");
                assert!(content.contains("GROUND ALREADY COVERED") && content.contains("site0.test"), "and everything before it");
            }
            other => panic!("the newest message should be the result: {other:?}"),
        }
        // One copy of the note, not one per turn.
        let notes = msgs.iter().filter(|m| matches!(m, Message::User(t) if t.starts_with("GROUND ALREADY COVERED"))).count();
        assert_eq!(notes, 1);
    }

    #[test]
    fn an_ordinary_page_is_not_mistaken_for_a_search() {
        assert_eq!(search_terms("https://www.ecosia.org/search?q=advisors+reno").as_deref(), Some("advisors reno"));
        assert_eq!(search_terms("https://napfa.org/find-an-advisor?q=x"), None, "a directory's own filter is not a web search");
        assert_eq!(search_terms("https://gbfinancial.org/about-us"), None);
    }

    #[test]
    fn the_model_is_told_the_rule_it_has_to_work_under() {
        assert!(NOTE.contains("last two pages"));
        assert!(NOTE.to_lowercase().contains("while you are looking at it"));
    }
}

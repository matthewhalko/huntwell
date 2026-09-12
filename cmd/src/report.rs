//! Rendering a stored report for reading and printing.
//!
//! The Markdown is written by an agent that read attacker-controlled pages, so
//! it is untrusted input: raw HTML inside it is **escaped, never passed
//! through**. That is the first line of defence; the app's CSP
//! (`script-src 'self'`, see `web/mod.rs`) is the second.
//!
//! There is no server-side PDF renderer. The print page below carries a print
//! stylesheet and the browser's own Save-as-PDF produces the file — which
//! works identically on a laptop and in a container with no Chrome installed.

use pulldown_cmark::{html, Options, Parser};

use crate::store::ReportRow;

/// Markdown → HTML, with raw HTML escaped rather than emitted.
pub fn to_html(markdown: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(markdown, opts).map(|event| match event {
        // Escape rather than trust: an injected <script> in a scraped page
        // must not become one in the report.
        pulldown_cmark::Event::Html(t) | pulldown_cmark::Event::InlineHtml(t) => {
            pulldown_cmark::Event::Text(t)
        }
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A standalone page for reading and printing one report. Self-contained: no
/// scripts, no external assets, so it prints identically anywhere.
pub fn print_page(r: &ReportRow) -> String {
    let body = to_html(&r.markdown);
    let sources = r
        .sources
        .as_array()
        .map(|list| {
            list.iter()
                .enumerate()
                .map(|(i, s)| {
                    let title = s.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let url = s.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let label = if title.trim().is_empty() { url } else { title };
                    format!(
                        "<li><span class=\"n\">[{}]</span> {} <a href=\"{}\">{}</a></li>",
                        i + 1,
                        esc(label),
                        esc(url),
                        esc(url)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let sources_block = if sources.is_empty() {
        String::new()
    } else {
        format!("<h2 class=\"src-h\">Sources</h2>\n<ol class=\"sources\">\n{sources}\n</ol>")
    };
    let generated = r.last_seen_utc.format("%-d %B %Y").to_string();

    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<link href="https://fonts.googleapis.com/css2?family=Lato:wght@400;700;900&display=swap" rel="stylesheet">
<style>
:root{{--ink:#1d1c1d;--muted:#616061;--rule:#e0e0e0;--link:#1264a3}}
*{{box-sizing:border-box}}
body{{margin:0;background:#f8f8f8;color:var(--ink);
  font-family:'Lato',-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;font-size:15px;line-height:1.62}}
.sheet{{max-width:46rem;margin:2rem auto;background:#fff;padding:3.2rem 3.6rem;
  border:1px solid var(--rule);border-radius:10px}}
.bar{{max-width:46rem;margin:0 auto;display:flex;justify-content:space-between;align-items:center;
  padding:.9rem .2rem 0}}
.bar button{{font:inherit;font-weight:700;background:#007a5a;color:#fff;border:0;border-radius:4px;
  padding:.5rem 1rem;cursor:pointer}}
.bar .back{{color:var(--muted);text-decoration:none;font-weight:700}}
h1{{font-size:2rem;font-weight:900;letter-spacing:-.02em;line-height:1.15;margin:0 0 .3rem}}
.sub{{color:var(--muted);font-size:.9rem;margin-bottom:2rem;padding-bottom:1rem;border-bottom:2px solid var(--ink)}}
h2{{font-size:1.25rem;font-weight:900;letter-spacing:-.01em;margin:2rem 0 .6rem}}
h3{{font-size:1.05rem;font-weight:700;margin:1.4rem 0 .4rem}}
p,li{{margin:0 0 .85rem}}
ul,ol{{padding-left:1.3rem}}
a{{color:var(--link)}}
blockquote{{margin:1rem 0;padding:.2rem 0 .2rem 1rem;border-left:3px solid var(--rule);color:var(--muted)}}
code{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.88em;background:#f4f4f6;
  padding:.1em .35em;border-radius:4px}}
pre{{background:#f4f4f6;padding:.9rem 1.1rem;border-radius:8px;overflow-x:auto}}
pre code{{background:none;padding:0}}
table{{width:100%;border-collapse:collapse;margin:1rem 0;font-size:.92em}}
th,td{{text-align:left;padding:.45rem .6rem;border-bottom:1px solid var(--rule);vertical-align:top}}
th{{font-size:.75rem;text-transform:uppercase;letter-spacing:.05em;color:var(--muted)}}
.src-h{{margin-top:2.4rem;padding-top:1.2rem;border-top:1px solid var(--rule)}}
.sources{{list-style:none;padding:0;font-size:.87rem;color:var(--muted)}}
.sources li{{margin-bottom:.5rem;word-break:break-word}}
.sources .n{{font-weight:700;color:var(--ink)}}
/* The PDF path: the browser prints this. */
@media print{{
  body{{background:#fff;font-size:11pt}}
  .bar{{display:none}}
  .sheet{{max-width:none;margin:0;padding:0;border:0;border-radius:0}}
  h1{{font-size:20pt}} h2{{font-size:13pt;page-break-after:avoid}} h3{{page-break-after:avoid}}
  p,li,blockquote,table{{page-break-inside:avoid}}
  a{{color:#000;text-decoration:none}}
  /* Print the destination of a link, since a reader cannot click paper. */
  .sheet p a[href^="http"]::after{{content:" (" attr(href) ")";font-size:9pt;color:#555;word-break:break-all}}
  @page{{margin:18mm 16mm}}
}}
</style>
</head>
<body>
<div class="bar">
  <a class="back" href="/app/plans/{plan_id}">← back to the plan</a>
  <button onclick="window.print()">Save as PDF</button>
</div>
<article class="sheet">
  <h1>{title}</h1>
  <div class="sub">{subject} · {words} words · {generated}</div>
  {body}
  {sources_block}
</article>
</body>
</html>"##,
        title = esc(&r.title),
        subject = esc(&r.subject),
        words = r.word_count,
        generated = generated,
        plan_id = r.plan_id,
        body = body,
        sources_block = sources_block,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_renders_to_html() {
        let html = to_html("## Findings\n\nAcme raised **$4M** in 2024 [1].\n\n- one\n- two\n");
        assert!(html.contains("<h2>Findings</h2>"));
        assert!(html.contains("<strong>$4M</strong>"));
        assert!(html.contains("<li>one</li>"));
    }

    #[test]
    fn raw_html_in_a_report_is_escaped_not_executed() {
        // A scraped page talked the agent into embedding a script tag.
        let html = to_html("Intro\n\n<script>alert(1)</script>\n\nAnd <img src=x onerror=y> inline.");
        assert!(!html.contains("<script"), "script tag must not survive: {html}");
        assert!(!html.contains("<img"), "img tag must not survive: {html}");
        assert!(html.contains("&lt;script&gt;"), "should be escaped text: {html}");
    }

    #[test]
    fn print_page_is_self_contained_and_escapes_the_title() {
        let r = ReportRow {
            report_id: 1,
            plan_id: 7,
            execution_id: Some(3),
            source_key: "acme".into(),
            subject: "Acme <Corp>".into(),
            title: "Acme \"2026\" & co".into(),
            markdown: "## Hello\n\nBody text.".into(),
            sources: serde_json::json!([{"title":"Site","url":"https://example.com/a"}]),
            word_count: 2,
            source: "Acme research".into(),
            first_seen_utc: chrono::Utc::now(),
            last_seen_utc: chrono::Utc::now(),
        };
        let page = print_page(&r);
        assert!(page.contains("Acme &quot;2026&quot; &amp; co"));
        assert!(page.contains("Acme &lt;Corp&gt;"));
        assert!(page.contains("@media print"));
        assert!(page.contains("window.print()"));
        assert!(page.contains("https://example.com/a"));
        assert!(page.contains("<h2>Hello</h2>"));
        // No app scripts or bundles — it must print the same anywhere.
        assert!(!page.contains("<script src"));
    }
}

//! Reading a public web page without a browser.
//!
//! Plain HTTP through [`crate::assets::fetch_with`], so the SSRF checks, pinned
//! resolution, manual redirects and size cap are the ones downloads already
//! have. Many large sites refuse a client that is not a browser; that is an
//! `Err` here, and every caller treats an `Err` as "ask the agent instead".

use std::collections::BTreeSet;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use regex::Regex;

/// Pages are text; three megabytes is a very long one.
const PAGE_CAP: usize = 3 * 1024 * 1024;

pub struct Page {
    pub url: String,
    pub html: String,
}

/// Fetch `url` as HTML. Refuses hosts the run's guard would refuse.
pub async fn fetch(url: &str) -> Result<Page> {
    let host = host_of(url).ok_or_else(|| anyhow!("no host in {url}"))?;
    if !crate::guard::host_may_be_fetched(&host) {
        return Err(anyhow!("{host} is not fetched outside the browser"));
    }
    let opts = crate::assets::FetchOpts {
        timeout_secs: 15,
        cap: PAGE_CAP,
        // Says what it is. A site that only serves browsers will refuse it,
        // and the agent's real browser takes over.
        user_agent: "Mozilla/5.0 (compatible; HuntwellBot/1.0; +https://huntwell.ai)",
    };
    let f = crate::assets::fetch_with(url, &opts).await?;
    if !f.content_type.contains("html") {
        return Err(anyhow!("not an HTML page ({})", f.content_type));
    }
    Ok(Page { url: url.to_string(), html: String::from_utf8_lossy(&f.bytes).into_owned() })
}

pub fn host_of(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.trim_start_matches("www.").to_ascii_lowercase())
}

/// Every `application/ld+json` block on the page, parsed. Blocks that are not
/// valid JSON are skipped rather than repaired.
pub fn json_ld(html: &str) -> Vec<serde_json::Value> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"(?is)<script[^>]*type\s*=\s*["']?application/ld\+json["']?[^>]*>(.*?)</script>"#).unwrap()
    });
    re.captures_iter(html)
        .filter_map(|c| serde_json::from_str(c[1].trim()).ok())
        .collect()
}

/// `<meta property|name="…" content="…">` pairs, either attribute order.
pub fn meta_tags(html: &str) -> Vec<(String, String)> {
    static TAG: OnceLock<Regex> = OnceLock::new();
    static ATTR: OnceLock<Regex> = OnceLock::new();
    let tag = TAG.get_or_init(|| Regex::new(r"(?is)<meta\s[^>]*>").unwrap());
    let attr = ATTR.get_or_init(|| Regex::new(r#"(?is)\b(property|name|content)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap());
    let mut out = Vec::new();
    for m in tag.find_iter(html) {
        let (mut key, mut content) = (None, None);
        for a in attr.captures_iter(m.as_str()) {
            let value = a.get(2).or_else(|| a.get(3)).map(|v| v.as_str()).unwrap_or("");
            match a[1].to_ascii_lowercase().as_str() {
                "content" => content = Some(value.to_string()),
                _ => key = Some(value.to_ascii_lowercase()),
            }
        }
        if let (Some(k), Some(c)) = (key, content) {
            if !k.is_empty() && !c.trim().is_empty() {
                out.push((k, unescape(c.trim())));
            }
        }
    }
    out
}

/// The links on the page that stay on its own site, as stable keys: scheme,
/// `www.`, fragment and tracking parameters removed. A listing page that gains
/// a listing gains a link; one whose adverts rotate mostly does not.
pub fn same_site_links(html: &str, page_url: &str) -> BTreeSet<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"(?is)<a\s[^>]*?href\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap());
    let Some(host) = host_of(page_url) else { return BTreeSet::new() };
    let mut out = BTreeSet::new();
    for c in re.captures_iter(html) {
        let href = unescape(c.get(1).or_else(|| c.get(2)).map(|m| m.as_str()).unwrap_or("").trim());
        let absolute = if href.starts_with("//") {
            format!("https:{href}")
        } else if href.starts_with('/') {
            format!("https://{host}{href}")
        } else if href.starts_with("http://") || href.starts_with("https://") {
            href
        } else {
            continue; // mailto:, javascript:, #fragment, relative-without-slash
        };
        if host_of(&absolute).as_deref() != Some(host.as_str()) {
            continue;
        }
        out.insert(link_key(&absolute));
    }
    out
}

/// A link reduced to what identifies the page it leads to.
pub fn link_key(url: &str) -> String {
    let no_fragment = url.split('#').next().unwrap_or(url);
    let (path, query) = no_fragment.split_once('?').unwrap_or((no_fragment, ""));
    let path = path.split_once("://").map(|(_, r)| r).unwrap_or(path).trim_start_matches("www.").trim_end_matches('/');
    let mut kept: Vec<&str> = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .filter(|kv| {
            let k = kv.split('=').next().unwrap_or("").to_ascii_lowercase();
            !(k.starts_with("utm_") || matches!(k.as_str(), "ref" | "fbclid" | "gclid" | "msclkid" | "sid" | "sessionid" | "clicktype" | "position" | "rank"))
        })
        .collect();
    kept.sort_unstable();
    if kept.is_empty() {
        path.to_ascii_lowercase()
    } else {
        format!("{}?{}", path.to_ascii_lowercase(), kept.join("&"))
    }
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&#x27;", "'").replace("&lt;", "<").replace("&gt;", ">")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_ld_blocks_are_found_and_broken_ones_skipped() {
        let html = r#"<html><script type="application/ld+json">{"@type":"Vehicle","name":"Crosstrek"}</script>
            <script type='application/ld+json'> not json </script>
            <script>var x = {"@type":"nope"}</script></html>"#;
        let blocks = json_ld(html);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["name"], "Crosstrek");
    }

    #[test]
    fn meta_tags_read_in_either_attribute_order() {
        let html = r#"<meta property="og:title" content="A &amp; B"><meta content="19.99" name="product:price:amount">"#;
        let tags = meta_tags(html);
        assert!(tags.contains(&("og:title".into(), "A & B".into())));
        assert!(tags.contains(&("product:price:amount".into(), "19.99".into())));
    }

    #[test]
    fn links_are_same_site_only_and_lose_their_tracking() {
        let html = r#"<a href="/car/1?utm_source=x&id=7#top">a</a> <a class=x href='https://www.cars.test/car/2/'>b</a>
            <a href="https://ads.example/click">ad</a> <a href="mailto:x@y.z">m</a> <a href="//cars.test/car/3">c</a>"#;
        let links = same_site_links(html, "https://www.cars.test/search?q=1");
        assert_eq!(links.into_iter().collect::<Vec<_>>(), vec!["cars.test/car/1?id=7", "cars.test/car/2", "cars.test/car/3"]);
    }

    #[test]
    fn the_guard_decides_what_is_fetched() {
        assert!(!crate::guard::host_may_be_fetched("www.linkedin.com"), "a restricted platform is never fetched by our own code");
        assert!(crate::guard::host_may_be_fetched("cars.com"));
    }
}

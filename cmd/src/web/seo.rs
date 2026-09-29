//! What search engines and link previews see.
//!
//! The UI is a single-page app: every URL is the same `index.html`, and its
//! `<title>` is only corrected once JavaScript runs. Crawlers that do not run
//! it — and every chat app or social site building a link preview — would see
//! one title and no description for the whole site. So the server writes the
//! head itself, per route, before the page leaves: title, description,
//! canonical URL, Open Graph and Twitter cards, and JSON-LD.
//!
//! [`PAGES`] is the one list of public pages. The sitemap, `robots.txt` and
//! the head all come from it; `UI/web/src/components/Site.tsx` (`SITE_PAGES`,
//! `PAGE_META`) mirrors it for in-app navigation. Add a public page in both.

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};

use super::App;

pub struct Page {
    pub path: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// Sitemap hints: how often it changes and how it ranks within the site.
    pub changefreq: &'static str,
    pub priority: &'static str,
}

pub const SITE_NAME: &str = "Huntwell";

/// The day the site's pages were last built (see `build.rs`), for `<lastmod>`.
/// Google ignores `changefreq` and `priority` and uses this, when it is honest.
const SITE_UPDATED: &str = env!("HUNTWELL_SITE_UPDATED");

pub const PAGES: &[Page] = &[
    Page {
        path: "/",
        title: "Huntwell — AI web research that turns pages into data",
        description: "Describe what you're looking for and Huntwell searches the web in a real browser, returning prospects, custom tables, written reports and files you can use.",
        changefreq: "weekly",
        priority: "1.0",
    },
    Page {
        path: "/product",
        title: "How Huntwell works — from a sentence to structured data",
        description: "Describe it, review the plan, and Huntwell browses for you: deduplicated results, schedules that skip when nothing is new, CSV export and an API.",
        changefreq: "monthly",
        priority: "0.9",
    },
    Page {
        path: "/use-cases",
        title: "Use cases — prospecting, listings, research and more | Huntwell",
        description: "Sales prospecting, market and listing watch, hiring research, research briefs, tenders and document collection: what people use Huntwell for.",
        changefreq: "monthly",
        priority: "0.8",
    },
    Page {
        path: "/pricing",
        title: "Pricing — pay for the tokens a run spends | Huntwell",
        description: "Huntwell charges by how many tokens a browse uses. Prepaid credits, no seat fee, and a cap on every run.",
        changefreq: "monthly",
        priority: "0.9",
    },
    Page {
        path: "/terms",
        title: "Terms of Service | Huntwell",
        description: "The terms that apply to using Huntwell.",
        changefreq: "yearly",
        priority: "0.3",
    },
    Page {
        path: "/privacy",
        title: "Privacy Policy | Huntwell",
        description: "What Huntwell collects, why, and what you can ask us to do with it.",
        changefreq: "yearly",
        priority: "0.3",
    },
];

/// Sign-in and sign-up pages: worth a title, not worth a search result.
const UNLISTED: &[(&str, &str)] = &[
    ("/login", "Sign in | Huntwell"),
    ("/signup", "Request access | Huntwell"),
    ("/forgot", "Reset your password | Huntwell"),
    ("/verify", "Confirm your email | Huntwell"),
];

/// Paths the UI answers that are not public pages: sign-in, invitations and
/// the app itself. Anything else is a 404 — served with the app, so a person
/// still lands somewhere, but not a page a search engine should keep.
fn is_app_route(path: &str) -> bool {
    UNLISTED.iter().any(|(p, _)| *p == path)
        || path == "/app"
        || path.starts_with("/app/")
        || path.starts_with("/join/")
}

/// What to do with a request for a page: serve it, send it to its one true
/// address, or serve it as not found.
pub enum Route {
    Page,
    Redirect(String),
    NotFound,
}

pub fn route(path: &str) -> Route {
    // One address per page: `/product/` → `/product`.
    if path.len() > 1 && path.ends_with('/') {
        let bare = normalise(path);
        if page(bare).is_some() {
            return Route::Redirect(bare.to_string());
        }
    }
    if page(path).is_some() || is_app_route(normalise(path)) {
        Route::Page
    } else {
        Route::NotFound
    }
}

pub fn page(path: &str) -> Option<&'static Page> {
    let path = normalise(path);
    PAGES.iter().find(|p| p.path == path)
}

/// `/product/` and `/product` are one page.
fn normalise(path: &str) -> &str {
    if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    }
}

/// The site's own address, for absolute URLs. `HUNTWELL_PUBLIC_URL` when set;
/// otherwise the host the request came to, which is right behind any proxy
/// that passes `Host` through.
pub fn base_url(headers: &HeaderMap) -> String {
    base_url_with(crate::config::get("HUNTWELL_PUBLIC_URL"), headers)
}

fn base_url_with(configured: Option<String>, headers: &HeaderMap) -> String {
    if let Some(b) = configured.map(|b| b.trim().trim_end_matches('/').to_string()).filter(|b| !b.is_empty()) {
        return b;
    }
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
    // Only a character set a host can have, so a forged Host cannot write markup.
    let host: String = host.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']')).collect();
    let scheme = if host.starts_with("localhost") || host.starts_with("127.") { "http" } else { "https" };
    format!("{scheme}://{host}")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// JSON inside `<script>`: `</` must not close the tag early.
fn json_ld(v: &serde_json::Value) -> String {
    format!("<script type=\"application/ld+json\">{}</script>", v.to_string().replace("</", "<\\/"))
}

/// The head block for `path`, replacing everything between the markers in
/// `index.html`. Public pages get the full set; sign-in pages a title and
/// `noindex`; the app and anything unknown just `noindex` — a signed-in page
/// has nothing to show a crawler, and an unknown URL is not a second copy of
/// the home page.
pub fn head_for(path: &str, base: &str) -> String {
    let path = normalise(path);
    if let Some(p) = page(path) {
        let url = if p.path == "/" { format!("{base}/") } else { format!("{base}{}", p.path) };
        let image = format!("{base}/og.png");
        let mut out = format!(
            "<title>{title}</title>\n    \
             <meta name=\"description\" content=\"{desc}\" />\n    \
             <link rel=\"canonical\" href=\"{url}\" />\n    \
             <meta name=\"robots\" content=\"index, follow\" />\n    \
             <meta property=\"og:type\" content=\"website\" />\n    \
             <meta property=\"og:site_name\" content=\"{SITE_NAME}\" />\n    \
             <meta property=\"og:title\" content=\"{title}\" />\n    \
             <meta property=\"og:description\" content=\"{desc}\" />\n    \
             <meta property=\"og:url\" content=\"{url}\" />\n    \
             <meta property=\"og:image\" content=\"{image}\" />\n    \
             <meta property=\"og:image:width\" content=\"1200\" />\n    \
             <meta property=\"og:image:height\" content=\"630\" />\n    \
             <meta property=\"og:image:alt\" content=\"Huntwell — find what you are looking for\" />\n    \
             <meta name=\"twitter:card\" content=\"summary_large_image\" />\n    \
             <meta name=\"twitter:title\" content=\"{title}\" />\n    \
             <meta name=\"twitter:description\" content=\"{desc}\" />\n    \
             <meta name=\"twitter:image\" content=\"{image}\" />",
            title = esc(p.title),
            desc = esc(p.description),
            url = esc(&url),
            image = esc(&image),
        );
        for ld in structured_data(p, base, &url) {
            out.push_str("\n    ");
            out.push_str(&json_ld(&ld));
        }
        return out;
    }
    let title = UNLISTED.iter().find(|(p, _)| *p == path).map(|(_, t)| *t).unwrap_or(SITE_NAME);
    format!("<title>{}</title>\n    <meta name=\"robots\" content=\"noindex, nofollow\" />", esc(title))
}

/// What schema.org says about each page. Only facts the pages themselves
/// state — a rich result built on a claim the page does not make is a penalty.
fn structured_data(p: &Page, base: &str, url: &str) -> Vec<serde_json::Value> {
    use serde_json::json;
    let org = json!({
        "@type": "Organization",
        "@id": format!("{base}/#organization"),
        "name": SITE_NAME,
        "legalName": "Yak Systems, Inc.",
        "url": format!("{base}/"),
        "logo": {
            "@type": "ImageObject",
            "url": format!("{base}/icon-512.png"),
            "width": 512,
            "height": 512,
        },
        "sameAs": ["https://x.com/huntwell", "https://www.linkedin.com/company/huntwell", "https://github.com/huntwell"],
    });
    let mut out = Vec::new();
    match p.path {
        "/" => {
            out.push(json!({
                "@context": "https://schema.org",
                "@graph": [
                    org,
                    {
                        "@type": "WebSite",
                        "@id": format!("{base}/#website"),
                        // What Google shows as the site name in results.
                        "name": SITE_NAME,
                        "alternateName": ["huntwell.ai", "Huntwell AI"],
                        "url": format!("{base}/"),
                        "publisher": { "@id": format!("{base}/#organization") },
                    },
                    {
                        "@type": "SoftwareApplication",
                        "name": SITE_NAME,
                        "applicationCategory": "BusinessApplication",
                        "operatingSystem": "Web",
                        "url": format!("{base}/"),
                        "description": p.description,
                        "publisher": { "@id": format!("{base}/#organization") },
                    },
                ],
            }));
        }
        _ => {}
    }
    if p.path != "/" {
        out.push(json!({
            "@context": "https://schema.org",
            "@type": "BreadcrumbList",
            "itemListElement": [
                { "@type": "ListItem", "position": 1, "name": SITE_NAME, "item": format!("{base}/") },
                { "@type": "ListItem", "position": 2, "name": p.title.split(" | ").next().unwrap_or(p.title).split(" — ").next().unwrap_or(p.title), "item": url },
            ],
        }));
    }
    out
}

/// Markers in `index.html` around the part of the head this module owns.
const HEAD_START: &str = "<!--seo-->";
const HEAD_END: &str = "<!--/seo-->";

/// `index.html` with the head for `path` written in. Unchanged if the markers
/// are missing (an old bundle), so the page still loads.
pub fn render_index(html: &str, path: &str, base: &str) -> String {
    let (Some(a), Some(b)) = (html.find(HEAD_START), html.find(HEAD_END)) else {
        return html.to_string();
    };
    if b < a {
        return html.to_string();
    }
    format!("{}\n    {}\n    {}", &html[..a + HEAD_START.len()], head_for(path, base), &html[b..])
}

pub async fn robots(headers: HeaderMap) -> Response {
    let base = base_url(&headers);
    let body = format!(
        "# The public site is for everyone; the app, its API and one-time links are not.\n\
         User-agent: *\n\
         Allow: /\n\
         Disallow: /app\n\
         Disallow: /api/\n\
         Disallow: /v1/\n\
         Disallow: /dl/\n\
         Disallow: /join/\n\
         Disallow: /login\n\
         Disallow: /signup\n\
         Disallow: /forgot\n\
         Disallow: /verify\n\
         \n\
         Sitemap: {base}/sitemap.xml\n"
    );
    ([(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8")), (header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600"))], body)
        .into_response()
}

pub async fn sitemap(State(_): State<App>, headers: HeaderMap) -> Response {
    let base = base_url(&headers);
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
    for p in PAGES {
        let loc = if p.path == "/" { format!("{base}/") } else { format!("{base}{}", p.path) };
        xml.push_str(&format!(
            "  <url>\n    <loc>{}</loc>\n    <lastmod>{SITE_UPDATED}</lastmod>\n    <changefreq>{}</changefreq>\n    <priority>{}</priority>\n  </url>\n",
            esc(&loc),
            p.changefreq,
            p.priority
        ));
    }
    xml.push_str("</urlset>\n");
    ([(header::CONTENT_TYPE, HeaderValue::from_static("application/xml; charset=utf-8")), (header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600"))], xml)
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = "<html><head>\n    <!--seo-->\n    <title>x</title>\n    <!--/seo-->\n</head></html>";

    #[test]
    fn public_pages_get_their_own_head() {
        let html = render_index(INDEX, "/product/", "https://huntwell.ai");
        assert!(html.contains("<title>How Huntwell works — from a sentence to structured data</title>"), "{html}");
        assert!(html.contains("<link rel=\"canonical\" href=\"https://huntwell.ai/product\" />"));
        assert!(html.contains("og:image\" content=\"https://huntwell.ai/og.png\""));
        assert!(html.contains("BreadcrumbList"));
        assert!(!html.contains("<title>x</title>"), "the default title is replaced, not duplicated");
    }

    #[test]
    fn the_app_and_unknown_urls_are_not_indexed() {
        for path in ["/app", "/app/plans/3", "/nope", "/join/abc"] {
            let html = render_index(INDEX, path, "https://huntwell.ai");
            assert!(html.contains("noindex"), "{path}: {html}");
            assert!(!html.contains("canonical"), "{path}");
        }
        assert!(render_index(INDEX, "/login", "https://huntwell.ai").contains("<title>Sign in | Huntwell</title>"));
    }

    #[test]
    fn a_forged_host_cannot_write_markup() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("evil.test\"><script>"));
        let b = base_url_with(None, &h);
        assert!(!b.contains('"') && !b.contains('<'), "{b}");
    }

    #[test]
    fn structured_data_cannot_close_its_script_tag() {
        let s = json_ld(&serde_json::json!({ "x": "</script><script>alert(1)" }));
        assert_eq!(s.matches("</script>").count(), 1, "{s}");
    }

    #[test]
    fn the_ui_carries_the_same_titles_and_descriptions() {
        let site = include_str!("../../../UI/web/src/components/Site.tsx");
        for p in PAGES {
            assert!(site.contains(p.title), "PAGE_META in Site.tsx is missing the title for {}", p.path);
            assert!(site.contains(p.description), "PAGE_META in Site.tsx has a different description for {}", p.path);
        }
    }

    #[test]
    fn unknown_urls_are_404_and_slashes_redirect() {
        assert!(matches!(route("/product"), Route::Page));
        assert!(matches!(route("/"), Route::Page));
        assert!(matches!(route("/app/plans/3"), Route::Page));
        assert!(matches!(route("/join/abc"), Route::Page));
        assert!(matches!(route("/login"), Route::Page));
        assert!(matches!(route("/product/"), Route::Redirect(ref to) if to == "/product"));
        assert!(matches!(route("/pricing"), Route::Page));
        assert!(matches!(route("/pricing/"), Route::Redirect(ref to) if to == "/pricing"));
        assert!(matches!(route("/developers"), Route::NotFound));
        assert!(matches!(route("/developers/"), Route::NotFound));
        assert!(matches!(route("/security"), Route::NotFound));
        assert!(matches!(route("/security/"), Route::NotFound));
        assert!(matches!(route("/wp-admin"), Route::NotFound));
        assert!(matches!(route("/pricingx"), Route::NotFound));
    }

    #[test]
    fn every_route_the_ui_has_is_served_not_404() {
        let app = include_str!("../../../UI/web/src/App.tsx");
        let mut seen = 0;
        for part in app.split("path=\"/").skip(1) {
            let path = format!("/{}", part.split('"').next().unwrap());
            // `/join/:token` and the like: any value in the parameter.
            let path = path.replace(":token", "abc").replace(":id", "7");
            assert!(!matches!(route(&path), Route::NotFound), "{path} is routed in App.tsx but the server would 404 it");
            seen += 1;
        }
        assert!(seen >= 10, "found only {seen} routes — has App.tsx changed shape?");
    }

    #[test]
    fn the_sitemap_date_is_a_date() {
        let d = SITE_UPDATED;
        assert!(d.len() == 10 && d.as_bytes()[4] == b'-' && d.as_bytes()[7] == b'-' && d.starts_with("20"), "{d}");
    }

    #[test]
    fn every_listed_page_is_a_route_the_ui_has() {
        let app = include_str!("../../../UI/web/src/App.tsx");
        for p in PAGES.iter().filter(|p| p.path != "/") {
            assert!(app.contains(&format!("path=\"{}\"", p.path)), "{} is in the sitemap but not routed in App.tsx", p.path);
        }
    }
}

//! Downloading the files an assets plan finds.
//!
//! The agent only ever reports URLs; huntwell fetches the bytes itself. That
//! matters for safety as much as tidiness: the URLs come from pages the agent
//! read, so they are attacker-influenced, and a naive fetch of an
//! agent-supplied URL is a server-side request forgery primitive pointed at
//! whatever the run host can reach — cloud metadata endpoints, cluster
//! services, localhost admin ports.
//!
//! So every URL is checked twice: the literal host before connecting, and
//! every redirect hop, both against the resolved IP rather than the name (a
//! hostname can resolve to 169.254.169.254 just as easily as an IP literal can
//! say so). Size is capped while streaming, not after.

use std::net::{IpAddr, SocketAddr};

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;

pub struct FetchedFile {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub content_type: String,
    pub ext: String,
    pub sha256: String,
}

/// Per-file byte cap. Generous enough for filings and decks, small enough that
/// one bad link cannot fill the object store.
pub fn max_bytes() -> usize {
    crate::config::get("HUNTWELL_ASSET_MAX_MB")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(25)
        .clamp(1, 200)
        * 1024
        * 1024
}

pub fn max_files_per_run() -> usize {
    crate::config::get("HUNTWELL_ASSET_MAX_FILES")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(50)
        .clamp(1, 1000)
}

/// Addresses no scrape has any business reaching.
fn is_forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local() // 169.254.0.0/16 — cloud metadata lives here
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                // 100.64.0.0/10 carrier-grade NAT, and 0.0.0.0/8
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
                || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // unique-local fc00::/7 and link-local fe80::/10
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // v4-mapped: judge by the embedded address
                || v6.to_ipv4_mapped().map(|v4| is_forbidden(IpAddr::V4(v4))).unwrap_or(false)
        }
    }
}

/// Cheap syntactic check, before any DNS or connection.
pub fn check_url(url: &str) -> Result<(), String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("empty url".into());
    }
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("only http(s) urls are fetched".into());
    }
    let host = host_of(url).ok_or_else(|| "no host in url".to_string())?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") || host.ends_with(".internal") {
        return Err(format!("host {host:?} is not reachable from a scrape"));
    }
    // An IP literal can be judged now; names need DNS, which happens in fetch.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if forbidden_now(ip) {
            return Err(format!("address {ip} is private or link-local"));
        }
    }
    Ok(())
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    // Strip the port, taking care with bracketed IPv6.
    let host = if let Some(end) = authority.strip_prefix('[').and_then(|a| a.find(']').map(|i| &a[..i])) {
        end.to_string()
    } else {
        authority.split(':').next()?.to_string()
    };
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// Test-only: let a server on 127.0.0.1 stand in for "the internet", so the
/// redirect handling can be exercised against a real listener. Link-local and
/// the rest stay forbidden, which is what the tests then check.
#[cfg(test)]
static ALLOW_LOOPBACK_FOR_TESTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn forbidden_now(ip: IpAddr) -> bool {
    #[cfg(test)]
    if ALLOW_LOOPBACK_FOR_TESTS.load(std::sync::atomic::Ordering::Relaxed) && ip.is_loopback() {
        return false;
    }
    is_forbidden(ip)
}

/// Resolve a host and return the addresses it answered with — every one of
/// them vetted. The connection is then pinned to exactly these, so a name
/// that answers differently a moment later (DNS rebinding) changes nothing.
async fn safe_addrs(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        if forbidden_now(ip) {
            anyhow::bail!("address {ip} is private or link-local");
        }
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("resolve {host}"))?
        .collect::<Vec<_>>();
    if addrs.is_empty() {
        anyhow::bail!("{host} did not resolve");
    }
    for a in &addrs {
        if forbidden_now(a.ip()) {
            anyhow::bail!("{host} resolves to {} which is private or link-local", a.ip());
        }
    }
    Ok(addrs)
}

/// The port a URL connects to: explicit, else the scheme's.
fn port_of(url: &str) -> u16 {
    let https = url.to_ascii_lowercase().starts_with("https://");
    let rest = url.split("://").nth(1).unwrap_or("");
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("").rsplit('@').next().unwrap_or("");
    let after_host = if authority.starts_with('[') {
        authority.find(']').map(|i| &authority[i + 1..]).unwrap_or("")
    } else {
        authority.find(':').map(|i| &authority[i..]).unwrap_or("")
    };
    after_host
        .strip_prefix(':')
        .and_then(|p| p.parse().ok())
        .unwrap_or(if https { 443 } else { 80 })
}

/// Where a redirect points, as an absolute URL.
fn next_hop(current: &str, location: &str) -> Result<String> {
    let base = reqwest::Url::parse(current).with_context(|| format!("parse {current}"))?;
    Ok(base.join(location.trim()).with_context(|| format!("bad redirect {location:?}"))?.to_string())
}

const MAX_HOPS: usize = 5;

/// Downloads one file, enforcing the size cap while streaming so an
/// unbounded response is dropped rather than buffered.
///
/// Every hop — the URL given and each redirect after it — is checked the same
/// way: the literal, then what it resolves to, and the connection is pinned to
/// the addresses that passed. Redirects are followed here rather than by the
/// client, because a client that follows on its own resolves the next name
/// itself, and that resolution is the one nobody checked.
pub async fn fetch(url: &str) -> Result<FetchedFile> {
    let mut current = url.trim().to_string();
    let mut resp = None;
    for hop in 0..=MAX_HOPS {
        check_url(&current).map_err(|e| anyhow!("{e}"))?;
        let host = host_of(&current).ok_or_else(|| anyhow!("no host in url"))?;
        let port = port_of(&current);
        let addrs = safe_addrs(&host, port).await?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(&host, &addrs)
            .user_agent("huntwell-assets/1.0")
            .build()
            .context("build download client")?;
        let r = client.get(&current).send().await.with_context(|| format!("GET {current}"))?;
        if r.status().is_redirection() {
            if hop == MAX_HOPS {
                anyhow::bail!("too many redirects");
            }
            let location = r
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect without a location"))?;
            current = next_hop(&current, location)?;
            continue;
        }
        resp = Some(r);
        break;
    }
    let resp = resp.ok_or_else(|| anyhow!("too many redirects"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("server answered {status}");
    }
    // Trust the declared length only to fail fast; the streaming cap is what
    // actually protects us.
    let cap = max_bytes();
    if let Some(len) = resp.content_length() {
        if len as usize > cap {
            anyhow::bail!("file is {} , over the {} cap", human(len as usize), human(cap));
        }
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or("application/octet-stream")
        .trim()
        .to_ascii_lowercase();
    let disposition_name = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(filename_from_disposition);

    let mut bytes: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read response body")?;
        if bytes.len() + chunk.len() > cap {
            anyhow::bail!("file exceeds the {} cap", human(cap));
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        anyhow::bail!("file was empty");
    }

    // Named for where it ended up, not where it started.
    let filename = disposition_name.unwrap_or_else(|| filename_from_url(&current));
    let ext = extension_for(&filename, &content_type);
    let filename = if filename.contains('.') { filename } else { format!("{filename}.{ext}") };
    let sha256 = crate::objstore::sha256_hex(&bytes);
    Ok(FetchedFile { bytes, filename, content_type, ext, sha256 })
}

fn human(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KB", n as f64 / 1024.0)
    }
}

fn filename_from_disposition(v: &str) -> Option<String> {
    let idx = v.to_ascii_lowercase().find("filename=")?;
    let raw = v[idx + 9..].trim().trim_matches('"');
    let name = sanitize_filename(raw.split(';').next().unwrap_or(raw));
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn filename_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.rsplit('/').next().unwrap_or("");
    let name = sanitize_filename(last);
    if name.is_empty() {
        "download".into()
    } else {
        name
    }
}

/// Keeps a filename to characters that are safe in a header, on a disk and in
/// an object key — the same discipline the CSV download uses.
pub fn sanitize_filename(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    cleaned.trim_matches(['.', '_']).chars().take(120).collect()
}

fn extension_for(filename: &str, content_type: &str) -> String {
    if let Some(ext) = filename.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()) {
        if !ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            return ext;
        }
    }
    match content_type {
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        "image/webp" => "webp",
        "text/csv" => "csv",
        "text/plain" => "txt",
        "text/html" => "html",
        "application/json" => "json",
        "application/zip" => "zip",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        _ => "bin",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_metadata_addresses_are_refused() {
        for url in [
            "http://127.0.0.1:8611/api/plans",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/secret",
            "http://192.168.1.143:7611/",
            "http://172.16.4.4/",
            "http://localhost/x",
            "http://postgres.huntwell.svc.cluster.internal/",
            "http://[::1]/x",
            "file:///etc/passwd",
            "ftp://example.com/f.pdf",
            "",
        ] {
            assert!(check_url(url).is_err(), "should have refused {url:?}");
        }
    }

    #[test]
    fn ordinary_public_urls_are_allowed() {
        for url in [
            "https://www.sec.gov/Archives/edgar/data/1318605/000156459021004599/tsla-10k_20201231.htm",
            "http://example.com/a.pdf",
            "https://user:pw@files.example.com:8443/deck.pdf?x=1#p2",
        ] {
            assert!(check_url(url).is_ok(), "should have allowed {url:?}");
        }
    }

    #[test]
    fn host_parsing_handles_ports_userinfo_and_ipv6() {
        assert_eq!(host_of("https://a.example.com/x").as_deref(), Some("a.example.com"));
        assert_eq!(host_of("https://u:p@a.example.com:8443/x").as_deref(), Some("a.example.com"));
        assert_eq!(host_of("http://[::1]:80/x").as_deref(), Some("::1"));
        assert_eq!(host_of("http://1.2.3.4/x").as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn filenames_are_sanitized_and_extensions_inferred() {
        assert_eq!(filename_from_url("https://x.com/a/b/tesla-10k.pdf?v=2"), "tesla-10k.pdf");
        assert_eq!(filename_from_url("https://x.com/../../etc/passwd"), "passwd");
        assert_eq!(filename_from_url("https://x.com/"), "download");
        assert_eq!(sanitize_filename("../../evil sh.pdf"), "evil_sh.pdf");
        assert_eq!(extension_for("deck", "application/pdf"), "pdf");
        assert_eq!(extension_for("a.PDF", "application/octet-stream"), "pdf");
        assert_eq!(extension_for("noext", "application/x-weird"), "bin");
        assert_eq!(
            filename_from_disposition("attachment; filename=\"annual report.pdf\"").as_deref(),
            Some("annual_report.pdf")
        );
    }

    #[test]
    fn ports_and_hops_are_read_from_the_url() {
        assert_eq!(port_of("https://a.example/x"), 443);
        assert_eq!(port_of("http://a.example/x"), 80);
        assert_eq!(port_of("http://a.example:8080/x?y=1"), 8080);
        assert_eq!(port_of("http://[::1]:9/x"), 9);
        assert_eq!(next_hop("https://a.example/dir/f.pdf", "/other").unwrap(), "https://a.example/other");
        assert_eq!(next_hop("https://a.example/dir/f.pdf", "g.pdf").unwrap(), "https://a.example/dir/g.pdf");
        assert_eq!(next_hop("https://a.example/", "http://b.example/z").unwrap(), "http://b.example/z");
    }

    /// A real listener: a public-looking first hop that redirects to link-local
    /// must stop at the redirect, and a redirect to another allowed address
    /// must be followed with the file named for where it ended up.
    #[tokio::test]
    async fn redirects_are_checked_hop_by_hop_and_pinned() {
        use axum::{response::Redirect, routing::get, Router};
        ALLOW_LOOPBACK_FOR_TESTS.store(true, std::sync::atomic::Ordering::Relaxed);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new()
            .route("/to-metadata", get(|| async { Redirect::temporary("http://169.254.169.254/latest/meta-data/") }))
            .route("/to-localhost", get(|| async { Redirect::temporary("http://localhost/secret") }))
            .route("/to-file", get(|| async { Redirect::temporary("/final.txt") }))
            .route("/final.txt", get(|| async { "hello" }))
            .route("/loop", get(|| async { Redirect::temporary("/loop") }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://127.0.0.1:{port}");
        async fn fails(url: String) -> String {
            match fetch(&url).await {
                Err(e) => e.to_string(),
                Ok(_) => panic!("{url} should have been refused"),
            }
        }

        let e = fails(format!("{base}/to-metadata")).await;
        assert!(e.contains("link-local") || e.contains("private"), "{e}");
        let e = fails(format!("{base}/to-localhost")).await;
        assert!(e.contains("not reachable"), "{e}");
        let e = fails(format!("{base}/loop")).await;
        assert!(e.contains("too many redirects"), "{e}");
        let f = fetch(&format!("{base}/to-file")).await.unwrap();
        assert_eq!(f.bytes, b"hello");
        assert_eq!(f.filename, "final.txt");
        ALLOW_LOOPBACK_FOR_TESTS.store(false, std::sync::atomic::Ordering::Relaxed);
    }

}

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

use std::net::IpAddr;

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
        if is_forbidden(ip) {
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

/// Resolves a host and rejects it if any address it answers with is one we
/// refuse to talk to.
async fn resolve_is_safe(host: &str, port: u16) -> Result<()> {
    if host.parse::<IpAddr>().is_ok() {
        return Ok(()); // already judged by check_url
    }
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("resolve {host}"))?
        .collect::<Vec<_>>();
    if addrs.is_empty() {
        return Err(anyhow!("{host} did not resolve"));
    }
    for a in addrs {
        if is_forbidden(a.ip()) {
            return Err(anyhow!("{host} resolves to {} which is private or link-local", a.ip()));
        }
    }
    Ok(())
}

/// Downloads one file, enforcing the size cap while streaming so an
/// unbounded response is dropped rather than buffered.
pub async fn fetch(url: &str) -> Result<FetchedFile> {
    check_url(url).map_err(|e| anyhow!("{e}"))?;
    let host = host_of(url).ok_or_else(|| anyhow!("no host in url"))?;
    let port = if url.to_ascii_lowercase().starts_with("https://") { 443 } else { 80 };
    resolve_is_safe(&host, port).await?;

    // Redirects are re-checked: the first hop being safe says nothing about
    // where it points. Names on a hop are resolved by the policy's own check
    // in the next request anyway, so the literal check here is the backstop.
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 {
            return attempt.stop();
        }
        match check_url(attempt.url().as_str()) {
            Ok(()) => attempt.follow(),
            Err(_) => attempt.stop(),
        }
    });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .redirect(policy)
        .user_agent("huntwell-assets/1.0")
        .build()
        .context("build download client")?;

    let resp = client.get(url).send().await.with_context(|| format!("GET {url}"))?;
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

    let filename = disposition_name.unwrap_or_else(|| filename_from_url(url));
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
}

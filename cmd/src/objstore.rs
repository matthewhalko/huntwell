//! Where collected files live.
//!
//! Asset bytes cannot live on the machine that downloaded them: a pool run
//! executes in a worker VM, and the process that later serves the file runs in
//! the app VM. So they go to an S3-compatible object store (MinIO
//! locally, S3/R2/whatever in production) and the database keeps only the key.
//!
//! The client is hand-rolled over `reqwest` rather than an SDK, matching the
//! rest of this codebase (the CDP probe in `browser.rs`, the SigV4 calls
//! in `aws.rs`): we need exactly three verbs against one bucket, and an
//! SDK would be thirty crates to get them.
//!
//! With no endpoint configured it falls back to a directory under the data
//! dir. That keeps `./dev.sh` working on one machine before MinIO exists —
//! but it is single-machine only, and a deployment with worker VMs must
//! configure a real endpoint or its workers will write files nothing else can
//! read.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};


/// Bytes we refuse to hold in one object regardless of the caller's own cap.
const HARD_MAX_BYTES: usize = 200 * 1024 * 1024;

enum Backend {
    S3 { endpoint: String, bucket: String, region: String, access_key: String, secret_key: String },
    /// Dev only: a directory on this machine.
    Fs { root: PathBuf },
}

fn backend() -> Backend {
    let bucket = crate::config::get("HUNTWELL_S3_BUCKET").unwrap_or_default();
    if bucket.trim().is_empty() {
        return Backend::Fs { root: crate::config::data_dir().join("objects") };
    }
    let region = crate::config::get_or("HUNTWELL_S3_REGION", "us-east-1");

    // Two credentials, in priority order:
    //
    //   AWS_S3_KEY / AWS_S3_SECRET          an IAM user scoped to this bucket
    //   HUNTWELL_S3_ACCESS_KEY / _SECRET  MinIO's root pair, and dev
    //
    // The scoped pair wins where both exist, which is how production ends up on
    // a credential that can touch one bucket and nothing else. It is not a
    // fallback in the other direction: a missing scoped key must not silently
    // reach for the bootstrap credential.
    let (access_key, secret_key) = if crate::aws::has_service_credentials("S3") {
        (
            crate::config::get("AWS_S3_KEY").unwrap_or_default(),
            crate::config::get("AWS_S3_SECRET").unwrap_or_default(),
        )
    } else {
        (
            crate::config::get("HUNTWELL_S3_ACCESS_KEY").unwrap_or_default(),
            crate::config::get("HUNTWELL_S3_SECRET_KEY").unwrap_or_default(),
        )
    };
    if access_key.trim().is_empty() || secret_key.trim().is_empty() {
        return Backend::Fs { root: crate::config::data_dir().join("objects") };
    }

    // With no endpoint, real S3 in that region. MinIO and every other clone is
    // named explicitly — the default should be the thing that needs no setting.
    let endpoint = match crate::config::get("HUNTWELL_S3_ENDPOINT") {
        Some(e) if !e.trim().is_empty() => e.trim().trim_end_matches('/').to_string(),
        _ => format!("https://s3.{region}.amazonaws.com"),
    };
    Backend::S3 { endpoint, bucket: bucket.trim().to_string(), region, access_key, secret_key }
}

/// True when a real object store is configured. False means the local
/// filesystem fallback, which does not work across machines.
pub fn is_remote() -> bool {
    matches!(backend(), Backend::S3 { .. })
}

/// A short description for logs and the doctor command.
pub fn describe() -> String {
    match backend() {
        Backend::S3 { endpoint, bucket, .. } => format!("{endpoint}/{bucket}"),
        Backend::Fs { root } => format!("{} (local filesystem — single machine only)", root.display()),
    }
}

/// Content-addressed key: the same file collected twice costs one object.
pub fn object_key(account_id: i64, plan_id: i64, sha256: &str, ext: &str) -> String {
    let ext = ext.trim().trim_start_matches('.');
    let suffix = if ext.is_empty() { String::new() } else { format!(".{}", sanitize_ext(ext)) };
    format!("acct/{account_id}/plan/{plan_id}/{sha256}{suffix}")
}

fn sanitize_ext(ext: &str) -> String {
    ext.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect::<String>().to_ascii_lowercase()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub async fn put(key: &str, bytes: &[u8], content_type: &str) -> Result<()> {
    if bytes.len() > HARD_MAX_BYTES {
        anyhow::bail!("object is {} bytes, over the {HARD_MAX_BYTES}-byte hard limit", bytes.len());
    }
    match backend() {
        Backend::Fs { root } => {
            let path = root.join(key);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
            }
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
            Ok(())
        }
        Backend::S3 { .. } => {
            match s3_request("PUT", key, Some(bytes), content_type).await {
                Ok(_) => Ok(()),
                // Self-healing first write: a fresh MinIO has no bucket yet,
                // and creating it is cheaper than making every deployment
                // remember to. Any other failure is reported as-is.
                Err(e) if format!("{e:#}").contains("NoSuchBucket") => {
                    ensure_bucket().await.context("create the bucket")?;
                    s3_request("PUT", key, Some(bytes), content_type).await.map(|_| ())
                }
                Err(e) => Err(e),
            }
        }
    }
}

/// `PUT /{bucket}` — idempotent enough in practice (an existing bucket answers
/// 409, which we treat as success).
pub async fn ensure_bucket() -> Result<()> {
    match s3_request("PUT", "", None, "").await {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.contains("BucketAlreadyOwnedByYou") || msg.contains("BucketAlreadyExists") || msg.contains("(409") {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

pub async fn get(key: &str) -> Result<Vec<u8>> {
    match backend() {
        Backend::Fs { root } => {
            let path = root.join(key);
            std::fs::read(&path).with_context(|| format!("read {}", path.display()))
        }
        Backend::S3 { .. } => s3_request("GET", key, None, "").await,
    }
}

pub async fn delete(key: &str) -> Result<()> {
    match backend() {
        Backend::Fs { root } => {
            let path = root.join(key);
            if path.exists() {
                std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
            }
            Ok(())
        }
        Backend::S3 { .. } => s3_request("DELETE", key, None, "").await.map(|_| ()),
    }
}

// ---------------------------------------------------------------------------
// S3 over SigV4
// ---------------------------------------------------------------------------

async fn s3_request(method: &str, key: &str, body: Option<&[u8]>, content_type: &str) -> Result<Vec<u8>> {
    let Backend::S3 { endpoint, bucket, region, access_key, secret_key } = backend() else {
        unreachable!("s3_request called without an S3 backend")
    };
    // Path-style addressing: MinIO and every S3 clone accept it, and it needs
    // no per-bucket DNS. An empty key addresses the bucket itself.
    let canonical_uri = if key.is_empty() {
        format!("/{}", uri_encode(&bucket, false))
    } else {
        format!("/{}/{}", uri_encode(&bucket, false), uri_encode(key, false))
    };
    let url = format!("{endpoint}{canonical_uri}");
    let host = host_of(&endpoint)?;

    let payload = body.unwrap_or(&[]);
    let payload_hash = sha256_hex(payload);
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();

    let auth = sign_v4(SignInput {
        method,
        canonical_uri: &canonical_uri,
        host: &host,
        amz_date: &amz_date,
        date_stamp: &date_stamp,
        region: &region,
        payload_hash: &payload_hash,
        access_key: &access_key,
        secret_key: &secret_key,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .context("build object-store client")?;
    let mut req = client
        .request(reqwest::Method::from_bytes(method.as_bytes())?, &url)
        .header("host", &host)
        .header("x-amz-date", &amz_date)
        .header("x-amz-content-sha256", &payload_hash)
        .header("authorization", auth);
    if !content_type.trim().is_empty() {
        req = req.header("content-type", content_type);
    }
    if let Some(b) = body {
        req = req.body(b.to_vec());
    }

    let resp = req.send().await.with_context(|| format!("{method} {url}"))?;
    let status = resp.status();
    let bytes = resp.bytes().await.unwrap_or_default();
    if !status.is_success() {
        let text = String::from_utf8_lossy(&bytes);
        anyhow::bail!("object store {method} failed ({status}): {}", text.trim().chars().take(300).collect::<String>());
    }
    Ok(bytes.to_vec())
}

fn host_of(endpoint: &str) -> Result<String> {
    let rest = endpoint.split("://").nth(1).unwrap_or(endpoint);
    let host = rest.split('/').next().unwrap_or(rest);
    if host.is_empty() {
        return Err(anyhow!("HUNTWELL_S3_ENDPOINT has no host: {endpoint:?}"));
    }
    Ok(host.to_string())
}

/// RFC 3986 encoding as S3 canonicalization wants it. `encode_slash` is false
/// for path components (the separators must stay literal).
fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else if c == '/' && !encode_slash {
            out.push('/');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

struct SignInput<'a> {
    method: &'a str,
    canonical_uri: &'a str,
    host: &'a str,
    amz_date: &'a str,
    date_stamp: &'a str,
    region: &'a str,
    payload_hash: &'a str,
    access_key: &'a str,
    secret_key: &'a str,
}

/// The `Authorization` header for one S3 request.
///
/// The signing itself lives in `crate::aws`, shared with Secrets Manager. Two
/// copies of SigV4 in one program is two things to keep correct, and a drifted
/// copy fails as a signature mismatch that says nothing about why.
fn sign_v4(i: SignInput<'_>) -> String {
    // The three headers an S3 request here always sends, in the required
    // lowercase-sorted order.
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";
    let canonical_headers =
        format!("host:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n", i.host, i.payload_hash, i.amz_date);
    crate::aws::authorization(crate::aws::Signed {
        method: i.method,
        canonical_uri: i.canonical_uri,
        canonical_query: "",
        host: i.host,
        canonical_headers: &canonical_headers,
        signed_headers,
        amz_date: i.amz_date,
        date_stamp: i.date_stamp,
        region: i.region,
        service: "s3",
        payload_hash: i.payload_hash,
        access_key: i.access_key,
        secret_key: i.secret_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_key_matches_the_aws_worked_example() {
        // From AWS's "deriving a signing key" documentation.
        let key = crate::aws::signing_key("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "20120215", "us-east-1", "iam");
        assert_eq!(hex::encode(key), "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d");
    }

    #[test]
    fn authorization_header_has_the_required_shape() {
        let auth = sign_v4(SignInput {
            method: "PUT",
            canonical_uri: "/bucket/acct/1/plan/2/abc.pdf",
            host: "127.0.0.1:9611",
            amz_date: "20260101T000000Z",
            date_stamp: "20260101",
            region: "us-east-1",
            payload_hash: &sha256_hex(b"hello"),
            access_key: "AKIDEXAMPLE",
            secret_key: "secret",
        });
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/s3/aws4_request, "));
        assert!(auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date, "));
        // 64 hex chars of signature, and it is deterministic for fixed inputs.
        let sig = auth.rsplit("Signature=").next().unwrap();
        assert_eq!(sig.len(), 64, "signature should be hex sha256");
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn uri_encoding_keeps_path_separators_but_escapes_the_rest() {
        assert_eq!(uri_encode("acct/1/plan/2/a b.pdf", false), "acct/1/plan/2/a%20b.pdf");
        assert_eq!(uri_encode("a/b", true), "a%2Fb");
        assert_eq!(uri_encode("safe-._~", false), "safe-._~");
    }

    #[test]
    fn object_keys_are_content_addressed_and_extension_safe() {
        assert_eq!(object_key(7, 9, "deadbeef", "pdf"), "acct/7/plan/9/deadbeef.pdf");
        assert_eq!(object_key(7, 9, "deadbeef", ".PDF"), "acct/7/plan/9/deadbeef.pdf");
        assert_eq!(object_key(7, 9, "deadbeef", ""), "acct/7/plan/9/deadbeef");
        // A hostile "extension" cannot escape the key.
        assert_eq!(object_key(7, 9, "deadbeef", "../../etc/passwd"), "acct/7/plan/9/deadbeef.etcpassw");
    }
}

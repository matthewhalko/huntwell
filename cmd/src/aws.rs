//! AWS request signing (SigV4) and Secrets Manager.
//!
//! No AWS SDK: two requests are signed in this whole program, and the signing
//! is sixty lines of HMAC. An SDK would bring a runtime, a credential chain and
//! a release cadence for that.
//!
//! # Where secrets come from
//!
//! ```text
//! genesis        a key compiled into the binary
//!   opens
//! global         sealed on disk; holds only the AWS key and secret
//!   authenticates
//! Secrets Manager   Huntwell_Local | Huntwell_Production
//!   supplies
//! HUNTWELL_DATABASE_URL, CURSOR_API_KEY, everything else
//! ```
//!
//! So `global` is a bootstrap credential and nothing more. Everything an
//! operator would otherwise hand-edit lives in Secrets Manager, where it can be
//! rotated without touching the host or rebuilding an image.

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Everything one signature needs. Taken as a struct rather than nine
/// arguments because two of them are secrets and the rest are strings, and a
/// transposed pair of those is a signature mismatch with no other symptom.
pub struct Signed<'a> {
    pub method: &'a str,
    pub canonical_uri: &'a str,
    pub canonical_query: &'a str,
    pub host: &'a str,
    /// Sorted, lowercased, `name:value\n` per line. Must agree with
    /// `signed_headers` exactly — AWS rejects any disagreement identically to a
    /// wrong key, which is why they are built together by the callers below.
    pub canonical_headers: &'a str,
    pub signed_headers: &'a str,
    pub amz_date: &'a str,
    pub date_stamp: &'a str,
    pub region: &'a str,
    pub service: &'a str,
    pub payload_hash: &'a str,
    pub access_key: &'a str,
    pub secret_key: &'a str,
}

/// The `Authorization` header for one request. Split out from sending so the
/// signature math is testable without a network.
pub fn authorization(i: Signed<'_>) -> String {
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        i.method, i.canonical_uri, i.canonical_query, i.canonical_headers, i.signed_headers, i.payload_hash
    );
    let scope = format!("{}/{}/{}/aws4_request", i.date_stamp, i.region, i.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        i.amz_date,
        scope,
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let key = signing_key(i.secret_key, i.date_stamp, i.region, i.service);
    let signature = hex::encode(hmac(&key, string_to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        i.access_key, scope, i.signed_headers, signature
    )
}

pub fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac accepts any key length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

pub fn signing_key(secret: &str, date_stamp: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    hmac(&k_service, b"aws4_request")
}

// ---------------------------------------------------------------------------
// Secrets Manager
// ---------------------------------------------------------------------------

/// The credentials that open Secrets Manager, and where to look.
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    /// For temporary credentials (an instance role, or `aws sts assume-role`).
    pub session_token: Option<String>,
    pub region: String,
}

/// Reads the bootstrap credentials. These are the one thing that cannot come
/// from Secrets Manager itself, so they are read only from the environment and
/// the `global` file — never through the full settings chain, which would be
/// circular.
pub fn credentials(get: impl Fn(&str) -> Option<String>) -> Option<Credentials> {
    // The AWS_* spellings are what every other AWS tool uses, so a box already
    // set up for the CLI needs nothing added.
    let pick = |names: &[&str]| -> Option<String> {
        names.iter().find_map(|n| get(n).filter(|v| !v.trim().is_empty()))
    };
    let access_key = pick(&["HUNTWELL_AWS_ACCESS_KEY_ID", "AWS_ACCESS_KEY_ID"])?;
    let secret_key = pick(&["HUNTWELL_AWS_SECRET_ACCESS_KEY", "AWS_SECRET_ACCESS_KEY"])?;
    Some(Credentials {
        access_key,
        secret_key,
        session_token: pick(&["HUNTWELL_AWS_SESSION_TOKEN", "AWS_SESSION_TOKEN"]),
        region: pick(&["HUNTWELL_AWS_REGION", "AWS_REGION"]).unwrap_or_else(|| "us-east-1".into()),
    })
}

/// Credentials for acting on one service: `AWS_SES_KEY` / `AWS_SES_SECRET`,
/// `AWS_S3_KEY` / `AWS_S3_SECRET`, and so on.
///
/// **The bootstrap credential is deliberately not accepted here.** That one
/// exists to read Secrets Manager and should be able to do nothing else; if it
/// were a silent fallback, an install with no SES key would quietly send mail
/// with a credential scoped to secrets — and the day someone narrows that
/// credential correctly, mail would break somewhere unrelated.
///
/// So each service gets its own IAM user with one permission, and a missing one
/// is reported as missing rather than papered over.
pub fn service_credentials(prefix: &str, region: &str) -> Result<Credentials> {
    service_credentials_from(prefix, region, crate::config::get)
}

/// The same, over any settings source. Separate so the test does not have to
/// mutate the process environment — `set_var` is not thread-safe, and tests run
/// in parallel, so an env-poking test makes some *other* test flaky.
pub fn service_credentials_from(
    prefix: &str,
    region: &str,
    get: impl Fn(&str) -> Option<String>,
) -> Result<Credentials> {
    let need = |name: &str| -> Result<String> {
        get(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow!("{name} is not set — add it to this environment's secret"))
    };
    Ok(Credentials {
        access_key: need(&format!("AWS_{prefix}_KEY"))?,
        secret_key: need(&format!("AWS_{prefix}_SECRET"))?,
        session_token: get(&format!("AWS_{prefix}_SESSION_TOKEN")),
        region: region.to_string(),
    })
}

/// Whether a service has its own credentials, without building them.
pub fn has_service_credentials(prefix: &str) -> bool {
    has_service_credentials_in(prefix, crate::config::get)
}

pub fn has_service_credentials_in(prefix: &str, get: impl Fn(&str) -> Option<String>) -> bool {
    let set = |n: &str| get(n).map(|v| !v.trim().is_empty()).unwrap_or(false);
    set(&format!("AWS_{prefix}_KEY")) && set(&format!("AWS_{prefix}_SECRET"))
}

/// Fetch one secret's value. Returns the raw string AWS holds — which for our
/// secrets is a JSON object of setting name to value.
pub async fn get_secret(creds: &Credentials, secret_id: &str) -> Result<String> {
    // An endpoint override for a VPC endpoint, or a stand-in under test.
    let (host, url) = match crate::config::get("HUNTWELL_AWS_SECRETS_ENDPOINT") {
        Some(e) if !e.trim().is_empty() => {
            let e = e.trim().trim_end_matches('/').to_string();
            let h = e.split("://").nth(1).unwrap_or(&e).split('/').next().unwrap_or("").to_string();
            (h, format!("{e}/"))
        }
        _ => {
            let h = format!("secretsmanager.{}.amazonaws.com", creds.region);
            let u = format!("https://{h}/");
            (h, u)
        }
    };
    let body = serde_json::json!({ "SecretId": secret_id }).to_string();
    let payload_hash = hex::encode(Sha256::digest(body.as_bytes()));

    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();

    // Secrets Manager is JSON-1.1 RPC: the operation is a header, not a path.
    const TARGET: &str = "secretsmanager.GetSecretValue";
    const CONTENT_TYPE: &str = "application/x-amz-json-1.1";

    // Headers and signed_headers are built together, in the sorted order AWS
    // requires. A session token must be signed when present, which is why this
    // is assembled rather than written out.
    let mut headers: Vec<(&str, String)> = vec![
        ("content-type", CONTENT_TYPE.to_string()),
        ("host", host.clone()),
        ("x-amz-date", amz_date.clone()),
    ];
    if let Some(t) = &creds.session_token {
        headers.push(("x-amz-security-token", t.clone()));
    }
    headers.push(("x-amz-target", TARGET.to_string()));
    headers.sort_by(|a, b| a.0.cmp(b.0));
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(";");

    let auth = authorization(Signed {
        method: "POST",
        canonical_uri: "/",
        canonical_query: "",
        host: &host,
        canonical_headers: &canonical_headers,
        signed_headers: &signed_headers,
        amz_date: &amz_date,
        date_stamp: &date_stamp,
        region: &creds.region,
        service: "secretsmanager",
        payload_hash: &payload_hash,
        access_key: &creds.access_key,
        secret_key: &creds.secret_key,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .context("build the Secrets Manager client")?;
    let mut req = client
        .post(&url)
        .header("content-type", CONTENT_TYPE)
        .header("x-amz-target", TARGET)
        .header("x-amz-date", &amz_date)
        .header("authorization", auth)
        .body(body);
    if let Some(t) = &creds.session_token {
        req = req.header("x-amz-security-token", t);
    }

    let res = req.send().await.context("call Secrets Manager")?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        // The body names the failure (AccessDeniedException,
        // ResourceNotFoundException); a bare status would send someone hunting.
        return Err(anyhow!("Secrets Manager returned {status} for {secret_id}: {}", text.trim()));
    }
    let v: serde_json::Value = serde_json::from_str(&text).context("parse the Secrets Manager reply")?;
    v.get("SecretString")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        // A binary secret is a deliberate choice by whoever wrote it, and not
        // one this program knows how to read.
        .ok_or_else(|| anyhow!("{secret_id} has no SecretString — a binary secret cannot be settings"))
}

/// A secret's JSON object, flattened to settings. Values that are not strings
/// are rendered as JSON, so a number or a boolean in the console still arrives
/// as something readable rather than being silently dropped.
pub fn parse_settings(raw: &str) -> Result<Vec<(String, String)>> {
    let v: serde_json::Value = serde_json::from_str(raw).context("the secret is not JSON")?;
    let obj = v.as_object().ok_or_else(|| anyhow!("the secret is JSON but not an object of name to value"))?;
    let mut out: Vec<(String, String)> = obj
        .iter()
        .map(|(k, val)| {
            let s = match val {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), s)
        })
        .collect();
    assemble_urls(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// SES
// ---------------------------------------------------------------------------

/// Is SES configured well enough to try?
///
/// Both halves: a From address SES has verified, and a credential scoped to
/// sending. Either alone is a misconfiguration that would fail per message.
pub fn ses_configured() -> bool {
    crate::config::get("HUNTWELL_MAIL_FROM").map(|v| !v.trim().is_empty()).unwrap_or(false)
        && has_service_credentials("SES")
}

fn ses_region() -> String {
    crate::config::get("HUNTWELL_SES_REGION")
        .or_else(|| crate::config::get("AWS_REGION"))
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "us-east-1".into())
}

/// Send one message through SES v2. Returns the SES message id.
pub async fn ses_send(to: &str, from: &str, subject: &str, html: &str, text: &str) -> Result<String> {
    let region = ses_region();
    let creds = service_credentials("SES", &region)?;
    let path = "/v2/email/outbound-emails";
    // An endpoint override for a VPC endpoint, or a stand-in under test.
    let (host, url) = match crate::config::get("HUNTWELL_SES_ENDPOINT") {
        Some(e) if !e.trim().is_empty() => {
            let e = e.trim().trim_end_matches('/').to_string();
            let h = e.split("://").nth(1).unwrap_or(&e).split('/').next().unwrap_or("").to_string();
            (h, format!("{e}{path}"))
        }
        _ => {
            let h = format!("email.{region}.amazonaws.com");
            let u = format!("https://{h}{path}");
            (h, u)
        }
    };

    // SES v2 is a REST-JSON API: the operation is the path, the body is the
    // message. Text and HTML both, so a client that refuses HTML still reads it.
    let mut content = serde_json::json!({
        "Simple": {
            "Subject": { "Data": subject, "Charset": "UTF-8" },
            "Body": {
                "Html": { "Data": html, "Charset": "UTF-8" },
                "Text": { "Data": text, "Charset": "UTF-8" }
            }
        }
    });
    if text.trim().is_empty() {
        content["Simple"]["Body"].as_object_mut().map(|b| b.remove("Text"));
    }
    let mut payload = serde_json::json!({
        "FromEmailAddress": from,
        "Destination": { "ToAddresses": [to] },
        "Content": content,
    });
    if let Some(set) = crate::config::get("HUNTWELL_SES_CONFIGURATION_SET").filter(|v| !v.trim().is_empty()) {
        payload["ConfigurationSetName"] = serde_json::Value::String(set);
    }
    let body = payload.to_string();
    let payload_hash = hex::encode(Sha256::digest(body.as_bytes()));

    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();

    let mut headers: Vec<(&str, String)> = vec![
        ("content-type", "application/json".to_string()),
        ("host", host.clone()),
        ("x-amz-date", amz_date.clone()),
    ];
    if let Some(t) = &creds.session_token {
        headers.push(("x-amz-security-token", t.clone()));
    }
    headers.sort_by(|a, b| a.0.cmp(b.0));
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(";");

    let auth = authorization(Signed {
        method: "POST",
        canonical_uri: path,
        canonical_query: "",
        host: &host,
        canonical_headers: &canonical_headers,
        signed_headers: &signed_headers,
        amz_date: &amz_date,
        date_stamp: &date_stamp,
        region: &region,
        service: "ses",
        payload_hash: &payload_hash,
        access_key: &creds.access_key,
        secret_key: &creds.secret_key,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .context("build the SES client")?;
    let mut req = client
        .post(&url)
        .header("content-type", "application/json")
        .header("x-amz-date", &amz_date)
        .header("authorization", auth)
        .body(body);
    if let Some(t) = &creds.session_token {
        req = req.header("x-amz-security-token", t);
    }

    let res = req.send().await.context("call SES")?;
    let status = res.status();
    let text_body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        // The body names the cause — an unverified From address and a throttle
        // look identical as a bare status.
        return Err(anyhow!("SES returned {status}: {}", text_body.trim()));
    }
    let v: serde_json::Value = serde_json::from_str(&text_body).unwrap_or_default();
    Ok(v.get("MessageId").and_then(|m| m.as_str()).unwrap_or("").to_string())
}

// ---------------------------------------------------------------------------
// Connection strings, assembled from parts
// ---------------------------------------------------------------------------

/// Where a connection's parts are looked up, most specific first.
///
/// `HUNTWELL_PG_HOST` is the name to write; `PG_HOST` and `DB_HOST` are
/// accepted because that is what an RDS-managed secret and most examples call
/// them, and a secret that almost works is worse than one that does not.
const PG_PREFIXES: [&str; 3] = ["HUNTWELL_PG", "PG", "DB"];

/// The pool URL — the database *as the worker pods reach it* — falls back to
/// the main connection, because on one server they are the same machine.
const POOL_PREFIXES: [&str; 4] = ["POOL_PG", "HUNTWELL_PG", "PG", "DB"];

/// Compose `HUNTWELL_DATABASE_URL` from separate fields when it is not given
/// outright.
///
/// Storing host, port, username, password and database separately is the point:
/// a managed rotation replaces one field and the next fetch picks it up, with no
/// URL anywhere to rewrite. An explicit URL always wins — assembling is the
/// fallback, never an override.
pub fn assemble_urls(map: &mut Vec<(String, String)>) {
    let snapshot: std::collections::HashMap<String, String> = map.iter().cloned().collect();
    let has = |k: &str| snapshot.get(k).map(|v| !v.trim().is_empty()).unwrap_or(false);

    if !has("HUNTWELL_DATABASE_URL") {
        if let Some(url) = compose(&snapshot, &PG_PREFIXES, "huntwell") {
            map.push(("HUNTWELL_DATABASE_URL".into(), url));
        }
    }
    // Only when something actually names a pool host: inventing one would point
    // every worker pod at a database that may not be reachable from a pod.
    if !has("HUNTWELL_POOL_DATABASE_URL") && field(&snapshot, &["POOL_PG"], "HOST", &[]).is_some() {
        if let Some(url) = compose(&snapshot, &POOL_PREFIXES, "huntwell") {
            map.push(("HUNTWELL_POOL_DATABASE_URL".into(), url));
        }
    }
}

/// One field, tried against every prefix in order. `also` carries the alternate
/// stems a field goes by — `USER` for `USERNAME`, `DBNAME` for `DATABASE`.
fn field(
    map: &std::collections::HashMap<String, String>,
    prefixes: &[&str],
    name: &str,
    also: &[&str],
) -> Option<String> {
    for p in prefixes {
        for stem in std::iter::once(name).chain(also.iter().copied()) {
            if let Some(v) = map.get(&format!("{p}_{stem}")) {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn compose(
    map: &std::collections::HashMap<String, String>,
    prefixes: &[&str],
    default_db: &str,
) -> Option<String> {
    // A host is the one part with no sensible default: without it there is
    // nothing to connect to and guessing localhost would point production at
    // its own pod.
    let host = field(map, prefixes, "HOST", &[])?;
    let port = field(map, prefixes, "PORT", &[]).unwrap_or_else(|| "5432".into());
    let user = field(map, prefixes, "USERNAME", &["USER"]).unwrap_or_else(|| "postgres".into());
    let db = field(map, prefixes, "DATABASE", &["DBNAME", "NAME"]).unwrap_or_else(|| default_db.into());
    let pass = field(map, prefixes, "PASSWORD", &["PASS"]);

    // Percent-encoded, and this is not decoration: a rotated password is
    // generated, so sooner or later it contains an @ or a / and an unencoded
    // one silently reparses the URL into a different host.
    let userinfo = match &pass {
        Some(p) => format!("{}:{}", percent_encode(&user), percent_encode(p)),
        None => percent_encode(&user),
    };
    let mut url = format!("postgres://{userinfo}@{host}:{port}/{db}");
    if let Some(mode) = field(map, prefixes, "SSLMODE", &[]) {
        url.push_str(&format!("?sslmode={mode}"));
    }
    Some(url)
}

/// RFC 3986 unreserved set; everything else escaped. Userinfo only.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AWS publishes a worked example for the signing key; if this drifts,
    /// every request fails with a signature mismatch and no other clue.
    #[test]
    fn signing_key_matches_the_aws_worked_example() {
        let k = signing_key("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "20150830", "us-east-1", "iam");
        assert_eq!(
            hex::encode(k),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    #[test]
    fn the_service_reaches_the_scope_and_the_key() {
        // The whole reason this is shared with objstore: same maths, different
        // service, and getting that wrong is a signature mismatch.
        let a = authorization(Signed {
            method: "POST", canonical_uri: "/", canonical_query: "", host: "h",
            canonical_headers: "host:h\n", signed_headers: "host",
            amz_date: "20260101T000000Z", date_stamp: "20260101", region: "us-east-1",
            service: "secretsmanager", payload_hash: "abc",
            access_key: "AKIDEXAMPLE", secret_key: "secret",
        });
        assert!(a.contains("/20260101/us-east-1/secretsmanager/aws4_request"), "{a}");
        assert!(a.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"), "{a}");
    }

    #[test]
    fn settings_come_out_of_the_json_object() {
        let got = parse_settings(r#"{"HUNTWELL_DATABASE_URL":"postgres://x","PORT":8611,"ON":true}"#).unwrap();
        let map: std::collections::HashMap<_, _> = got.into_iter().collect();
        assert_eq!(map["HUNTWELL_DATABASE_URL"], "postgres://x");
        // Not dropped just because someone typed a number into the console.
        assert_eq!(map["PORT"], "8611");
        assert_eq!(map["ON"], "true");
    }

    fn settings(json: &str) -> std::collections::HashMap<String, String> {
        parse_settings(json).unwrap().into_iter().collect()
    }

    #[test]
    fn the_database_url_is_assembled_from_its_parts() {
        // The shape a rotation-friendly secret has: one field per part, so a
        // new password is one edit and no URL needs rewriting.
        let m = settings(
            r#"{"HUNTWELL_PG_HOST":"db.internal","HUNTWELL_PG_PORT":"5432",
                "HUNTWELL_PG_USERNAME":"huntwell","HUNTWELL_PG_PASSWORD":"s3cret",
                "HUNTWELL_PG_DATABASE":"huntwell","HUNTWELL_PG_SSLMODE":"require"}"#,
        );
        assert_eq!(
            m["HUNTWELL_DATABASE_URL"],
            "postgres://huntwell:s3cret@db.internal:5432/huntwell?sslmode=require"
        );
    }

    #[test]
    fn an_explicit_url_is_never_overridden() {
        let m = settings(
            r#"{"HUNTWELL_DATABASE_URL":"postgres://written/by-hand",
                "HUNTWELL_PG_HOST":"db.internal","HUNTWELL_PG_PASSWORD":"x"}"#,
        );
        assert_eq!(m["HUNTWELL_DATABASE_URL"], "postgres://written/by-hand");
    }

    #[test]
    fn a_rotated_password_with_punctuation_does_not_break_the_url() {
        // The failure this prevents: an unencoded @ reparses the URL so the
        // host becomes something else entirely, and the error names a host
        // nobody configured.
        let m = settings(r#"{"HUNTWELL_PG_HOST":"db","HUNTWELL_PG_PASSWORD":"p@ss/wo rd:!"}"#);
        let url = &m["HUNTWELL_DATABASE_URL"];
        assert!(url.contains("p%40ss%2Fwo%20rd%3A%21"), "{url}");
        // Exactly one @, the one separating userinfo from host.
        assert_eq!(url.matches('@').count(), 1, "{url}");
        assert!(url.contains("@db:5432/huntwell"), "{url}");
    }

    #[test]
    fn the_generic_spellings_are_accepted_too() {
        // What an RDS-managed secret looks like.
        let m = settings(r#"{"PG_HOST":"rds.aws","PG_USER":"admin","PG_PASSWORD":"p","PG_DBNAME":"hw"}"#);
        assert_eq!(m["HUNTWELL_DATABASE_URL"], "postgres://admin:p@rds.aws:5432/hw");
    }

    #[test]
    fn no_host_means_no_url_rather_than_a_wrong_one() {
        // Guessing localhost would point production at its own pod.
        let m = settings(r#"{"HUNTWELL_PG_PASSWORD":"p","HUNTWELL_PG_DATABASE":"hw"}"#);
        assert!(!m.contains_key("HUNTWELL_DATABASE_URL"), "{m:?}");
    }

    #[test]
    fn the_pool_url_is_only_built_when_asked_for() {
        // It exists to say "the pods reach the database somewhere else". With
        // no POOL_PG_HOST there is nothing to say, and inventing one would send
        // every worker pod at an address that may not resolve from a pod.
        let m = settings(r#"{"HUNTWELL_PG_HOST":"db","HUNTWELL_PG_PASSWORD":"p"}"#);
        assert!(!m.contains_key("HUNTWELL_POOL_DATABASE_URL"));

        let m = settings(r#"{"HUNTWELL_PG_HOST":"127.0.0.1","HUNTWELL_PG_PASSWORD":"p",
                             "POOL_PG_HOST":"db.internal"}"#);
        // Inherits the credentials it did not restate.
        assert_eq!(m["HUNTWELL_POOL_DATABASE_URL"], "postgres://postgres:p@db.internal:5432/huntwell");
        assert_eq!(m["HUNTWELL_DATABASE_URL"], "postgres://postgres:p@127.0.0.1:5432/huntwell");
    }

    #[test]
    fn a_secret_that_is_not_an_object_is_refused() {
        assert!(parse_settings("[1,2]").is_err(), "an array is not settings");
        assert!(parse_settings("not json").is_err());
    }

    #[test]
    fn a_service_credential_never_falls_back_to_the_bootstrap_one() {
        // The property that makes scoping real: with only the bootstrap keys
        // present, asking for SES must fail — not quietly send mail with the
        // credential that reads secrets.
        let src = |pairs: Vec<(&str, &str)>| {
            let m: std::collections::HashMap<String, String> =
                pairs.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            move |k: &str| m.get(k).cloned()
        };

        let bootstrap_only = src(vec![
            ("AWS_ACCESS_KEY_ID", "BOOTSTRAP"),
            ("AWS_SECRET_ACCESS_KEY", "BOOTSTRAP"),
        ]);
        assert!(!has_service_credentials_in("SES", &bootstrap_only));
        // Matched rather than `unwrap_err`, which would need Debug on
        // Credentials — and a derived Debug on a struct holding a secret key is
        // how the key reaches a log line.
        let e = match service_credentials_from("SES", "us-east-1", &bootstrap_only) {
            Ok(_) => panic!("SES must not borrow the bootstrap credential"),
            Err(e) => e.to_string(),
        };
        assert!(e.contains("AWS_SES_KEY"), "the error should name what is missing: {e}");

        let scoped = src(vec![("AWS_SES_KEY", "SESKEY"), ("AWS_SES_SECRET", "SESSECRET")]);
        assert!(has_service_credentials_in("SES", &scoped));
        let c = service_credentials_from("SES", "eu-west-1", &scoped).unwrap();
        assert_eq!(c.access_key, "SESKEY");
        assert_eq!(c.region, "eu-west-1");
    }

    #[test]
    fn credentials_accept_either_spelling_and_need_both_halves() {
        let with = |pairs: &[(&str, &str)]| {
            let m: std::collections::HashMap<String, String> =
                pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            credentials(move |k| m.get(k).cloned())
        };
        assert!(with(&[("AWS_ACCESS_KEY_ID", "A"), ("AWS_SECRET_ACCESS_KEY", "B")]).is_some());
        assert!(with(&[("HUNTWELL_AWS_ACCESS_KEY_ID", "A"), ("HUNTWELL_AWS_SECRET_ACCESS_KEY", "B")]).is_some());
        // Half a credential is not a credential; it must not half-configure.
        assert!(with(&[("AWS_ACCESS_KEY_ID", "A")]).is_none());
        assert!(with(&[("AWS_ACCESS_KEY_ID", ""), ("AWS_SECRET_ACCESS_KEY", "B")]).is_none());
        let c = with(&[("AWS_ACCESS_KEY_ID", "A"), ("AWS_SECRET_ACCESS_KEY", "B")]).unwrap();
        assert_eq!(c.region, "us-east-1", "a missing region should not be a failure");
    }
}

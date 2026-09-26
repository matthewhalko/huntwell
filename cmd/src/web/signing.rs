//! Signed API requests, the way an exchange API does it — with the body signed
//! and every signature good for one use.
//!
//! A bearer token is the whole credential: whoever sees one can replay it, and
//! it crosses the wire on every call. A signed request is different — the
//! **key** identifies the caller and travels; the **secret** only ever signs,
//! and never leaves either end.
//!
//! ```text
//! X-HW-KEY:   hwk_live_9f2c…          the key, in the clear
//! X-HW-TS:    1758579312000           when the request was made, ms since epoch
//! X-HW-NONCE: 4f1d9c0e2b7a…           random, new for every request (16–64 of [A-Za-z0-9_-])
//! X-HW-SIGN:  3b1f…                   HMAC-SHA256(secret, payload), hex
//! ```
//!
//! The payload is six lines:
//!
//! ```text
//! METHOD\nPATH\nQUERY\nTIMESTAMP\nNONCE\nSHA256(BODY)
//! ```
//!
//! `SHA256(BODY)` is the lowercase hex digest of the exact bytes sent — for a
//! request with no body, the digest of nothing (`e3b0c442…b855`). Change any
//! line — the method, the path, the query, the time, the nonce or one byte of
//! the body — and the signature stops matching.
//!
//! The body is hashed as it arrives, before any handler parses it
//! (`download::stamp_signed_parts`, layered on `/v1` and `/dl`), and handed to
//! the check in a header nothing outside can set. So what is verified is what
//! the handler then reads, byte for byte.
//!
//! Three checks beyond the signature itself:
//!
//!   - **The timestamp must be recent** (30s, plus a little slack for a clock
//!     that runs fast).
//!   - **The nonce must be new.** Each (key, nonce) is remembered for the
//!     window, and a second request with the same one is refused — so a
//!     request captured in flight cannot be sent again even within its 30s.
//!   - **The key must carry a secret.** Keys from before signing have none and
//!     are refused, told to make a new key.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// How far out of date a signed request may be, in milliseconds.
const RECV_WINDOW_MS: i64 = 30_000;

/// How far *ahead* a caller's clock may be. Generous: a clock a few seconds
/// fast is common and is not an attack.
const FUTURE_SLACK_MS: i64 = 5_000;

/// The digest of an empty body, which is what a GET signs.
pub const EMPTY_BODY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

pub struct Presented {
    pub key: String,
    pub timestamp: i64,
    pub nonce: String,
    pub signature: String,
}

/// The signing headers, if this request carries them. A nonce that is missing
/// or not of the allowed shape makes it an unsigned request.
pub fn presented(headers: &HeaderMap) -> Option<Presented> {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty());
    let nonce = get("x-hw-nonce")?;
    if !(16..=64).contains(&nonce.len()) || !nonce.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    Some(Presented {
        key: get("x-hw-key")?.to_string(),
        timestamp: get("x-hw-ts")?.parse().ok()?,
        nonce: nonce.to_string(),
        signature: get("x-hw-sign")?.to_string(),
    })
}

/// Whether the caller sent signing headers at all, complete or not — so an
/// incomplete set can be told apart from a bearer token and explained.
pub fn attempted(headers: &HeaderMap) -> bool {
    headers.contains_key("x-hw-key") || headers.contains_key("x-hw-sign")
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Exactly what gets signed.
///
/// Everything that decides what the request does, in a fixed order, separated
/// by newlines so no field can be slid into the next one. The query is the raw
/// string as sent — re-encoding it here and there is the usual way a signing
/// scheme ends up rejecting honest callers.
pub fn payload(method: &str, path: &str, query: &str, timestamp: i64, nonce: &str, body_sha256: &str) -> Vec<u8> {
    format!("{}\n{path}\n{query}\n{timestamp}\n{nonce}\n{body_sha256}", method.to_ascii_uppercase()).into_bytes()
}

pub fn sign(secret: &str, payload: &[u8]) -> String {
    let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(payload);
    hex::encode(mac.finalize().into_bytes())
}

/// Why a signed request was refused. The caller is told which, because every
/// one of these is the caller's to fix and none of them says anything about
/// another account.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The timestamp is outside the window — usually a clock, sometimes a replay.
    Stale,
    /// The signature does not match what we computed.
    BadSignature,
    /// This nonce was already used with this key.
    Replayed,
}

impl Refused {
    pub fn message(&self) -> &'static str {
        match self {
            Refused::Stale => "that request's timestamp is outside the 30s window — check the clock on the calling machine",
            Refused::BadSignature => {
                "the signature does not match — sign METHOD\\nPATH\\nQUERY\\nTIMESTAMP\\nNONCE\\nSHA256(BODY) with your API secret, using the exact body bytes you send"
            }
            Refused::Replayed => "that nonce was already used — send a new random X-HW-NONCE with every request",
        }
    }
}

/// Check a signed request against the secret we hold for that key.
/// `body_sha256` is the digest of the body as it arrived.
#[allow(clippy::too_many_arguments)]
pub fn verify(
    secret: &str,
    presented: &Presented,
    method: &str,
    path: &str,
    query: &str,
    body_sha256: &str,
    now_ms: i64,
) -> Result<(), Refused> {
    let age = now_ms - presented.timestamp;
    if age > RECV_WINDOW_MS || age < -FUTURE_SLACK_MS {
        return Err(Refused::Stale);
    }
    let expected = sign(secret, &payload(method, path, query, presented.timestamp, &presented.nonce, body_sha256));
    // Constant time: a byte-by-byte comparison tells an attacker how much of a
    // guessed signature was right, which is enough to find the rest.
    if !constant_time_eq(expected.as_bytes(), presented.signature.to_ascii_lowercase().as_bytes()) {
        return Err(Refused::BadSignature);
    }
    Ok(())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Nonces seen, by (key, nonce), with when. Only a request that already
/// passed its signature gets here, so nobody without a secret can fill it.
/// In memory: the website is one process, and a nonce only has to be
/// remembered for as long as its timestamp would still be accepted.
fn seen_nonces() -> &'static Mutex<HashMap<(i64, String), i64>> {
    static SEEN: std::sync::OnceLock<Mutex<HashMap<(i64, String), i64>>> = std::sync::OnceLock::new();
    SEEN.get_or_init(Default::default)
}

/// Record a verified request's nonce. `Err(Replayed)` if this key used it already.
pub fn claim_nonce(key_id: i64, nonce: &str, now_ms: i64) -> Result<(), Refused> {
    let mut seen = seen_nonces().lock().unwrap_or_else(|e| e.into_inner());
    // Anything older than the window can no longer pass the timestamp check,
    // so it need not be remembered.
    let horizon = RECV_WINDOW_MS + FUTURE_SLACK_MS;
    if seen.len() > 10_000 {
        seen.retain(|_, at| now_ms - *at <= horizon);
    }
    let k = (key_id, nonce.to_string());
    match seen.get(&k) {
        Some(at) if now_ms - *at <= horizon => Err(Refused::Replayed),
        _ => {
            seen.insert(k, now_ms);
            Ok(())
        }
    }
}

/// The raw query string, as sent. Rebuilt from the parsed map only when there
/// is no original to hand — so a caller's own ordering and encoding is what
/// gets signed and what gets checked.
pub fn query_string(uri: &axum::http::Uri, parsed: &HashMap<String, String>) -> String {
    match uri.query() {
        Some(q) => q.to_string(),
        None if parsed.is_empty() => String::new(),
        None => parsed.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&"),
    }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---------------------------------------------------------------------------
// Keeping the secret
// ---------------------------------------------------------------------------

/// A signing secret has to be **recoverable**, not merely checkable: verifying
/// an HMAC means computing it, which means holding the same secret the caller
/// holds. A token can be stored as a hash; this cannot.
///
/// So it is encrypted at rest instead, under a key that lives in Secrets
/// Manager rather than in the database. Someone who reads the `api_key` table
/// gets ciphertext; opening it needs the deployment's own secret as well.
mod sealed {
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
    use base64::Engine;

    type Enc = cbc::Encryptor<aes::Aes256>;
    type Dec = cbc::Decryptor<aes::Aes256>;

    /// AES-256 key for API secrets, from the deployment's own secret.
    ///
    /// Derived rather than used directly so this key is not the session key:
    /// two purposes, two keys, and neither can be used to attack the other.
    fn key() -> Option<[u8; 32]> {
        let base = crate::config::get("HUNTWELL_API_SIGNING_KEY")
            .or_else(|| crate::config::get("HUNTWELL_SESSION_SECRET"))
            .filter(|v| v.trim().len() >= 16)?;
        let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
        sha2::Digest::update(&mut hasher, b"huntwell-api-signing-v1");
        sha2::Digest::update(&mut hasher, base.trim().as_bytes());
        Some(sha2::Digest::finalize(hasher).into())
    }

    /// `base64(iv || ciphertext)`. A fresh IV each time, so the same secret
    /// stored twice does not look the same twice.
    pub fn seal(plain: &str) -> Option<String> {
        let key = key()?;
        let mut iv = [0u8; 16];
        {
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut iv);
        }
        let ct = Enc::new(&key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(plain.as_bytes());
        let mut out = iv.to_vec();
        out.extend_from_slice(&ct);
        Some(base64::engine::general_purpose::STANDARD.encode(out))
    }

    pub fn open(sealed: &str) -> Option<String> {
        let key = key()?;
        let raw = base64::engine::general_purpose::STANDARD.decode(sealed.trim()).ok()?;
        if raw.len() <= 16 {
            return None;
        }
        let (iv, ct) = raw.split_at(16);
        let iv: [u8; 16] = iv.try_into().ok()?;
        let plain = Dec::new(&key.into(), &iv.into()).decrypt_padded_vec_mut::<Pkcs7>(ct).ok()?;
        String::from_utf8(plain).ok()
    }
}

pub use sealed::{open as open_secret, seal as seal_secret};

/// A new signing secret. Long enough that guessing is not a strategy, and
/// prefixed so one found in a log or a repository is recognisable.
pub fn new_secret() -> String {
    format!("hws_{}", crate::store::new_api_token())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "hws_live_2f6c8a1d";
    const NONCE: &str = "4f1d9c0e2b7a55aa";

    fn headers(key: &str, ts: i64, sign: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-hw-key", key.parse().unwrap());
        h.insert("x-hw-ts", ts.to_string().parse().unwrap());
        h.insert("x-hw-nonce", NONCE.parse().unwrap());
        h.insert("x-hw-sign", sign.parse().unwrap());
        h
    }

    fn signed(method: &str, path: &str, query: &str, ts: i64, body: &[u8]) -> String {
        sign(SECRET, &payload(method, path, query, ts, NONCE, &sha256_hex(body)))
    }

    #[test]
    fn a_correctly_signed_request_is_accepted() {
        let now = 1_758_579_312_000;
        let body = br#"{"source":"Chicago advisors"}"#;
        let sig = signed("POST", "/v1/plans", "", now, body);
        let p = presented(&headers("hwk_live_9f2c", now, &sig)).expect("the headers are there");
        assert_eq!(p.key, "hwk_live_9f2c");
        assert!(verify(SECRET, &p, "POST", "/v1/plans", "", &sha256_hex(body), now + 200).is_ok());
        assert_eq!(sha256_hex(b""), EMPTY_BODY_SHA256);
    }

    #[test]
    fn nothing_the_signature_covers_can_be_changed_in_flight() {
        let now = 1_758_579_312_000;
        let body = br#"{"plan_id":7}"#;
        let h = sha256_hex(body);
        let sig = signed("POST", "/v1/plans/7/run", "dry=1", now, body);
        let p = presented(&headers("k", now, &sig)).unwrap();
        // As sent: fine.
        assert!(verify(SECRET, &p, "POST", "/v1/plans/7/run", "dry=1", &h, now).is_ok());
        // Every one of these is a different request, and none of them verifies.
        assert_eq!(verify(SECRET, &p, "DELETE", "/v1/plans/7/run", "dry=1", &h, now), Err(Refused::BadSignature));
        assert_eq!(verify(SECRET, &p, "POST", "/v1/plans/9/run", "dry=1", &h, now), Err(Refused::BadSignature));
        assert_eq!(verify(SECRET, &p, "POST", "/v1/plans/7/run", "dry=0", &h, now), Err(Refused::BadSignature));
        // One byte of the body changed — the thing the old scheme could not see.
        let edited = sha256_hex(br#"{"plan_id":8}"#);
        assert_eq!(verify(SECRET, &p, "POST", "/v1/plans/7/run", "dry=1", &edited, now), Err(Refused::BadSignature));
        // A body added to a request signed as having none.
        assert_eq!(verify(SECRET, &p, "POST", "/v1/plans/7/run", "dry=1", EMPTY_BODY_SHA256, now), Err(Refused::BadSignature));
        // And another account's secret does not open it.
        assert_eq!(verify("someone-elses-secret", &p, "POST", "/v1/plans/7/run", "dry=1", &h, now), Err(Refused::BadSignature));
    }

    #[test]
    fn the_nonce_is_signed_and_used_once() {
        let now = 1_758_579_312_000;
        let sig = signed("GET", "/v1/me", "", now, b"");
        let p = presented(&headers("k", now, &sig)).unwrap();
        assert!(verify(SECRET, &p, "GET", "/v1/me", "", EMPTY_BODY_SHA256, now).is_ok());
        // The same signature with the nonce swapped does not verify.
        let mut swapped = headers("k", now, &sig);
        swapped.insert("x-hw-nonce", "0000000000000000".parse().unwrap());
        assert_eq!(verify(SECRET, &presented(&swapped).unwrap(), "GET", "/v1/me", "", EMPTY_BODY_SHA256, now), Err(Refused::BadSignature));
        // Used once: fine. Twice within the window: refused. Another key: its own.
        let key = 900_000 + (now % 1000);
        assert!(claim_nonce(key, "nonce-used-once-abc", now).is_ok());
        assert_eq!(claim_nonce(key, "nonce-used-once-abc", now + 5_000), Err(Refused::Replayed));
        assert!(claim_nonce(key + 1, "nonce-used-once-abc", now).is_ok());
    }

    #[test]
    fn a_nonce_must_look_like_one() {
        let mut h = headers("k", 1, "sig");
        for bad in ["short", "has space in it here", "way-too-long-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "semi;colon;semi;colon"] {
            h.insert("x-hw-nonce", bad.parse().unwrap());
            assert!(presented(&h).is_none(), "{bad}");
        }
        h.remove("x-hw-nonce");
        assert!(presented(&h).is_none(), "no nonce, not a signed request");
    }

    #[test]
    fn a_captured_request_stops_working_within_the_window() {
        let then = 1_758_579_312_000;
        let sig = signed("GET", "/v1/artifacts", "", then, b"");
        let p = presented(&headers("k", then, &sig)).unwrap();
        let e = EMPTY_BODY_SHA256;
        assert!(verify(SECRET, &p, "GET", "/v1/artifacts", "", e, then + 29_000).is_ok());
        assert_eq!(verify(SECRET, &p, "GET", "/v1/artifacts", "", e, then + 60_000), Err(Refused::Stale));
        assert!(verify(SECRET, &p, "GET", "/v1/artifacts", "", e, then - 4_000).is_ok());
        assert_eq!(verify(SECRET, &p, "GET", "/v1/artifacts", "", e, then - 60_000), Err(Refused::Stale));
    }

    #[test]
    fn a_partly_right_signature_is_refused_without_saying_how_close_it_was() {
        let now = 1_758_579_312_000;
        let real = signed("GET", "/v1/me", "", now, b"");
        let mut near = real.clone();
        near.pop();
        near.push(if real.ends_with('a') { 'b' } else { 'a' });
        let p = presented(&headers("k", now, &near)).unwrap();
        assert_eq!(verify(SECRET, &p, "GET", "/v1/me", "", EMPTY_BODY_SHA256, now), Err(Refused::BadSignature));
        // Comparison is length-safe too.
        let p = presented(&headers("k", now, "3b1f")).unwrap();
        assert_eq!(verify(SECRET, &p, "GET", "/v1/me", "", EMPTY_BODY_SHA256, now), Err(Refused::BadSignature));
    }

    #[test]
    fn an_unsigned_request_carries_none_of_the_headers() {
        assert!(presented(&HeaderMap::new()).is_none());
        let mut partial = HeaderMap::new();
        partial.insert("x-hw-key", "k".parse().unwrap());
        assert!(presented(&partial).is_none(), "a key without a signature is not a signed request");
    }

    #[test]
    fn a_secret_is_recoverable_but_not_from_the_database_alone() {
        let _env = crate::llm::test_env();
        std::env::set_var("HUNTWELL_API_SIGNING_KEY", "a-deployment-secret-long-enough");
        let secret = new_secret();
        assert!(secret.starts_with("hws_") && secret.len() > 24, "{secret}");

        let sealed = seal_secret(&secret).expect("a key is configured");
        assert!(!sealed.contains(&secret), "the plaintext must not be in there");
        assert_eq!(open_secret(&sealed).as_deref(), Some(secret.as_str()));

        // The same secret sealed twice looks different — no two rows reveal
        // that two keys share a secret.
        assert_ne!(seal_secret(&secret), seal_secret(&secret));

        // And the database alone is not enough: another deployment's key
        // cannot open it.
        std::env::set_var("HUNTWELL_API_SIGNING_KEY", "a-different-deployment-secret-x");
        assert!(open_secret(&sealed).is_none() || open_secret(&sealed).as_deref() != Some(secret.as_str()));
        std::env::remove_var("HUNTWELL_API_SIGNING_KEY");
    }

    #[test]
    fn the_query_is_signed_exactly_as_it_was_sent() {
        // Re-encoding is how a signing scheme ends up rejecting honest callers,
        // so the raw string wins whenever there is one.
        let uri: axum::http::Uri = "/v1/artifacts?plan_id=7&limit=50&q=a%20b".parse().unwrap();
        assert_eq!(query_string(&uri, &HashMap::new()), "plan_id=7&limit=50&q=a%20b");
        let none: axum::http::Uri = "/v1/me".parse().unwrap();
        assert_eq!(query_string(&none, &HashMap::new()), "");
    }
}

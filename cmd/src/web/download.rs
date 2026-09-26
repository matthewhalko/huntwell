//! The signed CSV download API, for cron jobs and spreadsheet importers.
//!
//!   GET /dl/prospects.csv[?plan=<name>][&bom=0]
//!   GET /dl/plans
//!
//! Signed like `/v1` (`web::signing`); bearer tokens and `?token=` are refused.
//! Every attempt is recorded for the owner to see under API Access.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use super::App;
use crate::csv;
use crate::store::{self};

pub fn router() -> Router<App> {
    Router::new()
        .route("/prospects.csv", get(prospects_csv))
        .route("/plans", get(plans))
        .layer(axum::middleware::from_fn(stamp_signed_parts))
}

/// Where `stamp_signed_parts` leaves the query exactly as it arrived.
const RAW_QUERY: &str = "x-hw-raw-query";
/// …and the SHA-256 of the body exactly as it arrived.
const BODY_SHA256: &str = "x-hw-raw-body-sha256";

/// The largest body a signed request may carry. Well above any real request;
/// the limit is so the whole body can be read and hashed before a handler runs.
const MAX_SIGNED_BODY: usize = 1024 * 1024;

/// Records the two things a signature covers that a handler would otherwise
/// see only after parsing: the raw query string and the body's digest. Both go
/// into headers the signature check reads, and any a caller sent under those
/// names are dropped first. The body is read once, hashed, and handed on
/// unchanged — so what was verified is exactly what the handler reads.
/// Layered on `/dl` and `/v1`.
pub(crate) async fn stamp_signed_parts(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let (mut parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_SIGNED_BODY).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "request bodies are limited to 1 MB\n").into_response(),
    };
    let raw = parts.uri.query().unwrap_or("").to_string();
    let h = &mut parts.headers;
    h.remove("x-hw-query");
    h.remove(RAW_QUERY);
    h.remove(BODY_SHA256);
    // A URI's query is visible ASCII, so this cannot fail; if it somehow did,
    // no header means no query is signed, and a request that had one fails.
    if let Ok(v) = HeaderValue::from_str(&raw) {
        h.insert(RAW_QUERY, v);
    }
    if let Ok(v) = HeaderValue::from_str(&crate::web::signing::sha256_hex(&bytes)) {
        h.insert(BODY_SHA256, v);
    }
    next.run(axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes))).await
}

/// The body's digest, stamped by `stamp_signed_parts`. Missing only if the
/// middleware did not run, in which case nothing can verify — the safe way round.
pub(crate) fn body_sha256(headers: &HeaderMap) -> String {
    headers.get(BODY_SHA256).and_then(|v| v.to_str().ok()).unwrap_or("unstamped").to_string()
}

// ---- rate limiting ----------------------------------------------------------

const RATE_PER_SEC: f64 = 1.0;
const RATE_BURST: f64 = 20.0;
const MAX_AUTH_FAILURES: u32 = 8;
const LOCKOUT: Duration = Duration::from_secs(15 * 60);
const FAILURE_WINDOW: Duration = Duration::from_secs(10 * 60);

struct Client {
    tokens: f64,
    last: Instant,
    failures: Vec<Instant>,
    locked_until: Option<Instant>,
}

static CLIENTS: Mutex<Option<HashMap<IpAddr, Client>>> = Mutex::new(None);

fn with_client<T>(ip: IpAddr, f: impl FnOnce(&mut Client) -> T) -> T {
    let mut guard = CLIENTS.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() > 4096 {
        let cutoff = Instant::now() - Duration::from_secs(30 * 60);
        map.retain(|_, c| c.last > cutoff);
    }
    let c = map.entry(ip).or_insert_with(|| Client { tokens: RATE_BURST, last: Instant::now(), failures: Vec::new(), locked_until: None });
    f(c)
}

/// Returns Some(reason) if the caller must be refused before touching the DB.
pub(crate) fn throttle(ip: IpAddr) -> Option<&'static str> {
    with_client(ip, |c| {
        let now = Instant::now();
        if let Some(until) = c.locked_until {
            if now < until {
                return Some("locked out");
            }
            c.locked_until = None;
        }
        let elapsed = now.duration_since(c.last).as_secs_f64();
        c.tokens = (c.tokens + elapsed * RATE_PER_SEC).min(RATE_BURST);
        c.last = now;
        if c.tokens < 1.0 {
            return Some("rate limited");
        }
        c.tokens -= 1.0;
        None
    })
}

pub(crate) fn note_failure(ip: IpAddr) {
    with_client(ip, |c| {
        let now = Instant::now();
        c.failures.retain(|t| now.duration_since(*t) < FAILURE_WINDOW);
        c.failures.push(now);
        if c.failures.len() as u32 >= MAX_AUTH_FAILURES {
            c.locked_until = Some(now + LOCKOUT);
        }
    })
}

// ---- auth -------------------------------------------------------------------

fn presented_token(headers: &HeaderMap, q: &HashMap<String, String>) -> Option<String> {
    if let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(t) = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")) {
            return Some(t.trim().to_string());
        }
    }
    q.get("token").map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

fn client_ip(headers: &HeaderMap, peer: SocketAddr) -> IpAddr {
    super::client_ip(headers, peer)
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, [(header::CACHE_CONTROL, "no-store")], "unauthorized\n").into_response()
}

/// A request that carries signing headers, checked against the key's secret.
///
/// Tried before the bearer path, and a key that *has* a secret may only be
/// used this way: accepting it as a bearer token as well would hand back the
/// property signing exists to add.
pub(crate) async fn authenticate_signed(
    state: &App,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    query: &str,
    ip: std::net::IpAddr,
) -> Option<Result<store::ApiKeyRow, Response>> {
    let presented = crate::web::signing::presented(headers)?;
    let key = match store::authenticate_api_key(&state.db, &presented.key, Some(ip)).await {
        Ok(Ok(k)) => k,
        _ => {
            note_failure(ip);
            let _ = store::record_api_audit(&state.db, None, None, &ip.to_string(), "unknown-key", path).await;
            return Some(Err(unauthorized()));
        }
    };
    let Some(secret) = key.secret_sealed.as_deref().and_then(crate::web::signing::open_secret) else {
        // A key with no secret cannot be checked; the caller signed for
        // nothing. Said plainly, because it is theirs to fix.
        return Some(Err((
            StatusCode::UNAUTHORIZED,
            "that key has no signing secret — create a new key to sign requests\n",
        )
            .into_response()));
    };
    let now = crate::web::signing::now_ms();
    // The nonce is claimed only once the signature is good, so nobody without
    // the secret can use up someone else's nonces.
    let checked = crate::web::signing::verify(&secret, &presented, method, path, query, &body_sha256(headers), now)
        .and_then(|()| crate::web::signing::claim_nonce(key.key_id, &presented.nonce, now));
    match checked {
        Ok(()) => {
            let _ = store::record_api_audit(&state.db, Some(key.account_id), Some(key.key_id), &ip.to_string(), "ok-signed", path).await;
            Some(Ok(key))
        }
        Err(why) => {
            note_failure(ip);
            let outcome = match why {
                crate::web::signing::Refused::Replayed => "replayed",
                crate::web::signing::Refused::Stale => "stale",
                crate::web::signing::Refused::BadSignature => "bad-signature",
            };
            let _ = store::record_api_audit(&state.db, Some(key.account_id), Some(key.key_id), &ip.to_string(), outcome, path).await;
            Some(Err((StatusCode::UNAUTHORIZED, format!("{}\n", why.message())).into_response()))
        }
    }
}

pub(crate) async fn authenticate(
    state: &App,
    headers: &HeaderMap,
    q: &HashMap<String, String>,
    peer: SocketAddr,
    method: &str,
    path: &str,
) -> Result<store::ApiKeyRow, Response> {
    let ip = client_ip(headers, peer);
    if let Some(why) = throttle(ip) {
        return Err((StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "60")], format!("{why}\n")).into_response());
    }
    // Signing first, and signing only. `method` is the real verb — POST /v1/plans
    // is signed as POST, not as GET — or a captured list-plans call could be
    // replayed as a create.
    if let Some(result) = authenticate_signed(state, headers, method, path, &signed_query(headers, q), ip).await {
        return result;
    }
    // Unsigned. Every key must sign now, so the only thing left to do is say
    // which problem the caller has: a key that predates signing has to be
    // replaced, and a current key simply was not signed.
    // Signing headers, but not a complete, well-formed set — most often a
    // client written for the earlier scheme, before the nonce and the body
    // were signed. Told what is missing rather than a bare "unauthorized".
    if crate::web::signing::attempted(headers) {
        note_failure(ip);
        let _ = store::record_api_audit(&state.db, None, None, &ip.to_string(), "incomplete-signature", path).await;
        return Err((
            StatusCode::UNAUTHORIZED,
            "incomplete signature — every request needs X-HW-KEY, X-HW-TS, X-HW-NONCE (16–64 random characters, new each time) and X-HW-SIGN, \
             the HMAC-SHA256 of METHOD\\nPATH\\nQUERY\\nTIMESTAMP\\nNONCE\\nSHA256(BODY)\n",
        )
            .into_response());
    }
    let Some(token) = presented_token(headers, q) else {
        note_failure(ip);
        let _ = store::record_api_audit(&state.db, None, None, &ip.to_string(), "missing-token", path).await;
        return Err(unauthorized());
    };
    if let Ok(Ok(key)) = store::authenticate_api_key(&state.db, &token, Some(ip)).await {
        note_failure(ip);
        let outcome = if key.secret_sealed.is_some() { "unsigned" } else { "legacy-key" };
        let _ = store::record_api_audit(&state.db, Some(key.account_id), Some(key.key_id), &ip.to_string(), outcome, path).await;
        return Err((
            StatusCode::UNAUTHORIZED,
            if key.secret_sealed.is_some() {
                "sign this request — send X-HW-KEY, X-HW-TS, X-HW-NONCE and X-HW-SIGN over METHOD\\nPATH\\nQUERY\\nTIMESTAMP\\nNONCE\\nSHA256(BODY)\n"
            } else {
                "this key predates request signing and no longer works — create a new key, which comes with a signing secret\n"
            },
        )
            .into_response());
    }
    // The token is not a credential any more; everything that could succeed
    // has returned above.
    note_failure(ip);
    let _ = store::record_api_audit(&state.db, None, None, &ip.to_string(), "unknown-token", path).await;
    Err(unauthorized())
}

/// The query a request signs: the raw string as it arrived, stamped by
/// `stamp_signed_parts`. Never rebuilt from the parsed map, and never taken from
/// anything the caller can set separately from the URL itself.
fn signed_query(headers: &HeaderMap, _q: &HashMap<String, String>) -> String {
    raw_query(headers)
}

/// The query as it arrived in the URL, stamped by `stamp_signed_parts` — what
/// every signed request on `/v1` is checked against.
pub(crate) fn raw_query(headers: &HeaderMap) -> String {
    headers.get(RAW_QUERY).and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
}

fn flag(q: &HashMap<String, String>, name: &str) -> Option<bool> {
    q.get(name).map(|v| matches!(v.trim().to_lowercase().as_str(), "" | "1" | "true" | "yes" | "on"))
}

// ---- endpoints --------------------------------------------------------------

async fn prospects_csv(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let key = match authenticate(&state, &headers, &q, peer, "GET", "/dl/prospects.csv").await {
        Ok(k) => k,
        Err(r) => return r,
    };
    // A key pinned to a plan ignores any `plan` the caller sends.
    let plan_id = match key.plan_id {
        Some(pid) => Some(pid),
        None => match q.get("plan").filter(|s| !s.trim().is_empty()) {
            Some(name) => {
                let plans = match store::list_plans(&state.db, key.account_id).await {
                    Ok(p) => p,
                    Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "error\n").into_response(),
                };
                match plans.iter().find(|p| p.plan.source.eq_ignore_ascii_case(name.trim())) {
                    Some(p) => Some(p.plan.plan_id),
                    // A mistyped plan fails loudly rather than writing an empty file.
                    None => return (StatusCode::NOT_FOUND, "no such plan\n").into_response(),
                }
            }
            None => None,
        },
    };
    let bom = flag(&q, "bom").unwrap_or(true);

    let (count, version) = match store::prospect_version(&state.db, key.account_id, plan_id).await {
        Ok(v) => v,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "error\n").into_response(),
    };
    let etag = format!(
        "\"v{version}-{}-{count}-{}\"",
        plan_id.map(|p| p.to_string()).unwrap_or_else(|| "all".into()),
        if bom { "bom" } else { "raw" }
    );
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    let rows = match store::export_prospects(&state.db, key.account_id, plan_id, 0).await {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "error\n").into_response(),
    };
    let text = csv::prospects_csv(&rows);
    let body = if bom { csv::with_bom(&text) } else { text.into_bytes() };
    let fname = key.source.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect::<String>();
    let fname = if fname.is_empty() { "prospects".to_string() } else { format!("prospects-{fname}") };
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8")),
            (header::CONTENT_DISPOSITION, HeaderValue::from_str(&format!("attachment; filename=\"{fname}.csv\"")).unwrap()),
            (header::ETAG, HeaderValue::from_str(&etag).unwrap()),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache, private")),
        ],
        body,
    )
        .into_response()
}

async fn plans(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let key = match authenticate(&state, &headers, &q, peer, "GET", "/dl/plans").await {
        Ok(k) => k,
        Err(r) => return r,
    };
    let plans = match store::list_plans(&state.db, key.account_id).await {
        Ok(p) => p,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "error\n").into_response(),
    };
    let list: Vec<serde_json::Value> = plans
        .into_iter()
        .filter(|p| key.plan_id.map_or(true, |pid| pid == p.plan.plan_id))
        .map(|p| serde_json::json!({"plan": p.plan.source, "prospects": p.prospects}))
        .collect();
    ([(header::CACHE_CONTROL, "no-store")], axum::Json(serde_json::json!({"plans": list}))).into_response()
}

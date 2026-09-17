//! The token-gated CSV download API, for cron jobs and spreadsheet importers.
//!
//!   GET /dl/prospects.csv?token=<key>[&plan=<name>][&bom=0]
//!   GET /dl/plans?token=<key>
//!
//! `Authorization: Bearer <key>` works in place of `?token=`. Every failure —
//! missing, unknown, revoked, expired, wrong address — is the same 401, and
//! every attempt is recorded for the owner to see under API Access.

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
use crate::store::{self, AuthFailure};

pub fn router() -> Router<App> {
    Router::new()
        .route("/prospects.csv", get(prospects_csv))
        .route("/plans", get(plans))
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
fn throttle(ip: IpAddr) -> Option<&'static str> {
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

fn note_failure(ip: IpAddr) {
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

pub(crate) async fn authenticate(state: &App, headers: &HeaderMap, q: &HashMap<String, String>, peer: SocketAddr, path: &str) -> Result<store::ApiKeyRow, Response> {
    let ip = client_ip(headers, peer);
    if let Some(why) = throttle(ip) {
        return Err((StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "60")], format!("{why}\n")).into_response());
    }
    let Some(token) = presented_token(headers, q) else {
        note_failure(ip);
        let _ = store::record_api_audit(&state.db, None, None, &ip.to_string(), "missing-token", path).await;
        return Err(unauthorized());
    };
    if crate::config::get("HUNTWELL_REQUIRE_HEADER").as_deref() == Some("1") && q.contains_key("token") {
        note_failure(ip);
        let _ = store::record_api_audit(&state.db, None, None, &ip.to_string(), "token-in-query", path).await;
        return Err(unauthorized());
    }
    match store::authenticate_api_key(&state.db, &token, Some(ip)).await {
        Ok(Ok(key)) => {
            let _ = store::record_api_audit(&state.db, Some(key.account_id), Some(key.key_id), &ip.to_string(), "ok", path).await;
            Ok(key)
        }
        Ok(Err(AuthFailure::AddressNotAllowed)) => {
            note_failure(ip);
            let _ = store::record_api_audit(&state.db, None, None, &ip.to_string(), "address-not-allowed", path).await;
            Err(unauthorized())
        }
        Ok(Err(AuthFailure::UnknownToken)) => {
            note_failure(ip);
            let _ = store::record_api_audit(&state.db, None, None, &ip.to_string(), "unknown-token", path).await;
            Err(unauthorized())
        }
        Err(e) => {
            tracing::error!("download auth: {e:#}");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "error\n").into_response())
        }
    }
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
    let key = match authenticate(&state, &headers, &q, peer, "/dl/prospects.csv").await {
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
    let key = match authenticate(&state, &headers, &q, peer, "/dl/plans").await {
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

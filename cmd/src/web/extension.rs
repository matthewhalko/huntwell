//! Huntwell for Chrome (2026-10-20): the browser extension that sends queued
//! outreach on LinkedIn and Reddit — by opening the person's page and filling
//! the message in. **The person presses Send.** Nothing here, and nothing in
//! the extension, sends on its own: LinkedIn's terms forbid automated
//! messaging and Reddit treats unsolicited automated DMs as spam.
//!
//! Signing in is OAuth's authorization code with PKCE, via Chrome's
//! `launchWebAuthFlow`:
//! 1. the extension opens `/app/extension/authorize?redirect_uri=…&code_challenge=…&state=…`;
//! 2. the signed-in person approves; `POST /api/extension/authorize` checks the
//!    redirect is an allowed extension's (`https://<id>.chromiumapp.org/…`) and
//!    answers with a five-minute, single-use code;
//! 3. the extension trades code + verifier at `POST /ext/v1/token` for its own
//!    key: `api_key.kind = 'ext'` (`hwx_…`, 180 days, shown under API access,
//!    revocable), made by the person who approved — so it never does more than
//!    they may, and stops when they leave the workspace.
//!
//! An `ext` key works only here: `/v1` needs a signature it cannot make, and
//! `/mcp` refuses any kind but its own.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::api::require_cap;
use super::auth::AuthUser;
use super::{bad_request, not_found, ApiError, App};
use crate::store::{self, OutreachRow};

/// The app's side (session cookie), under `/api`: approving a sign-in, and
/// queueing drafts.
pub fn routes() -> Router<App> {
    Router::new()
        .route("/extension/check", get(check))
        .route("/extension/authorize", post(authorize))
        .route("/outreach/{id}/queue", put(queue))
        .route("/outreach/{id}/sent", post(mark_sent_in_app))
}

/// The extension's side (its bearer key), nested at `/ext/v1`.
pub fn public_router() -> Router<App> {
    Router::new()
        .route("/token", post(token))
        .route("/me", get(me))
        .route("/queue", get(list_queue))
        .route("/outreach/{id}/sent", post(sent))
        .route("/outreach/{id}/unqueue", post(unqueue))
        .route("/signout", post(signout))
}

// ---- which extensions may sign in ------------------------------------------------

/// The extension id `extension/manifest.json`'s `key` pins, so an unpacked
/// build has the same id on every machine. Allowed unless
/// `HUNTWELL_EXTENSION_IDS` says otherwise (set it to the Chrome Web Store
/// id once published; comma-separated, both may be listed).
pub const BUILT_IN_ID: &str = "cicgcidophbcmijifdjjmgphkhggaddi";

fn allowed_ids() -> Vec<String> {
    match crate::config::get("HUNTWELL_EXTENSION_IDS") {
        Some(v) if !v.trim().is_empty() => v.split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect(),
        _ => vec![BUILT_IN_ID.to_string()],
    }
}

/// The extension id a redirect URI belongs to, when it is exactly
/// `https://<32 letters a–p>.chromiumapp.org/<path>` — Chrome's own address
/// for `launchWebAuthFlow`, which only that extension receives. No port, no
/// userinfo, no query, no fragment.
pub fn redirect_extension_id(uri: &str) -> Option<String> {
    let u = reqwest::Url::parse(uri.trim()).ok()?;
    if u.scheme() != "https" || u.port().is_some() || !u.username().is_empty() || u.password().is_some() || u.query().is_some() || u.fragment().is_some() {
        return None;
    }
    let host = u.host_str()?;
    let id = host.strip_suffix(".chromiumapp.org")?;
    (id.len() == 32 && id.chars().all(|c| ('a'..='p').contains(&c))).then(|| id.to_string())
}

fn allowed_redirect(uri: &str) -> Result<(), &'static str> {
    let id = redirect_extension_id(uri).ok_or("that sign-in did not come from a Chrome extension")?;
    // A dev server lets any build sign in, so a changed key is not a wall.
    if crate::config::is_dev() || allowed_ids().iter().any(|a| *a == id) {
        Ok(())
    } else {
        Err("that extension isn't Huntwell for Chrome")
    }
}

/// PKCE S256: BASE64URL(SHA-256(verifier)), unpadded.
pub fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn valid_challenge(c: &str) -> bool {
    c.len() == 43 && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

// ---- the app's side ------------------------------------------------------------------

#[derive(Deserialize)]
struct CheckQuery {
    #[serde(default)]
    redirect_uri: String,
}

/// Whether the consent page should offer Allow at all, before anyone clicks.
async fn check(AuthUser(_acc): AuthUser, Query(q): Query<CheckQuery>) -> Result<Json<Value>, ApiError> {
    allowed_redirect(&q.redirect_uri).map_err(bad_request)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct AuthorizeBody {
    redirect_uri: String,
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: String,
    #[serde(default)]
    state: String,
}

async fn authorize(State(state): State<App>, AuthUser(acc): AuthUser, Json(b): Json<AuthorizeBody>) -> Result<Json<Value>, ApiError> {
    allowed_redirect(&b.redirect_uri).map_err(bad_request)?;
    if b.code_challenge_method != "S256" || !valid_challenge(&b.code_challenge) {
        return Err(bad_request("the extension's sign-in request is malformed — update the extension and try again"));
    }
    if b.state.len() > 200 {
        return Err(bad_request("the extension's sign-in request is malformed"));
    }
    let code = store::create_extension_grant(&state.db, acc.tenant(), acc.account_id, &b.code_challenge, b.redirect_uri.trim()).await?;
    let mut back = reqwest::Url::parse(b.redirect_uri.trim()).map_err(|_| bad_request("bad redirect"))?;
    back.query_pairs_mut().append_pair("code", &code).append_pair("state", &b.state);
    Ok(Json(json!({ "redirect": back.to_string() })))
}

#[derive(Deserialize)]
struct QueueBody {
    /// 'linkedin' | 'reddit', or null to take it off the queue.
    channel: Option<String>,
    /// Their Reddit username, when queueing for Reddit.
    #[serde(default)]
    reddit: Option<String>,
}

async fn queue(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(b): Json<QueueBody>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_PLANS).await?;
    let o = store::get_outreach(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("draft not found"))?;
    let reddit = match b.reddit.as_deref().map(str::trim) {
        None => None,
        Some("") => Some(String::new()),
        Some(r) => Some(store::reddit_username(r).ok_or_else(|| bad_request("that isn't a Reddit username (3–20 letters, digits, _ or -)"))?),
    };
    match b.channel.as_deref() {
        Some("linkedin") if o.recipient_linkedin.trim().is_empty() && o.recipient_name.trim().is_empty() => {
            return Err(bad_request("add who it's to first — LinkedIn needs their profile or at least a name to search for"));
        }
        Some("reddit") if reddit.as_deref().unwrap_or(o.recipient_reddit.as_str()).is_empty() => {
            return Err(bad_request("add their Reddit username first"));
        }
        Some(c) if !store::SEND_CHANNELS.contains(&c) => return Err(bad_request("send it on LinkedIn or Reddit")),
        _ => {}
    }
    store::set_outreach_queue(&state.db, acc.tenant(), id, b.channel.as_deref(), reddit.as_deref()).await?;
    let o = store::get_outreach(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("draft not found"))?;
    Ok(Json(json!({ "outreach": o })))
}

#[derive(Deserialize)]
struct SentBody {
    #[serde(default)]
    via: String,
}

fn clean_via(v: &str) -> &'static str {
    match v {
        "linkedin" => "linkedin",
        "reddit" => "reddit",
        "email" => "email",
        _ => "other",
    }
}

/// Marked sent by hand, in the app — sent from their own mail, say.
async fn mark_sent_in_app(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(b): Json<SentBody>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_PLANS).await?;
    let o = store::mark_outreach_sent(&state.db, acc.tenant(), id, clean_via(&b.via)).await?.ok_or_else(|| not_found("draft not found"))?;
    log_to_crm(&state, acc.tenant(), acc.account_id, &o).await;
    Ok(Json(json!({ "outreach": o })))
}

/// When the workspace has the CRM, a sent message is a line in the contact's
/// history. Best effort: a draft with no matching contact logs nothing.
async fn log_to_crm(state: &App, ws: i64, by: i64, o: &OutreachRow) {
    if !store::has_module(&state.db, ws, "crm").await {
        return;
    }
    let contact = async {
        if let Some(p) = o.prospect_id {
            if let Some(c) = store::contacts_for_sources(&state.db, ws, "prospect", &[p]).await.ok()?.get(&p) {
                return Some(*c);
            }
        }
        if let Some(a) = o.artifact_id {
            if let Some(c) = store::contacts_for_sources(&state.db, ws, "artifact", &[a]).await.ok()?.get(&a) {
                return Some(*c);
            }
        }
        store::contact_by_email(&state.db, ws, &o.recipient_email).await.ok().flatten()
    }
    .await;
    let Some(contact_id) = contact else { return };
    let kind = match o.sent_via.as_str() {
        "linkedin" | "reddit" | "email" => o.sent_via.as_str(),
        _ => "note",
    };
    let body = format!("Sent: {}\n\n{}", o.subject.trim(), o.body.trim());
    if let Err(e) = store::add_contact_activity(&state.db, ws, contact_id, by, kind, &body, chrono::Utc::now()).await {
        tracing::warn!(contact_id, "logging a sent message to the CRM: {e:#}");
    }
}

// ---- the extension's side ---------------------------------------------------------------

struct Ext {
    workspace: i64,
    person: Option<i64>,
    key_id: i64,
    can_plans: bool,
}

fn refused(status: StatusCode, msg: &str) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], Json(json!({ "error": msg }))).into_response()
}

async fn ext_caller(state: &App, headers: &HeaderMap, peer: SocketAddr) -> Result<Ext, Response> {
    let ip = super::download::client_ip(headers, peer);
    if let Some(why) = super::download::throttle(ip) {
        return Err(refused(StatusCode::TOO_MANY_REQUESTS, &why));
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let Some(token) = token else {
        super::download::note_failure(ip);
        return Err(refused(StatusCode::UNAUTHORIZED, "sign in to Huntwell from the extension"));
    };
    let key = match store::authenticate_api_key(&state.db, token, Some(ip)).await {
        Ok(Ok(k)) if k.kind == "ext" => k,
        _ => {
            super::download::note_failure(ip);
            let _ = store::record_api_audit(&state.db, None, None, &ip.to_string(), "ext-refused", "/ext/v1").await;
            return Err(refused(StatusCode::UNAUTHORIZED, "signed out — sign in to Huntwell again"));
        }
    };
    // Its maker's rights, read now. A maker who left the team has no key at
    // all — `authenticate_api_key` refuses it — and one who lost the right to
    // change plans takes that right from the extension too.
    let can_plans = match key.created_by {
        None => false,
        Some(p) => store::workspace_caps(&state.db, key.account_id, p).await.map(|c| c.plans).unwrap_or(false),
    };
    Ok(Ext { workspace: key.account_id, person: key.created_by, key_id: key.key_id, can_plans })
}

#[derive(Deserialize)]
struct TokenBody {
    code: String,
    code_verifier: String,
    redirect_uri: String,
}

async fn token(State(state): State<App>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(b): Json<TokenBody>) -> Response {
    let ip = super::download::client_ip(&headers, peer);
    if let Some(why) = super::download::throttle(ip) {
        return refused(StatusCode::TOO_MANY_REQUESTS, &why);
    }
    // Claimed before it is checked, so a code tried with a wrong verifier is
    // spent too: a stolen code gets one guess, not many.
    let grant = match store::take_extension_grant(&state.db, &b.code).await {
        Ok(Some(g)) => g,
        Ok(None) => {
            super::download::note_failure(ip);
            return refused(StatusCode::BAD_REQUEST, "that sign-in expired — try again");
        }
        Err(e) => {
            tracing::error!("extension token: {e:#}");
            return refused(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    };
    let (workspace, person, challenge, redirect) = grant;
    let verifier_ok = (43..=128).contains(&b.code_verifier.len()) && crate::web::signing::constant_time_eq(pkce_challenge(&b.code_verifier).as_bytes(), challenge.as_bytes());
    if !verifier_ok || b.redirect_uri.trim() != redirect {
        super::download::note_failure(ip);
        return refused(StatusCode::BAD_REQUEST, "that sign-in could not be verified — try again");
    }
    let label = format!("Huntwell for Chrome · {}", chrono::Utc::now().format("%b %-d, %Y"));
    let key = match store::create_extension_key(&state.db, workspace, person, &label).await {
        Ok(k) => k,
        Err(e) => {
            tracing::error!("extension key: {e:#}");
            return refused(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    };
    let _ = store::record_api_audit(&state.db, Some(workspace), Some(key.key_id), &ip.to_string(), "ext-signed-in", "/ext/v1/token").await;
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "access_token": key.token, "token_type": "Bearer", "expires_at": key.expires_at })),
    )
        .into_response()
}

async fn me(State(state): State<App>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    let c = match ext_caller(&state, &headers, peer).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let workspace = store::workspace_name(&state.db, c.workspace).await.unwrap_or_default();
    let who = match c.person {
        Some(p) => store::get_account(&state.db, p).await.ok().flatten(),
        None => None,
    };
    Json(json!({
        "workspace": workspace,
        "email": who.as_ref().map(|a| a.email.clone()).unwrap_or_default(),
        "name": who.as_ref().map(|a| a.display_name.clone()).unwrap_or_default(),
        "can_send": c.can_plans,
    }))
    .into_response()
}

/// A queued draft as the extension needs it: where to go and what to fill.
fn queue_item(o: &OutreachRow) -> Value {
    let message = if o.footer.trim().is_empty() { o.body.trim_end().to_string() } else { format!("{}\n\n{}", o.body.trim_end(), o.footer.trim_end()) };
    json!({
        "id": o.outreach_id,
        "channel": o.send_channel,
        "name": o.recipient_name,
        "title": o.recipient_title,
        "company": o.recipient_company,
        "linkedin": crate::outreach::linkedin_url(&o.recipient_linkedin).unwrap_or_default(),
        "reddit": o.recipient_reddit,
        "subject": o.subject,
        "message": message,
        "queued_at": o.queued_at.map(|t| t.to_rfc3339()),
    })
}

async fn list_queue(State(state): State<App>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    let c = match ext_caller(&state, &headers, peer).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match store::list_outreach_queue(&state.db, c.workspace, 200).await {
        Ok(rows) => Json(json!({ "items": rows.iter().map(queue_item).collect::<Vec<_>>() })).into_response(),
        Err(e) => {
            tracing::error!("extension queue: {e:#}");
            refused(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

async fn sent(State(state): State<App>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(id): Path<i64>, Json(b): Json<SentBody>) -> Response {
    let c = match ext_caller(&state, &headers, peer).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !c.can_plans {
        return refused(StatusCode::FORBIDDEN, "your role in this workspace can't send outreach");
    }
    match store::mark_outreach_sent(&state.db, c.workspace, id, clean_via(&b.via)).await {
        Ok(Some(o)) => {
            if let Some(p) = c.person {
                log_to_crm(&state, c.workspace, p, &o).await;
            }
            Json(json!({ "ok": true })).into_response()
        }
        Ok(None) => refused(StatusCode::NOT_FOUND, "draft not found"),
        Err(e) => {
            tracing::error!("extension sent: {e:#}");
            refused(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

async fn unqueue(State(state): State<App>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(id): Path<i64>) -> Response {
    let c = match ext_caller(&state, &headers, peer).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !c.can_plans {
        return refused(StatusCode::FORBIDDEN, "your role in this workspace can't change outreach");
    }
    match store::set_outreach_queue(&state.db, c.workspace, id, None, None).await {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => refused(StatusCode::NOT_FOUND, "draft not found"),
        Err(e) => {
            tracing::error!("extension unqueue: {e:#}");
            refused(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// Signing out revokes the key, so a signed-out browser holds nothing that works.
async fn signout(State(state): State<App>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    let c = match ext_caller(&state, &headers, peer).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let _ = store::revoke_api_key(&state.db, c.workspace, c.key_id).await;
    Json(json!({ "ok": true })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_chrome_extension_redirect_is_accepted() {
        let id = "cicgcidophbcmijifdjjmgphkhggaddi";
        assert_eq!(redirect_extension_id(&format!("https://{id}.chromiumapp.org/cb")).as_deref(), Some(id));
        assert_eq!(redirect_extension_id(&format!("https://{id}.chromiumapp.org/")).as_deref(), Some(id));
        for bad in [
            format!("http://{id}.chromiumapp.org/cb"),
            format!("https://{id}.chromiumapp.org:8443/cb"),
            format!("https://user@{id}.chromiumapp.org/cb"),
            format!("https://{id}.chromiumapp.org/cb?x=1"),
            format!("https://{id}.chromiumapp.org/cb#x"),
            format!("https://{id}.chromiumapp.org.evil.com/cb"),
            "https://evil.com/cb".to_string(),
            "https://zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz.chromiumapp.org/cb".to_string(),
            "https://short.chromiumapp.org/cb".to_string(),
        ] {
            assert_eq!(redirect_extension_id(&bad), None, "{bad}");
        }
    }

    #[test]
    fn pkce_is_unpadded_base64url_sha256() {
        // Computed independently: python3 -c "import hashlib,base64;
        // print(base64.urlsafe_b64encode(hashlib.sha256(b'…').digest()).rstrip(b'='))"
        assert_eq!(pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r7wW1gFWZtXbCM"), "UW0sY8XlhpiN6euXC1LRzOoY9ksy4NEq4Dxmq9dIS1o");
        assert!(valid_challenge("UW0sY8XlhpiN6euXC1LRzOoY9ksy4NEq4Dxmq9dIS1o"));
        assert!(!valid_challenge("short") && !valid_challenge("UW0sY8XlhpiN6euXC1LRzOoY9ksy4NEq4Dxmq9dIS1="));
    }

    #[test]
    fn reddit_usernames_come_from_any_common_form() {
        for (raw, want) in [("spez", "spez"), ("u/spez", "spez"), ("/u/Some_User-1", "Some_User-1"), ("https://www.reddit.com/user/spez/", "spez"), ("https://reddit.com/u/spez?x=1", "spez")] {
            assert_eq!(store::reddit_username(raw).as_deref(), Some(want), "{raw}");
        }
        for bad in ["", "ab", "a b c", "this_name_is_far_too_long_for_reddit", "spez!", "<script>"] {
            assert_eq!(store::reddit_username(bad), None, "{bad}");
        }
    }
}

//! The support bubble's API: a chat with Huntwell from the public site or the
//! app (`components/SupportChat.tsx`). The operator side is admin → Support;
//! what both share is `crate::support`.
//!
//! A person has any number of chats, one current: they end it and start
//! another, and can read the earlier ones (as in Alpha Tech's terminal).
//!
//! Two ways in, and never a chat id the request names that is not theirs:
//! - **signed in** (`/support/mine`): the session's account owns its chats.
//!   The current one is the newest not ended; an id picks an earlier one,
//!   checked against the account.
//! - **a visitor** (`/support/start`, `/thread`, `/send`, `/end`,
//!   `/history`): the first step is their name, their email and Cloudflare
//!   Turnstile; passing it mints a `hwc_` token the bubble keeps (the row
//!   holds its SHA-256), one per chat. The token travels in a JSON body,
//!   never a URL, so no log ever holds it.
//!
//! A visitor's chat exists only behind Turnstile: with no Turnstile secret
//! configured the public chat is refused outright (dev excepted), never left
//! open to scripts. Starting is also five an hour per address, and the first
//! message is what posts to the operators' Slack.

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::auth::AuthUser;
use super::{bad_request, client_ip, ApiError, App};
use crate::store::{self, NewSupportChat, SupportConversation};

pub fn routes() -> Router<App> {
    Router::new()
        .route("/support/mine", get(mine).post(mine_send).layer(axum::extract::DefaultBodyLimit::max(SEND_LIMIT)))
        .route("/support/mine/end", post(mine_end))
        .route("/support/mine/history", get(mine_history))
        .route("/support/start", post(start))
        .route("/support/thread", post(thread))
        .route("/support/send", post(send).layer(axum::extract::DefaultBodyLimit::max(SEND_LIMIT)))
        .route("/support/image", post(image))
        .route("/support/end", post(end))
        .route("/support/history", post(history))
}

fn too_many(wait: u64) -> ApiError {
    ApiError(StatusCode::TOO_MANY_REQUESTS, format!("that's a lot of messages at once. Try again in {} minutes.", wait.div_ceil(60).max(1)))
}

fn gone() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "this chat could not be found. Start a new one.".into())
}

fn ended() -> ApiError {
    ApiError(StatusCode::CONFLICT, "this chat has ended. Start a new one to write to us.".into())
}

/// The person ended a chat: mark it, and say so in its Slack thread.
async fn end_chat(state: &App, c: &SupportConversation) -> Result<(), ApiError> {
    if store::support_end(&state.db, c.conversation_id, &c.email).await? {
        tracing::info!(conversation_id = c.conversation_id, "support chat ended by the person");
        let (db, c) = (state.db.clone(), c.clone());
        tokio::spawn(async move { crate::support::slack::note_ended(&db, &c).await });
    }
    Ok(())
}

/// The most a message with its images may weigh: four images at the cap,
/// as base64, and the text.
const SEND_LIMIT: usize = 24 * 1024 * 1024;

/// A message as typed: trimmed, not huge, and not empty unless it carries
/// an image.
fn body_of(raw: &str, has_images: bool) -> Result<String, ApiError> {
    let body = raw.trim();
    if body.is_empty() && !has_images {
        return Err(bad_request("write a message first"));
    }
    if body.chars().count() > store::SUPPORT_BODY_MAX {
        return Err(bad_request(format!("a message can be at most {} characters", store::SUPPORT_BODY_MAX)));
    }
    Ok(body.to_string())
}

/// A message's images, as the browser sent them (data URLs, or bare
/// base64): decoded, bounded, and each one an image by its bytes.
fn images_of(raw: &[String]) -> Result<Vec<store::SupportImage>, ApiError> {
    use base64::Engine;
    if raw.len() > store::SUPPORT_MAX_IMAGES {
        return Err(bad_request(format!("at most {} images in one message", store::SUPPORT_MAX_IMAGES)));
    }
    let mut out = Vec::with_capacity(raw.len());
    for item in raw {
        let encoded = item.split_once(";base64,").map(|(_, b)| b).unwrap_or(item);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| bad_request("an image could not be read"))?;
        if bytes.len() > store::SUPPORT_MAX_IMAGE_BYTES {
            return Err(bad_request(format!("each image must be under {} MB", store::SUPPORT_MAX_IMAGE_BYTES / (1024 * 1024))));
        }
        let content_type = store::support_image_type(&bytes).ok_or_else(|| bad_request("images must be PNG, JPEG, GIF or WebP"))?;
        out.push(store::SupportImage { content_type, bytes });
    }
    Ok(out)
}

/// The page they wrote from, as a path only: context for the operator, never
/// a URL anyone follows.
fn page_of(raw: &str) -> String {
    let p: String = raw.trim().chars().filter(|c| !c.is_control()).take(300).collect();
    if p.starts_with('/') { p } else { String::new() }
}

/// The chat as the person sees it. Operators are "Huntwell support", never
/// named; ids let the bubble ask only for what is new.
async fn view(state: &App, c: &SupportConversation) -> Result<Value, ApiError> {
    let msgs = store::support_messages(&state.db, c.conversation_id, 0).await?;
    let unread = store::support_unread_for_customer(c, &msgs);
    Ok(json!({
        "id": c.conversation_id,
        "name": c.name,
        "email": c.email,
        "status": c.status,
        "unread": unread,
        "messages": msgs.iter().map(|m| json!({
            "id": m.message_id,
            "from": if m.author == "operator" { "support" } else { "you" },
            "body": m.body,
            "images": m.image_ids,
            "at": m.created_at,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
pub struct MineQuery {
    /// The bubble is open, so everything in it has been seen.
    #[serde(default)]
    open: bool,
    /// An earlier chat of theirs; none for the current one.
    #[serde(default)]
    id: Option<i64>,
}

async fn mine(State(state): State<App>, AuthUser(me): AuthUser, Query(q): Query<MineQuery>) -> Result<Json<Value>, ApiError> {
    let found = match q.id {
        Some(id) => Some(store::support_of_account(&state.db, me.account_id, id).await?.ok_or_else(gone)?),
        None => store::support_for_account(&state.db, me.account_id).await?,
    };
    let Some(c) = found else {
        return Ok(Json(json!({ "conversation": null })));
    };
    let v = view(&state, &c).await?;
    if q.open {
        store::support_customer_seen(&state.db, c.conversation_id).await?;
    }
    Ok(Json(json!({ "conversation": v })))
}

#[derive(Deserialize)]
pub struct MineBody {
    #[serde(default)]
    body: String,
    #[serde(default)]
    page: String,
    /// The chat they are writing in, as the bubble shows it; none to write
    /// in the current one (or start one).
    #[serde(default)]
    id: Option<i64>,
    /// Screenshots: data URLs, at most four.
    #[serde(default)]
    images: Vec<String>,
}

async fn mine_send(
    State(state): State<App>,
    AuthUser(me): AuthUser,
    Json(b): Json<MineBody>,
) -> Result<Json<Value>, ApiError> {
    let images = images_of(&b.images)?;
    let body = body_of(&b.body, !images.is_empty())?;
    let current = store::support_for_account(&state.db, me.account_id).await?;
    // Written in an earlier chat they had on screen: only while it is still
    // the current one.
    if let Some(id) = b.id {
        if current.as_ref().map(|c| c.conversation_id) != Some(id) {
            store::support_of_account(&state.db, me.account_id, id).await?.ok_or_else(gone)?;
            return Err(ended());
        }
    }
    let c = match current {
        Some(c) => {
            crate::throttle::SUPPORT_MESSAGES.hit(&c.conversation_id.to_string()).map_err(too_many)?;
            store::support_customer_says(&state.db, c.conversation_id, &body, &images).await?;
            c
        }
        None => {
            crate::throttle::SUPPORT_CHATS.hit(&format!("account:{}", me.account_id)).map_err(too_many)?;
            let c = store::support_start(
                &state.db,
                &NewSupportChat {
                    account_id: Some(me.account_id),
                    workspace_id: Some(me.active_workspace_id.unwrap_or(me.account_id)),
                    email: &me.email,
                    name: &me.display_name,
                    token: None,
                    page: &page_of(&b.page),
                    ip: "",
                },
                Some(&body),
                &images,
            )
            .await?;
            tracing::info!(conversation_id = c.conversation_id, account_id = me.account_id, "support chat started");
            c
        }
    };
    let c = store::support_get(&state.db, c.conversation_id).await?.ok_or_else(gone)?;
    Ok(Json(json!({ "conversation": view(&state, &c).await? })))
}

#[derive(Deserialize)]
pub struct MineEnd {
    /// The chat to end; none for the current one.
    #[serde(default)]
    id: Option<i64>,
}

async fn mine_end(State(state): State<App>, AuthUser(me): AuthUser, Json(b): Json<MineEnd>) -> Result<Json<Value>, ApiError> {
    let found = match b.id {
        Some(id) => Some(store::support_of_account(&state.db, me.account_id, id).await?.ok_or_else(gone)?),
        None => store::support_for_account(&state.db, me.account_id).await?,
    };
    if let Some(c) = found {
        end_chat(&state, &c).await?;
    }
    Ok(Json(json!({ "ok": true })))
}

async fn mine_history(State(state): State<App>, AuthUser(me): AuthUser) -> Result<Json<Value>, ApiError> {
    let chats = store::support_history(&state.db, Some(me.account_id), &[]).await?;
    Ok(Json(json!({ "chats": chats })))
}

#[derive(Deserialize)]
pub struct StartBody {
    name: String,
    email: String,
    #[serde(default)]
    page: String,
    #[serde(default)]
    turnstile_token: String,
}

/// A visitor's first step: who they are, and proof they are not a script.
/// Opens an empty chat and returns its token; nothing reaches Slack until
/// they write in it.
async fn start(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(b): Json<StartBody>,
) -> Result<Json<Value>, ApiError> {
    if !crate::turnstile::configured() && !state.dev {
        tracing::error!("support chat refused: Turnstile is not configured (TURNSTILE_SECRET_KEY), and the public chat never runs without it");
        return Err(unavailable());
    }
    let ip = client_ip(&headers, peer);
    let name = b.name.trim();
    if name.is_empty() {
        return Err(bad_request("tell us your name"));
    }
    super::auth::validate_display_name(name).map_err(|e| bad_request(e.to_string()))?;
    let email = b.email.trim().to_lowercase();
    if email.len() > 320 || !email.contains('@') || email.starts_with('@') || email.ends_with('@') || email.contains(char::is_whitespace) {
        return Err(bad_request("enter your email address so we can reply"));
    }
    crate::throttle::SUPPORT_CHATS.hit(&ip.to_string()).map_err(too_many)?;
    crate::turnstile::verify(&b.turnstile_token, ip).await.map_err(|e| ApiError(StatusCode::FORBIDDEN, e.to_string()))?;
    let token = crate::support::new_token();
    // Without the seal key a reply email could not carry the link back, and
    // the chat would be one the person can never reopen from another device.
    let sealed = crate::web::signing::seal_secret(&token).ok_or_else(|| {
        tracing::error!("support chat refused: no key to seal its token (HUNTWELL_SESSION_SECRET)");
        unavailable()
    })?;
    let hash = store::sha256_hex(&token);
    let c = store::support_start(
        &state.db,
        &NewSupportChat {
            account_id: None,
            workspace_id: None,
            email: &email,
            name,
            token: Some((&hash, &sealed)),
            page: &page_of(&b.page),
            ip: &ip.to_string(),
        },
        None,
        &[],
    )
    .await?;
    tracing::info!(conversation_id = c.conversation_id, "support chat begun by a visitor");
    Ok(Json(json!({ "token": token, "conversation": view(&state, &c).await? })))
}

fn unavailable() -> ApiError {
    ApiError(StatusCode::SERVICE_UNAVAILABLE, "chat is unavailable right now. Please try again later.".into())
}

/// A visitor's chat by their token. A miss counts with sign-in failures: it is
/// a guess at a 192-bit token, and a loop of them meets the same wall.
async fn by_token(state: &App, ip: std::net::IpAddr, token: &str) -> Result<SupportConversation, ApiError> {
    let key = ip.to_string();
    crate::throttle::LOGIN_FAILURES.check(&key).map_err(too_many)?;
    let token = token.trim();
    let found = if token.starts_with("hwc_") && token.len() <= 80 {
        store::support_by_token(&state.db, &store::sha256_hex(token)).await?
    } else {
        None
    };
    found.ok_or_else(|| {
        crate::throttle::LOGIN_FAILURES.note(&key);
        gone()
    })
}

#[derive(Deserialize)]
pub struct ThreadBody {
    token: String,
    #[serde(default)]
    open: bool,
}

async fn thread(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(b): Json<ThreadBody>,
) -> Result<Json<Value>, ApiError> {
    let c = by_token(&state, client_ip(&headers, peer), &b.token).await?;
    let v = view(&state, &c).await?;
    if b.open {
        store::support_customer_seen(&state.db, c.conversation_id).await?;
    }
    Ok(Json(json!({ "conversation": v })))
}

#[derive(Deserialize)]
pub struct SendBody {
    token: String,
    #[serde(default)]
    body: String,
    /// Screenshots: data URLs, at most four.
    #[serde(default)]
    images: Vec<String>,
}

async fn send(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(b): Json<SendBody>,
) -> Result<Json<Value>, ApiError> {
    let images = images_of(&b.images)?;
    let body = body_of(&b.body, !images.is_empty())?;
    let c = by_token(&state, client_ip(&headers, peer), &b.token).await?;
    if c.status == "ended" {
        return Err(ended());
    }
    crate::throttle::SUPPORT_MESSAGES.hit(&c.conversation_id.to_string()).map_err(too_many)?;
    store::support_customer_says(&state.db, c.conversation_id, &body, &images).await?;
    let c = store::support_get(&state.db, c.conversation_id).await?.ok_or_else(gone)?;
    Ok(Json(json!({ "conversation": view(&state, &c).await? })))
}

#[derive(Deserialize)]
pub struct ImageBody {
    id: i64,
    /// A visitor's chat token; signed in, the session is enough.
    #[serde(default)]
    token: String,
}

/// One image from a chat, to the person whose chat it is: signed in, their
/// account's; a visitor, the chat their token opens. A POST with the token
/// in the body, never a URL, so no log ever holds it; the bubble shows the
/// bytes as a blob. Anyone else is told it does not exist.
async fn image(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    me: Result<AuthUser, ApiError>,
    Json(b): Json<ImageBody>,
) -> Result<Response, ApiError> {
    let missing = || ApiError(StatusCode::NOT_FOUND, "that image could not be found".into());
    let (conversation_id, content_type, bytes) = store::support_image(&state.db, b.id).await?.ok_or_else(missing)?;
    let allowed = if !b.token.trim().is_empty() {
        by_token(&state, client_ip(&headers, peer), &b.token).await?.conversation_id == conversation_id
    } else if let Ok(AuthUser(me)) = me {
        store::support_of_account(&state.db, me.account_id, conversation_id).await?.is_some()
    } else {
        false
    };
    if !allowed {
        return Err(missing());
    }
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, content_type),
            // An image never changes once sent.
            (axum::http::header::CACHE_CONTROL, "private, max-age=86400, immutable".to_string()),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct EndBody {
    token: String,
}

async fn end(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(b): Json<EndBody>,
) -> Result<Json<Value>, ApiError> {
    let c = by_token(&state, client_ip(&headers, peer), &b.token).await?;
    end_chat(&state, &c).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct HistoryBody {
    /// Every chat token this browser has kept, the current one included.
    #[serde(default)]
    tokens: Vec<String>,
}

/// A visitor's earlier chats: those this browser holds the tokens of. A token
/// that finds nothing is left out (its chat may have been deleted); guessing
/// is no use against 192 bits, and each call looks at twenty at most.
async fn history(State(state): State<App>, Json(b): Json<HistoryBody>) -> Result<Json<Value>, ApiError> {
    let pairs: Vec<(String, String)> = b
        .tokens
        .iter()
        .map(|t| t.trim())
        .filter(|t| t.starts_with("hwc_") && t.len() <= 80)
        .take(20)
        .map(|t| (store::sha256_hex(t), t.to_string()))
        .collect();
    let hashes: Vec<String> = pairs.iter().map(|p| p.0.clone()).collect();
    let chats = if hashes.is_empty() { Vec::new() } else { store::support_history(&state.db, None, &hashes).await? };
    // Each with its token, so the bubble can open it.
    let out: Vec<Value> = chats
        .iter()
        .map(|ch| {
            let token = pairs.iter().find(|p| ch.token_hash.as_deref() == Some(p.0.as_str())).map(|p| p.1.as_str());
            let mut v = json!(ch);
            v["token"] = json!(token);
            v
        })
        .collect();
    Ok(Json(json!({ "chats": out })))
}

/// `POST /slack/events`: Slack's Events API, for replies typed in a chat's
/// thread (`support::slack`). Not a session's: what proves a call is Slack's
/// is its signature, checked before anything in it is read. Answered at once;
/// a reply is delivered after, since Slack retries anything slower than
/// three seconds.
pub async fn slack_events(State(state): State<App>, headers: HeaderMap, raw: Bytes) -> Response {
    use crate::support::slack::{self, Inbound};
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    match slack::inbound(&state.db, &header("x-slack-request-timestamp"), &header("x-slack-signature"), &raw).await {
        Inbound::NotSet => StatusCode::NOT_FOUND.into_response(),
        Inbound::Refused => {
            tracing::warn!("slack events: refused a request with a bad or stale signature");
            StatusCode::UNAUTHORIZED.into_response()
        }
        Inbound::Unreadable => StatusCode::BAD_REQUEST.into_response(),
        Inbound::Challenge(c) => c.into_response(),
        Inbound::Event(reply) => {
            if let Some((cfg, r)) = reply {
                let db = state.db.clone();
                tokio::spawn(async move { slack::deliver(&db, &cfg, r).await });
            }
            StatusCode::OK.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_is_trimmed_and_bounded() {
        assert_eq!(body_of("  hello \n", false).ok().as_deref(), Some("hello"));
        assert!(body_of("   ", false).is_err());
        // An image alone is a message.
        assert_eq!(body_of("   ", true).ok().as_deref(), Some(""));
        assert!(body_of(&"x".repeat(store::SUPPORT_BODY_MAX + 1), true).is_err());
        assert!(body_of(&"x".repeat(store::SUPPORT_BODY_MAX), false).is_ok());
    }

    #[test]
    fn an_image_must_be_an_image_by_its_bytes() {
        use base64::Engine;
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let png = b64(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2]);
        let got = images_of(&[format!("data:image/png;base64,{png}")]).ok().unwrap();
        assert_eq!(got[0].content_type, "image/png");
        // Called a PNG, but is not one.
        assert!(images_of(&[format!("data:image/png;base64,{}", b64(b"<svg onload=alert(1)>"))]).is_err());
        assert!(images_of(&["not base64!".to_string()]).is_err());
        assert!(images_of(&vec![png.clone(); store::SUPPORT_MAX_IMAGES + 1]).is_err());
        assert!(images_of(&[]).is_ok_and(|i| i.is_empty()));
    }

    #[test]
    fn only_a_path_is_kept_as_the_page() {
        assert_eq!(page_of("/pricing"), "/pricing");
        assert_eq!(page_of("https://evil.example/x"), "");
        assert_eq!(page_of("/a\nb"), "/ab");
        assert_eq!(page_of(&format!("/{}", "a".repeat(400))).chars().count(), 300);
    }
}

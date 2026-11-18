//! Support chats as Slack threads, both ways.
//!
//! The operators' channel is the support desk, as in Alpha Tech's terminal.
//! A chat's first message starts a thread there ("*Pat (pat@acme.com)* needs
//! support:" and their words, quoted); everything the person writes after it
//! is quoted into that thread and shown in the channel too, so a message on
//! an old thread is not missed. Answers typed in the admin go into the thread
//! as well. A reply typed **in the thread** is the operator answering: it
//! reaches the person in the bubble, and by email when they have left the
//! page. A reply that starts with `//` stays in Slack. When the person ends
//! the chat, the thread says so.
//!
//! A Slack app, not a webhook: a webhook can only post, and cannot thread or
//! hear back. Replies arrive through Slack's **Events API**: Slack calls
//! `POST /slack/events` on the website, and `verify_signature` proves the
//! call is Slack's (the app's signing secret, a five-minute window) before
//! anything in it is read.
//!
//! The bot token and the signing secret are sealed in `control_setting`, set
//! in admin → Support. Anyone who can write in the channel can answer
//! customers: the channel is the trust boundary.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::Utc;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::slack::SendError;
use crate::store::{self, Db, SupportConversation, SupportMessage, SupportSlackDone};

/// The bot token (`xoxb-…`), sealed. Posts and reacts.
pub const BOT_TOKEN_KEY: &str = "support_slack_bot";
/// The app's signing secret, sealed. Proves a call to `/slack/events` is Slack's.
pub const SIGNING_KEY: &str = "support_slack_signing";
/// The channel's id (`C…`, or `G…` for an old private channel).
pub const CHANNEL_KEY: &str = "support_slack_channel";
/// What the admin shows for the connection: "#support in Acme".
pub const LABEL_KEY: &str = "support_slack_label";
/// When Slack last called `/slack/events` with a good signature (RFC 3339):
/// the admin's sign that replies are being heard.
pub const HEARD_KEY: &str = "support_slack_heard";
/// Where Slack sends events, on the website.
pub const EVENTS_PATH: &str = "/slack/events";
/// A thread with more owed than this when it starts gets only the newest.
const BACKFILL: usize = 20;
/// Slack's limit for posting to one channel is about one message a second.
const POST_GAP: Duration = Duration::from_millis(1_100);
/// A reply that starts with this is the team talking among themselves.
pub const INTERNAL_PREFIX: &str = "//";
/// Slack's own replay window: a signed request older than this is refused.
const MAX_SKEW_SECS: i64 = 5 * 60;

/// Where Slack should send events: the website's public address, when this
/// process knows it.
pub fn request_url() -> Option<String> {
    let base = crate::config::get("HUNTWELL_PUBLIC_URL")?;
    let base = base.trim().trim_end_matches('/');
    (!base.is_empty()).then(|| format!("{base}{EVENTS_PATH}"))
}

/// The app's manifest, for "Create New App → From a manifest". The scopes are
/// exactly what this file uses: post, upload screenshots, react, read the
/// channel's messages, and look up who replied. With the Request URL when it is known.
pub fn manifest(request_url: Option<&str>) -> Value {
    let mut events = json!({ "bot_events": ["message.channels", "message.groups"] });
    if let Some(u) = request_url {
        events["request_url"] = json!(u);
    }
    json!({
        "display_information": { "name": "Huntwell Support", "description": "Huntwell support chats, answered in threads." },
        "features": { "bot_user": { "display_name": "Huntwell Support", "always_online": true } },
        "oauth_config": { "scopes": { "bot": [
            "chat:write", "files:write", "reactions:write", "users:read",
            "channels:history", "groups:history", "channels:read", "groups:read"
        ] } },
        "settings": {
            "event_subscriptions": events,
            "socket_mode_enabled": false,
            "org_deploy_enabled": false,
            "token_rotation_enabled": false
        }
    })
}

// ---- configuration -------------------------------------------------------------

#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub bot: String,
    pub channel: String,
    /// Empty when none is saved: posting works, replies are not heard.
    pub signing: String,
}

/// Never the secrets, wherever it is printed.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config").field("channel", &self.channel).finish_non_exhaustive()
    }
}

pub enum NoConfig {
    /// Nobody connected Slack: chats live in the admin only.
    NotSet,
    /// Saved, but unusable here; the reason, for the log and the admin.
    Broken(&'static str),
}

pub fn valid_bot_token(t: &str) -> bool {
    t.starts_with("xoxb-") && t.len() <= 255 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The 32-character hex value on the app's Basic Information page.
pub fn valid_signing_secret(s: &str) -> bool {
    (32..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn valid_channel(c: &str) -> bool {
    (c.starts_with('C') || c.starts_with('G')) && (9..=20).contains(&c.len()) && c.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

pub async fn config(db: &Db) -> Result<Config, NoConfig> {
    let get = |k: &'static str| async move { store::get_setting(db, k).await.ok().flatten().unwrap_or_default() };
    let (bot, signing, channel) = (get(BOT_TOKEN_KEY).await, get(SIGNING_KEY).await, get(CHANNEL_KEY).await);
    if bot.trim().is_empty() || channel.trim().is_empty() {
        return Err(NoConfig::NotSet);
    }
    let open = |s: &str| crate::web::signing::open_secret(s);
    let bot = open(&bot).filter(|t| valid_bot_token(t)).ok_or(NoConfig::Broken("the saved Slack bot token cannot be opened on this server"))?;
    // The signing secret is only for hearing replies; posting works without it.
    let signing = if signing.trim().is_empty() { String::new() } else { open(&signing).filter(|s| valid_signing_secret(s)).unwrap_or_default() };
    Ok(Config { bot, channel: channel.trim().to_string(), signing })
}

// ---- the Web API -----------------------------------------------------------------

/// A stand-in Slack for local testing (`HUNTWELL_SLACK_API_BASE`), honoured
/// only on a dev server — a deployment always talks to slack.com.
fn base() -> String {
    crate::config::is_dev()
        .then(|| crate::config::get("HUNTWELL_SLACK_API_BASE"))
        .flatten()
        .map(|b| b.trim().trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "https://slack.com/api".into())
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("HuntwellBot (+slack)")
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// What a Web API error code means to the operator who has to fix it.
pub fn explain(code: &str) -> String {
    match code {
        "not_in_channel" => "The Huntwell Support app is not in that channel. In Slack, open the channel and type /invite @Huntwell Support.".into(),
        "channel_not_found" => "Slack cannot find that channel. Check the channel ID, and for a private channel invite the app first.".into(),
        "is_archived" => "That Slack channel is archived.".into(),
        "invalid_auth" | "not_authed" | "account_inactive" | "token_revoked" | "token_expired" => {
            "Slack refused the token. Paste a fresh one from the app's settings.".into()
        }
        "not_allowed_token_type" => "That token is the wrong kind. The bot token starts xoxb-.".into(),
        "missing_scope" => "The Slack app is missing a permission. Create it from the manifest on this page, or add the scopes listed there and reinstall it.".into(),
        "msg_too_long" => "The message is too long for Slack.".into(),
        "" => "Slack refused the request.".into(),
        c => format!("Slack refused the request ({c})."),
    }
}

/// One Web API call. Reads go as a query (several read methods refuse JSON);
/// writes as JSON.
async fn call(token: &str, method: &str, body: Option<&Value>, query: &[(&str, &str)]) -> Result<Value, SendError> {
    let url = format!("{}/{method}", base());
    let req = match body {
        Some(b) => client().post(&url).json(b),
        None => client().get(&url).query(query),
    };
    let resp = req
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| SendError::Retry(if e.is_timeout() { "Slack did not answer in time".into() } else { "could not reach Slack".into() }, None))?;
    let status = resp.status().as_u16();
    let wait = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<i64>().ok());
    if status == 429 {
        return Err(SendError::Retry("Slack asked us to slow down".into(), wait));
    }
    if status >= 500 {
        return Err(SendError::Retry(format!("Slack had a problem (HTTP {status})"), wait));
    }
    let v: Value = resp.json().await.map_err(|_| SendError::Retry(format!("Slack sent an answer we could not read (HTTP {status})"), None))?;
    if v["ok"].as_bool() == Some(true) {
        return Ok(v);
    }
    let code = v["error"].as_str().unwrap_or_default();
    if code == "ratelimited" {
        return Err(SendError::Retry("Slack asked us to slow down".into(), wait));
    }
    if code == "missing_scope" {
        tracing::warn!(method, needed = v["needed"].as_str().unwrap_or_default(), "Slack app is missing a scope");
    }
    Err(SendError::Permanent(explain(code)))
}

/// Post to the channel, or into a thread. Returns the new message's ts.
pub async fn post(cfg: &Config, text: &str, thread_ts: Option<&str>, broadcast: bool) -> Result<String, SendError> {
    let mut body = json!({ "channel": cfg.channel, "text": text, "unfurl_links": false, "unfurl_media": false });
    if let Some(t) = thread_ts {
        body["thread_ts"] = json!(t);
        if broadcast {
            body["reply_broadcast"] = json!(true);
        }
    }
    let v = call(&cfg.bot, "chat.postMessage", Some(&body), &[]).await?;
    v["ts"].as_str().map(str::to_string).ok_or_else(|| SendError::Retry("Slack did not say where it posted".into(), None))
}

/// Put an image into a thread: Slack's three steps (reserve an upload,
/// send the bytes, share the file in the thread).
pub async fn upload(cfg: &Config, thread_ts: &str, filename: &str, content_type: &str, bytes: Vec<u8>) -> Result<(), SendError> {
    let length = bytes.len().to_string();
    let reserved = client()
        .post(format!("{}/files.getUploadURLExternal", base()))
        .bearer_auth(&cfg.bot)
        .form(&[("filename", filename), ("length", length.as_str())])
        .send()
        .await
        .map_err(|_| SendError::Retry("could not reach Slack".into(), None))?;
    let reserved: Value = reserved.json().await.map_err(|_| SendError::Retry("Slack sent an answer we could not read".into(), None))?;
    if reserved["ok"].as_bool() != Some(true) {
        return Err(SendError::Permanent(explain(reserved["error"].as_str().unwrap_or_default())));
    }
    let (Some(url), Some(file_id)) = (reserved["upload_url"].as_str(), reserved["file_id"].as_str()) else {
        return Err(SendError::Retry("Slack did not say where to upload".into(), None));
    };
    // The bytes go only to Slack's own upload host (a stand-in's, in dev).
    if !url.starts_with("https://files.slack.com/") && !(crate::config::is_dev() && url.starts_with(&base())) {
        return Err(SendError::Permanent("Slack gave an upload address that is not Slack's".into()));
    }
    let sent = client()
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(bytes)
        .send()
        .await
        .map_err(|_| SendError::Retry("could not reach Slack".into(), None))?;
    if !sent.status().is_success() {
        return Err(SendError::Retry(format!("Slack refused the upload (HTTP {})", sent.status().as_u16()), None));
    }
    let done = json!({ "files": [{ "id": file_id, "title": filename }], "channel_id": cfg.channel, "thread_ts": thread_ts });
    call(&cfg.bot, "files.completeUploadExternal", Some(&done), &[]).await.map(|_| ())
}

/// A message's screenshots into the thread. Best effort, like the person's
/// view of them: the text is already there, and a failure says so in the
/// thread rather than posting the text again on a retry.
async fn upload_images(db: &Db, cfg: &Config, thread_ts: &str, m: &SupportMessage) {
    let images = match store::support_message_images(db, m.message_id).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(message_id = m.message_id, "support screenshots not read: {e:#}");
            return;
        }
    };
    for (i, (content_type, bytes)) in images.into_iter().enumerate() {
        let ext = content_type.trim_start_matches("image/").replace("jpeg", "jpg");
        let name = format!("screenshot-{}-{}.{ext}", m.message_id, i + 1);
        if let Err(e) = upload(cfg, thread_ts, &name, &content_type, bytes).await {
            tracing::warn!(message_id = m.message_id, "support screenshot not uploaded to Slack: {}", e.message());
            let _ = post(cfg, "_A screenshot could not be added here. It is in the chat in admin → Support._", Some(thread_ts), false).await;
        }
    }
}

/// Mark a message with an emoji. Only ever a courtesy: failures are logged.
async fn react(cfg: &Config, ts: &str, name: &str) {
    let body = json!({ "channel": cfg.channel, "timestamp": ts, "name": name });
    if let Err(e) = call(&cfg.bot, "reactions.add", Some(&body), &[]).await {
        if !e.message().contains("already_reacted") {
            tracing::debug!("Slack reaction not added: {}", e.message());
        }
    }
}

/// Who replied, by the name Slack shows; the user id when it cannot say.
async fn user_name(cfg: &Config, user: &str) -> String {
    static NAMES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let names = NAMES.get_or_init(Default::default);
    if let Some(n) = names.lock().unwrap().get(user) {
        return n.clone();
    }
    let name = match call(&cfg.bot, "users.info", None, &[("user", user)]).await {
        Ok(v) => {
            let p = &v["user"]["profile"];
            [p["display_name"].as_str(), p["real_name"].as_str(), v["user"]["name"].as_str()]
                .into_iter()
                .flatten()
                .map(str::trim)
                .find(|s| !s.is_empty())
                .unwrap_or(user)
                .to_string()
        }
        Err(_) => user.to_string(),
    };
    let name: String = name.chars().take(80).collect();
    names.lock().unwrap().insert(user.to_string(), name.clone());
    name
}

/// Check what an operator is about to save, against Slack itself: the bot
/// token works, and the channel exists and the app is in it. (The signing
/// secret cannot be checked from here: Slack proves it by calling us.)
/// Returns how to show it: "#support in Acme".
pub async fn check(bot: &str, channel: &str) -> Result<String, String> {
    let who = call(bot, "auth.test", Some(&json!({})), &[]).await.map_err(|e| format!("Bot token: {}", e.message()))?;
    let team = who["team"].as_str().unwrap_or_default().to_string();
    let info = call(bot, "conversations.info", None, &[("channel", channel)]).await.map_err(|e| e.message().to_string())?;
    let name = info["channel"]["name"].as_str().unwrap_or(channel).to_string();
    if info["channel"]["is_member"].as_bool() == Some(false) {
        return Err(explain("not_in_channel"));
    }
    Ok(if team.is_empty() { format!("#{name}") } else { format!("#{name} in {team}") })
}

// ---- what is posted ----------------------------------------------------------------

/// Who is asking, as the thread names them: "Pat (pat@acme.com)", escaped.
fn who(c: &SupportConversation) -> String {
    let e = crate::slack::escape;
    if c.name.trim().is_empty() {
        e(&c.email)
    } else {
        format!("{} ({})", e(c.name.trim()), e(&c.email))
    }
}

/// The person's words as a Slack quote block. Escaped, so nothing in them can
/// mention a channel or become a link.
pub fn quote(text: &str) -> String {
    crate::slack::escape(text).lines().map(|l| format!(">{l}")).collect::<Vec<_>>().join("\n")
}

/// The thread's parent: who needs support, what they said, how to answer.
pub fn parent_text(c: &SupportConversation, said: Option<&str>) -> String {
    let mut out = format!("*{}* needs support", who(c));
    if let Some(said) = said.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str(&format!(":\n{}", quote(&super::clip(said, 3000))));
    }
    out.push_str("\n_Reply in this thread to answer them._");
    out
}

/// A message after the parent, in the thread: the person's words quoted, or
/// an answer someone typed in the admin.
pub fn thread_text(m: &SupportMessage) -> String {
    let body = quote(&super::clip(&m.body, 3500));
    if m.author == "operator" {
        let who = if m.operator_email.is_empty() { "An operator".to_string() } else { crate::slack::escape(&m.operator_email) };
        return format!("_{who} answered from the admin:_\n{body}");
    }
    body
}

/// The note in the thread when the person ends the chat.
pub fn ended_text(c: &SupportConversation) -> String {
    let name = if c.name.trim().is_empty() { crate::slack::escape(&c.email) } else { crate::slack::escape(c.name.trim()) };
    format!("_{name} ended this conversation._")
}

/// A reply as Slack sends it, as the person should read it: mentions and
/// channel links reduced to their names, links to their text and address,
/// and Slack's three escapes undone. The customer's bubble shows text, never
/// markup, so nothing here needs escaping again.
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let inner = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        let (target, label) = match inner.split_once('|') {
            Some((t, l)) => (t, Some(l)),
            None => (inner, None),
        };
        match target.chars().next() {
            // A person or a group: their name if Slack gave it, else nothing.
            Some('@') => out.push_str(&label.map(|l| format!("@{l}")).unwrap_or_default()),
            Some('#') => out.push_str(&label.map(|l| format!("#{l}")).unwrap_or_default()),
            // @here, @channel, dates: meant for Slack.
            Some('!') => out.push_str(label.unwrap_or_default()),
            _ => {
                let url = target.strip_prefix("mailto:").unwrap_or(target);
                match label {
                    Some(l) if l != url && l != target => out.push_str(&format!("{l} ({url})")),
                    _ => out.push_str(url),
                }
            }
        }
    }
    out.push_str(rest);
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&").trim().to_string()
}

// ---- Huntwell → Slack ------------------------------------------------------------

/// Post everything owed for one chat: the parent first if it has no thread
/// in this channel yet, then the rest into the thread.
async fn sync(db: &Db, cfg: &Config, c: &SupportConversation) -> Result<(), SendError> {
    let db_err = |e: anyhow::Error| SendError::Retry(format!("database: {e:#}"), None);
    let mut owed = store::support_slack_pending(db, c.conversation_id).await.map_err(db_err)?;
    let mut thread = (c.slack_channel == cfg.channel).then(|| c.slack_thread_ts.clone()).filter(|t| !t.is_empty());
    if thread.is_none() {
        if owed.is_empty() {
            return Ok(());
        }
        if owed.len() > BACKFILL {
            let keep_from = owed[owed.len() - BACKFILL].message_id;
            store::support_slack_skip(db, c.conversation_id, keep_from).await.map_err(db_err)?;
            owed.drain(..owed.len() - BACKFILL);
        }
        let said = owed.first().filter(|m| m.author == "customer").map(|m| m.body.clone());
        let ts = post(cfg, &parent_text(c, said.as_deref()), None, false).await?;
        store::support_set_thread(db, c.conversation_id, &cfg.channel, &ts).await.map_err(db_err)?;
        tracing::info!(conversation_id = c.conversation_id, "support chat posted to Slack");
        if said.is_some() {
            let first = owed.remove(0);
            upload_images(db, cfg, &ts, &first).await;
            store::support_slack_posted(db, first.message_id, &ts).await.map_err(db_err)?;
        }
        thread = Some(ts);
    }
    let thread = thread.unwrap_or_default();
    for (i, m) in owed.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(POST_GAP).await;
        }
        // The person's messages show in the channel too, so a reply on an
        // old thread is seen; answers from the admin stay in the thread.
        let broadcast = m.author == "customer";
        // A screenshot sent alone has no text to post: its files are the
        // message, marked with a ts no Slack message has.
        let ts = if m.body.trim().is_empty() {
            format!("file-{}", m.message_id)
        } else {
            post(cfg, &thread_text(m), Some(&thread), broadcast).await?
        };
        upload_images(db, cfg, &thread, m).await;
        store::support_slack_posted(db, m.message_id, &ts).await.map_err(db_err)?;
    }
    Ok(())
}

/// Say in the chat's thread that the person ended it. A courtesy, so a
/// failure is only logged; a chat that never reached Slack has no thread.
pub async fn note_ended(db: &Db, c: &SupportConversation) {
    let Ok(cfg) = config(db).await else { return };
    if c.slack_channel != cfg.channel || c.slack_thread_ts.is_empty() {
        return;
    }
    if let Err(e) = post(&cfg, &ended_text(c), Some(&c.slack_thread_ts), false).await {
        tracing::warn!(conversation_id = c.conversation_id, "end of support chat not noted in Slack: {}", e.message());
    }
}

/// Post every chat Slack is owed. The notification service calls this every
/// couple of seconds, with the mail outbox's lease and backoff.
pub async fn drain(db: &Db) {
    use crate::notification::{backoff_seconds, CLAIM_LEASE_SECONDS, MAX_ATTEMPTS};
    loop {
        let (c, attempts) = match store::claim_due_support_slack(db, CLAIM_LEASE_SECONDS).await {
            Ok(Some(x)) => x,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!("claiming a support Slack post: {e:#}");
                return;
            }
        };
        let id = c.conversation_id;
        let done = match config(db).await {
            // Nothing to retry: the chat is in the admin inbox either way.
            Err(NoConfig::NotSet) => SupportSlackDone::Dropped(""),
            Err(NoConfig::Broken(why)) => {
                tracing::error!(conversation_id = id, "support Slack post not sent: {why}");
                SupportSlackDone::Dropped(why)
            }
            Ok(cfg) => match sync(db, &cfg, &c).await {
                Ok(()) => SupportSlackDone::Sent,
                Err(SendError::Retry(why, wait)) if attempts < MAX_ATTEMPTS => {
                    tracing::warn!(conversation_id = id, attempts, "support Slack post failed, will retry: {why}");
                    let _ = store::finish_support_slack(db, id, SupportSlackDone::Retry(&why, wait.unwrap_or_else(|| backoff_seconds(attempts)))).await;
                    continue;
                }
                Err(e) => {
                    tracing::error!(conversation_id = id, "support Slack post not sent: {}", e.message());
                    let _ = store::finish_support_slack(db, id, SupportSlackDone::Dropped(e.message())).await;
                    continue;
                }
            },
        };
        let _ = store::finish_support_slack(db, id, done).await;
    }
}

// ---- Slack → Huntwell ------------------------------------------------------------

/// Did Slack sign this request? `v0=` HMAC-SHA256 of `v0:{timestamp}:{body}`
/// with the signing secret, and a timestamp within five minutes of `now`, so
/// a captured request cannot be replayed later. Compared in constant time.
pub fn verify_signature(secret: &str, timestamp: &str, signature: &str, body: &[u8], now: i64) -> bool {
    let Ok(ts) = timestamp.trim().parse::<i64>() else {
        return false;
    };
    if (now - ts).abs() > MAX_SKEW_SECS {
        return false;
    }
    let Some(given) = signature.trim().strip_prefix("v0=").and_then(|h| hex::decode(h).ok()) else {
        return false;
    };
    let Ok(mut mac) = <Hmac<Sha256>>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(b"v0:");
    mac.update(timestamp.trim().as_bytes());
    mac.update(b":");
    mac.update(body);
    mac.verify_slice(&given).is_ok()
}

/// What a call to `/slack/events` is.
#[derive(Debug)]
pub enum Inbound {
    /// No signing secret saved: nobody set this up, so the URL is not here.
    NotSet,
    /// Not signed by Slack, or too old.
    Refused,
    /// Signed, but not JSON.
    Unreadable,
    /// Slack proving the Request URL while the app is set up.
    Challenge(String),
    /// An event: `Some` when it is an answer in a support thread, to deliver
    /// once the request has been answered (Slack retries anything slower than
    /// three seconds).
    Event(Option<(Config, Reply)>),
}

/// Read a call to `/slack/events`: the signature first, then the body.
pub async fn inbound(db: &Db, timestamp: &str, signature: &str, raw: &[u8]) -> Inbound {
    let cfg = match config(db).await {
        Ok(c) if !c.signing.is_empty() => c,
        _ => return Inbound::NotSet,
    };
    if !verify_signature(&cfg.signing, timestamp, signature, raw, Utc::now().timestamp()) {
        return Inbound::Refused;
    }
    let _ = store::set_setting(db, HEARD_KEY, &Utc::now().to_rfc3339()).await;
    let Ok(body) = serde_json::from_slice::<Value>(raw) else {
        return Inbound::Unreadable;
    };
    match body["type"].as_str() {
        Some("url_verification") => Inbound::Challenge(body["challenge"].as_str().unwrap_or_default().to_string()),
        Some("event_callback") => Inbound::Event(reply_in(&body["event"], &cfg.channel).map(|r| (cfg, r))),
        _ => Inbound::Event(None),
    }
}

/// A thread reply, as the Events API hands it over.
#[derive(Debug, PartialEq)]
pub struct Reply {
    pub user: String,
    pub text: String,
    pub ts: String,
    pub thread_ts: String,
    pub files: bool,
}

/// The one kind of event that is an operator answering: a person's message,
/// in the support channel, inside a thread (not the parent itself). Bots —
/// this app's own posts included — edits, deletions and joins are not.
pub fn reply_in(event: &Value, channel: &str) -> Option<Reply> {
    if event["type"] != "message" || event["channel"].as_str() != Some(channel) {
        return None;
    }
    if !matches!(event["subtype"].as_str(), None | Some("file_share") | Some("thread_broadcast")) || event.get("bot_id").is_some_and(|b| !b.is_null()) {
        return None;
    }
    let s = |k: &str| event[k].as_str().unwrap_or_default().to_string();
    let (ts, thread_ts, user) = (s("ts"), s("thread_ts"), s("user"));
    if ts.is_empty() || thread_ts.is_empty() || ts == thread_ts || user.is_empty() {
        return None;
    }
    let files = event["files"].as_array().is_some_and(|f| !f.is_empty());
    Some(Reply { user, text: s("text"), ts, thread_ts, files })
}

/// An operator answered in a thread: give it to the person. Failures are
/// logged; Slack has had its answer already.
pub async fn deliver(db: &Db, cfg: &Config, r: Reply) {
    if let Err(e) = on_reply(db, cfg, r).await {
        tracing::warn!("a support reply from Slack was not delivered: {e:#}");
    }
}

async fn on_reply(db: &Db, cfg: &Config, r: Reply) -> anyhow::Result<()> {
    let Some(c) = store::support_by_thread(db, &cfg.channel, &r.thread_ts).await? else {
        return Ok(()); // some other thread in the channel
    };
    let body = plain(&r.text);
    if body.starts_with(INTERNAL_PREFIX) {
        return Ok(());
    }
    let note = |text: String| {
        let (cfg, thread) = (cfg.clone(), r.thread_ts.clone());
        async move {
            let _ = post(&cfg, &text, Some(&thread), false).await;
        }
    };
    if body.is_empty() {
        if r.files {
            note("_Files do not reach the person, only text. Nothing was sent._".into()).await;
        }
        return Ok(());
    }
    if body.chars().count() > store::SUPPORT_BODY_MAX {
        note(format!("_Not sent: a reply can be at most {} characters._", store::SUPPORT_BODY_MAX)).await;
        return Ok(());
    }
    let name = user_name(cfg, &r.user).await;
    let Some(_) = store::support_operator_reply(db, c.conversation_id, &format!("Slack: {name}"), &body, &[], Some(&r.ts)).await? else {
        return Ok(()); // delivered twice; recorded the first time
    };
    let emailed = super::email_reply_if_away(db, &c, &body).await.unwrap_or_else(|e| {
        tracing::warn!(conversation_id = c.conversation_id, "support reply not emailed: {e:#}");
        false
    });
    tracing::info!(conversation_id = c.conversation_id, operator = %name, emailed, "support reply from Slack");
    react(cfg, &r.ts, "white_check_mark").await;
    if emailed {
        react(cfg, &r.ts, "email").await;
    }
    if r.files {
        note("_Sent the text. Files do not reach the person._".into()).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(account: Option<i64>) -> SupportConversation {
        SupportConversation {
            conversation_id: 42,
            account_id: account,
            workspace_id: account,
            email: "pat@example.com".into(),
            name: "Pat <!channel>".into(),
            token_sealed: String::new(),
            status: "open".into(),
            page: "/pricing".into(),
            ip: String::new(),
            created_at: Utc::now(),
            last_message_at: Utc::now(),
            customer_seen_at: None,
            operator_seen_at: None,
            closed_at: None,
            closed_by: String::new(),
            slack_reason: "new".into(),
            slack_error: String::new(),
            slack_channel: String::new(),
            slack_thread_ts: String::new(),
        }
    }

    fn msg(author: &str, body: &str) -> SupportMessage {
        SupportMessage {
            message_id: 1,
            conversation_id: 42,
            author: author.into(),
            operator_email: if author == "operator" { "op@huntwell.ai".into() } else { String::new() },
            body: body.into(),
            created_at: Utc::now(),
            slack_ts: String::new(),
            image_ids: Vec::new(),
        }
    }

    fn sign(secret: &str, ts: &str, body: &[u8]) -> String {
        let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("v0:{ts}:").as_bytes());
        mac.update(body);
        format!("v0={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn the_parent_says_who_needs_support_and_escapes_what_they_typed() {
        let t = parent_text(&conv(None), Some("hi <!here> see <https://evil.example|bank>\nline two"));
        assert_eq!(
            t,
            "*Pat &lt;!channel&gt; (pat@example.com)* needs support:\n>hi &lt;!here&gt; see &lt;https://evil.example|bank&gt;\n>line two\n_Reply in this thread to answer them._"
        );
        // Without words of their own (an answer was owed first).
        assert_eq!(parent_text(&conv(Some(7)), None), "*Pat &lt;!channel&gt; (pat@example.com)* needs support\n_Reply in this thread to answer them._");
    }

    #[test]
    fn thread_messages_are_quoted_and_the_end_is_noted() {
        let c = conv(None);
        assert_eq!(thread_text(&msg("customer", "a <b>\nc")), ">a &lt;b&gt;\n>c");
        assert_eq!(thread_text(&msg("operator", "on it")), "_op@huntwell.ai answered from the admin:_\n>on it");
        assert_eq!(ended_text(&c), "_Pat &lt;!channel&gt; ended this conversation._");
    }

    #[test]
    fn a_slack_reply_reads_as_plain_text() {
        assert_eq!(plain("Hi <@U123|sam>, see <https://huntwell.ai/pricing|pricing> &amp; <https://x.io>"), "Hi @sam, see pricing (https://huntwell.ai/pricing) & https://x.io");
        assert_eq!(plain("<mailto:a@b.co|a@b.co> <!here> 1 &lt; 2"), "a@b.co  1 < 2");
        assert_eq!(plain("<@U123> thanks"), "thanks");
        assert_eq!(plain("unclosed < bracket"), "unclosed < bracket");
        assert_eq!(plain("  // note to self "), "// note to self");
    }

    #[test]
    fn only_a_fresh_request_signed_with_the_secret_is_from_slack() {
        let secret = "8f742231b10e8888abcd990000008a5a".to_string();
        let body = br#"{"type":"event_callback"}"#;
        let good = sign(&secret, "1000", body);
        assert!(verify_signature(&secret, "1000", &good, body, 1000));
        assert!(verify_signature(&secret, "1000", &good, body, 1000 + MAX_SKEW_SECS));
        // Replayed later, signed with another secret, or altered.
        assert!(!verify_signature(&secret, "1000", &good, body, 1000 + MAX_SKEW_SECS + 1));
        assert!(!verify_signature("0123456789abcdef0123456789abcdef", "1000", &good, body, 1000));
        assert!(!verify_signature(&secret, "1000", &good, br#"{"type":"x"}"#, 1000));
        assert!(!verify_signature(&secret, "abc", &good, body, 1000));
        assert!(!verify_signature(&secret, "1000", "v0=zz", body, 1000));
        assert!(!verify_signature(&secret, "1000", "", body, 1000));
    }

    #[test]
    fn only_a_person_replying_in_a_thread_is_an_answer() {
        let ev = json!({"type":"message","channel":"C0123456789","user":"U1","text":"hello","ts":"2.0","thread_ts":"1.0"});
        let r = reply_in(&ev, "C0123456789").unwrap();
        assert_eq!((r.user.as_str(), r.ts.as_str(), r.thread_ts.as_str(), r.files), ("U1", "2.0", "1.0", false));
        // Another channel, the parent itself, a top-level post, a bot, an edit: no.
        assert!(reply_in(&ev, "C9999999999").is_none());
        let mut e = ev.clone();
        e["thread_ts"] = json!("2.0");
        assert!(reply_in(&e, "C0123456789").is_none());
        let mut e = ev.clone();
        e.as_object_mut().unwrap().remove("thread_ts");
        assert!(reply_in(&e, "C0123456789").is_none());
        let mut e = ev.clone();
        e["bot_id"] = json!("B1");
        assert!(reply_in(&e, "C0123456789").is_none());
        let mut e = ev.clone();
        e["subtype"] = json!("message_changed");
        assert!(reply_in(&e, "C0123456789").is_none());
        // "Also send to channel" and a file with text are still answers.
        let mut e = ev.clone();
        e["subtype"] = json!("thread_broadcast");
        assert!(reply_in(&e, "C0123456789").is_some());
        let mut e = ev.clone();
        e["subtype"] = json!("file_share");
        e["files"] = json!([{"id":"F1"}]);
        assert!(reply_in(&e, "C0123456789").unwrap().files);
    }

    #[test]
    fn tokens_secrets_and_channels_are_checked_for_shape() {
        assert!(valid_bot_token("xoxb-123-456-abcDEF"));
        assert!(!valid_bot_token("xapp-1-A1-2-abc"));
        assert!(!valid_bot_token("xoxb-bad token"));
        assert!(valid_signing_secret("0123456789abcdef0123456789abcdef"));
        assert!(!valid_signing_secret("0123456789abcdef") && !valid_signing_secret("xoxb-123-456-abcdef0123456789abcdef"));
        assert!(valid_channel("C0123456789") && valid_channel("G01ABCDEF"));
        assert!(!valid_channel("#support") && !valid_channel("c0123456789") && !valid_channel("C01"));
    }

    #[test]
    fn the_manifest_asks_for_what_the_code_uses() {
        let m = manifest(Some("https://huntwell.ai/slack/events"));
        let scopes: Vec<&str> = m["oauth_config"]["scopes"]["bot"].as_array().unwrap().iter().filter_map(|s| s.as_str()).collect();
        for s in ["chat:write", "reactions:write", "users:read", "channels:history", "groups:history", "channels:read"] {
            assert!(scopes.contains(&s), "{s}");
        }
        assert_eq!(m["settings"]["socket_mode_enabled"], false);
        assert_eq!(m["settings"]["event_subscriptions"]["request_url"], "https://huntwell.ai/slack/events");
        assert!(manifest(None)["settings"]["event_subscriptions"].get("request_url").is_none());
    }
}

#[cfg(test)]
mod two_way_tests {
    use super::*;
    use axum::extract::{Query, State};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use std::sync::Arc;

    const SECRET: &str = "00112233445566778899aabbccddeeff";

    /// What the stand-in Slack saw.
    #[derive(Default)]
    struct Stub {
        posts: Mutex<Vec<Value>>,
        reactions: Mutex<Vec<String>>,
        /// Uploaded bytes' lengths, and the shares that finished them.
        uploads: Mutex<Vec<usize>>,
        shared: Mutex<Vec<Value>>,
        addr: Mutex<String>,
    }

    async fn stub_slack() -> Arc<Stub> {
        let stub = Arc::new(Stub::default());
        let app = Router::new()
            .route("/chat.postMessage", post(|State(s): State<Arc<Stub>>, Json(b): Json<Value>| async move {
                let mut posts = s.posts.lock().unwrap();
                posts.push(b);
                Json(json!({ "ok": true, "ts": format!("1700000000.{:06}", posts.len()) }))
            }))
            .route("/reactions.add", post(|State(s): State<Arc<Stub>>, Json(b): Json<Value>| async move {
                s.reactions.lock().unwrap().push(format!("{}:{}", b["timestamp"].as_str().unwrap(), b["name"].as_str().unwrap()));
                Json(json!({ "ok": true }))
            }))
            .route("/files.getUploadURLExternal", post(|State(s): State<Arc<Stub>>| async move {
                Json(json!({ "ok": true, "upload_url": format!("http://{}/upload", s.addr.lock().unwrap()), "file_id": "F1" }))
            }))
            .route("/upload", post(|State(s): State<Arc<Stub>>, body: axum::body::Bytes| async move {
                s.uploads.lock().unwrap().push(body.len());
                "OK"
            }))
            .route("/files.completeUploadExternal", post(|State(s): State<Arc<Stub>>, Json(b): Json<Value>| async move {
                s.shared.lock().unwrap().push(b);
                Json(json!({ "ok": true }))
            }))
            .route("/users.info", get(|Query(q): Query<HashMap<String, String>>| async move {
                Json(json!({ "ok": true, "user": { "name": q["user"], "profile": { "display_name": "Sam", "real_name": "Sam Ops" } } }))
            }))
            .with_state(stub.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        *stub.addr.lock().unwrap() = addr.to_string();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        std::env::set_var("HUNTWELL_DEV", "1");
        std::env::set_var("HUNTWELL_SLACK_API_BASE", format!("http://{addr}"));
        stub
    }

    /// Slack calling `/slack/events` with one message event, signed.
    async fn slack_calls(db: &Db, text: &str, user: &str, ts: &str, thread: &str, extra: Value) -> Inbound {
        let mut ev = json!({ "type": "message", "channel": "CSUPPORT01", "user": user, "text": text, "ts": ts, "thread_ts": thread });
        if let (Some(e), Some(x)) = (ev.as_object_mut(), extra.as_object()) {
            e.extend(x.clone());
        }
        let raw = json!({ "type": "event_callback", "event": ev }).to_string();
        let now = Utc::now().timestamp().to_string();
        inbound(db, &now, &tests_sign(&now, raw.as_bytes()), raw.as_bytes()).await
    }

    fn tests_sign(ts: &str, body: &[u8]) -> String {
        let mut mac = <Hmac<Sha256>>::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(format!("v0:{ts}:").as_bytes());
        mac.update(body);
        format!("v0={}", hex::encode(mac.finalize().into_bytes()))
    }

    async fn deliver_if_any(db: &Db, i: Inbound) -> bool {
        match i {
            Inbound::Event(Some((cfg, r))) => {
                deliver(db, &cfg, r).await;
                true
            }
            Inbound::Event(None) => false,
            other => panic!("not an event: {other:?}"),
        }
    }

    /// The whole round trip against a stand-in Slack: a visitor's first
    /// message starts a thread, the next is quoted into it and shown in the
    /// channel, an admin answer is copied in; Slack proves the Request URL; a
    /// reply typed in the thread reaches the visitor (once, even delivered
    /// twice), a `//` note, a bot's post and an unsigned call do not, and the
    /// reply is marked delivered. Ending the chat says so in the thread.
    #[tokio::test]
    #[ignore = "needs the dev database"]
    async fn a_chat_is_a_slack_thread_both_ways() {
        let stub = stub_slack().await;
        let db = store::connect(&crate::config::database_url().unwrap(), 2).await.unwrap();
        let seal = |s: &str| crate::web::signing::seal_secret(s).expect("dev has a session secret");
        // The settings this test needs, put back afterwards.
        let keys = [BOT_TOKEN_KEY, SIGNING_KEY, CHANNEL_KEY, HEARD_KEY];
        let mut saved = Vec::new();
        for k in keys {
            saved.push(store::get_setting(&db, k).await.unwrap().unwrap_or_default());
        }
        store::set_setting(&db, BOT_TOKEN_KEY, &seal("xoxb-1-2-test")).await.unwrap();
        store::set_setting(&db, SIGNING_KEY, &seal(SECRET)).await.unwrap();
        store::set_setting(&db, CHANNEL_KEY, "CSUPPORT01").await.unwrap();
        let cfg = match config(&db).await {
            Ok(c) => c,
            Err(_) => panic!("the settings just saved do not open"),
        };
        // An earlier run that failed half way left its chat on the stand-in's thread.
        sqlx::query("DELETE FROM support_conversation WHERE slack_channel='CSUPPORT01'").execute(&db).await.unwrap();
        sqlx::query("DELETE FROM mail_outbox WHERE to_address LIKE 'thread+%@support.test'").execute(&db).await.unwrap();
        let tag = Utc::now().timestamp_nanos_opt().unwrap();
        let email = format!("thread+{tag}@support.test");
        let token = crate::support::new_token();
        let c = store::support_start(&db, &store::NewSupportChat {
            account_id: None, workspace_id: None, email: &email, name: "Val <b>", token: Some((&store::sha256_hex(&token), &seal(&token))), page: "/pricing", ip: "",
        }, Some("Is there a free trial?"), &[]).await.unwrap();
        let id = c.conversation_id;
        let reload = || async { store::support_get(&db, id).await.unwrap().unwrap() };

        // The first message: the parent, quoting it.
        sync(&db, &cfg, &reload().await).await.unwrap();
        let thread = reload().await.slack_thread_ts;
        assert_eq!(thread, "1700000000.000001");
        {
            let posts = stub.posts.lock().unwrap();
            assert_eq!(posts.len(), 1);
            assert!(posts[0].get("thread_ts").is_none());
            assert_eq!(posts[0]["channel"], "CSUPPORT01");
            assert_eq!(posts[0]["text"], format!("*Val &lt;b&gt; ({email})* needs support:\n>Is there a free trial?\n_Reply in this thread to answer them._"));
        }
        assert!(store::support_slack_pending(&db, id).await.unwrap().is_empty());

        // More from the visitor, and an answer from the admin: into the thread,
        // the visitor's shown in the channel too.
        store::support_customer_says(&db, id, "Also, do you do Reddit?", &[]).await.unwrap();
        store::support_operator_reply(&db, id, "op@admin.test", "Hi from the admin", &[], None).await.unwrap().unwrap();
        sync(&db, &cfg, &reload().await).await.unwrap();
        {
            let posts = stub.posts.lock().unwrap();
            assert_eq!(posts.len(), 3);
            assert!(posts[1..].iter().all(|p| p["thread_ts"] == thread.as_str()));
            assert_eq!(posts[1]["text"], ">Also, do you do Reddit?");
            assert_eq!(posts[1]["reply_broadcast"], true);
            assert!(posts[2]["text"].as_str().unwrap().contains("op@admin.test answered from the admin"));
            assert!(posts[2].get("reply_broadcast").is_none());
        }

        // A screenshot sent alone: no text post, the file shared in the thread.
        let png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 7, 7, 7];
        store::support_customer_says(&db, id, "", &[store::SupportImage { content_type: "image/png", bytes: png.clone() }]).await.unwrap();
        sync(&db, &cfg, &reload().await).await.unwrap();
        assert_eq!(stub.posts.lock().unwrap().len(), 3, "nothing posted for an image alone");
        assert_eq!(*stub.uploads.lock().unwrap(), vec![png.len()]);
        assert_eq!(stub.shared.lock().unwrap()[0]["thread_ts"], thread.as_str());
        assert!(store::support_slack_pending(&db, id).await.unwrap().is_empty());
        let shot = store::support_messages(&db, id, 0).await.unwrap().into_iter().last().unwrap();
        assert_eq!(shot.image_ids.len(), 1);
        assert_eq!(store::support_image(&db, shot.image_ids[0]).await.unwrap().unwrap().2, png);

        // Slack proving the Request URL; a call it did not sign.
        let raw = br#"{"type":"url_verification","challenge":"abc123"}"#;
        let now = Utc::now().timestamp().to_string();
        assert!(matches!(inbound(&db, &now, &tests_sign(&now, raw), raw).await, Inbound::Challenge(c) if c == "abc123"));
        assert!(matches!(inbound(&db, &now, "v0=00", raw).await, Inbound::Refused));
        let stale = (Utc::now().timestamp() - 600).to_string();
        assert!(matches!(inbound(&db, &stale, &tests_sign(&stale, raw), raw).await, Inbound::Refused));

        // Replies typed in Slack. The visitor has left the page.
        sqlx::query("UPDATE support_conversation SET customer_seen_at=now()-interval '10 minutes' WHERE conversation_id=$1").bind(id).execute(&db).await.unwrap();
        let reply = "Yes, <@U9|kim> can set it up &amp; more: <https://huntwell.ai/pricing|pricing>";
        assert!(deliver_if_any(&db, slack_calls(&db, reply, "U1", "1700000100.000001", &thread, json!({})).await).await);
        // Slack delivering the same reply again.
        assert!(deliver_if_any(&db, slack_calls(&db, reply, "U1", "1700000100.000001", &thread, json!({})).await).await);
        assert!(deliver_if_any(&db, slack_calls(&db, "// they asked about Reddit before", "U1", "1700000100.000002", &thread, json!({})).await).await);
        assert!(!deliver_if_any(&db, slack_calls(&db, "a bot talking", "U1", "1700000100.000003", &thread, json!({ "bot_id": "B1" })).await).await);
        assert!(deliver_if_any(&db, slack_calls(&db, "wrong thread", "U1", "1700000100.000004", "1699999999.000001", json!({})).await).await);
        assert!(!store::get_setting(&db, HEARD_KEY).await.unwrap().unwrap_or_default().is_empty());

        let msgs = store::support_messages(&db, id, 0).await.unwrap();
        let from_slack: Vec<&SupportMessage> = msgs.iter().filter(|m| m.operator_email.starts_with("Slack:")).collect();
        assert_eq!(from_slack.len(), 1, "once, and no note or bot post");
        assert_eq!(from_slack[0].body, "Yes, @kim can set it up & more: pricing (https://huntwell.ai/pricing)");
        assert_eq!(from_slack[0].operator_email, "Slack: Sam");
        assert_eq!(from_slack[0].slack_ts, "1700000100.000001");
        // Never posted back into the thread it came from.
        assert!(store::support_slack_pending(&db, id).await.unwrap().is_empty());
        // The visitor was away, so it was emailed too, and the reply says both.
        let reactions = stub.reactions.lock().unwrap().clone();
        assert_eq!(reactions, vec!["1700000100.000001:white_check_mark", "1700000100.000001:email"]);
        let mailed: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_outbox WHERE to_address=$1 AND kind='support_reply'")
            .bind(&email).fetch_one(&db).await.unwrap();
        assert_eq!(mailed, 1);
        // The visitor sees it as an unread answer from support.
        let c = reload().await;
        assert_eq!(store::support_unread_for_customer(&c, &msgs), 2);

        // The visitor ends the chat: the thread says so, and it stays ended
        // when an answer arrives after.
        assert!(store::support_end(&db, id, &email).await.unwrap());
        note_ended(&db, &reload().await).await;
        {
            let posts = stub.posts.lock().unwrap();
            let last = posts.last().unwrap();
            assert_eq!(last["text"], "_Val &lt;b&gt; ended this conversation._");
            assert_eq!(last["thread_ts"], thread.as_str());
        }
        assert!(deliver_if_any(&db, slack_calls(&db, "One last thing from us", "U1", "1700000100.000005", &thread, json!({})).await).await);
        assert_eq!(reload().await.status, "ended");
        store::finish_support_slack(&db, id, SupportSlackDone::Sent).await.unwrap();
        assert_eq!(reload().await.slack_reason, "sent");

        sqlx::query("DELETE FROM mail_outbox WHERE to_address=$1").bind(&email).execute(&db).await.unwrap();
        sqlx::query("DELETE FROM support_conversation WHERE conversation_id=$1").bind(id).execute(&db).await.unwrap();
        for (k, v) in keys.iter().zip(saved) {
            store::set_setting(&db, k, &v).await.unwrap();
        }
    }
}

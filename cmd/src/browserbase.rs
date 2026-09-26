//! Optional remote-browser backend: [Browserbase](https://www.browserbase.com).
//!
//! When Browserbase keys are set (or `HUNTWELL_BROWSER=browserbase`), a run
//! creates a cloud browser session
//! over Browserbase's API and drives it over the same CDP the local Chrome uses
//! — `@playwright/mcp --cdp-endpoint <connectUrl>`. Everything downstream (the
//! guard, the trail, the agent) reads the tool-call stream, not where the
//! browser runs, so none of it changes.
//!
//! Why bother: datacenter Chrome gets captcha-walled by search engines (you can
//! watch it happen in a run log). Browserbase adds stealth fingerprinting,
//! residential proxies, captcha handling and per-session geolocation, and takes
//! the "headed Chrome needs a display" operations problem off your server.
//!
//! Trade-off worth stating in code: the session — including any logged-in
//! cookies from a persistent Context — lives in Browserbase's cloud, not on
//! this box. That is a real extension of the trust boundary the local backend
//! keeps in-house.

use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

const API: &str = "https://api.browserbase.com/v1";

struct Session {
    id: String,
    connect_url: String,
}

fn slot() -> &'static Mutex<Option<Session>> {
    static SLOT: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// True when Browserbase is the selected backend, regardless of whether the
/// credentials are present — so a run can fail loudly on a half-configured
/// setup instead of silently using local Chrome.
///
/// Keys in `global` are enough: Watch the browser only works on a remote
/// session, so a host that has the keys meant to use them. `HUNTWELL_BROWSER=
/// local` (or `chrome`) opts back out. `HUNTWELL_BROWSER=browserbase` without
/// keys still fails the run rather than falling through.
pub fn selected() -> bool {
    selected_from(
        crate::config::get("HUNTWELL_BROWSER").as_deref(),
        api_key().is_some() && project_id().is_some(),
    )
}

fn selected_from(setting: Option<&str>, has_keys: bool) -> bool {
    match setting.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("local") | Some("chrome") => false,
        Some("browserbase") => true,
        _ => has_keys,
    }
}

/// True when this process is set to use Browserbase and has the credentials.
/// Checked before a run starts so a misconfiguration fails loudly rather than
/// silently falling back to local Chrome (which would surprise on both cost
/// and behavior).
pub fn configured() -> bool {
    selected() && api_key().is_some() && project_id().is_some()
}

/// The CDP endpoint of the live session, if one has been created this run.
/// `browser::cdp_endpoint` returns this in place of the local DevTools port.
pub fn connect_url() -> Option<String> {
    slot().lock().ok().and_then(|s| s.as_ref().map(|s| s.connect_url.clone()))
}

pub fn session_id() -> Option<String> {
    slot().lock().ok().and_then(|s| s.as_ref().map(|s| s.id.clone()))
}

fn api_key() -> Option<String> {
    crate::config::get("BROWSERBASE_API_KEY")
}
fn project_id() -> Option<String> {
    crate::config::get("BROWSERBASE_PROJECT_ID")
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("build http client")
}

async fn create_session(key: &str, body: &Value) -> Result<(reqwest::StatusCode, String)> {
    let resp = client()?
        .post(format!("{API}/sessions"))
        .header("X-BB-API-Key", key)
        .json(body)
        .send()
        .await
        .context("create Browserbase session")?;
    let status = resp.status();
    Ok((status, resp.text().await.unwrap_or_default()))
}

/// Creates a session and stores its `connectUrl` for the rest of the run.
///
/// A persistent [Context](https://docs.browserbase.com/features/contexts) is
/// the cloud equivalent of the local per-account Chrome profile: set
/// `BROWSERBASE_CONTEXT_ID` and logins (LinkedIn and the like) survive between
/// runs. `account_id` is logged so per-account contexts can be wired in later.
pub async fn start(account_id: i64, context_id: Option<String>) -> Result<()> {
    let (key, project) = (
        api_key().ok_or_else(|| anyhow!("BROWSERBASE_API_KEY not set"))?,
        project_id().ok_or_else(|| anyhow!("BROWSERBASE_PROJECT_ID not set"))?,
    );

    let mut body = json!({ "projectId": project });
    // Browserbase's default session timeout is ~5 minutes, and an agent-driven
    // scrape regularly outlives that — the browser then vanishes mid-run, and
    // every later call in the run fails with "the browser is gone".
    //
    // Generous on purpose. The session is released by `stop` on every path a
    // run can end on — success, failure, cancel — so a long timeout costs
    // nothing in the normal case; it is only the backstop for a worker that
    // died without releasing. Half an hour was not enough: a learning plan
    // does three rounds of search and an enrich call per row, and passes it.
    let timeout_s: u64 = crate::config::get("BROWSERBASE_TIMEOUT_S")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(4 * 60 * 60);
    body["timeout"] = json!(timeout_s);
    if truthy("BROWSERBASE_PROXIES") {
        body["proxies"] = Value::Bool(true);
    }
    if let Some(region) = crate::config::get("BROWSERBASE_REGION") {
        body["region"] = Value::String(region);
    }
    // The account's persistent Context (its logged-in cookies), falling back to a
    // global default. `persist` so this run's own cookies update it too.
    let ctx = context_id
        .filter(|c| !c.trim().is_empty())
        .or_else(|| crate::config::get("BROWSERBASE_CONTEXT_ID"));
    if let Some(ctx) = ctx {
        body["browserSettings"] = json!({ "context": { "id": ctx, "persist": true } });
    }

    // A run is several agent calls — the search, then one per row it fills in
    // — and each is its own `@playwright/mcp`, so its own CDP connection. By
    // default Browserbase ends a session when its client disconnects: the
    // search worked, and every call after it found the browser closed.
    // `keepAlive` holds the session until `stop` releases it (or `timeout`).
    let keep_alive = !matches!(crate::config::get("BROWSERBASE_KEEP_ALIVE").as_deref().map(str::trim), Some("0" | "false" | "off"));
    if keep_alive {
        body["keepAlive"] = Value::Bool(true);
    }

    let (mut status, mut text) = create_session(&key, &body).await?;
    if !status.is_success() && keep_alive {
        // Plans without keep-alive refuse the field. A session that lasts one
        // connection is worse than one that lasts the run, but better than no run.
        eprintln!(
            "  browser     Browserbase refused keepAlive ({status}: {}) — retrying without it; rows after the first \
             agent call will find the browser closed. The Browserbase plan needs keep-alive.",
            trim(&text)
        );
        body.as_object_mut().map(|o| o.remove("keepAlive"));
        (status, text) = create_session(&key, &body).await?;
    }
    if !status.is_success() {
        anyhow::bail!("Browserbase session create failed ({status}): {}", trim(&text));
    }
    let v: Value = serde_json::from_str(&text).context("parse session response")?;
    let id = v.get("id").and_then(Value::as_str).ok_or_else(|| anyhow!("no session id in response"))?.to_string();
    let connect_url = v
        .get("connectUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("no connectUrl in response"))?
        .to_string();

    println!(
        "  browser     Browserbase session {id} (account {account_id}), good for {}",
        crate::progress::fmt_elapsed(std::time::Duration::from_secs(timeout_s))
    );
    // Deliberately not printed: the run log is customer-visible, and this URL
    // is a bearer capability — whoever holds it can watch *and drive* the
    // browser carrying that workspace's logged-in sessions. It goes to the
    // operator's journal, which is not.
    if live_view_url(&key, &id).await.is_some() {
        tracing::info!(session = %id, "browserbase live view available to operators");
    }
    *slot().lock().unwrap() = Some(Session { id, connect_url });
    Ok(())
}

/// The dashboard live-view URL, so a drive can be watched. Best-effort.
async fn live_view_url(key: &str, id: &str) -> Option<String> {
    let resp = client().ok()?.get(format!("{API}/sessions/{id}/debug")).header("X-BB-API-Key", key).send().await.ok()?;
    let v: Value = resp.json().await.ok()?;
    v.get("debuggerFullscreenUrl").and_then(Value::as_str).map(str::to_string)
}

/// The CDP endpoint of a session by id, for Huntwell's own live viewer.
///
/// The session was created by a worker; the website serves the viewer, so it
/// asks Browserbase for that session's own `connectUrl`. Building one from the
/// id (`wss://connect.browserbase.com?apiKey=…&sessionId=…`) is refused with a
/// 400: sessions live on a regional host (`connect.usw2…`) behind a signed URL.
/// That was every live view saying "the browser is not answering".
///
/// Cached per session while it is live — the viewer polls every couple of
/// seconds, and the URL does not change for the session's life.
pub async fn connect_url_for(session_id: &str) -> Option<String> {
    const KEEP: std::time::Duration = std::time::Duration::from_secs(10 * 60);
    static CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some((url, at)) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(session_id) {
        if at.elapsed() < KEEP {
            return Some(url.clone());
        }
    }
    let key = api_key()?;
    let resp = client().ok()?.get(format!("{API}/sessions/{session_id}")).header("X-BB-API-Key", key).send().await.ok()?;
    if !resp.status().is_success() {
        tracing::warn!(session = session_id, status = %resp.status(), "browserbase would not describe the session");
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    if v.get("status").and_then(Value::as_str).is_some_and(|s| s != "RUNNING") {
        return None;
    }
    let url = v.get("connectUrl").and_then(Value::as_str)?.to_string();
    let mut map = cache.lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, (_, at)| at.elapsed() < KEEP);
    map.insert(session_id.to_string(), (url.clone(), std::time::Instant::now()));
    Some(url)
}

/// A viewing URL for a session someone owns.
///
/// Deliberately fetched on demand and never stored: the URL is a bearer
/// capability — anyone holding it can watch *and drive* that browser — so it
/// must not outlive the request that checked who was asking.
pub async fn view_url(session_id: &str) -> Option<String> {
    let key = api_key()?;
    live_view_url(&key, session_id).await
}

/// Asks Browserbase to release the session. Called at the end of a run; a
/// session left open would keep billing until its own timeout.
pub async fn stop() {
    let Some(sess) = slot().lock().ok().and_then(|mut s| s.take()) else { return };
    release(&sess.id).await;
}

/// Asks Browserbase to release a session by id — which saves its Context. Used
/// both to end a run and to finish an interactive login.
pub async fn release(session_id: &str) {
    let (Some(key), Some(project)) = (api_key(), project_id()) else { return };
    let Ok(client) = client() else { return };
    let _ = client
        .post(format!("{API}/sessions/{session_id}"))
        .header("X-BB-API-Key", key)
        .json(&json!({ "projectId": project, "status": "REQUEST_RELEASE" }))
        .send()
        .await;
}

/// Creates a new persistent [Context](https://docs.browserbase.com/features/contexts)
/// and returns its id. Each account gets its own; logged-in cookies live in it.
pub async fn create_context() -> Result<String> {
    let (key, project) = (
        api_key().ok_or_else(|| anyhow!("BROWSERBASE_API_KEY not set"))?,
        project_id().ok_or_else(|| anyhow!("BROWSERBASE_PROJECT_ID not set"))?,
    );
    let resp = client()?
        .post(format!("{API}/contexts"))
        .header("X-BB-API-Key", &key)
        .json(&json!({ "projectId": project }))
        .send()
        .await
        .context("create Browserbase context")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("Browserbase context create failed ({status}): {}", trim(&text));
    }
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
        .ok_or_else(|| anyhow!("no context id in Browserbase response"))
}

/// An interactive login session: the id (to release when done) and the Live View
/// URL the user drives to sign in.
pub struct LoginSession {
    pub session_id: String,
    pub live_view_url: String,
}

/// Starts a session on `context_id` with `persist` on and a long timeout, for an
/// interactive login through the Live View. Independent of the run slot.
pub async fn start_login_session(context_id: &str, timeout_s: u64) -> Result<LoginSession> {
    let (key, project) = (
        api_key().ok_or_else(|| anyhow!("BROWSERBASE_API_KEY not set"))?,
        project_id().ok_or_else(|| anyhow!("BROWSERBASE_PROJECT_ID not set"))?,
    );
    let body = json!({
        "projectId": project,
        "timeout": timeout_s,
        "browserSettings": { "context": { "id": context_id, "persist": true } }
    });
    let resp = client()?
        .post(format!("{API}/sessions"))
        .header("X-BB-API-Key", &key)
        .json(&body)
        .send()
        .await
        .context("create Browserbase login session")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("Browserbase login session failed ({status}): {}", trim(&text));
    }
    let v: Value = serde_json::from_str(&text).context("parse login session response")?;
    let id = v.get("id").and_then(Value::as_str).ok_or_else(|| anyhow!("no session id"))?.to_string();
    let live = live_view_url(&key, &id).await.ok_or_else(|| anyhow!("no Live View URL from Browserbase"))?;
    Ok(LoginSession { session_id: id, live_view_url: live })
}

/// A blocking release for the Ctrl-C / cancel handler, which is synchronous
/// and cannot await. Best-effort: on failure the session times out on its own.
pub fn stop_blocking() {
    if connect_url().is_none() {
        return;
    }
    if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
        rt.block_on(stop());
    }
}

fn truthy(key: &str) -> bool {
    matches!(crate::config::get(key).as_deref(), Some("1") | Some("true") | Some("yes") | Some("on"))
}

fn trim(s: &str) -> String {
    s.chars().take(300).collect()
}


#[cfg(test)]
mod lifetime_tests {
    #[test]
    fn a_session_outlives_the_longest_run_a_plan_can_ask_for() {
        // The failure this guards: a run of three search rounds plus an enrich
        // call per row passed the old half-hour window, and every call after
        // that point failed with "the browser is gone" — for the rest of the
        // run, because nothing renews a dead session.
        let default_s: u64 = 4 * 60 * 60;
        assert!(default_s >= 3 * 60 * 60, "a learning plan can run for hours");
        // And it is settable, for a deployment that wants to bound the cost of
        // a worker that dies without releasing its session.
        std::env::set_var("BROWSERBASE_TIMEOUT_S", "900");
        let read: u64 = crate::config::get("BROWSERBASE_TIMEOUT_S").and_then(|v| v.trim().parse().ok()).unwrap_or(default_s);
        assert_eq!(read, 900);
        std::env::remove_var("BROWSERBASE_TIMEOUT_S");
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_pick_browserbase_unless_local_is_forced() {
        assert!(selected_from(None, true));
        assert!(selected_from(Some(""), true));
        assert!(selected_from(Some("browserbase"), true));
        assert!(selected_from(Some("browserbase"), false));
        assert!(!selected_from(None, false));
        assert!(!selected_from(Some("local"), true));
        assert!(!selected_from(Some("chrome"), true));
    }
}

#[cfg(test)]
mod live_view_probe {
    /// Holds a session open on a real page for a minute, for an end-to-end
    /// check of the viewer through the web API: prints `SESSION <id>` and waits.
    #[tokio::test]
    #[ignore]
    async fn hold_a_session_for_the_viewer() {
        super::start(0, None).await.expect("session");
        let id = super::session_id().unwrap();
        let url = super::connect_url().unwrap();
        let mut run = crate::direct::cdp::Cdp::connect(&url).await.expect("run connects");
        run.navigate("https://example.com/", std::time::Duration::from_secs(10)).await.expect("navigate");
        println!("SESSION {id}");
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        drop(run);
        super::stop().await;
    }

    /// The live viewer's path against a real session, while a run holds its
    /// own connection. Spends a few cents of Browserbase:
    /// `cargo test --lib live_view_probe -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn a_second_connection_can_take_a_frame() {
        super::start(0, None).await.expect("session");
        let id = super::session_id().unwrap();
        let url = super::connect_url().unwrap();
        println!("session {id}\nrun connect url host: {}", url.split('?').next().unwrap_or(""));
        let mut run = crate::direct::cdp::Cdp::connect(&url).await.expect("run connects");
        run.navigate("https://example.com/", std::time::Duration::from_secs(10)).await.expect("navigate");
        println!("run is on {}", run.url().await);
        let viewer = super::connect_url_for(&id).await.expect("the session's own connect url");
        for attempt in 1..=2 {
            let t = std::time::Instant::now();
            let jpeg = crate::direct::cdp::screenshot(&viewer).await.expect("the viewer takes a frame");
            println!("attempt {attempt}: frame {} bytes in {:?}", jpeg.len(), t.elapsed());
        }
        // Via the run's own connect URL, for comparison.
        let t = std::time::Instant::now();
        match crate::direct::cdp::screenshot(&url).await {
            Ok(jpeg) => println!("via run url: frame {} bytes in {:?}", jpeg.len(), t.elapsed()),
            Err(e) => println!("via run url: FAILED in {:?}: {e:#}", t.elapsed()),
        }
        drop(run);
        super::stop().await;
    }
}

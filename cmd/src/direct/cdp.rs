//! A Chrome DevTools Protocol client, for driving the browser ourselves.
//!
//! The browser is the one `browser::cdp_endpoint()` names, which on a worker is
//! the Browserbase session carrying the account's logged-in Context and on a
//! dev box is the local Chrome. Nothing about how that browser is started,
//! kept alive or released changes here: this only replaces the Node process
//! that used to sit between the agent and it.
//!
//! Deliberately small. Enough to attach to a page, evaluate a script and send
//! input, because that is all [`super::tools`] needs; everything about *what*
//! to evaluate lives there and in `snapshot.js`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as Ws;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// A single command should not hang a run. A page that takes longer than this
/// to answer an evaluate is a page the agent is better off leaving.
const CALL_TIMEOUT: Duration = Duration::from_secs(45);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct Cdp {
    socket: Socket,
    next_id: AtomicU64,
    /// The page we are attached to. Every command is sent into this session.
    session: String,
}

impl Cdp {
    /// Attach to the browser and to one page in it.
    ///
    /// The endpoint is either a websocket URL (Browserbase hands one over) or
    /// an HTTP DevTools address, which is asked for its websocket URL first.
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let ws_url = websocket_url(endpoint).await?;
        let (socket, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .with_context(|| format!("connect to the browser at {}", crate::guard::safe_for_log(&ws_url, 80)))?;
        let mut cdp = Cdp { socket, next_id: AtomicU64::new(1), session: String::new() };
        cdp.attach_to_a_page().await?;
        Ok(cdp)
    }

    /// Find a page target and attach to it, so later commands have somewhere
    /// to go. `flatten` puts the session on the same socket rather than
    /// opening another.
    async fn attach_to_a_page(&mut self) -> Result<()> {
        let targets = self.call_raw("Target.getTargets", json!({})).await?;
        let page = targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t["type"] == "page" && t["url"].as_str().is_some_and(|u| !u.starts_with("devtools://")))
            .and_then(|t| t["targetId"].as_str().map(str::to_string));
        let target_id = match page {
            Some(id) => id,
            // A browser with no page yet (a fresh session): make one.
            None => self
                .call_raw("Target.createTarget", json!({ "url": "about:blank" }))
                .await?["targetId"]
                .as_str()
                .ok_or_else(|| anyhow!("the browser opened no page"))?
                .to_string(),
        };
        let attached = self.call_raw("Target.attachToTarget", json!({ "targetId": target_id, "flatten": true })).await?;
        self.session = attached["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow!("the browser gave no session for its page"))?
            .to_string();
        // Page events are what `navigate` waits on.
        let _ = self.call("Page.enable", json!({})).await;
        let _ = self.call("Runtime.enable", json!({})).await;
        Ok(())
    }

    /// One command, in the page's session.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let session = self.session.clone();
        self.send(method, params, Some(&session)).await
    }

    /// One command, to the browser itself rather than to a page.
    async fn call_raw(&mut self, method: &str, params: Value) -> Result<Value> {
        self.send(method, params, None).await
    }

    async fn send(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.socket.send(Ws::Text(msg.to_string())).await.with_context(|| format!("send {method}"))?;
        self.wait_for(id, method).await
    }

    /// Read until the answer to `id` arrives. Everything else on the socket is
    /// an event; we subscribe to few and none that need handling here, so they
    /// are read past rather than queued.
    async fn wait_for(&mut self, id: u64, method: &str) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + CALL_TIMEOUT;
        loop {
            let frame = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .map_err(|_| anyhow!("{method} did not answer within {}s", CALL_TIMEOUT.as_secs()))?
                .ok_or_else(|| anyhow!("the browser closed the connection during {method}"))?
                .with_context(|| format!("read the answer to {method}"))?;
            let text = match frame {
                Ws::Text(t) => t,
                Ws::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                Ws::Close(_) => bail!("the browser closed the connection during {method}"),
                // Ping/Pong are handled by the library.
                _ => continue,
            };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(e) = v.get("error") {
                // The page's own failures — a bad selector, a detached node —
                // are the model's to hear about, so the message is kept.
                bail!("{method}: {}", e.get("message").and_then(Value::as_str).unwrap_or("refused by the browser"));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Run JavaScript in the page and return its value.
    ///
    /// `await`ed, so a script may be async, and returned by value rather than
    /// as a handle so nothing is left for the page to hold.
    pub async fn eval(&mut self, script: &str) -> Result<Value> {
        let v = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": script,
                    "returnByValue": true,
                    "awaitPromise": true,
                    // The page's own console is not ours to fill, and a script
                    // that throws should say so rather than be swallowed.
                    "userGesture": true,
                }),
            )
            .await?;
        if let Some(thrown) = v.get("exceptionDetails") {
            let text = thrown
                .pointer("/exception/description")
                .or_else(|| thrown.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("the page refused the script");
            bail!("{}", crate::guard::safe_for_log(text, 200));
        }
        Ok(v.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// Go to `url` and wait for the page to settle.
    ///
    /// "Settled" is the document being ready plus a moment for the scripts
    /// that draw a listing to run. Waiting on the network going idle would be
    /// better and is not available without far more of the protocol; this is
    /// what Playwright's default amounts to in practice.
    pub async fn navigate(&mut self, url: &str, settle: Duration) -> Result<()> {
        let v = self.call("Page.navigate", json!({ "url": url })).await?;
        if let Some(err) = v.get("errorText").and_then(Value::as_str) {
            bail!("{url} did not load: {err}");
        }
        self.wait_until_ready(settle).await
    }

    async fn wait_until_ready(&mut self, settle: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + settle;
        loop {
            let ready = self.eval("document.readyState").await.unwrap_or(Value::Null);
            if ready.as_str() == Some("complete") {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        // A beat for the scripts that fill a listing in after load.
        tokio::time::sleep(Duration::from_millis(600)).await;
        Ok(())
    }

    pub async fn url(&mut self) -> String {
        self.eval("location.href").await.ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
    }

    pub async fn title(&mut self) -> String {
        self.eval("document.title").await.ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
    }

    /// A key, as a real key event so the page's handlers see it.
    pub async fn press(&mut self, key: &str) -> Result<()> {
        let (code, vk, text) = key_of(key)?;
        let base = json!({ "key": key, "code": code, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk });
        let mut down = base.clone();
        down["type"] = json!(if text.is_empty() { "rawKeyDown" } else { "keyDown" });
        if !text.is_empty() {
            down["text"] = json!(text);
        }
        self.call("Input.dispatchKeyEvent", down).await?;
        let mut up = base;
        up["type"] = json!("keyUp");
        self.call("Input.dispatchKeyEvent", up).await?;
        Ok(())
    }
}

/// One JPEG of whatever that browser is showing.
///
/// Opens its own connection and closes it with the returned frame: the run
/// owns the session, and a viewer must never hold anything the run needs. A
/// picture per poll is not free, but it is a person watching occasionally, not
/// a loop.
pub async fn screenshot(endpoint: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    let mut cdp = Cdp::connect(endpoint).await?;
    let shot = cdp
        .call("Page.captureScreenshot", json!({ "format": "jpeg", "quality": 55, "captureBeyondViewport": false }))
        .await?;
    let data = shot["data"].as_str().ok_or_else(|| anyhow!("the browser returned no image"))?;
    base64::engine::general_purpose::STANDARD.decode(data).context("decode the browser's image")
}

/// The keys a scrape needs. Anything else is refused by name rather than sent
/// as something the page will not understand.
fn key_of(key: &str) -> Result<(&'static str, i64, &'static str)> {
    Ok(match key {
        "Enter" => ("Enter", 13, "\r"),
        "Tab" => ("Tab", 9, "\t"),
        "Escape" => ("Escape", 27, ""),
        "Backspace" => ("Backspace", 8, ""),
        "ArrowDown" => ("ArrowDown", 40, ""),
        "ArrowUp" => ("ArrowUp", 38, ""),
        "ArrowLeft" => ("ArrowLeft", 37, ""),
        "ArrowRight" => ("ArrowRight", 39, ""),
        "PageDown" => ("PageDown", 34, ""),
        "PageUp" => ("PageUp", 33, ""),
        "Home" => ("Home", 36, ""),
        "End" => ("End", 35, ""),
        other => bail!("'{other}' is not a key this browser takes — try Enter, Tab, Escape, PageDown, PageUp, Home, End or an arrow key"),
    })
}

/// The websocket to talk to. Browserbase hands over a `wss://` URL; a local
/// Chrome is asked for its own at `/json/version`.
async fn websocket_url(endpoint: &str) -> Result<String> {
    let endpoint = endpoint.trim();
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        return Ok(endpoint.to_string());
    }
    let version = format!("{}/json/version", endpoint.trim_end_matches('/'));
    let body = reqwest::Client::new()
        .get(&version)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .with_context(|| format!("ask {version} for the browser's websocket"))?
        .text()
        .await
        .context("read the browser's version reply")?;
    serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v["webSocketDebuggerUrl"].as_str().map(str::to_string))
        .ok_or_else(|| anyhow!("{endpoint} is not a DevTools endpoint — is Chrome running with --remote-debugging-port?"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_keys_a_scrape_needs_are_sent() {
        assert_eq!(key_of("Enter").unwrap().1, 13);
        assert_eq!(key_of("PageDown").unwrap().0, "PageDown");
        // A printable key carries text; a control key does not.
        assert_eq!(key_of("Enter").unwrap().2, "\r");
        assert_eq!(key_of("Escape").unwrap().2, "");
        let e = key_of("F13").unwrap_err().to_string();
        assert!(e.contains("F13") && e.contains("PageDown"), "the model is told what it may use: {e}");
    }

    /// The live viewer's frame, against a real browser.
    /// `HUNTWELL_CDP_PORT=9222 cargo test live_frame -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_frame() {
        let endpoint = format!("http://127.0.0.1:{}", std::env::var("HUNTWELL_CDP_PORT").unwrap_or("9222".into()));
        let jpeg = screenshot(&endpoint).await.expect("a frame");
        assert!(jpeg.len() > 1000, "a real picture, not an empty one: {} bytes", jpeg.len());
        assert_eq!(&jpeg[..3], &[0xFF, 0xD8, 0xFF], "JPEG magic bytes");
        println!("frame: {} bytes", jpeg.len());
    }

    #[tokio::test]
    async fn a_websocket_endpoint_is_used_as_it_is() {
        assert_eq!(websocket_url("wss://connect.browserbase.test/abc").await.unwrap(), "wss://connect.browserbase.test/abc");
        assert_eq!(websocket_url(" ws://127.0.0.1:9222/x ").await.unwrap(), "ws://127.0.0.1:9222/x");
    }

    #[tokio::test]
    async fn a_devtools_address_is_asked_for_its_websocket() {
        use axum::{routing::get, Json, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let app = Router::new().route(
                "/json/version",
                get(|| async { Json(json!({ "webSocketDebuggerUrl": "ws://127.0.0.1:1/devtools/browser/abc" })) }),
            );
            axum::serve(listener, app).await.unwrap();
        });
        assert_eq!(websocket_url(&format!("http://{addr}")).await.unwrap(), "ws://127.0.0.1:1/devtools/browser/abc");
    }

    #[tokio::test]
    async fn an_endpoint_that_is_not_devtools_says_what_to_check() {
        use axum::{routing::get, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/json/version", get(|| async { "not json" }))).await.unwrap()
        });
        let e = websocket_url(&format!("http://{addr}")).await.unwrap_err().to_string();
        assert!(e.contains("remote-debugging-port"), "{e}");
    }
}

//! Chrome lifecycle for a run.
//!
//! huntwell starts its own Chrome and drives it over CDP. Nothing here waits
//! on a human.
//!
//! The previous transport was `@browsermcp/mcp`, whose Chrome extension only
//! attaches when someone clicks **Connect** in the toolbar popup. That click is
//! a user gesture inside Chrome; no amount of launching `chrome.exe` can
//! produce it. So every run stalled until the user opened a tab and started a
//! session by hand, and any second MCP server booting on :9009 killed the
//! first, silently dropping the attached tab mid-run.
//!
//! Now: [`start_for_run`] launches Chrome headed with `--remote-debugging-port`
//! against a profile huntwell owns, and the agent's `mcp.json` points at
//! `@playwright/mcp --cdp-endpoint …`, which attaches over that port with no
//! extension in the loop. Playwright MCP uses `browser_*` names
//! (`browser_navigate`, `browser_snapshot`, `browser_press_key`, …). It
//! does not ship Browser MCP's `browser_scroll` — paging is Page Down
//! or a click. [`crate::guard`] host checks and [`crate::progress`]
//! naming follow those Playwright names.
//!
//! Because Chrome — not the MCP server — owns the port, a fresh MCP server per
//! `agent -p` call is harmless: it is just another CDP client attaching to the
//! same long-lived browser. That is what the old Unix daemon existed to fake,
//! and why it is gone.
//!
//! The controlled tab is outlined by [`BORDER_JS`], handed to Playwright MCP as
//! an `--init-script`. It re-runs on every navigation, and only on pages the
//! agent actually drives — a tab the user opens themselves stays unmarked.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

/// Chrome's DevTools port. Chrome owns it for the whole run; MCP servers are
/// clients, so several may come and go without disturbing the browser.
pub const DEFAULT_CDP_PORT: u16 = 9222;

/// Printed on failure so the parent `agent` stderr (and our run log) can tell
/// "Chrome isn't there" from a generic MCP failure.
pub const UNAVAILABLE_TOKEN: &str = "HUNTWELL_BROWSER_UNAVAILABLE";

/// How long [`start_for_run`] waits for a freshly launched Chrome to answer on
/// the DevTools port.
const DEFAULT_WAIT_SECS: u64 = 30;
const POLL: Duration = Duration::from_millis(200);

/// What to tell the user when Chrome could not be started or reached.
pub const HOWTO: &str = "\
huntwell could not start Chrome.

A run drives a Chrome that huntwell launches itself, using its own profile,
so there is nothing to click and no extension to connect.

  1. Install Google Chrome (or Chromium).
  2. If it is not on PATH or in a standard location, point huntwell at it:
       HUNTWELL_CHROME=/path/to/chrome        (chrome.exe on Windows)
  3. Re-run `huntwell doctor` to confirm.

The profile huntwell drives is separate from your everyday Chrome, so both
can be open at once. Log in to sites (LinkedIn and friends) once inside the
huntwell window and those sessions persist across runs.";

/// Process-wide: a browser tool reported the browser was gone.
static DISCONNECTED: AtomicBool = AtomicBool::new(false);

/// The Chrome we launched, if we launched one. A Chrome that was already
/// listening is reused and never killed — it may be another run, or a window
/// the user opened themselves.
fn launched() -> &'static Mutex<Option<Child>> {
    static LAUNCHED: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
    LAUNCHED.get_or_init(|| Mutex::new(None))
}

pub fn mark_disconnected() {
    DISCONNECTED.store(true, Ordering::SeqCst);
}

pub fn disconnected() -> bool {
    DISCONNECTED.load(Ordering::SeqCst)
}

#[derive(Debug)]
pub struct Unavailable(pub String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.0, HOWTO)
    }
}

impl std::error::Error for Unavailable {}

// -----------------------------------------------------------------------
// Configuration
// -----------------------------------------------------------------------

pub fn cdp_port() -> u16 {
    std::env::var("HUNTWELL_CDP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|p: &u16| *p > 0)
        // A second instance must never share this port: whoever binds it first
        // owns the browser, and the other adopts it — same tabs, same cookies.
        .unwrap_or(DEFAULT_CDP_PORT)
}

pub fn cdp_endpoint() -> String {
    // A live Browserbase session takes the place of the local DevTools port,
    // so the browser MCP server attaches to the cloud browser instead.
    if let Some(url) = crate::browserbase::connect_url() {
        return url;
    }
    format!("http://127.0.0.1:{}", cdp_port())
}

pub fn wait_secs() -> u64 {
    std::env::var("HUNTWELL_BROWSER_WAIT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n: &u64| *n > 0)
        .unwrap_or(DEFAULT_WAIT_SECS)
}

fn skip_wait() -> bool {
    matches!(
        std::env::var("HUNTWELL_SKIP_BROWSER_WAIT").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

fn headless() -> bool {
    matches!(
        std::env::var("HUNTWELL_CHROME_HEADLESS").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

/// Where huntwell keeps the Chrome profile and the injected border script.
/// Overridable with `HUNTWELL_CHROME_DIR`; the profile itself lives in
/// `<dir>/profile`.
///
/// A persistent directory, not a temp one: the logged-in sessions the scrape
/// prompts rely on (LinkedIn, registries) have to survive between runs. It is
/// deliberately not the user's default Chrome profile — Chrome refuses to open
/// one profile twice, which would mean quitting their everyday browser before
/// every run.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("HUNTWELL_CHROME_DIR") {
        return PathBuf::from(d);
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))
        })
    };
    let _ = base;
    crate::config::data_dir().join("chrome")
}

fn profile_dir() -> PathBuf {
    data_dir().join("profile")
}

fn border_script_path() -> PathBuf {
    data_dir().join("ai-control-border.js")
}

/// Where the browser server writes screenshots and page dumps.
fn output_dir() -> PathBuf {
    data_dir().join("output")
}

/// Locates a Chrome (or Chromium) to drive.
///
/// `HUNTWELL_CHROME` wins. Otherwise the usual install locations per
/// platform, then PATH.
pub fn chrome_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HUNTWELL_CHROME") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
        // A bare name like "chromium" is still worth resolving on PATH.
        if let Some(found) = which(&p.to_string_lossy()) {
            return Some(found);
        }
        return None;
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for key in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(key) {
                candidates
                    .push(PathBuf::from(base).join(r"Google\Chrome\Application\chrome.exe"));
            }
        }
    } else if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ));
        candidates.push(PathBuf::from(
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ));
    } else {
        for p in [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ] {
            candidates.push(PathBuf::from(p));
        }
    }
    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(found);
    }

    for name in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "chrome",
        "chrome.exe",
    ] {
        if let Some(found) = which(name) {
            return Some(found);
        }
    }
    None
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|s| s.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        let direct = dir.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        if cfg!(windows) {
            for ext in &exts {
                let with_ext = dir.join(format!("{name}{ext}"));
                if with_ext.is_file() {
                    return Some(with_ext);
                }
            }
        }
    }
    None
}

// -----------------------------------------------------------------------
// The "AI is driving this tab" border
// -----------------------------------------------------------------------

/// Injected by Playwright MCP into every page the agent drives, before any of
/// the page's own scripts, and again after each navigation.
///
/// Scoped in a shadow root so page CSS cannot restyle it and page scripts
/// querying the DOM don't trip over its internals. `pointer-events:none`
/// throughout, so it never eats a click the agent is trying to make.
// `r##"…"##`: the script contains `"#6d4aff"`, and `"#` would close an `r#"`.
pub const BORDER_JS: &str = r##"// Written by huntwell. Marks tabs the agent is driving.
(() => {
  const ID = "__huntwell_ai_control__";
  const LABEL = "Huntwell AI is controlling this tab";
  const ACCENT = "#6d4aff";

  function install() {
    const root = document.documentElement;
    if (!root || document.getElementById(ID)) return;
    const host = document.createElement("div");
    host.id = ID;
    host.setAttribute("aria-hidden", "true");
    host.style.cssText =
      "position:fixed;top:0;left:0;width:0;height:0;margin:0;padding:0;border:0;" +
      "z-index:2147483647;pointer-events:none;";
    const markup =
      '<style>' +
      ':host{all:initial}' +
      '.frame{position:fixed;inset:0;box-sizing:border-box;pointer-events:none;' +
      'border:5px solid ' + ACCENT + ';' +
      'box-shadow:inset 0 0 0 1px rgba(255,255,255,.45),0 0 14px ' + ACCENT + '55;' +
      'animation:huntwell-pulse 2.4s ease-in-out infinite}' +
      '.tag{position:fixed;top:0;left:50%;transform:translateX(-50%);' +
      'background:' + ACCENT + ';color:#fff;pointer-events:none;' +
      'font:600 12px/18px system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;' +
      'letter-spacing:.02em;padding:3px 12px;border-radius:0 0 8px 8px;' +
      'box-shadow:0 2px 8px rgba(0,0,0,.28);white-space:nowrap}' +
      // Never fades below .7: the point is to be unmistakable at a glance,
      // and a deeper trough reads as a page decoration rather than a warning.
      '@keyframes huntwell-pulse{0%,100%{opacity:1}50%{opacity:.7}}' +
      '</style>' +
      '<div class="frame"></div><div class="tag">' + LABEL + '</div>';
    if (host.attachShadow) {
      host.attachShadow({ mode: "open" }).innerHTML = markup;
    } else {
      host.innerHTML = markup;
    }
    root.appendChild(host);
  }

  install();
  document.addEventListener("DOMContentLoaded", install);
  // Single-page apps and pages that rewrite <html> can drop it; put it back.
  setInterval(install, 1000);
})();
"##;

/// Writes the border script and makes sure the profile directory exists.
fn ensure_support_files() -> Result<()> {
    std::fs::create_dir_all(profile_dir())
        .with_context(|| format!("create Chrome profile dir {}", profile_dir().display()))?;
    std::fs::create_dir_all(output_dir())
        .with_context(|| format!("create browser output dir {}", output_dir().display()))?;
    let script = border_script_path();
    // Rewritten on every start so upgrading the binary upgrades the overlay.
    if std::fs::read_to_string(&script).ok().as_deref() != Some(BORDER_JS) {
        std::fs::write(&script, BORDER_JS)
            .with_context(|| format!("write border script {}", script.display()))?;
    }
    Ok(())
}

// -----------------------------------------------------------------------
// Talking to Chrome
// -----------------------------------------------------------------------

/// A plain HTTP GET against Chrome's DevTools HTTP endpoint.
///
/// Hand-rolled rather than pulled from a crate because it is two requests
/// against localhost. Unlike the old Browser MCP probe, connecting here is
/// safe: it is Chrome's own read-only endpoint, not a WebSocket whose second
/// client evicts the first.
fn cdp_get(path: &str) -> Option<String> {
    let addr: SocketAddr = ([127, 0, 0, 1], cdp_port()).into();
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(700)).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(1500))).ok()?;
    stream.set_write_timeout(Some(Duration::from_millis(700))).ok()?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).ok()?;
    stream.flush().ok()?;
    let mut buf = Vec::new();
    // Cap the read: /json/list on a busy browser is still tiny, and an
    // unbounded read here would be a way to stall the run.
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 512 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").or_else(|| text.split_once("\n\n"))?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return None;
    }
    Some(body.to_string())
}

/// True when Chrome is up and answering on the DevTools port.
pub fn chrome_up() -> bool {
    // The remote session is managed by Browserbase; treat it as always up so
    // nothing here tries to launch or adopt a local Chrome.
    if crate::browserbase::connect_url().is_some() {
        return true;
    }
    cdp_get("/json/version").is_some()
}

/// Ids of every page target (tab), in Chrome's own order — most recently
/// created first.
fn page_target_ids() -> Vec<String> {
    // Tab bookkeeping is over the local DevTools HTTP port; a remote session
    // has no such port, and Browserbase owns its lifecycle, so this is a no-op.
    if crate::browserbase::connect_url().is_some() {
        return Vec::new();
    }
    let Some(body) = cdp_get("/json/list") else {
        return Vec::new();
    };
    // The body may be chunked; find the JSON array inside it.
    let start = body.find('[').unwrap_or(0);
    let end = body.rfind(']').map(|i| i + 1).unwrap_or(body.len());
    if start >= end {
        return Vec::new();
    }
    let Ok(Value::Array(targets)) = serde_json::from_str::<Value>(&body[start..end]) else {
        return Vec::new();
    };
    targets
        .iter()
        .filter(|t| t.get("type").and_then(Value::as_str) == Some("page"))
        .filter_map(|t| t.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// Number of page targets (tabs) Chrome currently has open.
fn page_targets() -> usize {
    page_target_ids().len()
}

/// Closes one tab. Chrome's DevTools HTTP endpoint does this without a
/// WebSocket, so tab reaping costs nothing and needs no MCP round trip.
fn close_target(id: &str) -> bool {
    cdp_get(&format!("/json/close/{id}")).is_some()
}

/// How many tabs one agent call may hold open before the watchdog starts
/// closing its oldest. `HUNTWELL_MAX_TABS=0` turns the cap off.
pub fn max_tabs() -> usize {
    std::env::var("HUNTWELL_MAX_TABS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8)
}

/// The tabs one agent call is responsible for.
///
/// A scrape that visits forty companies can leave forty tabs behind:
/// `browser_tabs` opens them and nothing closes them, so memory climbs for the
/// whole run. Asking the model to tidy up is unreliable and costs another agent
/// call, so huntwell closes them itself.
///
/// Scoped by diff rather than "close everything but one" because concurrent
/// runs share a browser — Chrome allows only one instance per profile, so a
/// second run adopts the first one's Chrome. Closing every tab would kill the
/// sibling run's page mid-navigation. Only targets that appeared while this
/// call was running are ever touched.
pub struct TabScope {
    before: std::collections::HashSet<String>,
}

impl TabScope {
    /// Records the tabs that already existed, so they are never closed.
    pub fn open() -> Self {
        Self { before: page_target_ids().into_iter().collect() }
    }

    /// Tabs that appeared since [`TabScope::open`], newest first.
    fn mine(&self) -> Vec<String> {
        page_target_ids().into_iter().filter(|id| !self.before.contains(id)).collect()
    }

    /// Closes every tab this call opened. Chrome exits when its last tab
    /// closes, taking the run's browser with it, so one is always left behind.
    pub fn reap(&self) -> usize {
        self.close(self.mine())
    }

    /// Mid-call backstop: closes this call's oldest tabs once it is holding
    /// more than `max`. A single enrich step that opens tabs in a loop is
    /// bounded rather than left to exhaust memory.
    ///
    /// The newest `max` are kept because that is where the agent is working. If
    /// the guess is wrong the model simply navigates again; an unbounded
    /// browser is the worse failure.
    pub fn trim(&self, max: usize) -> usize {
        if max == 0 {
            return 0;
        }
        let mine = self.mine();
        if mine.len() <= max {
            return 0;
        }
        self.close(mine.into_iter().skip(max).collect())
    }

    fn close(&self, ids: Vec<String>) -> usize {
        if ids.is_empty() {
            return 0;
        }
        let mut remaining = page_targets();
        let mut closed = 0;
        for id in ids {
            if remaining <= 1 {
                break;
            }
            if close_target(&id) {
                closed += 1;
                remaining -= 1;
            }
        }
        closed
    }
}

/// Snapshot for `huntwell doctor` and the UI.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub port: u16,
    /// Chrome is running with the DevTools port open.
    pub listening: bool,
    /// Chrome is up and has at least one tab the agent can drive.
    pub connected: bool,
    /// Path to the Chrome binary huntwell would launch, if one was found.
    pub chrome: Option<String>,
    pub profile: String,
}

pub fn status() -> Status {
    let listening = chrome_up();
    Status {
        port: cdp_port(),
        listening,
        connected: listening && page_targets() > 0,
        chrome: chrome_binary().map(|p| p.to_string_lossy().into_owned()),
        profile: profile_dir().to_string_lossy().into_owned(),
    }
}

// -----------------------------------------------------------------------
// Lifecycle
// -----------------------------------------------------------------------

/// Starts (or adopts) the Chrome this run will drive.
///
/// Called once per run, before any agent. Returns once Chrome answers on the
/// DevTools port, so by the time the first `browser_navigate` is issued there
/// is a browser to navigate.
pub fn start_for_run() -> Result<()> {
    ensure_support_files()?;

    // Driving a remote Browserbase browser: nothing local to launch or wait
    // for. The MCP server (which runs locally) attaches over the connectUrl.
    if crate::browserbase::connect_url().is_some() {
        return Ok(());
    }

    if chrome_up() {
        // Someone is already listening: another huntwell run, or a Chrome the
        // user started with the same flags. Attach, and leave it alone at the
        // end of the run.
        return Ok(());
    }

    let exe = chrome_binary().ok_or_else(|| {
        anyhow::anyhow!("no Chrome binary found. Set HUNTWELL_CHROME to its full path")
    })?;

    let wanted = wanted_display();
    let first = match wanted {
        Some(mode) => mode,
        None if display_is_live() => DisplayMode::Headed,
        None => {
            let fallback = offscreen_mode();
            eprintln!(
                "[browser] no usable display — starting Chrome {}. \
                 This is what a scheduled run looks like with nobody logged in.",
                fallback.describe()
            );
            fallback
        }
    };

    match launch(&exe, first) {
        Ok(()) => Ok(()),
        Err(e) if first == DisplayMode::Headed => {
            // The display looked live but Chrome could not use it. A remote
            // desktop that has been disconnected is the usual reason: the
            // socket is still there, the compositor behind it is not. Rather
            // than fail a scheduled run, try again without a screen.
            let fallback = offscreen_mode();
            eprintln!("[browser] Chrome could not start on this display ({e:#})");
            eprintln!("[browser] retrying {}", fallback.describe());
            launch(&exe, fallback).map_err(|e2| {
                anyhow::anyhow!("{e2:#}\n  headed attempt failed first: {e:#}")
            })
        }
        Err(e) => Err(e),
    }
}

/// How Chrome is put on screen — or kept off it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    /// On the logged-in desktop, where the AI-control border is visible.
    Headed,
    /// Headed, but drawn into a virtual X server. Keeps Chrome a normal
    /// windowed browser — which matters to sites that fingerprint headless —
    /// without needing anyone logged in.
    Xvfb,
    /// No display at all. Always available, but the most detectable.
    Headless,
}

impl DisplayMode {
    fn describe(self) -> &'static str {
        match self {
            DisplayMode::Headed => "on the desktop",
            DisplayMode::Xvfb => "on a virtual display (xvfb-run)",
            DisplayMode::Headless => "headless",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DisplayMode::Headed => "desktop",
            DisplayMode::Xvfb => "virtual display",
            DisplayMode::Headless => "headless",
        }
    }
}

/// An explicit choice from the environment, if there is one.
///
/// `HUNTWELL_CHROME_HEADLESS=1` is still honoured; it predates there being
/// more than two options.
fn wanted_display() -> Option<DisplayMode> {
    if headless() {
        return Some(DisplayMode::Headless);
    }
    match std::env::var("HUNTWELL_CHROME_DISPLAY").ok()?.trim().to_ascii_lowercase().as_str() {
        "headed" | "desktop" => Some(DisplayMode::Headed),
        "xvfb" | "virtual" => Some(DisplayMode::Xvfb),
        "headless" | "none" => Some(DisplayMode::Headless),
        _ => None,
    }
}

/// What to use when there is no desktop to draw on.
///
/// Xvfb is preferred: a windowed Chrome on a virtual screen looks far more like
/// an ordinary browser than a headless one does, and these scrapes depend on
/// logged-in sessions at sites that care.
fn offscreen_mode() -> DisplayMode {
    if which("xvfb-run").is_some() {
        DisplayMode::Xvfb
    } else {
        DisplayMode::Headless
    }
}

/// Whether a display exists *and* something is still behind it.
///
/// The socket outliving its compositor is the case that matters here: a
/// disconnected remote desktop can leave `DISPLAY` set and `/tmp/.X11-unix/X0`
/// in place while nothing will accept a connection. Checking the variable alone
/// would send a scheduled run into a 30-second timeout every night.
fn display_is_live() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        if let (Ok(dir), Ok(wl)) =
            (std::env::var("XDG_RUNTIME_DIR"), std::env::var("WAYLAND_DISPLAY"))
        {
            let path = std::path::Path::new(&dir).join(&wl);
            if UnixStream::connect(&path).is_ok() {
                return true;
            }
        }
        if let Ok(display) = std::env::var("DISPLAY") {
            // ":0", ":0.0" and "hostname:0" all name screen 0 here.
            let n = display
                .rsplit(':')
                .next()
                .and_then(|s| s.split('.').next())
                .unwrap_or("0");
            if UnixStream::connect(format!("/tmp/.X11-unix/X{n}")).is_ok() {
                return true;
            }
        }
        false
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn launch(exe: &std::path::Path, mode: DisplayMode) -> Result<()> {
    let profile = profile_dir();
    let mut chrome_args: Vec<String> = vec![
        format!("--remote-debugging-port={}", cdp_port()),
        // Chrome 111+ rejects the DevTools WebSocket upgrade when the client
        // sends an Origin header unless it is allowed explicitly.
        "--remote-allow-origins=*".into(),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-session-crashed-bubble".into(),
        "--hide-crash-restore-bubble".into(),
        "--disable-features=Translate,MediaRouter".into(),
    ];
    match mode {
        DisplayMode::Headed => chrome_args.push("--start-maximized".into()),
        // A fixed window size: with no real screen, Chrome would otherwise pick
        // a tiny default and pages would render in a mobile layout.
        DisplayMode::Xvfb => chrome_args.push("--window-size=1920,1080".into()),
        DisplayMode::Headless => {
            chrome_args.push("--headless=new".into());
            chrome_args.push("--window-size=1920,1080".into());
            // No GPU or sandbox namespaces are available in most unattended
            // contexts, and Chrome exits rather than degrading.
            chrome_args.push("--disable-gpu".into());
        }
    }
    chrome_args.push("about:blank".into());

    let mut cmd = match mode {
        DisplayMode::Xvfb => {
            let mut c = Command::new("xvfb-run");
            c.arg("-a")
                .arg("--server-args=-screen 0 1920x1080x24")
                .arg(exe)
                .args(&chrome_args);
            c
        }
        _ => {
            let mut c = Command::new(exe);
            c.args(&chrome_args);
            c
        }
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group: lets stop_for_run take the renderers down with the
        // browser process, and keeps Ctrl+C in the terminal from killing Chrome
        // out from under a run that is still cleaning up.
        cmd.process_group(0);
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("start Chrome {} ({})", mode.describe(), exe.display()))?;
    if let Ok(mut slot) = launched().lock() {
        *slot = Some(child);
    }
    if let Ok(mut slot) = mode_slot().lock() {
        *slot = Some(mode);
    }

    if skip_wait() {
        return Ok(());
    }
    let budget = Duration::from_secs(wait_secs());
    let start = Instant::now();
    while start.elapsed() < budget {
        if chrome_up() {
            return Ok(());
        }
        if let Ok(mut slot) = launched().lock() {
            if let Some(child) = slot.as_mut() {
                if let Ok(Some(st)) = child.try_wait() {
                    anyhow::bail!("Chrome exited immediately ({st}) starting {}", mode.describe());
                }
            }
        }
        thread::sleep(POLL);
    }
    // Leave nothing behind for the fallback attempt to trip over.
    stop_for_run();
    anyhow::bail!(
        "Chrome did not open the DevTools port {} within {}s starting {}",
        cdp_port(),
        budget.as_secs(),
        mode.describe()
    )
}

/// How this run's Chrome was started, for the run header and `doctor`.
fn mode_slot() -> &'static Mutex<Option<DisplayMode>> {
    static MODE: OnceLock<Mutex<Option<DisplayMode>>> = OnceLock::new();
    MODE.get_or_init(|| Mutex::new(None))
}

pub fn display_mode() -> Option<DisplayMode> {
    mode_slot().lock().ok().and_then(|m| *m)
}

/// Whether this run started the Chrome it is driving, as opposed to adopting
/// one that was already listening.
///
/// Cleanup turns on this: tidying tabs costs an extra agent call, and is only
/// worth it when the browser outlives the run.
#[allow(dead_code)]
pub fn owns_chrome() -> bool {
    launched().lock().is_ok_and(|slot| slot.is_some())
}

/// Stops the Chrome this run started. A Chrome that was already running when
/// the run began is left alone.
pub fn stop_for_run() {
    let Ok(mut slot) = launched().lock() else {
        return;
    };
    let Some(mut child) = slot.take() else {
        return;
    };
    #[cfg(unix)]
    {
        // Negative pid = the process group, so renderers and GPU helpers go too.
        let pid = child.id();
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        // Give Chrome a moment to flush the profile (cookies, sessions) before
        // escalating — a SIGKILL here loses the logins the next run needs.
        for _ in 0..25 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

/// The `mcp.json` entry for the browser server, written into the agent's
/// sandboxed workspace by [`crate::sandbox`].
///
/// Playwright MCP attaches to the Chrome we started rather than launching one
/// of its own, so a new server per `agent -p` call costs a CDP connection and
/// nothing else.
pub fn mcp_server_config() -> Value {
    let (cmd, mut args) = playwright_command();
    args.push("--cdp-endpoint".into());
    args.push(cdp_endpoint());

    // Without this the server drops a `.playwright-mcp/` directory of
    // screenshots and page dumps into whatever its working directory happens to
    // be — which is the user's project when a run is started from there.
    args.push("--output-dir".into());
    args.push(output_dir().to_string_lossy().into_owned());

    // Token discipline. What a run actually spends its tokens on is the page
    // content the server hands back, not the prompts — so the two defaults that
    // send back content nothing needs are turned off: image responses (a scrape
    // returns JSON, it never needs to look at a screenshot) and console
    // messages below `error` (ad and analytics noise on every page).
    //
    // Both flags are recent. An older pinned server exits with a Node usage
    // error on an unknown flag rather than serving a single tool, which looks
    // like "every scrape returns nothing", so leave a way to drop them without
    // a rebuild: HUNTWELL_BROWSER_MCP_LEAN=0.
    if lean_pages() {
        args.push("--image-responses".into());
        args.push("omit".into());
        args.push("--console-level".into());
        args.push("error".into());
    }
    // Playwright's own help says mobile pages are lighter and save tokens, but
    // emulation belongs to a context the server creates and we hand it a Chrome
    // that already exists — so this stays opt-in until a run has measured it.
    if mobile_pages() {
        args.push("--mobile".into());
    }

    // Playwright MCP refuses to start at all when `--init-script` names a file
    // that isn't there — it exits with a Node stack trace before serving a
    // single tool. Passing the path blind would let a missing overlay take the
    // whole browser down with it, and the symptom (every scrape returns
    // nothing) looks nothing like the cause. So write it here as well as in
    // start_for_run, and go without the border rather than without a browser.
    match ensure_support_files() {
        Ok(()) if border_script_path().is_file() => {
            args.push("--init-script".into());
            args.push(border_script_path().to_string_lossy().into_owned());
        }
        _ => eprintln!(
            "[browser] could not write the AI-control border script; \
             continuing without the outline"
        ),
    }
    json!({ "command": cmd, "args": args })
}

/// Whether to drop page content no stage reads. On unless
/// `HUNTWELL_BROWSER_MCP_LEAN` is explicitly falsy.
fn lean_pages() -> bool {
    !matches!(
        std::env::var("HUNTWELL_BROWSER_MCP_LEAN").ok().as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off")
    )
}

/// Whether to ask for mobile pages (`HUNTWELL_BROWSER_MOBILE=1`).
fn mobile_pages() -> bool {
    matches!(
        std::env::var("HUNTWELL_BROWSER_MOBILE").ok().as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

fn playwright_command() -> (String, Vec<String>) {
    let cmd = std::env::var("HUNTWELL_BROWSER_MCP_COMMAND")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "npx".into());
    let args = std::env::var("HUNTWELL_BROWSER_MCP_ARGS_JSON")
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| {
            ["-y", "@playwright/mcp@latest"].iter().map(|s| (*s).to_string()).collect()
        });
    (cmd, args)
}

// -----------------------------------------------------------------------
// Failure recognition
// -----------------------------------------------------------------------

/// Text from a browser tool that means "there is no browser to drive".
///
/// Kept broad on purpose: the run should stop with an actionable error rather
/// than quietly scraping nothing. The Browser MCP phrases stay listed so a
/// stale config that still points at the extension is diagnosed, not guessed at.
pub fn looks_like_disconnect(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    const NEEDLES: [&str; 11] = [
        // Playwright / CDP
        "econnrefused",
        "failed to connect to the browser",
        "browser has been closed",
        "target page, context or browser has been closed",
        "browser is not connected",
        "connect over cdp",
        "websocket error",
        // Browser MCP, in case a stale mcp.json still points at it
        "no connection to browser extension",
        "extension is not connected",
        "clicking the 'connect' button",
        UNAVAILABLE_TOKEN,
    ];
    // `UNAVAILABLE_TOKEN` is upper-case, so the needles are folded too.
    NEEDLES.iter().any(|n| t.contains(&n.to_ascii_lowercase()))
}

/// The MCP client gave up waiting for a browser tool. Distinct from "no
/// browser": this is what a hung page looks like.
pub fn looks_like_mcp_timeout(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("mcp error -32001") || t.contains("request timed out")
}

pub fn result_looks_disconnected(result: &Value) -> bool {
    if result.is_null() {
        return false;
    }
    looks_like_disconnect(&result.to_string())
}

/// Watches Chrome while an agent child is alive.
///
/// Two jobs: a browser that dies mid-run shows up in the run log instead of as
/// a wall of empty scrapes, and this call's tab count is kept under
/// [`max_tabs`] so a step that opens tabs in a loop cannot exhaust memory
/// before it finishes.
pub fn spawn_browser_watchdog(
    alive: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    announced_wait: Arc<AtomicBool>,
    scope: Arc<TabScope>,
    emit: impl Fn(&str) + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut warned = false;
        let cap = max_tabs();
        while alive.load(Ordering::Relaxed) {
            if chrome_up() {
                connected.store(true, Ordering::Relaxed);
                if warned {
                    emit("Chrome is answering again");
                    warned = false;
                }
                let closed = scope.trim(cap);
                if closed > 0 {
                    emit(&format!("closed {closed} tab(s) — this step was holding more than {cap}"));
                }
            } else {
                connected.store(false, Ordering::Relaxed);
                if !announced_wait.swap(true, Ordering::Relaxed) {
                    emit("Chrome stopped answering on the DevTools port — the run cannot scrape until it is back");
                    warned = true;
                }
            }
            for _ in 0..10 {
                if !alive.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    })
}

pub fn print_doctor() {
    println!("huntwell doctor");
    println!();

    let agent = std::env::var_os("HUNTWELL_AGENT")
        .or_else(|| std::env::var_os("CURSOR_AGENT_BIN"))
        .unwrap_or_else(|| "agent".into());
    match Command::new(&agent).arg("--help").stdout(Stdio::null()).stderr(Stdio::null()).status()
    {
        Ok(s) if s.success() => println!("  agent      ok ({})", agent.to_string_lossy()),
        Ok(_) | Err(_) => println!(
            "  agent      MISSING ({}) — install Cursor CLI and run `agent login`",
            agent.to_string_lossy()
        ),
    }

    match Command::new("npx").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status()
    {
        Ok(s) if s.success() => println!("  npx        ok"),
        _ => println!("  npx        MISSING — install Node.js so `@playwright/mcp` can start"),
    }

    let st = status();
    match &st.chrome {
        Some(p) => println!("  chrome     ok ({p})"),
        None => println!("  chrome     MISSING — install Chrome or set HUNTWELL_CHROME"),
    }
    println!("  profile    {}", st.profile);
    println!(
        "  display    {}",
        if display_is_live() {
            "desktop available — Chrome runs windowed"
        } else if which("xvfb-run").is_some() {
            "no desktop — Chrome runs on a virtual display (xvfb-run)"
        } else {
            "no desktop and no xvfb-run — Chrome runs headless \
             (install xvfb for a windowed browser on scheduled runs)"
        }
    );
    println!("  devtools   127.0.0.1:{}", st.port);
    println!(
        "    status   {}",
        if st.connected {
            "running, tab available"
        } else if st.listening {
            "running, no tab open yet"
        } else {
            "not running (a run starts it)"
        }
    );
    println!();
    if st.chrome.is_some() {
        println!("A run starts Chrome itself and drives it over the DevTools port.");
        println!("There is no extension to install and no Connect button to click.");
        println!("Pages the agent drives are outlined and labelled while it works.");
        println!();
        println!("Log in to sites once inside the huntwell Chrome window; the profile");
        println!("above persists, so those sessions are there on the next run.");
    } else {
        println!("{HOWTO}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_phrases_from_real_runs() {
        assert!(looks_like_disconnect(
            "browserType.connectOverCDP: connect ECONNREFUSED 127.0.0.1:9222"
        ));
        assert!(looks_like_disconnect(
            "Target page, context or browser has been closed"
        ));
        // A stale Browser MCP config should still be diagnosed, not guessed at.
        assert!(looks_like_disconnect(
            "No connection to browser extension. In order to proceed, you must first connect a tab"
        ));
        assert!(looks_like_disconnect(UNAVAILABLE_TOKEN));
        assert!(!looks_like_disconnect("navigated to https://example.com"));
        assert!(looks_like_mcp_timeout(
            r#"{"error":"MCP error -32001: Request timed out"}"#
        ));
        assert!(!looks_like_mcp_timeout("navigated to https://example.com"));
    }

    #[test]
    fn mcp_config_attaches_to_our_chrome() {
        let cfg = mcp_server_config();
        let args: Vec<String> = cfg["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let i = args.iter().position(|a| a == "--cdp-endpoint").expect("cdp endpoint passed");
        assert_eq!(args[i + 1], cdp_endpoint());
        // The border is what tells the user the agent is driving; if this flag
        // goes missing the tab looks like any other.
        let j = args.iter().position(|a| a == "--init-script").expect("border script passed");
        assert!(args[j + 1].ends_with("ai-control-border.js"));
        // Playwright MCP must attach, never launch its own browser, or the
        // profile with the logged-in sessions is bypassed.
        assert!(!args.iter().any(|a| a == "--user-data-dir"));
        // The page content a scrape never reads is the biggest line in a run's
        // token bill, so these two are not optional decoration.
        let k = args.iter().position(|a| a == "--image-responses").expect("image responses omitted");
        assert_eq!(args[k + 1], "omit");
        let l = args.iter().position(|a| a == "--console-level").expect("console noise dropped");
        assert_eq!(args[l + 1], "error");
        // Mobile emulation over an attached Chrome is unproven, so it must not
        // arrive by default.
        assert!(!args.iter().any(|a| a == "--mobile"));
    }

    #[test]
    fn border_script_is_self_installing() {
        // Must survive navigation and pages that rewrite <html>, and never
        // swallow a click the agent is trying to make.
        assert!(BORDER_JS.contains("setInterval(install"));
        assert!(BORDER_JS.contains("pointer-events:none"));
        assert!(BORDER_JS.contains("attachShadow"));
    }

    /// The whole point of the change, end to end: huntwell starts Chrome by
    /// itself, an MCP server attaches over CDP with nobody clicking anything,
    /// and the page it drives is visibly marked.
    ///
    /// Ignored by default — it opens a real Chrome window, needs a display, and
    /// needs network for `npx`. It also sets process-wide env, so run it alone:
    ///
    ///   cargo test -- --ignored --test-threads=1 chrome_starts_by_itself
    #[test]
    #[ignore = "opens a real Chrome window; needs a display and npx"]
    fn chrome_starts_by_itself_and_marks_the_page() {
        use std::io::BufRead;

        // A port and profile of its own, so a run in progress is undisturbed.
        std::env::set_var("HUNTWELL_CDP_PORT", "9333");
        let profile = std::env::temp_dir().join(format!("huntwell-cdp-test-{}", std::process::id()));
        std::env::set_var("HUNTWELL_CHROME_DIR", &profile);

        start_for_run().expect("huntwell must be able to start Chrome unaided");
        assert!(chrome_up(), "Chrome did not answer on the DevTools port");

        let cfg = mcp_server_config();
        let args: Vec<String> = cfg["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let mut mcp = Command::new(cfg["command"].as_str().unwrap())
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the MCP server the agent would get");

        let mut stdin = mcp.stdin.take().unwrap();
        let mut stdout = std::io::BufReader::new(mcp.stdout.take().unwrap());
        let mut send = |v: Value| {
            let _ = writeln!(stdin, "{v}");
            let _ = stdin.flush();
        };
        let await_id = |want: i64, stdout: &mut std::io::BufReader<_>| -> Value {
            let deadline = Instant::now() + Duration::from_secs(120);
            let mut line = String::new();
            while Instant::now() < deadline {
                line.clear();
                if stdout.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                    if v.get("id").and_then(Value::as_i64) == Some(want) {
                        return v;
                    }
                }
            }
            panic!("no response to request {want}");
        };

        send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2024-11-05","capabilities":{},
            "clientInfo":{"name":"huntwell-test","version":"1"}}}));
        await_id(1, &mut stdout);
        send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));

        // No Connect click happens anywhere in here. If the transport still
        // needed one, this navigate is what would hang.
        send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"browser_navigate",
            "arguments":{"url":"data:text/html,<title>t</title><h1>huntwell</h1>"}}}));
        let nav = await_id(2, &mut stdout);
        assert_ne!(nav["result"]["isError"], json!(true), "navigate failed: {nav}");

        // The border is how the user knows the AI is driving; assert it landed
        // in the page rather than trusting the flag was passed.
        send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
            "name":"browser_evaluate",
            "arguments":{"function":"() => !!document.getElementById('__huntwell_ai_control__')"}}}));
        let probe = await_id(3, &mut stdout);
        assert!(
            probe.to_string().contains("true"),
            "the AI-control border never reached the page: {probe}"
        );

        let _ = mcp.kill();
        let _ = mcp.wait();
        stop_for_run();
        let _ = std::fs::remove_dir_all(&profile);
    }

    /// Tab accumulation, which is what made long runs eat memory: the agent
    /// opens a tab per company via `browser_tabs` and nothing closes them.
    ///
    /// Same conditions as the test above — run it alone.
    #[test]
    #[ignore = "opens a real Chrome window; needs a display and npx"]
    fn tabs_opened_by_a_call_are_closed_when_it_ends() {
        use std::io::BufRead;

        std::env::set_var("HUNTWELL_CDP_PORT", "9334");
        let dir = std::env::temp_dir().join(format!("huntwell-tabs-test-{}", std::process::id()));
        std::env::set_var("HUNTWELL_CHROME_DIR", &dir);

        start_for_run().expect("chrome starts");
        let pre_existing = page_targets();

        let cfg = mcp_server_config();
        let args: Vec<String> = cfg["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let mut mcp = Command::new(cfg["command"].as_str().unwrap())
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start MCP server");
        let mut stdin = mcp.stdin.take().unwrap();
        let mut stdout = std::io::BufReader::new(mcp.stdout.take().unwrap());
        let mut send = |v: Value| {
            let _ = writeln!(stdin, "{v}");
            let _ = stdin.flush();
        };
        let await_id = |want: i64, stdout: &mut std::io::BufReader<_>| {
            let deadline = Instant::now() + Duration::from_secs(120);
            let mut line = String::new();
            while Instant::now() < deadline {
                line.clear();
                if stdout.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                    if v.get("id").and_then(Value::as_i64) == Some(want) {
                        return;
                    }
                }
            }
            panic!("no response to {want}");
        };
        send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2024-11-05","capabilities":{},
            "clientInfo":{"name":"huntwell-test","version":"1"}}}));
        await_id(1, &mut stdout);
        send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));

        // The scope opens where an agent call would.
        let scope = TabScope::open();
        for (i, url) in ["https://example.com", "https://example.org", "https://example.net"]
            .iter()
            .enumerate()
        {
            send(json!({"jsonrpc":"2.0","id":10 + i as i64,"method":"tools/call","params":{
                "name":"browser_tabs","arguments":{"action":"new","url":url}}}));
            await_id(10 + i as i64, &mut stdout);
        }
        assert!(
            page_targets() >= pre_existing + 3,
            "the tabs the run has to clean up should exist first"
        );

        let closed = scope.reap();
        assert_eq!(closed, 3, "every tab the call opened must be closed");
        assert_eq!(
            page_targets(),
            pre_existing,
            "reaping must land back where the call started, not lower"
        );
        // Chrome exits with its last tab, which would end the run.
        assert!(chrome_up(), "reaping must never close the browser itself");

        // The mid-call cap: a step that opens tabs in a loop gets trimmed back
        // while it is still running, rather than at the end when the memory has
        // already been spent.
        let scope = TabScope::open();
        for (i, url) in ["https://example.com", "https://example.org", "https://example.net"]
            .iter()
            .enumerate()
        {
            send(json!({"jsonrpc":"2.0","id":20 + i as i64,"method":"tools/call","params":{
                "name":"browser_tabs","arguments":{"action":"new","url":url}}}));
            await_id(20 + i as i64, &mut stdout);
        }
        assert_eq!(scope.trim(1), 2, "everything past the cap should go");
        assert_eq!(scope.trim(1), 0, "already at the cap, nothing left to close");
        assert_eq!(page_targets(), pre_existing + 1);
        scope.reap();

        let _ = mcp.kill();
        let _ = mcp.wait();
        stop_for_run();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_explicit_choice_wins_over_detection() {
        // Set for this test only; the auto path is what the other tests cover.
        std::env::set_var("HUNTWELL_CHROME_DISPLAY", "headless");
        assert_eq!(wanted_display(), Some(DisplayMode::Headless));
        std::env::set_var("HUNTWELL_CHROME_DISPLAY", "xvfb");
        assert_eq!(wanted_display(), Some(DisplayMode::Xvfb));
        std::env::set_var("HUNTWELL_CHROME_DISPLAY", "headed");
        assert_eq!(wanted_display(), Some(DisplayMode::Headed));
        // Nonsense falls through to detection rather than failing the run.
        std::env::set_var("HUNTWELL_CHROME_DISPLAY", "sideways");
        assert_eq!(wanted_display(), None);
        std::env::remove_var("HUNTWELL_CHROME_DISPLAY");

        // The older switch still works; it predates there being three options.
        std::env::set_var("HUNTWELL_CHROME_HEADLESS", "1");
        assert_eq!(wanted_display(), Some(DisplayMode::Headless));
        std::env::remove_var("HUNTWELL_CHROME_HEADLESS");
        assert_eq!(wanted_display(), None);
    }

    #[test]
    fn a_dead_socket_does_not_count_as_a_display() {
        // The case that broke scheduled runs: a remote desktop disconnects and
        // leaves DISPLAY set with the socket gone. Believing the variable sent
        // every overnight run into a 30-second timeout.
        let dir = std::env::temp_dir().join(format!("huntwell-disp-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        std::env::set_var("WAYLAND_DISPLAY", "not-a-socket");
        std::env::set_var("DISPLAY", ":98");
        assert!(!display_is_live(), "a name with no socket behind it is not a display");
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var("DISPLAY");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn there_is_always_a_way_to_run_without_a_desktop() {
        // Whatever the machine has, an unattended run must get a browser.
        let mode = offscreen_mode();
        assert!(matches!(mode, DisplayMode::Xvfb | DisplayMode::Headless));
        assert_ne!(mode, DisplayMode::Headed, "that is the mode that just failed");
    }

    #[test]
    fn profile_is_not_the_users_default_chrome() {
        let p = profile_dir().to_string_lossy().to_lowercase();
        assert!(p.contains("huntwell"), "profile must be huntwell's own: {p}");
    }
}

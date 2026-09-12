//! Settings: process environment first, then `local-infra/global` (or the
//! file `HUNTWELL_GLOBAL` names, or `./global` beside a release binary).
//!
//! Only `HUNTWELL_*`, `CURSOR_API_KEY`, `BROWSERBASE_*` and `STRIPE_*` are
//! read from the file, so a line setting `PATH` in a checked-out working
//! directory cannot reach the process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static FILE: OnceLock<(Option<PathBuf>, HashMap<String, String>)> = OnceLock::new();

fn candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    // Read straight from the environment rather than through `get`, so a
    // recursive lookup cannot happen while we are still deciding which file
    // to open.
    if let Some(p) = std::env::var_os("HUNTWELL_GLOBAL") {
        v.push(PathBuf::from(p));
    }
    // The instance decides which file: `global` or `global-<name>`.
    let instance = std::env::var("HUNTWELL_INSTANCE").unwrap_or_default();
    let name = if instance.is_empty() || instance == "default" {
        "global".to_string()
    } else {
        format!("global-{instance}")
    };
    // A debug build reads the repo's local-infra; a release binary reads
    // beside itself and in the cwd.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../local-infra");
    v.push(repo.join(&name));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join(&name));
            v.push(dir.join("local-infra").join(&name));
        }
    }
    v.push(PathBuf::from("local-infra").join(&name));
    v.push(PathBuf::from(&name));
    v
}

fn settable(key: &str) -> bool {
    (key.starts_with("HUNTWELL_")
        && key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        || key == "CURSOR_API_KEY"
        // The bootstrap credentials: the sealed `global` holds these and
        // nothing else, and they are what opens Secrets Manager. Named
        // individually rather than allowing all of AWS_* — the point of this
        // list is that a file in a working directory cannot set anything it
        // likes, and AWS_* includes things like AWS_CA_BUNDLE.
        || matches!(
            key,
            "AWS_ACCESS_KEY_ID" | "AWS_SECRET_ACCESS_KEY" | "AWS_SESSION_TOKEN" | "AWS_REGION"
        )
        // Per-service credentials: AWS_SES_KEY, AWS_S3_SECRET and friends. Each
        // is an IAM user scoped to one service, which is the point — the
        // bootstrap pair above reads Secrets Manager and nothing else.
        // Suffix-matched rather than listed, so adding a service does not mean
        // remembering to edit this; still narrow, because AWS_* at large
        // includes things like AWS_CA_BUNDLE.
        || (key.starts_with("AWS_")
            && (key.ends_with("_KEY") || key.ends_with("_SECRET") || key.ends_with("_SESSION_TOKEN")))
        || key.starts_with("BROWSERBASE_")
        // Stripe keys live beside the others in local-infra/global; without
        // this the file entry is silently ignored and billing looks unconfigured.
        || key.starts_with("STRIPE_")
        // The user pool identity comes from (COGNITO_USER_POOL_ID and friends).
        // Production reads these from Secrets Manager, which no allowlist
        // touches — this is so a dev box can point at a pool by editing one
        // file, and so a missing entry is not silently ignored.
        || key.starts_with("COGNITO_")
}

fn load() -> &'static (Option<PathBuf>, HashMap<String, String>) {
    FILE.get_or_init(|| {
        for path in candidates() {
            if !path.is_file() {
                continue;
            }
            // Through `genesis`, so a whole-file seal opens here and no caller
            // learns the difference. A sealed file that will not open is loud
            // and then empty — silently reading as "nothing configured" would
            // send every credential down its "not set" path.
            let text = match crate::genesis::read_config(&path) {
                Ok(text) => text,
                Err(e) => {
                    eprintln!("{e}");
                    continue;
                }
            };
            let mut map = HashMap::new();
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let Some((k, v)) = line.split_once('=') else { continue };
                let k = k.trim();
                if !settable(k) {
                    continue;
                }
                let v = v.trim().trim_matches('"');
                // An individually sealed value opens here; one that will not
                // open is dropped, with the reason already on stderr.
                let Some(v) = crate::genesis::resolve(k, v) else { continue };
                map.insert(k.to_string(), v);
            }
            return (Some(path), map);
        }
        (None, HashMap::new())
    })
}

/// Which file settings came from, for `doctor`.
pub fn source_file() -> Option<&'static Path> {
    load().0.as_deref()
}

/// Settings fetched from AWS Secrets Manager at startup.
///
/// A separate map rather than merging into the file's, so `doctor` and the log
/// line at boot can say which setting came from where — "it is set but wrong"
/// and "it is coming from somewhere you forgot" look identical otherwise.
static REMOTE: OnceLock<HashMap<String, String>> = OnceLock::new();

/// The resolution order, highest first:
///
/// 1. the process environment — always wins, so a one-off override works
/// 2. AWS Secrets Manager — the deployed source of truth, rotatable without
///    touching the host
/// 3. the `global` file — the bootstrap credential, and everything on a dev box
///
/// Secrets Manager sits above the file because the file is what holds the
/// credentials that opened it: if a value is in both, the remote one is the
/// deliberate, rotatable copy.
pub fn get(key: &str) -> Option<String> {
    if let Ok(v) = std::env::var(key) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if let Some(v) = REMOTE.get().and_then(|m| m.get(key)).filter(|v| !v.is_empty()) {
        return Some(v.clone());
    }
    load().1.get(key).cloned().filter(|v| !v.is_empty())
}

/// Which secret this deployment reads.
///
/// `HUNTWELL_SECRET_ID` names one outright. Otherwise the environment picks:
/// anything but `production` is local, because guessing wrong towards production
/// means a dev box reading live credentials.
pub fn secret_id() -> String {
    if let Some(id) = file_or_env("HUNTWELL_SECRET_ID").filter(|v| !v.trim().is_empty()) {
        return id;
    }
    match file_or_env("HUNTWELL_ENV").unwrap_or_default().trim().to_ascii_lowercase().as_str() {
        "production" | "prod" => "Huntwell_Production".to_string(),
        _ => "Huntwell_Local".to_string(),
    }
}

/// Environment then file, skipping Secrets Manager. For the handful of settings
/// that decide *how* to reach Secrets Manager, which cannot come from it.
fn file_or_env(key: &str) -> Option<String> {
    if let Ok(v) = std::env::var(key) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    load().1.get(key).cloned()
}

/// Fetch the deployment's secret and hold it for the process.
///
/// Called once, at startup, from `boot`. Not fatal when it fails: a box with no
/// AWS credentials is a development box, and one whose credentials stopped
/// working should say so loudly rather than refuse to start with a stack trace.
/// What it cannot do is fail *quietly* — every outcome below is logged.
pub async fn load_remote_secrets() {
    let Some(creds) = crate::aws::credentials(file_or_env) else {
        tracing::debug!("no AWS credentials — settings come from the environment and the global file");
        return;
    };
    let id = secret_id();
    match crate::aws::get_secret(&creds, &id).await {
        Ok(raw) => match crate::aws::parse_settings(&raw) {
            Ok(pairs) => {
                let names: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
                // Names, never values. This line goes to a log that gets pasted
                // into issues.
                tracing::info!("secrets: {} setting(s) from {id} — {}", pairs.len(), names.join(", "));
                let _ = REMOTE.set(pairs.into_iter().collect());
            }
            Err(e) => tracing::error!("secrets: {id} could not be read as settings: {e:#}"),
        },
        Err(e) => tracing::error!("secrets: could not read {id}: {e:#}"),
    }
}

/// Which secret supplied a setting, for `doctor`. `None` means it did not.
pub fn remote_source(key: &str) -> Option<String> {
    REMOTE.get().and_then(|m| m.get(key)).map(|_| secret_id())
}

pub fn get_or(key: &str, default: &str) -> String {
    get(key).unwrap_or_else(|| default.to_string())
}

/// Exports every file setting into this process's environment, so children
/// (runs, the MCP server the agent starts) inherit them without each one
/// re-reading the file. Environment values already set are left alone.
pub fn export_to_env() {
    // File first, then remote over the top: a child process should see the same
    // precedence this process does. Anything already in the environment is left
    // alone in both passes, so an explicit override still wins.
    for (k, v) in &load().1 {
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
    if let Some(remote) = REMOTE.get() {
        for (k, v) in remote {
            match std::env::var(k) {
                // Set by us from the file a moment ago, not by the operator —
                // the remote value is the one that should reach the child.
                Ok(existing) if load().1.get(k).map(|f| f == &existing).unwrap_or(false) => {
                    std::env::set_var(k, v)
                }
                Ok(_) => {}
                Err(_) => std::env::set_var(k, v),
            }
        }
    }
}

pub fn database_url() -> anyhow::Result<String> {
    get("HUNTWELL_DATABASE_URL").ok_or_else(|| {
        anyhow::anyhow!(
            "HUNTWELL_DATABASE_URL is not set — run ./local-infra/start.sh, or export it"
        )
    })
}

/// The database a split microservice connects to. In k8s each service sets
/// `DATABASE_URL` to its own logical database (auth/plans/prospects/runs);
/// the all-in-one `serve` and the local-infra path fall back to the shared
/// `HUNTWELL_DATABASE_URL`.
pub fn service_database_url() -> anyhow::Result<String> {
    if let Ok(u) = std::env::var("DATABASE_URL") {
        if !u.trim().is_empty() {
            return Ok(u);
        }
    }
    database_url()
}

/// When set, HTTP handlers trust the `X-Account-Id` header (injected by the
/// gateway's forward-auth) instead of resolving the session cookie against a
/// local `Session`/`Account` table — which a split service does not have. Off
/// for `serve`/`dev.sh`, so the cookie path (and its tests) are unchanged.
pub fn trust_header_auth() -> bool {
    matches!(get("HUNTWELL_TRUST_HEADER_AUTH").as_deref(), Some("1") | Some("true") | Some("yes"))
}

/// Base URL of a sibling service, when one is configured.
///
/// Nothing sets these any more: the website serves the whole API in one
/// process, so the dashboard aggregate reads the database directly. Kept
/// because the call sites fall back to a local read when it returns `None`,
/// which is exactly what a service that is split out again would need.
pub fn sibling_url(name: &str) -> Option<String> {
    get(&format!("{name}_SVC_URL")).map(|u| u.trim_end_matches('/').to_string())
}

/// Where per-account Chrome profiles and per-run agent workspaces live.
pub fn data_dir() -> PathBuf {
    if let Some(d) = get("HUNTWELL_DATA_DIR") {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("huntwell-web")
}

pub fn cdp_port_base() -> u16 {
    get("HUNTWELL_CDP_PORT_BASE")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20611)
}

pub fn bind_addr() -> String {
    get_or("HUNTWELL_ADDR", "127.0.0.1:8611")
}

pub fn open_signup() -> bool {
    !matches!(get("HUNTWELL_OPEN_SIGNUP").as_deref(), Some("0") | Some("false"))
}

pub fn is_dev() -> bool {
    matches!(std::env::var("HUNTWELL_DEV").as_deref(), Ok("1") | Ok("true"))
}

/// What we charge the customer per million billable (input + output) tokens, in
/// US dollars. Set above Cursor's own per-token cost to make margin; the run's
/// recorded Cursor COGS shows the spread. Defaults to $5.00 per Mtoken.
pub fn sell_usd_per_mtoken() -> f64 {
    get("HUNTWELL_SELL_USD_PER_MTOKEN")
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(5.0)
}

/// What we multiply Cursor's published per-token rates by when we show a
/// model’s price to a customer. 1.30 is a 30% markup. Account billing is still
/// the flat [`sell_usd_per_mtoken`] rate until we charge per model.
pub fn model_markup() -> f64 {
    get("HUNTWELL_MODEL_MARKUP")
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| *v >= 1.0 && *v <= 5.0)
        .unwrap_or(1.30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_allowlist_is_huntwell_and_known_secrets() {
        assert!(settable("HUNTWELL_DATABASE_URL"));
        assert!(settable("HUNTWELL_MODEL_MARKUP"));
        assert!(settable("CURSOR_API_KEY"));
        assert!(!settable("PATH"));
        assert!(!settable("HUNTWELL_lowercase"));
    }
}

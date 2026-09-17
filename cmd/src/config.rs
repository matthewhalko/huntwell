//! Settings: process environment first, then `local-infra/global` (or the
//! file `HUNTWELL_GLOBAL` names, or `./global` beside a release binary).
//!
//! Only `HUNTWELL_*`, `CURSOR_API_KEY`, `BROWSERBASE_*` and `STRIPE_*` are
//! read from the file, so a line setting `PATH` in a checked-out working
//! directory cannot reach the process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static FILE: OnceLock<(Option<PathBuf>, HashMap<String, String>, Option<String>)> = OnceLock::new();

/// The nearest `name` above the working directory, then above the executable's
/// directory — each start included. Park River's rule: how deep the executable
/// sits under the shared files is the deployment's choice, so `/yaksoft/admin`
/// beside `/yaksoft/global` and `/yaksoft/bin/admin` below it both work, and
/// any fixed depth would get one of them wrong.
///
/// Symlinks are resolved first, so `/usr/local/bin/admin` pointing into
/// `/yaksoft/bin` searches from the real install.
pub fn find_upward(name: &str, want_dir: bool) -> Option<PathBuf> {
    let starts = [
        std::env::current_dir().ok(),
        std::env::current_exe()
            .ok()
            .map(|exe| std::fs::canonicalize(&exe).unwrap_or(exe))
            .and_then(|exe| exe.parent().map(PathBuf::from)),
    ];
    starts.into_iter().flatten().find_map(|start| upward_from(start, name, want_dir))
}

fn upward_from(mut current: PathBuf, name: &str, want_dir: bool) -> Option<PathBuf> {
    loop {
        let candidate = current.join(name);
        if (want_dir && candidate.is_dir()) || (!want_dir && candidate.is_file()) {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

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
    // Park River's rule. A debug build reads the repo's local-infra. A release
    // build reads the nearest `global` above the working directory or the
    // executable — /yaksoft/bin/admin reads /yaksoft/global — and nothing
    // compiled in from the machine that built it.
    if cfg!(debug_assertions) {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../local-infra");
        v.push(repo.join(&name));
        v.push(PathBuf::from("local-infra").join(&name));
    } else if let Some(found) = find_upward(&name, false) {
        v.push(found);
    }
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
            // KEY and SECRET are what Park River's sealed global holds, and what
            // Huntwell's holds too; the AWS_* spellings are what every other AWS
            // tool uses, so a box set up for the CLI needs nothing added.
            "KEY" | "SECRET" | "AWS_SECRETS_REGION"
                | "AWS_ACCESS_KEY_ID" | "AWS_SECRET_ACCESS_KEY" | "AWS_SESSION_TOKEN" | "AWS_REGION"
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
        // A host's Cloudflare tunnel edge (cloudflare.rs).
        || key.starts_with("CLOUDFLARE_")
        // The bot check on sign-up and sign-in (turnstile.rs).
        || key.starts_with("TURNSTILE_")
}

fn load() -> &'static (Option<PathBuf>, HashMap<String, String>, Option<String>) {
    FILE.get_or_init(|| {
        for path in candidates() {
            if !path.is_file() {
                continue;
            }
            // Through `genesis`, so a whole-file seal opens here and no caller
            // learns the difference. A sealed file that will not open is loud
            // and then empty — silently reading as "nothing configured" would
            // send every credential down its "not set" path.
            // The first file found is the file, as in Park River: one that will
            // not open is reported, never skipped for another further down.
            let text = match crate::genesis::read_config(&path) {
                Ok(text) => text,
                Err(e) => return (Some(path), HashMap::new(), Some(e)),
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
            return (Some(path), map, None);
        }
        (None, HashMap::new(), None)
    })
}

/// Which file settings came from, for `doctor`.
pub fn source_file() -> Option<&'static Path> {
    load().0.as_deref()
}

/// Why the settings file could not be read, when it was found but would not
/// open — wrong genesis key, wrong encoding.
pub fn source_error() -> Option<&'static str> {
    load().2.as_deref()
}

/// Settings fetched from AWS Secrets Manager at startup.
///
/// A separate map rather than merging into the file's, so `doctor` and the log
/// line at boot can say which setting came from where — "it is set but wrong"
/// and "it is coming from somewhere you forgot" look identical otherwise.
static REMOTE: OnceLock<HashMap<String, String>> = OnceLock::new();

/// Operator settings from the `setting` table, installed once the database is
/// open. Empty until then — the bootstrap window, where only the environment,
/// Secrets Manager and the sealed `global` exist. See `local-infra/db/public/setting.sql`.
static DB: OnceLock<std::sync::RwLock<HashMap<String, String>>> = OnceLock::new();

fn db_layer() -> &'static std::sync::RwLock<HashMap<String, String>> {
    DB.get_or_init(Default::default)
}

/// Values this process copied into its own environment (`export_to_env`), so a
/// child inherits them. Remembered so they are not then mistaken for an
/// operator's override: without this, every value exported from Secrets
/// Manager would sit at the environment's rank, above the database, and an
/// `admin config set` for any such name would silently do nothing.
static EXPORTED: OnceLock<std::sync::Mutex<HashMap<String, String>>> = OnceLock::new();

fn exported() -> &'static std::sync::Mutex<HashMap<String, String>> {
    EXPORTED.get_or_init(Default::default)
}

fn export(key: &str, value: &str) {
    std::env::set_var(key, value);
    if let Ok(mut m) = exported().lock() {
        m.insert(key.to_string(), value.to_string());
    }
}

/// The resolution order, highest first — the same as Park River's:
///
/// 1. the process environment — an emergency override, and how a VM's settings
///    file reaches its services. Values this process exported itself do not
///    count; they are copies of the layers below.
/// 2. the `setting` table — where an operator sets things (`admin config set`),
///    and the home of anything that is configuration rather than a credential
/// 3. AWS Secrets Manager — credentials and connection strings
/// 4. the sealed `global` file — the bootstrap credential, and a dev box
///
/// The database sits above Secrets Manager because it is what an operator can
/// actually edit; the environment stays on top so a bad row can be overridden
/// without a database round trip. Anything read before the database is open
/// sees an empty database layer — which is why the connection string can never
/// live there.
pub fn get(key: &str) -> Option<String> {
    let live = std::env::var(key).ok();
    let ours = exported().lock().ok().and_then(|m| m.get(key).cloned());
    let db = db_layer().read().ok().and_then(|m| m.get(key).cloned());
    let remote = REMOTE.get().and_then(|m| m.get(key).cloned());
    let file = load().1.get(key).cloned();
    pick(live, ours, db, remote, file)
}

/// The layering itself, apart from where each value lives, so the order is
/// testable without touching the process environment.
fn pick(
    live: Option<String>,
    exported_by_us: Option<String>,
    db: Option<String>,
    remote: Option<String>,
    file: Option<String>,
) -> Option<String> {
    let nonempty = |v: Option<String>| v.filter(|v| !v.trim().is_empty());
    if let Some(v) = nonempty(live) {
        // An operator's value, or one this process set at run time for itself —
        // either way not a copy of a lower layer, so it wins.
        if exported_by_us.as_deref() != Some(v.as_str()) {
            return Some(v);
        }
    }
    nonempty(db).or_else(|| nonempty(remote)).or_else(|| nonempty(file))
}

/// Replace the database layer. Called whenever a process opens the database.
///
/// Also exported into this process's environment — over values it exported
/// itself, never over an operator's — so a child process (a run, the agent's
/// MCP server) inherits the same view.
pub fn install_db_settings(rows: Vec<(String, String)>) {
    let map: HashMap<String, String> =
        rows.into_iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| (k, v.trim().to_string())).collect();
    for (k, v) in &map {
        let live = std::env::var(k).ok();
        let ours = exported().lock().ok().and_then(|m| m.get(k).cloned());
        if live.is_none() || live == ours {
            export(k, v);
        }
    }
    if let Ok(mut w) = db_layer().write() {
        *w = map;
    }
}

/// The database layer's names and values, for `admin config list`. Values are
/// safe to show: credentials are refused there (`is_credential_name`).
pub fn db_settings() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> =
        db_layer().read().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    v.sort();
    v
}

/// `--addr ADDR` from `admin serve`'s arguments — Park River's shape. `None`
/// when absent, so the setting or the default applies. Validated here, so a
/// typo fails at the command line rather than as a bind error.
pub fn parse_serve_addr(args: &[String]) -> anyhow::Result<Option<std::net::SocketAddr>> {
    let mut i = 0;
    let mut addr = None;
    while i < args.len() {
        match args[i].as_str() {
            "--addr" => {
                i += 1;
                let Some(v) = args.get(i) else { anyhow::bail!("--addr requires a value") };
                addr = Some(v.parse().map_err(|_| anyhow::anyhow!("invalid --addr {v:?} — expected host:port, e.g. 10.121.17.195:8710"))?);
            }
            other if other.starts_with('-') => anyhow::bail!("unknown flag: {other}"),
            other => anyhow::bail!("unexpected argument: {other}"),
        }
        i += 1;
    }
    Ok(addr)
}

/// Whether a name looks like a credential.
///
/// The `setting` table is readable by every process with the database — which
/// includes every worker VM. A credential stored there would undo the whole
/// point of giving each VM only its role's secrets, so such names are refused
/// and sent to Secrets Manager instead.
pub fn is_credential_name(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    k == "KEY"
        || k == "SECRET"
        || k.contains("PASSWORD")
        || k.contains("TOKEN")
        || k.ends_with("_SECRET")
        || k.ends_with("_KEY")
        || k.ends_with("DATABASE_URL")
        || k.starts_with("HUNTWELL_PG_")
        || k.starts_with("POOL_PG_")
}

/// Every name in the Secrets Manager layer, for `admin config list`. Names only.
pub fn remote_setting_names() -> Vec<String> {
    let mut v: Vec<String> = REMOTE.get().map(|m| m.keys().cloned().collect()).unwrap_or_default();
    v.sort();
    v
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
        "local" | "dev" | "development" => "Huntwell_Local".to_string(),
        // Unset — the normal case, with nothing configured anywhere: the build
        // decides, on the same axis Park River uses. `./build.sh` makes release
        // executables and they are what a server runs; `./dev.sh` builds debug.
        _ => default_secret_name(crate::genesis::embedded_variant() == "prod").to_string(),
    }
}

fn default_secret_name(release: bool) -> &'static str {
    if release {
        "Huntwell_Production"
    } else {
        "Huntwell_Local"
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
    // Lowest layer first, each overwriting only what this process exported
    // before — never an operator's value — so the environment a child inherits
    // ends up holding the same winner `get` would return.
    let layers: [Vec<(String, String)>; 3] = [
        load().1.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        REMOTE.get().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default(),
        db_layer().read().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default(),
    ];
    for layer in layers {
        for (k, v) in layer {
            let live = std::env::var(&k).ok();
            let ours = exported().lock().ok().and_then(|m| m.get(&k).cloned());
            if live.is_none() || live == ours {
                export(&k, &v);
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

/// The database a service connects to. The same as `database_url`; a separate
/// name only because every service calls it.
pub fn service_database_url() -> anyhow::Result<String> {
    database_url()
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

    fn o(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn the_layers_resolve_like_park_river() {
        // environment > setting table > Secrets Manager > global file
        assert_eq!(pick(o("env"), None, o("db"), o("remote"), o("file")), o("env"));
        assert_eq!(pick(None, None, o("db"), o("remote"), o("file")), o("db"));
        assert_eq!(pick(None, None, None, o("remote"), o("file")), o("remote"));
        assert_eq!(pick(None, None, None, None, o("file")), o("file"));
        assert_eq!(pick(None, None, None, None, None), None);
    }

    #[test]
    fn a_value_this_process_exported_does_not_outrank_the_database() {
        // The trap: Secrets Manager values are copied into the environment for
        // child processes. Counted as "the environment", they would sit above
        // the setting table and `admin config set` would do nothing for them.
        assert_eq!(pick(o("from-remote"), o("from-remote"), o("operator-set"), o("from-remote"), None), o("operator-set"));
        // But a live value that differs from what we exported is an operator's
        // (or set at run time, like --dev), and still wins.
        assert_eq!(pick(o("override"), o("from-remote"), o("operator-set"), o("from-remote"), None), o("override"));
    }

    #[test]
    fn blank_values_fall_through_rather_than_winning() {
        assert_eq!(pick(o("  "), None, o(""), o("remote"), None), o("remote"));
    }

    #[test]
    fn a_release_build_reads_production_and_a_debug_build_local() {
        // No environment variable decides it — the build does, as in Park River.
        assert_eq!(default_secret_name(true), "Huntwell_Production");
        assert_eq!(default_secret_name(false), "Huntwell_Local");
    }

    #[test]
    fn credentials_are_recognised_so_the_setting_table_refuses_them() {
        for cred in ["KEY", "SECRET", "AWS_COGNITO_SECRET", "AWS_SES_KEY", "CURSOR_API_KEY", "BROWSERBASE_API_KEY",
                     "HUNTWELL_SESSION_SECRET", "HUNTWELL_DATABASE_URL", "HUNTWELL_POOL_DATABASE_URL",
                     "HUNTWELL_PG_PASSWORD", "HUNTWELL_PG_HOST", "POOL_PG_HOST", "STRIPE_WEBHOOK_SECRET", "SOME_TOKEN"] {
            assert!(is_credential_name(cred), "{cred} should be refused");
        }
        for ok in ["HUNTWELL_ADMIN_ADDR", "HUNTWELL_PUBLIC_URL", "HUNTWELL_BUILD_DIR", "HUNTWELL_OPEN_SIGNUP",
                   "HUNTWELL_MAIL_FROM", "COGNITO_REGION"] {
            assert!(!is_credential_name(ok), "{ok} is configuration, not a credential");
        }
    }

    #[test]
    fn the_global_file_may_hold_park_rivers_bootstrap_names() {
        assert!(settable("KEY"));
        assert!(settable("SECRET"));
    }

    #[test]
    fn serve_takes_its_address_as_a_parameter() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_serve_addr(&a(&["--addr", "10.121.17.195:8710"])).unwrap().unwrap().to_string(), "10.121.17.195:8710");
        assert!(parse_serve_addr(&a(&[])).unwrap().is_none());
        assert!(parse_serve_addr(&a(&["--addr"])).is_err());
        assert!(parse_serve_addr(&a(&["--addr", "not-an-address"])).is_err());
        assert!(parse_serve_addr(&a(&["--port", "1"])).is_err());
    }
    #[test]
    fn shared_files_are_found_above_the_executable() {
        let root = std::env::temp_dir().join(format!("hw-upward-{}", std::process::id()));
        let bin = root.join("bin");
        std::fs::create_dir_all(bin.join("build")).unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("global"), "KEY=x\n").unwrap();
        assert_eq!(upward_from(bin.clone(), "global", false), Some(root.join("global")));
        // The nearest wins.
        assert_eq!(upward_from(bin.clone(), "build", true), Some(bin.join("build")));
        assert_eq!(upward_from(bin.clone(), "missing", false), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

}

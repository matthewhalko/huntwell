//! The Incus driver: the whole application, in VMs.
//!
//! ```text
//!   browser ──► app.<domain> ──► hw-edge (Caddy container on the app VM's host)
//!                                  │
//!                                  ▼
//!              hw-app ── systemd: huntwell-website       :8611
//!                                 huntwell-planning      (agent drafts plans)
//!                                 huntwell-scheduling
//!                                 huntwell-notification
//!                                 huntwell-nats          :4222, TLS + user + password
//!                                   ▲
//!   hw-edge also forwards <app host's private IP>:4222 ──┘
//!                                   ▲
//!              hw-<host>-w1 ── systemd: huntwell-worker@1 … @N
//!                                         each claims one run at a time,
//!                                         and publishes its events to NATS
//! ```
//!
//! Modelled on `../../parkriver/cmd/src/apps/admin/incus_driver.rs`. Nothing is
//! pulled from a registry: the control plane pushes executables from its build
//! folder, a settings file and the units through the host's Incus API, so
//! updating a VM is a file push and a restart.
//!
//! Runs never travel between VMs. A worker slot claims work from Postgres by
//! its own name — `(host_id, "<vm>-<n>")`. Postgres is the source of truth;
//! NATS is the doorbell.
//!
//! ## The bus
//!
//! One NATS, on the app VM. Every worker VM connects to it — on any host — at
//! the app host's private IP, port 4222. VMs cannot reach each other on the
//! bridge, so that address is a proxy device on the app host's edge container,
//! which is the one address the host lets reach VMs. Every VM server therefore
//! needs `--allow <app host IP>:4222` in `sys incus init`, as it does for
//! Postgres. It takes a login — `HUNTWELL_NATS_USER` and
//! `HUNTWELL_NATS_PASSWORD` from the Secrets Manager secret — given to the app
//! VM and to every worker.
//!
//! And it requires TLS, because its traffic crosses the network between
//! servers. The certificates come from the secret too: `HUNTWELL_NATS_TLS_CERT`
//! and `HUNTWELL_NATS_TLS_KEY` (the server's, pushed into the app VM only) and
//! `HUNTWELL_NATS_TLS_CA` (pushed into every VM, which verifies the server
//! against it). The server certificate must name the app host's private IP and
//! 127.0.0.1 — see docs/PRODUCTION.md.
//!
//! ## Blast radius
//!
//! VMs are spread 2–3 to a server so that a compromised one is contained. That
//! only holds if a VM carries nothing it does not need, so the settings file is
//! built per role (`env_file`): a worker gets the database, the agent's key and
//! the browser's, and never the identity pool's admin key, the mail key or the
//! session secret. Rooting a worker costs you run capacity, not your users.
//!
//! ## Inside every VM
//!
//! ```text
//!   /huntwell/bin/          executables — pushed, or from the image
//!   /huntwell/env           settings, root 0600
//!   /huntwell/VERSION       the build running
//!   /huntwell/data/         the only writable place, owned by `huntwell`
//! ```

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::incus;
use crate::store::{self, Db, Host, Vm};

pub const DIR: &str = "/huntwell";
pub const BIN_DIR: &str = "/huntwell/bin";
pub const DATA_DIR: &str = "/huntwell/data";
const ENV_PATH: &str = "/huntwell/env";
const VERSION_PATH: &str = "/huntwell/VERSION";
const NATS_PATH: &str = "/huntwell/bin/nats-server";
const USER: &str = "huntwell";

/// The profile and image `sys incus init --app huntwell` / `sys incus image
/// --app huntwell` create on a host.
pub const PROFILE: &str = "huntwell";
pub const APP_VM: &str = "hw-app";
/// The app VM's services, as the admin's Services card names them. Each runs as
/// the unit `huntwell-<name>`.
pub const APP_SERVICES: &[&str] = &["website", "planning", "scheduling", "notification"];
pub const EDGE: &str = "hw-edge";
pub const WEBSITE_PORT: u16 = 8611;
const NATS_PORT: u16 = 4222;
const NATS_CONF: &str = "/huntwell/nats.conf";
/// The bus's certificates inside a VM. The key only ever exists in the app VM.
const NATS_CA_FILE: &str = "/huntwell/tls/nats-ca.crt";
const NATS_CERT_FILE: &str = "/huntwell/tls/nats.crt";
const NATS_KEY_FILE: &str = "/huntwell/tls/nats.key";

/// The NATS server's config. No secret in it: the user and password come from
/// the settings file as environment variables, which nats-server expands — so
/// they are not on its command line for `ps` to show.
pub fn nats_conf() -> String {
    format!(
        "# Written by the Huntwell control plane.\n\
         listen: 0.0.0.0:{NATS_PORT}\n\
         # Core NATS only: events are doorbells, and Postgres holds anything durable.\n\
         jetstream: disabled\n\
         # Every client, loopback included, must use TLS.\n\
         tls {{\n\
         \x20 cert_file: \"{NATS_CERT_FILE}\"\n\
         \x20 key_file: \"{NATS_KEY_FILE}\"\n\
         \x20 timeout: 5\n\
         }}\n\
         authorization {{\n\
         \x20 user: $HUNTWELL_NATS_USER\n\
         \x20 password: $HUNTWELL_NATS_PASSWORD\n\
         }}\n"
    )
}

/// A worker slot waits this long, on stop, for the plan it is running to
/// finish. Long, because a plan is long — a deploy then rolls slots as their
/// work completes rather than killing it.
const WORKER_DRAIN: &str = "2h";

// ── roles ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    App,
    Worker,
}

impl Role {
    pub fn parse(s: &str) -> Result<Role> {
        match s {
            "app" => Ok(Role::App),
            "worker" => Ok(Role::Worker),
            other => bail!("unknown VM role '{other}' — expected app or worker"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::App => "app",
            Role::Worker => "worker",
        }
    }

    /// The executables this role runs, all pushed from the build folder.
    pub fn binaries(self) -> &'static [&'static str] {
        match self {
            Role::App => &["website", "planning", "scheduling", "notification"],
            // One binary: a slot spawns each run as `worker run --execution-id`,
            // its own executable, so nothing else has to be present.
            Role::Worker => &["worker"],
        }
    }

    /// The long-lived units, for everything but the worker's per-slot ones.
    fn fixed_units(self) -> &'static [&'static str] {
        match self {
            Role::App => &[
                "huntwell-nats",
                "huntwell-website",
                "huntwell-planning",
                "huntwell-scheduling",
                "huntwell-notification",
            ],
            Role::Worker => &[],
        }
    }
}

// ── settings ────────────────────────────────────────────────────────────────

/// Every VM: what it takes to run the agent against a remote browser and to
/// write what a run collects.
const SHARED_SETTINGS: &[&str] = &[
    "CURSOR_API_KEY",
    "BROWSERBASE_API_KEY",
    "BROWSERBASE_PROJECT_ID",
    "BROWSERBASE_REGION",
    "BROWSERBASE_TIMEOUT_S",
    "HUNTWELL_S3_BUCKET",
    "HUNTWELL_S3_ACCESS_KEY",
    "HUNTWELL_S3_SECRET_KEY",
    "HUNTWELL_SELL_USD_PER_MTOKEN",
    "HUNTWELL_MODEL_MARKUP",
];

/// The app VM alone. Each of these is something a compromised worker must not
/// be able to read: the user directory, outbound mail as you, billing, and the
/// key that signs every session.
const APP_ONLY_SETTINGS: &[&str] = &[
    "HUNTWELL_SESSION_SECRET",
    "HUNTWELL_PUBLIC_URL",
    "HUNTWELL_OPEN_SIGNUP",
    "HUNTWELL_IDENTITY",
    "COGNITO_USER_POOL_ID",
    "COGNITO_CLIENT_ID",
    "COGNITO_CLIENT_SECRET",
    "COGNITO_REGION",
    "AWS_COGNITO_KEY",
    "AWS_COGNITO_SECRET",
    "AWS_SES_KEY",
    "AWS_SES_SECRET",
    "HUNTWELL_SES_REGION",
    "HUNTWELL_MAIL_FROM",
    "HUNTWELL_MAIL_API_KEY",
    "STRIPE_SECRET_KEY",
    "STRIPE_WEBHOOK_SECRET",
    // Public, but the website needs it to mount the card field, and it is
    // read from the same place as the rest.
    "STRIPE_PUBLISHABLE_KEY",
    "TURNSTILE_SITE_KEY",
    "TURNSTILE_SECRET_KEY",
];

/// One `KEY="value"` line of a systemd EnvironmentFile.
///
/// Quoted and escaped, because a generated password eventually contains a
/// space, a `#` or a quote, and an unquoted one is cut at the first of them —
/// a truncated credential that fails somewhere far from here. A newline cannot
/// be represented safely at all, so it is refused.
pub fn env_line(key: &str, value: &str) -> Result<String> {
    if value.contains('\n') || value.contains('\r') {
        bail!("{key} contains a newline, which a systemd environment file cannot hold");
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("{key}=\"{escaped}\"\n"))
}

/// The settings file for a VM, from `get` (the admin's own settings).
///
/// `database_url` is the database as the VM reaches it, which is not always
/// how the admin reaches it.
pub fn env_file(
    role: Role,
    database_url: &str,
    host_id: i64,
    nats: Option<&NatsLogin>,
    edge_scheme: &str,
    get: impl Fn(&str) -> Option<String>,
) -> Result<String> {
    let mut out = format!("# Written by the Huntwell control plane for a {} VM.\n", role.as_str());
    let mut set = |k: &str, v: &str| -> Result<()> {
        out.push_str(&env_line(k, v)?);
        Ok(())
    };

    set("HUNTWELL_DATABASE_URL", database_url)?;
    set("HUNTWELL_DATA_DIR", DATA_DIR)?;
    set("HUNTWELL_BROWSER", &get("HUNTWELL_BROWSER").unwrap_or_else(|| "browserbase".into()))?;
    // The object store as a VM reaches it — the same distinction as the
    // database URL, and wrong in the same silent way.
    if let Some(v) = get("HUNTWELL_POOL_S3_ENDPOINT").or_else(|| get("HUNTWELL_S3_ENDPOINT")) {
        set("HUNTWELL_S3_ENDPOINT", &v)?;
    }
    for k in SHARED_SETTINGS {
        if let Some(v) = get(k).filter(|v| !v.trim().is_empty()) {
            set(k, &v)?;
        }
    }

    if let Some(n) = nats {
        set("HUNTWELL_NATS_URL", &n.url)?;
        set("HUNTWELL_NATS_USER", &n.user)?;
        set("HUNTWELL_NATS_PASSWORD", &n.password)?;
        set("HUNTWELL_NATS_CA_FILE", NATS_CA_FILE)?;
    }

    match role {
        Role::Worker => {
            // Which host this worker's slots claim runs on behalf of. The slot
            // name comes from its unit, not from here.
            set("HOST_ID", &host_id.to_string())?;
        }
        Role::App => {
            // The website queues runs for the placement loop instead of
            // forking them inside this VM. Anything else and the workers sit
            // idle while the app VM runs plans on its own.
            set("RUN_DISPATCH", "pool")?;
            set("DRAFT_DISPATCH", "queue")?;
            // Identity lives in Cognito in production; the app VM must not be
            // handed settings that would have it hash passwords into the
            // database instead. Caught here, at Deploy, where the operator is
            // looking — the website would refuse to start anyway.
            let explicit_local = get("HUNTWELL_IDENTITY").is_some_and(|v| v.trim().eq_ignore_ascii_case("local"));
            if !explicit_local && get("COGNITO_USER_POOL_ID").map_or(true, |v| v.trim().is_empty()) {
                bail!(
                    "no Cognito pool is configured, so the app VM would store password hashes in the database. \
                     Add COGNITO_USER_POOL_ID, COGNITO_CLIENT_ID, COGNITO_REGION, COGNITO_CLIENT_SECRET, \
                     AWS_COGNITO_KEY and AWS_COGNITO_SECRET to the Secrets Manager secret (docs/SECRETS.md)"
                );
            }
            // Secure cookies: the edge terminates TLS.
            set("HUNTWELL_DEV", "0")?;
            // Everything reaches the website through the edge, so the visitor's
            // address is in a header the edge sets: the last X-Forwarded-For
            // entry from Caddy, or CF-Connecting-IP through a tunnel. Without
            // this every visitor is the edge, and one bad API key would lock
            // them all out together.
            set("HUNTWELL_TRUST_PROXY", if edge_scheme == "cloudflare" { "cloudflare" } else { "1" })?;
            for k in APP_ONLY_SETTINGS {
                if let Some(v) = get(k).filter(|v| !v.trim().is_empty()) {
                    set(k, &v)?;
                }
            }
        }
    }
    Ok(out)
}

// ── units ───────────────────────────────────────────────────────────────────

/// The hardening every unit shares: the process can write `/huntwell/data` and
/// nothing else, and cannot gain privileges.
fn hardening() -> String {
    format!(
        "NoNewPrivileges=yes\nProtectSystem=strict\nReadWritePaths={DATA_DIR}\nPrivateTmp=yes\n\
         ProtectHome=yes\nProtectKernelTunables=yes\nProtectControlGroups=yes\n"
    )
}

/// The service environment besides the settings file. HOME is the data
/// directory because the agent CLI writes a cache there, and it is the one
/// place a unit may write.
fn service_env() -> String {
    format!(
        "Environment=HOME={DATA_DIR}\nEnvironment=PATH={BIN_DIR}:/usr/local/bin:/usr/bin:/bin\n"
    )
}

/// One app service. Each binds loopback except the website, so an attacker on
/// the bridge finds one port on this VM, not five.
fn app_unit(name: &str, extra_env: &str, stop_timeout: &str) -> String {
    format!(
        "[Unit]\nDescription=Huntwell {name}\nAfter=network-online.target huntwell-nats.service\n\
         Wants=network-online.target huntwell-nats.service\n\n\
         [Service]\nUser={USER}\nWorkingDirectory={DATA_DIR}\nEnvironmentFile={ENV_PATH}\n\
         {}{extra_env}ExecStart={BIN_DIR}/{name}\nRestart=always\nRestartSec=2\nTimeoutStopSec={stop_timeout}\n\
         {}\n[Install]\nWantedBy=multi-user.target\n",
        service_env(),
        hardening()
    )
}

/// Every unit a VM of this role carries, as (path, contents).
pub fn units(role: Role, vm_name: &str) -> Vec<(String, String)> {
    let path = |u: &str| format!("/etc/systemd/system/{u}.service");
    match role {
        Role::App => vec![
            (
                path("huntwell-nats"),
                format!(
                    "[Unit]\nDescription=Huntwell event bus\nAfter=network.target\n\n\
                     [Service]\nUser={USER}\nWorkingDirectory={DATA_DIR}\n\
                     # The user and password its config expands come from here.\n\
                     EnvironmentFile={ENV_PATH}\n\
                     # On the bridge, for the edge's forward to the workers.\n\
                     ExecStart={NATS_PATH} -c {NATS_CONF}\n\
                     Restart=always\nRestartSec=2\n{}\n[Install]\nWantedBy=multi-user.target\n",
                    hardening()
                ),
            ),
            (
                path("huntwell-website"),
                // The one port the edge reaches.
                app_unit("website", &format!("Environment=HUNTWELL_ADDR=0.0.0.0:{WEBSITE_PORT}\n"), "20"),
            ),
            // Drafting runs the agent: give a draft in progress time to finish.
            (path("huntwell-planning"), app_unit("planning", "Environment=HUNTWELL_PLANNING_ADDR=127.0.0.1:8612\n", "120")),
            (path("huntwell-scheduling"), app_unit("scheduling", "Environment=HUNTWELL_SCHEDULING_ADDR=127.0.0.1:8614\n", "20")),
            (path("huntwell-notification"), app_unit("notification", "Environment=HUNTWELL_NOTIFICATION_ADDR=127.0.0.1:8615\n", "20")),
        ],
        Role::Worker => vec![(
            path("huntwell-worker@"),
            format!(
                "[Unit]\nDescription=Huntwell worker slot %i\nAfter=network-online.target\nWants=network-online.target\n\n\
                 [Service]\nUser={USER}\nWorkingDirectory={DATA_DIR}\nEnvironmentFile={ENV_PATH}\n\
                 # What runs are assigned to. Must match store::slot_name.\n\
                 Environment=SLOT_NAME={vm_name}-%i\n\
                 # Every slot starts an ops server; port 0 gives each its own, on\n\
                 # loopback, instead of ten fighting over one.\n\
                 Environment=HUNTWELL_WORKER_ADDR=127.0.0.1:0\n\
                 {}ExecStart={BIN_DIR}/worker\nRestart=always\nRestartSec=2\n\
                 # Graceful drain. SIGTERM reaches the supervisor alone, which\n\
                 # finishes the plan it is running and then exits; only a slot still\n\
                 # busy after {WORKER_DRAIN} has its whole group killed. The default,\n\
                 # control-group, would SIGTERM the agent mid-plan on every deploy.\n\
                 KillMode=mixed\nTimeoutStopSec={WORKER_DRAIN}\n\
                 {}\n[Install]\nWantedBy=multi-user.target\n",
                service_env(),
                hardening()
            ),
        )],
    }
}

/// The systemd names of a worker's slots, `huntwell-worker@1` … `@n`.
pub fn slot_units(from: i32, to: i32) -> Vec<String> {
    (from..=to).map(|n| format!("huntwell-worker@{n}")).collect()
}

// ── builds ──────────────────────────────────────────────────────────────────

/// The CPU a Linux executable is built for, from its ELF header.
pub fn elf_arch(head: &[u8]) -> Option<&'static str> {
    if head.len() < 20 || &head[..4] != b"\x7fELF" || head[4] != 2 || head[5] != 1 {
        return None;
    }
    match u16::from_le_bytes([head[18], head[19]]) {
        0x3e => Some("x86_64"),
        0xb7 => Some("aarch64"),
        _ => None,
    }
}

/// Where deploys come from.
///
/// `HUNTWELL_BUILD_DIR` names it. Otherwise `bin/` in a checkout, which is
/// where `./build.sh` writes, or the nearest `build/` beside or above the
/// admin's own executable on a server (`/yaksoft/bin/admin` → `/yaksoft/build`).
pub fn build_dir() -> PathBuf {
    if let Some(d) = crate::config::get("HUNTWELL_BUILD_DIR").filter(|v| !v.trim().is_empty()) {
        return PathBuf::from(d.trim());
    }
    let checkout = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../bin"));
    if checkout.is_dir() {
        return checkout;
    }
    crate::config::find_upward("build", true).unwrap_or_else(|| PathBuf::from("build"))
}

/// One executable, ready to push.
pub struct Build {
    pub name: &'static str,
    pub path: PathBuf,
    pub sha256: String,
}

/// Every executable a role needs, for a host's CPU, and the version they make
/// up together.
///
/// Found before anything is created: a missing build is the cheapest failure
/// there is and should not leave a half-made VM behind. Each must be a Linux
/// executable for that CPU — a macOS build would push fine and then fail to
/// exec, with an error that does not mention architecture.
pub fn find_builds(dir: &std::path::Path, arch: &str, role: Role) -> Result<(Vec<Build>, String)> {
    let sub = if arch == "aarch64" { "ubuntu-arm64" } else { "ubuntu" };
    let mut builds = Vec::new();
    for name in role.binaries() {
        let candidates = [dir.join(sub).join(name), dir.join(name)];
        let mut seen = Vec::new();
        let mut found = None;
        for path in candidates {
            let mut head = [0u8; 20];
            use std::io::Read;
            if std::fs::File::open(&path).and_then(|mut f| f.read_exact(&mut head)).is_err() {
                continue;
            }
            match elf_arch(&head) {
                Some(a) if a == arch => {
                    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
                    found = Some(Build { name, path, sha256: hex::encode(Sha256::digest(&bytes)) });
                    break;
                }
                Some(a) => seen.push(format!("{} is for {a}", path.display())),
                None => seen.push(format!("{} is not a Linux executable", path.display())),
            }
        }
        match found {
            Some(b) => builds.push(b),
            None => bail!(
                "no {arch} `{name}` in {} — build it with `./build.sh{}` and copy bin/{sub}/ there{}",
                dir.display(),
                if arch == "aarch64" { " arm64" } else { "" },
                if seen.is_empty() { String::new() } else { format!(" (found: {})", seen.join("; ")) }
            ),
        }
    }
    // One label for the set: a VM "at a version" means all of its executables
    // are the ones that version names.
    let mut h = Sha256::new();
    for b in &builds {
        h.update(b.name.as_bytes());
        h.update(b.sha256.as_bytes());
    }
    let version = hex::encode(h.finalize())[..12].to_string();
    Ok((builds, version))
}

// ── talking to a VM ─────────────────────────────────────────────────────────

async fn host_arch(host: &Host) -> Result<String> {
    if !host.arch.is_empty() {
        return Ok(host.arch.clone());
    }
    let info = incus::query(host.remote(), "/1.0").await?;
    info["environment"]["kernel_architecture"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| anyhow!("host '{}' did not report its architecture", host.name))
}

/// The `/huntwell` tree and its user, made or brought up to date — and proof
/// that the image carries what this role needs.
async fn prepare_tree(host: &Host, vm: &str, role: Role) -> Result<()> {
    let mut script = format!(
        "set -e\n\
         id {USER} >/dev/null 2>&1 || useradd --system --home-dir {DIR} --shell /usr/sbin/nologin {USER}\n\
         mkdir -p {BIN_DIR} {DATA_DIR}\n\
         chown -R {USER}:{USER} {DATA_DIR}\n\
         # The agent and the browser tool, which planning and every worker slot run.\n\
         test -x {BIN_DIR}/agent || {{ echo 'no Cursor agent CLI in {BIN_DIR}'; exit 1; }}\n\
         command -v node >/dev/null || {{ echo 'no node'; exit 1; }}\n"
    );
    if role == Role::App {
        script.push_str(&format!("test -x {NATS_PATH} || {{ echo 'no nats-server in {BIN_DIR}'; exit 1; }}\n"));
    }
    incus::exec(host.remote(), vm, &["sh", "-c", &script]).await.map(|_| ()).map_err(|e| {
        anyhow!(
            "could not prepare {DIR} in {vm} — is the host's '{}' image from `sys incus image --app huntwell`? ({e})",
            host.base_image
        )
    })
}

/// Push each executable beside the running one and rename it into place, so a
/// restart mid-push can never exec half a file.
async fn push_builds(host: &Host, vm: &str, builds: &[Build], version: &str) -> Result<()> {
    let remote = host.remote();
    for b in builds {
        let dest = format!("{BIN_DIR}/{}", b.name);
        incus::push_path(remote, vm, &b.path, &format!("{dest}.new"), "0755").await?;
        incus::exec(remote, vm, &["mv", "-f", &format!("{dest}.new"), &dest]).await?;
    }
    incus::push_file(remote, vm, VERSION_PATH, format!("{version}\n").as_bytes(), "0644").await
}

async fn push_settings(db: &Db, host: &Host, vm: &Vm, role: Role) -> Result<bool> {
    let db_url = crate::config::get("HUNTWELL_POOL_DATABASE_URL")
        .or_else(|| crate::config::get("HUNTWELL_DATABASE_URL"))
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("no database URL to give the VM — set HUNTWELL_POOL_DATABASE_URL"))?;
    let nats = nats_login(db, role).await?;
    let env = env_file(role, &db_url, host.host_id, nats.as_ref(), &host.edge_scheme, crate::config::get)?;
    incus::push_file(host.remote(), &vm.name, ENV_PATH, env.as_bytes(), "0600").await?;
    if role == Role::App {
        incus::push_file(host.remote(), &vm.name, NATS_CONF, nats_conf().as_bytes(), "0644").await?;
    }
    if let Some(n) = &nats {
        incus::push_file(host.remote(), &vm.name, NATS_CA_FILE, n.ca.as_bytes(), "0644").await?;
        if let Some((cert, key)) = &n.server {
            incus::push_file(host.remote(), &vm.name, NATS_CERT_FILE, cert.as_bytes(), "0644").await?;
            // 0600 and owned by the service user: nats-server runs as it, and
            // nothing else in the VM needs to read the key.
            incus::push_file(host.remote(), &vm.name, NATS_KEY_FILE, key.as_bytes(), "0600").await?;
            incus::exec(host.remote(), &vm.name, &["chown", &format!("{USER}:{USER}"), NATS_KEY_FILE]).await?;
        }
    }
    for (path, unit) in units(role, &vm.name) {
        incus::push_file(host.remote(), &vm.name, &path, unit.as_bytes(), "0644").await?;
    }
    incus::exec(host.remote(), &vm.name, &["systemctl", "daemon-reload"]).await?;
    Ok(nats.is_some())
}

/// What a VM needs to join the bus.
#[derive(Debug, Clone, PartialEq)]
pub struct NatsLogin {
    pub url: String,
    pub user: String,
    pub password: String,
    /// PEM: the CA every VM verifies the server with.
    pub ca: String,
    /// PEM certificate and key the server presents. The app VM's only.
    pub server: Option<(String, String)>,
}

/// A PEM value from the secret, made usable whatever shape it was pasted in:
/// real newlines, `\n` escapes (what a single-line console field produces), or
/// base64 of the whole PEM.
pub fn pem_setting(name: &str, value: &str) -> Result<String> {
    let mut v = value.trim().to_string();
    if !v.contains('\n') && v.contains("\\n") {
        v = v.replace("\\n", "\n");
    }
    if !v.contains("-----BEGIN") {
        use base64::Engine;
        if let Ok(text) = base64::engine::general_purpose::STANDARD
            .decode(v.split_whitespace().collect::<String>())
            .map_err(|_| ())
            .and_then(|b| String::from_utf8(b).map_err(|_| ()))
        {
            v = text.trim().to_string();
        }
    }
    if !v.contains("-----BEGIN") || !v.contains("-----END") {
        bail!("{name} is not a PEM block (-----BEGIN … -----END) — paste the file's contents into the secret");
    }
    v.push('\n');
    Ok(v)
}

/// Where and as whom a VM of `role` joins the bus: loopback on the app VM; the
/// app host's private IP for a worker. A worker with no app VM yet gets no bus
/// — runs are claimed from Postgres either way — and joins on its next Deploy.
async fn nats_login(db: &Db, role: Role) -> Result<Option<NatsLogin>> {
    // tls:// — the client refuses a server that does not speak TLS.
    let url = match role {
        Role::App => format!("tls://127.0.0.1:{NATS_PORT}"),
        Role::Worker => match app_bus_address(db).await? {
            Some(ip) => format!("tls://{ip}:{NATS_PORT}"),
            None => {
                tracing::warn!("no app VM yet — this worker joins the event bus on its next Deploy");
                return Ok(None);
            }
        },
    };
    let need = |name: &str| {
        crate::config::get(name).filter(|v| !v.trim().is_empty()).ok_or_else(|| {
            anyhow!(
                "{name} is not set — the bus needs HUNTWELL_NATS_USER, HUNTWELL_NATS_PASSWORD, HUNTWELL_NATS_TLS_CA, \
                 HUNTWELL_NATS_TLS_CERT and HUNTWELL_NATS_TLS_KEY in the Secrets Manager secret"
            )
        })
    };
    let pem = |name: &str| need(name).and_then(|v| pem_setting(name, &v));
    let server = match role {
        Role::App => Some((pem("HUNTWELL_NATS_TLS_CERT")?, pem("HUNTWELL_NATS_TLS_KEY")?)),
        Role::Worker => None,
    };
    Ok(Some(NatsLogin {
        url,
        user: need("HUNTWELL_NATS_USER")?,
        password: need("HUNTWELL_NATS_PASSWORD")?,
        ca: pem("HUNTWELL_NATS_TLS_CA")?,
        server,
    }))
}

/// The private IP of the host carrying the app VM — where its edge forwards
/// NATS. Taken from the host's Incus address, which is the private network the
/// admin already reaches it on.
async fn app_bus_address(db: &Db) -> Result<Option<std::net::Ipv4Addr>> {
    let Some(app) = store::list_vms(db).await?.into_iter().find(|v| v.role == "app") else {
        return Ok(None);
    };
    let host = store::get_host(db, app.host_id).await?.ok_or_else(|| anyhow!("the app VM's host is gone"))?;
    endpoint_ip(&host.endpoint)
        .map(Some)
        .ok_or_else(|| anyhow!("host '{}' has Incus address '{}' — workers reach NATS at that host's IP, so it must be an IPv4 address", host.name, host.endpoint))
}

/// The IPv4 address in an Incus endpoint such as `https://10.0.0.2:8443`.
pub fn endpoint_ip(endpoint: &str) -> Option<std::net::Ipv4Addr> {
    let rest = endpoint.trim().split_once("://").map(|(_, r)| r).unwrap_or(endpoint.trim());
    let authority = rest.split('/').next()?;
    let host = authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority);
    host.parse().ok()
}

/// Prove the VM joined the bus — TLS verified, login accepted — from what its
/// service logged since it last started. A certificate that does not name the
/// address, or a CA that does not match, otherwise shows up only as live pages
/// that never update. (A worker's deploy is not checked: its slots restart only
/// as their runs finish.)
async fn wait_bus(host: &Host, vm: &Vm, role: Role) -> Result<()> {
    let unit = match role {
        Role::App => "huntwell-website",
        Role::Worker => "huntwell-worker@1",
    };
    let script = format!(
        "journalctl _SYSTEMD_INVOCATION_ID=$(systemctl show -p InvocationID --value {unit}) -o cat --no-pager \
         | grep -E 'bus: (connected|could not connect)' | tail -n 1"
    );
    for _ in 0..30 {
        let line = incus::exec(host.remote(), &vm.name, &["sh", "-c", &script]).await.unwrap_or_default();
        if line.contains("bus: connected") {
            return Ok(());
        }
        if line.contains("could not connect") {
            bail!(
                "{} could not join the event bus: {} — check that HUNTWELL_NATS_TLS_CERT names the app host's IP and \
                 127.0.0.1, that HUNTWELL_NATS_TLS_CA signed it, and the NATS user and password",
                vm.name,
                redact_urls(line.trim())
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    bail!("{} logged nothing about the event bus after 30s — see `journalctl -u {unit}` in the VM", vm.name)
}

/// Wait until the role's work is actually being done inside the VM.
async fn wait_serving(host: &Host, vm: &Vm, role: Role) -> Result<()> {
    let remote = host.remote();
    let (check, unit): (Vec<String>, String) = match role {
        Role::App => (
            vec!["curl".into(), "-sf".into(), "-o".into(), "/dev/null".into(), "--max-time".into(), "2".into(),
                 format!("http://127.0.0.1:{WEBSITE_PORT}/healthz")],
            "huntwell-website".into(),
        ),
        Role::Worker => (vec!["systemctl".into(), "is-active".into(), "--quiet".into(), "huntwell-worker@1".into()],
                         "huntwell-worker@1".into()),
    };
    let args: Vec<&str> = check.iter().map(String::as_str).collect();
    for _ in 0..90 {
        if incus::exec(remote, &vm.name, &args).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let log = incus::exec(remote, &vm.name, &["journalctl", "-u", &unit, "-n", "30", "--no-pager"])
        .await
        .unwrap_or_default();
    Err(anyhow!("{} is not working after 90s. {unit} log ends:\n{}", vm.name, redact_urls(&log)))
}

/// A log excerpt goes into the database and the console, both of which people
/// paste. Whatever a service printed, no connection string keeps its password.
pub fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail.find(|c: char| c.is_whitespace() || c == '"' || c == '\'').unwrap_or(tail.len());
        let authority = &tail[..end];
        match (authority.find('@'), authority.find(':')) {
            (Some(at), Some(colon)) if colon < at => {
                out.push_str(&authority[..colon]);
                out.push_str(":***");
                out.push_str(&authority[at..]);
            }
            _ => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

// ── lifecycle ───────────────────────────────────────────────────────────────

/// Create a VM from scratch and bring its role up. Always a new instance: a
/// provision is a fresh start, and reusing an old VM would carry whatever
/// drifted on it into the new one.
pub async fn provision(db: &Db, host: &Host, vm: &Vm) -> Result<()> {
    let result = provision_inner(db, host, vm).await;
    if let Err(e) = &result {
        let _ = store::set_vm_status(db, vm.vm_id, "Failed", &format!("{e:#}")).await;
    }
    result
}

async fn provision_inner(db: &Db, host: &Host, vm: &Vm) -> Result<()> {
    let role = Role::parse(&vm.role)?;
    let remote = host.remote();
    store::set_vm_status(db, vm.vm_id, "Provisioning", "").await?;

    let arch = host_arch(host).await?;
    let (builds, version) = find_builds(&build_dir(), &arch, role)?;

    if incus::instance_exists(remote, &vm.name).await {
        incus::run(&["delete", "--force", &incus::target(remote, &vm.name)]).await?;
    }
    incus::run(&[
        "launch",
        &incus::target(remote, &host.base_image),
        &incus::target(remote, &vm.name),
        "--vm",
        "--profile",
        PROFILE,
        "--config",
        &format!("limits.cpu={}", vm.cpu),
        "--config",
        &format!("limits.memory={}", vm.memory),
        "--config",
        "boot.autostart=true",
        "--device",
        &format!("root,size={}", vm.disk),
    ])
    .await
    .map_err(|e| {
        let msg = e.to_string();
        if msg.contains("not found") && msg.contains(&host.base_image) {
            anyhow!("host '{}' has no '{}' image — run `sys incus image --app huntwell` on it. ({msg})", host.name, host.base_image)
        } else if msg.contains("rofile") && msg.contains(PROFILE) {
            anyhow!("host '{}' has no '{PROFILE}' profile — run `sys incus init --app huntwell` on it. ({msg})", host.name)
        } else {
            e
        }
    })?;
    // Ten minutes: seconds on real hardware, but a VM nested inside a laptop's
    // Lima VM measured seven before its agent answered.
    incus::wait_for_agent(remote, &vm.name, 600).await?;

    prepare_tree(host, &vm.name, role).await?;
    let on_bus = push_settings(db, host, vm, role).await?;
    push_builds(host, &vm.name, &builds, &version).await?;
    start_units(host, vm, role).await?;
    wait_serving(host, vm, role).await?;
    if on_bus {
        wait_bus(host, vm, role).await?;
    }

    let address = incus::bridge_address(remote, &vm.name).await.unwrap_or_default();
    store::set_vm_running(db, vm.vm_id, &version, &address).await?;
    if role == Role::App {
        sync_edge(db, host).await?;
        if host.is_cloudflare() {
            // Park River's proof. The part this control plane owns is checked
            // from inside the edge container: Caddy answers for the domain with
            // the website. Cloudflare's side — a new connector registering, a
            // record propagating — routinely takes minutes and is not a reason
            // to fail a working VM, so it is waited for, then only reported.
            let domain = host.ingress_domain.trim().to_string();
            wait_edge(host, &domain).await?;
            let url = format!("https://{domain}");
            match wait_reachable(&url, 150).await {
                Ok(()) => tracing::info!(host = host.name, "reachable at {url}"),
                Err(e) => tracing::warn!(
                    host = host.name,
                    "the app is up behind the tunnel, but Cloudflare was not yet routing {url} ({e:#}). \
                     A new tunnel usually connects within a few minutes."
                ),
            }
        }
    }
    Ok(())
}

async fn start_units(host: &Host, vm: &Vm, role: Role) -> Result<()> {
    let mut units: Vec<String> = role.fixed_units().iter().map(|u| u.to_string()).collect();
    if role == Role::Worker {
        units.extend(slot_units(1, vm.slots));
    }
    let mut args = vec!["systemctl", "enable", "--now"];
    args.extend(units.iter().map(String::as_str));
    incus::exec(host.remote(), &vm.name, &args).await.map(|_| ())
}

/// Push a new build into a running VM.
///
/// The app VM restarts at once — the edge holds requests through the gap.
/// Worker slots restart with `--no-block`, so each rolls onto the new build
/// when the plan it is running finishes, instead of the deploy killing it.
pub async fn deploy(db: &Db, host: &Host, vm: &Vm) -> Result<String> {
    let role = Role::parse(&vm.role)?;
    let arch = host_arch(host).await?;
    let (builds, version) = find_builds(&build_dir(), &arch, role)?;
    // Settings too: a deploy is also how a rotated secret reaches a VM.
    let on_bus = push_settings(db, host, vm, role).await?;
    push_builds(host, &vm.name, &builds, &version).await?;
    match role {
        Role::App => {
            let mut args = vec!["systemctl", "restart"];
            args.extend(role.fixed_units());
            incus::exec(host.remote(), &vm.name, &args).await?;
            wait_serving(host, vm, role).await?;
            if on_bus {
                wait_bus(host, vm, role).await?;
            }
        }
        Role::Worker => {
            let units = slot_units(1, vm.slots);
            let mut args = vec!["systemctl", "restart", "--no-block"];
            args.extend(units.iter().map(String::as_str));
            incus::exec(host.remote(), &vm.name, &args).await?;
        }
    }
    let address = incus::bridge_address(host.remote(), &vm.name).await.unwrap_or_default();
    store::set_vm_running(db, vm.vm_id, &version, &address).await?;
    Ok(version)
}

/// Change a worker's concurrency. Added slots start now; removed ones drain —
/// each stops once its current plan finishes.
pub async fn set_slots(db: &Db, host: &Host, vm: &Vm, slots: i32) -> Result<()> {
    if Role::parse(&vm.role)? != Role::Worker {
        bail!("only a worker VM has slots");
    }
    if !(1..=50).contains(&slots) {
        bail!("slots must be between 1 and 50");
    }
    let remote = host.remote();
    if slots > vm.slots {
        let units = slot_units(vm.slots + 1, slots);
        let mut args = vec!["systemctl", "enable", "--now"];
        args.extend(units.iter().map(String::as_str));
        incus::exec(remote, &vm.name, &args).await?;
    } else if slots < vm.slots {
        let units = slot_units(slots + 1, vm.slots);
        let mut args = vec!["systemctl", "disable", "--now", "--no-block"];
        args.extend(units.iter().map(String::as_str));
        incus::exec(remote, &vm.name, &args).await?;
    }
    store::set_vm_slots(db, vm.vm_id, slots).await
}

pub async fn stop(db: &Db, host: &Host, vm: &Vm) -> Result<()> {
    incus::run(&["stop", &incus::target(host.remote(), &vm.name)]).await?;
    store::set_vm_status(db, vm.vm_id, "Stopped", "").await
}

pub async fn start(db: &Db, host: &Host, vm: &Vm) -> Result<()> {
    incus::run(&["start", &incus::target(host.remote(), &vm.name)]).await?;
    incus::wait_for_agent(host.remote(), &vm.name, 300).await?;
    let address = incus::bridge_address(host.remote(), &vm.name).await.unwrap_or_default();
    store::set_vm_running(db, vm.vm_id, &vm.version, &address).await?;
    if vm.role == "app" {
        // Its address can change across a stop.
        sync_edge(db, host).await?;
    }
    Ok(())
}

/// Remove a VM. Refused while a run is assigned to one of its slots.
pub async fn delete(db: &Db, host: &Host, vm: &Vm) -> Result<()> {
    // The database check first: it is the one that can say no.
    store::delete_vm(db, vm).await?;
    if incus::instance_exists(host.remote(), &vm.name).await {
        incus::run(&["delete", "--force", &incus::target(host.remote(), &vm.name)]).await?;
    }
    if vm.role == "app" {
        sync_edge(db, host).await?;
    }
    Ok(())
}

// ── the edge ────────────────────────────────────────────────────────────────

/// The edge's whole Caddyfile. Rebuilt from what is placed each time, so a
/// deleted app VM's route cannot linger. Behind a tunnel the site is plain
/// http: Cloudflare terminates TLS, and `cloudflared` hands requests to Caddy
/// on 127.0.0.1:80.
pub fn caddyfile(host: &Host, app_address: Option<&str>) -> String {
    let tls = host.edge_scheme == "https";
    let mut out = String::from("# Written by the Huntwell control plane. Rebuilt on every change.\n{\n\tadmin localhost:2019\n");
    if !tls {
        out.push_str("\tauto_https off\n");
    }
    out.push_str("}\n\n");
    if let Some(addr) = app_address {
        let domain = if host.ingress_domain.trim().is_empty() { "localhost" } else { host.ingress_domain.trim() };
        let site = if tls { domain.to_string() } else { format!("http://{domain}") };
        // lb_try_duration is the pause during a deploy: while the website
        // restarts nothing is listening, and instead of a 502 the edge keeps
        // retrying — the request simply waits.
        out.push_str(&format!(
            "{site} {{\n\treverse_proxy {addr}:{WEBSITE_PORT} {{\n\t\tlb_try_duration 30s\n\t\tlb_try_interval 250ms\n\t}}\n}}\n"
        ));
    }
    out
}

/// The edge container, created on first use. A container rather than a VM on
/// purpose: its published ports are real listening sockets on the host, where
/// a VM could only publish through NAT rules.
async fn ensure_edge(host: &Host) -> Result<()> {
    let remote = host.remote();
    if incus::instance_exists(remote, EDGE).await {
        return Ok(());
    }
    let network = incus::query(remote, "/1.0/networks/incusbr0").await?;
    let gateway = network["config"]["ipv4.address"]
        .as_str()
        .and_then(|c| c.split('/').next())
        .and_then(|a| a.parse::<std::net::Ipv4Addr>().ok())
        .ok_or_else(|| anyhow!("host '{}' has no IPv4 on incusbr0 — was it set up with `sys incus init`?", host.name))?;
    // A fixed address, the one `sys incus init` admits: the host's egress table
    // lets exactly gateway+1 reach VMs on the bridge. A DHCP address would come
    // up looking healthy and then be refused every connection to the app VM.
    let edge_ip = edge_address(gateway);
    incus::run(&[
        "launch",
        "images:alpine/3.21",
        &incus::target(remote, EDGE),
        "--device",
        &format!("eth0,ipv4.address={edge_ip}"),
    ])
    .await?;
    incus::wait_for_agent(remote, EDGE, 120).await?;
    incus::exec(
        remote,
        EDGE,
        &["sh", "-c",
          "for i in $(seq 1 30); do nslookup dl-cdn.alpinelinux.org >/dev/null 2>&1 && break; sleep 1; done; \
           apk update -q && apk add -q caddy && rc-update add caddy default && mkdir -p /etc/caddy"],
    )
    .await?;
    Ok(())
}

/// The ports the edge publishes on the server: none behind a tunnel, which
/// dials out; 443 and 80 for https; 80 for http.
pub fn edge_listens(host: &Host) -> Vec<(i32, u16)> {
    if host.is_cloudflare() {
        Vec::new()
    } else if host.edge_scheme == "https" {
        vec![(if host.edge_port == 0 { 443 } else { host.edge_port }, 443), (80, 80)]
    } else {
        vec![(if host.edge_port == 0 { 80 } else { host.edge_port }, 80)]
    }
}

/// The bus forward: the host's private IP, port 4222, to NATS in the app VM.
/// Only on the private address, never 0.0.0.0 — the bus is not for the
/// internet — and connected from inside the edge container, whose address is
/// the one the host's egress rules let reach a VM.
pub fn nats_forward(host: &Host, app_address: Option<&str>) -> Result<Option<(String, String, String)>> {
    let Some(app) = app_address.filter(|a| !a.is_empty()) else { return Ok(None) };
    let ip = endpoint_ip(&host.endpoint).ok_or_else(|| {
        anyhow!(
            "host '{}' has Incus address '{}' — workers reach NATS at this host's IP, so it must be an IPv4 address",
            host.name,
            host.endpoint
        )
    })?;
    Ok(Some(("nats".into(), format!("tcp:{ip}:{NATS_PORT}"), format!("tcp:{app}:{NATS_PORT}"))))
}

/// Bring the edge's proxy devices in line with the host's scheme. Checked on
/// every sync, not just at creation, so switching a host to Cloudflare stops
/// it listening on the server's 80 and 443, and switching back opens them.
async fn sync_edge_ports(host: &Host, app_address: Option<&str>) -> Result<()> {
    let remote = host.remote();
    let target = incus::target(remote, EDGE);
    let instance = incus::query(remote, &format!("/1.0/instances/{EDGE}")).await?;
    let want: Vec<(String, String, String)> = edge_listens(host)
        .iter()
        .enumerate()
        .map(|(i, (outer, inner))| {
            (format!("edge{i}"), format!("tcp:0.0.0.0:{outer}"), format!("tcp:127.0.0.1:{inner}"))
        })
        .chain(nats_forward(host, app_address)?)
        .collect();
    let have = instance["devices"].as_object().cloned().unwrap_or_default();
    for (name, dev) in &have {
        if dev["type"] != "proxy" || !(name.starts_with("edge") || name == "nats") {
            continue;
        }
        let keep = want.iter().any(|(n, l, c)| n == name && dev["listen"] == l.as_str() && dev["connect"] == c.as_str());
        if !keep {
            incus::run(&["config", "device", "remove", &target, name]).await?;
        }
    }
    for (name, listen, connect) in &want {
        let present = have.get(name).is_some_and(|d| d["listen"] == listen.as_str() && d["connect"] == connect.as_str());
        if !present {
            incus::run(&["config", "device", "add", &target, name, "proxy", &format!("listen={listen}"), &format!("connect={connect}")])
                .await?;
        }
    }
    Ok(())
}

/// The edge's address on the bridge. Must match `edge_address` in
/// `~/Desktop/projects/system/src/cmd/incus.rs`, which writes the egress rule
/// that admits it.
pub fn edge_address(gateway: std::net::Ipv4Addr) -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::from(u32::from(gateway).wrapping_add(1))
}

/// Route the host's ingress domain to the app VM, if this host carries it.
pub async fn sync_edge(db: &Db, host: &Host) -> Result<()> {
    let vms = store::list_host_vms(db, host.host_id).await?;
    let Some(app) = vms.iter().find(|v| v.role == "app") else {
        // No app VM here. The edge exists only for the app — its site, its one
        // tunnel, its bus forward — so a host without one has no edge at all:
        // never created, and removed (tunnel included) if the app moved away.
        return remove_edge(host).await;
    };
    if host.is_cloudflare() && host.ingress_domain.trim().is_empty() {
        bail!("host '{}' uses a Cloudflare tunnel but has no domain — set the domain the site answers on", host.name);
    }
    ensure_edge(host).await?;
    // Asked of Incus each time rather than stored: a lease can change across a
    // host reboot, and a stale address would route to nobody.
    let address = incus::bridge_address(host.remote(), &app.name).await;
    sync_edge_ports(host, address.as_deref()).await?;
    // Tunnel first, then Caddy — Park River's order.
    if host.is_cloudflare() {
        ensure_tunnel(host, address.is_some()).await?;
    } else {
        stop_tunnel(host).await;
    }
    load_caddyfile(host, &caddyfile(host, address.as_deref())).await
}

// ── Cloudflare Tunnel ───────────────────────────────────────────────────────
//
// Park River's arrangement. One tunnel per host, its `cloudflared` running in
// the edge container beside Caddy: Cloudflare sends the host's domain down the
// tunnel to Caddy on 127.0.0.1:80, and Caddy routes it to the app VM exactly as
// it does for any other edge. The domain gets a proxied CNAME to the tunnel.
//
// Needs CLOUDFLARE_API_TOKEN (Account › Cloudflare Tunnel › Edit, Zone › DNS ›
// Edit) in the Secrets Manager secret, and CLOUDFLARE_ACCOUNT_ID and
// CLOUDFLARE_ZONE_ID there or in the setting table.

const CLOUDFLARED: &str = "/huntwell/bin/cloudflared";

/// The tunnel's routes: the host's domain to Caddy, anything else refused.
pub fn tunnel_ingress(hostname: Option<&str>) -> Value {
    let mut rules: Vec<Value> = hostname
        .into_iter()
        .map(|h| json!({ "hostname": h, "service": "http://127.0.0.1:80" }))
        .collect();
    // cloudflared requires the last rule to match everything.
    rules.push(json!({ "service": "http_status:404" }));
    Value::Array(rules)
}

/// The OpenRC service that keeps `cloudflared` running in the edge container.
/// The token is read from /etc/conf.d/cloudflared (0600), not the command
/// line, so it never shows in `ps`.
const CLOUDFLARED_INIT: &str = "#!/sbin/openrc-run\n\
name=cloudflared\n\
supervisor=supervise-daemon\n\
command=/huntwell/bin/cloudflared\n\
command_args=\"--no-autoupdate tunnel run\"\n\
respawn_delay=2\n\
respawn_max=0\n\
depend() { need net; }\n";

/// Create the host's tunnel if needed, set its routes, install and run its
/// connector in the edge container, and point the domain at it — or, with no
/// app VM to route to, take the domain back off it.
async fn ensure_tunnel(host: &Host, routed: bool) -> Result<()> {
    let cf = crate::cloudflare::Cloudflare::load()?;
    let tunnel = cf.ensure_tunnel(&crate::cloudflare::tunnel_name(&host.name)).await?;
    let domain = host.ingress_domain.trim();
    let hostname = (routed && !domain.is_empty()).then_some(domain);
    cf.put_ingress(&tunnel, tunnel_ingress(hostname)).await?;

    let remote = host.remote();
    if incus::exec(remote, EDGE, &["test", "-x", CLOUDFLARED]).await.is_err() {
        let arch = if host_arch(host).await? == "aarch64" { "arm64" } else { "amd64" };
        incus::exec(remote, EDGE, &["sh", "-c", &format!(
            "set -e; apk add -q curl ca-certificates; mkdir -p /huntwell/bin; \
             curl -fsSL -o {CLOUDFLARED}.new https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-{arch}; \
             chmod 0755 {CLOUDFLARED}.new; mv -f {CLOUDFLARED}.new {CLOUDFLARED}"
        )])
        .await
        .map_err(|e| anyhow!("installing cloudflared in the edge container: {e:#}"))?;
    }
    let token = cf.tunnel_token(&tunnel).await?;
    let conf = format!("# Written by the Huntwell control plane.\nexport TUNNEL_TOKEN=\"{token}\"\n");
    let current = incus::exec(remote, EDGE, &["cat", "/etc/conf.d/cloudflared"]).await.unwrap_or_default();
    let changed = current != conf;
    if changed {
        incus::push_file(remote, EDGE, "/etc/conf.d/cloudflared", conf.as_bytes(), "0600").await?;
        incus::push_file(remote, EDGE, "/etc/init.d/cloudflared", CLOUDFLARED_INIT.as_bytes(), "0755").await?;
        incus::exec(remote, EDGE, &["rc-update", "add", "cloudflared", "default"]).await?;
    }
    let running = incus::exec(remote, EDGE, &["rc-service", "cloudflared", "status"]).await.is_ok();
    if changed || !running {
        incus::exec(remote, EDGE, &["rc-service", "cloudflared", "restart"]).await?;
    }
    match hostname {
        Some(h) => cf.ensure_dns(h, &tunnel).await?,
        None if !domain.is_empty() => cf.delete_dns_if_ours(domain, &tunnel).await?,
        None => {}
    }
    Ok(())
}

/// Ask Caddy in the edge container for `hostname`, as the tunnel will.
async fn wait_edge(host: &Host, hostname: &str) -> Result<()> {
    let mut last = String::new();
    for _ in 0..30 {
        match incus::exec(host.remote(), EDGE, &[
            "curl", "-s", "-o", "/dev/null", "-m", "5", "-w", "%{http_code}", "-H", &format!("Host: {hostname}"),
            "http://127.0.0.1:80/",
        ])
        .await
        {
            Ok(code) if code.trim().parse::<u16>().is_ok_and(|c| c > 0 && c < 500) => return Ok(()),
            Ok(code) => last = format!("HTTP {}", code.trim()),
            Err(e) => last = format!("{e:#}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    bail!("the edge container does not serve {hostname} ({last}) — check its Caddyfile and the app VM's website")
}

/// Wait for the site to answer through the edge — the whole path a browser
/// takes. Anything below 500 counts; every 5xx is someone else answering for
/// it: the edge with no upstream (502), or Cloudflare with no connector (530).
async fn wait_reachable(url: &str, attempts: u32) -> Result<()> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build()?;
    let mut last = String::new();
    for _ in 0..attempts {
        match client.get(url).send().await {
            Ok(r) if r.status().as_u16() < 500 => return Ok(()),
            Ok(r) => last = format!("HTTP {}", r.status()),
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    bail!("{url} never answered ({last})")
}

/// A host switched away from Cloudflare: stop its connector, so the tunnel
/// does not go on serving beside the new edge. The tunnel itself is left for
/// `remove_edge`, since switching back should find it again.
async fn stop_tunnel(host: &Host) {
    let remote = host.remote();
    if incus::exec(remote, EDGE, &["test", "-f", "/etc/init.d/cloudflared"]).await.is_ok() {
        let _ = incus::exec(remote, EDGE, &["rc-service", "cloudflared", "stop"]).await;
        let _ = incus::exec(remote, EDGE, &["rc-update", "del", "cloudflared", "default"]).await;
    }
}

async fn load_caddyfile(host: &Host, file: &str) -> Result<()> {
    incus::push_file(host.remote(), EDGE, "/etc/caddy/Caddyfile", file.as_bytes(), "0644").await?;
    if incus::exec(host.remote(), EDGE, &["caddy", "reload", "--config", "/etc/caddy/Caddyfile"]).await.is_err() {
        // Not running yet: a fresh edge, or a restarted container.
        incus::exec(host.remote(), EDGE, &["rc-service", "caddy", "restart"]).await?;
    }
    Ok(())
}

/// Remove the edge container, if this host has one. Part of forgetting a host:
/// once the trust is gone nothing tracks it, and it would go on listening on
/// the server's 80 and 443.
pub async fn remove_edge(host: &Host) -> Result<()> {
    // The tunnel and its DNS record outlive the container otherwise, and the
    // record would go on resolving to a tunnel with no connector.
    if host.is_cloudflare() {
        match crate::cloudflare::Cloudflare::load() {
            Ok(cf) => {
                let domain = host.ingress_domain.trim();
                cf.teardown(&crate::cloudflare::tunnel_name(&host.name), (!domain.is_empty()).then_some(domain)).await;
            }
            Err(e) => tracing::warn!(host = host.name, "could not remove the Cloudflare tunnel: {e:#}"),
        }
    }
    if incus::instance_exists(host.remote(), EDGE).await {
        incus::run(&["delete", "--force", &incus::target(host.remote(), EDGE)]).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn settings() -> HashMap<&'static str, String> {
        let mut m = HashMap::new();
        for (k, v) in [
            ("CURSOR_API_KEY", "crsr_live"),
            ("BROWSERBASE_API_KEY", "bb_live"),
            ("BROWSERBASE_PROJECT_ID", "proj"),
            ("HUNTWELL_S3_BUCKET", "huntwell"),
            ("HUNTWELL_SESSION_SECRET", "sess-secret"),
            ("AWS_COGNITO_KEY", "AKIACOGNITO"),
            ("AWS_COGNITO_SECRET", "cognito-secret"),
            ("AWS_SES_KEY", "AKIASES"),
            ("AWS_SES_SECRET", "ses-secret"),
            ("COGNITO_USER_POOL_ID", "us-east-1_x"),
            ("STRIPE_SECRET_KEY", "sk_live"),
        ] {
            m.insert(k, v.to_string());
        }
        m
    }

    fn env(role: Role) -> String {
        let s = settings();
        env_file(role, "postgres://u:p@10.0.0.2:5432/huntwell", 7, None, "https", |k| s.get(k).cloned()).unwrap()
    }

    #[test]
    fn an_app_vm_is_never_configured_to_hash_passwords() {
        let mut s = settings();
        s.remove("COGNITO_USER_POOL_ID");
        let e = env_file(Role::App, "postgres://u:p@10.0.0.2:5432/huntwell", 7, None, "https", |k| s.get(k).cloned())
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("Cognito"), "{e}");
        // Said out loud, local identity is allowed.
        s.insert("HUNTWELL_IDENTITY", "local".into());
        assert!(env_file(Role::App, "postgres://u:p@10.0.0.2:5432/huntwell", 7, None, "https", |k| s.get(k).cloned()).is_ok());
        // A worker never hashes anything either way.
        s.remove("HUNTWELL_IDENTITY");
        assert!(env_file(Role::Worker, "postgres://u:p@10.0.0.2:5432/huntwell", 7, None, "https", |k| s.get(k).cloned()).is_ok());
    }

    #[test]
    fn a_worker_never_holds_what_it_does_not_need() {
        // The reason VMs are spread across servers is to contain a compromise.
        // That only works if a rooted worker finds nothing worth stealing.
        let w = env(Role::Worker);
        for secret in ["sess-secret", "cognito-secret", "AKIACOGNITO", "ses-secret", "AKIASES", "us-east-1_x", "sk_live"] {
            assert!(!w.contains(secret), "a worker VM was handed {secret}:\n{w}");
        }
        // And it has what it does need.
        for needed in ["crsr_live", "bb_live", "HOST_ID=\"7\"", "HUNTWELL_DATABASE_URL"] {
            assert!(w.contains(needed), "a worker VM is missing {needed}:\n{w}");
        }
    }

    #[test]
    fn the_app_vm_queues_runs_for_the_workers() {
        let a = env(Role::App);
        assert!(a.contains("RUN_DISPATCH=\"pool\""), "{a}");
        assert!(a.contains("HUNTWELL_DEV=\"0\""), "{a}");
        // Behind Caddy the visitor's address is the last X-Forwarded-For entry;
        // behind a tunnel it is CF-Connecting-IP. A worker has no visitors.
        assert!(a.contains("HUNTWELL_TRUST_PROXY=\"1\""), "{a}");
        let s = settings();
        let cf = env_file(Role::App, "postgres://u:p@10.0.0.2:5432/huntwell", 7, None, "cloudflare", |k| s.get(k).cloned()).unwrap();
        assert!(cf.contains("HUNTWELL_TRUST_PROXY=\"cloudflare\""), "{cf}");
        assert!(!env(Role::Worker).contains("TRUST_PROXY"));
        assert!(a.contains("sess-secret") && a.contains("cognito-secret"), "{a}");
        // It is not a worker, so it claims nothing.
        assert!(!a.contains("HOST_ID"), "{a}");
    }

    #[test]
    fn every_vm_joins_the_bus_with_the_login_from_the_secret() {
        let s = settings();
        let login = |url: &str| NatsLogin {
            url: url.into(),
            user: "hw".into(),
            password: "p@ss word".into(),
            ca: "-----BEGIN CERTIFICATE-----\nx\n-----END CERTIFICATE-----\n".into(),
            server: None,
        };
        let w = env_file(Role::Worker, "postgres://u:p@10.0.0.2:5432/huntwell", 7, Some(&login("nats://10.0.0.2:4222")), "https", |k| s.get(k).cloned()).unwrap();
        assert!(w.contains("HUNTWELL_NATS_URL=\"nats://10.0.0.2:4222\""), "{w}");
        assert!(w.contains("HUNTWELL_NATS_USER=\"hw\"") && w.contains("HUNTWELL_NATS_PASSWORD=\"p@ss word\""), "{w}");
        assert!(w.contains("HUNTWELL_NATS_CA_FILE=\"/huntwell/tls/nats-ca.crt\""), "{w}");
        // PEM never goes in the env file (it cannot hold newlines); it is pushed as files.
        assert!(!w.contains("BEGIN"), "{w}");
        let a = env_file(Role::App, "postgres://u:p@10.0.0.2:5432/huntwell", 7, Some(&login("nats://127.0.0.1:4222")), "https", |k| s.get(k).cloned()).unwrap();
        assert!(a.contains("HUNTWELL_NATS_URL=\"nats://127.0.0.1:4222\"") && a.contains("HUNTWELL_NATS_PASSWORD"), "{a}");

        // The server takes the same login, from the environment, not its config file.
        let c = nats_conf();
        assert!(c.contains("user: $HUNTWELL_NATS_USER") && c.contains("password: $HUNTWELL_NATS_PASSWORD"), "{c}");
        assert!(c.contains("cert_file: \"/huntwell/tls/nats.crt\"") && c.contains("key_file: \"/huntwell/tls/nats.key\""), "{c}");
        assert!(c.contains("jetstream: disabled") && !c.contains("store_dir"), "{c}");
        let units = units(Role::App, "hw-app");
        let nats = &units.iter().find(|(p, _)| p.ends_with("huntwell-nats.service")).unwrap().1;
        assert!(nats.contains("EnvironmentFile=/huntwell/env") && nats.contains("-c /huntwell/nats.conf"), "{nats}");
    }

    /// Run `script` with bash in `dir` (the openssl steps in docs/PRODUCTION.md).
    fn sh(dir: &std::path::Path, script: &str) {
        let ok = std::process::Command::new("bash").arg("-c").arg(script).current_dir(dir).status().unwrap().success();
        assert!(ok, "failed: {script}");
    }

    /// Against a real nats-server and certificates made the documented way:
    /// `cargo test real_nats -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn real_nats_requires_tls_verified_against_the_ca_and_the_login() {
        let dir = std::env::temp_dir().join(format!("hw-nats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tls")).unwrap();
        // The documented commands, verbatim apart from the IP.
        sh(&dir, "openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
                  -subj '/CN=Huntwell NATS CA' -keyout nats-ca.key -out nats-ca.crt 2>/dev/null");
        sh(&dir, "openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
                  -subj '/CN=huntwell-nats' -keyout nats.key -out nats.csr 2>/dev/null");
        sh(&dir, "printf 'subjectAltName=IP:10.0.0.2,IP:127.0.0.1\\nextendedKeyUsage=serverAuth\\n' > nats.ext && \
                  openssl x509 -req -in nats.csr -CA nats-ca.crt -CAkey nats-ca.key -CAcreateserial -days 825 \
                  -extfile nats.ext -out nats.crt 2>/dev/null");
        // A certificate that does not name 127.0.0.1, and a CA that did not sign anything here.
        sh(&dir, "printf 'subjectAltName=IP:10.0.0.2\\n' > other.ext && openssl x509 -req -in nats.csr -CA nats-ca.crt \
                  -CAkey nats-ca.key -CAcreateserial -days 825 -extfile other.ext -out noloop.crt 2>/dev/null");
        sh(&dir, "openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 30 \
                  -subj '/CN=Stranger' -keyout stranger.key -out stranger.crt 2>/dev/null");

        // Through the same path the admin takes: the secret's values, as files.
        let read = |f: &str| pem_setting(f, &std::fs::read_to_string(dir.join(f)).unwrap()).unwrap();
        let start = |cert: &str, port: u16| {
            let conf = nats_conf()
                .replace("0.0.0.0:4222", &format!("127.0.0.1:{port}"))
                .replace(DATA_DIR, &dir.to_string_lossy())
                .replace(NATS_CERT_FILE, &dir.join(cert).to_string_lossy())
                .replace(NATS_KEY_FILE, &dir.join("nats.key").to_string_lossy());
            std::fs::write(dir.join(format!("nats-{port}.conf")), conf).unwrap();
            tokio::process::Command::new(std::env::var("NATS_SERVER").unwrap_or_else(|_| "nats-server".into()))
                .arg("-c").arg(dir.join(format!("nats-{port}.conf")))
                .env("HUNTWELL_NATS_USER", "huntwell")
                .env("HUNTWELL_NATS_PASSWORD", "s3cret-test")
                .kill_on_drop(true)
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        };
        std::fs::write(dir.join("tls/ca.crt"), read("nats-ca.crt")).unwrap();
        std::fs::write(dir.join("tls/stranger.crt"), read("stranger.crt")).unwrap();
        let mut good = start("nats.crt", 14222);
        let mut noloop = start("noloop.crt", 14223);
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

        let client = |ca: &str, pw: &str| {
            async_nats::ConnectOptions::new()
                .user_and_password("huntwell".into(), pw.into())
                .add_root_certificates(dir.join("tls").join(ca))
                .require_tls(true)
                .max_reconnects(1)
        };
        // The configuration the VMs get: verified, logged in, messages flow.
        let app = client("ca.crt", "s3cret-test").connect("tls://127.0.0.1:14222").await.unwrap();
        let mut seen = app.subscribe("huntwell.>").await.unwrap();
        app.flush().await.unwrap();
        let worker = client("ca.crt", "s3cret-test").connect("tls://127.0.0.1:14222").await.unwrap();
        worker.publish(crate::bus::subject::RUN_METERED, "ok".into()).await.unwrap();
        worker.flush().await.unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), futures_util::StreamExt::next(&mut seen)).await.unwrap().unwrap();
        assert_eq!(got.subject.as_str(), crate::bus::subject::RUN_METERED);

        // Refused: plaintext, an untrusted CA, a certificate not naming the address, a wrong password.
        let quick = |o: async_nats::ConnectOptions, url: &'static str| async move {
            tokio::time::timeout(std::time::Duration::from_secs(5), o.connect(url)).await.map(|r| r.is_ok()).unwrap_or(false)
        };
        assert!(!quick(async_nats::ConnectOptions::new().user_and_password("huntwell".into(), "s3cret-test".into()), "nats://127.0.0.1:14222").await, "plaintext was accepted");
        assert!(!quick(client("stranger.crt", "s3cret-test"), "tls://127.0.0.1:14222").await, "an untrusted CA was accepted");
        assert!(!quick(client("ca.crt", "s3cret-test"), "tls://127.0.0.1:14223").await, "a certificate not naming 127.0.0.1 was accepted");
        assert!(!quick(client("ca.crt", "wrong"), "tls://127.0.0.1:14222").await, "a wrong password was accepted");

        let _ = good.kill().await;
        let _ = noloop.kill().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pem_settings_survive_however_they_were_pasted() {
        let pem = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        let want = format!("{pem}\n");
        assert_eq!(pem_setting("X", pem).unwrap(), want);
        assert_eq!(pem_setting("X", &pem.replace('\n', "\\n")).unwrap(), want);
        use base64::Engine;
        assert_eq!(pem_setting("X", &base64::engine::general_purpose::STANDARD.encode(pem)).unwrap(), want);
        assert!(pem_setting("X", "not a certificate").is_err());
    }

    #[test]
    fn workers_reach_the_bus_on_the_app_hosts_private_ip() {
        assert_eq!(endpoint_ip("https://10.0.0.2:8443"), Some("10.0.0.2".parse().unwrap()));
        assert_eq!(endpoint_ip("10.0.0.2:8443"), Some("10.0.0.2".parse().unwrap()));
        assert_eq!(endpoint_ip("https://yak-01.lan:8443"), None);
        let mut h = test_host();
        h.endpoint = "https://10.0.0.2:8443".into();
        let (name, listen, connect) = nats_forward(&h, Some("10.150.0.20")).unwrap().unwrap();
        assert_eq!((name.as_str(), listen.as_str(), connect.as_str()), ("nats", "tcp:10.0.0.2:4222", "tcp:10.150.0.20:4222"));
        assert!(nats_forward(&h, None).unwrap().is_none());
        h.endpoint = "https://yak-01.lan:8443".into();
        assert!(nats_forward(&h, Some("10.150.0.20")).is_err());
    }

    #[test]
    fn env_values_survive_spaces_quotes_and_backslashes() {
        assert_eq!(env_line("K", "a b#c").unwrap(), "K=\"a b#c\"\n");
        assert_eq!(env_line("K", r#"p"q\r"#).unwrap(), "K=\"p\\\"q\\\\r\"\n");
        assert!(env_line("K", "line1\nline2").is_err());
    }

    #[test]
    fn a_slot_unit_is_named_what_runs_are_assigned_to() {
        let units = units(Role::Worker, "hw-yak-00-w1");
        let (path, body) = &units[0];
        assert_eq!(path, "/etc/systemd/system/huntwell-worker@.service");
        // SLOT_NAME must produce store::slot_name for each instance.
        assert!(body.contains("Environment=SLOT_NAME=hw-yak-00-w1-%i"), "{body}");
        assert_eq!(store::slot_name("hw-yak-00-w1", 3), "hw-yak-00-w1-3");
    }

    #[test]
    fn a_deploy_drains_worker_slots_instead_of_killing_plans() {
        let body = &units(Role::Worker, "w")[0].1;
        // mixed: SIGTERM to the supervisor only, which finishes its plan.
        assert!(body.contains("KillMode=mixed"), "{body}");
        assert!(body.contains(&format!("TimeoutStopSec={WORKER_DRAIN}")), "{body}");
        // And ten slots do not fight over one ops port.
        assert!(body.contains("HUNTWELL_WORKER_ADDR=127.0.0.1:0"), "{body}");
    }

    #[test]
    fn only_the_website_and_the_bus_listen_beyond_loopback() {
        let units = units(Role::App, "hw-app");
        let all: String = units.iter().map(|(_, b)| b.as_str()).collect();
        assert!(all.contains(&format!("HUNTWELL_ADDR=0.0.0.0:{WEBSITE_PORT}")));
        // The bus listens on the bridge, for the edge's forward to workers.
        assert!(nats_conf().contains("listen: 0.0.0.0:4222"));
        for loopback in ["HUNTWELL_PLANNING_ADDR=127.0.0.1", "HUNTWELL_SCHEDULING_ADDR=127.0.0.1",
                         "HUNTWELL_NOTIFICATION_ADDR=127.0.0.1"] {
            assert!(all.contains(loopback), "missing {loopback}");
        }
    }

    #[test]
    fn every_unit_can_write_only_its_data_directory() {
        for role in [Role::App, Role::Worker] {
            for (path, body) in units(role, "v") {
                assert!(body.contains("ProtectSystem=strict"), "{path}");
                assert!(body.contains(&format!("ReadWritePaths={DATA_DIR}")), "{path}");
                assert!(body.contains(&format!("User={USER}")), "{path}");
            }
        }
    }

    #[test]
    fn elf_headers_are_read_for_architecture() {
        let mut x86 = vec![0u8; 20];
        x86[..6].copy_from_slice(b"\x7fELF\x02\x01");
        x86[18..20].copy_from_slice(&0x3eu16.to_le_bytes());
        assert_eq!(elf_arch(&x86), Some("x86_64"));
        let mut arm = x86.clone();
        arm[18..20].copy_from_slice(&0xb7u16.to_le_bytes());
        assert_eq!(elf_arch(&arm), Some("aarch64"));
        // A Mach-O (a macOS build) is not a Linux executable at all.
        assert_eq!(elf_arch(b"\xcf\xfa\xed\xfe\x07\x00\x00\x01xxxxxxxxxxxx"), None);
    }

    #[test]
    fn a_missing_build_is_found_before_anything_is_created() {
        let dir = std::env::temp_dir().join(format!("hw-builds-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let err = find_builds(&dir, "x86_64", Role::Worker).err().expect("no build is an error");
        assert!(err.to_string().contains("no x86_64 `worker`"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_excerpts_lose_their_passwords() {
        let got = redact_urls("connect postgres://huntwell:Xk7pQ2@10.0.0.2:5432/huntwell failed");
        assert!(!got.contains("Xk7pQ2"), "{got}");
        assert!(got.contains("postgres://huntwell:***@10.0.0.2"), "{got}");
        assert_eq!(redact_urls("http://example.com/x"), "http://example.com/x");
    }

    #[test]
    fn the_edge_routes_the_domain_to_the_website() {
        let mut h = test_host();
        h.ingress_domain = "app.example.com".into();
        h.edge_scheme = "https".into();
        let file = caddyfile(&h, Some("10.150.0.20"));
        assert!(file.contains("app.example.com {"), "{file}");
        assert!(file.contains("reverse_proxy 10.150.0.20:8611"), "{file}");
        assert!(!file.contains("auto_https off"), "{file}");
        h.edge_scheme = "http".into();
        assert!(caddyfile(&h, Some("10.150.0.20")).contains("http://app.example.com"));
    }

    #[test]
    fn a_cloudflare_edge_publishes_nothing_and_serves_plain_http_to_the_tunnel() {
        let mut h = test_host();
        h.ingress_domain = "app.example.com".into();
        h.edge_scheme = "cloudflare".into();
        assert!(edge_listens(&h).is_empty());
        let file = caddyfile(&h, Some("10.150.0.20"));
        // Cloudflare terminates TLS; Caddy must not try to get a certificate.
        assert!(file.contains("auto_https off") && file.contains("http://app.example.com {"), "{file}");
        let rules = tunnel_ingress(Some("app.example.com"));
        assert_eq!(rules[0]["hostname"], "app.example.com");
        assert_eq!(rules[0]["service"], "http://127.0.0.1:80");
        assert_eq!(rules.as_array().unwrap().last().unwrap()["service"], "http_status:404");
        assert_eq!(tunnel_ingress(None).as_array().unwrap().len(), 1);
        h.edge_scheme = "https".into();
        assert_eq!(edge_listens(&h), vec![(443, 443), (80, 80)]);
        h.edge_scheme = "http".into();
        h.edge_port = 8080;
        assert_eq!(edge_listens(&h), vec![(8080, 80)]);
    }

    #[test]
    fn the_edge_takes_the_address_the_egress_table_admits() {
        // sys incus init admits gateway+1 to reach VMs. Anything else is refused.
        assert_eq!(edge_address("10.150.0.1".parse().unwrap()).to_string(), "10.150.0.2");
    }

    fn test_host() -> Host {
        Host {
            host_id: 1,
            name: "yak-00".into(),
            enabled: true,
            pool_size: 0,
            notes: String::new(),
            last_error: None,
            last_seen_at: None,
            created_at: chrono::Utc::now(),
            endpoint: "https://10.0.0.1:8443".into(),
            status: "Active".into(),
            base_image: "huntwell".into(),
            priority: 100,
            max_vms: 3,
            vm_cpu: 8,
            vm_memory: "16GiB".into(),
            vm_disk: "60GiB".into(),
            vm_slots: 10,
            ingress_domain: String::new(),
            edge_scheme: "https".into(),
            edge_port: 0,
            arch: "x86_64".into(),
            incus_version: String::new(),
            cpu_total: 0,
            memory_total_mb: 0,
        }
    }
}

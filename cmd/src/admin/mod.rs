//! The admin control plane: a separate server that manages bare-metal Incus
//! hosts, provisions the application's VMs on them, and routes queued runs to
//! worker slots (random / round-robin / pinned). See `docs/PRODUCTION.md`.
//!
//! It owns three background loops — host reconcile (Incus, parallel per
//! host), placement, and the heartbeat reaper — plus a small JSON API and an
//! embedded single-page dashboard. Operator auth is its own table
//! (`AdminUser`), seeded from `HUNTWELL_ADMIN_EMAIL` / `_PASSWORD`; it never
//! touches the product's `Account`s.

mod api;
mod hosts;
pub mod incus;
pub mod incus_driver;
mod placement;
mod ui;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::store::Db;

/// Three missed 5s beats. Longer than a brief NATS blip, short enough that a
/// dead process looks dead on the next dashboard poll.
pub const STALE_AFTER_SECS: i64 = 15;

/// Whether this process can run the process-backed local pool.
///
/// Only the all-in-one `huntwell` executable can: it spawns `current_exe()
/// worker-pool`, and it is the one binary that answers `worker-pool`. The
/// standalone `admin` service turns this off at startup. Without that, a
/// database carrying a local host row — a restored dev database, or one a
/// `huntwell admin` once ran against — would make the service fork copies of
/// itself every reconcile, each failing to bind the port the first one holds.
static LOCAL_POOL_SUPPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn disable_local_pool() {
    LOCAL_POOL_SUPPORTED.store(false, std::sync::atomic::Ordering::Relaxed);
}

pub fn local_pool_supported() -> bool {
    LOCAL_POOL_SUPPORTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// How long a sighting from a VM check stays fresh. Hosts are checked every 20
/// seconds, so a window shorter than a few checks would show a healthy service
/// flickering to stale between them.
pub const VM_CHECK_FRESH_SECS: i64 = 60;

/// One sighting of a service instance, and how long it stands for.
///
/// Two sources report at different rhythms. A process on the bus heartbeats
/// every few seconds; a service inside a VM is seen only when the reconcile
/// asks systemd, every 20. Judging both by one window would either call VM
/// services stale between checks or let a dead bus process look alive for a
/// minute — so each sighting carries its own.
#[derive(Debug, Clone, Copy)]
pub struct Ping {
    pub at: chrono::DateTime<chrono::Utc>,
    pub fresh_for: i64,
}

/// One worker slot as last seen on a host: a named process that claims the runs
/// addressed to it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SlotInfo {
    pub name: String,
    pub ready: bool,
    /// The node this slot is running on. Empty while it is still being
    /// scheduled, and on the process-backed local pool, which has no nodes.
    ///
    /// Shown because "the new machine is in the cluster" and "the new machine
    /// is doing work" are different facts, and only this one answers the
    /// second.
    pub node: String,
}

pub struct AdminState {
    pub db: Db,
    /// token → operator email. In-memory: restarting the admin logs out.
    pub sessions: Mutex<HashMap<String, String>>,
    /// host_id → slots from the last successful reconcile. Placement and the
    /// UI read this cache; Incus is never called on the request path.
    pub slots: Mutex<HashMap<i64, Vec<SlotInfo>>>,
    /// (host_id, slot name) → child for process-backed "local" hosts (dev mode:
    /// the pool runs as worker-pool child processes of the admin itself).
    pub local_slots: Mutex<HashMap<(i64, String), tokio::process::Child>>,
    /// (service, instance) → the last sighting: a bus heartbeat, or a unit
    /// systemd reported active inside a VM.
    pub pings: Mutex<HashMap<(String, String), Ping>>,
    /// VMs with a provision, deploy or start in flight. One at a time per VM:
    /// two deploys renaming the same executables can leave it running neither.
    pub busy: Mutex<std::collections::HashSet<i64>>,
}

pub type Admin = Arc<AdminState>;

/// The banner an unclaimed control plane prints. Boxed and spaced because it
/// has to be found in a scrolling log and read off a screen, sometimes from a
/// photograph of one.
fn first_run_banner(key: &str, addr: &str) -> String {
    let url = if addr.starts_with("0.0.0.0") || addr.starts_with("[::]") {
        format!("http://127.0.0.1:{}", addr.rsplit(':').next().unwrap_or("8710"))
    } else {
        format!("http://{addr}")
    };
    // Built by padding to a width rather than by hand-spacing each line: a
    // banner whose borders do not line up reads as something that went wrong,
    // and the URL and the key are both variable-length.
    let lines = [
        "This Huntwell control plane has no operator yet.".to_string(),
        String::new(),
        format!("Open {url}"),
        "and enter this setup key to create the first one:".to_string(),
        String::new(),
        format!("    {key}"),
        String::new(),
        "It lives only in this process: restarting mints a new".to_string(),
        "one, and it stops working once an operator exists.".to_string(),
    ];
    // Character count, not byte length — the box is drawn in box-drawing
    // characters and a URL may carry non-ASCII.
    let inner = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) + 4;
    let bar = "─".repeat(inner);
    let mut out = format!("┌{bar}┐\n");
    for l in lines {
        let pad = inner - 2 - l.chars().count();
        out.push_str(&format!("│  {l}{}│\n", " ".repeat(pad)));
    }
    out.push_str(&format!("└{bar}┘"));
    out
}

pub async fn serve(db: Db, addr: &str) -> Result<()> {
    // What this process is looking at. An operator's first question on a new
    // install is whether it found the right database, and the answer should not
    // require going and asking Postgres.
    match crate::store::database_facts(&db).await {
        Ok(f) => {
            tracing::info!("database  {} on {}", f.database, f.server);
            tracing::info!(
                "contents  {} tables · {} workspace(s) · {} plan(s) · {} host(s) · {} operator(s)",
                f.tables, f.accounts, f.plans, f.hosts, f.operators
            );
        }
        Err(e) => tracing::warn!("could not read the database: {e:#}"),
    }

    // Seeding from settings is still honoured, because a deployment that
    // already sets these should not break. It is no longer the only way in.
    match (crate::config::get("HUNTWELL_ADMIN_EMAIL"), crate::config::get("HUNTWELL_ADMIN_PASSWORD")) {
        (Some(email), Some(pass)) if !email.trim().is_empty() && !pass.trim().is_empty() => {
            let hash = crate::web::auth::hash_password(&pass)?;
            crate::store::upsert_admin_user(&db, &email, &hash).await?;
            tracing::info!("admin operator seeded from settings: {email}");
        }
        _ => {}
    }

    // With no operator there is nothing to sign in to, so the console would be
    // open to whoever reached it first. Mint a key and print it: holding it
    // means being able to read this output, which is the same access needed to
    // start the process at all.
    if crate::store::count_admin_users(&db).await.unwrap_or(0) == 0 {
        let key = crate::setup::arm();
        tracing::warn!("\n{}", first_run_banner(&key, addr));
    } else {
        crate::setup::disarm();
    }

    // Dev convenience: HUNTWELL_LOCAL_POOL=N registers a process-backed
    // "local" host (worker slots run as children of this admin — no VMs), so
    // the whole routing path works on a laptop.
    if let Some(n) = crate::config::get("HUNTWELL_LOCAL_POOL").and_then(|v| v.parse::<i32>().ok()) {
        let existing = crate::store::list_hosts(&db).await?.into_iter().any(|h| h.name == "local");
        if !existing && n > 0 {
            let h = crate::store::Host {
                host_id: 0,
                name: "local".into(),
                enabled: true,
                pool_size: n,
                notes: "process-backed dev pool, auto-registered by HUNTWELL_LOCAL_POOL".into(),
                last_error: None,
                last_seen_at: None,
                created_at: chrono::Utc::now(),
                // No endpoint is what makes it the local host (hosts::is_local).
                endpoint: String::new(),
                status: "Active".into(),
                base_image: String::new(),
                priority: 100,
                // It carries no VMs; its capacity is pool_size children.
                max_vms: 0,
                vm_cpu: 0,
                vm_memory: String::new(),
                vm_disk: String::new(),
                vm_slots: 0,
                ingress_domain: String::new(),
                edge_scheme: "http".into(),
                edge_port: 0,
                arch: String::new(),
                incus_version: String::new(),
                cpu_total: 0,
                memory_total_mb: 0,
            };
            let id = crate::store::create_host(&db, &h).await?;
            tracing::info!("registered local process pool as host #{id} ({n} workers)");
        }
    }

    let state: Admin = Arc::new(AdminState {
        db,
        sessions: Mutex::new(HashMap::new()),
        slots: Mutex::new(HashMap::new()),
        local_slots: Mutex::new(HashMap::new()),
        pings: Mutex::new(HashMap::new()),
        busy: Mutex::new(std::collections::HashSet::new()),
    });

    hosts::spawn_reconcile(state.clone());
    placement::spawn(state.clone());
    spawn_heartbeat_listener(state.clone());

    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("admin control plane on http://{addr}");
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;
    Ok(())
}

/// Keep last-ping in memory. Heartbeats are ephemeral: a restart of this
/// process empties the map and the next few beats refill it.
fn spawn_heartbeat_listener(state: Admin) {
    tokio::spawn(async move {
        let Some(mut sub) = crate::bus::subscribe(crate::bus::subject::SERVICE_HEARTBEAT).await else {
            tracing::info!("no bus — service last-ping will stay empty");
            return;
        };
        use futures_util::StreamExt;
        while let Some(msg) = sub.next().await {
            let Some(event) = crate::bus::decode(&msg) else { continue };
            let instance = event
                .data
                .get("instance")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            state.pings.lock().await.insert((event.source, instance), Ping { at: event.at, fresh_for: STALE_AFTER_SECS });
        }
        tracing::warn!("service heartbeat subscription ended");
    });
}

/// Fleet health from the pings we have heard. Pure so a test can pin the clock.
pub fn health_snapshot(
    pings: &HashMap<(String, String), Ping>,
    now: chrono::DateTime<chrono::Utc>,
    stale_after_secs: i64,
) -> Value {
    let mut names: Vec<String> = crate::svc::SERVICES.iter().map(|s| (*s).to_string()).collect();
    for (svc, _) in pings.keys() {
        if !names.iter().any(|n| n == svc) {
            names.push(svc.clone());
        }
    }
    let services: Vec<Value> = names
        .iter()
        .map(|name| {
            let mut instances: Vec<Value> = pings
                .iter()
                .filter(|((svc, _), _)| svc == name)
                .map(|((_, instance), ping)| {
                    let age = (now - ping.at).num_seconds().max(0);
                    let status = if age <= ping.fresh_for { "ok" } else { "stale" };
                    json!({
                        "id": instance,
                        "last_ping": ping.at,
                        "age_seconds": age,
                        "status": status,
                    })
                })
                .collect();
            instances.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            let expected = crate::svc::SERVICES.contains(&name.as_str());
            let status = if instances.iter().any(|i| i["status"] == "ok") {
                "ok"
            } else if !instances.is_empty() {
                "stale"
            } else {
                "missing"
            };
            json!({
                "name": name,
                "expected": expected,
                "status": status,
                "instances": instances,
            })
        })
        .collect();
    json!({
        "bus": { "connected": crate::bus::connected() },
        "stale_after_seconds": stale_after_secs,
        "services": services,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn ts(secs: i64) -> chrono::DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn expected_services_show_as_missing_until_a_ping() {
        let snap = health_snapshot(&HashMap::new(), ts(1_000), STALE_AFTER_SECS);
        let services = snap["services"].as_array().unwrap();
        assert_eq!(services.len(), crate::svc::SERVICES.len());
        assert!(services.iter().all(|s| s["status"] == "missing"));
        assert!(services.iter().all(|s| s["expected"] == true));
    }

    #[test]
    fn a_fresh_ping_is_ok_and_a_quiet_one_is_stale() {
        let now = ts(1_000);
        let mut pings = HashMap::new();
        let bus = |at| Ping { at, fresh_for: STALE_AFTER_SECS };
        pings.insert(("website".into(), "web-a".into()), bus(ts(995)));
        pings.insert(("admin".into(), "adm-a".into()), bus(ts(980)));
        let snap = health_snapshot(&pings, now, STALE_AFTER_SECS);
        let find = |name: &str| {
            snap["services"].as_array().unwrap().iter().find(|s| s["name"] == name).cloned().unwrap()
        };
        assert_eq!(find("website")["status"], "ok");
        assert_eq!(find("website")["instances"][0]["age_seconds"], 5);
        assert_eq!(find("admin")["status"], "stale");
        assert_eq!(find("planning")["status"], "missing");
    }

    #[test]
    fn a_vm_sighting_stays_fresh_between_checks_but_a_bus_one_does_not() {
        // Seen 40s ago. From the bus that is a dead process; from a VM check
        // run every 20s it is simply the last check but one.
        let now = ts(1_000);
        let mut pings = HashMap::new();
        pings.insert(("website".into(), "hw-app".into()), Ping { at: ts(960), fresh_for: VM_CHECK_FRESH_SECS });
        pings.insert(("planning".into(), "laptop:42".into()), Ping { at: ts(960), fresh_for: STALE_AFTER_SECS });
        let snap = health_snapshot(&pings, now, STALE_AFTER_SECS);
        let find = |name: &str| {
            snap["services"].as_array().unwrap().iter().find(|s| s["name"] == name).cloned().unwrap()
        };
        assert_eq!(find("website")["status"], "ok");
        assert_eq!(find("planning")["status"], "stale");
    }

    #[test]
    fn an_unknown_service_still_appears() {
        let mut pings = HashMap::new();
        pings.insert(("edge".into(), "e1".into()), Ping { at: ts(1_000), fresh_for: STALE_AFTER_SECS });
        let snap = health_snapshot(&pings, ts(1_000), STALE_AFTER_SECS);
        let extra = snap["services"].as_array().unwrap().iter().find(|s| s["name"] == "edge").unwrap();
        assert_eq!(extra["expected"], false);
        assert_eq!(extra["status"], "ok");
    }
}

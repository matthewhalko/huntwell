//! The HTTP surface every service has, whatever else it does.
//!
//! Four of the six are queue readers that nothing calls, and it would be easy to
//! argue they need no server at all. They do:
//!
//! - A deploy has to know when a service is up, and the only honest way to
//!   know is to ask it. A service with no probe is one nobody can tell is wedged.
//! - "Is the bus connected?" is the question you actually have when events stop
//!   arriving, and it should be answerable without reading a log.
//! - A service that already serves HTTP can be given a real endpoint later
//!   without first being given a server.
//!
//! `/healthz` is liveness: the process is up. `/statusz` is what it is doing.

use std::sync::OnceLock;

use anyhow::{Context, Result};
use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

static NAME: OnceLock<String> = OnceLock::new();
static STARTED: OnceLock<chrono::DateTime<chrono::Utc>> = OnceLock::new();

/// Extra facts a service wants in `/statusz`, e.g. how many drafts it holds.
type Extra = fn() -> Value;
static EXTRA: OnceLock<Extra> = OnceLock::new();

/// The six long-lived roles. Admin health lists these even before a ping
/// arrives, so a missing process is visible rather than invisible.
pub const SERVICES: &[&str] = &["website", "admin", "planning", "worker", "scheduling", "notification"];

/// The default address for a service, `HUNTWELL_<NAME>_ADDR` if set.
///
/// Each gets its own port so they can all run on one dev box or one VM; a
/// collision there looks like a service that will not start.
pub fn addr_for(service: &str) -> String {
    let key = format!("HUNTWELL_{}_ADDR", service.to_uppercase());
    if let Some(a) = crate::config::get(&key).filter(|a| !a.trim().is_empty()) {
        return a;
    }
    let port = match service {
        "website" => 8611,
        "admin" => 8710,
        "planning" => 8612,
        "worker" => 8613,
        "scheduling" => 8614,
        "notification" => 8615,
        _ => 8619,
    };
    format!("0.0.0.0:{port}")
}

/// Start the ops server in the background. Returns once it is listening, so a
/// failure to bind is reported at startup rather than discovered by a probe.
pub async fn spawn(service: &str, extra: Option<Extra>) -> Result<()> {
    spawn_with(service, extra, Router::new()).await
}

/// The same, plus this service's own endpoints.
pub async fn spawn_with(service: &str, extra: Option<Extra>, routes: Router) -> Result<()> {
    let _ = NAME.set(service.to_string());
    let _ = STARTED.set(chrono::Utc::now());
    if let Some(e) = extra {
        let _ = EXTRA.set(e);
    }
    let addr = addr_for(service);
    let app = routes
        .route("/healthz", get(|| async { "ok" }))
        .route("/statusz", get(statusz));
    let listener = tokio::net::TcpListener::bind(&addr).await.with_context(|| format!("bind {addr}"))?;
    // The bound address, not the requested one: a worker slot asks for port 0
    // so ten slots in one VM each get their own, and "127.0.0.1:0" in the log
    // would tell an operator nothing.
    let bound = listener.local_addr().map(|a| a.to_string()).unwrap_or(addr);
    tracing::info!("{service}: http on http://{bound}");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("ops server stopped: {e:#}");
        }
    });
    Ok(())
}

async fn statusz() -> Json<Value> {
    let started = STARTED.get().copied().unwrap_or_else(chrono::Utc::now);
    let mut v = json!({
        "service": NAME.get().cloned().unwrap_or_else(|| "unknown".into()),
        "started_at": started,
        "uptime_seconds": (chrono::Utc::now() - started).num_seconds(),
        // The question you have when events stop arriving.
        "bus": { "connected": crate::bus::connected() },
    });
    if let Some(extra) = EXTRA.get() {
        if let Some(obj) = extra().as_object() {
            for (k, val) in obj {
                v[k] = val.clone();
            }
        }
    }
    Json(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_service_gets_its_own_port() {
        let ports: Vec<String> = SERVICES.iter().map(|s| addr_for(s)).collect();
        let mut seen = ports.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), ports.len(), "two services share a port: {ports:?}");
    }

    #[test]
    fn an_explicit_address_wins() {
        std::env::set_var("HUNTWELL_PLANNING_ADDR", "127.0.0.1:9999");
        assert_eq!(addr_for("planning"), "127.0.0.1:9999");
        std::env::remove_var("HUNTWELL_PLANNING_ADDR");
    }
}

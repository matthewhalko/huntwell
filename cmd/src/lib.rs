//! Huntwell — hosted prospecting plans.
//!
//! Every module lives here so the service executables can share them. The
//! binaries in `src/bin/` are thin: each one boots a runtime, connects, and
//! calls into this library. What separates them is which of these concerns
//! they own, and therefore what has to be installed alongside them.
//!
//!   admin         hosts, warm slot pools, placement, run routing
//!   website       the UI and the whole public API
//!   planning      drafting plans and plan chat        — needs the agent CLI
//!   worker        executing plans                     — needs the agent CLI
//!   scheduling    firing due plans; a singleton
//!   notification  outbound mail
//!
//! `huntwell` remains the operator multi-tool: migrate, doctor, config,
//! account and the agent's own MCP server.

pub mod agent;
pub mod artifact;
pub mod aws;
pub mod assets;
pub mod browser;
pub mod browserbase;
pub mod bus;
pub mod cidr;
pub mod cloudflare;
pub mod cognito;
pub mod config;
pub mod csv;
pub mod genesis;
pub mod guard;
pub mod identity;
pub mod mcp;
pub mod mail;
pub mod meter;
pub mod model_catalog;
pub mod normalize;
pub mod notification;
pub mod objstore;
pub mod pipeline;
pub mod plan_chat;
pub mod planning;
pub mod progress;
pub mod report;
pub mod prospect;
pub mod sandbox;
pub mod setup;
pub mod store;
pub mod svc;
pub mod throttle;
pub mod turnstile;
pub mod trail;
pub mod web;
pub mod worker_pool;
pub mod admin;

use sha1::{Digest, Sha1};

pub fn sha1_hex(s: &str) -> String {
    hex::encode(Sha1::digest(s.as_bytes()))
}

/// One tracing setup for every executable, so they all honour RUST_LOG the
/// same way and default to the same level.
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "huntwell=info,tower_http=warn".into()),
        )
        .init();
}

/// The boot every service binary shares: settings from `global` before
/// anything reads the environment, logging, and a line saying which settings
/// file was used — on a server that file is the whole configuration, and a
/// missing or shadowed one is otherwise invisible until something fails.
pub fn boot(service: &str) {
    config::export_to_env();
    init_tracing();
    match (config::source_file(), config::source_error()) {
        (Some(_), Some(e)) => tracing::error!("{service}: {e}"),
        (Some(p), None) => tracing::info!("{service}: settings from {}", p.display()),
        (None, _) => tracing::warn!("{service}: no global settings file found — using the process environment only"),
    }
}

/// The async half of the boot: join the event bus. Separate from [`boot`] only
/// because that one runs before a runtime exists.
///
/// Every service calls this, including the ones that publish and never listen —
/// a service with no bus connection cannot be told to react to anything later
/// without a restart, and the connection costs nothing when idle.
pub async fn boot_bus(service: &str) {
    // Before the bus, because the bus URL can itself be a secret. Before
    // anything reads a setting, for the same reason.
    config::load_remote_secrets().await;
    // Re-export, so a child process (a run, the agent's MCP server) inherits
    // what came back from Secrets Manager and not just what was on disk.
    config::export_to_env();
    bus::connect(service).await;
}

//! The Huntwell scheduling service: fires plans when their slot comes round.
//!
//! A singleton, and it enforces that itself with a Postgres advisory lock
//! rather than trusting the deployment to run one replica. Start a second one
//! and it becomes a warm standby: it cannot take the lock, waits, and takes
//! over within seconds if the holder dies.
//!
//! It serves no HTTP. Nothing calls it — it reads the database on a timer.

use anyhow::Result;
use huntwell::store;
use huntwell::web::{scheduler, App, AppState};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

#[tokio::main]
async fn main() {
    huntwell::boot("scheduling");
    huntwell::boot_bus("scheduling").await;
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    // Small pool: one connection is pinned for the whole process to hold the
    // advisory lock, and the tick itself is a handful of queries every 20s.
    let db = store::connect(&huntwell::config::service_database_url()?, 4).await?;

    // Slots missed while nothing was running would otherwise fire in a burst
    // the moment this starts.
    let n = store::refresh_all_schedules(&db).await?;
    tracing::info!("recomputed {n} schedule(s)");

    // Firing goes through the same runner the website uses, which needs the
    // full App. The parts that only mean something to an HTTP server — the
    // log broadcast, dev mode — are inert here.
    let (log_tx, _) = broadcast::channel(64);
    let state: App = Arc::new(AppState {
        db,
        active: Mutex::new(HashMap::new()),
        start_gate: Mutex::new(()),
        log_tx,
        dev: false,
        open_signup: false,
    });

    huntwell::svc::spawn("scheduling", None).await?;
    scheduler::run(state).await
}

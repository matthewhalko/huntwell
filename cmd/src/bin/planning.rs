//! The Huntwell planning service: drafts plans the website queues.
//!
//! Serves no HTTP. It claims work from the database, runs the drafting agent,
//! and writes the plan back — the same shape as a worker, for the same reason:
//! drafting takes tens of seconds and needs the agent CLI, and neither belongs
//! on a request the user is waiting on.
//!
//! Safe to run several. The claim is an atomic `queued` -> `drafting` UPDATE,
//! so two replicas cannot draft the same plan.

use anyhow::Result;
use huntwell::{config, planning, store};

#[tokio::main]
async fn main() {
    huntwell::boot("planning");
    huntwell::boot_bus("planning").await;
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let db = store::connect(&config::service_database_url()?, 4).await?;
    huntwell::svc::spawn_with("planning", Some(planning::status), planning::routes(db.clone())).await?;
    planning::serve(db).await
}

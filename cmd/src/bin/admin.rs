//! The Huntwell admin control plane.
//!
//! Registers k3s/k3d hosts, keeps a warm `hw-pool` StatefulSet on each, and
//! assigns queued executions to pods. Runs on its own server, reaches the
//! hosts through kubectl, and reaches nothing else through HTTP.
//!
//! Needs `kubectl` on PATH. Bind it to loopback and reach it over a tunnel:
//! an operator here can see every workspace's routing and kill any pod.

use anyhow::Result;
use huntwell::{admin, config, store};

#[tokio::main]
async fn main() {
    huntwell::boot("admin");
    huntwell::boot_bus("admin").await;
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    // HUNTWELL_LOCAL_POOL spawns `current_exe() worker-pool` children — a dev
    // convenience that only makes sense in the all-in-one binary, which has the
    // agent beside it. This executable does not answer `worker-pool`, so left
    // unchecked it would fork copies of the control plane.
    if config::get("HUNTWELL_LOCAL_POOL").is_some_and(|v| v.trim() != "" && v.trim() != "0") {
        anyhow::bail!(
            "HUNTWELL_LOCAL_POOL runs worker pods as child processes of the admin, which the \
             admin service cannot do. Use `huntwell admin` for that (dev.sh does), or register \
             a host and let it run real worker pods."
        );
    }
    let addr = config::get_or("HUNTWELL_ADMIN_ADDR", "127.0.0.1:8710");
    // The control plane starts first on a fresh server, so it is the one that
    // creates the database if Postgres is still empty.
    let db = store::connect_or_create(&config::service_database_url()?, 8).await?;
    store::migrate(&db).await?;
    admin::serve(db, &addr).await
}

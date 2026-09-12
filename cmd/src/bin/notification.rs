//! The Huntwell notification service: drains the mail outbox.
//!
//! Serves no HTTP and is called by nothing. Everything that wants to send mail
//! writes a `MailOutbox` row and returns; this is the only process that talks
//! to the mail provider, so a slow or failing provider costs a retry here
//! instead of a user's request.

use anyhow::Result;
use huntwell::{config, notification, store};

#[tokio::main]
async fn main() {
    huntwell::boot("notification");
    huntwell::boot_bus("notification").await;
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let db = store::connect(&config::service_database_url()?, 4).await?;
    // Two jobs: react to what other services publish, and drain what that (and
    // everything else) queues.
    huntwell::svc::spawn("notification", None).await?;
    notification::listen(db.clone());
    notification::serve(db).await
}

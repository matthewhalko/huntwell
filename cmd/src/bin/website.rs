//! The Huntwell public website: the embedded UI and the whole `/api` surface.
//!
//! Runs on every node, behind the edge load balancer. Nothing periodic lives
//! here — the scheduling service fires due plans, and the notification service
//! sends mail — because anything on a timer in this process would run once per
//! node.
//!
//! It does not draft plans either: that is the planning service, which is why
//! this image needs no agent CLI and stays small.

use anyhow::Result;
use huntwell::{config, store, web};

#[tokio::main]
async fn main() {
    huntwell::boot("website");
    // Two subcommands, both of which exit rather than serve. They live here
    // rather than in a binary of their own so production has one image to pull:
    // the operator multi-tool is a development tool and is not deployed.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // The migrate Job's entrypoint.
        Some("migrate") => std::process::exit(finish(migrate().await)),
        // The way in when signups are closed, which they are in production.
        // Without it the only route to a first account is turning open signup
        // on, using it, and turning it back off — a window during which anyone
        // who knows the URL can register.
        Some("create-account") => {
            let email = flag(&args, "--email").unwrap_or_default().to_string();
            let password = flag(&args, "--password").unwrap_or_default().to_string();
            let name = flag(&args, "--name").unwrap_or("").to_string();
            std::process::exit(finish(create_account(&email, &password, &name).await));
        }
        Some(other) if other.starts_with('-') || !other.is_empty() => {
            eprintln!("error: unknown argument {other:?} — website takes no command, or `migrate` / `create-account`");
            std::process::exit(2);
        }
        _ => {}
    }
    huntwell::boot_bus("website").await;
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn finish(r: Result<()>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

/// `--name value`, the shape these subcommands use.
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(String::as_str)
}

/// Make an account from the command line. Validated exactly as a signup is —
/// the password rules are not something to skip because an operator typed it.
async fn create_account(email: &str, password: &str, name: &str) -> Result<()> {
    if email.trim().is_empty() || password.is_empty() {
        anyhow::bail!("create-account needs --email and --password (and optionally --name)");
    }
    huntwell::web::auth::validate_signup(email, password)?;
    // Through the identity store, like a signup: on a Cognito installation the
    // operator's account has to exist in the pool too, or it is a row that can
    // never sign in.
    let id = huntwell::identity::create_user(email, password).await?;
    let db = store::connect(&config::service_database_url()?, 2).await?;
    let acc = store::create_account(&db, email, name, &id).await?;
    println!(
        "created account #{} {} ({} identity)",
        acc.account_id,
        acc.email,
        huntwell::identity::provider_name()
    );
    Ok(())
}

async fn migrate() -> Result<()> {
    // Creating it if absent, so a fresh cluster needs nothing done by hand. The
    // schema itself is serialised by an advisory lock, so several services
    // starting at once cannot collide.
    let db = store::connect_or_create(&config::service_database_url()?, 2).await?;
    store::migrate(&db).await?;
    tracing::info!("schema applied");
    Ok(())
}

async fn run() -> Result<()> {
    // RUN_DISPATCH=local forks `current_exe() run --execution-id N` per run.
    // This executable does not answer `run` — and even if it did, the website
    // image carries no agent CLI, so the child would fail on its first call.
    // Said here, once, rather than as a mystery at the first run.
    if huntwell::web::dispatch::mode() == huntwell::web::dispatch::Mode::Local {
        anyhow::bail!(
            "RUN_DISPATCH=local runs plans as child processes of the web server, which the \
             website service cannot do — it has no agent. Use RUN_DISPATCH=pool with the admin \
             control plane and worker pods, or run the all-in-one `huntwell serve` for local dev."
        );
    }
    let addr = config::get_or("HUNTWELL_ADDR", "0.0.0.0:8611");
    let db = store::connect(&config::service_database_url()?, 16).await?;
    store::migrate(&db).await?;
    web::serve_website(db, &addr).await
}

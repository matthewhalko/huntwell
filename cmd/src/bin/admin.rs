//! The Huntwell admin control plane.
//!
//! Registers bare-metal Incus hosts, provisions the application's VMs on
//! them — one app VM, and worker VMs of N plan slots — and assigns queued
//! executions to worker slots. Runs on its own server and reaches the hosts
//! through the Incus API.
//!
//! ```text
//! admin serve [--addr ADDR]      run the control plane
//! admin config get NAME          what a setting resolves to
//! admin config set NAME VALUE    set an operator setting (empty VALUE clears it)
//! admin config list              every setting name, and where it comes from
//! ```
//!
//! Nothing is configured through the environment. Like Park River, settings
//! come from the sealed `global` beside this executable (KEY and SECRET), then
//! the Secrets Manager secret they open (the database and every credential),
//! then the `setting` table — `admin config set`.
//!
//! The address is a parameter, as in Park River: `admin serve --addr
//! 10.121.17.195:8710`. Without one, HUNTWELL_ADMIN_ADDR from the setting table,
//! then 127.0.0.1:8710.
//!
//! Needs the `incus` client on PATH. Bind it to loopback or a private network
//! and reach it over a tunnel: an operator here can see every workspace's
//! routing, kill any slot, and delete any VM.

use anyhow::{bail, Result};
use huntwell::{admin, config, store};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("config") {
        // Quietly: `admin config get NAME` is meant to be captured in a shell
        // substitution, and a log line would land in the captured value.
        huntwell::config::export_to_env();
    } else {
        huntwell::boot("admin");
    }
    let outcome = match args.first().map(String::as_str) {
        Some("serve") => {
            huntwell::boot_bus("admin").await;
            serve(&args[1..]).await
        }
        Some("config") => config_cmd(&args[1..]).await,
        None | Some("-h") | Some("--help") | Some("help") => {
            usage();
            Ok(())
        }
        Some(other) => {
            usage();
            Err(anyhow::anyhow!("unknown command {other:?}"))
        }
    };
    if let Err(e) = outcome {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn usage() {
    eprintln!(
        "admin serve [--addr ADDR]      run the control plane (default 127.0.0.1:8710)\n\
         admin config get NAME          what a setting resolves to\n\
         admin config set NAME VALUE    set an operator setting (empty VALUE clears it)\n\
         admin config list              every setting name, and where it comes from"
    );
}

async fn serve(args: &[String]) -> Result<()> {
    // Parsed before anything connects, so a mistyped address fails at once.
    let flag = config::parse_serve_addr(args)?;
    // HUNTWELL_LOCAL_POOL spawns `current_exe() worker-pool` children — a dev
    // convenience that only makes sense in the all-in-one binary, which has the
    // agent beside it. This executable does not answer `worker-pool`, so left
    // unchecked it would fork copies of the control plane.
    if config::get("HUNTWELL_LOCAL_POOL").is_some_and(|v| v.trim() != "" && v.trim() != "0") {
        bail!(
            "HUNTWELL_LOCAL_POOL runs worker slots as child processes of the admin, which the \
             admin service cannot do. Use `huntwell admin` for that (dev.sh does), or register \
             a host and let it run real worker slots."
        );
    }
    // And never fork one either, whatever the database says: this executable
    // does not answer `worker-pool`, so a child would be another control plane.
    admin::disable_local_pool();
    // The control plane starts first on a fresh server, so it is the one that
    // creates the database if Postgres is still empty.
    let db = store::connect_or_create(&config::service_database_url()?, 8).await?;
    store::migrate(&db).await?;
    // --addr wins. Otherwise the setting table — readable only now the
    // database is open — then loopback.
    let addr = match flag {
        Some(a) => a.to_string(),
        None => config::get_or("HUNTWELL_ADMIN_ADDR", "127.0.0.1:8710"),
    };
    admin::serve(db, &addr).await
}

async fn config_cmd(args: &[String]) -> Result<()> {
    // Every subcommand needs the full chain: Secrets Manager holds the database
    // the setting table lives in.
    config::load_remote_secrets().await;
    match args.first().map(String::as_str) {
        Some("get") => {
            let Some(name) = args.get(1) else { bail!("usage: admin config get NAME") };
            open_database().await?;
            match config::get(name) {
                Some(v) if config::is_credential_name(name) => {
                    // Confirm it is set without printing it into a terminal's
                    // scrollback and whatever that gets pasted into.
                    println!("(set — {} characters; credentials are not printed)", v.chars().count());
                    Ok(())
                }
                Some(v) => {
                    println!("{v}");
                    Ok(())
                }
                None => bail!("{name} is not set in the setting table, Secrets Manager, or the global file"),
            }
        }
        Some("set") => {
            let (Some(name), Some(value)) = (args.get(1), args.get(2)) else {
                bail!("usage: admin config set NAME VALUE   (an empty VALUE clears it)");
            };
            let db = open_database().await?;
            store::set_config_setting(&db, name, value).await?;
            if value.trim().is_empty() {
                println!("cleared {name}");
            } else {
                println!("set {name}");
            }
            println!("A running admin picks this up on its next restart; VMs get it on their next Deploy.");
            Ok(())
        }
        Some("list") => {
            println!("secret: {}", config::secret_id());
            let remote = config::remote_setting_names();
            println!("from Secrets Manager ({} — names only):", remote.len());
            if remote.is_empty() {
                println!("  (none — the store is empty or unreachable; is `global` sealed with KEY and SECRET?)");
            }
            for n in &remote {
                println!("  {n}");
            }
            match open_database().await {
                Ok(_) => {
                    let rows = config::db_settings();
                    println!("from the setting table ({}):", rows.len());
                    for (k, v) in rows {
                        println!("  {k} = {v}");
                    }
                }
                Err(e) => println!("from the setting table: unreachable ({e:#})"),
            }
            Ok(())
        }
        _ => bail!("usage: admin config get NAME | set NAME VALUE | list"),
    }
}

/// Open the database and apply the schema, so `config set` works on a fresh
/// install before the control plane has ever started.
async fn open_database() -> Result<store::Db> {
    let db = store::connect_or_create(&config::service_database_url()?, 2).await?;
    store::migrate(&db).await?;
    Ok(db)
}

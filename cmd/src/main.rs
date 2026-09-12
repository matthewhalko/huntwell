//! huntwell — the Huntwell operator multi-tool.
//!
//! The services are their own executables (see `src/bin/`); this binary is
//! everything an operator runs by hand, plus the entrypoints k8s Jobs use.
//!
//!   huntwell serve                    the web app (API + embedded UI + scheduler)
//!   huntwell run --execution-id N           one run, spawned by `serve` (or by hand)
//!   huntwell mcp-prospects --plan-id  the agent's read-only plan-memory server
//!   huntwell account create …         make an account from the shell
//!   huntwell doctor                   check the agent, Chrome, database
//!   huntwell config get KEY           print a resolved setting

use anyhow::Result;
use clap::{Parser, Subcommand};

use huntwell::{admin, agent, browser, browserbase, config, mcp, objstore, pipeline, store, web, worker_pool};

#[derive(Parser)]
#[command(name = "huntwell", version, about = "Hosted prospecting plans")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the web application.
    Serve {
        /// host:port to listen on (default HUNTWELL_ADDR or 127.0.0.1:8611)
        #[arg(long)]
        addr: Option<String>,
        /// Development mode: relaxed cookies, permissive CORS for the Vite dev server.
        #[arg(long)]
        dev: bool,
    },
    /// Execute one queued run. Normally spawned by `serve`.
    Run {
        #[arg(long)]
        execution_id: i64,
    },
    /// Execute one queued run — the Kubernetes Job entrypoint. Same as `run`.
    RunWorker {
        #[arg(long)]
        execution_id: i64,
    },
    /// Warm-pool worker: claims runs assigned to this pod and executes them.
    /// The StatefulSet pod entrypoint (needs HOST_ID + POD_NAME).
    WorkerPool,
    /// The admin control plane: manages k3d hosts, worker pod pools, and run
    /// routing. Deployed on its own server, talks to the core database.
    Admin {
        /// host:port to listen on (default HUNTWELL_ADMIN_ADDR or 127.0.0.1:8710)
        #[arg(long)]
        addr: Option<String>,
    },
    /// Apply a schema slice to DATABASE_URL. The k3d migrate Jobs' entrypoint.
    Migrate {
        /// all | auth | core
        #[arg(long, default_value = "all")]
        schema: String,
    },
    /// The read-only MCP server the scraping agent is given. Started by the agent.
    #[command(hide = true)]
    McpProspects {
        #[arg(long)]
        plan_id: i64,
    },
    /// Manage accounts from the shell.
    Account {
        #[command(subcommand)]
        cmd: AccountCmd,
    },
    /// Check the agent CLI, Chrome and the database.
    Doctor,
    /// Print a resolved setting (environment, then local-infra/global).
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Seal the `global` file, or one value in it, with the genesis key.
    Genesis {
        #[command(subcommand)]
        cmd: GenesisCmd,
    },
}

#[derive(Subcommand)]
enum GenesisCmd {
    /// Write a new key file. Refuses to overwrite one that exists.
    Keygen {
        /// Where to write it (default: the genesis path this binary reads).
        #[arg(long)]
        path: Option<String>,
    },
    /// Seal one value, for pasting into `global` as NAME=enc:v1:…
    Seal {
        /// The setting name — bound into the ciphertext, so it cannot be
        /// pasted over a different setting.
        name: String,
        /// The value. Omit to read it from stdin, which keeps it out of shell
        /// history.
        value: Option<String>,
    },
    /// Seal a whole file, so not even the setting names are readable.
    SealFile {
        /// The plaintext file (default: the `global` this binary reads).
        #[arg(long)]
        path: Option<String>,
        /// Write the sealed text here instead of stdout.
        #[arg(long)]
        out: Option<String>,
    },
    /// Print what this binary can see: key, file, and whether it opens.
    Show,
}

#[derive(Subcommand)]
enum AccountCmd {
    /// Create an account.
    Create {
        #[arg(long)]
        email: String,
        #[arg(long)]
        password: String,
        #[arg(long, default_value = "")]
        name: String,
    },
    /// List accounts.
    List,
}

#[derive(Subcommand)]
enum ConfigCmd {
    Get { key: String },
    List,
}

fn main() {
    // Settings first, so the file is in place before anything reads env.
    config::export_to_env();
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::McpProspects { plan_id } => mcp_cmd(plan_id),
        other => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async_main(other))
        }
    };
    std::process::exit(code);
}

fn mcp_cmd(plan_id: i64) -> i32 {
    let url = match config::database_url() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("error: {e:#}");
            return 2;
        }
    };
    match mcp::Server::open(&url, plan_id).and_then(|s| s.serve_stdio()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

/// `huntwell genesis …` — producing the sealed `global` a production box reads.
///
/// Sealing happens here rather than with `openssl` so the format, the key
/// location and the AAD binding all come from one place: a file sealed by this
/// command is by construction one this binary can open, which `seal-file`
/// proves by opening it again before writing anything.
fn genesis_cmd(cmd: GenesisCmd) -> Result<i32> {
    use huntwell::genesis;

    match cmd {
        GenesisCmd::Keygen { path } => {
            let path = path.map(std::path::PathBuf::from).unwrap_or_else(genesis::key_path);
            if path.exists() {
                // Overwriting silently would make every existing sealed value
                // unopenable, with no way back.
                anyhow::bail!(
                    "{} already exists — refusing to overwrite it.\n  \
                     Every value sealed with the current key would become unreadable.\n  \
                     Move it aside first if you really mean to rotate.",
                    path.display()
                );
            }
            let key = genesis::generate_key().map_err(anyhow::Error::msg)?;
            std::fs::write(
                &path,
                format!(
                    "# Huntwell genesis key — AES-256 for the sealed `global` file.\n\
                     # Keep it off the machine that holds `global` wherever you can:\n\
                     # together they are the credential, apart they are nothing.\n{}\n",
                    &*key
                ),
            )?;
            // Before anyone else can read it.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
            println!("wrote {}", path.display());
            println!("Back it up now — without it a sealed `global` cannot be opened.");
            Ok(0)
        }

        GenesisCmd::Seal { name, value } => {
            let (key, source) = genesis::key().map_err(anyhow::Error::msg)?;
            let plaintext = match value {
                Some(v) => v,
                None => {
                    use std::io::Read;
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf.trim_end_matches('\n').to_string()
                }
            };
            let sealed = genesis::encrypt_with(&key, &name, &plaintext).map_err(anyhow::Error::msg)?;
            // Proven before it is offered: a value this binary cannot open is
            // one the server will not open either.
            genesis::decrypt_with(&key, &name, &sealed).map_err(anyhow::Error::msg)?;
            eprintln!("sealed with the key in {source}");
            println!("{name}={sealed}");
            Ok(0)
        }

        GenesisCmd::SealFile { path, out } => {
            let path = path
                .map(std::path::PathBuf::from)
                .or_else(|| config::source_file().map(|p| p.to_path_buf()))
                .ok_or_else(|| anyhow::anyhow!("no global file found — pass --path"))?;
            let raw = std::fs::read_to_string(&path)?;
            if genesis::is_sealed_file(&raw) {
                anyhow::bail!("{} is already sealed", path.display());
            }
            let (key, source) = genesis::key().map_err(anyhow::Error::msg)?;
            let sealed = genesis::seal_file(&key, &raw).map_err(anyhow::Error::msg)?;
            // Round-tripped before anything is written: sealing with the wrong
            // environment's key produces a file that looks fine and opens
            // nowhere.
            let back = genesis::unseal_file(&key, &sealed).map_err(anyhow::Error::msg)?;
            anyhow::ensure!(back == raw, "the sealed file did not round-trip — refusing to write it");

            let body = format!("# Huntwell global, sealed. Opened with the genesis key.\n{sealed}\n");
            match out {
                Some(o) => {
                    std::fs::write(&o, &body)?;
                    eprintln!("sealed with the key in {source}");
                    println!("wrote {o}");
                }
                None => print!("{body}"),
            }
            Ok(0)
        }

        GenesisCmd::Show => {
            let key_path = genesis::key_path();
            println!("  key        {} ({})", key_path.display(), if genesis::have_key() { "present" } else { "not found" });
            match config::source_file() {
                Some(p) => {
                    let raw = std::fs::read_to_string(p).unwrap_or_default();
                    println!("  global     {}", p.display());
                    println!("  sealed     {}", if genesis::is_sealed_file(&raw) { "yes" } else { "no" });
                    // `source_file` only reports a file the loader opened, so
                    // reaching here at all means it opened.
                    println!("  opens      yes");
                }
                None => println!("  global     none found"),
            }
            Ok(0)
        }
    }
}

async fn async_main(cmd: Cmd) -> i32 {
    match dispatch(cmd).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

async fn dispatch(cmd: Cmd) -> Result<i32> {
    match cmd {
        Cmd::Serve { addr, dev } => {
            huntwell::init_tracing();
            if dev {
                std::env::set_var("HUNTWELL_DEV", "1");
            }
            huntwell::boot_bus("website").await;
            let db = store::connect(&config::database_url()?, 16).await?;
            store::migrate(&db).await?;
            // Drafting happens in this process, so it needs the admin's model
            // choices. Changing them takes effect on the next restart here;
            // runs pick them up per run, since each is its own process.
            agent::set_stage_models(store::stage_models(&db).await);
            let addr = addr.unwrap_or_else(config::bind_addr);
            web::serve(db, &addr).await?;
            Ok(0)
        }
        Cmd::Run { execution_id } | Cmd::RunWorker { execution_id } => {
            huntwell::boot_bus("worker").await;
            let db = store::connect(&config::service_database_url()?, 4).await?;
            Ok(pipeline::execution_by_id(&db, execution_id).await)
        }
        Cmd::WorkerPool => {
            worker_pool::main().await?;
            Ok(0)
        }
        Cmd::Admin { addr } => {
            huntwell::init_tracing();
            huntwell::boot_bus("admin").await;
            // Say where the secrets came from. On a production server the
            // release binary reads `global` beside itself, and a missing or
            // shadowed file is otherwise invisible until something fails.
            match config::source_file() {
                Some(p) => tracing::info!("settings from {}", p.display()),
                None => tracing::warn!("no global settings file found — using the process environment only"),
            }
            let addr = addr.unwrap_or_else(|| config::get_or("HUNTWELL_ADMIN_ADDR", "127.0.0.1:8710"));
            // The control plane starts first on a fresh server, so it is the
            // one that creates the database if Postgres is still empty.
            let db = store::connect_or_create(&config::service_database_url()?, 8).await?;
            store::migrate(&db).await?;
            admin::serve(db, &addr).await?;
            Ok(0)
        }
        Cmd::Migrate { schema } => {
            let db = store::connect(&config::service_database_url()?, 2).await?;
            store::migrate_schema(&db, &schema).await?;
            println!("applied schema '{schema}'");
            Ok(0)
        }
        Cmd::Account { cmd } => {
            let db = store::connect(&config::database_url()?, 2).await?;
            store::migrate(&db).await?;
            match cmd {
                AccountCmd::Create { email, password, name } => {
                    web::auth::validate_signup(&email, &password)?;
                    let id = huntwell::identity::create_user(&email, &password).await?;
                    let acc = store::create_account(&db, &email, &name, &id).await?;
                    println!(
                        "created account #{} {} ({} identity)",
                        acc.account_id,
                        acc.email,
                        huntwell::identity::provider_name()
                    );
                }
                AccountCmd::List => {
                    let rows: Vec<(i64, String, String)> =
                        sqlx::query_as(r#"SELECT account_id,email,display_name FROM account ORDER BY account_id"#)
                            .fetch_all(&db)
                            .await?;
                    for (id, email, name) in rows {
                        println!("{id:>6}  {email}  {name}");
                    }
                }
            }
            Ok(0)
        }
        Cmd::Doctor => {
            println!("huntwell doctor");
            println!();
            match config::source_file() {
                Some(p) => println!("  settings   {}", p.display()),
                None => println!("  settings   none found (environment only)"),
            }
            match config::database_url() {
                Ok(url) => match store::connect(&url, 1).await {
                    Ok(db) => {
                        let n = store::account_count(&db).await.unwrap_or(0);
                        println!("  database   ok ({n} account(s))");
                    }
                    Err(e) => println!("  database   FAILED: {e:#}"),
                },
                Err(e) => println!("  database   {e}"),
            }
            println!(
                "  cursor     {}",
                if config::get("CURSOR_API_KEY").is_some() { "CURSOR_API_KEY set" } else { "CURSOR_API_KEY not set" }
            );
            println!("  data dir   {}", config::data_dir().display());
            // A real round-trip, not just "is it configured": collected files
            // are useless if the store cannot be written and read back.
            {
                let key = format!("healthcheck/{}.txt", std::process::id());
                let probe = b"huntwell doctor" as &[u8];
                let outcome = async {
                    objstore::put(&key, probe, "text/plain").await?;
                    let back = objstore::get(&key).await?;
                    let _ = objstore::delete(&key).await;
                    if back == probe {
                        Ok::<_, anyhow::Error>(())
                    } else {
                        Err(anyhow::anyhow!("read back {} bytes, expected {}", back.len(), probe.len()))
                    }
                }
                .await;
                let where_ = objstore::describe();
                match outcome {
                    Ok(()) if objstore::is_remote() => println!("  files      ok — {where_}"),
                    Ok(()) => println!("  files      ok — {where_}"),
                    Err(e) => println!("  files      FAILED ({where_}): {e:#}"),
                }
            }
            if browserbase::configured() {
                println!("  browser    Browserbase (remote) — project {}", config::get("BROWSERBASE_PROJECT_ID").unwrap_or_default());
            } else if config::get("HUNTWELL_BROWSER").map(|v| v.eq_ignore_ascii_case("browserbase")).unwrap_or(false) {
                println!("  browser    Browserbase selected but BROWSERBASE_API_KEY / _PROJECT_ID missing — runs will fail");
            } else {
                println!("  browser    local Chrome");
            }
            println!();
            browser::print_doctor();
            Ok(0)
        }
        Cmd::Genesis { cmd } => genesis_cmd(cmd),
        Cmd::Config { cmd } => {
            match cmd {
                ConfigCmd::Get { key } => match config::get(&key) {
                    Some(v) => println!("{v}"),
                    None => {
                        eprintln!("{key} is not set");
                        return Ok(1);
                    }
                },
                ConfigCmd::List => {
                    for key in [
                        "HUNTWELL_DATABASE_URL",
                        "HUNTWELL_ADDR",
                        "HUNTWELL_INSTANCE",
                        "HUNTWELL_DATA_DIR",
                        "HUNTWELL_CDP_PORT_BASE",
                        "HUNTWELL_CHROME_DISPLAY",
                        "HUNTWELL_OPEN_SIGNUP",
                        "HUNTWELL_BROWSER",
                        "BROWSERBASE_PROJECT_ID",
                        "BROWSERBASE_API_KEY",
                        "CURSOR_API_KEY",
                    ] {
                        let shown = match config::get(key) {
                            Some(v) if key.contains("KEY") || key.contains("SECRET") => format!("{}…", v.chars().take(6).collect::<String>()),
                            Some(v) => store_redact(&v),
                            None => "(unset)".into(),
                        };
                        println!("{key:<28} {shown}");
                    }
                }
            }
            Ok(0)
        }
        Cmd::McpProspects { .. } => unreachable!(),
    }
}

fn store_redact(v: &str) -> String {
    match (v.find("://"), v.rfind('@')) {
        (Some(a), Some(b)) if b > a => format!("{}://***@{}", &v[..a], &v[b + 1..]),
        _ => v.to_string(),
    }
}

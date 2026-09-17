//! A Huntwell worker: claims the runs assigned to this slot and executes them.
//!
//! One slot of a worker VM, run by the `huntwell-worker@N` unit, which supplies
//! HOST_ID (from /huntwell/env) and SLOT_NAME (`<vm>-N`). Work arrives through
//! the database, never over HTTP: the admin writes (host_id, slot) onto an
//! Execution and this process claims it.
//!
//! Needs the agent CLI and the Playwright MCP server, which is what makes the
//! worker image the large one.
//!
//! # Why this one takes arguments
//!
//! Two things spawn this executable *as itself*, through `current_exe()`:
//!
//!   worker run --execution-id N     the pool, for each claimed run — one run
//!                                   per process is a contract the pipeline
//!                                   relies on, so it is a child, not a task
//!   worker mcp-prospects --plan-id N  the agent, from inside that run, for its
//!                                   read-only plan-memory server
//!
//! Answering both is what lets the worker image contain one binary. If it
//! ignored them it would start a second pool instead, and every claimed run
//! would spawn another pool worker.

use huntwell::{config, mcp, pipeline, store, worker_pool};

fn main() {
    config::export_to_env();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        // Sync, and deliberately outside any runtime: it speaks MCP over stdio
        // for as long as the agent keeps it open.
        Some("mcp-prospects") => {
            let plan_id = flag(&args, "--plan-id").and_then(|v| v.parse().ok());
            match plan_id {
                Some(id) => mcp_serve(id),
                None => {
                    eprintln!("error: mcp-prospects needs --plan-id N");
                    2
                }
            }
        }
        // `run-worker` is what the pre-split manifests asked for. Accepted so a
        // manifest that has not been updated yet meets a new image and still
        // works, rather than failing with "unknown argument".
        Some("run") | Some("run-worker") => {
            huntwell::init_tracing();
            let Some(execution_id) = flag(&args, "--execution-id").and_then(|v| v.parse::<i64>().ok()) else {
                eprintln!("error: run needs --execution-id N");
                std::process::exit(2);
            };
            rt().block_on(async move {
                huntwell::boot_bus("worker").await;
                match store::connect(&config::service_database_url()?, 4).await {
                    Ok(db) => Ok(pipeline::execution_by_id(&db, execution_id).await),
                    Err(e) => Err(e),
                }
            })
            .unwrap_or_else(|e: anyhow::Error| {
                eprintln!("error: {e:#}");
                1
            })
        }
        Some(other) => {
            eprintln!("error: unknown argument {other:?} — worker takes no command, or `run` / `mcp-prospects`");
            2
        }
        None => {
            huntwell::boot("worker");
            rt().block_on(async {
                huntwell::boot_bus("worker").await;
                if let Err(e) = huntwell::svc::spawn("worker", None).await {
                    eprintln!("error: {e:#}");
                    return 1;
                }
                match worker_pool::main().await {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("error: {e:#}");
                        1
                    }
                }
            })
        }
    };
    std::process::exit(code);
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime")
}

/// `--name value`, the only shape these two self-spawns use.
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(String::as_str)
}

fn mcp_serve(plan_id: i64) -> i32 {
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

//! The warm-pool worker: one slot of a worker VM (`huntwell-worker@N`), or one
//! child of the admin's process pool on a dev box.
//!
//! The admin control plane assigns queued runs to a (host, slot) pair; this
//! supervisor polls for runs assigned to *this* slot, claims one atomically,
//! and executes it as a child `huntwell run --execution-id N` — the same child the
//! local runner spawns, so the pipeline is completely unchanged. The child's
//! stdout/stderr are tailed into `ExecutionLog` (the web UI's SSE reader polls that
//! table, so live logs work across hosts with no extra plumbing).
//!
//! While the child runs, a heartbeat updates `Run.HeartbeatAt` every few
//! seconds and reads back `CancelRequested` in the same round-trip; a lost
//! heartbeat lets the admin's reaper fail the run, and a cancel kills the
//! child's whole process group (agent and Chrome go with it).
//!
//! One run per slot at a time — that is the isolation contract: a runaway
//! search can only starve its own slot's cgroup. A VM of ten slots runs ten.
//!
//! On SIGTERM it finishes the run in hand and then exits, which is what lets a
//! deploy roll a worker VM without killing plans (the unit's KillMode=mixed
//! sends the signal to this process alone).

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::store::{self, Db};

const CLAIM_POLL: Duration = Duration::from_millis(1500);
const HEARTBEAT_EVERY: Duration = Duration::from_secs(10);

pub async fn main() -> Result<()> {
    let host_id: i64 = std::env::var("HOST_ID")
        .context("HOST_ID not set (a worker VM's /huntwell/env sets it)")?
        .trim()
        .parse()
        .context("HOST_ID is not a number")?;
    let slot = std::env::var("SLOT_NAME").context("SLOT_NAME not set (the huntwell-worker@ unit sets it per slot)")?;

    // The central database is this slot's only dependency; keep retrying so a
    // brief outage or a slot that starts before the tunnel does self-heals.
    let url = crate::config::service_database_url()?;
    let db = loop {
        match store::connect(&url, 2).await {
            Ok(db) => break db,
            Err(e) => {
                eprintln!("worker-pool: database not reachable yet ({e:#}); retrying in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };
    println!("worker-pool {slot} on host {host_id}: ready");
    crate::boot_bus("worker").await;

    let term = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        let term = term.clone();
        tokio::spawn(async move {
            let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
            sig.recv().await;
            term.store(true, Ordering::SeqCst);
        });
    }

    loop {
        if term.load(Ordering::SeqCst) {
            println!("worker-pool {slot}: terminating (idle)");
            return Ok(());
        }
        let execution_id = match store::claim_next_execution(&db, host_id, &slot).await {
            Ok(Some(id)) => id,
            Ok(None) => {
                tokio::time::sleep(CLAIM_POLL).await;
                continue;
            }
            Err(e) => {
                eprintln!("worker-pool {slot}: claim query failed ({e:#}); retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let _ = store::append_execution_log(&db, execution_id, "stdout", &format!("picked up by {slot} on host {host_id}")).await;
        if let Err(e) = execute(&db, execution_id, &slot, &term).await {
            let _ = store::append_execution_log(&db, execution_id, "stderr", &format!("worker error: {e:#}")).await;
            let _ = store::finish_execution(&db, execution_id, "failed", Some(-1)).await;
        }
        if term.load(Ordering::SeqCst) {
            println!("worker-pool {slot}: terminating after run {execution_id}");
            return Ok(());
        }
    }
}

/// Runs one claimed run to completion. The child writes its own terminal
/// status (guarded, first writer wins); the mappings here only apply when it
/// died before it could, or was killed.
async fn execute(db: &Db, execution_id: i64, slot: &str, term: &Arc<AtomicBool>) -> Result<()> {
    let exe = std::env::current_exe().context("locate own executable")?;
    let mut cmd = Command::new(exe);
    cmd.arg("run")
        .arg("--execution-id")
        .arg(execution_id.to_string())
        .env("HUNTWELL_EXECUTION_ID", execution_id.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    #[cfg(unix)]
    {
        // Own process group, same as the local runner: a cancel signals the
        // group so the agent CLI and Chrome die with the run, not the slot.
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().context("spawn run child")?;
    let pid = child.id().unwrap_or(0);

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let d1 = db.clone();
    let d2 = db.clone();
    let t_out = tokio::spawn(async move {
        if let Some(out) = stdout {
            tail(&d1, execution_id, "stdout", out).await;
        }
    });
    let t_err = tokio::spawn(async move {
        if let Some(err) = stderr {
            tail(&d2, execution_id, "stderr", err).await;
        }
    });

    // Wait for the child while heartbeating; a heartbeat that reports a
    // cancel (or a slot SIGTERM) kills the group and lets the wait finish.
    let mut killed = false;
    let mut hb = tokio::time::interval(HEARTBEAT_EVERY);
    hb.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            _ = hb.tick() => {
                let cancel = store::heartbeat_execution(db, execution_id).await.unwrap_or(false);
                if (cancel || term.load(Ordering::SeqCst)) && !killed {
                    let why = if cancel { "cancel requested" } else { "slot terminating" };
                    let _ = store::append_execution_log(db, execution_id, "stderr", &format!("{why} — stopping the run")).await;
                    crate::web::runner::kill_group(pid).await;
                    killed = true;
                }
            }
        }
    };
    let _ = t_out.await;
    let _ = t_err.await;

    let code = status.as_ref().ok().and_then(|s| s.code());
    let (status_str, code) = match (killed, code) {
        (true, _) => ("cancelled", Some(130)),
        (false, Some(0)) => ("succeeded", Some(0)),
        (false, Some(c)) => ("failed", Some(c)),
        (false, None) => ("failed", Some(-1)),
    };
    let _ = store::finish_execution(db, execution_id, status_str, code).await;
    println!("worker-pool {slot}: run {execution_id} finished ({status_str})");
    Ok(())
}

/// Same shape as the local runner's tail, minus the in-process SSE wake — the
/// web tier polls `ExecutionLog`, so lines written here still stream live.
async fn tail<R: tokio::io::AsyncRead + Unpin>(db: &Db, execution_id: i64, stream: &str, reader: R) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line: String = line.chars().take(4000).collect();
        if let Err(e) = store::append_execution_log(db, execution_id, stream, &line).await {
            eprintln!("worker-pool: could not store log line for run {execution_id}: {e:#}");
        }
    }
}

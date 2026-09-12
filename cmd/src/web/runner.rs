//! Spawns `huntwell run --execution-id N` as a child, tails its output into
//! `ExecutionLog`, and reports the exit. One child per run; cancel kills the whole
//! process group so the agent and Chrome it started go with it.

use std::process::Stdio;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::{App, LogEvent};
use crate::pipeline::RunArgs;
use crate::store;

#[allow(dead_code)]
pub struct ActiveRun {
    pub pid: u32,
    pub plan_id: i64,
    pub account_id: i64,
}

/// A DevTools port per account: runs of one account share a Chrome (the
/// second adopts the first's), and two accounts never share cookies.
pub fn cdp_port_for(account_id: i64) -> u16 {
    let base = crate::config::cdp_port_base() as i64;
    (base + (account_id % 2000)).clamp(1024, 65535) as u16
}

/// Creates the Run row and starts the child. Refuses a second concurrent run
/// of the same plan.
pub async fn start(state: &App, account_id: i64, plan_id: i64, trigger: &str, args: RunArgs) -> Result<i64> {
    let _gate = state.start_gate.lock().await;
    let plan = store::get_plan(&state.db, account_id, plan_id)
        .await?
        .ok_or_else(|| anyhow!("plan not found"))?;
    if let Err(why) = store::plan_ready(&plan) {
        anyhow::bail!("{why} — the plan cannot run yet");
    }
    if let Some(active) = store::active_execution_for_plan(&state.db, plan_id).await? {
        anyhow::bail!("plan is already running (run #{})", active.execution_id);
    }
    // A run costs money to execute, so it needs someone to bill. Checked in the
    // same place as the budget cap — the single point every run passes through,
    // so the manual "Go now", a scheduled run and an API-key run are all held to
    // it, not just the button the UI happens to gate.
    if !store::has_payment_method(&state.db, account_id).await? {
        anyhow::bail!("no payment method on file — add a card in Usage & billing to start a run");
    }
    // The usage cap — the single point every run passes through, so manual and
    // scheduled runs are both stopped once the account is out of budget.
    if store::account_over_budget(&state.db, account_id).await? {
        anyhow::bail!("monthly usage limit reached — add tokens to keep running plans");
    }
    let cdp = cdp_port_for(account_id);
    let args_json = serde_json::to_value(&args)?;
    let execution_id = store::create_execution(&state.db, account_id, plan_id, trigger, &args_json, cdp).await?;
    // Published here, before the dispatch branches, so every mode announces a
    // queued run identically — a listener should not have to know whether this
    // deployment forks a child, creates a Job, or waits for a pool pod.
    crate::bus::publish(
        crate::bus::subject::RUN_QUEUED,
        Some(account_id),
        serde_json::json!({ "execution_id": execution_id, "plan_id": plan_id, "trigger": trigger }),
    )
    .await;

    // Pool path: the run stays queued; the admin control plane's placement
    // loop routes it to a warm worker pod (random / round-robin / pinned) and
    // that pod's supervisor claims and executes it. Nothing to spawn here.
    if super::dispatch::mode() == super::dispatch::Mode::Pool {
        let _ = store::append_execution_log(&state.db, execution_id, "stdout", "queued — waiting for a worker pod").await;
        tracing::info!(execution_id, plan_id, account_id, "run queued for pool dispatch");
        return Ok(execution_id);
    }

    // Hosted (k3d) path: run as a Kubernetes Job instead of a child process. The
    // Job's pod is one-run-per-process just like the child, so the pipeline's
    // process-wide state is unaffected; its log is streamed into RunLog by
    // `dispatch::track`.
    if super::dispatch::enabled() {
        if let Err(e) = super::dispatch::start_job(state, execution_id).await {
            let _ = store::append_execution_log(&state.db, execution_id, "stderr", &format!("could not start run: {e:#}")).await;
            let _ = store::finish_execution(&state.db, execution_id, "failed", Some(-1)).await;
            return Err(anyhow!("dispatch run: {e:#}"));
        }
        tracing::info!(execution_id, plan_id, account_id, "run dispatched as Job");
        return Ok(execution_id);
    }

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
        // Own process group: cancel signals the group and the `agent` and
        // Chrome children die with the run, not the server.
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = store::append_execution_log(&state.db, execution_id, "stderr", &format!("could not start run: {e}")).await;
            let _ = store::finish_execution(&state.db, execution_id, "failed", Some(-1)).await;
            return Err(anyhow!("spawn run: {e}"));
        }
    };
    let pid = child.id().unwrap_or(0);
    store::set_execution_running(&state.db, execution_id, Some(pid)).await?;
    state.active.lock().await.insert(execution_id, ActiveRun { pid, plan_id, account_id });
    tracing::info!(execution_id, plan_id, account_id, pid, "run started");

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let s1 = state.clone();
    let s2 = state.clone();
    let t_out = tokio::spawn(async move {
        if let Some(out) = stdout {
            tail(&s1, execution_id, "stdout", out).await;
        }
    });
    let t_err = tokio::spawn(async move {
        if let Some(err) = stderr {
            tail(&s2, execution_id, "stderr", err).await;
        }
    });

    let state = state.clone();
    tokio::spawn(async move {
        let status = child.wait().await;
        let _ = t_out.await;
        let _ = t_err.await;
        let code = status.as_ref().ok().and_then(|s| s.code());
        let cancelled = state.active.lock().await.remove(&execution_id).is_none();
        // The child normally writes its own final status; these only apply
        // if it died before it could.
        let (status_str, code) = match (cancelled, code) {
            (true, _) => ("cancelled", Some(130)),
            (false, Some(0)) => ("succeeded", Some(0)),
            (false, Some(c)) => ("failed", Some(c)),
            (false, None) => ("failed", Some(-1)),
        };
        let _ = store::finish_execution(&state.db, execution_id, status_str, code).await;
        let _ = state.log_tx.send(LogEvent { execution_id, seq: -1 });
        tracing::info!(execution_id, status = status_str, "run finished");
    });
    Ok(execution_id)
}

async fn tail<R: tokio::io::AsyncRead + Unpin>(state: &App, execution_id: i64, stream: &str, reader: R) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line: String = line.chars().take(4000).collect();
        match store::append_execution_log(&state.db, execution_id, stream, &line).await {
            Ok(seq) => {
                let _ = state.log_tx.send(LogEvent { execution_id, seq });
            }
            Err(e) => tracing::warn!(execution_id, "could not store log line: {e:#}"),
        }
    }
}

/// Stops a run: SIGTERM to the group (the child marks itself cancelled and
/// tidies Chrome), SIGKILL after a grace period if it is still there.
pub async fn cancel(state: &App, account_id: i64, execution_id: i64) -> Result<bool> {
    // Pool path: mark the run cancelled (guarded — first writer wins) and set
    // the flag the pod supervisor polls with its heartbeat; the supervisor
    // kills the child within one heartbeat interval. A queued-unclaimed run
    // simply never gets picked up once terminal.
    if super::dispatch::mode() == super::dispatch::Mode::Pool {
        let Some(run) = store::get_execution(&state.db, account_id, execution_id).await? else { return Ok(false) };
        if !matches!(run.status.as_str(), "queued" | "running") {
            return Ok(false);
        }
        store::request_execution_cancel(&state.db, execution_id).await?;
        store::finish_execution(&state.db, execution_id, "cancelled", Some(130)).await?;
        let _ = store::append_execution_log(&state.db, execution_id, "stderr", "execution cancelled from the UI").await;
        return Ok(true);
    }
    // Hosted (k3d) path: there is no in-memory child; cancel means delete the
    // Job. Ownership is checked against the account-scoped Run row.
    if super::dispatch::enabled() {
        let Some(run) = store::get_execution(&state.db, account_id, execution_id).await? else { return Ok(false) };
        if !matches!(run.status.as_str(), "queued" | "running") {
            return Ok(false);
        }
        let _ = super::dispatch::cancel_job(execution_id).await;
        store::finish_execution(&state.db, execution_id, "cancelled", Some(130)).await?;
        let _ = store::append_execution_log(&state.db, execution_id, "stderr", "execution cancelled from the UI").await;
        return Ok(true);
    }
    let entry = state.active.lock().await.remove(&execution_id);
    let Some(active) = entry else { return Ok(false) };
    if active.account_id != account_id {
        // Not theirs — put it back and pretend it does not exist.
        state.active.lock().await.insert(execution_id, active);
        return Ok(false);
    }
    store::finish_execution(&state.db, execution_id, "cancelled", Some(130)).await?;
    let _ = store::append_execution_log(&state.db, execution_id, "stderr", "execution cancelled from the UI").await;
    kill_group(active.pid).await;
    Ok(true)
}

pub async fn stop_all(state: &App) {
    let all: Vec<(i64, u32)> = state.active.lock().await.drain().map(|(id, a)| (id, a.pid)).collect();
    for (execution_id, pid) in all {
        let _ = store::finish_execution(&state.db, execution_id, "cancelled", Some(143)).await;
        kill_group(pid).await;
    }
}

pub(crate) async fn kill_group(pid: u32) {
    #[cfg(unix)]
    {
        let term = Command::new("kill").arg("-TERM").arg(format!("-{pid}")).status().await;
        if term.is_err() {
            return;
        }
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            let alive = Command::new("kill").arg("-0").arg(pid.to_string()).status().await.map(|s| s.success()).unwrap_or(false);
            if !alive {
                return;
            }
        }
        let _ = Command::new("kill").arg("-KILL").arg(format!("-{pid}")).status().await;
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status().await;
    }
}

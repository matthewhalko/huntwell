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
///
/// `base + account_id`, with no wrap-around while it fits: a Chrome already
/// listening on the port is adopted as-is (`browser::start_for_run`), so two
/// accounts on one port would share one browser and its signed-in sessions.
/// The old `% 2000` did exactly that for accounts 2000 apart. Past the top of
/// the port range it has to wrap; that is tens of thousands of accounts away,
/// and runs there use Browserbase, which has no local port.
pub fn cdp_port_for(account_id: i64) -> u16 {
    let base = crate::config::cdp_port_base() as i64;
    let port = base + account_id.max(0);
    if port <= 65535 {
        return port as u16;
    }
    let span = (65535 - base).max(1);
    (base + account_id.rem_euclid(span)) as u16
}

#[cfg(test)]
mod port_tests {
    #[test]
    fn accounts_2000_apart_get_different_browsers() {
        assert_ne!(super::cdp_port_for(17), super::cdp_port_for(2017));
        assert_eq!(super::cdp_port_for(17), super::cdp_port_for(17));
    }
}

/// How many runs this server executes at once in local dispatch (no worker
/// pool). More wait, queued, and start as others finish.
fn local_max_runs() -> usize {
    crate::config::get("HUNTWELL_LOCAL_MAX_RUNS").and_then(|v| v.trim().parse().ok()).filter(|n| *n > 0).unwrap_or(4)
}

/// Whether an account's runs share one browser on this machine. A local
/// Chrome is one profile per account (its cookies and logins), which two runs
/// cannot drive at once — so they take turns. Browserbase gives every run its
/// own browser, and an account's runs go side by side.
fn runs_share_a_browser() -> bool {
    !crate::browserbase::configured()
}

/// Whether a local run for `account_id` may start now, given what is running.
async fn local_room_for(state: &App, account_id: i64) -> bool {
    let active = state.active.lock().await;
    active.len() < local_max_runs() && !(runs_share_a_browser() && active.values().any(|a| a.account_id == account_id))
}

/// Creates the Run row and starts it — or queues it when there is no room.
///
/// An account may run many plans at once; only the same plan twice is
/// refused. Room is the pool's slots (pool dispatch: the run waits `queued`
/// until the placement loop finds a free slot) or this server's own limit
/// (local dispatch: `local_max_runs`, and one run at a time per account when
/// runs share a local browser). A queued run starts by itself when room opens.
///
/// Credits stay safe with several runs at once: every debit is atomic and
/// capped at what the wallet holds (`store::charge_account_usage`), and each
/// run stops itself when the wallet empties. What concurrency can add is at
/// most one unbilled agent turn per running plan — never a charge past zero.
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
    // A run costs money to execute, so it needs someone to bill and prepaid
    // credits to spend. Checked here — the single point every run passes
    // through — so the manual "Go now", a scheduled run and an API-key run
    // are all held to it, not just the button the UI happens to gate.
    let has_card = if crate::config::is_production() {
        store::has_real_payment_method(&state.db, account_id).await?
    } else {
        store::has_payment_method(&state.db, account_id).await?
    };
    // Free credit from an operator is spent without a card; the card is asked
    // for once it is gone (store::spends_free_credit).
    if !has_card && !store::spends_free_credit(&state.db, account_id).await? {
        if store::has_granted_credit(&state.db, account_id).await? {
            anyhow::bail!("your free credit is used up — add a card in Usage & billing to keep running searches");
        }
        anyhow::bail!("no payment method on file — add a card in Usage & billing to start a run");
    }
    if store::account_over_budget(&state.db, account_id).await? {
        anyhow::bail!("no credits remaining — buy credits in Usage & billing to start a run");
    }
    let cdp = cdp_port_for(account_id);
    let args_json = serde_json::to_value(&args)?;
    // Postgres holds the one-run-per-plan rule too; losing that race reads
    // the same as the check above rather than as a database error.
    let execution_id = match store::create_execution(&state.db, account_id, plan_id, trigger, &args_json, cdp).await {
        Ok(id) => id,
        Err(e) if format!("{e:#}").contains("execution_one_active_per_plan_idx") => anyhow::bail!("plan is already running"),
        Err(e) => return Err(e),
    };
    // Published here, before the dispatch branches, so every mode announces a
    // queued run identically — a listener should not have to know whether this
    // deployment forks a child or waits for a worker slot.
    crate::bus::publish(
        crate::bus::subject::RUN_QUEUED,
        Some(account_id),
        serde_json::json!({ "execution_id": execution_id, "plan_id": plan_id, "trigger": trigger }),
    )
    .await;

    // Pool path: the run stays queued; the admin control plane's placement
    // loop routes it to a warm worker slot (random / round-robin / pinned) and
    // that slot's supervisor claims and executes it. Nothing to spawn here.
    if super::dispatch::mode() == super::dispatch::Mode::Pool {
        let _ = store::append_execution_log(&state.db, execution_id, "stdout", "queued — waiting for a worker slot").await;
        tracing::info!(execution_id, plan_id, account_id, "run queued for pool dispatch");
        return Ok(execution_id);
    }

    if !local_room_for(state, account_id).await {
        let why = if runs_share_a_browser() && state.active.lock().await.values().any(|a| a.account_id == account_id) {
            "queued — waits for this workspace's other run to finish (they share one browser on this server)"
        } else {
            "queued — waiting for a free slot"
        };
        let _ = store::append_execution_log(&state.db, execution_id, "stdout", why).await;
        tracing::info!(execution_id, plan_id, account_id, "run queued locally");
        return Ok(execution_id);
    }
    launch(state, execution_id, plan_id, account_id).await?;
    Ok(execution_id)
}

/// Wakes the local queue: a run finished, so there may be room.
static QUEUE_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// The local queue's one worker: starts waiting runs when a run finishes, and
/// every 15s besides (and once at startup, for runs queued before a restart).
/// Pool dispatch has no local queue — the admin's placement loop is its twin.
pub fn spawn_local_queue(state: App) {
    if super::dispatch::mode() == super::dispatch::Mode::Pool {
        return;
    }
    tokio::spawn(async move {
        loop {
            drain_local_queue(&state).await;
            let _ = tokio::time::timeout(std::time::Duration::from_secs(15), QUEUE_WAKE.notified()).await;
        }
    });
}

/// Start queued local runs while there is room, oldest first.
async fn drain_local_queue(state: &App) {
    if super::dispatch::mode() == super::dispatch::Mode::Pool {
        return;
    }
    let _gate = state.start_gate.lock().await;
    let queued = match store::queued_local_executions(&state.db, 50).await {
        Ok(q) => q,
        Err(e) => {
            tracing::warn!("read the local run queue: {e:#}");
            return;
        }
    };
    for (execution_id, plan_id, account_id) in queued {
        if state.active.lock().await.len() >= local_max_runs() {
            break;
        }
        if !local_room_for(state, account_id).await {
            continue; // this account's turn comes when its other run ends
        }
        let _ = store::append_execution_log(&state.db, execution_id, "stdout", "a slot opened — starting").await;
        if let Err(e) = launch(state, execution_id, plan_id, account_id).await {
            tracing::warn!(execution_id, "could not start a queued run: {e:#}");
        }
    }
}

/// Spawn the run process for an execution that is ready to go.
async fn launch(state: &App, execution_id: i64, plan_id: i64, account_id: i64) -> Result<()> {
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
        // Room just opened: wake the queue so whatever was waiting starts.
        QUEUE_WAKE.notify_one();
    });
    Ok(())
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
    // the flag the slot supervisor polls with its heartbeat; the supervisor
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
    let entry = state.active.lock().await.remove(&execution_id);
    let Some(active) = entry else {
        // Not running here: it may be waiting in the local queue, which is
        // cancelled by marking it — it then never starts.
        let Some(run) = store::get_execution(&state.db, account_id, execution_id).await? else { return Ok(false) };
        if run.status != "queued" {
            return Ok(false);
        }
        store::finish_execution(&state.db, execution_id, "cancelled", Some(130)).await?;
        let _ = store::append_execution_log(&state.db, execution_id, "stderr", "execution cancelled before it started").await;
        return Ok(true);
    };
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

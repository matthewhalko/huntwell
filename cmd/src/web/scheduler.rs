//! Fires scheduled plans. Ticks every 20 seconds; a slot whose plan is still
//! running is skipped rather than queued, and NextRunAt is advanced *before*
//! the start is attempted so a failing start cannot retry every tick.

use std::time::Duration;

use super::{runner, App};
use crate::pipeline::RunArgs;
use crate::store;

/// The advisory-lock key that makes firing a singleton. Distinct from the
/// migration lock (7411) so a migration and the scheduler never block on each
/// other.
const SCHEDULER_LOCK: i64 = 7412;

/// In-process scheduler, for the all-in-one `huntwell serve`.
pub fn spawn(state: App) {
    tokio::spawn(async move { tick_forever(&state).await });
}

/// The scheduling service: the same loop, but only ever in one process.
///
/// Firing is not idempotent — `mark_schedule_fired` advances NextRunAt, and two
/// tickers that read `scheduled_plans_due` before either marks will both start
/// the same plan. That is not hypothetical here: the website runs on every node
/// behind the edge load balancer, so "one replica" is not something the
/// deployment can promise.
///
/// So the lock is the promise. It is a session-level Postgres advisory lock
/// held on one dedicated connection for as long as this process fires; a second
/// instance cannot take it and waits instead, which makes it a warm standby —
/// if the holder dies its session ends, the lock is released, and the standby
/// picks up within `STANDBY_RETRY`.
pub async fn run(state: App) -> anyhow::Result<()> {
    const STANDBY_RETRY: Duration = Duration::from_secs(10);
    let mut announced_standby = false;
    loop {
        // A pool connection, held for as long as we hold the lock: the lock
        // lives on the session, so returning this to the pool would drop it.
        let mut conn = state.db.acquire().await?;
        let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(SCHEDULER_LOCK)
            .fetch_one(&mut *conn)
            .await?;
        if !held {
            drop(conn);
            if !announced_standby {
                tracing::info!("another scheduling service is firing — standing by");
                announced_standby = true;
            }
            tokio::time::sleep(STANDBY_RETRY).await;
            continue;
        }
        tracing::info!("holding the scheduler lock — firing due plans");
        announced_standby = false;
        // Held until this connection is dropped, which happens when the loop
        // below returns because the connection itself failed.
        tick_forever(&state).await;
        tracing::warn!("scheduler loop ended — releasing the lock and retrying");
        drop(conn);
        tokio::time::sleep(STANDBY_RETRY).await;
    }
}

async fn tick_forever(state: &App) {
    let mut tick = tokio::time::interval(Duration::from_secs(20));
    loop {
        tick.tick().await;
        if let Err(e) = fire_due(state).await {
            tracing::warn!("scheduler: {e:#}");
        }
        // Session hygiene rides along on the same tick.
        let _ = store::delete_expired_sessions(&state.db).await;
    }
}

async fn fire_due(state: &App) -> anyhow::Result<()> {
    for (plan_id, account_id) in store::scheduled_plans_due(&state.db).await? {
        store::mark_schedule_fired(&state.db, plan_id).await?;
        store::refresh_schedule(&state.db, plan_id).await?;
        if let Some(active) = store::active_execution_for_plan(&state.db, plan_id).await? {
            tracing::info!(plan_id, execution_id = active.execution_id, "schedule slot skipped — plan still running");
            continue;
        }
        let Some(plan) = store::get_plan(&state.db, account_id, plan_id).await? else { continue };
        let args = RunArgs {
            target: (plan.target_prospects > 0).then_some(plan.target_prospects as i64),
            ..Default::default()
        };
        match runner::start(state, account_id, plan_id, "schedule", args).await {
            Ok(execution_id) => tracing::info!(plan_id, execution_id, "scheduled run started"),
            Err(e) => tracing::warn!(plan_id, "scheduled run did not start: {e:#}"),
        }
    }
    Ok(())
}

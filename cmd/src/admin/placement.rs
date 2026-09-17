//! Routing: assigns queued runs to free pool slots, and reaps runs whose
//! worker stopped heartbeating.
//!
//! Strategy lives in `ControlSetting`:
//!   routing_strategy = random | round_robin | pinned
//!   pinned_host_id / pinned_slot — the single target when pinned
//!   rr_cursor — last "host:slot" used, so round-robin survives restarts
//!
//! A slot is a candidate when its host is enabled and healthy, its unit was
//! active on the last reconcile, and no non-terminal run is placed on it —
//! one run per slot is the isolation contract.

use std::collections::HashSet;
use std::time::Duration;

use super::Admin;
use crate::store;

const PLACE_EVERY: Duration = Duration::from_secs(2);
const REAP_EVERY: Duration = Duration::from_secs(30);

pub fn spawn(state: Admin) {
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(PLACE_EVERY);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(e) = place(&state).await {
                    tracing::warn!("placement: {e:#}");
                }
            }
        });
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(REAP_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = reap(&state).await {
                tracing::warn!("reaper: {e:#}");
            }
        }
    });
}

/// Every free, Ready slot across enabled healthy hosts, host-major order.
pub async fn free_slots(state: &Admin) -> anyhow::Result<Vec<(i64, String)>> {
    let hosts_list = store::list_hosts(&state.db).await?;
    let busy: HashSet<(i64, String)> = store::pool_execution_snapshot(&state.db)
        .await?
        .into_iter()
        .map(|r| (r.host_id, r.slot_name))
        .collect();
    let cache = state.slots.lock().await;
    let mut free = Vec::new();
    for h in hosts_list.iter().filter(|h| h.enabled && h.last_error.is_none()) {
        if let Some(slots) = cache.get(&h.host_id) {
            for p in slots.iter().filter(|p| p.ready) {
                if !busy.contains(&(h.host_id, p.name.clone())) {
                    free.push((h.host_id, p.name.clone()));
                }
            }
        }
    }
    free.sort();
    Ok(free)
}

async fn place(state: &Admin) -> anyhow::Result<()> {
    let runs = store::unplaced_queued_executions(&state.db, 20).await?;
    if runs.is_empty() {
        return Ok(());
    }
    let strategy = store::get_setting(&state.db, "routing_strategy").await?.unwrap_or_else(|| "round_robin".into());
    let mut free = free_slots(state).await?;

    for execution_id in runs {
        if free.is_empty() {
            break; // pool saturated — runs stay visibly queued
        }
        let pick: Option<(i64, String)> = match strategy.as_str() {
            "pinned" => {
                let host: Option<i64> =
                    store::get_setting(&state.db, "pinned_host_id").await?.and_then(|v| v.parse().ok());
                let slot = store::get_setting(&state.db, "pinned_slot").await?.unwrap_or_default();
                match host {
                    // Only the one target counts; if it's busy the run waits.
                    Some(h) if free.iter().any(|(fh, fp)| *fh == h && *fp == slot) => Some((h, slot)),
                    _ => None,
                }
            }
            "random" => {
                if free.is_empty() {
                    None
                } else {
                    // Cheap uniform pick without a rand dependency.
                    let n = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.subsec_nanos() as usize)
                        .unwrap_or(0);
                    Some(free[n % free.len()].clone())
                }
            }
            // round_robin (default): the free slot after the stored cursor.
            _ => {
                let cursor = store::get_setting(&state.db, "rr_cursor").await?.unwrap_or_default();
                let start = free
                    .iter()
                    .position(|(h, p)| format!("{h}:{p}") == cursor)
                    .map(|i| (i + 1) % free.len())
                    .unwrap_or(0);
                Some(free[start].clone())
            }
        };
        let Some((host_id, slot)) = pick else {
            if strategy == "pinned" {
                break; // pinned target busy/gone: everything waits for it
            }
            break;
        };
        if store::assign_execution(&state.db, execution_id, host_id, &slot).await? {
            let _ = store::append_execution_log(&state.db, execution_id, "stdout", &format!("routed to {slot} on host {host_id} ({strategy})"))
                .await;
            let _ = store::append_route_log(&state.db, execution_id, "routed", Some(host_id), Some(&slot), &strategy).await;
            let _ = store::set_setting(&state.db, "rr_cursor", &format!("{host_id}:{slot}")).await;
            tracing::info!(execution_id, host_id, slot, strategy, "run placed");
        }
        free.retain(|(h, p)| !(*h == host_id && *p == slot));
    }
    Ok(())
}

async fn reap(state: &Admin) -> anyhow::Result<()> {
    let stale_secs: i64 = crate::config::get("HUNTWELL_POOL_STALE_SECS").and_then(|v| v.parse().ok()).unwrap_or(90);
    for execution_id in store::reap_stale_pool_executions(&state.db, stale_secs).await? {
        let _ = store::append_execution_log(
            &state.db,
            execution_id,
            "stderr",
            "worker heartbeat lost — the slot likely died; run marked failed",
        )
        .await;
        let _ = store::append_route_log(&state.db, execution_id, "reaped", None, None, "heartbeat lost — marked failed").await;
        tracing::warn!(execution_id, "reaped stale pool run");
    }
    // Queued runs on vanished slots are re-queued by hosts::reconcile_host
    // (unassign_lost_executions), so the reaper only handles claimed-and-silent.
    Ok(())
}

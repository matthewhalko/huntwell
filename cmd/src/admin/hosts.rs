//! Host operations: health checks, and turning each host's VMs into the list
//! of worker slots the placement loop assigns runs to.
//!
//! An Incus host is reached through `incus.rs`; its VMs are made and changed by
//! `incus_driver.rs`. What lives here is the every-20-seconds view: is the host
//! answering, which of its worker slots are up, and which runs were assigned to
//! a slot that no longer exists.
//!
//! A worker slot is a named process that claims the runs addressed to it, so
//! the slot cache is all `placement.rs` needs to know about any host.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use super::{incus, incus_driver, Admin, Ping, SlotInfo, VM_CHECK_FRESH_SECS};
use crate::store::{self, Host};

const RECONCILE_EVERY: Duration = Duration::from_secs(20);

/// The process-backed host `./dev.sh` registers: worker children of the admin
/// itself, no VMs. Recognised by having no Incus endpoint.
pub fn is_local(h: &Host) -> bool {
    h.endpoint.trim().is_empty()
}

/// Trust a new host with its one-time token (`sys incus token`).
pub async fn register(h: &Host, token: &str) -> Result<()> {
    if !h.endpoint.starts_with("https://") {
        bail!("the endpoint is the host's Incus API, e.g. https://10.0.0.1:8443");
    }
    if token.trim().is_empty() {
        bail!("a host needs a trust token — run `sys incus token` on it and paste what it prints");
    }
    incus::add_remote(h.remote(), &h.endpoint, token)
        .await
        .map_err(|e| anyhow!("could not register with {}: {e:#}", h.endpoint))
}

/// Ask a host how it is and record the answer. An unreachable host is marked
/// Offline, which placement skips; Draining is an operator's decision and
/// survives any number of failed checks.
pub async fn check_host(state: &Admin, h: &Host) -> Result<()> {
    let outcome = async {
        let server = incus::query(h.remote(), "/1.0").await?;
        let resources = incus::query(h.remote(), "/1.0/resources").await.unwrap_or(Value::Null);
        Ok::<_, anyhow::Error>((server, resources))
    }
    .await;
    match outcome {
        Ok((server, resources)) => {
            let arch = server["environment"]["kernel_architecture"].as_str().unwrap_or("");
            let version = server["environment"]["server_version"].as_str().unwrap_or("");
            let cpus = resources["cpu"]["total"].as_i64().unwrap_or(0) as i32;
            let mem_mb = resources["memory"]["total"].as_i64().unwrap_or(0) / (1024 * 1024);
            store::set_host_facts(&state.db, h.host_id, arch, version, cpus, mem_mb).await?;
            store::set_host_health(&state.db, h.host_id, None).await?;
            Ok(())
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let _ = store::set_host_health(&state.db, h.host_id, Some(&msg)).await;
            Err(e)
        }
    }
}

/// The host a new worker VM goes on: Active, under its VM limit, lowest
/// priority first, then fewest VMs.
pub async fn pick_for_placement(state: &Admin) -> Result<Host> {
    let hosts = store::list_hosts(&state.db).await?;
    let vms = store::list_vms(&state.db).await?;
    let mut candidates: Vec<(Host, usize)> = hosts
        .into_iter()
        .filter(|h| !is_local(h) && h.enabled && h.status == "Active")
        .map(|h| {
            let n = vms.iter().filter(|v| v.host_id == h.host_id).count();
            (h, n)
        })
        .filter(|(h, n)| h.max_vms == 0 || (*n as i32) < h.max_vms)
        .collect();
    if candidates.is_empty() {
        bail!(
            "no host can take another VM — every host is full, draining or offline. \
             Raise a host's VM limit, or add a server with `sys incus init --app huntwell`"
        );
    }
    candidates.sort_by(|(a, an), (b, bn)| a.priority.cmp(&b.priority).then(an.cmp(bn)).then(a.name.cmp(&b.name)));
    Ok(candidates.remove(0).0)
}

/// Bring one host's view up to date.
pub async fn reconcile_host(state: &Admin, h: &Host) {
    if is_local(h) {
        return reconcile_local(state, h).await;
    }
    if check_host(state, h).await.is_err() {
        // Unreachable is not "zero slots": clear the cache so placement stops
        // assigning here, and leave the error on the row for the console.
        state.slots.lock().await.remove(&h.host_id);
        return;
    }
    let vms = match store::list_host_vms(&state.db, h.host_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(host = h.name, "list vms: {e:#}");
            return;
        }
    };

    // The app VM's services, from systemd. The admin cannot hear their bus
    // heartbeats — that bus is loopback inside the app VM, on purpose — so
    // without this the Services card would call a healthy app VM down.
    for vm in vms.iter().filter(|v| v.role == "app" && v.status == "Running") {
        sight_units(state, h, &vm.name, incus_driver::APP_SERVICES).await;
    }

    let mut seen = Vec::new();
    for vm in vms.iter().filter(|v| v.is_worker()) {
        let names = vm.slot_names();
        let status = incus::instance_status(h.remote(), &vm.name).await;
        // Keep the console honest about a VM someone changed by hand.
        match status.as_deref() {
            None if vm.status == "Running" => {
                let _ = store::set_vm_status(&state.db, vm.vm_id, "Failed", "the instance no longer exists on the host").await;
            }
            Some("Stopped") if vm.status == "Running" => {
                let _ = store::set_vm_status(&state.db, vm.vm_id, "Stopped", "").await;
            }
            _ => {}
        }
        let active = if status.as_deref() == Some("Running") && vm.status == "Running" {
            slot_states(h, &vm.name, vm.slots).await
        } else {
            vec![false; names.len()]
        };
        {
            let now = chrono::Utc::now();
            let mut pings = state.pings.lock().await;
            for (name, ready) in names.iter().zip(&active) {
                if *ready {
                    // Each running slot is an instance of the worker service.
                    pings.insert(("worker".into(), name.clone()), Ping { at: now, fresh_for: VM_CHECK_FRESH_SECS });
                }
            }
        }
        for (name, ready) in names.into_iter().zip(active) {
            seen.push(SlotInfo { name, ready, node: vm.name.clone() });
        }
    }

    let live: Vec<String> = seen.iter().map(|p| p.name.clone()).collect();
    state.slots.lock().await.insert(h.host_id, seen);
    // Queued runs assigned to a slot that no longer exists go back to placement.
    if let Ok(runs) = store::unassign_lost_executions(&state.db, h.host_id, &live).await {
        for execution_id in &runs {
            let _ = store::append_route_log(&state.db, *execution_id, "requeued", Some(h.host_id), None, "assigned slot no longer exists").await;
        }
        if !runs.is_empty() {
            tracing::info!(host = h.name, n = runs.len(), "re-queued runs from lost slots");
        }
    }
}

/// Record every one of `services` whose unit is active inside `vm` as a live
/// instance of that service. One exec for all of them. A unit that is down is
/// simply not recorded, so it ages to stale on the card within a minute.
async fn sight_units(state: &Admin, h: &Host, vm: &str, services: &[&str]) {
    let units: Vec<String> = services.iter().map(|s| format!("huntwell-{s}")).collect();
    let script = format!("systemctl is-active {} || true", units.join(" "));
    let Ok(out) = incus::exec(h.remote(), vm, &["sh", "-c", &script]).await else { return };
    let now = chrono::Utc::now();
    let mut pings = state.pings.lock().await;
    for (service, line) in services.iter().zip(out.lines()) {
        if line.trim() == "active" {
            pings.insert((service.to_string(), vm.to_string()), Ping { at: now, fresh_for: VM_CHECK_FRESH_SECS });
        }
    }
}

/// Whether each of a worker's slots is running, in order — one exec for all of
/// them rather than one per slot, so a host of many VMs stays cheap to watch.
async fn slot_states(h: &Host, vm: &str, slots: i32) -> Vec<bool> {
    let units = incus_driver::slot_units(1, slots).join(" ");
    // `|| true`: is-active exits non-zero when any unit is down, and the
    // per-unit lines are exactly what is wanted in that case.
    let script = format!("systemctl is-active {units} || true");
    match incus::exec(h.remote(), vm, &["sh", "-c", &script]).await {
        Ok(out) => {
            let mut states: Vec<bool> = out.lines().map(|l| l.trim() == "active").collect();
            states.resize(slots.max(0) as usize, false);
            states
        }
        Err(_) => vec![false; slots.max(0) as usize],
    }
}

async fn reconcile_local(state: &Admin, h: &Host) {
    if !super::local_pool_supported() {
        // Said on the host row rather than acted on: this executable cannot run
        // a worker, so spawning one would start another control plane.
        state.slots.lock().await.remove(&h.host_id);
        let _ = store::set_host_health(
            &state.db,
            h.host_id,
            Some("a local process pool only runs under `huntwell admin` — delete this host in production"),
        )
        .await;
        return;
    }
    let desired: i32 = if h.enabled { h.pool_size } else { 0 };
    let mut map = state.local_slots.lock().await;
    // Reap exited children so their slots respawn.
    map.retain(|_, child| !matches!(child.try_wait(), Ok(Some(_))));
    // Kill children beyond the desired count (scale-down, highest names last).
    let excess: Vec<(i64, String)> = map
        .keys()
        .filter(|(hid, name)| {
            *hid == h.host_id
                && name
                    .strip_prefix("hw-pool-")
                    .and_then(|n| n.parse::<i32>().ok())
                    .map(|n| n >= desired)
                    .unwrap_or(true)
        })
        .cloned()
        .collect();
    for key in excess {
        if let Some(mut child) = map.remove(&key) {
            if let Some(pid) = child.id() {
                let _ = tokio::process::Command::new("kill").arg("-TERM").arg(pid.to_string()).status().await;
            } else {
                let _ = child.kill().await;
            }
        }
    }
    // Spawn the missing ones.
    for i in 0..desired {
        let name = format!("hw-pool-{i}");
        let key = (h.host_id, name.clone());
        if map.contains_key(&key) {
            continue;
        }
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                let _ = store::set_host_health(&state.db, h.host_id, Some(&format!("locate own binary: {e}"))).await;
                return;
            }
        };
        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("worker-pool")
            .env("HOST_ID", h.host_id.to_string())
            .env("SLOT_NAME", &name)
            .stdin(Stdio::null())
            // Workers share the admin's console — their per-run chatter goes
            // to RunLog anyway, so this is just lifecycle lines.
            .kill_on_drop(true);
        match cmd.spawn() {
            Ok(child) => {
                tracing::info!(host = h.name, slot = name, "spawned local pool worker");
                map.insert(key, child);
            }
            Err(e) => {
                let _ = store::set_host_health(&state.db, h.host_id, Some(&format!("spawn worker: {e}"))).await;
                return;
            }
        }
    }
    let slots: Vec<SlotInfo> = map
        .keys()
        .filter(|(hid, _)| *hid == h.host_id)
        .map(|(_, name)| SlotInfo { name: name.clone(), ready: true, node: String::new() })
        .collect();
    let live: Vec<String> = slots.iter().map(|p| p.name.clone()).collect();
    drop(map);
    state.slots.lock().await.insert(h.host_id, slots);
    let _ = store::set_host_health(&state.db, h.host_id, None).await;
    if let Ok(runs) = store::unassign_lost_executions(&state.db, h.host_id, &live).await {
        for execution_id in &runs {
            let _ = store::append_route_log(&state.db, *execution_id, "requeued", Some(h.host_id), None, "assigned slot no longer exists").await;
        }
    }
}

pub async fn reconcile_all(state: &Admin) {
    let hosts = match store::list_hosts(&state.db).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("list hosts: {e:#}");
            return;
        }
    };
    // Parallel per host — one unreachable server must not stall the rest.
    let tasks: Vec<_> = hosts
        .into_iter()
        .map(|h| {
            let state = state.clone();
            tokio::spawn(async move { reconcile_host(&state, &h).await })
        })
        .collect();
    for t in tasks {
        let _ = t.await;
    }
}

pub fn spawn_reconcile(state: Admin) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(RECONCILE_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            reconcile_all(&state).await;
        }
    });
}

/// Kill one worker slot. Its run is failed cleanly by the heartbeat reaper and
/// the slot comes back under the same name, so pins survive.
pub async fn kill_slot(state: &Admin, h: &Host, slot: &str) -> Result<()> {
    if is_local(h) {
        if !slot.starts_with("hw-pool-") {
            bail!("refusing to kill {slot:?}: not a local pool worker");
        }
        let mut map = state.local_slots.lock().await;
        let Some(mut child) = map.remove(&(h.host_id, slot.to_string())) else {
            bail!("no such local worker {slot:?}");
        };
        if let Some(pid) = child.id() {
            let _ = tokio::process::Command::new("kill").arg("-TERM").arg(pid.to_string()).status().await;
        } else {
            let _ = child.kill().await;
        }
        return Ok(());
    }
    // Resolved against what is recorded rather than parsed from the name: a
    // name from the request is not trusted to pick the VM it acts on.
    let vms = store::list_host_vms(&state.db, h.host_id).await?;
    let (vm, n) = vms
        .iter()
        .filter(|v| v.is_worker())
        .find_map(|v| (1..=v.slots).find(|n| store::slot_name(&v.name, *n) == slot).map(|n| (v, n)))
        .ok_or_else(|| anyhow!("no worker slot {slot:?} on host {}", h.name))?;
    // SIGKILL to the slot's whole cgroup — agent and browser tool included —
    // and Restart=always brings the slot straight back.
    incus::exec(
        h.remote(),
        &vm.name,
        &["systemctl", "kill", "--signal=SIGKILL", &format!("huntwell-worker@{n}")],
    )
    .await
    .map(|_| ())
}

/// Host removal. The VMs must already be gone (store::delete_host refuses
/// otherwise); what is left is the edge container, if the app VM ever lived
/// here, and the trust — the remote and its pinned certificate. The edge goes
/// first, while the remote can still reach it. A local host instead terminates
/// its worker children.
pub async fn forget_host(state: &Admin, h: &Host) -> Result<()> {
    if is_local(h) {
        let mut map = state.local_slots.lock().await;
        let mine: Vec<(i64, String)> = map.keys().filter(|(hid, _)| *hid == h.host_id).cloned().collect();
        for key in mine {
            if let Some(mut child) = map.remove(&key) {
                if let Some(pid) = child.id() {
                    let _ = tokio::process::Command::new("kill").arg("-TERM").arg(pid.to_string()).status().await;
                } else {
                    let _ = child.kill().await;
                }
            }
        }
        return Ok(());
    }
    // Best effort: an unreachable host is still forgotten, and says why.
    if let Err(e) = incus_driver::remove_edge(h).await {
        tracing::warn!(host = h.name, "could not remove the edge container: {e:#}");
    }
    incus::remove_remote(h.remote()).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, priority: i32, status: &str, max_vms: i32) -> Host {
        Host {
            host_id: 0,
            name: name.into(),
            enabled: true,
            pool_size: 0,
            notes: String::new(),
            last_error: None,
            last_seen_at: None,
            created_at: chrono::Utc::now(),
            endpoint: format!("https://{name}:8443"),
            status: status.into(),
            base_image: "huntwell".into(),
            priority,
            max_vms,
            vm_cpu: 8,
            vm_memory: "16GiB".into(),
            vm_disk: "60GiB".into(),
            vm_slots: 10,
            ingress_domain: String::new(),
            edge_scheme: "https".into(),
            edge_port: 0,
            arch: "x86_64".into(),
            incus_version: String::new(),
            cpu_total: 0,
            memory_total_mb: 0,
        }
    }

    #[test]
    fn the_dev_host_is_the_one_with_no_incus_endpoint() {
        let mut h = host("local", 100, "Active", 0);
        h.endpoint = String::new();
        assert!(is_local(&h));
        assert!(!is_local(&host("yak-00", 100, "Active", 3)));
    }
}

//! The Incus client: every call the control plane makes to a host.
//!
//! The port of `../../parkriver/cmd/src/apps/admin/incus.rs`, kept close so a
//! fix there carries across.
//!
//! Shells out to the `incus` CLI rather than speaking the REST API directly.
//! The CLI already does what the API makes hard — `exec` and `file push` are
//! websocket and multipart dances there — and it is what an operator reaches
//! for when something is wrong, so what the control plane ran can be repeated
//! by hand, word for word.
//!
//! Hosts are *remotes* in one client configuration directory. It holds this
//! control plane's client certificate and each host's pinned server
//! certificate; a host is added once with a one-time trust token, and after
//! that every call is authenticated by the certificate. The remote's name is
//! the host's name.

use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tokio::sync::Mutex;

/// Where the client certificate and the remotes live.
///
/// `HUNTWELL_INCUS_CONF` names it; otherwise beside the admin's other state.
/// It is a credential: whoever holds it can drive every host.
pub fn conf_dir() -> PathBuf {
    match crate::config::get("HUNTWELL_INCUS_CONF") {
        Some(p) if !p.trim().is_empty() => PathBuf::from(p.trim()),
        _ => crate::config::data_dir().join("admin").join("incus"),
    }
}

/// `incus remote add` rewrites config.yml; two at once would lose one.
fn remote_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Run `incus` with this control plane's configuration. Returns stdout; an
/// error carries stderr, so a failure says what Incus said.
pub async fn run(args: &[&str]) -> Result<String> {
    let dir = conf_dir();
    let out = tokio::process::Command::new("incus")
        .env("INCUS_CONF", &dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| anyhow!("cannot run incus ({e}) — is the incus client installed on the admin server?"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(anyhow!("incus {} failed: {}", redact(args), stderr.trim()))
    }
}

/// An Incus API path on a host, decoded. `incus query` is the one CLI command
/// that returns the API's own JSON, which is what anything parsed should read.
pub async fn query(remote: &str, path: &str) -> Result<Value> {
    let out = run(&["query", &format!("{remote}:{path}")]).await?;
    serde_json::from_str(&out).with_context(|| format!("incus query {path}: not JSON"))
}

/// The one argument that can carry a secret is a trust token.
fn redact(args: &[&str]) -> String {
    let mut out = Vec::with_capacity(args.len());
    let mut hide = false;
    for a in args {
        if hide {
            out.push("<token>");
            hide = false;
        } else {
            out.push(a);
            hide = *a == "--token";
        }
    }
    out.join(" ")
}

pub async fn remote_exists(remote: &str) -> bool {
    run(&["remote", "list", "--format", "json"])
        .await
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| v.get(remote).is_some())
        .unwrap_or(false)
}

/// Register a host: trust this control plane's certificate with a one-time
/// token and pin the host's own certificate.
pub async fn add_remote(remote: &str, endpoint: &str, token: &str) -> Result<()> {
    let _guard = remote_lock().lock().await;
    let dir = conf_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    // The client key lands here on first use. Nobody but the admin reads it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    if remote_exists(remote).await {
        let _ = run(&["remote", "remove", remote]).await;
    }
    run(&["remote", "add", remote, endpoint, "--token", token.trim(), "--accept-certificate"]).await?;
    Ok(())
}

pub async fn remove_remote(remote: &str) {
    let _guard = remote_lock().lock().await;
    let _ = run(&["remote", "remove", remote]).await;
}

/// `remote:name`, the way every instance command addresses an instance.
pub fn target(remote: &str, instance: &str) -> String {
    format!("{remote}:{instance}")
}

pub async fn instance_exists(remote: &str, instance: &str) -> bool {
    query(remote, &format!("/1.0/instances/{instance}")).await.is_ok()
}

/// Running | Stopped | Frozen | Error, as Incus reports it.
pub async fn instance_status(remote: &str, instance: &str) -> Option<String> {
    query(remote, &format!("/1.0/instances/{instance}"))
        .await
        .ok()
        .and_then(|v| v["status"].as_str().map(String::from))
}

/// Run a command inside an instance; stdout on success.
pub async fn exec(remote: &str, instance: &str, command: &[&str]) -> Result<String> {
    let t = target(remote, instance);
    let mut args = vec!["exec", t.as_str(), "--"];
    args.extend_from_slice(command);
    run(&args).await
}

/// Write a file into an instance, owner root, with `mode`.
///
/// Through a private temporary file, because `incus file push` takes a path,
/// and removed straight after: the contents are often a VM's secrets.
pub async fn push_file(remote: &str, instance: &str, path: &str, contents: &[u8], mode: &str) -> Result<()> {
    use std::io::Write;
    let tmp = std::env::temp_dir().join(format!("hw-incus-{}-{}", std::process::id(), random_suffix()));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).with_context(|| format!("cannot stage {path}"))?;
        f.write_all(contents).with_context(|| format!("cannot stage {path}"))?;
    }
    let dest = format!("{remote}:{instance}{path}");
    let result = run(&[
        "file", "push", &tmp.to_string_lossy(), &dest,
        "--create-dirs", "--mode", mode, "--uid", "0", "--gid", "0",
    ])
    .await;
    let _ = std::fs::remove_file(&tmp);
    result.map(|_| ())
}

/// Push a local file, as root, without reading it into memory — for the
/// executables, which are far larger than a settings file.
pub async fn push_path(remote: &str, instance: &str, local: &std::path::Path, path: &str, mode: &str) -> Result<()> {
    let dest = format!("{remote}:{instance}{path}");
    run(&[
        "file", "push", &local.to_string_lossy(), &dest,
        "--create-dirs", "--mode", mode, "--uid", "0", "--gid", "0",
    ])
    .await
    .map(|_| ())
}

/// Wait until the instance's agent answers `exec`.
pub async fn wait_for_agent(remote: &str, instance: &str, seconds: u64) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    loop {
        if exec(remote, instance, &["true"]).await.is_ok() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "{instance} did not answer within {seconds}s — `incus console {remote}:{instance} --show-log` \
                 shows how far it got"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// The instance's address on the host bridge.
///
/// Matched by the NIC's MAC rather than taken as "the first address": a VM can
/// grow other interfaces, and those addresses mean nothing outside it.
pub async fn bridge_address(remote: &str, instance: &str) -> Option<String> {
    let info = query(remote, &format!("/1.0/instances/{instance}")).await.ok()?;
    let mac = info["config"]["volatile.eth0.hwaddr"].as_str()?.to_ascii_lowercase();
    let state = query(remote, &format!("/1.0/instances/{instance}/state")).await.ok()?;
    let nets = state["network"].as_object()?;
    for iface in nets.values() {
        if iface["hwaddr"].as_str().map(str::to_ascii_lowercase).as_deref() != Some(mac.as_str()) {
            continue;
        }
        for addr in iface["addresses"].as_array()? {
            if addr["family"] == "inet" && addr["scope"] == "global" {
                return addr["address"].as_str().map(String::from);
            }
        }
    }
    None
}

/// Enough randomness for a temp file name that another process cannot guess.
fn random_suffix() -> String {
    use rand::Rng;
    format!("{:016x}", rand::thread_rng().gen::<u64>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trust_token_never_reaches_an_error_message() {
        let shown = redact(&["remote", "add", "yak-00", "https://10.0.0.1:8443", "--token", "eyJzZWNyZXQ", "--accept-certificate"]);
        assert!(!shown.contains("eyJzZWNyZXQ"), "{shown}");
        assert!(shown.contains("--token <token>"), "{shown}");
        assert!(shown.contains("--accept-certificate"), "{shown}");
    }

    #[test]
    fn instances_are_addressed_through_their_remote() {
        assert_eq!(target("yak-00", "hw-app"), "yak-00:hw-app");
    }
}

//! Kubernetes dispatch for runs.
//!
//! In the k3d deployment a run is not a child process of the API server but a
//! **Job** the runs service creates through the in-cluster Kubernetes API. One
//! Job → one pod → one `huntwell run-worker --execution-id N`, which keeps the
//! one-run-per-process contract the pipeline relies on. We talk to the API with
//! `reqwest` and the pod's own ServiceAccount token, so there is no heavy client
//! dependency.
//!
//! The worker's stdout/stderr go to the pod log; we stream that log back into
//! `ExecutionLog` exactly as the local runner tails a child, so the existing SSE path
//! (which reads `ExecutionLog`) works unchanged across pods.

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::{App, LogEvent};
use crate::store;

const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// How runs execute. `RUN_DISPATCH` selects it:
/// - unset/anything else → `Local`: fork `huntwell run` as a child.
/// - `k8s`/`kubernetes` → `K8sJobs`: one in-cluster Kubernetes Job per run.
/// - `pool` → `Pool`: leave the run queued; the admin control plane assigns it
///   to a warm worker pod on some host, whose supervisor claims and runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Local,
    K8sJobs,
    Pool,
}

pub fn mode() -> Mode {
    match std::env::var("RUN_DISPATCH").as_deref() {
        Ok("k8s") | Ok("kubernetes") => Mode::K8sJobs,
        Ok("pool") => Mode::Pool,
        _ => Mode::Local,
    }
}

/// True when the runs service should create Jobs instead of forking a child.
pub fn enabled() -> bool {
    mode() == Mode::K8sJobs
}

struct Cluster {
    api: String,
    token: String,
    namespace: String,
    client: reqwest::Client,
}

impl Cluster {
    fn load() -> Result<Self> {
        let token = std::fs::read_to_string(format!("{SA_DIR}/token")).context("read service-account token")?;
        let ca = std::fs::read(format!("{SA_DIR}/ca.crt")).context("read service-account CA")?;
        let namespace = std::env::var("HUNTWELL_NAMESPACE")
            .ok()
            .or_else(|| std::fs::read_to_string(format!("{SA_DIR}/namespace")).ok())
            .unwrap_or_else(|| "huntwell".into())
            .trim()
            .to_string();
        // The API server is reachable in-cluster at this DNS name.
        let host = std::env::var("KUBERNETES_SERVICE_HOST").unwrap_or_else(|_| "kubernetes.default.svc".into());
        let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(&ca).context("parse cluster CA")?)
            .build()
            .context("build kube http client")?;
        Ok(Self { api: format!("https://{host}:{port}"), token: token.trim().to_string(), namespace, client })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client.request(method, format!("{}{path}", self.api)).bearer_auth(&self.token)
    }
}

fn job_name(execution_id: i64) -> String {
    format!("run-{execution_id}")
}

/// The Job manifest for one run. Image, service account and the shared
/// Secret/ConfigMap names come from the runs service's own environment, so the
/// worker inherits `DATABASE_URL` (core), `CURSOR_API_KEY`, the Browserbase keys
/// and `HUNTWELL_BROWSER=browserbase` without any of them being named here.
fn job_manifest(execution_id: i64) -> Value {
    let image = std::env::var("RUN_WORKER_IMAGE").unwrap_or_else(|_| "huntwell-worker:dev".into());
    let sa = std::env::var("RUN_WORKER_SA").unwrap_or_else(|_| "huntwell-worker".into());
    let secret = std::env::var("HUNTWELL_SECRET").unwrap_or_else(|_| "huntwell-secrets".into());
    let config = std::env::var("HUNTWELL_CONFIG").unwrap_or_else(|_| "huntwell-config".into());
    let name = job_name(execution_id);
    let labels = json!({ "app": "huntwell", "component": "run-worker", "execution-id": execution_id.to_string() });
    json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": { "name": name, "labels": labels },
        "spec": {
            "backoffLimit": 0,
            "ttlSecondsAfterFinished": 900,
            "activeDeadlineSeconds": 3600,
            "template": {
                "metadata": { "labels": labels },
                "spec": {
                    "restartPolicy": "Never",
                    "serviceAccountName": sa,
                    "containers": [{
                        "name": "run-worker",
                        "image": image,
                        // The worker image's entrypoint is `worker`; this is the
                        // subcommand it answers for a single run.
                        "args": ["run", "--execution-id", execution_id.to_string()],
                        "env": [{ "name": "HUNTWELL_EXECUTION_ID", "value": execution_id.to_string() }],
                        "envFrom": [
                            { "secretRef": { "name": secret } },
                            { "configMapRef": { "name": config } }
                        ]
                    }]
                }
            }
        }
    })
}

/// Creates the run's Job and spawns a task that streams its pod log into
/// `ExecutionLog` and records the final status. Mirrors what `runner::start` does with
/// a child process.
pub async fn start_job(state: &App, execution_id: i64) -> Result<()> {
    let cluster = Cluster::load()?;
    let body = job_manifest(execution_id);
    let resp = cluster
        .req(reqwest::Method::POST, &format!("/apis/batch/v1/namespaces/{}/jobs", cluster.namespace))
        .json(&body)
        .send()
        .await
        .context("create run Job")?;
    if !resp.status().is_success() {
        let code = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("create Job failed ({code}): {}", text.chars().take(300).collect::<String>());
    }

    let state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = track(&state, execution_id).await {
            tracing::warn!(execution_id, "run tracking ended: {e:#}");
            let _ = store::finish_execution(&state.db, execution_id, "failed", Some(-1)).await;
            let _ = state.log_tx.send(LogEvent { execution_id, seq: -1 });
        }
    });
    Ok(())
}

/// Deletes a run's Job (and its pod) — the k8s equivalent of killing the
/// process group. Best-effort; the Job's TTL cleans up anything left behind.
pub async fn cancel_job(execution_id: i64) -> Result<()> {
    let cluster = Cluster::load()?;
    let path = format!("/apis/batch/v1/namespaces/{}/jobs/{}?propagationPolicy=Background", cluster.namespace, job_name(execution_id));
    let _ = cluster.req(reqwest::Method::DELETE, &path).send().await;
    Ok(())
}

/// Waits for the run's pod, streams its log into `ExecutionLog`, then records the
/// terminal run status from the Job.
async fn track(state: &App, execution_id: i64) -> Result<()> {
    let cluster = Cluster::load()?;
    let selector = format!("execution-id%3D{execution_id}"); // label selector execution-id=<id>, url-encoded

    // Wait for the pod to appear and reach a state whose log we can read.
    let pod = wait_for_pod(&cluster, &selector).await?;
    store::set_execution_running(&state.db, execution_id, None).await.ok();

    // Stream the log line by line into RunLog.
    let log_path = format!(
        "/api/v1/namespaces/{}/pods/{}/log?follow=true&container=run-worker&timestamps=false",
        cluster.namespace, pod
    );
    let resp = cluster.req(reqwest::Method::GET, &log_path).send().await.context("open pod log stream")?;
    if resp.status().is_success() {
        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(_) => break,
            };
            buf.extend_from_slice(&bytes);
            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=nl).collect();
                let line = String::from_utf8_lossy(&line[..line.len().saturating_sub(1)]);
                let line: String = line.chars().take(4000).collect();
                if let Ok(seq) = store::append_execution_log(&state.db, execution_id, "stdout", &line).await {
                    let _ = state.log_tx.send(LogEvent { execution_id, seq });
                }
            }
        }
    }

    // The log stream ends when the pod stops. Read the Job's terminal status.
    let (status, code) = job_status(&cluster, execution_id).await;
    // If the worker already wrote its own final status, finish_execution is a no-op
    // for a terminal row; otherwise this records it.
    let _ = store::finish_execution(&state.db, execution_id, status, code).await;
    let _ = state.log_tx.send(LogEvent { execution_id, seq: -1 });
    Ok(())
}

async fn wait_for_pod(cluster: &Cluster, selector: &str) -> Result<String> {
    for _ in 0..120 {
        let path = format!("/api/v1/namespaces/{}/pods?labelSelector={}", cluster.namespace, selector);
        if let Ok(resp) = cluster.req(reqwest::Method::GET, &path).send().await {
            if let Ok(v) = resp.json::<Value>().await {
                if let Some(items) = v.get("items").and_then(Value::as_array) {
                    for pod in items {
                        let name = pod.pointer("/metadata/name").and_then(Value::as_str);
                        let phase = pod.pointer("/status/phase").and_then(Value::as_str).unwrap_or("");
                        if let Some(name) = name {
                            // Running or already finished — its log is readable.
                            if phase != "Pending" || pod_has_started(pod) {
                                return Ok(name.to_string());
                            }
                        }
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    anyhow::bail!("run pod did not start within 60s")
}

fn pod_has_started(pod: &Value) -> bool {
    pod.pointer("/status/containerStatuses/0/state/running").is_some()
        || pod.pointer("/status/containerStatuses/0/state/terminated").is_some()
}

/// Maps the Job's terminal condition to a run status.
async fn job_status(cluster: &Cluster, execution_id: i64) -> (&'static str, Option<i32>) {
    let path = format!("/apis/batch/v1/namespaces/{}/jobs/{}", cluster.namespace, job_name(execution_id));
    if let Ok(resp) = cluster.req(reqwest::Method::GET, &path).send().await {
        if let Ok(v) = resp.json::<Value>().await {
            let succeeded = v.pointer("/status/succeeded").and_then(Value::as_i64).unwrap_or(0);
            let failed = v.pointer("/status/failed").and_then(Value::as_i64).unwrap_or(0);
            if succeeded > 0 {
                return ("succeeded", Some(0));
            }
            if failed > 0 {
                return ("failed", Some(1));
            }
        }
    }
    // Job gone (deleted by cancel or TTL) — treat as finished without override.
    ("succeeded", Some(0))
}

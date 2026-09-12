//! kubectl-backed host operations.
//!
//! Follows the yak-admin pattern (kubeconfig-as-data, shell out to kubectl)
//! with its weaknesses fixed: kubeconfigs are written once at
//! registration/startup rather than per operation, reconcile runs in parallel
//! per host, resources come from Host columns, and a kubectl transport error
//! is recorded on the Host row — never conflated with "zero pods".
//!
//! Manifests are built as a JSON `v1/List` and piped to `kubectl apply -f -`:
//! JSON needs no quoting rules, so operator-supplied values (image names,
//! secrets) cannot break out of the document.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::{Admin, PodInfo};
use crate::store::{self, Host};

pub const NAMESPACE: &str = "huntwell";
const RECONCILE_EVERY: Duration = Duration::from_secs(20);

fn kubeconfig_dir() -> PathBuf {
    crate::config::data_dir().join("admin").join("kubeconfigs")
}

pub fn kubeconfig_path(host_id: i64) -> PathBuf {
    kubeconfig_dir().join(format!("host-{host_id}.yaml"))
}

/// A process-backed host: its "pods" are worker-pool children of the admin
/// itself. Dev mode — the whole routing path with no Kubernetes anywhere.
pub fn is_local(h: &Host) -> bool {
    h.kubeconfig_yaml.trim() == "local"
}

/// Materializes the host's kubeconfig to a 0600 file. Called at
/// registration/update and admin startup — not per operation.
pub fn write_kubeconfig(h: &Host) -> Result<PathBuf> {
    if is_local(h) {
        return Ok(PathBuf::new());
    }
    let dir = kubeconfig_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = kubeconfig_path(h.host_id);
    std::fs::write(&path, &h.kubeconfig_yaml).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

/// Runs kubectl against one host; Err carries stderr. `stdin_doc` is piped in
/// when given (for `apply -f -`).
async fn kubectl(h: &Host, args: &[&str], stdin_doc: Option<&str>) -> Result<String> {
    let mut cmd = Command::new("kubectl");
    cmd.arg("--kubeconfig").arg(kubeconfig_path(h.host_id));
    if let Some(ctx) = h.kube_context.as_deref().filter(|c| !c.trim().is_empty()) {
        cmd.arg("--context").arg(ctx);
    }
    cmd.args(args).stdin(if stdin_doc.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().context("spawn kubectl (is it installed on the admin server?)")?;
    if let Some(doc) = stdin_doc {
        let mut si = child.stdin.take().expect("piped stdin");
        si.write_all(doc.as_bytes()).await?;
        drop(si);
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("{}", err.trim().chars().take(500).collect::<String>()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Cheap reachability check used at registration, so a bad kubeconfig is
/// rejected with kubectl's own words instead of surfacing later.
pub async fn probe(h: &Host) -> Result<()> {
    if is_local(h) {
        return Ok(());
    }
    kubectl(h, &["version", "--request-timeout=5s", "-o", "json"], None).await.map(|_| ())
}

/// What the pool pods need, materialized from the admin server's own env.
/// The database URL must be reachable FROM the host's pods — set
/// `HUNTWELL_POOL_DATABASE_URL` when the admin's own URL is loopback.
fn pool_env() -> (String, Vec<(String, String)>, Vec<(String, String)>) {
    let db_url = crate::config::get("HUNTWELL_POOL_DATABASE_URL")
        .or_else(|| crate::config::get("HUNTWELL_DATABASE_URL"))
        .unwrap_or_default();
    let mut secrets = vec![("HUNTWELL_DATABASE_URL".to_string(), db_url.clone())];
    for k in ["CURSOR_API_KEY", "BROWSERBASE_API_KEY", "BROWSERBASE_PROJECT_ID", "HUNTWELL_SESSION_SECRET"] {
        if let Some(v) = crate::config::get(k) {
            secrets.push((k.to_string(), v));
        }
    }
    // The object store is shared state, not a service's private scratch: an
    // `assets` run writes bytes a website pod later serves. Without these the
    // worker falls back to a directory inside its own pod, which the website
    // cannot read and which dies with the pod.
    for k in ["HUNTWELL_S3_ACCESS_KEY", "HUNTWELL_S3_SECRET_KEY"] {
        if let Some(v) = crate::config::get(k) {
            secrets.push((k.to_string(), v));
        }
    }
    let mut config = vec![
        ("HUNTWELL_DATA_DIR".to_string(), "/tmp/huntwell-data".to_string()),
        ("HUNTWELL_BROWSER".to_string(), crate::config::get_or("HUNTWELL_BROWSER", "browserbase")),
    ];
    for k in ["HUNTWELL_SELL_USD_PER_MTOKEN", "HUNTWELL_MODEL_MARKUP", "BROWSERBASE_REGION", "BROWSERBASE_TIMEOUT_S", "HUNTWELL_S3_BUCKET"] {
        if let Some(v) = crate::config::get(k) {
            config.push((k.to_string(), v));
        }
    }
    // The object store **as the pods reach it**, the same distinction
    // HUNTWELL_POOL_DATABASE_URL exists for. The admin's own endpoint is
    // often loopback or a private address that means nothing inside a remote
    // cluster; a pod handed one starts fine and fails on the first asset.
    if let Some(v) = crate::config::get("HUNTWELL_POOL_S3_ENDPOINT").or_else(|| crate::config::get("HUNTWELL_S3_ENDPOINT")) {
        config.push(("HUNTWELL_S3_ENDPOINT".to_string(), v));
    }
    (db_url, secrets, config)
}

/// Base64, standard alphabet, padded. Only the registry Secret's `auth` field
/// needs it; pulling in a crate would be more code than the encoder.
fn b64(input: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for c in input.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if c.len() > 1 { A[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if c.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Credentials for the registry the pool image is pulled from, if the operator
/// configured one.
///
/// A remote cluster cannot see an image built on the admin server: there is no
/// `k3d image import` across the network. So unless someone imported the image
/// onto that host by hand, it has to come from a registry, and a private one
/// needs a pull secret on every host.
fn registry() -> Option<(String, String, String)> {
    let nonempty = |k: &str| crate::config::get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    Some((
        nonempty("HUNTWELL_REGISTRY_SERVER")?,
        nonempty("HUNTWELL_REGISTRY_USERNAME")?,
        nonempty("HUNTWELL_REGISTRY_PASSWORD")?,
    ))
}

/// The `.dockerconfigjson` a `kubernetes.io/dockerconfigjson` Secret holds.
fn dockerconfigjson(server: &str, user: &str, pass: &str) -> String {
    json!({ "auths": { server: {
        "username": user,
        "password": pass,
        "auth": b64(format!("{user}:{pass}").as_bytes()),
    } } })
    .to_string()
}

/// `Always` for a registry image, so re-pushing a tag actually rolls the pool;
/// `IfNotPresent` for a bare name like `huntwell-worker:dev`, which exists only
/// because someone imported it into that cluster and could never be pulled —
/// `Always` there would put every pod in ErrImagePull. Override with
/// HUNTWELL_POOL_PULL_POLICY.
fn pull_policy(image: &str) -> String {
    if let Some(p) = crate::config::get("HUNTWELL_POOL_PULL_POLICY") {
        if !p.trim().is_empty() {
            return p.trim().to_string();
        }
    }
    let first = image.split('/').next().unwrap_or("");
    let has_registry_host =
        image.contains('/') && (first.contains('.') || first.contains(':') || first == "localhost");
    if has_registry_host { "Always".into() } else { "IfNotPresent".into() }
}

/// Settings only the application services need — the pool pods have no use for
/// a public URL or a mail key, and a worker host that is not serving anyone
/// should not be carrying them.
///
/// Two of these are decisions rather than pass-throughs. `RUN_DISPATCH=pool`
/// is what makes the deployed website queue executions for this admin to place
/// instead of forking children inside its own pod; `HUNTWELL_DEV=0` is what
/// keeps the `Secure` flag on session cookies. Both are wrong-by-default in a
/// way nothing reports: runs that execute nowhere near the pool, and a cookie
/// a browser will hand over in cleartext.
fn app_env() -> (Vec<(String, String)>, Vec<(String, String)>) {
    let mut secrets: Vec<(String, String)> = Vec::new();
    for k in ["HUNTWELL_MAIL_API_KEY", "HUNTWELL_MAIL_FROM", "STRIPE_SECRET_KEY", "STRIPE_WEBHOOK_SECRET"] {
        if let Some(v) = crate::config::get(k) {
            secrets.push((k.to_string(), v));
        }
    }
    let mut config = vec![
        ("RUN_DISPATCH".to_string(), "pool".to_string()),
        ("DRAFT_DISPATCH".to_string(), "queue".to_string()),
        ("HUNTWELL_DEV".to_string(), "0".to_string()),
        ("HUNTWELL_OPEN_SIGNUP".to_string(), crate::config::get_or("HUNTWELL_OPEN_SIGNUP", "0")),
        ("HUNTWELL_NATS_URL".to_string(), format!("nats://nats.{NAMESPACE}.svc.cluster.local:4222")),
    ];
    if let Some(v) = crate::config::get("HUNTWELL_PUBLIC_URL") {
        config.push(("HUNTWELL_PUBLIC_URL".to_string(), v));
    }
    (secrets, config)
}

/// The image for one service, derived from the host's worker image: same
/// registry, same repository path, same tag, only the name changed.
///
/// One field on the host stays one field. Six image names an operator can edit
/// independently is six ways to deploy half a release, and the half that is
/// stale is whichever one nobody looked at.
fn service_image(worker_image: &str, service: &str) -> String {
    // A colon in the last segment is a tag; one before a '/' is a registry
    // port — `localhost:5111/huntwell-worker` has no tag at all.
    let (path, tag) = match worker_image.rsplit_once(':') {
        Some((p, t)) if !t.contains('/') => (p, format!(":{t}")),
        _ => (worker_image, String::new()),
    };
    let prefix = match path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    };
    format!("{prefix}huntwell-{service}{tag}")
}

/// The hostname an Ingress rule should match, from the public URL. None means
/// a catch-all rule, which is what a cluster reached through a tunnel wants:
/// the tunnel already decided which hostname arrives.
fn public_ingress_host() -> Option<String> {
    let url = crate::config::get("HUNTWELL_PUBLIC_URL")?;
    let rest = url.split("://").nth(1).unwrap_or(&url);
    let host = rest.split('/').next()?.split(':').next()?.trim().to_string();
    (!host.is_empty()).then_some(host)
}

/// One application Deployment. They differ only in name, image, port and size:
/// every one reads the same Secret and ConfigMap, and every one answers
/// `/healthz` on its own port (`svc::addr_for` binds 0.0.0.0 by default, so a
/// probe reaching the pod IP is answered without any address configuration).
#[allow(clippy::too_many_arguments)]
fn app_deployment(
    name: &str,
    image: &str,
    pull: &str,
    port: u16,
    replicas: i32,
    cpu: (&str, &str),
    mem: (&str, &str),
    pull_secrets: &[Value],
    env: Value,
) -> Value {
    json!({ "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": { "name": name, "namespace": NAMESPACE, "labels": { "app": name } },
        "spec": {
          "replicas": replicas,
          "selector": { "matchLabels": { "app": name } },
          "template": {
            "metadata": { "labels": { "app": name } },
            "spec": {
              "imagePullSecrets": pull_secrets,
              "containers": [{
                "name": name,
                "image": image,
                "imagePullPolicy": pull,
                "ports": [{ "containerPort": port }],
                "env": env,
                "envFrom": [
                  { "secretRef": { "name": "huntwell-secrets" } },
                  { "configMapRef": { "name": "huntwell-config" } }
                ],
                "readinessProbe": {
                  "httpGet": { "path": "/healthz", "port": port },
                  "initialDelaySeconds": 3, "periodSeconds": 5
                },
                "resources": {
                  "requests": { "cpu": cpu.0, "memory": mem.0 },
                  "limits":   { "cpu": cpu.1, "memory": mem.1 }
                }
              }]
            }
          }
        } })
}

/// A ClusterIP Service in front of one Deployment.
fn app_service(name: &str, port: u16) -> Value {
    json!({ "apiVersion": "v1", "kind": "Service",
        "metadata": { "name": name, "namespace": NAMESPACE },
        "spec": { "selector": { "app": name }, "ports": [{ "port": 80, "targetPort": port }] } })
}

/// The application half of a host's desired state: the bus, the four services,
/// and the one Ingress that lets people in.
///
/// Deliberately absent: the admin itself (it drives this cluster from outside,
/// and an admin that deploys itself cannot be trusted to finish the rollout),
/// Postgres (it is the admin's own database, reached over the network, which is
/// what makes a second host possible at all), and any ServiceAccount or RBAC —
/// in `pool` dispatch the website never calls the Kubernetes API, so it needs
/// no rights in the cluster it runs in.
fn app_items(h: &Host, pull: &str, pull_secrets: &[Value]) -> Vec<Value> {
    let img = |svc: &str| service_image(&h.image, svc);
    let mut items = vec![
        // The bus. JetStream with a volume behind it, because core NATS drops a
        // message that has no listener at that instant — which is exactly the
        // moment a service is rolling.
        json!({ "apiVersion": "v1", "kind": "Service",
            "metadata": { "name": "nats", "namespace": NAMESPACE },
            "spec": { "selector": { "app": "nats" }, "ports": [
                { "name": "client", "port": 4222, "targetPort": 4222 },
                { "name": "monitor", "port": 8222, "targetPort": 8222 }] } }),
        json!({ "apiVersion": "apps/v1", "kind": "StatefulSet",
            "metadata": { "name": "nats", "namespace": NAMESPACE, "labels": { "app": "nats" } },
            "spec": {
              "serviceName": "nats",
              "replicas": 1,
              "selector": { "matchLabels": { "app": "nats" } },
              "template": {
                "metadata": { "labels": { "app": "nats" } },
                "spec": { "containers": [{
                    "name": "nats",
                    "image": "nats:2.14-alpine",
                    "args": ["--jetstream", "--store_dir=/data", "--http_port=8222"],
                    "ports": [{ "name": "client", "containerPort": 4222 },
                              { "name": "monitor", "containerPort": 8222 }],
                    "volumeMounts": [{ "name": "data", "mountPath": "/data" }],
                    "readinessProbe": { "httpGet": { "path": "/healthz", "port": 8222 },
                                        "initialDelaySeconds": 2, "periodSeconds": 5 },
                    "resources": { "requests": { "cpu": "50m", "memory": "64Mi" },
                                   "limits": { "cpu": "500m", "memory": "512Mi" } }
                }] }
              },
              "volumeClaimTemplates": [{
                "metadata": { "name": "data" },
                "spec": { "accessModes": ["ReadWriteOnce"], "resources": { "requests": { "storage": "1Gi" } } }
              }]
            } }),
        app_service("website", 8611),
    ];

    // The website is the only one anything connects to; the other three read
    // the queue and answer nothing but their own probe.
    items.push(app_deployment(
        "website",
        &img("website"),
        pull,
        8611,
        h.web_replicas.max(1),
        ("100m", "1"),
        ("128Mi", "1Gi"),
        pull_secrets,
        json!([{ "name": "HUNTWELL_ADDR", "value": "0.0.0.0:8611" }]),
    ));
    // Drafting runs the agent, which is why this one asks for room.
    items.push(app_deployment(
        "planning",
        &img("planning"),
        pull,
        8612,
        1,
        ("200m", "2"),
        ("512Mi", "3Gi"),
        pull_secrets,
        json!([]),
    ));
    items.push(app_deployment(
        "scheduling",
        &img("scheduling"),
        pull,
        8614,
        1,
        ("25m", "250m"),
        ("64Mi", "256Mi"),
        pull_secrets,
        json!([]),
    ));
    items.push(app_deployment(
        "notification",
        &img("notification"),
        pull,
        8615,
        1,
        ("25m", "250m"),
        ("64Mi", "256Mi"),
        pull_secrets,
        json!([]),
    ));

    // One rule to one backend: the website serves the UI, /api, /v1 and /dl in
    // one process, so there is no per-prefix routing to keep in step with the
    // code — and nothing that can fall through to the SPA and answer a failed
    // API call with 200 and an HTML page.
    let mut rule = json!({ "http": { "paths": [{
        "path": "/", "pathType": "Prefix",
        "backend": { "service": { "name": "website", "port": { "number": 80 } } } }] } });
    if let Some(host) = public_ingress_host() {
        rule["host"] = json!(host);
    }
    items.push(json!({ "apiVersion": "networking.k8s.io/v1", "kind": "Ingress",
        "metadata": { "name": "huntwell", "namespace": NAMESPACE },
        "spec": {
          "ingressClassName": crate::config::get_or("HUNTWELL_INGRESS_CLASS", "traefik"),
          "rules": [rule]
        } }));
    items
}

/// The desired state of one host, as a JSON List kubectl can apply:
/// Namespace + Secret + ConfigMap, then the application (only when the host
/// runs it), then the headless Service and the `hw-pool` StatefulSet.
/// StatefulSet, not Deployment, for the pool: stable pod names `hw-pool-0..N-1`
/// are the assignment and pinning key, and survive pod kills.
fn manifest(h: &Host) -> Value {
    build_manifest(h, registry(), &pull_policy(&h.image))
}

/// The manifest itself, with the two environment-derived decisions passed in
/// so both the registry and the no-registry shape are testable without
/// touching process-global env.
fn build_manifest(h: &Host, reg: Option<(String, String, String)>, pull: &str) -> Value {
    let (_, mut secrets, mut config) = pool_env();
    // A host that serves the app needs everything a pool host needs and more;
    // one Secret and one ConfigMap carry both, so a pod cannot be looking at a
    // different generation of the same setting than its neighbour.
    if h.runs_services {
        let (app_secrets, app_config) = app_env();
        secrets.extend(app_secrets);
        config.extend(app_config);
    }
    let secret_data: serde_json::Map<String, Value> = secrets.into_iter().map(|(k, v)| (k, json!(v))).collect();
    let config_data: serde_json::Map<String, Value> = config.into_iter().map(|(k, v)| (k, json!(v))).collect();

    // Built as a Vec so the optional registry Secret lands in the list ahead of
    // the StatefulSet that references it: kubectl applies a List in order, and
    // a pod scheduled before its pull secret exists sits in ErrImagePull until
    // something retries it.
    let mut items: Vec<Value> = vec![
        json!({ "apiVersion": "v1", "kind": "Namespace", "metadata": { "name": NAMESPACE } }),
        json!({ "apiVersion": "v1", "kind": "Secret", "metadata": { "name": "huntwell-secrets", "namespace": NAMESPACE },
                "type": "Opaque", "stringData": secret_data }),
        json!({ "apiVersion": "v1", "kind": "ConfigMap", "metadata": { "name": "huntwell-config", "namespace": NAMESPACE },
                "data": config_data }),
    ];

    // A private registry adds one Secret and one imagePullSecrets reference;
    // with none configured the manifest is exactly what it was.
    let mut pull_secrets: Vec<Value> = Vec::new();
    if let Some((server, user, pass)) = reg {
        items.push(json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": { "name": "huntwell-registry", "namespace": NAMESPACE },
            "type": "kubernetes.io/dockerconfigjson",
            "stringData": { ".dockerconfigjson": dockerconfigjson(&server, &user, &pass) }
        }));
        pull_secrets.push(json!({ "name": "huntwell-registry" }));
    }

    // The application, when this cluster is serving it. Ahead of the pool for
    // no reason but readability — kubectl applies a List in order and nothing
    // here references anything below it.
    if h.runs_services {
        items.extend(app_items(h, pull, &pull_secrets));
    }

    items.push(json!({ "apiVersion": "v1", "kind": "Service",
        "metadata": { "name": "hw-pool", "namespace": NAMESPACE },
        "spec": { "clusterIP": "None", "selector": { "app": "hw-pool" }, "ports": [{ "port": 1, "name": "none" }] } }));
    items.push(json!({ "apiVersion": "apps/v1", "kind": "StatefulSet",
        "metadata": { "name": "hw-pool", "namespace": NAMESPACE, "labels": { "app": "hw-pool" } },
        "spec": {
          "serviceName": "hw-pool",
          "replicas": h.pool_size,
          "podManagementPolicy": "Parallel",
          // Spread across the machines in the cluster. Nothing otherwise stops
          // Kubernetes putting every pool pod on one node — a second machine
          // then joins, reports Ready, and runs nothing, which looks exactly
          // like the pool being busy.
          //
          // ScheduleAnyway, not DoNotSchedule: one node is the normal starting
          // point, and a hard constraint there would leave every pod after the
          // first Pending for ever.
          "topologySpreadConstraints": [{
            "maxSkew": 1,
            "topologyKey": "kubernetes.io/hostname",
            "whenUnsatisfiable": "ScheduleAnyway",
            "labelSelector": { "matchLabels": { "app": "hw-pool" } }
          }],
          "selector": { "matchLabels": { "app": "hw-pool" } },
          "template": {
            "metadata": { "labels": { "app": "hw-pool" } },
            "spec": {
              "terminationGracePeriodSeconds": 20,
              "imagePullSecrets": pull_secrets,
              "containers": [{
                "name": "worker",
                "image": h.image,
                "imagePullPolicy": pull,
                "args": ["worker-pool"],
                "resources": {
                  "requests": { "cpu": h.cpu_request, "memory": h.mem_request },
                  "limits":   { "cpu": h.cpu_limit,   "memory": h.mem_limit }
                },
                "env": [
                  { "name": "HOST_ID", "value": h.host_id.to_string() },
                  { "name": "POD_NAME", "valueFrom": { "fieldRef": { "fieldPath": "metadata.name" } } }
                ],
                "envFrom": [
                  { "secretRef": { "name": "huntwell-secrets" } },
                  { "configMapRef": { "name": "huntwell-config" } }
                ]
              }]
            }
          }
        } }));

    json!({ "apiVersion": "v1", "kind": "List", "items": items })
}

/// The process backend: keep exactly `pool_size` worker-pool children alive,
/// named like StatefulSet pods so assignment/pinning work identically. A dead
/// or killed child is respawned under the same name on the next tick — the
/// local equivalent of the StatefulSet recreating a pod.
async fn reconcile_local(state: &Admin, h: &Host) {
    let desired: i32 = if h.enabled { h.pool_size } else { 0 };
    let mut map = state.local_pods.lock().await;
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
            .env("POD_NAME", &name)
            .stdin(Stdio::null())
            // Workers share the admin's console — their per-run chatter goes
            // to RunLog anyway, so this is just lifecycle lines.
            .kill_on_drop(true);
        match cmd.spawn() {
            Ok(child) => {
                tracing::info!(host = h.name, pod = name, "spawned local pool worker");
                map.insert(key, child);
            }
            Err(e) => {
                let _ = store::set_host_health(&state.db, h.host_id, Some(&format!("spawn worker: {e}"))).await;
                return;
            }
        }
    }
    let pods: Vec<PodInfo> = map
        .keys()
        .filter(|(hid, _)| *hid == h.host_id)
        .map(|(_, name)| PodInfo { name: name.clone(), ready: true, node: String::new() })
        .collect();
    let live: Vec<String> = pods.iter().map(|p| p.name.clone()).collect();
    drop(map);
    state.pods.lock().await.insert(h.host_id, pods);
    let _ = store::set_host_health(&state.db, h.host_id, None).await;
    if let Ok(runs) = store::unassign_lost_executions(&state.db, h.host_id, &live).await {
        for execution_id in &runs {
            let _ = store::append_route_log(&state.db, *execution_id, "requeued", Some(h.host_id), None, "assigned pod no longer exists").await;
        }
    }
}

/// Applies desired state and refreshes the pod cache for one host. Any error
/// lands in Host.LastError; success clears it and bumps LastSeenAt.
pub async fn reconcile_host(state: &Admin, h: &Host) {
    if is_local(h) {
        return reconcile_local(state, h).await;
    }
    let result = async {
        if h.enabled {
            let doc = serde_json::to_string(&manifest(h))?;
            kubectl(h, &["apply", "-f", "-"], Some(&doc)).await.context("apply manifests")?;
        } else {
            // Disabled host: scale the pool away but keep the namespace, so
            // re-enabling is instant and history keeps its meaning.
            //
            // The application, if this host serves it, is left running. This
            // switch means "place no runs here"; taking the product offline is
            // not something a compute-capacity checkbox should do, and an
            // operator who wants that has `runs_services` and `kubectl`.
            let _ = kubectl(h, &["-n", NAMESPACE, "scale", "statefulset/hw-pool", "--replicas=0"], None).await;
        }
        let pods_json = kubectl(h, &["-n", NAMESPACE, "get", "pods", "-l", "app=hw-pool", "-o", "json"], None)
            .await
            .context("list pool pods")?;
        let v: Value = serde_json::from_str(&pods_json)?;
        let pods: Vec<PodInfo> = v["items"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|p| {
                        let name = p["metadata"]["name"].as_str()?.to_string();
                        let node = p["spec"]["nodeName"].as_str().unwrap_or("").to_string();
                        let deleting = !p["metadata"]["deletionTimestamp"].is_null();
                        let ready = p["status"]["conditions"]
                            .as_array()
                            .map(|cs| cs.iter().any(|c| c["type"] == "Ready" && c["status"] == "True"))
                            .unwrap_or(false);
                        Some(PodInfo { name, ready: ready && !deleting, node })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok::<Vec<PodInfo>, anyhow::Error>(pods)
    }
    .await;

    match result {
        Ok(pods) => {
            let live: Vec<String> = pods.iter().map(|p| p.name.clone()).collect();
            state.pods.lock().await.insert(h.host_id, pods);
            let _ = store::set_host_health(&state.db, h.host_id, None).await;
            // Queued runs assigned to pods that no longer exist re-enter placement.
            if let Ok(runs) = store::unassign_lost_executions(&state.db, h.host_id, &live).await {
                for execution_id in &runs {
                    let _ = store::append_route_log(&state.db, *execution_id, "requeued", Some(h.host_id), None, "assigned pod no longer exists").await;
                }
                if !runs.is_empty() {
                    tracing::info!(host = h.name, n = runs.len(), "re-queued runs from lost pods");
                }
            }
        }
        Err(e) => {
            // Transport failure ≠ zero pods: keep the stale cache out of
            // placement by clearing it, and record the message for the UI.
            state.pods.lock().await.remove(&h.host_id);
            let _ = store::set_host_health(&state.db, h.host_id, Some(&format!("{e:#}"))).await;
            tracing::warn!(host = h.name, "reconcile failed: {e:#}");
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
    // Parallel per host — one unreachable cluster must not stall the rest.
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

/// Kill one pool pod. The supervisor's SIGTERM path fails its run cleanly and
/// the StatefulSet (or the local reconcile) recreates the pod under the same
/// name, so pins survive.
pub async fn kill_pod(state: &Admin, h: &Host, pod: &str) -> Result<()> {
    if !pod.starts_with("hw-pool-") {
        anyhow::bail!("refusing to delete non-pool pod {pod:?}");
    }
    if is_local(h) {
        let mut map = state.local_pods.lock().await;
        let Some(mut child) = map.remove(&(h.host_id, pod.to_string())) else {
            anyhow::bail!("no such local worker {pod:?}");
        };
        if let Some(pid) = child.id() {
            let _ = tokio::process::Command::new("kill").arg("-TERM").arg(pid.to_string()).status().await;
        } else {
            let _ = child.kill().await;
        }
        return Ok(());
    }
    kubectl(h, &["-n", NAMESPACE, "delete", "pod", pod, "--grace-period=10", "--wait=false"], None).await.map(|_| ())
}

/// Host removal: drop the namespace (pods, secret, config all go with it).
/// A local host instead terminates its worker children.
pub async fn delete_namespace(state: &Admin, h: &Host) -> Result<()> {
    if is_local(h) {
        let mut map = state.local_pods.lock().await;
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
    kubectl(h, &["delete", "namespace", NAMESPACE, "--wait=false", "--ignore-not-found"], None).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        // RFC 4648 §10 — the padding cases are the ones that break hand-rolled
        // encoders, and a wrong `auth` field is a registry 401 at pod start.
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foob"), "Zm9vYg==");
        assert_eq!(b64(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_non_ascii_bytes() {
        assert_eq!(b64(&[0xff, 0xfe, 0xfd]), "//79");
        assert_eq!(b64(&[0x00, 0x00, 0x00]), "AAAA");
    }

    #[test]
    fn dockerconfig_carries_the_auth_kubelet_reads() {
        let doc = dockerconfigjson("reg.example.com", "robot", "hunter2");
        let v: Value = serde_json::from_str(&doc).unwrap();
        let entry = &v["auths"]["reg.example.com"];
        assert_eq!(entry["username"], "robot");
        assert_eq!(entry["password"], "hunter2");
        assert_eq!(entry["auth"], b64(b"robot:hunter2"));
    }

    #[test]
    fn pull_policy_distinguishes_a_registry_image_from_an_imported_one() {
        // Imported by hand into the cluster — `Always` would be ErrImagePull.
        assert_eq!(pull_policy("huntwell-worker:dev"), "IfNotPresent");
        assert_eq!(pull_policy("huntwell-worker"), "IfNotPresent");
        // A bare Docker Hub name has no registry host either.
        assert_eq!(pull_policy("myorg/huntwell-worker:v3"), "IfNotPresent");
        // Real registries: pull, so re-pushing a tag rolls the pool.
        assert_eq!(pull_policy("ghcr.io/mh/huntwell-worker:v3"), "Always");
        assert_eq!(pull_policy("registry.example.com/huntwell-worker:v3"), "Always");
        assert_eq!(pull_policy("localhost:5000/huntwell-worker:dev"), "Always");
    }

    fn host_with_image(image: &str) -> Host {
        Host {
            host_id: 7,
            name: "h".into(),
            kubeconfig_yaml: "apiVersion: v1".into(),
            kube_context: None,
            enabled: true,
            pool_size: 3,
            cpu_request: "500m".into(),
            cpu_limit: "2".into(),
            mem_request: "512Mi".into(),
            mem_limit: "1Gi".into(),
            image: image.into(),
            runs_services: false,
            web_replicas: 1,
            notes: String::new(),
            last_error: None,
            last_seen_at: None,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn manifest_without_a_registry_has_no_pull_secret() {
        let doc = build_manifest(&host_with_image("huntwell-worker:dev"), None, "IfNotPresent");
        let items = doc["items"].as_array().unwrap();
        assert!(items.iter().all(|i| i["metadata"]["name"] != "huntwell-registry"));

        let sts = items.iter().find(|i| i["kind"] == "StatefulSet").unwrap();
        let spec = &sts["spec"]["template"]["spec"];
        assert_eq!(spec["imagePullSecrets"].as_array().unwrap().len(), 0);
        assert_eq!(spec["containers"][0]["imagePullPolicy"], "IfNotPresent");
        assert_eq!(spec["containers"][0]["image"], "huntwell-worker:dev");
    }

    #[test]
    fn the_statefulset_still_carries_what_the_pool_worker_needs() {
        let doc = build_manifest(&host_with_image("huntwell-worker:dev"), None, "IfNotPresent");
        let items = doc["items"].as_array().unwrap();
        let sts = items.iter().find(|i| i["kind"] == "StatefulSet").unwrap();
        assert_eq!(sts["spec"]["replicas"], 3);
        assert_eq!(sts["spec"]["serviceName"], "hw-pool");

        let c = &sts["spec"]["template"]["spec"]["containers"][0];
        assert_eq!(c["args"][0], "worker-pool");
        // HOST_ID + POD_NAME are how the supervisor knows which runs are its
        // own; losing either strands every run assigned to this host.
        let env = c["env"].as_array().unwrap();
        assert_eq!(env.iter().find(|e| e["name"] == "HOST_ID").unwrap()["value"], "7");
        assert_eq!(
            env.iter().find(|e| e["name"] == "POD_NAME").unwrap()["valueFrom"]["fieldRef"]["fieldPath"],
            "metadata.name"
        );
    }

    #[test]
    fn a_private_registry_adds_a_pull_secret_the_pods_reference() {
        let h = host_with_image("ghcr.io/mh/huntwell-worker:v3");
        let doc = build_manifest(
            &h,
            Some(("ghcr.io".into(), "robot".into(), "hunter2".into())),
            "Always",
        );
        let items = doc["items"].as_array().unwrap();

        let sec = items.iter().find(|i| i["metadata"]["name"] == "huntwell-registry").unwrap();
        assert_eq!(sec["type"], "kubernetes.io/dockerconfigjson");
        let inner: Value =
            serde_json::from_str(sec["stringData"][".dockerconfigjson"].as_str().unwrap()).unwrap();
        assert_eq!(inner["auths"]["ghcr.io"]["auth"], b64(b"robot:hunter2"));

        let sts = items.iter().find(|i| i["kind"] == "StatefulSet").unwrap();
        let spec = &sts["spec"]["template"]["spec"];
        assert_eq!(spec["imagePullSecrets"][0]["name"], "huntwell-registry");
        assert_eq!(spec["containers"][0]["imagePullPolicy"], "Always");
    }

    #[test]
    fn the_pull_secret_is_applied_before_the_statefulset_that_uses_it() {
        // kubectl applies a List in order. A pod created before its pull secret
        // exists lands in ErrImagePull and only recovers on a later retry.
        let doc = build_manifest(
            &host_with_image("ghcr.io/mh/huntwell-worker:v3"),
            Some(("ghcr.io".into(), "u".into(), "p".into())),
            "Always",
        );
        let items = doc["items"].as_array().unwrap();
        let sec = items.iter().position(|i| i["metadata"]["name"] == "huntwell-registry").unwrap();
        let sts = items.iter().position(|i| i["kind"] == "StatefulSet").unwrap();
        assert!(sec < sts, "pull secret at {sec} must precede the StatefulSet at {sts}");
    }

    #[test]
    fn the_namespace_is_applied_before_anything_that_lives_in_it() {
        // kubectl applies a List in order; a Secret ahead of its Namespace is
        // a hard error on a host being registered for the first time.
        let doc = build_manifest(&host_with_image("ghcr.io/mh/huntwell-worker:v3"), None, "Always");
        let items = doc["items"].as_array().unwrap();
        assert_eq!(items[0]["kind"], "Namespace");
        for i in items.iter().skip(1) {
            assert_eq!(i["metadata"]["namespace"], NAMESPACE, "{} escaped the namespace", i["kind"]);
        }
    }

    fn app_host(image: &str) -> Host {
        Host { runs_services: true, web_replicas: 2, ..host_with_image(image) }
    }

    fn kinds_named(doc: &Value) -> Vec<(String, String)> {
        doc["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| (i["kind"].as_str().unwrap_or("").to_string(), i["metadata"]["name"].as_str().unwrap_or("").to_string()))
            .collect()
    }

    #[test]
    fn a_pool_host_deploys_no_application() {
        let doc = build_manifest(&host_with_image("ghcr.io/mh/huntwell-worker:v3"), None, "Always");
        let got = kinds_named(&doc);
        // By kind and name: `huntwell` is also the Namespace, which every host
        // gets — it is the Ingress of that name that must not be here.
        for absent in [
            ("Deployment", "website"),
            ("Deployment", "planning"),
            ("Deployment", "scheduling"),
            ("Deployment", "notification"),
            ("StatefulSet", "nats"),
            ("Ingress", "huntwell"),
        ] {
            assert!(
                !got.iter().any(|(k, n)| k == absent.0 && n == absent.1),
                "{absent:?} should not be deployed to a pool-only host: {got:?}"
            );
        }
    }

    #[test]
    fn an_app_host_deploys_every_service_and_the_pool() {
        let doc = build_manifest(&app_host("ghcr.io/mh/huntwell-worker:v3"), None, "Always");
        let got = kinds_named(&doc);
        for want in [
            ("Deployment", "website"),
            ("Deployment", "planning"),
            ("Deployment", "scheduling"),
            ("Deployment", "notification"),
            ("Service", "website"),
            ("StatefulSet", "nats"),
            ("Ingress", "huntwell"),
            ("StatefulSet", "hw-pool"),
        ] {
            assert!(
                got.iter().any(|(k, n)| k == want.0 && n == want.1),
                "missing {want:?} in {got:?}"
            );
        }
    }

    #[test]
    fn service_images_follow_the_worker_image() {
        // A release is one tag: the registry, the path and the tag are the
        // worker's, and only the name changes.
        assert_eq!(service_image("ghcr.io/mh/huntwell-worker:0.1.0", "website"), "ghcr.io/mh/huntwell-website:0.1.0");
        assert_eq!(service_image("huntwell-worker:dev", "planning"), "huntwell-planning:dev");
        // A port in the registry host is not a tag.
        assert_eq!(service_image("localhost:5111/huntwell-worker", "notification"), "localhost:5111/huntwell-notification");
    }

    #[test]
    fn the_website_is_told_to_queue_runs_for_the_admin_to_place() {
        // The one setting that decides whether this whole arrangement works:
        // anything but `pool` and the website forks runs inside its own pod,
        // and the pool sits idle while the admin places nothing.
        let doc = build_manifest(&app_host("huntwell-worker:dev"), None, "IfNotPresent");
        let cm = doc["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["kind"] == "ConfigMap")
            .expect("a ConfigMap");
        assert_eq!(cm["data"]["RUN_DISPATCH"], "pool");
        assert_eq!(cm["data"]["HUNTWELL_DEV"], "0");
    }

    #[test]
    fn the_website_replica_count_is_never_zero() {
        // A host saved through an older client, or a hand-edited row, must not
        // deploy an application nobody can reach.
        let h = Host { web_replicas: 0, ..app_host("huntwell-worker:dev") };
        let doc = build_manifest(&h, None, "IfNotPresent");
        let web = doc["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["kind"] == "Deployment" && i["metadata"]["name"] == "website")
            .expect("the website Deployment");
        assert_eq!(web["spec"]["replicas"], 1);
    }

    #[test]
    fn the_application_pods_reference_the_pull_secret_too() {
        let doc = build_manifest(
            &app_host("registry.example.com/mh/huntwell-worker:v3"),
            Some(("registry.example.com".into(), "u".into(), "p".into())),
            "Always",
        );
        let items = doc["items"].as_array().unwrap();
        let web = items
            .iter()
            .find(|i| i["kind"] == "Deployment" && i["metadata"]["name"] == "website")
            .unwrap();
        assert_eq!(web["spec"]["template"]["spec"]["imagePullSecrets"][0]["name"], "huntwell-registry");
        // And it is applied before anything that uses it.
        let secret = items.iter().position(|i| i["metadata"]["name"] == "huntwell-registry").unwrap();
        let deploy = items.iter().position(|i| i["metadata"]["name"] == "website" && i["kind"] == "Deployment").unwrap();
        assert!(secret < deploy, "the pull secret must precede the pods that reference it");
    }


    #[test]
    fn the_pool_is_spread_across_the_machines_in_the_cluster() {
        // Adding a node should add capacity. Without this a StatefulSet is free
        // to put every pod on one machine, so the second box joins, reports
        // Ready, and runs nothing.
        let doc = build_manifest(&host_with_image("huntwell-worker:dev"), None, "IfNotPresent");
        let set = doc["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["kind"] == "StatefulSet" && i["metadata"]["name"] == "hw-pool")
            .expect("the pool StatefulSet");
        let spread = &set["spec"]["topologySpreadConstraints"][0];
        assert_eq!(spread["topologyKey"], "kubernetes.io/hostname");
        assert_eq!(spread["labelSelector"]["matchLabels"]["app"], "hw-pool");
        // Soft, so a one-node cluster still schedules every pod.
        assert_eq!(spread["whenUnsatisfiable"], "ScheduleAnyway");
    }
}


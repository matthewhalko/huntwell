//! The admin JSON API + embedded dashboard.
//!
//! Auth is deliberately simple (single-operator control plane): argon2 hashes
//! in `AdminUser`, a random bearer token in an in-memory map, delivered as an
//! HttpOnly cookie. Bind it to localhost or put TLS in front for anything
//! reachable.

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{hosts, incus, incus_driver, placement, Admin};
use crate::store::{self, Host};
use crate::web::ApiError;

const COOKIE: &str = "admin_session";

/// What this service is and whether it can still hear the bus.
async fn statusz(axum::extract::State(state): axum::extract::State<Admin>) -> Json<Value> {
    let hosts = crate::store::list_hosts(&state.db).await.map(|h| h.len()).unwrap_or(0);
    let slots: usize = state.slots.lock().await.values().map(|v| v.len()).sum();
    Json(json!({
        "service": "admin",
        "bus": { "connected": crate::bus::connected() },
        "hosts": hosts,
        "slots": slots,
    }))
}

pub fn router(state: Admin) -> Router {
    Router::new()
        .route("/", get(|| async { Html(super::ui::HTML) }))
        // The same two every other service answers, so a probe, a dashboard or
        // a person checking "is the bus up" does not need to know which service
        // it is talking to. Unauthenticated on purpose: a liveness probe has no
        // session, and neither says anything an operator login would protect.
        // The product's favicon, so a tab of each is told apart by title alone.
        .route("/favicon.png", get(|| async {
            (
                [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=86400")],
                include_bytes!("../../assets/favicon.png").as_slice(),
            )
        }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/statusz", get(statusz))
        .route("/admin/api/login", post(login))
        .route("/admin/api/claim", post(claim))
        .route("/admin/api/logout", post(logout))
        .route("/admin/api/session", get(session))
        .route("/admin/api/overview", get(overview))
        .route("/admin/api/services", get(services))
        .route("/admin/api/hosts", get(list_hosts).post(create_host))
        .route("/admin/api/hosts/{id}", axum::routing::put(update_host).delete(delete_host))
        .route("/admin/api/hosts/{id}/sync", post(sync_host))
        .route("/admin/api/hosts/{id}/slots", get(host_slots))
        .route("/admin/api/hosts/{id}/slots/{slot}/kill", post(kill_slot))
        .route("/admin/api/vms", get(list_vms).post(create_vm))
        .route("/admin/api/vms/deploy", post(deploy_all))
        .route("/admin/api/vms/{id}", axum::routing::delete(delete_vm))
        .route("/admin/api/vms/{id}/deploy", post(deploy_vm))
        .route("/admin/api/vms/{id}/slots", axum::routing::put(put_vm_slots))
        .route("/admin/api/vms/{id}/stop", post(stop_vm))
        .route("/admin/api/vms/{id}/start", post(start_vm))
        .route("/admin/api/accounts/{id}/connected-logins", axum::routing::put(put_account_connected_logins))
        .route("/admin/api/routing", get(get_routing).put(put_routing))
        .route("/admin/api/models", get(get_models).put(put_models))
        .route("/admin/api/models/available", get(available_models))
        .route("/admin/api/features", get(get_features).put(put_features))
        .route("/admin/api/accounts", get(list_accounts))
        .route("/admin/api/accounts/{id}/kinds", axum::routing::put(put_account_kinds))
        .route("/admin/api/executions", get(recent_executions))
        .route("/admin/api/route-log", get(route_log))
        .layer(middleware::from_fn(harden_headers))
        .with_state(state)
}

/// The same headers the website sends. The page's own script and event
/// handlers are inline, so scripts still need 'unsafe-inline'; what the policy
/// does hold is where a script may come from otherwise, what it may connect to
/// and what may frame the console — so a slip in escaping cannot ship anything
/// off to another origin.
async fn harden_headers(req: Request<Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline' https://cdn.jsdelivr.net; \
             style-src 'self' 'unsafe-inline' https://fonts.googleapis.com https://cdn.jsdelivr.net; \
             font-src 'self' https://fonts.gstatic.com data:; img-src 'self' data:; connect-src 'self'; \
             frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    res
}

// ---- auth ------------------------------------------------------------------

fn token_from(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|c| c.trim().strip_prefix(&format!("{COOKIE}=")).map(str::to_string))
}

async fn require(state: &Admin, headers: &HeaderMap) -> Result<String, ApiError> {
    let tok = token_from(headers).ok_or(ApiError(StatusCode::UNAUTHORIZED, "sign in".into()))?;
    state
        .sessions
        .lock()
        .await
        .get(&tok)
        .cloned()
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "sign in".into()))
}

#[derive(Deserialize)]
struct Login {
    email: String,
    password: String,
}

async fn login(
    State(state): State<Admin>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<Login>,
) -> Result<impl IntoResponse, ApiError> {
    // The peer itself: nothing sits in front of the admin.
    let ip = peer.ip().to_string();
    crate::throttle::ADMIN_LOGIN_FAILURES.check(&ip).map_err(|wait| {
        ApiError(StatusCode::TOO_MANY_REQUESTS, format!("too many attempts — try again in {} minutes", wait.div_ceil(60).max(1)))
    })?;
    let hash = store::get_admin_password_hash(&state.db, &req.email).await.map_err(internal)?;
    // A hash is always checked, so an unknown email costs the same time as a
    // wrong password and the reply does not say which it was.
    let hash = hash.unwrap_or_else(|| crate::web::auth::hash_password("no such operator").unwrap_or_default());
    let ok = tokio::task::spawn_blocking(move || crate::web::auth::verify_password(&req.password, &hash))
        .await
        .map_err(|e| internal(anyhow::anyhow!(e)))?;
    if !ok || store::get_admin_password_hash(&state.db, &req.email).await.map_err(internal)?.is_none() {
        crate::throttle::ADMIN_LOGIN_FAILURES.note(&ip);
        return Err(ApiError(StatusCode::UNAUTHORIZED, "email or password is incorrect".into()));
    }
    let tok = uuid_token();
    state.sessions.lock().await.insert(tok.clone(), req.email.to_lowercase());
    let cookie = format!("{COOKIE}={tok}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400");
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({"ok": true}))))
}

async fn logout(State(state): State<Admin>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(tok) = token_from(&headers) {
        state.sessions.lock().await.remove(&tok);
    }
    let cookie = format!("{COOKIE}=; Path=/; HttpOnly; Max-Age=0");
    ([(header::SET_COOKIE, cookie)], Json(json!({"ok": true})))
}

async fn session(State(state): State<Admin>, headers: HeaderMap) -> Json<Value> {
    let who = match token_from(&headers) {
        Some(tok) => state.sessions.lock().await.get(&tok).cloned(),
        None => None,
    };
    // `setupRequired` is what turns the sign-in card into the first-run form.
    Json(json!({ "email": who, "setupRequired": crate::setup::required() }))
}

#[derive(serde::Deserialize)]
struct Claim {
    #[serde(rename = "setupKey")]
    setup_key: String,
    email: String,
    password: String,
}

/// Claim an unclaimed control plane: create the first operator.
///
/// Guarded by the key printed at boot, and by there being no operator — both,
/// not either. The count is re-checked here rather than trusted from `arm()`,
/// so two people racing this form cannot both create an account.
async fn claim(State(state): State<Admin>, Json(req): Json<Claim>) -> Result<impl IntoResponse, ApiError> {
    if !crate::setup::required() {
        return Err(ApiError(StatusCode::CONFLICT, "this control plane already has an operator".into()));
    }
    if !crate::setup::matches(&req.setup_key) {
        // Deliberately the same shape as a bad password: nothing here should
        // tell an attacker whether they got the format right.
        return Err(ApiError(StatusCode::UNAUTHORIZED, "that setup key is not right".into()));
    }
    let email = req.email.trim().to_lowercase();
    if !email.contains('@') {
        return Err(ApiError(StatusCode::BAD_REQUEST, "an email address is required".into()));
    }
    if req.password.chars().count() < 12 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "the password must be at least 12 characters".into()));
    }
    if store::count_admin_users(&state.db).await.map_err(internal)? > 0 {
        crate::setup::disarm();
        return Err(ApiError(StatusCode::CONFLICT, "this control plane already has an operator".into()));
    }
    let hash = crate::web::auth::hash_password(&req.password).map_err(internal)?;
    store::upsert_admin_user(&state.db, &email, &hash).await.map_err(internal)?;
    // The key's whole life is this moment. Burn it before replying, so a
    // retried request cannot make a second account.
    crate::setup::disarm();
    tracing::info!("control plane claimed by {email}");

    // Signed in straight away: they just proved who they are twice over, and
    // bouncing them to a login form to retype the password they chose one
    // second ago is friction with nothing behind it.
    let tok = uuid_token();
    state.sessions.lock().await.insert(tok.clone(), email.clone());
    let cookie = format!("{COOKIE}={tok}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400");
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true, "email": email }))))
}

fn uuid_token() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

fn internal(e: anyhow::Error) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
}

// ---- hosts -----------------------------------------------------------------

#[derive(Deserialize)]
struct HostReq {
    name: String,
    /// The Incus API, e.g. https://10.0.0.1:8443.
    #[serde(default)]
    endpoint: String,
    /// One-time, from `sys incus token`. Only on create; never stored.
    #[serde(default)]
    token: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "d_status")]
    status: String,
    #[serde(default = "d_image")]
    base_image: String,
    #[serde(default = "d_priority")]
    priority: i32,
    #[serde(default = "d_max_vms")]
    max_vms: i32,
    #[serde(default = "d_vm_cpu")]
    vm_cpu: i32,
    #[serde(default = "d_vm_memory")]
    vm_memory: String,
    #[serde(default = "d_vm_disk")]
    vm_disk: String,
    #[serde(default = "d_vm_slots")]
    vm_slots: i32,
    #[serde(default)]
    ingress_domain: String,
    #[serde(default = "d_edge")]
    edge_scheme: String,
    #[serde(default)]
    edge_port: i32,
    #[serde(default)]
    notes: String,
}
fn default_true() -> bool {
    true
}
fn d_status() -> String {
    "Active".into()
}
fn d_image() -> String {
    "huntwell".into()
}
fn d_priority() -> i32 {
    100
}
fn d_max_vms() -> i32 {
    3
}
fn d_vm_cpu() -> i32 {
    8
}
fn d_vm_memory() -> String {
    "16GiB".into()
}
fn d_vm_disk() -> String {
    "60GiB".into()
}
fn d_vm_slots() -> i32 {
    10
}
fn d_edge() -> String {
    "https".into()
}

/// A host's name is its Incus remote, so it has to be one.
/// A hostname the edge will answer for: labels of letters, digits and hyphens.
/// It is written into the Caddyfile as a site address and sent to Cloudflare
/// as a DNS name, so anything else — a space, a brace, a newline — is refused
/// rather than becoming configuration.
pub fn valid_domain(d: &str) -> bool {
    d.is_empty()
        || ((1..=253).contains(&d.len())
            && d.split('.').all(|label| {
                (1..=63).contains(&label.len())
                    && label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')
            }))
}

fn valid_host_name(name: &str) -> bool {
    (1..=60).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && name != "local"
}

/// Incus size quantities: `16GiB`, `512MiB`, `60GB`.
fn valid_size(v: &str) -> bool {
    let v = v.trim();
    let digits = v.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && matches!(&v[digits..], "KiB" | "MiB" | "GiB" | "TiB" | "KB" | "MB" | "GB" | "TB")
}

impl HostReq {
    fn validate(&self) -> Result<(), ApiError> {
        let bad = |m: &str| Err(ApiError(StatusCode::BAD_REQUEST, m.to_string()));
        if !matches!(self.status.as_str(), "Active" | "Draining" | "Offline") {
            return bad("status must be Active, Draining or Offline");
        }
        if !matches!(self.edge_scheme.as_str(), "https" | "http" | "cloudflare") {
            return bad("edge must be https, http or cloudflare");
        }
        if !valid_domain(self.ingress_domain.trim()) {
            return bad("the domain is a hostname such as app.example.com — lowercase letters, digits, hyphens and dots");
        }
        if !valid_size(&self.vm_memory) || !valid_size(&self.vm_disk) {
            return bad("VM memory and disk are Incus sizes, e.g. 16GiB and 60GiB");
        }
        if !(1..=512).contains(&self.vm_cpu) || !(1..=50).contains(&self.vm_slots) || self.max_vms < 0 {
            return bad("vCPUs 1–512, slots 1–50, VM limit 0 or more");
        }
        Ok(())
    }

    fn into_host(self, host_id: i64) -> Host {
        Host {
            host_id,
            name: self.name.trim().to_string(),
            enabled: self.enabled,
            pool_size: 0,
            notes: self.notes,
            last_error: None,
            last_seen_at: None,
            created_at: chrono::Utc::now(),
            endpoint: self.endpoint.trim().trim_end_matches('/').to_string(),
            status: self.status,
            base_image: self.base_image.trim().to_string(),
            priority: self.priority,
            max_vms: self.max_vms,
            vm_cpu: self.vm_cpu,
            vm_memory: self.vm_memory.trim().to_string(),
            vm_disk: self.vm_disk.trim().to_string(),
            vm_slots: self.vm_slots,
            ingress_domain: self.ingress_domain.trim().to_string(),
            edge_scheme: self.edge_scheme,
            edge_port: self.edge_port,
            arch: String::new(),
            incus_version: String::new(),
            cpu_total: 0,
            memory_total_mb: 0,
        }
    }
}

async fn list_hosts(State(state): State<Admin>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let hosts_list = store::list_hosts(&state.db).await.map_err(internal)?;
    let vms = store::list_vms(&state.db).await.map_err(internal)?;
    let snapshot = store::pool_execution_snapshot(&state.db).await.map_err(internal)?;
    let cache = state.slots.lock().await;
    let rows: Vec<Value> = hosts_list
        .iter()
        .map(|h| {
            let slots = cache.get(&h.host_id);
            let ready = slots.map(|p| p.iter().filter(|x| x.ready).count()).unwrap_or(0);
            let busy = snapshot.iter().filter(|r| r.host_id == h.host_id).count();
            let mine: Vec<&store::Vm> = vms.iter().filter(|v| v.host_id == h.host_id).collect();
            json!({
                "host": h, "local": hosts::is_local(h), "vms": mine,
                "slots_ready": ready, "slots_busy": busy,
                "slots_seen": slots.map(|p| p.len()).unwrap_or(0),
            })
        })
        .collect();
    Ok(Json(json!({ "hosts": rows })))
}

/// Add a bare-metal host: trust it with its token, then record it. Trust first
/// so a bad endpoint or a spent token is refused with Incus's own words, and no
/// row is left behind for a host the control plane cannot reach.
async fn create_host(State(state): State<Admin>, headers: HeaderMap, Json(req): Json<HostReq>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let name = req.name.trim();
    if !valid_host_name(name) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "a host name is its Incus remote: lowercase letters, digits and hyphens, e.g. yak-02".into(),
        ));
    }
    req.validate()?;
    let token = req.token.clone();
    let mut h = req.into_host(0);
    hosts::register(&h, &token).await.map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let id = match store::create_host(&state.db, &h).await {
        Ok(id) => id,
        Err(e) => {
            // The trust was made for a row that will not exist; undo it.
            incus::remove_remote(h.remote()).await;
            return Err(ApiError(StatusCode::CONFLICT, format!("{e:#}")));
        }
    };
    h.host_id = id;
    hosts::reconcile_host(&state, &h).await;
    Ok(Json(json!({ "host_id": id })))
}

async fn update_host(
    State(state): State<Admin>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<HostReq>,
) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    req.validate()?;
    let existing = find_host(&state, id).await?;
    let mut h = req.into_host(id);
    // The name is the Incus remote the certificate is registered under.
    h.name = existing.name.clone();
    h.pool_size = existing.pool_size;
    if hosts::is_local(&existing) {
        h.endpoint = String::new();
    }
    store::update_host(&state.db, &h).await.map_err(internal)?;
    hosts::reconcile_host(&state, &h).await;
    // A changed domain or edge must reach Caddy now, not at the next provision.
    if !hosts::is_local(&h) {
        if let Err(e) = incus_driver::sync_edge(&state.db, &h).await {
            return Err(ApiError(StatusCode::BAD_GATEWAY, format!("saved, but the edge was not updated: {e:#}")));
        }
    }
    Ok(Json(json!({ "ok": true })))
}

async fn delete_host(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let h = find_host(&state, id).await?;
    store::delete_host(&state.db, id).await.map_err(|e| ApiError(StatusCode::CONFLICT, format!("{e:#}")))?;
    let _ = hosts::forget_host(&state, &h).await;
    state.slots.lock().await.remove(&id);
    Ok(Json(json!({ "ok": true })))
}

async fn sync_host(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let h = find_host(&state, id).await?;
    hosts::reconcile_host(&state, &h).await;
    let refreshed = store::get_host(&state.db, id).await.map_err(internal)?;
    Ok(Json(json!({ "host": refreshed })))
}

async fn find_host(state: &Admin, id: i64) -> Result<Host, ApiError> {
    store::get_host(&state.db, id)
        .await
        .map_err(internal)?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "no such host".into()))
}

// ── VMs ─────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct VmReq {
    role: String,
    /// Omit for a worker to let placement pick the host; the app VM names one.
    #[serde(default)]
    host_id: Option<i64>,
    #[serde(default)]
    slots: Option<i32>,
    #[serde(default)]
    cpu: Option<i32>,
    #[serde(default)]
    memory: Option<String>,
    #[serde(default)]
    disk: Option<String>,
}

async fn list_vms(State(state): State<Admin>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let vms = store::list_vms(&state.db).await.map_err(internal)?;
    let hosts_list = store::list_hosts(&state.db).await.map_err(internal)?;
    let busy = state.busy.lock().await;
    let rows: Vec<Value> = vms
        .iter()
        .map(|v| {
            let host = hosts_list.iter().find(|h| h.host_id == v.host_id).map(|h| h.name.as_str()).unwrap_or("?");
            json!({ "vm": v, "host": host, "busy": busy.contains(&v.vm_id) })
        })
        .collect();
    let build_dir = incus_driver::build_dir();
    Ok(Json(json!({ "vms": rows, "build_dir": build_dir })))
}

/// Provision a VM. Returns at once: a VM takes minutes to boot, so the work
/// runs in the background and the console follows the row's status.
async fn create_vm(State(state): State<Admin>, headers: HeaderMap, Json(req): Json<VmReq>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let role = incus_driver::Role::parse(&req.role).map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let host = match req.host_id {
        Some(id) => find_host(&state, id).await?,
        None if role == incus_driver::Role::Worker => hosts::pick_for_placement(&state)
            .await
            .map_err(|e| ApiError(StatusCode::CONFLICT, format!("{e:#}")))?,
        None => return Err(ApiError(StatusCode::BAD_REQUEST, "choose the host the app VM runs on".into())),
    };
    if hosts::is_local(&host) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "the local dev host runs no VMs".into()));
    }
    if host.status != "Active" || !host.enabled {
        return Err(ApiError(StatusCode::CONFLICT, format!("host '{}' is {} — it takes no new VMs", host.name, host.status)));
    }
    let name = match role {
        incus_driver::Role::App => incus_driver::APP_VM.to_string(),
        incus_driver::Role::Worker => store::next_worker_name(&state.db, &host.name).await.map_err(internal)?,
    };
    let memory = req.memory.unwrap_or_else(|| host.vm_memory.clone());
    let disk = req.disk.unwrap_or_else(|| host.vm_disk.clone());
    if !valid_size(&memory) || !valid_size(&disk) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "memory and disk are Incus sizes, e.g. 16GiB".into()));
    }
    let vm = store::create_vm(
        &state.db,
        host.host_id,
        &name,
        role.as_str(),
        req.slots.unwrap_or(host.vm_slots).clamp(1, 50),
        req.cpu.unwrap_or(host.vm_cpu).clamp(1, 512),
        &memory,
        &disk,
    )
    .await
    .map_err(|e| ApiError(StatusCode::CONFLICT, format!("{e:#}")))?;

    let vm_id = vm.vm_id;
    in_background(&state, vm_id, "provision", move |state| async move {
        incus_driver::provision(&state.db, &host, &vm).await.map(|_| ())
    })
    .await?;
    Ok(Json(json!({ "vm_id": vm_id, "name": name, "status": "Provisioning" })))
}

async fn deploy_vm(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let (host, vm) = find_vm(&state, id).await?;
    in_background(&state, id, "deploy", move |state| async move {
        incus_driver::deploy(&state.db, &host, &vm).await.map(|_| ())
    })
    .await?;
    Ok(Json(json!({ "ok": true })))
}

/// A release, in one request: every running VM onto the build folder's
/// current executables.
async fn deploy_all(State(state): State<Admin>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let vms = store::list_vms(&state.db).await.map_err(internal)?;
    let mut started = Vec::new();
    for vm in vms.into_iter().filter(|v| v.status == "Running") {
        let Ok(Some(host)) = store::get_host(&state.db, vm.host_id).await else { continue };
        let name = vm.name.clone();
        let vm_id = vm.vm_id;
        if in_background(&state, vm_id, "deploy", move |state| async move {
            incus_driver::deploy(&state.db, &host, &vm).await.map(|_| ())
        })
        .await
        .is_ok()
        {
            started.push(name);
        }
    }
    Ok(Json(json!({ "deploying": started })))
}

#[derive(Deserialize)]
struct SlotsReq {
    slots: i32,
}

async fn put_vm_slots(
    State(state): State<Admin>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<SlotsReq>,
) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let (host, vm) = find_vm(&state, id).await?;
    incus_driver::set_slots(&state.db, &host, &vm, req.slots)
        .await
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    hosts::reconcile_host(&state, &host).await;
    Ok(Json(json!({ "ok": true })))
}

async fn stop_vm(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let (host, vm) = find_vm(&state, id).await?;
    incus_driver::stop(&state.db, &host, &vm).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    hosts::reconcile_host(&state, &host).await;
    Ok(Json(json!({ "ok": true })))
}

async fn start_vm(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let (host, vm) = find_vm(&state, id).await?;
    in_background(&state, id, "start", move |state| async move { incus_driver::start(&state.db, &host, &vm).await })
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_vm(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let (host, vm) = find_vm(&state, id).await?;
    if state.busy.lock().await.contains(&id) {
        return Err(ApiError(StatusCode::CONFLICT, format!("{} is busy with another operation", vm.name)));
    }
    incus_driver::delete(&state.db, &host, &vm).await.map_err(|e| ApiError(StatusCode::CONFLICT, format!("{e:#}")))?;
    hosts::reconcile_host(&state, &host).await;
    Ok(Json(json!({ "ok": true })))
}

async fn find_vm(state: &Admin, id: i64) -> Result<(Host, store::Vm), ApiError> {
    let vm = store::get_vm(&state.db, id)
        .await
        .map_err(internal)?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "no such VM".into()))?;
    let host = find_host(state, vm.host_id).await?;
    Ok((host, vm))
}

/// Run a long VM operation off the request, one at a time per VM.
///
/// Refused while another is in flight: two deploys pushing and renaming the
/// same executables at once can leave a VM running neither build.
async fn in_background<F, Fut>(state: &Admin, vm_id: i64, what: &'static str, op: F) -> Result<(), ApiError>
where
    F: FnOnce(Admin) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    if !state.busy.lock().await.insert(vm_id) {
        return Err(ApiError(StatusCode::CONFLICT, "that VM is busy with another operation — wait for it to finish".into()));
    }
    let state = state.clone();
    tokio::spawn(async move {
        let outcome = op(state.clone()).await;
        if let Err(e) = &outcome {
            tracing::warn!(vm_id, "{what} failed: {e:#}");
            let _ = store::set_vm_status(&state.db, vm_id, "Failed", &incus_driver::redact_urls(&format!("{what}: {e:#}"))).await;
        } else {
            tracing::info!(vm_id, "{what} finished");
        }
        state.busy.lock().await.remove(&vm_id);
    });
    Ok(())
}

async fn host_slots(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let snapshot = store::pool_execution_snapshot(&state.db).await.map_err(internal)?;
    let cache = state.slots.lock().await;
    let slots: Vec<Value> = cache
        .get(&id)
        .map(|slots| {
            slots.iter()
                .map(|p| {
                    let run = snapshot.iter().find(|r| r.host_id == id && r.slot_name == p.name);
                    json!({ "name": p.name, "ready": p.ready, "node": p.node,
                            "execution_id": run.map(|r| r.execution_id) })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(json!({ "slots": slots })))
}

async fn kill_slot(
    State(state): State<Admin>,
    headers: HeaderMap,
    Path((id, slot)): Path<(i64, String)>,
) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let h = store::get_host(&state.db, id)
        .await
        .map_err(internal)?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "no such host".into()))?;
    hosts::kill_slot(&state, &h, &slot).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

// ---- routing ---------------------------------------------------------------

async fn get_routing(State(state): State<Admin>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let strategy = store::get_setting(&state.db, "routing_strategy").await.map_err(internal)?.unwrap_or_else(|| "round_robin".into());
    let pinned_host = store::get_setting(&state.db, "pinned_host_id").await.map_err(internal)?;
    let pinned_slot = store::get_setting(&state.db, "pinned_slot").await.map_err(internal)?;
    let free = placement::free_slots(&state).await.map_err(internal)?;
    Ok(Json(json!({
        "strategy": strategy,
        "pinned_host_id": pinned_host,
        "pinned_slot": pinned_slot,
        "free_slots": free.iter().map(|(h, p)| json!({"host_id": h, "slot": p})).collect::<Vec<_>>(),
    })))
}

/// Which model each agent stage uses, and what is on offer.
///
/// Stored in `ControlSetting`, read once per process by whoever runs agents —
/// the server for drafting, each run for scraping and enrichment. So a change
/// here lands on the next run started, and on the server's next restart.
async fn get_models(State(state): State<Admin>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let mut out = serde_json::Map::new();
    for (stage, key) in crate::agent::STAGES {
        let v = store::get_setting(&state.db, key).await.map_err(internal)?.unwrap_or_default();
        out.insert(stage.to_string(), Value::String(v));
    }
    Ok(Json(Value::Object(out)))
}

#[derive(Deserialize)]
struct ModelsReq {
    #[serde(default)]
    models: std::collections::HashMap<String, String>,
}

async fn put_models(State(state): State<Admin>, headers: HeaderMap, Json(body): Json<ModelsReq>) -> Result<Json<Value>, ApiError> {
    let who = require(&state, &headers).await?;
    for (stage, key) in crate::agent::STAGES {
        let Some(raw) = body.models.get(stage) else { continue };
        let value = raw.trim();
        // "" is a real choice: it means "leave this stage on the CLI default".
        if !value.is_empty() && crate::agent::normalize_model(value).is_none() {
            return Err(ApiError(StatusCode::BAD_REQUEST, format!("{value:?} is not a valid model id")));
        }
        store::set_setting(&state.db, key, value).await.map_err(internal)?;
    }
    tracing::info!(operator = %who, "models updated");
    Ok(Json(json!({ "ok": true })))
}

/// The models this installation's Cursor account can actually use.
///
/// Asked of the CLI rather than hard-coded: the list changes without us, and a
/// dropdown offering a model the account cannot run is worse than a text box.
/// If the CLI is missing the field still accepts a typed id.
async fn available_models(State(state): State<Admin>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let out = tokio::task::spawn_blocking(crate::agent::available_models)
        .await
        .unwrap_or_default();
    Ok(Json(json!({ "models": out })))
}

/// What a new account may build, installation-wide.
///
/// Tables and people are the product; reports and files are still experimental,
/// so they ship off. An account with nothing of its own follows this, which is
/// what makes "turn reports on for everyone" one write instead of a migration.
async fn get_features(State(state): State<Admin>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    Ok(Json(json!({
        "kinds": store::installation_kinds(&state.db).await,
        "all": ["prospects", "artifacts", "report", "assets"],
        "default": store::DEFAULT_KINDS,
    })))
}

#[derive(Deserialize)]
struct KindsReq {
    #[serde(default)]
    kinds: Vec<String>,
}

async fn put_features(State(state): State<Admin>, headers: HeaderMap, Json(body): Json<KindsReq>) -> Result<Json<Value>, ApiError> {
    let who = require(&state, &headers).await?;
    let kinds = store::parse_kinds(&body.kinds.join(","));
    if kinds.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "leave at least one kind on".into()));
    }
    store::set_setting(&state.db, store::KINDS_SETTING, &kinds.join(",")).await.map_err(internal)?;
    tracing::info!(operator = %who, kinds = %kinds.join(","), "installation kinds updated");
    Ok(Json(json!({ "ok": true, "kinds": kinds })))
}

async fn list_accounts(State(state): State<Admin>, headers: HeaderMap, Query(q): Query<LogQuery>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let rows = store::list_accounts_brief(&state.db, q.limit.clamp(1, 1000)).await.map_err(internal)?;
    Ok(Json(json!({ "accounts": rows })))
}

/// One account's own list. Empty puts them back on the installation default,
/// which is the difference between "this person is in the beta" and "this
/// person is explicitly held back".
async fn put_account_kinds(
    State(state): State<Admin>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<KindsReq>,
) -> Result<Json<Value>, ApiError> {
    let who = require(&state, &headers).await?;
    let kinds = store::parse_kinds(&body.kinds.join(","));
    store::set_account_kinds(&state.db, id, &kinds.join(",")).await.map_err(internal)?;
    tracing::info!(operator = %who, account = id, kinds = %kinds.join(","), "account kinds updated");
    Ok(Json(json!({ "ok": true, "kinds": kinds })))
}

#[derive(Deserialize)]
struct ConnectedLoginsReq {
    on: bool,
}

/// Turn connected logins on or off for one workspace.
///
/// Operator-only and per workspace, because the feature hands a real browser a
/// customer's real credentials. It is off for everyone until someone decides
/// otherwise — there is deliberately no "on for all" switch here.
async fn put_account_connected_logins(
    State(state): State<Admin>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<ConnectedLoginsReq>,
) -> Result<Json<Value>, ApiError> {
    let who = require(&state, &headers).await?;
    store::set_connected_logins(&state.db, id, body.on).await.map_err(internal)?;
    // Logged either way: granting this is the interesting event, and revoking it
    // is what someone will want to find afterwards.
    tracing::info!(operator = %who, account = id, on = body.on, "connected logins changed");
    Ok(Json(json!({ "ok": true, "connected_logins": body.on })))
}

#[derive(Deserialize)]
struct RoutingReq {
    strategy: String,
    #[serde(default)]
    pinned_host_id: Option<i64>,
    #[serde(default)]
    pinned_slot: Option<String>,
}

async fn put_routing(State(state): State<Admin>, headers: HeaderMap, Json(req): Json<RoutingReq>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    if !matches!(req.strategy.as_str(), "random" | "round_robin" | "pinned") {
        return Err(ApiError(StatusCode::BAD_REQUEST, "strategy must be random | round_robin | pinned".into()));
    }
    store::set_setting(&state.db, "routing_strategy", &req.strategy).await.map_err(internal)?;
    if let Some(h) = req.pinned_host_id {
        store::set_setting(&state.db, "pinned_host_id", &h.to_string()).await.map_err(internal)?;
    }
    if let Some(p) = req.pinned_slot {
        store::set_setting(&state.db, "pinned_slot", &p).await.map_err(internal)?;
    }
    Ok(Json(json!({ "ok": true })))
}

// ---- runs / overview -------------------------------------------------------

#[derive(Deserialize)]
struct RunsQuery {
    host_id: Option<i64>,
    slot: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    50
}

async fn recent_executions(State(state): State<Admin>, headers: HeaderMap, Query(q): Query<RunsQuery>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let rows = store::recent_executions_admin(&state.db, q.host_id, q.slot.as_deref(), q.limit).await.map_err(internal)?;
    Ok(Json(json!({ "executions": rows })))
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default = "default_log_limit")]
    limit: i64,
}
fn default_log_limit() -> i64 {
    200
}

/// The routing audit trail — every placement, re-queue and reap decision.
async fn route_log(State(state): State<Admin>, headers: HeaderMap, Query(q): Query<LogQuery>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let rows = store::list_route_log(&state.db, q.limit).await.map_err(internal)?;
    Ok(Json(json!({ "log": rows })))
}

async fn services(State(state): State<Admin>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let pings = state.pings.lock().await.clone();
    Ok(Json(super::health_snapshot(&pings, chrono::Utc::now(), super::STALE_AFTER_SECS)))
}

async fn overview(State(state): State<Admin>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let hosts_list = store::list_hosts(&state.db).await.map_err(internal)?;
    let snapshot = store::pool_execution_snapshot(&state.db).await.map_err(internal)?;
    let queued = store::unplaced_queued_executions(&state.db, 200).await.map_err(internal)?;
    let cache = state.slots.lock().await;
    let ready: usize = cache.values().map(|p| p.iter().filter(|x| x.ready).count()).sum();
    Ok(Json(json!({
        "hosts": hosts_list.len(),
        "hosts_healthy": hosts_list.iter().filter(|h| h.enabled && h.last_error.is_none()).count(),
        "slots_ready": ready,
        "slots_busy": snapshot.len(),
        "queued_unplaced": queued.len(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_hostnames_only() {
        assert!(valid_domain("app.example.com"));
        assert!(valid_domain(""));
        assert!(!valid_domain("App.Example.com"));
        assert!(!valid_domain("a.com {\n}\nb.com"));
        assert!(!valid_domain("a b.com"));
        assert!(!valid_domain("-a.com"));
        assert!(!valid_domain("a..com"));
    }
}

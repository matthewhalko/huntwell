//! The admin JSON API + embedded dashboard.
//!
//! Auth: operators are users of their own Cognito pool (`ADMIN_COGNITO_*`),
//! separate from the product's; the `admin_user` row keys to it by subject.
//! A sign-in yields a random bearer token in an in-memory map, delivered as an
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
        // Everything else under /admin/api needs a signed-in operator, checked
        // here — before a body is parsed or a handler runs — so nothing about
        // the admin API answers anyone else. Handlers still call `require`
        // for the operator's name; this is the gate in front of them.
        .merge(
            Router::new()
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
            .route("/admin/api/accounts/{id}/mfa", axum::routing::delete(reset_account_mfa))
            .route("/admin/api/routing", get(get_routing).put(put_routing))
            .route("/admin/api/models", get(get_models).put(put_models))
            .route("/admin/api/models/available", get(available_models))
            .route("/admin/api/features", get(get_features).put(put_features))
            .route("/admin/api/accounts", get(list_accounts))
            .route("/admin/api/waitlist", get(list_waitlist))
            .route("/admin/api/waitlist/invite", post(invite_signup))
            .route("/admin/api/waitlist/{id}/approve", post(approve_waitlist))
            .route("/admin/api/waitlist/{id}/decline", post(decline_waitlist))
            .route("/admin/api/waitlist/{id}", axum::routing::delete(delete_waitlist))
            .route("/admin/api/accounts/{id}/kinds", axum::routing::put(put_account_kinds))
            .route("/admin/api/accounts/{id}/rate", axum::routing::put(put_account_rate))
            .route("/admin/api/accounts/{id}/credits", post(grant_credits))
            .route("/admin/api/executions", get(recent_executions))
            .route("/admin/api/executions/{id}/log", get(execution_log))
            .route("/admin/api/route-log", get(route_log))
                .route_layer(middleware::from_fn_with_state(state.clone(), signed_in)),
        )
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

/// The gate on every protected admin route: a live operator session, or 401
/// before anything else happens.
async fn signed_in(State(state): State<Admin>, req: Request<Body>, next: Next) -> Response {
    if require(&state, req.headers()).await.is_err() {
        return ApiError(StatusCode::UNAUTHORIZED, "sign in".into()).into_response();
    }
    next.run(req).await
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
    let email = req.email.trim().to_lowercase();
    // The pool is asked whether or not the row exists, so an unknown email
    // costs the same time as a wrong password and the reply does not say
    // which it was. Then the row: being in the operators' pool is necessary,
    // being recorded as an operator here is what makes it sufficient.
    let signed_in = crate::cognito::sign_in_full_in(crate::cognito::PoolKind::Admins, &email, &req.password).await;
    let known = store::admin_user_sub(&state.db, &email).await.map_err(internal)?;
    let sub = match (signed_in, known) {
        (Ok(crate::cognito::SignIn::Done { sub, .. }), Some(bound)) if bound.is_empty() || bound == sub => sub,
        (Ok(crate::cognito::SignIn::MfaRequired { .. }), _) => {
            return Err(ApiError(
                StatusCode::UNAUTHORIZED,
                "this operator has two-factor on in the pool, which the console cannot answer yet — turn it off in Cognito".into(),
            ));
        }
        (Err(e), _) if e.to_string() == crate::cognito::UNAVAILABLE => {
            return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, e.to_string()));
        }
        _ => {
            crate::throttle::ADMIN_LOGIN_FAILURES.note(&ip);
            return Err(ApiError(StatusCode::UNAUTHORIZED, "email or password is incorrect".into()));
        }
    };
    // A row from before the pool binds on first sign-in, like an account's.
    store::upsert_admin_user(&state.db, &email, &sub).await.map_err(internal)?;
    let tok = uuid_token();
    state.sessions.lock().await.insert(tok.clone(), email);
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
    crate::identity::check_password_strength(&req.password).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    // The operator is made in the operators' pool first; only then is there a
    // row, so a failure cannot leave an operator who can never sign in.
    let sub = crate::cognito::create_user_in(crate::cognito::PoolKind::Admins, &email, &req.password)
        .await
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;
    store::upsert_admin_user(&state.db, &email, &sub).await.map_err(internal)?;
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
        // A `provider:model` id is checked here rather than at run time: a
        // typo, or a provider whose key is not set, should be said now and not
        // discovered by a run that has already spent its browser session.
        if !value.is_empty() {
            if let Err(why) = crate::llm::parse_model_id(value).and_then(|engine| match engine {
                crate::llm::Engine::Direct { .. } => crate::llm::for_model(value).map(|_| ()),
                // Outreach is one call from the website; the Cursor CLI cannot
                // answer it, so a Cursor id there would switch drafting off.
                crate::llm::Engine::Cursor { .. } if stage == "outreach" => {
                    Err(format!("{value} is a Cursor model; outreach needs a provider model (provider:model)"))
                }
                crate::llm::Engine::Cursor { .. } => Ok(()),
            }) {
                return Err(ApiError(StatusCode::BAD_REQUEST, why));
            }
            // And that the provider still has it. Names get retired, and a
            // retired one saved here is a run that dies at its first call
            // with "this model is no longer available" — which is worth one
            // request now to avoid.
            if let Ok((provider, model)) = crate::llm::for_model(value) {
                if let Ok(available) = provider.list_models().await {
                    if !available.is_empty() && !available.iter().any(|m| m == &model) {
                        return Err(ApiError(
                            StatusCode::BAD_REQUEST,
                            format!("{} does not offer {model} any more — pick one from the list", provider.label()),
                        ));
                    }
                }
            }
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
    // Two worlds in one list: the providers Huntwell talks to directly
    // (`provider:model`, priced from their own catalogues) and whatever the
    // Cursor CLI on this machine reports. The direct ones come first, because
    // they are the ones with a price we know and a loop we control.
    let mut out: Vec<Value> = Vec::new();
    for p in crate::llm::providers() {
        if !p.configured() {
            continue;
        }
        // Asked, not assumed. A list written here goes stale the day a
        // provider retires a name, and a stale name is a run that dies on
        // "this model is no longer available" — which is how this came to be
        // asked. The built-in list is the fallback when a provider is
        // unreachable, and says so.
        let (ids, live) = match p.list_models().await {
            Ok(ids) if !ids.is_empty() => (ids, true),
            Ok(_) => (p.models().into_iter().map(|m| m.id).collect(), false),
            Err(e) => {
                tracing::warn!("{}: could not list models ({e}) — offering the built-in list", p.id());
                (p.models().into_iter().map(|m| m.id).collect(), false)
            }
        };
        for id in ids {
            let price = p.price(&id);
            out.push(json!({
                "id": format!("{}:{}", p.id(), id),
                "label": format!("{} — {}", p.label(), id),
                "group": p.label(),
                "input_per_m": price.map(|x| x.input),
                "cached_input_per_m": price.map(|x| x.cached_input),
                "output_per_m": price.map(|x| x.output),
                "priced": price.is_some(),
                "live": live,
                "direct": true,
            }));
        }
    }
    let cursor = tokio::task::spawn_blocking(crate::agent::available_models).await.unwrap_or_default();
    for m in cursor {
        let id = m.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let label = m.get("label").and_then(Value::as_str).unwrap_or(&id).to_string();
        out.push(json!({ "id": id, "label": label, "group": "Cursor", "direct": false }));
    }
    Ok(Json(json!({ "models": out, "providers": crate::llm::status() })))
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
    Ok(Json(json!({ "accounts": rows, "default_usd_per_mtoken": crate::config::sell_usd_per_mtoken() })))
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
struct RateReq {
    /// USD per million billable tokens; `null` returns the account to the
    /// installation's rate.
    usd_per_mtoken: Option<f64>,
}

/// What one account (a workspace, so everyone working in it) is charged per
/// million tokens. Applies to tokens billed from now on.
async fn put_account_rate(
    State(state): State<Admin>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<RateReq>,
) -> Result<Json<Value>, ApiError> {
    let who = require(&state, &headers).await?;
    if let Some(r) = body.usd_per_mtoken {
        // Zero would make runs free and a typo of 5000 would drain a wallet in
        // one call; both are refused rather than saved.
        if !r.is_finite() || r <= 0.0 || r > 1000.0 {
            return Err(ApiError(StatusCode::BAD_REQUEST, "the rate must be more than $0 and at most $1,000 per million tokens".into()));
        }
    }
    let rate = body.usd_per_mtoken.map(|r| (r * 10_000.0).round() / 10_000.0);
    if !store::set_sell_rate(&state.db, id, rate).await.map_err(internal)? {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such account".into()));
    }
    let effective = store::effective_sell_rate(rate);
    tracing::info!(operator = %who, account = id, rate = ?rate, effective, "account token rate changed");
    Ok(Json(json!({ "ok": true, "sell_usd_per_mtoken": rate, "effective_usd_per_mtoken": effective })))
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
/// Turn two-factor off for an account whose owner lost their device. The
/// operator's own session is the authority here; the person then signs in
/// with their password and can set a new device up.
async fn reset_account_mfa(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let who = require(&state, &headers).await?;
    let acc = store::get_account(&state.db, id).await.map_err(internal)?.ok_or(ApiError(StatusCode::NOT_FOUND, "no such account".into()))?;
    crate::identity::disable_mfa(&acc.email, acc.account_id, &state.db).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    tracing::info!("two-factor reset for account {} by operator {who}", acc.account_id);
    Ok(Json(json!({ "ok": true })))
}

// ---- waitlist & invitations ---------------------------------------------------
//
// Huntwell is invite-only (`config::open_signup`). People ask to join from the
// sign-up page and land here; an operator invites them — or anyone else, by
// address — and they get an email with a single-use link bound to that
// address. Only the token's hash is stored, so the link is shown to the
// operator once, in the response, to copy if the email does not arrive.

async fn list_waitlist(State(state): State<Admin>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let rows = store::list_waitlist(&state.db, 500).await.map_err(internal)?;
    Ok(Json(json!({ "rows": rows, "open_signup": crate::config::open_signup() })))
}

#[derive(Deserialize)]
struct InviteReq {
    email: String,
    #[serde(default)]
    name: String,
}

/// Issue a fresh invitation to `email` — replacing any earlier link — and
/// queue the email. Shared by "Invite someone", Approve and Resend.
async fn send_signup_invite(state: &Admin, who: &str, email: &str, name: &str) -> Result<Json<Value>, ApiError> {
    let bad = |m: &str| ApiError(StatusCode::BAD_REQUEST, m.to_string());
    let email = email.trim().to_lowercase();
    if email.len() < 3 || !email.contains('@') || email.contains(char::is_whitespace) || email.len() > 320 {
        return Err(bad("a valid email address is required"));
    }
    let name = name.trim();
    if name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(bad("the name must be under 80 characters, on one line"));
    }
    // Without the website's address there is nothing to link to; say so
    // rather than email someone a link that goes nowhere.
    let base = crate::config::get("HUNTWELL_PUBLIC_URL")
        .map(|b| b.trim().trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
        .ok_or_else(|| bad("set HUNTWELL_PUBLIC_URL for the admin so invitation links know where the website is"))?;
    let token = store::new_invite_token();
    let row = store::invite_to_signup(&state.db, &email, name, who, &store::sha256_hex(&token), store::SIGNUP_INVITE_DAYS)
        .await
        .map_err(|e| ApiError(StatusCode::CONFLICT, format!("{e:#}")))?;
    let link = format!("{base}/signup?invite={token}");
    let msg = crate::mail::signup_invite(&row.name, &link, store::SIGNUP_INVITE_DAYS);
    let emailed = match store::queue_mail(&state.db, None, &row.email, "signup_invite", &msg).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("invitation for {} not queued: {e:#}", row.email);
            false
        }
    };
    tracing::info!(operator = %who, email = %row.email, "sign-up invitation sent");
    Ok(Json(json!({ "ok": true, "row": row, "link": link, "emailed": emailed })))
}

async fn invite_signup(State(state): State<Admin>, headers: HeaderMap, Json(req): Json<InviteReq>) -> Result<Json<Value>, ApiError> {
    let who = require(&state, &headers).await?;
    send_signup_invite(&state, &who, &req.email, &req.name).await
}

/// Approve someone off the waitlist — or resend an invitation, which is the
/// same thing: a new link, and the old one stops working.
async fn approve_waitlist(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let who = require(&state, &headers).await?;
    let row = store::get_waitlist(&state.db, id).await.map_err(internal)?.ok_or(ApiError(StatusCode::NOT_FOUND, "no such request".into()))?;
    send_signup_invite(&state, &who, &row.email, &row.name).await
}

async fn decline_waitlist(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let who = require(&state, &headers).await?;
    if !store::decline_waitlist(&state.db, id).await.map_err(internal)? {
        return Err(ApiError(StatusCode::CONFLICT, "only a waiting or invited request can be declined".into()));
    }
    tracing::info!(operator = %who, waitlist = id, "waitlist request declined");
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct DeleteReq {
    /// The address, typed by the operator. Checked here as well as in the
    /// page, so a stray request cannot delete anyone.
    confirm: String,
}

/// Delete a waitlist request — and, when it became an account, that account
/// and everything in it: plans, results, runs, files, keys, outreach, credits,
/// billing records and the sign-in itself. Unrecoverable.
async fn delete_waitlist(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>, Json(req): Json<DeleteReq>) -> Result<Json<Value>, ApiError> {
    let who = require(&state, &headers).await?;
    let row = store::get_waitlist(&state.db, id).await.map_err(internal)?.ok_or(ApiError(StatusCode::NOT_FOUND, "no such request".into()))?;
    if !req.confirm.trim().eq_ignore_ascii_case(row.email.trim()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "type the email address exactly to confirm".into()));
    }
    let Some(account_id) = row.account_id else {
        store::delete_waitlist(&state.db, id).await.map_err(internal)?;
        tracing::warn!(operator = %who, waitlist = id, email = %row.email, "waitlist request deleted");
        return Ok(Json(json!({ "ok": true, "account_deleted": false })));
    };
    match store::purge_account(&state.db, account_id).await.map_err(internal)? {
        store::Purge::NotFound => {
            // The account went some other way; the request is all that is left.
            store::delete_waitlist(&state.db, id).await.map_err(internal)?;
            Ok(Json(json!({ "ok": true, "account_deleted": false })))
        }
        store::Purge::Busy(n) => Err(ApiError(
            StatusCode::CONFLICT,
            format!("they have {n} run(s) queued or running — cancel those (or wait for them to finish), then delete"),
        )),
        store::Purge::Done { email, plans, object_keys } => {
            // After the commit: the rows are gone either way, and a file or a
            // sign-in left behind is logged rather than undoing the delete.
            let mut files_left = 0;
            for key in &object_keys {
                if let Err(e) = crate::objstore::delete(key).await {
                    files_left += 1;
                    tracing::warn!(account_id, key, "stored file not removed: {e:#}");
                }
            }
            let identity_removed = match crate::identity::delete_user(&email).await {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(account_id, "sign-in not removed from the user pool: {e:#}");
                    false
                }
            };
            tracing::warn!(operator = %who, account_id, email = %email, plans, files = object_keys.len(), "account deleted with all its data");
            Ok(Json(json!({
                "ok": true,
                "account_deleted": true,
                "plans": plans,
                "files": object_keys.len(),
                "files_left": files_left,
                "identity_removed": identity_removed,
            })))
        }
    }
}

#[derive(Deserialize)]
struct GrantReq {
    usd: f64,
    /// Shown to them in the email, as a note from Huntwell.
    #[serde(default)]
    note: String,
}

/// Add free credit to an account's wallet, and email its owner. Recorded as a
/// grant (`credit_purchase.kind`), never as a payment.
async fn grant_credits(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>, Json(req): Json<GrantReq>) -> Result<Json<Value>, ApiError> {
    let who = require(&state, &headers).await?;
    let bad = |m: &str| ApiError(StatusCode::BAD_REQUEST, m.to_string());
    if !req.usd.is_finite() || req.usd < 0.01 || req.usd > 10_000.0 {
        return Err(bad("the amount is between $0.01 and $10,000"));
    }
    let note = req.note.trim();
    if note.chars().count() > 500 {
        return Err(bad("keep the note under 500 characters"));
    }
    store::get_account(&state.db, id).await.map_err(internal)?.ok_or(ApiError(StatusCode::NOT_FOUND, "no such account".into()))?;
    let micros = (req.usd * 100.0).round() as i64 * 10_000;
    store::apply_credit_grant(&state.db, id, micros, note, &who).await.map_err(internal)?;
    crate::web::billing::announce_credit(&state.db, id, micros, crate::mail::CreditKind::Grant, note).await;
    let usage = store::ensure_usage(&state.db, id).await.map_err(internal)?;
    tracing::warn!(operator = %who, account = id, usd = micros as f64 / 1e6, "free credit added");
    Ok(Json(json!({ "ok": true, "credits_usd": usage.credits_usd })))
}

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
    let rows: Vec<Value> = rows.iter().map(execution_money).collect();
    Ok(Json(json!({ "executions": rows })))
}

/// A run with what it earned: what the customer was charged, what it cost us,
/// and the difference.
///
/// The charge is the same sum the customer sees — billed tokens at the sell
/// rate. Our cost is the real one when the run recorded it: a direct-provider
/// stage prices every call from its own provider's table as it goes, and
/// Cursor reports a figure on usage-based plans. Only a run that recorded
/// nothing falls back to an estimate, which is marked as one — and that
/// estimate can only use the scrape model's rate, so a run whose stages used
/// different models is approximate by construction.
fn execution_money(r: &store::AdminExecutionRow) -> Value {
    let charged = store::tokens_to_usd_micros_at(r.input_tokens + r.output_tokens, store::effective_sell_rate(r.sell_rate));
    let (cost, basis) = if r.cost_usd_micros > 0 {
        (Some(r.cost_usd_micros), "reported")
    } else {
        let est = crate::model_catalog::estimate_cost_micros(
            &r.model_scrape,
            r.input_tokens,
            r.output_tokens,
            r.cache_read_tokens,
            r.cache_write_tokens,
        );
        (est, if est.is_some() { "estimated" } else { "unknown" })
    };
    let mut v = serde_json::to_value(r).unwrap_or_else(|_| json!({}));
    v["charged_usd_micros"] = json!(charged);
    v["cost_usd_micros"] = json!(cost);
    v["cost_basis"] = json!(basis);
    v["profit_usd_micros"] = json!(cost.map(|c| charged - c));
    v
}

/// Everything one run printed, for the admin's log viewer.
///
/// Unscoped by account on purpose — this is the operator's view, and the
/// question it answers is "why did that run fail", which is usually somebody
/// else's run. The same lines a customer sees on their run page, plus the
/// stderr ones their page filters out, which is where the reason usually is.
async fn execution_log(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let run = store::recent_executions_admin(&state.db, None, None, 500)
        .await
        .map_err(internal)?
        .into_iter()
        .find(|r| r.execution_id == id);
    let lines = store::list_execution_logs(&state.db, id, 0, 5000).await.map_err(internal)?;
    Ok(Json(json!({
        "execution": run.as_ref().map(execution_money),
        "lines": lines.iter().map(|l| json!({
            "seq": l.seq, "ts": l.ts, "stream": l.stream, "line": l.line,
        })).collect::<Vec<_>>(),
    })))
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

#[cfg(test)]
mod money_tests {
    use super::*;

    /// What the operator's log viewer highlights. The rule is the wording the
    /// pipeline uses when something went wrong, not the stream it went to:
    /// the guard and the trail both narrate on stderr without anything being
    /// broken, and an agent's own error lines go to stdout.
    #[test]
    fn the_lines_worth_jumping_to_are_the_ones_that_say_what_broke() {
        // The same regexp the page uses, kept here so a change to one is
        // noticed against the other.
        let bad = regex::Regex::new(r"(?i)^\s*(!|✖)|\berror\b|\bfailed\b|panicked|refused|\bcould not\b|\bunavailable\b|exhausted").unwrap();
        for line in [
            "  ! scrape: the model provider rejected the request: HTTP 404 from gemini",
            "[iter 1] error: scrape: the model provider rejected the request",
            "thread 'main' panicked at src/mcp.rs:46:21",
            "     2m00s  ✖ [scrape] refused: attempt refused by policy",
            "[stop] credits exhausted — stopping this run",
            "provision: hw-app could not join the event bus",
        ] {
            assert!(bad.is_match(line), "should stand out: {line}");
        }
        for line in [
            "huntwell run \"Reno Crosstreks\" (run #5)",
            "[1/4 scrape] stored prompt, prompt 4.9 KB",
            "  ✓ stored 1/3  Laif E. Meidell",
            "  → 12 rows returned in 2m10s",
            "  tokens     1.2M in · 40k out",
        ] {
            assert!(!bad.is_match(line), "ordinary progress should not: {line}");
        }
    }

    fn run(input: i64, output: i64, cache_read: i64, reported: i64, model: &str) -> store::AdminExecutionRow {
        store::AdminExecutionRow {
            execution_id: 1,
            plan_id: 1,
            account_id: 1,
            source: "p".into(),
            status: "succeeded".into(),
            started_at: chrono::Utc::now(),
            finished_at: None,
            host_id: None,
            slot_name: None,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: 0,
            cost_usd_micros: reported,
            model_scrape: model.into(),
            sell_rate: None,
        }
    }

    #[test]
    fn an_account_rate_prices_its_own_runs() {
        // 1M tokens at an account's own $8/M is $8.00, whatever the default.
        let mut r = run(900_000, 100_000, 0, 1_200_000, "composer-2.5");
        r.sell_rate = Some(8.0);
        let v = execution_money(&r);
        assert_eq!(v["charged_usd_micros"], 8_000_000);
        assert_eq!(v["profit_usd_micros"], 6_800_000);
    }

    #[test]
    fn profit_is_the_charge_minus_what_cursor_said_or_our_estimate() {
        // 1M billed tokens at the default $5/M = $5.00; Cursor reported $1.20.
        let v = execution_money(&run(900_000, 100_000, 0, 1_200_000, "composer-2.5"));
        assert_eq!(v["charged_usd_micros"], 5_000_000);
        assert_eq!(v["cost_basis"], "reported");
        assert_eq!(v["profit_usd_micros"], 3_800_000);
        // Nothing reported: estimated from the model, and said to be.
        let v = execution_money(&run(900_000, 100_000, 0, 0, "composer-2.5"));
        assert_eq!(v["cost_basis"], "estimated");
        assert_eq!(v["cost_usd_micros"], 700_000);
        assert_eq!(v["profit_usd_micros"], 4_300_000);
        // A model with no known rate has no profit figure — not a made-up one.
        let v = execution_money(&run(900_000, 100_000, 0, 0, "mystery-1"));
        assert_eq!(v["cost_basis"], "unknown");
        assert!(v["profit_usd_micros"].is_null());
        // A run can lose money, and the number says so.
        let v = execution_money(&run(100_000, 0, 0, 2_000_000, ""));
        assert_eq!(v["profit_usd_micros"], -1_500_000);
    }
}

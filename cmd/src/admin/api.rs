//! The admin JSON API + embedded dashboard.
//!
//! Auth is deliberately simple (single-operator control plane): argon2 hashes
//! in `AdminUser`, a random bearer token in an in-memory map, delivered as an
//! HttpOnly cookie. Bind it to localhost or put TLS in front for anything
//! reachable.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{hosts, placement, Admin};
use crate::store::{self, Host};
use crate::web::ApiError;

const COOKIE: &str = "admin_session";

/// What this service is and whether it can still hear the bus.
async fn statusz(axum::extract::State(state): axum::extract::State<Admin>) -> Json<Value> {
    let hosts = crate::store::list_hosts(&state.db).await.map(|h| h.len()).unwrap_or(0);
    let pods: usize = state.pods.lock().await.values().map(|v| v.len()).sum();
    Json(json!({
        "service": "admin",
        "bus": { "connected": crate::bus::connected() },
        "hosts": hosts,
        "pods": pods,
    }))
}

pub fn router(state: Admin) -> Router {
    Router::new()
        .route("/", get(|| async { Html(super::ui::HTML) }))
        // The same two every other service answers, so a probe, a dashboard or
        // a person checking "is the bus up" does not need to know which service
        // it is talking to. Unauthenticated on purpose: a liveness probe has no
        // session, and neither says anything an operator login would protect.
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
        .route("/admin/api/hosts/{id}/pods", get(host_pods))
        .route("/admin/api/hosts/{id}/pods/{pod}/kill", post(kill_pod))
        .route("/admin/api/accounts/{id}/connected-logins", axum::routing::put(put_account_connected_logins))
        .route("/admin/api/routing", get(get_routing).put(put_routing))
        .route("/admin/api/models", get(get_models).put(put_models))
        .route("/admin/api/models/available", get(available_models))
        .route("/admin/api/features", get(get_features).put(put_features))
        .route("/admin/api/accounts", get(list_accounts))
        .route("/admin/api/accounts/{id}/kinds", axum::routing::put(put_account_kinds))
        .route("/admin/api/executions", get(recent_executions))
        .route("/admin/api/route-log", get(route_log))
        .with_state(state)
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

async fn login(State(state): State<Admin>, Json(req): Json<Login>) -> Result<impl IntoResponse, ApiError> {
    let hash = store::get_admin_password_hash(&state.db, &req.email)
        .await
        .map_err(internal)?
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "bad credentials".into()))?;
    if !crate::web::auth::verify_password(&req.password, &hash) {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "bad credentials".into()));
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
    #[serde(default)]
    kubeconfig_yaml: String,
    #[serde(default)]
    kube_context: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "default_pool")]
    pool_size: i32,
    #[serde(default = "d_cpu_req")]
    cpu_request: String,
    #[serde(default = "d_cpu_lim")]
    cpu_limit: String,
    #[serde(default = "d_mem_req")]
    mem_request: String,
    #[serde(default = "d_mem_lim")]
    mem_limit: String,
    #[serde(default = "d_image")]
    image: String,
    #[serde(default)]
    runs_services: bool,
    #[serde(default = "d_web_replicas")]
    web_replicas: i32,
    #[serde(default)]
    notes: String,
}
fn default_true() -> bool {
    true
}
fn default_pool() -> i32 {
    2
}
fn d_cpu_req() -> String {
    "250m".into()
}
fn d_cpu_lim() -> String {
    "1".into()
}
fn d_mem_req() -> String {
    "512Mi".into()
}
fn d_mem_lim() -> String {
    "1Gi".into()
}
fn d_image() -> String {
    "huntwell-worker:dev".into()
}
fn d_web_replicas() -> i32 {
    1
}

impl HostReq {
    fn into_host(self, host_id: i64) -> Host {
        Host {
            host_id,
            name: self.name.trim().to_string(),
            kubeconfig_yaml: self.kubeconfig_yaml,
            kube_context: self.kube_context.filter(|c| !c.trim().is_empty()),
            enabled: self.enabled,
            pool_size: self.pool_size.clamp(0, 50),
            cpu_request: self.cpu_request,
            cpu_limit: self.cpu_limit,
            mem_request: self.mem_request,
            mem_limit: self.mem_limit,
            image: self.image,
            runs_services: self.runs_services,
            web_replicas: self.web_replicas.clamp(1, 50),
            notes: self.notes,
            last_error: None,
            last_seen_at: None,
            created_at: chrono::Utc::now(),
        }
    }
}

async fn list_hosts(State(state): State<Admin>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let hosts_list = store::list_hosts(&state.db).await.map_err(internal)?;
    let snapshot = store::pool_execution_snapshot(&state.db).await.map_err(internal)?;
    let cache = state.pods.lock().await;
    let rows: Vec<Value> = hosts_list
        .iter()
        .map(|h| {
            let pods = cache.get(&h.host_id);
            let ready = pods.map(|p| p.iter().filter(|x| x.ready).count()).unwrap_or(0);
            let busy = snapshot.iter().filter(|r| r.host_id == h.host_id).count();
            json!({
                "host": h, "pods_ready": ready, "pods_busy": busy,
                "pods_seen": pods.map(|p| p.len()).unwrap_or(0),
            })
        })
        .collect();
    Ok(Json(json!({ "hosts": rows })))
}

async fn create_host(State(state): State<Admin>, headers: HeaderMap, Json(req): Json<HostReq>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    if req.name.trim().is_empty() || req.kubeconfig_yaml.trim().is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "name and kubeconfig are required".into()));
    }
    let mut h = req.into_host(0);
    // Probe before storing so a bad kubeconfig is rejected with kubectl's
    // words. ("local" is the process-backed dev host: nothing to probe.)
    let probe_host = Host { host_id: -1, ..h.clone() };
    hosts::write_kubeconfig(&probe_host).map_err(internal)?;
    hosts::probe(&probe_host).await.map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("cluster unreachable: {e:#}")))?;
    let _ = std::fs::remove_file(hosts::kubeconfig_path(-1));
    let id = store::create_host(&state.db, &h).await.map_err(internal)?;
    h.host_id = id;
    hosts::write_kubeconfig(&h).map_err(internal)?;
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
    let existing = store::get_host(&state.db, id)
        .await
        .map_err(internal)?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "no such host".into()))?;
    let mut h = req.into_host(id);
    if h.kubeconfig_yaml.trim().is_empty() {
        h.kubeconfig_yaml = existing.kubeconfig_yaml.clone();
    }
    store::update_host(&state.db, &h).await.map_err(internal)?;
    hosts::write_kubeconfig(&h).map_err(internal)?;
    hosts::reconcile_host(&state, &h).await;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_host(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let h = store::get_host(&state.db, id)
        .await
        .map_err(internal)?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "no such host".into()))?;
    store::delete_host(&state.db, id).await.map_err(|e| ApiError(StatusCode::CONFLICT, format!("{e:#}")))?;
    let _ = hosts::delete_namespace(&state, &h).await;
    let _ = std::fs::remove_file(hosts::kubeconfig_path(id));
    state.pods.lock().await.remove(&id);
    Ok(Json(json!({ "ok": true })))
}

async fn sync_host(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let h = store::get_host(&state.db, id)
        .await
        .map_err(internal)?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "no such host".into()))?;
    hosts::reconcile_host(&state, &h).await;
    let refreshed = store::get_host(&state.db, id).await.map_err(internal)?;
    Ok(Json(json!({ "host": refreshed })))
}

async fn host_pods(State(state): State<Admin>, headers: HeaderMap, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let snapshot = store::pool_execution_snapshot(&state.db).await.map_err(internal)?;
    let cache = state.pods.lock().await;
    let pods: Vec<Value> = cache
        .get(&id)
        .map(|pods| {
            pods.iter()
                .map(|p| {
                    let run = snapshot.iter().find(|r| r.host_id == id && r.pod_name == p.name);
                    json!({ "name": p.name, "ready": p.ready, "node": p.node,
                            "execution_id": run.map(|r| r.execution_id) })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(json!({ "pods": pods })))
}

async fn kill_pod(
    State(state): State<Admin>,
    headers: HeaderMap,
    Path((id, pod)): Path<(i64, String)>,
) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let h = store::get_host(&state.db, id)
        .await
        .map_err(internal)?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "no such host".into()))?;
    hosts::kill_pod(&state, &h, &pod).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

// ---- routing ---------------------------------------------------------------

async fn get_routing(State(state): State<Admin>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let strategy = store::get_setting(&state.db, "routing_strategy").await.map_err(internal)?.unwrap_or_else(|| "round_robin".into());
    let pinned_host = store::get_setting(&state.db, "pinned_host_id").await.map_err(internal)?;
    let pinned_pod = store::get_setting(&state.db, "pinned_pod").await.map_err(internal)?;
    let free = placement::free_pods(&state).await.map_err(internal)?;
    Ok(Json(json!({
        "strategy": strategy,
        "pinned_host_id": pinned_host,
        "pinned_pod": pinned_pod,
        "free_pods": free.iter().map(|(h, p)| json!({"host_id": h, "pod": p})).collect::<Vec<_>>(),
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
    let rows = store::list_accounts_brief(&state.db, q.limit.max(1)).await.map_err(internal)?;
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
    pinned_pod: Option<String>,
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
    if let Some(p) = req.pinned_pod {
        store::set_setting(&state.db, "pinned_pod", &p).await.map_err(internal)?;
    }
    Ok(Json(json!({ "ok": true })))
}

// ---- runs / overview -------------------------------------------------------

#[derive(Deserialize)]
struct RunsQuery {
    host_id: Option<i64>,
    pod: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    50
}

async fn recent_executions(State(state): State<Admin>, headers: HeaderMap, Query(q): Query<RunsQuery>) -> Result<Json<Value>, ApiError> {
    require(&state, &headers).await?;
    let rows = store::recent_executions_admin(&state.db, q.host_id, q.pod.as_deref(), q.limit).await.map_err(internal)?;
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
    let cache = state.pods.lock().await;
    let ready: usize = cache.values().map(|p| p.iter().filter(|x| x.ready).count()).sum();
    Ok(Json(json!({
        "hosts": hosts_list.len(),
        "hosts_healthy": hosts_list.iter().filter(|h| h.enabled && h.last_error.is_none()).count(),
        "pods_ready": ready,
        "pods_busy": snapshot.len(),
        "queued_unplaced": queued.len(),
    })))
}

//! The public API: `/v1`, authenticated by an API key.
//!
//! Everything the app can do, a key can do — create a plan from a sentence, run
//! it, watch the run, read what it found, and see what it cost. The app's own
//! `/api` routes are a cookie-authenticated implementation detail that changes
//! with the UI; this surface is the promise, so it is versioned, JSON in and
//! JSON out, and named in the product's own words (plans, runs, artifacts).
//!
//! Auth, rate limiting, address pinning and the audit trail are the download
//! API's, reused wholesale. **Every request is signed** (`web::signing`:
//! `X-HW-KEY`, `X-HW-TS`, `X-HW-SIGN`). Bearer tokens and `?token=` are gone:
//! a key that predates signing has no secret to sign with and is refused,
//! told to make a new one.

use std::collections::HashMap;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::download::authenticate;
use super::App;
use crate::store;

pub fn router() -> Router<App> {
    Router::new()
        .route("/me", get(me))
        .route("/plans", get(list_plans).post(create_plan))
        .route("/plans/{id}", get(get_plan).patch(update_plan).delete(delete_plan))
        .route("/plans/{id}/executions", get(plan_executions).post(start_execution))
        .route("/plans/{id}/artifacts", get(plan_artifacts))
        .route("/plans/{id}/graph", get(plan_graph))
        .route("/artifacts", get(all_artifacts))
        .route("/prospects", get(prospects))
        .route("/executions", get(list_executions))
        .route("/executions/{id}", get(get_execution))
        .route("/executions/{id}/log", get(execution_log))
        .route("/executions/{id}/cancel", post(cancel_execution))
        .route("/usage", get(usage))
        .route("/outreach", get(outreach_list).post(outreach_create))
        .route("/outreach/profile", get(outreach_profile).put(outreach_save_profile))
        .route("/plans/{id}/outreach", get(plan_outreach).put(plan_save_outreach))
        .route("/outreach/{id}", get(outreach_get).patch(outreach_edit).delete(outreach_delete))
        .route("/outreach/{id}/revise", post(outreach_revise))
        .route("/outreach/{id}/restore", post(outreach_restore))
        .route("/operator/workspaces", post(operator_workspace))
        // The workspace, live: plans drafting and changing, runs starting and
        // finishing, prospects as they are found. See `stream`.
        .route("/stream", get(stream))
        .layer(axum::middleware::from_fn(super::download::stamp_signed_parts))
}

/// The authenticated caller: which workspace, and whether the key is pinned to
/// a single plan.
struct Caller {
    workspace: i64,
    plan: Option<i64>,
    /// The teammate who made the key, when recorded — whose footer outreach
    /// drafts carry.
    person: Option<i64>,
    /// The key itself, so a long-lived stream can check it is still good.
    key_id: i64,
    /// Whether the key's maker may change plans and spend on the workspace's
    /// behalf. A key never does more than the person who made it: someone
    /// who may make keys but not build plans gets a read-only key.
    can_plans: bool,
}

/// Refuses a write when the key's maker may not change plans.
fn needs_plans(c: &Caller) -> Result<(), Response> {
    if c.can_plans {
        Ok(())
    } else {
        Err(err(StatusCode::FORBIDDEN, "the person who made this key can read this workspace but not change plans or spend its credits"))
    }
}

/// Authenticates and, for a plan-scoped route, refuses a key pinned elsewhere.
/// One helper so a pinned key cannot reach past its plan on any route.
async fn caller(
    state: &App,
    headers: &HeaderMap,
    q: &HashMap<String, String>,
    peer: SocketAddr,
    method: &str,
    path: &str,
) -> Result<Caller, Response> {
    let key = authenticate(state, headers, q, peer, method, path).await?;
    // A key without a recorded maker is the owner's or the operator's.
    let can_plans = match key.created_by {
        None => true,
        Some(p) => store::workspace_caps(&state.db, key.account_id, p).await.map(|caps| caps.plans).unwrap_or(false),
    };
    Ok(Caller { workspace: key.account_id, plan: key.plan_id, person: key.created_by, key_id: key.key_id, can_plans })
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

fn oops(e: anyhow::Error) -> Response {
    tracing::error!("v1: {e:#}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
}

/// The plan a plan-scoped request may act on, honouring a pinned key.
fn scoped(c: &Caller, id: i64) -> Result<i64, Response> {
    match c.plan {
        Some(p) if p != id => Err(err(StatusCode::FORBIDDEN, "this key is scoped to another plan")),
        _ => Ok(id),
    }
}

fn num(q: &HashMap<String, String>, k: &str, default: i64) -> i64 {
    q.get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

// ---- who am I ---------------------------------------------------------------

async fn me(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", "/v1/me").await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let name = store::workspace_name(&state.db, c.workspace).await.unwrap_or_default();
    Json(json!({
        "workspace_id": c.workspace,
        "workspace": name,
        "key_scope": match c.plan { Some(p) => json!({ "plan_id": p }), None => json!("workspace") },
    }))
    .into_response()
}

// ---- plans ------------------------------------------------------------------

/// A plan as the API presents it. The prompts that make it work are machinery
/// and are not part of this surface.
fn plan_json(p: &store::SourceConfig) -> Value {
    json!({
        "id": p.plan_id,
        "name": p.source,
        "description": p.description,
        "kind": p.kind_of().as_str(),
        "subject": p.subject,
        "status": store::public_draft_status(&p.draft_status),
        "effort": store::normalize_effort(&p.effort),
        "target": p.target_prospects,
        "schedule": {
            "enabled": p.schedule_enabled,
            "time": p.schedule_time,
            "days": p.schedule_days,
            "next_run_at": p.next_run_at.map(|t| t.to_rfc3339()),
        },
        "ready": store::plan_ready(p).is_ok(),
        "updated_at": p.updated_at,
        "models": {
            "scrape": p.model_scrape,
            "enrich": p.model_enrich,
            "planner": p.model_planner,
        },
    })
}

async fn list_plans(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", "/v1/plans").await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match store::list_plans(&state.db, c.workspace).await {
        Ok(plans) => Json(json!({
            "data": plans
                .iter()
                .filter(|s| c.plan.is_none_or(|p| p == s.plan.plan_id))
                .map(|s| {
                    let mut v = plan_json(&s.plan);
                    v["artifacts"] = json!(s.prospects);
                    v["executions"] = json!(s.executions);
                    v
                })
                .collect::<Vec<_>>()
        }))
        .into_response(),
        Err(e) => oops(e),
    }
}

#[derive(Deserialize)]
struct NewPlan {
    brief: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    target: Option<i64>,
    #[serde(default)]
    effort: Option<String>,
    /// Columns for a database plan: `[{"name": "Price", "prompt": "asking
    /// price in USD"}]`. Supplying any makes the plan collect rows with
    /// exactly these columns.
    #[serde(default)]
    columns: Vec<crate::artifact::ColumnRequest>,
    /// Websites to search first: `"cars.com, autotrader.com"`. Steering, not a
    /// restriction — the plan may still go elsewhere.
    #[serde(default)]
    sites: String,
    /// `prospects` | `artifacts` | `report` | `assets`. Omit to let the brief
    /// decide.
    #[serde(default)]
    kind: Option<String>,
}

/// Creates a plan from a sentence. Returns at once with `status: "drafting"` —
/// the search is written in the background — so poll the plan until it is
/// `ready` before running it.
async fn create_plan(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Json(body): Json<NewPlan>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "POST", "/v1/plans").await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if let Err(r) = needs_plans(&c) {
        return r;
    }
    if c.plan.is_some() {
        return err(StatusCode::FORBIDDEN, "this key is scoped to one plan and cannot create others");
    }
    let req = super::api::NewPlanBody {
        brief: body.brief,
        name: body.name,
        target: body.target,
        effort: body.effort,
        columns: body.columns,
        sites: body.sites,
        kind: body.kind,
    };
    match super::api::create_plan_from_brief(&state, c.workspace, req).await {
        Ok(p) => (StatusCode::CREATED, Json(plan_json(&p))).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &format!("{e:#}")),
    }
}

async fn load_plan(state: &App, c: &Caller, id: i64) -> Result<store::SourceConfig, Response> {
    let id = scoped(c, id)?;
    match store::get_plan(&state.db, c.workspace, id).await {
        Ok(Some(p)) => Ok(p),
        Ok(None) => Err(err(StatusCode::NOT_FOUND, "plan not found")),
        Err(e) => Err(oops(e)),
    }
}

async fn get_plan(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", &format!("/v1/plans/{id}")).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match load_plan(&state, &c, id).await {
        Ok(p) => Json(plan_json(&p)).into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct PlanPatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    target: Option<i64>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    schedule_enabled: Option<bool>,
    #[serde(default)]
    schedule_time: Option<String>,
    #[serde(default)]
    schedule_days: Option<String>,
    #[serde(default)]
    models: Option<super::api::PlanModelsBody>,
}

async fn update_plan(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
    Json(body): Json<PlanPatch>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "PATCH", &format!("/v1/plans/{id}")).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if let Err(r) = needs_plans(&c) {
        return r;
    }
    let mut p = match load_plan(&state, &c, id).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if let Some(v) = body.name {
        p.source = v;
    }
    if let Some(v) = body.description {
        p.description = v;
    }
    if let Some(v) = body.target {
        p.target_prospects = v.clamp(0, 500) as i32;
    }
    if let Some(v) = body.effort {
        super::api::apply_effort_public(&mut p, &v);
    }
    if let Some(v) = body.schedule_enabled {
        p.schedule_enabled = v;
    }
    if let Some(v) = body.schedule_time {
        p.schedule_time = v;
    }
    if let Some(v) = body.schedule_days {
        p.schedule_days = v;
    }
    if let Some(models) = &body.models {
        if let Err(why) = super::api::apply_plan_models(&mut p, models) {
            return err(StatusCode::BAD_REQUEST, &why);
        }
    }
    // Same rule as the app: a plan being drafted can still be renamed or
    // retargeted; only running it needs it to be ready.
    if store::normalize_draft_status(&p.draft_status) == "ready" {
        if let Err(why) = store::plan_ready(&p) {
            return err(StatusCode::BAD_REQUEST, &format!("{why}"));
        }
    }
    if let Err(e) = store::save_plan(&state.db, c.workspace, &p).await {
        return oops(e);
    }
    let _ = store::refresh_schedule(&state.db, id).await;
    match store::get_plan(&state.db, c.workspace, id).await {
        Ok(Some(p)) => Json(plan_json(&p)).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "plan not found"),
        Err(e) => oops(e),
    }
}

async fn delete_plan(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "DELETE", &format!("/v1/plans/{id}")).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if let Err(r) = needs_plans(&c) {
        return r;
    }
    let id = match scoped(&c, id) {
        Ok(i) => i,
        Err(r) => return r,
    };
    match store::delete_plan(&state.db, c.workspace, id).await {
        Ok(true) => Json(json!({ "deleted": id })).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "plan not found"),
        Err(e) => oops(e),
    }
}

// ---- runs -------------------------------------------------------------------

fn run_json(r: &store::ExecutionRecord) -> Value {
    json!({
        "id": r.execution_id,
        "plan_id": r.plan_id,
        "plan": r.source,
        "status": r.status,
        "trigger": r.trigger,
        "started_at": r.started_at.to_rfc3339(),
        "finished_at": r.finished_at.map(|t| t.to_rfc3339()),
        "new_artifacts": r.new_prospects,
        "tokens": r.input_tokens + r.output_tokens,
        "cost_usd": r.cost_usd(),
        "cost_per_artifact": r.cost_per_result(),
    })
}

/// Starts a run. Refused with 402 when the workspace has no card on file, and
/// 409 when the plan is still drafting or already running.
async fn start_execution(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "POST", &format!("/v1/plans/{id}/executions")).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if let Err(r) = needs_plans(&c) {
        return r;
    }
    let plan = match load_plan(&state, &c, id).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let args = crate::pipeline::RunArgs {
        target: q.get("target").and_then(|v| v.parse().ok()),
        // The most tokens this run may spend; it stops there and keeps what it found.
        max_tokens: q.get("max_tokens").and_then(|v| v.parse::<i64>().ok()).filter(|n| *n > 0),
        ..Default::default()
    };
    match super::runner::start(&state, c.workspace, plan.plan_id, "api", args).await {
        Ok(execution_id) => (StatusCode::ACCEPTED, Json(json!({ "id": execution_id, "status": "queued" }))).into_response(),
        Err(e) => {
            let msg = format!("{e:#}");
            let code = if msg.contains("payment method") {
                StatusCode::PAYMENT_REQUIRED
            } else if msg.contains("already running") || msg.contains("cannot run yet") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            err(code, &msg)
        }
    }
}

async fn plan_executions(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", &format!("/v1/plans/{id}/executions")).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    // Someone else's plan is "not found", the same as on every other route —
    // not an empty list, which would still confirm the id means something.
    if let Err(r) = load_plan(&state, &c, id).await {
        return r;
    }
    match store::list_executions(&state.db, c.workspace, Some(id), num(&q, "limit", 25)).await {
        Ok(runs) => Json(json!({ "data": runs.iter().map(run_json).collect::<Vec<_>>() })).into_response(),
        Err(e) => oops(e),
    }
}

async fn list_executions(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", "/v1/executions").await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let plan = c.plan.or_else(|| q.get("plan_id").and_then(|v| v.parse().ok()));
    match store::list_executions(&state.db, c.workspace, plan, num(&q, "limit", 25)).await {
        Ok(runs) => Json(json!({ "data": runs.iter().map(run_json).collect::<Vec<_>>() })).into_response(),
        Err(e) => oops(e),
    }
}

async fn get_execution(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", &format!("/v1/executions/{id}")).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match store::get_execution(&state.db, c.workspace, id).await {
        Ok(Some(r)) if c.plan.is_none_or(|p| p == r.plan_id) => Json(run_json(&r)).into_response(),
        Ok(_) => err(StatusCode::NOT_FOUND, "run not found"),
        Err(e) => oops(e),
    }
}

/// A run's output, line by line. `after` is the last `seq` you saw, so polling
/// returns only what is new.
async fn execution_log(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", &format!("/v1/executions/{id}/log")).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match store::get_execution(&state.db, c.workspace, id).await {
        Ok(Some(r)) if c.plan.is_none_or(|p| p == r.plan_id) => {}
        Ok(_) => return err(StatusCode::NOT_FOUND, "run not found"),
        Err(e) => return oops(e),
    }
    match store::list_execution_logs(&state.db, id, num(&q, "after", 0), num(&q, "limit", 500)).await {
        Ok(lines) => Json(json!({ "data": lines })).into_response(),
        Err(e) => oops(e),
    }
}

async fn cancel_execution(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "POST", &format!("/v1/executions/{id}/cancel")).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if let Err(r) = needs_plans(&c) {
        return r;
    }
    let run = match store::get_execution(&state.db, c.workspace, id).await {
        Ok(Some(r)) if c.plan.is_none_or(|p| p == r.plan_id) => r,
        Ok(_) => return err(StatusCode::NOT_FOUND, "run not found"),
        Err(e) => return oops(e),
    };
    let _ = super::runner::cancel(&state, c.workspace, id).await;
    if matches!(run.status.as_str(), "queued" | "running") {
        let _ = store::finish_execution(&state.db, id, "cancelled", Some(130)).await;
    }
    Json(json!({ "id": id, "status": "cancelled" })).into_response()
}

// ---- operator ---------------------------------------------------------------

#[derive(Deserialize)]
struct NewWorkspace {
    /// The sign-in address for the workspace. One per customer of the caller.
    email: String,
    #[serde(default)]
    name: String,
    /// Addresses the returned key may be used from. Empty allows any.
    #[serde(default)]
    allow_cidr: String,
}

/// The operator's credential: a key that names the caller and a secret that
/// only signs. `HUNTWELL_OPERATOR_KEY` and `HUNTWELL_OPERATOR_SECRET` — both,
/// or the endpoint does not exist (404), so a half-configured server does not
/// advertise it.
fn operator_credentials() -> Option<(String, String)> {
    let key = crate::config::get("HUNTWELL_OPERATOR_KEY").map(|k| k.trim().to_string()).filter(|k| k.len() >= 16)?;
    let secret = crate::config::get("HUNTWELL_OPERATOR_SECRET").map(|k| k.trim().to_string()).filter(|k| k.len() >= 32)?;
    Some((key, secret))
}

/// The path the operator signs — the route's full address, as every `/v1`
/// signature is.
const OPERATOR_PATH: &str = "/v1/operator/workspaces";

/// Whether a request is the operator's, signed.
///
/// The same scheme as a workspace key (`web::signing`): `X-HW-KEY` names the
/// caller, `X-HW-SIGN` is HMAC-SHA256 over `POST\n/v1/operator/workspaces\n
/// QUERY\nTIMESTAMP\nNONCE\nSHA256(BODY)` with the operator secret, the
/// timestamp has to be within the window, and the nonce may be used once. A bearer token is refused outright, with a message
/// saying so: this key mints credentials for other people's workspaces, and a
/// token that travels whole on every call is exactly what signing replaced.
fn check_operator(
    headers: &HeaderMap,
    query: &str,
    body_sha256: &str,
    key: &str,
    secret: &str,
    now_ms: i64,
) -> Result<(), &'static str> {
    let Some(presented) = crate::web::signing::presented(headers) else {
        return Err(if headers.contains_key(axum::http::header::AUTHORIZATION) {
            "the operator endpoint no longer takes a bearer token — sign the request: X-HW-KEY (the operator key), X-HW-TS, X-HW-NONCE and X-HW-SIGN"
        } else {
            "unauthorized"
        });
    };
    // Compared as digests: equal-length hashes give the comparison nothing to
    // leak about where a guessed key first goes wrong.
    if store::sha256_hex(&presented.key) != store::sha256_hex(key) {
        return Err("unauthorized");
    }
    crate::web::signing::verify(secret, &presented, "POST", OPERATOR_PATH, query, body_sha256, now_ms)
        // The operator key has no row of its own; its nonces are kept apart
        // from every workspace key's under an id no key can have.
        .and_then(|()| crate::web::signing::claim_nonce(OPERATOR_NONCE_ID, &presented.nonce, now_ms))
        .map_err(|why| why.message())
}

/// The id the operator key's nonces are remembered under.
const OPERATOR_NONCE_ID: i64 = -1;

/// Creates (or finds) a workspace and returns a fresh key for it.
///
/// For a platform that gives each of its own customers a private workspace
/// rather than sharing one: the caller holds the operator key and secret and
/// signs this request (`check_operator`), and each key this returns reaches
/// exactly one workspace. Idempotent by email — a re-provision returns the
/// same workspace with a new key, so a lost key is recovered by asking again
/// rather than by a support ticket.
///
/// Deliberately not part of a workspace key's powers: only the operator key
/// verifies here, and a workspace key gets the same 401 as no key at all.
async fn operator_workspace(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<NewWorkspace>,
) -> Response {
    let Some((key, secret)) = operator_credentials() else {
        return err(StatusCode::NOT_FOUND, "not found");
    };
    let ip = super::client_ip(&headers, peer);
    if let Some(why) = super::download::throttle(ip) {
        return err(StatusCode::TOO_MANY_REQUESTS, why);
    }
    let query = super::download::raw_query(&headers);
    let body_sha = super::download::body_sha256(&headers);
    if let Err(why) = check_operator(&headers, &query, &body_sha, &key, &secret, crate::web::signing::now_ms()) {
        // Counted like any failed key: the lockout that stops a guessed
        // workspace key stops a guessed operator key too.
        super::download::note_failure(ip);
        let _ = store::record_api_audit(&state.db, None, None, &ip.to_string(), "operator-refused", OPERATOR_PATH).await;
        return err(StatusCode::UNAUTHORIZED, why);
    }
    let email = body.email.trim().to_lowercase();
    if !email.contains('@') {
        return err(StatusCode::BAD_REQUEST, "a workspace needs an email address");
    }
    let name = if body.name.trim().is_empty() { email.clone() } else { body.name.trim().to_string() };

    let account = match store::find_account_by_email(&state.db, &email).await {
        // Only a workspace this endpoint made — one that already carries an
        // operator key, live or revoked. Anyone else's account, found by
        // nothing more than its address, is not the operator's to hand out
        // keys for.
        Ok(Some(a)) => match store::list_api_keys(&state.db, a.account_id).await {
            Ok(keys) if keys.iter().any(|k| k.label == "park river" && k.created_by.is_none()) => a,
            Ok(_) => return err(StatusCode::CONFLICT, "that address already has its own Huntwell account"),
            Err(e) => return oops(e),
        },
        Ok(None) => {
            // The pool holds the password, and nobody needs to know it: the
            // workspace is driven by the key this returns. A person who later
            // wants to sign in resets it by email like any other account.
            let password = format!("Hw-{}-{}", store::new_api_token(), "A9!");
            let sub = match crate::identity::create_user(&email, &password).await {
                Ok(s) => s,
                Err(e) => return err(StatusCode::BAD_REQUEST, &format!("{e:#}")),
            };
            match store::create_account(&state.db, &email, &name, &sub).await {
                Ok(a) => a,
                Err(e) => return err(StatusCode::BAD_REQUEST, &format!("{e:#}")),
            }
        }
        Err(e) => return oops(e),
    };
    // One live key per workspace: a re-provision replaces the key rather than
    // adding to a pile of equally powerful ones nobody can tell apart.
    if let Ok(keys) = store::list_api_keys(&state.db, account.account_id).await {
        for k in keys.iter().filter(|k| k.label == "park river" && !k.revoked) {
            let _ = store::revoke_api_key(&state.db, account.account_id, k.key_id).await;
        }
    }
    match store::create_api_key(&state.db, account.account_id, None, "park river", None, 0, &body.allow_cidr).await {
        Ok(key) => (
            StatusCode::CREATED,
            Json(json!({
                "workspace_id": account.account_id,
                "workspace": account.display_name,
                "email": account.email,
                "api_key": key.token,
                // Every request is signed with it; shown this once.
                "api_secret": key.secret,
                "key_hint": key.token_hint,
            })),
        )
            .into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &format!("{e:#}")),
    }
}

// ---- prospects --------------------------------------------------------------

/// A prospect in full — every field a CRM needs to create a contact, which the
/// artifact summary deliberately does not carry.
fn prospect_json(p: &store::ProspectRow) -> Value {
    json!({
        "id": p.prospect_id,
        "plan_id": p.plan_id,
        "plan": p.source,
        "name": p.name,
        "title": p.title,
        "company": p.company,
        "industry": p.industry,
        "email": p.email,
        "email_status": p.email_status,
        "phone": p.phone,
        "website": p.website,
        "linkedin": p.linkedin,
        "location": p.location,
        "notes": p.notes,
        "estimated_value": p.estimated_value,
        "source_key": p.source_key,
        "first_seen_at": p.first_seen_utc.to_rfc3339(),
        "last_seen_at": p.last_seen_utc.to_rfc3339(),
    })
}

/// What a plan has found, for a client that keeps its own copy.
///
/// `after` is the last `id` the caller stored; the reply carries
/// `next_cursor`, which is that id advanced, and `has_more` when the page was
/// full. Polling with the cursor returns only what is new, so a sync that runs
/// every few minutes costs one small query and usually an empty page.
async fn prospects(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", "/v1/prospects").await {
        Ok(c) => c,
        Err(r) => return r,
    };
    // A key pinned to a plan is answered for that plan whatever it asks for.
    let plan = c.plan.or_else(|| q.get("plan_id").and_then(|v| v.trim().parse().ok()));
    let after = num(&q, "after", 0);
    let limit = num(&q, "limit", 100).clamp(1, 500);
    match store::list_prospects_after(&state.db, c.workspace, plan, after, limit).await {
        Ok(rows) => {
            let next = rows.last().map(|r| r.prospect_id).unwrap_or(after);
            Json(json!({
                "data": rows.iter().map(prospect_json).collect::<Vec<_>>(),
                "next_cursor": next,
                "has_more": rows.len() as i64 == limit,
            }))
            .into_response()
        }
        Err(e) => oops(e),
    }
}

// ---- artifacts --------------------------------------------------------------

/// Everything a plan has found, whatever kind it collects: rows, documents and
/// files come back through one shape.
async fn artifacts(state: &App, c: &Caller, plan: Option<i64>, q: &HashMap<String, String>) -> Response {
    let f = store::ResultsFilter {
        // A pinned key's plan wins over whatever the caller asks for.
        plan_id: c.plan.or(plan),
        search: q.get("search").cloned().unwrap_or_default(),
        limit: num(q, "limit", 100),
        offset: num(q, "offset", 0),
    };
    match store::list_results(&state.db, c.workspace, &f).await {
        Ok((rows, total)) => Json(json!({ "data": rows, "total": total, "limit": f.limit, "offset": f.offset })).into_response(),
        Err(e) => oops(e),
    }
}

async fn plan_artifacts(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", &format!("/v1/plans/{id}/artifacts")).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match load_plan(&state, &c, id).await {
        Ok(_) => artifacts(&state, &c, Some(id), &q).await,
        Err(r) => r,
    }
}

async fn all_artifacts(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", "/v1/artifacts").await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let plan = q.get("plan_id").and_then(|v| v.parse().ok());
    artifacts(&state, &c, plan, &q).await
}

/// The plan's knowledge graph: the searches it has run, the pages those opened,
/// what came out, and the edges between them.
async fn plan_graph(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", &format!("/v1/plans/{id}/graph")).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let id = match scoped(&c, id) {
        Ok(i) => i,
        Err(r) => return r,
    };
    match super::api::graph_payload(&state, c.workspace, id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, &format!("{e:#}")),
    }
}

// ---- usage ------------------------------------------------------------------

async fn usage(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", "/v1/usage").await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match store::ensure_usage(&state.db, c.workspace).await {
        Ok(u) => Json(json!({
            "period_start": u.period_start,
            "budget_usd": u.available_usd,
            "used_usd": u.used_usd,
            "remaining_usd": u.remaining_usd,
            "credits_usd": u.credits_usd,
            "tokens_used": u.tokens_used,
        }))
        .into_response(),
        Err(e) => oops(e),
    }
}

#[cfg(test)]
mod operator_tests {
    use super::*;
    use crate::web::signing::{payload, sha256_hex, sign, verify, Presented, EMPTY_BODY_SHA256};

    const KEY: &str = "hwo_park_river_operator";
    const SECRET: &str = "hwos_0123456789abcdef0123456789abcdef";
    const BODY: &str = r#"{"email":"acme@tenants.parkriver.example","name":"Acme"}"#;

    fn signed(key: &str, secret: &str, method: &str, path: &str, ts: i64, nonce: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-hw-key", key.parse().unwrap());
        h.insert("x-hw-ts", ts.to_string().parse().unwrap());
        h.insert("x-hw-nonce", nonce.parse().unwrap());
        h.insert("x-hw-sign", sign(secret, &payload(method, path, "", ts, nonce, &sha256_hex(BODY.as_bytes()))).parse().unwrap());
        h
    }

    #[test]
    fn a_signed_operator_request_is_accepted_once() {
        let now = 1_758_579_312_000;
        let h = signed(KEY, SECRET, "POST", OPERATOR_PATH, now, "op-accepted-once-0001");
        let body = sha256_hex(BODY.as_bytes());
        assert_eq!(check_operator(&h, "", &body, KEY, SECRET, now), Ok(()));
        // The very same request again: a replay.
        assert!(check_operator(&h, "", &body, KEY, SECRET, now + 1_000).unwrap_err().contains("nonce"));
    }

    #[test]
    fn a_bearer_token_is_refused_and_says_why() {
        let mut h = HeaderMap::new();
        h.insert(axum::http::header::AUTHORIZATION, format!("Bearer {SECRET}").parse().unwrap());
        let why = check_operator(&h, "", EMPTY_BODY_SHA256, KEY, SECRET, 0).unwrap_err();
        assert!(why.contains("no longer takes a bearer token"), "{why}");
    }

    /// Vectors shared with Park River (`shared::huntwell` and
    /// `advisor::prospecting` there pin the same numbers), so the two ends
    /// cannot drift apart unnoticed. Secret, time and nonce are fixed; the
    /// body is the exact bytes sent.
    #[test]
    fn the_shared_vectors_verify() {
        let now = 1_758_579_312_000;
        let nonce = "0123456789abcdef0123456789abcdef";
        let p = |sig: &str| Presented { key: "hwk".into(), timestamp: now, nonce: nonce.into(), signature: sig.into() };
        let op = sign(SECRET, &payload("POST", OPERATOR_PATH, "", now, nonce, &sha256_hex(BODY.as_bytes())));
        assert_eq!(op, OPERATOR_VECTOR, "operator vector changed: {op}");
        let get = sign("hws_test_secret", &payload("GET", "/v1/plans/42/executions", "limit=1", now, nonce, EMPTY_BODY_SHA256));
        assert_eq!(get, GET_VECTOR, "GET vector changed: {get}");
        assert_eq!(verify("hws_test_secret", &p(GET_VECTOR), "GET", "/v1/plans/42/executions", "limit=1", EMPTY_BODY_SHA256, now), Ok(()));
        let post_body = sha256_hex(br#"{"target":25}"#);
        let post = sign("hws_test_secret", &payload("POST", "/v1/plans/42/executions", "", now, nonce, &post_body));
        assert_eq!(post, POST_VECTOR, "POST vector changed: {post}");
        // The same POST with its body edited in flight no longer verifies.
        assert!(verify("hws_test_secret", &p(POST_VECTOR), "POST", "/v1/plans/42/executions", "", &sha256_hex(br#"{"target":9999}"#), now).is_err());
    }

    const OPERATOR_VECTOR: &str = "903ae48ad1f74d7e9ef15f8de315f9118f871da93aa36491c8c25b481c7d8c2b";
    const GET_VECTOR: &str = "07a9e55879b1175823b3d411f6670c0048245411a30d3143b97d0b6329e914fa";
    const POST_VECTOR: &str = "ed77ded00addae1ebe61dab7db4b6e7a8d8a7cdd28d4c2dfea6691b47d2c977e";

    #[test]
    fn anything_else_is_refused() {
        let now = 1_758_579_312_000;
        let body = sha256_hex(BODY.as_bytes());
        // Another key, however well signed.
        assert!(check_operator(&signed("hwk_live_someone", SECRET, "POST", OPERATOR_PATH, now, "op-refused-000001"), "", &body, KEY, SECRET, now).is_err());
        // The right key, the wrong secret.
        assert!(check_operator(&signed(KEY, "hwos_wrong_wrong_wrong_wrong_wrong_", "POST", OPERATOR_PATH, now, "op-refused-000002"), "", &body, KEY, SECRET, now).is_err());
        // A signature made for another verb or another route.
        assert!(check_operator(&signed(KEY, SECRET, "GET", OPERATOR_PATH, now, "op-refused-000003"), "", &body, KEY, SECRET, now).is_err());
        assert!(check_operator(&signed(KEY, SECRET, "POST", "/v1/plans", now, "op-refused-000004"), "", &body, KEY, SECRET, now).is_err());
        // Replayed after the window.
        assert!(check_operator(&signed(KEY, SECRET, "POST", OPERATOR_PATH, now, "op-refused-000005"), "", &body, KEY, SECRET, now + 60_000).is_err());
        // A query added to a signature that did not cover one.
        assert!(check_operator(&signed(KEY, SECRET, "POST", OPERATOR_PATH, now, "op-refused-000006"), "email=x", &body, KEY, SECRET, now).is_err());
        // A different body than the one signed.
        assert!(check_operator(&signed(KEY, SECRET, "POST", OPERATOR_PATH, now, "op-refused-000007"), "", &sha256_hex(b"{}"), KEY, SECRET, now).is_err());
    }
}


// ---- the live stream --------------------------------------------------------

/// How often the stream looks at the workspace when nothing has woken it.
/// The bus wakes it sooner; this is what makes it right without one.
const STREAM_POLL: std::time::Duration = std::time::Duration::from_secs(2);
/// A frame this often even when nothing changed, so a proxy in between does
/// not close an idle socket and the caller can tell a quiet workspace from a
/// dead connection.
const STREAM_PING: std::time::Duration = std::time::Duration::from_secs(20);

/// `GET /v1/stream` — a websocket carrying everything that happens in the
/// workspace, as it happens.
///
/// Signed like every other `/v1` call (`X-HW-KEY`, `X-HW-TS`, `X-HW-SIGN` over
/// `GET\n/v1/stream\nQUERY\nTS`) — on the upgrade request itself, which a
/// server-side client can send headers on. `after` is the last prospect id
/// the caller already has, so a reconnect resumes rather than replays.
///
/// Frames, one JSON object each:
///
/// ```text
/// {"type":"hello","workspace_id":1,"plans":[…],"runs":[…]}   first, the state now
/// {"type":"plan","data":{…}}                                  created, drafted, renamed, rescheduled
/// {"type":"plan.deleted","id":4}
/// {"type":"run","data":{…}}                                   queued, running (with its count so far), finished
/// {"type":"prospect","data":{…}}                              each one found, in id order
/// {"type":"ping"}
/// ```
///
/// Postgres is the source of truth, as everywhere here: the stream compares
/// the workspace's rows on every pass and sends what changed. The bus only
/// wakes it early — an event for this workspace means "look now" — so it is
/// immediate with a bus and at most `STREAM_POLL` late without one, and it
/// never misses what another process wrote.
async fn stream(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "GET", "/v1/stream").await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let after = num(&q, "after", 0);
    ws.on_upgrade(move |socket| live(state, c, after, socket))
}

/// What the stream last told the caller, to send only what changed.
#[derive(Default)]
struct Seen {
    plans: HashMap<i64, String>,
    runs: HashMap<i64, String>,
    prospect_after: i64,
}

fn plan_summary_json(s: &store::PlanSummary) -> Value {
    let mut v = plan_json(&s.plan);
    v["artifacts"] = json!(s.prospects);
    v["executions"] = json!(s.executions);
    v
}

/// One pass over the workspace: the frames to send, and the new `Seen`.
async fn stream_pass(state: &App, c: &Caller, seen: &mut Seen, first: bool) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();

    let plans: Vec<Value> = store::list_plans(&state.db, c.workspace)
        .await?
        .iter()
        .filter(|s| c.plan.is_none_or(|p| p == s.plan.plan_id))
        .map(plan_summary_json)
        .collect();
    let runs: Vec<Value> = store::list_executions(&state.db, c.workspace, c.plan, 25)
        .await?
        .iter()
        .map(run_json)
        .collect();

    if first {
        out.push(json!({ "type": "hello", "workspace_id": c.workspace, "plans": plans, "runs": runs }));
        for p in &plans {
            seen.plans.insert(p["id"].as_i64().unwrap_or(0), p.to_string());
        }
        for r in &runs {
            seen.runs.insert(r["id"].as_i64().unwrap_or(0), r.to_string());
        }
    } else {
        let mut live = std::collections::HashSet::new();
        for p in plans {
            let id = p["id"].as_i64().unwrap_or(0);
            live.insert(id);
            let text = p.to_string();
            if seen.plans.get(&id) != Some(&text) {
                seen.plans.insert(id, text);
                out.push(json!({ "type": "plan", "data": p }));
            }
        }
        let gone: Vec<i64> = seen.plans.keys().filter(|id| !live.contains(id)).copied().collect();
        for id in gone {
            seen.plans.remove(&id);
            out.push(json!({ "type": "plan.deleted", "id": id }));
        }
        for r in runs {
            let id = r["id"].as_i64().unwrap_or(0);
            let text = r.to_string();
            if seen.runs.get(&id) != Some(&text) {
                seen.runs.insert(id, text);
                out.push(json!({ "type": "run", "data": r }));
            }
        }
    }

    // Prospects in id order from the caller's cursor, a page at a time.
    loop {
        let rows = store::list_prospects_after(&state.db, c.workspace, c.plan, seen.prospect_after, 200).await?;
        let full = rows.len() == 200;
        for r in &rows {
            seen.prospect_after = seen.prospect_after.max(r.prospect_id);
            out.push(json!({ "type": "prospect", "data": prospect_json(r) }));
        }
        if !full {
            break;
        }
    }
    Ok(out)
}

/// How often an open stream re-checks the key it was opened with.
const STREAM_KEY_RECHECK: std::time::Duration = std::time::Duration::from_secs(10);

async fn live(state: App, c: Caller, after: i64, socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};

    let (mut tx, mut rx) = socket.split();
    let mut seen = Seen { prospect_after: after, ..Seen::default() };
    // The whole bus, filtered to this workspace below. None without a bus:
    // the poll alone carries the stream then.
    let mut bus = crate::bus::subscribe(crate::bus::subject::ALL).await;
    let mut first = true;
    let mut last_sent = std::time::Instant::now();
    let mut last_key_check = std::time::Instant::now();

    loop {
        // The key was checked when the stream opened; it is checked again as
        // the stream runs, so revoking it, its expiry, or its maker leaving
        // the team ends the stream within `STREAM_KEY_RECHECK` rather than
        // whenever the caller happens to hang up.
        if last_key_check.elapsed() >= STREAM_KEY_RECHECK {
            last_key_check = std::time::Instant::now();
            match store::api_key_live(&state.db, c.key_id).await {
                Ok(true) => {}
                Ok(false) => {
                    let _ = tx.send(Message::Text(json!({ "type": "closed", "error": "this key is no longer valid" }).to_string().into())).await;
                    let _ = tx.send(Message::Close(None)).await;
                    return;
                }
                Err(e) => tracing::warn!("v1 stream key check: {e:#}"),
            }
        }
        match stream_pass(&state, &c, &mut seen, first).await {
            Ok(frames) => {
                for f in frames {
                    if tx.send(Message::Text(f.to_string().into())).await.is_err() {
                        return;
                    }
                    last_sent = std::time::Instant::now();
                }
            }
            Err(e) => {
                tracing::warn!("v1 stream for workspace {}: {e:#}", c.workspace);
                let _ = tx.send(Message::Text(json!({ "type": "error", "error": "internal error" }).to_string().into())).await;
                return;
            }
        }
        first = false;
        if last_sent.elapsed() >= STREAM_PING {
            if tx.send(Message::Text(json!({ "type": "ping" }).to_string().into())).await.is_err() {
                return;
            }
            last_sent = std::time::Instant::now();
        }

        // Wait for the next reason to look: a bus event for this workspace,
        // the poll interval, or the caller going away.
        let deadline = tokio::time::sleep(STREAM_POLL);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                incoming = rx.next() => match incoming {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => {}
                },
                ev = async { match bus.as_mut() { Some(b) => b.next().await, None => std::future::pending().await } } => {
                    match ev {
                        Some(msg) => {
                            let ours = crate::bus::decode(&msg).is_some_and(|e| e.account_id == Some(c.workspace));
                            if ours {
                                // A burst of events is one look, not many.
                                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                                break;
                            }
                        }
                        // The bus went away; carry on polling.
                        None => bus = None,
                    }
                }
            }
        }
    }
}

// ---- outreach -----------------------------------------------------------------
//
// The same drafting as the app (`super::outreach`): same checks, same billing,
// same history. Outreach spans the workspace, so a key pinned to one plan is
// refused, as it is for creating plans.

/// A draft as the API returns it: the stored fields plus the pieces a caller
/// would otherwise assemble — the address line, the body with its footer, and
/// the whole email ready to paste.
fn outreach_json(o: &store::OutreachRow) -> Value {
    let to = match (o.recipient_name.trim(), o.recipient_email.trim()) {
        ("", e) => e.to_string(),
        (n, "") => n.to_string(),
        (n, e) => format!("{n} <{e}>"),
    };
    let body = if o.footer.trim().is_empty() {
        o.body.trim_end().to_string()
    } else {
        format!("{}\n\n{}", o.body.trim_end(), o.footer.trim_end())
    };
    let email = format!("{}Subject: {}\n\n{}", if to.is_empty() { String::new() } else { format!("To: {to}\n") }, o.subject, body);
    json!({
        "id": o.outreach_id,
        "prospect_id": o.prospect_id,
        // The plan whose own outreach it was written to; null = the workspace's.
        "plan_id": o.plan_id,
        "design_id": o.design_id,
        "artifact_id": o.artifact_id,
        "campaign": o.campaign,
        "recipient": {
            "name": o.recipient_name,
            "email": o.recipient_email,
            "title": o.recipient_title,
            "company": o.recipient_company,
            "notes": o.recipient_notes,
        },
        "to": to,
        "subject": o.subject,
        "body": o.body,
        "footer": o.footer,
        "body_with_footer": body,
        "email": email,
        "version": o.version,
        "created_by": o.created_by_name,
        "created_at": o.created_at.to_rfc3339(),
        "updated_at": o.updated_at.to_rfc3339(),
    })
}

/// Authenticate for an outreach route, refuse a plan-pinned key, and say who
/// is writing: the key's maker, or the workspace's owner for a key with none.
async fn outreach_caller(
    state: &App,
    headers: &HeaderMap,
    q: &HashMap<String, String>,
    peer: SocketAddr,
    method: &str,
    path: &str,
) -> Result<(Caller, super::outreach::Writer), Response> {
    let c = caller(state, headers, q, peer, method, path).await?;
    if c.plan.is_some() {
        return Err(err(StatusCode::FORBIDDEN, "this key is scoped to one plan; outreach needs a workspace key"));
    }
    let person_id = c.person.unwrap_or(c.workspace);
    let person = match store::get_account(&state.db, person_id).await {
        Ok(Some(a)) => a,
        Ok(None) => return Err(err(StatusCode::FORBIDDEN, "the person this key was made by no longer has an account")),
        Err(e) => return Err(oops(e)),
    };
    let w = super::outreach::Writer::for_person(state, c.workspace, &person).await;
    Ok((c, w))
}

async fn outreach_list(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (c, _) = match outreach_caller(&state, &headers, &q, peer, "GET", "/v1/outreach").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let limit = num(&q, "limit", 100).clamp(1, 500);
    match store::list_outreach(&state.db, c.workspace, limit).await {
        Ok(rows) => Json(json!({ "data": rows.iter().map(outreach_json).collect::<Vec<_>>() })).into_response(),
        Err(e) => oops(e),
    }
}

async fn outreach_create(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Json(body): Json<super::outreach::CreateBody>,
) -> Response {
    let (c, w) = match outreach_caller(&state, &headers, &q, peer, "POST", "/v1/outreach").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = needs_plans(&c) {
        return r;
    }
    match super::outreach::draft_new(&state, &w, body).await {
        Ok(o) => (StatusCode::CREATED, Json(outreach_json(&o))).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn outreach_get(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (c, _) = match outreach_caller(&state, &headers, &q, peer, "GET", &format!("/v1/outreach/{id}")).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let o = match store::get_outreach(&state.db, c.workspace, id).await {
        Ok(Some(o)) => o,
        Ok(None) => return err(StatusCode::NOT_FOUND, "draft not found"),
        Err(e) => return oops(e),
    };
    match store::list_outreach_versions(&state.db, c.workspace, id).await {
        Ok(v) => {
            let mut out = outreach_json(&o);
            out["versions"] = json!(v);
            Json(out).into_response()
        }
        Err(e) => oops(e),
    }
}

async fn outreach_edit(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
    Json(body): Json<super::outreach::EditBody>,
) -> Response {
    let (c, w) = match outreach_caller(&state, &headers, &q, peer, "PATCH", &format!("/v1/outreach/{id}")).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = needs_plans(&c) {
        return r;
    }
    match super::outreach::edit_draft(&state, &w, id, &body.subject, &body.body).await {
        Ok(o) => Json(outreach_json(&o)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn outreach_revise(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
    Json(body): Json<super::outreach::ReviseBody>,
) -> Response {
    let (c, w) = match outreach_caller(&state, &headers, &q, peer, "POST", &format!("/v1/outreach/{id}/revise")).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = needs_plans(&c) {
        return r;
    }
    match super::outreach::revise_draft(&state, &w, id, &body.feedback).await {
        Ok(o) => Json(outreach_json(&o)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn outreach_restore(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
    Json(body): Json<super::outreach::RestoreBody>,
) -> Response {
    let (c, w) = match outreach_caller(&state, &headers, &q, peer, "POST", &format!("/v1/outreach/{id}/restore")).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = needs_plans(&c) {
        return r;
    }
    match super::outreach::restore_draft(&state, &w, id, body.version).await {
        Ok(o) => Json(outreach_json(&o)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn outreach_delete(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (c, _) = match outreach_caller(&state, &headers, &q, peer, "DELETE", &format!("/v1/outreach/{id}")).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = needs_plans(&c) {
        return r;
    }
    match store::delete_outreach(&state.db, c.workspace, id).await {
        Ok(true) => Json(json!({ "deleted": id })).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "draft not found"),
        Err(e) => oops(e),
    }
}

/// A plan's own outreach design — what a draft for one of its prospects is
/// written from — with the workspace's beside it, so a caller can show what a
/// blank field falls back to. The same as the app's `GET /plans/{id}/outreach`.
async fn plan_outreach(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (c, _) = match outreach_caller(&state, &headers, &q, peer, "GET", &format!("/v1/plans/{id}/outreach")).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    plan_outreach_json(&state, c.workspace, id).await
}

async fn plan_outreach_json(state: &App, workspace: i64, id: i64) -> Response {
    let o = match store::get_plan_outreach(&state.db, workspace, id).await {
        Ok(Some(o)) => o,
        Ok(None) => return err(StatusCode::NOT_FOUND, "plan not found"),
        Err(e) => return oops(e),
    };
    let ws = match store::get_outreach_profile(&state.db, workspace).await {
        Ok(p) => p,
        Err(e) => return oops(e),
    };
    Json(json!({
        "plan_id": id,
        "custom": o.custom,
        "design_id": o.design_id,
        "brief": o.brief,
        "product": o.product,
        "rules": o.rules,
        "workspace": { "product": ws.product, "rules": ws.rules },
    }))
    .into_response()
}

#[derive(Deserialize)]
struct PlanOutreachBody {
    #[serde(default)]
    custom: Option<bool>,
    #[serde(default)]
    brief: Option<String>,
    #[serde(default)]
    product: Option<String>,
    #[serde(default)]
    rules: Option<String>,
}

/// Any of custom, brief, product and rules; what is left out stays as it is.
/// Turning `custom` on makes the plan's own text what its drafts are written
/// from; off, they fall back to the workspace (or the plan's saved profile).
async fn plan_save_outreach(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
    Json(body): Json<PlanOutreachBody>,
) -> Response {
    let (c, _) = match outreach_caller(&state, &headers, &q, peer, "PUT", &format!("/v1/plans/{id}/outreach")).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = needs_plans(&c) {
        return r;
    }
    use crate::outreach::{MAX_BRIEF, MAX_PRODUCT, MAX_RULES};
    for (what, v, max) in [("brief", &body.brief, MAX_BRIEF), ("product", &body.product, MAX_PRODUCT), ("rules", &body.rules, MAX_RULES)] {
        if v.as_ref().is_some_and(|v| v.chars().count() > max) {
            return err(StatusCode::BAD_REQUEST, &format!("{what} must be {max} characters or fewer"));
        }
    }
    let current = match store::get_plan_outreach(&state.db, c.workspace, id).await {
        Ok(Some(o)) => o,
        Ok(None) => return err(StatusCode::NOT_FOUND, "plan not found"),
        Err(e) => return oops(e),
    };
    let o = store::PlanOutreach {
        custom: body.custom.unwrap_or(current.custom),
        design_id: current.design_id,
        brief: body.brief.as_deref().map(|v| v.trim().to_string()).unwrap_or(current.brief),
        product: body.product.as_deref().map(|v| v.trim().to_string()).unwrap_or(current.product),
        rules: body.rules.as_deref().map(|v| v.trim().to_string()).unwrap_or(current.rules),
    };
    if o.custom && o.brief.is_empty() && o.product.is_empty() && o.rules.is_empty() {
        return err(StatusCode::BAD_REQUEST, "say who this campaign is for, or what to offer them, before tailoring it");
    }
    match store::set_plan_outreach(&state.db, c.workspace, id, &o).await {
        Ok(true) => plan_outreach_json(&state, c.workspace, id).await,
        Ok(false) => err(StatusCode::NOT_FOUND, "plan not found"),
        Err(e) => oops(e),
    }
}

async fn outreach_profile(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (c, w) = match outreach_caller(&state, &headers, &q, peer, "GET", "/v1/outreach/profile").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let p = match store::get_outreach_profile(&state.db, c.workspace).await {
        Ok(p) => p,
        Err(e) => return oops(e),
    };
    let footer = store::get_outreach_footer(&state.db, w.person).await.unwrap_or_default();
    Json(json!({ "product": p.product, "rules": p.rules, "footer": footer })).into_response()
}

#[derive(Deserialize)]
struct OutreachProfileBody {
    #[serde(default)]
    product: Option<String>,
    #[serde(default)]
    rules: Option<String>,
    #[serde(default)]
    footer: Option<String>,
}

/// Any of product, rules and footer; what is left out stays as it is.
async fn outreach_save_profile(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Json(body): Json<OutreachProfileBody>,
) -> Response {
    let (c, w) = match outreach_caller(&state, &headers, &q, peer, "PUT", "/v1/outreach/profile").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = needs_plans(&c) {
        return r;
    }
    use crate::outreach::{MAX_FOOTER, MAX_PRODUCT, MAX_RULES};
    for (what, v, max) in [("product", &body.product, MAX_PRODUCT), ("rules", &body.rules, MAX_RULES), ("footer", &body.footer, MAX_FOOTER)] {
        if v.as_ref().is_some_and(|v| v.chars().count() > max) {
            return err(StatusCode::BAD_REQUEST, &format!("{what} must be {max} characters or fewer"));
        }
    }
    let current = match store::get_outreach_profile(&state.db, c.workspace).await {
        Ok(p) => p,
        Err(e) => return oops(e),
    };
    if body.product.is_some() || body.rules.is_some() {
        let product = body.product.as_deref().map(str::trim).unwrap_or(&current.product);
        let rules = body.rules.as_deref().map(str::trim).unwrap_or(&current.rules);
        if let Err(e) = store::set_outreach_profile(&state.db, c.workspace, product, rules, w.person).await {
            return oops(e);
        }
    }
    if let Some(f) = &body.footer {
        // A footer is a person's own. A key with no recorded maker (the
        // operator's, or one from before makers were recorded) has no person
        // to set it for — and falling back to the owner would rewrite theirs.
        if c.person.is_none() {
            return err(StatusCode::FORBIDDEN, "this key has no person behind it, so it cannot set a footer — use a key made by a teammate");
        }
        if let Err(e) = store::set_outreach_footer(&state.db, w.person, f.trim_end()).await {
            return oops(e);
        }
    }
    // Answered from what was just written — not by calling the GET handler,
    // which would verify this PUT's signature a second time, as a GET.
    let p = match store::get_outreach_profile(&state.db, c.workspace).await {
        Ok(p) => p,
        Err(e) => return oops(e),
    };
    let footer = store::get_outreach_footer(&state.db, w.person).await.unwrap_or_default();
    Json(json!({ "product": p.product, "rules": p.rules, "footer": footer })).into_response()
}

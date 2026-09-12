//! The public API: `/v1`, authenticated by an API key.
//!
//! Everything the app can do, a key can do — create a plan from a sentence, run
//! it, watch the run, read what it found, and see what it cost. The app's own
//! `/api` routes are a cookie-authenticated implementation detail that changes
//! with the UI; this surface is the promise, so it is versioned, JSON in and
//! JSON out, and named in the product's own words (plans, runs, artifacts).
//!
//! Auth, rate limiting, address pinning and the audit trail are the download
//! API's, reused wholesale: `Authorization: Bearer <key>`, or `?token=` where a
//! header is impossible.

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
        .route("/executions", get(list_executions))
        .route("/executions/{id}", get(get_execution))
        .route("/executions/{id}/log", get(execution_log))
        .route("/executions/{id}/cancel", post(cancel_execution))
        .route("/usage", get(usage))
}

/// The authenticated caller: which workspace, and whether the key is pinned to
/// a single plan.
struct Caller {
    workspace: i64,
    plan: Option<i64>,
}

/// Authenticates and, for a plan-scoped route, refuses a key pinned elsewhere.
/// One helper so a pinned key cannot reach past its plan on any route.
async fn caller(
    state: &App,
    headers: &HeaderMap,
    q: &HashMap<String, String>,
    peer: SocketAddr,
    path: &str,
) -> Result<Caller, Response> {
    let key = authenticate(state, headers, q, peer, path).await?;
    Ok(Caller { workspace: key.account_id, plan: key.plan_id })
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
    let c = match caller(&state, &headers, &q, peer, "/v1/me").await {
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
    let c = match caller(&state, &headers, &q, peer, "/v1/plans").await {
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
    let c = match caller(&state, &headers, &q, peer, "/v1/plans").await {
        Ok(c) => c,
        Err(r) => return r,
    };
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
    let c = match caller(&state, &headers, &q, peer, "/v1/plans/{id}").await {
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
    let c = match caller(&state, &headers, &q, peer, "/v1/plans/{id}").await {
        Ok(c) => c,
        Err(r) => return r,
    };
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
    let c = match caller(&state, &headers, &q, peer, "/v1/plans/{id}").await {
        Ok(c) => c,
        Err(r) => return r,
    };
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
    let c = match caller(&state, &headers, &q, peer, "/v1/plans/{id}/executions").await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let plan = match load_plan(&state, &c, id).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let args = crate::pipeline::RunArgs {
        target: q.get("target").and_then(|v| v.parse().ok()),
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
    let c = match caller(&state, &headers, &q, peer, "/v1/plans/{id}/executions").await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let id = match scoped(&c, id) {
        Ok(i) => i,
        Err(r) => return r,
    };
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
    let c = match caller(&state, &headers, &q, peer, "/v1/executions").await {
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
    let c = match caller(&state, &headers, &q, peer, "/v1/executions/{id}").await {
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
    let c = match caller(&state, &headers, &q, peer, "/v1/executions/{id}/log").await {
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
    let c = match caller(&state, &headers, &q, peer, "/v1/executions/{id}/cancel").await {
        Ok(c) => c,
        Err(r) => return r,
    };
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

// ---- artifacts --------------------------------------------------------------

/// Everything a plan has found, whatever kind it collects: rows, documents and
/// files come back through one shape.
async fn artifacts(state: &App, c: &Caller, plan: Option<i64>, q: &HashMap<String, String>) -> Response {
    let f = store::ResultsFilter {
        plan_id: plan.or(c.plan),
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
    let c = match caller(&state, &headers, &q, peer, "/v1/plans/{id}/artifacts").await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match scoped(&c, id) {
        Ok(id) => artifacts(&state, &c, Some(id), &q).await,
        Err(r) => r,
    }
}

async fn all_artifacts(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let c = match caller(&state, &headers, &q, peer, "/v1/artifacts").await {
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
    let c = match caller(&state, &headers, &q, peer, "/v1/plans/{id}/graph").await {
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
    let c = match caller(&state, &headers, &q, peer, "/v1/usage").await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match store::ensure_usage(&state.db, c.workspace).await {
        Ok(u) => Json(json!({
            "period_start": u.period_start,
            "budget_usd": u.budget_usd + u.topups_usd,
            "used_usd": u.used_usd,
            "remaining_usd": u.remaining_usd,
            "tokens_used": u.tokens_used,
        }))
        .into_response(),
        Err(e) => oops(e),
    }
}

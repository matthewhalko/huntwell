//! The JSON API behind the UI. Every handler takes `AuthUser`, and every store
//! call is scoped to that account.

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::stream::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};

use super::auth::{self, AuthUser};
use super::{bad_request, not_found, runner, ApiError, App};
use crate::csv;
use crate::pipeline::RunArgs;
use crate::plan_chat;
use crate::store::{self, SourceConfig};

/// The whole API, for the all-in-one `serve` (dev.sh / local-infra).
pub fn router() -> Router<App> {
    auth_routes()
        .merge(team_routes())
        .merge(super::billing::routes())
        .merge(overview_routes())
        .merge(plans_routes())
        .merge(runs_routes())
        .merge(prospects_routes())
}

/// The team around a workspace: who is in it, who has been asked, and which
/// workspace this session is working in.
///
/// The workspace *is* the account every other route scopes by, so sharing one
/// is a matter of membership rather than a second tenant — see membership.sql.
pub fn team_routes() -> Router<App> {
    Router::new()
        .route("/team", get(team).put(rename_workspace))
        .route("/team/invite", post(invite))
        .route("/team/invites/{id}", delete(revoke))
        .route("/team/members/{id}", delete(remove_member))
        .route("/team/workspaces", get(workspaces))
        .route("/team/switch", post(switch_workspace))
        // Redeeming an invitation: reading it needs no membership, only the
        // token, and accepting it is what creates the membership.
        .route("/team/join/{token}", get(invite_preview).post(join))
}

/// Sign-in, sessions, account profile, and the gateway forward-auth endpoint.
/// Owned by the auth service (the `Account`/`Session` tables).
pub fn auth_routes() -> Router<App> {
    Router::new()
        .route("/auth/signup", post(auth::signup))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me).put(auth::update_me))
        .route("/auth/password", post(auth::change_password))
        .route("/internal/introspect", get(auth::introspect))
}

/// The dashboard aggregate and the server-capability probe. In the split
/// deployment these live on the runs service, which fans out to the siblings.
pub fn overview_routes() -> Router<App> {
    Router::new()
        .route("/overview", get(overview))
        .route("/status", get(status))
        .route("/usage", get(usage))
        .route("/usage/topup", post(usage_topup))
}

/// Plan CRUD. Owned by the plans service.
///
/// Every response here goes through [`public_plan`]: a plan's prompts, field
/// mapping and dedupe key are machinery the drafting agent writes, and they
/// are never sent to a browser. Models the owner picked are a setting, not
/// machinery, so those go out. Creating a plan therefore takes a brief rather
/// than a plan body — the server drafts the rest — and updating one takes the
/// handful of fields a person actually owns.
pub fn plans_routes() -> Router<App> {
    Router::new()
        .route("/plans", get(list_plans).post(create_plan))
        .route("/models", get(list_models))
        .route("/plans/{id}", get(get_plan).put(update_plan).delete(delete_plan))
        .route("/plans/{id}/favorite", post(favorite_plan))
        .route("/plans/{id}/rebuild", post(rebuild_plan))
        .route("/plans/{id}/trail", get(plan_trail))
        .route("/plans/{id}/graph", get(plan_graph))
        .route("/plans/{id}/queue", delete(clear_queue))
        // Internal: the runs service asks for a plan's summary for /overview.
        .route("/internal/plan-summary", get(internal_plan_summary))
}

/// A plan as the user's own: what they asked for, what it collects, when it
/// runs, and which models they picked. Nothing about the prompts behind it.
fn public_plan(p: &SourceConfig) -> Value {
    json!({
        "PlanId": p.plan_id,
        "Source": p.source,
        "Description": p.description,
        "Kind": p.kind_of().as_str(),
        "Subject": p.subject,
        "TargetProspects": p.target_prospects,
        "Effort": store::normalize_effort(&p.effort),
        "Status": store::public_draft_status(&p.draft_status),
        "Favorite": p.favorite,
        "AlertEmail": p.alert_email,
        "ScheduleEnabled": p.schedule_enabled,
        "ScheduleTime": p.schedule_time,
        "ScheduleDays": p.schedule_days,
        "NextRunAt": p.next_run_at.map(|t| t.to_rfc3339()),
        "UpdatedAt": p.updated_at,
        // Whether it can run at all, so the UI can say so without being told
        // which prompt is missing.
        "Ready": store::plan_ready(p).is_ok(),
        // The owner-chosen models. Prompts stay hidden; these do not — they
        // are a setting the person picked, not machinery the drafter wrote.
        "Models": {
            "scrape": p.model_scrape,
            "enrich": p.model_enrich,
            "planner": p.model_planner,
        },
    })
}

/// Wakes every SSE log subscriber when a worker books tokens against a run.
/// Postgres is the source of truth; the bus is only the doorbell.
pub fn listen_metered_runs(state: App) {
    tokio::spawn(async move {
        let Some(mut sub) = crate::bus::subscribe(crate::bus::subject::RUN_METERED).await else {
            return;
        };
        use futures_util::StreamExt;
        while let Some(msg) = sub.next().await {
            let Some(event) = crate::bus::decode(&msg) else { continue };
            let Some(execution_id) = event.data.get("execution_id").and_then(|v| v.as_i64()) else {
                continue;
            };
            let _ = state.log_tx.send(super::LogEvent { execution_id, seq: -1 });
        }
    });
}

/// Run lifecycle and live logs. Owned by the runs service (which also hosts the
/// scheduler and dispatches run-worker Jobs).
pub fn runs_routes() -> Router<App> {
    Router::new()
        .route("/executions", get(list_executions).post(start_execution))
        .route("/executions/{id}", get(get_execution))
        .route("/executions/{id}/cancel", post(cancel_execution))
        .route("/executions/{id}/browser", get(execution_browser))
        .route("/executions/{id}/log", get(execution_log_sse))
        .route("/executions/{id}/log.json", get(execution_log_json))
        // Authenticated-login (Browserbase Context) management.
        .route("/browser/login", post(browser_login))
        .route("/browser/login/finish", post(browser_login_finish))
        .route("/browser/connections", get(browser_connections))
}

/// Prospect list/export, the token-gated download API, and API keys. Owned by
/// the prospects service.
pub fn prospects_routes() -> Router<App> {
    Router::new()
        .route("/prospects", get(list_prospects).delete(delete_prospects))
        .route("/prospects/{id}", delete(delete_prospect))
        .route("/prospects.csv", get(prospects_csv))
        .route("/artifacts", get(list_artifacts).delete(delete_artifacts))
        .route("/artifacts/{id}", delete(delete_artifact))
        .route("/artifacts.csv", get(artifacts_csv))
        .route("/reports", get(list_reports))
        .route("/reports/{id}", get(get_report).delete(delete_report))
        // Not "{id}.html": axum allows only one parameter per path segment.
        .route("/reports/{id}/print", get(report_html))
        .route("/assets", get(list_assets))
        .route("/assets/{id}", get(download_asset).delete(delete_asset))
        // Unified, cross-plan searchable results (every kind).
        .route("/results", get(results))
        .route("/keys", get(list_keys).post(create_key))
        .route("/keys/audit", get(key_audit))
        .route("/keys/{id}", delete(delete_key))
        .route("/keys/{id}/revoke", post(revoke_key))
        .route("/keys/{id}/allow", post(allow_key))
        // Internal: the runs service asks for prospect counts for /overview.
        .route("/internal/prospect-summary", get(internal_prospect_summary))
}

// ---- overview / status -------------------------------------------------------

async fn overview(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    // Split deployment: the runs service owns only Run data, so it fans out to
    // the plans and prospects services for their counts. Detected by the
    // presence of a sibling URL (unset in the all-in-one `serve`).
    if let Some(plans_url) = crate::config::sibling_url("PLANS") {
        let id = acc.tenant();
        let runs: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM execution WHERE account_id = $1"#)
            .bind(id).fetch_one(&state.db).await.unwrap_or(0);
        let active_executions: i64 = sqlx::query_scalar(
            r#"SELECT count(*) FROM execution WHERE account_id = $1 AND status IN ('queued','running')"#,
        ).bind(id).fetch_one(&state.db).await.unwrap_or(0);
        let last_execution_at: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar(r#"SELECT max(started_at) FROM execution WHERE account_id = $1"#)
                .bind(id).fetch_one(&state.db).await.unwrap_or(None);
        let recent = store::list_executions(&state.db, id, None, 8).await.unwrap_or_default();

        let plans_sum = sibling_json(&plans_url, "/api/internal/plan-summary", id).await.unwrap_or_default();
        let prospects_sum = crate::config::sibling_url("PROSPECTS")
            .map(|u| async move { sibling_json(&u, "/api/internal/prospect-summary", id).await.unwrap_or_default() });
        let prospects_sum = match prospects_sum {
            Some(f) => f.await,
            None => Value::Null,
        };
        let jget = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
        let o = json!({
            "plans": jget(&plans_sum, "plans"),
            "prospects": jget(&prospects_sum, "prospects"),
            "prospects_7d": jget(&prospects_sum, "prospects_7d"),
            "executions": runs,
            "active_executions": active_executions,
            "last_execution_at": last_execution_at.map(|t| t.to_rfc3339()),
        });
        let latest = prospects_sum.get("latest").cloned().unwrap_or_else(|| json!([]));
        return Ok(Json(json!({ "overview": o, "recent_executions": recent, "latest_prospects": latest })));
    }

    let o = store::overview(&state.db, acc.tenant()).await?;
    let recent = store::list_executions(&state.db, acc.tenant(), None, 8).await?;
    let (latest, _) = store::list_prospects(
        &state.db,
        acc.tenant(),
        &store::ProspectFilter { plan_id: None, min_value: 0, search: String::new(), limit: 8, offset: 0 },
    )
    .await?;
    Ok(Json(json!({ "overview": o, "recent_executions": recent, "latest_prospects": latest })))
}

/// A GET to a sibling service, forwarding the caller's account id as the
/// internal header the split services authenticate by.
async fn sibling_json(base: &str, path: &str, account_id: i64) -> anyhow::Result<Value> {
    let resp = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .header(auth::ACCOUNT_HEADER, account_id.to_string())
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
    Ok(resp.json().await?)
}

/// Internal: how many plans this account has (called by the runs service).
async fn internal_plan_summary(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    let plans: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM plan WHERE account_id = $1"#)
        .bind(acc.tenant()).fetch_one(&state.db).await?;
    Ok(Json(json!({ "plans": plans })))
}

/// Internal: prospect counts + the newest few (called by the runs service).
async fn internal_prospect_summary(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    let id = acc.tenant();
    let prospects: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM prospect WHERE account_id = $1"#)
        .bind(id).fetch_one(&state.db).await?;
    let prospects_7d: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM prospect WHERE account_id = $1 AND first_seen_utc > now() - interval '7 days'"#,
    ).bind(id).fetch_one(&state.db).await?;
    let (latest, _) = store::list_prospects(
        &state.db,
        id,
        &store::ProspectFilter { plan_id: None, min_value: 0, search: String::new(), limit: 8, offset: 0 },
    )
    .await?;
    Ok(Json(json!({ "prospects": prospects, "prospects_7d": prospects_7d, "latest": latest })))
}

/// The account's token budget and consumption for the current period — the
/// meter the UI shows and the cap the run dispatcher enforces.
async fn usage(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    let u = store::ensure_usage(&state.db, acc.tenant()).await?;
    Ok(Json(serde_json::to_value(u).unwrap_or_else(|_| json!({}))))
}

#[derive(Deserialize)]
struct TopupBody {
    usd: f64,
}

/// Adds dollars to the account's budget for this period. The hook a payment flow
/// calls; until then it lets an operator grant headroom.
async fn usage_topup(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<TopupBody>) -> Result<Json<Value>, ApiError> {
    if !(body.usd > 0.0) {
        return Err(bad_request("usd must be positive"));
    }
    let micros = (body.usd * 1_000_000.0).round() as i64;
    let u = store::add_topup(&state.db, acc.tenant(), micros).await?;
    Ok(Json(serde_json::to_value(u).unwrap_or_else(|_| json!({}))))
}

/// What this server can do: whether the agent and Chrome are reachable.
async fn status(State(_state): State<App>, AuthUser(_): AuthUser) -> Json<Value> {
    let agent_ok = tokio::task::spawn_blocking(|| {
        let bin = std::env::var("HUNTWELL_AGENT").or_else(|_| std::env::var("CURSOR_AGENT_BIN")).unwrap_or_else(|_| "agent".into());
        std::process::Command::new(bin)
            .arg("--help")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    let chrome = crate::browser::chrome_binary().map(|p| p.display().to_string());
    let browserbase = crate::browserbase::configured();
    Json(json!({
        "agent": agent_ok,
        "cursor_key": crate::config::get("CURSOR_API_KEY").is_some(),
        "chrome": chrome,
        "display": crate::browser::display_mode().map(|m| m.label()),
        "browser_backend": if browserbase { "browserbase" } else { "local" },
        "browserbase": browserbase,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ---- team -------------------------------------------------------------------

/// Everyone in the workspace this session is working in, plus the invitations
/// still outstanding. Members see the roster; only an admin sees the tokens,
/// since a token is a way in.
async fn team(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    let workspace = acc.tenant();
    let role = store::workspace_role(&state.db, workspace, acc.account_id)
        .await?
        .ok_or_else(|| not_found("workspace not found"))?;
    let members = store::list_members(&state.db, workspace).await?;
    let invites = if role == "owner" || role == "admin" {
        store::list_invites(&state.db, workspace).await?
    } else {
        Vec::new()
    };
    Ok(Json(json!({
        "workspace_id": workspace,
        "workspace": store::workspace_name(&state.db, workspace).await?,
        "your_role": role,
        "can_invite": role == "owner" || role == "admin",
        "members": members,
        "invites": invites,
    })))
}

/// Only the owner and admins may change who is in a workspace.
async fn require_admin(state: &App, acc: &store::Account) -> Result<i64, ApiError> {
    let workspace = acc.tenant();
    match store::workspace_role(&state.db, workspace, acc.account_id).await?.as_deref() {
        Some("owner") | Some("admin") => Ok(workspace),
        Some(_) => Err(ApiError(StatusCode::FORBIDDEN, "only an admin can change who is on the team".into())),
        None => Err(not_found("workspace not found")),
    }
}

#[derive(Deserialize)]
struct WorkspaceNameBody {
    name: String,
}

/// Names the workspace — the thing everyone in it sees in the switcher and in
/// the invitations it sends.
async fn rename_workspace(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<WorkspaceNameBody>) -> Result<Json<Value>, ApiError> {
    let workspace = require_admin(&state, &acc).await?;
    if body.name.chars().count() > 120 {
        return Err(bad_request("keep the workspace name under 120 characters"));
    }
    store::set_workspace_name(&state.db, workspace, &body.name).await?;
    Ok(Json(json!({ "workspace": store::workspace_name(&state.db, workspace).await? })))
}

#[derive(Deserialize)]
struct InviteBody {
    email: String,
    #[serde(default)]
    role: String,
}

/// Creates an invitation and returns the link to send. There is no mailer
/// here, so the link is the product of this call — the inviter passes it on.
async fn invite(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<InviteBody>) -> Result<Json<Value>, ApiError> {
    let workspace = require_admin(&state, &acc).await?;
    let email = body.email.trim().to_lowercase();
    if !email.contains('@') || email.len() > 320 {
        return Err(bad_request("that does not look like an email address"));
    }
    let role = match body.role.trim() {
        "admin" => "admin",
        _ => "member",
    };
    // Already in? Then there is nothing to invite them to.
    if let Some(existing) = store::find_account_by_email(&state.db, &email).await? {
        if store::workspace_role(&state.db, workspace, existing.account_id).await?.is_some() {
            return Err(bad_request("they are already on this team"));
        }
    }
    let token = store::random_token();
    let row = store::create_invite(&state.db, workspace, &email, role, acc.account_id, &token).await?;

    // Email it when this server knows its own address and has a mail provider;
    // the link is returned either way, because copying it is always allowed to
    // be how an invitation travels.
    let mut emailed = false;
    if let Some(base) = crate::config::get("HUNTWELL_PUBLIC_URL") {
        let link = format!("{}/join/{}", base.trim_end_matches('/'), token);
        let name = store::workspace_name(&state.db, workspace).await?;
        let inviter = if acc.display_name.trim().is_empty() { acc.email.clone() } else { acc.display_name.clone() };
        let msg = crate::mail::invite(&name, &inviter, role, &link);
        // Queued, so the response does not wait on the mail provider. "emailed"
        // now means "we owe this address a message", which is what the row is.
        match store::queue_mail(&state.db, Some(workspace), &email, "invite", &msg).await {
            Ok(_) => emailed = crate::mail::configured(),
            Err(e) => tracing::warn!("could not queue the invite email to {email}: {e:#}"),
        }
    }
    Ok(Json(json!({ "invite": row, "emailed": emailed })))
}

async fn revoke(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let workspace = require_admin(&state, &acc).await?;
    if !store::revoke_invite(&state.db, workspace, id).await? {
        return Err(not_found("invitation not found"));
    }
    Ok(Json(json!({"ok": true})))
}

async fn remove_member(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let workspace = require_admin(&state, &acc).await?;
    if id == workspace {
        return Err(bad_request("the workspace's owner cannot be removed from it"));
    }
    if !store::remove_member(&state.db, workspace, id).await? {
        return Err(not_found("they are not on this team"));
    }
    Ok(Json(json!({"ok": true})))
}

/// The workspaces this person can work in — their own, and any they joined.
async fn workspaces(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    let list = store::list_workspaces(&state.db, acc.account_id).await?;
    Ok(Json(json!({ "workspaces": list, "active": acc.tenant() })))
}

#[derive(Deserialize)]
struct SwitchBody {
    workspace_id: i64,
}

/// Switches which workspace this person is working in. Everything they see
/// afterwards — plans, results, billing — belongs to that workspace.
async fn switch_workspace(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<SwitchBody>) -> Result<Json<Value>, ApiError> {
    if store::workspace_role(&state.db, body.workspace_id, acc.account_id).await?.is_none() {
        return Err(ApiError(StatusCode::FORBIDDEN, "you are not on that team".into()));
    }
    store::set_active_workspace(&state.db, acc.account_id, Some(body.workspace_id)).await?;
    Ok(Json(json!({"workspace_id": body.workspace_id})))
}

/// What an invitation link says before anyone commits to it.
async fn invite_preview(State(state): State<App>, Path(token): Path<String>) -> Result<Json<Value>, ApiError> {
    let Some((_, email, role, workspace_name)) = store::invite_by_token(&state.db, &token).await? else {
        return Err(not_found("this invitation has expired or has already been used"));
    };
    Ok(Json(json!({ "email": email, "role": role, "workspace": workspace_name })))
}

/// Redeems an invitation for the signed-in account.
///
/// The invitation is bound to the address it was sent to, so a forwarded link
/// does not work for whoever it was forwarded to.
async fn join(State(state): State<App>, AuthUser(acc): AuthUser, Path(token): Path<String>) -> Result<Json<Value>, ApiError> {
    let Some((workspace, email, role, name)) = store::invite_by_token(&state.db, &token).await? else {
        return Err(not_found("this invitation has expired or has already been used"));
    };
    if !acc.email.eq_ignore_ascii_case(&email) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            format!("this invitation was sent to {email} — sign in as them to accept it"),
        ));
    }
    if !store::accept_invite(&state.db, &token, acc.account_id).await? {
        return Err(bad_request("that invitation has already been used"));
    }
    store::add_member(&state.db, workspace, acc.account_id, &role, workspace).await?;
    // Land them in the workspace they just joined; it is why they clicked.
    store::set_active_workspace(&state.db, acc.account_id, Some(workspace)).await?;
    Ok(Json(json!({ "workspace_id": workspace, "workspace": name })))
}

// ---- plans ------------------------------------------------------------------

async fn list_plans(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Vec<Value>>, ApiError> {
    let plans = store::list_plans(&state.db, acc.tenant()).await?;
    Ok(Json(
        plans
            .iter()
            .map(|s| {
                let mut v = public_plan(&s.plan);
                v["prospects"] = json!(s.prospects);
                v["executions"] = json!(s.executions);
                v["tokens"] = json!(s.tokens);
                v["spend_usd"] = json!(s.tokens as f64 * crate::config::sell_usd_per_mtoken() / 1e6);
                v["last_execution_at"] = json!(s.last_execution_at);
                v["last_execution_status"] = json!(s.last_execution_status);
                v["active_execution_id"] = json!(s.active_execution_id);
                v
            })
            .collect(),
    ))
}

async fn get_plan(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let plan = store::get_plan(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("plan not found"))?;
    Ok(Json(public_plan(&plan)))
}

fn validate_plan(sc: &mut SourceConfig) -> Result<(), ApiError> {
    sc.source = sc.source.trim().to_string();
    if sc.source.is_empty() {
        return Err(bad_request("plan name is required"));
    }
    if sc.source.chars().count() > 120 {
        return Err(bad_request("plan name must be under 120 characters"));
    }
    sc.subject = sc.subject.trim().to_string();
    // Readiness is a condition for *running*, not for editing: a plan still
    // being drafted has no scrape prompt yet, and renaming it must not fail on
    // that. The runner enforces it where it matters (web::runner::start).
    if store::normalize_draft_status(&sc.draft_status) == "ready" {
        store::plan_ready(sc).map_err(bad_request)?;
    }
    if sc.seed_vars_json.trim().is_empty() {
        sc.seed_vars_json = "{}".into();
    }
    if serde_json::from_str::<serde_json::Map<String, Value>>(&sc.seed_vars_json).is_err() {
        return Err(bad_request("seed vars must be a JSON object"));
    }
    if sc.iterations < 1 {
        return Err(bad_request("iterations must be at least 1"));
    }
    if sc.max_no_progress < 1 {
        return Err(bad_request("max no-progress must be at least 1"));
    }
    if sc.known_limit < 0 || sc.target_prospects < 0 || sc.min_value < 0 {
        return Err(bad_request("counts must not be negative"));
    }
    sc.plan_type = store::normalize_plan_type(&sc.plan_type);
    sc.schedule_time = store::normalize_schedule_time(&sc.schedule_time).map_err(|e| bad_request(e.to_string()))?;
    sc.schedule_days = store::normalize_schedule_days(&sc.schedule_days).map_err(|e| bad_request(e.to_string()))?;
    if sc.schedule_time.is_empty() {
        sc.schedule_enabled = false;
    }
    sc.allow_hosts = crate::guard::split_hosts(&sc.allow_hosts).join(", ");
    Ok(())
}

/// Applies an effort level to the machine settings behind it. The pipeline
/// still reads `Iterations`, `MaxNoProgress` and `Learn`; nobody has to know
/// that but this function.
pub(crate) fn apply_effort_public(sc: &mut SourceConfig, effort: &str) {
    apply_effort(sc, effort)
}

fn apply_effort(sc: &mut SourceConfig, effort: &str) {
    sc.effort = store::normalize_effort(effort);
    let (iterations, max_no_progress, learn) = store::effort_settings(&sc.effort);
    sc.iterations = iterations;
    sc.max_no_progress = max_no_progress;
    sc.learn = learn;
}

pub(crate) fn apply_plan_models(sc: &mut SourceConfig, body: &PlanModelsBody) -> Result<(), String> {
    if let Some(v) = &body.scrape {
        sc.model_scrape = parse_model_choice(v)?;
    }
    if let Some(v) = &body.enrich {
        sc.model_enrich = parse_model_choice(v)?;
    }
    if let Some(v) = &body.planner {
        sc.model_planner = parse_model_choice(v)?;
    }
    Ok(())
}

fn parse_model_choice(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Ok(String::new());
    }
    crate::agent::normalize_model(t).ok_or_else(|| format!("{t:?} is not a valid model id"))
}

/// Models this install can run, with Cursor list prices plus our markup.
async fn list_models(AuthUser(_acc): AuthUser) -> Result<Json<Value>, ApiError> {
    let body = tokio::task::spawn_blocking(crate::model_catalog::catalog)
        .await
        .unwrap_or_else(|_| crate::model_catalog::catalog());
    Ok(Json(body))
}

/// What a person supplies to make a plan: what they want, and optionally what
/// to call it and how many results to aim for. The prompts, the field mapping
/// and the dedupe key are drafted here and stay here.
#[derive(Deserialize)]
pub(crate) struct NewPlanBody {
    /// The brief, in their own words. Kept verbatim as the description.
    pub(crate) brief: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) target: Option<i64>,
    /// How hard to try: quick | normal | thorough | exhaustive.
    #[serde(default)]
    pub(crate) effort: Option<String>,
    /// Columns for a database plan, named by the user with a note on what each
    /// should contain. Supplying any makes this an `artifacts` plan.
    #[serde(default)]
    pub(crate) columns: Vec<crate::artifact::ColumnRequest>,
    /// Websites to search first: URLs or bare domains, comma or newline
    /// separated. A preference, not a restriction.
    #[serde(default)]
    pub(crate) sites: String,
    /// What to come back with: `prospects` | `artifacts` | `report` | `assets`.
    /// Empty or `auto` lets the brief decide, which is the normal case.
    #[serde(default)]
    pub(crate) kind: Option<String>,
}

async fn create_plan(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<NewPlanBody>) -> Result<Json<Value>, ApiError> {
    let p = create_plan_from_brief(&state, acc.tenant(), body).await.map_err(|e| bad_request(format!("{e:#}")))?;
    Ok(Json(public_plan(&p)))
}

/// Creating a plan, once, for both the app and the public API: check the
/// brief, put the row in as `drafting` so it is visible immediately, and let
/// the agent write the search behind it.
pub(crate) async fn create_plan_from_brief(state: &App, workspace: i64, body: NewPlanBody) -> anyhow::Result<SourceConfig> {
    if crate::config::get("CURSOR_API_KEY").is_none() && std::env::var_os("HOME").is_none() {
        anyhow::bail!("the AI agent is not configured on this server");
    }
    let brief = body.brief.trim().to_string();
    if brief.is_empty() {
        anyhow::bail!("describe what you are looking for first");
    }
    if brief.chars().count() > 600 {
        anyhow::bail!("keep the description under 600 characters");
    }

    let mut sc = plan_chat::default_draft_for("business");
    sc.plan_id = 0;
    sc.description = brief.clone();
    let named_by_user = !body.name.trim().is_empty();
    // Columns make it a table plan from the first moment, so the row on the
    // plans page says "artifacts" while it drafts rather than changing kind
    // under the user when the agent finishes.
    let columns = body.columns.clone();
    sc.sites = crate::guard::split_sites(&body.sites).join(", ");
    // An explicit choice beats inference, and beats the columns: someone who
    // picked "a written report" and left a stale column lying around meant the
    // report.
    let asked = body.kind.as_deref().unwrap_or("").trim().to_ascii_lowercase();
    let kind = if asked.is_empty() || asked == "auto" {
        if columns.is_empty() { "auto".to_string() } else { "artifacts".to_string() }
    } else {
        store::PlanKind::parse(&asked).as_str().to_string()
    };
    // What this account is allowed to build. Checked here rather than in the
    // UI alone: the API is the same door, and an experimental kind that is off
    // has to be off for both.
    let allowed = store::allowed_kinds(&state.db, workspace).await;
    if kind != "auto" && !allowed.iter().any(|k| k == &kind) {
        anyhow::bail!("this workspace cannot create {kind} plans yet");
    }
    if kind != "auto" {
        sc.kind = kind.clone();
    }
    if kind == "artifacts" && !columns.is_empty() {
        sc.fields_schema_json =
            serde_json::to_string(&crate::artifact::columns_to_schema(&columns)).unwrap_or_default();
    }
    // Unnamed plans get a stand-in so the row can exist immediately; the agent
    // replaces it with a proper short name when it finishes drafting.
    sc.source = unique_name(state, workspace, &body.name, &brief).await?;
    sc.target_prospects = body.target.unwrap_or(10).clamp(0, 500) as i32;
    apply_effort(&mut sc, body.effort.as_deref().unwrap_or("normal"));
    sc.draft_status = "drafting".into();
    let id = store::save_plan(&state.db, workspace, &sc).await?;

    let req = plan_chat::PlanDraftRequest {
        icp: brief,
        plan_type: String::new(),
        kind,
        source: sc.source.clone(),
        source_key_tmpl: String::new(),
        seed_vars_json: String::new(),
        learn: sc.learn,
        iterations: sc.iterations as i64,
        target_prospects: sc.target_prospects as i64,
        columns,
        sites: sc.sites.clone(),
        allowed: allowed.clone(),
    };
    spawn_draft(state.clone(), workspace, id, req, !named_by_user);

    // After the row is committed and the draft is under way, so a listener that
    // reads the plan back finds it, and finds it in the state we just described.
    crate::bus::publish(
        crate::bus::subject::PLAN_CREATED,
        Some(workspace),
        json!({ "plan_id": id, "name": sc.source, "kind": sc.kind_of().as_str(), "queued_draft": draft_queued() }),
    )
    .await;

    store::get_plan(&state.db, workspace, id).await?.ok_or_else(|| anyhow::anyhow!("plan not found"))
}

/// A name for a plan the app chose itself — the agent's, or a fallback. Always
/// deduped rather than refused: nobody typed it, so a collision is not an error
/// to hand back, it is a suffix to add.
async fn unique_plan_name(db: &crate::store::Db, account_id: i64, wanted: &str, exclude: Option<i64>) -> anyhow::Result<String> {
    let base: String = wanted.trim().chars().take(120).collect();
    let base = if base.is_empty() { "New search".to_string() } else { base };
    let taken: Vec<String> = store::list_plans(db, account_id)
        .await?
        .into_iter()
        .filter(|p| Some(p.plan.plan_id) != exclude)
        .map(|p| p.plan.source.to_lowercase())
        .collect();
    if !taken.contains(&base.to_lowercase()) {
        return Ok(base);
    }
    for n in 2..100 {
        let candidate = format!("{base} {n}");
        if !taken.contains(&candidate.to_lowercase()) {
            return Ok(candidate);
        }
    }
    anyhow::bail!("could not find a free name")
}

/// A plan name nobody else on this account is using. A name the user typed is
/// taken as given; a derived one gets a numeric suffix rather than failing,
/// since they never chose it in the first place.
async fn unique_name(state: &App, account_id: i64, wanted: &str, brief: &str) -> anyhow::Result<String> {
    let wanted = wanted.trim();
    if !wanted.is_empty() {
        return Ok(wanted.chars().take(120).collect());
    }
    let base: String = brief.split_whitespace().take(8).collect::<Vec<_>>().join(" ").chars().take(100).collect();
    let base = if base.is_empty() { "New search".to_string() } else { base };
    let taken: Vec<String> = store::list_plans(&state.db, account_id)
        .await?
        .into_iter()
        .map(|p| p.plan.source.to_lowercase())
        .collect();
    if !taken.contains(&base.to_lowercase()) {
        return Ok(base);
    }
    for n in 2..100 {
        let candidate = format!("{base} {n}");
        if !taken.contains(&candidate.to_lowercase()) {
            return Ok(candidate);
        }
    }
    anyhow::bail!("you already have a plan by that name")
}

/// Runs the drafting agent for a plan that already exists, then writes the
/// prompts it authored onto that row. The user's own fields — name, brief,
/// target, effort, schedule — are re-read and kept, so an edit made while the
/// agent was thinking is not overwritten by it.
/// `adopt_name` is set when the plan has no name of its own: the drafting
/// agent writes a short one alongside the search, and a summary of the brief
/// beats the first eight words of it.
/// Start drafting — here, or on the planning service.
///
/// `DRAFT_DISPATCH=queue` parks the brief on the plan row and returns; a
/// planning service claims it. Anything else drafts in this process, which is
/// what `huntwell serve` and dev.sh do, where one process is the whole
/// product. Same shape as `RUN_DISPATCH` for runs, and the same reason: the
/// caller should not have to know which it is.
///
/// Either way the plan is left in a "being built" state and the response says
/// so, so the UI's polling is identical.
fn spawn_draft(state: App, account_id: i64, plan_id: i64, req: plan_chat::PlanDraftRequest, adopt_name: bool) {
    let db = state.db.clone();
    if draft_queued() {
        tokio::spawn(async move {
            if let Err(e) = store::queue_draft(&db, account_id, plan_id, &req, adopt_name).await {
                tracing::warn!(plan_id, "could not queue the draft: {e:#}");
                let _ = store::set_draft_status(&db, account_id, plan_id, "failed").await;
            }
        });
        return;
    }
    tokio::spawn(async move { run_draft(&db, account_id, plan_id, req, adopt_name).await });
}

/// Whether drafting is someone else's job.
pub fn draft_queued() -> bool {
    matches!(std::env::var("DRAFT_DISPATCH").as_deref(), Ok("queue"))
}

/// The drafting itself, over a `Db` rather than the web `App`, so the planning
/// service can run exactly this without pretending to be an HTTP server.
/// One place to say a draft did not happen, so the three ways it can fail all
/// look the same to a listener.
async fn draft_failed(account_id: i64, plan_id: i64, reason: &str) {
    crate::bus::publish(
        crate::bus::subject::PLAN_DRAFT_FAILED,
        Some(account_id),
        json!({ "plan_id": plan_id, "reason": reason }),
    )
    .await;
}

pub async fn run_draft(
    db: &crate::store::Db,
    account_id: i64,
    plan_id: i64,
    req: plan_chat::PlanDraftRequest,
    adopt_name: bool,
) {
    // Timed because this is the wait a user actually feels, and the two
    // levers on it (which model drafts, how much it has to write) can only
    // be judged against a number.
    let started = std::time::Instant::now();
    let drafted = tokio::task::spawn_blocking(move || plan_chat::draft_from_brief(&req)).await;
    tracing::info!(plan_id, ms = started.elapsed().as_millis() as u64, "plan drafted");
    let drafted = match drafted {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            tracing::warn!(plan_id, "drafting failed: {e:#}");
            let _ = store::set_draft_status(db, account_id, plan_id, "failed").await;
            draft_failed(account_id, plan_id, &format!("{e:#}")).await;
            return;
        }
        Err(e) => {
            tracing::warn!(plan_id, "drafting panicked: {e}");
            let _ = store::set_draft_status(db, account_id, plan_id, "failed").await;
            draft_failed(account_id, plan_id, &format!("panicked: {e}")).await;
            return;
        }
    };
    let Ok(Some(mut sc)) = store::get_plan(db, account_id, plan_id).await else { return };
    // A name the user typed is theirs; a stand-in is replaced by the short
    // one the agent wrote, deduped so a collision cannot fail the save.
    if adopt_name && !drafted.source.trim().is_empty() {
        if let Ok(name) = unique_plan_name(db, account_id, &drafted.source, Some(plan_id)).await {
            sc.source = name;
        }
    }
    // Only the machinery comes from the agent.
    sc.kind = drafted.kind.clone();
    sc.plan_type = drafted.plan_type.clone();
    sc.subject = drafted.subject.clone();
    sc.fields_schema_json = drafted.fields_schema_json.clone();
    sc.scrape_prompt = drafted.scrape_prompt.clone();
    sc.enrich_prompt = drafted.enrich_prompt.clone();
    sc.planner_prompt = drafted.planner_prompt.clone();
    sc.source_key_tmpl = drafted.source_key_tmpl.clone();
    sc.name_tmpl = drafted.name_tmpl.clone();
    sc.title_tmpl = drafted.title_tmpl.clone();
    sc.company_tmpl = drafted.company_tmpl.clone();
    sc.industry_tmpl = drafted.industry_tmpl.clone();
    sc.email_tmpl = drafted.email_tmpl.clone();
    sc.email_status_tmpl = drafted.email_status_tmpl.clone();
    sc.phone_tmpl = drafted.phone_tmpl.clone();
    sc.website_tmpl = drafted.website_tmpl.clone();
    sc.linkedin_tmpl = drafted.linkedin_tmpl.clone();
    sc.location_tmpl = drafted.location_tmpl.clone();
    sc.notes_tmpl = drafted.notes_tmpl.clone();
    sc.estimated_value_tmpl = drafted.estimated_value_tmpl.clone();
    sc.seed_vars_json = drafted.seed_vars_json.clone();
    sc.allow_hosts = drafted.allow_hosts.clone();
    // The effort level the user picked still decides how hard it tries.
    let effort = sc.effort.clone();
    apply_effort(&mut sc, &effort);
    sc.draft_status = if store::plan_ready(&sc).is_ok() { "ready".into() } else { "failed".into() };
    if let Err(e) = store::save_plan(db, account_id, &sc).await {
        tracing::warn!(plan_id, "could not save the drafted plan: {e:#}");
        let _ = store::set_draft_status(db, account_id, plan_id, "failed").await;
        draft_failed(account_id, plan_id, &format!("could not save: {e:#}")).await;
        return;
    }
    let _ = store::refresh_schedule(db, plan_id).await;
    tracing::info!(plan_id, "plan drafted");

// Whoever drafted it says so — the website when it drafts in process, the
// planning service when it does. A listener cannot tell, and should not need to.
crate::bus::publish(
    crate::bus::subject::PLAN_DRAFTED,
    Some(account_id),
    json!({ "plan_id": plan_id, "name": sc.source, "kind": sc.kind_of().as_str(), "ready": sc.draft_status == "ready" }),
)
.await;
}

/// Try drafting again for a plan whose first attempt failed.
async fn rebuild_plan(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    agent_available()?;
    let sc = store::get_plan(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("plan not found"))?;
    // 'queued' counts: the plan is waiting for the planning service to pick it
    // up, and queueing it again would have two services draft the same plan.
    if store::draft_in_progress(&sc.draft_status) {
        return Err(bad_request("this plan is already being built"));
    }
    store::set_draft_status(&state.db, acc.tenant(), id, "drafting").await?;
    let req = plan_chat::PlanDraftRequest {
        icp: sc.description.clone(),
        plan_type: String::new(),
        // A rebuild is a redraft of this plan, not a re-decision about what it is.
        kind: sc.kind_of().as_str().to_string(),
        source: sc.source.clone(),
        source_key_tmpl: String::new(),
        seed_vars_json: String::new(),
        learn: sc.learn,
        iterations: sc.iterations as i64,
        target_prospects: sc.target_prospects as i64,
        // A rebuild keeps the columns this plan already has: they are the
        // shape of the rows already stored against it.
        columns: crate::artifact::parse_schema(&sc.fields_schema_json)
            .into_iter()
            .map(|f| crate::artifact::ColumnRequest {
                name: if f.label.trim().is_empty() { f.key.clone() } else { f.label.clone() },
                prompt: String::new(),
            })
            .collect(),
        // A rebuild keeps the sites the plan was pointed at.
        sites: sc.sites.clone(),
        // A plan that exists keeps working even if its kind was since turned
        // off: withdrawing a feature must not break what people already have.
        allowed: vec![sc.kind_of().as_str().to_string()],
    };
    spawn_draft(state.clone(), acc.tenant(), id, req, false);
    let plan = store::get_plan(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("plan not found"))?;
    Ok(Json(public_plan(&plan)))
}

/// The fields a person owns. Anything absent keeps the value the plan already
/// has, so a client that cannot see the machinery cannot blank it either.
#[derive(Deserialize)]
struct PlanUpdateBody {
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
    alert_email: Option<bool>,
    #[serde(default)]
    models: Option<PlanModelsBody>,
}

/// Per-stage model ids. Absent keeps the value the plan already has; "" is
/// the explicit "let Cursor / the install default pick" choice.
#[derive(Default, Deserialize)]
pub(crate) struct PlanModelsBody {
    #[serde(default)]
    pub scrape: Option<String>,
    #[serde(default)]
    pub enrich: Option<String>,
    #[serde(default)]
    pub planner: Option<String>,
}

async fn update_plan(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(body): Json<PlanUpdateBody>) -> Result<Json<Value>, ApiError> {
    let mut sc = store::get_plan(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("plan not found"))?;
    if let Some(n) = body.name {
        sc.source = n;
    }
    if let Some(d) = body.description {
        if d.chars().count() > 600 {
            return Err(bad_request("keep the description under 600 characters"));
        }
        sc.description = d;
    }
    if let Some(t) = body.target {
        sc.target_prospects = t.clamp(0, 500) as i32;
    }
    if let Some(e) = body.effort {
        apply_effort(&mut sc, &e);
    }
    if let Some(on) = body.alert_email {
        sc.alert_email = on;
    }
    if let Some(on) = body.schedule_enabled {
        sc.schedule_enabled = on;
    }
    if let Some(t) = body.schedule_time {
        sc.schedule_time = t;
    }
    if let Some(d) = body.schedule_days {
        sc.schedule_days = d;
    }
    if let Some(models) = &body.models {
        apply_plan_models(&mut sc, models).map_err(bad_request)?;
    }
    validate_plan(&mut sc)?;
    store::save_plan(&state.db, acc.tenant(), &sc).await?;
    store::refresh_schedule(&state.db, id).await?;
    let plan = store::get_plan(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("plan not found"))?;
    Ok(Json(public_plan(&plan)))
}

async fn delete_plan(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    if let Some(active) = store::active_execution_for_plan(&state.db, id).await? {
        let _ = runner::cancel(&state, acc.tenant(), active.execution_id).await;
    }
    if !store::delete_plan(&state.db, acc.tenant(), id).await? {
        return Err(not_found("plan not found"));
    }
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct FavBody {
    favorite: bool,
}

async fn favorite_plan(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(b): Json<FavBody>) -> Result<Json<Value>, ApiError> {
    store::set_favorite(&state.db, acc.tenant(), id, b.favorite).await?;
    Ok(Json(json!({"ok": true})))
}

async fn plan_trail(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let plan = store::get_plan(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("plan not found"))?;
    let queries = store::list_search_queries(&state.db, id, 60).await?;
    let pages = store::list_visited_pages(&state.db, id, 100).await?;
    let frontier = store::list_frontier(&state.db, id, 100).await?;
    let rotation = crate::pipeline::plan_search_rotation(&state.db, &plan, 0).await;
    Ok(Json(json!({
        "queries": queries,
        "pages": pages,
        "frontier": frontier,
        "rotation": { "summary": rotation.summary(), "block": rotation.render() },
    })))
}

/// A plan's knowledge graph: what it searched, what those searches opened, what
/// came out, and what each angle cost.
///
/// The thinking, assembled here rather than in the browser:
///   seed  → query    this angle asked for that search
///   seed  → page     a page opened with no search on screen
///   query → page     observed: the page was opened after that search
///   query → result   attributed: the angles in play when the row was stored
///   page  → result   derived: the row's key is that page's host
async fn plan_graph(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    Ok(Json(graph_payload(&state, acc.tenant(), id).await.map_err(|e| not_found(format!("{e:#}")))?))
}

/// Assembled once, for the app's graph tab and the public API alike.
pub(crate) async fn graph_payload(state: &App, workspace: i64, id: i64) -> anyhow::Result<Value> {
    let state = state.clone();
    let acc_tenant = workspace;
    store::get_plan(&state.db, acc_tenant, id).await?.ok_or_else(|| anyhow::anyhow!("plan not found"))?;
    let queries = store::list_search_queries(&state.db, id, 200).await?;
    let pages = store::list_visited_pages(&state.db, id, 300).await?;
    let results = store::list_result_nodes(&state.db, id, 300).await?;
    let seeds = store::list_frontier(&state.db, id, 80).await?;
    let mut edges = store::list_edges(&state.db, id, 3000).await?;
    // Older runs did not write seed → query. Hang those searches off the
    // earliest explored seed so the picture still reads as a thought, not two
    // unrelated lists.
    if !seeds.is_empty() && !edges.iter().any(|e| e.from_kind == "seed") {
        if let Some(root) = seeds.iter().rev().find(|s| s.status != "pending" && !s.seed_key.is_empty()) {
            for (i, q) in queries.iter().enumerate() {
                edges.push(store::GraphEdge {
                    from_kind: "seed".into(),
                    from: root.seed_key.clone(),
                    to_kind: "query".into(),
                    to: q.query_key.clone(),
                    weight: 1,
                    seq: i as i64,
                });
            }
        }
    }

    // A result keyed on a domain and a page on that domain are the same thing
    // seen from two sides. Derived here rather than stored: it is a fact about
    // the two rows, and it stays true if either changes.
    let hosts: Vec<(String, String)> = pages
        .iter()
        .map(|p| {
            let id = if p.url_key.is_empty() { p.url.clone() } else { p.url_key.clone() };
            (crate::normalize::cleanse_key(&p.host), id)
        })
        .collect();
    for r in &results {
        let key = crate::normalize::cleanse_key(&r.key);
        if key.is_empty() {
            continue;
        }
        for (host, page_id) in &hosts {
            if host == &key || host.ends_with(&format!(".{key}")) || key.ends_with(&format!(".{host}")) {
                edges.push(store::GraphEdge {
                    from_kind: "page".into(),
                    from: page_id.clone(),
                    to_kind: "result".into(),
                    to: r.key.clone(),
                    weight: 1,
                    // Derived, not walked: it belongs at the end of the path.
                    seq: i64::MAX,
                });
            }
        }
    }

    // Trail edges name a page by url_key (scheme-less, no www) and a search
    // by query_key. Nodes are those same rows under whatever id the list
    // used. Rewrite every endpoint onto a node id so the canvas is not left
    // with two lists and no lines.
    let mut alias: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let put = |m: &mut std::collections::HashMap<String, String>, key: &str, id: &str| {
        if !key.is_empty() {
            m.insert(key.to_string(), id.to_string());
        }
    };
    for q in &queries {
        put(&mut alias, &q.query_key, &q.query_key);
        put(&mut alias, &q.query, &q.query_key);
    }
    for p in &pages {
        let nid = if p.url_key.is_empty() { p.url.clone() } else { p.url_key.clone() };
        put(&mut alias, &p.url_key, &nid);
        put(&mut alias, &p.url, &nid);
        put(&mut alias, &crate::trail::normalize_url(&p.url), &nid);
    }
    for s in &seeds {
        put(&mut alias, &s.seed_key, &s.seed_key);
    }
    for r in &results {
        put(&mut alias, &r.key, &r.key);
    }
    for e in &mut edges {
        if let Some(id) = alias.get(&e.from).cloned() {
            e.from = id;
        }
        if let Some(id) = alias.get(&e.to).cloned() {
            e.to = id;
        }
    }

    let runs = store::list_executions(&state.db, acc_tenant, Some(id), 50).await?;
    let tokens: i64 = queries.iter().map(|q| q.tokens).sum();
    // Whether the graph is still being drawn — the UI follows this to keep
    // itself up to date while a run walks.
    let live = store::active_execution_for_plan(&state.db, id).await?.is_some();
    Ok(json!({
        "live": live,
        "seeds": seeds,
        "queries": queries,
        "pages": pages,
        "results": results,
        "edges": edges,
        "totals": {
            "seeds": seeds.len(),
            "queries": queries.len(),
            "pages": pages.len(),
            "results": results.len(),
            "tokens": tokens,
            "executions": runs.len(),
        },
    }))
}

async fn clear_queue(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    store::get_plan(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("plan not found"))?;
    let n = store::clear_pending_seeds(&state.db, id).await?;
    Ok(Json(json!({"dropped": n})))
}

// ---- AI plan authoring ------------------------------------------------------

fn agent_available() -> Result<(), ApiError> {
    if crate::config::get("CURSOR_API_KEY").is_none() && std::env::var_os("HOME").is_none() {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "the AI agent is not configured on this server".into()));
    }
    Ok(())
}

// ---- runs -------------------------------------------------------------------

#[derive(Deserialize)]
struct RunsQuery {
    #[serde(default)]
    plan_id: Option<i64>,
    #[serde(default = "fifty")]
    limit: i64,
}
fn fifty() -> i64 {
    50
}

/// A run, plus what it cost and what that bought. The meter is on the row
/// already; without this it never left the database.
pub(crate) fn run_json(r: &store::ExecutionRecord) -> Value {
    let mut v = serde_json::to_value(r).unwrap_or_else(|_| json!({}));
    v["tokens"] = json!(r.input_tokens + r.output_tokens);
    v["cost_usd"] = json!(r.cost_usd());
    // Null when the run found nothing — the case worth seeing, kept distinct
    // from "cheap per result" rather than dividing by zero.
    v["cost_per_result"] = json!(r.cost_per_result());
    // Whether Watch the browser will work — a remote session, never the id.
    v["watchable"] = json!(!r.browser_session_id.trim().is_empty());
    v
}

async fn list_executions(State(state): State<App>, AuthUser(acc): AuthUser, Query(q): Query<RunsQuery>) -> Result<Json<Vec<Value>>, ApiError> {
    let runs = store::list_executions(&state.db, acc.tenant(), q.plan_id, q.limit).await?;
    Ok(Json(runs.iter().map(run_json).collect()))
}

#[derive(Deserialize)]
struct StartRunBody {
    plan_id: i64,
    #[serde(flatten)]
    args: RunArgs,
}

async fn start_execution(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<StartRunBody>) -> Result<Json<Value>, ApiError> {
    let execution_id = runner::start(&state, acc.tenant(), body.plan_id, "manual", body.args)
        .await
        .map_err(|e| bad_request(format!("{e:#}")))?;
    Ok(Json(json!({"execution_id": execution_id})))
}

async fn get_execution(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let r = store::get_execution(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("run not found"))?;
    Ok(Json(run_json(&r)))
}

async fn cancel_execution(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let run = store::get_execution(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("run not found"))?;
    let stopped = runner::cancel(&state, acc.tenant(), id).await?;
    if !stopped && matches!(run.status.as_str(), "queued" | "running") {
        // Not in this server's table (a previous server started it) — the
        // row is all we can fix.
        store::finish_execution(&state.db, id, "cancelled", Some(130)).await?;
    }
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default)]
    after: i64,
}

async fn execution_log_json(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Query(q): Query<LogQuery>) -> Result<Json<Value>, ApiError> {
    let run = store::get_execution(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("run not found"))?;
    let lines = store::list_execution_logs(&state.db, id, q.after, 5000).await?;
    Ok(Json(json!({"run": run, "lines": lines})))
}

/// What the live page needs to refresh the meter without a second poll.
/// Tokens and the found-count change independently of log lines, so the
/// stream carries them whenever the row moves.
fn meter_key(r: &store::ExecutionRecord) -> (i64, i32, String, bool) {
    (
        r.input_tokens + r.output_tokens,
        r.new_prospects,
        r.status.clone(),
        !r.browser_session_id.trim().is_empty(),
    )
}

async fn send_meter(
    tx: &tokio::sync::mpsc::Sender<Event>,
    last: &mut Option<(i64, i32, String, bool)>,
    r: &store::ExecutionRecord,
) -> bool {
    let key = meter_key(r);
    if last.as_ref() == Some(&key) {
        return true;
    }
    *last = Some(key);
    let ev = Event::default().event("meter").json_data(&run_json(r)).unwrap_or_else(|_| Event::default());
    tx.send(ev).await.is_ok()
}

/// Live log: replays everything after `after`, then follows until the run
/// finishes. Wakes on the broadcast from the runner and also polls, so a
/// line written by a runner in another process still arrives. Token cost
/// rides the same stream (`meter`) so the page ticks as the row is billed.
async fn execution_log_sse(
    State(state): State<App>,
    AuthUser(acc): AuthUser,
    Path(id): Path<i64>,
    Query(q): Query<LogQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    store::get_execution(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("run not found"))?;
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(256);
    let mut wake = state.log_tx.subscribe();
    let mut after = q.after;
    tokio::spawn(async move {
        let mut last_meter = None;
        loop {
            let lines = store::list_execution_logs(&state.db, id, after, 500).await.unwrap_or_default();
            let had_lines = !lines.is_empty();
            if had_lines {
                after = lines.last().map(|l| l.seq).unwrap_or(after);
                for l in lines {
                    let ev = Event::default().event("line").json_data(&l).unwrap_or_else(|_| Event::default());
                    if tx.send(ev).await.is_err() {
                        return;
                    }
                }
            }
            let run = store::get_execution_unscoped(&state.db, id).await.ok().flatten();
            if let Some(r) = &run {
                if !send_meter(&tx, &mut last_meter, r).await {
                    return;
                }
            }
            if had_lines {
                continue;
            }
            let finished = run.as_ref().map(|r| !matches!(r.status.as_str(), "queued" | "running")).unwrap_or(true);
            if finished {
                // One last sweep so a line written between the two reads is not lost.
                for l in store::list_execution_logs(&state.db, id, after, 500).await.unwrap_or_default() {
                    let ev = Event::default().event("line").json_data(&l).unwrap_or_else(|_| Event::default());
                    let _ = tx.send(ev).await;
                }
                if let Some(r) = store::get_execution_unscoped(&state.db, id).await.ok().flatten() {
                    let _ = send_meter(&tx, &mut last_meter, &r).await;
                }
                let _ = tx.send(Event::default().event("done").data("done")).await;
                return;
            }
            if tx.is_closed() {
                return;
            }
            let _ = tokio::time::timeout(Duration::from_millis(700), wake.recv()).await;
        }
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok);
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping")))
}

// ---- prospects --------------------------------------------------------------

#[derive(Deserialize)]
struct ProspectsQuery {
    #[serde(default)]
    plan_id: Option<i64>,
    #[serde(default)]
    min_value: i64,
    #[serde(default)]
    search: String,
    #[serde(default = "hundred")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}
fn hundred() -> i64 {
    100
}

async fn list_prospects(State(state): State<App>, AuthUser(acc): AuthUser, Query(q): Query<ProspectsQuery>) -> Result<Json<Value>, ApiError> {
    let f = store::ProspectFilter { plan_id: q.plan_id, min_value: q.min_value, search: q.search, limit: q.limit, offset: q.offset };
    let (rows, total) = store::list_prospects(&state.db, acc.tenant(), &f).await?;
    Ok(Json(json!({"rows": rows, "total": total})))
}

async fn delete_prospects(State(state): State<App>, AuthUser(acc): AuthUser, Query(q): Query<ProspectsQuery>) -> Result<Json<Value>, ApiError> {
    let n = store::delete_prospects(&state.db, acc.tenant(), q.plan_id).await?;
    Ok(Json(json!({"deleted": n})))
}

async fn delete_prospect(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    if !store::delete_prospect(&state.db, acc.tenant(), id).await? {
        return Err(not_found("prospect not found"));
    }
    Ok(Json(json!({"ok": true})))
}

async fn prospects_csv(State(state): State<App>, AuthUser(acc): AuthUser, Query(q): Query<ProspectsQuery>) -> Result<Response, ApiError> {
    let rows = store::export_prospects(&state.db, acc.tenant(), q.plan_id, q.min_value).await?;
    let text = csv::prospects_csv(&rows);
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8")),
            (header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment; filename=\"prospects.csv\"")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        csv::with_bom(&text),
    )
        .into_response())
}

/// Unified results across every plan — prospects and custom artifacts together.
async fn results(State(state): State<App>, AuthUser(acc): AuthUser, Query(q): Query<ProspectsQuery>) -> Result<Json<Value>, ApiError> {
    let f = store::ResultsFilter { plan_id: q.plan_id, search: q.search, limit: q.limit, offset: q.offset };
    let (rows, total) = store::list_results(&state.db, acc.tenant(), &f).await?;
    Ok(Json(json!({ "rows": rows, "total": total })))
}

// ---- authenticated logins (Browserbase Context) -----------------------------

#[derive(Deserialize)]
struct BrowserLoginBody {
    #[serde(default)]
    site: String,
    #[serde(default)]
    url: String,
}

/// Starts an interactive Browserbase login session on the account's Context
/// (created on first use). The user drives the returned Live View URL to sign in.
/// A link to watch the browser this execution is driving.
///
/// The URL is a bearer capability — whoever holds it can watch and drive that
/// browser, which is carrying this workspace's logged-in sessions. So it is
/// never stored, never logged, and never put in a page that a third party
/// could frame: it is fetched from Browserbase per request, after the same
/// tenancy check as every other execution route, and handed to the caller to
/// open themselves.
async fn execution_browser(
    State(state): State<App>,
    AuthUser(acc): AuthUser,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let run = store::get_execution(&state.db, acc.tenant(), id)
        .await?
        .ok_or_else(|| not_found("execution not found"))?;
    if !matches!(run.status.as_str(), "queued" | "running") {
        return Err(bad_request("that execution has finished — its browser is gone"));
    }
    if run.browser_session_id.trim().is_empty() {
        return Err(bad_request("this execution is running a local browser, which cannot be watched remotely"));
    }
    let url = crate::browserbase::view_url(run.browser_session_id.trim())
        .await
        .ok_or_else(|| bad_request("that browser is no longer available"))?;
    Ok(Json(json!({ "url": url })))
}

/// Refuses unless this workspace may connect an authenticated session.
///
/// Every route in the feature calls it, including the read-only one: a
/// workspace that cannot use connections should not be told what it has, and a
/// list that answers while the actions do not is a UI that offers a button
/// which cannot work.
///
/// The wording says "not enabled", not "you may not" — this is a switch an
/// operator turns on, not a refusal about who is asking.
async fn connected_logins_allowed(state: &App, account_id: i64) -> Result<(), ApiError> {
    if store::connected_logins_enabled(&state.db, account_id).await {
        return Ok(());
    }
    Err(ApiError(
        StatusCode::FORBIDDEN,
        "connected logins aren't enabled for this workspace".into(),
    ))
}

async fn browser_login(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<BrowserLoginBody>) -> Result<Json<Value>, ApiError> {
    connected_logins_allowed(&state, acc.tenant()).await?;
    if !crate::browserbase::configured() {
        // User-facing: the vendor behind a connected session is our business,
        // not something to name at a customer.
        return Err(bad_request("connected logins aren't set up on this server yet"));
    }
    // Connecting a session to one of the restricted platforms is the act the
    // acknowledgement is about, so it is checked here rather than only in the
    // UI — the API is the same door.
    let target = format!("{} {}", body.site, body.url);
    if crate::guard::split_sites(&target).iter().any(|h| crate::guard::is_restricted_host(h))
        || crate::guard::is_restricted_host(body.site.trim())
    {
        let ws = store::get_account(&state.db, acc.tenant()).await?;
        if !ws.is_some_and(|a| a.platform_ack_at.is_some()) {
            return Err(bad_request(
                "that site is off limits until someone in this workspace confirms, in Settings, that you have the right to collect from it",
            ));
        }
    }
    let ctx = match store::account_context(&state.db, acc.tenant()).await? {
        Some(c) => c,
        None => {
            let c = crate::browserbase::create_context().await.map_err(|e| bad_request(format!("{e:#}")))?;
            store::set_account_context(&state.db, acc.tenant(), &c).await?;
            c
        }
    };
    let ls = crate::browserbase::start_login_session(&ctx, 600).await.map_err(|e| bad_request(format!("{e:#}")))?;
    let site = if body.site.trim().is_empty() { "site".to_string() } else { body.site.trim().to_string() };
    Ok(Json(json!({ "session_id": ls.session_id, "live_view_url": ls.live_view_url, "site": site, "url": body.url })))
}

#[derive(Deserialize)]
struct BrowserFinishBody {
    session_id: String,
    #[serde(default)]
    site: String,
    #[serde(default)]
    url: String,
}

/// Releases the login session (which saves the Context) and records the login.
async fn browser_login_finish(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<BrowserFinishBody>) -> Result<Json<Value>, ApiError> {
    connected_logins_allowed(&state, acc.tenant()).await?;
    crate::browserbase::release(&body.session_id).await;
    let site = if body.site.trim().is_empty() { "site" } else { body.site.trim() };
    store::record_browser_connection(&state.db, acc.tenant(), site, body.url.trim()).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Not gated with a 403 like the other two: this is what the settings page
/// calls to decide whether to offer the feature at all, and an error would make
/// the page fail rather than quietly not offer it. It reports the two reasons
/// separately, because "we have not switched this on yet" and "your workspace
/// does not have it" lead a person to do different things.
///
/// Connections are withheld when the workspace is not enabled, so a list of
/// sites someone once connected is not readable through a feature they no
/// longer have.
async fn browser_connections(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    let enabled = store::connected_logins_enabled(&state.db, acc.tenant()).await;
    let conns = if enabled { store::list_browser_connections(&state.db, acc.tenant()).await? } else { Vec::new() };
    Ok(Json(json!({
        "connections": conns,
        "available": crate::browserbase::configured(),
        "enabled": enabled,
    })))
}

// ---- artifacts (custom-schema rows) -----------------------------------------

async fn list_artifacts(State(state): State<App>, AuthUser(acc): AuthUser, Query(q): Query<ProspectsQuery>) -> Result<Json<Value>, ApiError> {
    let f = store::ArtifactFilter { plan_id: q.plan_id, search: q.search, limit: q.limit, offset: q.offset };
    let (rows, total) = store::list_artifacts(&state.db, acc.tenant(), &f).await?;
    // The columns travel with the rows they describe. A custom-schema table
    // cannot be rendered without them, and they are the one part of a plan's
    // machinery that is really just the shape of the user's own data — but they
    // belong to the results, not to the plan the UI shows.
    let columns = match f.plan_id {
        Some(id) => match store::get_plan(&state.db, acc.tenant(), id).await? {
            Some(p) => serde_json::to_value(crate::artifact::parse_schema(&p.fields_schema_json)).unwrap_or(Value::Null),
            None => Value::Null,
        },
        None => Value::Null,
    };
    Ok(Json(json!({"rows": rows, "total": total, "columns": columns})))
}

async fn delete_artifacts(State(state): State<App>, AuthUser(acc): AuthUser, Query(q): Query<ProspectsQuery>) -> Result<Json<Value>, ApiError> {
    let n = store::delete_artifacts(&state.db, acc.tenant(), q.plan_id).await?;
    Ok(Json(json!({"deleted": n})))
}

async fn delete_artifact(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    if !store::delete_artifact(&state.db, acc.tenant(), id).await? {
        return Err(not_found("artifact not found"));
    }
    Ok(Json(json!({"ok": true})))
}

async fn artifacts_csv(State(state): State<App>, AuthUser(acc): AuthUser, Query(q): Query<ProspectsQuery>) -> Result<Response, ApiError> {
    let plan_id = q.plan_id.ok_or_else(|| bad_request("plan_id is required to export artifacts"))?;
    let plan = store::get_plan(&state.db, acc.tenant(), plan_id).await?.ok_or_else(|| not_found("plan not found"))?;
    let schema = crate::artifact::parse_schema(&plan.fields_schema_json);
    let rows = store::export_artifacts(&state.db, acc.tenant(), Some(plan_id)).await?;
    let text = csv::artifacts_csv(&rows, &schema);
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8")),
            (header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment; filename=\"artifacts.csv\"")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        csv::with_bom(&text),
    )
        .into_response())
}

// ---- reports (one document per subject) --------------------------------------

async fn list_reports(State(state): State<App>, AuthUser(acc): AuthUser, Query(q): Query<ProspectsQuery>) -> Result<Json<Value>, ApiError> {
    let rows = store::list_reports(&state.db, acc.tenant(), q.plan_id).await?;
    Ok(Json(json!({ "rows": rows })))
}

async fn get_report(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let r = store::get_report(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("report not found"))?;
    let html = crate::report::to_html(&r.markdown);
    Ok(Json(json!({ "report": r, "html": html })))
}

/// The print view: a standalone, self-contained page whose print stylesheet is
/// the PDF path (the browser's own Save-as-PDF).
async fn report_html(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Response, ApiError> {
    let r = store::get_report(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("report not found"))?;
    let page = crate::report::print_page(&r);
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        page,
    )
        .into_response())
}

async fn delete_report(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    if !store::delete_report(&state.db, acc.tenant(), id).await? {
        return Err(not_found("report not found"));
    }
    Ok(Json(json!({"ok": true})))
}

// ---- assets (collected files) -------------------------------------------------

async fn list_assets(State(state): State<App>, AuthUser(acc): AuthUser, Query(q): Query<ProspectsQuery>) -> Result<Json<Value>, ApiError> {
    let rows = store::list_assets(&state.db, acc.tenant(), q.plan_id).await?;
    Ok(Json(json!({ "rows": rows })))
}

/// Streams the bytes back from the object store. The filename is
/// attacker-influenced (it came off the web), so it is sanitized before it
/// reaches a header.
async fn download_asset(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Response, ApiError> {
    let a = store::get_asset(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("file not found"))?;
    let bytes = crate::objstore::get(&a.object_key)
        .await
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("could not read the stored file: {e:#}")))?;
    let name = crate::assets::sanitize_filename(&a.filename);
    let name = if name.is_empty() { format!("file-{id}") } else { name };
    let ctype = HeaderValue::from_str(&a.content_type).unwrap_or(HeaderValue::from_static("application/octet-stream"));
    let disp = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
        .unwrap_or(HeaderValue::from_static("attachment"));
    Ok((
        [
            (header::CONTENT_TYPE, ctype),
            (header::CONTENT_DISPOSITION, disp),
            // Content-addressed: the bytes for an id never change.
            (header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400")),
        ],
        bytes,
    )
        .into_response())
}

async fn delete_asset(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let Some(key) = store::delete_asset(&state.db, acc.tenant(), id).await? else {
        return Err(not_found("file not found"));
    };
    // Best effort: a stranded object costs storage, a failed delete costs the
    // user their request.
    if let Err(e) = crate::objstore::delete(&key).await {
        tracing::warn!("could not delete object {key}: {e:#}");
    }
    Ok(Json(json!({"ok": true})))
}

// ---- API keys ---------------------------------------------------------------

async fn list_keys(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Vec<store::ApiKeyRow>>, ApiError> {
    Ok(Json(store::list_api_keys(&state.db, acc.tenant()).await?))
}

#[derive(Deserialize)]
struct CreateKeyBody {
    #[serde(default)]
    label: String,
    #[serde(default)]
    plan_id: Option<i64>,
    #[serde(default)]
    expires_in_days: i64,
    #[serde(default)]
    allow_cidr: String,
}

async fn create_key(State(state): State<App>, AuthUser(acc): AuthUser, Json(b): Json<CreateKeyBody>) -> Result<Json<store::ApiKeyRow>, ApiError> {
    let key = store::create_api_key(&state.db, acc.tenant(), &b.label, b.plan_id, b.expires_in_days, &b.allow_cidr).await?;
    Ok(Json(key))
}

async fn delete_key(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    if !store::delete_api_key(&state.db, acc.tenant(), id).await? {
        return Err(not_found("key not found"));
    }
    Ok(Json(json!({"ok": true})))
}

async fn revoke_key(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    if !store::revoke_api_key(&state.db, acc.tenant(), id).await? {
        return Err(not_found("key not found"));
    }
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct AllowBody {
    allow_cidr: String,
}

async fn allow_key(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(b): Json<AllowBody>) -> Result<Json<Value>, ApiError> {
    if !store::set_api_key_cidr(&state.db, acc.tenant(), id, &b.allow_cidr).await? {
        return Err(not_found("key not found"));
    }
    Ok(Json(json!({"ok": true})))
}

async fn key_audit(State(state): State<App>, AuthUser(acc): AuthUser, Query(q): Query<HashMap<String, String>>) -> Result<Json<Vec<store::ApiAuditRow>>, ApiError> {
    let limit = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(200);
    Ok(Json(store::list_api_audit(&state.db, acc.tenant(), limit).await?))
}

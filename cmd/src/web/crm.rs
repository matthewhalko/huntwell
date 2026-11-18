//! CRM connections and the loop that pushes results into them (2026-10-13).
//!
//! The pushing itself is `crate::crm`; this is the workspace's side of it —
//! connecting a CRM, choosing which plans send to it — and the sync loop. The
//! loop runs in the website, the only process that can open a connection's
//! sealed credential (worker VMs cannot), and is driven by the ledger: every
//! pass pushes the rows a plan has that `crm_push` does not, so a run on a
//! worker needs to know nothing about CRMs, and a missed pass loses nothing.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::require_cap;
use super::auth::AuthUser;
use super::{bad_request, not_found, ApiError, App};
use crate::crm::{self, Contact, Provider, PushError};
use crate::store;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/integrations", get(list).post(connect))
        .route("/integrations/{id}", put(update).delete(disconnect))
        .route("/plans/{id}/crm", get(plan_links).put(set_plan_link))
        .route("/plans/{id}/crm/sync", post(sync_now))
}

fn settings_of(c: &store::CrmConnection) -> Value {
    serde_json::from_str(&c.settings_json).unwrap_or_else(|_| json!({}))
}

fn public(c: &store::CrmConnection) -> Value {
    let s = settings_of(c);
    let provider = Provider::parse(&c.provider);
    json!({
        "connection_id": c.connection_id,
        "provider": c.provider,
        "provider_name": provider.map(|p| p.name()).unwrap_or("CRM"),
        "label": c.label,
        "paused": c.paused,
        "last_error": c.last_error,
        "last_ok_at": c.last_ok_at.map(|t| t.to_rfc3339()),
        "create_lead": s.get("create_lead").and_then(Value::as_bool).unwrap_or(false),
    })
}

async fn list(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    let conns = store::list_crm_connections(&state.db, acc.tenant()).await?;
    Ok(Json(json!({
        "connections": conns.iter().map(public).collect::<Vec<_>>(),
        "providers": [{ "id": "pipedrive", "name": "Pipedrive", "credential": "API token", "where": "Pipedrive → Settings → Personal preferences → API" }],
    })))
}

#[derive(Deserialize)]
struct ConnectBody {
    provider: String,
    api_token: String,
    #[serde(default)]
    create_lead: bool,
}

/// Check a credential with the CRM itself, then keep it sealed. Connecting the
/// same account again replaces the old credential instead of adding a twin.
async fn connect(State(state): State<App>, AuthUser(acc): AuthUser, Json(b): Json<ConnectBody>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_KEYS).await?;
    let provider = Provider::parse(&b.provider).ok_or_else(|| bad_request("Huntwell can't connect to that CRM yet"))?;
    let (label, settings) = match provider {
        Provider::Pipedrive => {
            let a = crm::pipedrive::connect(&b.api_token).await.map_err(|e| bad_request(e.message()))?;
            let name = if a.company_name.trim().is_empty() { a.company_domain.clone() } else { a.company_name.clone() };
            (
                format!("{name} ({}.pipedrive.com)", a.company_domain),
                json!({ "company_domain": a.company_domain, "company_name": a.company_name, "user_name": a.user_name, "create_lead": b.create_lead }),
            )
        }
    };
    let sealed = crate::web::signing::seal_secret(b.api_token.trim()).ok_or_else(|| {
        tracing::error!("cannot seal a CRM credential: no HUNTWELL_API_SIGNING_KEY or HUNTWELL_SESSION_SECRET");
        bad_request("CRM connections can't be stored on this server right now")
    })?;
    let existing = store::list_crm_connections(&state.db, acc.tenant())
        .await?
        .into_iter()
        .find(|c| c.provider == provider.as_str() && settings_of(c).get("company_domain") == settings.get("company_domain"));
    let id = match existing {
        Some(c) => {
            store::reconnect_crm(&state.db, acc.tenant(), c.connection_id, &label, &sealed, &settings.to_string()).await?;
            c.connection_id
        }
        None => store::create_crm_connection(&state.db, acc.tenant(), provider.as_str(), &label, &sealed, &settings.to_string(), acc.account_id).await?,
    };
    tracing::info!(account_id = acc.tenant(), by = acc.account_id, provider = provider.as_str(), "CRM connected");
    let c = store::get_crm_connection(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("connection not found"))?;
    WAKE.notify_one();
    Ok(Json(json!({ "connection": public(&c) })))
}

#[derive(Deserialize)]
struct UpdateBody {
    #[serde(default)]
    create_lead: Option<bool>,
}

async fn update(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(b): Json<UpdateBody>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_KEYS).await?;
    let c = store::get_crm_connection(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("connection not found"))?;
    let mut s = settings_of(&c);
    if let Some(on) = b.create_lead {
        s["create_lead"] = json!(on);
    }
    store::set_crm_settings(&state.db, acc.tenant(), id, &s.to_string()).await?;
    let c = store::get_crm_connection(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("connection not found"))?;
    Ok(Json(json!({ "connection": public(&c) })))
}

async fn disconnect(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_KEYS).await?;
    if !store::delete_crm_connection(&state.db, acc.tenant(), id).await? {
        return Err(not_found("connection not found"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn plan_links(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let plan = store::get_plan(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("plan not found"))?;
    let conns = store::list_crm_connections(&state.db, acc.tenant()).await?;
    let links = store::get_plan_crm(&state.db, acc.tenant(), id).await?;
    let kind = plan.kind_of();
    Ok(Json(json!({
        // Reports and files are not contacts.
        "supported": matches!(kind, store::PlanKind::Prospects | store::PlanKind::Artifacts),
        "connections": conns.iter().map(public).collect::<Vec<_>>(),
        "links": links,
        "results": store::plan_row_count(&state.db, id, kind.as_str()).await,
    })))
}

#[derive(Deserialize)]
struct LinkBody {
    connection_id: i64,
    enabled: bool,
    /// Also send the results this plan already has, not only new ones.
    #[serde(default)]
    backfill: bool,
}

async fn set_plan_link(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(b): Json<LinkBody>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_PLANS).await?;
    let plan = store::get_plan(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("plan not found"))?;
    if b.enabled && !matches!(plan.kind_of(), store::PlanKind::Prospects | store::PlanKind::Artifacts) {
        return Err(bad_request("this plan collects a report or files, not contacts to send to a CRM"));
    }
    if !store::set_plan_crm(&state.db, acc.tenant(), id, b.connection_id, b.enabled, b.backfill).await? {
        return Err(not_found("connection not found"));
    }
    store::hurry_crm_link(&state.db, acc.tenant(), id).await?;
    WAKE.notify_one();
    plan_links(State(state), AuthUser(acc), Path(id)).await
}

async fn sync_now(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_PLANS).await?;
    store::get_plan(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("plan not found"))?;
    store::hurry_crm_link(&state.db, acc.tenant(), id).await?;
    WAKE.notify_one();
    Ok(Json(json!({ "ok": true })))
}

// ---- the sync loop -----------------------------------------------------------

static WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();
const TICK: Duration = Duration::from_secs(45);
/// A pass that has not finished in this long died, and its link is taken again.
const LEASE_SECS: i64 = 600;
/// A link is worked at most this often (unless hurried).
const GAP_SECS: i64 = 30;
/// Rows pushed per link per pass: a long backfill goes out over several.
const PER_PASS: i64 = 25;
/// Between pushes, so a backfill stays well inside a CRM's rate limits.
const SPACING: Duration = Duration::from_millis(300);

pub fn spawn_sync(db: store::Db) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = pass(&db).await {
                tracing::warn!("crm sync: {e:#}");
            }
            let _ = tokio::time::timeout(TICK, WAKE.notified()).await;
        }
    });
}

async fn pass(db: &store::Db) -> anyhow::Result<()> {
    for (account_id, plan_id, connection_id, since) in store::claim_crm_links(db, LEASE_SECS, GAP_SECS, 20).await? {
        let err = match work(db, account_id, plan_id, connection_id, since).await {
            Ok(()) => String::new(),
            Err(e) => e,
        };
        store::finish_crm_link(db, account_id, plan_id, connection_id, &err).await?;
    }
    Ok(())
}

/// Push what one plan owes one connection. The error string is the link's
/// problem, for its settings card ("" = none).
async fn work(db: &store::Db, account_id: i64, plan_id: i64, connection_id: i64, since: chrono::DateTime<chrono::Utc>) -> Result<(), String> {
    let fail = |e: anyhow::Error| format!("{e:#}");
    let Some(conn) = store::get_crm_connection(db, account_id, connection_id).await.map_err(fail)? else { return Ok(()) };
    let Some(provider) = Provider::parse(&conn.provider) else { return Err("unknown CRM".into()) };
    let Some(credential) = crate::web::signing::open_secret(&conn.credential) else {
        let why = "the saved credential can't be read on this server — reconnect it";
        store::note_crm_connection(db, account_id, connection_id, true, why).await.map_err(fail)?;
        return Err(why.into());
    };
    let settings = settings_of(&conn);
    let Some(plan) = store::get_plan(db, account_id, plan_id).await.map_err(fail)? else { return Ok(()) };

    // The plan's rows owed to this connection, as contacts.
    let rows: Vec<(&'static str, i64, Contact)> = match plan.kind_of() {
        store::PlanKind::Prospects => store::crm_pending_prospects(db, account_id, plan_id, connection_id, since, PER_PASS)
            .await
            .map_err(fail)?
            .iter()
            .map(|p| ("prospect", p.prospect_id, Contact::from_prospect(p)))
            .collect(),
        store::PlanKind::Artifacts => {
            let cols = crate::artifact::output_columns(&crate::artifact::parse_schema(&plan.fields_schema_json));
            store::crm_pending_artifacts(db, account_id, plan_id, connection_id, since, PER_PASS)
                .await
                .map_err(fail)?
                .iter()
                .map(|a| ("artifact", a.artifact_id, Contact::from_artifact(a, &cols)))
                .collect()
        }
        _ => return Ok(()),
    };
    let mut any_ok = false;
    for (i, (kind, row_id, contact)) in rows.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(SPACING).await;
        }
        match crm::push(provider, &credential, &settings, contact).await {
            Ok(p) => {
                any_ok = true;
                let status = if p.matched { "matched" } else { "created" };
                store::record_crm_push(db, account_id, connection_id, plan_id, kind, *row_id, status, (&p.person_id, &p.org_id, &p.lead_id), "")
                    .await
                    .map_err(fail)?;
            }
            Err(PushError::Rejected(why)) => {
                store::record_crm_push(db, account_id, connection_id, plan_id, kind, *row_id, "failed", ("", "", ""), &why).await.map_err(fail)?;
            }
            Err(PushError::Auth(why)) => {
                // Every row would fail the same way: stop, and say so where it can be fixed.
                store::note_crm_connection(db, account_id, connection_id, true, &why).await.map_err(fail)?;
                tracing::warn!(account_id, connection_id, "CRM paused: {why}");
                return Err(why);
            }
            Err(PushError::Retry(why)) => {
                // Not this row's fault: nothing recorded, the next pass tries again.
                return Err(format!("will retry shortly: {why}"));
            }
        }
    }
    if any_ok {
        store::note_crm_connection(db, account_id, connection_id, false, "").await.map_err(fail)?;
    }
    Ok(())
}

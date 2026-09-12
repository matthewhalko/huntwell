//! The planning service: drafts the plans the website queues.
//!
//! Drafting was always a background job — the website spawned it, set
//! `DraftStatus`, and returned before it finished; the UI polls that status.
//! Making it a service therefore needs no new HTTP surface, just a queue, and
//! the queue is the column that was already there.
//!
//! Work arrives the same way it does for the run pool: through the database.
//! The website marks a plan `queued`; this claims it with an atomic
//! `queued` -> `drafting` UPDATE and writes the result back. Nothing calls
//! this service and it calls nothing — which is why the website needs no agent
//! CLI, and this does.

use std::time::Duration;

use anyhow::Result;

use crate::store::{self, Db, SourceConfig};

/// How long a plan may sit in 'drafting' before it is assumed abandoned.
/// Comfortably longer than a slow draft: requeuing one that is still running
/// would have two services drafting the same plan.
const ABANDONED_AFTER_MINUTES: i64 = 30;

/// The request that redrafts a plan that already exists.
///
/// Everything here comes off the plan row, which is what lets the drafting run
/// in a different process from the request that asked for it — the brief, the
/// kind, the columns and the sites are all already stored.
pub fn draft_request(sc: &SourceConfig) -> crate::plan_chat::PlanDraftRequest {
    crate::plan_chat::PlanDraftRequest {
        icp: sc.description.clone(),
        plan_type: String::new(),
        // A redraft is a redraft of this plan, not a re-decision about what it is.
        kind: sc.kind_of().as_str().to_string(),
        source: sc.source.clone(),
        source_key_tmpl: String::new(),
        seed_vars_json: String::new(),
        learn: sc.learn,
        iterations: sc.iterations as i64,
        target_prospects: sc.target_prospects as i64,
        // It keeps the columns the plan already has: they are the shape of the
        // rows already stored against it.
        columns: crate::artifact::parse_schema(&sc.fields_schema_json)
            .into_iter()
            .map(|f| crate::artifact::ColumnRequest {
                name: if f.label.trim().is_empty() { f.key.clone() } else { f.label.clone() },
                prompt: String::new(),
            })
            .collect(),
        sites: sc.sites.clone(),
        // A plan that exists keeps working even if its kind was since turned
        // off: withdrawing a feature must not break what people already have.
        allowed: vec![sc.kind_of().as_str().to_string()],
    }
}

/// This service's own HTTP surface.
///
/// `POST /drafts` is the endpoint version of what the website does by writing a
/// row: it queues a plan for drafting. Both paths exist on purpose — the website
/// already has the plan open in a transaction and a row is cheaper than a call,
/// while another service (or a person with curl) has neither.
pub fn routes(db: Db) -> axum::Router {
    use axum::{routing::post, Json, Router};
    Router::new()
        .route(
            "/drafts",
            post(|axum::extract::State(db): axum::extract::State<Db>, Json(body): Json<serde_json::Value>| async move {
                let account_id = body["account_id"].as_i64();
                let plan_id = body["plan_id"].as_i64();
                let (Some(account_id), Some(plan_id)) = (account_id, plan_id) else {
                    return (
                        axum::http::StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": "account_id and plan_id are required" })),
                    );
                };
                // No request body: the service rebuilds the brief from the plan,
                // which is what a redraft is.
                match store::queue_draft_bare(&db, account_id, plan_id).await {
                    Ok(()) => (
                        axum::http::StatusCode::ACCEPTED,
                        Json(serde_json::json!({ "plan_id": plan_id, "status": "queued" })),
                    ),
                    Err(e) => (
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({ "error": format!("{e:#}") })),
                    ),
                }
            }),
        )
        .with_state(db)
}

/// Depth of the queue, for `/statusz`. The number you want when someone says
/// "my plan has been building for ten minutes".
pub fn status() -> serde_json::Value {
    serde_json::json!({ "queue": QUEUE_DEPTH.load(std::sync::atomic::Ordering::Relaxed) })
}

static QUEUE_DEPTH: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);

pub async fn serve(db: Db) -> Result<()> {
    // A planning service that died mid-draft left plans stuck on a spinner
    // that never stops. Nothing was written, so a retry is free.
    match store::requeue_abandoned_drafts(&db, ABANDONED_AFTER_MINUTES).await {
        Ok(n) if n > 0 => tracing::warn!("requeued {n} plan(s) abandoned mid-draft"),
        Ok(_) => {}
        Err(e) => tracing::warn!("could not requeue abandoned drafts: {e:#}"),
    }

    // Idle poll. Drafting takes tens of seconds, so a second or two of latency
    // getting started is invisible next to it and the query is cheap — it hits
    // a partial index that is empty almost always.
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    tracing::info!("planning service ready");
    loop {
        tick.tick().await;
        if let Ok(n) = store::queued_draft_count(&db).await {
            QUEUE_DEPTH.store(n, std::sync::atomic::Ordering::Relaxed);
        }
        loop {
            let claimed = match store::claim_queued_draft(&db).await {
                Ok(Some(c)) => c,
                // Queue drained — back to the tick.
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("claiming a queued draft: {e:#}");
                    break;
                }
            };
            let store::ClaimedDraft { plan_id, account_id, request, adopt_name } = claimed;

            // The queued brief when there is one; otherwise this is a redraft
            // and the plan itself is the brief.
            let req = match request {
                Some(r) => r,
                None => match store::get_plan(&db, account_id, plan_id).await {
                    Ok(Some(sc)) => draft_request(&sc),
                    // The plan went away between the claim and the read.
                    // Nothing to draft, and nothing to report.
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!(plan_id, "could not read the claimed plan: {e:#}");
                        let _ = store::set_draft_status(&db, account_id, plan_id, "failed").await;
                        continue;
                    }
                },
            };

            tracing::info!(plan_id, account_id, "drafting");
            crate::web::api::run_draft(&db, account_id, plan_id, req, adopt_name).await;
        }
    }
}

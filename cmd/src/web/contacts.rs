//! The built-in CRM (2026-10-18): contacts, their stage, and a log of what
//! happened with each. A module — only a workspace an operator switched `crm`
//! on for (admin → Users → Modules) gets these routes; everyone else is told
//! it is not on. Not the CRM *integrations* (`web::crm`), which push results
//! to someone else's CRM.
//!
//! Contacts come from a plan's results (`import`: a whole plan, or picked
//! results) or are typed in. A result becomes a `crm::Contact` first — the
//! same reading of prospects and custom columns the integrations use — and is
//! imported once: the same result again is `Already`, and someone already here
//! with the same email is `Linked` (their blanks filled) rather than doubled.

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::require_cap;
use super::auth::AuthUser;
use super::{bad_request, not_found, ApiError, App};
use crate::crm::Contact;
use crate::store::{self, ContactFields, ContactImport};

pub fn routes() -> Router<App> {
    Router::new()
        .route("/contacts", get(list).post(create))
        .route("/contacts/import", post(import))
        .route("/contacts/lookup", post(lookup))
        .route("/contacts/{id}", get(detail).put(update).delete(remove))
        .route("/contacts/{id}/stage", put(set_stage))
        .route("/contacts/{id}/activity", post(log_activity))
        .route("/contacts/{id}/activity/{activity_id}", delete(remove_activity))
}

/// Most a single import brings in, so one click cannot stall the website.
const IMPORT_MAX: usize = 2000;

/// The module gate: every route goes through it, reads included.
async fn crm_on(state: &App, acc: &store::Account) -> Result<(), ApiError> {
    if store::has_module(&state.db, acc.tenant(), "crm").await {
        Ok(())
    } else {
        Err(ApiError(axum::http::StatusCode::FORBIDDEN, "The CRM isn't switched on for this workspace.".into()))
    }
}

/// Reading is open to anyone in the workspace; changing contacts is a plans
/// write, like drafting outreach.
async fn writer(state: &App, acc: &store::Account) -> Result<(), ApiError> {
    crm_on(state, acc).await?;
    require_cap(state, acc, store::CAP_PLANS).await
}

fn meta() -> Value {
    json!({
        "stages": store::CONTACT_STAGES.iter().map(|(id, label)| json!({ "id": id, "label": label })).collect::<Vec<_>>(),
        "kinds": store::ACTIVITY_KINDS.iter().map(|(id, label)| json!({ "id": id, "label": label })).collect::<Vec<_>>(),
    })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQuery {
    q: String,
    stage: String,
}

async fn list(State(state): State<App>, AuthUser(acc): AuthUser, Query(f): Query<ListQuery>) -> Result<Json<Value>, ApiError> {
    crm_on(&state, &acc).await?;
    let contacts = store::list_contacts(&state.db, acc.tenant(), &f.q, f.stage.trim(), 5000).await?;
    let counts = store::contact_stage_counts(&state.db, acc.tenant()).await?;
    let mut v = meta();
    v["contacts"] = json!(contacts);
    v["counts"] = json!(counts);
    Ok(Json(v))
}

async fn detail(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    crm_on(&state, &acc).await?;
    let c = store::get_contact(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("contact not found"))?;
    let activity = store::list_contact_activity(&state.db, acc.tenant(), id).await?;
    let mut v = meta();
    v["contact"] = json!(c);
    v["activity"] = json!(activity);
    Ok(Json(v))
}

async fn create(State(state): State<App>, AuthUser(acc): AuthUser, Json(f): Json<ContactFields>) -> Result<Json<Value>, ApiError> {
    writer(&state, &acc).await?;
    let f = f.clean().map_err(bad_request)?;
    let id = store::create_contact(&state.db, acc.tenant(), acc.account_id, &f).await?;
    Ok(Json(json!({ "contact_id": id })))
}

async fn update(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(f): Json<ContactFields>) -> Result<Json<Value>, ApiError> {
    writer(&state, &acc).await?;
    let f = f.clean().map_err(bad_request)?;
    if !store::update_contact(&state.db, acc.tenant(), id, &f).await? {
        return Err(not_found("contact not found"));
    }
    detail(State(state), AuthUser(acc), Path(id)).await
}

#[derive(Deserialize)]
struct StageBody {
    stage: String,
}

async fn set_stage(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(b): Json<StageBody>) -> Result<Json<Value>, ApiError> {
    writer(&state, &acc).await?;
    if !store::CONTACT_STAGES.iter().any(|(s, _)| *s == b.stage) {
        return Err(bad_request("unknown stage"));
    }
    if !store::set_contact_stage(&state.db, acc.tenant(), id, &b.stage).await? {
        return Err(not_found("contact not found"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn remove(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    writer(&state, &acc).await?;
    if !store::delete_contact(&state.db, acc.tenant(), id).await? {
        return Err(not_found("contact not found"));
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ActivityBody {
    kind: String,
    #[serde(default)]
    body: String,
    /// When it happened; now when left out.
    #[serde(default)]
    occurred_at: Option<DateTime<Utc>>,
}

async fn log_activity(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(b): Json<ActivityBody>) -> Result<Json<Value>, ApiError> {
    writer(&state, &acc).await?;
    if !store::ACTIVITY_KINDS.iter().any(|(k, _)| *k == b.kind) {
        return Err(bad_request("pick what kind of activity it was"));
    }
    if b.body.trim().is_empty() && b.kind == "note" {
        return Err(bad_request("write the note first"));
    }
    let now = Utc::now();
    // A date in the future is a plan, not something that happened: a follow-up.
    let at = b.occurred_at.unwrap_or(now).min(now + chrono::Duration::minutes(5));
    store::add_contact_activity(&state.db, acc.tenant(), id, acc.account_id, &b.kind, &b.body, at)
        .await?
        .ok_or_else(|| not_found("contact not found"))?;
    detail(State(state), AuthUser(acc), Path(id)).await
}

async fn remove_activity(State(state): State<App>, AuthUser(acc): AuthUser, Path((id, activity_id)): Path<(i64, i64)>) -> Result<Json<Value>, ApiError> {
    writer(&state, &acc).await?;
    if !store::delete_contact_activity(&state.db, acc.tenant(), id, activity_id).await? {
        return Err(not_found("activity not found"));
    }
    detail(State(state), AuthUser(acc), Path(id)).await
}

// ---- import ---------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct ImportBody {
    /// Every result a plan has.
    plan_id: Option<i64>,
    /// Or these results.
    prospect_ids: Vec<i64>,
    artifact_ids: Vec<i64>,
}

/// A result's contact as the CRM keeps it: the fields it has room for, and
/// the rest (a custom plan's other columns, where it was found) as notes.
fn fields_of(c: &Contact) -> ContactFields {
    let used = [&c.name, &c.email, &c.phone, &c.title, &c.company, &c.website, &c.linkedin];
    let mut notes: Vec<String> = Vec::new();
    for (label, value) in &c.extra {
        if !used.iter().any(|u| !u.is_empty() && *u == value) {
            notes.push(format!("{label}: {value}"));
        }
    }
    if !c.notes.is_empty() {
        notes.push(c.notes.clone());
    }
    if !c.source_url.is_empty() {
        notes.push(format!("Found on: {}", c.source_url));
    }
    ContactFields {
        name: c.name.clone(),
        email: c.email.clone(),
        phone: c.phone.clone(),
        title: c.title.clone(),
        company: c.company.clone(),
        website: c.website.clone(),
        linkedin: c.linkedin.clone(),
        location: c.location.clone(),
        notes: notes.join("\n"),
        stage: "lead".into(),
        follow_up_on: None,
    }
}

async fn import(State(state): State<App>, AuthUser(acc): AuthUser, Json(b): Json<ImportBody>) -> Result<Json<Value>, ApiError> {
    writer(&state, &acc).await?;
    let ws = acc.tenant();
    // (kind, row id, plan id, plan name, contact)
    let mut rows: Vec<(&'static str, i64, i64, String, Contact)> = Vec::new();
    if let Some(plan_id) = b.plan_id {
        let plan = store::get_plan(&state.db, ws, plan_id).await?.ok_or_else(|| not_found("plan not found"))?;
        match plan.kind_of() {
            store::PlanKind::Prospects => {
                for p in store::export_prospects(&state.db, ws, Some(plan_id), 0).await?.iter().take(IMPORT_MAX) {
                    rows.push(("prospect", p.prospect_id, p.plan_id, p.source.clone(), Contact::from_prospect(p)));
                }
            }
            store::PlanKind::Artifacts => {
                let cols = crate::artifact::output_columns(&crate::artifact::parse_schema(&plan.fields_schema_json));
                for a in store::export_artifacts(&state.db, ws, Some(plan_id)).await?.iter().take(IMPORT_MAX) {
                    rows.push(("artifact", a.artifact_id, a.plan_id, a.source.clone(), Contact::from_artifact(a, &cols)));
                }
            }
            _ => return Err(bad_request("this plan collects a report or files, not people to keep in a CRM")),
        }
    }
    for id in b.prospect_ids.iter().take(IMPORT_MAX) {
        if let Some(p) = store::get_prospect(&state.db, ws, *id).await? {
            rows.push(("prospect", p.prospect_id, p.plan_id, p.source.clone(), Contact::from_prospect(&p)));
        }
    }
    for id in b.artifact_ids.iter().take(IMPORT_MAX) {
        if let Some(a) = store::get_artifact(&state.db, ws, *id).await? {
            let Some(plan) = store::get_plan(&state.db, ws, a.plan_id).await? else { continue };
            let cols = crate::artifact::output_columns(&crate::artifact::parse_schema(&plan.fields_schema_json));
            rows.push(("artifact", a.artifact_id, a.plan_id, a.source.clone(), Contact::from_artifact(&a, &cols)));
        }
    }
    if rows.is_empty() {
        return Err(bad_request("there are no results to import"));
    }
    let (mut created, mut linked, mut already, mut skipped) = (0, 0, 0, 0);
    let mut last = None;
    for (kind, row_id, plan_id, plan_name, c) in &rows {
        // A result with nothing to call it by is not a contact.
        let Ok(f) = fields_of(c).clean() else {
            skipped += 1;
            continue;
        };
        let r = store::import_contact(&state.db, ws, acc.account_id, (kind, *row_id), (*plan_id, plan_name), &f).await?;
        match r {
            ContactImport::Created(_) => created += 1,
            ContactImport::Linked(_) => linked += 1,
            ContactImport::Already(_) => already += 1,
        }
        last = Some(r.contact_id());
    }
    Ok(Json(json!({
        "created": created,
        "linked": linked,
        "already": already,
        "skipped": skipped,
        // For a single result: where it now lives, to open it.
        "contact_id": if rows.len() == 1 { last } else { None },
    })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LookupBody {
    prospect_ids: Vec<i64>,
    artifact_ids: Vec<i64>,
}

/// Which of these results are contacts already, for the results pages: the
/// ones imported, and — for a few at a time, as a result's dialog asks — the
/// ones whose email matched someone already here.
async fn lookup(State(state): State<App>, AuthUser(acc): AuthUser, Json(b): Json<LookupBody>) -> Result<Json<Value>, ApiError> {
    crm_on(&state, &acc).await?;
    let ws = acc.tenant();
    let prospect_ids = &b.prospect_ids[..b.prospect_ids.len().min(1000)];
    let artifact_ids = &b.artifact_ids[..b.artifact_ids.len().min(1000)];
    let mut p = store::contacts_for_sources(&state.db, ws, "prospect", prospect_ids).await?;
    let mut a = store::contacts_for_sources(&state.db, ws, "artifact", artifact_ids).await?;
    if prospect_ids.len() + artifact_ids.len() <= 5 {
        let missing: Vec<i64> = prospect_ids.iter().copied().filter(|id| !p.contains_key(id)).collect();
        for id in &missing {
            if let Some(row) = store::get_prospect(&state.db, ws, *id).await? {
                if let Some(c) = store::contact_by_email(&state.db, ws, &Contact::from_prospect(&row).email).await? {
                    p.insert(*id, c);
                }
            }
        }
        let missing: Vec<i64> = artifact_ids.iter().copied().filter(|id| !a.contains_key(id)).collect();
        for id in &missing {
            let Some(row) = store::get_artifact(&state.db, ws, *id).await? else { continue };
            let Some(plan) = store::get_plan(&state.db, ws, row.plan_id).await? else { continue };
            let cols = crate::artifact::output_columns(&crate::artifact::parse_schema(&plan.fields_schema_json));
            if let Some(c) = store::contact_by_email(&state.db, ws, &Contact::from_artifact(&row, &cols).email).await? {
                a.insert(*id, c);
            }
        }
    }
    Ok(Json(json!({ "prospects": p, "artifacts": a })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_keeps_what_has_no_field_as_notes() {
        let c = Contact {
            name: "Dr. Ana Ruiz".into(),
            email: "ana@brightsmiles.example".into(),
            company: "Bright Smiles Dental".into(),
            source_url: "https://dir.example/bright".into(),
            extra: vec![
                ("Owner".into(), "Dr. Ana Ruiz".into()),
                ("Chairs".into(), "6".into()),
                ("Contact email".into(), "ana@brightsmiles.example".into()),
            ],
            ..Default::default()
        };
        let f = fields_of(&c).clean().unwrap();
        assert_eq!(f.notes, "Chairs: 6\nFound on: https://dir.example/bright");
        assert_eq!(f.stage, "lead");
    }

    #[test]
    fn a_contact_needs_something_to_call_it_by() {
        assert!(ContactFields::default().clean().is_err());
        let f = ContactFields { email: " Ana@Example.COM ".into(), stage: "bogus".into(), ..Default::default() }.clean().unwrap();
        assert_eq!((f.email.as_str(), f.stage.as_str()), ("ana@example.com", "lead"));
    }
}

//! Outreach: the app's cold-email drafts. `crate::outreach` writes them; this
//! is who may ask, what it costs, and where they are kept.
//!
//! Every route is scoped to the workspace (`acc.tenant()`); the footer is the
//! signed-in person's own. Drafting and revising are billed to the workspace's
//! wallet at its per-million-token rate, like a run.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::require_cap;
use super::auth::AuthUser;
use super::{bad_request, not_found, ApiError, App};
use crate::outreach as draft;
use crate::store::{self, OutreachText, Recipient};

pub fn routes() -> Router<App> {
    Router::new()
        .route("/outreach", get(list).post(create))
        .route("/outreach/profile", get(profile).put(save_profile))
        .route("/plans/{id}/outreach", get(plan_outreach).put(save_plan_outreach))
        .route("/outreach/designs", get(list_designs).post(create_design))
        .route("/outreach/designs/{id}", axum::routing::put(update_design).delete(delete_design))
        .route("/outreach/{id}", get(one).put(edit).delete(remove))
        .route("/outreach/{id}/revise", post(revise))
        .route("/outreach/{id}/restore", post(restore))
}

fn too_long(what: &str, max: usize) -> ApiError {
    bad_request(format!("{what} must be {max} characters or fewer"))
}

// ---- settings -----------------------------------------------------------------

async fn profile(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    let p = store::get_outreach_profile(&state.db, acc.tenant()).await?;
    let footer = store::get_outreach_footer(&state.db, acc.account_id).await?;
    Ok(Json(json!({
        "product": p.product,
        "rules": p.rules,
        "footer": footer,
        // Plans with their own outreach on — what a hand-entered draft can be
        // written for.
        "campaigns": store::outreach_campaigns(&state.db, acc.tenant()).await?
            .into_iter()
            .map(|(plan_id, name)| json!({ "plan_id": plan_id, "name": name }))
            .collect::<Vec<_>>(),
        // Saved profiles — any draft, to anyone, can be written to one.
        "designs": store::list_outreach_designs(&state.db, acc.tenant()).await?
            .into_iter()
            .map(|d| json!({ "design_id": d.design_id, "name": d.name }))
            .collect::<Vec<_>>(),
        // Whether drafting can work at all on this server, so the page can say
        // so instead of offering a button that fails.
        "ready": draft::model(&state.db).await.is_some(),
    })))
}

#[derive(Deserialize)]
struct ProfileBody {
    #[serde(default)]
    product: String,
    #[serde(default)]
    rules: String,
    #[serde(default)]
    footer: String,
}

async fn save_profile(State(state): State<App>, AuthUser(acc): AuthUser, Json(b): Json<ProfileBody>) -> Result<Json<Value>, ApiError> {
    if b.product.chars().count() > draft::MAX_PRODUCT {
        return Err(too_long("The product description", draft::MAX_PRODUCT));
    }
    if b.rules.chars().count() > draft::MAX_RULES {
        return Err(too_long("The rules", draft::MAX_RULES));
    }
    if b.footer.chars().count() > draft::MAX_FOOTER {
        return Err(too_long("The footer", draft::MAX_FOOTER));
    }
    // The footer is the person's own; the product and rules are the team's,
    // so changing those takes the right to build plans.
    let current = store::get_outreach_profile(&state.db, acc.tenant()).await?;
    if current.product != b.product.trim() || current.rules != b.rules.trim() {
        require_cap(&state, &acc, store::CAP_PLANS).await?;
        store::set_outreach_profile(&state.db, acc.tenant(), b.product.trim(), b.rules.trim(), acc.account_id).await?;
    }
    store::set_outreach_footer(&state.db, acc.account_id, b.footer.trim_end()).await?;
    Ok(Json(json!({ "ok": true })))
}

/// A plan's own outreach design, with the workspace's beside it so the page
/// can show what a blank field falls back to.
async fn plan_outreach(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    let o = store::get_plan_outreach(&state.db, acc.tenant(), id).await?.ok_or_else(|| not_found("plan not found"))?;
    let ws = store::get_outreach_profile(&state.db, acc.tenant()).await?;
    Ok(Json(json!({
        "custom": o.custom,
        "design_id": o.design_id,
        "brief": o.brief,
        "product": o.product,
        "rules": o.rules,
        "workspace": { "product": ws.product, "rules": ws.rules },
    })))
}

async fn save_plan_outreach(
    State(state): State<App>,
    AuthUser(acc): AuthUser,
    Path(id): Path<i64>,
    Json(b): Json<store::PlanOutreach>,
) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_PLANS).await?;
    if b.brief.chars().count() > draft::MAX_BRIEF {
        return Err(too_long("The campaign brief", draft::MAX_BRIEF));
    }
    if b.product.chars().count() > draft::MAX_PRODUCT {
        return Err(too_long("The product description", draft::MAX_PRODUCT));
    }
    if b.rules.chars().count() > draft::MAX_RULES {
        return Err(too_long("The rules", draft::MAX_RULES));
    }
    let o = store::PlanOutreach {
        custom: b.custom,
        design_id: b.design_id,
        brief: b.brief.trim().into(),
        product: b.product.trim().into(),
        rules: b.rules.trim().into(),
    };
    if o.custom && o.brief.is_empty() && o.product.is_empty() && o.rules.is_empty() {
        return Err(bad_request("say who this campaign is for, or what to offer them, before tailoring it"));
    }
    if let Some(d) = o.design_id {
        if !store::outreach_design_exists(&state.db, acc.tenant(), d).await? {
            return Err(not_found("outreach profile not found"));
        }
    }
    if !store::set_plan_outreach(&state.db, acc.tenant(), id, &o).await? {
        return Err(not_found("plan not found"));
    }
    plan_outreach(State(state), AuthUser(acc), Path(id)).await
}

// ---- saved profiles -----------------------------------------------------------
//
// A named outreach design that belongs to no plan: picked for any draft, or
// used by a plan as its outreach.

async fn list_designs(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "designs": store::list_outreach_designs(&state.db, acc.tenant()).await? })))
}

#[derive(Deserialize)]
struct DesignBody {
    name: String,
    #[serde(default)]
    brief: String,
    #[serde(default)]
    product: String,
    #[serde(default)]
    rules: String,
}

async fn save_design(state: &App, acc: &store::Account, id: Option<i64>, b: DesignBody) -> Result<Json<Value>, ApiError> {
    require_cap(state, acc, store::CAP_PLANS).await?;
    let name: String = b.name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string();
    if name.is_empty() {
        return Err(bad_request("give the profile a name"));
    }
    if name.chars().count() > 120 {
        return Err(too_long("The name", 120));
    }
    if b.brief.chars().count() > draft::MAX_BRIEF {
        return Err(too_long("Who it's for", draft::MAX_BRIEF));
    }
    if b.product.chars().count() > draft::MAX_PRODUCT {
        return Err(too_long("The product description", draft::MAX_PRODUCT));
    }
    if b.rules.chars().count() > draft::MAX_RULES {
        return Err(too_long("The rules", draft::MAX_RULES));
    }
    let d = store::PlanOutreach { custom: true, design_id: None, brief: b.brief.trim().into(), product: b.product.trim().into(), rules: b.rules.trim().into() };
    if d.brief.is_empty() && d.product.is_empty() && d.rules.is_empty() {
        return Err(bad_request("say who it's for, what to offer, or how the emails should read"));
    }
    let saved = store::save_outreach_design(&state.db, acc.tenant(), id, &name, &d, acc.account_id).await.map_err(|e| {
        if format!("{e:#}").contains("outreach_design_name_idx") {
            bad_request("you already have a profile with that name")
        } else {
            e.into()
        }
    })?;
    let id = saved.ok_or_else(|| not_found("outreach profile not found"))?;
    let all = store::list_outreach_designs(&state.db, acc.tenant()).await?;
    let one = all.into_iter().find(|d| d.design_id == id).ok_or_else(|| not_found("outreach profile not found"))?;
    Ok(Json(json!({ "design": one })))
}

async fn create_design(State(state): State<App>, AuthUser(acc): AuthUser, Json(b): Json<DesignBody>) -> Result<Json<Value>, ApiError> {
    save_design(&state, &acc, None, b).await
}

async fn update_design(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(b): Json<DesignBody>) -> Result<Json<Value>, ApiError> {
    save_design(&state, &acc, Some(id), b).await
}

async fn delete_design(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_PLANS).await?;
    if !store::delete_outreach_design(&state.db, acc.tenant(), id).await? {
        return Err(not_found("outreach profile not found"));
    }
    Ok(Json(json!({ "ok": true })))
}

// ---- drafts -------------------------------------------------------------------
//
// The work itself lives in the `pub(crate)` functions below, taking a
// workspace and a person rather than a session, so the signed `/v1` API
// (`public_api`) drafts through exactly the same checks, billing and history.

/// Who is writing: the person drafts are signed by and billed through, and
/// the workspace everything belongs to.
pub(crate) struct Writer {
    pub workspace: i64,
    pub person: i64,
    /// "Sam Lee, Acme" — for the prompt.
    pub sender: String,
}

impl Writer {
    /// From a signed-in session.
    async fn of(state: &App, acc: &store::Account) -> Writer {
        Writer::for_person(state, acc.tenant(), acc).await
    }

    /// For `person` writing in `workspace`.
    pub(crate) async fn for_person(state: &App, workspace: i64, person: &store::Account) -> Writer {
        let ws = store::workspace_name(&state.db, workspace).await.unwrap_or_default();
        let name = if person.display_name.trim().is_empty() {
            person.email.split('@').next().unwrap_or("").to_string()
        } else {
            person.display_name.clone()
        };
        let sender = if ws.trim().is_empty() { name } else { format!("{name}, {ws}") };
        Writer { workspace, person: person.account_id, sender }
    }
}

/// What a draft is to: a prospect in the workspace, or someone typed in.
#[derive(Deserialize)]
pub(crate) struct CreateBody {
    #[serde(default)]
    pub prospect_id: Option<i64>,
    #[serde(default)]
    pub recipient: Option<Recipient>,
    /// A custom-schema result: who it is to is read from its columns, and an
    /// Email column fills the To (`outreach::recipient_from_record`).
    #[serde(default)]
    pub artifact_id: Option<i64>,
    /// Write it to this plan's outreach design. A prospect's own plan is used
    /// when this is absent.
    #[serde(default)]
    pub plan_id: Option<i64>,
    /// Write it with this saved profile — for anyone, prospect or not. Wins
    /// over any plan.
    #[serde(default)]
    pub design_id: Option<i64>,
}

async fn list(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "items": store::list_outreach(&state.db, acc.tenant(), 500).await? })))
}

async fn one(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    Ok(Json(one_json(&state, acc.tenant(), id).await?))
}

/// A draft with its versions, newest first.
pub(crate) async fn one_json(state: &App, workspace: i64, id: i64) -> Result<Value, ApiError> {
    let o = store::get_outreach(&state.db, workspace, id).await?.ok_or_else(|| not_found("draft not found"))?;
    let versions = store::list_outreach_versions(&state.db, workspace, id).await?;
    Ok(json!({ "outreach": o, "versions": versions }))
}

/// The checks every paid call makes: not too fast, has credit, and there is a
/// model to ask. (Permission is the caller's: a session's role, or a key.)
async fn paid_call_checks(state: &App, workspace: i64) -> Result<String, ApiError> {
    crate::throttle::OUTREACH_DRAFTS
        .hit(&workspace.to_string())
        .map_err(|wait| ApiError(StatusCode::TOO_MANY_REQUESTS, format!("that's a lot of drafts at once — try again in {wait}s")))?;
    if store::account_credit_micros(&state.db, workspace).await? <= 0 {
        return Err(ApiError(StatusCode::PAYMENT_REQUIRED, "no credits remaining — buy credits in Usage & billing to draft outreach".into()));
    }
    draft::model(&state.db).await.ok_or_else(|| {
        ApiError(StatusCode::SERVICE_UNAVAILABLE, "outreach drafting isn't set up on this server yet".into())
    })
}

/// Bill a call to the workspace. A failure here is logged, not shown: the
/// person has their draft, and the charge is the platform's problem.
async fn bill(state: &App, workspace: i64, d: &draft::Drafted) {
    if let Err(e) = store::charge_account_usage(&state.db, workspace, d.usage, d.cost_micros).await {
        tracing::error!(account = workspace, "outreach draft not billed: {e:#}");
    }
}

/// What a model failure is told as: which kind of failure, not the
/// provider's details (those go to the log).
///
/// Never 502 or 504: behind Cloudflare those are replaced with Cloudflare's own
/// "Host Error" page, and the app would show that page's HTML as the message.
fn model_failed(e: anyhow::Error) -> ApiError {
    use crate::llm::LlmError;
    tracing::warn!("outreach draft failed: {e:#}");
    if let Some(u) = e.downcast_ref::<draft::Unusable>() {
        return ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{u} — try again, or change the rules if it keeps happening"));
    }
    match e.downcast_ref::<LlmError>() {
        Some(LlmError::RateLimited { .. }) => {
            ApiError(StatusCode::TOO_MANY_REQUESTS, "the writing model is busy right now — try again in a minute".into())
        }
        Some(LlmError::Unauthorized | LlmError::BadRequest(_)) => ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "outreach drafting isn't working on this server right now — its model setup needs attention".into(),
        ),
        _ => ApiError(StatusCode::SERVICE_UNAVAILABLE, "the draft couldn't be written just now — try again in a moment".into()),
    }
}

/// The extra facts a prospect carries beyond name, title and company.
fn prospect_extra(p: &store::ProspectRow) -> Vec<(&str, &str)> {
    vec![("Industry", p.industry.as_str()), ("Location", p.location.as_str()), ("Website", p.website.as_str())]
}

/// Draft a new email. Validates who it is to before anything is spent.
pub(crate) async fn draft_new(state: &App, w: &Writer, b: CreateBody) -> Result<store::OutreachRow, ApiError> {
    // The plan a record came from, for its outreach design.
    let mut record_plan: Option<i64> = None;
    let mut artifact_id: Option<i64> = None;
    let (prospect, to) = match (b.prospect_id, b.recipient) {
        (None, _) if b.artifact_id.is_some() => {
            let id = b.artifact_id.unwrap_or_default();
            let a = store::get_artifact(&state.db, w.workspace, id).await?.ok_or_else(|| not_found("result not found"))?;
            let plan = store::get_plan(&state.db, w.workspace, a.plan_id).await?.ok_or_else(|| not_found("result not found"))?;
            let cols = crate::artifact::output_columns(&crate::artifact::parse_schema(&plan.fields_schema_json));
            let to = draft::recipient_from_record(&a.title, &a.fields, &cols);
            record_plan = Some(a.plan_id);
            artifact_id = Some(a.artifact_id);
            (None, to)
        }
        (Some(id), _) => {
            let p = store::get_prospect(&state.db, w.workspace, id).await?.ok_or_else(|| not_found("prospect not found"))?;
            let to = Recipient {
                name: p.name.clone(),
                email: p.email.clone(),
                title: p.title.clone(),
                company: p.company.clone(),
                notes: p.notes.clone(),
            };
            (Some(p), to)
        }
        (None, Some(r)) => {
            let to = Recipient {
                name: r.name.trim().chars().take(200).collect(),
                email: r.email.trim().chars().take(320).collect(),
                title: r.title.trim().chars().take(200).collect(),
                company: r.company.trim().chars().take(200).collect(),
                notes: r.notes.trim().chars().take(2000).collect(),
            };
            if to.name.is_empty() && to.company.is_empty() {
                return Err(bad_request("give the recipient's name or company"));
            }
            if !to.email.is_empty() && (!to.email.contains('@') || to.email.contains(char::is_whitespace)) {
                return Err(bad_request("that email address doesn't look right"));
            }
            (None, to)
        }
        (None, None) => return Err(bad_request("choose a prospect or enter a recipient")),
    };
    // Checked before anything is spent: a plan named here must be this
    // workspace's.
    if let Some(pid) = b.plan_id {
        store::get_plan_outreach(&state.db, w.workspace, pid).await?.ok_or_else(|| not_found("plan not found"))?;
    }
    if let Some(d) = b.design_id {
        if !store::outreach_design_exists(&state.db, w.workspace, d).await? {
            return Err(not_found("outreach profile not found"));
        }
    }
    let campaign = store::outreach_campaign(
        &state.db,
        w.workspace,
        b.design_id,
        b.plan_id.or(prospect.as_ref().map(|p| p.plan_id)).or(record_plan),
    )
    .await?;
    let model = paid_call_checks(state, w.workspace).await?;
    let profile = store::get_outreach_profile(&state.db, w.workspace).await?;
    let footer = store::get_outreach_footer(&state.db, w.person).await?;
    let extra = prospect.as_ref().map(prospect_extra).unwrap_or_default();
    let prompt = draft::prompt(&profile, campaign.as_ref(), &w.sender, &to, &extra);
    let d = draft::draft(&model, &prompt, None, None).await.map_err(model_failed)?;
    bill(state, w.workspace, &d).await;
    let text = OutreachText { subject: &d.subject, body: &d.body, source: "draft", feedback: "", model: &model, usage: d.usage };
    Ok(store::create_outreach(&state.db, w.workspace, w.person, prospect.map(|p| p.prospect_id), artifact_id, campaign.as_ref(), &to, &footer, &text).await?)
}

/// Rewrite a draft from feedback.
pub(crate) async fn revise_draft(state: &App, w: &Writer, id: i64, feedback: &str) -> Result<store::OutreachRow, ApiError> {
    let feedback = feedback.trim();
    if feedback.is_empty() {
        return Err(bad_request("say what to change"));
    }
    if feedback.chars().count() > draft::MAX_FEEDBACK {
        return Err(too_long("Feedback", draft::MAX_FEEDBACK));
    }
    let o = store::get_outreach(&state.db, w.workspace, id).await?.ok_or_else(|| not_found("draft not found"))?;
    let model = paid_call_checks(state, w.workspace).await?;
    // The conversation is rebuilt from what is stored: the same first prompt,
    // then the draft as it stands now (hand edits included), then the ask.
    let profile = store::get_outreach_profile(&state.db, w.workspace).await?;
    let to = Recipient {
        name: o.recipient_name.clone(),
        email: o.recipient_email.clone(),
        title: o.recipient_title.clone(),
        company: o.recipient_company.clone(),
        notes: o.recipient_notes.clone(),
    };
    let prospect = match o.prospect_id {
        Some(pid) => store::get_prospect(&state.db, w.workspace, pid).await?,
        None => None,
    };
    let extra = prospect.as_ref().map(prospect_extra).unwrap_or_default();
    // The campaign it was first written to, as that plan's design reads now.
    let campaign = store::outreach_campaign(&state.db, w.workspace, o.design_id, o.plan_id).await?;
    let prompt = draft::prompt(&profile, campaign.as_ref(), &w.sender, &to, &extra);
    let d = draft::draft(&model, &prompt, Some((&o.subject, &o.body)), Some(feedback)).await.map_err(model_failed)?;
    bill(state, w.workspace, &d).await;
    let text = OutreachText { subject: &d.subject, body: &d.body, source: "revise", feedback, model: &model, usage: d.usage };
    store::add_outreach_version(&state.db, w.workspace, id, w.person, &text).await?.ok_or_else(|| not_found("draft not found"))
}

/// Replace the text by hand: a new version, free.
pub(crate) async fn edit_draft(state: &App, w: &Writer, id: i64, subject: &str, body: &str) -> Result<store::OutreachRow, ApiError> {
    let subject = subject.trim();
    let body = body.trim();
    if body.is_empty() {
        return Err(bad_request("the email can't be empty"));
    }
    if subject.chars().count() > draft::MAX_SUBJECT {
        return Err(too_long("The subject", draft::MAX_SUBJECT));
    }
    if body.chars().count() > draft::MAX_BODY {
        return Err(too_long("The email", draft::MAX_BODY));
    }
    let subject: String = subject.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let text = OutreachText { subject: &subject, body, source: "edit", feedback: "", model: "", usage: Default::default() };
    store::add_outreach_version(&state.db, w.workspace, id, w.person, &text).await?.ok_or_else(|| not_found("draft not found"))
}

/// Go back to an earlier version — as a new version, so nothing is lost.
pub(crate) async fn restore_draft(state: &App, w: &Writer, id: i64, version: i32) -> Result<store::OutreachRow, ApiError> {
    let versions = store::list_outreach_versions(&state.db, w.workspace, id).await?;
    let v = versions.iter().find(|v| v.version == version).ok_or_else(|| not_found("version not found"))?;
    let feedback = format!("restored version {}", v.version);
    let text = OutreachText { subject: &v.subject, body: &v.body, source: "restore", feedback: &feedback, model: "", usage: Default::default() };
    store::add_outreach_version(&state.db, w.workspace, id, w.person, &text).await?.ok_or_else(|| not_found("draft not found"))
}

async fn create(State(state): State<App>, AuthUser(acc): AuthUser, Json(b): Json<CreateBody>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_PLANS).await?;
    let w = Writer::of(&state, &acc).await;
    Ok(Json(json!({ "outreach": draft_new(&state, &w, b).await? })))
}

#[derive(Deserialize)]
pub(crate) struct ReviseBody {
    pub feedback: String,
}

async fn revise(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(b): Json<ReviseBody>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_PLANS).await?;
    let w = Writer::of(&state, &acc).await;
    Ok(Json(json!({ "outreach": revise_draft(&state, &w, id, &b.feedback).await? })))
}

#[derive(Deserialize)]
pub(crate) struct EditBody {
    pub subject: String,
    pub body: String,
}

async fn edit(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(b): Json<EditBody>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_PLANS).await?;
    let w = Writer::of(&state, &acc).await;
    Ok(Json(json!({ "outreach": edit_draft(&state, &w, id, &b.subject, &b.body).await? })))
}

#[derive(Deserialize)]
pub(crate) struct RestoreBody {
    pub version: i32,
}

async fn restore(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>, Json(b): Json<RestoreBody>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_PLANS).await?;
    let w = Writer::of(&state, &acc).await;
    Ok(Json(json!({ "outreach": restore_draft(&state, &w, id, b.version).await? })))
}

async fn remove(State(state): State<App>, AuthUser(acc): AuthUser, Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    require_cap(&state, &acc, store::CAP_PLANS).await?;
    if !store::delete_outreach(&state.db, acc.tenant(), id).await? {
        return Err(not_found("draft not found"));
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::LlmError;

    #[test]
    fn a_failed_draft_says_what_kind_of_failure_and_never_502s() {
        let status = |e: anyhow::Error| model_failed(e).0;
        assert_eq!(status(anyhow::anyhow!(draft::Unusable("cut off".into()))), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(status(anyhow::Error::from(LlmError::RateLimited { retry_after: None })), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(status(anyhow::Error::from(LlmError::Unauthorized)), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(status(anyhow::Error::from(LlmError::Unavailable("test".into()))), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(status(anyhow::anyhow!("anything else")), StatusCode::SERVICE_UNAVAILABLE);
        let setup = model_failed(anyhow::Error::from(LlmError::BadRequest("unknown model".into())));
        assert!(setup.1.contains("model setup") && !setup.1.contains("unknown model"), "provider detail stays in the log: {}", setup.1);
    }
}

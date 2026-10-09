//! The card on file, prepaid credits, and the Stripe flow that puts both there.
//!
//! A run is refused without a payment method *and* a prepaid credit balance
//! (see [`super::runner::start`]), so this is the gate every account passes
//! through. It is deliberately the smallest Stripe integration that is real:
//!
//!   1. `POST /billing/session` creates (or reuses) a Stripe **Customer** and a
//!      **Checkout Session** in `setup` mode, and hands the browser its URL.
//!      Stripe collects the card; a card number never reaches this process or
//!      this database.
//!   2. Stripe returns the browser to the app with `?setup={CHECKOUT_SESSION_ID}`
//!      and `POST /billing/confirm` retrieves that session with the payment
//!      method expanded, keeping only the brand, last four, and `pm_…` id.
//!   3. `POST /billing/credits` charges that saved card and credits the prepaid
//!      wallet. A running job spends those credits as tokens are booked and
//!      is stopped the moment the wallet would go negative.
//!
//! Card setup and purchases read their result back from Stripe; a signed
//! webhook reconciles later refunds and disputes so reversed money cannot
//! remain spendable. No PAN ever reaches this process or database.
//!
//! **Local development** has no Stripe key, so `session` attaches a mock card
//! and a starter credit grant instead of returning a URL. The gate, the UI,
//! and the store path are then exercised exactly as they are in production;
//! only Stripe itself is absent. A production process never takes that path.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;

use super::auth::AuthUser;
use super::{bad_request, ApiError, App};
use crate::store;

/// Stripe's REST base. Overridable so a test can point at a local double.
fn api_base() -> String {
    crate::config::get("HUNTWELL_STRIPE_API_BASE").unwrap_or_else(|| "https://api.stripe.com/v1".into())
}

/// The secret key, when one is configured. Absent means mock mode.
fn secret_key() -> Option<String> {
    crate::config::get("STRIPE_SECRET_KEY").filter(|k| !k.trim().is_empty())
}

pub fn configured() -> bool {
    secret_key().is_some()
}

/// Mock cards and free credit grants are a local-dev stand-in. Production
/// never invents a card — Stripe has to be configured and a real card added.
fn mock_allowed() -> bool {
    !configured()
        && crate::config::is_dev()
        && crate::genesis::embedded_variant() == "local"
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/billing", get(billing))
        .route("/billing/intent", post(intent))
        .route("/billing/attach", post(attach))
        .route("/billing/session", post(session))
        .route("/billing/confirm", post(confirm))
        .route("/billing/credits", post(credits))
        .route("/billing/credits/confirm", post(credits_confirm))
        .route("/billing/webhook", post(webhook))
        .route("/billing/card", delete(remove_card))
        .route("/billing/auto-reload", axum::routing::put(put_auto_reload))
}

/// Cards and payments need the credits cap. Role defaults give it to the
/// owner and admins; an admin can grant or take it per member.
async fn require_payer(state: &App, acc: &store::Account) -> Result<(), ApiError> {
    let caps = store::workspace_caps(&state.db, acc.tenant(), acc.account_id).await?;
    if caps.credits {
        Ok(())
    } else {
        Err(ApiError(
            axum::http::StatusCode::FORBIDDEN,
            "you do not have permission to buy credits or change the card".into(),
        ))
    }
}

/// The publishable key the browser needs to mount Stripe's card field. Public
/// by design — it can only start a payment, never move money.
fn publishable_key() -> Option<String> {
    crate::config::get("STRIPE_PUBLISHABLE_KEY").filter(|k| !k.trim().is_empty())
}

/// Everything the in-app card dialog needs: the key to mount Stripe's own card
/// field with, and a SetupIntent to confirm against.
///
/// This is the "add a card without leaving the page" path. The card is typed
/// into Stripe's iframe and confirmed by Stripe's own script — the number never
/// touches this server, exactly as with Checkout, but the user stays put.
async fn intent(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    require_payer(&state, &acc).await?;
    let (Some(key), Some(pk)) = (secret_key(), publishable_key()) else {
        attach_mock_card(&state, acc.tenant()).await?;
        let pm = store::payment_method(&state.db, acc.tenant()).await?;
        return Ok(Json(json!({"mocked": true, "has_card": true, "card": card_json(pm)})));
    };
    let customer = customer_id(&state, &acc, &key).await?;
    let si: Value = post_form(
        &key,
        "setup_intents",
        &[("customer", customer.as_str()), ("usage", "off_session"), ("automatic_payment_methods[enabled]", "true")],
    )
    .await?;
    let secret = si
        .get("client_secret")
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request("Stripe did not return a setup secret"))?;
    Ok(Json(json!({"publishable_key": pk, "client_secret": secret})))
}

#[derive(Deserialize)]
struct AttachBody {
    setup_intent_id: String,
}

/// Records the card after the browser confirmed the SetupIntent. The id from
/// the browser is only a lookup key — what gets stored is what Stripe says.
async fn attach(
    State(state): State<App>,
    AuthUser(acc): AuthUser,
    Json(body): Json<AttachBody>,
) -> Result<Json<Value>, ApiError> {
    require_payer(&state, &acc).await?;
    let Some(key) = secret_key() else {
        return Err(bad_request("Stripe is not configured on this server"));
    };
    let id = body.setup_intent_id.trim();
    if id.is_empty() || !id.starts_with("seti_") {
        return Err(bad_request("not a setup intent id"));
    }
    let si: Value = get_json(&key, &format!("setup_intents/{id}?expand[]=payment_method")).await?;
    // The intent must be this account's, or someone else's card lands here.
    let customer = si
        .get("customer")
        .and_then(|c| c.as_str().map(str::to_string).or_else(|| c.get("id").and_then(Value::as_str).map(str::to_string)))
        .unwrap_or_default();
    let ours = customer_ref(&state, acc.tenant()).await?;
    if !ours.starts_with("cus_") || customer != ours {
        return Err(bad_request("that card setup belongs to another account"));
    }
    if si.get("status").and_then(Value::as_str) != Some("succeeded") {
        return Err(bad_request("the card was not confirmed"));
    }
    let card = si.pointer("/payment_method/card");
    let brand = card.and_then(|c| c.get("brand")).and_then(Value::as_str).unwrap_or("card");
    let last4 = card.and_then(|c| c.get("last4")).and_then(Value::as_str).unwrap_or_default();
    if last4.is_empty() {
        return Err(bad_request("Stripe returned no card details"));
    }
    let pm_id = stripe_id(si.get("payment_method"));
    store::set_payment_method(&state.db, acc.tenant(), &customer, brand, last4, &pm_id).await?;
    let pm = store::payment_method(&state.db, acc.tenant()).await?;
    Ok(Json(json!({"has_card": true, "card": card_json(pm)})))
}

fn card_json(pm: Option<store::PaymentMethod>) -> Value {
    match pm {
        Some(p) => json!({
            "brand": p.brand,
            "last4": p.last4,
            "added_at": p.added_at.map(|t| t.to_rfc3339()),
        }),
        None => Value::Null,
    }
}

/// What the billing page and the Go-now gate both read.
async fn billing(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    let pm = store::payment_method(&state.db, acc.tenant()).await?;
    let usage = store::ensure_usage(&state.db, acc.tenant()).await?;
    let has_card = if crate::config::is_production() {
        store::has_real_payment_method(&state.db, acc.tenant()).await?
    } else {
        pm.is_some()
    };
    Ok(Json(json!({
        // `stripe` says a real card can be taken here; `in_app` says it can be
        // taken without leaving the page (both keys present).
        "stripe": configured(),
        "in_app": configured() && publishable_key().is_some(),
        "production": crate::config::is_production(),
        "has_card": has_card,
        "has_credits": usage.credits_usd > 0.0,
        // Whether a run needs a card right now: not while free credit an
        // operator added is left (runner::start holds the same rule).
        "card_required": !has_card && !store::spends_free_credit(&state.db, acc.tenant()).await?,
        // Running on free credit, no card: say so, and that a card comes later.
        "free_credit": !has_card && store::spends_free_credit(&state.db, acc.tenant()).await?,
        // That free credit is gone and there is still no card: the prompt says why.
        "free_credit_spent": !has_card && usage.credits_usd <= 0.0 && store::has_granted_credit(&state.db, acc.tenant()).await?,
        "credits_usd": usage.credits_usd,
        "card": if has_card { card_json(pm) } else { Value::Null },
        "auto_reload": store::get_auto_reload(&state.db, acc.tenant()).await?,
        "auto_reload_terms": AUTO_RELOAD_TERMS,
        "auto_reload_max_per_day": AUTO_RELOAD_MAX_PER_DAY,
    })))
}

#[derive(Deserialize)]
struct SessionBody {
    /// Where to send the browser back to, as an absolute URL the app itself
    /// supplied. Only ever used as a Stripe redirect target.
    #[serde(default)]
    return_url: String,
}

/// Starts the card setup. Returns either a Stripe URL to send the browser to,
/// or — with no Stripe key — a card attached on the spot.
async fn session(
    State(state): State<App>,
    AuthUser(acc): AuthUser,
    Json(body): Json<SessionBody>,
) -> Result<Json<Value>, ApiError> {
    require_payer(&state, &acc).await?;
    let Some(key) = secret_key() else {
        attach_mock_card(&state, acc.tenant()).await?;
        let pm = store::payment_method(&state.db, acc.tenant()).await?;
        return Ok(Json(json!({"mocked": true, "has_card": true, "card": card_json(pm)})));
    };

    let return_url = return_url(&body.return_url)?;
    let customer = customer_id(&state, &acc, &key).await?;
    // `setup` mode saves a card for later charges rather than taking one now.
    let session: Value = post_form(
        &key,
        "checkout/sessions",
        &[
            ("mode", "setup"),
            ("customer", &customer),
            ("success_url", &format!("{return_url}?setup={{CHECKOUT_SESSION_ID}}")),
            ("cancel_url", &format!("{return_url}?setup=cancelled")),
        ],
    )
    .await?;
    let url = session
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request("Stripe did not return a checkout URL"))?;
    Ok(Json(json!({"url": url})))
}

#[derive(Deserialize)]
struct ConfirmBody {
    session_id: String,
}

/// Reads the finished checkout back from Stripe and stores the card's brand and
/// last four. The browser is told the session id; Stripe is asked what it means.
async fn confirm(
    State(state): State<App>,
    AuthUser(acc): AuthUser,
    Json(body): Json<ConfirmBody>,
) -> Result<Json<Value>, ApiError> {
    require_payer(&state, &acc).await?;
    let Some(key) = secret_key() else {
        return Err(bad_request("Stripe is not configured on this server"));
    };
    let id = body.session_id.trim();
    if id.is_empty() || !id.starts_with("cs_") {
        return Err(bad_request("not a checkout session id"));
    }
    let session: Value = get_json(
        &key,
        &format!("checkout/sessions/{id}?expand[]=setup_intent.payment_method"),
    )
    .await?;

    // The session must belong to the customer this account owns, or a stolen
    // session id from another account would attach someone else's card here.
    let customer = session
        .get("customer")
        .and_then(|c| c.as_str().map(str::to_string).or_else(|| c.get("id").and_then(Value::as_str).map(str::to_string)))
        .unwrap_or_default();
    let ours = store::payment_method(&state.db, acc.tenant())
        .await?
        .map(|p| p.payment_ref)
        .unwrap_or_default();
    let expected = if ours.is_empty() { customer_ref(&state, acc.tenant()).await? } else { ours };
    if !expected.starts_with("cus_") || customer != expected {
        return Err(bad_request("that checkout session belongs to another account"));
    }

    let card = session.pointer("/setup_intent/payment_method/card");
    let brand = card.and_then(|c| c.get("brand")).and_then(Value::as_str).unwrap_or("card");
    let last4 = card.and_then(|c| c.get("last4")).and_then(Value::as_str).unwrap_or_default();
    if last4.is_empty() {
        return Err(bad_request("checkout finished without a card attached"));
    }
    let pm_id = stripe_id(session.pointer("/setup_intent/payment_method"));
    store::set_payment_method(&state.db, acc.tenant(), &customer, brand, last4, &pm_id).await?;
    let pm = store::payment_method(&state.db, acc.tenant()).await?;
    Ok(Json(json!({"has_card": true, "card": card_json(pm)})))
}

/// Forgets the card. The Stripe customer is left alone — removing it there
/// would lose the account's billing history for the sake of a UI action.
async fn remove_card(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    require_payer(&state, &acc).await?;
    let payment_ref = customer_ref(&state, acc.tenant()).await?;
    store::set_payment_method(&state.db, acc.tenant(), &payment_ref, "", "", "").await?;
    // Nothing left to charge: auto-reload goes off with the card, and has to
    // be agreed to again for the next one.
    store::disable_auto_reload(&state.db, acc.tenant(), "turned off because the card was removed").await?;
    Ok(Json(json!({"has_card": false, "card": Value::Null})))
}

/// The Stripe customer already recorded for this account, if any.
async fn customer_ref(state: &App, account_id: i64) -> Result<String, ApiError> {
    Ok(store::payment_ref(&state.db, account_id).await?)
}

/// The account's Stripe customer, created on first use and remembered — an
/// abandoned checkout must not leave a second customer behind on the next try.
async fn customer_id(state: &App, acc: &store::Account, key: &str) -> Result<String, ApiError> {
    let existing = customer_ref(state, acc.tenant()).await?;
    if existing.starts_with("cus_") {
        return Ok(existing);
    }
    let customer: Value = post_form(
        key,
        "customers",
        &[
            ("email", acc.email.as_str()),
            ("name", acc.display_name.as_str()),
            ("metadata[account_id]", &acc.tenant().to_string()),
        ],
    )
    .await?;
    let id = customer
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request("Stripe did not return a customer id"))?;
    store::set_payment_ref(&state.db, acc.tenant(), id).await?;
    Ok(id.to_string())
}

/// Local-only: attach Stripe's test card and seed the prepaid wallet so a
/// box without keys can still exercise the same gates production uses.
async fn attach_mock_card(state: &App, account_id: i64) -> Result<(), ApiError> {
    if !mock_allowed() {
        return Err(bad_request(
            "Stripe is not configured on this server — a card cannot be added in production without it",
        ));
    }
    store::set_payment_method(&state.db, account_id, &format!("cus_mock_{account_id}"), "Visa", "4242", "pm_mock")
        .await?;
    // $50 starter so local runs are not blocked on a purchase flow that has
    // no Stripe to charge.
    store::grant_dev_credits_if_empty(&state.db, account_id, 50_000_000).await?;
    Ok(())
}

/// Stripe returns a payment method as either an id string or an expanded object.
fn stripe_id(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(obj) => obj.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
        None => String::new(),
    }
}

/// Whole dollars between $5 and $500 — enough to run, not enough to be a
/// typo that empties a card.
pub fn credit_pack_micros(usd: f64) -> Result<i64, ApiError> {
    if !usd.is_finite() || usd.fract() != 0.0 || usd < 5.0 || usd > 500.0 {
        return Err(bad_request("buy between $5 and $500 in whole dollars"));
    }
    Ok((usd * 1_000_000.0).round() as i64)
}

#[derive(Deserialize)]
struct CreditsBody {
    usd: f64,
    purchase_id: String,
}

/// Charges the card on file and credits the prepaid wallet.
async fn credits(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<CreditsBody>) -> Result<Json<Value>, ApiError> {
    purchase_credits(&state, &acc, body.usd, &body.purchase_id).await
}

/// The same purchase the Usage page and the leftover `/usage/topup` route share.
/// Production always goes through Stripe; local mock-credits the wallet.
pub async fn purchase_credits(
    state: &App,
    acc: &store::Account,
    usd: f64,
    purchase_id: &str,
) -> Result<Json<Value>, ApiError> {
    require_payer(state, acc).await?;
    let micros = credit_pack_micros(usd)?;
    let purchase_id = purchase_id.trim();
    if !(8..=80).contains(&purchase_id.len())
        || !purchase_id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(bad_request("purchase_id must be an 8–80 character request id"));
    }
    let Some(pm) = store::payment_method(&state.db, acc.tenant()).await? else {
        return Err(bad_request("add a card in Usage & billing before buying credits"));
    };
    let Some(key) = secret_key() else {
        if !mock_allowed() {
            return Err(bad_request("Stripe is not configured on this server"));
        }
        let ref_id = format!("mock_{}_{}", acc.tenant(), purchase_id);
        if store::apply_credit_purchase(&state.db, acc.tenant(), micros, &ref_id).await? {
            announce_credit(&state.db, acc.tenant(), micros, crate::mail::CreditKind::Purchase, "").await;
        }
        let usage = store::ensure_usage(&state.db, acc.tenant()).await?;
        return Ok(Json(json!({"mocked": true, "credits_usd": usage.credits_usd, "usage": usage})));
    };
    let customer = if pm.payment_ref.starts_with("cus_") {
        pm.payment_ref.clone()
    } else {
        return Err(bad_request("add a card in Usage & billing before buying credits"));
    };
    let mut card_pm = pm.card_pm.clone();
    if card_pm.is_empty() {
        card_pm = first_card_pm(&key, &customer).await?;
        if card_pm.is_empty() {
            return Err(bad_request("no card on file at Stripe — add one again"));
        }
    }
    let cents = (micros / 10_000).to_string();
    let account = acc.tenant().to_string();
    let pi: Value = post_form_idempotent(
        &key,
        "payment_intents",
        &format!("huntwell-{}-{purchase_id}", acc.tenant()),
        &[
            ("amount", cents.as_str()),
            ("currency", "usd"),
            ("customer", customer.as_str()),
            ("payment_method", card_pm.as_str()),
            ("confirm", "true"),
            ("off_session", "true"),
            ("metadata[account_id]", account.as_str()),
        ],
    )
    .await?;
    apply_payment_intent(state, acc.tenant(), micros, &pi).await
}

#[derive(Deserialize)]
struct CreditsConfirmBody {
    payment_intent_id: String,
}

/// Finishes a credit purchase that needed a second factor. The id from the
/// browser is only a lookup key — Stripe is asked what it means.
async fn credits_confirm(
    State(state): State<App>,
    AuthUser(acc): AuthUser,
    Json(body): Json<CreditsConfirmBody>,
) -> Result<Json<Value>, ApiError> {
    require_payer(&state, &acc).await?;
    let Some(key) = secret_key() else {
        return Err(bad_request("Stripe is not configured on this server"));
    };
    let id = body.payment_intent_id.trim();
    if id.is_empty() || !id.starts_with("pi_") {
        return Err(bad_request("not a payment intent id"));
    }
    let pi: Value = get_json(&key, &format!("payment_intents/{id}")).await?;
    let customer = stripe_id(pi.get("customer"));
    let ours = customer_ref(&state, acc.tenant()).await?;
    if !ours.starts_with("cus_") || customer != ours {
        return Err(bad_request("that payment belongs to another account"));
    }
    let amount_cents = pi.get("amount").and_then(Value::as_i64).unwrap_or(0);
    let micros = amount_cents.saturating_mul(10_000);
    apply_payment_intent(&state, acc.tenant(), micros, &pi).await
}

async fn apply_payment_intent(state: &App, account_id: i64, micros: i64, pi: &Value) -> Result<Json<Value>, ApiError> {
    let status = pi.get("status").and_then(Value::as_str).unwrap_or("");
    let id = pi.get("id").and_then(Value::as_str).unwrap_or_default();
    if status == "requires_action" || status == "requires_source_action" {
        let secret = pi.get("client_secret").and_then(Value::as_str).unwrap_or_default();
        return Ok(Json(json!({
            "requires_action": true,
            "payment_intent_id": id,
            "client_secret": secret,
        })));
    }
    if status != "succeeded" {
        return Err(bad_request(format!("the charge did not complete ({status})")));
    }
    if id.is_empty() {
        return Err(bad_request("Stripe returned no payment id"));
    }
    let amount_micros = pi.get("amount").and_then(Value::as_i64).unwrap_or(0).saturating_mul(10_000);
    let currency = pi.get("currency").and_then(Value::as_str).unwrap_or("");
    let owner = pi.pointer("/metadata/account_id").and_then(Value::as_str).unwrap_or("");
    if amount_micros != micros || currency != "usd" || owner != account_id.to_string() {
        return Err(bad_request("Stripe payment details do not match this credit purchase"));
    }
    // `false` when this payment was already applied (the browser retried, or
    // the 3-D Secure confirm raced the first call) — one email per payment.
    if store::apply_credit_purchase(&state.db, account_id, micros, id).await? {
        announce_credit(&state.db, account_id, micros, crate::mail::CreditKind::Purchase, "").await;
    }
    let usage = store::ensure_usage(&state.db, account_id).await?;
    Ok(Json(json!({"credits_usd": usage.credits_usd, "usage": usage})))
}

/// Email the workspace's owner that credit landed — bought, or added free by
/// an operator (`granted`, with their note). Queued, never sent inline, and a
/// failure to queue is logged rather than failing the credit, which is done.
pub async fn announce_credit(db: &store::Db, account_id: i64, usd_micros: i64, kind: crate::mail::CreditKind, note: &str) {
    let Ok(Some(owner)) = store::get_account(db, account_id).await else { return };
    let balance = store::ensure_usage(db, account_id).await.map(|u| u.credits_usd).unwrap_or(0.0);
    let link = crate::config::get("HUNTWELL_PUBLIC_URL")
        .map(|b| b.trim().trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
        .map(|b| format!("{b}/app/usage"));
    let msg = crate::mail::credits_added(usd_micros as f64 / 1e6, balance, kind, note, link.as_deref());
    let kind = match kind {
        crate::mail::CreditKind::Grant => "credit_grant",
        crate::mail::CreditKind::Purchase => "credit_purchase",
        crate::mail::CreditKind::AutoReload { .. } => "credit_auto",
    };
    if let Err(e) = store::queue_mail(db, Some(account_id), &owner.email, kind, &msg).await {
        tracing::warn!(account_id, "credit email not queued: {e:#}");
    }
}

async fn first_card_pm(key: &str, customer: &str) -> Result<String, ApiError> {
    let body: Value = get_json(key, &format!("customers/{customer}/payment_methods?type=card")).await?;
    let id = body
        .get("data")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .map(|pm| stripe_id(Some(pm)))
        .unwrap_or_default();
    Ok(id)
}

/// Stripe retries these events until acknowledged. The signature proves the
/// raw body came from Stripe; `billing_event` makes applying it idempotent.
async fn webhook(State(state): State<App>, headers: HeaderMap, body: Bytes) -> Result<Json<Value>, ApiError> {
    let secret = crate::config::get("STRIPE_WEBHOOK_SECRET")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| bad_request("Stripe webhook is not configured"))?;
    let signature = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| bad_request("missing Stripe signature"))?;
    verify_webhook_signature(&secret, signature, &body)?;
    let event: Value = serde_json::from_slice(&body).map_err(|_| bad_request("invalid Stripe webhook JSON"))?;
    if crate::config::is_production() && event.get("livemode").and_then(Value::as_bool) != Some(true) {
        return Err(bad_request("test-mode Stripe event refused in production"));
    }
    let event_id = event.get("id").and_then(Value::as_str).unwrap_or("");
    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
    if !event_id.starts_with("evt_") {
        return Err(bad_request("Stripe event has no id"));
    }
    let object = event.pointer("/data/object").ok_or_else(|| bad_request("Stripe event has no object"))?;
    let (payment_intent, reversed_cents) = match event_type {
        "charge.refunded" => (
            stripe_id(object.get("payment_intent")),
            object.get("amount_refunded").and_then(Value::as_i64).unwrap_or(0),
        ),
        "charge.dispute.created" => (
            stripe_id(object.get("payment_intent")),
            object.get("amount").and_then(Value::as_i64).unwrap_or(0),
        ),
        _ => return Ok(Json(json!({"received": true, "ignored": true}))),
    };
    if !payment_intent.starts_with("pi_") || reversed_cents <= 0 {
        return Err(bad_request("Stripe reversal has invalid payment details"));
    }
    // Retrieve the intent rather than trusting nested webhook metadata.
    let key = secret_key().ok_or_else(|| bad_request("Stripe is not configured on this server"))?;
    let pi: Value = get_json(&key, &format!("payment_intents/{payment_intent}")).await?;
    let account_id = pi
        .pointer("/metadata/account_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .ok_or_else(|| bad_request("Stripe payment has no Huntwell account"))?;
    store::apply_credit_reversal(
        // Webhook metadata supplies the tenant, and the store still scopes
        // both the purchase and wallet updates by it.
        &state.db,
        account_id,
        event_id,
        event_type,
        &payment_intent,
        reversed_cents.saturating_mul(10_000),
    )
    .await?;
    Ok(Json(json!({"received": true})))
}

fn verify_webhook_signature(secret: &str, header: &str, body: &[u8]) -> Result<(), ApiError> {
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        if let Some(v) = part.trim().strip_prefix("t=") {
            timestamp = v.parse::<i64>().ok();
        } else if let Some(v) = part.trim().strip_prefix("v1=") {
            if let Ok(bytes) = hex::decode(v) {
                signatures.push(bytes);
            }
        }
    }
    let timestamp = timestamp.ok_or_else(|| bad_request("invalid Stripe signature timestamp"))?;
    if (chrono::Utc::now().timestamp() - timestamp).abs() > 300 {
        return Err(bad_request("stale Stripe webhook"));
    }
    let mut payload = timestamp.to_string().into_bytes();
    payload.push(b'.');
    payload.extend_from_slice(body);
    let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(&payload);
    if signatures.iter().any(|sig| mac.clone().verify_slice(sig).is_ok()) {
        Ok(())
    } else {
        Err(bad_request("invalid Stripe webhook signature"))
    }
}

/// Where Stripe sends the browser back. The app supplies it, but only an
/// http(s) URL is ever handed on.
fn return_url(raw: &str) -> Result<String, ApiError> {
    let url = raw.trim().trim_end_matches('?').to_string();
    if url.starts_with("http://") || url.starts_with("https://") {
        return Ok(url);
    }
    if let Some(base) = crate::config::get("HUNTWELL_PUBLIC_URL") {
        return Ok(format!("{}/app/usage", base.trim_end_matches('/')));
    }
    Err(bad_request("no return URL for the checkout"))
}

async fn post_form(key: &str, path: &str, form: &[(&str, &str)]) -> Result<Value, ApiError> {
    let res = reqwest::Client::new()
        .post(format!("{}/{path}", api_base()))
        .bearer_auth(key)
        .form(form)
        .send()
        .await
        .map_err(|e| bad_request(format!("Stripe request failed: {e}")))?;
    read(res).await
}

async fn post_form_idempotent(
    key: &str,
    path: &str,
    idempotency_key: &str,
    form: &[(&str, &str)],
) -> Result<Value, ApiError> {
    let res = reqwest::Client::new()
        .post(format!("{}/{path}", api_base()))
        .bearer_auth(key)
        .header("Idempotency-Key", idempotency_key)
        .form(form)
        .send()
        .await
        .map_err(|e| bad_request(format!("Stripe request failed: {e}")))?;
    read(res).await
}

async fn get_json(key: &str, path: &str) -> Result<Value, ApiError> {
    let res = reqwest::Client::new()
        .get(format!("{}/{path}", api_base()))
        .bearer_auth(key)
        .send()
        .await
        .map_err(|e| bad_request(format!("Stripe request failed: {e}")))?;
    read(res).await
}

/// Stripe's own error message is the useful one, so it is passed through rather
/// than replaced with a generic failure.
async fn read(res: reqwest::Response) -> Result<Value, ApiError> {
    let status = res.status();
    let body: Value = res.json().await.map_err(|e| bad_request(format!("Stripe sent no JSON: {e}")))?;
    if !status.is_success() {
        let msg = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("Stripe rejected the request");
        return Err(bad_request(format!("Stripe: {msg}")));
    }
    Ok(body)
}

// ---- auto-reload ----------------------------------------------------------------
//
// "When my balance falls below $10, add $50." The website's own loop watches
// balances and charges the saved card off-session — the only process that
// holds the Stripe key (workers never do). It charges only what the payer
// agreed to, recorded with the wording's version; at most
// AUTO_RELOAD_MAX_PER_DAY times a day; and a card that fails turns it off and
// says so by email, rather than trying again and again.

/// The version of the wording the payer agrees to. The text itself is on the
/// Usage page (`AutoReloadCard`); change both together, and bump this, and
/// everyone agrees again the next time they save.
pub const AUTO_RELOAD_TERMS: &str = "auto-reload-v1";
/// A runaway (a looping plan, a stolen session) cannot charge more than this.
pub const AUTO_RELOAD_MAX_PER_DAY: i64 = 3;
const AUTO_RELOAD_TICK: std::time::Duration = std::time::Duration::from_secs(20);
/// A claim older than this is a crash mid-charge, and is taken again.
const AUTO_RELOAD_LEASE_SECS: i64 = 600;
/// Never two reloads within this of each other, nor a retry sooner.
const AUTO_RELOAD_GAP_SECS: i64 = 120;

static AUTO_RELOAD_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

#[derive(Deserialize)]
struct AutoReloadBody {
    enabled: bool,
    below_usd: f64,
    amount_usd: f64,
    /// The payer ticked the box agreeing to be charged. Needed whenever it is
    /// turned on or its numbers change.
    #[serde(default)]
    agree: bool,
}

/// Whole dollars from $5 to $500 for the threshold; the amount is a credit
/// pack, and at least $10 so a reload is worth a card charge.
fn auto_reload_numbers(below: f64, amount: f64) -> Result<(i64, i64), ApiError> {
    if !below.is_finite() || below.fract() != 0.0 || !(5.0..=500.0).contains(&below) {
        return Err(bad_request("the balance to reload at is between $5 and $500 in whole dollars"));
    }
    if !amount.is_finite() || amount.fract() != 0.0 || !(10.0..=500.0).contains(&amount) {
        return Err(bad_request("the amount to add is between $10 and $500 in whole dollars"));
    }
    Ok(((below * 1e6) as i64, (amount * 1e6) as i64))
}

async fn put_auto_reload(State(state): State<App>, AuthUser(acc): AuthUser, Json(b): Json<AutoReloadBody>) -> Result<Json<Value>, ApiError> {
    require_payer(&state, &acc).await?;
    let (below, amount) = auto_reload_numbers(b.below_usd, b.amount_usd)?;
    let current = store::get_auto_reload(&state.db, acc.tenant()).await?;
    let mut agreed = None;
    if b.enabled {
        if store::payment_method(&state.db, acc.tenant()).await?.is_none() {
            return Err(bad_request("add a card before turning on auto-reload"));
        }
        // What was agreed to is the numbers and the wording; any change to
        // either is a new agreement.
        let same = current.enabled
            && (current.below_usd * 1e6).round() as i64 == below
            && (current.amount_usd * 1e6).round() as i64 == amount
            && current.terms == AUTO_RELOAD_TERMS;
        if !same {
            if !b.agree {
                return Err(bad_request("agree to be charged automatically to turn on auto-reload"));
            }
            agreed = Some((acc.account_id, AUTO_RELOAD_TERMS));
        }
    }
    store::set_auto_reload(&state.db, acc.tenant(), b.enabled, below, amount, agreed).await?;
    if b.enabled {
        tracing::info!(account_id = acc.tenant(), by = acc.account_id, below, amount, "auto-reload on");
        // Already below the line: reload now rather than at the next tick.
        AUTO_RELOAD_WAKE.notify_one();
    }
    Ok(Json(json!({ "auto_reload": store::get_auto_reload(&state.db, acc.tenant()).await? })))
}

/// Start the loop. Nothing to do on a server that cannot charge a card.
pub fn spawn_auto_reload(db: store::Db) {
    if !configured() && !mock_allowed() {
        return;
    }
    tokio::spawn(async move {
        loop {
            reload_due(&db).await;
            let _ = tokio::time::timeout(AUTO_RELOAD_TICK, AUTO_RELOAD_WAKE.notified()).await;
        }
    });
}

/// How a charge went, as far as what to do next.
#[derive(Debug)]
enum Charge {
    /// Credited (`true`), or this exact charge was already applied (`false`).
    Done(bool),
    /// The card or the bank said no, or wants the payer present. Retrying
    /// will not help; auto-reload turns off.
    Declined(String),
    /// Stripe or the network failed. Tried again after the gap.
    Retry(String),
}

async fn reload_due(db: &store::Db) {
    let due = match store::claim_auto_reloads(db, AUTO_RELOAD_LEASE_SECS, AUTO_RELOAD_GAP_SECS, 20).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("auto-reload: claiming: {e:#}");
            return;
        }
    };
    for (account_id, amount, below) in due {
        if store::auto_reloads_today(db, account_id).await.unwrap_or(0) >= AUTO_RELOAD_MAX_PER_DAY {
            let why = format!("paused: it already reloaded {AUTO_RELOAD_MAX_PER_DAY} times in the last 24 hours — it resumes after that, or buy credit by hand");
            let _ = store::finish_auto_reload(db, account_id, &why, true).await;
            continue;
        }
        match charge_saved_card(db, account_id, amount).await {
            Charge::Done(fresh) => {
                let _ = store::finish_auto_reload(db, account_id, "", true).await;
                if fresh {
                    tracing::info!(account_id, usd = amount as f64 / 1e6, "auto-reload charged");
                    announce_credit(db, account_id, amount, crate::mail::CreditKind::AutoReload { below_usd: below as f64 / 1e6 }, "").await;
                }
            }
            Charge::Declined(why) => {
                tracing::warn!(account_id, "auto-reload declined, turning it off: {why}");
                let _ = store::disable_auto_reload(db, account_id, &format!("turned off: {why}")).await;
                announce_reload_failed(db, account_id, amount, &why).await;
            }
            Charge::Retry(why) => {
                tracing::warn!(account_id, "auto-reload will retry: {why}");
                let _ = store::finish_auto_reload(db, account_id, &format!("will try again shortly: {why}"), true).await;
            }
        }
    }
}

/// Charge the card on file `micros`, off-session, and credit the wallet.
async fn charge_saved_card(db: &store::Db, account_id: i64, micros: i64) -> Charge {
    let pm = match store::payment_method(db, account_id).await {
        Ok(Some(pm)) => pm,
        Ok(None) => return Charge::Declined("there is no card on file".into()),
        Err(e) => return Charge::Retry(format!("{e:#}")),
    };
    // Numbered, so a retry of this reload is the same charge at Stripe and
    // the next reload is a new one.
    let n = match store::auto_reload_count(db, account_id).await {
        Ok(n) => n + 1,
        Err(e) => return Charge::Retry(format!("{e:#}")),
    };
    let Some(key) = secret_key() else {
        if !mock_allowed() {
            return Charge::Declined("card payments are not set up on this server".into());
        }
        return match store::apply_credit_auto_reload(db, account_id, micros, &format!("mock_auto_{account_id}_{n}")).await {
            Ok(fresh) => Charge::Done(fresh),
            Err(e) => Charge::Retry(format!("{e:#}")),
        };
    };
    if !pm.payment_ref.starts_with("cus_") {
        return Charge::Declined("there is no card on file".into());
    }
    let mut card = pm.card_pm.clone();
    if card.is_empty() {
        card = match first_card_pm(&key, &pm.payment_ref).await {
            Ok(c) if !c.is_empty() => c,
            Ok(_) => return Charge::Declined("there is no card on file at Stripe".into()),
            Err(e) => return Charge::Retry(e.1),
        };
    }
    let cents = (micros / 10_000).to_string();
    let account = account_id.to_string();
    let res = reqwest::Client::new()
        .post(format!("{}/payment_intents", api_base()))
        .bearer_auth(&key)
        .header("Idempotency-Key", format!("huntwell-auto-{account_id}-{n}"))
        .form(&[
            ("amount", cents.as_str()),
            ("currency", "usd"),
            ("customer", pm.payment_ref.as_str()),
            ("payment_method", card.as_str()),
            ("confirm", "true"),
            ("off_session", "true"),
            ("description", "Huntwell auto-reload"),
            ("metadata[account_id]", account.as_str()),
            ("metadata[kind]", "auto_reload"),
        ])
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await;
    let res = match res {
        Ok(r) => r,
        Err(_) => return Charge::Retry("could not reach Stripe".into()),
    };
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = body.pointer("/error/message").and_then(Value::as_str).unwrap_or("the payment failed").to_string();
        // A card error (402) is the card's answer; anything else may pass.
        let card_error = status.as_u16() == 402 || body.pointer("/error/type").and_then(Value::as_str) == Some("card_error");
        return if card_error { Charge::Declined(msg) } else { Charge::Retry(format!("Stripe: {msg}")) };
    }
    match body.get("status").and_then(Value::as_str).unwrap_or("") {
        "succeeded" => {}
        "processing" => return Charge::Retry("the payment is still processing".into()),
        "requires_action" | "requires_source_action" => {
            return Charge::Declined("your bank asked to confirm the charge yourself, which auto-reload can't do — buy credit by hand once to confirm the card".into())
        }
        other => return Charge::Declined(format!("the payment did not complete ({other})")),
    }
    let id = body.get("id").and_then(Value::as_str).unwrap_or_default();
    let amount = body.get("amount").and_then(Value::as_i64).unwrap_or(0).saturating_mul(10_000);
    let owner = body.pointer("/metadata/account_id").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() || amount != micros || body.get("currency").and_then(Value::as_str) != Some("usd") || owner != account {
        tracing::error!(account_id, "auto-reload: Stripe's payment does not match what was asked for");
        return Charge::Declined("the payment details did not match".into());
    }
    match store::apply_credit_auto_reload(db, account_id, micros, id).await {
        Ok(fresh) => Charge::Done(fresh),
        // Charged but not yet credited: the same idempotency key on the next
        // try returns this payment, and `payment_ref` credits it once.
        Err(e) => Charge::Retry(format!("{e:#}")),
    }
}

/// Tell the owner auto-reload failed and is now off.
async fn announce_reload_failed(db: &store::Db, account_id: i64, micros: i64, why: &str) {
    let Ok(Some(owner)) = store::get_account(db, account_id).await else { return };
    let link = crate::config::get("HUNTWELL_PUBLIC_URL")
        .map(|b| b.trim().trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
        .map(|b| format!("{b}/app/usage"));
    let msg = crate::mail::auto_reload_failed(micros as f64 / 1e6, why.trim_end_matches('.'), link.as_deref());
    if let Err(e) = store::queue_mail(db, Some(account_id), &owner.email, "auto_reload_failed", &msg).await {
        tracing::warn!(account_id, "auto-reload failure email not queued: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_reload_numbers_are_bounded_whole_dollars() {
        assert_eq!(auto_reload_numbers(10.0, 50.0).ok(), Some((10_000_000, 50_000_000)));
        for (below, amount) in [(4.0, 50.0), (10.5, 50.0), (10.0, 5.0), (10.0, 501.0), (f64::NAN, 50.0), (10.0, 49.99)] {
            assert!(auto_reload_numbers(below, amount).is_err(), "{below} {amount}");
        }
    }

    fn ok(raw: &str) -> Option<String> {
        return_url(raw).ok()
    }

    #[test]
    fn a_return_url_must_be_a_real_url() {
        assert_eq!(ok("https://app.example.com/app/usage").as_deref(), Some("https://app.example.com/app/usage"));
        assert_eq!(ok("http://127.0.0.1:8611/app/usage").as_deref(), Some("http://127.0.0.1:8611/app/usage"));
        // Anything else falls back to the configured public URL, or refuses —
        // a scheme the browser would execute never reaches Stripe.
        let js = ok("javascript:alert(1)");
        assert!(js.is_none() || js.as_deref().is_some_and(|u| u.starts_with("http")));
    }

    #[test]
    fn a_card_renders_as_null_when_there_is_none() {
        assert_eq!(card_json(None), Value::Null);
        let pm = store::PaymentMethod {
            payment_ref: "cus_x".into(),
            brand: "Visa".into(),
            last4: "4242".into(),
            added_at: None,
            card_pm: "pm_x".into(),
        };
        assert_eq!(card_json(Some(pm))["last4"], json!("4242"));
    }

    #[test]
    fn credit_packs_are_whole_dollars_in_range() {
        assert!(credit_pack_micros(4.0).is_err());
        assert!(matches!(credit_pack_micros(25.0), Ok(25_000_000)));
        assert!(credit_pack_micros(25.5).is_err());
        assert!(credit_pack_micros(501.0).is_err());
        assert!(matches!(credit_pack_micros(5.0), Ok(5_000_000)));
    }

    #[test]
    fn a_stripe_id_reads_both_shapes() {
        assert_eq!(stripe_id(Some(&json!("pm_abc"))), "pm_abc");
        assert_eq!(stripe_id(Some(&json!({"id": "pm_abc"}))), "pm_abc");
        assert_eq!(stripe_id(None), "");
    }

    #[test]
    fn webhook_signature_covers_timestamp_and_raw_body() {
        let secret = "whsec_test";
        let body = br#"{"id":"evt_test"}"#;
        let timestamp = chrono::Utc::now().timestamp();
        let mut payload = timestamp.to_string().into_bytes();
        payload.push(b'.');
        payload.extend_from_slice(body);
        let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(&payload);
        let header = format!("t={timestamp},v1={}", hex::encode(mac.finalize().into_bytes()));
        assert!(verify_webhook_signature(secret, &header, body).is_ok());
        assert!(verify_webhook_signature(secret, &header, br#"{"id":"evt_other"}"#).is_err());
    }
}

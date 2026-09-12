//! The card on file, and the Stripe flow that puts it there.
//!
//! A run is refused without a payment method (see [`super::runner::start`]), so
//! this is the gate every account passes through once. It is deliberately the
//! smallest Stripe integration that is real:
//!
//!   1. `POST /billing/session` creates (or reuses) a Stripe **Customer** and a
//!      **Checkout Session** in `setup` mode, and hands the browser its URL.
//!      Stripe collects the card; a card number never reaches this process or
//!      this database.
//!   2. Stripe returns the browser to the app with `?setup={CHECKOUT_SESSION_ID}`
//!      and `POST /billing/confirm` retrieves that session with the payment
//!      method expanded, keeping only the brand and last four.
//!
//! No webhook, no signature verification, no stored PAN — the redirect carries
//! the session id and the confirm call reads the truth back from Stripe over
//! the API, which is what makes this safe to skip a webhook for.
//!
//! **Local development** has no Stripe key, so `session` attaches a mock card
//! instead of returning a URL and the client goes straight to the confirmed
//! state. The gate, the UI, and the store path are then exercised exactly as
//! they are in production; only Stripe itself is absent.

use axum::extract::State;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

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

pub fn routes() -> Router<App> {
    Router::new()
        .route("/billing", get(billing))
        .route("/billing/intent", post(intent))
        .route("/billing/attach", post(attach))
        .route("/billing/session", post(session))
        .route("/billing/confirm", post(confirm))
        .route("/billing/card", delete(remove_card))
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
    let (Some(key), Some(pk)) = (secret_key(), publishable_key()) else {
        // No keys here: attach the test card so the dialog can finish and the
        // gate behaves the way it will in production.
        store::set_payment_method(&state.db, acc.tenant(), &format!("cus_mock_{}", acc.tenant()), "Visa", "4242").await?;
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
    if customer.is_empty() || (!ours.is_empty() && customer != ours) {
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
    store::set_payment_method(&state.db, acc.tenant(), &customer, brand, last4).await?;
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
    Ok(Json(json!({
        // `stripe` says a real card can be taken here; `in_app` says it can be
        // taken without leaving the page (both keys present).
        "stripe": configured(),
        "in_app": configured() && publishable_key().is_some(),
        "has_card": pm.is_some(),
        "card": card_json(pm),
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
    let Some(key) = secret_key() else {
        // Mock mode: the same store path, so the gate and the UI behave as they
        // will in production. Stripe's own test card, to make it obvious.
        store::set_payment_method(
            &state.db,
            acc.tenant(),
            &format!("cus_mock_{}", acc.tenant()),
            "Visa",
            "4242",
        )
        .await?;
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
    if customer.is_empty() || (!expected.is_empty() && customer != expected) {
        return Err(bad_request("that checkout session belongs to another account"));
    }

    let card = session.pointer("/setup_intent/payment_method/card");
    let brand = card.and_then(|c| c.get("brand")).and_then(Value::as_str).unwrap_or("card");
    let last4 = card.and_then(|c| c.get("last4")).and_then(Value::as_str).unwrap_or_default();
    if last4.is_empty() {
        return Err(bad_request("checkout finished without a card attached"));
    }
    store::set_payment_method(&state.db, acc.tenant(), &customer, brand, last4).await?;
    let pm = store::payment_method(&state.db, acc.tenant()).await?;
    Ok(Json(json!({"has_card": true, "card": card_json(pm)})))
}

/// Forgets the card. The Stripe customer is left alone — removing it there
/// would lose the account's billing history for the sake of a UI action.
async fn remove_card(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<Value>, ApiError> {
    let payment_ref = customer_ref(&state, acc.tenant()).await?;
    store::set_payment_method(&state.db, acc.tenant(), &payment_ref, "", "").await?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        assert_eq!(card_json(Some(pm))["last4"], json!("4242"));
    }
}

//! Cookie sessions and passwords.
//!
//! The cookie holds a random 192-bit token; the database holds its SHA-256.
//! Passwords are argon2id. Sign-in failures for a missing account and a wrong
//! password take the same path (a hash is always computed) and return the
//! same message, so the login form does not confirm which emails exist.

use anyhow::{bail, Result};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{FromRequestParts, State};
use axum::http::{header, request::Parts, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use super::{bad_request, ApiError, App};
use crate::store::{self, Account};

/// The session cookie's name. Written as `huntwell_session`.
pub const COOKIE: &str = "huntwell_session";
const SESSION_TTL_HOURS: i64 = 24 * 30;

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hash password: {e}"))?
        .to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
        .unwrap_or(false)
}

pub fn validate_signup(email: &str, password: &str) -> Result<()> {
    let email = email.trim();
    if email.len() < 3 || !email.contains('@') || email.contains(char::is_whitespace) || email.len() > 320 {
        bail!("a valid email address is required");
    }
    if password.chars().count() < 10 {
        bail!("password must be at least 10 characters");
    }
    if password.len() > 512 {
        bail!("password must be under 512 characters");
    }
    Ok(())
}

fn new_token() -> String {
    let mut b = [0u8; 24];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut b);
    hex::encode(b)
}

fn cookie_value(parts: &Parts) -> Option<String> {
    let raw = parts.headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == COOKIE).then(|| v.trim().to_string())
    })
}

fn set_cookie(token: &str, dev: bool, max_age: i64) -> HeaderValue {
    let secure = if dev { "" } else { "; Secure" };
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .unwrap()
}

/// The signed-in account, or 401. Handlers take this as an argument.
pub struct AuthUser(pub Account);

/// The internal header the gateway's forward-auth injects after it has resolved
/// the session cookie. A split service (plans/prospects/runs) has no `Session`
/// or `Account` table of its own, so it trusts this — safe because the gateway
/// strips any client-supplied copy and NetworkPolicy makes the service
/// reachable only from the gateway.
pub const ACCOUNT_HEADER: &str = "x-account-id";

/// A stand-in `Account` carrying only the id, for services that authenticate by
/// the gateway header and never touch the `Account` row. Handlers that need the
/// real profile (me/update_me/change_password) run only in the auth service and
/// the all-in-one `serve`, both of which take the cookie path below.
/// A stand-in for the gateway path, where the header already carries the
/// resolved tenant — so this account *is* its own workspace by construction.
fn account_stub(account_id: i64) -> Account {
    Account {
        account_id,
        email: String::new(),
        display_name: String::new(),
        password_hash: String::new(),
        cognito_sub: String::new(),
        timezone: "UTC".into(),
        timezone_auto: true,
        theme: "system".into(),
        created_at: chrono::Utc::now(),
        // The stub never renders the setup dialog: only the auth service reads
        // the real row, and it is the one that answers /auth/me.
        onboarded_at: None,
        active_workspace_id: None,
        workspace_name: String::new(),
        enabled_kinds: String::new(),
        platform_ack_at: None,
        platform_ack_by: None,
        platform_ack_text: String::new(),
        // False, not "unknown": a stub is built without reading the row, and a
        // permission that defaults to granted when nobody looked is how a
        // feature switched off for a workspace stays on for it anyway. The run
        // path re-reads the column from the database regardless.
        connected_logins: false,
    }
}

impl FromRequestParts<App> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &App) -> Result<Self, Self::Rejection> {
        if crate::config::trust_header_auth() {
            let id = parts
                .headers
                .get(ACCOUNT_HEADER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<i64>().ok());
            return match id {
                Some(id) if id > 0 => Ok(AuthUser(account_stub(id))),
                _ => Err(ApiError(StatusCode::UNAUTHORIZED, "sign in required".into())),
            };
        }
        let Some(token) = cookie_value(parts) else {
            return Err(ApiError(StatusCode::UNAUTHORIZED, "sign in required".into()));
        };
        let hash = store::sha256_hex(&token);
        match store::session_account(&state.db, &hash).await {
            Ok(Some(mut acc)) => {
                // A workspace they were switched into may have been taken away
                // since. Checking it here — once, at the edge — is what lets
                // every handler downstream trust `tenant()` without knowing
                // teams exist.
                if let Some(w) = acc.active_workspace_id {
                    let ok = store::workspace_role(&state.db, w, acc.account_id)
                        .await
                        .map(|r| r.is_some())
                        .unwrap_or(false);
                    if !ok {
                        let _ = store::set_active_workspace(&state.db, acc.account_id, None).await;
                        acc.active_workspace_id = None;
                    }
                }
                Ok(AuthUser(acc))
            }
            Ok(None) => Err(ApiError(StatusCode::UNAUTHORIZED, "session expired — sign in again".into())),
            Err(e) => Err(anyhow::Error::from(e).into()),
        }
    }
}

/// Forward-auth endpoint for the gateway. Resolves the session cookie and, on
/// success, answers 200 with `X-Account-Id` set — which Traefik copies onto the
/// upstream request. Any other outcome is 401 and the request never reaches a
/// service. Always uses the cookie path (this runs in the auth service, which
/// owns the `Session`/`Account` tables).
pub async fn introspect(State(state): State<App>, parts: axum::http::request::Parts) -> Response {
    let Some(token) = cookie_value(&parts) else {
        return (StatusCode::UNAUTHORIZED, "sign in required").into_response();
    };
    let hash = store::sha256_hex(&token);
    match store::session_account(&state.db, &hash).await {
        // The header is what every service scopes its queries by, so it must
        // carry the workspace being worked in, not the person working.
        Ok(Some(acc)) => (
            StatusCode::OK,
            [(ACCOUNT_HEADER, HeaderValue::from_str(&acc.tenant().to_string()).unwrap())],
        )
            .into_response(),
        Ok(None) => (StatusCode::UNAUTHORIZED, "session expired").into_response(),
        Err(e) => {
            tracing::error!("introspect: {e:#}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct SignupBody {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub display_name: String,
}

#[derive(Deserialize)]
pub struct LoginBody {
    pub email: String,
    pub password: String,
}

fn user_agent(headers: &axum::http::HeaderMap) -> String {
    headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
}

async fn start_session(state: &App, account_id: i64, ua: &str) -> Result<HeaderValue, ApiError> {
    let token = new_token();
    store::create_session(&state.db, account_id, &store::sha256_hex(&token), SESSION_TTL_HOURS, ua).await?;
    Ok(set_cookie(&token, state.dev, SESSION_TTL_HOURS * 3600))
}

pub async fn signup(State(state): State<App>, headers: axum::http::HeaderMap, Json(body): Json<SignupBody>) -> Result<Response, ApiError> {
    if !state.open_signup && store::account_count(&state.db).await? > 0 {
        return Err(ApiError(StatusCode::FORBIDDEN, "sign-up is closed on this server".into()));
    }
    validate_signup(&body.email, &body.password).map_err(|e| bad_request(e.to_string()))?;
    // The identity store owns the password. Under `cognito` the pool creates
    // the user and this row gets a subject and no hash; under `local` the
    // reverse. Either way the account row is written only once the credential
    // exists, so a failure there cannot leave an account nobody can sign in to.
    let id = crate::identity::create_user(&body.email, &body.password)
        .await
        .map_err(|e| bad_request(e.to_string()))?;
    let name = if body.display_name.trim().is_empty() {
        body.email.split('@').next().unwrap_or("").to_string()
    } else {
        body.display_name.clone()
    };
    let acc = store::create_account(&state.db, &body.email, &name, &id).await?;
    let cookie = start_session(&state, acc.account_id, &user_agent(&headers)).await?;
    Ok(([(header::SET_COOKIE, cookie)], Json(me_json(&acc, &state))).into_response())
}

pub async fn login(State(state): State<App>, headers: axum::http::HeaderMap, Json(body): Json<LoginBody>) -> Result<Response, ApiError> {
    let found = store::find_account_by_email(&state.db, &body.email).await?;
    // Checked even when no account exists, so a missing address costs the same
    // time as a wrong password — under `local` that is a hash comparison, and
    // under `cognito` the pool is asked either way.
    let hash = found.as_ref().map(|a| a.password_hash.clone()).unwrap_or_default();
    let email = body.email.clone();
    let password = body.password.clone();
    let outcome = if crate::identity::is_cognito() {
        crate::identity::authenticate(&email, &password, &hash).await
    } else {
        // argon2 is deliberately slow; off the runtime thread so one sign-in
        // does not stall every other request.
        tokio::task::spawn_blocking(move || {
            if crate::identity::verify_local_password(&password, &hash) {
                Ok(String::new())
            } else {
                Err(anyhow::anyhow!("invalid email or password"))
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!(e))?
    };
    let (Some(acc), Ok(sub)) = (found, outcome) else {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "email or password is incorrect".into()));
    };
    // First sign-in after a migration to Cognito: the pool knows this person,
    // the row does not know its subject yet. Recorded here rather than by a
    // migration, because only a successful sign-in proves the two are the same
    // account.
    if !sub.is_empty() && acc.cognito_sub != sub {
        store::set_cognito_sub(&state.db, acc.account_id, &sub).await?;
    }
    let cookie = start_session(&state, acc.account_id, &user_agent(&headers)).await?;
    Ok(([(header::SET_COOKIE, cookie)], Json(me_json(&acc, &state))).into_response())
}

pub async fn logout(State(state): State<App>, parts: axum::http::request::Parts) -> Result<Response, ApiError> {
    if let Some(token) = cookie_value(&parts) {
        let _ = store::delete_session(&state.db, &store::sha256_hex(&token)).await;
    }
    Ok(([(header::SET_COOKIE, set_cookie("", state.dev, 0))], Json(serde_json::json!({"ok": true}))).into_response())
}

pub fn me_json(acc: &Account, state: &App) -> serde_json::Value {
    serde_json::json!({
        "account_id": acc.account_id,
        "email": acc.email,
        "display_name": acc.display_name,
        "timezone": acc.timezone,
        "timezone_auto": acc.timezone_auto,
        "theme": acc.theme,
        "created_at": acc.created_at.to_rfc3339(),
        "onboarded": acc.onboarded_at.is_some(),
        // Which workspace this session is working in — their own unless they
        // switched into one they were invited to.
        "workspace_id": acc.tenant(),
        "own_workspace": acc.in_own_workspace(),
        "open_signup": state.open_signup,
    })
}

pub async fn me(State(state): State<App>, AuthUser(acc): AuthUser) -> Json<serde_json::Value> {
    // The kinds this workspace may build ride along with the session, so the
    // app can offer exactly what the API will accept.
    let kinds = store::allowed_kinds(&state.db, acc.tenant()).await;
    // The platform claim belongs to the workspace being worked in, which is not
    // this person's own account when they are working in someone else's.
    let ws = store::get_account(&state.db, acc.tenant()).await.ok().flatten();
    let mut v = me_json(&acc, &state);
    if let Some(o) = v.as_object_mut() {
        o.insert("kinds".into(), serde_json::json!(kinds));
        o.insert("platform_ack".into(), serde_json::json!(ws.as_ref().is_some_and(|a| a.platform_ack_at.is_some())));
        o.insert(
            "platform_ack_at".into(),
            serde_json::json!(ws.as_ref().and_then(|a| a.platform_ack_at).map(|t| t.to_rfc3339())),
        );
    }
    Json(v)
}

#[derive(Deserialize)]
pub struct PrefsBody {
    #[serde(default)]
    pub display_name: Option<String>,
    /// A deliberate choice, from Settings. Picking one turns the browser sync
    /// off, otherwise the next page load would undo it.
    #[serde(default)]
    pub timezone: Option<String>,
    /// The zone this browser is in, sent passively on every load. Applied only
    /// while the account is still on automatic.
    #[serde(default)]
    pub browser_timezone: Option<String>,
    /// Back to following the browser.
    #[serde(default)]
    pub timezone_auto: Option<bool>,
    /// Claiming (or withdrawing) the right to collect from the restricted
    /// platforms. Recorded with a timestamp, since when matters in a dispute.
    #[serde(default)]
    pub platform_ack: Option<bool>,
    #[serde(default)]
    pub theme: Option<String>,
    /// Set by the first-run setup dialog when it is submitted, so saving the
    /// profile and marking setup done is one request.
    #[serde(default)]
    pub onboarded: bool,
}

pub async fn update_me(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<PrefsBody>) -> Result<Json<serde_json::Value>, ApiError> {
    // Three ways the zone can move, in precedence order: a manual pick (which
    // also pins it), an explicit switch back to automatic, and the passive
    // browser report, which only counts while automatic is on.
    let mut tz = acc.timezone.clone();
    let mut tz_auto = acc.timezone_auto;
    if let Some(z) = body.timezone {
        tz = z;
        tz_auto = false;
    }
    if let Some(a) = body.timezone_auto {
        tz_auto = a;
    }
    if let Some(z) = body.browser_timezone {
        if tz_auto {
            tz = z;
        }
    }
    if tz.parse::<chrono_tz::Tz>().is_err() {
        return Err(bad_request(format!("unknown timezone {tz:?}")));
    }
    let theme = body.theme.unwrap_or(acc.theme.clone());
    if !["light", "dark", "system"].contains(&theme.as_str()) {
        return Err(bad_request("theme must be light, dark or system"));
    }
    let name = body.display_name.unwrap_or(acc.display_name.clone());
    store::update_account_prefs(&state.db, acc.account_id, &name, &tz, tz_auto, &theme).await?;
    if tz != acc.timezone {
        // A schedule is local wall-clock; a new zone moves every NextRunAt.
        let plans = store::list_plans(&state.db, acc.account_id).await?;
        for p in plans {
            let _ = store::refresh_schedule(&state.db, p.plan.plan_id).await;
        }
    }
    if let Some(on) = body.platform_ack {
        // The workspace holds the claim; the person who clicked is recorded on it.
        store::set_platform_ack(&state.db, acc.tenant(), acc.account_id, on).await?;
    }
    if body.onboarded {
        store::mark_onboarded(&state.db, acc.account_id).await?;
    }
    let acc = store::get_account(&state.db, acc.account_id).await?.ok_or_else(|| anyhow::anyhow!("account not found"))?;
    Ok(Json(me_json(&acc, &state)))
}

#[derive(Deserialize)]
pub struct PasswordBody {
    pub current: String,
    pub new: String,
}

pub async fn change_password(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<PasswordBody>) -> Result<Json<serde_json::Value>, ApiError> {
    // Proving the current password is the identity store's job too: under
    // Cognito the row holds no hash to check it against.
    crate::identity::authenticate(&acc.email, &body.current, &acc.password_hash)
        .await
        .map_err(|_| ApiError(StatusCode::UNAUTHORIZED, "current password is incorrect".into()))?;
    validate_signup(&acc.email, &body.new).map_err(|e| bad_request(e.to_string()))?;
    let hash = crate::identity::set_password(&acc.email, &body.new)
        .await
        .map_err(|e| bad_request(e.to_string()))?;
    // Empty under Cognito, which is the point: the row stops carrying one.
    store::update_password(&state.db, acc.account_id, &hash).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_round_trip() {
        let h = hash_password("correct horse battery").unwrap();
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("x", "not a hash"));
    }

    #[test]
    fn signup_validation() {
        assert!(validate_signup("a@b.co", "longenoughpassword").is_ok());
        assert!(validate_signup("nope", "longenoughpassword").is_err());
        assert!(validate_signup("a@b.co", "short").is_err());
    }
}

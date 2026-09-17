//! Cookie sessions and passwords.
//!
//! The cookie holds a random 192-bit token; the database holds its SHA-256.
//! Passwords are argon2id. Sign-in failures for a missing account and a wrong
//! password take the same path (a hash is always computed) and return the
//! same message, so the login form does not confirm which emails exist.

use anyhow::{bail, Result};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{ConnectInfo, FromRequestParts, State};
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

/// Display names are shown to teammates, in emails and in the operator
/// console, so they are bounded and single-line.
pub const DISPLAY_NAME_MAX: usize = 80;

pub fn validate_display_name(name: &str) -> Result<()> {
    if name.chars().count() > DISPLAY_NAME_MAX {
        bail!("a name must be {DISPLAY_NAME_MAX} characters or fewer");
    }
    if name.chars().any(|c| c.is_control()) {
        bail!("a name cannot contain line breaks or control characters");
    }
    Ok(())
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

/// The routes an unverified account may still call. Suffix-matched: under
/// `Router::nest` a handler sees the path without its `/api` prefix.
fn verification_exempt(path: &str) -> bool {
    ["/auth/me", "/auth/logout", "/auth/resend", "/auth/verify"].iter().any(|p| path.ends_with(p))
}

/// The signed-in account, or 401. Handlers take this as an argument.
pub struct AuthUser(pub Account);

impl FromRequestParts<App> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &App) -> Result<Self, Self::Rejection> {
        let Some(token) = cookie_value(parts) else {
            return Err(ApiError(StatusCode::UNAUTHORIZED, "sign in required".into()));
        };
        let hash = store::sha256_hex(&token);
        match store::session_account(&state.db, &hash).await {
            Ok(Some(mut acc)) => {
                // Until the address is proven theirs the account can do
                // nothing but ask for the link again and leave. That is what
                // makes an invitation bound to an address mean something.
                if acc.email_verified_at.is_none() && !verification_exempt(parts.uri.path()) {
                    return Err(ApiError(StatusCode::FORBIDDEN, "verify your email address to continue".into()));
                }
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

#[derive(Deserialize)]
pub struct SignupBody {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub display_name: String,
    /// From the Turnstile widget. Required when the check is configured.
    #[serde(default)]
    pub turnstile_token: String,
}

#[derive(Deserialize)]
pub struct LoginBody {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub turnstile_token: String,
}

fn too_many(wait: u64) -> ApiError {
    ApiError(StatusCode::TOO_MANY_REQUESTS, format!("too many attempts — try again in {} minutes", wait.div_ceil(60).max(1)))
}

fn challenge_failed(e: anyhow::Error) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, e.to_string())
}

/// What the sign-in and sign-up pages need before anyone is signed in.
pub async fn config(State(state): State<App>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "open_signup": state.open_signup,
        // The site key only when tokens will be checked: a widget nobody
        // verifies is friction with nothing behind it.
        "turnstile_site_key": if crate::turnstile::configured() { crate::turnstile::site_key() } else { None },
    }))
}

fn user_agent(headers: &axum::http::HeaderMap) -> String {
    headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
}

async fn start_session(state: &App, account_id: i64, ua: &str) -> Result<HeaderValue, ApiError> {
    let token = new_token();
    store::create_session(&state.db, account_id, &store::sha256_hex(&token), SESSION_TTL_HOURS, ua).await?;
    Ok(set_cookie(&token, state.dev, SESSION_TTL_HOURS * 3600))
}

pub async fn signup(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<SignupBody>,
) -> Result<Response, ApiError> {
    if !state.open_signup && store::account_count(&state.db).await? > 0 {
        return Err(ApiError(StatusCode::FORBIDDEN, "sign-up is closed on this server".into()));
    }
    let ip = super::client_ip(&headers, peer);
    crate::throttle::SIGNUPS.hit(&ip.to_string()).map_err(too_many)?;
    crate::turnstile::verify(&body.turnstile_token, ip).await.map_err(challenge_failed)?;
    validate_signup(&body.email, &body.password).map_err(|e| bad_request(e.to_string()))?;
    validate_display_name(&body.display_name).map_err(|e| bad_request(e.to_string()))?;
    let name = if body.display_name.trim().is_empty() {
        body.email.split('@').next().unwrap_or("").to_string()
    } else {
        body.display_name.clone()
    };
    // An address already registered but never verified is not anyone's yet:
    // whoever proves it owns it. The old row is taken over rather than left
    // squatting on the address, and its identity (a pool user, under Cognito)
    // is replaced with this one.
    if let Some(existing) = store::find_account_by_email(&state.db, &body.email).await? {
        if existing.email_verified_at.is_some() {
            return Err(bad_request("an account with that email already exists"));
        }
        if let Err(e) = crate::identity::delete_user(&body.email).await {
            tracing::warn!("could not replace the identity for {}: {e:#}", body.email);
        }
        let id = crate::identity::create_user(&body.email, &body.password).await.map_err(|e| bad_request(e.to_string()))?;
        let acc = store::reclaim_unverified_account(&state.db, existing.account_id, &name, &id).await?;
        begin_verification(&state, &acc).await?;
        let cookie = start_session(&state, acc.account_id, &user_agent(&headers)).await?;
        return Ok(([(header::SET_COOKIE, cookie)], Json(me_json(&acc, &state))).into_response());
    }
    // The identity store owns the password. Under `cognito` the pool creates
    // the user and this row gets a subject and no hash; under `local` the
    // reverse. Either way the account row is written only once the credential
    // exists, so a failure there cannot leave an account nobody can sign in to.
    let id = crate::identity::create_user(&body.email, &body.password)
        .await
        .map_err(|e| bad_request(e.to_string()))?;
    let acc = match store::create_account(&state.db, &body.email, &name, &id).await {
        Ok(acc) => acc,
        Err(e) => {
            // The credential exists and the row does not: take the credential
            // back, or the pool keeps a user no account can ever reach.
            if let Err(e2) = crate::identity::delete_user(&body.email).await {
                tracing::warn!("could not remove the identity for {}: {e2:#}", body.email);
            }
            return Err(e.into());
        }
    };
    begin_verification(&state, &acc).await?;
    let cookie = start_session(&state, acc.account_id, &user_agent(&headers)).await?;
    let acc = store::get_account(&state.db, acc.account_id).await?.unwrap_or(acc);
    Ok(([(header::SET_COOKIE, cookie)], Json(me_json(&acc, &state))).into_response())
}

pub async fn login(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<LoginBody>,
) -> Result<Response, ApiError> {
    let ip = super::client_ip(&headers, peer);
    crate::throttle::LOGIN_FAILURES.check(&ip.to_string()).map_err(too_many)?;
    crate::turnstile::verify(&body.turnstile_token, ip).await.map_err(challenge_failed)?;
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
        crate::throttle::LOGIN_FAILURES.note(&ip.to_string());
        return Err(ApiError(StatusCode::UNAUTHORIZED, "email or password is incorrect".into()));
    };
    // First sign-in after a migration to Cognito: the pool knows this person,
    // the row does not know its subject yet. Recorded here rather than by a
    // migration, because only a successful sign-in proves the two are the same
    // account. Once bound, a *different* subject for the same address is not
    // the same person — it is a pool user someone else made with this email —
    // and gets no session, however good its password was.
    if !sub.is_empty() && acc.cognito_sub != sub {
        if acc.cognito_sub.is_empty() {
            store::set_cognito_sub(&state.db, acc.account_id, &sub).await?;
        } else {
            tracing::warn!("sign-in for account {} presented Cognito subject {sub}, not the bound one — refused", acc.account_id);
            crate::throttle::LOGIN_FAILURES.note(&ip.to_string());
            return Err(ApiError(StatusCode::UNAUTHORIZED, "email or password is incorrect".into()));
        }
    }
    let cookie = start_session(&state, acc.account_id, &user_agent(&headers)).await?;
    Ok(([(header::SET_COOKIE, cookie)], Json(me_json(&acc, &state))).into_response())
}

/// Email a fresh confirmation code — or, on a server with no way to send
/// email (a dev box), verify on the spot and say so in the log.
async fn begin_verification(state: &App, acc: &Account) -> Result<(), ApiError> {
    if acc.email_verified_at.is_some() {
        return Ok(());
    }
    if !crate::mail::configured() {
        tracing::warn!("no mail provider — {} is verified without an email", acc.email);
        store::mark_email_verified(&state.db, acc.account_id).await?;
        return Ok(());
    }
    let code = new_code();
    store::set_verify_token(&state.db, acc.account_id, &store::sha256_hex(&code)).await?;
    let msg = crate::mail::verification(&acc.display_name, &code);
    store::queue_mail(&state.db, Some(acc.account_id), &acc.email, "verify", &msg).await?;
    Ok(())
}

/// Six digits from the OS's randomness, zero-padded — 000123 is a code too.
fn new_code() -> String {
    use rand::RngCore;
    let mut b = [0u8; 4];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!("{:06}", u32::from_le_bytes(b) % 1_000_000)
}

#[derive(Deserialize)]
pub struct VerifyBody {
    pub code: String,
}

/// Type in the code from the email. For the signed-in account only: a code
/// proves the address of whoever is entering it, not of anyone in general.
pub async fn verify(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<VerifyBody>) -> Result<Json<serde_json::Value>, ApiError> {
    // Digits only — a code pasted as "482 913" is still the code.
    let code: String = body.code.chars().filter(|c| c.is_ascii_digit()).collect();
    if code.len() != 6 {
        return Err(bad_request("the code is six digits"));
    }
    match store::verify_email_code(&state.db, acc.account_id, &store::sha256_hex(&code)).await? {
        store::CodeCheck::Verified(acc) => {
            if let Some(base) = crate::config::get("HUNTWELL_PUBLIC_URL").filter(|v| !v.trim().is_empty()) {
                let app = format!("{}/app", base.trim().trim_end_matches('/'));
                let _ = store::queue_mail(&state.db, Some(acc.account_id), &acc.email, "welcome", &crate::mail::welcome(&acc.display_name, &app)).await;
            }
            Ok(Json(me_json(&acc, &state)))
        }
        store::CodeCheck::Wrong { left } => Err(bad_request(format!(
            "that is not the code — {left} {} left before you need a new one",
            if left == 1 { "try" } else { "tries" }
        ))),
        store::CodeCheck::NeedNew => Err(ApiError(
            StatusCode::GONE,
            "that code has expired or been used up — send a new one".into(),
        )),
    }
}

/// Send a new code. Bounded: each one is an email, and each one is six fresh
/// digits with five guesses.
pub async fn resend(State(state): State<App>, AuthUser(acc): AuthUser) -> Result<Json<serde_json::Value>, ApiError> {
    if acc.email_verified_at.is_some() {
        return Ok(Json(serde_json::json!({ "ok": true, "verified": true })));
    }
    crate::throttle::VERIFY_RESENDS.hit(&acc.account_id.to_string()).map_err(too_many)?;
    begin_verification(&state, &acc).await?;
    Ok(Json(serde_json::json!({ "ok": true, "verified": false })))
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
        "email_verified": acc.email_verified_at.is_some(),
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
    validate_display_name(&name).map_err(|e| bad_request(e.to_string()))?;
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

pub async fn change_password(
    State(state): State<App>,
    AuthUser(acc): AuthUser,
    parts: Parts,
    Json(body): Json<PasswordBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
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
    // Every other session ends. Changing the password is what someone does
    // when they think another device has it, and this is what makes that work.
    let keep = cookie_value(&parts).map(|t| store::sha256_hex(&t)).unwrap_or_default();
    store::delete_other_sessions(&state.db, acc.account_id, &keep).await?;
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
    fn an_unverified_account_can_only_leave_or_ask_again() {
        for p in ["/auth/me", "/api/auth/me", "/auth/logout", "/auth/resend", "/auth/verify"] {
            assert!(verification_exempt(p), "{p}");
        }
        for p in ["/plans", "/team/join/abc", "/auth/password", "/keys", "/auth/me/x"] {
            assert!(!verification_exempt(p), "{p}");
        }
    }

    #[test]
    fn display_names_are_bounded_and_single_line() {
        assert!(validate_display_name("Ada Lovelace").is_ok());
        assert!(validate_display_name("").is_ok());
        assert!(validate_display_name(&"x".repeat(81)).is_err());
        assert!(validate_display_name("two\nlines").is_err());
    }

    #[test]
    fn signup_validation() {
        assert!(validate_signup("a@b.co", "longenoughpassword").is_ok());
        assert!(validate_signup("nope", "longenoughpassword").is_err());
        assert!(validate_signup("a@b.co", "short").is_err());
    }
}

//! Cookie sessions. Passwords live in Cognito (`identity`), never here.
//!
//! The cookie holds a random 192-bit token; the database holds its SHA-256.
//! Sign-in failures for a missing account and a wrong password take the same
//! path (the pool is asked either way) and return the same message, so the
//! login form does not confirm which emails exist.

use anyhow::{bail, Result};
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
    /// The token from an invitation link. Required while sign-up is
    /// invite-only; otherwise it only pre-fills the form.
    #[serde(default)]
    pub invite: String,
}

#[derive(Deserialize)]
pub struct WaitlistBody {
    pub email: String,
    #[serde(default)]
    pub name: String,
    /// What they want Huntwell for, in their words.
    #[serde(default)]
    pub note: String,
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

/// An identity-store error as the caller sees it: a validation failure is a
/// 400 with its reason; the store itself failing is a 503 with a sentence
/// that says nothing about why (the log has that).
fn identity_error(e: anyhow::Error) -> ApiError {
    let msg = e.to_string();
    if msg == crate::cognito::UNAVAILABLE {
        ApiError(StatusCode::SERVICE_UNAVAILABLE, msg)
    } else {
        bad_request(msg)
    }
}

/// Whether a new account needs an invitation. The very first account never
/// does — someone has to be able to create the operator's own workspace.
async fn invite_only(state: &App) -> Result<bool, ApiError> {
    Ok(!state.open_signup && store::account_count(&state.db).await? > 0)
}

/// What an invitation link is good for, once it has been checked.
enum Invitation {
    /// From the admin: sent to this address by us, so holding the link is
    /// proof of the address and the emailed code can be skipped.
    Platform { email: String, name: String },
    /// A teammate's invitation to their workspace. Also a way in while
    /// sign-up is closed — a team that could not add anyone would be broken —
    /// but its link may have been copied into a chat, so it proves nothing
    /// about the address and the code is still sent.
    Team { email: String, workspace: String },
}

impl Invitation {
    fn email(&self) -> &str {
        match self {
            Invitation::Platform { email, .. } | Invitation::Team { email, .. } => email,
        }
    }
}

async fn find_invitation(state: &App, token: &str) -> Result<Option<Invitation>, ApiError> {
    let token = token.trim();
    if token.is_empty() || token.len() > 128 {
        return Ok(None);
    }
    if let Some((email, name)) = store::signup_invite(&state.db, &store::sha256_hex(token)).await? {
        return Ok(Some(Invitation::Platform { email, name }));
    }
    if let Some((_, email, _, workspace)) = store::invite_by_token(&state.db, token).await? {
        return Ok(Some(Invitation::Team { email, workspace }));
    }
    Ok(None)
}

/// What an invitation link says, for pre-filling the sign-up form.
pub async fn invitation(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // A miss is a guess at a 256-bit token; counted with sign-in failures so a
    // loop of guesses runs into the same wall.
    let ip = super::client_ip(&headers, peer);
    crate::throttle::LOGIN_FAILURES.check(&ip.to_string()).map_err(too_many)?;
    match find_invitation(&state, &token).await? {
        Some(Invitation::Platform { email, name }) => Ok(Json(serde_json::json!({ "email": email, "name": name, "kind": "platform" }))),
        Some(Invitation::Team { email, workspace }) => Ok(Json(serde_json::json!({ "email": email, "workspace": workspace, "kind": "team" }))),
        None => {
            crate::throttle::LOGIN_FAILURES.note(&ip.to_string());
            Err(ApiError(StatusCode::NOT_FOUND, "this invitation has expired or has already been used".into()))
        }
    }
}

/// Ask to be let in, while sign-up is invite-only.
///
/// Always answers the same way whatever the address — already waiting,
/// already a customer, declined — so the form cannot be used to find out who
/// has an account.
pub async fn join_waitlist(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<WaitlistBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !invite_only(&state).await? {
        return Err(ApiError(StatusCode::CONFLICT, "sign-up is open — you can create an account straight away".into()));
    }
    let ip = super::client_ip(&headers, peer);
    crate::throttle::WAITLIST_REQUESTS.hit(&ip.to_string()).map_err(too_many)?;
    crate::turnstile::verify(&body.turnstile_token, ip).await.map_err(challenge_failed)?;
    let email = body.email.trim().to_lowercase();
    if email.len() > 320 || !email.contains('@') || email.starts_with('@') || email.ends_with('@') {
        return Err(bad_request("enter a valid email address"));
    }
    validate_display_name(&body.name).map_err(|e| bad_request(e.to_string()))?;
    let note: String = body.note.trim().chars().take(600).collect();
    let added = store::join_waitlist(&state.db, &email, &body.name, &note, &ip.to_string()).await?;
    if added {
        let msg = crate::mail::waitlist_received(&body.name);
        if let Err(e) = store::queue_mail(&state.db, None, &email, "waitlist", &msg).await {
            tracing::warn!("could not queue the waitlist note to {email}: {e:#}");
        }
        tracing::info!("waitlist: {email} asked to join");
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// What the sign-in and sign-up pages need before anyone is signed in.
pub async fn config(State(state): State<App>) -> Json<serde_json::Value> {
    let invite_only = invite_only(&state).await.unwrap_or(!state.open_signup);
    Json(serde_json::json!({
        "open_signup": !invite_only,
        "invite_only": invite_only,
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
    let ip = super::client_ip(&headers, peer);
    crate::throttle::SIGNUPS.check(&ip.to_string()).map_err(too_many)?;
    crate::turnstile::verify(&body.turnstile_token, ip).await.map_err(challenge_failed)?;
    // One spelling of the address for everything that follows — the row, the
    // pool user, the silent sign-in after it. The pool user used to be made
    // with the address as typed while later calls used the stored lower-case
    // one, so a capital letter at sign-up broke whatever came next.
    let mut body = body;
    body.email = body.email.trim().to_lowercase();
    // Invite-only: no invitation, no account. An invitation is for one
    // address, so a forwarded link cannot be spent on another.
    let invitation = if body.invite.trim().is_empty() { None } else { find_invitation(&state, &body.invite).await? };
    if !body.invite.trim().is_empty() && invitation.is_none() {
        return Err(ApiError(StatusCode::GONE, "this invitation has expired or has already been used".into()));
    }
    if let Some(inv) = &invitation {
        if inv.email() != body.email {
            return Err(ApiError(StatusCode::FORBIDDEN, format!("this invitation is for {} — sign up with that address", inv.email())));
        }
    }
    if invitation.is_none() && invite_only(&state).await? {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Huntwell is invite-only right now — request an invitation and we'll email you if we can make room".into(),
        ));
    }
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
        // Only an empty one. A row made outside sign-up (the CLI, the
        // operator endpoint) is unverified too, and may already hold plans
        // and keys — its owner signs in or resets the password instead.
        if existing.email_verified_at.is_some() || store::account_holds_anything(&state.db, existing.account_id).await? {
            return Err(bad_request("an account with that email already exists"));
        }
        // Before anything is deleted: a password the pool would refuse must
        // not cost the address its current identity.
        crate::identity::check_password_strength(&body.password).map_err(|e| bad_request(e.to_string()))?;
        if let Err(e) = crate::identity::delete_user(&body.email).await {
            tracing::warn!("could not replace the identity for {}: {e:#}", body.email);
        }
        let sub = crate::identity::create_user(&body.email, &body.password).await.map_err(identity_error)?;
        let acc = store::reclaim_unverified_account(&state.db, existing.account_id, &name, &sub).await?;
        crate::throttle::SIGNUPS.note(&ip.to_string());
        let acc = spend_invitation(&state, invitation.as_ref(), acc).await?;
        begin_verification(&state, &acc).await?;
        prime_enrolment(&acc, &body.password).await;
        let cookie = start_session(&state, acc.account_id, &user_agent(&headers)).await?;
        return Ok(([(header::SET_COOKIE, cookie)], Json(me_json(&acc, &state))).into_response());
    }
    // The pool owns the password: the user is created there first, and the
    // account row is written only once that credential exists, so a failure
    // cannot leave an account nobody can sign in to.
    let sub = crate::identity::create_user(&body.email, &body.password).await.map_err(identity_error)?;
    let acc = match store::create_account(&state.db, &body.email, &name, &sub).await {
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
    crate::throttle::SIGNUPS.note(&ip.to_string());
    let acc = spend_invitation(&state, invitation.as_ref(), acc).await?;
    begin_verification(&state, &acc).await?;
    prime_enrolment(&acc, &body.password).await;
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
    let mut body = body;
    body.email = body.email.trim().to_lowercase();
    let found = store::find_account_by_email(&state.db, &body.email).await?;
    // The pool is asked even when no row exists, so a missing address costs
    // the same time as a wrong password.
    let account_id = found.as_ref().map(|a| a.account_id).unwrap_or(0);
    let outcome = crate::identity::sign_in(&body.email, &body.password, account_id).await;
    // The pool being down or misconfigured is not a wrong password: it is not
    // counted as a guess, and the page says to try later rather than sending
    // the person off to reset a password that was fine.
    if let Err(e) = &outcome {
        if e.to_string() == crate::cognito::UNAVAILABLE {
            return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, e.to_string()));
        }
        tracing::info!("sign-in refused for account {account_id}: {e:#}");
    }
    let (Some(acc), Ok(outcome)) = (found, outcome) else {
        crate::throttle::LOGIN_FAILURES.note(&ip.to_string());
        return Err(ApiError(StatusCode::UNAUTHORIZED, "email or password is incorrect".into()));
    };
    // The password was right and a second factor is on: no session yet. The
    // browser comes back to `mfa` with the code and this opaque challenge.
    let (sub, access_token) = match outcome {
        crate::identity::SignIn::Done { sub, access_token } => (sub, access_token),
        crate::identity::SignIn::MfaRequired { challenge } => {
            return Ok(Json(serde_json::json!({ "mfa_required": true, "challenge": challenge, "email": acc.email })).into_response());
        }
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
    // Only now that the subject is known to be this account's.
    crate::identity::remember_access_token(acc.account_id, &access_token);
    let cookie = start_session(&state, acc.account_id, &user_agent(&headers)).await?;
    Ok(([(header::SET_COOKIE, cookie)], Json(me_json(&acc, &state))).into_response())
}

/// Use up the invitation an account was made with.
///
/// A platform invitation came to this address from us, so following its link
/// proves the address: the account is verified here and the emailed code is
/// skipped. A team invitation is left for the join page to accept, and proves
/// nothing, so the code is still sent.
async fn spend_invitation(state: &App, invitation: Option<&Invitation>, acc: Account) -> Result<Account, ApiError> {
    let Some(Invitation::Platform { .. }) = invitation else { return Ok(acc) };
    store::mark_waitlist_joined(&state.db, &acc.email, acc.account_id).await?;
    store::mark_email_verified(&state.db, acc.account_id).await?;
    tracing::info!("waitlist: {} joined by invitation", acc.email);
    Ok(store::get_account(&state.db, acc.account_id).await?.unwrap_or(acc))
}

/// Email a fresh confirmation code — or, on a server with no way to send
/// email (a dev box), verify on the spot and say so in the log.
async fn begin_verification(state: &App, acc: &Account) -> Result<(), ApiError> {
    if acc.email_verified_at.is_some() {
        return Ok(());
    }
    // Only off production: there, an address nobody proved would let anyone
    // claim any email — and with it every team invitation sent to it.
    if !crate::mail::configured() && !crate::config::is_production() {
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

/// Under Cognito, two-factor enrolment acts on an access token, and the
/// sign-up page offers enrolment right after the email code — so a token is
/// fetched now, while the password is in hand, and kept for an hour. Best
/// effort: without it, setup asks for the password again.
async fn prime_enrolment(acc: &Account, password: &str) {
    match crate::cognito::sign_in_full(&acc.email, password).await {
        Ok(crate::cognito::SignIn::Done { access_token, .. }) => {
            crate::identity::remember_access_token(acc.account_id, &access_token);
        }
        Ok(crate::cognito::SignIn::MfaRequired { .. }) => {}
        // Not fatal — two-factor setup will ask for the password instead —
        // but said, because a silent failure here is a confusing screen later.
        Err(e) => tracing::warn!("could not fetch an enrolment token for account {} after sign-up: {e:#}", acc.account_id),
    }
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

#[derive(Deserialize)]
pub struct MfaBody {
    pub email: String,
    pub challenge: String,
    pub code: String,
}

/// The second half of a sign-in: the code from the authenticator app.
pub async fn mfa(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<MfaBody>,
) -> Result<Response, ApiError> {
    let ip = super::client_ip(&headers, peer);
    crate::throttle::LOGIN_FAILURES.check(&ip.to_string()).map_err(too_many)?;
    // The account is the one whose password earned this challenge, recorded
    // when it was issued — never the address in the request, which the
    // browser can set to anyone's. `body.email` is kept only for old clients.
    let expired = || ApiError(StatusCode::UNAUTHORIZED, "that sign-in took too long — start again".into());
    let Some(account_id) = crate::identity::mfa_challenge_account(&body.challenge) else {
        crate::throttle::LOGIN_FAILURES.note(&ip.to_string());
        return Err(expired());
    };
    let acc = store::get_account(&state.db, account_id).await?.ok_or_else(expired)?;
    let (sub, access_token) = match crate::identity::answer_mfa(&body.challenge, &body.code).await {
        Ok(v) => v,
        Err(e) => {
            crate::throttle::LOGIN_FAILURES.note(&ip.to_string());
            return Err(identity_error(e));
        }
    };
    // The same rule as a password sign-in: a bound subject must match.
    if sub.is_empty() || (!acc.cognito_sub.is_empty() && acc.cognito_sub != sub) {
        tracing::warn!("two-factor sign-in for account {} presented Cognito subject {sub:?}, not the bound one — refused", acc.account_id);
        crate::throttle::LOGIN_FAILURES.note(&ip.to_string());
        return Err(expired());
    }
    if acc.cognito_sub.is_empty() {
        store::set_cognito_sub(&state.db, acc.account_id, &sub).await?;
    }
    crate::identity::remember_access_token(acc.account_id, &access_token);
    let cookie = start_session(&state, acc.account_id, &user_agent(&headers)).await?;
    Ok(([(header::SET_COOKIE, cookie)], Json(me_json(&acc, &state))).into_response())
}

#[derive(Deserialize)]
pub struct MfaSetupBody {
    /// Needed under Cognito when no recent sign-in is at hand (Settings, later).
    #[serde(default)]
    pub password: String,
}

/// Start two-factor setup: the secret as a QR and as text. Not on until
/// `mfa_confirm` proves the app has it.
pub async fn mfa_setup(State(_state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<MfaSetupBody>) -> Result<Json<serde_json::Value>, ApiError> {
    if acc.mfa_enabled_at.is_some() {
        return Err(bad_request("two-factor authentication is already on — turn it off first to set up a new device"));
    }
    let e = crate::identity::begin_mfa(&acc.email, acc.account_id, Some(&body.password)).await.map_err(|e| {
        // No token at hand and no password given: not a failure, a question.
        // 428 tells the page to show its password field and ask again.
        if e.to_string() == crate::identity::NEEDS_PASSWORD {
            ApiError(StatusCode::PRECONDITION_REQUIRED, e.to_string())
        } else if e.to_string().contains("invalid email or password") {
            ApiError(StatusCode::UNAUTHORIZED, "that password is not right".into())
        } else {
            identity_error(e)
        }
    })?;
    Ok(Json(serde_json::json!({ "secret": e.secret, "uri": e.uri, "qr_svg": e.qr_svg })))
}

#[derive(Deserialize)]
pub struct MfaCodeBody {
    pub code: String,
    #[serde(default)]
    pub password: String,
}

pub async fn mfa_confirm(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<MfaCodeBody>) -> Result<Json<serde_json::Value>, ApiError> {
    crate::identity::confirm_mfa(&acc.email, acc.account_id, &body.code, &state.db).await.map_err(identity_error)?;
    Ok(Json(serde_json::json!({ "ok": true, "mfa_enabled": true })))
}

/// Turn two-factor off: the password and a current code, so a stolen session
/// alone cannot strip the second factor from an account.
pub async fn mfa_disable(State(state): State<App>, AuthUser(acc): AuthUser, Json(body): Json<MfaCodeBody>) -> Result<Json<serde_json::Value>, ApiError> {
    if acc.mfa_enabled_at.is_none() {
        return Ok(Json(serde_json::json!({ "ok": true, "mfa_enabled": false })));
    }
    crate::identity::check_mfa_code(&acc.email, &body.password, &body.code)
        .await
        .map_err(|_| ApiError(StatusCode::UNAUTHORIZED, "password or code is incorrect".into()))?;
    crate::identity::disable_mfa(&acc.email, acc.account_id, &state.db).await.map_err(identity_error)?;
    Ok(Json(serde_json::json!({ "ok": true, "mfa_enabled": false })))
}

#[derive(Deserialize)]
pub struct ForgotBody {
    pub email: String,
    #[serde(default)]
    pub turnstile_token: String,
}

/// "Forgot password": email a reset code. Answers the same whether or not the
/// address has an account, so this form cannot be used to find out which
/// addresses do. Bounded per address like every other thing that sends mail.
pub async fn forgot(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ForgotBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ip = super::client_ip(&headers, peer);
    crate::throttle::RESET_REQUESTS.hit(&ip.to_string()).map_err(too_many)?;
    crate::turnstile::verify(&body.turnstile_token, ip).await.map_err(challenge_failed)?;
    let email = body.email.trim().to_lowercase();
    if !email.contains('@') || email.len() > 320 {
        return Err(bad_request("that does not look like an email address"));
    }
    if !crate::mail::configured() {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "this server cannot send email, so passwords cannot be reset here".into()));
    }
    let code = new_code();
    if let Some(acc) = store::set_reset_token(&state.db, &email, &store::sha256_hex(&code)).await? {
        let msg = crate::mail::password_reset(&acc.display_name, &code);
        store::queue_mail(&state.db, Some(acc.account_id), &acc.email, "reset", &msg).await?;
    } else {
        tracing::info!("password reset asked for {email}, which has no account — nothing sent");
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ResetBody {
    pub email: String,
    pub code: String,
    pub password: String,
}

/// Finish a reset: the code proves the address, the new password replaces the
/// old one in the identity store, and every existing session ends.
pub async fn reset(
    State(state): State<App>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ResetBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Guesses at the code are sign-in guesses by another name.
    let ip = super::client_ip(&headers, peer);
    crate::throttle::LOGIN_FAILURES.check(&ip.to_string()).map_err(too_many)?;
    let email = body.email.trim().to_lowercase();
    let code: String = body.code.chars().filter(|c| c.is_ascii_digit()).collect();
    if code.len() != 6 {
        return Err(bad_request("the code is six digits"));
    }
    validate_signup(&email, &body.password).map_err(|e| bad_request(e.to_string()))?;
    let acc = match store::check_reset_code(&state.db, &email, &store::sha256_hex(&code)).await? {
        store::CodeCheck::Verified(acc) => acc,
        store::CodeCheck::Wrong { left } => {
            crate::throttle::LOGIN_FAILURES.note(&ip.to_string());
            return Err(bad_request(format!(
                "that is not the code — {left} {} left before you need a new one",
                if left == 1 { "try" } else { "tries" }
            )));
        }
        store::CodeCheck::NeedNew => {
            crate::throttle::LOGIN_FAILURES.note(&ip.to_string());
            return Err(ApiError(StatusCode::GONE, "that code has expired or been used up — ask for a new one".into()));
        }
    };
    crate::identity::set_password(&acc.email, &body.password).await.map_err(identity_error)?;
    let ended = store::delete_all_sessions(&state.db, acc.account_id).await?;
    tracing::info!("password reset for account {} — {ended} session(s) ended", acc.account_id);
    Ok(Json(serde_json::json!({ "ok": true })))
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
        "mfa_enabled": acc.mfa_enabled_at.is_some(),
        "onboarded": acc.onboarded_at.is_some(),
        // Which workspace this session is working in — their own unless they
        // switched into one they were invited to.
        "workspace_id": acc.tenant(),
        "own_workspace": acc.in_own_workspace(),
        "open_signup": state.open_signup,
        "invite_only": !state.open_signup,
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
    let caps = store::workspace_caps(&state.db, acc.tenant(), acc.account_id).await.unwrap_or_else(|_| store::Caps::none());
    if let Some(o) = v.as_object_mut() {
        o.insert("kinds".into(), serde_json::json!(kinds));
        o.insert("platform_ack".into(), serde_json::json!(ws.as_ref().is_some_and(|a| a.platform_ack_at.is_some())));
        o.insert(
            "platform_ack_at".into(),
            serde_json::json!(ws.as_ref().and_then(|a| a.platform_ack_at).map(|t| t.to_rfc3339())),
        );
        o.insert(
            "can".into(),
            serde_json::json!({
                "plans": caps.plans,
                "credits": caps.credits,
                "keys": caps.keys,
            }),
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
        let caps = store::workspace_caps(&state.db, acc.tenant(), acc.account_id).await?;
        if !caps.plans {
            return Err(ApiError(StatusCode::FORBIDDEN, "you do not have permission to change this workspace".into()));
        }
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
    // Proving the current password is the pool's job: the row holds nothing
    // to check it against.
    crate::identity::authenticate(&acc.email, &body.current)
        .await
        .map_err(|_| ApiError(StatusCode::UNAUTHORIZED, "current password is incorrect".into()))?;
    validate_signup(&acc.email, &body.new).map_err(|e| bad_request(e.to_string()))?;
    crate::identity::set_password(&acc.email, &body.new).await.map_err(identity_error)?;
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

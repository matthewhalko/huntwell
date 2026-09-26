//! Where a password actually lives: an AWS Cognito user pool, always.
//!
//! No password, hash or authenticator secret ever reaches this database — the
//! `account` row carries the pool's subject and nothing else. That holds in
//! every environment, so a dev checkout points at a dev pool (see
//! `local-infra/global.example`) rather than keeping a local stand-in that
//! could one day be what production runs on.
//!
//! The port of `../../parkriver/cmd/src/shared/identity/cognito.rs`.
use anyhow::{anyhow, Result};

/// What every service refuses to run without: the pool. Said at startup,
/// naming the settings, rather than as a sign-up that fails later.
pub fn enforce_configured() -> Result<()> {
    if crate::cognito::configured() {
        return Ok(());
    }
    Err(anyhow!(
        "no Cognito pool is configured, and every account signs in through one. Add COGNITO_USER_POOL_ID, \
         COGNITO_CLIENT_ID, COGNITO_REGION (and COGNITO_CLIENT_SECRET, AWS_COGNITO_KEY, AWS_COGNITO_SECRET) — \
         to the Secrets Manager secret in production, to local-infra/global on a dev box (docs/SECRETS.md)"
    ))
}

/// Password rules applied before anything is sent to the pool. Cognito
/// enforces its own policy too; this exists so a weak password is refused
/// with a sentence rather than an AWS error code.
pub fn check_password_strength(password: &str) -> Result<()> {
    if password.chars().count() < 12 {
        return Err(anyhow!("password must be at least 12 characters"));
    }
    let has_lower = password.chars().any(|c| c.is_lowercase());
    let has_upper = password.chars().any(|c| c.is_uppercase());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    if !(has_lower && has_upper && has_digit) {
        return Err(anyhow!("password must contain an uppercase letter, a lowercase letter and a digit"));
    }
    Ok(())
}

/// Create the identity for a new account; returns the pool's subject.
pub async fn create_user(email: &str, password: &str) -> Result<String> {
    check_password_strength(password)?;
    crate::cognito::create_user(email, password).await
}

/// Check a password. Returns the subject, so a caller can reconcile a row
/// whose `cognito_sub` is not yet filled in. A pending second factor counts
/// as not signed in here; `sign_in` is the call that handles it.
pub async fn authenticate(email: &str, password: &str) -> Result<String> {
    crate::cognito::sign_in(email, password).await
}

/// Set or reset a password in the pool.
pub async fn set_password(email: &str, password: &str) -> Result<()> {
    check_password_strength(password)?;
    crate::cognito::set_password(email, password).await
}

/// Remove the identity — when an account is deleted, or a sign-up failed
/// after the pool user was made.
pub async fn delete_user(email: &str) -> Result<()> {
    crate::cognito::delete_user(email).await
}

// ── two-factor (TOTP) ────────────────────────────────────────────────────────
//
// The secret lives in the pool and every step is a pool call; this database
// keeps only `mfa_enabled_at`, a mirror for the UI.

/// What a sign-in came to once the password was right.
#[derive(Debug, Clone)]
pub enum SignIn {
    /// The password was enough. `access_token` is for `remember_access_token`,
    /// which the caller does only once it has checked `sub` is this account.
    Done { sub: String, access_token: String },
    /// Present a TOTP code next. `challenge` is opaque; it comes back with it.
    MfaRequired { challenge: String },
}

/// A TOTP secret to show once: as a QR and as text for the person who can't scan.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Enrolment {
    pub secret: String,
    pub uri: String,
    pub qr_svg: String,
}

/// Access tokens from recent Cognito sign-ins, by account, for enrolment —
/// `AssociateSoftwareToken` and `VerifySoftwareToken` act on a token, not a
/// name. Kept an hour (a token's life) and in memory: the website is one
/// process, and losing them costs a person their password prompt, not access.
static ENROL: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<i64, (String, std::time::Instant)>>> =
    std::sync::OnceLock::new();

fn enrol_tokens() -> &'static std::sync::Mutex<std::collections::HashMap<i64, (String, std::time::Instant)>> {
    ENROL.get_or_init(Default::default)
}

/// Remember a sign-in's access token so enrolment can follow without asking
/// for the password again. No-op for an empty token (local identity).
pub fn remember_access_token(account_id: i64, access_token: &str) {
    if access_token.is_empty() {
        return;
    }
    let mut map = enrol_tokens().lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    map.retain(|_, (_, at)| now.duration_since(*at) < std::time::Duration::from_secs(3600));
    map.insert(account_id, (access_token.to_string(), now));
}

fn access_token_for(account_id: i64) -> Option<String> {
    let map = enrol_tokens().lock().unwrap_or_else(|e| e.into_inner());
    map.get(&account_id)
        .filter(|(_, at)| at.elapsed() < std::time::Duration::from_secs(3600))
        .map(|(t, _)| t.clone())
}

/// Second-factor challenges this process handed out, by the SHA-256 of the
/// challenge, to the account whose password earned them. The challenge a
/// browser brings back is looked up here — it names nobody by itself. Taking
/// the account from the request instead once let anyone with their own
/// authenticator finish their own challenge and be handed a session for any
/// address they typed. Five minutes, Cognito's own limit for the session.
static PENDING_MFA: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (i64, std::time::Instant)>>> =
    std::sync::OnceLock::new();
const MFA_CHALLENGE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

fn pending_mfa() -> &'static std::sync::Mutex<std::collections::HashMap<String, (i64, std::time::Instant)>> {
    PENDING_MFA.get_or_init(Default::default)
}

fn challenge_key(challenge: &str) -> String {
    crate::store::sha256_hex(challenge)
}

/// Sign in for real: password, then possibly a second factor.
pub async fn sign_in(email: &str, password: &str, account_id: i64) -> Result<SignIn> {
    match crate::cognito::sign_in_full(email, password).await? {
        crate::cognito::SignIn::Done { sub, access_token } => Ok(SignIn::Done { sub, access_token }),
        crate::cognito::SignIn::MfaRequired { session, username } => {
            // Both halves travel together; the answer needs the echoed name.
            let challenge = format!("{username}\n{session}");
            let mut map = pending_mfa().lock().unwrap_or_else(|e| e.into_inner());
            map.retain(|_, (_, at)| at.elapsed() < MFA_CHALLENGE_TTL);
            map.insert(challenge_key(&challenge), (account_id, std::time::Instant::now()));
            Ok(SignIn::MfaRequired { challenge })
        }
    }
}

/// Which account a challenge was issued to, if it was issued here and is live.
pub fn mfa_challenge_account(challenge: &str) -> Option<i64> {
    let map = pending_mfa().lock().unwrap_or_else(|e| e.into_inner());
    map.get(&challenge_key(challenge)).filter(|(_, at)| at.elapsed() < MFA_CHALLENGE_TTL).map(|(id, _)| *id)
}

/// Answer the second factor. Returns the subject on success.
/// Returns the subject and access token on success; the caller checks the
/// subject is the account's before remembering the token or starting a session.
pub async fn answer_mfa(challenge: &str, code: &str) -> Result<(String, String)> {
    let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    if code.len() != 6 {
        return Err(anyhow!("the code is six digits"));
    }
    let (username, session) = challenge.split_once('\n').ok_or_else(|| anyhow!("that sign-in took too long — start again"))?;
    match crate::cognito::respond_to_mfa(username, &code, session).await? {
        crate::cognito::SignIn::Done { sub, access_token } => {
            // Spent: a challenge answers once.
            pending_mfa().lock().unwrap_or_else(|e| e.into_inner()).remove(&challenge_key(challenge));
            Ok((sub, access_token))
        }
        crate::cognito::SignIn::MfaRequired { .. } => Err(anyhow!("that code is not right — try the next one your app shows")),
    }
}

/// What `begin_mfa` says when it has no token to act with and was given no
/// password to get one. The caller turns it into a prompt, not an error.
pub const NEEDS_PASSWORD: &str = "enter your password to set up two-factor authentication";

/// Start enrolment. `password` is needed when no recent sign-in's token is at
/// hand (the Settings page, an hour after signing in).
pub async fn begin_mfa(email: &str, account_id: i64, password: Option<&str>) -> Result<Enrolment> {
    let token = match access_token_for(account_id) {
        Some(t) => t,
        None => {
            let pw = password.filter(|p| !p.is_empty()).ok_or_else(|| anyhow!(NEEDS_PASSWORD))?;
            match crate::cognito::sign_in_full(email, pw).await? {
                crate::cognito::SignIn::Done { access_token, .. } => {
                    remember_access_token(account_id, &access_token);
                    access_token
                }
                crate::cognito::SignIn::MfaRequired { .. } => return Err(anyhow!("two-factor authentication is already on for this account")),
            }
        }
    };
    let secret = crate::cognito::associate_totp(&token).await?;
    let uri = totp_uri(&secret, email);
    let qr_svg = crate::qr::svg(&uri).map_err(anyhow::Error::msg)?;
    Ok(Enrolment { secret, uri, qr_svg })
}

/// Finish enrolment with a code from the app; two-factor is on from here.
pub async fn confirm_mfa(email: &str, account_id: i64, code: &str, db: &crate::store::Db) -> Result<()> {
    let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    let token = access_token_for(account_id).ok_or_else(|| anyhow!("that setup took too long — start it again"))?;
    crate::cognito::confirm_totp(email, &token, &code).await?;
    crate::store::set_mfa_enabled(db, account_id, true).await
}

/// Turn two-factor off for an account: the person, with their code, or an
/// operator for someone who lost their phone.
pub async fn disable_mfa(email: &str, account_id: i64, db: &crate::store::Db) -> Result<()> {
    crate::cognito::set_totp_enabled(email, false).await?;
    crate::store::set_mfa_enabled(db, account_id, false).await
}

/// Prove password and code together, by signing in — for "turn it off".
pub async fn check_mfa_code(email: &str, password: &str, code: &str) -> Result<()> {
    match crate::cognito::sign_in_full(email, password).await? {
        crate::cognito::SignIn::MfaRequired { session, username } => {
            let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
            crate::cognito::respond_to_mfa(&username, &code, &session).await.map(|_| ())
        }
        crate::cognito::SignIn::Done { .. } => Ok(()),
    }
}

/// The `otpauth://` URI an authenticator app scans.
pub fn totp_uri(secret: &str, email: &str) -> String {
    let issuer = crate::config::get("HUNTWELL_MFA_ISSUER").filter(|v| !v.trim().is_empty()).unwrap_or_else(|| "Huntwell".into());
    let label = urlencode(&format!("{issuer}:{email}"));
    format!("otpauth://totp/{label}?secret={secret}&issuer={}&algorithm=SHA1&digits=6&period=30", urlencode(&issuer))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod totp_tests {
    use super::*;

    #[test]
    fn the_uri_names_the_account_and_issuer() {
        let uri = totp_uri("ABC234", "a b+c@example.com");
        assert!(uri.starts_with("otpauth://totp/Huntwell%3Aa%20b%2Bc%40example.com?secret=ABC234&issuer=Huntwell"), "{uri}");
    }
}

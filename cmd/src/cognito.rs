//! AWS Cognito, as the store that owns passwords.
//!
//! The same shape as `../../parkriver/cmd/src/shared/identity/cognito.rs`, so a
//! fix made there can be carried across: two kinds of call, a `SECRET_HASH`
//! when the app client has a secret, and Cognito's own error names translated
//! into something a sign-in page can render.
//!
//! Settings — from the environment, Secrets Manager, or the `global` file:
//!
//! - `COGNITO_USER_POOL_ID` — e.g. `us-east-1_AbC123`
//! - `COGNITO_CLIENT_ID` — an app client with `ALLOW_USER_PASSWORD_AUTH`
//! - `COGNITO_CLIENT_SECRET` — only if that app client has one
//! - `COGNITO_REGION`, falling back to `AWS_REGION`
//! - `AWS_COGNITO_KEY` / `AWS_COGNITO_SECRET` — an IAM credential allowed
//!   `AdminCreateUser`, `AdminSetUserPassword`, `AdminDeleteUser` on the pool
//!
//! The admin credential is deliberately not the bootstrap key: that one reads
//! Secrets Manager and should be able to do nothing else.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::aws::{authorization, service_credentials, Signed};

const SERVICE: &str = "cognito-idp";
const TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";
const JSON_CONTENT_TYPE: &str = "application/x-amz-json-1.1";

/// One client per call, matching `aws.rs`: these are a handful of requests at
/// sign-in, not a hot path, and a shared pool would outlive a rotated proxy
/// setting.
fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| anyhow!("http client: {e}"))
}

/// Everything the pool calls need, read per call so a rotated secret is picked
/// up without a restart.
struct Pool {
    user_pool_id: String,
    client_id: String,
    client_secret: Option<String>,
    region: String,
}

/// Which pool: the product's accounts, or the control plane's operators. Two
/// pools, so a customer can never be an operator by accident and each can be
/// locked down on its own terms. The operator pool's settings carry the
/// `ADMIN_` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolKind {
    Users,
    Admins,
}

impl PoolKind {
    fn setting(self, name: &str) -> String {
        match self {
            PoolKind::Users => format!("COGNITO_{name}"),
            PoolKind::Admins => format!("ADMIN_COGNITO_{name}"),
        }
    }
}

fn set(name: &str) -> bool {
    crate::config::get(name).map(|v| !v.trim().is_empty()).unwrap_or(false)
}

/// Whether this installation has the product's pool to talk to.
pub fn configured() -> bool {
    set("COGNITO_USER_POOL_ID") && set("COGNITO_CLIENT_ID")
}

/// Whether this installation has the operators' pool to talk to.
pub fn admin_configured() -> bool {
    set("ADMIN_COGNITO_USER_POOL_ID") && set("ADMIN_COGNITO_CLIENT_ID")
}

impl Pool {
    fn load() -> Result<Self> {
        Self::load_kind(PoolKind::Users)
    }

    fn load_kind(kind: PoolKind) -> Result<Self> {
        let need = |k: String| -> Result<String> {
            crate::config::get(&k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow!("{k} is not set — every account signs in through Cognito (see docs/SECRETS.md)"))
        };
        Ok(Self {
            user_pool_id: need(kind.setting("USER_POOL_ID"))?,
            client_id: need(kind.setting("CLIENT_ID"))?,
            client_secret: crate::config::get(&kind.setting("CLIENT_SECRET")).filter(|v| !v.trim().is_empty()),
            region: crate::config::get(&kind.setting("REGION"))
                .or_else(|| crate::config::get("COGNITO_REGION"))
                .or_else(|| crate::config::get("AWS_REGION"))
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "us-east-1".into()),
        })
    }

    fn host(&self) -> String {
        format!("{SERVICE}.{}.amazonaws.com", self.region)
    }

    /// Auth parameters always carry the client id, and the secret hash too when
    /// the app client has a secret — Cognito rejects the call outright if a
    /// client with a secret is used without one.
    fn auth_params(&self, username: &str, mut params: serde_json::Map<String, Value>) -> Value {
        if let Some(secret) = &self.client_secret {
            params.insert(
                String::from("SECRET_HASH"),
                Value::String(secret_hash(secret, username, &self.client_id)),
            );
        }
        Value::Object(params)
    }

    /// The public sign-in API: `InitiateAuth`.
    ///
    /// Deliberately unsigned. This is the call a person's own password
    /// authorises, not the platform's. Signing it would put AWS credentials on
    /// the login path, so a pool misconfiguration and a credential problem
    /// would fail the same way.
    async fn public_call(&self, operation: &str, body: Value) -> Result<Value> {
        let host = self.host();
        let response = http()?
            .post(format!("https://{host}/"))
            .header("content-type", JSON_CONTENT_TYPE)
            .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
            .body(body.to_string())
            .send()
            .await
            .with_context(|| format!("cannot reach cognito for {operation}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(serde_json::from_str(&text).unwrap_or(Value::Null));
        }
        Err(translate(operation, &text))
    }

    /// The admin API: `AdminCreateUser`, `AdminSetUserPassword`,
    /// `AdminDeleteUser`. SigV4-signed, because creating an account is
    /// something the platform does rather than something a visitor does.
    async fn call(&self, operation: &str, body: Value) -> Result<Value> {
        let creds = service_credentials("COGNITO", &self.region)?;
        let host = self.host();
        let target = format!("{TARGET_PREFIX}.{operation}");
        let payload = body.to_string();
        let payload_hash = hex::encode(Sha256::digest(payload.as_bytes()));

        let now = chrono::Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        let mut headers: Vec<(&str, String)> = vec![
            ("content-type", JSON_CONTENT_TYPE.to_string()),
            ("host", host.clone()),
            ("x-amz-date", amz_date.clone()),
            ("x-amz-target", target.clone()),
        ];
        if let Some(t) = &creds.session_token {
            headers.push(("x-amz-security-token", t.clone()));
        }
        headers.sort_by(|a, b| a.0.cmp(b.0));
        let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let signed_headers = headers.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(";");

        let auth = authorization(Signed {
            method: "POST",
            canonical_uri: "/",
            canonical_query: "",
            host: &host,
            canonical_headers: &canonical_headers,
            signed_headers: &signed_headers,
            amz_date: &amz_date,
            date_stamp: &date_stamp,
            region: &creds.region,
            service: SERVICE,
            payload_hash: &payload_hash,
            access_key: &creds.access_key,
            secret_key: &creds.secret_key,
        });

        let mut request = http()?
            .post(format!("https://{host}/"))
            .header("authorization", auth)
            .body(payload);
        for (name, value) in &headers {
            // `host` is set by the transport from the URL; sending it twice is
            // what produces a signature mismatch that names nothing useful.
            if *name != "host" {
                request = request.header(*name, value);
            }
        }

        let response = request.send().await.with_context(|| format!("cognito {operation}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(serde_json::from_str(&text).unwrap_or(Value::Null));
        }
        Err(translate(operation, &text))
    }
}

/// Turn Cognito's error shape into something the caller can show a person.
///
/// `__type` names the failure precisely, and several of them mean "the user
/// typed something wrong" rather than "the service is broken" — the difference
/// between a message the sign-in page renders and one that reads as an outage.
/// What a person is told when the pool itself failed — a permission the IAM
/// user lacks, a pool that is gone, AWS having a bad minute. None of that is
/// theirs to act on, and the AWS wording names account ids, ARNs and IAM
/// users; the detail goes to the log, where the operator reads it.
pub const UNAVAILABLE: &str = "sign-in is temporarily unavailable — please try again in a few minutes";

/// The pool wants this person to set a new password (an operator reset it, or
/// it was imported). Theirs to fix, with the reset flow — not an outage.
pub const RESET_REQUIRED: &str = "this account needs a new password — use \"Forgot your password?\" to set one";

fn translate(operation: &str, body: &str) -> anyhow::Error {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let kind = parsed["__type"].as_str().unwrap_or("");
    let message = parsed["message"].as_str().unwrap_or(body);
    // `__type` arrives either bare or as `com.amazonaws…#NotAuthorizedException`.
    let kind = kind.rsplit('#').next().unwrap_or(kind);
    match kind {
        // Both mean "these credentials are not good", and saying which would
        // tell an attacker whether the address has an account.
        // AWS files a broken app client under the same error as a wrong
        // password: a missing or wrong client secret, a client that is gone.
        // Told to a person as "incorrect password" it sends them to reset a
        // password that was never the problem, so those are an outage instead.
        "NotAuthorizedException" if !is_credentials_fault(message) => {
            tracing::error!("cognito {operation} refused the app client, not the password (NotAuthorizedException): {message}");
            anyhow!(UNAVAILABLE)
        }
        "NotAuthorizedException" if message.contains("attempts exceeded") => {
            anyhow!("too many attempts — wait a moment and try again")
        }
        "NotAuthorizedException" | "UserNotFoundException" => anyhow!("invalid email or password"),
        // "USER_PASSWORD_AUTH flow not enabled for this client" and its kin:
        // on sign-in an invalid parameter is the app client's setup, never
        // something the person typed.
        "InvalidParameterException" if operation == "InitiateAuth" => {
            tracing::error!("cognito InitiateAuth is misconfigured (InvalidParameterException): {message}");
            anyhow!(UNAVAILABLE)
        }
        "PasswordResetRequiredException" | "UserNotConfirmedException" => {
            tracing::warn!("cognito {operation}: {kind}: {message}");
            anyhow!(RESET_REQUIRED)
        }
        "UsernameExistsException" => anyhow!("that email already has an account"),
        "CodeMismatchException" | "EnableSoftwareTokenMFAException" => anyhow!("that code is not right — try the next one your app shows"),
        "ExpiredCodeException" => anyhow!("that sign-in took too long — start again"),
        "InvalidPasswordException" | "InvalidParameterException" => anyhow!("{message}"),
        "TooManyRequestsException" | "LimitExceededException" => {
            anyhow!("too many attempts — wait a moment and try again")
        }
        _ => {
            tracing::error!("cognito {operation} failed ({}): {message}", if kind.is_empty() { "no error type" } else { kind });
            anyhow!(UNAVAILABLE)
        }
    }
}

/// Whether a `NotAuthorizedException` is about the person's credentials. The
/// pool's wordings for that are few and stable; anything else under this error
/// type is about the app client.
fn is_credentials_fault(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("incorrect username or password")
        || m.contains("attempts exceeded")
        || m.contains("user is disabled")
        || m.contains("user does not exist")
        || m.contains("invalid session")
        || m.contains("access token")
}

/// `SECRET_HASH` = base64(HMAC-SHA256(client_secret, username + client_id)).
///
/// AWS's documented formula. Getting it wrong fails every call against a pool
/// whose app client has a secret, with an error that blames the credentials
/// rather than the hash.
fn secret_hash(client_secret: &str, username: &str, client_id: &str) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<Sha256>>::new_from_slice(client_secret.as_bytes()).expect("hmac accepts any key length");
    mac.update(username.as_bytes());
    mac.update(client_id.as_bytes());
    b64(&mac.finalize().into_bytes())
}

/// Base64, standard alphabet, padded. One encoder for one field — the same
/// choice `admin::hosts` made for the registry secret.
fn b64(input: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for c in input.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if c.len() > 1 { A[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if c.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Check an email and password against the pool.
///
/// Returns the account's Cognito subject — the stable id the `account` row is
/// keyed to. An email can be changed; `sub` never is, which is why the row
/// keys on it rather than on the address.
/// Username and password against the pool, for callers that only need the
/// subject and treat a pending second factor as "not signed in".
pub async fn sign_in(email: &str, password: &str) -> Result<String> {
    match sign_in_full(email, password).await? {
        SignIn::Done { sub, .. } => Ok(sub),
        SignIn::MfaRequired { .. } => Err(anyhow!("this account needs its two-factor code to sign in")),
    }
}

/// What a sign-in (or a challenge answer) produced.
#[derive(Debug, Clone)]
pub enum SignIn {
    /// Tokens issued: the subject, and the access token the TOTP enrolment
    /// calls act on (Cognito identifies a user for those by token, not name).
    Done { sub: String, access_token: String },
    /// The password was right and the pool wants a TOTP code. `session` is
    /// opaque state to hand back with it; `username` is the name Cognito
    /// echoed, which the answer has to use.
    MfaRequired { session: String, username: String },
}

fn read_sign_in(parsed: Value, username: &str) -> Result<SignIn> {
    if let Some(name) = parsed["ChallengeName"].as_str() {
        if name == "SOFTWARE_TOKEN_MFA" {
            return Ok(SignIn::MfaRequired {
                session: parsed["Session"].as_str().unwrap_or_default().to_string(),
                username: parsed["ChallengeParameters"]["USER_ID_FOR_SRP"].as_str().unwrap_or(username).to_string(),
            });
        }
        // Any other challenge means a pool configured by hand: Huntwell makes
        // users with permanent passwords, so nothing else should ever come up.
        return Err(anyhow!("this account needs to finish {name} in Cognito before it can sign in"));
    }
    let id_token = parsed["AuthenticationResult"]["IdToken"].as_str().ok_or_else(|| anyhow!("cognito returned no id token"))?;
    let sub = sub_from_id_token(id_token).ok_or_else(|| anyhow!("cognito's id token carried no sub"))?;
    let access_token = parsed["AuthenticationResult"]["AccessToken"].as_str().unwrap_or_default().to_string();
    Ok(SignIn::Done { sub, access_token })
}

/// Username and password against the pool — a sign-in, or a TOTP challenge.
pub async fn sign_in_full(email: &str, password: &str) -> Result<SignIn> {
    sign_in_full_in(PoolKind::Users, email, password).await
}

pub async fn sign_in_full_in(kind: PoolKind, email: &str, password: &str) -> Result<SignIn> {
    let pool = Pool::load_kind(kind)?;
    let mut params = serde_json::Map::new();
    params.insert(String::from("USERNAME"), Value::String(email.to_string()));
    params.insert(String::from("PASSWORD"), Value::String(password.to_string()));
    let parsed = pool
        .public_call(
            "InitiateAuth",
            json!({
                "AuthFlow": "USER_PASSWORD_AUTH",
                "ClientId": pool.client_id,
                "AuthParameters": pool.auth_params(email, params),
            }),
        )
        .await?;
    read_sign_in(parsed, email)
}

/// Try a sign-in for an address that cannot exist and report exactly what the
/// pool said, untranslated — for `huntwell doctor`. A healthy pool answers
/// "Incorrect username or password" (or "user does not exist"); anything else
/// is the app client, the pool or the region, which a real sign-in reports to
/// a person only as "temporarily unavailable".
pub async fn probe_sign_in(kind: PoolKind) -> Result<String, String> {
    let pool = Pool::load_kind(kind).map_err(|e| format!("{e:#}"))?;
    let email = "huntwell-doctor-probe@invalid.example";
    let mut params = serde_json::Map::new();
    params.insert(String::from("USERNAME"), Value::String(email.into()));
    params.insert(String::from("PASSWORD"), Value::String("Probe-not-a-password-1".into()));
    let body = json!({ "AuthFlow": "USER_PASSWORD_AUTH", "ClientId": pool.client_id, "AuthParameters": pool.auth_params(email, params) });
    let response = http()
        .map_err(|e| format!("{e:#}"))?
        .post(format!("https://{}/", pool.host()))
        .header("content-type", JSON_CONTENT_TYPE)
        .header("x-amz-target", format!("{TARGET_PREFIX}.InitiateAuth"))
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| format!("cannot reach {}: {e}", pool.host()))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let kind_ = parsed["__type"].as_str().unwrap_or("").rsplit('#').next().unwrap_or("").to_string();
    let message = parsed["message"].as_str().unwrap_or(&text).to_string();
    let region = &pool.region;
    let what = format!("pool {} in {region}: {status} {kind_}: {message}", pool.user_pool_id);
    match kind_.as_str() {
        "UserNotFoundException" => Ok(what),
        "NotAuthorizedException" if is_credentials_fault(&message) => Ok(what),
        _ => Err(what),
    }
}

/// Answer the TOTP challenge `sign_in_full` raised.
pub async fn respond_to_mfa(username: &str, code: &str, session: &str) -> Result<SignIn> {
    let pool = Pool::load()?;
    let mut responses = serde_json::Map::new();
    responses.insert(String::from("USERNAME"), Value::String(username.to_string()));
    responses.insert(String::from("SOFTWARE_TOKEN_MFA_CODE"), Value::String(code.to_string()));
    let parsed = pool
        .public_call(
            "RespondToAuthChallenge",
            json!({
                "ClientId": pool.client_id,
                "ChallengeName": "SOFTWARE_TOKEN_MFA",
                "Session": session,
                "ChallengeResponses": pool.auth_params(username, responses),
            }),
        )
        .await?;
    read_sign_in(parsed, username)
}

/// Start TOTP enrolment for the user this access token belongs to: the pool
/// mints the secret, and it never touches this database. `public_call`, not
/// `call`: the user's own token authenticates it, and signing it as well
/// would make AWS want an IAM permission for something the platform
/// credential is not doing.
pub async fn associate_totp(access_token: &str) -> Result<String> {
    let pool = Pool::load()?;
    let v = pool.public_call("AssociateSoftwareToken", json!({ "AccessToken": access_token })).await?;
    v["SecretCode"].as_str().map(String::from).ok_or_else(|| anyhow!("cognito returned no TOTP secret"))
}

/// Finish enrolment: prove the app is synchronised, then make TOTP the
/// factor this account is challenged for. Both calls matter — the first
/// alone leaves a factor that is never asked for.
pub async fn confirm_totp(email: &str, access_token: &str, code: &str) -> Result<()> {
    let pool = Pool::load()?;
    let v = pool
        .public_call(
            "VerifySoftwareToken",
            json!({ "AccessToken": access_token, "UserCode": code, "FriendlyDeviceName": "Huntwell" }),
        )
        .await?;
    if v["Status"].as_str() != Some("SUCCESS") {
        return Err(anyhow!("that code is not right — try the next one your app shows"));
    }
    set_totp_enabled(email, true).await
}

/// Turn the software-token factor on or off for a user. Off is how an
/// operator lets someone who lost their phone back in.
pub async fn set_totp_enabled(email: &str, enabled: bool) -> Result<()> {
    let pool = Pool::load()?;
    pool.call(
        "AdminSetUserMFAPreference",
        json!({
            "UserPoolId": pool.user_pool_id,
            "Username": email,
            "SoftwareTokenMfaSettings": { "Enabled": enabled, "PreferredMfa": enabled },
        }),
    )
    .await
    .map(|_| ())
}

/// The `sub` claim, read without verifying the signature.
///
/// Safe here and only here: this token came back over TLS from the call we
/// just made, so there is no untrusted party between. A token arriving from a
/// browser would have to be verified against the pool's JWKS instead.
pub fn sub_from_id_token(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let decoded = b64url_decode(payload)?;
    let parsed: Value = serde_json::from_slice(&decoded).ok()?;
    parsed["sub"].as_str().map(String::from)
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => continue,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// Create a user with a password already set, and return its `sub`.
///
/// Two calls, not one: `AdminCreateUser` leaves the account in
/// FORCE_CHANGE_PASSWORD, which would meet the next sign-in with a
/// NEW_PASSWORD_REQUIRED challenge the sign-in page has no screen for.
/// `AdminSetUserPassword` with `Permanent` clears that.
pub async fn create_user(email: &str, password: &str) -> Result<String> {
    create_user_in(PoolKind::Users, email, password).await
}

pub async fn create_user_in(kind: PoolKind, email: &str, password: &str) -> Result<String> {
    let pool = Pool::load_kind(kind)?;
    let created = pool
        .call(
            "AdminCreateUser",
            json!({
                "UserPoolId": pool.user_pool_id,
                "Username": email,
                // SUPPRESS: Huntwell sends its own verification mail, and the
                // person is choosing this password right now — Cognito's
                // invitation would arrive with a temporary one they never used.
                "MessageAction": "SUPPRESS",
                "UserAttributes": [
                    { "Name": "email", "Value": email },
                    { "Name": "email_verified", "Value": "true" },
                ],
            }),
        )
        .await?;

    let sub = created["User"]["Attributes"]
        .as_array()
        .and_then(|attrs| attrs.iter().find(|a| a["Name"] == "sub"))
        .and_then(|a| a["Value"].as_str())
        .map(String::from)
        .ok_or_else(|| anyhow!("cognito did not return a sub for the new user"))?;

    set_password_in(kind, email, password).await?;
    Ok(sub)
}

/// The subject of an existing user, by name.
pub async fn subject_of(kind: PoolKind, email: &str) -> Result<String> {
    let pool = Pool::load_kind(kind)?;
    let user = pool.call("AdminGetUser", json!({ "UserPoolId": pool.user_pool_id, "Username": email })).await?;
    user["UserAttributes"]
        .as_array()
        .and_then(|attrs| attrs.iter().find(|a| a["Name"] == "sub"))
        .and_then(|a| a["Value"].as_str())
        .map(String::from)
        .ok_or_else(|| anyhow!("cognito returned no sub for {email}"))
}

/// Set (or reset) a password, permanently — no challenge on next sign-in.
pub async fn set_password(email: &str, password: &str) -> Result<()> {
    set_password_in(PoolKind::Users, email, password).await
}

pub async fn set_password_in(kind: PoolKind, email: &str, password: &str) -> Result<()> {
    let pool = Pool::load_kind(kind)?;
    pool.call(
        "AdminSetUserPassword",
        json!({
            "UserPoolId": pool.user_pool_id,
            "Username": email,
            "Password": password,
            "Permanent": true,
        }),
    )
    .await
    .map(|_| ())
}

/// Remove a user from the pool. Used when an account is deleted, so the
/// directory does not keep identities for workspaces that no longer exist.
pub async fn delete_user(email: &str) -> Result<()> {
    let pool = Pool::load()?;
    pool.call(
        "AdminDeleteUser",
        json!({ "UserPoolId": pool.user_pool_id, "Username": email }),
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_hash_is_hmac_of_username_then_client_id() {
        use hmac::{Hmac, Mac};
        let expected = {
            let mut mac = <Hmac<Sha256>>::new_from_slice(b"shhh").unwrap();
            mac.update(b"someone@example.com");
            mac.update(b"client-123");
            b64(&mac.finalize().into_bytes())
        };
        assert_eq!(secret_hash("shhh", "someone@example.com", "client-123"), expected);
    }

    #[test]
    fn the_sub_is_read_out_of_an_id_token() {
        // header.payload.signature — only the payload is looked at.
        let payload = b64url(br#"{"sub":"9f1c-42","email":"a@b.test"}"#);
        let token = format!("eyJhbGciOiJSUzI1NiJ9.{payload}.signature");
        assert_eq!(sub_from_id_token(&token).as_deref(), Some("9f1c-42"));
    }

    #[test]
    fn a_token_with_no_sub_is_none_rather_than_a_panic() {
        let token = format!("h.{}.s", b64url(br#"{"email":"a@b.test"}"#));
        assert_eq!(sub_from_id_token(&token), None);
        assert_eq!(sub_from_id_token("not-a-token"), None);
        assert_eq!(sub_from_id_token(""), None);
    }

    #[test]
    fn cognito_errors_become_messages_a_person_can_act_on() {
        let bad = translate("InitiateAuth", r#"{"__type":"NotAuthorizedException","message":"Incorrect username or password."}"#);
        // Never "no such user": that tells an attacker which addresses exist.
        assert_eq!(bad.to_string(), "invalid email or password");
        let missing = translate("InitiateAuth", r#"{"__type":"com.amazonaws.cognitoidp#UserNotFoundException","message":"x"}"#);
        assert_eq!(missing.to_string(), "invalid email or password");
        // A broken app client arrives under the wrong-password error type. It
        // must not read as a wrong password: the person would reset it forever.
        for m in [
            "Unable to verify secret hash for client 4abc",
            "Client 4abc is configured with secret but SECRET_HASH was not received",
        ] {
            let e = translate("InitiateAuth", &format!(r#"{{"__type":"NotAuthorizedException","message":"{m}"}}"#));
            assert_eq!(e.to_string(), UNAVAILABLE, "{m}");
        }
        let flow = translate("InitiateAuth", r#"{"__type":"InvalidParameterException","message":"USER_PASSWORD_AUTH flow not enabled for this client"}"#);
        assert_eq!(flow.to_string(), UNAVAILABLE);
        let locked = translate("InitiateAuth", r#"{"__type":"NotAuthorizedException","message":"Password attempts exceeded"}"#);
        assert!(locked.to_string().starts_with("too many attempts"));
        let taken = translate("AdminCreateUser", r#"{"__type":"UsernameExistsException","message":"x"}"#);
        assert_eq!(taken.to_string(), "that email already has an account");
        // A policy rejection is the pool's own wording, which says what is wrong.
        let weak = translate("AdminSetUserPassword", r#"{"__type":"InvalidPasswordException","message":"Password does not conform to policy"}"#);
        // AWS's own words name the account, the IAM user and the pool; none of
        // that reaches a person at a sign-up form.
        let denied = translate("AdminCreateUser", r#"{"__type":"AccessDeniedException","Message":"User: arn:aws:iam::123:user/x is not authorized to perform: cognito-idp:AdminCreateUser on resource: arn:aws:cognito-idp:us-east-1:123:userpool/us-east-1_abc"}"#);
        assert_eq!(denied.to_string(), UNAVAILABLE);
        assert!(!denied.to_string().contains("arn:"));
        assert_eq!(weak.to_string(), "Password does not conform to policy");
    }

    /// base64url without padding, for building test tokens.
    fn b64url(input: &[u8]) -> String {
        b64(input).trim_end_matches('=').replace('+', "-").replace('/', "_")
    }
}

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

/// Whether this installation has a pool to talk to.
pub fn configured() -> bool {
    crate::config::get("COGNITO_USER_POOL_ID").map(|v| !v.trim().is_empty()).unwrap_or(false)
        && crate::config::get("COGNITO_CLIENT_ID").map(|v| !v.trim().is_empty()).unwrap_or(false)
}

impl Pool {
    fn load() -> Result<Self> {
        let need = |k: &str| -> Result<String> {
            crate::config::get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow!("{k} is not set — this installation uses Cognito for identity (see docs/SECRETS.md)"))
        };
        Ok(Self {
            user_pool_id: need("COGNITO_USER_POOL_ID")?,
            client_id: need("COGNITO_CLIENT_ID")?,
            client_secret: crate::config::get("COGNITO_CLIENT_SECRET").filter(|v| !v.trim().is_empty()),
            region: crate::config::get("COGNITO_REGION")
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
fn translate(operation: &str, body: &str) -> anyhow::Error {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let kind = parsed["__type"].as_str().unwrap_or("");
    let message = parsed["message"].as_str().unwrap_or(body);
    // `__type` arrives either bare or as `com.amazonaws…#NotAuthorizedException`.
    let kind = kind.rsplit('#').next().unwrap_or(kind);
    match kind {
        // Both mean "these credentials are not good", and saying which would
        // tell an attacker whether the address has an account.
        "NotAuthorizedException" | "UserNotFoundException" => anyhow!("invalid email or password"),
        "UsernameExistsException" => anyhow!("that email already has an account"),
        "InvalidPasswordException" | "InvalidParameterException" => anyhow!("{message}"),
        "TooManyRequestsException" | "LimitExceededException" => {
            anyhow!("too many attempts — wait a moment and try again")
        }
        "" => anyhow!("cognito {operation} failed: {body}"),
        other => anyhow!("cognito {operation} failed ({other}): {message}"),
    }
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
pub async fn sign_in(email: &str, password: &str) -> Result<String> {
    let pool = Pool::load()?;
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

    // A challenge is not a sign-in. Huntwell creates users with a permanent
    // password, so the only way to land here is a pool configured by hand —
    // and treating a challenge as success would sign in someone who never
    // finished authenticating.
    if let Some(challenge) = parsed["ChallengeName"].as_str() {
        return Err(anyhow!(
            "this account needs to finish {challenge} in Cognito before it can sign in"
        ));
    }
    let id_token = parsed["AuthenticationResult"]["IdToken"]
        .as_str()
        .ok_or_else(|| anyhow!("cognito returned no id token"))?;
    sub_from_id_token(id_token).ok_or_else(|| anyhow!("cognito's id token carried no sub"))
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
    let pool = Pool::load()?;
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

    set_password(email, password).await?;
    Ok(sub)
}

/// Set (or reset) a password, permanently — no challenge on next sign-in.
pub async fn set_password(email: &str, password: &str) -> Result<()> {
    let pool = Pool::load()?;
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
        let taken = translate("AdminCreateUser", r#"{"__type":"UsernameExistsException","message":"x"}"#);
        assert_eq!(taken.to_string(), "that email already has an account");
        // A policy rejection is the pool's own wording, which says what is wrong.
        let weak = translate("AdminSetUserPassword", r#"{"__type":"InvalidPasswordException","message":"Password does not conform to policy"}"#);
        assert_eq!(weak.to_string(), "Password does not conform to policy");
    }

    /// base64url without padding, for building test tokens.
    fn b64url(input: &[u8]) -> String {
        b64(input).trim_end_matches('=').replace('+', "-").replace('/', "_")
    }
}

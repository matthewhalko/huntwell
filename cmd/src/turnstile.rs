//! Cloudflare Turnstile: a bot check on sign-up and sign-in.
//!
//! The browser renders Cloudflare's widget with the site key and gets a token;
//! the token comes up with the form and is verified here against the secret,
//! once — Cloudflare refuses a token the second time. With no secret
//! configured (a dev box) nothing is checked, and the site key endpoint says
//! so, which is what tells the UI not to render the widget.
//!
//! | Setting                | What                                  |
//! |------------------------|---------------------------------------|
//! | `TURNSTILE_SITE_KEY`   | public; handed to the browser         |
//! | `TURNSTILE_SECRET_KEY` | verifies tokens; Secrets Manager only |

use std::net::IpAddr;

use anyhow::{anyhow, Result};

const VERIFY: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";

pub fn site_key() -> Option<String> {
    crate::config::get("TURNSTILE_SITE_KEY").map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn secret() -> Option<String> {
    crate::config::get("TURNSTILE_SECRET_KEY").map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Whether tokens are required. The secret decides: a site key alone would
/// render a widget nothing checks.
pub fn configured() -> bool {
    secret().is_some()
}

/// Verify a token from the widget. `Ok(())` when Turnstile is not configured.
///
/// The client address is sent along, so a token minted for one visitor cannot
/// be replayed by another.
pub async fn verify(token: &str, ip: IpAddr) -> Result<()> {
    verify_impl(token, ip).await
}

async fn verify_impl(token: &str, ip: IpAddr) -> Result<()> {
    let Some(secret) = secret() else { return Ok(()) };
    let token = token.trim();
    if token.is_empty() {
        return Err(anyhow!("the verification challenge was not completed"));
    }
    let url = crate::config::get("HUNTWELL_TURNSTILE_VERIFY_URL").unwrap_or_else(|| VERIFY.into());
    let resp = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?
        .post(&url)
        .form(&[("secret", secret.as_str()), ("response", token), ("remoteip", &ip.to_string())])
        .send()
        .await
        .map_err(|e| anyhow!("turnstile: {e}"))?;
    let v: serde_json::Value = resp.json().await.map_err(|e| anyhow!("turnstile: {e}"))?;
    if v["success"].as_bool() == Some(true) {
        return Ok(());
    }
    let codes = v["error-codes"]
        .as_array()
        .map(|a| a.iter().filter_map(|c| c.as_str()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    // Logged with the codes — a misconfigured secret looks like every visitor
    // failing — and reported to the visitor without them.
    tracing::warn!("turnstile refused a token ({codes})");
    Err(anyhow!("verification failed — reload the page and try again"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against a stand-in for Cloudflare's endpoint, reached through the same
    /// override a test deployment would use.
    #[tokio::test]
    async fn tokens_are_verified_with_the_secret_and_the_visitors_address() {
        use axum::{routing::post, Form, Json, Router};
        #[derive(serde::Deserialize)]
        struct Req { secret: String, response: String, remoteip: String }
        async fn verify(Form(r): Form<Req>) -> Json<serde_json::Value> {
            let ok = r.secret == "shh" && r.response == "good-token" && r.remoteip == "203.0.113.9";
            Json(serde_json::json!({ "success": ok, "error-codes": if ok { vec![] } else { vec!["invalid-input-response"] } }))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, Router::new().route("/verify", post(verify))).await.unwrap() });
        std::env::set_var("HUNTWELL_TURNSTILE_VERIFY_URL", format!("http://{addr}/verify"));
        std::env::set_var("TURNSTILE_SECRET_KEY", "shh");
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        assert!(verify_impl("good-token", ip).await.is_ok());
        assert!(verify_impl("bad-token", ip).await.is_err());
        assert!(verify_impl("", ip).await.is_err(), "a missing token is refused before any request");
        assert!(verify_impl("good-token", "198.51.100.1".parse().unwrap()).await.is_err(), "bound to the address");
        std::env::remove_var("TURNSTILE_SECRET_KEY");
        assert!(verify_impl("anything", ip).await.is_ok(), "unconfigured: nothing is checked");
        std::env::remove_var("HUNTWELL_TURNSTILE_VERIFY_URL");
    }
}

//! Cloudflare Tunnel and DNS, for a host's edge — the port of Park River's
//! `shared/cloudflare.rs`.
//!
//! A host whose edge is `cloudflare` gets its own tunnel, and its `cloudflared`
//! runs in that host's edge container beside Caddy. Nothing is published on the
//! server's 80 or 443: the connector dials out, which is what makes it the edge
//! for a server with no public IP — or one you would rather not expose.
//!
//! Settings, through the usual chain:
//!
//! | Setting                  | What                                          |
//! |--------------------------|-----------------------------------------------|
//! | `CLOUDFLARE_API_TOKEN`   | Account › Cloudflare Tunnel › Edit, and       |
//! |                          | Zone › DNS › Edit — Secrets Manager only      |
//! | `CLOUDFLARE_ACCOUNT_ID`  | the account the tunnels belong to             |
//! | `CLOUDFLARE_ZONE_ID`     | the zone the app's hostname is created in     |

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

const API: &str = "https://api.cloudflare.com/client/v4";

/// Tunnels are named for their host, which is what makes creation idempotent:
/// a sync that failed after creating the tunnel finds it again instead of
/// minting a second.
pub fn tunnel_name(host: &str) -> String {
    format!("huntwell-host-{host}")
}

pub struct Cloudflare {
    token: String,
    account: String,
    zone: String,
    http: reqwest::Client,
}

impl Cloudflare {
    /// Load credentials, or say exactly which one is missing.
    pub fn load() -> Result<Self> {
        let need = |name: &str| {
            crate::config::get(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).ok_or_else(|| {
                anyhow!(
                    "{name} is not set — a host whose edge is a Cloudflare tunnel needs CLOUDFLARE_API_TOKEN \
                     (in the Secrets Manager secret), CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_ZONE_ID"
                )
            })
        };
        Ok(Cloudflare {
            token: need("CLOUDFLARE_API_TOKEN")?,
            account: need("CLOUDFLARE_ACCOUNT_ID")?,
            zone: need("CLOUDFLARE_ZONE_ID")?,
            http: reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build()?,
        })
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut req = self.http.request(method.clone(), format!("{API}{path}")).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(|e| anyhow!("cloudflare {method} {path}: {e}"))?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        // Cloudflare reports failure in the body as well as the status, and the
        // body is the half with a message in it.
        if !status.is_success() || v["success"].as_bool() == Some(false) {
            let why = v["errors"]
                .as_array()
                .map(|es| es.iter().filter_map(|e| e["message"].as_str()).collect::<Vec<_>>().join("; "))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| status.to_string());
            bail!("cloudflare {method} {path}: {why}");
        }
        Ok(v["result"].clone())
    }

    /// The tunnel's id, if one of this name exists.
    pub async fn find_tunnel(&self, name: &str) -> Result<Option<String>> {
        let found = self
            .call(
                reqwest::Method::GET,
                &format!("/accounts/{}/cfd_tunnel?name={name}&is_deleted=false", self.account),
                None,
            )
            .await?;
        Ok(found.as_array().and_then(|a| a.first()).and_then(|t| t["id"].as_str()).map(String::from))
    }

    /// Find the tunnel, or create it. Returns its id.
    pub async fn ensure_tunnel(&self, name: &str) -> Result<String> {
        if let Some(id) = self.find_tunnel(name).await? {
            return Ok(id);
        }
        // config_src "cloudflare": routing lives in Cloudflare and is set with
        // `put_ingress`, so the connector carries a token and nothing else.
        let created = self
            .call(
                reqwest::Method::POST,
                &format!("/accounts/{}/cfd_tunnel", self.account),
                Some(json!({ "name": name, "config_src": "cloudflare" })),
            )
            .await?;
        created["id"].as_str().map(String::from).ok_or_else(|| anyhow!("cloudflare created a tunnel but returned no id"))
    }

    /// The token a `cloudflared` connector runs with.
    pub async fn tunnel_token(&self, tunnel_id: &str) -> Result<String> {
        let v = self
            .call(reqwest::Method::GET, &format!("/accounts/{}/cfd_tunnel/{tunnel_id}/token", self.account), None)
            .await?;
        v.as_str().map(String::from).ok_or_else(|| anyhow!("cloudflare returned no tunnel token"))
    }

    /// Replace the tunnel's routing.
    pub async fn put_ingress(&self, tunnel_id: &str, ingress: Value) -> Result<()> {
        self.call(
            reqwest::Method::PUT,
            &format!("/accounts/{}/cfd_tunnel/{tunnel_id}/configurations", self.account),
            Some(json!({ "config": { "ingress": ingress } })),
        )
        .await
        .map(|_| ())
    }

    /// Point `hostname` at the tunnel: create the record, or correct it.
    pub async fn ensure_dns(&self, hostname: &str, tunnel_id: &str) -> Result<()> {
        let record = json!({
            "type": "CNAME",
            "name": hostname,
            "content": tunnel_target(tunnel_id),
            // Proxied is not optional: an unproxied CNAME to cfargotunnel.com
            // resolves to nothing a browser can connect to.
            "proxied": true,
        });
        match self.dns_record(hostname).await? {
            Some((id, _)) => self
                .call(reqwest::Method::PUT, &format!("/zones/{}/dns_records/{id}", self.zone), Some(record))
                .await
                .map(|_| ()),
            None => self
                .call(reqwest::Method::POST, &format!("/zones/{}/dns_records", self.zone), Some(record))
                .await
                .map(|_| ()),
        }
    }

    /// Remove `hostname`'s record — but only while it still points at this
    /// tunnel. The app VM may have moved to another host, whose sync already
    /// pointed the name there; deleting it then would take the site down.
    pub async fn delete_dns_if_ours(&self, hostname: &str, tunnel_id: &str) -> Result<()> {
        if let Some((id, content)) = self.dns_record(hostname).await? {
            if content == tunnel_target(tunnel_id) {
                self.call(reqwest::Method::DELETE, &format!("/zones/{}/dns_records/{id}", self.zone), None).await?;
            }
        }
        Ok(())
    }

    async fn dns_record(&self, hostname: &str) -> Result<Option<(String, String)>> {
        let v = self
            .call(reqwest::Method::GET, &format!("/zones/{}/dns_records?name={hostname}", self.zone), None)
            .await?;
        Ok(v.as_array().and_then(|a| a.first()).and_then(|r| {
            Some((r["id"].as_str()?.to_string(), r["content"].as_str().unwrap_or_default().to_string()))
        }))
    }

    /// Remove a host's hostname record (if it is ours) and its tunnel. Best
    /// effort, record first, so the name stops resolving before the thing it
    /// pointed at disappears.
    pub async fn teardown(&self, tunnel: &str, hostname: Option<&str>) {
        let Ok(Some(id)) = self.find_tunnel(tunnel).await else { return };
        if let Some(h) = hostname {
            let _ = self.delete_dns_if_ours(h, &id).await;
        }
        // A tunnel with live connections cannot be deleted, and a connector
        // that has not noticed its container is gone would block it for minutes.
        let _ = self
            .call(reqwest::Method::DELETE, &format!("/accounts/{}/cfd_tunnel/{id}/connections", self.account), None)
            .await;
        let _ = self.call(reqwest::Method::DELETE, &format!("/accounts/{}/cfd_tunnel/{id}", self.account), None).await;
    }
}

fn tunnel_target(tunnel_id: &str) -> String {
    format!("{tunnel_id}.cfargotunnel.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnels_are_named_for_their_host() {
        assert_eq!(tunnel_name("yak-01"), "huntwell-host-yak-01");
        assert_eq!(tunnel_target("abc"), "abc.cfargotunnel.com");
    }
}

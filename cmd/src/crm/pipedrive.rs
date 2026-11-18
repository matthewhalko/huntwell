//! Pipedrive (2026-10-13).
//!
//! Auth is a personal API token (Pipedrive: Settings → Personal preferences →
//! API), sent as the `api_token` query parameter Pipedrive documents. Requests
//! go to the account's own `https://{company_domain}.pipedrive.com`, learnt at
//! connect time from `GET /v1/users/me` — so the token is all a person pastes.
//!
//! One contact becomes:
//! - an **organization**, found by exact name or created (when there is a company);
//! - a **person**, matched by exact email when one exists in Pipedrive already,
//!   else created with the email, phone and organization;
//! - a **note** on the person (or the organization, for a company-only row)
//!   with everything else Huntwell knows, escaped;
//! - optionally a **lead** (`create_lead` in the connection's settings).
//!
//! Persons and organizations use API v2; notes and leads, v1 (their documented
//! endpoints). The token never appears in an error: reqwest errors are taken
//! `without_url`, and Pipedrive's own error text is passed on bounded.

use std::time::Duration;

use serde_json::{json, Value};

use super::{Contact, PushError, Pushed};

/// What `connect` learnt about the account behind a token.
#[derive(Debug, Clone)]
pub struct Account {
    pub company_domain: String,
    pub company_name: String,
    pub user_name: String,
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("HuntwellBot (+crm)")
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// A Pipedrive company domain, as its subdomain: letters, digits and dashes
/// only, so the URL built from it can only ever be `*.pipedrive.com`.
pub fn domain(raw: &str) -> Option<String> {
    let d = raw.trim().to_ascii_lowercase();
    let d = d.trim_start_matches("https://").trim_start_matches("http://");
    let d = d.split('/').next().unwrap_or("");
    let d = d.strip_suffix(".pipedrive.com").unwrap_or(d);
    (!d.is_empty() && d.len() <= 63 && d.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !d.starts_with('-'))
        .then(|| d.to_string())
}

/// A stand-in Pipedrive for local testing (`HUNTWELL_PIPEDRIVE_API_BASE`),
/// honoured only on a dev server — a deployment always talks to pipedrive.com.
fn dev_base() -> Option<String> {
    crate::config::is_dev()
        .then(|| crate::config::get("HUNTWELL_PIPEDRIVE_API_BASE"))
        .flatten()
        .map(|b| b.trim().trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
}

fn base(settings: &Value) -> Result<String, PushError> {
    if let Some(b) = dev_base() {
        return Ok(b);
    }
    let d = settings.get("company_domain").and_then(Value::as_str).and_then(domain).ok_or_else(|| PushError::Auth("the Pipedrive account's domain is missing — reconnect it".into()))?;
    Ok(format!("https://{d}.pipedrive.com"))
}

/// Pipedrive's answer as JSON, or the failure as a [`PushError`].
async fn send(req: reqwest::RequestBuilder) -> Result<Value, PushError> {
    let resp = req.send().await.map_err(|e| PushError::Retry(format!("could not reach Pipedrive ({})", e.without_url())))?;
    let status = resp.status().as_u16();
    let wait = resp.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(str::to_string);
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        return Ok(body);
    }
    let said: String = body
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| body.get("error_info").and_then(Value::as_str))
        .unwrap_or("")
        .chars()
        .take(200)
        .collect();
    let said = if said.is_empty() { format!("HTTP {status}") } else { said };
    Err(match status {
        401 | 403 => PushError::Auth(format!("Pipedrive refused the API token ({said}) — reconnect with a current token")),
        429 => PushError::Retry(format!("Pipedrive asked us to slow down{}", wait.map(|w| format!(" (retry after {w}s)")).unwrap_or_default())),
        s if s >= 500 => PushError::Retry(format!("Pipedrive had a problem ({said})")),
        _ => PushError::Rejected(format!("Pipedrive refused it: {said}")),
    })
}

/// Check a token and learn the account it belongs to.
pub async fn connect(token: &str) -> Result<Account, PushError> {
    let token = token.trim();
    if token.is_empty() || token.len() > 200 || token.chars().any(|c| c.is_whitespace()) {
        return Err(PushError::Auth("paste the API token from Pipedrive → Settings → Personal preferences → API".into()));
    }
    let root = dev_base().unwrap_or_else(|| "https://api.pipedrive.com".into());
    let me = send(client().get(format!("{root}/v1/users/me")).query(&[("api_token", token)])).await?;
    let d = &me["data"];
    let company_domain = d.get("company_domain").and_then(Value::as_str).and_then(domain).ok_or_else(|| PushError::Rejected("Pipedrive did not say which company account this token is for".into()))?;
    Ok(Account {
        company_domain,
        company_name: d.get("company_name").and_then(Value::as_str).unwrap_or("").to_string(),
        user_name: d.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
    })
}

fn id_of(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// The first exact match from a v2 search, if any.
fn first_hit(v: &Value) -> Option<String> {
    v.pointer("/data/items/0/item/id").and_then(id_of)
}

pub async fn push(token: &str, settings: &Value, c: &Contact) -> Result<Pushed, PushError> {
    let base = base(settings)?;
    let http = client();
    let auth = [("api_token", token.trim())];
    if c.name.trim().is_empty() && c.company.trim().is_empty() {
        return Err(PushError::Rejected("no name or company to create a contact from".into()));
    }
    let mut out = Pushed::default();

    // The organization: found by exact name, else created.
    if !c.company.trim().is_empty() {
        let name: String = c.company.trim().chars().take(255).collect();
        let found = send(
            http.get(format!("{base}/api/v2/organizations/search"))
                .query(&auth)
                .query(&[("term", name.as_str()), ("fields", "name"), ("exact_match", "true"), ("limit", "1")]),
        )
        .await?;
        out.org_id = match first_hit(&found) {
            Some(id) => id,
            None => {
                let made = send(http.post(format!("{base}/api/v2/organizations")).query(&auth).json(&json!({ "name": name }))).await?;
                made.pointer("/data/id").and_then(id_of).ok_or_else(|| PushError::Retry("Pipedrive created the organization but returned no id".into()))?
            }
        };
    }

    // The person: matched by exact email, else created.
    if !c.name.trim().is_empty() {
        if !c.email.is_empty() {
            let found = send(
                http.get(format!("{base}/api/v2/persons/search"))
                    .query(&auth)
                    .query(&[("term", c.email.as_str()), ("fields", "email"), ("exact_match", "true"), ("limit", "1")]),
            )
            .await?;
            if let Some(id) = first_hit(&found) {
                out.person_id = id;
                out.matched = true;
            }
        }
        if out.person_id.is_empty() {
            let mut person = json!({ "name": c.name.trim().chars().take(255).collect::<String>() });
            if !c.email.is_empty() {
                person["emails"] = json!([{ "value": c.email, "primary": true, "label": "work" }]);
            }
            if !c.phone.trim().is_empty() {
                person["phones"] = json!([{ "value": c.phone.trim().chars().take(100).collect::<String>(), "primary": true, "label": "work" }]);
            }
            if let Ok(org) = out.org_id.parse::<i64>() {
                person["org_id"] = json!(org);
            }
            let made = send(http.post(format!("{base}/api/v2/persons")).query(&auth).json(&person)).await?;
            out.person_id = made.pointer("/data/id").and_then(id_of).ok_or_else(|| PushError::Retry("Pipedrive created the person but returned no id".into()))?;
        }
    }

    // The note: on the person, or the organization for a company-only row.
    // A person matched by email already has their history; Huntwell's note is
    // added anyway, since it says where and why they turned up again.
    let mut note = json!({ "content": super::note_html(c) });
    if let Ok(p) = out.person_id.parse::<i64>() {
        note["person_id"] = json!(p);
    }
    if let Ok(o) = out.org_id.parse::<i64>() {
        note["org_id"] = json!(o);
    }
    send(http.post(format!("{base}/v1/notes")).query(&auth).json(&note)).await?;

    // A lead, when the connection asks for one and this is not someone already there.
    let want_lead = settings.get("create_lead").and_then(Value::as_bool).unwrap_or(false);
    if want_lead && !out.matched {
        let mut lead = json!({ "title": c.display().chars().take(255).collect::<String>() });
        if !out.person_id.is_empty() {
            lead["person_id"] = json!(out.person_id.parse::<i64>().unwrap_or_default());
        }
        if !out.org_id.is_empty() {
            lead["organization_id"] = json!(out.org_id.parse::<i64>().unwrap_or_default());
        }
        let made = send(http.post(format!("{base}/v1/leads")).query(&auth).json(&lead)).await?;
        out.lead_id = made.pointer("/data/id").and_then(id_of).unwrap_or_default();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_pipedrive_subdomain_is_a_domain() {
        assert_eq!(domain("acme").as_deref(), Some("acme"));
        assert_eq!(domain("https://Acme-Co.pipedrive.com/").as_deref(), Some("acme-co"));
        for bad in ["", "acme.evil.com/x?", "acme.pipedrive.com.evil", "a b", "-acme", "acme@evil"] {
            assert_eq!(domain(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_search_hit_is_its_item_id() {
        let v = json!({ "success": true, "data": { "items": [{ "result_score": 1.0, "item": { "id": 42, "name": "Acme" } }] } });
        assert_eq!(first_hit(&v).as_deref(), Some("42"));
        assert_eq!(first_hit(&json!({ "data": { "items": [] } })), None);
    }
}

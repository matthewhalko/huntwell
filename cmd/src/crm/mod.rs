//! Pushing search results into a CRM (2026-10-13).
//!
//! A workspace connects a CRM (`crm_connection`); a plan sends its results to
//! it (`plan_crm`); the website's sync loop (`web::crm::spawn_sync`) pushes
//! every result not yet in the ledger (`crm_push`). Each result becomes a
//! [`Contact`] first — the same shape whichever CRM it goes to — and an
//! adapter turns that into the CRM's own records.
//!
//! Adding a CRM: a file beside `pipedrive.rs` with `connect` (check a
//! credential, describe the account) and `push` (one contact), and a line in
//! [`Provider`]. Nothing else changes.
//!
//! Everything in a contact was scraped from the web. Adapters send it as
//! field values and escaped note text, never as anything a CRM would act on.

pub mod pipedrive;

use serde_json::Value;

use crate::store::{ArtifactRow, ProspectRow};

/// A result as a CRM contact. Every field may be empty except one of `name`
/// and `company`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Contact {
    pub name: String,
    pub email: String,
    pub phone: String,
    pub title: String,
    pub company: String,
    pub website: String,
    pub linkedin: String,
    pub location: String,
    pub notes: String,
    /// The page Huntwell found them on.
    pub source_url: String,
    /// The plan that found them — said in the note, so the CRM shows why.
    pub plan: String,
    /// A custom-columns plan's other columns, in order, as (label, value).
    pub extra: Vec<(String, String)>,
}

impl Contact {
    pub fn from_prospect(p: &ProspectRow) -> Contact {
        Contact {
            name: p.name.trim().to_string(),
            email: crate::outreach::find_email(&p.email).unwrap_or_default(),
            phone: p.phone.trim().to_string(),
            title: p.title.trim().to_string(),
            company: p.company.trim().to_string(),
            website: p.website.trim().to_string(),
            linkedin: crate::outreach::linkedin_url(&p.linkedin).unwrap_or_default(),
            location: p.location.trim().to_string(),
            notes: p.notes.trim().to_string(),
            source_url: String::new(),
            plan: p.source.clone(),
            extra: Vec::new(),
        }
    }

    /// A custom-columns row: who it is to is read from its columns by name
    /// (`outreach::recipient_from_record`), a phone from a phone-like column,
    /// and every column goes along as `extra`.
    pub fn from_artifact(a: &ArtifactRow, cols: &[crate::artifact::FieldSpec]) -> Contact {
        let who = crate::outreach::recipient_from_record(&a.title, &a.fields, cols);
        let mut extra = Vec::new();
        let mut phone = String::new();
        let mut website = String::new();
        for f in cols {
            let label = if f.label.trim().is_empty() { f.key.clone() } else { f.label.clone() };
            let v = match a.fields.get(&f.key) {
                Some(Value::String(s)) => s.trim().to_string(),
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::Bool(b)) => b.to_string(),
                _ => String::new(),
            };
            if v.is_empty() {
                continue;
            }
            let l = label.to_ascii_lowercase();
            if phone.is_empty() && (l.contains("phone") || l.contains("tel") || l.contains("mobile")) {
                phone = v.clone();
            }
            if website.is_empty() && (l.contains("website") || l == "site" || l.contains("homepage")) {
                website = v.clone();
            }
            extra.push((label, v));
        }
        Contact {
            name: who.name,
            email: who.email,
            phone,
            title: who.title,
            company: who.company,
            website,
            linkedin: who.linkedin,
            location: String::new(),
            notes: String::new(),
            source_url: a.url.trim().to_string(),
            plan: a.source.clone(),
            extra,
        }
    }

    /// Something to call this contact by, for a lead's title or a log line.
    pub fn display(&self) -> String {
        match (self.name.is_empty(), self.company.is_empty()) {
            (false, false) => format!("{} — {}", self.name, self.company),
            (false, true) => self.name.clone(),
            (true, false) => self.company.clone(),
            (true, true) => "(unnamed)".into(),
        }
    }
}

/// What a push came to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pushed {
    /// True when the person was already in the CRM (matched by email).
    pub matched: bool,
    pub person_id: String,
    pub org_id: String,
    pub lead_id: String,
}

#[derive(Debug)]
pub enum PushError {
    /// The CRM refused the credential: stop pushing for this connection
    /// until someone reconnects it.
    Auth(String),
    /// Busy or briefly down: try again later, nothing is wrong with the row.
    Retry(String),
    /// This contact was refused (bad data, say): record it, move on.
    Rejected(String),
}

impl PushError {
    pub fn message(&self) -> &str {
        match self {
            PushError::Auth(m) | PushError::Retry(m) | PushError::Rejected(m) => m,
        }
    }
}

/// The CRMs Huntwell can push to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Pipedrive,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Provider> {
        match s {
            "pipedrive" => Some(Provider::Pipedrive),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Pipedrive => "pipedrive",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Pipedrive => "Pipedrive",
        }
    }
}

/// Push one contact with a connection's credential and settings.
pub async fn push(provider: Provider, credential: &str, settings: &Value, c: &Contact) -> Result<Pushed, PushError> {
    match provider {
        Provider::Pipedrive => pipedrive::push(credential, settings, c).await,
    }
}

/// HTML-escape for a note body: scraped text, shown as text.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A link in a note only when it is plainly http(s); otherwise the text.
pub fn note_link(url: &str) -> String {
    let u = url.trim();
    let ok = (u.starts_with("https://") || u.starts_with("http://"))
        && !u.chars().any(|c| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''))
        && u.len() <= 1500;
    if ok {
        format!("<a href=\"{}\">{}</a>", escape_html(u), escape_html(u))
    } else {
        escape_html(u)
    }
}

/// The note a contact carries into the CRM: where it came from and every fact
/// Huntwell has, so nothing is lost to fields the CRM has no place for.
pub fn note_html(c: &Contact) -> String {
    let mut lines = vec![format!("<p><b>Found by Huntwell</b> — {}</p>", escape_html(&c.plan))];
    let mut row = |label: &str, value: String| {
        if !value.trim().is_empty() {
            lines.push(format!("<p><b>{}:</b> {}</p>", escape_html(label), value));
        }
    };
    row("Title", escape_html(&c.title));
    row("Location", escape_html(&c.location));
    row("Website", note_link(&c.website));
    row("LinkedIn", note_link(&c.linkedin));
    row("Found on", note_link(&c.source_url));
    for (label, value) in &c.extra {
        let v = if value.starts_with("http://") || value.starts_with("https://") { note_link(value) } else { escape_html(value) };
        row(label, v);
    }
    row("Notes", escape_html(&c.notes));
    let html = lines.join("");
    // Pipedrive's limit on a note is generous; bounded anyway.
    if html.len() > 60_000 {
        html.chars().take(60_000).collect()
    } else {
        html
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_custom_row_becomes_a_contact_by_its_column_names() {
        let cols = crate::artifact::columns_to_schema(&[
            crate::artifact::ColumnRequest { name: "Practice name".into(), prompt: String::new() },
            crate::artifact::ColumnRequest { name: "Owner".into(), prompt: String::new() },
            crate::artifact::ColumnRequest { name: "Contact email".into(), prompt: String::new() },
            crate::artifact::ColumnRequest { name: "Phone".into(), prompt: String::new() },
            crate::artifact::ColumnRequest { name: "Website".into(), prompt: String::new() },
        ]);
        let k = |i: usize| cols[i].key.clone();
        let a = ArtifactRow {
            artifact_id: 1,
            plan_id: 1,
            source_key: "k".into(),
            title: "Bright Smiles".into(),
            url: "https://dir.example/bright".into(),
            fields: serde_json::json!({ k(0): "Bright Smiles Dental", k(1): "Dr. Ana Ruiz", k(2): "ana@brightsmiles.example", k(3): "(330) 555-0100", k(4): "https://brightsmiles.example" }),
            source: "Dental practices".into(),
            first_seen_utc: chrono::Utc::now(),
            last_seen_utc: chrono::Utc::now(),
        };
        let c = Contact::from_artifact(&a, &cols);
        assert_eq!((c.name.as_str(), c.company.as_str(), c.email.as_str()), ("Dr. Ana Ruiz", "Bright Smiles Dental", "ana@brightsmiles.example"));
        assert_eq!((c.phone.as_str(), c.website.as_str(), c.source_url.as_str()), ("(330) 555-0100", "https://brightsmiles.example", "https://dir.example/bright"));
        assert_eq!(c.extra.len(), 5);
    }

    #[test]
    fn a_note_escapes_what_was_scraped() {
        let c = Contact {
            name: "Eve".into(),
            plan: "Plan <b>".into(),
            title: "<script>alert(1)</script>".into(),
            website: "javascript:alert(1)".into(),
            linkedin: "https://linkedin.com/in/eve".into(),
            ..Default::default()
        };
        let n = note_html(&c);
        assert!(!n.contains("<script>") && n.contains("&lt;script&gt;"), "{n}");
        assert!(n.contains("Plan &lt;b&gt;"));
        assert!(!n.contains("href=\"javascript"), "only http(s) becomes a link: {n}");
        assert!(n.contains("<a href=\"https://linkedin.com/in/eve\">"));
    }
}

//! Posting a plan's new results into a Slack channel.
//!
//! Through an incoming webhook, which can only post messages: no files. So a
//! run with more new rows than the plan's limit posts a summary and a link to
//! the plan instead of the rows themselves (layout 'auto'); layout 'all' posts
//! every row, over as many messages as that takes.
//!
//! A run never talks to Slack. It queues a `slack_outbox` row
//! (`store::queue_slack_post`), and the notification service — on the app VM,
//! the only place the sealed webhook can be opened — renders and sends it with
//! the same retry-and-backoff as mail.
//!
//! Everything posted is scraped data, so all of it is escaped for Slack's
//! mrkdwn, and a scraped URL only becomes a link when it is plainly http(s).
//! The one destination we ever send to is `https://hooks.slack.com/services/…`,
//! checked when the webhook is saved and again when it is opened, and the
//! client follows no redirects.

use std::time::Duration;

use serde_json::{json, Value};

use crate::store::{self, ArtifactRow, Db, ProspectRow, SlackPost, SourceConfig};

/// The layouts a plan can pick.
pub const LAYOUTS: [&str; 2] = ["auto", "all"];
/// Rows in one message under 'auto'. Slack allows 50 blocks a message, and a
/// post needs a few of its own.
pub const MAX_LIMIT: i32 = 40;
pub const DEFAULT_LIMIT: i32 = 10;
/// Rows a summary shows before its link.
const PREVIEW_ROWS: usize = 5;
/// Rows per message under 'all', and the most messages one run posts. Past
/// that the last message says how many more there are and links to the plan.
const ROWS_PER_MESSAGE: usize = 40;
const MAX_MESSAGES: usize = 10;
/// Slack cuts a section's text at 3000 characters; a row stays under it.
const ROW_CHARS: usize = 2_800;
/// Rows read for one post; far more than MAX_MESSAGES can carry.
const MAX_ROWS: i64 = 1_000;
/// Slack asks for no more than one message a second per webhook.
const PART_GAP: Duration = Duration::from_millis(1_100);

// ---- the webhook -------------------------------------------------------------

/// Accept only a Slack incoming-webhook URL, normalised. Anything else is
/// refused outright — this is the whole of the SSRF defence, so it is strict:
/// https, exactly `hooks.slack.com`, no port, no credentials, no query.
pub fn validate_webhook(raw: &str) -> Result<String, &'static str> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("paste the webhook URL Slack gave you");
    }
    if raw.len() > 300 {
        return Err("that is too long to be a Slack webhook URL");
    }
    let bad = "that is not a Slack incoming-webhook URL — it starts with https://hooks.slack.com/services/";
    let url = reqwest::Url::parse(raw).map_err(|_| bad)?;
    let path_ok = url.path().starts_with("/services/") && url.path().len() > "/services/".len();
    if url.scheme() != "https"
        || url.host_str() != Some("hooks.slack.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !path_ok
    {
        return Err(bad);
    }
    Ok(url.to_string())
}

/// Seal a validated webhook for storage. `None` when this server has no key
/// to seal with.
pub fn seal_webhook(url: &str) -> Option<String> {
    crate::web::signing::seal_secret(url)
}

/// Open a stored webhook, and check it again: a row is not trusted to still
/// hold what validation let in.
pub fn open_webhook(sealed: &str) -> Option<String> {
    let url = crate::web::signing::open_secret(sealed)?;
    validate_webhook(&url).ok()
}

/// What the settings page shows of a saved webhook: enough to recognise it,
/// not enough to post with.
pub fn hint(sealed: &str) -> String {
    match open_webhook(sealed) {
        Some(url) => {
            let tail: String = url.trim_end_matches('/').chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
            format!("hooks.slack.com/services/…{tail}")
        }
        None if sealed.is_empty() => String::new(),
        None => "saved".into(),
    }
}

// ---- mrkdwn --------------------------------------------------------------------

/// Slack's three control characters. With `<` escaped, nothing scraped can
/// become a mention (`<!channel>`), a user ping or a link.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// One line, bounded, escaped.
fn clip(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let out = if flat.chars().count() > max { format!("{}…", flat.chars().take(max).collect::<String>()) } else { flat };
    escape(&out)
}

/// A scraped URL Slack may render as a link: plainly http(s), with none of
/// the characters that would end the `<url|label>` form early.
fn safe_url(u: &str) -> bool {
    let u = u.trim();
    (u.starts_with("https://") || u.starts_with("http://"))
        && u.len() <= 1_500
        && !u.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '|'))
        && reqwest::Url::parse(u).map(|p| p.host_str().is_some()).unwrap_or(false)
}

fn link(url: &str, label: &str) -> String {
    let label = clip(label, 150).replace('|', "¦");
    if safe_url(url) {
        format!("<{}|{}>", url.trim(), label)
    } else {
        label
    }
}

fn bounded(s: String) -> String {
    if s.chars().count() > ROW_CHARS {
        format!("{}…", s.chars().take(ROW_CHARS).collect::<String>())
    } else {
        s
    }
}

// ---- rows ----------------------------------------------------------------------

/// A person or company: who, then how to reach them.
pub fn prospect_line(r: &ProspectRow) -> String {
    let name = if r.name.trim().is_empty() { "(no name)" } else { r.name.as_str() };
    let mut head = format!("*{}*", clip(name, 150));
    let role: Vec<&str> = [r.title.as_str(), r.company.as_str()].into_iter().filter(|s| !s.trim().is_empty()).collect();
    if !role.is_empty() {
        head.push_str(" — ");
        head.push_str(&clip(&role.join(", "), 200));
    }
    let mut facts: Vec<String> = Vec::new();
    for s in [&r.location, &r.email, &r.phone] {
        if !s.trim().is_empty() {
            facts.push(clip(s, 120));
        }
    }
    if !r.website.trim().is_empty() {
        facts.push(link(&r.website, "Website"));
    }
    if !r.linkedin.trim().is_empty() {
        facts.push(link(&r.linkedin, "LinkedIn"));
    }
    if let Some(v) = r.estimated_value.filter(|v| *v > 0) {
        facts.push(format!("est. ${}", thousands(v)));
    }
    if facts.is_empty() {
        head
    } else {
        bounded(format!("{head}\n{}", facts.join(" · ")))
    }
}

/// A custom-schema row: its title (linked to its page when that is safe),
/// then the plan's own columns.
pub fn artifact_line(r: &ArtifactRow, cols: &[crate::artifact::FieldSpec]) -> String {
    let values: Vec<(String, String)> = cols
        .iter()
        .map(|f| (if f.label.trim().is_empty() { f.key.clone() } else { f.label.clone() }, cell(&r.fields, &f.key)))
        .filter(|(_, v)| !v.trim().is_empty())
        .collect();
    let title = if !r.title.trim().is_empty() {
        r.title.clone()
    } else {
        values.first().map(|(_, v)| v.clone()).unwrap_or_else(|| "(untitled)".into())
    };
    let mut out = format!("*{}*", link(&r.url, &title));
    for (label, value) in values.iter().filter(|(_, v)| v.trim() != title.trim()).take(8) {
        out.push_str(&format!("\n*{}:* {}", clip(label, 60), clip(value, 200)));
    }
    bounded(out)
}

fn cell(fields: &Value, key: &str) -> String {
    match fields.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    }
}

fn thousands(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

// ---- messages ------------------------------------------------------------------

/// Everything one post is made of, already rendered to mrkdwn.
pub struct Post {
    pub plan: String,
    /// `{public url}/app/plans/{id}`, when this server knows its public URL.
    pub link: Option<String>,
    /// "results", "rows", "reports", "files".
    pub noun: &'static str,
    pub rows: Vec<String>,
    pub layout: String,
    pub limit: i32,
}

fn section(text: String) -> Value {
    json!({ "type": "section", "text": { "type": "mrkdwn", "text": text } })
}

fn context(text: String) -> Value {
    json!({ "type": "context", "elements": [{ "type": "mrkdwn", "text": text }] })
}

fn button(link: &str, label: String) -> Value {
    json!({ "type": "actions", "elements": [{ "type": "button", "text": { "type": "plain_text", "text": label }, "url": link }] })
}

fn message(fallback: &str, blocks: Vec<Value>) -> Value {
    // Unfurling would have Slack fetch every scraped link and fill the channel
    // with previews of them.
    json!({ "text": fallback, "blocks": blocks, "unfurl_links": false, "unfurl_media": false })
}

/// The messages a post goes out as, in order. Empty when there is nothing new.
pub fn messages(p: &Post) -> Vec<Value> {
    let total = p.rows.len();
    if total == 0 {
        return Vec::new();
    }
    let noun = if total == 1 { p.noun.trim_end_matches('s') } else { p.noun };
    let plan = match &p.link {
        Some(l) => link(l, &p.plan),
        None => clip(&p.plan, 150),
    };
    let header = section(format!("*{plan}* found *{total} new {noun}*"));
    let fallback = format!("{}: {total} new {noun}", clip(&p.plan, 150));
    let view_all = |n: usize| p.link.as_ref().map(|l| button(l, format!("View all {n} in Huntwell")));

    // Over the limit: a summary, and the way to the rest.
    if p.layout != "all" && total > p.limit.clamp(1, MAX_LIMIT) as usize {
        let shown = PREVIEW_ROWS.min(total);
        let mut blocks = vec![header];
        blocks.extend(p.rows.iter().take(shown).cloned().map(section));
        let more = total - shown;
        blocks.push(context(if p.link.is_some() {
            format!("…and {more} more.")
        } else {
            format!("…and {more} more — open the plan in Huntwell to see them all.")
        }));
        blocks.extend(view_all(total));
        return vec![message(&fallback, blocks)];
    }

    let chunks: Vec<&[String]> = p.rows.chunks(ROWS_PER_MESSAGE).collect();
    let parts = chunks.len().min(MAX_MESSAGES);
    let mut out = Vec::with_capacity(parts);
    for (i, rows) in chunks.iter().take(parts).enumerate() {
        let mut blocks = Vec::with_capacity(rows.len() + 3);
        if i == 0 {
            blocks.push(header.clone());
        } else {
            blocks.push(context(format!("*{plan}* — continued ({}/{parts})", i + 1)));
        }
        blocks.extend(rows.iter().cloned().map(section));
        if i + 1 == parts {
            let posted = (parts * ROWS_PER_MESSAGE).min(total);
            if posted < total {
                blocks.push(context(format!("…and {} more in Huntwell.", total - posted)));
                blocks.extend(view_all(total));
            }
        }
        out.push(message(&fallback, blocks));
    }
    out
}

/// What "Send a test message" posts.
pub fn test_message(plan: &str, plan_url: Option<&str>) -> Value {
    let name = match plan_url {
        Some(l) => link(l, plan),
        None => clip(plan, 150),
    };
    message(
        &format!("Huntwell is connected: {}", clip(plan, 150)),
        vec![section(format!(":white_check_mark: Connected. New results from *{name}* will be posted in this channel."))],
    )
}

pub fn plan_link(plan_id: i64) -> Option<String> {
    let base = crate::config::get("HUNTWELL_PUBLIC_URL")?;
    let base = base.trim().trim_end_matches('/');
    (!base.is_empty()).then(|| format!("{base}/app/plans/{plan_id}"))
}

// ---- sending -------------------------------------------------------------------

#[derive(Debug)]
pub enum SendError {
    /// Worth another try: Slack was busy, down, or unreachable. The seconds
    /// Slack asked us to wait, when it said.
    Retry(String, Option<i64>),
    /// Will fail the same way every time; a person has to fix the webhook.
    Permanent(String),
}

impl SendError {
    pub fn message(&self) -> &str {
        match self {
            SendError::Retry(m, _) | SendError::Permanent(m) => m,
        }
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        // The URL was checked; a redirect would send the post somewhere that was not.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("HuntwellBot (+slack)")
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// What a refusal from Slack means to the person who has to fix it.
pub fn explain(status: u16, body: &str) -> String {
    // Slack answers a refusal with a bare code ("no_service"). Anything else —
    // a proxy's page, say — is not repeated to the person.
    let body = body.trim();
    let code = if body.len() <= 40 && !body.is_empty() && body.chars().all(|c| c.is_ascii_lowercase() || c == '_') { body } else { "" };
    match code {
        "no_service" | "no_team" | "invalid_token" | "team_disabled" | "no_service_id" => {
            "Slack says this webhook no longer exists — it may have been removed. Paste a new one.".into()
        }
        "channel_not_found" | "channel_is_archived" => "The channel this webhook posts to is gone or archived. Paste a new webhook.".into(),
        "action_prohibited" | "posting_to_general_channel_denied" => "A Slack admin has blocked this webhook from posting there.".into(),
        "" => format!("Slack refused the message (HTTP {status})."),
        c => format!("Slack refused the message ({c})."),
    }
}

pub async fn send(url: &str, body: &Value) -> Result<(), SendError> {
    let url = validate_webhook(url).map_err(|e| SendError::Permanent(e.to_string()))?;
    let resp = client()
        .post(&url)
        .json(body)
        .send()
        .await
        .map_err(|e| SendError::Retry(if e.is_timeout() { "Slack did not answer in time".into() } else { "could not reach Slack".into() }, None))?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    let wait = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<i64>().ok());
    let text = resp.text().await.unwrap_or_default();
    match status {
        429 => Err(SendError::Retry("Slack asked us to slow down".into(), wait)),
        s if s >= 500 => Err(SendError::Retry(format!("Slack had a problem (HTTP {s})"), wait)),
        s => Err(SendError::Permanent(explain(s, &text))),
    }
}

// ---- the outbox ----------------------------------------------------------------

/// Render a queued post from the rows its run found.
pub async fn render(db: &Db, post: &SlackPost, sc: &SourceConfig, layout: &str, limit: i32) -> anyhow::Result<Vec<Value>> {
    let kind = sc.kind_of().as_str().to_string();
    let (noun, rows): (&'static str, Vec<String>) = match kind.as_str() {
        "artifacts" => {
            let cols = crate::artifact::output_columns(&crate::artifact::parse_schema(&sc.fields_schema_json));
            let rows = store::artifacts_found_between(db, post.account_id, post.plan_id, post.since, post.until, MAX_ROWS).await?;
            ("rows", rows.iter().map(|r| artifact_line(r, &cols)).collect())
        }
        "report" | "assets" => {
            let titles = store::titles_found_between(db, post.account_id, post.plan_id, &kind, post.since, post.until, MAX_ROWS).await?;
            let noun = if kind == "report" { "reports" } else { "files" };
            let untitled = if kind == "report" { "(untitled report)" } else { "(untitled file)" };
            (noun, titles.iter().map(|t| format!("*{}*", clip(if t.trim().is_empty() { untitled } else { t }, 200))).collect())
        }
        _ => {
            let rows = store::prospects_found_between(db, post.account_id, post.plan_id, post.since, post.until, MAX_ROWS).await?;
            ("results", rows.iter().map(prospect_line).collect())
        }
    };
    Ok(messages(&Post { plan: sc.source.clone(), link: plan_link(sc.plan_id), noun, rows, layout: layout.into(), limit }))
}

/// Send every Slack post that is owed and due. Called from the notification
/// service's tick.
pub async fn drain(db: &Db) {
    use crate::notification::{backoff_seconds, CLAIM_LEASE_SECONDS, MAX_ATTEMPTS};
    loop {
        let post = match store::claim_due_slack(db, CLAIM_LEASE_SECONDS).await {
            Ok(Some(p)) => p,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!("claiming due slack posts: {e:#}");
                return;
            }
        };
        let (slack_id, account_id, plan_id) = (post.slack_id, post.account_id, post.plan_id);
        let give_up = move |err: String| async move {
            let _ = store::mark_slack_failed(db, slack_id, &err, None).await;
            let _ = store::record_slack_result(db, account_id, plan_id, &err).await;
            tracing::warn!(slack_id, plan_id, "slack post dropped: {err}");
        };
        let (Ok(Some(sc)), Ok(Some(settings))) = (
            store::get_plan(db, post.account_id, post.plan_id).await,
            store::get_plan_slack(db, post.account_id, post.plan_id).await,
        ) else {
            let _ = store::mark_slack_failed(db, post.slack_id, "plan not found", None).await;
            continue;
        };
        if !settings.enabled || settings.webhook.is_empty() {
            // Turned off after the run queued it: not an error, just not wanted.
            let _ = store::mark_slack_failed(db, post.slack_id, "Slack was turned off for this plan", None).await;
            continue;
        }
        let Some(url) = open_webhook(&settings.webhook) else {
            give_up("The saved webhook can't be read on this server. Paste it again.".into()).await;
            continue;
        };
        let msgs = match render(db, &post, &sc, &settings.layout, settings.limit).await {
            Ok(m) => m,
            Err(e) => {
                let retry = (post.attempts < MAX_ATTEMPTS).then(|| backoff_seconds(post.attempts));
                let _ = store::mark_slack_failed(db, post.slack_id, &format!("{e:#}"), retry).await;
                continue;
            }
        };
        let resume = post.parts_sent.max(0) as usize;
        let mut failure = None;
        for (i, m) in msgs.iter().enumerate().skip(resume) {
            if i > resume {
                tokio::time::sleep(PART_GAP).await;
            }
            match send(&url, m).await {
                Ok(()) => {
                    let _ = store::mark_slack_parts(db, post.slack_id, (i + 1) as i32).await;
                }
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
        match failure {
            None => {
                let _ = store::mark_slack_sent(db, post.slack_id).await;
                let _ = store::record_slack_result(db, post.account_id, post.plan_id, "").await;
                tracing::info!(slack_id = post.slack_id, plan_id = post.plan_id, messages = msgs.len(), "posted to slack");
            }
            Some(SendError::Permanent(m)) => give_up(m).await,
            Some(SendError::Retry(m, wait)) => {
                if post.attempts >= MAX_ATTEMPTS {
                    give_up(format!("{m} — gave up after {} tries", post.attempts)).await;
                } else {
                    let retry = backoff_seconds(post.attempts).max(wait.unwrap_or(0).clamp(0, 3_600));
                    let _ = store::mark_slack_failed(db, post.slack_id, &m, Some(retry)).await;
                    tracing::warn!(slack_id = post.slack_id, attempts = post.attempts, "slack post failed, will retry: {m}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_slack_incoming_webhooks_are_accepted() {
        assert!(validate_webhook("https://hooks.slack.com/services/T000/B000/XXXX").is_ok());
        assert!(validate_webhook("  https://hooks.slack.com/services/T000/B000/XXXX  ").is_ok());
        for bad in [
            "http://hooks.slack.com/services/T000/B000/XXXX",
            "https://hooks.slack.com.evil.test/services/T/B/X",
            "https://evil.test/services/T/B/X",
            "https://hooks.slack.com:8443/services/T/B/X",
            "https://user:pw@hooks.slack.com/services/T/B/X",
            "https://hooks.slack.com/services/T/B/X?redirect=http://169.254.169.254",
            "https://hooks.slack.com/triggers/T/B/X",
            "https://hooks.slack.com/services/",
            "https://169.254.169.254/services/T/B/X",
            "file:///etc/passwd",
            "",
        ] {
            assert!(validate_webhook(bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn scraped_text_cannot_mention_or_link() {
        let r = ProspectRow {
            prospect_id: 1,
            plan_id: 1,
            name: "<!channel> Eve".into(),
            title: "CEO & <@U123>".into(),
            company: "Acme".into(),
            industry: String::new(),
            email: "eve@acme.test".into(),
            email_status: String::new(),
            phone: String::new(),
            website: "https://acme.test/a|<!here>".into(),
            linkedin: "javascript:alert(1)".into(),
            location: String::new(),
            notes: String::new(),
            estimated_value: Some(1_250_000),
            source: String::new(),
            source_key: String::new(),
            first_seen_utc: chrono::Utc::now(),
            last_seen_utc: chrono::Utc::now(),
        };
        let line = prospect_line(&r);
        assert!(!line.contains("<!"), "{line}");
        assert!(!line.contains("<@"), "{line}");
        assert!(line.contains("&lt;!channel&gt; Eve"));
        assert!(line.contains("CEO &amp; &lt;@U123&gt;, Acme"));
        // Neither unsafe URL became a link.
        assert!(!line.contains("<https://acme.test"), "{line}");
        assert!(!line.contains("<javascript"), "{line}");
        assert!(line.contains("est. $1,250,000"));
    }

    #[test]
    fn a_plain_url_becomes_a_link() {
        assert_eq!(link("https://acme.test/about", "Acme | Home"), "<https://acme.test/about|Acme ¦ Home>");
        assert_eq!(link("ftp://acme.test", "Acme"), "Acme");
    }

    fn post(n: usize, layout: &str, limit: i32, link: bool) -> Post {
        Post {
            plan: "Dentists in Ohio".into(),
            link: link.then(|| "https://huntwell.test/app/plans/7".to_string()),
            noun: "results",
            rows: (1..=n).map(|i| format!("*Row {i}*")).collect(),
            layout: layout.into(),
            limit,
        }
    }

    fn blocks(m: &Value) -> &Vec<Value> {
        m["blocks"].as_array().unwrap()
    }

    #[test]
    fn nothing_new_posts_nothing() {
        assert!(messages(&post(0, "auto", 10, true)).is_empty());
    }

    #[test]
    fn up_to_the_limit_is_one_post_with_every_row() {
        let m = messages(&post(10, "auto", 10, true));
        assert_eq!(m.len(), 1);
        assert_eq!(blocks(&m[0]).len(), 11); // header + 10 rows
        assert!(m[0]["text"].as_str().unwrap().contains("10 new results"));
        assert_eq!(m[0]["unfurl_links"], false);
    }

    #[test]
    fn past_the_limit_is_a_summary_and_a_link() {
        let m = messages(&post(11, "auto", 10, true));
        assert_eq!(m.len(), 1);
        let b = blocks(&m[0]);
        // header + 5 rows + "and 6 more" + the button
        assert_eq!(b.len(), 8);
        assert_eq!(b[7]["elements"][0]["url"], "https://huntwell.test/app/plans/7");
        assert_eq!(b[7]["elements"][0]["text"]["text"], "View all 11 in Huntwell");
        // Without a public URL there is no button, and the text says where to look.
        let m = messages(&post(11, "auto", 10, false));
        let b = blocks(&m[0]);
        assert_eq!(b.len(), 7);
        assert!(b[6]["elements"][0]["text"].as_str().unwrap().contains("open the plan in Huntwell"));
    }

    #[test]
    fn all_splits_long_posts_and_stops_at_the_cap() {
        let m = messages(&post(95, "all", 10, true));
        assert_eq!(m.len(), 3);
        assert!(m.iter().all(|x| blocks(x).len() <= 50));
        assert!(blocks(&m[1])[0]["elements"][0]["text"].as_str().unwrap().contains("continued (2/3)"));
        let huge = messages(&post(1_000, "all", 10, true));
        assert_eq!(huge.len(), MAX_MESSAGES);
        let last = blocks(huge.last().unwrap());
        assert!(last.iter().any(|b| b["elements"][0]["text"].as_str().is_some_and(|t| t.contains("600 more"))));
    }

    #[test]
    fn one_new_row_reads_singular() {
        let m = messages(&post(1, "auto", 10, false));
        assert!(m[0]["text"].as_str().unwrap().ends_with("1 new result"));
    }

    #[test]
    fn slack_refusals_read_as_what_to_do() {
        assert!(explain(404, "no_service").contains("Paste a new one"));
        assert!(explain(404, "channel_is_archived").contains("archived"));
        assert_eq!(explain(400, "invalid_payload"), "Slack refused the message (invalid_payload).");
        assert_eq!(explain(502, "<html>oops</html>"), "Slack refused the message (HTTP 502).");
        assert_eq!(explain(400, ""), "Slack refused the message (HTTP 400).");
    }
}

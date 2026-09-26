//! The three emails this product sends, and the one place they are sent from.
//!
//! Email is not HTML: a client may strip a `<style>` block, ignore flexbox, or
//! render the plain-text part instead. So these are built the way email has to
//! be built — tables, inline styles, a light background stated explicitly, and
//! a real text alternative for every message rather than a stripped-tag
//! afterthought.
//!
//! Sending goes through an HTTP provider (Resend's API shape, which several
//! others copy). With no key configured — local development — nothing leaves
//! the machine: the rendered message is logged instead, so the flow can be
//! walked end to end without a mail account, exactly as the Stripe path does.

use anyhow::{bail, Result};
use serde_json::json;

/// A rendered message: both parts, because both are sent.
#[derive(Debug, Clone)]
pub struct Email {
    pub subject: String,
    pub html: String,
    pub text: String,
}

// The app's own tokens (UI/web/src/styles.css, light theme). Email clients
// ignore CSS variables and most of dark mode, so they are spelled out here;
// change them together with the stylesheet.
const BRAND: &str = "#007a5a"; // --accent
const BRAND_DEEP: &str = "#00604a"; // --accent-deep
const BRAND_SOFT: &str = "#e6f3ee"; // --accent-soft
const CHROME: &str = "#3f0e40"; // --chrome: the topbar the wordmark sits on
const INK: &str = "#1d1c1d"; // --text
const MUTED: &str = "#616061"; // --text-2
const RULE: &str = "#e0e0e0"; // --border
const PAPER: &str = "#f8f8f8"; // --bg-2
/// The logo lockup, as an image. Email cannot draw it: no client does gradient
/// text, and Gmail drops web fonts, so a ✦ and a word set in text is always
/// somebody else's star in somebody else's font. This is the topbar's own
/// markup rendered by a browser (UI/web/brand/email-logo.html), and it travels
/// *inside* the message as an inline attachment — so it shows where remote
/// images are blocked, and needs no public URL to be reachable.
const LOGO_PNG: &[u8] = include_bytes!("../assets/email-logo.png");
const LOGO_CID: &str = "huntwell-logo";

/// The app's display face, with the stack it falls back to. Gmail drops the
/// web font and uses the fallbacks; Apple Mail and most others load it.
const FONT: &str = "'Lato',-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";

/// Escapes text going into the HTML part. Every value here comes from a person
/// — a display name, a workspace name — so none of it can be trusted as markup.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The shell every message shares, dressed like the app: the plum topbar with
/// the ✦ wordmark, a white card on the grey ground, and a footer. `body` is
/// already-escaped HTML; `preview` is not, and is escaped here — it carries
/// names too, and the preheader is still the page.
fn layout(preview: &str, body: &str) -> String {
    let preview = esc(preview);
    format!(
        r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="light only"><title>Huntwell</title>
<link href="https://fonts.googleapis.com/css2?family=Lato:wght@400;700;900&display=swap" rel="stylesheet"></head>
<body style="margin:0;padding:0;background:{PAPER};color:{INK};font-family:{FONT};">
<div style="display:none;max-height:0;overflow:hidden;opacity:0;">{preview}</div>
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background:{PAPER};padding:32px 12px;">
<tr><td align="center">
  <table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="max-width:560px;border-radius:10px;overflow:hidden;border:1px solid {RULE};">
    <tr><td style="background:{CHROME};padding:10px 22px;line-height:0;">
      <img src="cid:{LOGO_CID}" width="160" height="40" alt="huntwell" style="display:block;border:0;outline:none;width:160px;height:40px;">
    </td></tr>
    <tr><td style="background:#ffffff;padding:28px 28px 24px;">
      {body}
    </td></tr>
  </table>
  <table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="max-width:560px;">
    <tr><td style="padding:16px 8px 0;color:{MUTED};font-size:12px;line-height:1.6;font-family:{FONT};">
      You're receiving this because someone used this address on <a href="https://huntwell.ai" style="color:{BRAND};text-decoration:underline;">huntwell.ai</a>.
    </td></tr>
  </table>
</td></tr></table>
</body></html>"#
    )
}

/// A button that survives Outlook: a table cell with a background, not a
/// styled anchor.
fn button(label: &str, url: &str) -> String {
    format!(
        r#"<table role="presentation" cellpadding="0" cellspacing="0" style="margin:22px 0 18px;">
  <tr><td style="background:{BRAND};border-radius:8px;">
    <a href="{url}" style="display:inline-block;padding:12px 22px;color:#ffffff;font-size:15px;font-weight:700;text-decoration:none;">{label}</a>
  </td></tr>
</table>"#,
        url = esc(url),
        label = esc(label)
    )
}

/// The line under every button: some clients do not make buttons clickable, and
/// some people paste links rather than click them.
fn fallback(url: &str) -> String {
    format!(
        r#"<p style="margin:0;color:{MUTED};font-size:13px;line-height:1.6;">Or paste this into your browser:<br>
<a href="{u}" style="color:{BRAND};word-break:break-all;">{u}</a></p>"#,
        u = esc(url)
    )
}

fn h1(text: &str) -> String {
    format!(
        r#"<h1 style="margin:0 0 10px;font-family:{FONT};font-size:22px;font-weight:900;letter-spacing:-0.3px;line-height:1.25;color:{INK};">{}</h1>"#,
        esc(text)
    )
}

fn p(text: &str) -> String {
    format!(r#"<p style="margin:0 0 14px;font-family:{FONT};font-size:15px;line-height:1.6;color:{INK};">{}</p>"#, esc(text))
}

/// Sent on signup. Nothing else can happen until this code is typed into the
/// page that asked for it, so the message says one thing and shows one number.
pub fn verification(name: &str, code: &str) -> Email {
    let hello = if name.trim().is_empty() { "Hi".to_string() } else { format!("Hi {}", name.trim()) };
    let code = code.trim();
    let body = format!(
        "{}{}{}{}",
        h1("Your confirmation code"),
        p(&format!("{hello} — enter this code on the page you signed up from and your Huntwell account is ready to use.")),
        format!(
            r#"<table role="presentation" cellpadding="0" cellspacing="0" style="margin:18px 0;"><tr>
  <td style="background:{BRAND_SOFT};border-radius:10px;padding:14px 26px;font-size:34px;font-weight:700;letter-spacing:.28em;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;color:{BRAND_DEEP};">{}</td>
</tr></table>"#,
            esc(code)
        ),
        format!(
            r#"<p style="margin:14px 0 0;color:{MUTED};font-size:13px;line-height:1.6;">It expires in 15 minutes. If you didn't create a Huntwell account, ignore this email — nothing happens without the code.</p>"#
        ),
    );
    Email {
        subject: "Your Huntwell Code".into(),
        html: layout(&format!("Your Huntwell code is {code}."), &body),
        text: format!(
            "{hello} — your Huntwell confirmation code is:\n\n    {code}\n\n\
             Enter it on the page you signed up from. It expires in 15 minutes.\n\
             If you didn't create a Huntwell account, ignore this email.\n"
        ),
    }
}

/// Sent when someone asks to reset their password. Same shape as the sign-up
/// code: one number, a short life, and a line for the person who did not ask.
pub fn password_reset(name: &str, code: &str) -> Email {
    let hello = if name.trim().is_empty() { "Hi".to_string() } else { format!("Hi {}", name.trim()) };
    let code = code.trim();
    let body = format!(
        "{}{}{}{}",
        h1("Reset your password"),
        p(&format!("{hello} — enter this code on the reset page, then choose a new password.")),
        format!(
            r#"<table role="presentation" cellpadding="0" cellspacing="0" style="margin:18px 0;"><tr>
  <td style="background:{BRAND_SOFT};border-radius:10px;padding:14px 26px;font-size:34px;font-weight:700;letter-spacing:.28em;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;color:{BRAND_DEEP};">{}</td>
</tr></table>"#,
            esc(code)
        ),
        format!(
            r#"<p style="margin:14px 0 0;color:{MUTED};font-size:13px;line-height:1.6;">It expires in 15 minutes. If you didn't ask to reset your password, ignore this email — your password stays as it is, and nothing happens without the code.</p>"#
        ),
    );
    Email {
        subject: "Your Huntwell Reset Code".into(),
        html: layout(&format!("Your Huntwell reset code is {code}."), &body),
        text: format!(
            "{hello} — your Huntwell password reset code is:\n\n    {code}\n\n\
             Enter it on the reset page, then choose a new password. It expires in 15 minutes.\n\
             If you didn't ask to reset your password, ignore this email — your password stays as it is.\n"
        ),
    }
}

/// Sent once the address is confirmed. The two steps mirror the in-app setup
/// dialog exactly, so arriving from either side tells the same story.
pub fn welcome(name: &str, app_url: &str) -> Email {
    let hello = if name.trim().is_empty() { "You're in".to_string() } else { format!("You're in, {}", name.trim()) };
    let step = |n: &str, title: &str, text: &str| {
        format!(
            r#"<table role="presentation" cellpadding="0" cellspacing="0" style="margin:0 0 14px;"><tr>
  <td valign="top" width="30" style="padding-top:2px;">
    <div style="width:24px;height:24px;border-radius:12px;background:{PAPER};border:1px solid {RULE};color:{MUTED};font-size:12px;font-weight:800;text-align:center;line-height:24px;">{n}</div>
  </td>
  <td valign="top" style="padding-left:10px;">
    <div style="font-size:15px;font-weight:700;color:{INK};">{title}</div>
    <div style="font-size:14px;line-height:1.55;color:{MUTED};">{text}</div>
  </td>
</tr></table>"#,
            n = esc(n),
            title = esc(title),
            text = esc(text)
        )
    };
    let body = format!(
        "{}{}{}{}{}{}",
        h1(&hello),
        p("Huntwell reads the open web in a real browser and hands back a clean table — or a written report. Two steps and your first list is on its way."),
        step("1", "Add a payment method", "Searches cost money to run, so a card comes first. Nothing is charged until one runs."),
        step("2", "Say what you're looking for", "Plain words are enough: \"treasury leads at Mexican fintechs\". Huntwell works out how to find it."),
        button("Start your first search", app_url),
        format!(
            r#"<p style="margin:6px 0 0;color:{MUTED};font-size:13px;line-height:1.6;">Any search can run again on a schedule and bring back only what's new.</p>"#
        ),
    );
    Email {
        subject: "Welcome! Here's how Huntwell works".into(),
        html: layout("Two steps and your first list is on its way.", &body),
        text: format!(
            "{hello}\n\n\
             Huntwell reads the open web in a real browser and hands back a clean table — or a written report.\n\n\
             1. Add a payment method. Searches cost money to run, so a card comes first. Nothing is charged until one runs.\n\
             2. Say what you're looking for. Plain words are enough: \"treasury leads at Mexican fintechs\".\n\n\
             Start your first search:\n{app_url}\n\n\
             Any search can run again on a schedule and bring back only what's new.\n"
        ),
    }
}

/// Sent when someone is invited to a workspace. It names who invited them and
/// what joining actually gives them — shared plans and shared results — because
/// that is the part worth understanding before clicking.
pub fn invite(workspace: &str, inviter: &str, role: &str, link: &str) -> Email {
    let as_what = if role == "admin" { "an admin" } else { "a member" };
    let inviter = if inviter.trim().is_empty() { "Someone".to_string() } else { inviter.trim().to_string() };
    let body = format!(
        "{}{}{}{}{}{}",
        h1(&format!("Join {workspace} on Huntwell")),
        p(&format!("{inviter} invited you to work in {workspace} as {as_what}.")),
        p("You'll share their search plans and everything those plans find — the same lists, reports and files, kept up to date by the same runs."),
        button(&format!("Join {workspace}"), link),
        fallback(link),
        format!(
            r#"<p style="margin:14px 0 0;color:{MUTED};font-size:13px;line-height:1.6;">This invitation is for this address only and expires in 14 days. Your own account and your own searches stay yours.</p>"#
        ),
    );
    Email {
        subject: format!("{inviter} invited you to {workspace} on Huntwell"),
        html: layout(&format!("Work with {inviter} in {workspace}."), &body),
        text: format!(
            "{inviter} invited you to work in {workspace} on Huntwell as {as_what}.\n\n\
             You'll share their search plans and everything those plans find.\n\n\
             Join {workspace}:\n{link}\n\n\
             This invitation is for this address only and expires in 14 days.\n\
             Your own account and your own searches stay yours.\n"
        ),
    }
}

/// An invitation to create an account, while sign-up is invite-only. Sent when
/// an operator invites someone, or approves them off the waitlist — to the
/// person, the two are the same good news.
pub fn signup_invite(name: &str, link: &str, valid_days: i64) -> Email {
    let hello = if name.trim().is_empty() { "Hi".to_string() } else { format!("Hi {}", name.trim()) };
    let body = format!(
        "{}{}{}{}{}{}",
        h1("You're invited to Huntwell"),
        p(&format!("{hello} — your invitation to Huntwell is ready.")),
        p("Describe who or what you are looking for in a sentence, and Huntwell searches the web for it, keeps the list current, and tells you when something new turns up."),
        button("Create your account", link),
        fallback(link),
        format!(
            r#"<p style="margin:14px 0 0;color:{MUTED};font-size:13px;line-height:1.6;">This invitation is for this address only and expires in {valid_days} days.</p>"#
        ),
    );
    Email {
        subject: "You're invited to Huntwell".into(),
        html: layout("Your invitation to Huntwell is ready.", &body),
        text: format!(
            "{hello} — your invitation to Huntwell is ready.\n\n\
             Describe who or what you are looking for in a sentence, and Huntwell searches the web for it,\n\
             keeps the list current, and tells you when something new turns up.\n\n\
             Create your account:\n{link}\n\n\
             This invitation is for this address only and expires in {valid_days} days.\n"
        ),
    }
}

/// Sent once, when someone first asks to be let in. Says what happens next
/// and promises nothing: they may not be approved.
pub fn waitlist_received(name: &str) -> Email {
    let hello = if name.trim().is_empty() { "Thanks".to_string() } else { format!("Thanks, {}", name.trim()) };
    let body = format!(
        "{}{}{}",
        h1("You're on the list"),
        p(&format!("{hello} — we've got your request to join Huntwell.")),
        p("Huntwell is invite-only while we grow. We read every request, and if we can make room for you we'll email this address a link to create your account. There's nothing else you need to do."),
    );
    Email {
        subject: "You're on the Huntwell waitlist".into(),
        html: layout("We've got your request to join Huntwell.", &body),
        text: format!(
            "{hello} — we've got your request to join Huntwell.\n\n\
             Huntwell is invite-only while we grow. We read every request, and if we can make room for you\n\
             we'll email this address a link to create your account. There's nothing else you need to do.\n"
        ),
    }
}

/// Sent when building a plan did not work.
///
/// Building is the one thing a user waits on without being able to do anything
/// about it, and a plan stuck at "drafting" in a tab they closed is a dead end.
/// The message says what failed and offers the one action that helps — try
/// again — and says nothing about *why*, because the reason is a stack of
/// internal machinery and never the user's fault.
pub fn draft_failed(plan: &str, link: &str) -> Email {
    let body = format!(
        "{}{}{}{}{}",
        h1("We could not finish building your search"),
        p(&format!("Something went wrong while writing the search for {plan}. Nothing you did caused it, and nothing was lost — the plan is still there exactly as you described it.")),
        p("Opening it and choosing Rebuild starts again from the same description."),
        button("Open the plan", link),
        fallback(link),
    );
    Email {
        subject: format!("Could not finish building {plan}"),
        html: layout(&format!("Building {plan} did not finish. You can try again."), &body),
        text: format!(
            "Something went wrong while writing the search for {plan}.\n\n\
             Nothing you did caused it, and nothing was lost — the plan is still there \
             exactly as you described it. Open it and choose Rebuild to start again.\n\n\
             {link}\n"
        ),
    }
}

/// Sent when a run stored rows the plan had not seen before.
///
/// The point of the message is the number and a way in, so both are above the
/// fold; the sample rows are there to make it obvious at a glance whether the
/// run found the right sort of thing, without opening anything. Nothing about
/// what the plan is *for* is in here — a subject line that reads "3 new rows in
/// Reno Crosstrek listings" is enough context, and email is not a private place
/// to put someone's scraped data.
pub fn new_records(plan: &str, noun: &str, found: i64, total: i64, samples: &[String], link: &str) -> Email {
    let n = found.max(0);
    let one = n == 1;
    let headline = format!("{n} new {} in {plan}", if one { noun.trim_end_matches('s').to_string() } else { noun.to_string() });
    let sample_html = if samples.is_empty() {
        String::new()
    } else {
        let items = samples
            .iter()
            .map(|s| {
                format!(
                    r#"<tr><td style="padding:9px 0;border-top:1px solid {RULE};font-size:14px;line-height:1.5;color:{INK};">{}</td></tr>"#,
                    esc(s)
                )
            })
            .collect::<Vec<_>>()
            .join("");
        format!(r#"<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="margin:4px 0 6px;">{items}</table>"#)
    };
    let more = if n > samples.len() as i64 && !samples.is_empty() {
        p(&format!("…and {} more.", n - samples.len() as i64))
    } else {
        String::new()
    };
    let body = format!(
        "{}{}{}{}{}{}",
        h1(&headline),
        p(&format!(
            "Your latest execution added {n} {} that {} not there before. The plan now holds {total}.",
            if one { "row" } else { "rows" },
            if one { "was" } else { "were" }
        )),
        sample_html,
        more,
        button("Open the plan", link),
        format!(
            r#"<p style="margin:14px 0 0;color:{MUTED};font-size:13px;line-height:1.6;">You get this because alerts are on for this plan. Turn them off on the plan itself.</p>"#
        ),
    );
    let sample_text = if samples.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", samples.iter().map(|s| format!("  - {s}")).collect::<Vec<_>>().join("\n"))
    };
    Email {
        subject: headline.clone(),
        html: layout(&format!("{n} added. The plan now holds {total}."), &body),
        text: format!(
            "{headline}\n\n\
             Your latest execution added {n} that were not there before. The plan now holds {total}.\n\
             {sample_text}\n\
             Open the plan:\n{link}\n\n\
             You get this because alerts are on for this plan. Turn them off on the plan itself.\n"
        ),
    }
}

/// Resend's API key (`re_…`). `HUNTWELL_MAIL_API_KEY` is the older name for
/// the same thing and is still read, so an existing install keeps sending.
fn api_key() -> Option<String> {
    ["RESEND_API_KEY", "HUNTWELL_MAIL_API_KEY"]
        .iter()
        .find_map(|k| crate::config::get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()))
}

fn api_base() -> String {
    ["RESEND_API_BASE", "HUNTWELL_MAIL_API_BASE"]
        .iter()
        .find_map(|k| crate::config::get(k).filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| "https://api.resend.com".into())
}

/// The From header. The domain has to be one verified in Resend.
fn from_address() -> String {
    crate::config::get("HUNTWELL_MAIL_FROM").filter(|v| !v.trim().is_empty()).unwrap_or_else(|| "Huntwell <no-reply@huntwell.app>".into())
}

/// Whether mail actually leaves this server.
pub fn configured() -> bool {
    api_key().is_some()
}

/// The request body for Resend. The logo rides along as an inline attachment
/// whenever the HTML refers to it — which is every message built by `layout`.
fn message(to: &str, email: &Email) -> serde_json::Value {
    use base64::Engine;
    let mut body = json!({
        "from": from_address(),
        "to": [to],
        "subject": email.subject,
        "html": email.html,
        "text": email.text,
    });
    if email.html.contains(&format!("cid:{LOGO_CID}")) {
        body["attachments"] = json!([{
            "filename": "huntwell.png",
            "content": base64::engine::general_purpose::STANDARD.encode(LOGO_PNG),
            "content_type": "image/png",
            "content_id": LOGO_CID,
        }]);
    }
    body
}

/// Sends one message through Resend.
pub async fn send(to: &str, email: &Email) -> Result<()> {
    send_keyed(to, email, None).await
}

/// Sends one message through Resend (`POST /emails`).
///
/// `idempotency_key` is for callers that retry: the notification service sends
/// from a queue with a lease, and "sent but could not record it" means the
/// same row is sent again when the lease expires. Resend drops a repeat of a
/// key it has seen in the last 24 hours, so a retry never becomes a second
/// email.
///
/// With no key configured this logs the whole message and succeeds: local
/// development gets the full flow — including the code — with no mail account,
/// and a missing key can never fail a signup or an invitation.
pub async fn send_keyed(to: &str, email: &Email, idempotency_key: Option<&str>) -> Result<()> {
    let Some(key) = api_key() else {
        tracing::info!(to, subject = %email.subject, "mail not configured — message logged instead of sent\n{}", email.text);
        return Ok(());
    };
    let mut req = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?
        .post(format!("{}/emails", api_base().trim_end_matches('/')))
        .bearer_auth(key)
        .json(&message(to, email));
    if let Some(k) = idempotency_key.filter(|k| !k.is_empty()) {
        req = req.header("Idempotency-Key", k);
    }
    let res = req.send().await?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        // Resend answers {"name": "...", "message": "..."}; the message is the
        // useful half — "domain is not verified", "API key is invalid".
        let why = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["message"].as_str().map(String::from))
            .unwrap_or_else(|| body.chars().take(300).collect());
        bail!("Resend refused the message ({status}): {why}");
    }
    let id = serde_json::from_str::<serde_json::Value>(&body).ok().and_then(|v| v["id"].as_str().map(String::from)).unwrap_or_default();
    tracing::info!(to, subject = %email.subject, message_id = %id, "sent via Resend");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both parts carry the link, because either one may be what gets read.
    /// Writes each message to target/mail-samples/ so it can be opened in a
    /// browser: `cargo test dump_mail_samples -- --ignored`.
    #[test]
    #[ignore]
    fn dump_mail_samples() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/mail-samples");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, m) in [
            ("verification", verification("Ada", "482913")),
            ("password_reset", password_reset("Ada", "271044")),
            ("welcome", welcome("Ada", "https://app.example.com/app")),
            ("invite", invite("Acme", "Bob", "member", "https://app.example.com/join/tok")),
        ] {
            std::fs::write(dir.join(format!("{name}.html")), &m.html).unwrap();
        }
    }

    #[test]
    fn every_message_carries_its_link_in_both_parts() {
        let v = verification("Ada", "482913");
        assert!(v.html.contains("482913") && v.text.contains("482913") && !v.subject.contains("482913"));
        let r = password_reset("Ada", "271044");
        assert!(r.html.contains("271044") && r.text.contains("271044") && !r.subject.contains("271044"));
        let i = invite("Alice", "Bob", "member", "https://app.example.com/join/tok");
        assert!(i.html.contains("/join/tok") && i.text.contains("/join/tok"));
        let w = welcome("Ada", "https://app.example.com/app");
        assert!(w.html.contains("/app") && w.text.contains("/app"));
    }

    #[test]
    fn an_invitation_carries_its_link_and_the_waitlist_note_promises_nothing() {
        let link = "https://huntwell.ai/signup?invite=abc123";
        let m = signup_invite("Ada", link, 14);
        assert_eq!(m.subject, "You're invited to Huntwell");
        assert!(m.html.contains(link) && m.text.contains(link));
        assert!(m.text.contains("Hi Ada") && m.text.contains("14 days"));
        let w = waitlist_received("");
        assert_eq!(w.subject, "You're on the Huntwell waitlist");
        assert!(w.text.starts_with("Thanks —"));
        assert!(!w.text.to_lowercase().contains("you will be approved"), "it must not promise a place");
    }

    #[test]
    fn subjects_say_what_the_message_is() {
        assert_eq!(verification("", "000111").subject, "Your Huntwell Code");
        assert_eq!(welcome("", "x").subject, "Welcome! Here's how Huntwell works");
        assert_eq!(invite("Acme", "Bob", "member", "x").subject, "Bob invited you to Acme on Huntwell");
    }

    /// A workspace or display name is somebody's text, and it lands in HTML.
    #[test]
    fn names_cannot_smuggle_markup() {
        let e = invite("<script>alert(1)</script>", "Bob", "member", "https://x.test/join/t");
        assert!(!e.html.contains("<script>"));
        assert!(e.html.contains("&lt;script&gt;"));
    }

    #[test]
    fn a_missing_name_still_reads_as_a_sentence() {
        assert!(verification("", "000111").text.starts_with("Hi — your"));
        assert!(welcome("", "x").text.starts_with("You're in\n"));
        assert!(invite("Acme", "", "member", "x").text.starts_with("Someone invited you"));
    }
}

#[cfg(test)]
mod alert_tests {
    use super::*;

    #[test]
    fn the_count_and_the_plan_are_in_the_subject() {
        let m = new_records("Reno Crosstreks", "rows", 3, 41, &["2019 Crosstrek Premium".into()], "https://x/app/plans/7");
        assert_eq!(m.subject, "3 new rows in Reno Crosstreks");
        assert!(m.text.contains("The plan now holds 41."));
        assert!(m.html.contains("https://x/app/plans/7"));
    }

    #[test]
    fn one_row_reads_as_one_row() {
        let m = new_records("Toyotas", "rows", 1, 1, &[], "https://x");
        assert_eq!(m.subject, "1 new row in Toyotas");
        assert!(m.text.contains("added 1"));
    }

    #[test]
    fn samples_are_escaped_like_everything_else() {
        // A row label is scraped from a page, so it is markup until proven otherwise.
        let m = new_records("P", "rows", 1, 1, &["<script>alert(1)</script>".into()], "https://x");
        assert!(!m.html.contains("<script>"));
        assert!(m.html.contains("&lt;script&gt;"));
    }

    #[test]
    fn the_extra_line_only_appears_when_there_are_more() {
        let two = new_records("P", "rows", 9, 9, &["a".into(), "b".into()], "https://x");
        assert!(two.html.contains("and 7 more"));
        let all = new_records("P", "rows", 2, 2, &["a".into(), "b".into()], "https://x");
        assert!(!all.html.contains("more."));
    }

    /// Against a stand-in for Resend's API: the shape of the call is the
    /// contract, so it is what is checked.
    #[tokio::test]
    async fn a_message_goes_to_resend_with_the_key_and_an_idempotency_key() {
        use axum::{extract::Json as J, http::HeaderMap, routing::post, Router};
        use std::sync::{Arc, Mutex};
        let seen: Arc<Mutex<Vec<(String, String, serde_json::Value)>>> = Arc::default();
        let sink = seen.clone();
        let app = Router::new().route(
            "/emails",
            post(move |h: HeaderMap, J(body): J<serde_json::Value>| {
                let sink = sink.clone();
                async move {
                    let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    if get("authorization") != "Bearer re_test_key" {
                        return (axum::http::StatusCode::UNAUTHORIZED, J(serde_json::json!({"name":"validation_error","message":"API key is invalid"})));
                    }
                    sink.lock().unwrap().push((get("authorization"), get("idempotency-key"), body));
                    (axum::http::StatusCode::OK, J(serde_json::json!({"id":"em_123"})))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        std::env::set_var("RESEND_API_BASE", format!("http://{addr}"));
        std::env::set_var("RESEND_API_KEY", "re_test_key");
        std::env::set_var("HUNTWELL_MAIL_FROM", "Huntwell <hello@example.com>");

        assert!(configured());
        send_keyed("ada@example.com", &verification("Ada", "482913"), Some("huntwell-mail-7")).await.unwrap();
        let got = seen.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, "huntwell-mail-7");
        assert_eq!(got[0].2["from"], "Huntwell <hello@example.com>");
        assert_eq!(got[0].2["to"][0], "ada@example.com");
        assert_eq!(got[0].2["subject"], "Your Huntwell Code");
        assert!(got[0].2["html"].as_str().unwrap().contains("482913") && got[0].2["text"].as_str().unwrap().contains("482913"));
        // The logo travels inside the message, referenced by content id.
        assert!(got[0].2["html"].as_str().unwrap().contains("cid:huntwell-logo"));
        let att = &got[0].2["attachments"][0];
        assert_eq!(att["content_id"], "huntwell-logo");
        assert_eq!(att["content_type"], "image/png");
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD.decode(att["content"].as_str().unwrap()).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "the attachment is the PNG itself");

        // A refused key comes back as Resend's own sentence, not a status code alone.
        std::env::set_var("RESEND_API_KEY", "re_wrong");
        let e = send("ada@example.com", &verification("Ada", "1")).await.unwrap_err().to_string();
        assert!(e.contains("API key is invalid"), "{e}");
        for k in ["RESEND_API_BASE", "RESEND_API_KEY", "HUNTWELL_MAIL_FROM"] {
            std::env::remove_var(k);
        }
    }

}

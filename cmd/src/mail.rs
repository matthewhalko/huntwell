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
<style>
/* Phones: the inline padding is for a desktop reading pane. Clients that
   drop <style> keep it, which still fits; the rest get room back. */
@media only screen and (max-width:480px){{
  .hw-outer{{padding:12px 6px !important}}
  .hw-body{{padding:20px 16px 18px !important}}
  .hw-card{{padding:12px 13px !important}}
}}
</style>
<link href="https://fonts.googleapis.com/css2?family=Lato:wght@400;700;900&display=swap" rel="stylesheet"></head>
<body style="margin:0;padding:0;background:{PAPER};color:{INK};font-family:{FONT};">
<div style="display:none;max-height:0;overflow:hidden;opacity:0;">{preview}</div>
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" class="hw-outer" style="background:{PAPER};padding:32px 12px;">
<tr><td align="center">
  <table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="max-width:560px;border-radius:10px;overflow:hidden;border:1px solid {RULE};">
    <tr><td style="background:{CHROME};padding:10px 22px;line-height:0;">
      <img src="cid:{LOGO_CID}" width="160" height="40" alt="huntwell" style="display:block;border:0;outline:none;width:160px;height:40px;">
    </td></tr>
    <tr><td class="hw-body" style="background:#ffffff;padding:28px 28px 24px;">
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
pub fn password_reset(name: &str, code: &str, link: &str) -> Email {
    let hello = if name.trim().is_empty() { "Hi".to_string() } else { format!("Hi {}", name.trim()) };
    let code = code.trim();
    let hours = crate::store::RESET_VALID_HOURS;
    let body = format!(
        "{}{}{}{}{}{}",
        h1("Reset your password"),
        p(&format!("{hello} — choose a new password for your Huntwell account.")),
        button("Set a new password", link),
        fallback(link),
        format!(
            r#"<p style="margin:22px 0 0;color:{MUTED};font-size:13px;line-height:1.6;">On another device? Use this code on the reset page instead:</p>
<table role="presentation" cellpadding="0" cellspacing="0" style="margin:8px 0 0;"><tr>
  <td style="background:{BRAND_SOFT};border-radius:10px;padding:10px 20px;font-size:26px;font-weight:700;letter-spacing:.28em;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;color:{BRAND_DEEP};">{}</td>
</tr></table>"#,
            esc(code)
        ),
        format!(
            r#"<p style="margin:18px 0 0;color:{MUTED};font-size:13px;line-height:1.6;">The link and the code work once, for {hours} hours. If you didn't ask to reset your password, ignore this email — your password stays as it is.</p>"#
        ),
    );
    Email {
        subject: "Reset your Huntwell password".into(),
        html: layout("Set a new password for your Huntwell account.", &body),
        text: format!(
            "{hello} — choose a new password for your Huntwell account:\n\n{link}\n\n\
             On another device? Use this code on the reset page instead:  {code}\n\n\
             The link and the code work once, for {hours} hours.\n\
             If you didn't ask to reset your password, ignore this email — your password stays as it is.\n"
        ),
    }
}

/// The reset page for one reset: the link's own token, which the page trades
/// for whose account it is. Never the six-digit code — that is short enough to
/// guess, and a link lives in an inbox for a day.
pub fn reset_link(token: &str) -> String {
    format!("{}/forgot?token={}", link_base(), token.trim())
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

/// Sent once, when someone first asks to be let in. Says we will email them
/// for access, and that there is nothing else to do.
pub fn waitlist_received(name: &str) -> Email {
    let hello = if name.trim().is_empty() { "Thanks".to_string() } else { format!("Thanks, {}", name.trim()) };
    let body = format!(
        "{}{}{}",
        h1("You're on the list"),
        p(&format!("{hello} — we've got your request to join Huntwell.")),
        p("Huntwell is invite-only. We will email this address for access. There's nothing else you need to do."),
    );
    Email {
        subject: "You're on the Huntwell waitlist".into(),
        html: layout("We've got your request to join Huntwell.", &body),
        text: format!(
            "{hello} — we've got your request to join Huntwell.\n\n\
             Huntwell is invite-only. We will email this address for access.\n\
             There's nothing else you need to do.\n"
        ),
    }
}

/// An operator answered a support chat while the person was away from it.
/// The reply itself is in the message, so most never need to click; the link
/// opens the chat where they left it.
pub fn support_reply(name: &str, reply: &str, link: &str) -> Email {
    let hello = if name.trim().is_empty() { "Hi".to_string() } else { format!("Hi {}", name.trim()) };
    let quoted = esc(reply.trim()).replace("\r\n", "\n").replace('\n', "<br>");
    let body = format!(
        "{}{}{}{}{}",
        h1("Huntwell support replied"),
        p(&format!("{hello}, here is our answer to your message.")),
        format!(
            r#"<div style="margin:0 0 6px;padding:14px 16px;border-left:3px solid {BRAND};background:{PAPER};border-radius:6px;font-family:{FONT};font-size:15px;line-height:1.6;color:{INK};">{quoted}</div>"#
        ),
        button("Continue the chat", link),
        fallback(link),
    );
    Email {
        subject: "Huntwell support replied".into(),
        html: layout("Here is our answer to your message.", &body),
        text: format!("{hello}, here is our answer to your message.\n\n{}\n\nContinue the chat: {link}\n", reply.trim()),
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

/// One new result in an alert: a title, a line under it, and the plan's own
/// columns. Built from the row by [`AlertCard::from_prospect`] /
/// [`AlertCard::from_artifact`]; rendered by [`card_html`] / [`card_text`].
/// Every value is scraped from the web, so all of it is escaped and bounded,
/// and a value becomes a link only when it is plainly http(s) or an address.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlertCard {
    pub title: String,
    /// The page it was found on, linked from the title when it is safe.
    pub url: String,
    pub subtitle: String,
    pub fields: Vec<(String, String)>,
}

/// Fields shown on one card — enough to judge a row without opening the app.
const CARD_FIELDS: usize = 8;
/// Cards in one alert; the rest are a count and the button.
pub const ALERT_CARDS: usize = 10;

fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        format!("{}…", s.chars().take(max).collect::<String>().trim_end())
    }
}

fn safe_url(u: &str) -> bool {
    let u = u.trim();
    (u.starts_with("https://") || u.starts_with("http://"))
        && u.len() <= 2000
        && !u.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '"' | '\''))
}

fn safe_email(e: &str) -> bool {
    let e = e.trim();
    e.len() <= 254 && e.split_once('@').is_some_and(|(a, d)| !a.is_empty() && d.contains('.')) && e.chars().all(|c| c.is_ascii_alphanumeric() || "@._+-".contains(c))
}

/// A link's text: the host and a short path, which is what a person reads.
fn short_url(u: &str) -> String {
    let rest = u.trim().trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.");
    clip(rest.trim_end_matches('/'), 48)
}

impl AlertCard {
    pub fn from_prospect(r: &crate::store::ProspectRow) -> AlertCard {
        let title = [&r.name, &r.company].iter().map(|s| s.trim()).find(|s| !s.is_empty()).unwrap_or("(no name)").to_string();
        let subtitle = [r.title.trim(), if r.name.trim().is_empty() { "" } else { r.company.trim() }]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let mut fields = Vec::new();
        let mut add = |label: &str, v: &str| {
            if !v.trim().is_empty() {
                fields.push((label.to_string(), v.trim().to_string()));
            }
        };
        add("Email", &r.email);
        add("Phone", &r.phone);
        add("Location", &r.location);
        add("Website", &r.website);
        add("LinkedIn", &r.linkedin);
        add("Industry", &r.industry);
        if let Some(v) = r.estimated_value.filter(|v| *v > 0) {
            add("Est. value", &money(v as f64));
        }
        add("Notes", &r.notes);
        AlertCard { title, url: String::new(), subtitle, fields }
    }

    /// A custom-columns row: its title (else its first column), linked to the
    /// page it came from, then the plan's columns in the plan's order.
    pub fn from_artifact(r: &crate::store::ArtifactRow, cols: &[crate::artifact::FieldSpec]) -> AlertCard {
        let values: Vec<(String, String)> = cols
            .iter()
            .map(|f| {
                let v = match r.fields.get(&f.key) {
                    Some(serde_json::Value::String(s)) => s.trim().to_string(),
                    Some(serde_json::Value::Number(n)) => n.to_string(),
                    Some(serde_json::Value::Bool(b)) => if *b { "Yes" } else { "No" }.to_string(),
                    _ => String::new(),
                };
                (if f.label.trim().is_empty() { f.key.clone() } else { f.label.clone() }, v)
            })
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let title = if r.title.trim().is_empty() { values.first().map(|(_, v)| v.clone()).unwrap_or_else(|| "(untitled)".into()) } else { r.title.trim().to_string() };
        let fields = values.into_iter().filter(|(_, v)| v.trim() != title.trim()).collect();
        AlertCard { title, url: r.url.trim().to_string(), subtitle: String::new(), fields }
    }

    /// A report or a file: its title alone.
    pub fn titled(title: &str) -> AlertCard {
        AlertCard { title: title.trim().to_string(), ..Default::default() }
    }
}

/// One value, as HTML: a link when it is a URL or an address, else text.
fn value_html(v: &str) -> String {
    let v = v.trim();
    if safe_url(v) {
        format!(r#"<a href="{}" style="color:{BRAND};text-decoration:underline;word-break:break-all;">{}</a>"#, esc(v), esc(&short_url(v)))
    } else if safe_email(v) {
        format!(r#"<a href="mailto:{0}" style="color:{BRAND};text-decoration:underline;word-break:break-all;">{0}</a>"#, esc(v))
    } else {
        esc(&clip(v, 240))
    }
}

/// A card that holds up on a phone: one column, nothing fixed-width, a label
/// over its value so a long value wraps under it instead of squeezing beside it.
fn card_html(c: &AlertCard) -> String {
    let title = clip(&c.title, 140);
    let title_html = if safe_url(&c.url) {
        format!(r#"<a href="{}" style="color:{INK};text-decoration:none;">{}</a>"#, esc(&c.url), esc(&title))
    } else {
        esc(&title)
    };
    let subtitle = if c.subtitle.trim().is_empty() {
        String::new()
    } else {
        format!(r#"<div style="margin-top:2px;font-size:13px;line-height:1.45;color:{MUTED};">{}</div>"#, esc(&clip(&c.subtitle, 160)))
    };
    let fields: String = c
        .fields
        .iter()
        .take(CARD_FIELDS)
        .map(|(label, v)| {
            format!(
                r#"<tr><td class="hw-field" style="padding:7px 0 0;font-family:{FONT};">
<div style="font-size:11px;line-height:1.3;font-weight:700;letter-spacing:.04em;text-transform:uppercase;color:{MUTED};">{}</div>
<div style="font-size:14px;line-height:1.45;color:{INK};word-break:break-word;overflow-wrap:anywhere;">{}</div></td></tr>"#,
                esc(&clip(label, 40)),
                value_html(v)
            )
        })
        .collect();
    let more = c.fields.len().saturating_sub(CARD_FIELDS);
    let more = if more > 0 {
        format!(r#"<tr><td style="padding:7px 0 0;font-size:12px;color:{MUTED};">+{more} more in the app</td></tr>"#)
    } else {
        String::new()
    };
    let link = if safe_url(&c.url) {
        format!(
            r#"<tr><td style="padding:10px 0 0;font-size:13px;"><a href="{}" style="color:{BRAND};font-weight:700;text-decoration:none;">View source ↗</a></td></tr>"#,
            esc(&c.url)
        )
    } else {
        String::new()
    };
    format!(
        r#"<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="margin:0 0 12px;border:1px solid {RULE};border-radius:10px;border-collapse:separate;">
<tr><td class="hw-card" style="padding:14px 16px;font-family:{FONT};">
<div style="font-size:16px;line-height:1.35;font-weight:700;color:{INK};word-break:break-word;">{title_html}</div>{subtitle}
<table role="presentation" width="100%" cellpadding="0" cellspacing="0">{fields}{more}{link}</table>
</td></tr></table>"#
    )
}

fn card_text(c: &AlertCard) -> String {
    let mut out = format!("■ {}", clip(&c.title, 140));
    if !c.subtitle.trim().is_empty() {
        out.push_str(&format!("\n  {}", clip(&c.subtitle, 160)));
    }
    for (label, v) in c.fields.iter().take(CARD_FIELDS) {
        out.push_str(&format!("\n  {}: {}", clip(label, 40), clip(v, 240)));
    }
    if safe_url(&c.url) {
        out.push_str(&format!("\n  {}", c.url.trim()));
    }
    out
}

/// Sent when a run stored rows the plan had not seen before.
///
/// The number and a way in come first, then the new rows themselves as cards —
/// each with the plan's columns, so a row can be judged from the inbox, on a
/// phone as well as a desktop (one column, labels over values, nothing wider
/// than the screen). At most [`ALERT_CARDS`]; past that, a count and the button.
/// The rows are the plan owner's own results, sent only to them.
pub fn new_records(plan: &str, noun: &str, found: i64, total: i64, cards: &[AlertCard], link: &str) -> Email {
    let n = found.max(0);
    let one = n == 1;
    let headline = format!("{n} new {} in {plan}", if one { noun.trim_end_matches('s').to_string() } else { noun.to_string() });
    let shown = &cards[..cards.len().min(ALERT_CARDS)];
    let cards_html: String = shown.iter().map(card_html).collect();
    let rest = n - shown.len() as i64;
    let more = if rest > 0 && !shown.is_empty() {
        p(&format!("…and {rest} more in the plan."))
    } else {
        String::new()
    };
    let body = format!(
        "{}{}{}{}{}{}{}",
        h1(&headline),
        p(&format!(
            "Your latest execution added {n} {} that {} not there before. The plan now holds {total}.",
            if one { "row" } else { "rows" },
            if one { "was" } else { "were" }
        )),
        button("Open the plan", link),
        if shown.is_empty() { String::new() } else { format!(r#"<div style="margin:4px 0 0;">{cards_html}</div>"#) },
        more,
        if shown.len() > 3 { button("See them all", link) } else { String::new() },
        format!(
            r#"<p style="margin:14px 0 0;color:{MUTED};font-size:13px;line-height:1.6;">You get this because alerts are on for this plan. Turn them off on the plan itself.</p>"#
        ),
    );
    let cards_text = if shown.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", shown.iter().map(card_text).collect::<Vec<_>>().join("\n\n"))
    };
    let more_text = if rest > 0 && !shown.is_empty() { format!("\n…and {rest} more in the plan.\n") } else { String::new() };
    Email {
        subject: headline.clone(),
        html: layout(&format!("{n} added. The plan now holds {total}."), &body),
        text: format!(
            "{headline}\n\n\
             Your latest execution added {n} that were not there before. The plan now holds {total}.\n\
             {cards_text}{more_text}\n\
             Open the plan:\n{link}\n\n\
             You get this because alerts are on for this plan. Turn them off on the plan itself.\n"
        ),
    }
}

/// Sent when credit lands in a workspace's wallet: a card purchase, or free
/// credit an operator added (`granted`, with the operator's note if any).
/// Amount and new balance up top; the link goes to Usage & billing.
/// How credit came to land, for what the email says about it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CreditKind {
    /// Bought by a person, on the Usage page.
    Purchase,
    /// Added free by an operator.
    Grant,
    /// Charged automatically: the balance fell below `below_usd`.
    AutoReload { below_usd: f64 },
}

pub fn credits_added(usd: f64, balance_usd: f64, kind: CreditKind, note: &str, link: Option<&str>) -> Email {
    let amount = money(usd);
    let balance = money(balance_usd);
    let headline = match kind {
        CreditKind::Grant => format!("{amount} in free credit added"),
        CreditKind::Purchase => format!("{amount} in credit added"),
        CreditKind::AutoReload { .. } => format!("Auto-reload: {amount} in credit added"),
    };
    let lead = match kind {
        CreditKind::Grant => format!("We've added {amount} of credit to your Huntwell account, free of charge. Your balance is now {balance}."),
        CreditKind::Purchase => format!("Your purchase of {amount} in credit went through. Your balance is now {balance}."),
        CreditKind::AutoReload { below_usd } => format!(
            "Your balance fell below {}, so your card on file was charged {amount}, as you set up with auto-reload. Your balance is now {balance}.",
            money(below_usd)
        ),
    };
    let note = if let CreditKind::AutoReload { .. } = kind {
        "You can change the amount or turn auto-reload off any time under Usage & billing."
    } else {
        note
    };
    let note = note.trim();
    let note_html = if note.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div style="margin:0 0 14px;padding:12px 14px;border-left:3px solid {RULE};background:{PAPER};font-family:{FONT};font-size:14px;line-height:1.55;color:{INK};">{}</div>"#,
            esc(note).replace('\n', "<br>")
        )
    };
    let after = "Credit is spent as your searches and drafts run, at your account's per-token rate.";
    let body = format!(
        "{}{}{}{}{}{}",
        h1(&headline),
        p(&lead),
        note_html,
        p(after),
        link.map(|l| button("See usage & billing", l)).unwrap_or_default(),
        link.map(fallback).unwrap_or_default(),
    );
    Email {
        subject: headline.clone(),
        html: layout(&format!("Your balance is now {balance}."), &body),
        text: format!(
            "{headline}\n\n{lead}\n{}\n{after}\n{}",
            if note.is_empty() { String::new() } else { format!("\n{note}\n") },
            link.map(|l| format!("\nUsage & billing:\n{l}\n")).unwrap_or_default(),
        ),
    }
}

/// Auto-reload could not charge the card, and has been switched off so it
/// does not keep trying. Says what happened in our words and what to do.
pub fn auto_reload_failed(usd: f64, reason: &str, link: Option<&str>) -> Email {
    let amount = money(usd);
    let headline = "Auto-reload couldn't charge your card".to_string();
    let lead = format!("We tried to add {amount} of credit with auto-reload, but the charge didn't go through: {reason}.");
    let next = "Auto-reload is now off, so nothing more will be charged. Your searches stop when your credit runs out — update your card or buy credit under Usage & billing, then turn auto-reload back on.";
    let body = format!(
        "{}{}{}{}{}",
        h1(&headline),
        p(&lead),
        p(next),
        link.map(|l| button("Go to usage & billing", l)).unwrap_or_default(),
        link.map(fallback).unwrap_or_default(),
    );
    Email {
        subject: headline.clone(),
        html: layout("Auto-reload is off until you fix the card.", &body),
        text: format!("{headline}\n\n{lead}\n\n{next}\n{}", link.map(|l| format!("\n{l}\n")).unwrap_or_default()),
    }
}

/// `$1,234.50` — cents always shown, so $100 reads as $100.00.
fn money(usd: f64) -> String {
    let cents = (usd * 100.0).round() as i64;
    let (whole, frac) = (cents.abs() / 100, cents.abs() % 100);
    let digits = whole.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{}${grouped}.{frac:02}", if cents < 0 { "-" } else { "" })
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

/// Stands in for `HUNTWELL_PUBLIC_URL` in a message queued by a process that
/// does not have it — a run on a worker VM, which is deliberately never given
/// it. The notification service, which has it, fills it in at send
/// ([`with_public_url`]). Plain ASCII that escaping leaves alone.
pub const PUBLIC_URL_MARK: &str = "{{huntwell_public_url}}";

/// The site's address for a link in a message: this process's
/// `HUNTWELL_PUBLIC_URL`, else [`PUBLIC_URL_MARK`] for the sender to fill.
pub fn link_base() -> String {
    crate::config::get("HUNTWELL_PUBLIC_URL")
        .map(|b| b.trim().trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| PUBLIC_URL_MARK.to_string())
}

/// Fill [`PUBLIC_URL_MARK`] in with `base`. Err when the message needs it and
/// there is none: a message whose only call to action is a dead link is worse
/// than one held back.
pub fn with_public_url(email: &Email, base: Option<&str>) -> std::result::Result<Email, &'static str> {
    if !email.html.contains(PUBLIC_URL_MARK) && !email.text.contains(PUBLIC_URL_MARK) && !email.subject.contains(PUBLIC_URL_MARK) {
        return Ok(email.clone());
    }
    let base = base.map(|b| b.trim().trim_end_matches('/')).filter(|b| !b.is_empty()).ok_or("HUNTWELL_PUBLIC_URL is not set on the notification service, so its link cannot be filled in")?;
    Ok(Email {
        subject: email.subject.replace(PUBLIC_URL_MARK, base),
        html: email.html.replace(PUBLIC_URL_MARK, &esc(base)),
        text: email.text.replace(PUBLIC_URL_MARK, base),
    })
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
    #[test]
    fn a_queued_link_is_filled_in_by_the_sender_or_held() {
        let link = format!("{PUBLIC_URL_MARK}/app/plans/7");
        let m = new_records("Dentists", "results", 2, 9, &[AlertCard::titled("A")], &link);
        assert!(m.html.contains(PUBLIC_URL_MARK) && m.text.contains(PUBLIC_URL_MARK), "escaping must leave the mark intact");
        let sent = with_public_url(&m, Some("https://app.huntwell.example/")).unwrap();
        assert!(sent.html.contains("https://app.huntwell.example/app/plans/7") && sent.text.contains("https://app.huntwell.example/app/plans/7"));
        assert!(!sent.html.contains(PUBLIC_URL_MARK) && !sent.text.contains(PUBLIC_URL_MARK));
        assert!(with_public_url(&m, None).is_err() && with_public_url(&m, Some(" ")).is_err());
        // A message with no mark goes as it is, setting or not.
        let plain = new_records("Dentists", "results", 1, 1, &[], "https://x.example/app/plans/1");
        assert_eq!(with_public_url(&plain, None).unwrap().html, plain.html);
    }

    use super::*;

    #[test]
    fn credit_emails_say_how_much_and_the_new_balance() {
        let m = credits_added(100.0, 112.5, CreditKind::Grant, "Welcome aboard <b>Ana</b>\nEnjoy", Some("https://huntwell.test/app/usage"));
        assert_eq!(m.subject, "$100.00 in free credit added");
        assert!(m.text.contains("Your balance is now $112.50.") && m.text.contains("free of charge"));
        // The operator's note is escaped, and keeps its line breaks.
        assert!(m.html.contains("Welcome aboard &lt;b&gt;Ana&lt;/b&gt;<br>Enjoy"), "{}", m.html);
        assert!(m.html.contains("https://huntwell.test/app/usage"));
        let bought = credits_added(1250.0, 1250.0, CreditKind::Purchase, "", None);
        assert_eq!(bought.subject, "$1,250.00 in credit added");
        assert!(bought.text.contains("Your purchase of $1,250.00"));
        assert!(!bought.html.contains("See usage"), "no link without a public URL");
        let auto = credits_added(50.0, 58.2, CreditKind::AutoReload { below_usd: 10.0 }, "", None);
        assert_eq!(auto.subject, "Auto-reload: $50.00 in credit added");
        assert!(auto.text.contains("fell below $10.00") && auto.text.contains("turn auto-reload off"));
        let failed = auto_reload_failed(50.0, "the card was declined", None);
        assert!(failed.text.contains("$50.00") && failed.text.contains("the card was declined") && failed.text.contains("now off"));
    }

    /// Both parts carry the link, because either one may be what gets read.
    /// Writes each message to target/mail-samples/ so it can be opened in a
    /// browser: `cargo test dump_mail_samples -- --ignored`.
    /// What a run's alert looks like with real-shaped rows: people with every
    /// field, one with few, and a custom-columns row with a long value.
    fn sample_cards() -> Vec<AlertCard> {
        let f = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        vec![
            AlertCard {
                title: "Dr. Ana Ruiz".into(),
                subtitle: "Owner · Bright Smiles Dental".into(),
                fields: f(&[("Email", "ana@brightsmiles.example"), ("Phone", "(330) 555-0100"), ("Location", "Akron, OH"),
                    ("Website", "https://www.brightsmiles.example/"), ("LinkedIn", "https://www.linkedin.com/in/ana-ruiz-dds-4b1a2c"),
                    ("Notes", "Six chairs, opened 2014; hiring a second hygienist according to their careers page.")]),
                ..Default::default()
            },
            AlertCard { title: "Okafor Family Dentistry".into(), fields: f(&[("Phone", "(330) 555-0144"), ("Location", "Akron, OH")]), ..Default::default() },
            AlertCard {
                title: "Summit Dental Group".into(),
                url: "https://directory.example/oh/akron/summit-dental-group-cuyahoga-falls".into(),
                fields: f(&[("Practice manager", "Priya Shah"), ("Chairs", "11"), ("Accepts new patients", "Yes"),
                    ("Services", "General dentistry, implants, Invisalign, pediatric, sedation, same-day crowns, emergency appointments")]),
                ..Default::default()
            },
        ]
    }

    #[test]
    #[ignore]
    fn dump_mail_samples() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/mail-samples");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, m) in [
            ("verification", verification("Ada", "482913")),
            ("password_reset", password_reset("Ada", "271044", "https://huntwell.ai/forgot?token=hwr_0123abcd")),
            ("welcome", welcome("Ada", "https://app.example.com/app")),
            ("invite", invite("Acme", "Bob", "member", "https://app.example.com/join/tok")),
            ("new_records", new_records("Dental practices in Akron", "results", 12, 48, &sample_cards(), "https://app.example.com/app/plans/7")),
            ("support_reply", support_reply("Robin", "Yes! Huntwell can collect commercial listings in Austin.\nTell me which sites you use and I'll suggest a plan.", "https://huntwell.ai/#support=hwc_0123abcd")),
        ] {
            std::fs::write(dir.join(format!("{name}.html")), &m.html).unwrap();
            std::fs::write(dir.join(format!("{name}.txt")), &m.text).unwrap();
        }
    }

    #[test]
    fn a_reset_link_carries_its_token_and_nothing_else() {
        let l = reset_link(" hwr_0123abcd ");
        assert!(l.ends_with("/forgot?token=hwr_0123abcd"), "{l}");
        assert!(!l.contains("email") && !l.contains("code"));
    }

    #[test]
    fn every_message_carries_its_link_in_both_parts() {
        let v = verification("Ada", "482913");
        assert!(v.html.contains("482913") && v.text.contains("482913") && !v.subject.contains("482913"));
        let r = password_reset("Ada", "271044", "https://app.example.com/forgot?token=hwr_0123abcd");
        assert!(r.html.contains("271044") && r.text.contains("271044") && !r.subject.contains("271044"));
        assert!(r.html.contains("/forgot?token=hwr_0123abcd") && r.text.contains("/forgot?token=hwr_0123abcd"));
        assert!(r.text.contains("24 hours") && !r.text.contains("15 minutes"));
        let i = invite("Alice", "Bob", "member", "https://app.example.com/join/tok");
        assert!(i.html.contains("/join/tok") && i.text.contains("/join/tok"));
        let w = welcome("Ada", "https://app.example.com/app");
        assert!(w.html.contains("/app") && w.text.contains("/app"));
        let s = support_reply("Ada", "Use <b>this</b>\nthen that", "https://app.example.com/#support=hwc_ab");
        assert!(s.html.contains("#support=hwc_ab") && s.text.contains("#support=hwc_ab"));
        assert!(s.html.contains("Use &lt;b&gt;this&lt;/b&gt;<br>then that"), "the reply is escaped, its lines kept");
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
        let m = new_records("Reno Crosstreks", "rows", 3, 41, &[AlertCard::titled("2019 Crosstrek Premium")], "https://x/app/plans/7");
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
        let m = new_records("P", "rows", 1, 1, &[AlertCard::titled("<script>alert(1)</script>")], "https://x");
        assert!(!m.html.contains("<script>"));
        assert!(m.html.contains("&lt;script&gt;"));
    }

    #[test]
    fn the_extra_line_only_appears_when_there_are_more() {
        let two = new_records("P", "rows", 9, 9, &[AlertCard::titled("a"), AlertCard::titled("b")], "https://x");
        assert!(two.html.contains("and 7 more"));
        let all = new_records("P", "rows", 2, 2, &[AlertCard::titled("a"), AlertCard::titled("b")], "https://x");
        assert!(!all.html.contains("more."));
    }

    #[test]
    fn a_card_carries_the_columns_and_links_only_what_is_safe() {
        let c = AlertCard {
            title: "Bright <b>Smiles</b>".into(),
            url: "javascript:alert(1)".into(),
            subtitle: "Owner · Bright Smiles Dental".into(),
            fields: vec![
                ("Email".into(), "ana@brightsmiles.example".into()),
                ("Website".into(), "https://www.brightsmiles.example/about/".into()),
                ("Chairs".into(), "6".into()),
                ("Bio".into(), "<img src=x onerror=alert(1)>".into()),
            ],
        };
        let m = new_records("Dentists", "results", 1, 5, &[c], "https://x/app/plans/1");
        assert!(m.html.contains("Bright &lt;b&gt;Smiles&lt;/b&gt;") && !m.html.contains("<b>Smiles"));
        assert!(!m.html.contains("href=\"javascript"), "an unsafe URL is never a link");
        assert!(m.html.contains(r#"href="mailto:ana@brightsmiles.example""#));
        assert!(m.html.contains(r#"href="https://www.brightsmiles.example/about/""#) && m.html.contains(">brightsmiles.example/about</a>"));
        assert!(m.html.contains("&lt;img") && !m.html.contains("<img src=x"));
        assert!(m.text.contains("  Chairs: 6") && m.text.contains("  Owner · Bright Smiles Dental"));
    }

    #[test]
    fn at_most_ten_cards_then_a_count() {
        let cards: Vec<AlertCard> = (0..14).map(|i| AlertCard::titled(&format!("Row {i}"))).collect();
        let m = new_records("P", "rows", 14, 14, &cards, "https://x");
        assert!(m.html.contains("Row 9") && !m.html.contains("Row 10"));
        assert!(m.html.contains("and 4 more in the plan"));
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

//! Support chat: what the website, the admin and the notification service share.
//!
//! Someone writes from the bubble on the site or in the app (`web::support`);
//! the operator answers in admin → Support (`admin::api`) or in the chat's
//! Slack thread (`slack`). What the person writes is posted to Slack by the
//! notification service, never inline: the website must answer the person
//! whatever Slack is doing. A reply typed in Slack reaches the website through
//! Slack's Events API. A reply reaches the person in the bubble, and by email
//! when they are no longer on the page.

pub mod slack;

use anyhow::Result;
use chrono::{Duration, Utc};

use crate::store::{self, Db, SupportConversation};

/// A reply is emailed too when the person has not had the chat open for this
/// long. Under it they are watching the bubble, and an email would be noise.
const AWAY_SECONDS: i64 = 120;

/// A visitor's key to their chat: `hwc_` and 192 random bits.
pub fn new_token() -> String {
    let mut b = [0u8; 24];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut b);
    format!("hwc_{}", hex::encode(b))
}

/// Where the person picks the chat up again. Signed in: the app, which knows
/// them. A visitor: the site, with their token after `#` so it never reaches
/// a server log.
pub fn customer_link(c: &SupportConversation) -> Option<String> {
    let base = crate::mail::link_base();
    if c.account_id.is_some() {
        return Some(format!("{base}/app#support"));
    }
    let token = crate::web::signing::open_secret(&c.token_sealed)?;
    Some(format!("{base}/#support={token}"))
}

/// Email a reply when the person is not watching the chat. `true` if queued.
pub async fn email_reply_if_away(db: &Db, c: &SupportConversation, reply: &str) -> Result<bool> {
    let away = c.customer_seen_at.map_or(true, |t| Utc::now() - t > Duration::seconds(AWAY_SECONDS));
    if !away {
        return Ok(false);
    }
    let Some(link) = customer_link(c) else {
        tracing::warn!(conversation_id = c.conversation_id, "support reply not emailed: the chat's link could not be opened on this server");
        return Ok(false);
    };
    let msg = crate::mail::support_reply(&c.name, reply, &link);
    store::queue_mail(db, c.account_id, &c.email, "support_reply", &msg).await?;
    Ok(true)
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}…", s.chars().take(max).collect::<String>().trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_visitor_token_is_long_and_marked() {
        let t = new_token();
        assert!(t.starts_with("hwc_") && t.len() == 52 && t[4..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t, new_token());
    }
}

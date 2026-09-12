//! The notification service: sends the mail everything else queues.
//!
//! Sending used to happen inline, on the request that caused it. That made an
//! invite as slow as the mail provider, and a provider failure a message nobody
//! ever heard about again — no record, no retry. Now the caller writes a row and
//! returns; this drains it.
//!
//! Safe to run several: claiming a message is an atomic UPDATE with
//! `SKIP LOCKED`, so two replicas take different messages.

use std::time::Duration;

use anyhow::Result;

use crate::store::{self, Db};

/// Backoff between attempts, indexed by how many have been made. The last entry
/// repeats until `MAX_ATTEMPTS`. Minutes.
const BACKOFF_MINUTES: [i64; 5] = [1, 5, 30, 120, 360];
/// After this many failures the message is left in the table, unsent, with its
/// last error — visible to anyone looking, retried by nobody.
const MAX_ATTEMPTS: i32 = 8;

/// How long a claimed message is invisible before another attempt. Long enough
/// that a slow provider call is not treated as a crash.
const CLAIM_LEASE_SECONDS: i64 = 300;

fn backoff_seconds(attempts: i32) -> i64 {
    let i = (attempts.max(1) as usize - 1).min(BACKOFF_MINUTES.len() - 1);
    BACKOFF_MINUTES[i] * 60
}

/// What this service listens for.
///
/// The point of the bus: nothing in the drafting path knows that a failed draft
/// should tell anyone. It publishes what happened; this decides that it is worth
/// an email. Adding "and also tell Slack" is another subscriber, not an edit to
/// the planning service.
pub fn listen(db: Db) {
    tokio::spawn(async move {
        let Some(mut sub) = crate::bus::subscribe("huntwell.plan.draft_failed").await else {
            // No bus. The service still drains the outbox — it just will not
            // learn about anything it was not told directly.
            return;
        };
        use futures_util::StreamExt;
        while let Some(msg) = sub.next().await {
            let Some(event) = crate::bus::decode(&msg) else { continue };
            if let Err(e) = on_draft_failed(&db, &event).await {
                tracing::warn!("reacting to {}: {e:#}", event.subject);
            }
        }
        tracing::warn!("bus subscription ended — no longer reacting to events");
    });
}

async fn on_draft_failed(db: &Db, event: &crate::bus::Event) -> Result<()> {
    let Some(account_id) = event.account_id else { return Ok(()) };
    let plan_id = event.data["plan_id"].as_i64().unwrap_or_default();
    let Some(acc) = crate::store::get_account(db, account_id).await? else { return Ok(()) };
    let plan_name = crate::store::get_plan(db, account_id, plan_id)
        .await?
        .map(|p| p.source)
        .unwrap_or_else(|| format!("plan {plan_id}"));
    let base = crate::config::get("HUNTWELL_PUBLIC_URL").unwrap_or_default();
    let link = format!("{}/app/plans/{}", base.trim_end_matches('/'), plan_id);
    let msg = crate::mail::draft_failed(&plan_name, &link);
    crate::store::queue_mail(db, Some(account_id), &acc.email, "draft_failed", &msg).await?;
    tracing::info!(plan_id, account_id, "queued a draft-failure notice");
    Ok(())
}

pub async fn serve(db: Db) -> Result<()> {
    if !crate::mail::configured() {
        // Not fatal: mail::send logs the message instead of sending it, which is
        // what a dev box wants. Said once, loudly, so it is not a mystery later.
        tracing::warn!("no mail provider configured — queued mail will be logged, not delivered");
    }
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tracing::info!("notification service ready");
    loop {
        tick.tick().await;
        loop {
            let mail = match store::claim_due_mail(&db, CLAIM_LEASE_SECONDS).await {
                Ok(Some(m)) => m,
                // Nothing owed — back to the tick.
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("claiming due mail: {e:#}");
                    break;
                }
            };
            match crate::mail::send(&mail.to, &mail.email).await {
                Ok(()) => {
                    if let Err(e) = store::mark_mail_sent(&db, mail.mail_id).await {
                        // Sent but not recorded: the lease will expire and it
                        // will send again. Better a duplicate than a silence.
                        tracing::warn!(mail_id = mail.mail_id, "sent but could not record it: {e:#}");
                    } else {
                        tracing::info!(mail_id = mail.mail_id, to = mail.to, "sent");
                        crate::bus::publish(
                            crate::bus::subject::MAIL_SENT,
                            None,
                            serde_json::json!({ "mail_id": mail.mail_id, "to": mail.to, "attempts": mail.attempts }),
                        )
                        .await;
                    }
                }
                Err(e) => {
                    let give_up = mail.attempts >= MAX_ATTEMPTS;
                    let retry_in = (!give_up).then(|| backoff_seconds(mail.attempts));
                    if give_up {
                        tracing::error!(mail_id = mail.mail_id, to = mail.to, attempts = mail.attempts, "giving up: {e:#}");
                    } else {
                        tracing::warn!(mail_id = mail.mail_id, attempts = mail.attempts, "send failed, will retry: {e:#}");
                    }
                    let _ = store::mark_mail_failed(&db, mail.mail_id, &format!("{e:#}"), retry_in).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_climbs_then_holds() {
        // First failure waits a minute; the curve flattens rather than growing
        // without bound, so a provider outage does not push a message days out.
        assert_eq!(backoff_seconds(1), 60);
        assert_eq!(backoff_seconds(3), 30 * 60);
        assert_eq!(backoff_seconds(5), 360 * 60);
        assert_eq!(backoff_seconds(50), 360 * 60);
    }

    #[test]
    fn backoff_survives_a_zero() {
        // Attempts is incremented by the claim, so 0 should never arrive — but
        // indexing with a wrapped subtraction would panic if it ever did.
        assert_eq!(backoff_seconds(0), 60);
    }
}

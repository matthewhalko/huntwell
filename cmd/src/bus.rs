//! The event bus: NATS.
//!
//! Every service publishes what it did and subscribes to what it cares about.
//! A plan is created, `huntwell.plan.created` goes out, and whoever wants to act
//! on that acts on it — the publisher does not know or care who is listening,
//! which is the point. Adding a service that reacts to plans means adding a
//! subscriber, not editing the service that creates them.
//!
//! # The bus is not the source of truth
//!
//! Postgres is. Events say *that* something happened; the row says *what* is
//! true. Two consequences, both deliberate:
//!
//! - **Publishing never fails a request.** A publish that cannot reach the bus
//!   is logged and dropped. Creating a plan that is committed to the database
//!   must not fail because a message broker is restarting.
//! - **Durable work stays in the database.** Runs, drafts and outbound mail are
//!   claimed with an atomic UPDATE, which gives exactly-once delivery to exactly
//!   one worker and survives everything restarting at once. Those are work
//!   queues, not events, and the bus does not replace them.
//!
//! With `HUNTWELL_NATS_URL` unset there is no bus at all and every publish is
//! a no-op, so a checkout with nothing running still works.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How often a living process announces itself. The admin treats three missed
/// beats (~15s) as stale, so this wants to stay well under that.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);

/// Subject prefix. Everything this system emits lives under it, so a shared
/// NATS cluster can carry other tenants without collisions.
pub const PREFIX: &str = "huntwell";

/// Subjects, named once here rather than spelled out at each call site — a
/// publisher and a subscriber disagreeing by one character is a bug that looks
/// like "the event never arrived".
pub mod subject {
    pub const PLAN_CREATED: &str = "huntwell.plan.created";
    pub const PLAN_DRAFTED: &str = "huntwell.plan.drafted";
    pub const PLAN_DRAFT_FAILED: &str = "huntwell.plan.draft_failed";
    pub const PLAN_DELETED: &str = "huntwell.plan.deleted";

    pub const RUN_QUEUED: &str = "huntwell.run.queued";
    pub const RUN_STARTED: &str = "huntwell.run.started";
    /// Tokens booked against a live run. Postgres is still the source of
    /// truth; this is the wake so a watching page does not wait for a poll.
    pub const RUN_METERED: &str = "huntwell.run.metered";
    pub const RUN_FINISHED: &str = "huntwell.run.finished";

    pub const MAIL_QUEUED: &str = "huntwell.mail.queued";
    pub const MAIL_SENT: &str = "huntwell.mail.sent";

    /// A living process announcing itself. The admin control plane listens and
    /// shows last-ping; nothing else should act on these.
    pub const SERVICE_HEARTBEAT: &str = "huntwell.service.heartbeat";

    /// Everything, for a service that wants the lot (or for `nats sub`).
    pub const ALL: &str = "huntwell.>";
}

/// What goes on the wire.
///
/// `account_id` is on the envelope rather than buried in `data` because every
/// consumer needs it and none should have to know the shape of a payload it
/// does not otherwise care about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique per publish. A consumer that must not act twice can remember it.
    pub id: String,
    pub subject: String,
    /// Which workspace this concerns. `None` for system-wide events.
    pub account_id: Option<i64>,
    /// Which service published it, for tracing a chain of reactions.
    pub source: String,
    pub at: chrono::DateTime<chrono::Utc>,
    pub data: Value,
}

static CLIENT: OnceLock<Option<async_nats::Client>> = OnceLock::new();
static SOURCE: OnceLock<String> = OnceLock::new();
static HEARTBEAT: OnceLock<()> = OnceLock::new();

/// Connect once, at startup. Safe to skip: everything degrades to a no-op.
///
/// Failure to connect is a warning, not an error. A bus that is down must not
/// stop a service from starting — it would turn an optional dependency into a
/// required one, which is the opposite of what a bus is for.
pub async fn connect(service: &str) {
    let _ = SOURCE.set(service.to_string());
    let Some(url) = crate::config::get("HUNTWELL_NATS_URL").filter(|u| !u.trim().is_empty()) else {
        tracing::info!("no HUNTWELL_NATS_URL — running without an event bus");
        let _ = CLIENT.set(None);
        return;
    };
    match async_nats::connect(&url).await {
        Ok(c) => {
            tracing::info!("bus: connected to {url}");
            let _ = CLIENT.set(Some(c));
        }
        Err(e) => {
            tracing::warn!("bus: could not connect to {url} ({e}) — events will be dropped");
            let _ = CLIENT.set(None);
        }
    }
    // After the attempt, not only on success: the loop no-ops when there is no
    // client, and starting it here means every service that joins the bus also
    // announces itself without each binary having to remember.
    start_heartbeat();
}

fn client() -> Option<&'static async_nats::Client> {
    CLIENT.get().and_then(|c| c.as_ref())
}

/// Is there a bus? For a health endpoint to report, not for callers to branch on.
pub fn connected() -> bool {
    client().is_some()
}

/// Which service this process publishes as. Set by [`connect`].
pub fn source() -> String {
    SOURCE.get().cloned().unwrap_or_else(|| "unknown".into())
}

/// Who this process is, so two replicas of the same service do not look like
/// one. A k8s pod uses its name; everywhere else it is `host:pid`.
pub fn instance_id() -> String {
    if let Ok(pod) = std::env::var("POD_NAME") {
        let pod = pod.trim();
        if !pod.is_empty() {
            return pod.to_string();
        }
    }
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "unknown".into());
    format!("{host}:{}", std::process::id())
}

/// Announce this process as [`source`] every [`HEARTBEAT_EVERY`]. Safe to call
/// more than once: only the first starts the loop.
pub fn start_heartbeat() {
    if HEARTBEAT.set(()).is_err() {
        return;
    }
    start_heartbeat_as(source());
}

/// Announce a role this process is fulfilling. The all-in-one `serve` uses
/// this so planning and scheduling show as alive without being their own
/// binaries — the instance id is the same process, which the UI can show.
pub fn start_heartbeat_as(service: impl Into<String>) {
    let service = service.into();
    if service.is_empty() {
        return;
    }
    tokio::spawn(async move {
        loop {
            if connected() {
                publish_from(
                    &service,
                    subject::SERVICE_HEARTBEAT,
                    None,
                    serde_json::json!({ "instance": instance_id() }),
                )
                .await;
            }
            tokio::time::sleep(HEARTBEAT_EVERY).await;
        }
    });
}

/// Publish. Never fails the caller: a bus that cannot be reached is logged.
///
/// Takes `&Value` rather than a generic so every call site is forced to name
/// the shape it is sending, which is what a consumer has to read.
pub async fn publish(subject: &str, account_id: Option<i64>, data: Value) {
    publish_from(&source(), subject, account_id, data).await;
}

/// Publish as a named source. Heartbeats for in-process roles use this so the
/// envelope's `source` is the role, not whichever name [`connect`] was given.
pub async fn publish_from(source: &str, subject: &str, account_id: Option<i64>, data: Value) {
    let Some(c) = client() else { return };
    let event = Event {
        id: uuid(),
        subject: subject.to_string(),
        account_id,
        source: source.to_string(),
        at: chrono::Utc::now(),
        data,
    };
    let body = match serde_json::to_vec(&event) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("bus: could not encode {subject}: {e}");
            return;
        }
    };
    if let Err(e) = c.publish(subject.to_string(), body.into()).await {
        tracing::warn!("bus: could not publish {subject}: {e}");
    }
}

/// Subscribe to a subject or wildcard (`huntwell.plan.*`, `huntwell.>`).
///
/// Returns `None` when there is no bus, so a listener task can simply not run
/// rather than every caller having to handle the absence.
pub async fn subscribe(subject: &str) -> Option<async_nats::Subscriber> {
    let c = client()?;
    match c.subscribe(subject.to_string()).await {
        Ok(s) => {
            tracing::info!("bus: subscribed to {subject}");
            Some(s)
        }
        Err(e) => {
            tracing::warn!("bus: could not subscribe to {subject}: {e}");
            None
        }
    }
}

/// Decode a received message. A malformed one is logged and skipped rather than
/// killing the listener: the publisher may be a newer version than this reader.
pub fn decode(msg: &async_nats::Message) -> Option<Event> {
    match serde_json::from_slice::<Event>(&msg.payload) {
        Ok(e) => Some(e),
        Err(e) => {
            tracing::warn!(subject = %msg.subject, "bus: undecodable event: {e}");
            None
        }
    }
}

/// Flush anything buffered. Used before a short-lived process exits, where the
/// connection would otherwise be dropped with the publish still in flight.
pub async fn flush() -> Result<()> {
    if let Some(c) = client() {
        c.flush().await.context("flush the bus")?;
    }
    Ok(())
}

/// A random id, without pulling in a uuid crate for one field.
fn uuid() -> String {
    use rand::Rng;
    let mut b = [0u8; 16];
    rand::thread_rng().fill(&mut b);
    // Version 4, variant 1 — so it reads as a UUID to anything that parses one.
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_looks_like_one() {
        let u = uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(u.matches('-').count(), 4);
        // Version and variant nibbles, so it parses as a v4 anywhere else.
        assert_eq!(&u[14..15], "4");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"), "variant nibble was {}", &u[19..20]);
        assert_ne!(u, uuid());
    }

    #[test]
    fn publishing_with_no_bus_is_a_no_op() {
        // The property the whole design rests on: no bus, no panic, no error.
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            publish(subject::PLAN_CREATED, Some(1), serde_json::json!({"plan_id": 1})).await;
            assert!(!connected());
        });
    }

    #[test]
    fn instance_id_is_stable_for_a_process() {
        let a = instance_id();
        let b = instance_id();
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn every_subject_sits_under_the_prefix() {
        // A subject outside the prefix would be invisible to `huntwell.>`, which
        // is what a catch-all consumer and `nats sub` both use.
        for s in [
            subject::PLAN_CREATED, subject::PLAN_DRAFTED, subject::PLAN_DRAFT_FAILED,
            subject::PLAN_DELETED, subject::RUN_QUEUED, subject::RUN_STARTED,
            subject::RUN_METERED, subject::RUN_FINISHED, subject::MAIL_QUEUED, subject::MAIL_SENT,
            subject::SERVICE_HEARTBEAT,
        ] {
            assert!(s.starts_with(&format!("{PREFIX}.")), "{s} is outside the prefix");
        }
    }
}

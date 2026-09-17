//! In-memory rate limits, per key, for the handful of endpoints that need one.
//!
//! One implementation for sign-in failures, sign-ups, invitations and the
//! operator login. Each limiter counts events per key inside a sliding window
//! and, once the count reaches its cap, refuses that key for a lockout period.
//! Keys are client addresses or account ids — never anything a caller chooses
//! freely, or a caller could fill the table.
//!
//! In-process, so a restart forgets everything and every website replica keeps
//! its own count. That is fine for what these protect: a brute force has to be
//! slowed, not accounted for exactly.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Entry {
    events: Vec<Instant>,
    locked_until: Option<Instant>,
    last: Instant,
}

pub struct Limiter {
    max: u32,
    window: Duration,
    lockout: Duration,
    map: Mutex<Option<HashMap<String, Entry>>>,
}

impl Limiter {
    /// Refuse a key for `lockout_secs` once it has produced `max` events within
    /// `window_secs`.
    pub const fn new(max: u32, window_secs: u64, lockout_secs: u64) -> Self {
        Limiter {
            max,
            window: Duration::from_secs(window_secs),
            lockout: Duration::from_secs(lockout_secs),
            map: Mutex::new(None),
        }
    }

    fn with<T>(&self, key: &str, f: impl FnOnce(&mut Entry, Instant, u32, Duration, Duration) -> T) -> T {
        let now = Instant::now();
        let mut guard = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let map = guard.get_or_insert_with(HashMap::new);
        // Bounded: forget anything idle for longer than the lockout and the
        // window put together, so an attacker cycling addresses cannot grow it.
        if map.len() > 8192 {
            let idle = self.window + self.lockout;
            map.retain(|_, e| now.duration_since(e.last) < idle);
        }
        let e = map.entry(key.to_string()).or_insert_with(|| Entry { events: Vec::new(), locked_until: None, last: now });
        e.last = now;
        f(e, now, self.max, self.window, self.lockout)
    }

    /// Seconds until `key` may try again, if it is locked out.
    pub fn check(&self, key: &str) -> Result<(), u64> {
        self.with(key, |e, now, _, _, _| match e.locked_until {
            Some(until) if now < until => Err(until.duration_since(now).as_secs().max(1)),
            _ => {
                e.locked_until = None;
                Ok(())
            }
        })
    }

    /// Record one event for `key` — a failed sign-in, a sign-up — and lock the
    /// key once it reaches the cap.
    pub fn note(&self, key: &str) {
        self.with(key, |e, now, max, window, lockout| {
            e.events.retain(|t| now.duration_since(*t) < window);
            e.events.push(now);
            if e.events.len() as u32 >= max {
                e.locked_until = Some(now + lockout);
                e.events.clear();
            }
        });
    }

    /// `check` then `note`, for events that count whether or not they succeed.
    pub fn hit(&self, key: &str) -> Result<(), u64> {
        self.check(key)?;
        self.note(key);
        Ok(())
    }
}

/// Failed sign-ins from one address: ten in fifteen minutes locks it for
/// fifteen. Ten, not three — a person retyping a password is not an attack,
/// and argon2 already makes each guess cost.
pub static LOGIN_FAILURES: Limiter = Limiter::new(10, 15 * 60, 15 * 60);

/// Accounts created from one address: five an hour.
pub static SIGNUPS: Limiter = Limiter::new(5, 60 * 60, 60 * 60);

/// Invitations sent by one account: thirty a day. Each is an email from this
/// domain with the sender's own words in it.
pub static INVITES: Limiter = Limiter::new(30, 24 * 60 * 60, 24 * 60 * 60);

/// Verification emails re-sent by one account: five an hour.
pub static VERIFY_RESENDS: Limiter = Limiter::new(5, 60 * 60, 60 * 60);

/// Operator sign-in failures, per address. Tighter: there is one operator.
pub static ADMIN_LOGIN_FAILURES: Limiter = Limiter::new(5, 15 * 60, 15 * 60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_after_the_cap_and_only_that_key() {
        let l = Limiter::new(3, 60, 60);
        assert!(l.check("a").is_ok());
        l.note("a");
        l.note("a");
        assert!(l.check("a").is_ok(), "two failures is not a lockout");
        l.note("a");
        let wait = l.check("a").unwrap_err();
        assert!((1..=60).contains(&wait), "{wait}");
        assert!(l.check("b").is_ok(), "another key is unaffected");
    }

    #[test]
    fn hit_counts_every_attempt() {
        let l = Limiter::new(2, 60, 60);
        assert!(l.hit("x").is_ok());
        assert!(l.hit("x").is_ok());
        assert!(l.hit("x").is_err());
    }

    #[test]
    fn a_lockout_expires() {
        let l = Limiter::new(1, 60, 0);
        l.note("k");
        // Zero-second lockout: expired by the time it is checked.
        std::thread::sleep(Duration::from_millis(5));
        assert!(l.check("k").is_ok());
    }
}

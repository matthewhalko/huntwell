//! The first-run key: how an operator proves they are the person who started
//! the process, before there is any account to authenticate against.
//!
//! An admin with no operators is a wide-open console — there is nothing to sign
//! in to, and whoever reaches the page first could claim it. Seeding from
//! `HUNTWELL_ADMIN_EMAIL`/`_PASSWORD` closed that window by putting the first
//! password in a settings file, which means it is in a file, in a Secret, and
//! in whatever wrote them.
//!
//! Instead the process mints a key at boot and writes it to the log. Holding it
//! means having access to the machine's output — the same access needed to
//! start the process — so the install is claimed by whoever deployed it rather
//! than by whoever connects first.
//!
//! Properties that matter:
//!
//! - **In memory only.** Never written to the database or to disk. A restart
//!   mints a new one, so a key from an old log is already dead.
//! - **Gone once used.** Cleared the moment the first operator exists, so the
//!   endpoint it guards stops working rather than staying open.
//! - **Constant-time comparison**, so the key cannot be guessed a character at
//!   a time by timing the reply.

use std::sync::{OnceLock, RwLock};

use rand::Rng;

/// Crockford base32 without the letters that get misread in a terminal font —
/// no I, L, O or U. This key is read off a console and typed into a browser,
/// often from a photo of a screen, so the alphabet is chosen for transcription
/// rather than for density.
const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const GROUPS: usize = 4;
const GROUP_LEN: usize = 5;

fn slot() -> &'static RwLock<Option<String>> {
    static KEY: OnceLock<RwLock<Option<String>>> = OnceLock::new();
    KEY.get_or_init(|| RwLock::new(None))
}

/// Mint the key for this process. Returns it so the caller can log it.
pub fn arm() -> String {
    let mut rng = rand::thread_rng();
    let key = (0..GROUPS)
        .map(|_| {
            (0..GROUP_LEN)
                .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("-");
    *slot().write().expect("setup key lock") = Some(key.clone());
    key
}

/// Forget the key. Called the moment the first operator exists.
pub fn disarm() {
    *slot().write().expect("setup key lock") = None;
}

/// Is this process waiting to be claimed?
pub fn required() -> bool {
    slot().read().expect("setup key lock").is_some()
}

/// Constant-time comparison against the armed key.
///
/// Length is compared first and separately, which does leak the length — but
/// the length is a constant of the format, not a secret.
pub fn matches(candidate: &str) -> bool {
    let guard = slot().read().expect("setup key lock");
    let Some(key) = guard.as_ref() else { return false };
    let a = key.as_bytes();
    let b = candidate.trim().to_ascii_uppercase();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key is process-global, and `cargo test` runs these on threads of one
    /// process — without this they interleave and each other's arm/disarm makes
    /// the assertions flaky. Serialising is the honest fix; making the key
    /// thread-local would be testing something the program does not do.
    fn exclusively<T>(f: impl FnOnce() -> T) -> T {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // A panic in one test poisons the mutex; that must not cascade into
        // "every other test failed too".
        let guard = LOCK.get_or_init(|| Mutex::new(()));
        let _held = guard.lock().unwrap_or_else(|e| e.into_inner());
        f()
    }

    #[test]
    fn a_fresh_process_is_not_claimable() {
        exclusively(|| {
        // Nothing armed: `matches` must refuse everything, including the empty
        // string, rather than treating "no key" as "any key".
        disarm();
        assert!(!required());
        assert!(!matches(""));
        assert!(!matches("ANYTHING"));
        });
    }

    #[test]
    fn the_key_reads_and_compares() {
        exclusively(|| {
        let key = arm();
        assert!(required());
        assert_eq!(key.len(), GROUPS * GROUP_LEN + (GROUPS - 1));
        assert_eq!(key.matches('-').count(), GROUPS - 1);
        // Typed back in any case, with the whitespace a copy-paste picks up.
        assert!(matches(&key));
        assert!(matches(&format!("  {}  ", key.to_lowercase())));
        assert!(!matches(&key[..key.len() - 1]));
        assert!(!matches("00000-00000-00000-00000"));
        disarm();
        assert!(!matches(&key), "a disarmed key must stop working");
        });
    }

    #[test]
    fn the_alphabet_avoids_the_letters_people_misread() {
        for c in "ILOU".chars() {
            assert!(!ALPHABET.contains(&(c as u8)), "{c} is easy to misread");
        }
        // And it is exactly base32, so the key carries the entropy it looks like.
        assert_eq!(ALPHABET.len(), 32);
    }
}

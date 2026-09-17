//! Where a password actually lives.
//!
//! Two providers, on the same axis as the secret Secrets Manager serves, so one
//! process never straddles two identity stores:
//!
//! | Provider  | Password              | Row carries      |
//! |-----------|-----------------------|------------------|
//! | `local`   | argon2 in `account`   | `password_hash`  |
//! | `cognito` | an AWS Cognito pool   | `cognito_sub`    |
//!
//! `local` is for a dev box, which has no AWS credentials and should need none.
//! Production is `cognito`, and a production row stores no hash at all.
//!
//! The port of `../../parkriver/cmd/src/shared/identity.rs`. Park River reaches
//! the same shape through a trait because it also owns TOTP enrolment there;
//! Huntwell has four operations and no second factor, so the same idea costs an
//! enum and four functions.

use anyhow::{anyhow, Result};

/// What to store on the `account` row once a provider has taken the password.
/// Exactly one side is ever filled: `local` has a hash and no subject,
/// `cognito` a subject and no hash.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct NewIdentity {
    pub cognito_sub: String,
    pub password_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Local,
    Cognito,
}

/// Which store this installation uses.
///
/// `HUNTWELL_IDENTITY` decides it outright — that is how a dev box is pointed
/// at a real pool, or a deployed one held on local identity during a migration.
/// Otherwise: a configured pool means Cognito.
///
/// The fallback is deliberately "whether a pool is configured" rather than a
/// build profile. Huntwell ships one binary for both, and a release build that
/// assumed Cognito would make `./dev.sh` need AWS credentials to sign anybody
/// in.
pub fn provider() -> Provider {
    if let Some(v) = crate::config::get("HUNTWELL_IDENTITY") {
        match v.trim().to_ascii_lowercase().as_str() {
            "local" => return Provider::Local,
            "cognito" => return Provider::Cognito,
            // Anything unrecognised falls through rather than silently
            // choosing one.
            _ => {}
        }
    }
    if crate::cognito::configured() {
        Provider::Cognito
    } else {
        Provider::Local
    }
}

/// Whether identity settings say `local` in so many words. Only that opts a
/// production build out of Cognito; silence never does.
fn explicitly_local() -> bool {
    crate::config::get("HUNTWELL_IDENTITY").is_some_and(|v| v.trim().eq_ignore_ascii_case("local"))
}

/// What a production build refuses to run without: a pool. The fallback to
/// local identity exists for a dev box, and on a production build silence
/// about Cognito would otherwise mean password hashes quietly landing in the
/// `account` table — which is the one thing the pool is there to prevent.
/// `HUNTWELL_IDENTITY=local` still works, because it is said out loud.
pub fn enforce_production_provider() -> Result<()> {
    if crate::genesis::embedded_variant() != "prod" || explicitly_local() {
        return Ok(());
    }
    if provider() == Provider::Cognito {
        return Ok(());
    }
    Err(anyhow!(
        "this is a production build and no Cognito pool is configured, so sign-ups would store \
         password hashes in the database. Add COGNITO_USER_POOL_ID, COGNITO_CLIENT_ID, COGNITO_REGION \
         (and COGNITO_CLIENT_SECRET, AWS_COGNITO_KEY, AWS_COGNITO_SECRET) to the Secrets Manager secret — \
         see docs/SECRETS.md — or set HUNTWELL_IDENTITY=local to mean it"
    ))
}

pub fn is_cognito() -> bool {
    provider() == Provider::Cognito
}

pub fn provider_name() -> &'static str {
    match provider() {
        Provider::Local => "local",
        Provider::Cognito => "cognito",
    }
}

/// Password rules applied before anything is stored, in either provider.
///
/// Cognito enforces its own policy too; this exists so a weak password is
/// refused with a sentence rather than an AWS error code, and so the local
/// provider holds the same line.
pub fn check_password_strength(password: &str) -> Result<()> {
    if password.chars().count() < 12 {
        return Err(anyhow!("password must be at least 12 characters"));
    }
    let has_lower = password.chars().any(|c| c.is_lowercase());
    let has_upper = password.chars().any(|c| c.is_uppercase());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    if !(has_lower && has_upper && has_digit) {
        return Err(anyhow!(
            "password must contain an uppercase letter, a lowercase letter and a digit"
        ));
    }
    Ok(())
}

/// Create the identity for a new account.
pub async fn create_user(email: &str, password: &str) -> Result<NewIdentity> {
    check_password_strength(password)?;
    match provider() {
        Provider::Local => Ok(NewIdentity {
            cognito_sub: String::new(),
            password_hash: crate::web::auth::hash_password(password)?,
        }),
        Provider::Cognito => Ok(NewIdentity {
            cognito_sub: crate::cognito::create_user(email, password).await?,
            password_hash: String::new(),
        }),
    }
}

/// Check a password. `stored_hash` is the row's `password_hash`, which the
/// Cognito provider ignores — under Cognito it is empty, and the answer comes
/// from the pool.
///
/// Returns the Cognito subject when there is one, so a caller can reconcile a
/// row whose `cognito_sub` is not yet filled in.
pub async fn authenticate(email: &str, password: &str, stored_hash: &str) -> Result<String> {
    match provider() {
        Provider::Local => {
            if verify_local_password(password, stored_hash) {
                Ok(String::new())
            } else {
                Err(anyhow!("invalid email or password"))
            }
        }
        Provider::Cognito => crate::cognito::sign_in(email, password).await,
    }
}

/// Set or reset a password. The local provider returns the new hash to store;
/// Cognito returns an empty string, because the row holds nothing.
pub async fn set_password(email: &str, password: &str) -> Result<String> {
    check_password_strength(password)?;
    match provider() {
        Provider::Local => crate::web::auth::hash_password(password),
        Provider::Cognito => crate::cognito::set_password(email, password).await.map(|()| String::new()),
    }
}

/// Remove the identity behind a deleted account. A failure is the caller's to
/// report, not to ignore: an orphaned pool user keeps an address unusable.
pub async fn delete_user(email: &str) -> Result<()> {
    match provider() {
        Provider::Local => Ok(()),
        Provider::Cognito => crate::cognito::delete_user(email).await,
    }
}

/// Verify a password against a stored argon2 hash.
///
/// Always runs a verification, even when the hash is absent, so the time taken
/// does not reveal whether an account has a password set — which under Cognito
/// is every local row.
pub fn verify_local_password(password: &str, hash: &str) -> bool {
    if hash.trim().is_empty() {
        let _ = crate::web::auth::verify_password(password, DUMMY_HASH);
        return false;
    }
    crate::web::auth::verify_password(password, hash)
}

/// An argon2 hash of nothing in particular, to spend the same time on an
/// account with no password as on one with the wrong password.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHR2YWx1ZQ$K7X1dLQ8H5HvVbDqQ2Xk3jZ8p1n5tOqR0wYyYyYyYyY";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weak_passwords_are_refused() {
        for bad in ["short", "alllowercase1", "ALLUPPERCASE1", "NoDigitsHere"] {
            assert!(check_password_strength(bad).is_err(), "{bad} should fail");
        }
        assert!(check_password_strength("CorrectHorse1Battery").is_ok());
    }

    #[test]
    fn an_empty_hash_never_verifies() {
        // Under Cognito every row has one. If this returned true, an account
        // with no local password would be signed in by any password at all.
        assert!(!verify_local_password("anything", ""));
        assert!(!verify_local_password("", "   "));
    }

    #[test]
    fn a_real_hash_still_verifies() {
        let hash = crate::web::auth::hash_password("CorrectHorse1Battery").unwrap();
        assert!(verify_local_password("CorrectHorse1Battery", &hash));
        assert!(!verify_local_password("CorrectHorse1Batterz", &hash));
    }

    #[test]
    fn an_identity_carries_one_side_or_the_other() {
        // The invariant the account row depends on: a hash and a subject are
        // never both present, so nothing has to decide which one wins.
        let local = NewIdentity { cognito_sub: String::new(), password_hash: "argon2…".into() };
        let cognito = NewIdentity { cognito_sub: "9f1c-42".into(), password_hash: String::new() };
        for id in [&local, &cognito] {
            assert!(id.cognito_sub.is_empty() != id.password_hash.is_empty());
        }
    }
}

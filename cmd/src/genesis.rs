//! Encryption for the `global` config file.
//!
//! The port of `../../parkriver/cmd/src/shared/genesis.rs`, with the same
//! format and the same reasoning, so a file sealed by either project's tooling
//! is the same artifact and a fix in one carries across.
//!
//! `global` on a production box holds one thing worth protecting: the AWS
//! access key and secret that open Secrets Manager. Everything else — the
//! database URL, the Cursor key, the Cognito pool — lives in Secrets Manager
//! and arrives through it. So `global` is a bootstrap credential and nothing
//! else, and it is stored sealed rather than in the clear.
//!
//! The key lives in a **separate** file (`genesis`) so the config and the thing
//! that opens it are never the same artifact: copying `global` off the box gets
//! you nothing without `genesis`.
//!
//! **AES-256-GCM**, via `ring`. GCM is authenticated, so a tampered value fails
//! to decrypt instead of silently decoding to garbage — the property
//! `openssl enc` in CBC mode cannot give you.
//!
//! Encoding of an encrypted value:
//!
//! ```text
//! AWS_SECRET_ACCESS_KEY=enc:v1:<base64( nonce[12] || ciphertext || tag[16] )>
//! ```
//!
//! The `enc:` prefix makes the format self-describing, so plaintext settings
//! keep working unchanged and only the values worth protecting need wrapping.
//!
//! The setting's own name is bound in as additional authenticated data, so a
//! ciphertext cannot be moved from one key to another — pasting the value of
//! `AWS_SECRET_ACCESS_KEY` over `AWS_ACCESS_KEY_ID` fails rather than quietly
//! swapping them.

use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use zeroize::{Zeroize, Zeroizing};

/// Marker for an encrypted value, with a format version so this can change.
pub const PREFIX: &str = "enc:v1:";

/// AES-256 key length.
pub const KEY_LEN: usize = 32;

/// AAD for a whole-file seal. Deliberately not a valid setting name, so a
/// sealed file cannot be pasted in as a single value or the other way round.
const FILE_AAD: &str = "\u{0}global-file";

/// Where the key file lives.
///
/// `HUNTWELL_GENESIS_KEY_FILE` names it outright. Otherwise it mirrors
/// `config::candidates`: `local-infra/` in a checkout, beside the binary for a
/// deployed one — the same places `global` itself is looked for, because the
/// two travel together and are deployed apart only by accident.
pub fn key_path() -> PathBuf {
    if let Some(explicit) = std::env::var("HUNTWELL_GENESIS_KEY_FILE").ok().filter(|v| !v.trim().is_empty()) {
        return PathBuf::from(explicit);
    }
    let checkout = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../local-infra/genesis"));
    if checkout.is_file() {
        return checkout;
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("genesis")));
    match beside {
        Some(p) if p.is_file() => p,
        _ => PathBuf::from("genesis"),
    }
}

/// True when a value carries the encrypted marker.
pub fn is_encrypted(value: &str) -> bool {
    value.trim_start().starts_with(PREFIX)
}

/// True when a file's contents are a whole-file seal rather than settings.
pub fn is_sealed_file(contents: &str) -> bool {
    contents
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.starts_with(PREFIX))
        .unwrap_or(false)
}

/// A fresh 32-byte key, base64 — for the tests, and for `huntwell genesis
/// keygen` on a machine that is deliberately not the server.
pub fn generate_key() -> Result<Zeroizing<String>, String> {
    let mut key = Zeroizing::new(vec![0u8; KEY_LEN]);
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| String::from("could not read from the system RNG"))?;
    Ok(Zeroizing::new(B64.encode(&*key)))
}

/// Turn base64 key material into raw key bytes.
fn material_to_key(encoded: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let raw = B64
        .decode(encoded.trim().as_bytes())
        .map_err(|e| format!("key is not valid base64: {e}"))?;
    if raw.len() != KEY_LEN {
        return Err(format!("key must be {KEY_LEN} bytes ({} after base64)", raw.len()));
    }
    Ok(Zeroizing::new(raw))
}

/// Read key material from a file, tolerating comments and blank lines so the
/// file can carry a note about what it is.
pub fn read_key(path: &Path) -> Result<Zeroizing<Vec<u8>>, String> {
    let raw = Zeroizing::new(
        std::fs::read_to_string(path).map_err(|e| format!("cannot read key file {}: {e}", path.display()))?,
    );
    let encoded = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .ok_or_else(|| format!("key file {} has no key in it", path.display()))?;
    material_to_key(encoded).map_err(|e| format!("key in {}: {e}", path.display()))
}

/// The key this process should use, and where it came from.
///
/// The source is returned so the tooling can *say* it: sealing with the wrong
/// environment's key produces a file that looks fine and cannot be opened on
/// the server, and this line is the only warning anyone gets.
pub fn key() -> Result<(Zeroizing<Vec<u8>>, String), String> {
    let path = key_path();
    read_key(&path).map(|k| (k, path.display().to_string()))
}

/// Whether a key is available at all. Used to tell "no key here" apart from
/// "the key does not open this file".
pub fn have_key() -> bool {
    key_path().is_file()
}

/// Encrypt one setting value. `name` is the setting's key, bound in as AAD.
pub fn encrypt_with(key_bytes: &[u8], name: &str, plaintext: &str) -> Result<String, String> {
    let unbound =
        UnboundKey::new(&AES_256_GCM, key_bytes).map_err(|_| String::from("invalid key length for AES-256-GCM"))?;
    let sealing = LessSafeKey::new(unbound);

    // A fresh random nonce per value. Reusing one under the same key would
    // break GCM outright, so this is never derived from anything.
    let mut nonce_bytes = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| String::from("could not read from the system RNG"))?;
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);

    let mut buffer = plaintext.as_bytes().to_vec();
    sealing
        .seal_in_place_append_tag(nonce, Aad::from(name.as_bytes()), &mut buffer)
        .map_err(|_| String::from("encryption failed"))?;

    let mut out = Vec::with_capacity(NONCE_LEN + buffer.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&buffer);
    Ok(format!("{PREFIX}{}", B64.encode(&out)))
}

/// Decrypt one value produced by `encrypt_with`.
pub fn decrypt_with(key_bytes: &[u8], name: &str, value: &str) -> Result<String, String> {
    let body = value
        .trim()
        .strip_prefix(PREFIX)
        .ok_or_else(|| format!("{name} is not an {PREFIX} value"))?;
    let raw = B64.decode(body.as_bytes()).map_err(|e| format!("{name}: invalid base64: {e}"))?;
    if raw.len() <= NONCE_LEN {
        return Err(format!("{name}: encrypted value is truncated"));
    }

    let (nonce_bytes, sealed) = raw.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce_bytes).map_err(|_| format!("{name}: bad nonce"))?;
    let unbound =
        UnboundKey::new(&AES_256_GCM, key_bytes).map_err(|_| String::from("invalid key length for AES-256-GCM"))?;
    let opening = LessSafeKey::new(unbound);

    let mut buffer = sealed.to_vec();
    let plaintext = opening.open_in_place(nonce, Aad::from(name.as_bytes()), &mut buffer).map_err(|_| {
        format!(
            "{name}: could not decrypt — the key in {} does not match, the value was copied \
             from a different setting, or it was altered",
            key_path().display()
        )
    })?;
    let out = String::from_utf8(plaintext.to_vec()).map_err(|e| format!("{name}: not valid UTF-8: {e}"));
    buffer.zeroize(); // held the decrypted bytes in place
    out
}

/// Seal an entire config file: one `enc:v1:` line holding the whole plaintext,
/// so nothing about the contents — not even which settings exist — is readable
/// without the key.
pub fn seal_file(key_bytes: &[u8], plaintext: &str) -> Result<String, String> {
    encrypt_with(key_bytes, FILE_AAD, plaintext)
}

/// Open a file sealed by `seal_file`.
pub fn unseal_file(key_bytes: &[u8], sealed: &str) -> Result<String, String> {
    let line = sealed
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(PREFIX))
        .ok_or_else(|| String::from("file is not sealed"))?;
    decrypt_with(key_bytes, FILE_AAD, line)
}

/// Read a config file's text, transparently unsealing a whole-file seal.
///
/// A sealed file that will not open is an error rather than an empty config:
/// behaving as though nothing were configured would send every caller down its
/// "not set" path, which for credentials looks like a different bug entirely.
pub fn read_config(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if !is_sealed_file(&text) {
        return Ok(text);
    }
    let (key, _) = key().map_err(|e| {
        format!("{} is sealed, but the key could not be read: {e}", path.display())
    })?;
    unseal_file(&key, &text).map_err(|e| format!("{}: {e}", path.display()))
}

/// One `KEY=value` from a config file, opened if it is sealed.
///
/// `None` — with the reason on stderr — when a sealed value will not open, so
/// it reads as unset rather than as its own ciphertext. A ciphertext used as a
/// password fails somewhere far away from here.
pub fn resolve(name: &str, value: &str) -> Option<String> {
    if !is_encrypted(value) {
        return Some(value.to_string());
    }
    let (key, _) = match key() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("{name} is sealed but no key is available: {e}");
            return None;
        }
    };
    match decrypt_with(&key, name, value) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("{e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> Zeroizing<Vec<u8>> {
        material_to_key(&generate_key().unwrap()).unwrap()
    }

    #[test]
    fn a_value_round_trips() {
        let k = test_key();
        let sealed = encrypt_with(&k, "AWS_SECRET_ACCESS_KEY", "s3cr3t/value+with=chars").unwrap();
        assert!(sealed.starts_with(PREFIX));
        assert!(is_encrypted(&sealed));
        assert_eq!(
            decrypt_with(&k, "AWS_SECRET_ACCESS_KEY", &sealed).unwrap(),
            "s3cr3t/value+with=chars"
        );
    }

    #[test]
    fn the_same_plaintext_seals_differently_every_time() {
        // A fresh nonce per value. Identical ciphertexts would tell anyone
        // holding the file that two settings have the same value.
        let k = test_key();
        let a = encrypt_with(&k, "KEY", "same").unwrap();
        let b = encrypt_with(&k, "KEY", "same").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_value_cannot_be_moved_to_another_setting() {
        // The AAD binding. Without it, pasting the secret over the key id
        // would quietly swap two credentials.
        let k = test_key();
        let sealed = encrypt_with(&k, "AWS_SECRET_ACCESS_KEY", "the-secret").unwrap();
        assert!(decrypt_with(&k, "AWS_ACCESS_KEY_ID", &sealed).is_err());
    }

    #[test]
    fn another_key_cannot_open_it() {
        let sealed = encrypt_with(&test_key(), "KEY", "value").unwrap();
        assert!(decrypt_with(&test_key(), "KEY", &sealed).is_err());
    }

    #[test]
    fn a_tampered_value_fails_rather_than_decoding_to_garbage() {
        // What GCM buys over CBC: the tag catches the edit.
        let k = test_key();
        let sealed = encrypt_with(&k, "KEY", "value").unwrap();
        let mut raw = B64.decode(sealed.strip_prefix(PREFIX).unwrap()).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let tampered = format!("{PREFIX}{}", B64.encode(&raw));
        assert!(decrypt_with(&k, "KEY", &tampered).is_err());
    }

    #[test]
    fn a_whole_file_round_trips_and_is_recognised() {
        let k = test_key();
        let plain = "AWS_ACCESS_KEY_ID=AKIA123\nAWS_SECRET_ACCESS_KEY=shhh\n";
        let sealed = seal_file(&k, plain).unwrap();
        assert!(is_sealed_file(&sealed));
        assert!(!is_sealed_file(plain));
        assert_eq!(unseal_file(&k, &sealed).unwrap(), plain);
        // Not even the setting names survive in the file.
        assert!(!sealed.contains("AWS_ACCESS_KEY_ID"));
    }

    #[test]
    fn a_sealed_file_is_not_a_sealed_value_or_the_other_way_round() {
        // Distinct AAD, so the two shapes cannot be swapped.
        let k = test_key();
        let file = seal_file(&k, "A=1\n").unwrap();
        assert!(decrypt_with(&k, "A", &file).is_err());
        let value = encrypt_with(&k, "A", "1").unwrap();
        assert!(unseal_file(&k, &value).is_err());
    }

    #[test]
    fn a_comment_before_the_seal_still_reads_as_sealed() {
        let k = test_key();
        let sealed = format!("# huntwell global, sealed\n{}\n", seal_file(&k, "A=1\n").unwrap());
        assert!(is_sealed_file(&sealed));
        assert_eq!(unseal_file(&k, &sealed).unwrap(), "A=1\n");
    }

    #[test]
    fn plaintext_passes_through_resolve_untouched() {
        // The common case on a dev box: no key anywhere, nothing sealed.
        assert_eq!(resolve("A", "plain").as_deref(), Some("plain"));
    }

    #[test]
    fn a_short_key_is_refused() {
        assert!(material_to_key(&B64.encode([0u8; 16])).is_err());
        assert!(material_to_key("not base64!").is_err());
        assert!(material_to_key(&B64.encode([0u8; KEY_LEN])).is_ok());
    }
}

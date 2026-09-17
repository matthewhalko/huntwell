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

/// The obfuscated key blob `build.rs` produced, if this build had a key.
const BLOB: Option<&str> = option_env!("HUNTWELL_GENESIS_BLOB");

/// Reverses `build.rs`'s `obfuscate`: hex → de-interleave → XOR out the pad.
#[inline(never)]
fn deobfuscate(blob: &str) -> Option<Zeroizing<Vec<u8>>> {
    if blob.is_empty() || blob.len() % 4 != 0 {
        return None;
    }
    let bytes: Vec<u8> = (0..blob.len() / 2)
        .map(|i| u8::from_str_radix(&blob[i * 2..i * 2 + 2], 16).ok())
        .collect::<Option<_>>()?;
    let mut out = Zeroizing::new(Vec::with_capacity(bytes.len() / 2));
    for pair in bytes.chunks_exact(2) {
        out.push(pair[0] ^ pair[1]);
    }
    Some(out)
}

/// The key material this binary carries, reassembled. Zeroized on drop.
fn embedded_material() -> Option<Zeroizing<String>> {
    let raw = deobfuscate(BLOB?)?;
    let text = std::str::from_utf8(&raw).ok()?.trim().to_string();
    (!text.is_empty()).then(|| Zeroizing::new(text))
}

/// True when this binary was built with a key inside it.
pub fn has_embedded_key() -> bool {
    BLOB.map(|b| !b.is_empty()).unwrap_or(false)
}

/// Which environment this build is for — `prod` or `local` — as `build.rs`
/// stamped it. Everything keyed off the environment follows this, so it
/// always agrees with the key actually inside the binary.
pub fn embedded_variant() -> &'static str {
    match option_env!("HUNTWELL_BUILD_VARIANT") {
        Some("prod") => "prod",
        Some("local") => "local",
        _ if cfg!(debug_assertions) => "local",
        _ => "prod",
    }
}

/// Where a key file lives, for a build that embedded no key.
///
/// `HUNTWELL_GENESIS_KEY_FILE` names it outright. Otherwise `genesis_<variant>`
/// (or `.txt`, or plain `genesis`): in `local-infra/` for a debug build, and
/// beside or above the executable or the working directory for a deployed one.
pub fn key_path() -> PathBuf {
    if let Some(explicit) = std::env::var("HUNTWELL_GENESIS_KEY_FILE").ok().filter(|v| !v.trim().is_empty()) {
        return PathBuf::from(explicit);
    }
    let variant = embedded_variant();
    let names = [format!("genesis_{variant}"), format!("genesis_{variant}.txt"), String::from("genesis")];
    if cfg!(debug_assertions) {
        let dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../local-infra"));
        return names.iter().map(|n| dir.join(n)).find(|p| p.is_file()).unwrap_or_else(|| dir.join(&names[0]));
    }
    names
        .iter()
        .find_map(|n| crate::config::find_upward(n, false))
        .unwrap_or_else(|| PathBuf::from(&names[0]))
}

/// The key material to use: embedded first, else the key file.
fn key_material() -> Option<Zeroizing<String>> {
    embedded_material().or_else(|| {
        let raw = Zeroizing::new(std::fs::read_to_string(key_path()).ok()?);
        let line = raw.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#'))?;
        Some(Zeroizing::new(line.to_string()))
    })
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

/// Fixed salt and cost for turning a passphrase into an AES key. Constant on
/// purpose: the same passphrase must give the same key on every machine.
const KDF_SALT: &[u8] = b"huntwell-genesis-v1";
const KDF_ITERATIONS: u32 = 600_000;

/// Key material → 32 bytes. Base64 of exactly 32 bytes is used directly;
/// anything else is a passphrase, stretched with PBKDF2-HMAC-SHA256 — which is
/// what a `genesis_prod.txt` holding an `openssl -pass file:` passphrase is.
pub fn material_to_key(material: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let trimmed = material.trim();
    if trimmed.is_empty() {
        return Err(String::from("key material is empty"));
    }
    if let Ok(raw) = B64.decode(trimmed.as_bytes()) {
        if raw.len() == KEY_LEN {
            return Ok(Zeroizing::new(raw));
        }
    }
    let mut key = Zeroizing::new(vec![0u8; KEY_LEN]);
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        std::num::NonZeroU32::new(KDF_ITERATIONS).expect("non-zero"),
        KDF_SALT,
        trimmed.as_bytes(),
        &mut key,
    );
    Ok(key)
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

/// The key this process should use, and where it came from: the one compiled
/// in if there is one, otherwise the key file.
///
/// The source is returned so the tooling can *say* it: sealing with the wrong
/// environment's key produces a file that looks fine and cannot be opened on
/// the server, and this line is the only warning anyone gets.
pub fn key() -> Result<(Zeroizing<Vec<u8>>, String), String> {
    if let Some(material) = embedded_material() {
        return material_to_key(&material).map(|k| (k, format!("the {} key embedded in this binary", embedded_variant())));
    }
    let path = key_path();
    read_key(&path).map(|k| (k, path.display().to_string()))
}

/// Whether a key is available at all — embedded, or a file.
pub fn have_key() -> bool {
    key_material().is_some()
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
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    // Park River's other form, and the one an operator makes by hand on the
    // machine holding the key:
    //   openssl enc -aes-256-cbc -pbkdf2 -iter 600000 -salt -pass file:genesis -in global.plain -out global
    // Binary, so it is recognised before anything reads it as text.
    if is_openssl_file(&bytes) {
        let passphrase = passphrase().ok_or_else(|| {
            format!(
                "{} is encrypted with `openssl enc`, but this binary embedded no genesis key \
                 and none was found at {}",
                path.display(),
                key_path().display()
            )
        })?;
        return decrypt_openssl(&passphrase, &bytes).map_err(|e| format!("{}: {e}", path.display()));
    }
    let text = decode_text(&bytes).map_err(|hint| format!("{} is not valid UTF-8{hint}", path.display()))?;
    if !is_sealed_file(&text) {
        return Ok(text);
    }
    let (key, _) = key().map_err(|e| {
        format!("{} is sealed, but the key could not be read: {e}", path.display())
    })?;
    unseal_file(&key, &text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Magic prefix `openssl enc -salt` writes, followed by the 8-byte salt.
const OPENSSL_MAGIC: &[u8] = b"Salted__";

/// Must match the `-iter` the file was written with — Park River's value.
const OPENSSL_ITERATIONS: u32 = 600_000;

/// True when the bytes are an `openssl enc -salt` file.
pub fn is_openssl_file(bytes: &[u8]) -> bool {
    bytes.len() > OPENSSL_MAGIC.len() + 8 && bytes.starts_with(OPENSSL_MAGIC)
}

/// The genesis file as `openssl -pass file:` reads it: the first line,
/// verbatim, without its newline. Not `read_key`'s comment-skipping rule —
/// openssl takes the first line whatever it holds, and only the same rule
/// agrees with it.
pub fn passphrase() -> Option<Zeroizing<Vec<u8>>> {
    if let Some(material) = embedded_material() {
        return Some(Zeroizing::new(material.as_bytes().to_vec()));
    }
    let contents = Zeroizing::new(std::fs::read_to_string(key_path()).ok()?);
    let first = contents.lines().next()?.trim_end();
    (!first.is_empty()).then(|| Zeroizing::new(first.as_bytes().to_vec()))
}

/// Decrypt `openssl enc -aes-256-cbc -pbkdf2 -iter 600000 -salt -pass file:<genesis>`.
///
/// CBC is unauthenticated: a wrong key usually fails on padding, but can
/// decode to garbage, which the UTF-8 check then catches. That is why this is
/// accepted for the file an operator supplies and `seal-file` still writes GCM.
pub fn decrypt_openssl(passphrase: &[u8], bytes: &[u8]) -> Result<String, String> {
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    if !is_openssl_file(bytes) {
        return Err(String::from("not an openssl `Salted__` file"));
    }
    let salt = &bytes[OPENSSL_MAGIC.len()..OPENSSL_MAGIC.len() + 8];
    let ciphertext = &bytes[OPENSSL_MAGIC.len() + 8..];
    // 48 bytes: 32 of key then 16 of IV, exactly as `openssl enc` derives them.
    let mut key_iv = Zeroizing::new([0u8; 48]);
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        std::num::NonZeroU32::new(OPENSSL_ITERATIONS).expect("non-zero"),
        salt,
        passphrase,
        &mut key_iv[..],
    );
    let wrong = || {
        String::from(
            "could not decrypt `global` — the genesis key does not match the one it was encrypted with, \
             or it was written with other openssl flags (expected -aes-256-cbc -pbkdf2 -iter 600000 -salt)",
        )
    };
    let plain = Zeroizing::new(
        cbc::Decryptor::<aes::Aes256>::new_from_slices(&key_iv[..32], &key_iv[32..])
            .map_err(|_| wrong())?
            .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
            .map_err(|_| wrong())?,
    );
    String::from_utf8(plain.to_vec()).map_err(|_| wrong())
}

/// A config file's bytes as a string, converting UTF-16 rather than refusing it
/// — a file written in a Windows editor is correct and otherwise unreadable.
/// Anything that is text in neither encoding returns a hint instead.
pub fn decode_text(bytes: &[u8]) -> Result<String, String> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_string());
    }
    if let Some(text) = decode_utf16(bytes) {
        return Ok(text);
    }
    Err(encoding_hint(bytes))
}

/// UTF-16 in either byte order, with or without a byte-order mark. Only
/// reached for bytes that already failed as UTF-8.
fn decode_utf16(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 || bytes.len() % 2 != 0 {
        return None;
    }
    let (body, little_endian) = match bytes {
        [0xFF, 0xFE, rest @ ..] => (rest, true),
        [0xFE, 0xFF, rest @ ..] => (rest, false),
        _ => {
            let all_zero_at = |offset: usize| bytes.len() / 2 >= 4 && bytes.iter().skip(offset).step_by(2).all(|b| *b == 0);
            if all_zero_at(1) {
                (bytes, true)
            } else if all_zero_at(0) {
                (bytes, false)
            } else {
                return None;
            }
        }
    };
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|c| if little_endian { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
        .collect();
    char::decode_utf16(units).collect::<Result<String, _>>().ok()
}

/// Why a config file is not text, in terms of what to do about it.
fn encoding_hint(bytes: &[u8]) -> String {
    let head = &bytes[..bytes.len().min(64)];
    if head.starts_with(b"\x7fELF") || head.starts_with(&[0xCF, 0xFA, 0xED, 0xFE]) {
        return String::from(" — this is an executable, not a config file. Something was copied over it.");
    }
    if head.starts_with(&[0x1F, 0x8B]) {
        return String::from(" — this is gzip data. Decompress it first.");
    }
    if head.windows(5).any(|w| w == b"CD001") {
        return String::from(" — this looks like a disc image, not a config file.");
    }
    String::from(
        " — it is not an `openssl enc` file (no Salted__ header), and a sealed file is `enc:v1:` \
         followed by base64, which is plain ASCII. Write the KEY=value lines as UTF-8 text and \
         encrypt them with openssl or `huntwell genesis seal-file`.",
    )
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
    fn anything_but_a_32_byte_key_is_a_passphrase() {
        assert!(material_to_key("").is_err());
        let raw = [7u8; KEY_LEN];
        assert_eq!(&*material_to_key(&B64.encode(raw)).unwrap(), &raw);
        // A passphrase is stretched, deterministically.
        let a = material_to_key("a forty character passphrase from a file").unwrap();
        assert_eq!(a.len(), KEY_LEN);
        assert_eq!(a, material_to_key("a forty character passphrase from a file").unwrap());
        assert_ne!(a, material_to_key("another passphrase").unwrap());
    }

    #[test]
    fn the_embedded_key_reassembles() {
        // build.rs's obfuscate for "ab" with pad [0x10, 0x20].
        let blob = format!("{:02x}10{:02x}20", b'a' ^ 0x10, b'b' ^ 0x20);
        assert_eq!(&*deobfuscate(&blob).unwrap(), b"ab");
        assert!(deobfuscate("abc").is_none());
    }

    #[test]
    fn a_utf16_file_is_read_and_binary_is_explained() {
        // As Windows Notepad saves it: a byte-order mark, then little-endian.
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("KEY=x\n".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert_eq!(decode_text(&utf16).unwrap(), "KEY=x\n");
        assert!(decode_text(&[0x7f, b'E', b'L', b'F', 0xff, 0xfe, 0x00]).unwrap_err().contains("executable"));
    }

    #[test]
    fn a_global_written_by_openssl_enc_opens() {
        let dir = std::env::temp_dir().join(format!("hw-openssl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (key, plain, out) = (dir.join("genesis"), dir.join("plain"), dir.join("global"));
        std::fs::write(&key, "c2VjcmV0LWtleS1tYXRlcmlhbC1mb3ItdGVzdHMhIQ==\n").unwrap();
        std::fs::write(&plain, "KEY=AKIAEXAMPLE\nSECRET=abc/def+ghi\n").unwrap();
        let ran = std::process::Command::new("openssl")
            .args(["enc", "-aes-256-cbc", "-pbkdf2", "-iter", "600000", "-salt", "-pass"])
            .arg(format!("file:{}", key.display()))
            .arg("-in").arg(&plain).arg("-out").arg(&out)
            .status();
        let Ok(status) = ran else { return }; // no openssl CLI on this machine
        assert!(status.success());
        let bytes = std::fs::read(&out).unwrap();
        assert!(is_openssl_file(&bytes));
        let opened = decrypt_openssl(b"c2VjcmV0LWtleS1tYXRlcmlhbC1mb3ItdGVzdHMhIQ==", &bytes).unwrap();
        assert_eq!(opened, "KEY=AKIAEXAMPLE\nSECRET=abc/def+ghi\n");
        assert!(decrypt_openssl(b"another key", &bytes).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

}

//! Concatenates the schema files listed in local-infra/db/schema.order into
//! one string the server applies at startup, so the SQL on disk is the only
//! source of truth and the binary never carries a `CREATE TABLE` of its own.
use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../local-infra/db");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let order = root.join("schema.order");
    println!("cargo:rerun-if-changed={}", order.display());
    let mut out = String::new();
    for line in fs::read_to_string(&order).unwrap_or_else(|e| panic!("schema.order: {e}")).lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let path = root.join("public").join(line);
        println!("cargo:rerun-if-changed={}", path.display());
        out.push_str(&format!("-- ===== {line} =====\n"));
        out.push_str(&fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())));
        out.push('\n');
    }
    fs::write(out_dir.join("schema.sql"), out).unwrap();
    // The UI bundle is embedded from UI/web/dist; an absent dist is fine for
    // `cargo test`, rust-embed just serves nothing.
    println!("cargo:rerun-if-changed=../UI/web/dist");

    embed_genesis();
}

/// Bakes the genesis key into the binary, obfuscated — Park River's build.rs.
///
/// | Build                   | Key embedded     | Opens              |
/// |-------------------------|------------------|--------------------|
/// | `cargo build`           | `genesis_local`  | the local `global` |
/// | `cargo build --release` | `genesis_prod`   | the prod `global`  |
///
/// Sources, in order: `HUNTWELL_GENESIS_KEY` in the build environment, then
/// `<HUNTWELL_GENESIS_DIR>/genesis_<variant>` (or `.txt`), defaulting to
/// `local-infra/`.
///
/// The material is XORed with a pad drawn fresh at build time and interleaved
/// with it, so the key is not in `strings` or a grep of the binary. It is not
/// a security boundary — the pad ships beside it — just the honest ceiling for
/// a key a binary must reassemble unaided.
fn embed_genesis() {
    println!("cargo:rerun-if-env-changed=HUNTWELL_GENESIS_KEY");
    println!("cargo:rerun-if-env-changed=HUNTWELL_GENESIS_DIR");
    println!("cargo:rerun-if-env-changed=HUNTWELL_GENESIS_VARIANT");

    // The profile picks the key; HUNTWELL_GENESIS_VARIANT is for a release
    // binary that must open the local environment's file.
    let variant = match env::var("HUNTWELL_GENESIS_VARIANT").as_deref().map(str::trim) {
        Ok("local") | Ok("test") => "local",
        Ok("prod") => "prod",
        Ok(other) if !other.is_empty() => panic!("HUNTWELL_GENESIS_VARIANT must be `local` or `prod`, not {other:?}"),
        _ => match env::var("PROFILE").as_deref() {
            Ok("release") => "prod",
            _ => "local",
        },
    };
    let dir = env::var("HUNTWELL_GENESIS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../local-infra"));
    for v in ["local", "prod"] {
        for name in [format!("genesis_{v}"), format!("genesis_{v}.txt")] {
            println!("cargo:rerun-if-changed={}", dir.join(name).display());
        }
    }

    let from_env = env::var("HUNTWELL_GENESIS_KEY").ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let material = match from_env {
        Some(v) => Some((v, String::from("HUNTWELL_GENESIS_KEY"))),
        None => [format!("genesis_{variant}"), format!("genesis_{variant}.txt")]
            .into_iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file())
            .and_then(|path| read_material(&path).map(|m| (m, path.display().to_string()))),
    };

    // The runtime follows the key actually inside the binary — which secret it
    // reads, which key file it falls back to.
    println!("cargo:rustc-env=HUNTWELL_BUILD_VARIANT={variant}");
    match material {
        Some((m, source)) => {
            println!("cargo:warning=genesis: embedded the {variant} key from {source}");
            println!("cargo:rustc-env=HUNTWELL_GENESIS_BLOB={}", obfuscate(m.as_bytes()));
        }
        None => {
            println!(
                "cargo:warning=genesis: no {variant} key found in {} — this binary cannot open an \
                 encrypted `global` unless a key file sits beside it",
                dir.display()
            );
            // Always defined, so the runtime can use option_env! without cfg.
            println!("cargo:rustc-env=HUNTWELL_GENESIS_BLOB=");
        }
    }
}

/// `interleave(material XOR pad, pad)` as hex. Reversed by
/// `genesis::deobfuscate`; the two change together.
fn obfuscate(material: &[u8]) -> String {
    use std::io::Read;
    let mut pad = vec![0u8; material.len()];
    fs::File::open("/dev/urandom")
        .expect("no /dev/urandom — genesis obfuscation needs a system RNG")
        .read_exact(&mut pad)
        .expect("short read from /dev/urandom");
    let mut out = String::with_capacity(material.len() * 4);
    for (m, p) in material.iter().zip(pad.iter()) {
        out.push_str(&format!("{:02x}{:02x}", m ^ p, p));
    }
    out
}

/// First non-empty, non-comment line — so a key file can carry a note.
fn read_material(path: &std::path::Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()?
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
}

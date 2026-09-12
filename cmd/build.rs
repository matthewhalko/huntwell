//! Concatenates the schema files listed in local-infra/db/schema.order into
//! one string the server applies at startup, so the SQL on disk is the only
//! source of truth and the binary never carries a `CREATE TABLE` of its own.
use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../local-infra/db");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // `schema.order` is the whole schema (the all-in-one `serve` / local-infra
    // path). `schema.order.auth` and `schema.order.core` are the per-database
    // subsets the k3d migrate Jobs apply. All are concatenated the same way and
    // embedded, so `huntwell migrate --schema <all|auth|core>` can pick one.
    for (order_name, dest_name) in [
        ("schema.order", "schema.sql"),
        ("schema.order.auth", "schema.auth.sql"),
        ("schema.order.core", "schema.core.sql"),
    ] {
        let order = root.join(order_name);
        println!("cargo:rerun-if-changed={}", order.display());
        let mut out = String::new();
        for line in fs::read_to_string(&order).unwrap_or_else(|e| panic!("{order_name}: {e}")).lines() {
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
        fs::write(out_dir.join(dest_name), out).unwrap();
    }
    // The UI bundle is embedded from UI/web/dist; an absent dist is fine for
    // `cargo test`, rust-embed just serves nothing.
    println!("cargo:rerun-if-changed=../UI/web/dist");
}

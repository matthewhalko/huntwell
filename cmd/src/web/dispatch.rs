//! How runs execute: in this process's children, or on the admin's workers.
//!
//! `RUN_DISPATCH` selects it:
//!
//! - `pool` — leave the run queued. The admin control plane's placement loop
//!   assigns it to a worker slot — a `huntwell-worker@N` in a worker VM, or a
//!   child of the admin's process pool on a dev box — and that slot claims and
//!   runs it. What production runs.
//! - unset, or anything else — `Local`: fork `huntwell run` as a child of this
//!   server. The all-in-one `huntwell serve` on a laptop.

/// Where a run executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Local,
    Pool,
}

pub fn mode() -> Mode {
    mode_from(std::env::var("RUN_DISPATCH").ok().as_deref())
}

fn mode_from(value: Option<&str>) -> Mode {
    match value {
        Some("pool") => Mode::Pool,
        _ => Mode::Local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_is_the_only_remote_mode() {
        assert_eq!(mode_from(Some("pool")), Mode::Pool);
        assert_eq!(mode_from(None), Mode::Local);
        assert_eq!(mode_from(Some("")), Mode::Local);
    }
}

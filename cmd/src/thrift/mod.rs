//! Ways a run spends less without an agent doing the work.
//!
//! Every piece here follows one rule: **when in doubt, do what the run did
//! before this module existed.** A page that will not fetch, data that does not
//! parse, a match that is not certain — each falls through to the agent, so the
//! worst case is today's cost, never a wrong or missing row.
//!
//!   - [`page`]       read a public page over plain HTTP, behind the download guards
//!   - [`structured`] the page's own published data (schema.org), as a head start for enrich
//!   - [`watch`]      skip a scheduled run when the sites it watches show nothing new
//!   - [`trim`]       cut repeated page furniture out of what the browser tools return
//!   - [`twins`]      the same listing found on two sites is enriched once
//!   - [`filters`]    rows outside a column's stated limits are dropped before enrich
//!   - [`rounds`]     the search as one short call per site, each with a page budget
//!
//! `HUNTWELL_THRIFT_OFF` switches pieces off by name (comma-separated, or `all`).

pub mod filters;
pub mod page;
pub mod rounds;
pub mod structured;
pub mod trim;
pub mod twins;
pub mod watch;

/// Whether a piece is on. All are, unless named in `HUNTWELL_THRIFT_OFF`.
pub fn on(piece: &str) -> bool {
    match crate::config::get("HUNTWELL_THRIFT_OFF") {
        Some(raw) => !raw.split([',', ' ']).map(str::trim).any(|p| p.eq_ignore_ascii_case(piece) || p.eq_ignore_ascii_case("all")),
        None => true,
    }
}

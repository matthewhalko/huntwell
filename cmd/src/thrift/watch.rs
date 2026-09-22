//! A scheduled run that would find nothing new is not run.
//!
//! Most scheduled plans watch the same few listing pages, and on most days
//! those pages have not gained a listing. Finding that out with an agent costs
//! a whole run. Finding it out with an HTTP request costs nothing.
//!
//! **Which pages.** Not the sites someone typed — a home page says nothing
//! about new listings. A page is watched only once it has *shown* that it is a
//! listing page for this plan: it was visited by a run, it can be read over
//! plain HTTP, and it links to at least [`PROOF`] of the plan's stored results.
//! That is evidence, the same on every site, and a page that stops showing it
//! stops being watched.
//!
//! **What "nothing new" means.** A listing page that gains a listing gains a
//! link. So: every watched page was read, and none carries a same-site link
//! that was not there last time. Rotating adverts make new links, which only
//! ever errs toward running.
//!
//! **What it cannot see.** A page sorted so that new listings land on page
//! three. For that reason a plan is never skipped more than [`MAX_SKIPS`]
//! times running — the next run is a real one regardless, and re-learns the
//! pages. Runs someone starts by hand are never skipped.

use std::collections::BTreeSet;

use crate::store::{self, Db};

/// Stored results a page must link to before it counts as a listing page.
const PROOF: usize = 3;
/// Consecutive scheduled runs that may be skipped.
const MAX_SKIPS: i64 = 2;
/// Visited pages tried as candidates after a run.
const CANDIDATES: usize = 12;

fn max_skips() -> i64 {
    crate::config::get("HUNTWELL_WATCH_MAX_SKIPS").and_then(|v| v.trim().parse().ok()).unwrap_or(MAX_SKIPS)
}

/// `Some(pages checked)` when this scheduled run can be skipped. `None` —
/// run as normal — whenever anything is uncertain.
pub async fn nothing_new(db: &Db, account_id: i64, plan_id: i64) -> Option<usize> {
    if !super::on("watch") {
        return None;
    }
    if store::skip_streak(db, account_id, plan_id).await.ok()? >= max_skips() {
        println!("  watch       skipped {} run(s) in a row — running this one for real", max_skips());
        return None;
    }
    let watched = store::watched_pages(db, account_id, plan_id).await.ok()?;
    if watched.is_empty() {
        return None;
    }
    for page in &watched {
        let now = match super::page::fetch(&page.url).await {
            Ok(p) => super::page::same_site_links(&p.html, &page.url),
            Err(e) => {
                println!("  watch       could not read {} ({}) — running", crate::guard::safe_for_log(&page.url, 80), crate::guard::safe_for_log(&format!("{e:#}"), 80));
                return None;
            }
        };
        let before: BTreeSet<&str> = page.links.iter().map(String::as_str).collect();
        // A page that came back without its listings is a different page
        // (a block, an error shell), not an unchanged one.
        if now.len() * 2 < before.len() {
            println!("  watch       {} came back with far fewer links — running", crate::guard::safe_for_log(&page.url, 80));
            return None;
        }
        if let Some(new) = now.iter().find(|l| !before.contains(l.as_str())) {
            println!("  watch       new link on {}: {} — running", crate::guard::safe_for_log(&page.url, 60), crate::guard::safe_for_log(new, 80));
            return None;
        }
    }
    Some(watched.len())
}

/// After a real run: work out which visited pages are listing pages for this
/// plan, and record the links they carry now. Best effort, never an error.
pub async fn remember(db: &Db, account_id: i64, plan_id: i64) {
    if !super::on("watch") {
        return;
    }
    let results: BTreeSet<String> = match store::result_urls(db, account_id, plan_id).await {
        Ok(urls) => urls.iter().map(|u| super::page::link_key(u)).collect(),
        Err(_) => return,
    };
    if results.len() < PROOF {
        return;
    }
    let Ok(mut visited) = store::list_visited_pages(db, plan_id, 200).await else { return };
    // Most recently seen first: those are the pages the plan reads now.
    visited.reverse();
    let mut kept: Vec<String> = Vec::new();
    for page in visited.iter().filter(|p| !results.contains(&super::page::link_key(&p.url))).take(CANDIDATES) {
        let Ok(fetched) = super::page::fetch(&page.url).await else { continue };
        let links = super::page::same_site_links(&fetched.html, &page.url);
        let proof = links.iter().filter(|l| results.contains(*l)).count();
        if proof < PROOF {
            continue;
        }
        let links: Vec<String> = links.into_iter().take(3000).collect();
        if store::upsert_watched_page(db, account_id, plan_id, &page.url_key, &page.url, &links, proof as i32).await.is_ok() {
            kept.push(page.url_key.clone());
        }
    }
    let _ = store::forget_watched_pages_except(db, account_id, plan_id, &kept).await;
    if !kept.is_empty() {
        println!("  watch       {} listing page(s) will be checked before the next scheduled run", kept.len());
    }
}

//! Less page for the same information.
//!
//! The browser tools describe a page as an accessibility tree, and an agent
//! pays to read every line of it — then again on every later step, because the
//! conversation is re-read each turn. Most of a tree is not the page's content:
//! wrappers around wrappers, icon glyphs, decorative images, and the same site
//! header and footer on every page visited.
//!
//! The rules here are about tree *structure*, the same for every site:
//!
//!   - a nameless, textless leaf says nothing, and goes;
//!   - a wrapper with one child is replaced by that child;
//!   - `[cursor=pointer]` goes from things that are obviously clickable;
//!   - text that only repeats the name of the control it sits in, or beside,
//!     is said once;
//!   - refs go from plain structure (table cells, list items, unclickable
//!     wrappers) — refs exist to click and type with, and nobody clicks those;
//!   - a header, navigation or footer identical to one already shown for this
//!     site is cut down to its input fields (its links were listed the first
//!     time, as full URLs, so they can still be opened);
//!   - a drop-down with hundreds of options keeps the first few dozen and says
//!     how many more there are.
//!
//! Nothing in `main` content is ever dropped for being long: a listing page
//! *is* a long list. Every link, button, field and anything styled as
//! clickable keeps its ref, so what the agent can act on is unchanged.
//!
//! [`watch`] applies it. The browser tool server writes each snapshot to a
//! file and tells the agent where; the agent reads that file on its next turn,
//! a model round-trip later. A thread in the run process watches the directory
//! and rewrites each snapshot as soon as it is complete — long before the
//! agent asks for it. Nothing sits between the agent and its tools: if the
//! thread is late, the agent reads the page untrimmed, which is today's cost.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Options kept in a long drop-down.
const MAX_OPTIONS: usize = 40;

/// One line of the tree and what hangs under it.
#[derive(Debug, Clone)]
struct Node {
    /// The line after its `- `.
    text: String,
    /// Lines that are not list items (a block scalar's body), kept verbatim.
    trailing: Vec<String>,
    children: Vec<Node>,
}

impl Node {
    fn role(&self) -> &str {
        let end = self.text.find([' ', ':', '[']).unwrap_or(self.text.len());
        &self.text[..end]
    }
    fn named(&self) -> bool {
        self.text[self.role().len()..].trim_start().starts_with('"')
    }
    /// The text after the attributes — `CL` in `generic [ref=e7]: CL`.
    fn value(&self) -> &str {
        // Past any quoted name, so a colon inside the name is not the split.
        let after_name = match self.text.find('"') {
            Some(open) => self.text[open + 1..].find('"').map(|close| open + close + 2).unwrap_or(0),
            None => 0,
        };
        match self.text[after_name.min(self.text.len())..].find(": ") {
            Some(at) => self.text[after_name + at + 2..].trim(),
            None => "",
        }
    }
    fn has(&self, attr: &str) -> bool {
        self.text.contains(attr)
    }
    /// The quoted accessible name, if there is one.
    fn name(&self) -> Option<&str> {
        let rest = self.text[self.role().len()..].trim_start().strip_prefix('"')?;
        rest.find('"').map(|end| &rest[..end])
    }
    fn strip_ref(&mut self) {
        if let Some(at) = self.text.find(" [ref=") {
            let end = self.text[at + 1..].find(']').map(|e| at + e + 2).unwrap_or(self.text.len());
            self.text.replace_range(at..end, "");
        }
    }
}

/// Structure nobody clicks. A ref on one of these is never used.
const INERT: [&str; 12] =
    ["generic", "text", "cell", "row", "rowgroup", "list", "listitem", "paragraph", "table", "superscript", "strong", "emphasis"];

/// Icon fonts put private-use glyphs in the tree; they read as nothing.
fn says_nothing(s: &str) -> bool {
    s.chars().all(|c| c.is_whitespace() || matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0x10FFFF | 0xFE00..=0xFE0F | 0x200B..=0x200D))
}

fn parse(yaml: &str) -> Option<Vec<Node>> {
    // (indent, node) stack; a line's parent is the nearest shallower item.
    let mut roots: Vec<Node> = Vec::new();
    let mut stack: Vec<(usize, Node)> = Vec::new();
    fn close(stack: &mut Vec<(usize, Node)>, roots: &mut Vec<Node>, down_to: usize) {
        while stack.last().is_some_and(|(i, _)| *i >= down_to) {
            let (_, done) = stack.pop().unwrap();
            match stack.last_mut() {
                Some((_, parent)) => parent.children.push(done),
                None => roots.push(done),
            }
        }
    }
    for line in yaml.lines() {
        let indent = line.len() - line.trim_start().len();
        let body = line.trim_start();
        if let Some(text) = body.strip_prefix("- ") {
            close(&mut stack, &mut roots, indent);
            stack.push((indent, Node { text: text.to_string(), trailing: Vec::new(), children: Vec::new() }));
        } else if body.is_empty() {
            continue;
        } else {
            // Not a list item: the body of a block scalar. Belongs to the last item.
            stack.last_mut()?.1.trailing.push(line.to_string());
        }
    }
    close(&mut stack, &mut roots, 0);
    (!roots.is_empty()).then_some(roots)
}

fn write(nodes: &[Node], depth: usize, out: &mut String) {
    for n in nodes {
        out.push_str(&"  ".repeat(depth));
        out.push_str("- ");
        out.push_str(&n.text);
        out.push('\n');
        for t in &n.trailing {
            out.push_str(t);
            out.push('\n');
        }
        write(&n.children, depth + 1, out);
    }
}

const CLICKABLE: [&str; 12] =
    ["link", "button", "checkbox", "radio", "tab", "menuitem", "option", "combobox", "textbox", "searchbox", "switch", "slider"];
const INPUTS: [&str; 4] = ["textbox", "searchbox", "combobox", "spinbutton"];
const FURNITURE: [&str; 3] = ["banner", "navigation", "contentinfo"];

/// Subtrees already shown, per site, so a repeat can be recognised.
#[derive(Default)]
pub struct Seen {
    furniture: HashSet<String>,
}

/// The identity of a subtree with its refs removed — refs are renumbered on
/// every snapshot, so they are the one thing that always differs.
fn fingerprint(host: &str, n: &Node) -> String {
    fn feed(n: &Node, h: &mut Sha256) {
        let mut text = n.text.clone();
        while let Some(at) = text.find("[ref=") {
            let end = text[at..].find(']').map(|e| at + e + 1).unwrap_or(text.len());
            text.replace_range(at..end, "");
        }
        h.update(text.as_bytes());
        h.update(b"\n");
        for c in &n.children {
            feed(c, h);
        }
    }
    let mut h = Sha256::new();
    h.update(host.as_bytes());
    feed(n, &mut h);
    hex::encode(&h.finalize()[..12])
}

fn collect_inputs(n: &Node, out: &mut Vec<Node>) {
    for c in &n.children {
        if INPUTS.contains(&c.role()) || (c.role() == "button" && c.text.to_ascii_lowercase().contains("search")) {
            out.push(Node { text: c.text.clone(), trailing: c.trailing.clone(), children: Vec::new() });
        } else {
            collect_inputs(c, out);
        }
    }
}

fn tidy(mut n: Node, host: &str, seen: &mut Seen, top: bool) -> Option<Node> {
    // Site furniture seen before on this site: keep its inputs, drop its links.
    if FURNITURE.contains(&n.role()) && n.children.len() > 3 {
        let id = fingerprint(host, &n);
        if !seen.furniture.insert(id) {
            let mut kept = Vec::new();
            collect_inputs(&n, &mut kept);
            kept.push(Node {
                text: format!("text: (this site's {} is the same as on the page before — its links were listed there and can be opened by URL)", n.role()),
                trailing: Vec::new(),
                children: Vec::new(),
            });
            n.children = kept;
            return Some(n);
        }
    }

    let children = std::mem::take(&mut n.children);
    n.children = children.into_iter().filter_map(|c| tidy(c, host, seen, false)).collect();

    // A long drop-down: the first few dozen options, and a count of the rest.
    if matches!(n.role(), "combobox" | "listbox") {
        let options = n.children.iter().filter(|c| c.role() == "option").count();
        if options > MAX_OPTIONS {
            let mut kept = 0;
            n.children.retain(|c| {
                if c.role() != "option" || c.has("[selected]") {
                    return true;
                }
                kept += 1;
                kept <= MAX_OPTIONS
            });
            n.children.push(Node {
                text: format!("text: ({} more options not shown — an option can still be selected by its label)", options - MAX_OPTIONS),
                trailing: Vec::new(),
                children: Vec::new(),
            });
        }
    }

    // Text that only repeats a name already on the page beside it: the label
    // under a radio button, the caption inside a link named the same.
    let own = n.name().map(str::to_string);
    let named_siblings: Vec<String> = n.children.iter().filter_map(|c| c.name().map(str::to_string)).collect();
    n.children.retain(|c| {
        let echo = c.children.is_empty() && !c.named() && matches!(c.role(), "generic" | "text" | "paragraph") && !c.has("[cursor=pointer]");
        let said = c.value();
        !(echo && !said.is_empty() && (own.as_deref() == Some(said) || named_siblings.iter().any(|s| s == said)))
    });

    let role = n.role().to_string();
    let quiet = !n.named() && says_nothing(n.value()) && n.trailing.is_empty();

    // A leaf that says nothing. A clickable one may be an icon button: kept.
    if n.children.is_empty() && quiet {
        let clickable = n.has("[cursor=pointer]");
        if role == "img" || role == "text" || (role == "generic" && !clickable) {
            return None;
        }
    }
    // A wrapper around exactly one thing is that thing.
    if role == "generic" && quiet && !top && n.children.len() == 1 && !n.has("[cursor=pointer]") && !n.has("[active]") {
        let only = n.children.pop().unwrap();
        if !only.role().starts_with('/') {
            return Some(only);
        }
        n.children.push(only);
    }
    if CLICKABLE.contains(&role.as_str()) {
        n.text = n.text.replace(" [cursor=pointer]", "");
    }
    if INERT.contains(&role.as_str()) && !n.has("[cursor=pointer]") && !top {
        n.strip_ref();
    }
    Some(n)
}

/// Trim one snapshot. Returns `None` — leave it alone — if the text is not a
/// tree this understands, or if trimming would not make it smaller.
pub fn trim_snapshot(yaml: &str, host: &str, seen: &mut Seen) -> Option<String> {
    let roots = parse(yaml)?;
    let kept: Vec<Node> = roots.into_iter().filter_map(|n| tidy(n, host, seen, true)).collect();
    if kept.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(yaml.len());
    write(&kept, 0, &mut out);
    (out.len() < yaml.len()).then_some(out)
}

// ---------------------------------------------------------------------------
// Watching the snapshot directory
// ---------------------------------------------------------------------------

/// First line of a snapshot this module has already rewritten.
const MARK: &str = "# trimmed by huntwell";
const POLL: Duration = Duration::from_millis(150);

/// Trims snapshots as they land in `dir`, until dropped.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<(usize, usize)>>,
}

impl Watcher {
    pub fn stop(mut self) -> (usize, usize) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().and_then(|t| t.join().ok()).unwrap_or((0, 0))
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Start trimming snapshots written under `dir`. `None` when the piece is off.
pub fn watch(dir: PathBuf) -> Option<Watcher> {
    if !super::on("trim") {
        return None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::spawn(move || {
        let mut seen = Seen::default();
        let mut sizes: HashMap<PathBuf, u64> = HashMap::new();
        let mut done: HashSet<PathBuf> = HashSet::new();
        let mut saved = (0usize, 0usize);
        while !flag.load(Ordering::Relaxed) {
            for path in snapshot_files(&dir) {
                if done.contains(&path) {
                    continue;
                }
                // A file still being written grows between polls; one that
                // held still for a poll is whole.
                let Ok(meta) = std::fs::metadata(&path) else { continue };
                let size = meta.len();
                if sizes.insert(path.clone(), size) != Some(size) {
                    continue;
                }
                done.insert(path.clone());
                if let Some((before, after)) = trim_file(&path, &mut seen) {
                    saved.0 += before;
                    saved.1 += after;
                }
            }
            std::thread::sleep(POLL);
        }
        saved
    });
    Some(Watcher { stop, thread: Some(thread) })
}

fn snapshot_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        .collect();
    files.sort();
    files
}

/// Rewrite one snapshot in place. `Some((bytes before, bytes after))` when it
/// was trimmed; `None` when it was left as it was.
pub fn trim_file(path: &Path, seen: &mut Seen) -> Option<(usize, usize)> {
    let yaml = std::fs::read_to_string(path).ok()?;
    if yaml.starts_with(MARK) {
        return None;
    }
    let host = host_of_snapshot(path);
    let smaller = trim_snapshot(&yaml, &host, seen)?;
    let out = format!("{MARK}\n{smaller}");
    // Written whole to a sibling and renamed, so a reader never sees half a file.
    let tmp = path.with_extension("yml.tmp");
    std::fs::write(&tmp, &out).ok()?;
    std::fs::rename(&tmp, path).ok()?;
    Some((yaml.len(), out.len()))
}

/// The site a snapshot is of: the run's browser tools log `- Page URL:` into
/// the tool reply, not the file, so the file itself is asked. Its first
/// `/url:` line is a link on the page, which is the page's own site nearly
/// always — and a wrong host only means a header is trimmed one page late.
fn host_of_snapshot(path: &Path) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| {
            s.lines()
                .filter_map(|l| l.trim().strip_prefix("- /url: "))
                .filter_map(|u| super::page::host_of(u.trim()))
                .next()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"- generic [active] [ref=e1]:
  - banner [ref=e3]:
    - link "Home" [ref=e5] [cursor=pointer]:
      - /url: https://cars.test/
      - generic [ref=e6]: 
    - link "Sell" [ref=e7] [cursor=pointer]:
      - /url: https://cars.test/sell
    - link "Finance" [ref=e8] [cursor=pointer]:
      - /url: https://cars.test/finance
    - searchbox "Search cars" [ref=e9]
  - main [ref=e33]:
    - generic [ref=e34]:
      - generic [ref=e35]:
        - link "2022 Subaru Crosstrek: Limited" [ref=e36] [cursor=pointer]:
          - /url: https://cars.test/car/1
    - img [ref=e40]
    - img "Front view of the car" [ref=e41]
    - generic [ref=e42] [cursor=pointer]
    - generic [ref=e43]: $27,995
"#;

    fn trimmed(seen: &mut Seen) -> String {
        trim_snapshot(PAGE, "cars.test", seen).expect("smaller")
    }

    #[test]
    fn nothing_the_agent_could_read_or_click_is_lost() {
        let out = trimmed(&mut Seen::default());
        for keep in ["https://cars.test/car/1", "2022 Subaru Crosstrek: Limited", "[ref=e36]", "$27,995", "Front view of the car", "searchbox \"Search cars\" [ref=e9]", "[ref=e42] [cursor=pointer]"] {
            assert!(out.contains(keep), "lost {keep:?}:\n{out}");
        }
    }

    #[test]
    fn a_label_is_said_once_and_plain_structure_carries_no_ref() {
        let yaml = r#"- main [ref=e1]:
  - generic [ref=e2]:
    - radio "Wide" [ref=e3]
    - generic [ref=e4]: Wide
    - generic [ref=e9]: Narrow is different
  - link "2022 Crosstrek" [ref=e5] [cursor=pointer]:
    - /url: https://cars.test/car/1
    - generic [ref=e6]: 2022 Crosstrek
  - row [ref=e7]:
    - cell [ref=e8]: $27,995
"#;
        let out = trim_snapshot(yaml, "cars.test", &mut Seen::default()).unwrap();
        assert_eq!(out.matches("Wide").count(), 1, "{out}");
        assert_eq!(out.matches("2022 Crosstrek").count(), 1, "{out}");
        assert!(out.contains("Narrow is different") && out.contains("- cell: $27,995"), "{out}");
        assert!(out.contains("[ref=e3]") && out.contains("[ref=e5]"), "controls keep their refs:\n{out}");
        assert!(!out.contains("[ref=e7]") && !out.contains("[ref=e8]"), "{out}");
    }

    /// `HUNTWELL_TRIM_SAMPLES=/dir/of/yml cargo test measure_samples -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn measure_samples() {
        let dir = std::env::var("HUNTWELL_TRIM_SAMPLES").expect("set HUNTWELL_TRIM_SAMPLES");
        let mut seen = Seen::default();
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "yml")).collect();
        entries.sort_by_key(|e| e.file_name());
        let (mut before, mut after) = (0, 0);
        for entry in entries {
            let yaml = std::fs::read_to_string(entry.path()).unwrap();
            let out = trim_snapshot(&yaml, "sample", &mut seen).unwrap_or_else(|| yaml.clone());
            if let Ok(dest) = std::env::var("HUNTWELL_TRIM_WRITE") {
                std::fs::write(std::path::Path::new(&dest).join(entry.file_name()), &out).unwrap();
            }
            before += yaml.len();
            after += out.len();
            println!("{:>8} -> {:>8}  ({:>2}% smaller)  {}", yaml.len(), out.len(), 100 - out.len() * 100 / yaml.len().max(1), entry.file_name().to_string_lossy());
        }
        println!("{before:>8} -> {after:>8}  ({:>2}% smaller)  TOTAL", 100 - after * 100 / before.max(1));
    }

    #[test]
    fn what_says_nothing_goes() {
        let out = trimmed(&mut Seen::default());
        assert!(!out.contains("[ref=e6]"), "an icon glyph is not content:\n{out}");
        assert!(!out.contains("img [ref=e40]"), "a nameless image is not content");
        assert!(!out.contains("[ref=e34]") && !out.contains("[ref=e35]"), "wrappers of one thing collapse:\n{out}");
        assert!(out.contains("- link \"Home\" [ref=e5]:"), "clickable things do not need to say so:\n{out}");
        assert!(out.len() < PAGE.len());
    }

    #[test]
    fn a_header_seen_before_keeps_its_search_box_and_drops_its_links() {
        let mut seen = Seen::default();
        let first = trimmed(&mut seen);
        assert!(first.contains("https://cars.test/finance"), "the first page lists the site's links in full");
        let second = trimmed(&mut seen);
        assert!(!second.contains("https://cars.test/finance"));
        assert!(second.contains("searchbox \"Search cars\""), "the search box is how the agent searches:\n{second}");
        assert!(second.contains("https://cars.test/car/1"), "main content is never cut");
        // Another site's header is another header.
        let other = trim_snapshot(PAGE, "trucks.test", &mut seen).unwrap();
        assert!(other.contains("https://cars.test/finance"));
    }

    #[test]
    fn a_long_drop_down_is_capped_and_says_so() {
        let mut yaml = String::from("- combobox \"Make\" [ref=e1]:\n");
        for i in 0..200 {
            yaml.push_str(&format!("  - option \"Make {i}\"{}\n", if i == 150 { " [selected]" } else { "" }));
        }
        let out = trim_snapshot(&yaml, "x.test", &mut Seen::default()).unwrap();
        assert_eq!(out.matches("- option").count(), MAX_OPTIONS + 1, "the selected option is always kept");
        assert!(out.contains("Make 150") && out.contains("160 more options"));
    }

    #[test]
    fn a_long_list_of_results_is_never_cut() {
        let mut yaml = String::from("- main [ref=e1]:\n  - list [ref=e2]:\n");
        for i in 0..500 {
            yaml.push_str(&format!("    - listitem [ref=r{i}]:\n      - link \"Car {i}\" [ref=l{i}] [cursor=pointer]:\n        - /url: https://cars.test/car/{i}\n"));
        }
        let out = trim_snapshot(&yaml, "cars.test", &mut Seen::default()).unwrap();
        assert_eq!(out.matches("/url: https://cars.test/car/").count(), 500);
    }

    #[test]
    fn text_that_is_not_a_tree_is_left_alone() {
        assert!(trim_snapshot("", "x", &mut Seen::default()).is_none());
        assert!(trim_snapshot("just some words\nand more", "x", &mut Seen::default()).is_none());
    }

    #[test]
    fn a_snapshot_written_to_the_watched_directory_is_trimmed_in_place() {
        let dir = std::env::temp_dir().join(format!("huntwell-trimwatch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let w = watch(dir.clone()).expect("on by default");
        let file = dir.join("page-1.yml");
        std::fs::write(&file, PAGE).unwrap();
        let mut waited = 0;
        while waited < 40 && !std::fs::read_to_string(&file).unwrap().starts_with(MARK) {
            std::thread::sleep(Duration::from_millis(100));
            waited += 1;
        }
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.starts_with(MARK), "not trimmed within 4s:\n{after}");
        assert!(after.len() < PAGE.len() && after.contains("https://cars.test/car/1"));
        // Not a snapshot: left alone.
        let other = dir.join("console-1.log");
        std::fs::write(&other, "x").unwrap();
        let (before, _) = w.stop();
        assert_eq!(before, PAGE.len());
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "x");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `HUNTWELL_TRIM_WATCH=/dir HUNTWELL_TRIM_SECS=60 cargo test watch_a_directory -- --ignored --nocapture`
    /// — a watcher to run beside a real agent, to see it trim before the agent reads.
    #[test]
    #[ignore]
    fn watch_a_directory() {
        let dir = PathBuf::from(std::env::var("HUNTWELL_TRIM_WATCH").expect("set HUNTWELL_TRIM_WATCH"));
        let secs: u64 = std::env::var("HUNTWELL_TRIM_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
        let w = watch(dir).unwrap();
        std::thread::sleep(Duration::from_secs(secs));
        let (before, after) = w.stop();
        println!("trimmed {before} -> {after} bytes");
    }

    #[test]
    fn a_trimmed_file_is_not_trimmed_twice() {
        let dir = std::env::temp_dir().join(format!("huntwell-trimtwice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("page-1.yml");
        std::fs::write(&file, PAGE).unwrap();
        let mut seen = Seen::default();
        assert!(trim_file(&file, &mut seen).is_some());
        assert!(trim_file(&file, &mut seen).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

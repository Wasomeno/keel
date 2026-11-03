//! The agent side of the browser pane, without the page: which sites an
//! agent may use, the per-chat request queue, what a snapshot reply says
//! (the whole snapshot, or what changed since the agent's last one), the
//! console ring buffer, and dialog wording. Pure, so it is tested here.

use std::collections::VecDeque;

// ---- allowed sites ----------------------------------------------------------

/// Settings → Browser's list, one site per comma, space, or line.
pub fn parse_sites(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in text
        .split([',', '\n', ' ', '\t', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let s = s.trim_end_matches('/').to_ascii_lowercase();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// Scheme, host, port (default ports filled in), and path of an absolute URL.
fn split_url(url: &str) -> Option<(String, String, u16, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, a)| a)
        .to_ascii_lowercase();
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || authority.starts_with('[') => match p.parse::<u16>() {
            Ok(p) => (h.to_string(), Some(p)),
            Err(_) => (authority.clone(), None),
        },
        _ => (authority.clone(), None),
    };
    let default = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => 0,
    };
    Some((scheme, host, port.unwrap_or(default), path.to_string()))
}

/// May an agent use `url`? An empty list allows every site. An entry is
/// `[scheme://]host[:port][/path]`: `example.com` covers its subdomains,
/// `*.example.com` only the subdomains; a port or scheme, when given, must
/// match. `about:` pages are always allowed; `data:` URLs only with no list.
pub fn site_allowed(url: &str, sites: &[String]) -> bool {
    if sites.is_empty() {
        return true;
    }
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("about:") {
        return true;
    }
    let url = lower.strip_prefix("blob:").unwrap_or(&lower);
    let Some((scheme, host, port, path)) = split_url(url) else {
        return false;
    };
    if scheme != "http" && scheme != "https" {
        return false;
    }
    sites.iter().any(|entry| {
        let entry = entry.trim().to_ascii_lowercase();
        let (want_scheme, rest) = match entry.split_once("://") {
            Some((s, r)) => (Some(s.to_string()), r.to_string()),
            None => (None, entry.clone()),
        };
        let Some((_, e_host, e_port, e_path)) = split_url(&format!(
            "{}://{rest}",
            want_scheme.as_deref().unwrap_or("x")
        )) else {
            return false;
        };
        let explicit_port = rest.split(['/', '?', '#']).next().is_some_and(|a| {
            a.rsplit_once(':')
                .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
        });
        if want_scheme.as_ref().is_some_and(|s| *s != scheme) {
            return false;
        }
        if (explicit_port || want_scheme.is_some()) && e_port != 0 && e_port != port {
            return false;
        }
        let host_ok = match e_host.strip_prefix("*.") {
            Some(base) => {
                host.len() > base.len()
                    && host.ends_with(base)
                    && host[..host.len() - base.len()].ends_with('.')
            }
            None => {
                host == e_host
                    || (host.ends_with(&e_host) && host[..host.len() - e_host.len()].ends_with('.'))
            }
        };
        host_ok && (e_path.is_empty() || path.starts_with(&e_path))
    })
}

// ---- the per-chat queue -----------------------------------------------------

/// A chat's browser requests: one runs at a time, the rest wait in order.
/// Nothing supersedes anything; only a cancel removes requests.
#[derive(Debug)]
pub struct Fifo<T> {
    items: VecDeque<T>,
    busy: bool,
}

impl<T> Default for Fifo<T> {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
            busy: false,
        }
    }
}

impl<T> Fifo<T> {
    /// Queue `item`. It comes straight back when nothing is running (the
    /// caller starts it; the queue is now busy).
    pub fn push(&mut self, item: T) -> Option<T> {
        if !self.busy && self.items.is_empty() {
            self.busy = true;
            return Some(item);
        }
        self.items.push_back(item);
        None
    }

    /// The running request finished: the next one to start, if any.
    pub fn done(&mut self) -> Option<T> {
        let next = self.items.pop_front();
        self.busy = next.is_some();
        next
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Remove the waiting requests `drop` picks, in order.
    pub fn cancel(&mut self, mut drop: impl FnMut(&T) -> bool) -> Vec<T> {
        let mut out = Vec::new();
        let mut keep = VecDeque::new();
        for item in self.items.drain(..) {
            if drop(&item) {
                out.push(item);
            } else {
                keep.push_back(item);
            }
        }
        self.items = keep;
        out
    }

    /// Everything waiting, and the queue idle.
    pub fn clear(&mut self) -> Vec<T> {
        self.busy = false;
        self.items.drain(..).collect()
    }
}

// ---- snapshot replies -------------------------------------------------------

/// A compact snapshot the agent saw: its page and lines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snap {
    pub url: String,
    pub text: String,
}

/// What an action's reply carries about the page afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapWant {
    /// What changed since the agent's last snapshot (the whole thing when
    /// the page changed, or the change is most of it).
    Diff,
    Full,
    None,
}

impl SnapWant {
    pub fn parse(v: Option<&str>, default: Self) -> Self {
        match v.map(str::trim) {
            Some("full") => Self::Full,
            Some("none") | Some("off") => Self::None,
            Some("diff") => Self::Diff,
            _ => default,
        }
    }
}

/// Most lines a diff lists before it says how many more.
const DIFF_LINES: usize = 60;

/// The page part of a reply: `next` in full, or its change from `prev`.
pub fn snapshot_reply(prev: Option<&Snap>, next: &Snap, want: SnapWant) -> String {
    let full = || {
        if next.text.is_empty() {
            "(no interactive elements)".to_string()
        } else {
            next.text.clone()
        }
    };
    match want {
        SnapWant::None => return String::new(),
        SnapWant::Full => return full(),
        SnapWant::Diff => {}
    }
    let Some(prev) = prev.filter(|p| same_page(&p.url, &next.url)) else {
        return format!("Page snapshot:\n{}", full());
    };
    let old: Vec<&str> = prev.text.lines().collect();
    let new: Vec<&str> = next.text.lines().collect();
    let (removed, added) = line_diff(&old, &new);
    if removed.is_empty() && added.is_empty() {
        return "No change to the page's interactive elements.".into();
    }
    // A diff no shorter than the page is just noise.
    let changed = removed.len() + added.len();
    if changed >= new.len() || changed >= new.len().max(8) * 3 / 5 {
        return format!("Page snapshot (most of it changed):\n{}", full());
    }
    let mut out = format!(
        "Changes since your last snapshot (+{} −{}, refs unchanged elsewhere):",
        added.len(),
        removed.len()
    );
    for (sign, lines) in [("+", &added), ("−", &removed)] {
        for line in lines.iter().take(DIFF_LINES) {
            out.push('\n');
            out.push_str(sign);
            out.push(' ');
            out.push_str(line.trim_start());
        }
        if lines.len() > DIFF_LINES {
            out.push_str(&format!("\n{sign} … {} more", lines.len() - DIFF_LINES));
        }
    }
    out
}

/// Same document for diffing: the URL without its fragment.
fn same_page(a: &str, b: &str) -> bool {
    let strip = |u: &str| u.split('#').next().unwrap_or(u).to_string();
    strip(a) == strip(b)
}

/// Lines only in `old`, lines only in `new` (by content, counting repeats),
/// each in its own order.
fn line_diff<'a>(old: &[&'a str], new: &[&'a str]) -> (Vec<&'a str>, Vec<&'a str>) {
    use std::collections::HashMap;
    let mut have: HashMap<&str, isize> = HashMap::new();
    for l in old {
        *have.entry(l).or_default() += 1;
    }
    let mut added = Vec::new();
    for l in new {
        let n = have.entry(l).or_default();
        if *n > 0 {
            *n -= 1;
        } else {
            added.push(*l);
        }
    }
    let mut left: HashMap<&str, isize> = HashMap::new();
    for l in new {
        *left.entry(l).or_default() += 1;
    }
    let mut removed = Vec::new();
    for l in old {
        let n = left.entry(l).or_default();
        if *n > 0 {
            *n -= 1;
        } else {
            removed.push(*l);
        }
    }
    (removed, added)
}

// ---- console ---------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct ConsoleEntry {
    /// `log`, `info`, `warning`, `error`, `debug`.
    pub level: String,
    pub text: String,
    /// Seconds since the tab's log started.
    pub at: f32,
    /// A child frame's host, empty for the page itself.
    pub frame: String,
}

/// A tab's console and page errors, newest last, at most [`CONSOLE_CAP`].
#[derive(Debug, Default)]
pub struct ConsoleLog {
    entries: VecDeque<ConsoleEntry>,
    /// Errors pushed since the agent last heard about them.
    unseen_errors: usize,
    dropped: usize,
}

pub const CONSOLE_CAP: usize = 300;

impl ConsoleLog {
    pub fn push(&mut self, mut entry: ConsoleEntry) {
        if entry.text.chars().count() > 1000 {
            entry.text = entry.text.chars().take(1000).collect::<String>() + "…";
        }
        if entry.level == "error" {
            self.unseen_errors += 1;
        }
        if self.entries.len() == CONSOLE_CAP {
            self.entries.pop_front();
            self.dropped += 1;
        }
        self.entries.push_back(entry);
    }

    /// New errors since the agent last read the console or heard about
    /// them; the count resets.
    pub fn take_unseen_errors(&mut self) -> usize {
        std::mem::take(&mut self.unseen_errors)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `browser_console`: the newest `limit` entries at `level` or worse
    /// (`all`, `log`/`info`, `warning`, `error`), oldest first.
    pub fn read(&mut self, level: &str, limit: usize, clear: bool) -> String {
        let rank = |l: &str| match l {
            "error" => 3,
            "warning" | "warn" => 2,
            "debug" => 0,
            _ => 1,
        };
        let min = match level {
            "error" | "errors" => 3,
            "warning" | "warn" | "warnings" => 2,
            "all" | "" | "debug" => 0,
            _ => 1,
        };
        self.unseen_errors = 0;
        let picked: Vec<&ConsoleEntry> = self
            .entries
            .iter()
            .filter(|e| rank(&e.level) >= min)
            .collect();
        let skip = picked.len().saturating_sub(limit.max(1));
        let mut out = String::new();
        if picked.is_empty() {
            out.push_str("No console messages");
            if min > 0 {
                out.push_str(" at that level");
            }
            out.push('.');
        } else {
            if skip > 0 {
                out.push_str(&format!("({skip} older not shown)\n"));
            }
            for e in &picked[skip..] {
                let frame = if e.frame.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", e.frame)
                };
                out.push_str(&format!(
                    "[{:>7.1}s {}{}] {}\n",
                    e.at, e.level, frame, e.text
                ));
            }
            out.pop();
        }
        if self.dropped > 0 {
            out.push_str(&format!(
                "\n({} oldest messages dropped; the buffer keeps {CONSOLE_CAP})",
                self.dropped
            ));
        }
        if clear {
            self.entries.clear();
            self.dropped = 0;
        }
        out
    }
}

// ---- dialogs ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    Alert,
    Confirm,
    Prompt,
}

impl DialogKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Alert => "alert",
            Self::Confirm => "confirm",
            Self::Prompt => "prompt",
        }
    }
}

/// A JavaScript dialog a page opened.
#[derive(Clone, Debug, PartialEq)]
pub struct DialogInfo {
    pub kind: DialogKind,
    pub message: String,
    /// A prompt's default answer.
    pub default_text: Option<String>,
    /// The host of the frame that opened it.
    pub site: String,
}

/// How long a confirm/prompt waits for `browser_handle_dialog` before it
/// gets [`DIALOG_DEFAULT_ACCEPT`]. The page is paused meanwhile.
pub const DIALOG_TIMEOUT_SECS: u64 = 30;
/// The unanswered default: dismiss (the safer answer to "Delete this?").
pub const DIALOG_DEFAULT_ACCEPT: bool = false;

impl DialogInfo {
    pub fn describe(&self) -> String {
        let site = if self.site.is_empty() {
            "the page".to_string()
        } else {
            self.site.clone()
        };
        let mut s = format!(
            "{} opened a {} dialog: {:?}",
            site,
            self.kind.name(),
            clip(&self.message, 300)
        );
        if let Some(d) = self.default_text.as_deref().filter(|d| !d.is_empty()) {
            s.push_str(&format!(" (default answer {:?})", clip(d, 100)));
        }
        s
    }

    /// The note for an action that ran into this dialog.
    pub fn ask(&self) -> String {
        format!(
            "{}. The page is paused until it is answered: call browser_handle_dialog with accept true or false{}. Unanswered, it is {} in {}s.",
            self.describe(),
            if self.kind == DialogKind::Prompt {
                " (and text for the answer)"
            } else {
                ""
            },
            if DIALOG_DEFAULT_ACCEPT {
                "accepted"
            } else {
                "dismissed"
            },
            DIALOG_TIMEOUT_SECS
        )
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        s.chars().take(n).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sites(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_empty_list_allows_everything() {
        assert!(site_allowed("https://anything.example/x", &[]));
        assert!(site_allowed("data:text/html,hi", &[]));
    }

    #[test]
    fn hosts_cover_their_subdomains_and_nothing_else() {
        let list = sites(&["example.com"]);
        assert!(site_allowed("https://example.com/", &list));
        assert!(site_allowed("http://www.example.com:8080/a", &list));
        assert!(!site_allowed("https://notexample.com/", &list));
        assert!(!site_allowed("https://example.com.evil.io/", &list));
        assert!(!site_allowed("https://evil.io/?u=example.com", &list));
        assert!(!site_allowed("https://user@evil.io/", &list));
        let wild = sites(&["*.example.com"]);
        assert!(site_allowed("https://a.example.com", &wild));
        assert!(!site_allowed("https://example.com", &wild));
    }

    #[test]
    fn ports_schemes_and_paths_narrow_an_entry() {
        let list = sites(&["localhost:3000", "https://docs.rs/serde"]);
        assert!(site_allowed("http://localhost:3000/app", &list));
        assert!(!site_allowed("http://localhost:5173/", &list));
        assert!(site_allowed("https://docs.rs/serde/latest", &list));
        assert!(!site_allowed("http://docs.rs/serde", &list));
        assert!(!site_allowed("https://docs.rs/tokio", &list));
        let https = sites(&["https://example.com"]);
        assert!(site_allowed("https://example.com/x", &https));
        assert!(!site_allowed("https://example.com:8443/x", &https));
    }

    #[test]
    fn special_schemes_with_a_list() {
        let list = sites(&["example.com"]);
        assert!(site_allowed("about:blank", &list));
        assert!(!site_allowed("data:text/html,<b>x</b>", &list));
        assert!(site_allowed("blob:https://example.com/1234", &list));
        assert!(!site_allowed("blob:https://evil.io/1234", &list));
        assert!(!site_allowed("file:///etc/passwd", &list));
        assert!(!site_allowed("not a url", &list));
    }

    #[test]
    fn the_settings_field_parses_into_sites() {
        assert_eq!(
            parse_sites(" Example.com, localhost:3000\n*.dev.io/  example.com "),
            sites(&["example.com", "localhost:3000", "*.dev.io"])
        );
        assert!(parse_sites("  ").is_empty());
    }

    #[test]
    fn the_queue_runs_one_at_a_time_in_order() {
        let mut q = Fifo::default();
        assert_eq!(q.push(1), Some(1), "idle: starts now");
        assert_eq!(q.push(2), None, "busy: waits");
        assert_eq!(q.push(3), None);
        assert!(q.busy());
        assert_eq!(q.done(), Some(2));
        assert_eq!(q.push(4), None, "still busy with 2");
        assert_eq!(q.done(), Some(3));
        assert_eq!(q.done(), Some(4));
        assert_eq!(q.done(), None);
        assert!(!q.busy());
        assert_eq!(q.push(5), Some(5), "idle again");
    }

    #[test]
    fn a_cancel_removes_only_what_it_picks() {
        let mut q = Fifo::default();
        q.push(("a", 1));
        q.push(("b", 2));
        q.push(("a", 3));
        q.push(("b", 4));
        let gone = q.cancel(|(chat, _)| *chat == "b");
        assert_eq!(gone, vec![("b", 2), ("b", 4)]);
        assert_eq!(q.done(), Some(("a", 3)));
        assert_eq!(q.done(), None);
        q.push(("a", 5));
        q.push(("a", 6));
        assert_eq!(q.clear(), vec![("a", 6)]);
        assert!(!q.busy());
    }

    fn snap(url: &str, text: &str) -> Snap {
        Snap {
            url: url.into(),
            text: text.into(),
        }
    }

    #[test]
    fn a_reply_carries_the_change_since_the_last_snapshot() {
        let before = snap(
            "https://a.io/#x",
            "- main:\n  - textbox \"Q\" [ref=e1]\n  - button \"Go\" [ref=e2]\n  - link \"A\" [ref=e3]\n  - link \"B\" [ref=e4]\n  - link \"C\" [ref=e5]\n  - link \"D\" [ref=e6]\n  - link \"E\" [ref=e7]\n  - link \"F\" [ref=e8]\n  - link \"G\" [ref=e9]",
        );
        let after = snap(
            "https://a.io/#y",
            &before.text.replace("[ref=e1]", "[ref=e1]: hi").replace(
                "  - link \"G\" [ref=e9]",
                "  - link \"G\" [ref=e9]\n  - alert \"Saved\"",
            ),
        );
        let out = snapshot_reply(Some(&before), &after, SnapWant::Diff);
        assert!(
            out.starts_with("Changes since your last snapshot (+2 −1"),
            "{out}"
        );
        assert!(
            out.contains("+ - textbox \"Q\" [ref=e1]: hi")
                && out.contains("+ - alert \"Saved\"")
                && out.contains("− - textbox \"Q\" [ref=e1]"),
            "{out}"
        );
        assert_eq!(
            snapshot_reply(Some(&before), &before, SnapWant::Diff),
            "No change to the page's interactive elements."
        );
    }

    #[test]
    fn a_new_page_or_a_big_change_sends_the_whole_snapshot() {
        let a = snap("https://a.io/", "- button \"Go\" [ref=e1]");
        let b = snap("https://a.io/next", "- link \"Back\" [ref=e2]");
        assert_eq!(
            snapshot_reply(Some(&a), &b, SnapWant::Diff),
            "Page snapshot:\n- link \"Back\" [ref=e2]"
        );
        assert_eq!(
            snapshot_reply(None, &a, SnapWant::Diff),
            "Page snapshot:\n- button \"Go\" [ref=e1]"
        );
        let c = snap("https://a.io/", "- link \"X\" [ref=e3]");
        assert!(
            snapshot_reply(Some(&a), &c, SnapWant::Diff)
                .starts_with("Page snapshot (most of it changed):")
        );
        assert_eq!(snapshot_reply(Some(&a), &c, SnapWant::Full), c.text);
        assert_eq!(snapshot_reply(Some(&a), &c, SnapWant::None), "");
    }

    #[test]
    fn the_console_keeps_the_newest_and_filters_by_level() {
        let mut log = ConsoleLog::default();
        for i in 0..(CONSOLE_CAP + 5) {
            log.push(ConsoleEntry {
                level: if i % 3 == 0 {
                    "error".into()
                } else {
                    "log".into()
                },
                text: format!("m{i}"),
                at: i as f32,
                frame: String::new(),
            });
        }
        assert_eq!(log.len(), CONSOLE_CAP);
        assert!(log.take_unseen_errors() > 0);
        assert_eq!(log.take_unseen_errors(), 0);
        let errors = log.read("error", 2, false);
        assert!(
            errors.contains("older not shown")
                && errors.contains("m303")
                && !errors.contains("] m304"),
            "{errors}"
        );
        assert!(errors.contains("5 oldest messages dropped"), "{errors}");
        let all = log.read("all", 1000, true);
        assert!(all.contains("m304") && !all.contains("] m4\n"), "{all}");
        assert!(log.is_empty());
        assert_eq!(log.read("all", 10, false), "No console messages.");
    }

    #[test]
    fn dialogs_tell_the_agent_how_to_answer() {
        let d = DialogInfo {
            kind: DialogKind::Confirm,
            message: "Delete it?".into(),
            default_text: None,
            site: "a.io".into(),
        };
        let ask = d.ask();
        assert!(
            ask.starts_with("a.io opened a confirm dialog: \"Delete it?\""),
            "{ask}"
        );
        assert!(
            ask.contains("browser_handle_dialog") && ask.contains("dismissed in 30s"),
            "{ask}"
        );
        let p = DialogInfo {
            kind: DialogKind::Prompt,
            message: "Name?".into(),
            default_text: Some("Bob".into()),
            site: String::new(),
        };
        assert!(
            p.ask().contains("default answer \"Bob\"") && p.ask().contains("text for the answer")
        );
    }

    /// Token-size measurement: `KEEL_MEASURE_DIR=/tmp/keel-snap-measure
    /// cargo test -p keel-ui --lib measure_action_diffs -- --ignored`
    /// writes each action's diff reply beside its two snapshots.
    #[test]
    #[ignore]
    fn measure_action_diffs() {
        let Ok(dir) = std::env::var("KEEL_MEASURE_DIR") else {
            return;
        };
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".act0.txt") else {
                continue;
            };
            let read =
                |suffix: &str| std::fs::read_to_string(format!("{dir}/{stem}{suffix}")).unwrap();
            let prev = Snap {
                url: "https://x.test/a".into(),
                text: read(".act0.txt"),
            };
            let next = Snap {
                url: "https://x.test/a".into(),
                text: read(".act1.txt"),
            };
            let reply = format!(
                "{}\n{}",
                read(".act-outcome.txt"),
                snapshot_reply(Some(&prev), &next, SnapWant::Diff)
            );
            std::fs::write(format!("{dir}/{stem}.act-reply.txt"), &reply).unwrap();
            println!(
                "{stem}: diff reply {} bytes (full snapshot {})",
                reply.len(),
                next.text.len()
            );
        }
    }
}

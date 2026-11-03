//! Trace steps for a turn: kinds, verb tenses, and how reads fold.
//!
//! Pure data. Nothing here paints. Phase 2 reads these types.
//! Times are unix millis. An [`std::time::Instant`] does not survive a reopen.

use crate::browser_trace::{self, BrowserBody, BrowserOp};
use crate::markdown::{ToolDiffView, ToolEntry};

/// Pending approval, in flight, finished, failed, or stopped with the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Running,
    Done,
    Error,
    Cancelled,
}

/// What the line is. Browser tools stay out of an Explored fold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    Browser,
    Subagent,
    Plan,
    Sources,
    Other,
}

/// The five title forms. Search and switch-mode keep "for" / "to" on the
/// verb; an empty subject drops that dangling word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verb {
    pub imperative: &'static str,
    pub present: &'static str,
    pub past: &'static str,
    /// Infinitive in "Failed to {failed} {subject}".
    pub failed: &'static str,
    /// Participle in "Stopped {stopped} {subject}".
    pub stopped: &'static str,
}

const fn verb(
    imperative: &'static str,
    present: &'static str,
    past: &'static str,
    failed: &'static str,
    stopped: &'static str,
) -> Verb {
    Verb {
        imperative,
        present,
        past,
        failed,
        stopped,
    }
}

/// Forms for `kind`. Browser lines use the tool name (`browser_click`, …).
pub fn verb_for(kind: StepKind, name: &str) -> Verb {
    match kind {
        StepKind::Read => verb("Read", "Reading", "Read", "read", "reading"),
        StepKind::Edit => verb("Edit", "Editing", "Edited", "edit", "editing"),
        StepKind::Delete => verb("Delete", "Deleting", "Deleted", "delete", "deleting"),
        StepKind::Move => verb("Move", "Moving", "Moved", "move", "moving"),
        StepKind::Search => verb(
            "Search for",
            "Searching for",
            "Searched for",
            "search for",
            "searching for",
        ),
        StepKind::Execute => verb("Run", "Running", "Ran", "run", "running"),
        StepKind::Think => verb("Think", "Thinking", "Thought", "think", "thinking"),
        StepKind::Fetch => verb("Fetch", "Fetching", "Fetched", "fetch", "fetching"),
        StepKind::SwitchMode => verb(
            "Switch to",
            "Switching to",
            "Switched to",
            "switch to",
            "switching to",
        ),
        StepKind::Browser => browser_verb(name),
        StepKind::Subagent => verb("Run", "Running", "Ran", "run", "running"),
        StepKind::Plan => verb("Plan", "Planning", "Planned", "plan", "planning"),
        StepKind::Sources => verb(
            "Check sources",
            "Checking sources",
            "Checked sources",
            "check sources",
            "checking sources",
        ),
        StepKind::Other => verb("Use", "Using", "Used", "use", "using"),
    }
}

fn browser_verb(name: &str) -> Verb {
    let op =
        browser_trace::tool_base(name).map_or(BrowserOp::Other, |base| browser_trace::op_of(&base));
    browser_trace::verb(op, &browser_trace::Args::default())
}

/// One plan row. `priority` is absent when the harness didn't send one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    pub content: String,
    pub status: String,
    pub priority: Option<String>,
}

/// A link a fetch or sources line can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub title: String,
    pub url: String,
}

/// What sits under the line when it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepBody {
    None,
    Text(String),
    Output {
        tail: String,
        error: bool,
    },
    Diff {
        path: String,
        old: Option<String>,
        new: String,
    },
    Terminal {
        id: String,
    },
    Plan(Vec<PlanItem>),
    Links(Vec<Link>),
    Shots(Vec<String>),
    /// A browser action: URL, title and result, with the raw output behind a toggle.
    Browser(BrowserBody),
}

/// One line. `started_ms` and `ended_ms` are unix millis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceStep {
    pub id: String,
    pub kind: StepKind,
    pub status: StepStatus,
    pub verb: Verb,
    pub subject: String,
    pub description: Option<String>,
    pub started_ms: Option<i64>,
    pub ended_ms: Option<i64>,
    pub body: StepBody,
    pub children: Vec<TraceStep>,
    /// Pending approval. Never folded into Explored.
    pub needs_user: bool,
    /// Which browser action this is, for its icon.
    pub browser: Option<BrowserOp>,
}

impl TraceStep {
    /// A plan line. "Planning" while any entry is in progress, otherwise
    /// "Plan · 3 of 5 done".
    pub fn plan(id: impl Into<String>, entries: Vec<PlanItem>, status: StepStatus) -> Self {
        Self {
            id: id.into(),
            kind: StepKind::Plan,
            status,
            verb: verb_for(StepKind::Plan, "plan"),
            subject: String::new(),
            description: None,
            started_ms: None,
            ended_ms: None,
            body: StepBody::Plan(entries),
            children: Vec::new(),
            needs_user: false,
            browser: None,
        }
    }

    /// The line's title for its status.
    pub fn title(&self) -> String {
        if self.kind == StepKind::Plan {
            let entries = match &self.body {
                StepBody::Plan(entries) => entries.as_slice(),
                _ => &[],
            };
            return plan_title(entries, self.status);
        }
        if self.kind == StepKind::Other {
            let name = if self.subject.is_empty() {
                "tool"
            } else {
                self.subject.as_str()
            };
            return match self.status {
                StepStatus::Pending | StepStatus::Done => name.to_string(),
                StepStatus::Running => format!("Running {name}"),
                StepStatus::Error => format!("Failed to run {name}"),
                StepStatus::Cancelled => format!("Stopped running {name}"),
            };
        }
        // A browser action that failed: "Couldn't open traveloka.com".
        if self.kind == StepKind::Browser && self.status == StepStatus::Error {
            return format!("Couldn't {}", join_subject(self.verb.failed, &self.subject));
        }
        let (head, prefix) = match self.status {
            StepStatus::Pending => (self.verb.imperative, ""),
            StepStatus::Running => (self.verb.present, ""),
            StepStatus::Done => (self.verb.past, ""),
            StepStatus::Error => (self.verb.failed, "Failed to "),
            StepStatus::Cancelled => (self.verb.stopped, "Stopped "),
        };
        format!("{prefix}{}", join_subject(head, &self.subject))
    }
}

fn plan_title(entries: &[PlanItem], status: StepStatus) -> String {
    let live = entries.iter().any(|entry| {
        matches!(
            entry.status.as_str(),
            "in_progress" | "inProgress" | "running"
        )
    });
    if live || (entries.is_empty() && status == StepStatus::Running) {
        return "Planning".to_string();
    }
    if entries.is_empty() {
        return "Plan".to_string();
    }
    let done = entries
        .iter()
        .filter(|entry| matches!(entry.status.as_str(), "completed" | "done" | "complete"))
        .count();
    format!("Plan · {done} of {} done", entries.len())
}

/// "Searching for" with no subject is "Searching".
fn join_subject(head: &str, subject: &str) -> String {
    let head = if subject.is_empty() {
        head.strip_suffix(" for")
            .or_else(|| head.strip_suffix(" to"))
            .unwrap_or(head)
    } else {
        head
    };
    if subject.is_empty() {
        head.to_string()
    } else if head.is_empty() {
        subject.to_string()
    } else {
        format!("{head} {subject}")
    }
}

impl From<&ToolEntry> for TraceStep {
    fn from(tool: &ToolEntry) -> Self {
        if let Some(step) = browser_step(tool) {
            return step;
        }
        if let Some(step) = web_step(tool) {
            return step;
        }
        let status = status_of(&tool.state);
        let kind = step_kind(&tool.name, &tool.kind);
        let subject = subject_for(kind, &tool.detail, &tool.name);
        let mut step = Self {
            id: tool.tool_id.clone(),
            kind,
            status,
            verb: verb_for(kind, &tool.name),
            subject,
            description: None,
            started_ms: tool.started_ms,
            ended_ms: tool.ended_ms,
            body: body_of(tool, status),
            children: Vec::new(),
            needs_user: matches!(status, StepStatus::Pending),
            browser: None,
        };
        step.description = describe(&step);
        step
    }
}

/// A browser tool's line, under any harness's naming: verb and subject from
/// its arguments and result, a readable body, and its own icon.
fn browser_step(tool: &ToolEntry) -> Option<TraceStep> {
    let output = if tool.output_tail.is_empty() {
        &tool.summary
    } else {
        &tool.output_tail
    };
    let base = browser_trace::tool_base(&tool.name)?;
    let op = browser_trace::op_of(&base);
    let status = status_of(&tool.state);
    let failed = status == StepStatus::Error;
    let args = browser_trace::args_of(&tool.detail);
    let outcome = browser_trace::outcome_of(output, failed);
    let body = if !tool.images.is_empty() {
        StepBody::Shots(tool.images.clone())
    } else {
        let body = browser_trace::body(op, &args, &outcome, output, failed);
        if body.rows.is_empty() && body.raw.trim().is_empty() {
            StepBody::None
        } else {
            StepBody::Browser(body)
        }
    };
    let mut step = TraceStep {
        id: tool.tool_id.clone(),
        kind: StepKind::Browser,
        status,
        verb: browser_trace::verb(op, &args),
        subject: browser_trace::subject(op, &args, &outcome),
        description: None,
        started_ms: tool.started_ms,
        ended_ms: tool.ended_ms,
        body,
        children: Vec::new(),
        needs_user: matches!(status, StepStatus::Pending),
        browser: Some(op),
    };
    step.description = describe(&step);
    Some(step)
}

/// A harness's built-in web tool (Grok's `web_search`, `x_search`,
/// `web_fetch`; Claude's `WebSearch`, `WebFetch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebTool {
    Search,
    XSearch,
    Fetch,
}

fn web_tool(name: &str) -> Option<WebTool> {
    // Grok titles its calls "Web search:"; MCP-style names carry a prefix.
    let n = name
        .trim()
        .trim_end_matches(':')
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-'], "_");
    let n = n.rsplit("__").next().unwrap_or(&n);
    match n {
        "web_search" | "websearch" | "search_web" | "google_web_search" | "web_search_preview" => {
            Some(WebTool::Search)
        }
        "x_search" | "xsearch" | "search_x" | "x_keyword_search" | "x_semantic_search" => {
            Some(WebTool::XSearch)
        }
        "web_fetch" | "webfetch" | "fetch_url" | "fetch_content" | "open_page" | "browse_page"
        | "open_url" | "read_url" => Some(WebTool::Fetch),
        _ => None,
    }
}

fn quoted(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = if text.chars().count() > 60 {
        text.chars()
            .take(59)
            .collect::<String>()
            .trim_end()
            .to_string()
            + "\u{2026}"
    } else {
        text
    };
    format!("\u{201c}{short}\u{201d}")
}

/// Grok reports a running search's progress as its title text:
/// `Searching "query" (0/4 completed)`. The query is what's quoted (else
/// the text minus the verb and the counter); the counter is kept apart.
fn web_progress(text: &str) -> (String, Option<String>) {
    let mut rest = text.trim();
    let mut progress = None;
    if rest.ends_with(')') {
        if let Some(open) = rest.rfind('(') {
            let inner = rest[open + 1..rest.len() - 1].trim();
            let count = inner.split_whitespace().next().unwrap_or_default();
            let is_count = count.split_once('/').is_some_and(|(a, b)| {
                !a.is_empty()
                    && !b.is_empty()
                    && a.chars().all(|c| c.is_ascii_digit())
                    && b.chars().all(|c| c.is_ascii_digit())
            });
            if is_count {
                progress = Some(count.to_string());
                rest = rest[..open].trim_end();
            }
        }
    }
    let lower = rest.to_ascii_lowercase();
    for lead in [
        "searching the web for",
        "searching x for",
        "searched x for",
        "searching for",
        "searched for",
        "searching",
        "searched",
        "reading",
        "fetching",
    ] {
        if lower.starts_with(lead) && rest[lead.len()..].starts_with([' ', ':', '"', '\u{201c}']) {
            rest = rest[lead.len()..].trim_start_matches([' ', ':']).trim();
            break;
        }
    }
    let quotes = [('"', '"'), ('\u{201c}', '\u{201d}')];
    for (open, close) in quotes {
        if let Some(body) = rest.strip_prefix(open) {
            if let Some(end) = body.rfind(close) {
                return (body[..end].trim().to_string(), progress);
            }
            return (body.trim_end_matches(close).trim().to_string(), progress);
        }
    }
    (rest.to_string(), progress)
}

fn is_url(text: &str) -> bool {
    text.starts_with("https://") || text.starts_with("http://")
}

/// `Searching “query”` / `Searched “query”` with the search icon, and
/// `Reading host` for a page. The query or page comes from the call's
/// arguments, else the result's first line (Grok sends it only at the end).
/// A Grok search that opened a page is a read.
fn web_step(tool: &ToolEntry) -> Option<TraceStep> {
    let mut which = web_tool(&tool.name)?;
    let status = status_of(&tool.state);
    let output = if tool.output_tail.is_empty() {
        tool.summary.as_str()
    } else {
        tool.output_tail.as_str()
    };
    let detail = tool.detail.trim();
    let usable =
        |text: &str| !text.is_empty() && !text.starts_with(['{', '[']) && web_tool(text).is_none();
    let first = output.lines().next().unwrap_or_default().trim();
    let raw = if usable(detail) {
        detail
    } else if usable(first) {
        first
    } else {
        ""
    };
    let (cleaned, progress) = web_progress(raw);
    let target = cleaned.as_str();
    if is_url(target) {
        which = WebTool::Fetch;
    }
    let (kind, verb, subject) = match which {
        WebTool::Search => (
            StepKind::Search,
            verb("Search", "Searching", "Searched", "search", "searching"),
            if target.is_empty() {
                "the web".to_string()
            } else {
                quoted(target)
            },
        ),
        WebTool::XSearch => (
            StepKind::Search,
            verb(
                "Search X for",
                "Searching X for",
                "Searched X for",
                "search X for",
                "searching X for",
            ),
            if target.is_empty() {
                String::new()
            } else {
                quoted(target)
            },
        ),
        WebTool::Fetch => (
            StepKind::Fetch,
            verb("Read", "Reading", "Read", "read", "reading"),
            if target.is_empty() {
                "the page".to_string()
            } else {
                fetch_host(target)
            },
        ),
    };
    let mut urls: Vec<&str> = output
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| is_url(line))
        .collect();
    if which == WebTool::Fetch && is_url(target) && urls.is_empty() {
        urls.push(target);
    }
    let body = if !urls.is_empty() {
        StepBody::Links(
            urls.into_iter()
                .map(|url| Link {
                    title: fetch_host(url),
                    url: url.to_string(),
                })
                .collect(),
        )
    } else if output.trim().is_empty() || output.trim() == target || output.trim() == raw {
        StepBody::None
    } else {
        body_of(tool, status)
    };
    let mut step = TraceStep {
        id: tool.tool_id.clone(),
        kind,
        status,
        verb,
        subject,
        description: None,
        started_ms: tool.started_ms,
        ended_ms: tool.ended_ms,
        body,
        children: Vec::new(),
        needs_user: matches!(status, StepStatus::Pending),
        browser: None,
    };
    step.description = describe(&step);
    // A running search's counter, muted beside the query.
    if status == StepStatus::Running && step.description.is_none() {
        step.description = progress;
    }
    Some(step)
}

fn status_of(state: &str) -> StepStatus {
    match state {
        "running" | "in_progress" => StepStatus::Running,
        "pending" | "awaiting" | "needs_user" => StepStatus::Pending,
        "error" | "failed" => StepStatus::Error,
        "cancelled" | "canceled" | "stopped" => StepStatus::Cancelled,
        _ => StepStatus::Done,
    }
}

/// ACP kind, or the name table when the stored kind is missing or `other`.
/// A keel-browser tool is always [`StepKind::Browser`].
pub fn step_kind(name: &str, stored: &str) -> StepKind {
    if is_browser(name) {
        return StepKind::Browser;
    }
    known_kind(stored)
        .or_else(|| kind_from_name(name).and_then(known_kind))
        .unwrap_or(StepKind::Other)
}

fn known_kind(raw: &str) -> Option<StepKind> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "read" => Some(StepKind::Read),
        "edit" => Some(StepKind::Edit),
        "delete" => Some(StepKind::Delete),
        "move" => Some(StepKind::Move),
        "search" => Some(StepKind::Search),
        "execute" => Some(StepKind::Execute),
        "think" => Some(StepKind::Think),
        "fetch" => Some(StepKind::Fetch),
        "switch_mode" | "switchmode" => Some(StepKind::SwitchMode),
        "browser" => Some(StepKind::Browser),
        "subagent" => Some(StepKind::Subagent),
        "plan" => Some(StepKind::Plan),
        "sources" => Some(StepKind::Sources),
        "list" | "ls" | "dir" => Some(StepKind::Search),
        _ => None,
    }
}

fn is_browser(name: &str) -> bool {
    browser_trace::tool_base(name).is_some()
}

/// Same fallback as `keel_harness::kind_from_name`. The UI crate does not
/// depend on the harness, so old sessions with an empty kind infer it here.
fn kind_from_name(name: &str) -> Option<&'static str> {
    let base = name.rsplit("__").next().unwrap_or(name);
    let base = base.rsplit(['/', ':']).next().unwrap_or(base);
    let n = base.to_ascii_lowercase().replace(['-', ' '], "_");
    if is_plan_name(&n) {
        return Some("plan");
    }
    if n == "bash" || n == "shell" || n.starts_with("exec") {
        return Some("execute");
    }
    if n.starts_with("read") || n.starts_with("view") || n == "cat" {
        return Some("read");
    }
    if n == "grep"
        || n.starts_with("grep_")
        || n == "glob"
        || n.starts_with("glob_")
        || n.starts_with("search")
        || n == "find"
        || n.starts_with("find_")
        || n == "ls"
        || n == "list"
        || n == "list_dir"
        || n.starts_with("list_")
    {
        return Some("search");
    }
    if n == "edit"
        || n.starts_with("edit_")
        || n == "write"
        || n.starts_with("write_")
        || n == "apply_patch"
        || n.starts_with("apply_patch")
        || n.starts_with("str_replace")
    {
        return Some("edit");
    }
    if n.starts_with("web") || n == "fetch" || n.starts_with("fetch_") {
        return Some("fetch");
    }
    None
}

fn is_plan_name(n: &str) -> bool {
    matches!(
        n,
        "todowrite"
            | "todo_write"
            | "enterplanmode"
            | "enter_plan_mode"
            | "exitplanmode"
            | "exit_plan_mode"
            | "plan"
    ) || n.starts_with("todo_write")
        || n.starts_with("todowrite")
}

fn subject_for(kind: StepKind, detail: &str, name: &str) -> String {
    let detail = detail.trim();
    match kind {
        StepKind::Fetch => fetch_host(if detail.is_empty() { name } else { detail }),
        StepKind::Subagent => agent_subject(if detail.is_empty() { name } else { detail }),
        StepKind::Other if detail.is_empty() => humanise(name),
        _ => detail.to_string(),
    }
}

fn fetch_host(subject: &str) -> String {
    let Some(rest) = subject
        .strip_prefix("https://")
        .or_else(|| subject.strip_prefix("http://"))
    else {
        return subject.to_string();
    };
    let auth = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let hostport = auth.rsplit('@').next().unwrap_or(auth);
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        hostport.split(':').next().unwrap_or(hostport)
    };
    if host.is_empty() {
        subject.to_string()
    } else {
        host.to_string()
    }
}

fn agent_subject(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() || name == "agent" || name.ends_with(" agent") {
        if name.is_empty() {
            "agent".to_string()
        } else {
            name.to_string()
        }
    } else {
        format!("{name} agent")
    }
}

fn humanise(name: &str) -> String {
    let (base, _) = keel_proto::ToolAsk::split_name(name);
    let base = base.rsplit(['/', ':']).next().unwrap_or(&base).to_string();
    let words = base.replace(['_', '-'], " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Tool".to_string(),
    }
}

fn body_of(tool: &ToolEntry, status: StepStatus) -> StepBody {
    if let Some(ToolDiffView {
        path,
        old_text,
        new_text,
    }) = &tool.diff
    {
        return StepBody::Diff {
            path: path.clone(),
            old: old_text.clone(),
            new: new_text.clone(),
        };
    }
    if !tool.images.is_empty() {
        return StepBody::Shots(tool.images.clone());
    }
    let tail = if !tool.output_tail.is_empty() {
        tool.output_tail.clone()
    } else {
        tool.summary.clone()
    };
    if tail.is_empty() {
        StepBody::None
    } else {
        StepBody::Output {
            tail,
            error: status == StepStatus::Error,
        }
    }
}

fn describe(step: &TraceStep) -> Option<String> {
    if let StepBody::Diff { old, new, .. } = &step.body {
        let stat = diffstat(old.as_deref(), new);
        if stat != format!("+0 \u{2212}0") {
            return Some(stat);
        }
    }
    let (Some(start), Some(end)) = (step.started_ms, step.ended_ms) else {
        return None;
    };
    if matches!(step.status, StepStatus::Pending | StepStatus::Running) {
        return None;
    }
    Some(format_duration(end.saturating_sub(start).max(0) as u64))
}

/// `+added −removed`. A unified diff (no old body) counts `+`/`-` lines
/// and skips `+++`, `---` and `@@`. Two full bodies share a prefix and suffix.
pub fn diffstat(old: Option<&str>, new: &str) -> String {
    let (add, del) = if old.is_none() && is_unified(new) {
        unified_counts(new)
    } else if let Some(old) = old {
        line_delta(old, new)
    } else {
        (new.lines().count(), 0)
    };
    format!("+{add} \u{2212}{del}")
}

fn is_unified(text: &str) -> bool {
    let mut plus_file = false;
    let mut minus_file = false;
    let mut hunk = false;
    for line in text.lines() {
        if line.starts_with("diff --git") || line.starts_with("@@") {
            hunk = true;
        }
        if line.starts_with("+++ ") {
            plus_file = true;
        }
        if line.starts_with("--- ") {
            minus_file = true;
        }
    }
    hunk || (plus_file && minus_file)
}

fn unified_counts(text: &str) -> (usize, usize) {
    let mut add = 0;
    let mut del = 0;
    for line in text.lines() {
        if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
            continue;
        }
        if line.starts_with('+') {
            add += 1;
        } else if line.starts_with('-') {
            del += 1;
        }
    }
    (add, del)
}

fn line_delta(old: &str, new: &str) -> (usize, usize) {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix + prefix < old.len()
        && suffix + prefix < new.len()
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }
    (new.len() - prefix - suffix, old.len() - prefix - suffix)
}

/// Under a second is "{ms}ms". Otherwise hours, minutes and seconds, with
/// zero components left out. 72_000 ms is "1m 12s".
pub fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let total = ms / 1_000;
    let hours = total / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;
    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 {
        parts.push(format!("{seconds}s"));
    }
    if parts.is_empty() {
        "0s".to_string()
    } else {
        parts.join(" ")
    }
}

/// "Worked for 1m 12s · 1 failed". No failure suffix when nothing failed.
pub fn worked_label(ms: u64, failed: u64) -> String {
    let duration = format_duration(ms);
    if failed == 0 {
        format!("Worked for {duration}")
    } else {
        format!("Worked for {duration} · {failed} failed")
    }
}

/// None while any step (or a nested child) is still pending or running.
pub fn turn_summary(steps: &[TraceStep], worked_ms: u64) -> Option<String> {
    if any_live(steps) {
        return None;
    }
    Some(worked_label(worked_ms, count_failed(steps)))
}

fn any_live(steps: &[TraceStep]) -> bool {
    steps.iter().any(|step| {
        matches!(step.status, StepStatus::Pending | StepStatus::Running) || any_live(&step.children)
    })
}

fn count_failed(steps: &[TraceStep]) -> u64 {
    steps
        .iter()
        .map(|step| u64::from(step.status == StepStatus::Error) + count_failed(&step.children))
        .sum()
}

/// A folded run of reads, searches and fetches, or one top-level step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceNode {
    Step(TraceStep),
    Explored(TraceGroup),
    /// Three or more browser lines in a row: "Browsed traveloka.com · 5 actions".
    Browsed(TraceGroup),
}

/// Three or more consecutive read / search / fetch lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceGroup {
    pub title: String,
    pub status: StepStatus,
    pub steps: Vec<TraceStep>,
}

/// Edits and executes stay top-level and break a run. A `needs_user` step
/// is never folded. An error stays inside and lifts the parent, unless a
/// child is still running.
pub fn group_steps(steps: Vec<TraceStep>) -> Vec<TraceNode> {
    let mut out = Vec::new();
    let mut run = Vec::new();
    let mut browsing = Vec::new();
    for step in steps {
        if browsable(&step) {
            flush_run(&mut run, &mut out);
            browsing.push(step);
        } else if foldable(&step) {
            flush_browsing(&mut browsing, &mut out);
            run.push(step);
        } else {
            flush_run(&mut run, &mut out);
            flush_browsing(&mut browsing, &mut out);
            out.push(TraceNode::Step(step));
        }
    }
    flush_run(&mut run, &mut out);
    flush_browsing(&mut browsing, &mut out);
    out
}

fn browsable(step: &TraceStep) -> bool {
    !step.needs_user && step.kind == StepKind::Browser
}

fn flush_browsing(run: &mut Vec<TraceStep>, out: &mut Vec<TraceNode>) {
    let taken = std::mem::take(run);
    if taken.len() >= 3 {
        out.push(TraceNode::Browsed(browsed_group(taken)));
    } else {
        out.extend(taken.into_iter().map(TraceNode::Step));
    }
}

/// Calls a line stands for: a folded repeat says "×3".
fn weight(step: &TraceStep) -> usize {
    step.description
        .as_deref()
        .and_then(|d| d.strip_prefix('\u{d7}'))
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
}

fn browsed_group(steps: Vec<TraceStep>) -> TraceGroup {
    let actions: usize = steps.iter().map(weight).sum();
    let failed = steps
        .iter()
        .filter(|s| s.status == StepStatus::Error)
        .count();
    let running = steps.iter().rev().find(|s| s.status == StepStatus::Running);
    let host = steps.iter().rev().find_map(|s| match &s.body {
        StepBody::Browser(b) => b.host.clone(),
        _ => None,
    });
    let status = if running.is_some() {
        StepStatus::Running
    } else if failed > 0 {
        StepStatus::Error
    } else if steps.iter().all(|s| s.status == StepStatus::Cancelled) {
        StepStatus::Cancelled
    } else {
        StepStatus::Done
    };
    let site = host.map(|h| format!(" {h}")).unwrap_or_default();
    let count = format!("{actions} action{}", if actions == 1 { "" } else { "s" });
    let title = if let Some(live) = running {
        format!("Browsing{site} \u{b7} {}", live.title())
    } else if status == StepStatus::Cancelled {
        format!("Stopped browsing{site} \u{b7} {count}")
    } else if failed > 0 {
        format!("Browsed{site} \u{b7} {count}, {failed} failed")
    } else {
        format!("Browsed{site} \u{b7} {count}")
    };
    TraceGroup {
        title,
        status,
        steps,
    }
}

fn foldable(step: &TraceStep) -> bool {
    !step.needs_user
        && matches!(
            step.kind,
            StepKind::Read | StepKind::Search | StepKind::Fetch
        )
}

fn flush_run(run: &mut Vec<TraceStep>, out: &mut Vec<TraceNode>) {
    let taken = std::mem::take(run);
    if taken.len() >= 3 {
        out.push(TraceNode::Explored(explored_group(taken)));
    } else {
        out.extend(taken.into_iter().map(TraceNode::Step));
    }
}

fn explored_group(steps: Vec<TraceStep>) -> TraceGroup {
    let files = steps
        .iter()
        .filter(|step| step.kind == StepKind::Read)
        .count();
    let searches = steps
        .iter()
        .filter(|step| step.kind == StepKind::Search)
        .count();
    let fetches = steps
        .iter()
        .filter(|step| step.kind == StepKind::Fetch)
        .count();
    let failed = steps
        .iter()
        .filter(|step| step.status == StepStatus::Error)
        .count();
    let running = steps
        .iter()
        .rev()
        .find(|step| step.status == StepStatus::Running);
    let status = if running.is_some() {
        StepStatus::Running
    } else if failed > 0 {
        StepStatus::Error
    } else if steps
        .iter()
        .all(|step| step.status == StepStatus::Cancelled)
    {
        StepStatus::Cancelled
    } else {
        StepStatus::Done
    };
    let counts = counts_label(files, searches, fetches);
    let title = if let Some(live) = running {
        format!("Exploring · {}", live.title())
    } else if failed > 0 {
        format!("Explored {counts} · {failed} failed")
    } else if status == StepStatus::Cancelled {
        format!("Stopped exploring {counts}")
    } else {
        format!("Explored {counts}")
    };
    TraceGroup {
        title,
        status,
        steps,
    }
}

fn counts_label(files: usize, searches: usize, fetches: usize) -> String {
    let mut parts = Vec::new();
    if files > 0 {
        parts.push(format!(
            "{files} {}",
            if files == 1 { "file" } else { "files" }
        ));
    }
    if searches > 0 {
        parts.push(format!(
            "{searches} {}",
            if searches == 1 { "search" } else { "searches" }
        ));
    }
    if fetches > 0 {
        parts.push(format!(
            "{fetches} {}",
            if fetches == 1 { "fetch" } else { "fetches" }
        ));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(kind: StepKind, status: StepStatus, subject: &str) -> TraceStep {
        TraceStep {
            id: format!("{kind:?}-{subject}"),
            kind,
            status,
            verb: verb_for(kind, subject),
            subject: subject.to_string(),
            description: None,
            started_ms: None,
            ended_ms: None,
            body: StepBody::None,
            children: Vec::new(),
            needs_user: false,
            browser: None,
        }
    }

    fn read(subject: &str, status: StepStatus) -> TraceStep {
        step(StepKind::Read, status, subject)
    }

    #[test]
    fn titles_follow_the_verb_forms() {
        assert_eq!(
            step(StepKind::Execute, StepStatus::Pending, "pnpm test").title(),
            "Run pnpm test"
        );
        assert_eq!(
            step(StepKind::Execute, StepStatus::Running, "pnpm build").title(),
            "Running pnpm build"
        );
        assert_eq!(
            step(StepKind::Execute, StepStatus::Done, "pnpm install").title(),
            "Ran pnpm install"
        );
        assert_eq!(
            step(StepKind::Execute, StepStatus::Error, "pnpm lint").title(),
            "Failed to run pnpm lint"
        );
        assert_eq!(
            step(StepKind::Execute, StepStatus::Cancelled, "pnpm test").title(),
            "Stopped running pnpm test"
        );

        assert_eq!(
            read("src/lib.rs", StepStatus::Pending).title(),
            "Read src/lib.rs"
        );
        assert_eq!(
            read("src/lib.rs", StepStatus::Running).title(),
            "Reading src/lib.rs"
        );
        assert_eq!(
            read("src/lib.rs", StepStatus::Done).title(),
            "Read src/lib.rs"
        );
        assert_eq!(
            step(StepKind::Edit, StepStatus::Done, "src/lib.rs").title(),
            "Edited src/lib.rs"
        );
        assert_eq!(
            step(StepKind::Delete, StepStatus::Running, "old.rs").title(),
            "Deleting old.rs"
        );
        assert_eq!(
            step(StepKind::Move, StepStatus::Done, "a.rs").title(),
            "Moved a.rs"
        );

        assert_eq!(
            step(StepKind::Search, StepStatus::Running, "foo").title(),
            "Searching for foo"
        );
        assert_eq!(
            step(StepKind::Search, StepStatus::Done, "foo").title(),
            "Searched for foo"
        );
        assert_eq!(
            step(StepKind::Search, StepStatus::Error, "foo").title(),
            "Failed to search for foo"
        );
        assert_eq!(
            step(StepKind::Search, StepStatus::Running, "").title(),
            "Searching"
        );
        assert_eq!(
            step(StepKind::Search, StepStatus::Error, "").title(),
            "Failed to search"
        );
        assert_eq!(
            step(StepKind::Search, StepStatus::Done, "").title(),
            "Searched"
        );

        assert_eq!(
            step(StepKind::Fetch, StepStatus::Done, "example.com").title(),
            "Fetched example.com"
        );
        assert_eq!(
            step(StepKind::SwitchMode, StepStatus::Done, "plan").title(),
            "Switched to plan"
        );
        assert_eq!(
            step(StepKind::SwitchMode, StepStatus::Pending, "").title(),
            "Switch"
        );
        assert_eq!(
            step(StepKind::Subagent, StepStatus::Running, "explorer agent").title(),
            "Running explorer agent"
        );
        assert_eq!(
            step(StepKind::Subagent, StepStatus::Done, "explorer agent").title(),
            "Ran explorer agent"
        );
        assert_eq!(
            step(StepKind::Think, StepStatus::Running, "").title(),
            "Thinking"
        );
        assert_eq!(
            step(StepKind::Think, StepStatus::Done, "").title(),
            "Thought"
        );

        let planning = TraceStep::plan(
            "p",
            vec![PlanItem {
                content: "a".into(),
                status: "in_progress".into(),
                priority: None,
            }],
            StepStatus::Running,
        );
        assert_eq!(planning.title(), "Planning");
        let planned = TraceStep::plan(
            "p",
            vec![
                PlanItem {
                    content: "a".into(),
                    status: "completed".into(),
                    priority: None,
                },
                PlanItem {
                    content: "b".into(),
                    status: "completed".into(),
                    priority: None,
                },
                PlanItem {
                    content: "c".into(),
                    status: "completed".into(),
                    priority: None,
                },
                PlanItem {
                    content: "d".into(),
                    status: "pending".into(),
                    priority: None,
                },
                PlanItem {
                    content: "e".into(),
                    status: "pending".into(),
                    priority: Some("high".into()),
                },
            ],
            StepStatus::Done,
        );
        assert_eq!(planned.title(), "Plan · 3 of 5 done");
    }

    #[test]
    fn from_tool_entry_maps_state_kind_and_body() {
        let tool = |name: &str, state: &str| ToolEntry {
            tool_id: name.into(),
            name: name.into(),
            state: state.into(),
            ..Default::default()
        };
        assert_eq!(
            TraceStep::from(&tool("bash", "running")).status,
            StepStatus::Running
        );
        let pending = TraceStep::from(&tool("bash", "pending"));
        assert_eq!(pending.status, StepStatus::Pending);
        assert!(pending.needs_user);
        assert_eq!(
            TraceStep::from(&tool("bash", "error")).status,
            StepStatus::Error
        );
        assert_eq!(
            TraceStep::from(&tool("bash", "cancelled")).status,
            StepStatus::Cancelled
        );
        assert_eq!(
            TraceStep::from(&tool("bash", "done")).status,
            StepStatus::Done
        );
        assert_eq!(
            TraceStep::from(&tool("bash", "done")).kind,
            StepKind::Execute
        );
        assert_eq!(step_kind("list_dir", "other"), StepKind::Search);
        assert_eq!(step_kind("list_dir", ""), StepKind::Search);
        assert_eq!(step_kind("web_search", ""), StepKind::Fetch);
        assert_eq!(step_kind("TodoWrite", ""), StepKind::Plan);
        assert_eq!(step_kind("mcp__srv__str_replace", ""), StepKind::Edit);
        assert_eq!(step_kind("mystery_tool", ""), StepKind::Other);
        assert_eq!(step_kind("bash", "read"), StepKind::Read);
        assert_eq!(
            step_kind("mcp__keel_browser__browser_screenshot", "execute"),
            StepKind::Browser
        );
        assert_eq!(
            step_kind("mcp__keel-browser__navigate", ""),
            StepKind::Browser
        );

        let mut shot = tool("mcp__keel_browser__browser_screenshot", "done");
        assert_eq!(TraceStep::from(&shot).title(), "Took screenshot");
        shot.state = "running".into();
        assert_eq!(TraceStep::from(&shot).title(), "Taking screenshot");

        let fetched = ToolEntry {
            tool_id: "f".into(),
            // A generic fetch tool; the built-in web_fetch reads "Read <host>".
            name: "fetch".into(),
            state: "done".into(),
            detail: "https://user@example.com:443/a".into(),
            ..Default::default()
        };
        let fetch = TraceStep::from(&fetched);
        assert_eq!(fetch.kind, StepKind::Fetch);
        assert_eq!(fetch.title(), "Fetched example.com");
        let plain = ToolEntry {
            detail: "docs".into(),
            ..fetched.clone()
        };
        assert_eq!(TraceStep::from(&plain).title(), "Fetched docs");

        let agent = ToolEntry {
            tool_id: "s".into(),
            name: "Task".into(),
            state: "running".into(),
            detail: "explorer".into(),
            kind: "subagent".into(),
            ..Default::default()
        };
        assert_eq!(TraceStep::from(&agent).title(), "Running explorer agent");

        let mut failed = tool("read", "error");
        failed.detail = "src/lib.rs".into();
        failed.summary = "no such file".into();
        failed.kind = "read".into();
        let failed = TraceStep::from(&failed);
        assert_eq!(failed.title(), "Failed to read src/lib.rs");
        assert!(
            matches!(failed.body, StepBody::Output { ref tail, error: true } if tail == "no such file")
        );

        let diff = ToolEntry {
            tool_id: "e".into(),
            name: "edit".into(),
            state: "done".into(),
            detail: "src/lib.rs".into(),
            kind: "edit".into(),
            started_ms: Some(0),
            ended_ms: Some(1_400),
            diff: Some(ToolDiffView {
                path: "src/lib.rs".into(),
                old_text: Some("a\nb".into()),
                new_text: "a\nc".into(),
            }),
            output_tail: "ignored".into(),
            ..Default::default()
        };
        let edited = TraceStep::from(&diff);
        assert!(matches!(edited.body, StepBody::Diff { ref path, .. } if path == "src/lib.rs"));
        assert_eq!(edited.description.as_deref(), Some("+1 \u{2212}1"));

        let shots = ToolEntry {
            images: vec!["/tmp/a.png".into()],
            ..tool("bash", "done")
        };
        assert!(
            matches!(TraceStep::from(&shots).body, StepBody::Shots(ref paths) if paths.len() == 1)
        );

        let old = ToolEntry::from_json(&serde_json::json!({
            "kind": "tool",
            "tool_id": "t",
            "name": "bash",
            "state": "done",
            "summary": "ok",
            "detail": "ls"
        }));
        assert!(old.kind.is_empty());
        assert!(old.started_ms.is_none());
        assert!(old.diff.is_none());
        let step = TraceStep::from(&old);
        assert_eq!(step.kind, StepKind::Execute);
        assert_eq!(step.title(), "Ran ls");
        assert!(matches!(step.body, StepBody::Output { ref tail, error: false } if tail == "ok"));

        let newer = ToolEntry::from_json(&serde_json::json!({
            "kind": "tool",
            "tool_kind": "search",
            "tool_id": "g",
            "name": "grep",
            "state": "done",
            "started_ms": 10,
            "ended_ms": 80,
            "output_tail": "hit\n",
            "diff": { "path": "a.rs", "new_text": "z" }
        }));
        assert_eq!(newer.kind, "search");
        assert_eq!(newer.started_ms, Some(10));
        assert_eq!(newer.diff.as_ref().map(|d| d.path.as_str()), Some("a.rs"));
        assert!(newer.diff.as_ref().unwrap().old_text.is_none());
        let string_diff = ToolEntry::from_json(&serde_json::json!({
            "tool_id": "d",
            "name": "edit",
            "diff": "{\"path\":\"b.rs\",\"old_text\":\"a\",\"new_text\":\"b\"}"
        }));
        assert_eq!(string_diff.diff.unwrap().path, "b.rs");
        let other = ToolEntry {
            name: "create_issue".into(),
            state: "done".into(),
            ..Default::default()
        };
        assert_eq!(TraceStep::from(&other).title(), "Create issue");
    }

    #[test]
    fn grouping_folds_reads_and_lifts_errors() {
        let five: Vec<_> = (0..5)
            .map(|i| read(&format!("f{i}.rs"), StepStatus::Done))
            .collect();
        let two_searches = vec![
            step(StepKind::Search, StepStatus::Done, "q"),
            step(StepKind::Search, StepStatus::Done, "r"),
        ];
        let mut many = five;
        many.extend(two_searches);
        let grouped = group_steps(many);
        assert!(
            matches!(&grouped[..], [TraceNode::Explored(group)] if group.title == "Explored 5 files, 2 searches")
        );

        assert!(matches!(
            group_steps(vec![
                read("a", StepStatus::Done),
                read("b", StepStatus::Done)
            ])
            .as_slice(),
            [TraceNode::Step(_), TraceNode::Step(_)]
        ));

        let broken = group_steps(vec![
            read("a", StepStatus::Done),
            read("b", StepStatus::Done),
            step(StepKind::Edit, StepStatus::Done, "c.rs"),
            step(StepKind::Execute, StepStatus::Done, "ls"),
            read("d", StepStatus::Done),
            read("e", StepStatus::Done),
            read("f", StepStatus::Done),
        ]);
        assert!(matches!(
            &broken[..],
            [
                TraceNode::Step(_),
                TraceNode::Step(_),
                TraceNode::Step(edit),
                TraceNode::Step(exec),
                TraceNode::Explored(group),
            ] if edit.kind == StepKind::Edit && exec.kind == StepKind::Execute && group.steps.len() == 3
        ));

        let mut failed = vec![
            read("a", StepStatus::Done),
            read("b", StepStatus::Error),
            read("c", StepStatus::Done),
            read("d", StepStatus::Done),
        ];
        let group = group_steps(failed.split_off(0));
        let TraceNode::Explored(group) = &group[0] else {
            panic!("{:?}", group)
        };
        assert_eq!(group.title, "Explored 4 files · 1 failed");
        assert_eq!(group.status, StepStatus::Error);
        assert_eq!(group.steps.len(), 4);

        let mut waiting = read("secret", StepStatus::Pending);
        waiting.needs_user = true;
        let split = group_steps(vec![
            read("a", StepStatus::Done),
            read("b", StepStatus::Done),
            waiting,
            read("c", StepStatus::Done),
            read("d", StepStatus::Done),
        ]);
        assert!(split.iter().all(|node| matches!(node, TraceNode::Step(_))));
        assert_eq!(split.len(), 5);

        let live = group_steps(vec![
            read("a.rs", StepStatus::Done),
            read("b.rs", StepStatus::Error),
            read("src/lib.rs", StepStatus::Running),
        ]);
        let TraceNode::Explored(group) = &live[0] else {
            panic!("{:?}", live)
        };
        assert_eq!(group.title, "Exploring · Reading src/lib.rs");
        assert_eq!(group.status, StepStatus::Running);
        assert!(!group.title.contains("failed"));

        let mixed = group_steps(vec![
            read("a", StepStatus::Done),
            step(StepKind::Search, StepStatus::Done, "q"),
            step(StepKind::Fetch, StepStatus::Done, "example.com"),
        ]);
        let TraceNode::Explored(group) = &mixed[0] else {
            panic!("{:?}", mixed)
        };
        assert_eq!(group.title, "Explored 1 file, 1 search, 1 fetch");

        let top = group_steps(vec![
            step(StepKind::Delete, StepStatus::Done, "a"),
            step(StepKind::Move, StepStatus::Done, "b"),
            step(StepKind::Think, StepStatus::Done, ""),
            step(StepKind::Browser, StepStatus::Done, ""),
            step(StepKind::Other, StepStatus::Done, "Create issue"),
            TraceStep::plan("p", vec![], StepStatus::Done),
        ]);
        assert_eq!(top.len(), 6);
        assert!(top.iter().all(|node| matches!(node, TraceNode::Step(_))));
    }

    #[test]
    fn worked_label_and_durations() {
        assert_eq!(worked_label(72_000, 1), "Worked for 1m 12s · 1 failed");
        assert_eq!(worked_label(72_000, 0), "Worked for 1m 12s");
        assert_eq!(format_duration(0), "0ms");
        assert_eq!(format_duration(500), "500ms");
        assert_eq!(format_duration(1_000), "1s");
        assert_eq!(format_duration(14_000), "14s");
        assert_eq!(format_duration(60_000), "1m");
        assert_eq!(format_duration(3_600_000), "1h");
        assert_eq!(format_duration(3_601_000), "1h 1s");
        assert_eq!(format_duration(3_661_000), "1h 1m 1s");

        let done = vec![read("a", StepStatus::Done), read("b", StepStatus::Error)];
        assert_eq!(
            turn_summary(&done, 72_000).as_deref(),
            Some("Worked for 1m 12s · 1 failed")
        );
        let mut running = done.clone();
        running.push(read("c", StepStatus::Running));
        assert!(turn_summary(&running, 72_000).is_none());
        let mut pending = read("d", StepStatus::Pending);
        pending.needs_user = true;
        assert!(turn_summary(&[pending], 1_000).is_none());

        let mut parent = step(StepKind::Subagent, StepStatus::Done, "explorer agent");
        parent.children = vec![read("nested", StepStatus::Error)];
        assert_eq!(
            turn_summary(&[parent], 2_000).as_deref(),
            Some("Worked for 2s · 1 failed")
        );
        let mut live_child = step(StepKind::Subagent, StepStatus::Done, "explorer agent");
        live_child.children = vec![read("nested", StepStatus::Running)];
        assert!(turn_summary(&[live_child], 2_000).is_none());
    }

    #[test]
    fn diffstat_counts_unified_and_full_bodies() {
        let unified = "\
--- a/f
+++ b/f
@@ -1,2 +1,3 @@
 keep
-old
+new1
+new2
";
        assert_eq!(diffstat(None, unified), "+2 \u{2212}1");
        assert_eq!(diffstat(Some("a\nb\nc"), "a\nb\nd"), "+1 \u{2212}1");
        assert_eq!(diffstat(Some("a\nb\nc\nd"), "a\nx\ny\nd"), "+2 \u{2212}2");
        assert_eq!(diffstat(Some("same"), "same"), "+0 \u{2212}0");
        assert!(diffstat(None, unified).contains('\u{2212}'));
    }
}

#[cfg(test)]
mod browser_line_tests {
    use super::*;

    fn tool(id: &str, name: &str, state: &str, detail: &str, out: &str) -> ToolEntry {
        ToolEntry {
            tool_id: id.into(),
            name: name.into(),
            state: state.into(),
            detail: detail.into(),
            summary: out.lines().next().unwrap_or_default().into(),
            output_tail: out.into(),
            ..Default::default()
        }
    }

    /// Browser payloads under the server's spellings, running and failed.
    #[test]
    fn real_payloads_read_as_browser_actions() {
        let nav = TraceStep::from(&tool(
            "1",
            "mcp__keel-browser__browser_navigate",
            "done",
            r#"{"url":"https://www.traveloka.com/en-id"}"#,
            "{\n  \"url\": \"https://www.traveloka.com/en-id\",\n  \"title\": \"Traveloka\"\n}",
        ));
        assert_eq!(nav.browser, Some(BrowserOp::Navigate));
        assert_eq!(nav.title(), "Opened traveloka.com/en-id");

        let running = TraceStep::from(&tool(
            "2",
            "keel-browser__browser_navigate",
            "running",
            r#"{"url":"https://www.traveloka.com"}"#,
            "",
        ));
        assert_eq!(running.title(), "Opening traveloka.com");

        let pane = TraceStep::from(&tool("4", "keel-browser_browser_open_pane", "done", "", ""));
        assert_eq!(pane.title(), "Opened browser");
        let pane = TraceStep::from(&tool(
            "4",
            "mcp__keel_browser__browser_open_pane",
            "running",
            "",
            "",
        ));
        assert_eq!(pane.title(), "Opening browser");

        let resona = TraceStep::from(&tool(
            "5",
            "mcp__keel_browser__browser_navigate",
            "error",
            "https://www.resonafinance.co.id/id/about-us/company-profile",
            "browser request timed out",
        ));
        assert_eq!(
            resona.title(),
            "Couldn't open resonafinance.co.id/id/about-us/company-profile"
        );

        let typed = TraceStep::from(&tool(
            "6",
            "mcp__keel-browser__browser_type",
            "done",
            r##"{"selector":"#q","text":"Jakarta to Bali"}"##,
            "{\n  \"text\": \"typed 15 chars into input#q \\\"Jakarta to Bali\\\", pressed Enter\",\n  \"title\": \"Traveloka\",\n  \"url\": \"https://www.traveloka.com/en-id\"\n}",
        ));
        assert_eq!(
            typed.title(),
            "Typed \u{201c}Jakarta to Bali\u{201d} in input#q"
        );
        let StepBody::Browser(b) = &typed.body else {
            panic!("{:?}", typed.body)
        };
        assert!(!b.raw.contains("\\\"Jakarta"), "{}", b.raw);

        let scroll = TraceStep::from(&tool(
            "7",
            "keel-browser:browser_scroll",
            "done",
            r#"{"dy":800}"#,
            "",
        ));
        assert_eq!(scroll.title(), "Scrolled down");
        let read = TraceStep::from(&tool(
            "8",
            "mcp__keel_browser__browser_snapshot",
            "done",
            "",
            "{\n  \"text\": \"Flights Hotels\",\n  \"url\": \"https://www.traveloka.com/en-id\"\n}",
        ));
        assert_eq!(read.title(), "Read page traveloka.com");
        let mut shot = tool(
            "9",
            "mcp__keel_browser__browser_screenshot",
            "done",
            "",
            "Saved to /tmp/s.png",
        );
        shot.images = vec!["/tmp/s.png".into()];
        let shot = TraceStep::from(&shot);
        assert_eq!(shot.title(), "Took screenshot");
        assert!(matches!(shot.body, StepBody::Shots(_)));

        // Consecutive browser lines fold into one natural title.
        let steps = vec![nav, typed, scroll, read, shot];
        let nodes = group_steps(steps);
        assert_eq!(nodes.len(), 1);
        let TraceNode::Browsed(group) = &nodes[0] else {
            panic!("{nodes:?}")
        };
        assert_eq!(group.title, "Browsed traveloka.com \u{b7} 5 actions");
    }

    #[test]
    fn other_mcp_tools_are_not_browser_lines() {
        let other = TraceStep::from(&tool("1", "mcp__linear__save_issue", "done", "", "ok"));
        assert_ne!(other.kind, StepKind::Browser);
        let search = TraceStep::from(&tool(
            "2",
            "search_tool",
            "done",
            "",
            "{\n  \"results\": [{\"server\": \"keel-browser\"}]}",
        ));
        assert_ne!(search.kind, StepKind::Browser);
    }

    fn web(name: &str, state: &str, detail: &str, output: &str) -> TraceStep {
        TraceStep::from(&ToolEntry {
            tool_id: "w".into(),
            name: name.into(),
            state: state.into(),
            detail: detail.into(),
            summary: output.lines().next().unwrap_or_default().into(),
            output_tail: output.into(),
            kind: "search".into(),
            ..ToolEntry::default()
        })
    }

    /// Grok's built-in web tools, as the captured 1.0.52 session sends them:
    /// title "Web search:", no input, the query or page only in the result.
    #[test]
    fn grok_web_tools_read_as_searches_and_reads() {
        let running = web("Web search:", "running", "", "");
        assert_eq!(running.title(), "Searching the web");
        assert_eq!(running.kind, StepKind::Search);
        let done = web(
            "Web search:",
            "done",
            "",
            "Indonesia haze today\n\nhttps://a.example/x\nhttps://b.example/y",
        );
        assert_eq!(
            done.title(),
            "Searched \u{201c}Indonesia haze today\u{201d}"
        );
        assert!(
            matches!(&done.body, StepBody::Links(links) if links.len() == 2 && links[0].title == "a.example")
        );
        let page = web(
            "Web search:",
            "done",
            "",
            "https://bernama.com/en/news.php?id=2621719",
        );
        assert_eq!(page.title(), "Read bernama.com");
        assert_eq!(page.kind, StepKind::Fetch);
        assert_eq!(
            web("web_search", "running", "rust gpui", "").title(),
            "Searching \u{201c}rust gpui\u{201d}"
        );
        assert_eq!(
            web("web_fetch", "running", "https://docs.rs/gpui", "").title(),
            "Reading docs.rs"
        );
        assert_eq!(
            web("fetch_content", "running", "https://docs.rs/gpui", "").title(),
            "Reading docs.rs"
        );
        assert_eq!(
            web("x_search", "done", "keel app", "").title(),
            "Searched X for \u{201c}keel app\u{201d}"
        );
        assert_eq!(web("x_search", "running", "", "").title(), "Searching X");
        assert_eq!(
            web("WebSearch", "error", "q", "boom").title(),
            "Failed to search \u{201c}q\u{201d}"
        );
    }

    /// Grok's running search title carries the verb and a counter:
    /// `Searching "<q>" (0/4 completed)`. Only the query is the subject.
    #[test]
    fn grok_search_progress_title_is_parsed() {
        let running = web(
            "Web search:",
            "running",
            "",
            "Searching \"Indonesia fire haze latest news 2026\" (0/4 completed)",
        );
        assert_eq!(
            running.title(),
            "Searching \u{201c}Indonesia fire haze latest news 2026\u{201d}"
        );
        assert_eq!(running.description.as_deref(), Some("0/4"));
        assert!(matches!(running.body, StepBody::None));
        let curly = web(
            "web_search",
            "running",
            "Searching \u{201c}rust gpui\u{201d} (2/4)",
            "",
        );
        assert_eq!(curly.title(), "Searching \u{201c}rust gpui\u{201d}");
        assert_eq!(curly.description.as_deref(), Some("2/4"));
        assert_eq!(
            web_progress("Indonesia haze today"),
            ("Indonesia haze today".to_string(), None)
        );
        assert_eq!(
            web_progress("Searching for: tokio (1/3 completed)"),
            ("tokio".to_string(), Some("1/3".to_string()))
        );
        // A query that merely starts with the word is left alone.
        assert_eq!(
            web_progress("searchingly odd"),
            ("searchingly odd".to_string(), None)
        );
    }
}

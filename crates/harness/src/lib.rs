//! Harness trait, the mock driver, and the Pi driver that runs Keel.
//!
//! `run` returns a `'static` stream. The engine keeps [`ControlMsg`]'s sender.
//! The task that owns the child owns `controls.inbound`.

mod antigravity;
pub mod keel;
mod mock;
mod pi;
mod proc;

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::Stream;
use keel_proto::{HarnessId, PermissionMode, UserInputAnswer};
use tokio::sync::mpsc;

pub use keel::{KeelProfile, PI_VERSION, PiProfile, pi_agent_dir, set_pi_agent_dir};
pub use mock::{MockHarness, Step};
pub use pi::{MEMORY_PREAMBLE, MEMORY_PROTO, MEMORY_UNAVAILABLE, PiHarness};

/// Reports a CLI session id and, when the CLI has one, its session file.
pub type SessionReport = Arc<dyn Fn(String, Option<String>) + Send + Sync>;

/// Why a driver could not start or continue a turn.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("harness is not installed at {path}: {hint}")]
    NotInstalled { path: PathBuf, hint: String },
    #[error("{0}")]
    Failed(String),
}

/// One signed-in check: an account or a provider of a CLI.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AuthAccount {
    /// Pi's provider id (`anthropic`, `commandcode`, …).
    pub id: String,
    pub name: String,
    /// What signing in to it takes, for Settings. Empty for unlisted providers.
    pub hint: String,
    pub signed_in: bool,
    /// The stored credential's type (`oauth`, `api_key`), or why not.
    pub detail: String,
}

/// A step of a [`Harness::sign_in`] the user has to see or act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInEvent {
    /// Finish in the browser at `url`; a local callback completes it.
    Browser { url: String },
    /// Enter `code` at `url` (GitHub's device flow).
    DeviceCode { code: String, url: String },
    /// What the login is doing now ("Enabling models...").
    Progress(String),
}

/// Run the CLI once in the temp dir, stdin closed, stdout and stderr
/// captured. `None` when it can't start or outlives `limit`.
pub(crate) async fn capture(
    exe: &std::path::Path,
    args: &[String],
    limit: std::time::Duration,
) -> Option<std::process::Output> {
    let mut cmd = proc::command(exe);
    cmd.args(args)
        .current_dir(std::env::temp_dir())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    tokio::task::spawn_blocking(move || proc::output_timeout(cmd, limit))
        .await
        .ok()
        .flatten()
}

/// A one-shot prompt that runs longer than this is killed.
pub(crate) const ONE_SHOT_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// `[flag, model]` when a model is set, else nothing (the CLI's default).
fn model_flag(flag: &str, model: &ModelId) -> Vec<String> {
    if model.0.is_empty() {
        Vec::new()
    } else {
        vec![flag.to_string(), model.0.clone()]
    }
}

/// Model id the session spec stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelId(pub String);

/// One model a driver reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub id: ModelId,
    pub name: String,
}

/// Reasoning level name. Empty means the driver default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningLevel(pub String);

/// An image to hand the CLI with the first prompt of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentFile {
    pub path: PathBuf,
    pub mime: String,
    pub name: String,
}

impl AttachmentFile {
    /// Base64 of the file, for protocols that inline images.
    pub fn base64(&self) -> std::io::Result<String> {
        use base64::Engine;
        Ok(base64::engine::general_purpose::STANDARD.encode(std::fs::read(&self.path)?))
    }
}

/// Read each image for inlining. Files that cannot be read are returned
/// separately so the prompt can name their paths instead.
pub fn inline_images(
    files: &[AttachmentFile],
) -> (Vec<(&AttachmentFile, String)>, Vec<&AttachmentFile>) {
    let mut inline = Vec::new();
    let mut unread = Vec::new();
    for file in files {
        match file.base64() {
            Ok(data) => inline.push((file, data)),
            Err(_) => unread.push(file),
        }
    }
    (inline, unread)
}

/// Text that tells the agent where attached files are, for any image the
/// protocol could not carry inline. Empty when every image went inline.
pub fn attachment_note(files: &[&AttachmentFile]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut note = String::from("\n\nAttached images:");
    for file in files {
        note.push_str(&format!("\n- {} ({})", file.path.display(), file.name));
    }
    note
}

/// Id the CLI uses for its own session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSessionId(pub String);

/// What the engine sends the task that owns the child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlMsg {
    Steer { text: String },
    Interrupt,
    Answer { answers: Vec<UserInputAnswer> },
}

/// Receiver half. It moves into the child task inside [`Harness::run`].
pub struct RunControls {
    pub inbound: mpsc::UnboundedReceiver<ControlMsg>,
}

/// Arguments for one run. Later steers arrive on [`RunControls`].
pub struct SessionSpec {
    pub cwd: PathBuf,
    pub model: ModelId,
    pub reasoning: Option<ReasoningLevel>,
    pub resume: Option<NativeSessionId>,
    /// The Send, or a steer promoted while the session is idle.
    pub initial_text: String,
    /// Images on the Send, resolved to files on this device.
    pub attachments: Vec<AttachmentFile>,
    /// The engine stores the CLI session file. Drivers that have none ignore it.
    pub report_session: Option<SessionReport>,
    /// Report token usage events (including idle cache warming).
    pub report_usage: Option<Arc<dyn Fn(keel_proto::TokenUsage) + Send + Sync>>,
    /// Keel's data folder. Session files and per-run extensions (chat cards,
    /// permission gate) go here, whether or not the browser is wired up.
    pub data_dir: Option<PathBuf>,
    /// When set, every driver injects Keel's browser MCP at this socket, in
    /// its own dialect (see [`browser_mcp`]).
    pub browser_sock: Option<PathBuf>,
    /// The chat this run belongs to. The browser shim sends it with each
    /// call so the pane gives every chat its own tab and queue.
    pub browser_chat: Option<String>,
    /// The chat this run belongs to, if known.
    pub chat_id: Option<String>,
    /// Picked by the user; drivers map it onto the CLI's own flags.
    pub permission: PermissionMode,
    /// Plan mode for this run. Off unless the chat saved it. It narrows
    /// tools; it does not change [`Self::permission`].
    pub plan_mode: bool,
    /// The chat's project, when it has one: its folders in scope as named
    /// roots, the first being `cwd`. Drivers tell the agent about them out of
    /// sight of the chat and, where the CLI can, allow them as working dirs.
    pub project: Option<ProjectRoots>,
    /// Opt-in: extended prompt cache retention (PI_CACHE_RETENTION=long).
    pub longer_cache: bool,
    /// Keep prompt cache warm between turns (cacheWarming: "idle" vs "streaming").
    pub keep_cache_warm: bool,
    /// Idle eviction timeout in seconds (default: 15 min).
    pub idle_eviction_timeout_secs: Option<u32>,
}

/// A project's folders as the agent sees them (see [`SessionSpec::project`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectRoots {
    pub name: String,
    /// `(alias, absolute path)`, primary first.
    pub roots: Vec<(String, String)>,
}

/// True when a tool name (or a longer tool title containing it) spawns a
/// subagent: a whole-name match on the normalized name (`task`, `agent`,
/// `spawn_subagent`, …), or a standalone spawn word inside a longer title.
pub fn is_spawn_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
    let norm = lower.replace(['-', ' ', '_'], "");
    if matches!(
        norm.as_str(),
        "task"
            | "agent"
            | "subagent"
            | "startsubagent"
            | "invokesubagent"
            | "spawnagent"
            | "spawnsubagent"
    ) {
        return true;
    }
    lower
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|word| {
            matches!(
                word,
                "start_subagent" | "invoke_subagent" | "spawn_agent" | "spawn_subagent"
            )
        })
}

/// A spawn call's `(name, description)` from its arguments: an object or a
/// JSON-encoded string.
pub fn spawn_meta(args: &serde_json::Value) -> (Option<String>, Option<String>) {
    let parsed;
    let args = match args.as_str() {
        Some(text) => {
            parsed = serde_json::from_str::<serde_json::Value>(text).unwrap_or_default();
            &parsed
        }
        None => args,
    };
    let field = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            args.get(*key)?
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        })
    };
    (
        field(&["agent", "name", "agent_name", "subagent_type", "role"]),
        field(&["description", "task", "prompt", "goal"]),
    )
}

/// One of Keel's browser pane tools: `mcp__keel-browser__browser_click` as
/// Pi names it, another spelling of the server, or a bare `browser_…`.
pub fn is_browser_tool(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase().replace('-', "_");
    n.contains(&BROWSER_MCP_NAME.replace('-', "_")) || n.starts_with("browser_")
}

/// `detail` for a browser tool: the arguments that name what it acts on, as
/// compact JSON (`{"url":…}`, `{"selector":…,"text":…}`), so the row can say
/// "Clicked …" or "Typed … in …". Text typed into a password-like field is
/// masked here, before it is stored.
pub fn browser_detail(input: &serde_json::Value) -> String {
    use serde_json::Value;
    let cut = |s: &str, n: usize| -> String {
        if s.chars().count() <= n {
            s.to_string()
        } else {
            s.chars().take(n).collect::<String>() + "…"
        }
    };
    let mut out = serde_json::Map::new();
    let selector = input
        .get("selector")
        .cloned()
        .and_then(|v| v.as_str().map(str::to_string));
    for (key, cap) in [
        ("url", 160usize),
        ("selector", 80),
        ("text", 60),
        ("to", 20),
        ("key", 30),
        ("value", 60),
        ("ref", 16),
        ("level", 12),
        ("mode", 12),
    ] {
        if let Some(Value::String(s)) = input.get(key).cloned() {
            if s.is_empty() {
                continue;
            }
            let s = if matches!(key, "text" | "value")
                && selector.as_deref().is_some_and(secret_field)
            {
                "•".repeat(s.chars().count().clamp(4, 8))
            } else {
                cut(&s, cap)
            };
            out.insert(key.to_string(), Value::String(s));
        }
    }
    for key in ["x", "y", "dx", "dy"] {
        match input.get(key).cloned() {
            Some(Value::Number(n)) => {
                out.insert(key.to_string(), Value::Number(n));
            }
            _ => {}
        }
    }
    // select_option's values read as one value; upload paths by file name.
    let list = |key: &str| -> Option<Vec<String>> {
        match input.get(key).cloned()? {
            Value::Array(items) => Some(
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
            ),
            Value::String(s) => serde_json::from_str::<Vec<String>>(&s)
                .ok()
                .or(Some(vec![s])),
            _ => None,
        }
        .filter(|v: &Vec<String>| !v.is_empty())
    };
    if !out.contains_key("value")
        && let Some(values) = list("values")
    {
        out.insert(
            "value".to_string(),
            Value::String(cut(&values.join(", "), 60)),
        );
    }
    if let Some(paths) = list("paths") {
        let names: Vec<&str> = paths
            .iter()
            .map(|p| p.rsplit('/').next().unwrap_or(p))
            .collect();
        out.insert(
            "paths".to_string(),
            Value::String(cut(&names.join(", "), 80)),
        );
    }
    for key in ["timeout_ms", "seconds"] {
        if let Some(Value::Number(n)) = input.get(key).cloned() {
            out.insert(key.to_string(), Value::Number(n));
        }
    }
    for key in ["submit", "full_page", "accept", "double"] {
        if let Some(Value::Bool(b)) = input.get(key).cloned() {
            out.insert(key.to_string(), Value::Bool(b));
        }
    }
    if out.is_empty() {
        String::new()
    } else {
        Value::Object(out).to_string()
    }
}

/// A selector that looks like a password, PIN or one-time-code field.
pub fn secret_field(selector: &str) -> bool {
    let s = selector.to_ascii_lowercase();
    [
        "pass",
        "pwd",
        "secret",
        "otp",
        "one-time-code",
        "cvv",
        "cvc",
        "pin\"",
        "pin]",
        "#pin",
        "[name=pin",
    ]
    .iter()
    .any(|k| s.contains(k))
}

/// [`tool_detail`], or [`browser_detail`] for a browser tool.
pub fn tool_detail_for(name: &str, input: &serde_json::Value) -> String {
    if is_browser_tool(name) {
        let detail = browser_detail(input);
        if !detail.is_empty() {
            return detail;
        }
    }
    tool_detail(input)
}

/// Agent-written progress label from tool args (`label`, else Antigravity
/// `toolAction`). Empty when missing or looking like a raw command.
pub fn tool_label(input: &serde_json::Value) -> String {
    for key in ["label", "toolAction"] {
        if let Some(raw) = input.get(key).and_then(|v| v.as_str()) {
            let cleaned = sanitize_label(raw);
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
    }
    String::new()
}

/// Keep a short human label; drop command-like or empty text.
pub fn sanitize_label(raw: &str) -> String {
    let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = keel_proto::trim_progress_label(&text);
    if text.is_empty() {
        return String::new();
    }

    // Check if the entire string is wrapped in backticks (e.g. `rg oncycle` or `bad`).
    let is_entirely_backticked = text.starts_with('`') && text.ends_with('`') && text.len() >= 2;
    let stripped = text.replace('`', "");
    let stripped = stripped.trim();
    if stripped.is_empty() {
        return String::new();
    }

    // If the entire text was a single backticked token or command, drop it.
    if is_entirely_backticked && (!stripped.contains(' ') || looks_like_command(stripped)) {
        return String::new();
    }

    if looks_like_command(stripped) {
        return String::new();
    }

    if stripped.starts_with('/') || stripped.starts_with("~/") || stripped.starts_with("./") {
        return String::new();
    }

    // A bare file path (slash, no spaces, ASCII path characters) is a command/target, e.g. "src/lib.rs".
    // Natural non-ASCII phrases with slashes (e.g. Japanese "認証/認可フローの確認") are preserved.
    if stripped.contains('/') && !stripped.contains(' ') {
        let is_ascii_path = stripped
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'));
        if is_ascii_path {
            return String::new();
        }
    }

    // Cap at 60 characters cleanly.
    if stripped.chars().count() > 60 {
        if !stripped.contains(' ') {
            let is_all_ascii = stripped.chars().all(|c| c.is_ascii());
            if is_all_ascii {
                return String::new();
            }
            let head: String = stripped.chars().take(60).collect();
            return keel_proto::trim_progress_label(&head).to_string();
        }
        let head: String = stripped.chars().take(60).collect();
        let trimmed = head.rfind(' ').map(|ix| &head[..ix]).unwrap_or(&head);
        return keel_proto::trim_progress_label(trimmed).to_string();
    }

    stripped.to_string()
}

fn looks_like_command(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.starts_with("--") || trimmed.starts_with('-') {
        return true;
    }
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    const COMMAND_BINARIES: &[&str] = &[
        "rg", "grep", "find", "cat", "ls", "cd", "cargo", "pnpm", "npm", "yarn", "bun",
        "git", "python", "python3", "node", "curl", "wget", "bash", "sh", "zsh", "rm", "cp", "mv",
        "mkdir", "touch", "chmod", "chown", "kill", "ps", "sed", "awk", "diff",
    ];
    if COMMAND_BINARIES.contains(&first_word) {
        return true;
    }
    false
}

#[cfg(test)]
mod label_tests {
    use super::{sanitize_label, tool_label};
    use serde_json::json;

    #[test]
    fn sanitize_keeps_phrases_and_drops_commands() {
        assert_eq!(
            sanitize_label("Reading the on/off cycle logic"),
            "Reading the on/off cycle logic"
        );
        assert_eq!(
            sanitize_label("Reading the on/off cycle logic..."),
            "Reading the on/off cycle logic"
        );
        assert_eq!(
            sanitize_label("Reading the on/off cycle logic…"),
            "Reading the on/off cycle logic"
        );
        assert_eq!(
            sanitize_label("Reading the on/off cycle logic."),
            "Reading the on/off cycle logic"
        );
        assert_eq!(
            sanitize_label("Checking v1.2 config"),
            "Checking v1.2 config"
        );
        assert_eq!(
            sanitize_label("Checking v1.2 config..."),
            "Checking v1.2 config"
        );
        assert_eq!(sanitize_label("  Searching payroll code  "), "Searching payroll code");
        assert!(sanitize_label("").is_empty());
        assert!(sanitize_label("`rg oncycle`").is_empty());
        assert!(sanitize_label("rg --type rust oncycle").is_empty());
        assert!(sanitize_label("/Users/kevin/Codes/keel").is_empty());
        assert!(sanitize_label("~/Codes/keel").is_empty());
        assert!(sanitize_label("./src/lib.rs").is_empty());
        assert!(sanitize_label("src/lib.rs").is_empty());
        assert!(sanitize_label(&"x".repeat(61)).is_empty());
        // Resilient preservation: inline backticks stripped, CLI flags in phrases kept, CJK with slashes kept
        assert_eq!(
            sanitize_label("Checking `StepSwap` timing"),
            "Checking StepSwap timing"
        );
        assert_eq!(
            sanitize_label("Testing --dry-run option"),
            "Testing --dry-run option"
        );
        assert_eq!(
            sanitize_label("Updating parser -- handling errors"),
            "Updating parser -- handling errors"
        );
        assert_eq!(
            sanitize_label("認証/認可フローの確認"),
            "認証/認可フローの確認"
        );
    }

    #[test]
    fn tool_label_prefers_label_then_tool_action() {
        assert_eq!(
            tool_label(&json!({"label": "Reading the on/off cycle logic", "path": "a.rs"})),
            "Reading the on/off cycle logic"
        );
        assert_eq!(
            tool_label(&json!({"toolAction": "Inspecting payroll state transitions"})),
            "Inspecting payroll state transitions"
        );
        assert_eq!(
            tool_label(&json!({"label": "`bad`", "toolAction": "Searching oncycle"})),
            "Searching oncycle"
        );
        assert!(tool_label(&json!({"command": "ls"})).is_empty());
    }
}


/// What a tool call works on, for its row: the file, command, pattern, query
/// or URL in its input, on
/// one line with the home dir as `~`. Empty when there is none.
pub fn tool_detail(input: &serde_json::Value) -> String {
    const KEYS: [&str; 8] = [
        "file_path",
        "path",
        "filePath",
        "notebook_path",
        "command",
        "cmd",
        "pattern",
        "query",
    ];
    let text = |v: &serde_json::Value| -> Option<String> {
        Some(
            v.as_str()?
                .trim()
                .lines()
                .next()
                .unwrap_or_default()
                .to_string(),
        )
        .filter(|s| !s.is_empty())
    };
    let Some(raw) = KEYS
        .iter()
        .chain(&["url"])
        .find_map(|key| text(&input[*key]))
    else {
        return String::new();
    };
    let raw = match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && raw.starts_with(&home) => {
            format!("~{}", &raw[home.len()..])
        }
        _ => raw,
    };
    raw.chars().take(200).collect()
}

/// ACP kind inferred from a tool name when the harness omits `kind`, or sent
/// only a generic `other`. The last path segment is the name (`mcp__server__read`
/// → `read`). Returns the lowercase ACP kind.
pub fn kind_from_name(name: &str) -> Option<&'static str> {
    let base = name.strip_prefix("Running ").unwrap_or(name);
    let base = base.rsplit("__").next().unwrap_or(base);
    let base = base.rsplit(['/', ':']).next().unwrap_or(base);
    let n = base.to_ascii_lowercase().replace(['-', ' '], "_");
    if n == "memory_read" || n == "memory_write" {
        return Some("memory");
    }
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

/// Plan rows from an `entries` array, a `plan` array, or a
/// TodoWrite `todos` / `tasks` object. Empty when nothing structured is there.
pub fn plan_entries(value: &serde_json::Value) -> Vec<keel_proto::PlanEntry> {
    let items = value
        .as_array()
        .or_else(|| value.get("entries").and_then(|v| v.as_array()))
        .or_else(|| value.get("plan").and_then(|v| v.as_array()))
        .or_else(|| value.get("todos").and_then(|v| v.as_array()))
        .or_else(|| value.get("tasks").and_then(|v| v.as_array()));
    let Some(items) = items else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            if let Some(text) = item.as_str().filter(|text| !text.is_empty()) {
                return Some(keel_proto::PlanEntry {
                    content: text.to_string(),
                    status: "pending".to_string(),
                    priority: None,
                });
            }
            let content = ["content", "text", "step", "title"]
                .iter()
                .find_map(|key| item[key].as_str().filter(|text| !text.is_empty()))?;
            let status = match item["status"].as_str().unwrap_or("pending") {
                "inProgress" | "in_progress" | "running" => "in_progress",
                "completed" | "done" | "complete" => "completed",
                "cancelled" | "canceled" => "cancelled",
                "pending" | "todo" => "pending",
                other => other,
            };
            Some(keel_proto::PlanEntry {
                content: content.to_string(),
                status: status.to_string(),
                priority: item["priority"]
                    .as_str()
                    .filter(|p| !p.is_empty())
                    .map(str::to_string),
            })
        })
        .collect()
}

/// At most ten streaming output updates a second for one tool.
#[derive(Debug, Default)]
pub struct ProgressGate {
    last: HashMap<String, Instant>,
}

impl ProgressGate {
    const GAP: Duration = Duration::from_millis(100);

    pub fn allow(&mut self, tool_id: &str) -> bool {
        self.allow_at(tool_id, Instant::now())
    }

    pub fn allow_at(&mut self, tool_id: &str, now: Instant) -> bool {
        if let Some(then) = self.last.get(tool_id)
            && now.saturating_duration_since(*then) < Self::GAP
        {
            return false;
        }
        self.last.insert(tool_id.to_string(), now);
        true
    }
}

/// Images in tool-result content blocks, as `data:` URIs, plus files a
/// result names with `Saved to <absolute path>` (Keel's browser screenshot
/// says where it saved the PNG). Handles MCP / ACP image blocks (also
/// wrapped in ACP `{"type":"content"}`), Claude base64 `source` blocks and
/// image `resource` blobs.
pub fn content_images(parts: &[serde_json::Value]) -> Vec<String> {
    let mut out = Vec::new();
    for part in parts {
        let part = if part["type"] == "content" && part["content"].is_object() {
            &part["content"]
        } else {
            part
        };
        let data_uri = |mime: Option<&str>, data: Option<&str>| {
            data.filter(|d| !d.is_empty())
                .map(|d| format!("data:{};base64,{d}", mime.unwrap_or("image/png")))
        };
        let found = match part["type"].as_str() {
            Some("image") => data_uri(
                part["mimeType"].as_str().or(part["mime_type"].as_str()),
                part["data"].as_str(),
            )
            .or_else(|| {
                data_uri(
                    part["source"]["media_type"].as_str(),
                    part["source"]["data"].as_str(),
                )
            }),
            Some("resource")
                if part["resource"]["mimeType"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("image/")) =>
            {
                data_uri(
                    part["resource"]["mimeType"].as_str(),
                    part["resource"]["blob"].as_str(),
                )
            }
            Some("text") => {
                out.extend(saved_paths(part["text"].as_str().unwrap_or_default()));
                None
            }
            _ => None,
        };
        out.extend(found);
    }
    out
}

/// Absolute image paths after `Saved to ` in `text`.
pub fn saved_paths(text: &str) -> Vec<String> {
    text.match_indices("Saved to ")
        .filter_map(|(at, needle)| {
            let rest = &text[at + needle.len()..];
            let path: String = rest
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '`' && *c != '"')
                .collect();
            let path = path.trim_end_matches(['.', ',', ';', ')']).to_string();
            let lower = path.to_ascii_lowercase();
            (path.starts_with('/')
                && [".png", ".jpg", ".jpeg", ".gif", ".webp"]
                    .iter()
                    .any(|e| lower.ends_with(e)))
            .then_some(path)
        })
        .collect()
}

/// Server name for Keel's browser pane tools. Pi shows them as
/// `mcp__keel-browser__browser_navigate`.
pub const BROWSER_MCP_NAME: &str = "keel-browser";

/// Server name for the chat-card tools (`show_chart`, `show_choices`, …).
pub const UI_MCP_NAME: &str = "keel-ui";

/// Read-only tool bases allowed while Plan mode is on. Anything else,
/// including edit, write, shell, and a mutating MCP call, is denied by Keel.
pub fn plan_readonly_tools() -> &'static [&'static str] {
    &[
        "read",
        "read_file",
        "grep",
        "find",
        "ls",
        "list",
        "list_dir",
        "glob",
        "search",
        "memory_read",
        "cat",
        "web_search",
        "web_fetch",
        "fetch",
        "open_page",
        "browser_navigate",
        "browser_snapshot",
        "browser_read",
        "browser_screenshot",
        "browser_tabs",
        "browser_console",
        "browser_list",
        "browser_get",
    ]
}

/// The tool's own name: the last `__` segment, lowercase, dashes as underscores.
pub fn tool_base(name: &str) -> String {
    let base = name.rsplit("__").next().unwrap_or(name);
    let base = base.rsplit(['/', ':']).next().unwrap_or(base);
    base.trim().to_ascii_lowercase().replace('-', "_")
}

/// Whether Plan mode lets this tool run. UI cards (including `show_plan`) are
/// allowed. Edit, write, shell, exec, memory write, and browser click or type
/// are not.
pub fn plan_mode_allows(name: &str) -> bool {
    let base = tool_base(name);
    if keel_proto::is_ui_tool(&base) {
        return true;
    }
    plan_readonly_tools().contains(&base.as_str())
}

/// While Plan mode is on, answer a Pi confirm or an Antigravity `select`
/// here instead of asking. Reads are allowed. Mutating calls are denied.
/// `None` leaves the question for the user (a real question, not a tool ask).
pub fn plan_mode_ui_reply(value: &serde_json::Value) -> Option<serde_json::Value> {
    if value.get("type").and_then(|t| t.as_str()) != Some("extension_ui_request") {
        return None;
    }
    let method = value.get("method").and_then(|m| m.as_str())?;
    let id = value.get("id").and_then(|id| id.as_str()).unwrap_or("pi-question");
    let title = value.get("title").and_then(|t| t.as_str()).unwrap_or("");
    let message = value.get("message").and_then(|m| m.as_str()).unwrap_or("");
    match method {
        "confirm" => {
            let name = confirm_tool_name(title).or_else(|| confirm_tool_name(message))?;
            let allow = plan_mode_allows(&name);
            Some(serde_json::json!({
                "type": "extension_ui_response",
                "id": id,
                "confirmed": allow,
            }))
        }
        "select" if title.starts_with("agy requests") || message.starts_with("agy requests") => {
            let text = if title.starts_with("agy requests") {
                title
            } else {
                message
            };
            let allow = agy_request_is_read(text);
            let options = select_labels(value);
            if allow {
                let picked = options
                    .iter()
                    .find(|o| o.to_ascii_lowercase().contains("allow"))
                    .cloned()
                    .or_else(|| options.first().cloned())
                    .unwrap_or_default();
                if picked.is_empty() {
                    Some(serde_json::json!({
                        "type": "extension_ui_response",
                        "id": id,
                        "cancelled": true,
                    }))
                } else {
                    Some(serde_json::json!({
                        "type": "extension_ui_response",
                        "id": id,
                        "value": picked,
                    }))
                }
            } else {
                let reject = options.into_iter().find(|o| {
                    let l = o.to_ascii_lowercase();
                    l.contains("reject") || l.contains("deny") || l.contains("cancel") || l == "no"
                });
                match reject {
                    Some(picked) => Some(serde_json::json!({
                        "type": "extension_ui_response",
                        "id": id,
                        "value": picked,
                    })),
                    None => Some(serde_json::json!({
                        "type": "extension_ui_response",
                        "id": id,
                        "cancelled": true,
                    })),
                }
            }
        }
        _ => None,
    }
}

fn confirm_tool_name(text: &str) -> Option<String> {
    let rest = text.trim().strip_prefix("Allow ")?;
    let name = rest.trim().trim_end_matches(['?', '.', ' ']);
    let name = name.split_whitespace().next().unwrap_or("");
    (!name.is_empty()).then(|| name.to_string())
}

fn select_labels(value: &serde_json::Value) -> Vec<String> {
    value
        .get("options")
        .and_then(|o| o.as_array())
        .map(|options| {
            options
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .or_else(|| item.get("label").and_then(|l| l.as_str()).map(str::to_string))
                        .filter(|s| !s.is_empty())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// An Antigravity permission title is a read when it names a read and no
/// mutating verb. Unknown requests are denied.
fn agy_request_is_read(text: &str) -> bool {
    let words: Vec<String> = text
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect();
    // A bare "memory" word must not deny memory_read. Deny the pair.
    let mutating = words.windows(2).any(|w| {
        matches!(
            (w[0].as_str(), w[1].as_str()),
            ("memory", "write") | ("apply", "patch") | ("str", "replace")
        )
    }) || words.iter().any(|w| {
        matches!(
            w.as_str(),
            "edit"
                | "write"
                | "bash"
                | "shell"
                | "exec"
                | "execute"
                | "click"
                | "type"
                | "delete"
                | "remove"
                | "mkdir"
        )
    });
    if mutating {
        return false;
    }
    words.iter().any(|w| {
        matches!(
            w.as_str(),
            "read" | "grep" | "search" | "list" | "glob" | "find" | "fetch" | "snapshot"
                | "navigate" | "web" | "ls" | "cat"
        )
    })
}

/// Told to the agent only while Plan mode is on.
pub const PLAN_MODE_PROMPT: &str = "\
Plan mode is on. Research with read-only tools first, then emit exactly one plan card with show_plan, and stop. Do not edit files, run commands, or call a tool that changes state.";

/// The `keel_cards` prompt section, added on turns whose tools include the
/// cards. The tools' own descriptions say how to call each one; this says
/// when a card beats writing the same thing yourself.
pub const UI_CARDS_PROMPT: &str = "\
Keel draws native cards in the chat with the mcp__keel_ui__show_* tools. Prefer a card to drawing the same thing yourself in markdown:
- A flow, architecture, sequence or data model: show_diagram with Mermaid source, not an ASCII drawing or a mermaid fence in your reply.
- Main task execution steps in the right session panel: task_steps (or show_steps with kind: \"task\"). For how-to guides/tutorials in chat: show_steps with kind: \"guide\".
- Options or a before/after, as claims: show_compare, not side-by-side prose.
- A question with 2 to 6 answers: ask it in a sentence, then show_choices.
- A test run you report: show_tests. Files you changed this turn: show_files. A merge request you opened or updated: show_mr.
A card is the user's copy of what it shows. Never repeat its content in your reply; refer to it in a sentence instead.
Three cards go with text you write: show_chart turns a markdown table of measured numbers into a chart (it replaces the table), show_stats adds before/after tiles for figures you wrote, and show_commands adds a Run button to a shell block you wrote.
Skip cards when plain text is enough.";

/// The stdio MCP server behind Keel's browser pane:
/// `<keel exe> mcp-browser --socket <sock>`, registered with Pi by a per-run
/// extension calling `registerMcpServer`.
///
/// `env` carries the per-launch socket token and the chat id. Each dialect
/// hands it only to the shim's own environment, never to argv, and the
/// struct's `Debug` redacts the token.
#[derive(Clone, PartialEq, Eq)]
pub struct BrowserMcp {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl std::fmt::Debug for BrowserMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env: Vec<(&str, &str)> = self
            .env
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str(),
                    if k == BROWSER_TOKEN_ENV {
                        "<redacted>"
                    } else {
                        v.as_str()
                    },
                )
            })
            .collect();
        f.debug_struct("BrowserMcp")
            .field("command", &self.command)
            .field("args", &self.args)
            .field("env", &env)
            .finish()
    }
}

/// Env var the shim reads its socket token from.
pub const BROWSER_TOKEN_ENV: &str = "KEEL_BROWSER_TOKEN";
/// Env var naming the chat the shim works for.
pub const BROWSER_CHAT_ENV: &str = "KEEL_BROWSER_CHAT";

static BROWSER_TOKEN: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// The engine sets the browser socket's per-launch token once at startup.
pub fn set_browser_token(token: &str) {
    if let Ok(mut slot) = BROWSER_TOKEN.write() {
        *slot = Some(token.to_string()).filter(|t| !t.is_empty());
    }
}

fn browser_token() -> Option<String> {
    BROWSER_TOKEN.read().ok().and_then(|t| t.clone())
}

/// `None` without a socket or when Keel cannot name its own executable; the
/// run then goes ahead without browser tools.
pub fn browser_mcp(sock: Option<&std::path::Path>, chat: Option<&str>) -> Option<BrowserMcp> {
    let sock = sock?;
    // `KEEL_BROWSER_MCP_EXE` names the Keel binary when this process isn't
    // one (live harness tests run from the test binary).
    let exe = std::env::var_os("KEEL_BROWSER_MCP_EXE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())?;
    let mut env = Vec::new();
    if let Some(token) = browser_token() {
        env.push((BROWSER_TOKEN_ENV.to_string(), token));
    }
    if let Some(chat) = chat.filter(|c| !c.is_empty()) {
        env.push((BROWSER_CHAT_ENV.to_string(), chat.to_string()));
    }
    Some(BrowserMcp {
        command: exe.to_string_lossy().into_owned(),
        args: vec![
            "mcp-browser".to_string(),
            "--socket".to_string(),
            sock.to_string_lossy().into_owned(),
        ],
        env,
    })
}

/// The stdio MCP server for chat cards: `<keel exe> mcp-ui`. No socket and
/// no token. `None` when Keel cannot name its own executable.
pub fn ui_mcp() -> Option<BrowserMcp> {
    let exe = std::env::var_os("KEEL_BROWSER_MCP_EXE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())?;
    Some(BrowserMcp {
        command: exe.to_string_lossy().into_owned(),
        args: vec!["mcp-ui".to_string()],
        env: Vec::new(),
    })
}

/// Writes a file only its owner can read (0600), in a 0700 folder, for
/// configs that carry the browser token. Replaces it atomically.
pub fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp)?;
    file.write_all(text.as_bytes())?;
    drop(file);
    std::fs::rename(&tmp, path)
}

/// A file-name-safe form of a chat id for per-chat config files.
pub fn chat_file_stem(chat: Option<&str>) -> String {
    let stem: String = chat
        .unwrap_or("")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if stem.is_empty() {
        "shared".to_string()
    } else {
        stem
    }
}

/// A coding-agent CLI. Discovery does not install.
#[async_trait]
pub trait Harness: Send + Sync {
    fn id(&self) -> HarnessId;
    fn display_name(&self) -> &str;
    fn installed(&self) -> bool;
    fn executable(&self) -> Option<PathBuf>;
    fn version(&self) -> &str;

    /// True when the CLI's own terminal frame ends the turn.
    fn deterministic_turn_end(&self) -> bool {
        false
    }

    /// What this CLI's wire lets Keel see and do with the subagents a run
    /// spawns. The default hides them: most drivers have no subagent events
    /// to give, and the engine routes none without `visibility`.
    fn subagent_caps(&self) -> keel_proto::SubagentCaps {
        keel_proto::SubagentCaps::default()
    }

    /// The [`PermissionMode`]s this driver can honour. One entry: no picker.
    fn permission_modes(&self) -> &'static [PermissionMode] {
        &[PermissionMode::Ask]
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError>;
    async fn reasoning_levels(&self, model: &ModelId) -> Result<Vec<ReasoningLevel>, HarnessError>;

    /// One line on what this agent is, for Settings.
    fn description(&self) -> &'static str {
        ""
    }

    /// Evict any warm process kept alive for `chat_id`.
    fn evict_chat(&self, _chat_id: &str) {}

    /// Evict all warm processes across all chats.
    fn evict_all(&self) {}

    /// Whether this CLI is signed in, per account or provider. Read-only:
    /// asks the CLI, never reads or stores its credentials. Empty when the
    /// CLI has nothing to check.
    async fn auth_status(&self) -> Vec<AuthAccount> {
        Vec::new()
    }

    /// Sign in to model provider `provider` with its subscription (OAuth),
    /// reporting each step to `on`. Resolves once the credential is stored.
    /// Dropping the future cancels the sign-in.
    async fn sign_in(
        &self,
        _provider: &str,
        _on: &(dyn Fn(SignInEvent) + Send + Sync),
    ) -> Result<(), HarnessError> {
        Err(HarnessError::Failed(format!(
            "{} has no sign-in",
            self.display_name()
        )))
    }

    /// Arguments for a throwaway, tool-less, session-less prompt whose answer
    /// is printed to stdout. `None`: the CLI has no such mode.
    fn one_shot_args(&self, _model: &ModelId, _prompt: &str) -> Option<Vec<String>> {
        None
    }

    /// Whether [`Self::one_shot`] works for this CLI. Drivers that answer a
    /// one-shot over their own protocol (no print mode) override both.
    fn has_one_shot(&self, model: &ModelId) -> bool {
        self.one_shot_args(model, "").is_some()
    }

    /// Ask once and return what the CLI printed. Runs in the temp dir so no
    /// project instructions load, and never touches a chat's own session.
    async fn one_shot(&self, model: &ModelId, prompt: &str) -> Result<String, HarnessError> {
        let fail = |why: &str| HarnessError::Failed(format!("{}: {why}", self.display_name()));
        let exe = self.executable().ok_or_else(|| fail("not installed"))?;
        let args = self
            .one_shot_args(model, prompt)
            .ok_or_else(|| fail("no one-shot mode"))?;
        let output = capture(&exe, &args, ONE_SHOT_LIMIT)
            .await
            .ok_or_else(|| fail("timed out"))?;
        if !output.status.success() {
            return Err(fail(&format!("exited with {}", output.status)));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Spawn and return a `'static` stream. `controls.inbound` moves into the
    /// task that produces the stream.
    async fn run(
        &self,
        spec: SessionSpec,
        controls: RunControls,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<keel_proto::AgentEvent, HarnessError>> + Send>>,
        HarnessError,
    >;
}

#[cfg(test)]
mod image_tests {
    use serde_json::json;

    #[test]
    fn content_images_from_every_dialect() {
        let parts = [
            json!({"type": "text", "text": "Saved to /data/screenshots/a.png. It is shown to the user in the chat."}),
            json!({"type": "image", "data": "iVBOR", "mimeType": "image/png"}),
            json!({"type": "content", "content": {"type": "image", "data": "R0lG", "mimeType": "image/gif"}}),
            json!({"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "/9j/"}}),
            json!({"type": "resource", "resource": {"mimeType": "image/webp", "blob": "UklG"}}),
            json!({"type": "resource", "resource": {"mimeType": "text/plain", "text": "x"}}),
            json!({"type": "text", "text": "Saved to relative.png and Saved to /x/notes.txt"}),
        ];
        assert_eq!(
            super::content_images(&parts),
            [
                "/data/screenshots/a.png",
                "data:image/png;base64,iVBOR",
                "data:image/gif;base64,R0lG",
                "data:image/jpeg;base64,/9j/",
                "data:image/webp;base64,UklG",
            ]
        );
    }
}

#[cfg(test)]
mod tool_detail_tests {
    use serde_json::json;

    #[test]
    fn picks_what_the_call_works_on() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            super::tool_detail(&json!({"file_path": format!("{home}/a.rs"), "limit": 5})),
            "~/a.rs"
        );
        // Commands: one line.
        assert_eq!(
            super::tool_detail(&json!({"command": "ls\nrm -rf x"})),
            "ls"
        );
        assert_eq!(
            super::tool_detail(&json!({"url": "https://a.b/c"})),
            "https://a.b/c"
        );
        assert_eq!(super::tool_detail(&json!({"limit": 5})), "");
        assert_eq!(super::tool_detail(&json!(null)), "");
        // Browser tools keep the arguments that name their target; a
        // password field's text never reaches the row.
        assert_eq!(
            super::tool_detail_for(
                "mcp__keel-browser__browser_click",
                &json!({"selector": "#go"})
            ),
            r##"{"selector":"#go"}"##
        );
        assert_eq!(
            super::tool_detail_for(
                "mcp__keel-browser__browser_navigate",
                &json!({"url": "https://a.b/c"})
            ),
            r#"{"url":"https://a.b/c"}"#
        );
        assert_eq!(
            super::tool_detail_for("keel-browser:browser_scroll", &json!({"dy": 600})),
            r#"{"dy":600}"#
        );
        let pw = super::tool_detail_for(
            "mcp__keel_browser__browser_type",
            &json!({"selector": "input[type=password]", "text": "hunter22"}),
        );
        assert!(!pw.contains("hunter"), "{pw}");
        assert_eq!(
            super::tool_detail_for("Read", &json!({"file_path": "/x"})),
            "/x"
        );
    }

    #[test]
    fn kind_from_name_follows_the_fallback_table() {
        let kind = super::kind_from_name;
        assert_eq!(kind("bash"), Some("execute"));
        assert_eq!(kind("Bash"), Some("execute"));
        assert_eq!(kind("shell"), Some("execute"));
        assert_eq!(kind("exec_command"), Some("execute"));
        assert_eq!(kind("read_file"), Some("read"));
        assert_eq!(kind("Read"), Some("read"));
        assert_eq!(kind("view_file"), Some("read"));
        assert_eq!(kind("cat"), Some("read"));
        assert_eq!(kind("grep"), Some("search"));
        assert_eq!(kind("glob"), Some("search"));
        assert_eq!(kind("search_files"), Some("search"));
        assert_eq!(kind("find"), Some("search"));
        assert_eq!(kind("list_dir"), Some("search"));
        assert_eq!(kind("ls"), Some("search"));
        assert_eq!(kind("edit"), Some("edit"));
        assert_eq!(kind("write"), Some("edit"));
        assert_eq!(kind("apply_patch"), Some("edit"));
        assert_eq!(kind("str_replace_editor"), Some("edit"));
        assert_eq!(kind("WebFetch"), Some("fetch"));
        assert_eq!(kind("WebSearch"), Some("fetch"));
        assert_eq!(kind("fetch_content"), Some("fetch"));
        assert_eq!(kind("TodoWrite"), Some("plan"));
        assert_eq!(kind("EnterPlanMode"), Some("plan"));
        assert_eq!(kind("memory_read"), Some("memory"));
        assert_eq!(kind("memory_write"), Some("memory"));
        assert_eq!(kind("mcp__keel__memory_read"), Some("memory"));
        assert_eq!(kind("mcp__keel-browser__read_file"), Some("read"));
        assert_eq!(kind("unknown_tool"), None);
    }

    #[test]
    fn plan_entries_read_each_harness_shape() {
        let acp = super::plan_entries(&json!({"entries": [
            {"content": "read notes", "status": "pending", "priority": "high"},
            {"content": "list files", "status": "in_progress", "priority": "medium"}
        ]}));
        assert_eq!(acp[0].content, "read notes");
        assert_eq!(acp[0].priority.as_deref(), Some("high"));
        assert_eq!(acp[1].status, "in_progress");
        let codex = super::plan_entries(&json!([
            {"step": "Run the tests", "status": "inProgress"},
            {"step": "Done", "status": "completed"}
        ]));
        assert_eq!(codex[0].status, "in_progress");
        assert_eq!(codex[0].priority, None);
        assert_eq!(codex[1].status, "completed");
        let todos = super::plan_entries(&json!({"todos": [
            {"content": "Fix the parser", "status": "pending", "priority": "high", "id": "1"}
        ]}));
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].content, "Fix the parser");
        assert!(super::plan_entries(&json!({"mode": "plan"})).is_empty());
    }

    #[test]
    fn progress_gate_allows_ten_a_second() {
        let mut gate = super::ProgressGate::default();
        let t0 = std::time::Instant::now();
        assert!(gate.allow_at("t", t0));
        assert!(!gate.allow_at("t", t0 + std::time::Duration::from_millis(50)));
        assert!(gate.allow_at("t", t0 + std::time::Duration::from_millis(100)));
        assert!(gate.allow_at("other", t0));
    }
}

#[cfg(test)]
mod browser_mcp_tests {
    use serde_json::json;

    #[test]
    fn new_tool_details_name_refs_values_and_files() {
        // Compared as values: key order depends on serde_json's features.
        let d = |input: serde_json::Value| {
            serde_json::from_str::<serde_json::Value>(&super::browser_detail(&input)).unwrap()
        };
        assert_eq!(
            d(json!({"ref": "e12", "double": true})),
            json!({"double":true,"ref":"e12"})
        );
        assert_eq!(
            d(json!({"ref": "e4", "values": ["Business", "First"]})),
            json!({"ref":"e4","value":"Business, First"})
        );
        assert_eq!(
            d(json!({"ref": "e9", "paths": ["shots/a.png", "/p/docs/b.pdf"]})),
            json!({"paths":"a.png, b.pdf","ref":"e9"})
        );
        assert_eq!(d(json!({"accept": false})), json!({"accept":false}));
        assert_eq!(
            d(json!({"text": "Saved", "timeout_ms": 5000})),
            json!({"text":"Saved","timeout_ms":5000})
        );
        assert_eq!(d(json!({"level": "error"})), json!({"level":"error"}));
    }

    /// The token reaches only the shim's env; `Debug` (and so logs) never
    /// shows it, and argv never carries it.
    #[test]
    fn the_token_rides_in_env_and_is_redacted() {
        super::set_browser_token(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        );
        let mcp = super::browser_mcp(
            Some(std::path::Path::new("/tmp/k/browser.sock")),
            Some("chat-1"),
        )
        .unwrap();
        assert!(mcp.env.contains(&(
            super::BROWSER_TOKEN_ENV.to_string(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string()
        )));
        assert!(
            mcp.env
                .contains(&(super::BROWSER_CHAT_ENV.to_string(), "chat-1".to_string()))
        );
        assert!(!mcp.args.iter().any(|a| a.contains("0123456789abcdef")));
        let shown = format!("{mcp:?}");
        assert!(
            !shown.contains("0123456789abcdef") && shown.contains("<redacted>"),
            "{shown}"
        );
        assert_eq!(super::chat_file_stem(Some("a/b..c")), "a_b__c");
        assert_eq!(super::chat_file_stem(None), "shared");
    }

    #[test]
    fn private_files_are_owner_only() {
        let dir = std::env::temp_dir().join(format!("keel-private-{}", std::process::id()));
        let path = dir.join("sub").join("x.json");
        super::write_private(&path, "{}").unwrap();
        super::write_private(&path, "{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod plan_mode_tests {
    use serde_json::json;

    #[test]
    fn plan_mode_permission_allows_reads_and_denies_mutations() {
        let allow = super::plan_mode_ui_reply(&json!({
            "type": "extension_ui_request",
            "method": "confirm",
            "id": "c1",
            "title": "Allow read?",
        }));
        assert_eq!(allow.unwrap()["confirmed"], true);
        let deny = super::plan_mode_ui_reply(&json!({
            "type": "extension_ui_request",
            "method": "confirm",
            "id": "c2",
            "title": "Allow edit?",
        }));
        assert_eq!(deny.unwrap()["confirmed"], false);
        let agy = super::plan_mode_ui_reply(&json!({
            "type": "extension_ui_request",
            "method": "select",
            "id": "s1",
            "title": "agy requests: Edit src/lib.rs",
            "options": ["Allow once", "Reject"],
        }));
        assert_eq!(agy.unwrap()["value"], "Reject");
        let read = super::plan_mode_ui_reply(&json!({
            "type": "extension_ui_request",
            "method": "select",
            "id": "s2",
            "title": "agy requests: Read src/lib.rs",
            "options": ["Allow once", "Reject"],
        }));
        assert_eq!(read.unwrap()["value"], "Allow once");
        assert!(super::plan_mode_ui_reply(&json!({
            "type": "extension_ui_request",
            "method": "input",
            "id": "q",
            "title": "What next?",
        })).is_none());
        assert!(super::PLAN_MODE_PROMPT.contains("Research"));
        assert!(super::PLAN_MODE_PROMPT.contains("exactly one plan card"));
        assert!(super::PLAN_MODE_PROMPT.contains("stop"));
    }
}

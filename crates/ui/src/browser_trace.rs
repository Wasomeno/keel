//! Keel's browser pane tools as trace lines: which tool ran (under any
//! harness's naming), what it acted on, and a readable result.
//!
//! Pure data, like `trace`. Pi names them `mcp__keel-browser__browser_…`;
//! other spellings of the server (`keel_browser`, `keel-browser:…`) and a
//! bare `browser_…` read the same.

use serde_json::Value;

use crate::icons::Icon;
use crate::trace::Verb;

/// What a browser tool did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserOp {
    OpenPane,
    Navigate,
    NewTab,
    Back,
    Forward,
    Reload,
    Click,
    Type,
    Scroll,
    Snapshot,
    Screenshot,
    Wait,
    PressKey,
    Select,
    Hover,
    Evaluate,
    Console,
    Network,
    Dialog,
    Upload,
    Tabs,
    Request,
    Complete,
    Other,
}

const SERVER: &str = "keel_browser";

fn norm(s: &str) -> String {
    s.trim().to_ascii_lowercase().replace('-', "_")
}

/// `browser_click` for a browser tool's name, else `None`.
pub fn tool_base(name: &str) -> Option<String> {
    let raw = name.trim();
    let prefixed = |base: &str| {
        if base.starts_with("browser_") {
            base.to_string()
        } else {
            format!("browser_{base}")
        }
    };
    let (base, server) = keel_proto::ToolAsk::split_name(raw);
    let base = norm(&base);
    if let Some(server) = server {
        let server = norm(&server);
        // Another server's `browser_*` (Playwright MCP) reads the same way.
        return (server == SERVER || base.starts_with("browser_")).then(|| prefixed(&base));
    }
    if let Some(rest) = base.strip_prefix(SERVER) {
        let rest = rest.trim_start_matches(['_', ':', '/', '.']);
        if !rest.is_empty() {
            return Some(prefixed(rest));
        }
    }
    base.starts_with("browser_").then_some(base)
}

pub fn op_of(base: &str) -> BrowserOp {
    match norm(base).trim_start_matches("browser_") {
        "open_pane" | "open" | "show" | "show_pane" => BrowserOp::OpenPane,
        "navigate" | "goto" | "go_to" | "open_url" | "load" | "visit" => BrowserOp::Navigate,
        "new_tab" | "tab_new" | "open_tab" | "tabs_create" | "create_tab" => BrowserOp::NewTab,
        "back" | "go_back" | "navigate_back" | "history_back" => BrowserOp::Back,
        "forward" | "go_forward" | "navigate_forward" | "history_forward" => BrowserOp::Forward,
        "reload" | "refresh" => BrowserOp::Reload,
        "click" | "double_click" | "dblclick" | "tap" => BrowserOp::Click,
        "type" | "fill" | "fill_form" | "input" | "type_text" => BrowserOp::Type,
        "scroll" | "scroll_to" => BrowserOp::Scroll,
        "snapshot" | "read" | "read_page" | "get_text" | "find" | "extract" | "page_text" => {
            BrowserOp::Snapshot
        }
        "screenshot" | "take_screenshot" | "capture" => BrowserOp::Screenshot,
        "wait" | "wait_for" => BrowserOp::Wait,
        "press_key" | "press" | "key" | "keypress" => BrowserOp::PressKey,
        "select_option" | "select" => BrowserOp::Select,
        "hover" => BrowserOp::Hover,
        "evaluate" | "eval" | "run_code" | "run_code_unsafe" | "execute_script" => {
            BrowserOp::Evaluate
        }
        "console_messages" | "console" | "console_logs" => BrowserOp::Console,
        "network_requests" | "network_request" | "network" => BrowserOp::Network,
        "handle_dialog" | "dialog" | "accept_dialog" | "dismiss_dialog" => BrowserOp::Dialog,
        "upload" | "file_upload" | "upload_file" | "set_input_files" => BrowserOp::Upload,
        "tabs" | "list_tabs" | "tab_list" | "tab_select" | "tab_close" => BrowserOp::Tabs,
        "request" => BrowserOp::Request,
        "complete" => BrowserOp::Complete,
        _ => BrowserOp::Other,
    }
}

/// Each action its own icon (never the terminal).
pub fn icon(op: BrowserOp) -> Icon {
    match op {
        BrowserOp::OpenPane | BrowserOp::Navigate | BrowserOp::Other => Icon::Globe,
        BrowserOp::NewTab => Icon::TabPlus,
        BrowserOp::Back => Icon::Back,
        BrowserOp::Forward => Icon::Forward,
        BrowserOp::Reload => Icon::Reload,
        BrowserOp::Click => Icon::PointerClick,
        BrowserOp::Type | BrowserOp::PressKey => Icon::Keyboard,
        BrowserOp::Scroll => Icon::ArrowUpDown,
        BrowserOp::Snapshot => Icon::FileText,
        BrowserOp::Screenshot => Icon::Camera,
        BrowserOp::Wait => Icon::Timer,
        BrowserOp::Select => Icon::List,
        BrowserOp::Hover => Icon::Mouse,
        BrowserOp::Evaluate => Icon::Code,
        BrowserOp::Console => Icon::ScrollText,
        BrowserOp::Network => Icon::Network,
        BrowserOp::Dialog => Icon::Dialog,
        BrowserOp::Upload => Icon::Paperclip,
        BrowserOp::Tabs => Icon::AppWindow,
        BrowserOp::Request => Icon::Comment,
        BrowserOp::Complete => Icon::Check,
    }
}

const fn v(
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

/// The verb forms. Errors read "Couldn't {failed} {subject}".
pub fn verb(op: BrowserOp, args: &Args) -> Verb {
    match op {
        BrowserOp::OpenPane | BrowserOp::Navigate => {
            v("Open", "Opening", "Opened", "open", "opening")
        }
        BrowserOp::NewTab => v(
            "Open new tab",
            "Opening new tab",
            "Opened new tab",
            "open new tab",
            "opening new tab",
        ),
        BrowserOp::Back => v(
            "Go back",
            "Going back",
            "Went back",
            "go back",
            "going back",
        ),
        BrowserOp::Forward => v(
            "Go forward",
            "Going forward",
            "Went forward",
            "go forward",
            "going forward",
        ),
        BrowserOp::Reload => v("Reload", "Reloading", "Reloaded", "reload", "reloading"),
        BrowserOp::Click => v("Click", "Clicking", "Clicked", "click", "clicking"),
        BrowserOp::Type => v("Type", "Typing", "Typed", "type", "typing"),
        BrowserOp::Scroll => v("Scroll", "Scrolling", "Scrolled", "scroll", "scrolling"),
        BrowserOp::Snapshot => v(
            "Read page",
            "Reading page",
            "Read page",
            "read page",
            "reading page",
        ),
        BrowserOp::Screenshot if args.full_page => v(
            "Take full-page screenshot",
            "Taking full-page screenshot",
            "Took full-page screenshot",
            "take full-page screenshot",
            "taking full-page screenshot",
        ),
        BrowserOp::Screenshot => v(
            "Take screenshot",
            "Taking screenshot",
            "Took screenshot",
            "take screenshot",
            "taking screenshot",
        ),
        BrowserOp::Wait => v(
            "Wait for",
            "Waiting for",
            "Waited for",
            "wait for",
            "waiting for",
        ),
        BrowserOp::PressKey => v("Press", "Pressing", "Pressed", "press", "pressing"),
        BrowserOp::Select => v("Select", "Selecting", "Selected", "select", "selecting"),
        BrowserOp::Hover => v(
            "Hover over",
            "Hovering over",
            "Hovered over",
            "hover over",
            "hovering over",
        ),
        BrowserOp::Evaluate => v(
            "Run script",
            "Running script",
            "Ran script",
            "run script",
            "running script",
        ),
        BrowserOp::Console => v(
            "Read console",
            "Reading console",
            "Read console",
            "read console",
            "reading console",
        ),
        BrowserOp::Dialog if args.accept == Some(false) => v(
            "Dismiss dialog",
            "Dismissing dialog",
            "Dismissed dialog",
            "dismiss dialog",
            "dismissing dialog",
        ),
        BrowserOp::Dialog => v(
            "Answer dialog",
            "Answering dialog",
            "Answered dialog",
            "answer dialog",
            "answering dialog",
        ),
        BrowserOp::Upload => v("Upload", "Uploading", "Uploaded", "upload", "uploading"),
        BrowserOp::Network => v(
            "Read network requests",
            "Reading network requests",
            "Read network requests",
            "read network requests",
            "reading network requests",
        ),
        BrowserOp::Tabs => v(
            "Check tabs",
            "Checking tabs",
            "Checked tabs",
            "check tabs",
            "checking tabs",
        ),
        BrowserOp::Request => v(
            "Ask you",
            "Asking you",
            "Asked you",
            "ask you",
            "asking you",
        ),
        BrowserOp::Complete => v(
            "Finish up",
            "Finishing up",
            "Finished in browser",
            "finish up",
            "finishing up",
        ),
        BrowserOp::Other => v(
            "Use browser",
            "Using browser",
            "Used browser",
            "use browser",
            "using browser",
        ),
    }
}

/// The call's arguments, from the row's `detail`: the JSON the harness
/// stores for browser tools, or (older rows) a bare URL.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Args {
    pub url: Option<String>,
    pub selector: Option<String>,
    pub text: Option<String>,
    pub key: Option<String>,
    pub value: Option<String>,
    pub to: Option<String>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub dx: Option<f64>,
    pub dy: Option<f64>,
    pub full_page: bool,
    /// A snapshot ref (`e12`) the call targeted.
    pub target: Option<String>,
    /// Upload file names, joined.
    pub paths: Option<String>,
    pub accept: Option<bool>,
    pub level: Option<String>,
}

pub fn args_of(detail: &str) -> Args {
    let detail = detail.trim();
    if detail.is_empty() {
        return Args::default();
    }
    if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(detail) {
        let s = |k: &str| {
            m.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        };
        let n = |k: &str| m.get(k).and_then(Value::as_f64);
        return Args {
            url: s("url"),
            selector: s("selector"),
            text: s("text"),
            key: s("key"),
            value: s("value"),
            to: s("to"),
            x: n("x"),
            y: n("y"),
            dx: n("dx"),
            dy: n("dy"),
            full_page: m.get("full_page").and_then(Value::as_bool).unwrap_or(false),
            target: s("ref"),
            paths: s("paths"),
            accept: m.get("accept").and_then(Value::as_bool),
            level: s("level"),
        };
    }
    if detail.contains("://") || detail.starts_with("about:") {
        Args {
            url: Some(detail.to_string()),
            ..Args::default()
        }
    } else {
        Args {
            selector: Some(detail.to_string()),
            ..Args::default()
        }
    }
}

/// What the tool returned, read from its text (whole JSON, or the tail of
/// a long one).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub url: Option<String>,
    pub title: Option<String>,
    pub text: Option<String>,
    pub saved: Option<String>,
    /// A failure's message (the output when it isn't JSON).
    pub error: Option<String>,
}

pub fn outcome_of(output: &str, failed: bool) -> Outcome {
    let text = output.trim();
    // Current shim: the page text raw, then one JSON line of fields.
    if let Some(out) = footer_outcome(text) {
        return out;
    }
    let json = text
        .find('{')
        .and_then(|start| serde_json::from_str::<Value>(&text[start..text.rfind('}')? + 1]).ok());
    let field = |k: &str| match &json {
        Some(v) => v.get(k).and_then(Value::as_str).map(str::to_string),
        None => json_field(text, k),
    };
    let mut out = Outcome {
        url: field("url").filter(|s| !s.is_empty()),
        title: field("title").filter(|s| !s.is_empty()),
        text: field("text").filter(|s| !s.is_empty()),
        saved: field("saved_path")
            .or_else(|| saved_lead(text))
            .filter(|s| !s.is_empty()),
        error: field("error").filter(|s| !s.is_empty()),
    };
    if failed && out.error.is_none() && out.url.is_none() && out.text.is_none() {
        let line = text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default()
            .trim();
        out.error = (!line.is_empty()).then(|| cut(line, 200));
    }
    out
}

/// `…page text…\n\n{"url":…,"title":…}`: the text before the last line,
/// and the fields from it.
fn footer_outcome(text: &str) -> Option<Outcome> {
    let (page, last) = match text.rsplit_once('\n') {
        Some((page, last)) => (page.trim(), last.trim()),
        None => ("", text),
    };
    if !last.starts_with('{') || !last.ends_with('}') {
        return None;
    }
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(last) else {
        return None;
    };
    if !(m.contains_key("url") || m.contains_key("title")) || m.contains_key("text") {
        return None;
    }
    let field = |k: &str| {
        m.get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    // A screenshot's lead line comes first: it is not page text.
    let saved = field("saved_path").or_else(|| saved_lead(page));
    let page = if page.starts_with("Saved to ") {
        page.split_once('\n')
            .map(|(_, rest)| rest.trim())
            .unwrap_or("")
    } else {
        page
    };
    Some(Outcome {
        url: field("url"),
        title: field("title"),
        text: (!page.is_empty()).then(|| page.to_string()),
        saved,
        error: field("error"),
    })
}

/// "Saved to /path.png — …", the screenshot lead line.
fn saved_lead(text: &str) -> Option<String> {
    let rest = text.strip_prefix("Saved to ")?;
    Some(rest.split([' ', '\n']).next()?.to_string())
}

/// The string value of `"key": "…"` anywhere in `text`, unescaped.
fn json_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut from = 0;
    while let Some(at) = text[from..].find(&needle) {
        let after = from + at + needle.len();
        let rest = text[after..].trim_start();
        if let Some(rest) = rest.strip_prefix(':') {
            let rest = rest.trim_start();
            if rest.starts_with('"') {
                let bytes = rest.as_bytes();
                let mut i = 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => return serde_json::from_str::<String>(&rest[..=i]).ok(),
                        _ => i += 1,
                    }
                }
                return None;
            }
        }
        from = after;
    }
    None
}

fn cut(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars()
            .take(n.saturating_sub(1))
            .collect::<String>()
            .trim_end()
            .to_string()
            + "…"
    }
}

/// `traveloka.com`, or `traveloka.com/en-id/flight` with the path.
pub fn site(url: &str, with_path: bool) -> String {
    let url = url.trim();
    let Some((_, rest)) = url.split_once("://") else {
        return cut(url, 48);
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let auth = &rest[..end];
    let host = auth.rsplit('@').next().unwrap_or(auth);
    let host = host.strip_prefix("www.").unwrap_or(host);
    if !with_path {
        return host.to_string();
    }
    let path = rest[end..]
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_matches('/');
    if path.is_empty() {
        host.to_string()
    } else {
        format!("{host}/{}", cut(path, 32))
    }
}

/// "clicked button#go \"Search\"" → `("button#go", Some("Search"))`.
fn element_of(result: &str, lead: &str) -> Option<(String, Option<String>)> {
    let rest = result.trim().strip_prefix(lead)?.trim_start();
    let (tag, label) = match rest.split_once(" \"") {
        Some((tag, tail)) => (
            tag,
            tail.rsplit_once('"').map(|(label, _)| label.to_string()),
        ),
        None => (rest.split([',', ' ']).next().unwrap_or(rest), None),
    };
    Some((
        tag.trim().to_string(),
        label.filter(|l| !l.trim().is_empty()),
    ))
}

/// A typed field as the row names it: never with its value.
fn typed_field(result: &str) -> Option<String> {
    let rest = result.split_once(" into ")?.1;
    // A ref target reads `textbox "Search" (e5)`: name it by its label.
    if let Some((_, tail)) = rest.split_once(" \"")
        && let Some((name, after)) = tail.split_once('"')
        && after.trim_start().starts_with("(e")
        && !name.trim().is_empty()
    {
        return Some(format!("\u{201c}{}\u{201d}", cut(name, 32)));
    }
    let tag = rest.split([' ', ',']).next()?.trim();
    (!tag.is_empty()).then(|| tag.to_string())
}

fn secret(args: &Args, result: &str) -> bool {
    args.selector.as_deref().is_some_and(keel_proto_secret)
        || result.contains("(password)")
        || args
            .text
            .as_deref()
            .is_some_and(|t| t.chars().all(|c| c == '•'))
}

/// Same test as the harness's `secret_field` (the UI does not depend on it).
fn keel_proto_secret(selector: &str) -> bool {
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

/// The value a type result quotes back (`into input#q "hello"`), removed.
pub fn redact_typed(text: &str) -> String {
    let Some(at) = text.find(" into ") else {
        return text.to_string();
    };
    let head = &text[..at + 6];
    let rest = &text[at + 6..];
    match rest.split_once(" \"") {
        Some((_, tail))
            if tail
                .split_once('"')
                .is_some_and(|(_, after)| after.trim_start().starts_with("(e")) =>
        {
            text.to_string()
        }
        Some((tag, tail)) => match tail.split_once('"') {
            Some((_, after)) => format!("{head}{tag}{after}"),
            None => format!("{head}{tag}"),
        },
        None => text.to_string(),
    }
}

/// What the line names: a site, an element, the typed text and field.
pub fn subject(op: BrowserOp, args: &Args, out: &Outcome) -> String {
    let url = out.url.as_deref().or(args.url.as_deref());
    // The outcome is the first line; a page diff may follow it.
    let result = out
        .text
        .as_deref()
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("");
    let short_sel = |s: &str| cut(s, 40);
    match op {
        BrowserOp::OpenPane => "browser".to_string(),
        BrowserOp::Navigate => args
            .url
            .as_deref()
            .or(url)
            .map(|u| site(out.url.as_deref().unwrap_or(u), true))
            .unwrap_or_else(|| "page".to_string()),
        BrowserOp::Console if args.level.as_deref().is_some_and(|l| l != "all") => {
            format!("{}s", args.level.as_deref().unwrap_or_default())
        }
        BrowserOp::NewTab
        | BrowserOp::Back
        | BrowserOp::Forward
        | BrowserOp::Snapshot
        | BrowserOp::Console
        | BrowserOp::Network => url.map(|u| site(u, false)).unwrap_or_default(),
        BrowserOp::Reload => url
            .map(|u| site(u, false))
            .unwrap_or_else(|| "page".to_string()),
        BrowserOp::Click | BrowserOp::Hover => {
            let lead = if op == BrowserOp::Click {
                "clicked "
            } else {
                "hovered "
            };
            match element_of(result, lead) {
                Some((_, Some(label))) => format!("\u{201c}{}\u{201d}", cut(&label, 40)),
                Some((tag, None)) if tag.contains(['#', '.']) => tag,
                _ => match (&args.selector, args.x, args.y) {
                    _ if args.target.is_some() && args.selector.is_none() => {
                        args.target.clone().unwrap_or_default()
                    }
                    (Some(sel), _, _) => short_sel(sel),
                    (None, Some(x), Some(y)) => format!("at {}, {}", x.round(), y.round()),
                    _ => match element_of(result, lead) {
                        Some((tag, _)) if !tag.is_empty() => tag,
                        _ => String::new(),
                    },
                },
            }
        }
        BrowserOp::Type => {
            let field = typed_field(result)
                .or_else(|| args.selector.as_deref().map(short_sel))
                .or_else(|| args.target.clone());
            let what = match &args.text {
                Some(_) if secret(args, result) => {
                    Some("\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}".to_string())
                }
                Some(text) => Some(format!("\u{201c}{}\u{201d}", cut(text, 24))),
                None => result
                    .strip_prefix("typed ")
                    .and_then(|r| r.split(" chars").next())
                    .and_then(|n| n.parse::<usize>().ok())
                    .map(|n| format!("{n} character{}", if n == 1 { "" } else { "s" })),
            };
            match (what, field) {
                (Some(what), Some(field)) => format!("{what} in {field}"),
                (Some(what), None) => what,
                (None, Some(field)) => format!("in {field}"),
                (None, None) => String::new(),
            }
        }
        BrowserOp::Scroll => {
            let dir = match (&args.to, args.dy, args.dx) {
                (Some(to), _, _) => format!("to {to}"),
                (None, Some(dy), _) if dy > 0. => "down".to_string(),
                (None, Some(dy), _) if dy < 0. => "up".to_string(),
                (None, _, Some(dx)) if dx > 0. => "right".to_string(),
                (None, _, Some(dx)) if dx < 0. => "left".to_string(),
                _ => "page".to_string(),
            };
            match &args.selector {
                Some(sel) => format!("{dir} in {}", short_sel(sel)),
                None => dir,
            }
        }
        BrowserOp::Wait => args
            .text
            .as_deref()
            .map(|t| format!("\u{201c}{}\u{201d}", cut(t, 32)))
            .or_else(|| args.selector.as_deref().map(short_sel))
            .or_else(|| args.target.clone())
            .or_else(|| args.url.as_deref().map(|u| site(u, true)))
            .unwrap_or_else(|| "page".to_string()),
        BrowserOp::PressKey => args
            .key
            .clone()
            .or_else(|| args.text.clone())
            .unwrap_or_default(),
        BrowserOp::Select => args
            .value
            .as_deref()
            .map(|v| format!("\u{201c}{}\u{201d}", cut(v, 32)))
            .unwrap_or_default(),
        BrowserOp::Upload => args
            .paths
            .as_deref()
            .map(|p| cut(p, 48))
            .unwrap_or_else(|| "file".to_string()),
        BrowserOp::Dialog => args
            .text
            .as_deref()
            .map(|t| format!("with \u{201c}{}\u{201d}", cut(t, 24)))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// The row's body: URL, title and a short result, plus the raw output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserBody {
    pub rows: Vec<(&'static str, String)>,
    pub raw: String,
    pub error: bool,
    /// The site the call was on, for a group's title.
    pub host: Option<String>,
}

pub fn body(op: BrowserOp, args: &Args, out: &Outcome, output: &str, error: bool) -> BrowserBody {
    let mut rows = Vec::new();
    let url = out.url.as_deref().or(args.url.as_deref());
    if let Some(url) = url {
        rows.push(("URL", cut(url, 160)));
    }
    if let Some(title) = &out.title {
        rows.push(("Title", cut(title, 120)));
    }
    if let Some(err) = &out.error {
        rows.push(("Error", cut(err, 240)));
    } else if let Some(text) = out.text.as_deref().filter(|t| !t.trim().is_empty()) {
        let text = if op == BrowserOp::Type {
            redact_typed(text)
        } else {
            text.to_string()
        };
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let result = match op {
            BrowserOp::Snapshot => {
                format!("{} ({} characters)", cut(&flat, 140), text.chars().count())
            }
            BrowserOp::Navigate if flat == "loaded" => "Loaded".to_string(),
            _ => {
                // The outcome line; the page diff after it stays in the raw view.
                let first = text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut c = first.chars();
                c.next().map_or_else(String::new, |f| {
                    f.to_uppercase().collect::<String>() + c.as_str()
                })
            }
        };
        rows.push(("Result", cut(&result, 200)));
    }
    if let Some(saved) = &out.saved {
        rows.push(("Saved", saved.clone()));
    }
    let inner = output.to_string();
    // A type result quotes the field's new value back; the raw view drops it too.
    let raw = match serde_json::from_str::<Value>(inner.trim()) {
        Ok(mut v) => {
            if op == BrowserOp::Type
                && let Some(text) = v.get("text").and_then(Value::as_str).map(redact_typed)
            {
                v["text"] = Value::String(text);
            }
            serde_json::to_string_pretty(&v).unwrap_or(inner)
        }
        Err(_) if op == BrowserOp::Type => redact_typed(&inner),
        Err(_) => inner,
    };
    BrowserBody {
        rows,
        raw,
        error,
        host: url.map(|u| site(u, false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_harness_name_is_recognised() {
        for (name, base) in [
            ("mcp__keel_browser__browser_navigate", "browser_navigate"),
            ("mcp__keel-browser__browser_click", "browser_click"),
            ("keel-browser:browser_type", "browser_type"),
            ("keel-browser_browser_open_pane", "browser_open_pane"),
            ("keel-browser__browser_scroll", "browser_scroll"),
            (
                "browser_screenshot (keel-browser MCP Server)",
                "browser_screenshot",
            ),
            ("mcp__keel-browser__navigate", "browser_navigate"),
            ("browser_back", "browser_back"),
        ] {
            assert_eq!(tool_base(name).as_deref(), Some(base), "{name}");
        }
        assert_eq!(tool_base("mcp__github__create_issue"), None);
        assert_eq!(tool_base("Bash"), None);
    }

    #[test]
    fn subjects_name_what_was_acted_on() {
        let ok = |text: &str| Outcome {
            url: Some("https://www.traveloka.com/en-id".into()),
            text: Some(text.into()),
            ..Outcome::default()
        };
        let click = ok("clicked button#search \"Search flights\"");
        assert_eq!(
            subject(
                BrowserOp::Click,
                &args_of(r##"{"selector":"#search"}"##),
                &click
            ),
            "\u{201c}Search flights\u{201d}"
        );
        assert_eq!(
            subject(
                BrowserOp::Click,
                &args_of(r#"{"x":640,"y":400}"#),
                &ok("clicked div")
            ),
            "at 640, 400"
        );
        let typed = ok("typed 7 chars into input#q \"jakarta\", pressed Enter");
        assert_eq!(
            subject(
                BrowserOp::Type,
                &args_of(r##"{"text":"jakarta","selector":"#q"}"##),
                &typed
            ),
            "\u{201c}jakarta\u{201d} in input#q"
        );
        assert_eq!(
            subject(BrowserOp::Type, &Args::default(), &typed),
            "7 characters in input#q"
        );
        let pw = ok("typed 8 chars into input#pw (password)");
        let masked = subject(
            BrowserOp::Type,
            &args_of(r##"{"text":"hunter22","selector":"#pw"}"##),
            &pw,
        );
        assert!(!masked.contains("hunter"), "{masked}");
        assert!(
            subject(
                BrowserOp::Type,
                &args_of(r#"{"text":"x","selector":"input[type=password]"}"#),
                &ok("typed")
            )
            .starts_with('\u{2022}')
        );
        assert_eq!(
            redact_typed("typed 7 chars into input#q \"jakarta\", pressed Enter"),
            "typed 7 chars into input#q, pressed Enter"
        );
        assert_eq!(
            subject(
                BrowserOp::Scroll,
                &args_of(r#"{"dy":600}"#),
                &Outcome::default()
            ),
            "down"
        );
        assert_eq!(
            subject(
                BrowserOp::Scroll,
                &args_of(r#"{"to":"bottom"}"#),
                &Outcome::default()
            ),
            "to bottom"
        );
        assert_eq!(
            subject(BrowserOp::Snapshot, &Args::default(), &ok("Flights Hotels")),
            "traveloka.com"
        );
        assert_eq!(
            subject(
                BrowserOp::Navigate,
                &args_of("https://www.resonafinance.co.id/id/about-us/company-profile"),
                &Outcome::default()
            ),
            "resonafinance.co.id/id/about-us/company-profile"
        );
        assert_eq!(
            subject(
                BrowserOp::Navigate,
                &args_of(r#"{"url":"https://traveloka.com"}"#),
                &Outcome::default()
            ),
            "traveloka.com"
        );
    }

    #[test]
    fn the_raw_text_reply_reads_like_the_json_one() {
        let out = "clicked button \"Search\" (e12)\nThe page navigated to https://shop.test/results.\n- heading \"Results\" [ref=e40]\n\n{\"url\":\"https://shop.test/results\",\"title\":\"Results\",\"open\":false}";
        let o = outcome_of(out, false);
        assert_eq!(o.url.as_deref(), Some("https://shop.test/results"));
        assert_eq!(o.title.as_deref(), Some("Results"));
        assert!(
            o.text
                .as_deref()
                .unwrap()
                .starts_with("clicked button \"Search\" (e12)")
        );
        assert_eq!(
            subject(BrowserOp::Click, &args_of(r#"{"ref":"e12"}"#), &o),
            "\u{201c}Search\u{201d}"
        );
        // Only the tail kept: the fields still come through.
        let o = outcome_of(&out[60..], false);
        assert_eq!(o.url.as_deref(), Some("https://shop.test/results"));
        let shot = "Saved to /d/s/1.png — the screenshot is shown to the user\n{\"url\":\"https://a.b\",\"title\":\"A\",\"saved_path\":\"/d/s/1.png\"}";
        let o = outcome_of(shot, false);
        assert_eq!(o.saved.as_deref(), Some("/d/s/1.png"));
        assert_eq!(o.text, None);
        // The old pretty-JSON shape still parses.
        let old = "{\n  \"text\": \"loaded\",\n  \"url\": \"https://a.b\"\n}";
        assert_eq!(outcome_of(old, false).text.as_deref(), Some("loaded"));
    }

    #[test]
    fn a_long_snapshot_keeps_url_and_title_from_its_tail() {
        let tail = "s, Hotels\",\n  \"title\": \"Traveloka\",\n  \"url\": \"https://www.traveloka.com/en-id\"\n}";
        let o = outcome_of(tail, false);
        assert_eq!(o.url.as_deref(), Some("https://www.traveloka.com/en-id"));
        assert_eq!(o.title.as_deref(), Some("Traveloka"));
    }

    #[test]
    fn every_op_has_its_own_icon() {
        use BrowserOp::*;
        let all = [
            OpenPane, Navigate, NewTab, Back, Forward, Reload, Click, Type, Scroll, Snapshot,
            Screenshot, Wait, PressKey, Select, Hover, Evaluate, Console, Network, Dialog, Upload,
            Tabs, Request, Complete, Other,
        ];
        for op in all {
            assert_ne!(icon(op), Icon::Terminal);
            assert_ne!(icon(op), Icon::Tool);
        }
        assert_eq!(icon(op_of("browser_click")), Icon::PointerClick);
        assert_eq!(icon(op_of("browser_type")), Icon::Keyboard);
        assert_eq!(icon(op_of("browser_scroll")), Icon::ArrowUpDown);
        assert_eq!(icon(op_of("browser_screenshot")), Icon::Camera);
        assert_eq!(icon(op_of("browser_new_tab")), Icon::TabPlus);
        assert_eq!(icon(op_of("browser_upload")), Icon::Paperclip);
        assert_eq!(icon(op_of("browser_handle_dialog")), Icon::Dialog);
        assert_eq!(icon(op_of("browser_wait_for")), Icon::Timer);
        assert_eq!(icon(op_of("browser_select_option")), Icon::List);
        assert_eq!(icon(op_of("browser_press_key")), Icon::Keyboard);
        assert_eq!(icon(op_of("browser_console")), Icon::ScrollText);
        // Every action this server offers has an icon of its own, apart from
        // read/scroll pairs that share nothing with the generic globe.
        for name in [
            "browser_click",
            "browser_type",
            "browser_select_option",
            "browser_scroll",
            "browser_wait_for",
            "browser_handle_dialog",
            "browser_upload",
            "browser_console",
            "browser_screenshot",
            "browser_snapshot",
        ] {
            assert_ne!(icon(op_of(name)), Icon::Globe, "{name}");
        }
    }

    #[test]
    fn the_new_tools_read_as_plain_lines() {
        let at = |text: &str| Outcome {
            url: Some("https://shop.test/cart".into()),
            text: Some(text.into()),
            ..Outcome::default()
        };
        let line = |name: &str, detail: &str, out: &Outcome| {
            let op = op_of(name);
            let args = args_of(detail);
            (verb(op, &args).past, subject(op, &args, out))
        };
        assert_eq!(
            line(
                "browser_click",
                r#"{"ref":"e12"}"#,
                &at("clicked button \"Send\" (e12)")
            ),
            ("Clicked", "\u{201c}Send\u{201d}".to_string())
        );
        assert_eq!(
            line(
                "browser_click",
                r#"{"ref":"e12"}"#,
                &at("clicked div (e12)")
            ),
            ("Clicked", "e12".to_string())
        );
        assert_eq!(
            line(
                "browser_type",
                r#"{"ref":"e5","text":"jakarta"}"#,
                &at("typed 7 chars into textbox \"Search\" (e5)")
            ),
            (
                "Typed",
                "\u{201c}jakarta\u{201d} in \u{201c}Search\u{201d}".to_string()
            )
        );
        let pw = line(
            "browser_type",
            r#"{"ref":"e6","text":"hunter22"}"#,
            &at("typed 8 chars into textbox \"Password\" (e6) (password)"),
        );
        assert!(!pw.1.contains("hunter"), "{pw:?}");
        assert_eq!(
            line(
                "browser_select_option",
                r#"{"ref":"e7","value":"Business"}"#,
                &at("selected \"Business\" in combobox \"Class\" (e7)")
            ),
            ("Selected", "\u{201c}Business\u{201d}".to_string())
        );
        assert_eq!(
            line(
                "browser_press_key",
                r#"{"key":"Enter"}"#,
                &at("pressed Enter")
            ),
            ("Pressed", "Enter".to_string())
        );
        assert_eq!(
            line(
                "browser_wait_for",
                r#"{"text":"Saved"}"#,
                &at("waited 0.4s")
            ),
            ("Waited for", "\u{201c}Saved\u{201d}".to_string())
        );
        assert_eq!(
            line(
                "browser_wait_for",
                r#"{"url":"https://shop.test/done"}"#,
                &at("waited")
            )
            .1,
            "shop.test/done"
        );
        assert_eq!(
            line(
                "browser_upload",
                r#"{"ref":"e9","paths":"a.png, b.pdf"}"#,
                &at("uploaded 2 files")
            ),
            ("Uploaded", "a.png, b.pdf".to_string())
        );
        assert_eq!(
            line(
                "browser_handle_dialog",
                r#"{"accept":false}"#,
                &at("dismissed")
            )
            .0,
            "Dismissed dialog"
        );
        assert_eq!(
            line(
                "browser_handle_dialog",
                r#"{"accept":true,"text":"Ana"}"#,
                &at("accepted")
            ),
            ("Answered dialog", "with \u{201c}Ana\u{201d}".to_string())
        );
        assert_eq!(
            line("browser_console", r#"{"level":"error"}"#, &at("")),
            ("Read console", "errors".to_string())
        );
        // A ref label survives the typed-value redaction.
        assert_eq!(
            redact_typed("typed 7 chars into textbox \"Search\" (e5)"),
            "typed 7 chars into textbox \"Search\" (e5)"
        );
    }
}

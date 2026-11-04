//! The approval card's logic, apart from gpui: titles, argument order, the
//! buttons a permission request gets, keys, receipt text, and the card's
//! lifecycle timing. `view/question.rs` draws what this decides.

use std::time::Duration;

use keel_proto::{Decision, OptionRole, Receipt, ToolAsk, UserInputQuestion};

// ── Titles ──────────────────────────────────────────────────────────────

/// A tool name as a short human title: a table for the tools Keel knows
/// (its browser pane, shells, file edits), else the name split on `_` in
/// sentence case.
pub fn humanize_title(name: &str) -> String {
    let (base, _) = ToolAsk::split_name(name);
    let key = base.to_lowercase();
    let known = match key.as_str() {
        "navigate" | "browser_navigate" => "Navigate browser",
        "snapshot" | "browser_snapshot" => "Read page",
        "click" | "browser_click" => "Click in browser",
        "type" | "browser_type" => "Type in browser",
        "scroll" | "browser_scroll" => "Scroll page",
        "screenshot" | "browser_screenshot" | "take_screenshot" => "Take screenshot",
        "open_pane" | "browser_open_pane" => "Open browser pane",
        "bash" | "run_command" | "shell" | "run_shell_command" | "execute" | "exec_command"
        | "run_terminal_cmd" => "Run command",
        "write" | "write_file" | "edit" | "edit_file" | "multiedit" | "multi_edit"
        | "str_replace" | "apply_patch" | "notebookedit" | "replace" | "create_file" => "Edit file",
        "read" | "read_file" | "view" | "view_file" => "Read file",
        "fetch" | "webfetch" | "web_fetch" | "fetch_url" | "read_url_content" => "Fetch URL",
        "websearch" | "web_search" | "google_web_search" => "Search the web",
        "delete_file" => "Delete file",
        "move_file" => "Move file",
        "search" | "grep" | "glob" | "find" => "Search files",
        "request_permissions" => "Grant permissions",
        "tool" | "" => "Use a tool",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    sentence_case(&base)
}

fn sentence_case(raw: &str) -> String {
    let words: Vec<String> = raw
        .split(['_', '-', '.', ' '])
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let joined = words.join(" ");
    let mut chars = joined.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Use a tool".to_string(),
    }
}

/// The icon tile's glyph for a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Browser,
    Command,
    File,
    Image,
    Web,
    Tool,
}

pub fn glyph(tool: &ToolAsk) -> Glyph {
    let key = tool.name.to_lowercase();
    if tool.source.as_deref() == Some("keel-browser") || key.starts_with("browser_") {
        return if key.contains("screenshot") {
            Glyph::Image
        } else {
            Glyph::Browser
        };
    }
    match humanize_title(&tool.name).as_str() {
        "Run command" => Glyph::Command,
        "Edit file" | "Read file" | "Delete file" | "Move file" | "Search files" => Glyph::File,
        "Fetch URL" | "Search the web" => Glyph::Web,
        "Take screenshot" => Glyph::Image,
        _ => Glyph::Tool,
    }
}

/// The tool a permission question is about. Harnesses that send no tool
/// only say "Allow {tool}: {detail}?"; that is read back here.
pub fn tool_of(question: &UserInputQuestion) -> ToolAsk {
    if let Some(tool) = &question.tool {
        return tool.clone();
    }
    let prompt = question.prompt.trim();
    let Some(rest) = prompt.strip_prefix("Allow ") else {
        return ToolAsk {
            name: "tool".into(),
            source: None,
            description: Some(prompt.to_string()),
            args: Vec::new(),
        };
    };
    let rest = rest.strip_suffix('?').unwrap_or(rest);
    let (name, detail) = match rest.split_once(": ") {
        Some((name, detail)) => (name.trim(), Some(detail.trim())),
        None => (rest.trim(), None),
    };
    let (name, source) = ToolAsk::split_name(name);
    let args = detail
        .map(|d| vec![("input".to_string(), d.to_string())])
        .unwrap_or_default();
    ToolAsk {
        name,
        source,
        description: None,
        args,
    }
}

// ── Arguments ───────────────────────────────────────────────────────────

/// Keys that say what a call is about, most telling first.
const PRIMARY: &[&str] = &[
    "url",
    "command",
    "cmd",
    "file_path",
    "path",
    "filepath",
    "notebook_path",
    "query",
    "pattern",
    "input",
];

pub fn is_command_key(key: &str) -> bool {
    matches!(key, "command" | "cmd")
}

/// Index of the primary argument, if any.
pub fn primary_index(args: &[(String, String)]) -> Option<usize> {
    PRIMARY
        .iter()
        .find_map(|want| args.iter().position(|(k, _)| k.to_lowercase() == *want))
}

/// The arguments with the primary one first; the rest keep their order.
pub fn ordered_args(args: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = args.to_vec();
    if let Some(ix) = primary_index(args) {
        let primary = out.remove(ix);
        out.insert(0, primary);
    }
    out
}

/// Lines the arguments take at about `cols` characters per line.
pub fn arg_lines(args: &[(String, String)], cols: usize) -> usize {
    let cols = cols.max(10);
    args.iter()
        .map(|(_, v)| {
            v.lines()
                .map(|l| l.chars().count().div_ceil(cols).max(1))
                .sum::<usize>()
                .max(1)
        })
        .sum()
}

/// Lines of arguments a waiting card shows before "Show all".
pub const ARG_LINES: usize = 3;

/// The primary argument, short enough for a receipt: a URL without its
/// scheme, a command's first line, a path's tail.
pub fn short_arg(tool: &ToolAsk) -> Option<String> {
    let (key, value) = &tool.args[primary_index(&tool.args)?];
    let key = key.to_lowercase();
    let value = value.trim();
    let mut text = if key == "url" {
        let v = value.split_once("://").map_or(value, |(_, rest)| rest);
        let v = v.strip_prefix("www.").unwrap_or(v);
        v.trim_end_matches('/').to_string()
    } else {
        value.lines().next().unwrap_or("").trim().to_string()
    };
    const MAX: usize = 48;
    let count = text.chars().count();
    if count > MAX {
        text = if key.contains("path") {
            format!(
                "…{}",
                text.chars().skip(count - (MAX - 1)).collect::<String>()
            )
        } else {
            format!("{}…", text.chars().take(MAX - 1).collect::<String>())
        };
    }
    (!text.is_empty()).then_some(text)
}

// ── Buttons ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    /// Allow: the filled one.
    Primary,
    /// Allow always, and options the card does not know.
    Secondary,
    /// Deny: quiet until hovered, then red.
    Ghost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardButton {
    /// Index into the question's options.
    pub option: usize,
    pub label: String,
    pub role: OptionRole,
    pub style: ButtonStyle,
    /// '1'..'9' by position, left to right.
    pub key: Option<char>,
}

fn rank(role: OptionRole) -> u8 {
    match role {
        OptionRole::AllowOnce => 0,
        OptionRole::AllowAlways => 1,
        OptionRole::RejectOnce => 2,
        OptionRole::RejectAlways => 3,
        OptionRole::Choice => 4,
    }
}

/// A permission request's buttons: Allow, Allow always, Deny (by role, as
/// the harness tagged them, never by position), then anything else it
/// offered under its own label.
pub fn permission_buttons(question: &UserInputQuestion) -> Vec<CardButton> {
    let mut order: Vec<usize> = (0..question.options.len()).collect();
    order.sort_by_key(|&ix| (rank(question.options[ix].role), ix));
    let count = |role: OptionRole| question.options.iter().filter(|o| o.role == role).count();
    order
        .into_iter()
        .enumerate()
        .map(|(pos, ix)| {
            let option = &question.options[ix];
            let ours = match option.role {
                OptionRole::AllowOnce => Some("Allow"),
                OptionRole::AllowAlways => Some("Allow always"),
                OptionRole::RejectOnce => Some("Deny"),
                OptionRole::RejectAlways => Some("Always deny"),
                OptionRole::Choice => None,
            };
            let label = match ours {
                Some(label) if count(option.role) == 1 => label.to_string(),
                _ => option.label.clone(),
            };
            let style = match option.role {
                OptionRole::AllowOnce => ButtonStyle::Primary,
                OptionRole::RejectOnce | OptionRole::RejectAlways => ButtonStyle::Ghost,
                _ => ButtonStyle::Secondary,
            };
            let key = (pos < 9).then(|| char::from(b'1' + pos as u8));
            CardButton {
                option: ix,
                label,
                role: option.role,
                style,
                key,
            }
        })
        .collect()
}

/// The button Enter starts on: Allow, else the first.
pub fn default_focus(buttons: &[CardButton]) -> usize {
    buttons
        .iter()
        .position(|b| b.role == OptionRole::AllowOnce)
        .unwrap_or(0)
}

// ── Keys ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Digit(u8),
    Enter,
    Esc,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Space,
    Char(char),
}

/// A gpui keystroke key (`"1"`, `"enter"`, `"tab"`, ...).
pub fn parse_key(key: &str, shift: bool) -> Option<Key> {
    Some(match key {
        "enter" | "return" => Key::Enter,
        "escape" => Key::Esc,
        "tab" if shift => Key::BackTab,
        "tab" => Key::Tab,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "space" => Key::Space,
        k if k.len() == 1 => {
            let ch = k.chars().next()?;
            match ch.to_digit(10) {
                Some(d) if d >= 1 => Key::Digit(d as u8),
                Some(_) => return None,
                None => Key::Char(ch.to_ascii_lowercase()),
            }
        }
        _ => return None,
    })
}

/// What a key does to the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKey {
    /// Answer with this button (permission) or row (question).
    Pick(usize),
    /// Move the focus ring here.
    Focus(usize),
    /// Multi-select: check or uncheck this row.
    Toggle(usize),
    /// Multi-select: send what is checked.
    Submit,
    /// Open the "Other…" field.
    OpenOther,
    /// Dismiss an optional question.
    Skip,
}

fn step(focus: usize, len: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward {
        (focus + 1) % len
    } else {
        (focus + len - 1) % len
    }
}

/// Keys on a permission card: 1/2/3… pick left to right, Enter picks the
/// focused button (Allow until moved), Cmd+Enter is Allow always, Esc is
/// Deny, Tab and arrows move the focus.
pub fn permission_key(
    buttons: &[CardButton],
    focus: usize,
    key: Key,
    cmd: bool,
) -> Option<CardKey> {
    let by_role = |role: OptionRole| buttons.iter().position(|b| b.role == role);
    match key {
        Key::Digit(d) => {
            let ix = d as usize - 1;
            (ix < buttons.len()).then_some(CardKey::Pick(ix))
        }
        Key::Enter if cmd => by_role(OptionRole::AllowAlways).map(CardKey::Pick),
        Key::Enter | Key::Space => (focus < buttons.len()).then_some(CardKey::Pick(focus)),
        Key::Esc => by_role(OptionRole::RejectOnce)
            .or_else(|| by_role(OptionRole::RejectAlways))
            .map(CardKey::Pick),
        Key::Tab | Key::Right | Key::Down => Some(CardKey::Focus(step(focus, buttons.len(), true))),
        Key::BackTab | Key::Left | Key::Up => {
            Some(CardKey::Focus(step(focus, buttons.len(), false)))
        }
        Key::Char(_) => None,
    }
}

/// Rows a question card lists: its options, then "Other…" when it takes text.
pub fn question_rows(question: &UserInputQuestion) -> usize {
    question.options.len() + usize::from(question.other)
}

/// Keys on a question card: digits pick (multi-select: toggle), O opens
/// "Other…", arrows and Tab move, Enter chooses the focused row (multi:
/// submits), Space toggles, Esc skips an optional question.
pub fn question_key(
    question: &UserInputQuestion,
    focus: usize,
    checked: usize,
    key: Key,
    cmd: bool,
) -> Option<CardKey> {
    let options = question.options.len();
    let rows = question_rows(question);
    let on_other = question.other && focus == options;
    match key {
        Key::Digit(d) => {
            let ix = d as usize - 1;
            (ix < options).then_some(if question.multi {
                CardKey::Toggle(ix)
            } else {
                CardKey::Pick(ix)
            })
        }
        Key::Char('o') if question.other => Some(CardKey::OpenOther),
        Key::Char(_) => None,
        Key::Tab | Key::Down | Key::Right => {
            (rows > 0).then(|| CardKey::Focus(step(focus, rows, true)))
        }
        Key::BackTab | Key::Up | Key::Left => {
            (rows > 0).then(|| CardKey::Focus(step(focus, rows, false)))
        }
        Key::Space if question.multi && focus < options => Some(CardKey::Toggle(focus)),
        Key::Space | Key::Enter if on_other => Some(CardKey::OpenOther),
        Key::Enter if question.multi || cmd => {
            (question.multi && checked > 0).then_some(CardKey::Submit)
        }
        Key::Enter => (focus < options).then_some(CardKey::Pick(focus)),
        Key::Space => None,
        Key::Esc => (!question.required).then_some(CardKey::Skip),
    }
}

// ── Receipts ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// ✓ in green.
    Check,
    /// ✕ in red.
    Cross,
    /// ■ for a stopped run.
    Stop,
    /// Nothing decided.
    None,
}

/// A receipt as the transcript shows it: `✓ Allowed · Navigate browser · example.com`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptLine {
    pub mark: Mark,
    pub verb: String,
    pub title: Option<String>,
    /// Monospace tail (the primary argument).
    pub arg: Option<String>,
}

impl ReceiptLine {
    pub fn text(&self) -> String {
        let mark = match self.mark {
            Mark::Check => "✓ ",
            Mark::Cross => "✕ ",
            Mark::Stop => "■ ",
            Mark::None => "",
        };
        let mut out = format!("{mark}{}", self.verb);
        for part in [&self.title, &self.arg].into_iter().flatten() {
            out.push_str(" · ");
            out.push_str(part);
        }
        out
    }
}

fn one_line(text: &str, max: usize) -> String {
    let line = text
        .split('\n')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max - 1).collect::<String>())
    } else {
        line
    }
}

pub fn receipt_line(receipt: &Receipt) -> ReceiptLine {
    let title = receipt
        .tool
        .as_ref()
        .map(|t| humanize_title(&t.name))
        .or_else(|| (!receipt.prompt.trim().is_empty()).then(|| one_line(&receipt.prompt, 60)));
    let arg = receipt.tool.as_ref().and_then(short_arg);
    let line = |mark, verb: &str, title: Option<String>, arg: Option<String>| ReceiptLine {
        mark,
        verb: verb.to_string(),
        title,
        arg,
    };
    match receipt.decision {
        Decision::Allowed => line(Mark::Check, "Allowed", title, arg),
        Decision::AlwaysAllowed => line(Mark::Check, "Always allowed", title, None),
        Decision::Denied => line(Mark::Cross, "Denied", title, None),
        Decision::Answered if receipt.tool.is_some() || receipt.choice.is_some() => {
            let verb = receipt
                .choice
                .clone()
                .or_else(|| receipt.answer.clone())
                .unwrap_or_else(|| "Answered".into());
            line(Mark::Check, &one_line(&verb, 40), title, None)
        }
        Decision::Answered => line(
            Mark::Check,
            "Answered",
            receipt.answer.as_deref().map(|a| one_line(a, 80)),
            None,
        ),
        Decision::Skipped => line(Mark::None, "Skipped question", None, None),
        Decision::Stopped => line(Mark::Stop, "Run stopped", None, None),
        Decision::Canceled => line(Mark::None, "Request canceled", None, None),
    }
}

// ── Lifecycle ───────────────────────────────────────────────────────────

/// Where a card is: waiting, leaving after an answer, leaving after a
/// stop or cancel, or folded into its receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    Answered,
    Canceled,
    Receipt,
}

/// A card leaving the composer slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// Stopped or canceled, rather than answered.
    pub canceled: bool,
    pub reduced: bool,
}

pub const ENTER_MS: f32 = 200.;
pub const EXIT_MS: f32 = 200.;
pub const CANCEL_MS: f32 = 150.;
pub const REDUCED_MS: f32 = 120.;
pub const RECEIPT_H: f32 = 28.;

impl Exit {
    pub fn duration(&self) -> Duration {
        let ms = if self.reduced {
            REDUCED_MS
        } else if self.canceled {
            CANCEL_MS
        } else {
            EXIT_MS
        };
        Duration::from_millis(ms as u64)
    }

    pub fn phase(&self, elapsed: Duration) -> Phase {
        if elapsed >= self.duration() {
            Phase::Receipt
        } else if self.canceled {
            Phase::Canceled
        } else {
            Phase::Answered
        }
    }
}

/// `cubic-bezier(x1, y1, x2, y2)` at `t`.
pub fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    if t <= 0. {
        return 0.;
    }
    if t >= 1. {
        return 1.;
    }
    let b = |a: f32, c: f32, s: f32| {
        3. * a * s * (1. - s).powi(2) + 3. * c * s * s * (1. - s) + s.powi(3)
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..20 {
        let mid = (lo + hi) / 2.;
        if b(x1, x2, mid) < t {
            lo = mid
        } else {
            hi = mid
        }
    }
    b(y1, y2, (lo + hi) / 2.)
}

/// Out-quint: `cubic-bezier(0.22, 1, 0.36, 1)`.
pub fn out_quint(t: f32) -> f32 {
    bezier(0.22, 1., 0.36, 1., t)
}

/// In-out: `cubic-bezier(0.65, 0, 0.35, 1)`.
pub fn in_out(t: f32) -> f32 {
    bezier(0.65, 0., 0.35, 1., t)
}

/// Linear progress of `ms` through `[start, start + len]`.
pub fn span(ms: f32, start: f32, len: f32) -> f32 {
    ((ms - start) / len).clamp(0., 1.)
}

/// An element's opacity and downward offset (px).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub opacity: f32,
    pub dy: f32,
}

/// The whole card entering: 0→1, 8 px rise over 200 ms (a 120 ms fade under
/// reduced motion).
pub fn card_enter(ms: f32, reduced: bool) -> Look {
    if reduced {
        return Look {
            opacity: span(ms, 0., REDUCED_MS),
            dy: 0.,
        };
    }
    let e = out_quint(span(ms, 0., ENTER_MS));
    Look {
        opacity: e,
        dy: 8. * (1. - e),
    }
}

/// One piece of the card entering after `delay` ms: fade and 4 px rise over
/// 120 ms. No stagger under reduced motion.
pub fn part_enter(ms: f32, delay: f32, reduced: bool) -> Look {
    if reduced {
        return Look {
            opacity: 1.,
            dy: 0.,
        };
    }
    let e = out_quint(span(ms, delay, 120.));
    Look {
        opacity: e,
        dy: 4. * (1. - e),
    }
}

/// The card leaving at `ms`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExitFrame {
    /// The chosen button shows its check.
    pub check: bool,
    /// Everything but the chosen button.
    pub others: f32,
    /// The chosen button.
    pub chosen: f32,
    /// Card contents' downward drift (stop/cancel).
    pub dy: f32,
    /// 0 = full card height, 1 = receipt height.
    pub collapse: f32,
    /// Background, border, shadow.
    pub chrome: f32,
    pub receipt: f32,
    pub receipt_dy: f32,
}

pub fn exit_frame(ms: f32, exit: Exit) -> ExitFrame {
    if exit.reduced {
        let x = span(ms, 0., REDUCED_MS);
        return ExitFrame {
            check: true,
            others: 1. - x,
            chosen: 1. - x,
            dy: 0.,
            collapse: 1.,
            chrome: 0.,
            receipt: x,
            receipt_dy: 0.,
        };
    }
    if exit.canceled {
        // 150 ms: fade, collapse, receipt, all ease-out.
        let fade = out_quint(span(ms, 0., 80.));
        let collapse = out_quint(span(ms, 40., 100.));
        let receipt = out_quint(span(ms, 70., 80.));
        return ExitFrame {
            check: false,
            others: 1. - fade,
            chosen: 1. - fade,
            dy: 4. * fade,
            collapse,
            chrome: 1. - collapse,
            receipt,
            receipt_dy: 2. * (1. - receipt),
        };
    }
    // 200 ms: a short beat on the check, then fade, collapse, receipt.
    let others = 1. - out_quint(span(ms, 30., 90.));
    let collapse = out_quint(span(ms, 60., 120.));
    let receipt = out_quint(span(ms, 100., 100.));
    ExitFrame {
        check: true,
        others,
        chosen: 1. - out_quint(span(ms, 80., 70.)),
        dy: 0.,
        collapse,
        chrome: 1. - collapse,
        receipt,
        receipt_dy: 2. * (1. - receipt),
    }
}

/// A sample card for `KEEL_TEST_CARD`: a keel-browser permission (ACP
/// option ids, in Antigravity's order), a question, or a multi-select.
pub fn sample_question(kind: &str) -> UserInputQuestion {
    use keel_proto::{QuestionKind, QuestionOption};
    match kind {
        "question" | "multi" => {
            let mut q = UserInputQuestion::new(
                "test-q",
                QuestionKind::Select,
                "Which layout should the settings page use?\nThe sidebar keeps sections visible; tabs save width.",
                vec![
                    QuestionOption::new("sidebar", "Sidebar (Recommended)")
                        .with_detail(Some("Sections stay visible while scrolling".into())),
                    QuestionOption::new("tabs", "Tabs")
                        .with_detail(Some("Compact, one section at a time".into())),
                    QuestionOption::new("single", "Single page"),
                ],
                false,
            );
            q.other = true;
            q.multi = kind == "multi";
            q
        }
        _ => {
            let mut q = UserInputQuestion::new(
                "test-p",
                QuestionKind::Select,
                "Allow navigate?",
                vec![
                    QuestionOption::new("proceed_always", "Allow always")
                        .with_role(OptionRole::AllowAlways),
                    QuestionOption::new("proceed_once", "Allow").with_role(OptionRole::AllowOnce),
                    QuestionOption::new("cancel", "Reject").with_role(OptionRole::RejectOnce),
                ],
                true,
            );
            q.tool = Some(ToolAsk {
                name: "navigate".into(),
                source: Some("keel-browser".into()),
                description: None,
                args: vec![
                    ("url".into(), "https://example.com".into()),
                    ("new_tab".into(), "false".into()),
                ],
            });
            q
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keel_proto::{QuestionKind, QuestionOption};

    fn ask(options: Vec<QuestionOption>) -> UserInputQuestion {
        UserInputQuestion::new("q", QuestionKind::Select, "Allow navigate?", options, true)
    }

    fn acp_options() -> Vec<QuestionOption> {
        // Antigravity / Gemini order: always first, reject last.
        vec![
            QuestionOption::new("proceed_always", "Allow All Edits")
                .with_role(OptionRole::AllowAlways),
            QuestionOption::new("proceed_once", "Allow").with_role(OptionRole::AllowOnce),
            QuestionOption::new("cancel", "Reject").with_role(OptionRole::RejectOnce),
        ]
    }

    #[test]
    fn titles_are_humanized() {
        assert_eq!(humanize_title("navigate"), "Navigate browser");
        assert_eq!(humanize_title("mcp__keel-browser__snapshot"), "Read page");
        assert_eq!(humanize_title("write_file"), "Edit file");
        assert_eq!(humanize_title("run_command"), "Run command");
        assert_eq!(humanize_title("Bash"), "Run command");
        assert_eq!(humanize_title("Read"), "Read file");
        assert_eq!(humanize_title("WebFetch"), "Fetch URL");
        assert_eq!(humanize_title("open_pane"), "Open browser pane");
        assert_eq!(humanize_title("mcp__linear__create_issue"), "Create issue");
        assert_eq!(humanize_title("list_merge_requests"), "List merge requests");
        assert_eq!(
            humanize_title("navigate (keel-browser MCP Server)"),
            "Navigate browser"
        );
    }

    #[test]
    fn primary_arg_leads() {
        let args = vec![
            ("timeout".to_string(), "5".to_string()),
            ("url".to_string(), "https://www.example.com/".to_string()),
        ];
        assert_eq!(ordered_args(&args)[0].0, "url");
        let tool = ToolAsk {
            name: "navigate".into(),
            source: None,
            description: None,
            args,
        };
        assert_eq!(short_arg(&tool).as_deref(), Some("example.com"));
        assert_eq!(arg_lines(&[("a".into(), "x".repeat(150))], 70), 3);
    }

    #[test]
    fn buttons_follow_roles_not_positions() {
        let q = ask(acp_options());
        let buttons = permission_buttons(&q);
        let labels: Vec<_> = buttons
            .iter()
            .map(|b| (b.label.as_str(), q.options[b.option].id.as_str(), b.key))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("Allow", "proceed_once", Some('1')),
                ("Allow always", "proceed_always", Some('2')),
                ("Deny", "cancel", Some('3'))
            ]
        );
        assert_eq!(buttons[0].style, ButtonStyle::Primary);
        assert_eq!(buttons[2].style, ButtonStyle::Ghost);
        assert_eq!(default_focus(&buttons), 0);
        // Codex: an option the card does not know goes last, under its own label.
        let q = ask(vec![
            QuestionOption::new("abort", "Deny and stop"),
            QuestionOption::new("denied", "Deny").with_role(OptionRole::RejectOnce),
            QuestionOption::new("approved", "Allow").with_role(OptionRole::AllowOnce),
        ]);
        let ids: Vec<_> = permission_buttons(&q)
            .iter()
            .map(|b| q.options[b.option].id.clone())
            .collect();
        assert_eq!(ids, vec!["approved", "denied", "abort"]);
        assert_eq!(permission_buttons(&q)[2].label, "Deny and stop");
    }

    #[test]
    fn permission_keys() {
        let buttons = permission_buttons(&ask(acp_options()));
        let k = |key, cmd| permission_key(&buttons, 0, key, cmd);
        assert_eq!(k(Key::Digit(1), false), Some(CardKey::Pick(0)));
        assert_eq!(k(Key::Digit(2), false), Some(CardKey::Pick(1)));
        assert_eq!(k(Key::Digit(3), false), Some(CardKey::Pick(2)));
        assert_eq!(k(Key::Digit(4), false), None);
        assert_eq!(k(Key::Enter, false), Some(CardKey::Pick(0)));
        assert_eq!(k(Key::Enter, true), Some(CardKey::Pick(1)));
        assert_eq!(k(Key::Esc, false), Some(CardKey::Pick(2)));
        assert_eq!(k(Key::Tab, false), Some(CardKey::Focus(1)));
        assert_eq!(k(Key::BackTab, false), Some(CardKey::Focus(2)));
        assert_eq!(
            permission_key(&buttons, 1, Key::Enter, false),
            Some(CardKey::Pick(1))
        );
        assert_eq!(parse_key("tab", true), Some(Key::BackTab));
        assert_eq!(parse_key("O", false), Some(Key::Char('o')));
        assert_eq!(parse_key("0", false), None);
        // Claude: no Allow always, so Cmd+Enter does nothing.
        let claude = permission_buttons(&ask(vec![
            QuestionOption::new("allow", "Allow").with_role(OptionRole::AllowOnce),
            QuestionOption::new("deny", "Deny").with_role(OptionRole::RejectOnce),
        ]));
        assert_eq!(permission_key(&claude, 0, Key::Enter, true), None);
        assert_eq!(
            permission_key(&claude, 0, Key::Esc, false),
            Some(CardKey::Pick(1))
        );
    }

    #[test]
    fn question_keys() {
        let mut q = UserInputQuestion::new(
            "q",
            QuestionKind::Select,
            "Color?",
            vec![
                QuestionOption::new("b", "Blue"),
                QuestionOption::new("g", "Green"),
            ],
            false,
        );
        q.other = true;
        assert_eq!(question_rows(&q), 3);
        assert_eq!(
            question_key(&q, 0, 0, Key::Digit(2), false),
            Some(CardKey::Pick(1))
        );
        assert_eq!(
            question_key(&q, 1, 0, Key::Enter, false),
            Some(CardKey::Pick(1))
        );
        assert_eq!(
            question_key(&q, 2, 0, Key::Enter, false),
            Some(CardKey::OpenOther)
        );
        assert_eq!(
            question_key(&q, 0, 0, Key::Char('o'), false),
            Some(CardKey::OpenOther)
        );
        assert_eq!(
            question_key(&q, 2, 0, Key::Down, false),
            Some(CardKey::Focus(0))
        );
        assert_eq!(question_key(&q, 0, 0, Key::Esc, false), Some(CardKey::Skip));
        q.multi = true;
        assert_eq!(
            question_key(&q, 0, 0, Key::Digit(1), false),
            Some(CardKey::Toggle(0))
        );
        assert_eq!(
            question_key(&q, 1, 0, Key::Space, false),
            Some(CardKey::Toggle(1))
        );
        assert_eq!(question_key(&q, 0, 0, Key::Enter, false), None);
        assert_eq!(
            question_key(&q, 0, 1, Key::Enter, false),
            Some(CardKey::Submit)
        );
        q.required = true;
        assert_eq!(question_key(&q, 0, 0, Key::Esc, false), None);
    }

    #[test]
    fn receipt_text() {
        let tool = ToolAsk {
            name: "navigate".into(),
            source: Some("keel-browser".into()),
            description: None,
            args: vec![("url".into(), "https://example.com".into())],
        };
        let r = |decision| Receipt {
            decision,
            tool: Some(tool.clone()),
            prompt: "Allow navigate?".into(),
            answer: None,
            choice: None,
        };
        assert_eq!(
            receipt_line(&r(Decision::Allowed)).text(),
            "✓ Allowed · Navigate browser · example.com"
        );
        assert_eq!(
            receipt_line(&r(Decision::AlwaysAllowed)).text(),
            "✓ Always allowed · Navigate browser"
        );
        assert_eq!(
            receipt_line(&r(Decision::Denied)).text(),
            "✕ Denied · Navigate browser"
        );
        assert_eq!(receipt_line(&r(Decision::Stopped)).text(), "■ Run stopped");
        assert_eq!(
            receipt_line(&r(Decision::Canceled)).text(),
            "Request canceled"
        );
        let q = Receipt {
            decision: Decision::Answered,
            tool: None,
            prompt: "Color?".into(),
            answer: Some("Blue\nGreen".into()),
            choice: None,
        };
        assert_eq!(receipt_line(&q).text(), "✓ Answered · Blue, Green");
        let skipped = Receipt {
            decision: Decision::Skipped,
            answer: None,
            ..q
        };
        assert_eq!(receipt_line(&skipped).text(), "Skipped question");
    }

    #[test]
    fn prompt_only_permissions_still_get_a_tool() {
        let q = ask(vec![]);
        let tool = tool_of(&UserInputQuestion {
            prompt: "Allow Bash: ls -la?".into(),
            ..q
        });
        assert_eq!(
            (tool.name.as_str(), tool.args[0].1.as_str()),
            ("Bash", "ls -la")
        );
    }

    #[test]
    fn lifecycle_waiting_answered_receipt_and_cancel() {
        let answered = Exit {
            canceled: false,
            reduced: false,
        };
        assert_eq!(answered.phase(Duration::ZERO), Phase::Answered);
        assert_eq!(answered.phase(Duration::from_millis(199)), Phase::Answered);
        assert_eq!(answered.phase(Duration::from_millis(200)), Phase::Receipt);
        let f = exit_frame(20., answered);
        assert!(f.check && f.others == 1. && f.collapse == 0. && f.receipt == 0.);
        let f = exit_frame(200., answered);
        assert!(f.others == 0. && (f.collapse - 1.).abs() < 1e-3 && (f.receipt - 1.).abs() < 1e-3);

        let canceled = Exit {
            canceled: true,
            reduced: false,
        };
        assert_eq!(canceled.phase(Duration::from_millis(100)), Phase::Canceled);
        assert_eq!(canceled.phase(Duration::from_millis(150)), Phase::Receipt);
        assert!(!exit_frame(10., canceled).check);

        let reduced = Exit {
            canceled: false,
            reduced: true,
        };
        assert_eq!(reduced.phase(Duration::from_millis(120)), Phase::Receipt);
        let f = exit_frame(60., reduced);
        assert_eq!((f.collapse, f.dy, f.receipt_dy), (1., 0., 0.));
        assert_eq!(card_enter(60., true).dy, 0.);
        assert_eq!(part_enter(0., 80., true).opacity, 1.);
        assert!(part_enter(80., 80., false).opacity < 0.01);
    }
}

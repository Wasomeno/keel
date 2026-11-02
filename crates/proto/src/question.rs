//! Questions an agent asks the user mid-turn, and the receipt a settled one
//! leaves in the transcript.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    Select,
    Confirm,
    Input,
    Editor,
}

/// What a permission option does, so a client labels and keys it by meaning,
/// never by its position in the list (harnesses order them differently).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionRole {
    /// An ordinary answer to a question.
    #[default]
    Choice,
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

impl OptionRole {
    /// ACP `PermissionOption.kind`.
    pub fn from_acp_kind(kind: &str) -> Self {
        match kind {
            "allow_once" => Self::AllowOnce,
            "allow_always" => Self::AllowAlways,
            "reject_once" => Self::RejectOnce,
            "reject_always" => Self::RejectAlways,
            _ => Self::Choice,
        }
    }

    /// From an option's id and label, for servers that send no kind.
    pub fn guess(id: &str, label: &str) -> Self {
        let text = format!("{id} {label}").to_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| text.split(|c: char| !c.is_alphanumeric()).any(|t| t == *w));
        let always = has(&["always", "session", "forever", "all"]);
        if has(&["allow", "approve", "approved", "accept", "yes", "proceed", "allowed"]) {
            if always { Self::AllowAlways } else { Self::AllowOnce }
        } else if has(&["deny", "reject", "decline", "denied", "no", "abort", "cancel"]) {
            if always { Self::RejectAlways } else { Self::RejectOnce }
        } else {
            Self::Choice
        }
    }

    pub fn is_choice(&self) -> bool {
        *self == Self::Choice
    }

    pub fn is_allow(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }

    pub fn is_reject(self) -> bool {
        matches!(self, Self::RejectOnce | Self::RejectAlways)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "OptionRole::is_choice")]
    pub role: OptionRole,
    /// A second line under the label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl QuestionOption {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), role: OptionRole::Choice, detail: None }
    }

    pub fn with_role(mut self, role: OptionRole) -> Self {
        self.role = role;
        self
    }

    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail.filter(|d| !d.trim().is_empty());
        self
    }
}

/// The tool call a permission question is about.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolAsk {
    /// The tool's own name (`navigate`, `bash`, `mcp__server__tool`, ...).
    pub name: String,
    /// The server or harness it comes from (`keel-browser`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// What the harness says the call does, when it is more than the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Its arguments, flattened to text, in the order given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<(String, String)>,
}

impl ToolAsk {
    /// A tool call from its raw name (`mcp__keel-browser__navigate`,
    /// `navigate (keel-browser MCP Server)`, `Bash`) and JSON input.
    pub fn from_input(raw_name: &str, source: Option<&str>, input: &serde_json::Value) -> Self {
        let (name, parsed) = Self::split_name(raw_name);
        Self { name, source: source.map(str::to_string).or(parsed), description: None, args: flatten_args(input) }
    }

    /// Tool name and server, from the forms harnesses use.
    pub fn split_name(raw: &str) -> (String, Option<String>) {
        let raw = raw.trim();
        if let Some(rest) = raw.strip_prefix("mcp__") {
            if let Some((server, tool)) = rest.split_once("__") {
                return (tool.to_string(), Some(server.to_string()));
            }
        }
        if let Some((tool, tail)) = raw.split_once(" (") {
            if let Some(server) = tail.strip_suffix(')') {
                let server = server.trim_end_matches(" MCP Server").trim_end_matches(" MCP").trim();
                if !tool.contains(char::is_whitespace) && !server.is_empty() {
                    return (tool.to_string(), Some(server.to_string()));
                }
            }
        }
        (raw.to_string(), None)
    }
}

/// Top-level fields of a tool's input as text: strings verbatim, the rest as
/// compact JSON. Long values are cut so a card never holds megabytes.
pub fn flatten_args(input: &serde_json::Value) -> Vec<(String, String)> {
    const CAP: usize = 4000;
    let text = |value: &serde_json::Value| {
        let mut s = match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if s.len() > CAP {
            let mut end = CAP;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            s.push('…');
        }
        s
    };
    match input {
        serde_json::Value::Object(map) => map
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| (k.clone(), text(v)))
            .collect(),
        serde_json::Value::Null => Vec::new(),
        other => vec![("input".to_string(), text(other))],
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInputQuestion {
    pub id: String,
    pub kind: QuestionKind,
    pub prompt: String,
    /// Empty for `Input` and `Editor`.
    pub options: Vec<QuestionOption>,
    /// Escape does not dismiss the panel when this is true. Stop still interrupts.
    pub required: bool,
    /// A permission request: the tool call it would let through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolAsk>,
    /// Several options may be picked; the answer lists their ids, one per line.
    #[serde(default, skip_serializing_if = "is_false")]
    pub multi: bool,
    /// Besides the options, free text is accepted (the answer is the text).
    #[serde(default, skip_serializing_if = "is_false")]
    pub other: bool,
}

impl UserInputQuestion {
    pub fn new(id: impl Into<String>, kind: QuestionKind, prompt: impl Into<String>, options: Vec<QuestionOption>, required: bool) -> Self {
        Self { id: id.into(), kind, prompt: prompt.into(), options, required, tool: None, multi: false, other: false }
    }

    /// A request to let a tool call through, rather than a question.
    pub fn is_permission(&self) -> bool {
        self.tool.is_some() || self.options.iter().any(|o| !o.role.is_choice())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInputAnswer {
    pub question_id: String,
    pub kind: QuestionKind,
    /// `Select` / `Confirm`: chosen option id (multi-select: ids, one per
    /// line). `Input` / `Editor` (or an "Other" answer): the text.
    pub value: String,
}

/// Name of the tool part a settled question leaves in the session doc. Its
/// `summary` is a [`Receipt`] as JSON.
pub const RECEIPT_TOOL: &str = "keel:receipt";

/// How a question was settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allowed,
    AlwaysAllowed,
    Denied,
    Answered,
    Skipped,
    /// The run was stopped while it waited.
    Stopped,
    /// The request went away unanswered (the turn ended, or it timed out).
    Canceled,
}

/// What a settled question leaves in the transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub decision: Decision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolAsk>,
    /// The question as asked (for a permission without a tool, its prompt).
    pub prompt: String,
    /// Chosen labels (one per line) or the text typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Option label behind a permission decision that is neither allow nor
    /// deny (a harness's own option).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
}

impl Receipt {
    pub fn for_answer(question: &UserInputQuestion, answer: &UserInputAnswer) -> Self {
        let mut receipt = Self::ended(question, Decision::Answered);
        let value = answer.value.trim();
        if question.is_permission() {
            match question.options.iter().find(|o| o.id == answer.value) {
                Some(option) => {
                    receipt.decision = match option.role {
                        OptionRole::AllowOnce => Decision::Allowed,
                        OptionRole::AllowAlways => Decision::AlwaysAllowed,
                        OptionRole::RejectOnce | OptionRole::RejectAlways => Decision::Denied,
                        OptionRole::Choice => Decision::Answered,
                    };
                    if option.role.is_choice() {
                        receipt.choice = Some(option.label.clone());
                    }
                }
                None if value.is_empty() => receipt.decision = Decision::Canceled,
                None => receipt.answer = Some(value.to_string()),
            }
            return receipt;
        }
        if value.is_empty() {
            receipt.decision = Decision::Skipped;
            return receipt;
        }
        let labels: Vec<String> = answer
            .value
            .lines()
            .map(|id| question.options.iter().find(|o| o.id == id).map_or_else(|| id.trim().to_string(), |o| o.label.clone()))
            .filter(|label| !label.is_empty())
            .collect();
        receipt.answer = Some(labels.join("\n"));
        receipt
    }

    /// A question that ends without an answer (stopped, canceled), or a
    /// blank receipt to fill in.
    pub fn ended(question: &UserInputQuestion, decision: Decision) -> Self {
        Self { decision, tool: question.tool.clone(), prompt: question.prompt.clone(), answer: None, choice: None }
    }

    /// One line of JSON, kept under 12k chars by shortening long args.
    pub fn to_summary(&self) -> String {
        let text = serde_json::to_string(self).unwrap_or_default();
        if text.len() <= 12_000 {
            return text;
        }
        let mut short = self.clone();
        if let Some(tool) = short.tool.as_mut() {
            for (_, value) in tool.args.iter_mut() {
                if value.chars().count() > 300 {
                    *value = value.chars().take(300).chain(std::iter::once('…')).collect();
                }
            }
            tool.args.truncate(20);
        }
        short.prompt = short.prompt.chars().take(500).collect();
        short.answer = short.answer.map(|a| a.chars().take(2000).collect());
        serde_json::to_string(&short).unwrap_or_default()
    }

    pub fn from_summary(summary: &str) -> Option<Self> {
        serde_json::from_str(summary).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission() -> UserInputQuestion {
        let mut q = UserInputQuestion::new(
            "q",
            QuestionKind::Select,
            "Allow navigate?",
            vec![
                QuestionOption::new("a1", "Allow").with_role(OptionRole::AllowOnce),
                QuestionOption::new("a2", "Always").with_role(OptionRole::AllowAlways),
                QuestionOption::new("r1", "Reject").with_role(OptionRole::RejectOnce),
            ],
            true,
        );
        q.tool = Some(ToolAsk { name: "navigate".into(), source: Some("keel-browser".into()), description: None, args: vec![("url".into(), "https://example.com".into())] });
        q
    }

    fn answer(value: &str) -> UserInputAnswer {
        UserInputAnswer { question_id: "q".into(), kind: QuestionKind::Select, value: value.into() }
    }

    #[test]
    fn roles_from_acp_and_guessed() {
        assert_eq!(OptionRole::from_acp_kind("allow_always"), OptionRole::AllowAlways);
        assert_eq!(OptionRole::from_acp_kind("weird"), OptionRole::Choice);
        assert_eq!(OptionRole::guess("approved", "Yes"), OptionRole::AllowOnce);
        assert_eq!(OptionRole::guess("approved_for_session", "Yes, for this session"), OptionRole::AllowAlways);
        assert_eq!(OptionRole::guess("abort", "No, and tell Codex what to do"), OptionRole::RejectOnce);
        assert_eq!(OptionRole::guess("opt_a", "Blue"), OptionRole::Choice);
    }

    #[test]
    fn receipts_follow_the_role_not_the_position() {
        let q = permission();
        assert_eq!(Receipt::for_answer(&q, &answer("a2")).decision, Decision::AlwaysAllowed);
        assert_eq!(Receipt::for_answer(&q, &answer("a1")).decision, Decision::Allowed);
        assert_eq!(Receipt::for_answer(&q, &answer("r1")).decision, Decision::Denied);
        assert_eq!(Receipt::for_answer(&q, &answer("")).decision, Decision::Canceled);
        let r = Receipt::for_answer(&q, &answer("a1"));
        assert_eq!(Receipt::from_summary(&r.to_summary()), Some(r));
    }

    #[test]
    fn question_receipts_carry_labels() {
        let q = UserInputQuestion::new("q", QuestionKind::Select, "Color?", vec![QuestionOption::new("b", "Blue"), QuestionOption::new("g", "Green")], false);
        let r = Receipt::for_answer(&q, &answer("b\ng"));
        assert_eq!((r.decision, r.answer.as_deref()), (Decision::Answered, Some("Blue\nGreen")));
        assert_eq!(Receipt::for_answer(&q, &answer("")).decision, Decision::Skipped);
        assert_eq!(Receipt::for_answer(&q, &answer("purple")).answer.as_deref(), Some("purple"));
        assert!(!q.is_permission());
    }

    #[test]
    fn tool_names_split_out_their_server() {
        assert_eq!(ToolAsk::split_name("mcp__keel-browser__navigate"), ("navigate".into(), Some("keel-browser".into())));
        assert_eq!(ToolAsk::split_name("navigate (keel-browser MCP Server)"), ("navigate".into(), Some("keel-browser".into())));
        assert_eq!(ToolAsk::split_name("Bash"), ("Bash".into(), None));
        let t = ToolAsk::from_input("Bash", None, &serde_json::json!({"command": "ls", "timeout": 5, "x": null}));
        assert_eq!(t.args, vec![("command".into(), "ls".into()), ("timeout".into(), "5".into())]);
    }

    #[test]
    fn old_wire_format_still_reads() {
        let q: UserInputQuestion = serde_json::from_str(r#"{"id":"q","kind":"select","prompt":"p","options":[{"id":"a","label":"A"}],"required":true}"#).unwrap();
        assert_eq!(q.options[0].role, OptionRole::Choice);
        assert!(q.tool.is_none() && !q.multi && !q.other);
    }
}

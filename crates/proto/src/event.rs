//! Agent events and the harness identity carried on them.

use serde::{Deserialize, Serialize};

use crate::question::UserInputQuestion;
use crate::subagent::SubagentRef;

/// Bytes of a tool's output kept in `ToolFinished::output_tail`. Test
/// runners print their totals at the end.
pub const OUTPUT_TAIL: usize = 64 * 1024;

/// The last [`OUTPUT_TAIL`] bytes of `raw`, cut on a char boundary.
pub fn output_tail(raw: &str) -> String {
    let mut start = raw.len().saturating_sub(OUTPUT_TAIL);
    while !raw.is_char_boundary(start) {
        start += 1;
    }
    raw[start..].to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessId {
    Mock,
    /// Keel's own harness, built on a bundled Pi.
    Keel,
}

/// How much a harness may do before asking. `Ask` is each CLI's own default;
/// stored as its snake_case name, empty reads as `Ask`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    #[default]
    Ask,
    /// File edits go through; commands still ask.
    AutoEdit,
    /// Nothing asks.
    FullAccess,
}

impl PermissionMode {
    pub const ALL: [Self; 3] = [Self::Ask, Self::AutoEdit, Self::FullAccess];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::AutoEdit => "auto_edit",
            Self::FullAccess => "full_access",
        }
    }

    /// Unknown or empty is `Ask`: never widen access on a typo.
    pub fn parse(s: &str) -> Self {
        Self::ALL.into_iter().find(|m| m.as_str() == s).unwrap_or_default()
    }
}

/// One file a tool edited. `old_text` is absent when the harness only sent
/// the new body, or a unified diff (Codex `changes[].diff` lands in `new_text`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDiff {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub new_text: String,
}

/// One entry of a `Plan` update. `priority` is the ACP value when the
/// harness sent one; Codex plan steps have none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Text {
        message_id: String,
        part_id: String,
        delta: String,
        /// Set when this traffic belongs to a subagent the parent spawned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<SubagentRef>,
    },
    Reasoning {
        message_id: String,
        part_id: String,
        delta: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<SubagentRef>,
    },
    ToolStarted {
        tool_id: String,
        name: String,
        input_bytes: u64,
        /// What the call works on (file, command, pattern, URL), one line;
        /// empty when unknown. Absent in older journals.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<SubagentRef>,
        /// ACP `kind` (`read`, `edit`, …), or a name inferred when the
        /// harness omitted it. Absent in older journals.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
    },
    /// Streaming tool output, or a later frame that names `kind` (Grok's
    /// status-less `tool_call_update`). At most ten output updates a second
    /// per tool; a kind-only update is not throttled.
    ToolProgress {
        tool_id: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        output_tail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<SubagentRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
    },
    /// The turn's plan, replaced whole on each update.
    Plan {
        message_id: String,
        entries: Vec<PlanEntry>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<SubagentRef>,
    },
    ToolFinished {
        tool_id: String,
        summary: String,
        output_bytes: u64,
        /// The tool reported failure. Absent in older journals: false.
        #[serde(default)]
        is_error: bool,
        /// Images the tool returned: absolute file paths, or `data:` URIs
        /// the engine writes to disk. The doc stores paths only.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
        /// The output's last [`OUTPUT_TAIL`] bytes, for the engine to read
        /// (a test run's totals). Journaled on the host; never sent to
        /// viewers or written to the doc. Absent in older journals.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        output_tail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<SubagentRef>,
        /// A diff the tool carried. Absent in older journals.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<ToolDiff>,
        /// How long the tool ran, when the harness says (Pi and Codex
        /// `durationMs`). Absent: the engine times start to finish itself.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    Question {
        id: String,
        questions: Vec<UserInputQuestion>,
        /// A subagent asking is the panel's blocked state. No driver sends it
        /// yet; the routing is here so one can start without a wire change.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<SubagentRef>,
    },
    /// The parent spawned a subagent (`Task`). `subagent_id` is the spawn
    /// tool call's id; the child's later traffic carries the same ref.
    SubagentStarted {
        tool_id: String,
        subagent_id: String,
        name: String,
        description: String,
    },
    /// The subagent's spawn tool call finished. `status` says how it ended.
    SubagentDone {
        subagent_id: String,
        status: TurnStatus,
    },
    /// Tokens spent since the last `Usage` of this run, and how full the
    /// context is now. Drivers send it after each model call or turn.
    Usage {
        usage: TokenUsage,
    },
    Done {
        status: TurnStatus,
    },
}

/// Token counts as the CLI reported them, normalized: `input` excludes
/// cached input, which is in `cache_read` / `cache_write`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// The model that spent them; empty means the chat's model.
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    /// Tokens in the context after the latest call, when the CLI says.
    #[serde(default)]
    pub context_used: Option<u64>,
    /// The model's context window, when the CLI says.
    #[serde(default)]
    pub context_window: Option<u64>,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// Empty struct variants. `tag = "type"` rejects unit variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnStatus {
    Completed {},
    Interrupted {},
    Aborted {},
    Errored { message: String },
}

impl AgentEvent {
    /// The subagent this traffic belongs to, when the driver scoped it.
    pub fn subagent(&self) -> Option<&SubagentRef> {
        match self {
            Self::Text { subagent, .. }
            | Self::Reasoning { subagent, .. }
            | Self::ToolStarted { subagent, .. }
            | Self::ToolProgress { subagent, .. }
            | Self::ToolFinished { subagent, .. }
            | Self::Plan { subagent, .. }
            | Self::Question { subagent, .. } => subagent.as_ref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_tool_events_decode_and_new_fields_round_trip() {
        let started: AgentEvent = serde_json::from_str(
            r#"{"type":"tool_started","tool_id":"t","name":"bash","input_bytes":1}"#,
        )
        .unwrap();
        assert!(
            matches!(started, AgentEvent::ToolStarted { kind: None, ref detail, .. } if detail.is_empty())
        );
        let finished: AgentEvent = serde_json::from_str(
            r#"{"type":"tool_finished","tool_id":"t","summary":"ok","output_bytes":2}"#,
        )
        .unwrap();
        assert!(matches!(
            finished,
            AgentEvent::ToolFinished {
                diff: None,
                duration_ms: None,
                is_error: false,
                ..
            }
        ));
        let progress: AgentEvent =
            serde_json::from_str(r#"{"type":"tool_progress","tool_id":"t"}"#).unwrap();
        assert!(matches!(
            progress,
            AgentEvent::ToolProgress { ref output_tail, kind: None, subagent: None, .. } if output_tail.is_empty()
        ));

        let event = AgentEvent::ToolFinished {
            tool_id: "t".to_string(),
            summary: "ok".to_string(),
            output_bytes: 1,
            is_error: false,
            images: Vec::new(),
            output_tail: String::new(),
            subagent: None,
            diff: Some(ToolDiff {
                path: "a.rs".to_string(),
                old_text: None,
                new_text: "x".to_string(),
            }),
            duration_ms: Some(12),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("output_tail").is_none());
        assert!(json.get("images").is_none());
        assert_eq!(json["diff"]["path"], "a.rs");
        assert!(json["diff"].get("old_text").is_none());
        assert_eq!(json["duration_ms"], 12);
        let back: AgentEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, event);
    }
}

//! Wire types and the sidebar view rules. No I/O.

pub mod command;
pub mod dotify;
pub mod effort;
pub mod event;
pub mod mr;
pub mod projects;
pub mod question;
pub mod rpc;
pub mod settings;
pub mod subagent;
pub mod view;

pub use command::{Attachment, UserCommand};
pub use effort::{RETIRED_POTETO, migrate_effort};
pub use event::{
    AgentEvent, HarnessId, OUTPUT_TAIL, PermissionMode, PlanEntry, TokenUsage, ToolDiff, TurnStatus, output_tail,
};
pub use subagent::{SubagentCaps, SubagentRef, SubagentVisibility};
pub use question::{
    Decision, OptionRole, QuestionKind, QuestionOption, RECEIPT_TOOL, Receipt, ToolAsk, flatten_args, UserInputAnswer, UserInputQuestion,
};
pub use rpc::{
    DocKind, FILE_CHANGED, RpcErrorBody, RpcErrorCode, RpcFailure, RpcRequest, RpcStreamItem, SubscribeEvent,
};
pub use settings::{
    BadgeStyle, DEFAULT_SOUND, HarnessPrefs, MAX_BACKGROUND_DIM, NotifySettings, SoundChoice, ThemeChoice, UiSettings,
};
pub use view::{
    AttentionChat, ChatStatus, DisplayStatus, STALE_AFTER_MS, SpaceGroup, attention_sort,
    display_status, group_by_space,
};

/// Tools the `keel-ui` MCP server advertises. A harness may prefix the name.
pub const UI_TOOLS: [&str; 13] = [
    "show_chart",
    "show_choices",
    "show_stats",
    "show_commands",
    "show_files",
    "show_mr",
    "show_steps",
    "task_steps",
    "show_callout",
    "show_compare",
    "show_diagram",
    "show_tests",
    "show_plan",
];

/// Trims trailing dots and ellipses (`.`, `…`) from progress labels, keeping meaningful internal punctuation.
pub fn trim_progress_label(raw: &str) -> &str {
    let mut s = raw.trim();
    while s.ends_with('.') || s.ends_with('…') {
        s = s.trim_end_matches(['.', '…']).trim();
    }
    s
}

/// Whether `name` is one of those tools (`show_chart`, `mcp__keel-ui__show_chart`).
pub fn is_ui_tool(name: &str) -> bool {
    UI_TOOLS.iter().any(|tool| {
        name == *tool || name.ends_with(&format!("__{tool}")) || name.ends_with(&format!("_{tool}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_trailing_ellipses_and_dots_preserving_internal_punctuation() {
        assert_eq!(trim_progress_label("Thinking…"), "Thinking");
        assert_eq!(trim_progress_label("Thinking..."), "Thinking");
        assert_eq!(trim_progress_label("Thinking."), "Thinking");
        assert_eq!(trim_progress_label("Working…"), "Working");
        assert_eq!(trim_progress_label("Working..."), "Working");
        assert_eq!(trim_progress_label("Reading the on/off cycle logic..."), "Reading the on/off cycle logic");
        assert_eq!(trim_progress_label("Reading the on/off cycle logic…"), "Reading the on/off cycle logic");
        assert_eq!(trim_progress_label("Reading the on/off cycle logic."), "Reading the on/off cycle logic");
        assert_eq!(trim_progress_label("Checking v1.2 config"), "Checking v1.2 config");
        assert_eq!(trim_progress_label("Checking v1.2 config..."), "Checking v1.2 config");
        assert_eq!(trim_progress_label("Running checks (0/4)..."), "Running checks (0/4)");
    }
}

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
pub const UI_TOOLS: [&str; 12] = [
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
];

/// Whether `name` is one of those tools (`show_chart`, `mcp__keel-ui__show_chart`).
pub fn is_ui_tool(name: &str) -> bool {
    UI_TOOLS.iter().any(|tool| {
        name == *tool || name.ends_with(&format!("__{tool}")) || name.ends_with(&format!("_{tool}"))
    })
}

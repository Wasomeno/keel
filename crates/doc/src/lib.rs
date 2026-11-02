//! Session and registry documents.
//!
//! One Loro document per chat and one registry document per profile.
//! Command entries are the ledger. `keel-store` stores snapshots of these documents
//! and imports raw Loro updates into them.

mod error;
mod limits;
mod loro_util;
mod registry;
mod session;

pub use error::DocError;
pub use limits::{
    COMMAND_TTL_MS, CONTINUATION_SPLIT, SCHEMA_VERSION, TOOL_SUMMARY_CHARS, tool_summary,
};
pub use registry::{ChatRow, ChatUsage, DeviceRow, DiffRow, ModelTokens, RegistryDoc, RegistryView, SpaceRow};
pub use session::{
    ARCHIVE_MESSAGE_ID, ARCHIVE_TEXT, CommandView, MessageView, NewSession, Outcome, PartView,
    PlanEntryView, SessionDoc, SessionMeta, SessionView, SubagentRow, SubagentView, ToolRow,
};

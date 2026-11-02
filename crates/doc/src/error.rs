//! Failures a document write or open can return.

use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("schema version {found} is newer than {known}; upgrade Keel to open this document")]
    SchemaTooNew { found: i64, known: i64 },
    #[error("document has no schema version")]
    SchemaMissing,
    #[error("actor {actor} cannot append a command originated by {origin}")]
    ForeignOrigin { actor: String, origin: String },
    #[error("command {0} is already in the ledger")]
    DuplicateCommand(Uuid),
    #[error("device {actor} is not the host {host}")]
    NotHost { actor: String, host: String },
    #[error("command {0} is not in the ledger")]
    UnknownCommand(Uuid),
    #[error("command {command_id} already has outcome {outcome}")]
    OutcomeSet { command_id: Uuid, outcome: String },
    #[error("message {0} is not in the session")]
    UnknownMessage(String),
    #[error("subagent {0} is not in the session")]
    UnknownSubagent(String),
    #[error("message {0} is already in the session")]
    DuplicateMessage(String),
    #[error("space {0} is not in the registry")]
    UnknownSpace(String),
    #[error("chat {0} is not in the registry")]
    UnknownChat(String),
    #[error("{0} is tombstoned")]
    Tombstoned(String),
    #[error("device {actor} cannot write device row {device}")]
    NotDevice { actor: String, device: String },
    #[error("device {actor} cannot write status for a chat hosted by {host}")]
    NotChatHost { actor: String, host: String },
    #[error("device {actor} cannot stamp git for a space owned by {owner}")]
    NotSpaceOwner { actor: String, owner: String },
    #[error("snapshot export failed: {0}")]
    Encode(String),
    #[error(transparent)]
    Loro(#[from] loro::LoroError),
}

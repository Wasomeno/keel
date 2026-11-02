//! Failures from the profile layout and the snapshot store.

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("schema version {found} is newer than {known}; upgrade Keel to open this profile")]
    SchemaTooNew { found: i64, known: i64 },
    #[error("session {0} is already in the store")]
    DuplicateSession(String),
    #[error("document {0} is not in the store")]
    UnknownDoc(String),
    #[error("sidecar kind {0} is not tail or diff")]
    BadSidecarKind(String),
    #[error("device-id is not a uuid")]
    BadDeviceId,
    #[error("KEEL_DATA_DIR is unset and this platform has no default data directory")]
    NoDataDir,
    #[error("snapshot store stayed unreadable after moving the corrupt file aside")]
    CorruptStore,
    #[error(transparent)]
    Doc(#[from] keel_doc::DocError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

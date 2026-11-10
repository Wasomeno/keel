//! SQLite snapshot store. WAL, `synchronous = NORMAL`.
//!
//! The store hosts the session and registry documents. `commit` writes each
//! document's snapshot bytes in one transaction, with its `cursor` column: a
//! room position from the removed sync feature, kept so existing rows
//! round-trip unchanged. A blob that will
//! not import is a corrupt store: the file is renamed to
//! `store.sqlite.bak-{unix}` and the next open starts empty. A newer
//! `meta.schema` refuses the open and leaves the file in place.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use keel_doc::{DocError, NewSession, RegistryDoc, SessionDoc};
use rusqlite::{Connection, params};

use crate::error::StoreError;

/// `snapshots.doc_id` for the profile registry.
pub const REGISTRY_DOC_ID: &str = "registry";

/// Sidecar kind for the derived tail.
pub const SIDECAR_TAIL: &str = "tail";

/// Sidecar kind for the diff summary pointer.
pub const SIDECAR_DIFF: &str = "diff";

/// `snapshots.doc_id` for one chat.
pub fn session_doc_id(chat_id: &str) -> String {
    format!("session:{chat_id}")
}

/// Native CLI session row. `file_path` is optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSession {
    pub chat_id: String,
    pub harness: String,
    pub native_id: String,
    pub file_path: Option<String>,
}

/// Hosts the in-memory mirrors and the `store.sqlite` file in a profile directory.
pub struct DocsStore {
    path: PathBuf,
    conn: Connection,
    sessions: HashMap<String, Arc<SessionDoc>>,
    registry: Arc<RegistryDoc>,
    cursors: HashMap<String, i64>,
    /// Sessions stored on disk but not imported yet (see [`DocsStore::open_lazy`]).
    unloaded: HashSet<String>,
    /// Version and cursor of each doc as last written by [`Self::commit`]:
    /// a doc whose version and cursor still match is not exported again.
    written: HashMap<String, (Vec<u8>, i64)>,
}

impl DocsStore {
    /// Open `profile_dir/store.sqlite`, creating the directory and the schema.
    /// A corrupt snapshot blob moves the file aside and opens an empty store.
    pub fn open(profile_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with(profile_dir.as_ref(), false)
    }

    /// Like [`Self::open`], but imports only the registry. Session snapshots stay
    /// on disk until [`Self::load_session`]. `session_ids` still lists them.
    /// A corrupt session blob surfaces as an error from `load_session` for that
    /// chat instead of moving the whole store aside.
    pub fn open_lazy(profile_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with(profile_dir.as_ref(), true)
    }

    fn open_with(profile_dir: &Path, lazy: bool) -> Result<Self, StoreError> {
        fs::create_dir_all(profile_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(profile_dir, fs::Permissions::from_mode(0o700))?;
        }
        let path = profile_dir.join("store.sqlite");
        match load(&path, lazy) {
            Ok(store) => Ok(store),
            Err(LoadError::Restart) => match load(&path, lazy) {
                Ok(store) => Ok(store),
                Err(LoadError::Restart) => Err(StoreError::CorruptStore),
                Err(err) => Err(err.into()),
            },
            Err(err) => Err(err.into()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn registry(&self) -> &RegistryDoc {
        &self.registry
    }

    pub fn registry_arc(&self) -> Arc<RegistryDoc> {
        self.registry.clone()
    }

    pub fn session(&self, chat_id: &str) -> Option<&SessionDoc> {
        self.sessions.get(chat_id).map(|s| &**s)
    }

    pub fn session_arc(&self, chat_id: &str) -> Option<Arc<SessionDoc>> {
        self.sessions.get(chat_id).cloned()
    }

    /// Import a session on demand. Already-loaded sessions are returned as is.
    pub fn load_session(&mut self, chat_id: &str) -> Result<Option<Arc<SessionDoc>>, StoreError> {
        if let Some(doc) = self.sessions.get(chat_id) {
            return Ok(Some(doc.clone()));
        }
        if !self.unloaded.contains(chat_id) {
            return Ok(None);
        }
        let bytes: Vec<u8> = self.conn.query_row(
            "SELECT bytes FROM snapshots WHERE doc_id = ?1",
            params![session_doc_id(chat_id)],
            |row| row.get(0),
        )?;
        let doc = Arc::new(SessionDoc::open(&bytes)?);
        self.unloaded.remove(chat_id);
        self.sessions.insert(chat_id.to_string(), doc.clone());
        Ok(Some(doc))
    }

    pub fn session_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .sessions
            .keys()
            .chain(self.unloaded.iter())
            .cloned()
            .collect();
        ids.sort();
        ids
    }

    /// Replace a hosted session with a shallow import. The chat id must already exist.
    pub fn replace_session(&mut self, chat_id: &str, bytes: &[u8]) -> Result<(), StoreError> {
        if !self.sessions.contains_key(chat_id) && !self.unloaded.contains(chat_id) {
            return Err(StoreError::UnknownDoc(session_doc_id(chat_id)));
        }
        let doc = SessionDoc::from_import(bytes)?;
        self.unloaded.remove(chat_id);
        self.sessions.insert(chat_id.to_string(), Arc::new(doc));
        // Same version, new bytes (a compaction): the next commit must write.
        self.written.remove(&session_doc_id(chat_id));
        Ok(())
    }

    pub fn create_session(&mut self, fields: NewSession) -> Result<(), StoreError> {
        let chat_id = fields.chat_id.clone();
        if self.sessions.contains_key(&chat_id) || self.unloaded.contains(&chat_id) {
            return Err(StoreError::DuplicateSession(chat_id));
        }
        let doc = SessionDoc::create(fields)?;
        self.cursors.entry(session_doc_id(&chat_id)).or_insert(0);
        self.sessions.insert(chat_id, Arc::new(doc));
        Ok(())
    }

    pub fn cursor(&self, doc_id: &str) -> i64 {
        self.cursors.get(doc_id).copied().unwrap_or(0)
    }

    /// Stage `cursor` for the next [`Self::commit`], which writes it with the bytes.
    pub fn set_cursor(&mut self, doc_id: &str, cursor: i64) -> Result<(), StoreError> {
        if doc_id != REGISTRY_DOC_ID {
            // commit writes bytes with the cursor, so the session must be in memory.
            let chat_id = doc_id.strip_prefix("session:").unwrap_or_default();
            if self.load_session(chat_id)?.is_none() {
                return Err(StoreError::UnknownDoc(doc_id.to_string()));
            }
        }
        self.cursors.insert(doc_id.to_string(), cursor);
        Ok(())
    }

    /// Export every loaded document that changed since its last commit (its
    /// version or cursor moved) and write `bytes` plus `cursor` together.
    /// Unchanged and unloaded documents keep their stored rows.
    pub fn commit(&mut self, updated_at: i64) -> Result<(), StoreError> {
        let mut rows: Vec<(String, Vec<u8>, i64)> = Vec::new();
        let mut marks: Vec<(String, Vec<u8>, i64)> = Vec::new();
        let unchanged = |written: &HashMap<String, (Vec<u8>, i64)>, doc_id: &str, version: &[u8], cursor: i64| {
            written.get(doc_id).is_some_and(|(v, c)| v == version && *c == cursor)
        };
        for (chat_id, doc) in &self.sessions {
            let doc_id = session_doc_id(chat_id);
            let cursor = self.cursors.get(&doc_id).copied().unwrap_or(0);
            let version = doc.version_key();
            if unchanged(&self.written, &doc_id, &version, cursor) {
                continue;
            }
            rows.push((doc_id.clone(), doc.export_snapshot()?, cursor));
            marks.push((doc_id, version, cursor));
        }
        let registry_cursor = self.cursors.get(REGISTRY_DOC_ID).copied().unwrap_or(0);
        let registry_version = self.registry.version_key();
        if !unchanged(&self.written, REGISTRY_DOC_ID, &registry_version, registry_cursor) {
            rows.push((
                REGISTRY_DOC_ID.to_string(),
                self.registry.export_snapshot()?,
                registry_cursor,
            ));
            marks.push((REGISTRY_DOC_ID.to_string(), registry_version, registry_cursor));
        }
        if rows.is_empty() {
            return Ok(());
        }

        let tx = self.conn.transaction()?;
        for (doc_id, bytes, cursor) in &rows {
            tx.execute(
                "INSERT INTO snapshots (doc_id, bytes, cursor, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(doc_id) DO UPDATE SET
                   bytes = excluded.bytes,
                   cursor = excluded.cursor,
                   updated_at = excluded.updated_at",
                params![doc_id, bytes, cursor, updated_at],
            )?;
        }
        tx.commit()?;
        for (doc_id, version, cursor) in marks {
            self.written.insert(doc_id, (version, cursor));
        }
        Ok(())
    }

    /// First write wins. A repeated `command_id` leaves the stored outcome.
    pub fn record_processed(
        &self,
        command_id: &str,
        doc_id: &str,
        outcome: &str,
        at: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO processed_commands (command_id, doc_id, outcome, at)
             VALUES (?1, ?2, ?3, ?4)",
            params![command_id, doc_id, outcome, at],
        )?;
        Ok(())
    }

    pub fn processed(&self, command_id: &str) -> Result<Option<(String, String, i64)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT doc_id, outcome, at FROM processed_commands WHERE command_id = ?1")?;
        let mut rows = stmt.query(params![command_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?)))
    }

    pub fn put_sidecar(&self, doc_id: &str, kind: &str, bytes: &[u8]) -> Result<(), StoreError> {
        check_kind(kind)?;
        self.conn.execute(
            "INSERT INTO sidecars (doc_id, kind, bytes) VALUES (?1, ?2, ?3)
             ON CONFLICT(doc_id, kind) DO UPDATE SET bytes = excluded.bytes",
            params![doc_id, kind, bytes],
        )?;
        Ok(())
    }

    pub fn sidecar(&self, doc_id: &str, kind: &str) -> Result<Option<Vec<u8>>, StoreError> {
        check_kind(kind)?;
        let mut stmt = self
            .conn
            .prepare("SELECT bytes FROM sidecars WHERE doc_id = ?1 AND kind = ?2")?;
        let mut rows = stmt.query(params![doc_id, kind])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(row.get(0)?))
    }

    pub fn put_native_session(&self, row: &NativeSession) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO native_sessions (chat_id, harness, native_id, file_path)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat_id) DO UPDATE SET
               harness = excluded.harness,
               native_id = excluded.native_id,
               file_path = excluded.file_path",
            params![row.chat_id, row.harness, row.native_id, row.file_path],
        )?;
        Ok(())
    }

    pub fn native_session(&self, chat_id: &str) -> Result<Option<NativeSession>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT chat_id, harness, native_id, file_path FROM native_sessions WHERE chat_id = ?1",
        )?;
        let mut rows = stmt.query(params![chat_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(NativeSession {
            chat_id: row.get(0)?,
            harness: row.get(1)?,
            native_id: row.get(2)?,
            file_path: row.get(3)?,
        }))
    }

    pub fn delete_native_session(&self, chat_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM native_sessions WHERE chat_id = ?1",
            params![chat_id],
        )?;
        Ok(())
    }

    pub fn journal_mode(&self) -> Result<String, StoreError> {
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        Ok(mode)
    }
}

fn check_kind(kind: &str) -> Result<(), StoreError> {
    if kind == SIDECAR_TAIL || kind == SIDECAR_DIFF {
        Ok(())
    } else {
        Err(StoreError::BadSidecarKind(kind.to_string()))
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS snapshots (
  doc_id     TEXT PRIMARY KEY,
  bytes      BLOB NOT NULL,
  cursor     INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS processed_commands (
  command_id TEXT PRIMARY KEY,
  doc_id     TEXT NOT NULL,
  outcome    TEXT NOT NULL,
  at         INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sidecars (
  doc_id TEXT NOT NULL,
  kind   TEXT NOT NULL,
  bytes  BLOB NOT NULL,
  PRIMARY KEY (doc_id, kind)
);
CREATE TABLE IF NOT EXISTS native_sessions (
  chat_id    TEXT PRIMARY KEY,
  harness    TEXT NOT NULL,
  native_id  TEXT NOT NULL,
  file_path  TEXT
);
";

enum Hosted {
    Session { chat_id: String, doc: SessionDoc },
    Registry(RegistryDoc),
}

enum LoadError {
    Schema { found: i64, known: i64 },
    Restart,
    Store(StoreError),
}

impl From<StoreError> for LoadError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::SchemaTooNew { found, known } => Self::Schema { found, known },
            other => Self::Store(other),
        }
    }
}

impl From<LoadError> for StoreError {
    fn from(err: LoadError) -> Self {
        match err {
            LoadError::Schema { found, known } => Self::SchemaTooNew { found, known },
            LoadError::Restart => Self::CorruptStore,
            LoadError::Store(err) => err,
        }
    }
}

impl From<rusqlite::Error> for LoadError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Store(err.into())
    }
}

impl From<DocError> for LoadError {
    fn from(err: DocError) -> Self {
        match err {
            DocError::SchemaTooNew { found, known } => Self::Schema { found, known },
            other => Self::Store(other.into()),
        }
    }
}

fn open_conn(path: &Path) -> Result<Connection, LoadError> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

fn load(path: &Path, lazy: bool) -> Result<DocsStore, LoadError> {
    let conn = open_conn(path)?;
    let mut stmt = conn.prepare(if lazy {
        "SELECT doc_id, CASE WHEN doc_id = 'registry' THEN bytes ELSE x'' END, cursor FROM snapshots"
    } else {
        "SELECT doc_id, bytes, cursor FROM snapshots"
    })?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut stored = Vec::new();
    for row in rows {
        stored.push(row?);
    }
    drop(stmt);

    let mut hosted = Vec::new();
    let mut unloaded = HashSet::new();
    let mut lazy_cursors = Vec::new();
    let mut corrupt: Option<(String, String)> = None;
    for (doc_id, bytes, cursor) in stored {
        if lazy
            && let Some(chat_id) = doc_id.strip_prefix("session:")
            && !chat_id.is_empty()
        {
            unloaded.insert(chat_id.to_string());
            lazy_cursors.push((doc_id, cursor));
            continue;
        }
        match import_doc(&doc_id, &bytes) {
            Ok(Some(doc)) => hosted.push((doc_id, cursor, doc)),
            Err(LoadError::Schema { found, known }) => {
                return Err(LoadError::Schema { found, known });
            }
            Ok(None) => {}
            Err(LoadError::Store(err)) => {
                if corrupt.is_none() {
                    corrupt = Some((doc_id, err.to_string()));
                }
            }
            Err(LoadError::Restart) => {
                if corrupt.is_none() {
                    corrupt = Some((doc_id, "unreadable snapshot".into()));
                }
            }
        }
    }
    if let Some((doc_id, reason)) = corrupt {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
        drop(conn);
        quarantine(path, &doc_id, &reason)?;
        return Err(LoadError::Restart);
    }

    let mut sessions = HashMap::new();
    let mut registry = None;
    let mut cursors: HashMap<String, i64> = lazy_cursors.into_iter().collect();
    for (doc_id, cursor, doc) in hosted {
        cursors.insert(doc_id, cursor);
        match doc {
            Hosted::Session { chat_id, doc } => {
                sessions.insert(chat_id, Arc::new(doc));
            }
            Hosted::Registry(doc) => registry = Some(Arc::new(doc)),
        }
    }
    let registry = match registry {
        Some(doc) => doc,
        None => Arc::new(RegistryDoc::create()?),
    };
    cursors.entry(REGISTRY_DOC_ID.to_string()).or_insert(0);
    Ok(DocsStore {
        path: path.to_path_buf(),
        conn,
        sessions,
        registry,
        cursors,
        unloaded,
        written: HashMap::new(),
    })
}

fn import_doc(doc_id: &str, bytes: &[u8]) -> Result<Option<Hosted>, LoadError> {
    if doc_id == REGISTRY_DOC_ID {
        return Ok(Some(Hosted::Registry(RegistryDoc::open(bytes)?)));
    }
    let Some(chat_id) = doc_id.strip_prefix("session:") else {
        return Ok(None);
    };
    if chat_id.is_empty() {
        return Err(LoadError::Store(StoreError::UnknownDoc(doc_id.to_string())));
    }
    Ok(Some(Hosted::Session {
        chat_id: chat_id.to_string(),
        doc: SessionDoc::open(bytes)?,
    }))
}

fn quarantine(path: &Path, doc_id: &str, reason: &str) -> Result<(), LoadError> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let mut dest = path.with_file_name(format!("store.sqlite.bak-{secs}"));
    if dest.exists() {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        dest = path.with_file_name(format!("store.sqlite.bak-{millis}"));
    }
    fs::rename(path, &dest).map_err(StoreError::from)?;
    // The checkpoint before this may have failed. Keep the WAL next to the backup
    // (`{bak}-wal` is what SQLite reads when the backup is opened) instead of
    // dropping committed pages that never reached the main file.
    for suffix in ["-wal", "-shm"] {
        let from = sibling(path, suffix);
        if from.exists() {
            fs::rename(&from, sibling(&dest, suffix)).map_err(StoreError::from)?;
        }
    }
    tracing::error!(
        doc_id,
        path = %path.display(),
        bak = %dest.display(),
        reason,
        "snapshot bytes are corrupt; moved the store aside and starting empty"
    );
    Ok(())
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use keel_doc::{NewSession, SpaceRow};
    use keel_proto::HarnessId;
    use uuid::Uuid;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("keel-store-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn session(chat_id: &str) -> NewSession {
        NewSession {
            chat_id: chat_id.into(),
            host_device: "host".into(),
            space_id: "space".into(),
            harness: HarnessId::Keel,
            cwd: "/tmp/work".into(),
            model: "pi".into(),
            reasoning: String::new(),
            native_session: String::new(),
            created_at: 10,
        }
    }

    #[test]
    fn commit_drop_reopen_matches_the_mirror() {
        let dir = TempDir::new();
        let chat_id = Uuid::new_v4().to_string();
        let text = {
            let mut store = DocsStore::open(&dir.0).unwrap();
            assert_eq!(store.journal_mode().unwrap().to_lowercase(), "wal");
            store.create_session(session(&chat_id)).unwrap();
            store
                .session(&chat_id)
                .unwrap()
                .append_message("m", "user", 1)
                .unwrap();
            store
                .session(&chat_id)
                .unwrap()
                .append_text("m", "p", "hello keel")
                .unwrap();
            store
                .registry()
                .put_space(&SpaceRow {
                    id: "space".into(),
                    device_id: "host".into(),
                    path: "/tmp/work".into(),
                    name: "work".into(),
                    git_detected: false,
                    checkout_id: String::new(),
                    repository_id: String::new(),
                })
                .unwrap();
            store.set_cursor(&session_doc_id(&chat_id), 7).unwrap();
            store.commit(1_700_000_000_000).unwrap();
            let text = store.session(&chat_id).unwrap().view().messages[0]
                .text
                .clone();
            assert_eq!(text, "hello keel");
            let sync: i64 = store
                .conn
                .query_row("PRAGMA synchronous", [], |row| row.get(0))
                .unwrap();
            assert_eq!(sync, 1);
            text
        };

        let store = DocsStore::open(&dir.0).unwrap();
        let view = store.session(&chat_id).unwrap().view();
        assert_eq!(view.messages.len(), 1);
        assert_eq!(view.messages[0].text, text);
        assert_eq!(view.meta.chat_id, chat_id);
        assert_eq!(view.meta.harness, "keel");
        assert_eq!(store.registry().view().spaces.len(), 1);
        assert_eq!(store.registry().view().spaces[0].name, "work");
        assert_eq!(store.cursor(&session_doc_id(&chat_id)), 7);
        assert_eq!(store.cursor(REGISTRY_DOC_ID), 0);
    }

    #[test]
    fn ledger_sidecar_and_native_session_round_trip() {
        let dir = TempDir::new();
        let chat_id = "chat";
        {
            let mut store = DocsStore::open(&dir.0).unwrap();
            store.create_session(session(chat_id)).unwrap();
            let command = keel_proto::UserCommand::Send {
                command_id: Uuid::from_u128(4),
                message_id: "m".into(),
                text: "go".into(),
                attachments: Vec::new(),
                shown: true,
            };
            store
                .session(chat_id)
                .unwrap()
                .append_command("host", "host", &command, 5)
                .unwrap();
            store
                .session(chat_id)
                .unwrap()
                .set_outcome("host", Uuid::from_u128(4), keel_doc::Outcome::Processed)
                .unwrap();
            store.commit(20).unwrap();
            let doc_id = session_doc_id(chat_id);
            store
                .record_processed(&Uuid::from_u128(4).to_string(), &doc_id, "processed", 20)
                .unwrap();
            store
                .record_processed(&Uuid::from_u128(4).to_string(), &doc_id, "rejected", 21)
                .unwrap();
            store.put_sidecar(&doc_id, SIDECAR_TAIL, b"tail").unwrap();
            store
                .put_native_session(&NativeSession {
                    chat_id: chat_id.into(),
                    harness: "pi".into(),
                    native_id: "native".into(),
                    file_path: Some("session.jsonl".into()),
                })
                .unwrap();
        }
        let store = DocsStore::open(&dir.0).unwrap();
        let command = &store.session(chat_id).unwrap().view().commands[0];
        assert_eq!(command.outcome.as_deref(), Some("processed"));
        assert_eq!(command.text, "go");
        let doc_id = session_doc_id(chat_id);
        assert_eq!(
            store.processed(&Uuid::from_u128(4).to_string()).unwrap(),
            Some((doc_id.clone(), "processed".into(), 20))
        );
        assert_eq!(
            store.sidecar(&doc_id, SIDECAR_TAIL).unwrap(),
            Some(b"tail".to_vec())
        );
        let native = store.native_session(chat_id).unwrap().unwrap();
        assert_eq!(native.native_id, "native");
        assert_eq!(native.file_path.as_deref(), Some("session.jsonl"));
    }

    #[test]
    fn corrupt_blob_is_moved_aside_and_the_store_starts_empty() {
        let dir = TempDir::new();
        {
            let mut store = DocsStore::open(&dir.0).unwrap();
            store.create_session(session("chat")).unwrap();
            store
                .session("chat")
                .unwrap()
                .append_message("m", "user", 1)
                .unwrap();
            store
                .session("chat")
                .unwrap()
                .append_text("m", "p", "gone")
                .unwrap();
            store.commit(1).unwrap();
        }
        {
            let conn = Connection::open(dir.0.join("store.sqlite")).unwrap();
            conn.execute(
                "UPDATE snapshots SET bytes = ?1 WHERE doc_id = ?2",
                params![b"not-a-snapshot".as_slice(), session_doc_id("chat")],
            )
            .unwrap();
        }
        let store = DocsStore::open(&dir.0).unwrap();
        assert!(store.session("chat").is_none());
        assert!(store.registry().view().spaces.is_empty());
        let names: Vec<String> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names
                .iter()
                .any(|name| name.starts_with("store.sqlite.bak-"))
        );
        assert!(names.iter().any(|name| name == "store.sqlite"));
    }

    #[test]
    fn lazy_open_lists_sessions_and_loads_on_demand() {
        let dir = TempDir::new();
        {
            let mut store = DocsStore::open(&dir.0).unwrap();
            store.create_session(session("a")).unwrap();
            store.create_session(session("b")).unwrap();
            store.session("a").unwrap().append_message("m", "user", 1).unwrap();
            store.set_cursor(&session_doc_id("a"), 3).unwrap();
            store.commit(1).unwrap();
        }
        let mut store = DocsStore::open_lazy(&dir.0).unwrap();
        assert_eq!(store.session_ids(), vec!["a".to_string(), "b".to_string()]);
        assert!(store.session("a").is_none());
        assert_eq!(store.cursor(&session_doc_id("a")), 3);
        assert!(store.create_session(session("a")).is_err());
        let a = store.load_session("a").unwrap().unwrap();
        assert_eq!(a.view().messages.len(), 1);
        assert!(store.session("a").is_some());
        assert!(store.load_session("missing").unwrap().is_none());
        // Commit leaves the unloaded session's row intact.
        store.commit(2).unwrap();
        drop(store);
        let store = DocsStore::open(&dir.0).unwrap();
        assert!(store.session("b").is_some());
    }

    #[test]
    fn quarantine_keeps_the_wal_with_the_backup() {
        let dir = TempDir::new();
        let path = dir.0.join("store.sqlite");
        fs::write(&path, b"main").unwrap();
        fs::write(sibling(&path, "-wal"), b"wal pages").unwrap();
        assert!(quarantine(&path, "doc", "test").is_ok());
        let bak = fs::read_dir(&dir.0)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().ends_with("-wal"))
            .expect("backup wal");
        assert!(bak.to_string_lossy().contains(".bak-"));
        assert_eq!(fs::read(bak).unwrap(), b"wal pages");
    }

    #[test]
    fn newer_schema_refuses_the_profile_and_leaves_the_file() {
        let dir = TempDir::new();
        DocsStore::open(&dir.0).unwrap();
        let raw = loro::LoroDoc::new();
        raw.get_map("meta").insert("schema", 2_i64).unwrap();
        raw.commit();
        let bytes = raw.export(loro::ExportMode::Snapshot).unwrap();
        {
            let conn = Connection::open(dir.0.join("store.sqlite")).unwrap();
            conn.execute(
                "INSERT INTO snapshots (doc_id, bytes, cursor, updated_at) VALUES (?1, ?2, 0, 1)",
                params![session_doc_id("chat"), bytes],
            )
            .unwrap();
        }
        let err = match DocsStore::open(&dir.0) {
            Err(err) => err,
            Ok(_) => panic!("opened a newer schema"),
        };
        assert!(matches!(
            err,
            StoreError::SchemaTooNew { found: 2, known: 1 }
        ));
        let names: Vec<String> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|name| !name.contains(".bak-")));
        assert!(dir.0.join("store.sqlite").is_file());
    }
}

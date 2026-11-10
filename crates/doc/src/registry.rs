//! One registry document per profile.
//!
//! Rows are indexes. A device writes its own device row, the status of chats
//! it hosts, and git stamps for spaces it owns. Creates, renames, archives,
//! and seen-marks are last-write-wins from any device. `delete_space`
//! tombstones the space and every chat in it in one commit. Session documents
//! stay where they are. Tombstones stay in the map so a concurrent write
//! cannot resurrect a deleted key. The mirror omits them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use keel_proto::{ChatStatus, TokenUsage};
use loro::{ExportMode, LoroDoc, LoroMap, Subscription, ToJson, VersionVector};

use crate::error::DocError;
use crate::limits::SCHEMA_VERSION;
use crate::loro_util::{as_map, map_bool, map_i64, map_str, map_u64};

/// A folder on one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceRow {
    pub id: String,
    pub device_id: String,
    pub path: String,
    pub name: String,
    pub git_detected: bool,
    pub checkout_id: String,
    pub repository_id: String,
}

/// Sidebar row for one chat. `status` is `idle`, `running`, `awaiting_input`, or `aborted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRow {
    pub id: String,
    pub device_id: String,
    pub title: String,
    pub archived: bool,
    pub cwd: String,
    pub branch: String,
    pub checkout_id: String,
    pub space_id: String,
    pub last_seen_at: i64,
    pub last_message_preview: String,
    pub last_message_at: i64,
    pub harness: String,
    pub model: String,
    /// Thinking effort for the next turn. Empty is the harness default.
    pub reasoning: String,
    /// [`keel_proto::PermissionMode`] name for the next turn. Empty is Ask.
    pub permission: String,
    /// Plan mode for the next turn. Off when the chat has no saved value.
    pub plan_mode: bool,
    pub status: String,
    /// Written only by [`RegistryDoc::add_chat_usage`]; `put_chat` leaves it.
    pub usage: ChatUsage,
}

/// Tokens a chat has spent, per model, and its context after the last turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatUsage {
    /// Sorted by model.
    pub models: Vec<ModelTokens>,
    pub context_used: u64,
    /// 0 when the CLI never said.
    pub context_window: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelTokens {
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cache_warm: u64,
}

/// A device that has announced itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub id: String,
    pub name: String,
    pub os: String,
}

/// Numstat pointer. The patch itself stays on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub checkout_id: String,
    pub additions: i64,
    pub deletions: i64,
    pub files: i64,
    pub sha256: String,
    pub captured_at: i64,
}

/// Mirror of the registry. Tombstoned rows are absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryView {
    pub spaces: Vec<SpaceRow>,
    pub chats: Vec<ChatRow>,
    pub devices: Vec<DeviceRow>,
    pub diffs: Vec<DiffRow>,
}

struct Inner {
    doc: LoroDoc,
    view: Mutex<RegistryView>,
    commits: AtomicU64,
}

/// Registry document plus its mirror.
pub struct RegistryDoc {
    _sub: Subscription,
    inner: Arc<Inner>,
}

impl RegistryDoc {
    pub fn create() -> Result<Self, DocError> {
        let doc = LoroDoc::new();
        let meta = doc.get_map("meta");
        meta.insert("schema", SCHEMA_VERSION)?;
        doc.get_map("spaces");
        doc.get_map("chats");
        doc.get_map("devices");
        doc.get_map("diffs");
        doc.commit();
        Self::from_attached(doc)
    }

    pub fn open(bytes: &[u8]) -> Result<Self, DocError> {
        let doc = LoroDoc::from_snapshot(bytes)?;
        check_schema(&doc)?;
        Self::from_attached(doc)
    }

    /// Import a snapshot or an update blob into a new document.
    pub fn from_import(bytes: &[u8]) -> Result<Self, DocError> {
        let doc = LoroDoc::new();
        doc.import(bytes)?;
        check_schema(&doc)?;
        Self::from_attached(doc)
    }

    /// The doc's version (its oplog version vector, encoded): unchanged
    /// bytes mean nothing to write. Commits a pending transaction first,
    /// like an export would, so no op is missed.
    pub fn version_key(&self) -> Vec<u8> {
        self.inner.doc.commit();
        self.inner.doc.oplog_vv().encode()
    }

    pub fn export_snapshot(&self) -> Result<Vec<u8>, DocError> {
        self.inner
            .doc
            .export(ExportMode::Snapshot)
            .map_err(|err| DocError::Encode(err.to_string()))
    }

    /// Ops not covered by `from`. The bytes are a raw `LoroDoc` update.
    pub fn export_updates_since(&self, from: &VersionVector) -> Result<Vec<u8>, DocError> {
        self.inner
            .doc
            .export(ExportMode::updates(from))
            .map_err(|err| DocError::Encode(err.to_string()))
    }

    pub fn oplog_vv(&self) -> VersionVector {
        self.inner.doc.oplog_vv()
    }

    /// Import a raw snapshot or update. The mirror follows the subscription.
    pub fn import_bytes(&self, bytes: &[u8]) -> Result<(), DocError> {
        self.inner.doc.import(bytes)?;
        Ok(())
    }

    pub fn view(&self) -> RegistryView {
        self.inner.view.lock().expect("registry view").clone()
    }

    pub fn deep_json(&self) -> serde_json::Value {
        self.inner.doc.get_deep_value().to_json_value()
    }

    pub fn commits(&self) -> u64 {
        self.inner.commits.load(Ordering::Relaxed)
    }

    /// Create or replace index fields. Any device may write a create.
    /// A tombstoned id stays tombstoned. An existing row keeps its owning
    /// `device_id` and the git stamp, which only the owner writes.
    /// `tombstoned` is never written `false`, so a concurrent delete wins.
    pub fn put_space(&self, row: &SpaceRow) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let spaces = doc.get_map("spaces");
        let existing = spaces.get(row.id.as_str()).and_then(as_map);
        if let Some(existing) = &existing
            && map_bool(existing, "tombstoned")
        {
            return Err(DocError::Tombstoned(row.id.clone()));
        }
        let fresh = existing.is_none();
        let space = ensure_map(&spaces, &row.id)?;
        space.insert("id", row.id.as_str())?;
        space.insert("path", row.path.as_str())?;
        space.insert("name", row.name.as_str())?;
        if fresh {
            space.insert("device_id", row.device_id.as_str())?;
            space.insert("git_detected", row.git_detected)?;
            space.insert("checkout_id", row.checkout_id.as_str())?;
            space.insert("repository_id", row.repository_id.as_str())?;
        }
        doc.commit();
        Ok(())
    }

    pub fn rename_space(&self, id: &str, name: &str) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let space = require_space(doc, id)?;
        space.insert("name", name)?;
        doc.commit();
        Ok(())
    }

    /// Git fields for a space. Only the space's `device_id` may write them.
    pub fn set_git_stamp(
        &self,
        actor: &str,
        space_id: &str,
        git_detected: bool,
        checkout_id: &str,
        repository_id: &str,
    ) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let space = require_space(doc, space_id)?;
        let owner = map_str(&space, "device_id");
        if actor != owner {
            return Err(DocError::NotSpaceOwner {
                actor: actor.to_string(),
                owner,
            });
        }
        space.insert("git_detected", git_detected)?;
        space.insert("checkout_id", checkout_id)?;
        space.insert("repository_id", repository_id)?;
        doc.commit();
        Ok(())
    }

    /// Create or replace a chat index row. Any device may create one.
    /// Later status writes go through [`Self::set_chat_status`]. An existing
    /// row keeps its host (`device_id`) and `status`; a chat cannot be put
    /// into a tombstoned space; `tombstoned` is never written `false`.
    pub fn put_chat(&self, row: &ChatRow) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let chats = doc.get_map("chats");
        let existing = chats.get(row.id.as_str()).and_then(as_map);
        if let Some(existing) = &existing
            && map_bool(existing, "tombstoned")
        {
            return Err(DocError::Tombstoned(row.id.clone()));
        }
        if let Some(space) = doc.get_map("spaces").get(row.space_id.as_str()).and_then(as_map)
            && map_bool(&space, "tombstoned")
        {
            return Err(DocError::Tombstoned(row.space_id.clone()));
        }
        let chat = ensure_map(&chats, &row.id)?;
        match existing {
            Some(existing) => {
                let mut kept = row.clone();
                kept.device_id = map_str(&existing, "device_id");
                kept.status = map_str(&existing, "status");
                write_chat(&chat, &kept)?;
            }
            None => write_chat(&chat, row)?,
        }
        doc.commit();
        Ok(())
    }

    /// Add a turn's tokens to the chat's total for that model and record its
    /// context. Only the host writes it: it ran the turn. A later turn that
    /// doesn't report the context keeps the last known one.
    pub fn add_chat_usage(&self, actor: &str, id: &str, usage: &TokenUsage) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let chat = require_chat(doc, id)?;
        let host = map_str(&chat, "device_id");
        if actor != host {
            return Err(DocError::NotChatHost {
                actor: actor.to_string(),
                host,
            });
        }
        let model = if usage.model.is_empty() { map_str(&chat, "model") } else { usage.model.clone() };
        let model = if model.is_empty() { "default".to_string() } else { model };
        let totals = ensure_map(&ensure_map(&chat, "usage")?, &model)?;
        for (key, add) in [
            ("input", usage.input),
            ("output", usage.output),
            ("cache_read", usage.cache_read),
            ("cache_write", usage.cache_write),
            ("cache_warm", usage.cache_warm),
        ] {
            let sum = map_u64(&totals, key).saturating_add(add);
            totals.insert(key, i64::try_from(sum).unwrap_or(i64::MAX))?;
        }
        if let Some(used) = usage.context_used {
            chat.insert("context_used", i64::try_from(used).unwrap_or(i64::MAX))?;
        }
        if let Some(window) = usage.context_window.filter(|w| *w > 0) {
            chat.insert("context_window", i64::try_from(window).unwrap_or(i64::MAX))?;
        }
        doc.commit();
        Ok(())
    }

    /// Git checkout fields of a chat. Only the chat's host writes them.
    pub fn set_chat_checkout(
        &self,
        actor: &str,
        id: &str,
        branch: &str,
        checkout_id: &str,
    ) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let chat = require_chat(doc, id)?;
        let host = map_str(&chat, "device_id");
        if actor != host {
            return Err(DocError::NotChatHost {
                actor: actor.to_string(),
                host,
            });
        }
        chat.insert("branch", branch)?;
        chat.insert("checkout_id", checkout_id)?;
        doc.commit();
        Ok(())
    }

    /// Model and thinking effort of a chat, shown in the composer. Any device
    /// may write them. The harness is not a field here: a chat keeps its own.
    pub fn set_chat_model(&self, id: &str, model: &str, reasoning: &str) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let chat = require_chat(doc, id)?;
        chat.insert("model", model)?;
        chat.insert("reasoning", reasoning)?;
        doc.commit();
        Ok(())
    }

    /// The chat's permission mode for its next turn. Any device may write it.
    pub fn set_chat_permission(&self, id: &str, permission: &str) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let chat = require_chat(doc, id)?;
        chat.insert("permission", permission)?;
        doc.commit();
        Ok(())
    }

    /// Plan mode for this chat's next turn. Any device may write it. It does
    /// not change [`Self::set_chat_permission`].
    pub fn set_chat_plan_mode(&self, id: &str, plan_mode: bool) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let chat = require_chat(doc, id)?;
        chat.insert("plan_mode", plan_mode)?;
        doc.commit();
        Ok(())
    }

    pub fn rename_chat(&self, id: &str, title: &str) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let chat = require_chat(doc, id)?;
        chat.insert("title", title)?;
        doc.commit();
        Ok(())
    }

    pub fn set_archived(&self, id: &str, archived: bool) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let chat = require_chat(doc, id)?;
        chat.insert("archived", archived)?;
        doc.commit();
        Ok(())
    }

    /// Seen-mark. Any device may write it. Status stays as it was.
    pub fn mark_seen(
        &self,
        id: &str,
        last_seen_at: i64,
        last_message_preview: &str,
        last_message_at: i64,
    ) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let chat = require_chat(doc, id)?;
        chat.insert("last_seen_at", last_seen_at)?;
        chat.insert("last_message_preview", last_message_preview)?;
        chat.insert("last_message_at", last_message_at)?;
        doc.commit();
        Ok(())
    }

    /// Liveness and preview, without `last_message_at`: streamed output and
    /// heartbeats call this, so they never move the chat in the sidebar's
    /// order. Only a sent prompt (or creation) bumps `last_message_at`.
    pub fn mark_live(&self, id: &str, last_seen_at: i64, last_message_preview: Option<&str>) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let chat = require_chat(doc, id)?;
        chat.insert("last_seen_at", last_seen_at)?;
        if let Some(preview) = last_message_preview {
            chat.insert("last_message_preview", preview)?;
        }
        doc.commit();
        Ok(())
    }

    /// Status of a chat. Only the chat's host (`device_id`) may write it.
    pub fn set_chat_status(
        &self,
        actor: &str,
        id: &str,
        status: ChatStatus,
    ) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let chat = require_chat(doc, id)?;
        let host = map_str(&chat, "device_id");
        if actor != host {
            return Err(DocError::NotChatHost {
                actor: actor.to_string(),
                host,
            });
        }
        chat.insert("status", status_name(status))?;
        doc.commit();
        Ok(())
    }

    /// A device writes its own row.
    pub fn put_device(&self, actor: &str, row: &DeviceRow) -> Result<(), DocError> {
        if actor != row.id {
            return Err(DocError::NotDevice {
                actor: actor.to_string(),
                device: row.id.clone(),
            });
        }
        let doc = &self.inner.doc;
        let devices = doc.get_map("devices");
        let device = ensure_map(&devices, &row.id)?;
        device.insert("id", row.id.as_str())?;
        device.insert("name", row.name.as_str())?;
        device.insert("os", row.os.as_str())?;
        doc.commit();
        Ok(())
    }

    pub fn put_diff(&self, row: &DiffRow) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let diffs = doc.get_map("diffs");
        let diff = ensure_map(&diffs, &row.checkout_id)?;
        diff.insert("checkout_id", row.checkout_id.as_str())?;
        diff.insert("additions", row.additions)?;
        diff.insert("deletions", row.deletions)?;
        diff.insert("files", row.files)?;
        diff.insert("sha256", row.sha256.as_str())?;
        diff.insert("captured_at", row.captured_at)?;
        doc.commit();
        Ok(())
    }

    /// Tombstone the space and every chat whose `space_id` matches, in one commit.
    /// Session documents are not opened or deleted here.
    pub fn delete_space(&self, space_id: &str) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let _space = require_space(doc, space_id)?;
        let chats = doc.get_map("chats");
        let mut chat_ids = Vec::new();
        chats.for_each(|id, value| {
            let Some(chat) = as_map(value) else {
                return;
            };
            if map_str(&chat, "space_id") == space_id {
                chat_ids.push(id.to_string());
            }
        });
        let spaces = doc.get_map("spaces");
        let space = spaces
            .get(space_id)
            .and_then(as_map)
            .ok_or_else(|| DocError::UnknownSpace(space_id.to_string()))?;
        space.insert("tombstoned", true)?;
        for id in &chat_ids {
            if let Some(chat) = chats.get(id.as_str()).and_then(as_map) {
                chat.insert("tombstoned", true)?;
            }
        }
        doc.commit();
        Ok(())
    }

    /// Tombstone chats, in one commit. Unknown ids fail before anything changes.
    /// Their session docs stay on disk, as with `delete_space`.
    pub fn delete_chats(&self, ids: &[String]) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let chats = ids.iter().map(|id| require_chat(doc, id)).collect::<Result<Vec<_>, _>>()?;
        for chat in chats {
            chat.insert("tombstoned", true)?;
        }
        doc.commit();
        Ok(())
    }

    fn from_attached(doc: LoroDoc) -> Result<Self, DocError> {
        let view = read_registry(&doc);
        let inner = Arc::new(Inner {
            doc,
            view: Mutex::new(view),
            commits: AtomicU64::new(0),
        });
        let sub_inner = Arc::clone(&inner);
        let sub = inner.doc.subscribe_root(Arc::new(move |_event| {
            let next = read_registry(&sub_inner.doc);
            *sub_inner.view.lock().expect("registry view") = next;
            sub_inner.commits.fetch_add(1, Ordering::Relaxed);
        }));
        Ok(Self { _sub: sub, inner })
    }
}

fn check_schema(doc: &LoroDoc) -> Result<(), DocError> {
    let schema = match doc.get_map("meta").get("schema") {
        Some(loro::ValueOrContainer::Value(loro::LoroValue::I64(schema))) => schema,
        _ => return Err(DocError::SchemaMissing),
    };
    if schema > SCHEMA_VERSION {
        return Err(DocError::SchemaTooNew {
            found: schema,
            known: SCHEMA_VERSION,
        });
    }
    Ok(())
}

fn status_name(status: ChatStatus) -> &'static str {
    match status {
        ChatStatus::Idle => "idle",
        ChatStatus::Running => "running",
        ChatStatus::AwaitingInput => "awaiting_input",
        ChatStatus::Aborted => "aborted",
    }
}

fn ensure_map(parent: &LoroMap, key: &str) -> Result<LoroMap, DocError> {
    if let Some(existing) = parent.get(key).and_then(as_map) {
        return Ok(existing);
    }
    Ok(parent.insert_container(key, LoroMap::new())?)
}

fn require_space(doc: &LoroDoc, id: &str) -> Result<LoroMap, DocError> {
    let spaces = doc.get_map("spaces");
    let Some(space) = spaces.get(id).and_then(as_map) else {
        return Err(DocError::UnknownSpace(id.to_string()));
    };
    if map_bool(&space, "tombstoned") {
        return Err(DocError::Tombstoned(id.to_string()));
    }
    Ok(space)
}

fn require_chat(doc: &LoroDoc, id: &str) -> Result<LoroMap, DocError> {
    let chats = doc.get_map("chats");
    let Some(chat) = chats.get(id).and_then(as_map) else {
        return Err(DocError::UnknownChat(id.to_string()));
    };
    if map_bool(&chat, "tombstoned") {
        return Err(DocError::Tombstoned(id.to_string()));
    }
    Ok(chat)
}

fn write_chat(chat: &LoroMap, row: &ChatRow) -> Result<(), DocError> {
    chat.insert("id", row.id.as_str())?;
    chat.insert("device_id", row.device_id.as_str())?;
    chat.insert("title", row.title.as_str())?;
    chat.insert("archived", row.archived)?;
    chat.insert("cwd", row.cwd.as_str())?;
    chat.insert("branch", row.branch.as_str())?;
    chat.insert("checkout_id", row.checkout_id.as_str())?;
    chat.insert("space_id", row.space_id.as_str())?;
    chat.insert("last_seen_at", row.last_seen_at)?;
    chat.insert("last_message_preview", row.last_message_preview.as_str())?;
    chat.insert("last_message_at", row.last_message_at)?;
    chat.insert("harness", row.harness.as_str())?;
    chat.insert("model", row.model.as_str())?;
    chat.insert("reasoning", row.reasoning.as_str())?;
    chat.insert("permission", row.permission.as_str())?;
    chat.insert("plan_mode", row.plan_mode)?;
    chat.insert("status", row.status.as_str())?;
    Ok(())
}

fn read_registry(doc: &LoroDoc) -> RegistryView {
    let mut spaces = Vec::new();
    doc.get_map("spaces").for_each(|_, value| {
        let Some(map) = as_map(value) else {
            return;
        };
        if map_bool(&map, "tombstoned") {
            return;
        }
        spaces.push(SpaceRow {
            id: map_str(&map, "id"),
            device_id: map_str(&map, "device_id"),
            path: map_str(&map, "path"),
            name: map_str(&map, "name"),
            git_detected: map_bool(&map, "git_detected"),
            checkout_id: map_str(&map, "checkout_id"),
            repository_id: map_str(&map, "repository_id"),
        });
    });
    let mut chats = Vec::new();
    let space_map = doc.get_map("spaces");
    doc.get_map("chats").for_each(|_, value| {
        let Some(map) = as_map(value) else {
            return;
        };
        if map_bool(&map, "tombstoned") {
            return;
        }
        // A chat put concurrently with its space's delete still disappears.
        let space_id = map_str(&map, "space_id");
        if space_map
            .get(space_id.as_str())
            .and_then(as_map)
            .is_some_and(|space| map_bool(&space, "tombstoned"))
        {
            return;
        }
        chats.push(read_chat(&map));
    });
    let mut devices = Vec::new();
    doc.get_map("devices").for_each(|_, value| {
        let Some(map) = as_map(value) else {
            return;
        };
        devices.push(DeviceRow {
            id: map_str(&map, "id"),
            name: map_str(&map, "name"),
            os: map_str(&map, "os"),
        });
    });
    let mut diffs = Vec::new();
    doc.get_map("diffs").for_each(|_, value| {
        let Some(map) = as_map(value) else {
            return;
        };
        diffs.push(DiffRow {
            checkout_id: map_str(&map, "checkout_id"),
            additions: map_i64(&map, "additions"),
            deletions: map_i64(&map, "deletions"),
            files: map_i64(&map, "files"),
            sha256: map_str(&map, "sha256"),
            captured_at: map_i64(&map, "captured_at"),
        });
    });
    spaces.sort_by(|a, b| a.id.cmp(&b.id));
    chats.sort_by(|a, b| a.id.cmp(&b.id));
    devices.sort_by(|a, b| a.id.cmp(&b.id));
    diffs.sort_by(|a, b| a.checkout_id.cmp(&b.checkout_id));
    RegistryView {
        spaces,
        chats,
        devices,
        diffs,
    }
}

fn read_chat(map: &LoroMap) -> ChatRow {
    ChatRow {
        id: map_str(map, "id"),
        device_id: map_str(map, "device_id"),
        title: map_str(map, "title"),
        archived: map_bool(map, "archived"),
        cwd: map_str(map, "cwd"),
        branch: map_str(map, "branch"),
        checkout_id: map_str(map, "checkout_id"),
        space_id: map_str(map, "space_id"),
        last_seen_at: map_i64(map, "last_seen_at"),
        last_message_preview: map_str(map, "last_message_preview"),
        last_message_at: map_i64(map, "last_message_at"),
        harness: map_str(map, "harness"),
        model: map_str(map, "model"),
        reasoning: map_str(map, "reasoning"),
        permission: map_str(map, "permission"),
        plan_mode: map_bool(map, "plan_mode"),
        status: map_str(map, "status"),
        usage: read_usage(map),
    }
}

fn read_usage(chat: &LoroMap) -> ChatUsage {
    let mut models = Vec::new();
    if let Some(usage) = chat.get("usage").and_then(as_map) {
        usage.for_each(|model, value| {
            if let Some(m) = as_map(value) {
                models.push(ModelTokens {
                    model: model.to_string(),
                    input: map_u64(&m, "input"),
                    output: map_u64(&m, "output"),
                    cache_read: map_u64(&m, "cache_read"),
                    cache_write: map_u64(&m, "cache_write"),
                    cache_warm: map_u64(&m, "cache_warm"),
                });
            }
        });
    }
    models.sort_by(|a, b| a.model.cmp(&b.model));
    ChatUsage {
        models,
        context_used: map_u64(chat, "context_used"),
        context_window: map_u64(chat, "context_window"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{NewSession, SessionDoc};
    use keel_proto::HarnessId;

    fn space(id: &str, device: &str) -> SpaceRow {
        SpaceRow {
            id: id.into(),
            device_id: device.into(),
            path: format!("/work/{id}"),
            name: id.into(),
            git_detected: false,
            checkout_id: String::new(),
            repository_id: String::new(),
        }
    }

    fn chat(id: &str, space_id: &str, device: &str) -> ChatRow {
        ChatRow {
            id: id.into(),
            device_id: device.into(),
            title: id.into(),
            archived: false,
            cwd: format!("/work/{space_id}"),
            branch: String::new(),
            checkout_id: String::new(),
            space_id: space_id.into(),
            last_seen_at: 1,
            last_message_preview: String::new(),
            last_message_at: 1,
            harness: "pi".into(),
            model: "pi".into(),
            reasoning: String::new(),
            permission: String::new(),
            plan_mode: false,
            status: "idle".into(),
            usage: Default::default(),
        }
    }

    #[test]
    fn delete_space_tombstones_one_commit_and_leaves_the_session() {
        let registry = RegistryDoc::create().unwrap();
        registry.put_space(&space("s1", "host")).unwrap();
        registry.put_space(&space("s2", "host")).unwrap();
        registry.put_chat(&chat("c1", "s1", "host")).unwrap();
        registry.put_chat(&chat("c2", "s2", "host")).unwrap();
        registry.set_chat_model("c1", "pi", "high").unwrap();
        assert_eq!(registry.view().chats.iter().find(|c| c.id == "c1").unwrap().reasoning, "high");

        let session = SessionDoc::create(NewSession {
            chat_id: "c1".into(),
            host_device: "host".into(),
            space_id: "s1".into(),
            harness: HarnessId::Keel,
            cwd: "/work/s1".into(),
            model: "pi".into(),
            reasoning: String::new(),
            native_session: String::new(),
            created_at: 1,
        })
        .unwrap();
        session.append_message("m", "user", 1).unwrap();
        session.append_text("m", "p", "kept").unwrap();
        let snapshot = session.export_snapshot().unwrap();

        let before = registry.commits();
        registry.delete_space("s1").unwrap();
        assert_eq!(registry.commits(), before + 1);

        let view = registry.view();
        assert!(view.spaces.iter().all(|row| row.id != "s1"));
        assert!(view.chats.iter().all(|row| row.id != "c1"));
        assert!(view.spaces.iter().any(|row| row.id == "s2"));
        assert!(view.chats.iter().any(|row| row.id == "c2"));

        let json = registry.deep_json();
        assert_eq!(json["spaces"]["s1"]["tombstoned"], true);
        assert_eq!(json["chats"]["c1"]["tombstoned"], true);
        assert_ne!(json["chats"]["c2"]["tombstoned"], true);

        let opened = SessionDoc::open(&snapshot).unwrap();
        assert_eq!(opened.view().messages[0].text, "kept");
        assert_eq!(session.export_snapshot().unwrap(), snapshot);
    }

    #[test]
    fn delete_chats_tombstones_only_those_in_one_commit() {
        let registry = RegistryDoc::create().unwrap();
        registry.put_space(&space("s1", "host")).unwrap();
        for id in ["c1", "c2", "c3"] {
            registry.put_chat(&chat(id, "s1", "host")).unwrap();
        }
        let before = registry.commits();
        registry.delete_chats(&["c1".into(), "c3".into()]).unwrap();
        assert_eq!(registry.commits(), before + 1);
        let ids: Vec<_> = registry.view().chats.into_iter().map(|row| row.id).collect();
        assert_eq!(ids, vec!["c2"]);
        // One unknown id: nothing changes.
        assert!(registry.delete_chats(&["c2".into(), "nope".into()]).is_err());
        assert_eq!(registry.view().chats.len(), 1);
    }

    #[test]
    fn writer_discipline_for_device_status_and_git() {
        let registry = RegistryDoc::create().unwrap();
        registry.put_space(&space("s1", "host")).unwrap();
        registry.put_chat(&chat("c1", "s1", "host")).unwrap();

        let err = registry
            .put_device(
                "other",
                &DeviceRow {
                    id: "host".into(),
                    name: "Host".into(),
                    os: "macos".into(),
                },
            )
            .unwrap_err();
        assert!(matches!(err, DocError::NotDevice { .. }));

        registry
            .put_device(
                "host",
                &DeviceRow {
                    id: "host".into(),
                    name: "Host".into(),
                    os: "macos".into(),
                },
            )
            .unwrap();
        assert_eq!(registry.view().devices[0].name, "Host");

        let err = registry
            .set_chat_status("other", "c1", ChatStatus::Running)
            .unwrap_err();
        assert!(matches!(err, DocError::NotChatHost { .. }));
        assert_eq!(registry.view().chats[0].status, "idle");

        registry
            .set_chat_status("host", "c1", ChatStatus::Running)
            .unwrap();
        assert_eq!(registry.view().chats[0].status, "running");

        let err = registry
            .set_git_stamp("other", "s1", true, "co", "repo")
            .unwrap_err();
        assert!(matches!(err, DocError::NotSpaceOwner { .. }));
        registry
            .set_git_stamp("host", "s1", true, "co", "repo")
            .unwrap();
        let space = &registry.view().spaces[0];
        assert!(space.git_detected);
        assert_eq!(space.checkout_id, "co");
        assert_eq!(space.repository_id, "repo");

        registry.rename_chat("c1", "Renamed").unwrap();
        registry.set_archived("c1", true).unwrap();
        registry.mark_seen("c1", 50, "hi", 40).unwrap();
        let chat = &registry.view().chats[0];
        assert_eq!(chat.title, "Renamed");
        assert!(chat.archived);
        assert_eq!(chat.last_seen_at, 50);
        assert_eq!(chat.last_message_preview, "hi");
        assert_eq!(chat.status, "running");
    }

    #[test]
    fn newer_registry_schema_refuses_to_open() {
        let raw = LoroDoc::new();
        raw.get_map("meta").insert("schema", 2_i64).unwrap();
        raw.commit();
        let bytes = raw.export(ExportMode::Snapshot).unwrap();
        match RegistryDoc::open(&bytes) {
            Err(DocError::SchemaTooNew { found: 2, known: 1 }) => {}
            Err(err) => panic!("unexpected error: {err}"),
            Ok(_) => panic!("opened a newer schema"),
        }
    }

    #[test]
    fn put_keeps_host_and_status_and_a_concurrent_delete_wins() {
        let a = RegistryDoc::create().unwrap();
        a.put_space(&space("s1", "host")).unwrap();
        a.put_chat(&chat("c1", "s1", "host")).unwrap();
        a.set_chat_status("host", "c1", ChatStatus::Running).unwrap();

        let mut hijack = chat("c1", "s1", "other");
        hijack.status = "idle".into();
        hijack.title = "renamed".into();
        a.put_chat(&hijack).unwrap();
        let row = &a.view().chats[0];
        assert_eq!(row.device_id, "host");
        assert_eq!(row.status, "running");
        assert_eq!(row.title, "renamed");

        a.put_space(&space("s1", "other")).unwrap();
        assert_eq!(a.view().spaces[0].device_id, "host");

        // B learns the rows, then A deletes while B re-puts the chat.
        let b = RegistryDoc::from_import(&a.export_snapshot().unwrap()).unwrap();
        a.delete_space("s1").unwrap();
        b.put_chat(&chat("c1", "s1", "host")).unwrap();
        b.put_chat(&chat("c2", "s1", "host")).unwrap();
        a.import_bytes(&b.export_snapshot().unwrap()).unwrap();
        b.import_bytes(&a.export_snapshot().unwrap()).unwrap();
        for doc in [&a, &b] {
            let view = doc.view();
            assert!(view.chats.is_empty(), "{view:?}");
            assert!(view.spaces.is_empty());
        }
        assert!(matches!(
            a.put_chat(&chat("c3", "s1", "host")),
            Err(DocError::Tombstoned(_))
        ));
    }

    #[test]
    fn usage_adds_per_model_and_keeps_the_last_context() {
        let registry = RegistryDoc::create().unwrap();
        registry.put_space(&space("s1", "host")).unwrap();
        registry.put_chat(&chat("c1", "s1", "host")).unwrap();
        let turn = |model: &str, input, context_used, context_window| TokenUsage {
            model: model.into(),
            input,
            output: 10,
            cache_read: 100,
            cache_write: 0,
            cache_warm: 0,
            context_used,
            context_window,
        };
        registry.add_chat_usage("host", "c1", &turn("", 5, Some(115), Some(1000))).unwrap();
        registry.add_chat_usage("host", "c1", &turn("", 7, None, None)).unwrap();
        registry.add_chat_usage("host", "c1", &turn("other", 1, Some(300), None)).unwrap();
        assert!(registry.add_chat_usage("guest", "c1", &turn("", 1, None, None)).is_err());
        // put_chat (a rename, a re-create) leaves the totals alone.
        registry.put_chat(&chat("c1", "s1", "host")).unwrap();
        let usage = registry.view().chats[0].usage.clone();
        // An empty model is the chat's own ("pi").
        assert_eq!(usage.models.iter().map(|m| (m.model.as_str(), m.input, m.output, m.cache_read)).collect::<Vec<_>>(), [
            ("other", 1, 10, 100),
            ("pi", 12, 20, 200),
        ]);
        assert_eq!((usage.context_used, usage.context_window), (300, 1000));
    }

    #[test]
    fn plan_mode_is_off_until_saved_and_does_not_change_permission() {
        let registry = RegistryDoc::create().unwrap();
        registry.put_space(&space("s1", "host")).unwrap();
        registry.put_chat(&chat("c1", "s1", "host")).unwrap();
        registry.put_chat(&chat("c2", "s1", "host")).unwrap();
        let row = |id: &str| {
            registry
                .view()
                .chats
                .into_iter()
                .find(|c| c.id == id)
                .unwrap()
        };
        assert!(!row("c1").plan_mode && row("c1").permission.is_empty());
        registry.set_chat_plan_mode("c1", true).unwrap();
        registry.set_chat_permission("c1", "auto_edit").unwrap();
        let saved = RegistryDoc::from_import(&registry.export_snapshot().unwrap()).unwrap();
        let c1 = saved.view().chats.into_iter().find(|c| c.id == "c1").unwrap();
        let c2 = saved.view().chats.into_iter().find(|c| c.id == "c2").unwrap();
        assert!(c1.plan_mode);
        assert_eq!(c1.permission, "auto_edit");
        assert!(!c2.plan_mode);
    }
}

//! One Loro document per chat: meta, messages, and the command ledger.
//!
//! Text parts hold a `LoroText`. A part that would pass [`CONTINUATION_SPLIT`]
//! is sealed and the rest is written to a new part. Readers concatenate text
//! parts in order. The mirror replaces only the message arcs a diff names.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use keel_proto::{Attachment, HarnessId, UserCommand};
use loro::{
    Container, ContainerID, ContainerTrait, ExportMode, Index, LoroDoc, LoroList, LoroMap,
    LoroText, Subscription, ToJson, ValueOrContainer, VersionVector,
};
use uuid::Uuid;

use crate::error::DocError;
use crate::limits::{COMMAND_TTL_MS, CONTINUATION_SPLIT, SCHEMA_VERSION, tool_part_summary};
use crate::loro_util::{as_map, map_i64, map_opt_i64, map_opt_str, map_str, map_u64};

/// Message id of the mechanical retention archive. Later passes leave it in place.
pub const ARCHIVE_MESSAGE_ID: &str = "keel-archive";

/// Body of the single archive part written by [`SessionDoc::collapse_before`].
pub const ARCHIVE_TEXT: &str = "earlier turns omitted";

/// Fields written once when a session document is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub chat_id: String,
    pub host_device: String,
    pub space_id: String,
    pub harness: HarnessId,
    pub cwd: String,
    pub model: String,
    pub reasoning: String,
    pub native_session: String,
    pub created_at: i64,
}

/// `meta` as the mirror last read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub chat_id: String,
    pub host_device: String,
    pub space_id: String,
    pub harness: String,
    pub cwd: String,
    pub model: String,
    pub reasoning: String,
    pub native_session: String,
    pub created_at: i64,
    pub schema: i64,
}

/// A render row for a tool call. The summary is already capped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolRow {
    pub tool_id: String,
    pub name: String,
    pub state: String,
    pub summary: String,
    pub input_bytes: u64,
    pub output_bytes: u64,
    /// What the call works on (file, command, ...). Empty on a finish: the
    /// start's detail stays.
    pub detail: String,
    /// Short agent-written progress label. Empty on a finish: the start's
    /// label stays. Absent on older rows.
    pub label: String,
    /// Absolute paths of images the tool returned (screenshots), shown
    /// inline under the row. Paths only: never image bytes.
    pub images: Vec<String>,
    /// ACP kind (`read`, `execute`, …). Empty when the harness didn't say.
    /// Stored as `tool_kind`: the part's own `kind` is `"tool"`.
    pub kind: String,
    /// When the call started, unix millis. Absent on older rows.
    pub started_ms: Option<i64>,
    /// When the call finished or was cancelled.
    pub ended_ms: Option<i64>,
    /// JSON of one diff (`path`, `old_text`, `new_text`). Empty when none.
    pub diff: String,
    /// Last streamed output. A finish does not clear a tail already stored.
    pub output_tail: String,
}

/// One plan row. `priority` is absent when the harness didn't send one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntryView {
    pub content: String,
    pub status: String,
    pub priority: Option<String>,
}

/// The parent transcript's row for a spawned subagent: the inline
/// indicator beside thinking and tool rows. Details live in the
/// `subagents` map; this row keeps what renders at a glance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubagentRow {
    pub subagent_id: String,
    pub name: String,
    pub state: String,
    pub summary: String,
}

/// One subagent of this session, from the `subagents` map. `status`
/// strings match [`ToolRow::state`]: `running`, `blocked`, `done`,
/// `error`, `interrupted`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubagentView {
    pub id: String,
    /// The spawn tool call's id in the parent transcript.
    pub parent_tool_id: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub created_at: i64,
    pub ended_at: Option<i64>,
    pub last_summary: String,
    pub blocked_question: Option<String>,
}

/// One part of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartView {
    Text { id: String, body: String },
    /// The model's thinking. Kept out of `MessageView::text`.
    Reasoning { id: String, body: String },
    Tool(ToolRow),
    /// "Spawned subagent X": the transcript indicator. The subagent's
    /// own transcript lives under `subagents.{subagent_id}`.
    Subagent {
        id: String,
        subagent_id: String,
        name: String,
        state: String,
        summary: String,
    },
    /// Generative UI the host chose for this message (a chart, ...), as
    /// one JSON spec string. Readers that don't know a component skip it.
    Ui { id: String, spec: String },
    /// The turn's plan, replaced whole on each update. One part per message.
    Plan { entries: Vec<PlanEntryView> },
}

/// One transcript row. `text` is the text parts concatenated in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageView {
    pub id: String,
    pub role: String,
    pub created_at: i64,
    pub parts: Vec<PartView>,
    pub text: String,
    /// Images on a user message.
    pub attachments: Vec<Attachment>,
}

/// One ledger entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandView {
    pub command_id: Uuid,
    pub kind: String,
    pub origin_device: String,
    pub created_at: i64,
    pub message_id: String,
    pub text: String,
    pub question_id: String,
    pub answers: String,
    /// Images on a send.
    pub attachments: Vec<Attachment>,
    pub outcome: Option<String>,
    /// False for an implement turn the transcript does not draw. Absent in
    /// older commands, which are shown.
    pub shown: bool,
}

/// Typed mirror kept next to the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    pub meta: SessionMeta,
    pub messages: Vec<Arc<MessageView>>,
    pub commands: Vec<CommandView>,
    /// The session's subagents, in map order (insertion order).
    pub subagents: Vec<SubagentView>,
    /// Times each message slot was materialized. A shifted row keeps its count.
    pub(crate) materialized: Vec<u64>,
}

/// Outcome the host writes onto a ledger entry. A set outcome stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Processed,
    Rejected,
    Error,
    Superseded,
    Expired,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Processed => "processed",
            Self::Rejected => "rejected",
            Self::Error => "error",
            Self::Superseded => "superseded",
            Self::Expired => "expired",
        }
    }
}

struct Inner {
    doc: LoroDoc,
    view: Mutex<SessionView>,
    messages_id: ContainerID,
    commands_id: ContainerID,
    meta_id: ContainerID,
    subagents_id: ContainerID,
    commits: AtomicU64,
}

/// Session document plus its mirror.
pub struct SessionDoc {
    _sub: Subscription,
    inner: Arc<Inner>,
}

impl SessionDoc {
    pub fn create(fields: NewSession) -> Result<Self, DocError> {
        let doc = LoroDoc::new();
        write_new(&doc, &fields)?;
        doc.commit();
        Self::from_attached(doc)
    }

    /// Import a snapshot. A `meta.schema` greater than [`SCHEMA_VERSION`] refuses.
    pub fn open(bytes: &[u8]) -> Result<Self, DocError> {
        let doc = LoroDoc::from_snapshot(bytes)?;
        let schema = read_schema(&doc)?;
        if schema > SCHEMA_VERSION {
            return Err(DocError::SchemaTooNew {
                found: schema,
                known: SCHEMA_VERSION,
            });
        }
        Self::from_attached(doc)
    }

    /// Import a snapshot or an update blob into a new document.
    pub fn from_import(bytes: &[u8]) -> Result<Self, DocError> {
        let doc = LoroDoc::new();
        doc.import(bytes)?;
        let schema = read_schema(&doc)?;
        if schema > SCHEMA_VERSION {
            return Err(DocError::SchemaTooNew {
                found: schema,
                known: SCHEMA_VERSION,
            });
        }
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

    /// State-only snapshot. History before the current frontiers is dropped.
    pub fn export_shallow(&self) -> Result<Vec<u8>, DocError> {
        let frontiers = self.inner.doc.state_frontiers();
        self.inner
            .doc
            .export(ExportMode::state_only(Some(&frontiers)))
            .map_err(|err| DocError::Encode(err.to_string()))
    }

    /// Drop user and assistant messages older than `cutoff` and clear older tool
    /// summaries. Inserts one archive message when any message was removed.
    /// The archive id is [`ARCHIVE_MESSAGE_ID`]. A later pass leaves it in place.
    pub fn collapse_before(&self, cutoff: i64) -> Result<bool, DocError> {
        let view = self.view();
        let mut remove = Vec::new();
        let mut changed = false;
        for (index, message) in view.messages.iter().enumerate() {
            if message.id == ARCHIVE_MESSAGE_ID {
                continue;
            }
            if message.created_at >= cutoff {
                continue;
            }
            if message.role == "user" || message.role == "assistant" {
                remove.push(index);
                continue;
            }
            for part in &message.parts {
                let PartView::Tool(row) = part else {
                    continue;
                };
                if row.summary.is_empty() {
                    continue;
                }
                let mut cleared = row.clone();
                cleared.summary.clear();
                self.upsert_tool(&message.id, &cleared)?;
                changed = true;
            }
        }
        if remove.is_empty() {
            return Ok(changed);
        }
        let doc = &self.inner.doc;
        let messages = doc.get_list("messages");
        for index in remove.into_iter().rev() {
            messages.delete(index, 1)?;
        }
        doc.commit();
        if message_pos(doc, ARCHIVE_MESSAGE_ID).is_none() {
            let messages = doc.get_list("messages");
            let message = messages.insert_container(0, LoroMap::new())?;
            message.insert("id", ARCHIVE_MESSAGE_ID)?;
            message.insert("role", "assistant")?;
            message.insert("created_at", cutoff)?;
            message.insert_container("parts", LoroList::new())?;
            doc.commit();
            self.append_text(ARCHIVE_MESSAGE_ID, "archive", ARCHIVE_TEXT)?;
        }
        Ok(true)
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

    pub fn view(&self) -> SessionView {
        self.inner.view.lock().expect("session view").clone()
    }

    pub fn deep_json(&self) -> serde_json::Value {
        self.inner.doc.get_deep_value().to_json_value()
    }

    /// Commits applied since this handle subscribed.
    pub fn commits(&self) -> u64 {
        self.inner.commits.load(Ordering::Relaxed)
    }

    /// Record the images a user message carries.
    pub fn set_attachments(&self, message_id: &str, attachments: &[Attachment]) -> Result<(), DocError> {
        if attachments.is_empty() {
            return Ok(());
        }
        let doc = &self.inner.doc;
        let index = require_message(doc, message_id)?;
        let message = doc
            .get_list("messages")
            .get(index)
            .and_then(as_map)
            .ok_or_else(|| DocError::UnknownMessage(message_id.to_string()))?;
        message.insert("attachments", encode_attachments(attachments)?.as_str())?;
        doc.commit();
        Ok(())
    }

    pub fn append_message(&self, id: &str, role: &str, created_at: i64) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        if message_pos(doc, id).is_some() {
            return Err(DocError::DuplicateMessage(id.to_string()));
        }
        let messages = doc.get_list("messages");
        let message = messages.push_container(LoroMap::new())?;
        message.insert("id", id)?;
        message.insert("role", role)?;
        message.insert("created_at", created_at)?;
        message.insert_container("parts", LoroList::new())?;
        doc.commit();
        Ok(())
    }

    /// Append text to `part_id`. Bytes past [`CONTINUATION_SPLIT`] go to a new part.
    pub fn append_text(
        &self,
        message_id: &str,
        part_id: &str,
        delta: &str,
    ) -> Result<(), DocError> {
        self.append_body(TEXT, message_id, part_id, delta)
    }

    /// Like [`Self::append_text`], into a `reasoning` part.
    pub fn append_reasoning(&self, message_id: &str, part_id: &str, delta: &str) -> Result<(), DocError> {
        self.append_body(REASONING, message_id, part_id, delta)
    }

    fn append_body(&self, kind: &str, message_id: &str, part_id: &str, delta: &str) -> Result<(), DocError> {
        if delta.is_empty() {
            return Ok(());
        }
        let doc = &self.inner.doc;
        let index = require_message(doc, message_id)?;
        let parts = parts_of(doc, index)?;
        let mut rest = delta;
        while !rest.is_empty() {
            let text = open_text_part_in(&parts, kind, part_id)?;
            let len = text.len_utf8();
            let room = CONTINUATION_SPLIT.saturating_sub(len);
            let (head, tail) = split_utf8_prefix(rest, room);
            if head.is_empty() {
                drop(text);
                create_text_part_in(&parts, kind, part_id)?;
                continue;
            }
            text.insert_utf8(len, head)?;
            drop(text);
            rest = tail;
        }
        doc.commit();
        Ok(())
    }

    /// Store a tool render row. The summary keeps the first non-empty line, capped at 160 chars.
    pub fn upsert_tool(&self, message_id: &str, row: &ToolRow) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let index = require_message(doc, message_id)?;
        let summary = tool_part_summary(&row.name, &row.summary);
        let parts = parts_of(doc, index)?;
        if let Some(existing) = find_tool(&parts, &row.tool_id) {
            write_tool(&existing, row, &summary)?;
        } else {
            let part = parts.push_container(LoroMap::new())?;
            part.insert("kind", "tool")?;
            part.insert("tool_id", row.tool_id.as_str())?;
            write_tool(&part, row, &summary)?;
        }
        doc.commit();
        Ok(())
    }

    /// Replace this message's plan. One part, id `"plan"`.
    pub fn upsert_plan(&self, message_id: &str, entries_json: &str) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let index = require_message(doc, message_id)?;
        write_plan(&parts_of(doc, index)?, entries_json)?;
        doc.commit();
        Ok(())
    }

    /// Create the subagent record when missing; update nothing else. The
    /// engine calls this on any subagent-scoped event, which can arrive
    /// before the spawn is announced.
    pub fn ensure_subagent(&self, id: &str, created_at: i64) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let subagents = doc.get_map("subagents");
        if subagents.get(id).is_some() {
            return Ok(());
        }
        let entry = subagents.insert_container(id, LoroMap::new())?;
        entry.insert("id", id)?;
        entry.insert("parent_tool_id", id)?;
        entry.insert("status", "running")?;
        entry.insert("created_at", created_at)?;
        entry.insert_container("parts", LoroList::new())?;
        doc.commit();
        Ok(())
    }

    /// Write the whole subagent record. The engine keeps its own copy and
    /// upserts it, so field-level APIs stay out of the surface.
    pub fn upsert_subagent(&self, view: &SubagentView) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let subagents = doc.get_map("subagents");
        let entry = match subagents.get(&view.id) {
            Some(ValueOrContainer::Container(Container::Map(entry))) => entry,
            _ => {
                let entry = subagents.insert_container(&view.id, LoroMap::new())?;
                entry.insert("id", view.id.as_str())?;
                entry.insert_container("parts", LoroList::new())?;
                entry
            }
        };
        entry.insert("parent_tool_id", view.parent_tool_id.as_str())?;
        entry.insert("name", view.name.as_str())?;
        entry.insert("description", view.description.as_str())?;
        entry.insert("status", view.status.as_str())?;
        entry.insert("created_at", view.created_at)?;
        if let Some(ended_at) = view.ended_at {
            entry.insert("ended_at", ended_at)?;
        }
        entry.insert("last_summary", view.last_summary.as_str())?;
        if let Some(question) = view.blocked_question.as_deref() {
            entry.insert("blocked_question", question)?;
        }
        doc.commit();
        Ok(())
    }

    /// Append text to a subagent's transcript, split like message text.
    pub fn append_sub_text(&self, subagent_id: &str, part_id: &str, delta: &str) -> Result<(), DocError> {
        self.append_sub_body(TEXT, subagent_id, part_id, delta)
    }

    /// Like [`Self::append_sub_text`], into a `reasoning` part.
    pub fn append_sub_reasoning(&self, subagent_id: &str, part_id: &str, delta: &str) -> Result<(), DocError> {
        self.append_sub_body(REASONING, subagent_id, part_id, delta)
    }

    fn append_sub_body(&self, kind: &str, subagent_id: &str, part_id: &str, delta: &str) -> Result<(), DocError> {
        if delta.is_empty() {
            return Ok(());
        }
        let doc = &self.inner.doc;
        let parts = require_sub_parts(doc, subagent_id)?;
        let mut rest = delta;
        while !rest.is_empty() {
            let text = open_text_part_in(&parts, kind, part_id)?;
            let len = text.len_utf8();
            let room = CONTINUATION_SPLIT.saturating_sub(len);
            let (head, tail) = split_utf8_prefix(rest, room);
            if head.is_empty() {
                drop(text);
                create_text_part_in(&parts, kind, part_id)?;
                continue;
            }
            text.insert_utf8(len, head)?;
            drop(text);
            rest = tail;
        }
        doc.commit();
        Ok(())
    }

    /// Store a tool row in a subagent's transcript.
    pub fn upsert_sub_tool(&self, subagent_id: &str, row: &ToolRow) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let parts = require_sub_parts(doc, subagent_id)?;
        let summary = tool_part_summary(&row.name, &row.summary);
        if let Some(existing) = find_tool(&parts, &row.tool_id) {
            write_tool(&existing, row, &summary)?;
        } else {
            let part = parts.push_container(LoroMap::new())?;
            part.insert("kind", "tool")?;
            part.insert("tool_id", row.tool_id.as_str())?;
            write_tool(&part, row, &summary)?;
        }
        doc.commit();
        Ok(())
    }

    /// Replace a subagent's plan. One part, id `"plan"`.
    pub fn upsert_sub_plan(&self, subagent_id: &str, entries_json: &str) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        write_plan(&require_sub_parts(doc, subagent_id)?, entries_json)?;
        doc.commit();
        Ok(())
    }

    /// Tools still `running` in these messages and subagents become
    /// `cancelled`. Summary and the other fields stay. Returns the ids
    /// that changed.
    pub fn cancel_running_tools(
        &self,
        message_ids: &[String],
        subagent_ids: &[String],
        ended_ms: i64,
    ) -> Result<(Vec<String>, Vec<String>), DocError> {
        let doc = &self.inner.doc;
        let mut messages = Vec::new();
        let mut subs = Vec::new();
        for id in message_ids {
            let Ok(index) = require_message(doc, id) else { continue };
            let Ok(parts) = parts_of(doc, index) else { continue };
            if cancel_running_in(&parts, ended_ms)? {
                messages.push(id.clone());
            }
        }
        for id in subagent_ids {
            let Ok(parts) = require_sub_parts(doc, id) else { continue };
            if cancel_running_in(&parts, ended_ms)? {
                subs.push(id.clone());
            }
        }
        if !messages.is_empty() || !subs.is_empty() {
            doc.commit();
        }
        Ok((messages, subs))
    }

    /// A subagent's transcript parts, newest first is not needed: stream order.
    pub fn sub_parts(&self, subagent_id: &str) -> Result<Vec<PartView>, DocError> {
        let doc = &self.inner.doc;
        let parts = require_sub_parts(doc, subagent_id)?;
        Ok(read_parts(&parts))
    }

    /// The parent transcript's indicator row for a spawned subagent.
    pub fn upsert_subagent_part(&self, message_id: &str, row: &SubagentRow) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let index = require_message(doc, message_id)?;
        let parts = parts_of(doc, index)?;
        if let Some(existing) = find_sub_part(&parts, &row.subagent_id) {
            write_sub_part(&existing, row)?;
        } else {
            let id = format!("sub-{}", row.subagent_id);
            let part = parts.push_container(LoroMap::new())?;
            part.insert("kind", "subagent")?;
            part.insert("id", id.as_str())?;
            part.insert("subagent_id", row.subagent_id.as_str())?;
            write_sub_part(&part, row)?;
        }
        doc.commit();
        Ok(())
    }

    /// Store a generative UI part on `message_id`, replacing one with the same id.
    pub fn put_ui(&self, message_id: &str, id: &str, spec: &str) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let index = require_message(doc, message_id)?;
        let parts = parts_of(doc, index)?;
        let existing = (0..parts.len())
            .filter_map(|ix| parts.get(ix).and_then(as_map))
            .find(|part| map_str(part, "kind") == "ui" && map_str(part, "id") == id);
        let part = match existing {
            Some(part) => part,
            None => {
                let part = parts.push_container(LoroMap::new())?;
                part.insert("kind", "ui")?;
                part.insert("id", id)?;
                part
            }
        };
        part.insert("spec", spec)?;
        doc.commit();
        Ok(())
    }

    /// Update every indicator row of `subagent_id` (one, in practice) with
    /// the subagent's final state. Returns whether a row was found.
    pub fn set_subagent_part(&self, subagent_id: &str, state: &str, summary: &str) -> Result<bool, DocError> {
        let doc = &self.inner.doc;
        let list = doc.get_list("messages");
        let mut changed = false;
        for index in 0..list.len() {
            let Ok(parts) = parts_of(doc, index) else {
                continue;
            };
            if let Some(part) = find_sub_part(&parts, subagent_id) {
                part.insert("state", state)?;
                part.insert("summary", summary)?;
                changed = true;
            }
        }
        if changed {
            doc.commit();
        }
        Ok(changed)
    }

    /// Append a command. `actor` must be `origin_device`. Origin fields are not edited later.
    pub fn append_command(
        &self,
        actor: &str,
        origin_device: &str,
        command: &UserCommand,
        created_at: i64,
    ) -> Result<(), DocError> {
        if actor != origin_device {
            return Err(DocError::ForeignOrigin {
                actor: actor.to_string(),
                origin: origin_device.to_string(),
            });
        }
        let doc = &self.inner.doc;
        let command_id = command_id_of(command);
        if find_command(doc, command_id).is_some() {
            return Err(DocError::DuplicateCommand(command_id));
        }
        let commands = doc.get_list("commands");
        let entry = commands.push_container(LoroMap::new())?;
        entry.insert("command_id", command_id.to_string().as_str())?;
        entry.insert("origin_device", origin_device)?;
        entry.insert("created_at", created_at)?;
        match command {
            UserCommand::Send {
                message_id,
                text,
                attachments,
                shown,
                ..
            } => {
                entry.insert("kind", "send")?;
                entry.insert("message_id", message_id.as_str())?;
                entry.insert("text", text.as_str())?;
                if !attachments.is_empty() {
                    entry.insert("attachments", encode_attachments(attachments)?.as_str())?;
                }
                if !shown {
                    entry.insert("shown", false)?;
                }
            }
            UserCommand::Steer {
                message_id, text, ..
            } => {
                entry.insert("kind", "steer")?;
                entry.insert("message_id", message_id.as_str())?;
                entry.insert("text", text.as_str())?;
            }
            UserCommand::Interrupt { .. } => {
                entry.insert("kind", "interrupt")?;
            }
            UserCommand::Answer {
                question_id,
                answers,
                ..
            } => {
                entry.insert("kind", "answer")?;
                entry.insert("question_id", question_id.as_str())?;
                let encoded = serde_json::to_string(answers)
                    .map_err(|err| DocError::Encode(err.to_string()))?;
                entry.insert("answers", encoded.as_str())?;
            }
        }
        doc.commit();
        Ok(())
    }

    /// Writes `meta.native_session`. The profile store keeps the file path beside it.
    pub fn set_native_session(&self, native_session: &str) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        doc.get_map("meta")
            .insert("native_session", native_session)?;
        doc.commit();
        Ok(())
    }

    /// `meta.model` / `meta.reasoning` for the next turn. Any device may write them.
    pub fn set_options(&self, model: &str, reasoning: &str) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        let meta = doc.get_map("meta");
        meta.insert("model", model)?;
        meta.insert("reasoning", reasoning)?;
        doc.commit();
        Ok(())
    }

    /// Host-only. Sets `outcome` when the entry has none.
    pub fn set_outcome(
        &self,
        actor: &str,
        command_id: Uuid,
        outcome: Outcome,
    ) -> Result<(), DocError> {
        let doc = &self.inner.doc;
        require_host(doc, actor)?;
        let Some(entry) = find_command(doc, command_id) else {
            return Err(DocError::UnknownCommand(command_id));
        };
        if let Some(existing) = map_opt_str(&entry, "outcome") {
            return Err(DocError::OutcomeSet {
                command_id,
                outcome: existing,
            });
        }
        entry.insert("outcome", outcome.as_str())?;
        doc.commit();
        Ok(())
    }

    /// Host drain. Expires unprocessed entries older than 7 days, supersedes
    /// older unprocessed steers from the same origin, and returns the entries
    /// that remain unprocessed, in list order. One commit covers every mark.
    pub fn drain(&self, actor: &str, now: i64) -> Result<Vec<Uuid>, DocError> {
        let doc = &self.inner.doc;
        require_host(doc, actor)?;
        let commands = read_commands(doc);
        let (changes, remain) = plan_drain(&commands, now);
        if changes.is_empty() {
            return Ok(remain);
        }
        let list = doc.get_list("commands");
        for (index, outcome) in changes {
            let Some(entry) = list.get(index).and_then(as_map) else {
                continue;
            };
            entry.insert("outcome", outcome)?;
        }
        doc.commit();
        Ok(remain)
    }

    fn from_attached(doc: LoroDoc) -> Result<Self, DocError> {
        let messages_id = doc.get_list("messages").id();
        let commands_id = doc.get_list("commands").id();
        let meta_id = doc.get_map("meta").id();
        let subagents_id = doc.get_map("subagents").id();
        let view = read_view(&doc);
        let inner = Arc::new(Inner {
            doc,
            view: Mutex::new(view),
            messages_id,
            commands_id,
            meta_id,
            subagents_id,
            commits: AtomicU64::new(0),
        });
        let sub_inner = Arc::clone(&inner);
        let sub = inner.doc.subscribe_root(Arc::new(move |event| {
            sub_inner.apply(&event);
        }));
        Ok(Self { _sub: sub, inner })
    }
}

impl Inner {
    fn apply(&self, event: &loro::event::DiffEvent<'_>) {
        let structural = event
            .events
            .iter()
            .any(|diff| diff.target == &self.messages_id);
        let mut touched = Vec::new();
        for diff in &event.events {
            if let Some(index) = message_seq(diff.path, &self.messages_id)
                && !touched.contains(&index)
            {
                touched.push(index);
            }
        }
        let mut view = self.view.lock().expect("session view");
        if structural {
            reconcile(&self.doc, &mut view);
        }
        for index in touched {
            if index < view.messages.len() {
                view.messages[index] = Arc::new(read_message(&self.doc, index));
                if index < view.materialized.len() {
                    view.materialized[index] = view.materialized[index].saturating_add(1);
                }
            }
        }
        if event_touches(event, &self.commands_id) {
            view.commands = read_commands(&self.doc);
        }
        if event_touches(event, &self.subagents_id) {
            view.subagents = read_subagents(&self.doc);
        }
        if event_touches(event, &self.meta_id) {
            view.meta = read_meta(&self.doc);
        }
        drop(view);
        self.commits.fetch_add(1, Ordering::Relaxed);
    }
}

fn event_touches(event: &loro::event::DiffEvent<'_>, id: &ContainerID) -> bool {
    event
        .events
        .iter()
        .any(|diff| diff.target == id || diff.path.iter().any(|(container, _)| container == id))
}

/// Path entry after the messages list is `(message map, Seq(index))`.
fn message_seq(path: &[(ContainerID, Index)], messages: &ContainerID) -> Option<usize> {
    let pos = path.iter().position(|(id, _)| id == messages)?;
    match path.get(pos + 1) {
        Some((_, Index::Seq(index))) => Some(*index),
        _ => None,
    }
}

fn reconcile(doc: &LoroDoc, view: &mut SessionView) {
    let list = doc.get_list("messages");
    let mut pool: Vec<(Arc<MessageView>, u64)> = view
        .messages
        .drain(..)
        .zip(view.materialized.drain(..))
        .collect();
    let mut messages = Vec::with_capacity(list.len());
    let mut materialized = Vec::with_capacity(list.len());
    for index in 0..list.len() {
        let id = message_id_at(&list, index);
        if let Some(pos) = pool.iter().position(|(message, _)| message.id == id) {
            let (message, count) = pool.remove(pos);
            messages.push(message);
            materialized.push(count);
        } else {
            messages.push(Arc::new(read_message(doc, index)));
            materialized.push(1);
        }
    }
    view.messages = messages;
    view.materialized = materialized;
}

fn write_new(doc: &LoroDoc, fields: &NewSession) -> Result<(), DocError> {
    let meta = doc.get_map("meta");
    meta.insert("chat_id", fields.chat_id.as_str())?;
    meta.insert("host_device", fields.host_device.as_str())?;
    meta.insert("space_id", fields.space_id.as_str())?;
    meta.insert("harness", harness_name(fields.harness))?;
    meta.insert("cwd", fields.cwd.as_str())?;
    meta.insert("model", fields.model.as_str())?;
    meta.insert("reasoning", fields.reasoning.as_str())?;
    meta.insert("native_session", fields.native_session.as_str())?;
    meta.insert("created_at", fields.created_at)?;
    meta.insert("schema", SCHEMA_VERSION)?;
    doc.get_list("messages");
    doc.get_list("commands");
    doc.get_map("subagents");
    Ok(())
}

pub(crate) fn harness_name(id: HarnessId) -> &'static str {
    match id {
        HarnessId::Mock => "mock",
        HarnessId::Keel => "keel",
    }
}

fn read_schema(doc: &LoroDoc) -> Result<i64, DocError> {
    match doc.get_map("meta").get("schema") {
        Some(ValueOrContainer::Value(loro::LoroValue::I64(schema))) => Ok(schema),
        _ => Err(DocError::SchemaMissing),
    }
}

fn read_view(doc: &LoroDoc) -> SessionView {
    let list = doc.get_list("messages");
    let len = list.len();
    let messages: Vec<Arc<MessageView>> = (0..len)
        .map(|index| Arc::new(read_message(doc, index)))
        .collect();
    let materialized = vec![1; messages.len()];
    SessionView {
        meta: read_meta(doc),
        messages,
        materialized,
        commands: read_commands(doc),
        subagents: read_subagents(doc),
    }
}

fn read_meta(doc: &LoroDoc) -> SessionMeta {
    let meta = doc.get_map("meta");
    SessionMeta {
        chat_id: map_str(&meta, "chat_id"),
        host_device: map_str(&meta, "host_device"),
        space_id: map_str(&meta, "space_id"),
        harness: map_str(&meta, "harness"),
        cwd: map_str(&meta, "cwd"),
        model: map_str(&meta, "model"),
        reasoning: map_str(&meta, "reasoning"),
        native_session: map_str(&meta, "native_session"),
        created_at: map_i64(&meta, "created_at"),
        schema: map_i64(&meta, "schema"),
    }
}

fn read_message(doc: &LoroDoc, index: usize) -> MessageView {
    let list = doc.get_list("messages");
    let message = list.get(index).and_then(as_map).expect("message map");
    let parts_list = match message.get("parts") {
        Some(ValueOrContainer::Container(Container::List(parts))) => parts,
        _ => LoroList::new(),
    };
    let parts = read_parts(&parts_list);
    let text = parts
        .iter()
        .filter_map(|part| match part {
            PartView::Text { body, .. } => Some(body.as_str()),
            _ => None,
        })
        .collect();
    MessageView {
        id: map_str(&message, "id"),
        role: map_str(&message, "role"),
        created_at: map_i64(&message, "created_at"),
        parts,
        text,
        attachments: decode_attachments(&map_str(&message, "attachments")),
    }
}

/// Attachments ride as one JSON string, like an answer list.
fn encode_attachments(attachments: &[Attachment]) -> Result<String, DocError> {
    serde_json::to_string(attachments).map_err(|err| DocError::Encode(err.to_string()))
}

/// Unknown or malformed values read as none; ids that are not a sha256 are dropped.
fn decode_attachments(raw: &str) -> Vec<Attachment> {
    if raw.is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<Attachment>>(raw)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| Attachment::valid_id(&a.id))
        .collect()
}

fn read_commands(doc: &LoroDoc) -> Vec<CommandView> {
    let list = doc.get_list("commands");
    let mut commands = Vec::with_capacity(list.len());
    for index in 0..list.len() {
        let Some(entry) = list.get(index).and_then(as_map) else {
            continue;
        };
        let Some(command_id) = Uuid::parse_str(&map_str(&entry, "command_id")).ok() else {
            continue;
        };
        commands.push(CommandView {
            command_id,
            kind: map_str(&entry, "kind"),
            origin_device: map_str(&entry, "origin_device"),
            created_at: map_i64(&entry, "created_at"),
            message_id: map_str(&entry, "message_id"),
            text: map_str(&entry, "text"),
            question_id: map_str(&entry, "question_id"),
            answers: map_str(&entry, "answers"),
            attachments: decode_attachments(&map_str(&entry, "attachments")),
            outcome: map_opt_str(&entry, "outcome"),
            shown: match entry.get("shown") {
                Some(loro::ValueOrContainer::Value(loro::LoroValue::Bool(value))) => value,
                _ => true,
            },
        });
    }
    commands
}

fn message_id_at(list: &LoroList, index: usize) -> String {
    list.get(index)
        .and_then(as_map)
        .map(|message| map_str(&message, "id"))
        .unwrap_or_default()
}

fn message_pos(doc: &LoroDoc, id: &str) -> Option<usize> {
    let list = doc.get_list("messages");
    (0..list.len()).find(|&index| message_id_at(&list, index) == id)
}

fn require_message(doc: &LoroDoc, id: &str) -> Result<usize, DocError> {
    message_pos(doc, id).ok_or_else(|| DocError::UnknownMessage(id.to_string()))
}

fn parts_of(doc: &LoroDoc, index: usize) -> Result<LoroList, DocError> {
    let messages = doc.get_list("messages");
    let Some(message) = messages.get(index).and_then(as_map) else {
        return Err(DocError::UnknownMessage(index.to_string()));
    };
    match message.get("parts") {
        Some(ValueOrContainer::Container(Container::List(parts))) => Ok(parts),
        _ => Ok(message.insert_container("parts", LoroList::new())?),
    }
}

const TEXT: &str = "text";
const REASONING: &str = "reasoning";

fn open_text_part_in(parts: &LoroList, kind: &str, part_id: &str) -> Result<LoroText, DocError> {
    if let Some(text) = last_text_part_in(parts, kind, part_id) {
        return Ok(text);
    }
    create_text_part_in(parts, kind, part_id)
}

fn last_text_part_in(parts: &LoroList, kind: &str, part_id: &str) -> Option<LoroText> {
    let mut found = None;
    for part_index in 0..parts.len() {
        let Some(part) = parts.get(part_index).and_then(as_map) else {
            continue;
        };
        if map_str(&part, "kind") != kind {
            continue;
        }
        if !part_belongs(&map_str(&part, "id"), part_id) {
            continue;
        }
        if let Some(ValueOrContainer::Container(Container::Text(text))) = part.get("body") {
            found = Some(text);
        }
    }
    found
}

fn create_text_part_in(parts: &LoroList, kind: &str, part_id: &str) -> Result<LoroText, DocError> {
    let id = next_part_id_in(parts, kind, part_id);
    let part = parts.push_container(LoroMap::new())?;
    part.insert("kind", kind)?;
    part.insert("id", id.as_str())?;
    Ok(part.insert_container("body", LoroText::new())?)
}

fn next_part_id_in(parts: &LoroList, kind: &str, part_id: &str) -> String {
    let mut max_suffix: Option<u32> = None;
    for part_index in 0..parts.len() {
        let Some(part) = parts.get(part_index).and_then(as_map) else {
            continue;
        };
        if map_str(&part, "kind") != kind {
            continue;
        }
        let id = map_str(&part, "id");
        if let Some(suffix) = part_suffix(&id, part_id) {
            max_suffix = Some(max_suffix.map_or(suffix, |current| current.max(suffix)));
        }
    }
    match max_suffix {
        None => part_id.to_string(),
        Some(suffix) => format!("{part_id}:{}", suffix + 1),
    }
}

fn part_belongs(id: &str, part_id: &str) -> bool {
    part_suffix(id, part_id).is_some()
}

fn part_suffix(id: &str, part_id: &str) -> Option<u32> {
    if id == part_id {
        return Some(0);
    }
    let rest = id.strip_prefix(part_id)?.strip_prefix(':')?;
    if rest.is_empty() || !rest.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

fn split_utf8_prefix(text: &str, max: usize) -> (&str, &str) {
    if max == 0 || text.is_empty() {
        return ("", text);
    }
    if text.len() <= max {
        return (text, "");
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        return ("", text);
    }
    text.split_at(end)
}

fn find_tool(parts: &LoroList, tool_id: &str) -> Option<LoroMap> {
    for index in 0..parts.len() {
        let Some(part) = parts.get(index).and_then(as_map) else {
            continue;
        };
        if map_str(&part, "kind") == "tool" && map_str(&part, "tool_id") == tool_id {
            return Some(part);
        }
    }
    None
}

fn find_sub_part(parts: &LoroList, subagent_id: &str) -> Option<LoroMap> {
    for index in 0..parts.len() {
        let Some(part) = parts.get(index).and_then(as_map) else {
            continue;
        };
        if map_str(&part, "kind") == "subagent" && map_str(&part, "subagent_id") == subagent_id {
            return Some(part);
        }
    }
    None
}

fn write_sub_part(part: &LoroMap, row: &SubagentRow) -> Result<(), DocError> {
    part.insert("name", row.name.as_str())?;
    part.insert("state", row.state.as_str())?;
    part.insert("summary", row.summary.as_str())?;
    Ok(())
}

/// A subagent's transcript list, creating nothing: writes go through
/// [`SessionDoc::ensure_subagent`] first.
fn require_sub_parts(doc: &LoroDoc, subagent_id: &str) -> Result<LoroList, DocError> {
    let entry = doc
        .get_map("subagents")
        .get(subagent_id)
        .and_then(|v| match v {
            ValueOrContainer::Container(Container::Map(entry)) => Some(entry),
            _ => None,
        })
        .ok_or_else(|| DocError::UnknownSubagent(subagent_id.to_string()))?;
    match entry.get("parts") {
        Some(ValueOrContainer::Container(Container::List(parts))) => Ok(parts),
        _ => Ok(entry.insert_container("parts", LoroList::new())?),
    }
}

fn read_subagents(doc: &LoroDoc) -> Vec<SubagentView> {
    let subagents = doc.get_map("subagents");
    subagents
        .keys()
        .filter_map(|key| {
            let entry = subagents.get(key.as_str()).and_then(|v| match v {
                ValueOrContainer::Container(Container::Map(entry)) => Some(entry),
                _ => None,
            })?;
            let ended_at = match entry.get("ended_at") {
                Some(ValueOrContainer::Value(loro::LoroValue::I64(at))) => Some(at),
                _ => None,
            };
            Some(SubagentView {
                id: map_str(&entry, "id"),
                parent_tool_id: map_str(&entry, "parent_tool_id"),
                name: map_str(&entry, "name"),
                description: map_str(&entry, "description"),
                status: map_str(&entry, "status"),
                created_at: map_i64(&entry, "created_at"),
                ended_at,
                last_summary: map_str(&entry, "last_summary"),
                blocked_question: map_opt_str(&entry, "blocked_question"),
            })
        })
        .collect()
}

/// One part list (a message's or a subagent's) as views.
fn read_parts(parts: &LoroList) -> Vec<PartView> {
    let mut out = Vec::with_capacity(parts.len());
    for part_index in 0..parts.len() {
        let Some(part) = parts.get(part_index).and_then(as_map) else {
            continue;
        };
        match map_str(&part, "kind").as_str() {
            "text" => out.push(PartView::Text {
                id: map_str(&part, "id"),
                body: match part.get("body") {
                    Some(ValueOrContainer::Container(Container::Text(body))) => body.to_string(),
                    _ => String::new(),
                },
            }),
            "reasoning" => out.push(PartView::Reasoning {
                id: map_str(&part, "id"),
                body: match part.get("body") {
                    Some(ValueOrContainer::Container(Container::Text(body))) => body.to_string(),
                    _ => String::new(),
                },
            }),
            "tool" => out.push(PartView::Tool(ToolRow {
                tool_id: map_str(&part, "tool_id"),
                name: map_str(&part, "name"),
                state: map_str(&part, "state"),
                summary: map_str(&part, "summary"),
                input_bytes: map_u64(&part, "input_bytes"),
                output_bytes: map_u64(&part, "output_bytes"),
                detail: map_str(&part, "detail"),
                label: map_str(&part, "label"),
                images: map_str(&part, "images")
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect(),
                kind: map_str(&part, "tool_kind"),
                started_ms: map_opt_i64(&part, "started_ms"),
                ended_ms: map_opt_i64(&part, "ended_ms"),
                diff: map_str(&part, "diff"),
                output_tail: map_str(&part, "output_tail"),
            })),
            "plan" => out.push(PartView::Plan { entries: parse_plan_entries(&map_str(&part, "entries")) }),
            "subagent" => out.push(PartView::Subagent {
                id: map_str(&part, "id"),
                subagent_id: map_str(&part, "subagent_id"),
                name: map_str(&part, "name"),
                state: map_str(&part, "state"),
                summary: map_str(&part, "summary"),
            }),
            "ui" => out.push(PartView::Ui { id: map_str(&part, "id"), spec: map_str(&part, "spec") }),
            _ => {}
        }
    }
    out
}

fn write_tool(part: &LoroMap, row: &ToolRow, summary: &str) -> Result<(), DocError> {
    part.insert("name", row.name.as_str())?;
    part.insert("state", row.state.as_str())?;
    part.insert("summary", summary)?;
    part.insert(
        "input_bytes",
        i64::try_from(row.input_bytes).unwrap_or(i64::MAX),
    )?;
    part.insert(
        "output_bytes",
        i64::try_from(row.output_bytes).unwrap_or(i64::MAX),
    )?;
    if !row.images.is_empty() {
        part.insert("images", row.images.join("\n").as_str())?;
    }
    if !row.detail.is_empty() {
        part.insert("detail", row.detail.as_str())?;
    }
    if !row.label.is_empty() {
        part.insert("label", row.label.as_str())?;
    }
    // Empty leaves the start's value: a finish upsert does not carry these.
    if !row.kind.is_empty() {
        part.insert("tool_kind", row.kind.as_str())?;
    }
    if let Some(started_ms) = row.started_ms {
        part.insert("started_ms", started_ms)?;
    }
    if let Some(ended_ms) = row.ended_ms {
        part.insert("ended_ms", ended_ms)?;
    }
    if !row.diff.is_empty() {
        part.insert("diff", row.diff.as_str())?;
    }
    if !row.output_tail.is_empty() {
        part.insert("output_tail", row.output_tail.as_str())?;
    }
    Ok(())
}

fn parse_plan_entries(raw: &str) -> Vec<PlanEntryView> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let content = item["content"].as_str().filter(|text| !text.is_empty())?.to_string();
            Some(PlanEntryView {
                content,
                status: item["status"].as_str().unwrap_or("pending").to_string(),
                priority: item["priority"].as_str().filter(|p| !p.is_empty()).map(str::to_string),
            })
        })
        .collect()
}

fn write_plan(parts: &LoroList, entries_json: &str) -> Result<(), DocError> {
    if let Some(existing) = (0..parts.len()).find_map(|index| {
        let part = parts.get(index).and_then(as_map)?;
        (map_str(&part, "kind") == "plan").then_some(part)
    }) {
        existing.insert("entries", entries_json)?;
        return Ok(());
    }
    let part = parts.push_container(LoroMap::new())?;
    part.insert("kind", "plan")?;
    part.insert("id", "plan")?;
    part.insert("entries", entries_json)?;
    Ok(())
}

fn cancel_running_in(parts: &LoroList, ended_ms: i64) -> Result<bool, DocError> {
    let mut changed = false;
    for index in 0..parts.len() {
        let Some(part) = parts.get(index).and_then(as_map) else { continue };
        if map_str(&part, "kind") != "tool" || map_str(&part, "state") != "running" {
            continue;
        }
        part.insert("state", "cancelled")?;
        part.insert("ended_ms", ended_ms)?;
        changed = true;
    }
    Ok(changed)
}

fn command_id_of(command: &UserCommand) -> Uuid {
    match command {
        UserCommand::Send { command_id, .. }
        | UserCommand::Steer { command_id, .. }
        | UserCommand::Interrupt { command_id }
        | UserCommand::Answer { command_id, .. } => *command_id,

    }
}

fn find_command(doc: &LoroDoc, command_id: Uuid) -> Option<LoroMap> {
    let list = doc.get_list("commands");
    let needle = command_id.to_string();
    for index in 0..list.len() {
        let Some(entry) = list.get(index).and_then(as_map) else {
            continue;
        };
        if map_str(&entry, "command_id") == needle {
            return Some(entry);
        }
    }
    None
}

fn require_host(doc: &LoroDoc, actor: &str) -> Result<(), DocError> {
    let host = map_str(&doc.get_map("meta"), "host_device");
    if actor == host {
        Ok(())
    } else {
        Err(DocError::NotHost {
            actor: actor.to_string(),
            host,
        })
    }
}

/// Marks to apply, then the unprocessed ids that remain, in list order.
fn plan_drain(commands: &[CommandView], now: i64) -> (Vec<(usize, &'static str)>, Vec<Uuid>) {
    let mut marked: Vec<Option<&'static str>> = vec![None; commands.len()];
    // Rule 3: dedupe on `command_id`. A merge can bring a second entry with the
    // same id. Only the first entry is ever eligible (it is the one
    // `set_outcome` writes), and none is when any copy already has an outcome.
    let mut first: HashMap<Uuid, (usize, bool)> = HashMap::new();
    for (index, command) in commands.iter().enumerate() {
        let entry = first.entry(command.command_id).or_insert((index, false));
        entry.1 |= command.outcome.is_some();
    }
    let duplicate: Vec<bool> = commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let (head, settled) = first[&command.command_id];
            head != index || settled
        })
        .collect();
    for (index, command) in commands.iter().enumerate() {
        if command.outcome.is_some() || duplicate[index] {
            continue;
        }
        let expired = now
            .checked_sub(command.created_at)
            .is_some_and(|age| age > COMMAND_TTL_MS);
        if expired {
            marked[index] = Some(Outcome::Expired.as_str());
        }
    }
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, command) in commands.iter().enumerate() {
        if command.outcome.is_some()
            || duplicate[index]
            || marked[index].is_some()
            || command.kind != "steer"
        {
            continue;
        }
        if let Some((_, indexes)) = groups
            .iter_mut()
            .find(|(origin, _)| origin == &command.origin_device)
        {
            indexes.push(index);
        } else {
            groups.push((command.origin_device.clone(), vec![index]));
        }
    }
    for (_, indexes) in &groups {
        if indexes.len() < 2 {
            continue;
        }
        let winner = indexes
            .iter()
            .copied()
            .max_by_key(|&index| (commands[index].created_at, index))
            .expect("group");
        for &index in indexes {
            if index != winner {
                marked[index] = Some(Outcome::Superseded.as_str());
            }
        }
    }
    let changes = marked
        .iter()
        .enumerate()
        .filter_map(|(index, outcome)| outcome.map(|outcome| (index, outcome)))
        .collect();
    let remain = commands
        .iter()
        .enumerate()
        .filter(|(index, command)| {
            command.outcome.is_none() && marked[*index].is_none() && !duplicate[*index]
        })
        .map(|(_, command)| command.command_id)
        .collect();
    (changes, remain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use keel_proto::UserCommand;

    fn session() -> SessionDoc {
        SessionDoc::create(NewSession {
            chat_id: "chat".into(),
            host_device: "host".into(),
            space_id: "space".into(),
            harness: HarnessId::Keel,
            cwd: "/tmp/work".into(),
            model: "pi".into(),
            reasoning: String::new(),
            native_session: String::new(),
            created_at: 10,
        })
        .expect("session")
    }

    fn send(id: u128, text: &str) -> UserCommand {
        UserCommand::Send {
            command_id: Uuid::from_u128(id),
            message_id: format!("m{id}"),
            text: text.into(),
            attachments: Vec::new(),
            shown: true,
        }
    }

    #[test]
    fn attachments_round_trip_on_command_and_message_and_bad_ids_drop() {
        let doc = session();
        let good = Attachment { id: "a".repeat(64), name: "shot.png".into(), mime: "image/png".into() };
        let bad = Attachment { id: "../etc/passwd".into(), name: "x".into(), mime: "image/png".into() };
        let command = UserCommand::Send {
            command_id: Uuid::from_u128(77),
            message_id: "m77".into(),
            text: "look".into(),
            attachments: vec![good.clone(), bad.clone()],
            shown: true,
        };
        doc.append_command("phone", "phone", &command, 5).expect("append");
        let stored = doc.view().commands.into_iter().find(|c| c.message_id == "m77").expect("command");
        assert_eq!(stored.attachments, vec![good.clone()]);

        doc.append_message("m77", "user", 5).expect("message");
        doc.set_attachments("m77", &[good.clone(), bad]).expect("attach");
        let message = doc.view().messages.into_iter().find(|m| m.id == "m77").expect("message view");
        assert_eq!(message.attachments, vec![good]);
    }

    fn steer(id: u128, text: &str) -> UserCommand {
        UserCommand::Steer {
            command_id: Uuid::from_u128(id),
            message_id: format!("m{id}"),
            text: text.into(),
        }
    }

    fn outcome_of(doc: &SessionDoc, id: u128) -> Option<String> {
        doc.view()
            .commands
            .into_iter()
            .find(|command| command.command_id == Uuid::from_u128(id))
            .and_then(|command| command.outcome)
    }

    #[test]
    fn tool_part_stores_capped_summary_only() {
        let doc = session();
        doc.append_message("m", "assistant", 1).unwrap();
        let raw = format!("\n\n{}\nsecond-line", "x".repeat(200));
        doc.upsert_tool(
            "m",
            &ToolRow {
                tool_id: "t".into(),
                name: "bash".into(),
                state: "done".into(),
                summary: raw,
                input_bytes: 12,
                output_bytes: 4000,
                detail: String::new(),
                images: vec!["/tmp/a b.png".into(), "/tmp/c.png".into()],
                ..ToolRow::default()
            },
        )
        .unwrap();
        let view = doc.view();
        let PartView::Tool(tool) = &view.messages[0].parts[0] else {
            panic!("tool part");
        };
        assert_eq!(tool.images, vec!["/tmp/a b.png".to_string(), "/tmp/c.png".to_string()]);
        assert_eq!(tool.summary, "x".repeat(160));
        assert_eq!(tool.input_bytes, 12);
        assert_eq!(tool.output_bytes, 4000);
        let json = doc.deep_json().to_string();
        assert!(!json.contains(&"x".repeat(161)));
        assert!(!json.contains("second-line"));
        assert!(!json.contains("tool_kind") && !json.contains("output_tail") && !json.contains("started_ms"));
    }

    /// New tool fields round-trip. A later upsert that omits them leaves them.
    /// An old row (no new keys) still reads. A running tool cancels in place.
    #[test]
    fn tool_kind_diff_and_plan_survive_a_finish_and_old_rows_still_read() {
        let doc = session();
        doc.append_message("m", "assistant", 1).unwrap();
        doc.upsert_tool(
            "m",
            &ToolRow {
                tool_id: "t".into(),
                name: "bash".into(),
                state: "running".into(),
                summary: String::new(),
                input_bytes: 4,
                kind: "execute".into(),
                started_ms: Some(10),
                diff: r#"{"path":"a.rs","new_text":"+hi"}"#.into(),
                output_tail: "hi\n".into(),
                ..ToolRow::default()
            },
        )
        .unwrap();
        doc.upsert_tool(
            "m",
            &ToolRow {
                tool_id: "t".into(),
                name: "bash".into(),
                state: "done".into(),
                summary: "ok".into(),
                output_bytes: 3,
                ended_ms: Some(50),
                ..ToolRow::default()
            },
        )
        .unwrap();
        doc.upsert_plan("m", r#"[{"content":"read","status":"completed"},{"content":"edit","status":"in_progress","priority":"high"}]"#).unwrap();
        doc.upsert_plan("m", r#"[{"content":"read","status":"completed"},{"content":"edit","status":"completed"}]"#).unwrap();
        let view = doc.view();
        let PartView::Tool(tool) = &view.messages[0].parts[0] else { panic!("tool") };
        assert_eq!(tool.state, "done");
        assert_eq!(tool.summary, "ok");
        assert_eq!(tool.kind, "execute");
        assert_eq!(tool.started_ms, Some(10));
        assert_eq!(tool.ended_ms, Some(50));
        assert_eq!(tool.diff, r#"{"path":"a.rs","new_text":"+hi"}"#);
        assert_eq!(tool.output_tail, "hi\n");
        assert_eq!(tool.input_bytes, 0);
        let PartView::Plan { entries } = &view.messages[0].parts[1] else { panic!("{:?}", view.messages[0].parts) };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].status, "completed");
        assert!(view.messages[0].parts.iter().filter(|p| matches!(p, PartView::Plan { .. })).count() == 1);

        doc.upsert_tool(
            "m",
            &ToolRow { tool_id: "old".into(), name: "read".into(), state: "done".into(), summary: "f".into(), ..ToolRow::default() },
        )
        .unwrap();
        let tool = doc.view().messages[0].parts.iter().find_map(|p| match p {
            PartView::Tool(row) if row.tool_id == "old" => Some(row.clone()),
            _ => None,
        }).unwrap();
        assert!(tool.kind.is_empty() && tool.started_ms.is_none() && tool.ended_ms.is_none() && tool.diff.is_empty());

        doc.upsert_tool(
            "m",
            &ToolRow { tool_id: "run".into(), name: "bash".into(), state: "running".into(), summary: "still".into(), ..ToolRow::default() },
        )
        .unwrap();
        let (messages, subs) = doc.cancel_running_tools(&["m".into()], &[], 90).unwrap();
        assert_eq!(messages, vec!["m".to_string()]);
        assert!(subs.is_empty());
        let tool = doc.view().messages[0].parts.iter().find_map(|p| match p {
            PartView::Tool(row) if row.tool_id == "run" => Some(row.clone()),
            _ => None,
        }).unwrap();
        assert_eq!(tool.state, "cancelled");
        assert_eq!(tool.ended_ms, Some(90));
        assert_eq!(tool.summary, "still");
        let done = doc.view().messages[0].parts.iter().find_map(|p| match p {
            PartView::Tool(row) if row.tool_id == "t" => Some(row.state.clone()),
            _ => None,
        }).unwrap();
        assert_eq!(done, "done");
    }

    #[test]
    fn continuation_splits_on_the_next_byte_and_concatenates() {
        let doc = session();
        doc.append_message("m", "assistant", 1).unwrap();
        let full = "a".repeat(CONTINUATION_SPLIT);
        doc.append_text("m", "p", &full).unwrap();
        let view = doc.view();
        assert_eq!(view.messages[0].parts.len(), 1);
        assert_eq!(view.messages[0].text.len(), CONTINUATION_SPLIT);

        doc.append_text("m", "p", "0123456789").unwrap();
        let view = doc.view();
        assert_eq!(view.messages[0].parts.len(), 2);
        let PartView::Text { id, body } = &view.messages[0].parts[0] else {
            panic!("text");
        };
        assert_eq!(id, "p");
        assert_eq!(body.len(), CONTINUATION_SPLIT);
        let PartView::Text { id, body } = &view.messages[0].parts[1] else {
            panic!("continuation");
        };
        assert_eq!(id, "p:1");
        assert_eq!(body, "0123456789");
        assert_eq!(view.messages[0].text, format!("{full}0123456789"));
    }

    #[test]
    fn reasoning_is_its_own_part_and_stays_out_of_text() {
        let doc = session();
        doc.append_message("m", "assistant", 1).unwrap();
        doc.append_reasoning("m", "p", "hmm").unwrap();
        doc.append_text("m", "p", "hi").unwrap();
        doc.append_reasoning("m", "p", " ok").unwrap();
        let view = doc.view();
        assert_eq!(view.messages[0].text, "hi");
        assert_eq!(
            view.messages[0].parts[0],
            PartView::Reasoning { id: "p".to_string(), body: "hmm ok".to_string() }
        );
    }

    #[test]
    fn one_insert_past_the_split_breaks_across_parts() {
        let doc = session();
        doc.append_message("m", "assistant", 1).unwrap();
        let big = "b".repeat(CONTINUATION_SPLIT + 10);
        doc.append_text("m", "body", &big).unwrap();
        let view = doc.view();
        assert_eq!(view.messages[0].parts.len(), 2);
        assert_eq!(view.messages[0].text, big);
    }

    #[test]
    fn snapshot_round_trip_keeps_text() {
        let doc = session();
        doc.append_message("m", "user", 1).unwrap();
        doc.append_text("m", "p", "hello").unwrap();
        let opened = SessionDoc::open(&doc.export_snapshot().unwrap()).unwrap();
        assert_eq!(opened.view().messages[0].text, "hello");
        assert_eq!(opened.view().meta.schema, SCHEMA_VERSION);
        assert_eq!(opened.view().meta.harness, "keel");
    }

    #[test]
    fn newer_schema_refuses_to_open() {
        let raw = LoroDoc::new();
        raw.get_map("meta").insert("schema", 2_i64).unwrap();
        raw.commit();
        let bytes = raw.export(ExportMode::Snapshot).unwrap();
        match SessionDoc::open(&bytes) {
            Err(DocError::SchemaTooNew { found: 2, known: 1 }) => {}
            Err(err) => panic!("unexpected error: {err}"),
            Ok(_) => panic!("opened a newer schema"),
        }
    }

    #[test]
    fn two_edits_leave_the_other_message_arc() {
        let doc = session();
        doc.append_message("m0", "user", 1).unwrap();
        doc.append_text("m0", "p0", "hello").unwrap();
        doc.append_message("m1", "assistant", 2).unwrap();
        doc.append_text("m1", "p1", "a").unwrap();

        let before = doc.view();
        let first = Arc::clone(&before.messages[0]);
        let second = Arc::clone(&before.messages[1]);
        let rebuilt_first = before.materialized[0];
        let rebuilt_second = before.materialized[1];

        doc.append_text("m1", "p1", "b").unwrap();
        doc.append_text("m1", "p1", "c").unwrap();

        let after = doc.view();
        assert!(
            Arc::ptr_eq(&first, &after.messages[0]),
            "message 0 was rebuilt: before {rebuilt_first} after {}",
            after.materialized[0]
        );
        assert!(!Arc::ptr_eq(&second, &after.messages[1]));
        assert_eq!(after.materialized[0], rebuilt_first);
        assert!(after.materialized[1] > rebuilt_second);
        assert_eq!(after.messages[0].text, "hello");
        assert_eq!(after.messages[1].text, "abc");
    }

    #[test]
    fn merged_duplicate_command_id_drains_once() {
        let host = session();
        let a = SessionDoc::from_import(&host.export_snapshot().unwrap()).unwrap();
        let b = SessionDoc::from_import(&host.export_snapshot().unwrap()).unwrap();
        a.append_command("phone", "phone", &send(7, "one"), 1).unwrap();
        b.append_command("phone", "phone", &send(7, "one"), 1).unwrap();
        host.import_bytes(&a.export_snapshot().unwrap()).unwrap();
        host.import_bytes(&b.export_snapshot().unwrap()).unwrap();
        assert_eq!(host.view().commands.len(), 2);
        let host_id = host.view().meta.host_device;
        let remain = host.drain(&host_id, 2).unwrap();
        assert_eq!(remain, vec![Uuid::from_u128(7)]);
        host.set_outcome(&host_id, Uuid::from_u128(7), Outcome::Processed)
            .unwrap();
        assert!(host.drain(&host_id, 3).unwrap().is_empty());
    }

    #[test]
    fn foreign_device_cannot_append_and_duplicate_id_is_rejected() {
        let doc = session();
        let err = doc
            .append_command("phone", "host", &send(1, "hi"), 1)
            .unwrap_err();
        assert!(matches!(err, DocError::ForeignOrigin { .. }));
        assert!(doc.view().commands.is_empty());
        assert_eq!(doc.commits(), 0);

        doc.append_command("phone", "phone", &send(1, "hi"), 1)
            .unwrap();
        let err = doc
            .append_command("phone", "phone", &send(1, "again"), 2)
            .unwrap_err();
        assert!(matches!(err, DocError::DuplicateCommand(_)));
        assert_eq!(doc.view().commands.len(), 1);
        assert_eq!(doc.view().commands[0].text, "hi");
    }

    #[test]
    fn only_the_host_writes_an_outcome_and_a_set_outcome_stays() {
        let doc = session();
        let id = Uuid::from_u128(1);
        doc.append_command("phone", "phone", &send(1, "hi"), 1)
            .unwrap();
        let err = doc
            .set_outcome("phone", id, Outcome::Processed)
            .unwrap_err();
        assert!(matches!(err, DocError::NotHost { .. }));
        assert_eq!(outcome_of(&doc, 1), None);

        doc.set_outcome("host", id, Outcome::Processed).unwrap();
        let err = doc.set_outcome("host", id, Outcome::Rejected).unwrap_err();
        assert!(matches!(err, DocError::OutcomeSet { .. }));
        assert_eq!(outcome_of(&doc, 1), Some("processed".into()));

        let pending = doc.drain("host", 1).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn drain_supersedes_older_steers_and_expires_past_seven_days() {
        let doc = session();
        doc.append_command("phone", "phone", &steer(1, "old"), 10)
            .unwrap();
        doc.append_command("phone", "phone", &steer(2, "new"), 20)
            .unwrap();
        doc.append_command("host", "host", &send(3, "keep"), 20)
            .unwrap();
        doc.append_command("other", "other", &steer(4, "other"), 20)
            .unwrap();
        let before = doc.commits();
        let pending = doc.drain("host", 20).unwrap();
        assert_eq!(doc.commits(), before + 1);
        assert_eq!(
            pending,
            vec![Uuid::from_u128(2), Uuid::from_u128(3), Uuid::from_u128(4)]
        );
        assert_eq!(outcome_of(&doc, 1), Some("superseded".into()));
        assert_eq!(outcome_of(&doc, 2), None);

        let again = doc.drain("host", 20).unwrap();
        assert_eq!(again, pending);
        assert_eq!(doc.commits(), before + 1);
    }

    #[test]
    fn expiry_is_strictly_older_than_seven_days() {
        let doc = session();
        doc.append_command("host", "host", &send(1, "hi"), 1_000)
            .unwrap();
        let edge = 1_000 + COMMAND_TTL_MS;
        let pending = doc.drain("host", edge).unwrap();
        assert_eq!(pending, vec![Uuid::from_u128(1)]);
        assert_eq!(outcome_of(&doc, 1), None);

        let pending = doc.drain("host", edge + 1).unwrap();
        assert!(pending.is_empty());
        assert_eq!(outcome_of(&doc, 1), Some("expired".into()));
    }

    #[test]
    fn two_sends_both_stay_and_a_send_does_not_supersede_a_steer() {
        let doc = session();
        doc.append_command("phone", "phone", &steer(1, "steer"), 1)
            .unwrap();
        doc.append_command("phone", "phone", &send(2, "one"), 2)
            .unwrap();
        doc.append_command("phone", "phone", &send(3, "two"), 3)
            .unwrap();
        let pending = doc.drain("host", 3).unwrap();
        assert_eq!(
            pending,
            vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)]
        );
        assert!(doc.drain("phone", 3).is_err());
    }

    #[test]
    fn equal_steer_timestamps_keep_the_later_entry() {
        let doc = session();
        doc.append_command("phone", "phone", &steer(1, "first"), 5)
            .unwrap();
        doc.append_command("phone", "phone", &steer(2, "second"), 5)
            .unwrap();
        let pending = doc.drain("host", 5).unwrap();
        assert_eq!(pending, vec![Uuid::from_u128(2)]);
        assert_eq!(outcome_of(&doc, 1), Some("superseded".into()));
    }
}

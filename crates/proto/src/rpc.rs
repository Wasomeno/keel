//! RPC envelopes. One JSON object per message. No second hello shape.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::AgentEvent;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: String,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcFailure {
    pub id: String,
    pub error: RpcErrorBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcErrorBody {
    pub code: RpcErrorCode,
    pub message: String,
}

/// A snake_case string in `error.code`. Not an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcErrorCode {
    UnknownMethod,
    BadToken,
    BadParams,
    NotHost,
    Busy,
    PathRejected,
    PayloadTooLarge,
    Internal,
    /// No chat with that id on this engine. Nothing is created.
    UnknownChat,
    /// A write found the file changed since it was read. Nothing is written.
    Conflict,
}

/// The message a [`RpcErrorCode::Conflict`] file write fails with (clients
/// see messages, not codes).
pub const FILE_CHANGED: &str = "the file changed on disk";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocKind {
    Session,
    Registry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcStreamItem {
    pub stream: String,
    pub event: SubscribeEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubscribeEvent {
    /// One incremental view change. Not a Loro update and not a full dump.
    MirrorDiff {
        doc: DocKind,
        path: String,
        view: Value,
    },
    /// The agent event is the field `agent_event`, not a tuple payload.
    Live {
        agent_event: AgentEvent,
    },
    StreamClosed {
        reason: String,
    },
    /// Bytes from a terminal, base64: a PTY can split a UTF-8 sequence.
    TerminalOutput {
        data: String,
    },
    /// The terminal's shell ended.
    TerminalExit {
        code: Option<i32>,
    },
    /// The headed UI should run a browser host op and reply with `browser_complete`.
    BrowserRequest {
        request_id: String,
        /// `open_pane`, `navigate`, `snapshot`, `click`, `type`, `select`,
        /// `press`, `scroll`, `wait_for`, `screenshot`, `handle_dialog`,
        /// `upload`, or `console`.
        op: String,
        /// URL for `navigate`; empty otherwise.
        #[serde(default)]
        url: String,
        /// Arguments for `click` / `type` / `scroll` (selector, x/y, text,
        /// submit, dx/dy); null otherwise. Old clients ignore it.
        #[serde(default, skip_serializing_if = "Value::is_null")]
        params: Value,
        /// The chat whose agent asked: each chat drives its own tab, and its
        /// requests run in order. Empty from an old shim (a shared tab).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        chat: String,
    },
    /// Stop browser work nobody waits for any more: one request (its shim
    /// went away or timed out), or everything for a chat (it was stopped).
    BrowserCancel {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        request_id: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        chat: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proto_sketch_wires_round_trip_exactly() {
        let failure = r#"{"id":"c2","error":{"code":"unknown_method","message":"no such method"}}"#;
        let parsed: RpcFailure = serde_json::from_str(failure).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), failure);

        let diff = r#"{"stream":"s1","event":{"type":"mirror_diff","doc":"session","path":"messages.3","view":{}}}"#;
        let parsed: RpcStreamItem = serde_json::from_str(diff).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), diff);

        let live = r#"{"stream":"s1","event":{"type":"live","agent_event":{"type":"text","message_id":"m","part_id":"p","delta":"hi"}}}"#;
        let parsed: RpcStreamItem = serde_json::from_str(live).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), live);
    }
}

//! Subagent contracts: the stream scope key and how much of the subagents a
//! harness spawns it can show.
//!
//! A harness that runs subagents reports their traffic with [`SubagentRef`]
//! on the usual events, lifecycle through
//! [`crate::AgentEvent::SubagentStarted`] / [`crate::AgentEvent::SubagentDone`],
//! and what it can show through [`SubagentCaps`].

use serde::{Deserialize, Serialize};

/// Stable key tying one subagent's traffic and doc record together: the id
/// of the tool call that spawned it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubagentRef {
    pub id: String,
}

impl SubagentRef {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

/// How much of a subagent's work the driver can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentVisibility {
    /// The wire hides subagents (the transcript shows the parent only).
    #[default]
    None,
    /// Spawn and end are visible, not the subagent's own work.
    Lifecycle,
    /// The subagent's text, thinking, and tools stream too.
    Full,
}

/// Per-harness subagent abilities, reported by [`crate::HarnessId`] drivers.
/// The engine enforces them; the UI only decorates with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SubagentCaps {
    pub visibility: SubagentVisibility,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_round_trip() {
        let caps = SubagentCaps {
            visibility: SubagentVisibility::Full,
        };
        let json = serde_json::to_string(&caps).unwrap();
        assert_eq!(json, r#"{"visibility":"full"}"#);
        assert_eq!(serde_json::from_str::<SubagentCaps>(&json).unwrap(), caps);
        assert_eq!(serde_json::to_string(&SubagentRef::new("tu_1")).unwrap(), r#"{"id":"tu_1"}"#);
    }
}

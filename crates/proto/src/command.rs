//! Commands appended to a session document.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::question::UserInputAnswer;

/// An image attached to a send. The bytes live on the host device under
/// `{profile}/attachments/{id}`; `id` is their sha256 in lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime: String,
}

fn shown_by_default() -> bool {
    true
}

impl Attachment {
    /// The image types every harness and the UI can show.
    pub const MIMES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

    /// A well-formed id: 64 lowercase hex chars, so it is safe as a file name.
    pub fn valid_id(id: &str) -> bool {
        id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserCommand {
    Send {
        command_id: Uuid,
        message_id: String,
        text: String,
        /// Images uploaded to the host first (`upload_attachment`). Absent: none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
        /// When false, the text goes to the agent and is stored, but the
        /// transcript does not draw it as a user message. Absent means shown.
        #[serde(default = "shown_by_default")]
        shown: bool,
    },
    Steer {
        command_id: Uuid,
        message_id: String,
        text: String,
    },
    Interrupt {
        command_id: Uuid,
    },
    Answer {
        command_id: Uuid,
        question_id: String,
        answers: Vec<UserInputAnswer>,
    },
}

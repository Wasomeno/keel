//! One sentence the engine's generative UI pulled out of a reply (a `ui`
//! part with `component: "callout"`): a note, a warning, or the result.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Note,
    Warning,
    Result,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callout {
    pub kind: Kind,
    pub text: String,
}

impl Callout {
    /// A callout spec; `None` for another component or empty text.
    pub fn from_spec(spec: &Value) -> Option<Self> {
        if spec["component"] != "callout" {
            return None;
        }
        let text = spec["text"].as_str().unwrap_or_default().trim().to_string();
        if text.is_empty() {
            return None;
        }
        let kind = match spec["kind"].as_str() {
            Some("warning") => Kind::Warning,
            Some("result") => Kind::Result,
            _ => Kind::Note,
        };
        Some(Self { kind, text })
    }

    pub fn label(&self) -> &'static str {
        match self.kind {
            Kind::Note => "Note",
            Kind::Warning => "Warning",
            Kind::Result => "Result",
        }
    }

    pub fn content_len(&self) -> usize {
        self.text.len() + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_unknown_kind_is_a_note_and_blank_text_is_dropped() {
        let note = Callout::from_spec(&json!({ "component": "callout", "kind": "aside", "text": " The watcher. " })).unwrap();
        assert_eq!((note.kind, note.text.as_str(), note.label()), (Kind::Note, "The watcher.", "Note"));
        assert!(Callout::from_spec(&json!({ "component": "callout", "text": "  " })).is_none());
        assert!(Callout::from_spec(&json!({ "component": "chart", "text": "x" })).is_none());
    }
}

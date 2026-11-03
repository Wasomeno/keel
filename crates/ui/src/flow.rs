//! A diagram the engine's generative UI chose for a reply (a `ui` part with
//! `component: "diagram"`). It draws as the fence it carries: Mermaid source
//! (`format: "mermaid"`, checked by the engine) goes to merman; an older card's
//! box drawing goes the `diagram` fence's way.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    pub source: String,
    pub mermaid: bool,
}

impl Flow {
    /// A diagram spec; `None` for another component or an empty source.
    pub fn from_spec(spec: &Value) -> Option<Self> {
        if spec["component"] != "diagram" {
            return None;
        }
        let source = spec["source"]
            .as_str()
            .filter(|s| !s.trim().is_empty())?
            .to_string();
        Some(Self {
            source,
            mermaid: spec["format"] == "mermaid",
        })
    }

    /// The fence it draws as, with its id.
    pub fn block(&self, id: String) -> crate::markdown::TranscriptBlock {
        let language = if self.mermaid { "mermaid" } else { "diagram" };
        let mut block =
            crate::markdown::code_block(Some(language.into()), self.source.clone(), true);
        block.set_id(id);
        block
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::TranscriptBlock;
    use serde_json::json;

    #[test]
    fn a_card_draws_as_its_fence() {
        let new = Flow::from_spec(&json!({ "component": "diagram", "format": "mermaid", "source": "flowchart TD\n  A --> B" })).unwrap();
        assert!(
            matches!(new.block("m#ui-d".into()), TranscriptBlock::Code { ref id, language: Some(ref l), closed: true, .. } if id == "m#ui-d" && l == "mermaid")
        );
        // A card from before Mermaid keeps its box drawing.
        let old =
            Flow::from_spec(&json!({ "component": "diagram", "source": "┌───┐\n│ A │\n└───┘" }))
                .unwrap();
        assert!(
            matches!(old.block("x".into()), TranscriptBlock::Code { language: Some(ref l), .. } if l == "diagram")
        );
        assert!(Flow::from_spec(&json!({ "component": "diagram", "source": "  " })).is_none());
    }
}

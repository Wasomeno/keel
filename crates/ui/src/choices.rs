//! Choice buttons the engine's generative UI chose for a reply (a `ui` part
//! with `component: "choices"`): the options of a list the agent asked the
//! user to pick from. A click sends the option's label as the next message.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choices {
    pub options: Vec<String>,
    /// The option the agent recommended, drawn as the primary button.
    pub recommended: Option<usize>,
}

impl Choices {
    /// A choices spec; `None` for another component or a malformed one.
    pub fn from_spec(spec: &Value) -> Option<Self> {
        if spec["component"] != "choices" {
            return None;
        }
        let options: Vec<String> = spec["options"]
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .filter(|label| !label.trim().is_empty())
            .map(str::to_string)
            .collect();
        if options.len() < 2 {
            return None;
        }
        let recommended = spec["recommended"]
            .as_u64()
            .map(|ix| ix as usize)
            .filter(|&ix| ix < options.len());
        Some(Self {
            options,
            recommended,
        })
    }

    /// For the transcript's height memo.
    pub fn content_len(&self) -> usize {
        self.options.iter().map(|o| o.len() + 1).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn spec_needs_two_options_and_a_recommendation_in_range() {
        let choices = Choices::from_spec(
            &json!({ "component": "choices", "options": ["Redis", "Memory"], "recommended": 1 }),
        )
        .unwrap();
        assert_eq!(choices.recommended, Some(1));
        let out_of_range = Choices::from_spec(
            &json!({ "component": "choices", "options": ["a", "b"], "recommended": 5 }),
        )
        .unwrap();
        assert_eq!(out_of_range.recommended, None);
        assert!(
            Choices::from_spec(&json!({ "component": "choices", "options": ["a", " "] })).is_none()
        );
        assert!(Choices::from_spec(&json!({ "component": "chart" })).is_none());
    }
}

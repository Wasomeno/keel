//! A numbered step list the engine's generative UI chose for a reply (a `ui`
//! part with `component: "steps"`). `current` is the step in progress; the
//! ones before it are done.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StepKind {
    #[default]
    Task,
    Guide,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Steps {
    pub items: Vec<String>,
    /// Index of the step in progress. `None` when every step is done.
    pub current: Option<usize>,
    pub kind: StepKind,
}

impl Steps {
    /// A steps spec; `None` for another component or fewer than two steps.
    pub fn from_spec(spec: &Value) -> Option<Self> {
        if spec["component"] != "steps" {
            return None;
        }
        let items: Vec<String> = spec["items"]
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if items.len() < 2 {
            return None;
        }
        let current = spec["current"].as_u64().map(|ix| ix as usize).filter(|&ix| ix < items.len());
        let kind = match spec["kind"].as_str() {
            Some("guide") => StepKind::Guide,
            _ => StepKind::Task,
        };
        Some(Self { items, current, kind })
    }

    /// How many steps are done: everything before `current`, or all of them.
    pub fn done(&self) -> usize {
        self.current.unwrap_or(self.items.len())
    }

    pub fn content_len(&self) -> usize {
        self.items.iter().map(|s| s.len() + 1).sum::<usize>() + self.current.unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn current_marks_the_step_in_progress_and_an_out_of_range_one_is_dropped() {
        let steps = Steps::from_spec(&json!({
            "component": "steps",
            "items": ["Read the test", "Close the watcher"],
            "current": 1
        }))
        .unwrap();
        assert_eq!((steps.done(), steps.current), (1, Some(1)));
        assert_eq!(steps.kind, StepKind::Task);

        let guide = Steps::from_spec(&json!({
            "component": "steps",
            "items": ["Read the test", "Close the watcher"],
            "current": 1,
            "kind": "guide"
        }))
        .unwrap();
        assert_eq!(guide.kind, StepKind::Guide);
        let finished = Steps::from_spec(&json!({
            "component": "steps",
            "items": ["a", "b"],
            "current": 9
        }))
        .unwrap();
        assert_eq!(finished.current, None);
        assert_eq!(finished.done(), 2);
        assert!(Steps::from_spec(&json!({ "component": "steps", "items": ["only"] })).is_none());
    }
}

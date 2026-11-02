//! Shell blocks the engine's generative UI marked runnable (a `ui` part with
//! `component: "commands"`): each code block whose text matches gets a Run
//! button that types the command into a new terminal tab.

use serde_json::Value;

/// What a code block's Run button runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub command: String,
    /// Deletes, pushes, deploys or needs sudo: Run asks for a second click.
    pub risky: bool,
}

/// A commands spec: each block's text (to find its code block) and its run.
pub fn from_spec(spec: &Value) -> Option<Vec<(String, Run)>> {
    if spec["component"] != "commands" {
        return None;
    }
    let items: Vec<(String, Run)> = spec["items"]
        .as_array()?
        .iter()
        .filter_map(|item| {
            let run = Run { command: item["run"].as_str()?.to_string(), risky: item["risky"] == true };
            Some((item["content"].as_str()?.trim().to_string(), run))
        })
        .filter(|(content, run)| !content.is_empty() && !run.command.trim().is_empty())
        .collect();
    (!items.is_empty()).then_some(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn items_pair_block_text_with_the_command() {
        let items = from_spec(&json!({ "component": "commands", "items": [
            { "content": "$ cargo t\ntest result: ok\n", "run": "cargo t", "risky": false },
            { "content": "git push --force", "run": "git push --force", "risky": true },
        ] }))
        .unwrap();
        assert_eq!(items[0].0, "$ cargo t\ntest result: ok");
        assert_eq!(items[1].1, Run { command: "git push --force".into(), risky: true });
    }
}

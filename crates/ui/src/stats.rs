//! Stat tiles the engine's generative UI chose for a reply (a `ui` part
//! with `component: "stats"`): before → after figures the reply reported,
//! and which direction the agent said is better.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Better {
    Lower,
    Higher,
    Neither,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub label: String,
    /// As the reply wrote them: `41s`, `412 KB`.
    pub before: String,
    pub after: String,
    /// Change in whole percent; `None` when the before value is zero.
    pub change: Option<i64>,
    pub better: Better,
}

impl Stat {
    /// Whether the change is good (`Some(true)`), bad, or neither.
    pub fn verdict(&self) -> Option<bool> {
        let change = self.change.filter(|c| *c != 0)?;
        match self.better {
            Better::Lower => Some(change < 0),
            Better::Higher => Some(change > 0),
            Better::Neither => None,
        }
    }

    /// `↓ 32%`, `↑ 7%`, or empty.
    pub fn change_text(&self) -> String {
        match self.change {
            Some(c) if c < 0 => format!("↓ {}%", -c),
            Some(c) if c > 0 => format!("↑ {c}%"),
            _ => String::new(),
        }
    }
}

/// A stats spec's tiles; `None` for another component or no tiles.
pub fn from_spec(spec: &Value) -> Option<Vec<Stat>> {
    if spec["component"] != "stats" {
        return None;
    }
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
    let items: Vec<Stat> = spec["items"]
        .as_array()?
        .iter()
        .map(|item| Stat {
            label: s(item, "label"),
            before: s(item, "before"),
            after: s(item, "after"),
            change: item["change"].as_f64().map(|c| c.round() as i64),
            better: match item["better"].as_str() {
                Some("lower") => Better::Lower,
                Some("higher") => Better::Higher,
                _ => Better::Neither,
            },
        })
        .filter(|stat| !stat.label.is_empty() && !stat.after.is_empty())
        .collect();
    (!items.is_empty()).then_some(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_drop_is_good_when_lower_is_better() {
        let tiles = from_spec(&json!({ "component": "stats", "items": [
            { "label": "Build time", "before": "41s", "after": "28s", "change": -31.7, "better": "lower" },
            { "label": "Coverage", "before": "71%", "after": "68%", "change": -4.2, "better": "higher" },
            { "label": "Files", "before": "0", "after": "3", "change": null, "better": "neither" },
        ] }))
        .unwrap();
        assert_eq!(
            (tiles[0].verdict(), tiles[0].change_text().as_str()),
            (Some(true), "↓ 32%")
        );
        assert_eq!(tiles[1].verdict(), Some(false));
        assert_eq!(
            (tiles[2].verdict(), tiles[2].change_text().as_str()),
            (None, "")
        );
        assert!(from_spec(&json!({ "component": "stats", "items": [] })).is_none());
    }
}

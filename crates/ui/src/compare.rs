//! Side-by-side claims the engine's generative UI chose for a reply (a `ui`
//! part with `component: "compare"`). Numbers stay on the stats card.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub title: String,
    pub points: Vec<String>,
    pub recommended: bool,
}

/// A compare spec's columns; `None` for another component or fewer than two.
pub fn from_spec(spec: &Value) -> Option<Vec<Column>> {
    if spec["component"] != "compare" {
        return None;
    }
    let columns: Vec<Column> = spec["columns"]
        .as_array()?
        .iter()
        .filter_map(|col| {
            let title = col["title"].as_str().unwrap_or_default().trim().to_string();
            let points: Vec<String> = col["points"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            (!title.is_empty() && !points.is_empty()).then_some(Column {
                title,
                points,
                recommended: col["recommended"] == true,
            })
        })
        .take(3)
        .collect();
    (columns.len() >= 2).then_some(columns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn two_columns_keep_a_recommendation_and_one_column_is_dropped() {
        let columns = from_spec(&json!({
            "component": "compare",
            "columns": [
                { "title": "Redis", "points": ["Survives a restart"], "recommended": true },
                { "title": "Memory", "points": ["Nothing to run", " "] }
            ]
        }))
        .unwrap();
        assert_eq!(columns[0].recommended, true);
        assert_eq!(columns[1].points, vec!["Nothing to run".to_string()]);
        assert!(from_spec(&json!({ "component": "compare", "columns": [{ "title": "Only", "points": ["a"] }] })).is_none());
    }
}

//! Evidence & source citations (`component: "citations"`): links inline
//! `[1]`, `[2]` badges in agent prose to exact file lines, diffs, command
//! outputs, or URLs verified during the turn.

use std::ops::Range;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CitationKind {
    File,
    Diff,
    Command,
    Url,
}

impl CitationKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "file" => Some(Self::File),
            "diff" => Some(Self::Diff),
            "command" => Some(Self::Command),
            "url" => Some(Self::Url),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::File => "File",
            Self::Diff => "Diff",
            Self::Command => "Output",
            Self::Url => "Source",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationItem {
    pub id: usize,
    pub kind: CitationKind,
    pub label: String,
    pub excerpt: String,
    pub path: Option<String>,
    pub lines: Option<(u32, u32)>,
    pub url: Option<String>,
    pub command: Option<String>,
}

impl CitationItem {
    /// Display path with optional `:start–end` line suffix.
    pub fn location_text(&self) -> String {
        match (&self.path, self.lines) {
            (Some(path), Some((start, end))) if end > start => {
                format!("{path}:{start}–{end}")
            }
            (Some(path), Some((start, _))) => format!("{path}:{start}"),
            (Some(path), None) => path.clone(),
            (None, _) => self
                .command
                .clone()
                .or_else(|| self.url.clone())
                .unwrap_or_else(|| self.label.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Citations {
    pub items: Vec<CitationItem>,
}

impl Citations {
    /// Parse a `component: "citations"` UI part spec.
    pub fn from_spec(spec: &Value) -> Option<Self> {
        if spec["component"] != "citations" {
            return None;
        }
        let raw_items = spec["items"].as_array()?;
        let mut items = Vec::with_capacity(raw_items.len());
        let mut used_ids = std::collections::HashSet::new();
        let mut next_id = 1usize;

        for raw in raw_items {
            let kind = CitationKind::parse(raw["kind"].as_str().unwrap_or(""))?;
            let label = raw["label"].as_str().unwrap_or("").trim().to_string();
            if label.is_empty() {
                continue;
            }
            let excerpt = raw["excerpt"].as_str().unwrap_or("").trim().to_string();
            let path = raw["path"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let url = raw["url"]
                .as_str()
                .map(str::trim)
                .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
                .map(str::to_string);
            let command = raw["command"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            if excerpt.is_empty() && url.is_none() {
                continue;
            }
            let lines = raw["lines"].as_array().and_then(|arr| {
                let start = arr.first()?.as_u64()? as u32;
                if start == 0 {
                    return None;
                }
                let end = arr
                    .get(1)
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or(start);
                Some((start, end.max(start)))
            });

            let requested = raw["id"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|&n| n >= 1 && !used_ids.contains(&n));
            let id = match requested {
                Some(id) => id,
                None => {
                    while used_ids.contains(&next_id) {
                        next_id += 1;
                    }
                    next_id
                }
            };
            used_ids.insert(id);

            items.push(CitationItem {
                id,
                kind,
                label,
                excerpt,
                path,
                lines,
                url,
                command,
            });
        }

        if items.is_empty() {
            return None;
        }
        Some(Self { items })
    }

    pub fn get(&self, id: usize) -> Option<&CitationItem> {
        self.items.iter().find(|item| item.id == id)
    }

    pub fn ids(&self) -> Vec<usize> {
        self.items.iter().map(|item| item.id).collect()
    }

    /// Merge another citation block from the same assistant turn into `self`.
    pub fn extend_unique(&mut self, other: &Self) {
        for item in &other.items {
            if self.get(item.id).is_none() {
                self.items.push(item.clone());
            }
        }
    }

    pub fn content_len(&self) -> usize {
        self.items
            .iter()
            .map(|item| item.label.len() + item.excerpt.len() + 16)
            .sum::<usize>()
            + 16
    }
}

/// Find inline `[1]`, `[2]` citation tokens in a plain-text slice whose numbers
/// match one of `available_ids`. Adjacent `[1][2]` matches both cleanly.
pub fn find_citation_badges(text: &str, available_ids: &[usize]) -> Vec<(Range<usize>, usize)> {
    if available_ids.is_empty() || !text.contains('[') {
        return Vec::new();
    }
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            let prev_ok = i == 0 || {
                let prev = text[..i].chars().next_back();
                prev.is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
            };
            if prev_ok {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j > i + 1
                    && j < bytes.len()
                    && bytes[j] == b']'
                    && bytes.get(j + 1) != Some(&b'(')
                    && let Ok(id) = text[i + 1..j].parse::<usize>()
                    && available_ids.contains(&id)
                {
                    found.push((i..j + 1, id));
                    i = j + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_valid_citation_spec_and_skips_incomplete_items() {
        let spec = json!({
            "component": "citations",
            "items": [
                {
                    "id": 1,
                    "kind": "file",
                    "label": "transcript.rs:3856",
                    "path": "crates/ui/src/transcript.rs",
                    "lines": [3856, 3862],
                    "excerpt": "let now_hovered = !menu_open && hb.is_hovered(window);"
                },
                {
                    "kind": "command",
                    "label": "cargo t",
                    "command": "cargo t",
                    "excerpt": "398 passed; 0 failed"
                },
                {
                    "kind": "file",
                    "label": "   ",
                    "excerpt": "ignored"
                }
            ]
        });
        let parsed = Citations::from_spec(&spec).unwrap();
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(parsed.items[0].id, 1);
        assert_eq!(parsed.items[0].lines, Some((3856, 3862)));
        assert_eq!(parsed.items[0].location_text(), "crates/ui/src/transcript.rs:3856–3862");
        assert_eq!(parsed.items[1].id, 2);
        assert_eq!(parsed.items[1].kind, CitationKind::Command);
    }

    #[test]
    fn finds_only_valid_inline_citation_badges() {
        let ids = [1, 2, 12];
        assert_eq!(
            find_citation_badges("Fixed lag [1][2] and skipped arr[1] and [99].", &ids),
            vec![(10..13, 1), (13..16, 2)]
        );
        assert_eq!(
            find_citation_badges("Claim [12]. Fixed.[1] Not [link](https://example.com) or [1](x).", &ids),
            vec![(6..10, 12), (18..21, 1)]
        );
    }
}

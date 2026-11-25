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

    /// `start–end` when the citation names a line range, or the single line.
    pub fn line_span(&self) -> Option<String> {
        self.lines.map(|(start, end)| {
            if end > start {
                format!("{start}–{end}")
            } else {
                start.to_string()
            }
        })
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

/// One row of the sources list.
///
/// File citations that share a path are one row, in the order they were
/// cited. Everything else (commands, URLs, diffs, and files with no path)
/// is its own row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CitationGroup {
    File {
        path: String,
        items: Vec<CitationItem>,
    },
    Single(CitationItem),
}

impl CitationGroup {
    pub fn items(&self) -> &[CitationItem] {
        match self {
            Self::File { items, .. } => items,
            Self::Single(item) => std::slice::from_ref(item),
        }
    }
}

/// Header under the hairline, design px.
pub const SOURCE_HEAD_H: f32 = 24.;
/// One source row at zoom 1, design px.
pub const SOURCE_ROW_H: f32 = 28.;
/// Space under the hairline, and between the header and the first row.
pub const SOURCE_PAD: f32 = 4.;
/// Grouped rows at or under this start open. Longer lists start closed.
pub const SOURCE_OPEN_MAX: usize = 4;
/// Ranges that fit beside a filename on the reading measure before wrapping.
const SOURCE_RANGES_PER_LINE: usize = 6;

/// Group `items` for the sources list.
///
/// File citations with the same path join the group at that path's first
/// appearance, so a later range does not open a second row and the groups
/// stay in citation order. Diffs are not files: the same path stays its own
/// row.
pub fn group_citation_rows(items: &[CitationItem]) -> Vec<CitationGroup> {
    let mut groups = Vec::new();
    let mut file_at: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for item in items {
        if item.kind == CitationKind::File
            && let Some(path) = item.path.as_deref()
        {
            if let Some(&ix) = file_at.get(path) {
                if let CitationGroup::File { items, .. } = &mut groups[ix] {
                    items.push(item.clone());
                }
                continue;
            }
            file_at.insert(path, groups.len());
            groups.push(CitationGroup::File {
                path: path.to_string(),
                items: vec![item.clone()],
            });
            continue;
        }
        groups.push(CitationGroup::Single(item.clone()));
    }
    groups
}

/// Whether the sources list is open. `forced_open` / `forced_closed` are the
/// user's later choice; with neither, lists of more than [`SOURCE_OPEN_MAX`]
/// grouped rows start closed.
pub fn sources_expanded(row_count: usize, forced_open: bool, forced_closed: bool) -> bool {
    if forced_open {
        return true;
    }
    if forced_closed {
        return false;
    }
    row_count <= SOURCE_OPEN_MAX
}

fn group_lines(count: usize) -> usize {
    count.div_ceil(SOURCE_RANGES_PER_LINE).max(1)
}

/// Design-pixel height of the open rows, including the gap under the header.
pub fn source_body_height(groups: &[CitationGroup]) -> f32 {
    if groups.is_empty() {
        return 0.;
    }
    SOURCE_PAD
        + groups
            .iter()
            .map(|group| group_lines(group.items().len()) as f32 * SOURCE_ROW_H)
            .sum::<f32>()
}

/// Design-pixel height of the sources block, hairline excluded.
/// Collapsed is the header only, so the memo key can tell the two apart.
pub fn source_list_height(groups: &[CitationGroup], expanded: bool) -> f32 {
    let head = SOURCE_PAD + SOURCE_HEAD_H;
    if expanded {
        head + source_body_height(groups)
    } else {
        head
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

    fn cite(
        id: usize,
        kind: CitationKind,
        path: Option<&str>,
        lines: Option<(u32, u32)>,
    ) -> CitationItem {
        CitationItem {
            id,
            kind,
            label: path.unwrap_or("cargo t").to_string(),
            excerpt: "cited".into(),
            path: path.map(str::to_string),
            lines,
            url: (kind == CitationKind::Url).then(|| "https://example.com".into()),
            command: (kind == CitationKind::Command).then(|| "cargo t".into()),
        }
    }

    fn shape(groups: &[CitationGroup]) -> Vec<(String, Vec<usize>)> {
        groups
            .iter()
            .map(|group| match group {
                CitationGroup::File { path, items } => (
                    format!("file:{path}"),
                    items.iter().map(|item| item.id).collect(),
                ),
                CitationGroup::Single(item) => {
                    let kind = match item.kind {
                        CitationKind::File => "file",
                        CitationKind::Diff => "diff",
                        CitationKind::Command => "command",
                        CitationKind::Url => "url",
                    };
                    (format!("{kind}:{}", item.id), vec![item.id])
                }
            })
            .collect()
    }

    #[test]
    fn groups_file_ranges_by_path_and_keeps_first_seen_order() {
        let items = vec![
            cite(
                1,
                CitationKind::File,
                Some("crates/ui/src/transcript.rs"),
                Some((10, 12)),
            ),
            cite(
                2,
                CitationKind::File,
                Some("crates/ui/src/transcript.rs"),
                Some((40, 40)),
            ),
            cite(3, CitationKind::Command, None, None),
            cite(
                4,
                CitationKind::File,
                Some("crates/ui/src/transcript.rs"),
                Some((80, 90)),
            ),
            cite(
                5,
                CitationKind::File,
                Some("crates/ui/src/style.rs"),
                Some((1, 4)),
            ),
            cite(
                6,
                CitationKind::Diff,
                Some("crates/ui/src/transcript.rs"),
                Some((10, 12)),
            ),
            cite(7, CitationKind::Url, None, None),
            cite(8, CitationKind::File, None, Some((3, 3))),
            cite(9, CitationKind::File, Some("src/a.rs"), None),
            cite(10, CitationKind::File, Some("tests/a.rs"), Some((2, 2))),
        ];
        assert_eq!(
            shape(&group_citation_rows(&items)),
            vec![
                ("file:crates/ui/src/transcript.rs".into(), vec![1, 2, 4]),
                ("command:3".into(), vec![3]),
                ("file:crates/ui/src/style.rs".into(), vec![5]),
                ("diff:6".into(), vec![6]),
                ("url:7".into(), vec![7]),
                ("file:8".into(), vec![8]),
                ("file:src/a.rs".into(), vec![9]),
                ("file:tests/a.rs".into(), vec![10]),
            ]
        );
        assert!(group_citation_rows(&[]).is_empty());
        // A command before any file stays first; the later range joins the file.
        let lead = vec![
            cite(1, CitationKind::Command, None, None),
            cite(2, CitationKind::File, Some("a.rs"), Some((1, 1))),
            cite(3, CitationKind::File, Some("b.rs"), Some((1, 1))),
            cite(4, CitationKind::File, Some("a.rs"), Some((9, 9))),
        ];
        assert_eq!(
            shape(&group_citation_rows(&lead)),
            vec![
                ("command:1".into(), vec![1]),
                ("file:a.rs".into(), vec![2, 4]),
                ("file:b.rs".into(), vec![3]),
            ]
        );
    }

    #[test]
    fn sources_start_open_through_four_rows_and_a_choice_wins() {
        assert!(sources_expanded(1, false, false));
        assert!(sources_expanded(SOURCE_OPEN_MAX, false, false));
        assert!(!sources_expanded(SOURCE_OPEN_MAX + 1, false, false));
        assert!(sources_expanded(9, true, false));
        assert!(!sources_expanded(1, false, true));
        // Open wins if both are set; the toggle never stores both.
        assert!(sources_expanded(9, true, true));

        let one = group_citation_rows(&[cite(1, CitationKind::File, Some("a.rs"), Some((1, 2)))]);
        let two = group_citation_rows(&[
            cite(1, CitationKind::File, Some("a.rs"), Some((1, 2))),
            cite(2, CitationKind::Command, None, None),
        ]);
        assert_eq!(
            source_list_height(&one, false),
            source_list_height(&two, false)
        );
        assert!(source_list_height(&one, true) > source_list_height(&one, false));
        assert_eq!(
            source_list_height(&two, true) - source_list_height(&one, true),
            SOURCE_ROW_H
        );
        assert_eq!(
            cite(1, CitationKind::File, Some("a.rs"), Some((10, 12)))
                .line_span()
                .as_deref(),
            Some("10–12")
        );
        assert_eq!(
            cite(1, CitationKind::File, Some("a.rs"), Some((4, 4)))
                .line_span()
                .as_deref(),
            Some("4")
        );
    }
}

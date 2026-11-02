//! The files a turn changed, as the engine's generative UI chose to show
//! them (a `ui` part with `component: "files"`): each with its git status
//! and line counts against the last commit.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// Relative to the chat folder.
    pub path: String,
    /// `M`, `A`, `D`, `R`, `U` or `?` (untracked).
    pub status: char,
    /// `None` for binary and untracked files.
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChanges {
    /// The chat folder, to open a file.
    pub root: String,
    pub files: Vec<FileChange>,
    /// Changed files not listed.
    pub more: usize,
}

impl FileChanges {
    /// A files spec; `None` for another component or no files.
    pub fn from_spec(spec: &Value) -> Option<Self> {
        if spec["component"] != "files" {
            return None;
        }
        let count = |v: &Value| v.as_u64().map(|n| n as u32);
        let files: Vec<FileChange> = spec["files"]
            .as_array()?
            .iter()
            .filter_map(|f| {
                Some(FileChange {
                    path: f["path"].as_str()?.to_string(),
                    status: f["status"].as_str().and_then(|s| s.chars().next()).unwrap_or('M'),
                    added: count(&f["added"]),
                    removed: count(&f["removed"]),
                })
            })
            .collect();
        (!files.is_empty()).then(|| Self {
            root: spec["root"].as_str().unwrap_or_default().to_string(),
            files,
            more: spec["more"].as_u64().unwrap_or(0) as usize,
        })
    }

    /// Lines added and removed across the listed files.
    pub fn totals(&self) -> (u32, u32) {
        self.files.iter().fold((0, 0), |(a, r), f| (a + f.added.unwrap_or(0), r + f.removed.unwrap_or(0)))
    }

    /// "3 files changed".
    pub fn headline(&self) -> String {
        let n = self.files.len() + self.more;
        format!("{n} {} changed", if n == 1 { "file" } else { "files" })
    }

    /// The absolute path of a listed file.
    pub fn abs(&self, file: &FileChange) -> String {
        std::path::Path::new(&self.root).join(&file.path).to_string_lossy().into_owned()
    }

    /// For the transcript's height memo.
    pub fn content_len(&self) -> usize {
        self.files.iter().map(|f| f.path.len() + 4).sum::<usize>() + self.more
    }
}

/// Five diffstat blocks, as GitHub draws them: how many are green and red.
pub fn blocks(added: u32, removed: u32) -> (usize, usize) {
    let total = added + removed;
    if total == 0 {
        return (0, 0);
    }
    let filled = (total as usize).min(5);
    let green = ((added as f32 / total as f32) * filled as f32).round() as usize;
    (green, filled - green)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn totals_headline_and_blocks() {
        let card = FileChanges::from_spec(&json!({ "component": "files", "root": "/w", "more": 1, "files": [
            { "path": "src/a.rs", "status": "M", "added": 12, "removed": 3 },
            { "path": "img.png", "status": "?", "added": null, "removed": null },
        ] }))
        .unwrap();
        assert_eq!(card.totals(), (12, 3));
        assert_eq!(card.headline(), "3 files changed");
        assert_eq!(card.abs(&card.files[0]), "/w/src/a.rs");
        assert_eq!(blocks(12, 3), (4, 1));
        assert_eq!(blocks(1, 1), (1, 1));
        assert_eq!(blocks(0, 0), (0, 0));
    }
}

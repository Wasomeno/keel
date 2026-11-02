//! Slash commands and skills Keel's Pi picks up from disk, for the
//! composer's `/` menu. The entry carries the exact text to put in the
//! message, and Pi expands it when the message arrives: `prompts/*.md` as
//! `/name` (a subfolder namespaces the name, `a/b.md` → `/a:b`) and
//! `skills/*/SKILL.md` as `/skill:name`, under Keel's Pi folder and the
//! project's `.pi/`.
//!
//! A project entry wins over a user one with the same name.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandEntry {
    /// What the menu shows: `review`, `frontend:component`.
    pub name: String,
    /// The text that invokes it, as typed into the message: `/review`.
    pub insert: String,
    pub description: String,
    /// `"command"` or `"skill"`.
    pub kind: &'static str,
    /// `"project"` or `"user"`.
    pub scope: &'static str,
}

/// Most entries returned; a runaway commands folder can't flood the menu.
const MAX_ENTRIES: usize = 500;
/// Deepest a commands folder is walked for namespaced commands.
const MAX_DEPTH: usize = 4;

/// The commands and skills Pi would pick up in `cwd` with `agent` as its
/// folder, project ones first, each group by name.
pub fn list_commands_sync(cwd: Option<&Path>, agent: &Path) -> Vec<CommandEntry> {
    // (folder, kind, scope, invoke prefix)
    let mut sources: Vec<(PathBuf, &'static str, &'static str, &'static str)> = Vec::new();
    for (rel, kind, prefix) in [("prompts", "command", "/"), ("skills", "skill", "/skill:")] {
        if let Some(cwd) = cwd {
            sources.push((cwd.join(".pi").join(rel), kind, "project", prefix));
        }
        sources.push((agent.join(rel), kind, "user", prefix));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (dir, kind, scope, prefix) in sources {
        let mut found = if kind == "skill" { skills_in(&dir) } else { commands_in(&dir) };
        found.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, description) in found {
            if out.len() >= MAX_ENTRIES {
                return out;
            }
            // Same kind and name: the project's shadows the user's.
            if seen.insert((kind, name.clone())) {
                out.push(CommandEntry { insert: format!("{prefix}{name}"), name, description, kind, scope });
            }
        }
    }
    out
}

/// `*.md` under `dir`, a subfolder joining the name with `:`.
fn commands_in(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk_md(dir, dir, 0, &mut out);
    out
}

fn walk_md(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < MAX_DEPTH {
                walk_md(root, &path, depth + 1, out);
            }
        } else if path.extension().is_some_and(|e| e == "md") {
            let Ok(rel) = path.with_extension("").strip_prefix(root).map(Path::to_path_buf) else {
                continue;
            };
            let name = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join(":");
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            out.push((name, describe(&text).1));
        }
    }
}

/// Each `*/SKILL.md` under `dir`: its front matter `name` (else the folder's).
fn skills_in(dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let text = std::fs::read_to_string(entry.path().join("SKILL.md")).ok()?;
            let (name, description) = describe(&text);
            let name = name.unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
            Some((name, description))
        })
        .collect()
}

/// A markdown file's front matter `name` and `description`; without a
/// description, its first line of text, cut to a menu's length.
fn describe(text: &str) -> (Option<String>, String) {
    let mut lines = text.lines();
    let (mut name, mut description) = (None, None);
    let mut body_start = 0;
    if text.starts_with("---") {
        lines.next();
        for (ix, line) in lines.by_ref().enumerate() {
            if line.trim() == "---" {
                body_start = ix + 2;
                break;
            }
            let unquote = |v: &str| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
            if let Some(v) = line.strip_prefix("name:") {
                name = Some(unquote(v)).filter(|v| !v.is_empty());
            } else if let Some(v) = line.strip_prefix("description:") {
                description = Some(unquote(v)).filter(|v| !v.is_empty());
            }
        }
    }
    let description = description.unwrap_or_else(|| {
        text.lines()
            .skip(body_start)
            .map(|l| l.trim().trim_start_matches('#').trim())
            .find(|l| !l.is_empty())
            .unwrap_or_default()
            .to_string()
    });
    let description = match description.char_indices().nth(140) {
        Some((cut, _)) => format!("{}…", &description[..cut]),
        None => description,
    };
    (name, description)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn finds_pi_commands_and_skills_project_first() {
        let tmp = std::env::temp_dir().join(format!("keel-commands-{}", uuid::Uuid::new_v4()));
        let (agent, project) = (tmp.join("agent"), tmp.join("project"));
        write(&agent.join("prompts/review.md"), "---\ndescription: Review the diff\n---\nBody");
        write(&agent.join("prompts/deploy.md"), "# Deploy to staging\nsteps");
        write(&project.join(".pi/prompts/review.md"), "Project review");
        write(&project.join(".pi/prompts/frontend/component.md"), "Make a component");
        write(&agent.join("skills/pdf/SKILL.md"), "---\nname: pdf-tools\ndescription: \"Work with PDFs\"\n---\n");
        write(&agent.join("skills/bare/SKILL.md"), "Just text");

        let got = list_commands_sync(Some(&project), &agent);
        let names: Vec<(&str, &str, &str)> = got.iter().map(|c| (c.insert.as_str(), c.scope, c.description.as_str())).collect();
        assert_eq!(
            names,
            [
                ("/frontend:component", "project", "Make a component"),
                ("/review", "project", "Project review"),
                ("/deploy", "user", "Deploy to staging"),
                ("/skill:bare", "user", "Just text"),
                ("/skill:pdf-tools", "user", "Work with PDFs"),
            ]
        );
        assert_eq!(got.iter().map(|c| c.kind).collect::<Vec<_>>(), ["command", "command", "command", "skill", "skill"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

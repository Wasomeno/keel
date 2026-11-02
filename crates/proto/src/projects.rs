//! Projects: a name over an ordered set of folders. The first folder is
//! where the project's chats start (the harness's working folder); the
//! others reach the agent as named roots (`web` -> `/Users/me/web`), and
//! `@alias/path` in a message means that path. The UI keeps the list in
//! `{data_dir}/projects.json`; the engine reads it when a run starts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The file under the data folder.
pub const FILE: &str = "projects.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFolder {
    /// Absolute path.
    pub path: String,
    /// The short name it goes by (`web`, `api`), unique in its project.
    pub alias: String,
    /// The letter its mark shows; empty, the alias's first.
    #[serde(default)]
    pub letter: String,
    /// What the folder is for, in a few words. Optional.
    #[serde(default)]
    pub note: String,
}

impl ProjectFolder {
    /// The mark's letter, upper case.
    pub fn mark(&self) -> String {
        let from = if self.letter.trim().is_empty() { &self.alias } else { &self.letter };
        from.trim().chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// In order; the first is primary.
    pub folders: Vec<ProjectFolder>,
    #[serde(default)]
    pub created_at: u64,
}

impl Project {
    pub fn primary(&self) -> Option<&ProjectFolder> {
        self.folders.first()
    }

    pub fn folder(&self, alias: &str) -> Option<&ProjectFolder> {
        self.folders.iter().find(|f| f.alias == alias)
    }
}

/// Which project a chat belongs to, and the folders it works in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatScope {
    pub project: String,
    /// Aliases in scope. Empty: every folder. The primary is always in.
    #[serde(default)]
    pub scope: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projects {
    #[serde(default)]
    pub projects: Vec<Project>,
    /// Chat id -> its project and scope.
    #[serde(default)]
    pub chats: BTreeMap<String, ChatScope>,
}

impl Projects {
    /// A missing or unreadable file is no projects.
    pub fn parse(text: &str) -> Self {
        serde_json::from_str(text).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn by_name(&self, name: &str) -> Option<&Project> {
        let name = name.trim();
        self.projects.iter().find(|p| p.name.eq_ignore_ascii_case(name) || p.id == name)
    }

    /// The chat's project and its folders in scope, primary first.
    pub fn for_chat(&self, chat: &str) -> Option<(&Project, Vec<&ProjectFolder>)> {
        let link = self.chats.get(chat)?;
        let project = self.project(&link.project)?;
        let folders = in_scope(project, &link.scope);
        Some((project, folders))
    }

    /// `(alias, absolute path)` for every folder in the chat's scope, primary
    /// first. Empty for a chat outside any project.
    pub fn roots_for_chat(&self, chat: &str) -> Vec<(String, String)> {
        self.for_chat(chat)
            .map(|(_, fs)| fs.into_iter().map(|f| (f.alias.clone(), f.path.clone())).collect())
            .unwrap_or_default()
    }

    pub fn chats_of(&self, project: &str) -> Vec<&str> {
        self.chats.iter().filter(|(_, l)| l.project == project).map(|(c, _)| c.as_str()).collect()
    }
}

/// `scope` (aliases; empty, all) applied to `project`, primary always first.
pub fn in_scope<'a>(project: &'a Project, scope: &[String]) -> Vec<&'a ProjectFolder> {
    project
        .folders
        .iter()
        .enumerate()
        .filter(|(i, f)| *i == 0 || scope.is_empty() || scope.iter().any(|a| a == &f.alias))
        .map(|(_, f)| f)
        .collect()
}

/// A short alias for a folder: its name's last word, lower case
/// (`resona-finance-cms` -> `cms`), made unique against `taken`.
pub fn default_alias(path: &str, taken: &[String]) -> String {
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("folder");
    let word = name.rsplit(['-', '_', '.', ' ']).find(|w| !w.is_empty()).unwrap_or(name);
    let base: String = word.to_lowercase().chars().filter(|c| c.is_alphanumeric()).take(12).collect();
    let base = if base.is_empty() { "folder".to_string() } else { base };
    let mut alias = base.clone();
    let mut n = 2;
    while taken.iter().any(|t| t == &alias) {
        alias = format!("{base}{n}");
        n += 1;
    }
    alias
}

/// Whether `alias` can name a root: letters, digits, `-` and `_`.
pub fn valid_alias(alias: &str) -> bool {
    !alias.is_empty() && alias.len() <= 24 && alias.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// `@alias` / `@alias/rest` (as the files pane and the engine spell a path
/// in another root): the root's absolute path and the rest.
pub fn split_root<'a>(roots: &'a [(String, String)], path: &'a str) -> Option<(&'a str, &'a str)> {
    let tail = path.strip_prefix('@')?;
    let (alias, rest) = tail.split_once('/').unwrap_or((tail, ""));
    roots.iter().find(|(a, _)| a == alias).map(|(_, abs)| (abs.as_str(), rest))
}

/// Every `@alias/...` in `text` naming one of `roots`, spelled out as the
/// absolute path (`@api/src/x.ts` -> `@/Users/me/api/src/x.ts`). An `@`
/// inside a word (an email) is left alone.
pub fn resolve_mentions(text: &str, roots: &[(String, String)]) -> String {
    if roots.is_empty() || !text.contains('@') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut prev: Option<char> = None;
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let ch = rest.chars().next().unwrap_or(' ');
        if ch == '@' && prev.is_none_or(|p| p.is_whitespace() || "([{\"'`".contains(p)) {
            let after = &rest[1..];
            let word_end = after.find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')).unwrap_or(after.len());
            let alias = &after[..word_end];
            let next = after[word_end..].chars().next();
            let ends = next.is_none_or(|c| c == '/' || c.is_whitespace() || ",.;:)]}\"'`!?".contains(c));
            if ends && let Some((_, abs)) = roots.iter().find(|(a, _)| a == alias) {
                out.push('@');
                out.push_str(abs.trim_end_matches('/'));
                i += 1 + word_end;
                prev = Some('x');
                continue;
            }
        }
        out.push(ch);
        prev = Some(ch);
        i += ch.len_utf8();
    }
    out
}

/// What the agent is told about the project, out of sight of the chat.
pub fn roots_prompt(project: &str, roots: &[(String, String)]) -> String {
    let mut s = format!(
        "# Project folders\n\nThis chat belongs to the project \"{project}\", which spans several folders. \
         You start in the first one; the others are just as much part of the work. \
         Refer to them by absolute path. When the user writes `@alias/path`, they mean the path inside that folder.\n\n"
    );
    for (i, (alias, path)) in roots.iter().enumerate() {
        let tag = if i == 0 { " (working folder)" } else { "" };
        s.push_str(&format!("- {alias}: {path}{tag}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Vec<(String, String)> {
        vec![("web".into(), "/r/web".into()), ("api".into(), "/r/api".into())]
    }

    #[test]
    fn mentions_spell_out_roots() {
        let r = roots();
        assert_eq!(resolve_mentions("see @api/src/x.ts and @web", &r), "see @/r/api/src/x.ts and @/r/web");
        assert_eq!(resolve_mentions("mail me@api/x or @other/y", &r), "mail me@api/x or @other/y");
        assert_eq!(resolve_mentions("(@api/a), @apix", &r), "(@/r/api/a), @apix");
    }

    #[test]
    fn roots_split() {
        let r = roots();
        assert_eq!(split_root(&r, "@api/src"), Some(("/r/api", "src")));
        assert_eq!(split_root(&r, "@web"), Some(("/r/web", "")));
        assert_eq!(split_root(&r, "src/@api"), None);
    }

    #[test]
    fn scope_keeps_primary_first() {
        let p = Project {
            id: "p".into(),
            name: "P".into(),
            folders: vec![
                ProjectFolder { path: "/w".into(), alias: "web".into(), ..Default::default() },
                ProjectFolder { path: "/a".into(), alias: "api".into(), ..Default::default() },
                ProjectFolder { path: "/l".into(), alias: "landing".into(), ..Default::default() },
            ],
            created_at: 0,
        };
        let names = |s: &[String]| in_scope(&p, s).iter().map(|f| f.alias.clone()).collect::<Vec<_>>();
        assert_eq!(names(&[]), ["web", "api", "landing"]);
        assert_eq!(names(&["landing".into()]), ["web", "landing"]);
        assert_eq!(default_alias("/x/resona-finance-cms", &[]), "cms");
        assert_eq!(default_alias("/x/cms", &["cms".into()]), "cms2");
    }
}

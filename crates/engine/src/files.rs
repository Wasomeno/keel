//! The files pane: what a chat's folder holds and what changed in it, for the
//! UI's Changes / Files lists, diff view and editor. Everything is relative to
//! the chat's own working folder (its worktree, if it has one).
//!
//! Cheap by construction: a folder is listed one level at a time (ignored
//! entries left out), a diff is one file's, and nothing here watches the disk;
//! the UI asks again when it needs to. Reads and writes go through the
//! hardened [`crate::workspace`] walk.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::repo::run_git_cmd_sync;
use crate::workspace;

/// Largest file or diff sent: well under the 1 MiB RPC frame once JSON
/// escapes it.
pub const MAX_VIEW_BYTES: usize = 512 * 1024;
/// Most changed files and folder entries listed.
const MAX_ENTRIES: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    /// `M`odified, `A`dded, `D`eleted, `R`enamed, `U`nmerged, or `?` untracked.
    pub status: char,
    /// Line counts; `None` for binary files and untracked ones.
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
}

/// Why a file op failed, for the reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    /// Outside the folder, `.git`, a symlink out, binary, or missing.
    Rejected(String),
    TooLarge,
    /// The file changed on disk since it was read.
    Conflict,
}

/// Changed files under `cwd`, as `git status` sees them, with line counts.
pub fn changes(cwd: &Path) -> Result<Vec<Change>, String> {
    // v2: every record starts with its kind, so nothing leading gets trimmed.
    let status = run_git_cmd_sync(
        cwd,
        &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
    )
    .map_err(|_| "not a git repository".to_string())?;
    let mut out = Vec::new();
    let mut records = status.split('\0').filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        let (kind, rest) = record.split_at(1);
        let (status, path) = match kind {
            "?" => ('?', rest.trim_start().to_string()),
            "1" | "2" | "u" => {
                // Fields before the path: XY sub mH mI mW hH hI (+ score for renames,
                // + extra modes and hashes for unmerged).
                let skip = match kind {
                    "1" => 7,
                    "2" => 8,
                    _ => 9,
                };
                let fields: Vec<&str> = rest.trim_start().splitn(skip + 1, ' ').collect();
                let xy = fields.first().copied().unwrap_or("..");
                let path = fields.get(skip).copied().unwrap_or_default().to_string();
                if kind == "2" {
                    records.next(); // the original path
                }
                let status = if kind == "u" {
                    'U'
                } else if kind == "2" {
                    'R'
                } else if xy.contains('D') {
                    'D'
                } else if xy.contains('A') {
                    'A'
                } else {
                    'M'
                };
                (status, path)
            }
            _ => continue,
        };
        if !path.is_empty() {
            out.push(Change {
                path,
                status,
                added: None,
                removed: None,
            });
        }
        if out.len() >= MAX_ENTRIES {
            break;
        }
    }
    // Counts against HEAD (staged and unstaged together); none yet in a repo
    // without commits.
    if let Ok(numstat) = run_git_cmd_sync(cwd, &["diff", "HEAD", "--numstat", "-z"]) {
        let mut counts = std::collections::HashMap::new();
        let mut records = numstat.split('\0');
        while let Some(record) = records.next() {
            let mut fields = record.splitn(3, '\t');
            let (Some(added), Some(removed), Some(path)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            // A rename names its paths in the next two records.
            let path = if path.is_empty() {
                records.next();
                records.next().unwrap_or_default()
            } else {
                path
            };
            counts.insert(path.to_string(), (added.parse().ok(), removed.parse().ok()));
        }
        for change in &mut out {
            if let Some(&(added, removed)) = counts.get(&change.path) {
                change.added = added;
                change.removed = removed;
            }
        }
    }
    Ok(out)
}

/// The session card's project line: where HEAD is and how much changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GitSummary {
    pub branch: Option<String>,
    /// Short sha when HEAD is detached.
    pub detached: Option<String>,
    /// Changed files as `git status` lists them, untracked ones included.
    pub files: usize,
    pub untracked: usize,
    /// Line counts from `git diff HEAD --numstat` (untracked and binary
    /// files add none).
    pub added: u64,
    pub removed: u64,
}

/// Totals over [`changes`].
pub fn summarize(changes: &[Change]) -> GitSummary {
    GitSummary {
        files: changes.len(),
        untracked: changes.iter().filter(|c| c.status == '?').count(),
        added: changes.iter().filter_map(|c| c.added).map(u64::from).sum(),
        removed: changes
            .iter()
            .filter_map(|c| c.removed)
            .map(u64::from)
            .sum(),
        ..GitSummary::default()
    }
}

/// [`summarize`] plus HEAD, for `cwd`. Errs when it isn't a git checkout.
pub fn git_summary(cwd: &Path) -> Result<GitSummary, String> {
    let mut summary = summarize(&changes(cwd)?);
    match crate::repo::current_head_sync(cwd) {
        Ok(crate::repo::Head::Branch(name)) => summary.branch = Some(name),
        Ok(crate::repo::Head::Detached(sha)) => summary.detached = Some(sha),
        Err(_) => {}
    }
    Ok(summary)
}

/// `rel` as a pathspec: relative, no `..`, no `.git`.
fn check_rel(rel: &str) -> Result<(), FileError> {
    let bad = rel.is_empty()
        || rel.starts_with('/')
        || rel.contains('\0')
        || rel
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == ".." || c.eq_ignore_ascii_case(".git"));
    if bad {
        Err(FileError::Rejected("path rejected".into()))
    } else {
        Ok(())
    }
}

/// One file's unified diff against HEAD. An untracked file reads as all
/// added. `true` when it was cut at [`MAX_VIEW_BYTES`].
pub fn diff(cwd: &Path, rel: &str) -> Result<(String, bool), FileError> {
    check_rel(rel)?;
    let git = |args: &[&str]| run_git_cmd_sync(cwd, args).map_err(FileError::Rejected);
    let mut patch = git(&[
        "--literal-pathspecs",
        "diff",
        "HEAD",
        "--no-color",
        "--no-ext-diff",
        "--",
        rel,
    ])
    .or_else(|_| {
        git(&[
            "--literal-pathspecs",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--",
            rel,
        ])
    })?;
    if patch.is_empty() {
        let status = git(&[
            "--literal-pathspecs",
            "status",
            "--porcelain=v2",
            "--untracked-files=all",
            "--",
            rel,
        ])?;
        if status.starts_with('?') {
            let (content, _) = read(cwd, rel)?;
            patch = added_patch(rel, &content);
        }
    }
    Ok(cut(patch))
}

/// A whole file as a new-file patch.
fn added_patch(rel: &str, content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut patch = format!(
        "--- /dev/null\n+++ b/{rel}\n@@ -0,0 +1,{} @@\n",
        lines.len()
    );
    for line in lines {
        patch.push('+');
        patch.push_str(line);
        patch.push('\n');
    }
    patch
}

fn cut(mut text: String) -> (String, bool) {
    if text.len() <= MAX_VIEW_BYTES {
        return (text, false);
    }
    let mut end = MAX_VIEW_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    // Whole lines only.
    end = text[..end].rfind('\n').map_or(end, |i| i + 1);
    text.truncate(end);
    (text, true)
}

/// One folder of `cwd` (`rel` empty for the top): folders first, then
/// files, each by name. `.git` and git-ignored entries are left out.
pub fn list_dir(cwd: &Path, rel: &str) -> Result<Vec<Entry>, FileError> {
    let root = cwd
        .canonicalize()
        .map_err(|_| FileError::Rejected("folder missing".into()))?;
    let dir = if rel.is_empty() {
        root.clone()
    } else {
        check_rel(rel)?;
        // A symlinked folder may only lead somewhere inside the root.
        root.join(rel)
            .canonicalize()
            .ok()
            .filter(|d| d.starts_with(&root) && d.is_dir())
            .ok_or_else(|| FileError::Rejected("not a folder".into()))?
    };
    let ignored = ignored_in(cwd, rel);
    let mut entries: Vec<Entry> = std::fs::read_dir(&dir)
        .map_err(|e| FileError::Rejected(e.to_string()))?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.eq_ignore_ascii_case(".git") || ignored.contains(&name) {
                return None;
            }
            // Follows a symlink to say whether it is a folder.
            let dir = std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir());
            Some(Entry { name, dir })
        })
        .take(MAX_ENTRIES)
        .collect();
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Names directly in `rel` that git ignores. Ignored folders come back
/// whole (`--directory`), so this never walks into `node_modules`.
fn ignored_in(cwd: &Path, rel: &str) -> std::collections::HashSet<String> {
    let spec = if rel.is_empty() {
        ".".to_string()
    } else {
        format!("{rel}/")
    };
    let prefix = if rel.is_empty() {
        String::new()
    } else {
        format!("{rel}/")
    };
    let listed = run_git_cmd_sync(
        cwd,
        &[
            "--literal-pathspecs",
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "--",
            &spec,
        ],
    )
    .unwrap_or_default();
    listed
        .split('\0')
        .filter_map(|path| path.strip_prefix(prefix.as_str()))
        .filter_map(|rest| {
            let rest = rest.trim_end_matches('/');
            (!rest.is_empty() && !rest.contains('/')).then(|| rest.to_string())
        })
        .collect()
}

fn hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

fn reject(code: keel_proto::RpcErrorCode) -> FileError {
    match code {
        keel_proto::RpcErrorCode::PayloadTooLarge => FileError::TooLarge,
        _ => FileError::Rejected(
            "can't open this file (binary, missing, or outside the folder)".into(),
        ),
    }
}

/// Where a file is on disk and its size in bytes, for viewers that read it
/// themselves (images). Only a regular file inside `cwd`: a symlink may not
/// lead outside the folder.
pub fn locate(cwd: &Path, rel: &str) -> Result<(std::path::PathBuf, u64), FileError> {
    check_rel(rel)?;
    let root = cwd
        .canonicalize()
        .map_err(|_| FileError::Rejected("folder missing".into()))?;
    let path = root
        .join(rel)
        .canonicalize()
        .ok()
        .filter(|p| p.starts_with(&root) && p.is_file())
        .ok_or_else(|| FileError::Rejected("file missing".into()))?;
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok((path, bytes))
}

/// A file's text and its hash (the base for [`write`]).
pub fn read(cwd: &Path, rel: &str) -> Result<(String, String), FileError> {
    check_rel(rel)?;
    let content = workspace::read_workspace_file(cwd, rel).map_err(reject)?;
    if content.len() > MAX_VIEW_BYTES {
        return Err(FileError::TooLarge);
    }
    let digest = hash(&content);
    Ok((content, digest))
}

/// Write `content` if the file still has hash `base` (what the editor
/// loaded); otherwise [`FileError::Conflict`] and nothing is written.
/// Returns the new hash.
pub fn write(cwd: &Path, rel: &str, content: &str, base: &str) -> Result<String, FileError> {
    check_rel(rel)?;
    if content.len() > MAX_VIEW_BYTES {
        return Err(FileError::TooLarge);
    }
    match workspace::read_workspace_file(cwd, rel) {
        Ok(current) if hash(&current) != base => return Err(FileError::Conflict),
        Ok(_) => {}
        // Deleted since it was read: writing would bring it back unasked.
        Err(_) => return Err(FileError::Conflict),
    }
    workspace::write_workspace_file(cwd, rel, content).map_err(reject)?;
    Ok(hash(content))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("keel-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let git = |args: &[&str]| run_git_cmd_sync(&dir, args).unwrap();
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.join("src/a.rs"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "x\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        dir.canonicalize().unwrap()
    }

    #[test]
    fn changes_list_status_and_counts() {
        let dir = repo("changes");
        std::fs::write(dir.join("src/a.rs"), "one\n2\nthree\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        std::fs::write(dir.join("new file.md"), "hi\n").unwrap();
        let mut got = changes(&dir).unwrap();
        got.sort_by(|a, b| a.path.cmp(&b.path));
        let view: Vec<_> = got
            .iter()
            .map(|c| (c.path.as_str(), c.status, c.added, c.removed))
            .collect();
        assert_eq!(
            view,
            vec![
                ("gone.txt", 'D', Some(0), Some(1)),
                ("new file.md", '?', None, None),
                ("src/a.rs", 'M', Some(2), Some(1))
            ]
        );
        let (patch, cut) = diff(&dir, "src/a.rs").unwrap();
        assert!(
            !cut && patch.contains("-two") && patch.contains("+three"),
            "{patch}"
        );
        let (patch, _) = diff(&dir, "new file.md").unwrap();
        assert!(patch.ends_with("@@ -0,0 +1,1 @@\n+hi\n"), "{patch}");
        assert!(diff(&dir, "../etc/passwd").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn locate_stays_inside_the_folder() {
        let dir = repo("locate");
        std::fs::write(dir.join("logo.png"), [0u8, 1, 2, 3]).unwrap();
        let (path, bytes) = locate(&dir, "logo.png").unwrap();
        assert!(path.is_absolute() && path.ends_with("logo.png") && bytes == 4);
        assert!(locate(&dir, "../etc/passwd").is_err());
        assert!(locate(&dir, "src").is_err(), "a folder is not a file");
        assert!(locate(&dir, "nope.png").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn summary_counts_files_lines_and_untracked() {
        let change = |path: &str, status, added, removed| Change {
            path: path.into(),
            status,
            added,
            removed,
        };
        let got = summarize(&[
            change("a.rs", 'M', Some(3), Some(1)),
            change("new.md", '?', None, None),
            change("logo.png", 'M', None, None),
            change("gone.txt", 'D', Some(0), Some(4)),
        ]);
        assert_eq!(
            (got.files, got.untracked, got.added, got.removed),
            (4, 1, 3, 5)
        );
        assert_eq!(summarize(&[]), GitSummary::default());

        let dir = repo("summary");
        assert_eq!(git_summary(&dir).unwrap().files, 0);
        std::fs::write(dir.join("src/a.rs"), "one\n2\nthree\n").unwrap();
        std::fs::write(dir.join("new file.md"), "hi\n").unwrap();
        let got = git_summary(&dir).unwrap();
        assert_eq!(
            (got.files, got.untracked, got.added, got.removed),
            (2, 1, 2, 1),
            "{got:?}"
        );
        assert!(got.branch.is_some() && got.detached.is_none(), "{got:?}");
        let plain = std::env::temp_dir().join(format!("keel-files-plain-{}", std::process::id()));
        std::fs::create_dir_all(&plain).unwrap();
        assert!(git_summary(&plain).is_err());
        let _ = std::fs::remove_dir_all(&plain);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn folders_list_one_level_without_ignored() {
        let dir = repo("list");
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join("target/debug/big"), "x").unwrap();
        let names = |rel: &str| {
            list_dir(&dir, rel)
                .unwrap()
                .into_iter()
                .map(|e| (e.name, e.dir))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(""),
            vec![
                ("src".into(), true),
                (".gitignore".into(), false),
                ("gone.txt".into(), false)
            ]
        );
        assert_eq!(names("src"), vec![("a.rs".into(), false)]);
        assert!(list_dir(&dir, "..").is_err());
        assert!(list_dir(&dir, ".git").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn writes_refuse_a_file_changed_since_it_was_read() {
        let dir = repo("write");
        let (content, base) = read(&dir, "src/a.rs").unwrap();
        assert_eq!(content, "one\ntwo\n");
        let next = write(&dir, "src/a.rs", "mine\n", &base).unwrap();
        // The agent writes in between: the stale base no longer matches.
        std::fs::write(dir.join("src/a.rs"), "agent\n").unwrap();
        assert_eq!(
            write(&dir, "src/a.rs", "mine again\n", &next),
            Err(FileError::Conflict)
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("src/a.rs")).unwrap(),
            "agent\n"
        );
        assert!(read(&dir, ".git/config").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn long_diffs_are_cut_at_a_line() {
        let text = "x".repeat(100) + "\n";
        let (out, cut) = super::cut(text.repeat(MAX_VIEW_BYTES / 101 + 10));
        assert!(cut && out.len() <= MAX_VIEW_BYTES && out.ends_with('\n'));
    }
}

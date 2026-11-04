//! Keel memory: curated markdown notes the agent keeps for later sessions.
//!
//! Two scopes, both under Keel's data dir and never in the user's repo:
//!
//! ```text
//! {home}/agent/memory/          user, every project
//! {home}/projects/<id>/memory/  this checkout, this machine
//! {home}/projects/<id>/root     one line, the identity path
//! ```
//!
//! `{home}` is `KeelProfile::home(data_dir)`. A scope holds `MEMORY.md` (the
//! index, injected every turn under a cap), one `<slug>.md` per topic,
//! `archive/<slug>.md` for retired topics, and `memory.lock` during a write.
//! This module owns the format, the paths, the lock, the jail and the cap;
//! the engine's RPC methods are thin wrappers over it.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Lines of `MEMORY.md` injected per scope.
pub const CAP_LINES: usize = 200;
/// Bytes of `MEMORY.md` injected per scope.
pub const CAP_BYTES: usize = 25_000;
/// Longest description, in characters.
pub const DESCRIPTION_MAX: usize = 200;
/// Largest topic body.
pub const BODY_MAX: usize = 32 * 1024;

pub const TYPES: [&str; 4] = ["user", "feedback", "project", "reference"];

const INDEX: &str = "MEMORY.md";
const ARCHIVE: &str = "archive";
const LOCK: &str = "memory.lock";
const SEP: &str = " \u{2014} ";
const INDEX_HEADER: &str = "# Memory\n\n";
const NONE_YET: &str = "None yet.";
const NO_FOLDER: &str = "This chat has no folder.";
/// Shown in place of a scope's index when it cannot be read this turn.
pub const UNAVAILABLE: &str = "Memory is unavailable this turn.";
const TRUNCATED: &str = "(The index is over the cap. Later lines are not shown.)";

/// The fixed text above both scopes. The extension carries the same string
/// for the case where the engine cannot be reached.
pub const PREAMBLE: &str = keel_harness::MEMORY_PREAMBLE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryError {
    /// The caller sent something invalid: a bad slug, scope, type, size.
    BadParams(String),
    /// The files could not be read or written, or the folder is not ours.
    Failed(String),
}

impl MemoryError {
    pub fn message(&self) -> &str {
        match self {
            Self::BadParams(m) | Self::Failed(m) => m,
        }
    }
}

fn bad(msg: impl Into<String>) -> MemoryError {
    MemoryError::BadParams(msg.into())
}

fn failed(msg: impl Into<String>) -> MemoryError {
    MemoryError::Failed(msg.into())
}

fn io(context: &str, err: std::io::Error) -> MemoryError {
    failed(format!("{context}: {err}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    User,
    Project,
}

impl ScopeKind {
    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "user" => Ok(Self::User),
            "project" => Ok(Self::Project),
            _ => Err(bad("scope must be user or project")),
        }
    }
}

/// `^[a-z0-9][a-z0-9-]{0,63}$`, minus the names the layout uses.
pub fn validate_slug(slug: &str) -> Result<(), MemoryError> {
    let bytes = slug.as_bytes();
    let ok_first = bytes.first().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let ok_rest = bytes[1.min(bytes.len())..].iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
    if !ok_first || !ok_rest || bytes.len() > 64 {
        return Err(bad(format!("slug must match [a-z0-9][a-z0-9-]{{0,63}}: {slug:?}")));
    }
    if matches!(slug, "memory" | "archive" | "MEMORY") {
        return Err(bad(format!("slug {slug:?} is reserved")));
    }
    Ok(())
}

fn validate_type(kind: &str) -> Result<(), MemoryError> {
    if TYPES.contains(&kind) { Ok(()) } else { Err(bad("type must be user, feedback, project or reference")) }
}

fn validate_description(description: &str) -> Result<(), MemoryError> {
    if description.trim().is_empty() {
        return Err(bad("description is empty"));
    }
    if description.contains('\n') || description.contains('\r') {
        return Err(bad("description must be one line"));
    }
    if description.chars().count() > DESCRIPTION_MAX {
        return Err(bad(format!("description is over {DESCRIPTION_MAX} characters")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Index format

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub slug: String,
    pub description: String,
}

impl Entry {
    fn line(&self) -> String {
        format!("- [{slug}]({slug}.md){SEP}{desc}", slug = self.slug, desc = self.description)
    }
}

/// `MEMORY.md` split into the lines that are not index bullets (kept at the
/// top, in order) and the bullets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    pub header: Vec<String>,
    pub entries: Vec<Entry>,
}

/// One bullet: `- [<slug>](<slug>.md) — <description>`. The split is on the
/// first ` — ` only, so a description may contain an em dash.
pub fn parse_line(line: &str) -> Option<Entry> {
    let (link, description) = line.split_once(SEP)?;
    let rest = link.strip_prefix("- [")?;
    let (slug, target) = rest.split_once("](")?;
    let target = target.strip_suffix(')')?;
    if target != format!("{slug}.md") || validate_slug(slug).is_err() {
        return None;
    }
    Some(Entry { slug: slug.to_string(), description: description.trim_end().to_string() })
}

pub fn parse_index(text: &str) -> Index {
    let mut index = Index::default();
    for line in text.lines() {
        match parse_line(line) {
            Some(entry) => index.entries.push(entry),
            None => index.header.push(line.to_string()),
        }
    }
    while index.header.last().is_some_and(|l| l.trim().is_empty()) {
        index.header.pop();
    }
    index
}

impl Index {
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.header.is_empty() {
            out.push_str(INDEX_HEADER);
        } else {
            for line in &self.header {
                out.push_str(line);
                out.push('\n');
            }
            out.push('\n');
        }
        for entry in &self.entries {
            out.push_str(&entry.line());
            out.push('\n');
        }
        out
    }

    fn upsert(&mut self, entry: Entry) {
        match self.entries.iter_mut().find(|e| e.slug == entry.slug) {
            Some(slot) => *slot = entry,
            None => self.entries.push(entry),
        }
    }

    fn remove(&mut self, slug: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.slug != slug);
        before != self.entries.len()
    }
}

/// The first [`CAP_LINES`] lines or [`CAP_BYTES`] bytes, whichever comes
/// first, cut on a line boundary. The flag is true when anything was cut.
pub fn cap(text: &str) -> (&str, bool) {
    let mut end = 0;
    for (lines, line) in text.split_inclusive('\n').enumerate() {
        if lines >= CAP_LINES || end + line.len() > CAP_BYTES {
            return (&text[..end], true);
        }
        end += line.len();
    }
    (text, false)
}

fn over_cap(text: &str) -> bool {
    cap(text).1
}

// ---------------------------------------------------------------------------
// Topic format

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topic {
    pub kind: String,
    pub description: String,
    pub body: String,
}

pub fn render_topic(topic: &Topic) -> String {
    let mut out = format!("---\ntype: {}\ndescription: {}\n---\n", topic.kind, topic.description);
    let body = topic.body.trim_end();
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
        out.push('\n');
    }
    out
}

/// Frontmatter `type:` and `description:`, then the body. A file with no
/// frontmatter is all body.
pub fn parse_topic(text: &str) -> Topic {
    let mut topic = Topic { kind: String::new(), description: String::new(), body: String::new() };
    let rest = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n"));
    let Some(rest) = rest else {
        topic.body = text.trim().to_string();
        return topic;
    };
    let (front, body) = match rest.find("\n---") {
        Some(at) => {
            let after = &rest[at + 4..];
            (&rest[..at], after.split_once('\n').map(|(_, b)| b).unwrap_or(""))
        }
        None => (rest, ""),
    };
    for line in front.lines() {
        if let Some((key, value)) = line.split_once(':') {
            match key.trim() {
                "type" => topic.kind = value.trim().to_string(),
                "description" => topic.description = value.trim().to_string(),
                _ => {}
            }
        }
    }
    topic.body = body.trim_matches('\n').trim_end().to_string();
    topic
}

// ---------------------------------------------------------------------------
// Paths and identity

/// Where a scope lives. `dir` may not exist yet; it is made on first write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeDir {
    pub kind: ScopeKind,
    pub dir: PathBuf,
    pub id: String,
    /// The identity path for a project; empty for the user scope.
    pub root: String,
}

pub fn user_scope(home: &Path) -> ScopeDir {
    ScopeDir { kind: ScopeKind::User, dir: home.join("agent").join("memory"), id: "user".into(), root: String::new() }
}

/// The checkout a folder belongs to: the canonical git common dir, shared by
/// every worktree of a repo, else the canonical folder.
pub fn identity_path(cwd: &Path) -> Result<PathBuf, MemoryError> {
    let git = crate::repo::run_git_cmd_sync(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    if let Ok(out) = git {
        let path = PathBuf::from(out.trim());
        if path.is_absolute() {
            if let Ok(canon) = path.canonicalize() {
                return Ok(canon);
            }
        }
    }
    cwd.canonicalize().map_err(|e| io("cannot resolve the chat folder", e))
}

/// First 32 hex characters of SHA-256 over the identity path.
pub fn project_id(identity: &Path) -> String {
    let digest = Sha256::digest(identity.to_string_lossy().as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex.truncate(32);
    hex
}

/// The project scope for a chat folder. Refuses a folder whose `root` file
/// names a different checkout (a collision or a hand edit).
pub fn project_scope(home: &Path, cwd: &Path) -> Result<ScopeDir, MemoryError> {
    let identity = identity_path(cwd)?;
    let id = project_id(&identity);
    let base = home.join("projects").join(&id);
    let root = identity.to_string_lossy().into_owned();
    match fs::read_to_string(base.join("root")) {
        Ok(found) if found.trim_end_matches('\n') != root => {
            return Err(failed("This project's memory folder belongs to another checkout."));
        }
        _ => {}
    }
    Ok(ScopeDir { kind: ScopeKind::Project, dir: base.join("memory"), id, root })
}

/// The scope a request names. `cwd` is required for the project scope.
pub fn resolve(home: &Path, kind: ScopeKind, cwd: Option<&Path>) -> Result<ScopeDir, MemoryError> {
    match kind {
        ScopeKind::User => Ok(user_scope(home)),
        ScopeKind::Project => match cwd {
            Some(cwd) => project_scope(home, cwd),
            None => Err(bad("This chat has no folder, so it has no project memory.")),
        },
    }
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Makes the scope folder (and a project's `root`) and returns its canonical
/// path. Refuses a symlinked folder.
fn ensure_scope(scope: &ScopeDir) -> Result<PathBuf, MemoryError> {
    if scope.kind == ScopeKind::Project {
        let base = scope.dir.parent().ok_or_else(|| failed("bad project folder"))?;
        fs::create_dir_all(base).map_err(|e| io("cannot create the memory folder", e))?;
        let root = base.join("root");
        if !root.exists() {
            fs::write(&root, format!("{}\n", scope.root)).map_err(|e| io("cannot write the project root", e))?;
        }
    }
    fs::create_dir_all(&scope.dir).map_err(|e| io("cannot create the memory folder", e))?;
    canonical_scope(scope)
}

fn canonical_scope(scope: &ScopeDir) -> Result<PathBuf, MemoryError> {
    if is_symlink(&scope.dir) {
        return Err(failed("The memory folder is a symlink. Keel will not use it."));
    }
    scope.dir.canonicalize().map_err(|e| io("cannot resolve the memory folder", e))
}

/// A file directly in `dir` (already canonical). Refuses a symlink or a path
/// that resolves outside `dir`.
fn jailed(dir: &Path, name: &str) -> Result<PathBuf, MemoryError> {
    let path = dir.join(name);
    if is_symlink(&path) {
        return Err(failed(format!("{name} is a symlink. Keel will not follow it.")));
    }
    if path.exists() {
        let canon = path.canonicalize().map_err(|e| io("cannot resolve a memory file", e))?;
        if canon.parent() != Some(dir) {
            return Err(failed(format!("{name} is outside the memory folder.")));
        }
    }
    Ok(path)
}

fn read_optional(path: &Path) -> Result<String, MemoryError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(io("cannot read memory", e)),
    }
}

/// The scope's `MEMORY.md` text; empty when the scope has no folder yet.
fn read_index_text(scope: &ScopeDir) -> Result<String, MemoryError> {
    if !scope.dir.exists() && !is_symlink(&scope.dir) {
        return Ok(String::new());
    }
    let dir = canonical_scope(scope)?;
    read_optional(&jailed(&dir, INDEX)?)
}

// ---------------------------------------------------------------------------
// Lock

/// `memory.lock`, held for one write. Removed on drop, so a panic releases it.
struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// A lock older than this is from a writer that died mid-write.
const STALE_LOCK: Duration = Duration::from_secs(30);

fn lock(dir: &Path) -> Result<LockGuard, MemoryError> {
    let path = dir.join(LOCK);
    for _ in 0..20 {
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let _ = writeln!(file, "{}", std::process::id());
                return Ok(LockGuard(path));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > STALE_LOCK);
                if stale {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(io("cannot lock memory", e)),
        }
    }
    Err(failed("Memory is busy. Another chat is saving a note; try again."))
}

fn write_tmp(path: &Path, text: &str) -> Result<PathBuf, MemoryError> {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    if is_symlink(&tmp) {
        let _ = fs::remove_file(&tmp);
    }
    fs::write(&tmp, text).map_err(|e| io("cannot write memory", e))?;
    Ok(tmp)
}

fn rename(from: &Path, to: &Path) -> Result<(), MemoryError> {
    fs::rename(from, to).map_err(|e| io("cannot save memory", e))
}

// ---------------------------------------------------------------------------
// Operations

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteAction {
    Upsert { kind: String, description: String, body: String },
    Archive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    pub over_cap: bool,
    /// The index line written, or the slug archived, as tool-result text.
    pub text: String,
}

/// Upsert or archive one topic under the scope's lock.
pub fn write(scope: &ScopeDir, slug: &str, action: WriteAction) -> Result<WriteOutcome, MemoryError> {
    validate_slug(slug)?;
    if let WriteAction::Upsert { kind, description, body } = &action {
        validate_type(kind)?;
        validate_description(description)?;
        if body.len() > BODY_MAX {
            return Err(bad(format!("body is over {} KiB", BODY_MAX / 1024)));
        }
    }
    let dir = ensure_scope(scope)?;
    let _guard = lock(&dir)?;
    let index_path = jailed(&dir, INDEX)?;
    let topic_name = format!("{slug}.md");
    let topic_path = jailed(&dir, &topic_name)?;
    let mut index = parse_index(&read_optional(&index_path)?);
    let line = match action {
        WriteAction::Upsert { kind, description, body } => {
            let description = description.trim().to_string();
            let topic = Topic { kind, description: description.clone(), body };
            let entry = Entry { slug: slug.to_string(), description };
            let line = entry.line();
            index.upsert(entry);
            let topic_tmp = write_tmp(&topic_path, &render_topic(&topic))?;
            let index_tmp = write_tmp(&index_path, &index.render())?;
            rename(&topic_tmp, &topic_path)?;
            rename(&index_tmp, &index_path)?;
            format!("Saved: {line}")
        }
        WriteAction::Archive => {
            if !topic_path.is_file() {
                return Err(bad(format!("No note named {slug}.")));
            }
            let archive = dir.join(ARCHIVE);
            if is_symlink(&archive) {
                return Err(failed("archive is a symlink. Keel will not follow it."));
            }
            fs::create_dir_all(&archive).map_err(|e| io("cannot create the archive", e))?;
            let archived = jailed(&archive, &topic_name)?;
            index.remove(slug);
            let index_tmp = write_tmp(&index_path, &index.render())?;
            rename(&topic_path, &archived)?;
            rename(&index_tmp, &index_path)?;
            format!("Archived {slug}. Its line is out of the index and the file is under archive/.")
        }
    };
    let rendered = read_optional(&index_path)?;
    let over = over_cap(&rendered);
    let text = if over {
        format!("{line}\nThe index is over the cap ({CAP_LINES} lines or {CAP_BYTES} bytes). Merge or archive lines until it fits; the next session drops the tail.")
    } else {
        line
    };
    Ok(WriteOutcome { over_cap: over, text })
}

/// Removes the index line and the topic file. `archive/` is left alone.
pub fn delete(scope: &ScopeDir, slug: &str) -> Result<(), MemoryError> {
    validate_slug(slug)?;
    if !scope.dir.exists() {
        return Err(bad(format!("No note named {slug}.")));
    }
    let dir = canonical_scope(scope)?;
    let _guard = lock(&dir)?;
    let index_path = jailed(&dir, INDEX)?;
    let topic_path = jailed(&dir, &format!("{slug}.md"))?;
    let mut index = parse_index(&read_optional(&index_path)?);
    let had_line = index.remove(slug);
    let had_file = topic_path.is_file();
    if !had_line && !had_file {
        return Err(bad(format!("No note named {slug}.")));
    }
    if had_line {
        let tmp = write_tmp(&index_path, &index.render())?;
        rename(&tmp, &index_path)?;
    }
    if had_file {
        fs::remove_file(&topic_path).map_err(|e| io("cannot delete the note", e))?;
    }
    Ok(())
}

fn read_topic(dir: &Path, slug: &str) -> Result<Option<Topic>, MemoryError> {
    let path = jailed(dir, &format!("{slug}.md"))?;
    match fs::read_to_string(&path) {
        Ok(text) => Ok(Some(parse_topic(&text))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io("cannot read the note", e)),
    }
}

/// Topic slugs in the folder (not `archive/`), sorted.
fn topic_slugs(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut slugs: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".md")).map(str::to_string))
        .filter(|slug| validate_slug(slug).is_ok())
        .collect();
    slugs.sort();
    slugs
}

/// The full index (past the cap too) and the topic slugs, or one topic.
pub fn read(scope: &ScopeDir, slug: Option<&str>) -> Result<Value, MemoryError> {
    match slug {
        None => {
            let text = read_index_text(scope)?;
            let slugs = if scope.dir.is_dir() { topic_slugs(&canonical_scope(scope)?) } else { Vec::new() };
            let shown = if parse_index(&text).entries.is_empty() { NONE_YET.to_string() } else { text.trim_end().to_string() };
            let listing = if slugs.is_empty() { "none".to_string() } else { slugs.join(", ") };
            Ok(json!({
                "index": text,
                "slugs": slugs,
                "text": format!("{shown}\n\nTopics: {listing}"),
            }))
        }
        Some(slug) => {
            validate_slug(slug)?;
            let missing = || bad(format!("No note named {slug}."));
            if !scope.dir.is_dir() {
                return Err(missing());
            }
            let dir = canonical_scope(scope)?;
            let topic = read_topic(&dir, slug)?.ok_or_else(missing)?;
            let text = format!(
                "type: {}\ndescription: {}\n\n{}",
                topic.kind,
                topic.description,
                if topic.body.is_empty() { "(no body)" } else { &topic.body }
            );
            Ok(json!({
                "slug": slug,
                "type": topic.kind,
                "description": topic.description,
                "body": topic.body,
                "text": text,
            }))
        }
    }
}

/// `{ id, root, notes, truncated }` for Settings. Topic files with no index
/// line come last with `unindexed: true`.
pub fn list_scope(scope: &ScopeDir) -> Result<Value, MemoryError> {
    let text = read_index_text(scope)?;
    let index = parse_index(&text);
    let dir = if scope.dir.is_dir() { Some(canonical_scope(scope)?) } else { None };
    let mut notes = Vec::new();
    for entry in &index.entries {
        let kind = match &dir {
            Some(dir) => read_topic(dir, &entry.slug).ok().flatten().map(|t| t.kind).unwrap_or_default(),
            None => String::new(),
        };
        notes.push(json!({ "slug": entry.slug, "type": kind, "description": entry.description, "archived": false }));
    }
    if let Some(dir) = &dir {
        for slug in topic_slugs(dir) {
            if index.entries.iter().any(|e| e.slug == slug) {
                continue;
            }
            let topic = read_topic(dir, &slug).ok().flatten();
            let (kind, description) = topic.map(|t| (t.kind, t.description)).unwrap_or_default();
            notes.push(json!({
                "slug": slug, "type": kind, "description": description, "archived": false, "unindexed": true,
            }));
        }
    }
    Ok(json!({ "id": scope.id, "root": scope.root, "notes": notes, "truncated": over_cap(&text) }))
}

/// `{ user, project }`; `project` is null without a folder.
pub fn list(home: &Path, cwd: Option<&Path>) -> Result<Value, MemoryError> {
    let user = list_scope(&user_scope(home))?;
    let project = match cwd {
        Some(cwd) => list_scope(&project_scope(home, cwd)?)?,
        None => Value::Null,
    };
    Ok(json!({ "user": user, "project": project }))
}

/// One scope's block: the capped index, or "None yet.".
fn scope_block(scope: &ScopeDir) -> Result<String, MemoryError> {
    let text = read_index_text(scope)?;
    if parse_index(&text).entries.is_empty() {
        return Ok(NONE_YET.to_string());
    }
    let (shown, truncated) = cap(&text);
    let mut block = shown.trim_end().to_string();
    if truncated {
        block.push('\n');
        block.push_str(TRUNCATED);
    }
    Ok(block)
}

/// The `keel_memory` prompt section: the preamble, then both scopes.
pub fn prompt(home: &Path, cwd: Option<&Path>) -> String {
    let user = scope_block(&user_scope(home)).unwrap_or_else(|_| UNAVAILABLE.to_string());
    let project = match cwd {
        None => NO_FOLDER.to_string(),
        Some(cwd) => project_scope(home, cwd).and_then(|s| scope_block(&s)).unwrap_or_else(|_| UNAVAILABLE.to_string()),
    };
    format!("{PREAMBLE}\n\n## User\n{user}\n\n## Project\n{project}")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);

    impl Tmp {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("keel-memory-{label}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir.canonicalize().unwrap())
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn upsert(kind: &str, description: &str, body: &str) -> WriteAction {
        WriteAction::Upsert { kind: kind.into(), description: description.into(), body: body.into() }
    }

    fn index_of(scope: &ScopeDir) -> String {
        fs::read_to_string(scope.dir.join(INDEX)).unwrap()
    }

    fn git(dir: &Path, args: &[&str]) {
        crate::repo::run_git_cmd_sync(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    }

    fn repo(at: &Path) {
        fs::create_dir_all(at).unwrap();
        git(at, &["init", "-q"]);
        git(at, &["-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "init"]);
    }

    #[test]
    fn bad_slugs_are_refused() {
        let long = format!("a{}", "b".repeat(64));
        for slug in ["../x", "/tmp/x", "Foo", "a/b", "", "archive", "memory", "MEMORY", "a.b", "-a", long.as_str()] {
            assert!(validate_slug(slug).is_err(), "{slug:?} passed");
        }
        let max = format!("a{}", "b".repeat(63));
        for slug in ["pnpm", "reply-tone", "a", "0x", max.as_str()] {
            assert!(validate_slug(slug).is_ok(), "{slug:?} refused");
        }
    }

    #[test]
    fn upsert_writes_one_line_and_replaces_it() {
        let tmp = Tmp::new("upsert");
        let scope = user_scope(&tmp.0);
        let out = write(&scope, "pnpm", upsert("project", "This repo uses pnpm.", "Use pnpm.")).unwrap();
        assert!(!out.over_cap);
        assert_eq!(out.text, "Saved: - [pnpm](pnpm.md) \u{2014} This repo uses pnpm.");
        assert_eq!(index_of(&scope), "# Memory\n\n- [pnpm](pnpm.md) \u{2014} This repo uses pnpm.\n");
        let topic = parse_topic(&fs::read_to_string(scope.dir.join("pnpm.md")).unwrap());
        assert_eq!(topic, Topic { kind: "project".into(), description: "This repo uses pnpm.".into(), body: "Use pnpm.".into() });

        write(&scope, "pnpm", upsert("project", "Use pnpm, not npm.", "")).unwrap();
        let index = parse_index(&index_of(&scope));
        assert_eq!(index.entries, vec![Entry { slug: "pnpm".into(), description: "Use pnpm, not npm.".into() }]);
        assert!(!scope.dir.join("pnpm.md.tmp").exists() && !scope.dir.join("MEMORY.md.tmp").exists());
        assert!(!scope.dir.join(LOCK).exists(), "lock released");
    }

    #[test]
    fn em_dash_in_a_description_round_trips() {
        let tmp = Tmp::new("dash");
        let scope = user_scope(&tmp.0);
        let desc = "Tone \u{2014} casual \u{2014} unless formal";
        write(&scope, "tone", upsert("user", desc, "")).unwrap();
        let entry = parse_line(index_of(&scope).lines().last().unwrap()).unwrap();
        assert_eq!(entry, Entry { slug: "tone".into(), description: desc.into() });
        assert_eq!(read(&scope, Some("tone")).unwrap()["description"], desc);
    }

    #[test]
    fn bad_bullets_are_header_lines() {
        let index = parse_index("# Memory\n\n- [a](b.md) \u{2014} wrong target\n- [ok](ok.md) \u{2014} fine\nfree text\n");
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.header, vec!["# Memory", "", "- [a](b.md) \u{2014} wrong target", "free text"]);
    }

    #[test]
    fn archive_moves_the_topic_and_keeps_older_archives() {
        let tmp = Tmp::new("archive");
        let scope = user_scope(&tmp.0);
        write(&scope, "one", upsert("user", "First.", "body one")).unwrap();
        write(&scope, "two", upsert("user", "Second.", "body two")).unwrap();
        write(&scope, "one", WriteAction::Archive).unwrap();
        assert!(!index_of(&scope).contains("[one]"));
        assert!(fs::read_to_string(scope.dir.join("archive/one.md")).unwrap().contains("body one"));
        assert!(!scope.dir.join("one.md").exists());
        write(&scope, "two", WriteAction::Archive).unwrap();
        assert!(scope.dir.join("archive/one.md").is_file() && scope.dir.join("archive/two.md").is_file());
        assert_eq!(write(&scope, "one", WriteAction::Archive).unwrap_err(), bad("No note named one."));
    }

    #[test]
    fn delete_leaves_the_archive() {
        let tmp = Tmp::new("delete");
        let scope = user_scope(&tmp.0);
        write(&scope, "old", upsert("user", "Old.", "old body")).unwrap();
        write(&scope, "old", WriteAction::Archive).unwrap();
        write(&scope, "old", upsert("user", "New.", "new body")).unwrap();
        delete(&scope, "old").unwrap();
        assert!(!index_of(&scope).contains("[old]"));
        assert!(!scope.dir.join("old.md").exists());
        assert!(fs::read_to_string(scope.dir.join("archive/old.md")).unwrap().contains("old body"));
        assert!(delete(&scope, "old").is_err());
    }

    #[test]
    fn cap_cuts_on_a_line_boundary() {
        let lines: String = (0..250).map(|i| format!("line {i}\n")).collect();
        let (shown, truncated) = cap(&lines);
        assert!(truncated);
        assert_eq!(shown.lines().count(), CAP_LINES);
        assert!(shown.ends_with("line 199\n"));

        let wide: String = (0..100).map(|i| format!("{i:03}{}\n", "x".repeat(296))).collect();
        let (shown, truncated) = cap(&wide);
        assert!(truncated && shown.len() <= CAP_BYTES && shown.ends_with('\n'));
        assert_eq!(shown.len(), 83 * 300);

        assert_eq!(cap("a\nb\n"), ("a\nb\n", false));
    }

    #[test]
    fn a_write_over_the_cap_still_saves() {
        let tmp = Tmp::new("overcap");
        let scope = user_scope(&tmp.0);
        let mut last = None;
        for i in 0..200 {
            last = Some(write(&scope, &format!("n{i}"), upsert("user", &format!("Note {i}."), "")).unwrap());
        }
        assert!(last.unwrap().over_cap, "200 bullets plus the header cross 200 lines");
        assert!(index_of(&scope).contains("[n199]"));
        assert!(scope.dir.join("n199.md").is_file());
        assert_eq!(list_scope(&scope).unwrap()["truncated"], true);
        assert!(prompt(&tmp.0, None).contains(TRUNCATED));
    }

    #[test]
    fn prompt_has_the_preamble_and_skips_the_archive() {
        let tmp = Tmp::new("prompt");
        let empty = prompt(&tmp.0, None);
        assert!(empty.starts_with(PREAMBLE));
        assert!(empty.contains("## User\nNone yet.") && empty.ends_with("## Project\nThis chat has no folder."), "{empty}");

        let scope = user_scope(&tmp.0);
        write(&scope, "keep", upsert("user", "Kept line.", "")).unwrap();
        write(&scope, "gone", upsert("user", "Archived line.", "secret archived body")).unwrap();
        write(&scope, "gone", WriteAction::Archive).unwrap();
        let text = prompt(&tmp.0, None);
        assert!(text.contains("## User\n# Memory\n\n- [keep](keep.md) \u{2014} Kept line."), "{text}");
        assert!(!text.contains("Archived line") && !text.contains("secret archived body"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_out_of_the_scope_are_refused() {
        let tmp = Tmp::new("jail");
        let scope = user_scope(&tmp.0);
        write(&scope, "ok", upsert("user", "Fine.", "")).unwrap();
        let outside = tmp.0.join("outside.md");
        fs::write(&outside, "---\ntype: user\ndescription: x\n---\n").unwrap();
        std::os::unix::fs::symlink(&outside, scope.dir.join("evil.md")).unwrap();
        assert!(read(&scope, Some("evil")).is_err());
        assert!(write(&scope, "evil", upsert("user", "Nope.", "")).is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "---\ntype: user\ndescription: x\n---\n");

        let other = Tmp::new("jail-index");
        let scope = user_scope(&other.0);
        fs::create_dir_all(&scope.dir).unwrap();
        std::os::unix::fs::symlink(&outside, scope.dir.join(INDEX)).unwrap();
        assert!(write(&scope, "a", upsert("user", "A.", "")).is_err());
        assert!(read(&scope, None).is_err());
    }

    #[test]
    fn sequential_writes_keep_both_lines() {
        let tmp = Tmp::new("lock");
        let scope = user_scope(&tmp.0);
        let threads: Vec<_> = ["alpha", "beta"]
            .into_iter()
            .map(|slug| {
                let scope = scope.clone();
                std::thread::spawn(move || write(&scope, slug, upsert("user", &format!("{slug} note."), "")).unwrap())
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let index = parse_index(&index_of(&scope));
        let mut slugs: Vec<_> = index.entries.iter().map(|e| e.slug.as_str()).collect();
        slugs.sort();
        assert_eq!(slugs, ["alpha", "beta"]);
    }

    #[test]
    fn worktrees_share_a_project_and_repos_do_not() {
        let tmp = Tmp::new("identity");
        let main = tmp.0.join("main");
        repo(&main);
        let linked = tmp.0.join("linked");
        git(&main, &["worktree", "add", "-q", "--detach", linked.to_str().unwrap()]);
        let other = tmp.0.join("other");
        repo(&other);
        let plain = tmp.0.join("plain");
        fs::create_dir_all(&plain).unwrap();

        let home = tmp.0.join("home");
        let a = project_scope(&home, &main).unwrap();
        let b = project_scope(&home, &linked).unwrap();
        let c = project_scope(&home, &other).unwrap();
        assert_eq!(a.id, b.id);
        assert_ne!(a.id, c.id);
        assert_eq!(a.id.len(), 32);
        assert_eq!(a.root, main.join(".git").canonicalize().unwrap().to_string_lossy());
        let p = project_scope(&home, &plain).unwrap();
        assert_eq!(p.root, plain.canonicalize().unwrap().to_string_lossy());

        write(&a, "pnpm", upsert("project", "Uses pnpm.", "")).unwrap();
        assert!(prompt(&home, Some(&linked)).contains("Uses pnpm."));
        assert!(!prompt(&home, Some(&other)).contains("Uses pnpm."));
        assert_eq!(fs::read_to_string(home.join("projects").join(&a.id).join("root")).unwrap().trim(), a.root);

        fs::write(home.join("projects").join(&a.id).join("root"), "/somewhere/else\n").unwrap();
        assert!(project_scope(&home, &main).is_err());
        assert!(prompt(&home, Some(&main)).ends_with(UNAVAILABLE));
    }

    #[test]
    fn reads_do_not_create_folders() {
        let tmp = Tmp::new("reads");
        let plain = tmp.0.join("plain");
        fs::create_dir_all(&plain).unwrap();
        let home = tmp.0.join("home");
        let listed = list(&home, Some(&plain)).unwrap();
        assert_eq!(listed["user"]["notes"], json!([]));
        assert_eq!(listed["project"]["notes"], json!([]));
        assert_eq!(list(&home, None).unwrap()["project"], Value::Null);
        assert!(!home.exists());
    }

    #[test]
    fn unindexed_topics_are_listed() {
        let tmp = Tmp::new("unindexed");
        let scope = user_scope(&tmp.0);
        write(&scope, "a", upsert("feedback", "A.", "")).unwrap();
        fs::write(scope.dir.join("stray.md"), render_topic(&Topic { kind: "user".into(), description: "Stray.".into(), body: String::new() })).unwrap();
        let notes = list_scope(&scope).unwrap()["notes"].clone();
        assert_eq!(notes[0], json!({ "slug": "a", "type": "feedback", "description": "A.", "archived": false }));
        assert_eq!(notes[1]["unindexed"], true);
        assert_eq!(notes[1]["slug"], "stray");
        delete(&scope, "stray").unwrap();
        assert!(!scope.dir.join("stray.md").exists());
    }
}

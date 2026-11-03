//! Skills Pi 1.3.0 would load for a chat, and the edits that turn one on or
//! off. One scan serves Settings and the composer's `/` menu. Pi is not
//! shelled out to: the rules are the ones in its package manager
//! (`addAutoDiscoveredResources`, `isEnabledByOverrides`, name collision
//! keeps the first enabled skill).
//!
//! Default places, first match wins: the project's `.pi/skills`, then
//! `.agents/skills` from the chat folder up to the git root, then the agent
//! `skills` folder, then `~/.agents/skills`, then skills declared by
//! installed packages. A `skills` array adds plain paths and filters with
//! `!pattern`, `+path`, and `-path`. A skill with no description, or a
//! `SKILL.md` whose front matter doesn't close, is not a row.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillRow {
    pub name: String,
    pub description: String,
    pub insert: String,
    pub scope: String,
    pub path: String,
    pub enabled: bool,
    pub manual_only: bool,
    pub removable: bool,
    /// The pack this skill was installed from (`emilkowalski/skill`), or a
    /// shared name prefix when several skills are one set (`principle`).
    /// Empty when it stands alone.
    pub collection: String,
    pub author: String,
    pub source_url: String,
}

const MAX_DEPTH: usize = 12;

/// Skills for `cwd` (a chat folder; none if the chat has no folder) with
/// `agent` as Pi's config directory.
pub fn list_skills(cwd: Option<&Path>, agent: &Path) -> Result<Vec<SkillRow>, String> {
    list_skills_at(cwd, agent, &home_dir())
}

pub(crate) fn list_skills_at(
    cwd: Option<&Path>,
    agent: &Path,
    home: &Path,
) -> Result<Vec<SkillRow>, String> {
    let user = read_entries(&agent.join("settings.json"))?;
    let project_file = cwd.map(|cwd| cwd.join(".pi").join("settings.json"));
    let project = match &project_file {
        Some(path) => read_entries(path)?,
        None => Vec::new(),
    };
    let user_base = agent.to_path_buf();
    let project_base = cwd.map(|cwd| cwd.join(".pi"));

    let mut found: Vec<Found> = Vec::new();
    if let Some(base) = &project_base {
        push_plain(&mut found, &project, base, "project", cwd)?;
        push_tree(
            &mut found,
            &base.join("skills"),
            "project",
            true,
            &project,
            base,
        )?;
        for dir in ancestor_agents(cwd.unwrap(), home) {
            let base_dir = dir.parent().unwrap_or(&dir).to_path_buf();
            push_tree(&mut found, &dir, "project", false, &project, &base_dir)?;
        }
    }
    push_plain(&mut found, &user, &user_base, "user", cwd)?;
    push_tree(
        &mut found,
        &agent.join("skills"),
        "user",
        true,
        &user,
        &user_base,
    )?;
    let shared = home.join(".agents").join("skills");
    push_tree(
        &mut found,
        &shared,
        "shared",
        false,
        &user,
        &home.join(".agents"),
    )?;
    // Project packages outrank user packages when two skills share a name.
    if let Some(base) = &project_base {
        push_packages(&mut found, base)?;
    }
    push_packages(&mut found, agent)?;

    let mut rows = select(found);
    for row in &mut rows {
        let path = Path::new(&row.path);
        row.removable = inside(&agent.join("skills"), path)
            || cwd.is_some_and(|cwd| inside(&cwd.join(".pi").join("skills"), path));
    }
    assign_collections(&mut rows, &read_skill_lock(home));
    Ok(rows)
}

#[derive(Debug, Clone, Default)]
pub struct SkillLockMeta {
    pub source: String,
    pub source_url: String,
}

/// `~/.agents/.skill-lock.json` records which repo each installed skill came
/// from. A missing or unreadable file just means nothing is grouped.
fn read_skill_lock(home: &Path) -> std::collections::HashMap<String, SkillLockMeta> {
    let Ok(text) = fs::read_to_string(home.join(".agents").join(".skill-lock.json")) else {
        return std::collections::HashMap::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return std::collections::HashMap::new();
    };
    let Some(skills) = value.get("skills").and_then(|v| v.as_object()) else {
        return std::collections::HashMap::new();
    };
    skills
        .iter()
        .filter_map(|(name, meta)| {
            let source = meta.get("source")?.as_str()?.trim();
            if source.is_empty() {
                return None;
            }
            let source_url = meta
                .get("sourceUrl")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            Some((
                name.clone(),
                SkillLockMeta {
                    source: source.to_string(),
                    source_url,
                },
            ))
        })
        .collect()
}

pub fn human_author(source: &str) -> String {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let lower = trimmed.to_lowercase();
    if lower.starts_with("emilkowalski") {
        return "Emil Kowalski".to_string();
    }
    if lower.starts_with("mattpocock") {
        return "Matt Pocock".to_string();
    }
    if lower.starts_with("pbakaus") {
        return "Peter Bakaus".to_string();
    }
    if lower.starts_with("vercel-labs") || lower == "vercel" {
        return "Vercel Labs".to_string();
    }
    if lower.starts_with("obra") {
        return "Jesse Vincent".to_string();
    }
    if lower.starts_with("leonxlnx") {
        return "Leonxlnx".to_string();
    }
    if lower.starts_with("heygen-com") || lower.starts_with("heygen") {
        return "HeyGen".to_string();
    }
    if lower.starts_with("github") {
        return "GitHub".to_string();
    }
    if lower.starts_with("backnotprop") {
        return "Backnotprop".to_string();
    }
    if lower.starts_with("stablyai") {
        return "Stably AI".to_string();
    }
    if lower.starts_with("mirage-hq") {
        return "Mirage".to_string();
    }
    if lower == "pstack" {
        return "Pstack".to_string();
    }

    let owner = if let Some((o, _)) = trimmed.split_once('/') {
        o
    } else {
        trimmed
    };
    format_handle(owner)
}

fn format_handle(handle: &str) -> String {
    let clean = handle.trim_start_matches('@');
    if clean.contains('-') || clean.contains('_') {
        let parts: Vec<String> = clean
            .split(|c| c == '-' || c == '_')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let mut chars = p.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect(),
                    None => String::new(),
                }
            })
            .collect();
        return parts.join(" ");
    }
    let mut chars = clean.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => clean.to_string(),
    }
}

fn assign_collections(rows: &mut [SkillRow], lock: &std::collections::HashMap<String, SkillLockMeta>) {
    for row in rows.iter_mut() {
        let Some(folder) = Path::new(&row.path).file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(entry) = lock.get(folder) {
            row.collection = entry.source.clone();
            row.source_url = entry.source_url.clone();
            if row.author.is_empty() {
                row.author = human_author(&entry.source);
            }
        }
    }
    for row in rows.iter_mut() {
        if !row.collection.is_empty() {
            continue;
        }
        let norm_path = row.path.replace('\\', "/");
        if norm_path.contains("cursor-plugins/pstack") || norm_path.contains("/pstack/") {
            row.collection = "pstack".to_string();
            if row.author.is_empty() {
                row.author = "Pstack".to_string();
            }
        } else if norm_path.contains(".claude/skills") {
            row.collection = "claude".to_string();
            if row.author.is_empty() {
                row.author = "Claude".to_string();
            }
        } else if norm_path.contains(".local/share/ego") {
            row.collection = "ego".to_string();
            if row.author.is_empty() {
                row.author = "Ego".to_string();
            }
        }
    }
    let mut counts: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for row in rows.iter() {
        if !row.collection.is_empty() {
            continue;
        }
        if let Some(prefix) = name_prefix(&row.name) {
            *counts.entry((row.scope.clone(), prefix)).or_insert(0) += 1;
        }
    }
    for row in rows.iter_mut() {
        if !row.collection.is_empty() {
            continue;
        }
        if let Some(prefix) = name_prefix(&row.name) {
            if counts
                .get(&(row.scope.clone(), prefix.clone()))
                .copied()
                .unwrap_or(0)
                >= 3
            {
                row.collection = prefix.clone();
                if row.author.is_empty() {
                    row.author = format_handle(&prefix);
                }
            }
        }
    }
    for row in rows.iter_mut() {
        if row.author.is_empty() {
            if row.scope == "user" {
                row.author = "Local".to_string();
            } else if row.scope == "package" {
                row.author = "Package".to_string();
            }
        }
    }
}

/// `principle-laziness-protocol` → `principle`, when the token is a real word.
fn name_prefix(name: &str) -> Option<String> {
    let (prefix, _) = name.split_once('-')?;
    (prefix.len() >= 4 && prefix.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .then(|| prefix.to_string())
}

/// Turn the skill at `path` on or off by editing Pi's `skills` array.
/// Yours and Shared write the agent `settings.json`. This project writes
/// `{cwd}/.pi/settings.json`. Every other key is left alone.
pub fn set_skill_enabled(
    agent: &Path,
    cwd: Option<&Path>,
    path: &Path,
    enabled: bool,
) -> Result<(), String> {
    let dir = canonicalize(path).map_err(|_| "That skill is no longer there.".to_string())?;
    let rows = list_skills(cwd, agent)?;
    let Some(row) = rows.iter().find(|row| Path::new(&row.path) == dir) else {
        return Err("That skill is no longer there.".into());
    };
    if row.scope == "package" {
        return Err("A skill in a package can't be switched here.".into());
    }
    if row.enabled == enabled {
        return Ok(());
    }
    let file = settings_file(agent, cwd, &row.scope)?;
    let original = fs::read(&file).unwrap_or_default();
    let base = override_base(agent, cwd, &dir, &row.scope);
    let mut doc = read_doc(&file)?;
    if enabled {
        retain_skills(&mut doc, |entry| !exclusion_of(entry, &dir, &base));
    } else if plus_forced(&skill_strings(&doc), &base, &dir) {
        retain_skills(&mut doc, |entry| !plus_of(entry, &dir, &base));
    } else {
        let marker = format!("-{}", dir.display());
        let already = skill_strings(&doc).iter().any(|entry| entry == &marker);
        if !already {
            push_skill(&mut doc, &marker);
        }
    }
    write_doc(&file, &doc)?;
    if list_skills(cwd, agent)?
        .iter()
        .any(|row| Path::new(&row.path) == dir && row.enabled == enabled)
    {
        return Ok(());
    }
    // Still excluded by a `!pattern`: `+path` is what puts it back, and it
    // loses to a later `-path`, which was just removed.
    if enabled {
        push_skill(&mut doc, &format!("+{}", dir.display()));
        write_doc(&file, &doc)?;
        if list_skills(cwd, agent)?
            .iter()
            .any(|row| Path::new(&row.path) == dir && row.enabled)
        {
            return Ok(());
        }
    }
    restore(&file, &original)?;
    Err("Couldn't update settings.".into())
}

/// Copy `source` into the agent skills folder (`user`) or the chat's
/// `.pi/skills` (`project`). Fails without copying when the skill is not
/// one Pi would accept. `exists:<name>` means the folder is already there.
pub fn add_skill(
    agent: &Path,
    cwd: Option<&Path>,
    source: &Path,
    dest: &str,
    replace: bool,
) -> Result<(), String> {
    let source = canonicalize(source).map_err(|_| "That folder couldn't be read.".to_string())?;
    if !source.is_dir() {
        return Err("Choose a folder.".into());
    }
    let text = fs::read_to_string(source.join("SKILL.md"))
        .map_err(|_| "SKILL.md is missing.".to_string())?;
    let parsed = parse_matter(&text);
    if parsed.broken {
        return Err("SKILL.md has broken front matter.".into());
    }
    let Some(description) = parsed.description.filter(|d| !d.trim().is_empty()) else {
        return Err("SKILL.md is missing a description.".into());
    };
    let _ = description;
    let Some(name) = parsed.name.filter(|n| !n.is_empty()) else {
        return Err("SKILL.md is missing a name.".into());
    };
    let folder = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    if !valid_name(&name) {
        return Err(format!(
            "SKILL.md name \"{name}\" must be 1–64 lowercase letters, digits, or hyphens, with no leading, trailing, or doubled hyphen."
        ));
    }
    if name != folder {
        return Err(format!(
            "SKILL.md name \"{name}\" must match the folder \"{folder}\"."
        ));
    }
    ensure_contained(&source)?;

    let (root, ancestor) = match dest {
        "user" => (agent.join("skills"), agent.to_path_buf()),
        "project" => {
            let cwd = cwd.ok_or_else(|| "This chat has no project folder.".to_string())?;
            (cwd.join(".pi").join("skills"), cwd.to_path_buf())
        }
        _ => return Err("Pick Every chat or This project.".into()),
    };
    fs::create_dir_all(&root).map_err(|err| err.to_string())?;
    let root_real = canonicalize(&root).map_err(|err| err.to_string())?;
    let ancestor_real = canonicalize(&ancestor).map_err(|err| err.to_string())?;
    if !root_real.starts_with(&ancestor_real) {
        return Err("That folder is outside the skills directory.".into());
    }
    let target = root_real.join(&name);
    if !target.starts_with(&root_real) {
        return Err("That folder is outside the skills directory.".into());
    }
    if target.exists() {
        let target_real = canonicalize(&target).unwrap_or(target.clone());
        if !target_real.starts_with(&root_real) || target_real == root_real {
            return Err("That folder is outside the skills directory.".into());
        }
        if !replace {
            return Err(format!("exists:{name}"));
        }
        fs::remove_dir_all(&target_real).map_err(|err| err.to_string())?;
    }
    if source.starts_with(&target) {
        return Err("That folder is outside the skills directory.".into());
    }
    copy_dir(&source, &target).map_err(|err| err.to_string())?;
    Ok(())
}

/// Delete a skill folder. Only a folder under the agent `skills` directory
/// or the chat's `.pi/skills` can be deleted.
pub fn remove_skill(agent: &Path, cwd: Option<&Path>, path: &Path) -> Result<(), String> {
    let path = canonicalize(path).map_err(|_| "That skill is no longer there.".to_string())?;
    if !path.is_dir() {
        return Err("That skill is no longer there.".into());
    }
    let user_root = agent.join("skills");
    let project_root = cwd.map(|cwd| cwd.join(".pi").join("skills"));
    let allowed = inside(&user_root, &path)
        || project_root
            .as_ref()
            .is_some_and(|root| inside(root, &path));
    if !allowed {
        return Err("That skill can't be removed from here.".into());
    }
    fs::remove_dir_all(&path).map_err(|err| err.to_string())
}

struct Found {
    name: String,
    description: String,
    dir: PathBuf,
    scope: &'static str,
    enabled: bool,
    manual_only: bool,
    removable: bool,
    author: String,
    collection: String,
}

fn select(found: Vec<Found>) -> Vec<SkillRow> {
    let mut seen_dir = HashSet::new();
    let mut taken_name = HashSet::new();
    let mut rows = Vec::new();
    for skill in found {
        if !seen_dir.insert(skill.dir.clone()) {
            continue;
        }
        if skill.enabled && !taken_name.insert(skill.name.clone()) {
            continue;
        }
        rows.push(SkillRow {
            insert: format!("/{}", skill.name),
            name: skill.name,
            description: skill.description,
            scope: skill.scope.to_string(),
            path: skill.dir.to_string_lossy().into_owned(),
            enabled: skill.enabled,
            manual_only: skill.manual_only,
            removable: skill.removable,
            collection: skill.collection,
            author: skill.author,
            source_url: String::new(),
        });
    }
    rows
}

fn push_plain(
    out: &mut Vec<Found>,
    entries: &[String],
    base: &Path,
    scope: &'static str,
    cwd: Option<&Path>,
) -> Result<(), String> {
    for entry in entries {
        if entry.starts_with('!')
            || entry.starts_with('+')
            || entry.starts_with('-')
            || entry.contains('*')
            || entry.contains('?')
        {
            continue;
        }
        let path = resolve_from(base, entry);
        if !path.exists() {
            continue;
        }
        let patterns: Vec<String> = entries.iter().filter(|e| is_pattern(e)).cloned().collect();
        push_path(out, &path, scope, false, &patterns, base, cwd)?;
    }
    Ok(())
}

fn push_tree(
    out: &mut Vec<Found>,
    dir: &Path,
    scope: &'static str,
    removable_root: bool,
    entries: &[String],
    base: &Path,
) -> Result<(), String> {
    if !dir.is_dir() {
        return Ok(());
    }
    let mut seen = HashSet::new();
    for (skill_dir, matter) in walk(dir, 0, &mut seen)? {
        let enabled = enabled_by_overrides(entries, base, &skill_dir);
        push_found(out, skill_dir, matter, scope, enabled, removable_root);
    }
    Ok(())
}

fn push_path(
    out: &mut Vec<Found>,
    path: &Path,
    scope: &'static str,
    removable_root: bool,
    patterns: &[String],
    base: &Path,
    _cwd: Option<&Path>,
) -> Result<(), String> {
    let mut seen = HashSet::new();
    let files = if path.is_dir() {
        walk(path, 0, &mut seen)?
    } else if path.file_name().is_some_and(|n| n == "SKILL.md") {
        match load_skill(path) {
            Some(pair) => vec![pair],
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    for (skill_dir, matter) in files {
        let enabled = apply_patterns(patterns, base, &skill_dir);
        push_found(out, skill_dir, matter, scope, enabled, removable_root);
    }
    Ok(())
}

fn push_found(
    out: &mut Vec<Found>,
    dir: PathBuf,
    matter: Matter,
    scope: &'static str,
    enabled: bool,
    removable_root: bool,
) {
    let Some(dir) = canonicalize(&dir).ok() else {
        return;
    };
    let name = matter.name.filter(|n| !n.is_empty()).unwrap_or_else(|| {
        dir.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("skill")
            .to_string()
    });
    let Some(description) = matter.description.filter(|d| !d.trim().is_empty()) else {
        return;
    };
    let author = matter.author.unwrap_or_default();
    let collection = matter.source.unwrap_or_default();
    out.push(Found {
        name,
        description: clip(&description, 1024),
        dir,
        scope,
        enabled,
        manual_only: matter.manual,
        removable: removable_root,
        author,
        collection,
    });
}

fn push_packages(out: &mut Vec<Found>, base: &Path) -> Result<(), String> {
    let doc = read_doc(&base.join("settings.json")).unwrap_or_else(|_| json!({}));
    let Some(packages) = doc.get("packages").and_then(Value::as_array) else {
        return Ok(());
    };
    for package in packages {
        let (source, filter) = match package {
            Value::String(source) => (source.clone(), None),
            Value::Object(map) => {
                let Some(source) = map.get("source").and_then(Value::as_str) else {
                    continue;
                };
                let filter = map.get("skills").and_then(Value::as_array).map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                });
                (source.to_string(), filter)
            }
            _ => continue,
        };
        let Some(root) = resolve_package(&source, base) else {
            continue;
        };
        let skills = package_skills(&root);
        for (dir, matter) in skills {
            let enabled = match &filter {
                Some(patterns) if patterns.is_empty() => false,
                Some(patterns) => apply_patterns(patterns, &root, &dir),
                None => true,
            };
            if !enabled {
                continue;
            }
            push_found(out, dir, matter, "package", true, false);
        }
    }
    Ok(())
}

/// Where a package actually lives. npm and git use Pi's install folders
/// under the settings directory. Anything else is a local path. Missing
/// layouts are skipped, not guessed.
fn resolve_package(source: &str, base: &Path) -> Option<PathBuf> {
    if let Some(spec) = source.strip_prefix("npm:") {
        let name = npm_package_name(spec)?;
        let path = base.join("npm").join("node_modules").join(&name);
        return path.is_dir().then_some(path);
    }
    if let Some(spec) = source.strip_prefix("git:") {
        let path = git_checkout(base, spec)?;
        return path.is_dir().then_some(path);
    }
    let path = resolve_from(base, source);
    path.is_dir().then_some(path)
}

fn npm_package_name(spec: &str) -> Option<String> {
    if let Some(rest) = spec.strip_prefix('@') {
        let (scope, name) = rest.split_once('/')?;
        let name = name.split('@').next().filter(|n| !n.is_empty())?;
        return Some(format!("@{scope}/{name}"));
    }
    let name = spec.split('@').next().filter(|n| !n.is_empty())?;
    Some(name.to_string())
}

/// `git:github.com/owner/repo` → `{base}/git/github.com/owner/repo`, the
/// folder Pi's package manager clones into.
fn git_checkout(base: &Path, spec: &str) -> Option<PathBuf> {
    let spec = spec.trim().trim_end_matches(".git");
    let (host, path) = spec.split_once('/')?;
    if host.is_empty() || path.is_empty() || path.contains("..") {
        return None;
    }
    Some(base.join("git").join(host).join(path))
}

fn package_skills(root: &Path) -> Vec<(PathBuf, Matter)> {
    let manifest = fs::read_to_string(root.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&strip_bom(&text)).ok());
    if let Some(pi) = manifest
        .as_ref()
        .and_then(|doc| doc.get("pi"))
        .filter(|v| v.is_object())
    {
        let Some(entries) = pi.get("skills").and_then(Value::as_array) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for entry in entries.iter().filter_map(Value::as_str) {
            if entry.starts_with('!') || entry.starts_with('+') || entry.starts_with('-') {
                continue;
            }
            if entry.contains('*') || entry.contains('?') {
                continue;
            }
            let path = root.join(entry);
            if path.is_dir() {
                out.extend(walk(&path, 0, &mut seen).unwrap_or_default());
            } else if path.file_name().is_some_and(|n| n == "SKILL.md") {
                if let Some(pair) = load_skill(&path) {
                    out.push(pair);
                }
            }
        }
        return out;
    }
    let mut seen = HashSet::new();
    walk(&root.join("skills"), 0, &mut seen).unwrap_or_default()
}

fn walk(
    dir: &Path,
    depth: usize,
    seen: &mut HashSet<PathBuf>,
) -> Result<Vec<(PathBuf, Matter)>, String> {
    if depth > MAX_DEPTH {
        return Ok(Vec::new());
    }
    let Ok(canon) = canonicalize(dir) else {
        return Ok(Vec::new());
    };
    if !seen.insert(canon) {
        return Ok(Vec::new());
    }
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(err) if depth == 0 => return Err(format!("{}: {err}", dir.display())),
        Err(_) => return Ok(Vec::new()),
    };
    let mut entries: Vec<_> = read
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;
    entries.sort_by_key(|e| e.file_name());
    if let Some(skill) = entries.iter().find(|e| e.file_name() == "SKILL.md") {
        return Ok(load_skill(&skill.path()).into_iter().collect());
    }
    let mut out = Vec::new();
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type().map_err(|err| err.to_string())?;
        let is_dir = kind.is_dir() || (kind.is_symlink() && path.is_dir());
        if is_dir {
            out.extend(walk(&path, depth + 1, seen)?);
        }
    }
    Ok(out)
}

fn load_skill(file: &Path) -> Option<(PathBuf, Matter)> {
    let text = fs::read_to_string(file).ok()?;
    let matter = parse_matter(&text);
    if matter.broken
        || matter
            .description
            .as_ref()
            .is_none_or(|d| d.trim().is_empty())
    {
        return None;
    }
    Some((file.parent()?.to_path_buf(), matter))
}

struct Matter {
    name: Option<String>,
    description: Option<String>,
    author: Option<String>,
    source: Option<String>,
    manual: bool,
    broken: bool,
}

fn parse_matter(text: &str) -> Matter {
    let text = strip_bom(text);
    if !text.starts_with("---") {
        return Matter {
            name: None,
            description: None,
            author: None,
            source: None,
            manual: false,
            broken: false,
        };
    }
    let rest = text[3..].trim_start_matches(['\r', '\n']);
    let Some(end) = rest.find("\n---") else {
        return Matter {
            name: None,
            description: None,
            author: None,
            source: None,
            manual: false,
            broken: true,
        };
    };
    let front = &rest[..end];
    let mut name = None;
    let mut description = None;
    let mut author = None;
    let mut source = None;
    let mut manual = false;
    let lines: Vec<&str> = front.split('\n').collect();
    let mut ix = 0;
    while ix < lines.len() {
        let line = lines[ix].trim_end_matches('\r');
        if let Some(value) = yaml_value(line, "name") {
            name = Some(unquote(&value)).filter(|v| !v.is_empty());
        } else if let Some(value) = yaml_value(line, "description") {
            if matches!(value.as_str(), "|" | ">" | "|-" | ">-" | "") {
                let mut folded = Vec::new();
                ix += 1;
                while ix < lines.len()
                    && (lines[ix].starts_with(' ') || lines[ix].starts_with('\t'))
                {
                    folded.push(lines[ix].trim());
                    ix += 1;
                }
                description = Some(folded.join(" ")).filter(|v| !v.is_empty());
                continue;
            }
            description = Some(unquote(&value)).filter(|v| !v.is_empty());
        } else if let Some(value) = yaml_value(line, "author") {
            author = Some(unquote(&value)).filter(|v| !v.is_empty());
        } else if let Some(value) = yaml_value(line, "source") {
            source = Some(unquote(&value)).filter(|v| !v.is_empty());
        } else if let Some(value) = yaml_value(line, "disable-model-invocation") {
            manual = matches!(
                unquote(&value).to_ascii_lowercase().as_str(),
                "true" | "yes" | "on"
            );
        }
        ix += 1;
    }
    Matter {
        name,
        description,
        author,
        source,
        manual,
        broken: false,
    }
}

fn yaml_value(line: &str, key: &str) -> Option<String> {
    let rest = line.trim().strip_prefix(key)?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(':')?;
    Some(rest.trim().to_string())
}

fn unquote(value: &str) -> String {
    let value = value.trim().trim_end_matches(|c| c == '#').trim();
    value.trim_matches(|c| c == '"' || c == '\'').to_string()
}

fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

fn enabled_by_overrides(entries: &[String], base: &Path, dir: &Path) -> bool {
    let mut enabled = true;
    for entry in entries {
        if let Some(pattern) = entry.strip_prefix('!') {
            if skill_matches(pattern, base, dir, true) {
                enabled = false;
            }
        }
    }
    for entry in entries {
        if let Some(pattern) = entry.strip_prefix('+') {
            if skill_matches(pattern, base, dir, false) {
                enabled = true;
            }
        }
    }
    for entry in entries {
        if let Some(pattern) = entry.strip_prefix('-') {
            if skill_matches(pattern, base, dir, false) {
                enabled = false;
            }
        }
    }
    enabled
}

fn apply_patterns(patterns: &[String], base: &Path, dir: &Path) -> bool {
    let includes: Vec<&str> = patterns
        .iter()
        .filter_map(|p| (!p.starts_with(['!', '+', '-'])).then_some(p.as_str()))
        .collect();
    let mut enabled = if includes.is_empty() {
        true
    } else {
        includes
            .iter()
            .any(|pattern| skill_matches(pattern, base, dir, true))
    };
    for pattern in patterns.iter().filter_map(|p| p.strip_prefix('!')) {
        if skill_matches(pattern, base, dir, true) {
            enabled = false;
        }
    }
    for pattern in patterns.iter().filter_map(|p| p.strip_prefix('+')) {
        if skill_matches(pattern, base, dir, false) {
            enabled = true;
        }
    }
    for pattern in patterns.iter().filter_map(|p| p.strip_prefix('-')) {
        if skill_matches(pattern, base, dir, false) {
            enabled = false;
        }
    }
    enabled
}

fn skill_matches(pattern: &str, base: &Path, dir: &Path, glob: bool) -> bool {
    let pattern = pattern.trim().trim_start_matches("./");
    if pattern.is_empty() {
        return false;
    }
    // `/var` and `/private/var` are the same folder. Compare after resolving.
    let dir = canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let base = canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    let file = dir.join("SKILL.md");
    let rel_dir = path_of(&base, &dir);
    let rel_file = path_of(&base, &file);
    let abs_dir = dir.to_string_lossy().replace('\\', "/");
    let abs_file = file.to_string_lossy().replace('\\', "/");
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let targets = [
        rel_dir.as_str(),
        rel_file.as_str(),
        abs_dir.as_str(),
        abs_file.as_str(),
        name,
    ];
    let resolved = resolve_from(&base, pattern);
    let resolved_hit = canonicalize(&resolved)
        .ok()
        .is_some_and(|path| path == dir || path == file);
    if resolved_hit {
        return true;
    }
    if glob {
        targets.iter().any(|target| glob_match(pattern, target))
    } else {
        let pattern = pattern.replace('\\', "/");
        targets.iter().any(|target| *target == pattern)
    }
}

fn path_of(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn glob_match(pattern: &str, text: &str) -> bool {
    fn rec(pattern: &[u8], text: &[u8]) -> bool {
        let (mut i, mut j) = (0, 0);
        while i < pattern.len() {
            if pattern[i] == b'*' {
                let double = i + 1 < pattern.len() && pattern[i + 1] == b'*';
                if double {
                    i += 2;
                    if i < pattern.len() && pattern[i] == b'/' {
                        i += 1;
                    }
                    if i == pattern.len() {
                        return true;
                    }
                    while j <= text.len() {
                        if rec(&pattern[i..], &text[j..]) {
                            return true;
                        }
                        j += 1;
                    }
                    return false;
                }
                i += 1;
                loop {
                    if rec(&pattern[i..], &text[j..]) {
                        return true;
                    }
                    if j >= text.len() || text[j] == b'/' {
                        return false;
                    }
                    j += 1;
                }
            } else if pattern[i] == b'?' {
                if j >= text.len() || text[j] == b'/' {
                    return false;
                }
                i += 1;
                j += 1;
            } else {
                if j >= text.len() || pattern[i] != text[j] {
                    return false;
                }
                i += 1;
                j += 1;
            }
        }
        j == text.len()
    }
    rec(pattern.as_bytes(), text.as_bytes())
}

fn is_pattern(entry: &str) -> bool {
    entry.starts_with('!')
        || entry.starts_with('+')
        || entry.starts_with('-')
        || entry.contains('*')
        || entry.contains('?')
}

fn ancestor_agents(start: &Path, home: &Path) -> Vec<PathBuf> {
    let shared = home.join(".agents").join("skills");
    let root = git_root(start);
    let mut dir = start.to_path_buf();
    let mut out = Vec::new();
    loop {
        let skills = dir.join(".agents").join("skills");
        if canonicalize(&skills).ok() != canonicalize(&shared).ok() {
            out.push(skills);
        }
        if root.as_ref() == Some(&dir) {
            break;
        }
        if !dir.pop() {
            break;
        }
    }
    out
}

fn git_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn plus_forced(entries: &[String], base: &Path, dir: &Path) -> bool {
    if !enabled_by_overrides(entries, base, dir) {
        return false;
    }
    let without: Vec<String> = entries
        .iter()
        .filter(|entry| !entry.starts_with('+'))
        .cloned()
        .collect();
    !enabled_by_overrides(&without, base, dir)
}

fn exclusion_of(entry: &str, dir: &Path, base: &Path) -> bool {
    entry
        .strip_prefix('-')
        .is_some_and(|pattern| skill_matches(pattern, base, dir, false))
}

fn plus_of(entry: &str, dir: &Path, base: &Path) -> bool {
    entry
        .strip_prefix('+')
        .is_some_and(|pattern| skill_matches(pattern, base, dir, false))
}

fn settings_file(agent: &Path, cwd: Option<&Path>, scope: &str) -> Result<PathBuf, String> {
    if scope == "project" {
        let cwd = cwd.ok_or_else(|| "This chat has no project folder.".to_string())?;
        return Ok(cwd.join(".pi").join("settings.json"));
    }
    Ok(agent.join("settings.json"))
}

fn override_base(agent: &Path, cwd: Option<&Path>, dir: &Path, scope: &str) -> PathBuf {
    // `…/.agents/skills/<name>` is filtered against the `.agents` directory.
    if dir
        .parent()
        .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "skills"))
    {
        if let Some(agents) = dir.parent().and_then(Path::parent) {
            if agents.file_name().is_some_and(|name| name == ".agents") {
                return agents.to_path_buf();
            }
        }
    }
    if scope == "project" {
        cwd.map(|cwd| cwd.join(".pi"))
            .unwrap_or_else(|| agent.to_path_buf())
    } else {
        agent.to_path_buf()
    }
}

fn read_entries(path: &Path) -> Result<Vec<String>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(skill_strings(&read_doc(path)?))
}

fn read_doc(path: &Path) -> Result<Value, String> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = fs::read_to_string(path)
        .map_err(|err| format!("Couldn't read {}: {err}", path.display()))?;
    let doc: Value = serde_json::from_str(&strip_bom(&text))
        .map_err(|_| format!("{} isn't valid JSON.", path.display()))?;
    if !doc.is_object() {
        return Err(format!("{} isn't valid JSON.", path.display()));
    }
    Ok(doc)
}

fn skill_strings(doc: &Value) -> Vec<String> {
    doc.get("skills")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn retain_skills(doc: &mut Value, keep: impl Fn(&str) -> bool) {
    let Some(array) = doc
        .as_object_mut()
        .and_then(|map| map.get_mut("skills"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    array.retain(|entry| entry.as_str().is_none_or(&keep));
}

fn push_skill(doc: &mut Value, entry: &str) {
    let obj = doc.as_object_mut().expect("settings object");
    let array = obj.entry("skills").or_insert_with(|| json!([]));
    if !array.is_array() {
        *array = json!([]);
    }
    array
        .as_array_mut()
        .expect("skills array")
        .push(json!(entry));
}

fn write_doc(path: &Path, doc: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let text = serde_json::to_string_pretty(doc).map_err(|err| err.to_string())?;
    fs::write(path, text + "\n").map_err(|err| err.to_string())
}

fn restore(path: &Path, original: &[u8]) -> Result<(), String> {
    if original.is_empty() {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    fs::write(path, original).map_err(|err| err.to_string())
}

fn resolve_from(base: &Path, raw: &str) -> PathBuf {
    let raw = raw.trim();
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    fs::canonicalize(path)
}

fn inside(root: &Path, path: &Path) -> bool {
    let Ok(root) = canonicalize(root) else {
        return false;
    };
    let Ok(path) = canonicalize(path) else {
        return false;
    };
    path.starts_with(&root) && path != root
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => text[..cut].to_string(),
        None => text.to_string(),
    }
}

fn ensure_contained(source: &Path) -> Result<(), String> {
    let mut pending = vec![source.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).map_err(|err| err.to_string())? {
            let entry = entry.map_err(|err| err.to_string())?;
            let path = entry.path();
            if entry
                .file_type()
                .map_err(|err| err.to_string())?
                .is_symlink()
            {
                let target = fs::read_link(&path).map_err(|err| err.to_string())?;
                let resolved = if target.is_absolute() {
                    target
                } else {
                    path.parent().unwrap_or(&dir).join(target)
                };
                let resolved = canonicalize(&resolved).unwrap_or(resolved);
                if !resolved.starts_with(source) {
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("a file");
                    return Err(format!("{name} points outside the skill folder."));
                }
                continue;
            }
            if path.is_dir() {
                pending.push(path);
            }
        }
    }
    Ok(())
}

fn copy_dir(source: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            let target = fs::read_link(&from)?;
            std::os::unix::fs::symlink(target, to)?;
        } else if kind.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let path = std::env::temp_dir().join(format!("keel-skills-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn skill(dir: &Path, name: &str, description: &str, extra: &str) {
        write(
            &dir.join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: {description}\n{extra}---\n"),
        );
    }

    #[test]
    fn nested_skill_is_found_and_a_blank_description_is_not() {
        let root = tmp();
        let (agent, home, project) = (root.join("agent"), root.join("home"), root.join("project"));
        write(&project.join(".git"), "");
        skill(
            &agent.join("skills/tools/pdf"),
            "pdf-tools",
            "Work with PDFs",
            "",
        );
        write(
            &agent.join("skills/bare/SKILL.md"),
            "Just text, no front matter",
        );
        write(
            &agent.join("skills/broken/SKILL.md"),
            "---\nname: broken\ndescription: nope\n",
        );
        write(
            &agent.join("skills/empty/SKILL.md"),
            "---\nname: empty\ndescription:\n---\n",
        );

        let got = list_skills_at(Some(&project), &agent, &home).unwrap();
        assert_eq!(
            got.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["pdf-tools"]
        );
        assert_eq!(got[0].insert, "/pdf-tools");
        assert!(got[0].enabled && got[0].removable && got[0].scope == "user");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn project_skill_shadows_a_user_skill() {
        let root = tmp();
        let (agent, home, project) = (root.join("agent"), root.join("home"), root.join("project"));
        write(&project.join(".git"), "");
        skill(&agent.join("skills/pdf"), "pdf-tools", "User copy", "");
        skill(
            &project.join(".pi/skills/pdf"),
            "pdf-tools",
            "Project copy",
            "",
        );

        let got = list_skills_at(Some(&project), &agent, &home).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(
            (got[0].description.as_str(), got[0].scope.as_str()),
            ("Project copy", "project")
        );

        // When cwd is None (Keel Settings page scope), project skills are never included.
        let user_only = list_skills_at(None, &agent, &home).unwrap();
        assert_eq!(user_only.len(), 1);
        assert_eq!(
            (user_only[0].description.as_str(), user_only[0].scope.as_str()),
            ("User copy", "user")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn exclusion_flips_enabled_without_dropping_other_keys() {
        let root = tmp();
        let (agent, home) = (root.join("agent"), root.join("home"));
        skill(&agent.join("skills/pdf"), "pdf-tools", "Work with PDFs", "");
        write(
            &agent.join("settings.json"),
            r#"{"theme":"dark","packages":["npm:a"]}"#,
        );

        let path = list_skills_at(None, &agent, &home).unwrap()[0].path.clone();
        set_skill_enabled(&agent, None, Path::new(&path), false).unwrap();
        let doc: Value =
            serde_json::from_str(&fs::read_to_string(agent.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(doc["theme"], "dark");
        assert_eq!(doc["packages"], json!(["npm:a"]));
        let skills = doc["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 1);
        assert!(skills[0].as_str().unwrap().starts_with('-'));
        assert!(!list_skills_at(None, &agent, &home).unwrap()[0].enabled);

        set_skill_enabled(&agent, None, Path::new(&path), true).unwrap();
        let doc: Value =
            serde_json::from_str(&fs::read_to_string(agent.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(doc["theme"], "dark");
        assert_eq!(doc["packages"], json!(["npm:a"]));
        assert!(doc["skills"].as_array().unwrap().is_empty());
        assert!(list_skills_at(None, &agent, &home).unwrap()[0].enabled);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn manual_invocation_does_not_turn_the_skill_off() {
        let root = tmp();
        let (agent, home) = (root.join("agent"), root.join("home"));
        skill(
            &agent.join("skills/pdf"),
            "pdf-tools",
            "Work with PDFs",
            "disable-model-invocation: true\n",
        );
        let got = list_skills_at(None, &agent, &home).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].manual_only && got[0].enabled);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn add_rejects_a_bad_name_a_missing_description_and_a_path_outside_the_roots() {
        let root = tmp();
        let (agent, home) = (root.join("agent"), root.join("home"));
        fs::create_dir_all(&agent).unwrap();
        let bad = root.join("src-bad");
        skill(&bad, "PDF_Tools", "Work with PDFs", "");
        let err = add_skill(&agent, None, &bad, "user", false).unwrap_err();
        assert!(err.contains("SKILL.md"), "{err}");

        let blank = root.join("src-blank");
        write(&blank.join("SKILL.md"), "---\nname: blank\n---\n");
        let err = add_skill(&agent, None, &blank, "user", false).unwrap_err();
        assert!(err.contains("description"), "{err}");

        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(agent.join("skills-real")).unwrap();
        std::os::unix::fs::symlink(agent.join("skills-real"), agent.join("skills")).unwrap();
        // The symlink stays inside the agent dir, so point it outside.
        fs::remove_file(agent.join("skills")).unwrap();
        std::os::unix::fs::symlink(&outside, agent.join("skills")).unwrap();
        let good = root.join("pdf-tools");
        skill(&good, "pdf-tools", "Work with PDFs", "");
        let err = add_skill(&agent, None, &good, "user", false).unwrap_err();
        assert!(err.contains("outside"), "{err}");
        assert!(!outside.join("pdf-tools").exists());
        let _ = home;
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_rejects_a_shared_path_and_a_package_path() {
        let root = tmp();
        let (agent, home) = (root.join("agent"), root.join("home"));
        skill(&home.join(".agents/skills/pdf"), "shared-pdf", "Shared", "");
        let package = root.join("pkg/skills/pdf");
        skill(&package, "packaged-pdf", "Packaged", "");
        let spec = root.join("pkg");
        write(
            &agent.join("settings.json"),
            &serde_json::to_string(&json!({ "packages": [spec] })).unwrap(),
        );
        // The package path is a local directory, which is how Keel registers
        // bundled packages. It is not under either skills root.
        let shared = list_skills_at(None, &agent, &home).unwrap();
        let shared_path = shared
            .iter()
            .find(|s| s.scope == "shared")
            .unwrap()
            .path
            .clone();
        let package_path = shared
            .iter()
            .find(|s| s.scope == "package")
            .unwrap()
            .path
            .clone();
        assert!(remove_skill(&agent, None, Path::new(&shared_path)).is_err());
        assert!(remove_skill(&agent, None, Path::new(&package_path)).is_err());
        assert!(Path::new(&shared_path).join("SKILL.md").is_file());
        assert!(Path::new(&package_path).join("SKILL.md").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn skills_group_by_the_repo_they_came_from() {
        let root = tmp();
        let (agent, home) = (root.join("agent"), root.join("home"));
        skill(
            &home.join(".agents/skills/animate"),
            "animate",
            "Motion",
            "",
        );
        skill(
            &home.join(".agents/skills/emil-design-eng"),
            "emil-design-eng",
            "Craft",
            "",
        );
        skill(
            &home.join(".agents/skills/design-taste-frontend"),
            "design-taste-frontend",
            "Pages",
            "",
        );
        skill(
            &home.join(".agents/skills/principle-a"),
            "principle-a",
            "A",
            "",
        );
        skill(
            &home.join(".agents/skills/principle-b"),
            "principle-b",
            "B",
            "",
        );
        skill(
            &home.join(".agents/skills/principle-c"),
            "principle-c",
            "C",
            "",
        );
        skill(&home.join(".agents/skills/solo"), "solo", "Alone", "");
        write(
            &home.join(".agents/.skill-lock.json"),
            r#"{"version":3,"skills":{
                "animate":{"source":"emilkowalski/skill"},
                "emil-design-eng":{"source":"emilkowalski/skill"},
                "design-taste-frontend":{"source":"Leonxlnx/taste-skill"}
            }}"#,
        );
        let got = list_skills_at(None, &agent, &home).unwrap();
        let col = |name: &str| {
            got.iter()
                .find(|s| s.name == name)
                .unwrap()
                .collection
                .as_str()
        };
        assert_eq!(col("animate"), "emilkowalski/skill");
        assert_eq!(col("emil-design-eng"), "emilkowalski/skill");
        assert_eq!(col("design-taste-frontend"), "Leonxlnx/taste-skill");
        assert_eq!(col("principle-a"), "principle");
        assert_eq!(col("solo"), "");

        let author = |name: &str| {
            got.iter()
                .find(|s| s.name == name)
                .unwrap()
                .author
                .as_str()
        };
        assert_eq!(author("animate"), "Emil Kowalski");
        assert_eq!(author("emil-design-eng"), "Emil Kowalski");
        assert_eq!(author("design-taste-frontend"), "Leonxlnx");
        assert_eq!(author("principle-a"), "Principle");
        let _ = fs::remove_dir_all(&root);
    }
}

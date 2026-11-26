//! Curated skill catalog: client-side `skills.json` refresh with ETag caching,
//! bundled snapshot fallback, on-demand `SKILL.md` preview, and tarball install.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::skills;

/// Default remote URL for the curated `skills.json` index.
/// Override with `KEEL_SKILLS_INDEX_URL` for local testing or custom feeds.
pub const DEFAULT_INDEX_URL: &str =
    "https://raw.githubusercontent.com/Wasomeno/keel/main/assets/skills.json";

/// Bundled fallback snapshot so Discover works offline and on first launch.
pub const BUNDLED_INDEX_JSON: &str = include_str!("../../../assets/skills.json");

/// Stale-while-revalidate cache TTL: 6 hours.
const CACHE_TTL_SECS: u64 = 6 * 60 * 60;
const INDEX_TIMEOUT: Duration = Duration::from_secs(8);
const README_TIMEOUT: Duration = Duration::from_secs(8);
const TARBALL_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_INDEX_BYTES: usize = 2 * 1024 * 1024;
const MAX_README_BYTES: usize = 512 * 1024;
const MAX_TARBALL_BYTES: u64 = 30 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillIndex {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub skills: Vec<CatalogEntry>,
}

fn default_version() -> u32 {
    1
}

fn default_branch() -> String {
    "main".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub trending: bool,
    pub repo: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub commit: String,
}

impl CatalogEntry {
    pub fn git_ref(&self) -> &str {
        if !self.commit.trim().is_empty() {
            self.commit.trim()
        } else if !self.branch.trim().is_empty() {
            self.branch.trim()
        } else {
            "main"
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogSkillView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub category: String,
    pub trending: bool,
    pub repo: String,
    pub branch: String,
    pub path: String,
    pub source_url: String,
    pub commit: String,
    pub installed: bool,
    pub installed_path: Option<String>,
    pub update_available: bool,
    pub avatar_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseResponse {
    pub skills: Vec<CatalogSkillView>,
    pub updated_at: String,
    pub fetched_at: u64,
    /// `"remote"`, `"cache"`, or `"bundled"`.
    pub source: String,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskCache {
    fetched_at: u64,
    etag: Option<String>,
    index: SkillIndex,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("skills-catalog-cache.json")
}

fn index_url() -> String {
    std::env::var("KEEL_SKILLS_INDEX_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_INDEX_URL.to_string())
}

fn bundled_index() -> Result<SkillIndex, String> {
    serde_json::from_str::<SkillIndex>(BUNDLED_INDEX_JSON)
        .map_err(|err| format!("Bundled skills index is invalid: {err}"))
}

fn read_disk_cache(data_dir: &Path) -> Option<DiskCache> {
    let text = fs::read_to_string(cache_path(data_dir)).ok()?;
    serde_json::from_str::<DiskCache>(&text).ok()
}

fn write_disk_cache(data_dir: &Path, cache: &DiskCache) -> Result<(), String> {
    let path = cache_path(data_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(cache).map_err(|err| err.to_string())?;
    fs::write(&tmp, body).map_err(|err| err.to_string())?;
    fs::rename(&tmp, &path).map_err(|err| err.to_string())?;
    Ok(())
}

fn is_stale(fetched_at: u64, now: u64) -> bool {
    now.saturating_sub(fetched_at) >= CACHE_TTL_SECS
}

enum FetchOutcome {
    Updated {
        index: SkillIndex,
        etag: Option<String>,
    },
    NotModified,
}

fn fetch_remote_index(url: &str, etag: Option<&str>) -> Result<FetchOutcome, String> {
    let mut req = ureq::get(url)
        .set("User-Agent", "Keel")
        .set("Accept", "application/json")
        .timeout(INDEX_TIMEOUT);
    if let Some(tag) = etag {
        req = req.set("If-None-Match", tag);
    }
    match req.call() {
        Ok(resp) => {
            if resp.status() == 304 {
                return Ok(FetchOutcome::NotModified);
            }
            let new_etag = resp.header("ETag").map(str::to_string);
            let mut buf = Vec::new();
            resp.into_reader()
                .take((MAX_INDEX_BYTES + 1) as u64)
                .read_to_end(&mut buf)
                .map_err(|err| format!("Couldn't read skills index: {err}"))?;
            if buf.len() > MAX_INDEX_BYTES {
                return Err("Skills index is too large.".into());
            }
            let index: SkillIndex = serde_json::from_slice(&buf)
                .map_err(|err| format!("Invalid skills index JSON: {err}"))?;
            Ok(FetchOutcome::Updated {
                index,
                etag: new_etag,
            })
        }
        Err(ureq::Error::Status(304, _)) => Ok(FetchOutcome::NotModified),
        Err(ureq::Error::Status(code, _)) => Err(format!("Catalog server returned HTTP {code}.")),
        Err(ureq::Error::Transport(err)) => Err(format!("Couldn't reach catalog: {err}")),
    }
}

/// Browse the curated skill catalog.
///
/// Uses stale-while-revalidate semantics:
/// - `cache_only == true` returns immediately from disk cache or bundled snapshot
///   without touching the network (so the UI can paint right away).
/// - `force == true` always checks the remote URL (bypassing TTL).
/// - Otherwise refreshes from remote when the disk cache is missing or older than 6 hours.
pub fn browse_skills(
    data_dir: &Path,
    agent: &Path,
    force: bool,
    cache_only: bool,
) -> Result<BrowseResponse, String> {
    let now = now_unix();
    let cached = read_disk_cache(data_dir);
    let mut warning: Option<String> = None;

    let (index, fetched_at, source) = if cache_only {
        match cached {
            Some(c) => (c.index, c.fetched_at, "cache".to_string()),
            None => (bundled_index()?, 0, "bundled".to_string()),
        }
    } else {
        let should_fetch = force
            || cached
                .as_ref()
                .is_none_or(|c| is_stale(c.fetched_at, now));
        if should_fetch {
            let etag = if force {
                None
            } else {
                cached.as_ref().and_then(|c| c.etag.as_deref())
            };
            match fetch_remote_index(&index_url(), etag) {
                Ok(FetchOutcome::Updated { index, etag }) => {
                    let next = DiskCache {
                        fetched_at: now,
                        etag,
                        index: index.clone(),
                    };
                    let _ = write_disk_cache(data_dir, &next);
                    (index, now, "remote".to_string())
                }
                Ok(FetchOutcome::NotModified) => {
                    if let Some(mut c) = cached {
                        c.fetched_at = now;
                        let _ = write_disk_cache(data_dir, &c);
                        (c.index, now, "cache".to_string())
                    } else {
                        (bundled_index()?, 0, "bundled".to_string())
                    }
                }
                Err(err) => {
                    warning = Some(err);
                    match cached {
                        Some(c) => (c.index, c.fetched_at, "cache".to_string()),
                        None => (bundled_index()?, 0, "bundled".to_string()),
                    }
                }
            }
        } else if let Some(c) = cached {
            (c.index, c.fetched_at, "cache".to_string())
        } else {
            (bundled_index()?, 0, "bundled".to_string())
        }
    };

    let installed = skills::list_skills(None, agent).unwrap_or_default();
    let locks = skills::read_all_skill_locks(agent);
    let views = build_views(data_dir, index.skills, &installed, &locks);

    Ok(BrowseResponse {
        skills: views,
        updated_at: index.updated_at,
        fetched_at,
        source,
        warning,
    })
}

fn build_views(
    data_dir: &Path,
    entries: Vec<CatalogEntry>,
    installed: &[skills::SkillRow],
    locks: &HashMap<String, skills::SkillLockMeta>,
) -> Vec<CatalogSkillView> {
    let installed_by_name: HashMap<&str, &skills::SkillRow> = installed
        .iter()
        .filter(|r| r.scope != "project")
        .map(|r| (r.name.as_str(), r))
        .collect();

    entries
        .into_iter()
        .map(|entry| {
            let existing = installed_by_name.get(entry.name.as_str()).copied();
            let installed = existing.is_some();
            let installed_path = existing.map(|r| r.path.clone());
            let update_available = installed
                && !entry.commit.is_empty()
                && locks
                    .get(&entry.name)
                    .map(|meta| !meta.commit.is_empty() && meta.commit != entry.commit)
                    .unwrap_or(false);
            let source_url = if entry.source_url.is_empty() {
                default_source_url(&entry.repo, entry.git_ref(), &entry.path)
            } else {
                entry.source_url
            };
            let avatar_path = repo_owner(&entry.repo)
                .map(|owner| avatar_file_path(data_dir, owner))
                .filter(|p| p.is_file())
                .map(|p| p.to_string_lossy().into_owned());
            CatalogSkillView {
                id: entry.id,
                name: entry.name,
                description: entry.description,
                author: entry.author,
                category: entry.category,
                trending: entry.trending,
                repo: entry.repo,
                branch: entry.branch,
                path: entry.path,
                source_url,
                commit: entry.commit,
                installed,
                installed_path,
                update_available,
                avatar_path,
            }
        })
        .collect()
}

fn repo_owner(repo: &str) -> Option<&str> {
    let (owner, _) = repo.split_once('/')?;
    if !owner.is_empty()
        && !owner.contains("..")
        && owner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        Some(owner)
    } else {
        None
    }
}

fn avatar_file_path(data_dir: &Path, owner: &str) -> PathBuf {
    data_dir
        .join("skill-avatars")
        .join(format!("{}.img", owner.to_ascii_lowercase()))
}

fn looks_like_image(bytes: &[u8]) -> bool {
    // PNG, JPEG, or WebP magic bytes.
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || (bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
}

/// Download and cache GitHub owner avatars (`https://github.com/{owner}.png?size=64`)
/// under `{data_dir}/skill-avatars/`. Returns `owner_lowercase -> absolute_path`.
pub fn prefetch_avatars(data_dir: &Path, repos: &[String]) -> HashMap<String, String> {
    const MAX_AVATAR_BYTES: usize = 256 * 1024;
    let mut unique_owners: Vec<String> = Vec::new();
    for repo in repos {
        if let Some(owner) = repo_owner(repo) {
            let lower = owner.to_ascii_lowercase();
            if !unique_owners.iter().any(|o| o.eq_ignore_ascii_case(&lower)) {
                unique_owners.push(owner.to_string());
            }
        }
    }

    let dir = data_dir.join("skill-avatars");
    let _ = fs::create_dir_all(&dir);
    let mut out = HashMap::new();

    for owner in unique_owners {
        let key = owner.to_ascii_lowercase();
        let path = avatar_file_path(data_dir, &owner);
        if path.is_file() {
            out.insert(key, path.to_string_lossy().into_owned());
            continue;
        }
        let url = format!("https://github.com/{owner}.png?size=64");
        let Ok(resp) = ureq::get(&url)
            .set("User-Agent", "Keel")
            .timeout(Duration::from_secs(5))
            .call()
        else {
            continue;
        };
        let mut buf = Vec::new();
        if resp
            .into_reader()
            .take((MAX_AVATAR_BYTES + 1) as u64)
            .read_to_end(&mut buf)
            .is_err()
        {
            continue;
        }
        if buf.len() > MAX_AVATAR_BYTES || !looks_like_image(&buf) {
            continue;
        }
        let tmp = path.with_extension("img.tmp");
        if fs::write(&tmp, &buf).is_ok() && fs::rename(&tmp, &path).is_ok() {
            out.insert(key, path.to_string_lossy().into_owned());
        } else {
            let _ = fs::remove_file(&tmp);
        }
    }
    out
}

fn default_source_url(repo: &str, git_ref: &str, path: &str) -> String {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        format!("https://github.com/{repo}")
    } else {
        format!("https://github.com/{repo}/tree/{git_ref}/{trimmed}")
    }
}

fn validate_repo(repo: &str) -> Result<(), String> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err("Invalid repository format.".into());
    };
    let valid_part = |s: &str| {
        !s.is_empty()
            && !s.contains("..")
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    if !valid_part(owner) || !valid_part(name) || name.contains('/') {
        return Err("Invalid repository name.".into());
    }
    Ok(())
}

fn validate_git_ref(git_ref: &str) -> Result<(), String> {
    if git_ref.is_empty()
        || git_ref.contains("..")
        || !git_ref
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        return Err("Invalid branch or commit ref.".into());
    }
    Ok(())
}

fn validate_subpath(subpath: &str) -> Result<String, String> {
    let raw = subpath.trim();
    if raw.is_empty() {
        return Ok(String::new());
    }
    if raw.starts_with('/') || Path::new(raw).is_absolute() {
        return Err("Skill path must be relative.".into());
    }
    let trimmed = raw.trim_end_matches('/');
    let p = Path::new(trimmed);
    for comp in p.components() {
        match comp {
            Component::Normal(_) => {}
            _ => return Err("Skill path contains invalid segments.".into()),
        }
    }
    Ok(trimmed.to_string())
}

pub fn raw_skill_md_url(repo: &str, git_ref: &str, path: &str) -> Result<String, String> {
    validate_repo(repo)?;
    validate_git_ref(git_ref)?;
    let clean_path = validate_subpath(path)?;
    if clean_path.is_empty() {
        Ok(format!(
            "https://raw.githubusercontent.com/{repo}/{git_ref}/SKILL.md"
        ))
    } else {
        Ok(format!(
            "https://raw.githubusercontent.com/{repo}/{git_ref}/{clean_path}/SKILL.md"
        ))
    }
}

/// Fetch the raw `SKILL.md` for a catalog skill so the user can inspect its
/// full instructions before installing.
pub fn fetch_catalog_readme(repo: &str, git_ref: &str, path: &str) -> Result<String, String> {
    let url = raw_skill_md_url(repo, git_ref, path)?;
    let resp = ureq::get(&url)
        .set("User-Agent", "Keel")
        .timeout(README_TIMEOUT)
        .call()
        .map_err(|err| match err {
            ureq::Error::Status(404, _) => "SKILL.md was not found in the repository.".to_string(),
            ureq::Error::Status(code, _) => format!("GitHub returned HTTP {code}."),
            ureq::Error::Transport(e) => format!("Couldn't fetch SKILL.md: {e}"),
        })?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take((MAX_README_BYTES + 1) as u64)
        .read_to_end(&mut buf)
        .map_err(|err| format!("Couldn't read SKILL.md: {err}"))?;
    if buf.len() > MAX_README_BYTES {
        return Err("SKILL.md is too large to preview.".into());
    }
    String::from_utf8(buf).map_err(|_| "SKILL.md is not valid UTF-8.".into())
}

/// Verify `tar -tzf` entry paths: no absolute paths, no `..`, non-empty.
pub(crate) fn check_archive_entries<S: AsRef<str>>(entries: &[S]) -> Result<(), String> {
    if entries.is_empty() {
        return Err("Downloaded archive is empty.".into());
    }
    for raw in entries {
        let name = raw.as_ref();
        if name.is_empty() {
            continue;
        }
        let path = Path::new(name);
        if path.is_absolute() {
            return Err(format!("Archive contains an absolute path: {name}"));
        }
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(format!("Archive contains a parent traversal path: {name}"));
        }
    }
    Ok(())
}

/// Download a skill from GitHub, validate it with `skills::add_skill`, and
/// record its source metadata in `{agent}/skill-lock.json`.
pub fn install_catalog_skill(
    data_dir: &Path,
    agent: &Path,
    entry: &CatalogEntry,
    replace: bool,
) -> Result<(), String> {
    validate_repo(&entry.repo)?;
    let git_ref = entry.git_ref();
    validate_git_ref(git_ref)?;
    let clean_path = validate_subpath(&entry.path)?;

    let work_dir = data_dir
        .join("tmp")
        .join(format!("skill-install-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&work_dir).map_err(|err| err.to_string())?;

    let result = install_in_workdir(&work_dir, agent, entry, git_ref, &clean_path, replace);
    let _ = fs::remove_dir_all(&work_dir);
    result
}

fn install_in_workdir(
    work_dir: &Path,
    agent: &Path,
    entry: &CatalogEntry,
    git_ref: &str,
    clean_path: &str,
    replace: bool,
) -> Result<(), String> {
    let tarball_url = format!("https://codeload.github.com/{}/tar.gz/{}", entry.repo, git_ref);
    let archive_path = work_dir.join("repo.tar.gz");

    let resp = ureq::get(&tarball_url)
        .set("User-Agent", "Keel")
        .timeout(TARBALL_TIMEOUT)
        .call()
        .map_err(|err| match err {
            ureq::Error::Status(404, _) => {
                format!("Repository or branch not found ({}@{git_ref}).", entry.repo)
            }
            ureq::Error::Status(code, _) => format!("GitHub returned HTTP {code}."),
            ureq::Error::Transport(e) => format!("Couldn't download skill archive: {e}"),
        })?;

    let mut file = fs::File::create(&archive_path).map_err(|err| err.to_string())?;
    let written = std::io::copy(&mut resp.into_reader().take(MAX_TARBALL_BYTES + 1), &mut file)
        .map_err(|err| format!("Download failed: {err}"))?;
    drop(file);
    if written > MAX_TARBALL_BYTES {
        return Err("Skill repository archive exceeds the 30 MB safety limit.".into());
    }

    // List and validate archive entries before extracting.
    let list_out = Command::new("tar")
        .args(["-tzf"])
        .arg(&archive_path)
        .output()
        .map_err(|err| format!("Couldn't inspect archive: {err}"))?;
    if !list_out.status.success() {
        return Err("Downloaded archive is corrupted.".into());
    }
    let listing = String::from_utf8_lossy(&list_out.stdout);
    let names: Vec<&str> = listing.lines().collect();
    check_archive_entries(&names)?;

    let extract_dir = work_dir.join("extracted");
    fs::create_dir_all(&extract_dir).map_err(|err| err.to_string())?;
    let extract_out = Command::new("tar")
        .args(["-xzf"])
        .arg(&archive_path)
        .arg("-C")
        .arg(&extract_dir)
        .output()
        .map_err(|err| format!("Couldn't extract archive: {err}"))?;
    if !extract_out.status.success() {
        return Err("Failed to extract skill archive.".into());
    }

    // GitHub tarballs unpack into a single top-level `<repo>-<ref>/` directory.
    let top_dir = fs::read_dir(&extract_dir)
        .map_err(|err| err.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .ok_or_else(|| "Archive did not contain a repository directory.".to_string())?;

    let source_dir = if clean_path.is_empty() {
        top_dir
    } else {
        top_dir.join(clean_path)
    };
    if !source_dir.is_dir() || !source_dir.join("SKILL.md").is_file() {
        return Err(format!(
            "SKILL.md was not found at '{}' in {}.",
            if clean_path.is_empty() { "." } else { clean_path },
            entry.repo
        ));
    }

    // Stage under a folder matching SKILL.md's declared `name` so `add_skill`
    // accepts it even when the repo folder has a different name.
    let skill_name = skills::skill_name_from_file(&source_dir.join("SKILL.md"))
        .unwrap_or_else(|| entry.name.clone());
    let staged_root = work_dir.join("staged");
    let staged_dir = staged_root.join(&skill_name);
    fs::create_dir_all(&staged_root).map_err(|err| err.to_string())?;
    skills::copy_skill_tree(&source_dir, &staged_dir).map_err(|err| err.to_string())?;

    skills::add_skill(agent, None, &staged_dir, "user", replace)?;

    let source_url = if entry.source_url.is_empty() {
        default_source_url(&entry.repo, git_ref, clean_path)
    } else {
        entry.source_url.clone()
    };
    let _ = write_agent_skill_lock(agent, &skill_name, &entry.repo, &source_url, &entry.commit);
    Ok(())
}

fn write_agent_skill_lock(
    agent: &Path,
    skill_name: &str,
    repo: &str,
    source_url: &str,
    commit: &str,
) -> Result<(), String> {
    let lock_path = agent.join("skill-lock.json");
    let mut doc: Value = fs::read_to_string(&lock_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(|v: &Value| v.is_object())
        .unwrap_or_else(|| json!({ "version": 1, "skills": {} }));
    if !doc.get("skills").is_some_and(Value::is_object) {
        doc["skills"] = json!({});
    }
    doc["skills"][skill_name] = json!({
        "source": repo,
        "sourceUrl": source_url,
        "commit": commit,
    });
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let tmp = lock_path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&doc).map_err(|err| err.to_string())?;
    fs::write(&tmp, text).map_err(|err| err.to_string())?;
    fs::rename(&tmp, &lock_path).map_err(|err| err.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_index_parses_and_has_valid_entries() {
        let index = bundled_index().expect("bundled skills.json must parse");
        assert!(index.version >= 1);
        assert!(!index.skills.is_empty());
        for skill in &index.skills {
            assert!(!skill.id.is_empty());
            assert!(!skill.name.is_empty());
            assert!(!skill.description.is_empty());
            validate_repo(&skill.repo).expect("valid repo");
            validate_git_ref(skill.git_ref()).expect("valid ref");
            validate_subpath(&skill.path).expect("valid path");
        }
    }

    #[test]
    fn stale_check_respects_ttl() {
        assert!(!is_stale(1_000, 1_000 + CACHE_TTL_SECS - 1));
        assert!(is_stale(1_000, 1_000 + CACHE_TTL_SECS));
        assert!(is_stale(0, 1_000_000));
    }

    #[test]
    fn archive_entry_check_rejects_traversal_and_absolute_paths() {
        assert!(check_archive_entries(&["repo-main/", "repo-main/skills/pdf/SKILL.md"]).is_ok());
        assert!(check_archive_entries(&["../etc/passwd"]).is_err());
        assert!(check_archive_entries(&["repo-main/../../etc/passwd"]).is_err());
        assert!(check_archive_entries(&["/etc/passwd"]).is_err());
        assert!(check_archive_entries::<&str>(&[]).is_err());
    }

    #[test]
    fn raw_skill_md_url_builds_cleanly_and_rejects_bad_paths() {
        assert_eq!(
            raw_skill_md_url("anthropics/skills", "main", "skills/pdf").unwrap(),
            "https://raw.githubusercontent.com/anthropics/skills/main/skills/pdf/SKILL.md"
        );
        assert_eq!(
            raw_skill_md_url("owner/repo", "main", "").unwrap(),
            "https://raw.githubusercontent.com/owner/repo/main/SKILL.md"
        );
        assert!(raw_skill_md_url("../evil", "main", "skills/pdf").is_err());
        assert!(raw_skill_md_url("owner/repo", "main", "../secret").is_err());
        assert!(raw_skill_md_url("owner/repo", "main", "/abs/path").is_err());
    }

    #[test]
    fn disk_cache_roundtrip_and_cross_reference_marks_installed() {
        let dir = std::env::temp_dir().join(format!("keel-catalog-test-{}", uuid::Uuid::new_v4()));
        let agent = dir.join("agent");
        fs::create_dir_all(agent.join("skills/pdf")).unwrap();
        fs::write(
            agent.join("skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: PDF tools\n---\nInstructions here.\n",
        )
        .unwrap();
        write_agent_skill_lock(
            &agent,
            "pdf",
            "anthropics/skills",
            "https://github.com/anthropics/skills/tree/main/skills/pdf",
            "oldsha",
        )
        .unwrap();

        let index = bundled_index().unwrap();
        write_disk_cache(
            &dir,
            &DiskCache {
                fetched_at: 42,
                etag: Some("\"abc\"".into()),
                index,
            },
        )
        .unwrap();

        let resp = browse_skills(&dir, &agent, false, true).unwrap();
        assert_eq!(resp.source, "cache");
        assert_eq!(resp.fetched_at, 42);
        let pdf = resp.skills.iter().find(|s| s.name == "pdf").unwrap();
        assert!(pdf.installed);
        assert!(pdf.installed_path.is_some());

        let _ = fs::remove_dir_all(&dir);
    }
}

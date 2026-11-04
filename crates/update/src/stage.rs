//! Extract a verified archive into `keel-update-*/stage`, check it, and
//! find or clean staged trees across launches.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use crate::{Result, UpdateError, msg};

const PREFIX: &str = "keel-update-";
const MARKER: &str = "ready.json";

/// A staged, verified `Keel.app` waiting for the Relaunch button.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Ready {
    pub version: String,
    #[serde(default)]
    pub notes_url: Option<String>,
    /// The `keel-update-*` directory.
    #[serde(skip)]
    pub dir: PathBuf,
}

impl Ready {
    pub fn app(&self) -> PathBuf {
        self.dir.join("stage/Keel.app")
    }
}

/// A fresh `keel-update-<pid>-<nanos>` under `std::env::temp_dir()`.
pub fn new_work_dir() -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("{PREFIX}{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)
        .map_err(|e| UpdateError::new(msg::BAD_ARCHIVE, format!("temp dir: {e}")))?;
    Ok(dir)
}

fn bad_archive(detail: impl Into<String>) -> UpdateError {
    UpdateError::new(msg::BAD_ARCHIVE, detail)
}

/// `codesign --verify --strict` on a bundle.
pub fn codesign_ok(app: &Path) -> std::result::Result<(), String> {
    let out = Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(app)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Check the listing, extract with the system `tar`, check links, the
/// code signature, the bundle id and the version. Writes the marker.
pub fn stage(archive: &Path, version: &str, notes_url: Option<String>) -> Result<Ready> {
    let dir = archive.parent().ok_or_else(|| bad_archive("archive has no folder"))?.to_path_buf();
    let listing = Command::new("/usr/bin/tar")
        .arg("-tf")
        .arg(archive)
        .output()
        .map_err(|e| bad_archive(format!("tar -t: {e}")))?;
    if !listing.status.success() {
        return Err(bad_archive(String::from_utf8_lossy(&listing.stderr).trim().to_string()));
    }
    let names: Vec<String> = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    crate::check_names(&names).map_err(bad_archive)?;

    let stage = dir.join("stage");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| bad_archive(e.to_string()))?;
    let out = Command::new("/usr/bin/tar")
        .arg("-xJf")
        .arg(archive)
        .arg("-C")
        .arg(&stage)
        .output()
        .map_err(|e| bad_archive(format!("tar -x: {e}")))?;
    if !out.status.success() {
        return Err(bad_archive(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    let tops: Vec<_> = std::fs::read_dir(&stage)
        .map_err(|e| bad_archive(e.to_string()))?
        .flatten()
        .map(|e| e.file_name())
        .collect();
    if tops.len() != 1 || tops[0] != "Keel.app" {
        return Err(bad_archive("the archive must hold only Keel.app"));
    }
    check_links(&stage, Path::new("Keel.app"))?;

    let app = stage.join("Keel.app");
    codesign_ok(&app).map_err(|e| UpdateError::new(msg::BAD_APP_SIGNATURE, e))?;
    let plist = std::fs::read_to_string(app.join("Contents/Info.plist"))
        .map_err(|e| bad_archive(format!("Info.plist: {e}")))?;
    if crate::bundle_id_of(&plist).as_deref() != Some(crate::BUNDLE_ID) {
        return Err(bad_archive("the archive is not the Keel release bundle"));
    }
    let staged_version = crate::gate::plist_string_after(&plist, "CFBundleShortVersionString");
    if staged_version.as_deref() != Some(version) {
        return Err(bad_archive(format!("archive is {staged_version:?}, manifest says {version}")));
    }
    let ready = Ready { version: version.to_string(), notes_url, dir: dir.clone() };
    let _ = std::fs::write(dir.join(MARKER), serde_json::to_string(&ready).unwrap_or_default());
    Ok(ready)
}

/// Every symlink under `root/rel` resolves inside `Keel.app`.
fn check_links(root: &Path, rel: &Path) -> Result<()> {
    let here = root.join(rel);
    let meta = std::fs::symlink_metadata(&here).map_err(|e| bad_archive(e.to_string()))?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&here).map_err(|e| bad_archive(e.to_string()))?;
        return crate::check_link(rel, &target).map_err(bad_archive);
    }
    if meta.is_dir() {
        for entry in std::fs::read_dir(&here).map_err(|e| bad_archive(e.to_string()))?.flatten() {
            check_links(root, &rel.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn work_dirs(temp_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(temp_root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(PREFIX))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect()
}

fn read_marker(dir: &Path) -> Option<Ready> {
    let text = std::fs::read_to_string(dir.join(MARKER)).ok()?;
    let mut ready: Ready = serde_json::from_str(&text).ok()?;
    ready.dir = dir.to_path_buf();
    Some(ready)
}

fn newer(version: &str, current: &str) -> bool {
    match (semver::Version::parse(version), semver::Version::parse(current)) {
        (Ok(v), Ok(c)) => v > c,
        _ => false,
    }
}

/// A staged tree from an earlier launch that is still newer than this
/// build and still passes its signature check.
pub fn find_ready(temp_root: &Path, current: &str) -> Option<Ready> {
    let mut found: Vec<Ready> = work_dirs(temp_root)
        .iter()
        .filter_map(|d| read_marker(d))
        .filter(|r| newer(&r.version, current) && r.app().is_dir())
        .collect();
    found.sort_by(|a, b| {
        let v = |r: &Ready| semver::Version::parse(&r.version).ok();
        v(b).cmp(&v(a))
    });
    found.into_iter().find(|r| codesign_ok(&r.app()).is_ok())
}

/// Remove `keel-update-*` dirs older than a day, staged trees this build
/// already is (or is past), and with `keep` set every other work dir.
/// Never removes `keep` itself.
pub fn cleanup_stale(temp_root: &Path, current: &str, keep: Option<&Path>) {
    let day = Duration::from_secs(24 * 60 * 60);
    for dir in work_dirs(temp_root) {
        if keep.is_some_and(|k| k == dir) {
            continue;
        }
        let marker = read_marker(&dir);
        let obsolete = marker.as_ref().is_some_and(|r| !newer(&r.version, current));
        let live_ready = marker.is_some() && !obsolete;
        let old = std::fs::metadata(&dir)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > day);
        let replaced = keep.is_some();
        if obsolete || replaced || (old && !live_ready) {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                tracing::info!(dir = %dir.display(), "update: could not remove old work dir: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_keeps_a_newer_ready_tree_and_drops_obsolete_ones() {
        let root = std::env::temp_dir().join(format!("kut-cleanup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, version) in [("keel-update-a", Some("0.2.4")), ("keel-update-b", Some("0.2.3")), ("keel-update-c", None)] {
            let d = root.join(name);
            std::fs::create_dir_all(d.join("stage/Keel.app")).unwrap();
            if let Some(v) = version {
                std::fs::write(d.join(MARKER), format!(r#"{{"version":"{v}"}}"#)).unwrap();
            }
        }
        std::fs::create_dir_all(root.join("unrelated")).unwrap();
        cleanup_stale(&root, "0.2.3", None);
        assert!(root.join("keel-update-a").exists(), "newer ready tree stays");
        assert!(!root.join("keel-update-b").exists(), "installed version goes");
        assert!(root.join("keel-update-c").exists(), "young unmarked dir stays");
        assert!(root.join("unrelated").exists());
        cleanup_stale(&root, "0.2.3", Some(&root.join("keel-update-c")));
        assert!(!root.join("keel-update-a").exists(), "replaced by a newer stage");
        assert!(root.join("keel-update-c").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}

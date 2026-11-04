//! Archive entry checks: one top-level `Keel.app`, nothing escapes it.

use std::path::{Component, Path, PathBuf};

const ROOT: &str = "Keel.app";

/// Normalize an archive path lexically. `None` when it is absolute or a
/// `..` climbs above the archive root.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out: Vec<&std::ffi::OsStr> = Vec::new();
    for part in path.components() {
        match part {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out.iter().collect())
}

fn inside_root(path: &Path) -> bool {
    path.components().next() == Some(Component::Normal(ROOT.as_ref()))
}

/// The listing from `tar -tf`, in order. The first entry must be under
/// `Keel.app`, and so must every other; no `..`, no absolute paths.
pub fn check_names<S: AsRef<str>>(names: &[S]) -> Result<(), String> {
    if names.is_empty() {
        return Err("the archive is empty".into());
    }
    for name in names {
        let name = name.as_ref();
        let path = Path::new(name);
        if name.is_empty() || path.is_absolute() {
            return Err(format!("absolute or empty entry {name:?}"));
        }
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(format!("entry climbs out: {name:?}"));
        }
        if !inside_root(&normalize(path).unwrap_or_default()) {
            return Err(format!("entry outside Keel.app: {name:?}"));
        }
    }
    Ok(())
}

/// A symlink at archive path `entry` pointing at `target` must resolve,
/// lexically, inside `Keel.app`.
pub fn check_link(entry: &Path, target: &Path) -> Result<(), String> {
    if target.is_absolute() {
        return Err(format!("{} links to an absolute path", entry.display()));
    }
    let base = entry.parent().unwrap_or(Path::new(""));
    match normalize(&base.join(target)) {
        Some(resolved) if inside_root(&resolved) => Ok(()),
        _ => Err(format!("{} links outside Keel.app", entry.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_bundle() {
        check_names(&["Keel.app/", "Keel.app/Contents/", "Keel.app/Contents/MacOS/keel"]).unwrap();
        check_names(&["./Keel.app/Contents/MacOS/keel"]).unwrap();
    }

    #[test]
    fn rejects_escapes_and_second_roots() {
        assert!(check_names(&["../Keel.app"]).is_err());
        assert!(check_names(&["Keel.app/../../etc/passwd"]).is_err());
        assert!(check_names(&["/Applications/Keel.app/Contents/MacOS/keel"]).is_err());
        assert!(check_names(&["Keel.app/Contents/MacOS/keel", "Other.app/x"]).is_err());
        assert!(check_names(&["README", "Keel.app/Contents/MacOS/keel"]).is_err());
        assert!(check_names::<&str>(&[]).is_err());
    }

    #[test]
    fn symlink_targets_stay_inside() {
        let fw = Path::new("Keel.app/Contents/Frameworks/Chromium Embedded Framework.framework/Resources");
        check_link(fw, Path::new("Versions/Current/Resources")).unwrap();
        check_link(Path::new("Keel.app/Contents/Frameworks/X.framework/Versions/Current"), Path::new("A")).unwrap();
        assert!(check_link(Path::new("Keel.app/Contents/evil"), Path::new("../../../etc")).is_err());
        assert!(check_link(Path::new("Keel.app/evil"), Path::new("/etc/passwd")).is_err());
        assert!(check_link(Path::new("Keel.app/evil"), Path::new("..")).is_err());
    }
}

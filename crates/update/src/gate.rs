//! "When an update is allowed to run": only the installed, writable,
//! release `Keel.app`. Everything else stops before the network.

use std::path::{Path, PathBuf};

use crate::msg;

/// The bundle this process runs from, cleared to update.
#[derive(Debug, Clone, PartialEq)]
pub struct Gate {
    /// `…/Keel.app`, canonical.
    pub app: PathBuf,
    /// `…/Keel.app/Contents/MacOS/keel`, canonical.
    pub exe: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    /// `cargo run`, `KeelDebug.app`, a renamed or foreign bundle.
    NotInstalled,
    /// App Translocation, or a bundle this user cannot write (a disk image).
    MustMove,
}

impl GateError {
    pub fn message(self) -> &'static str {
        match self {
            GateError::NotInstalled => msg::NOT_INSTALLED,
            GateError::MustMove => msg::MOVE_APP,
        }
    }
}

/// `CFBundleIdentifier` from an XML `Info.plist`.
pub fn bundle_id_of(plist: &str) -> Option<String> {
    plist_string_after(plist, "CFBundleIdentifier")
}

/// The `<string>` value that follows `<key>{key}</key>`.
pub(crate) fn plist_string_after(plist: &str, key: &str) -> Option<String> {
    let tag = format!("<key>{key}</key>");
    let rest = &plist[plist.find(&tag)? + tag.len()..];
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("<string>")?;
    let end = rest.find("</string>")?;
    Some(xml_unescape(rest[..end].trim()))
}

pub(crate) fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The gate for this process.
pub fn gate() -> Result<Gate, GateError> {
    let exe = std::env::current_exe().map_err(|_| GateError::NotInstalled)?;
    gate_exe(&exe, writable)
}

/// The gate for `exe`, with the writability probe injectable for tests.
pub fn gate_exe(exe: &Path, is_writable: impl Fn(&Path) -> bool) -> Result<Gate, GateError> {
    let exe = std::fs::canonicalize(exe).map_err(|_| GateError::NotInstalled)?;
    let app = app_of(&exe).ok_or(GateError::NotInstalled)?;
    let plist = std::fs::read_to_string(app.join("Contents/Info.plist"))
        .map_err(|_| GateError::NotInstalled)?;
    if bundle_id_of(&plist).as_deref() != Some(crate::BUNDLE_ID) {
        return Err(GateError::NotInstalled);
    }
    if exe.to_string_lossy().contains("AppTranslocation") || !is_writable(&app) {
        return Err(GateError::MustMove);
    }
    Ok(Gate { app, exe })
}

/// `…/Keel.app` when `exe` is `…/Keel.app/Contents/MacOS/keel`.
fn app_of(exe: &Path) -> Option<PathBuf> {
    let named = |p: &Path, n: &str| p.file_name() == Some(n.as_ref());
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (named(exe, "keel") && named(macos, "MacOS") && named(contents, "Contents") && named(app, "Keel.app"))
        .then(|| app.to_path_buf())
}

/// `access(W_OK)`: honors read-only volumes, not only mode bits.
pub(crate) fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: a valid NUL-terminated path; access() only reads it.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLIST: &str = "<?xml version=\"1.0\"?><plist><dict>\n  <key>CFBundleIdentifier</key><string>com.keel.dev</string>\n</dict></plist>";

    fn fake_app(root: &Path, name: &str, id: &str) -> PathBuf {
        let app = root.join(name);
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/keel"), b"#!/bin/sh\n").unwrap();
        std::fs::write(app.join("Contents/Info.plist"), PLIST.replace("com.keel.dev", id)).unwrap();
        app.join("Contents/MacOS/keel")
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("keel-update-test-gate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_the_bundle_id() {
        assert_eq!(bundle_id_of(PLIST).as_deref(), Some("com.keel.dev"));
        assert_eq!(bundle_id_of("<dict></dict>"), None);
    }

    #[test]
    fn a_release_bundle_is_accepted() {
        let dir = temp("ok");
        let exe = fake_app(&dir, "Keel.app", "com.keel.dev");
        let got = gate_exe(&exe, writable).unwrap();
        assert!(got.app.ends_with("Keel.app"));
        assert!(got.exe.ends_with("Keel.app/Contents/MacOS/keel"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn debug_bundle_other_names_and_loose_binaries_are_not_installed() {
        let dir = temp("no");
        let debug = fake_app(&dir, "KeelDebug.app", "com.keel.dev");
        assert_eq!(gate_exe(&debug, writable), Err(GateError::NotInstalled));
        let wrong_id = fake_app(&dir.join("x"), "Keel.app", "com.keel.dev.debug");
        assert_eq!(gate_exe(&wrong_id, writable), Err(GateError::NotInstalled));
        let other_bin = dir.join("x/Keel.app/Contents/MacOS/keel-helper");
        std::fs::write(&other_bin, b"").unwrap();
        assert_eq!(gate_exe(&other_bin, writable), Err(GateError::NotInstalled));
        let loose = dir.join("keel");
        std::fs::write(&loose, b"").unwrap();
        assert_eq!(gate_exe(&loose, writable), Err(GateError::NotInstalled));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn translocated_or_read_only_must_move() {
        let dir = temp("tl");
        let exe = fake_app(&dir.join("AppTranslocation/ABC/d"), "Keel.app", "com.keel.dev");
        assert_eq!(gate_exe(&exe, writable), Err(GateError::MustMove));
        let exe = fake_app(&dir.join("ro"), "Keel.app", "com.keel.dev");
        assert_eq!(gate_exe(&exe, |_| false), Err(GateError::MustMove));
        let _ = std::fs::remove_dir_all(dir);
    }
}

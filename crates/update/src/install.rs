//! Replace the live `Keel.app` with a staged one, restart the launchd
//! engine when it runs from this bundle, and hand off to a `/bin/sh`
//! waiter that opens the new app once this process is gone.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::stage::codesign_ok;
use crate::{Result, UpdateError, msg};

#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    /// The live bundle, now holding the new version.
    pub app: PathBuf,
    /// `Keel.app.previous` when the sibling swap ran; the waiter deletes it.
    pub previous: Option<PathBuf>,
    /// Whether `launchctl kickstart -k` ran.
    pub kickstarted: bool,
}

/// The launchd job to restart when it runs this bundle's binary.
#[derive(Debug, Clone, Copy)]
pub struct Daemon<'a> {
    pub plist: &'a Path,
    pub label: &'a str,
}

fn run(cmd: &mut Command) -> std::result::Result<(), String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn rsync(from: &Path, to: &Path, delete: bool) -> std::result::Result<(), String> {
    // No -X, no -E: extended attributes are not copied.
    let mut cmd = Command::new("/usr/bin/rsync");
    cmd.arg("-a");
    if delete {
        cmd.arg("--delete");
    }
    cmd.args(["--exclude", "Icon?"]);
    let slash = |p: &Path| format!("{}/", p.display());
    cmd.arg(slash(from)).arg(slash(to));
    run(&mut cmd)
}

/// Install `staged` over `live`. Verifies before and after; on any failure
/// the caller must not relaunch.
pub fn install(staged: &Path, live: &Path, daemon: Option<Daemon<'_>>) -> Result<Installed> {
    codesign_ok(staged).map_err(|e| UpdateError::new(msg::BAD_APP_SIGNATURE, e))?;
    let parent = live
        .parent()
        .ok_or_else(|| UpdateError::new(msg::REDOWNLOAD, "the app has no parent folder"))?;
    let mut previous = None;
    if crate::gate::writable(parent) {
        // Sibling swap: the running process keeps the old inode.
        let next = parent.join("Keel.app.next");
        let prev = parent.join("Keel.app.previous");
        let _ = std::fs::remove_dir_all(&next);
        rsync(staged, &next, false).map_err(|e| UpdateError::new(msg::REDOWNLOAD, e))?;
        if let Err(e) = codesign_ok(&next) {
            let _ = std::fs::remove_dir_all(&next);
            return Err(UpdateError::new(msg::BAD_APP_SIGNATURE, e));
        }
        let _ = std::fs::remove_dir_all(&prev);
        std::fs::rename(live, &prev).map_err(|e| UpdateError::new(msg::REDOWNLOAD, e.to_string()))?;
        if let Err(e) = std::fs::rename(&next, live) {
            let _ = std::fs::rename(&prev, live);
            return Err(UpdateError::new(msg::REDOWNLOAD, e.to_string()));
        }
        previous = Some(prev);
    } else {
        // In place: only needs write permission inside the bundle.
        rsync(staged, live, true).map_err(|e| UpdateError::new(msg::REDOWNLOAD, e))?;
    }
    let _ = run(Command::new("/usr/bin/xattr").arg("-cr").arg(live));
    if let Err(e) = codesign_ok(live) {
        // Put the old bundle back when the swap kept it.
        if let Some(prev) = previous.take() {
            let failed = parent.join("Keel.app.failed");
            let _ = std::fs::remove_dir_all(&failed);
            if std::fs::rename(live, &failed).is_ok() {
                let _ = std::fs::rename(&prev, live);
                let _ = std::fs::remove_dir_all(&failed);
            }
        }
        return Err(UpdateError::new(msg::BAD_APP_SIGNATURE, e));
    }
    let exe = live.join("Contents/MacOS/keel");
    let kickstarted = daemon.is_some_and(|d| kickstart_if_ours(d, &exe));
    Ok(Installed { app: live.to_path_buf(), previous, kickstarted })
}

/// `launchctl kickstart -k` when the plist runs `exe` and the job is loaded.
fn kickstart_if_ours(daemon: Daemon<'_>, exe: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(daemon.plist) else {
        return false;
    };
    if !daemon_runs(&text, exe) {
        return false;
    }
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let target = format!("gui/{uid}/{}", daemon.label);
    let loaded = Command::new("/bin/launchctl")
        .args(["print", &target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !loaded {
        return false;
    }
    match run(Command::new("/bin/launchctl").args(["kickstart", "-k", &target])) {
        Ok(()) => true,
        Err(e) => {
            tracing::info!("update: kickstart {target} failed: {e}");
            false
        }
    }
}

/// Whether the plist's first `ProgramArguments` string is `exe`.
pub fn daemon_runs(plist: &str, exe: &Path) -> bool {
    let Some(at) = plist.find("<key>ProgramArguments</key>") else {
        return false;
    };
    let rest = &plist[at..];
    let Some(arr) = rest.find("<array>") else {
        return false;
    };
    let rest = &rest[arr + "<array>".len()..];
    let Some(open) = rest.find("<string>") else {
        return false;
    };
    if rest.find("</array>").is_some_and(|close| close < open) {
        return false;
    }
    let rest = &rest[open + "<string>".len()..];
    let Some(end) = rest.find("</string>") else {
        return false;
    };
    let first = PathBuf::from(crate::gate::xml_unescape(rest[..end].trim()));
    if first == exe {
        return true;
    }
    matches!(
        (std::fs::canonicalize(&first), std::fs::canonicalize(exe)),
        (Ok(a), Ok(b)) if a == b
    )
}

/// The relaunch waiter: a `/bin/sh` that waits for `pid` to exit (at most
/// a minute), removes the old bundle and the work dir, then `open`s `app`.
pub fn spawn_waiter(
    pid: u32,
    app: &Path,
    previous: Option<&Path>,
    work_dir: Option<&Path>,
) -> std::io::Result<()> {
    const SCRIPT: &str = r#"i=0
while kill -0 "$1" 2>/dev/null && [ "$i" -lt 300 ]; do sleep 0.2; i=$((i+1)); done
[ -n "$3" ] && rm -rf "$3"
[ -n "$4" ] && rm -rf "$4"
exec /usr/bin/open "$2""#;
    let empty = Path::new("");
    Command::new("/bin/sh")
        .arg("-c")
        .arg(SCRIPT)
        .arg("keel-update-waiter")
        .arg(pid.to_string())
        .arg(app)
        .arg(previous.unwrap_or(empty))
        .arg(work_dir.unwrap_or(empty))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLIST: &str = r#"<plist><dict>
  <key>Label</key><string>sh.keel.engine</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Applications/Keel.app/Contents/MacOS/keel</string>
    <string>headless</string>
  </array>
</dict></plist>"#;

    #[test]
    fn program_argument_compare() {
        assert!(daemon_runs(PLIST, Path::new("/Applications/Keel.app/Contents/MacOS/keel")));
        assert!(!daemon_runs(PLIST, Path::new("/Users/me/Codes/keel/target/release/keel")));
        assert!(!daemon_runs("<plist/>", Path::new("/Applications/Keel.app/Contents/MacOS/keel")));
        let empty = "<key>ProgramArguments</key><array></array><string>/x</string>";
        assert!(!daemon_runs(empty, Path::new("/x")));
    }
}

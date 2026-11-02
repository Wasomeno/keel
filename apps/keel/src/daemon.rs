//! Management of the macOS launchd background daemon.
//!
//! Subcommands: `keel daemon start|stop|restart|status|uninstall`.
//! The plist runs `keel headless` with `KEEL_DATA_DIR` set and the login-shell PATH
//! captured into the environment (`~/Library/LaunchAgents/sh.keel.engine.plist`).
//! It is rendered from the checked-in `scripts/sh.keel.engine.plist`.
//! Does not fork from the CLI itself; commands control `launchctl`.

use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::Command;
use std::time::Duration;

use clap::Subcommand;

pub const PLIST_LABEL: &str = "sh.keel.engine";

/// Set by the launchd plist and the systemd unit. Under it, a headless process
/// that finds another engine (lock held / already running) exits 0, which the
/// supervisor treats as "do not respawn".
pub const SUPERVISED_ENV: &str = "KEEL_SUPERVISED";

/// Exit code for headless when another engine owns `engine.lock`.
/// 0 under a supervisor (no respawn loop), 1 for an interactive run.
#[allow(dead_code)] // called from main.rs once the EDGE change lands
pub fn engine_elsewhere_exit_code() -> i32 {
    if std::env::var_os(SUPERVISED_ENV).is_some() {
        0
    } else {
        1
    }
}

#[cfg(target_os = "macos")]
const PLIST_TEMPLATE: &str = include_str!("../../../scripts/sh.keel.engine.plist");

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum DaemonAction {
    /// Bootstrap and start the launchd daemon.
    Start,
    /// Stop and bootout the launchd daemon.
    Stop,
    /// Restart the launchd daemon (refreshes plist PATH).
    Restart,
    /// Show daemon status via launchctl and engine status.
    Status,
    /// Stop the daemon and delete its plist. Never touches the data directory.
    Uninstall,
}

pub fn handle_daemon(action: DaemonAction, data_dir: &Path, port: u16) -> i32 {
    #[cfg(target_os = "macos")]
    {
        handle_macos_daemon(action, data_dir, port)
    }

    #[cfg(not(target_os = "macos"))]
    {
        match action {
            DaemonAction::Status => {
                println!("Daemon control via launchd is supported on macOS.");
                println!(
                    "On Linux, see scripts/linux-headless.service for running keel as a systemd user service."
                );
                print!(
                    "{}",
                    keel_rpc::status_report(data_dir, port, Duration::from_millis(300))
                );
                0
            }
            _ => {
                eprintln!("Daemon management via launchd is only supported on macOS.");
                eprintln!(
                    "On Linux, run as a systemd user service using scripts/linux-headless.service."
                );
                1
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn handle_macos_daemon(action: DaemonAction, data_dir: &Path, port: u16) -> i32 {
    let plist_path = match launch_agent_plist_path() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("failed to resolve LaunchAgents directory: {err}");
            return 1;
        }
    };

    match action {
        DaemonAction::Start => daemon_start(data_dir, &plist_path, false),
        DaemonAction::Restart => daemon_start(data_dir, &plist_path, true),
        DaemonAction::Stop => daemon_stop(),
        DaemonAction::Status => daemon_status(data_dir, port, &plist_path),
        DaemonAction::Uninstall => daemon_uninstall(&plist_path),
    }
}

#[cfg(target_os = "macos")]
pub fn launch_agent_plist_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME is unset".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{PLIST_LABEL}.plist")))
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(target_os = "macos")]
pub fn render_plist(binary_path: &Path, data_dir: &Path, path_env: &str) -> String {
    let log_path = data_dir.join("logs/launchd.log");
    PLIST_TEMPLATE
        .replace(
            "{{KEEL_BINARY}}",
            &xml_escape(&binary_path.to_string_lossy()),
        )
        .replace(
            "{{KEEL_DATA_DIR}}",
            &xml_escape(&data_dir.to_string_lossy()),
        )
        .replace("{{PATH}}", &xml_escape(path_env))
        .replace("{{LOG_PATH}}", &xml_escape(&log_path.to_string_lossy()))
}

/// Login + interactive zsh, like the Pi driver, so `~/.zshrc` PATH edits count.
/// Falls back to this process's PATH on error or timeout.
#[cfg(target_os = "macos")]
fn capture_login_shell_path() -> String {
    use std::io::Read;
    use std::process::Stdio;

    let fallback = || std::env::var("PATH").unwrap_or_default();
    let Ok(mut child) = Command::new("/bin/zsh")
        .args(["-lic", "print -r -- $PATH"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return fallback();
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("could not read the login-shell PATH; using the current PATH");
                return fallback();
            }
        }
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    // An interactive shell may print banners; PATH is the last path-shaped line.
    out.lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with('/'))
        .map(str::to_string)
        .unwrap_or_else(fallback)
}

#[cfg(target_os = "macos")]
fn user_domain_target() -> String {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}")
}

#[cfg(target_os = "macos")]
fn service_target() -> String {
    format!("{}/{PLIST_LABEL}", user_domain_target())
}

#[cfg(target_os = "macos")]
fn launchctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|err| format!("launchctl: {err}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let mut msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if msg.is_empty() {
            msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        Err(msg)
    }
}

/// `None` when not loaded, `Some(pid)` when loaded (`pid` is `None` while idle).
#[cfg(target_os = "macos")]
fn service_state() -> Option<Option<u32>> {
    let text = launchctl(&["print", &service_target()]).ok()?;
    Some(parse_pid(&text))
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_pid(launchctl_print: &str) -> Option<u32> {
    launchctl_print
        .lines()
        .find_map(|line| line.trim().strip_prefix("pid = "))
        .and_then(|pid| pid.trim().parse().ok())
}

/// `start`: bootstrap when not loaded; when loaded, kickstart only if idle.
/// `restart`: refresh the plist; a changed plist is booted out and back in,
/// an unchanged one is `kickstart -k`.
#[cfg(target_os = "macos")]
fn daemon_start(data_dir: &Path, plist_path: &Path, restart: bool) -> i32 {
    let binary = match std::env::current_exe() {
        Ok(b) => b,
        Err(err) => {
            eprintln!("failed to determine keel executable path: {err}");
            return 1;
        }
    };
    let state = service_state();
    if !restart && let Some(pid) = state {
        if let Some(pid) = pid {
            println!("Keel daemon ({PLIST_LABEL}) is already running (pid {pid})");
            return 0;
        }
        return match launchctl(&["kickstart", &service_target()]) {
            Ok(_) => {
                println!("Started Keel daemon ({PLIST_LABEL})");
                0
            }
            Err(err) => {
                eprintln!("launchctl kickstart failed: {err}");
                1
            }
        };
    }

    let content = render_plist(&binary, data_dir, &capture_login_shell_path());
    if let Some(parent) = plist_path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        eprintln!("failed to create {}: {err}", parent.display());
        return 1;
    }
    if let Err(err) = std::fs::create_dir_all(data_dir.join("logs")) {
        eprintln!("failed to create the log directory: {err}");
        return 1;
    }
    let unchanged = std::fs::read_to_string(plist_path).is_ok_and(|old| old == content);
    if !unchanged && let Err(err) = std::fs::write(plist_path, &content) {
        eprintln!(
            "failed to write launchd plist at {}: {err}",
            plist_path.display()
        );
        return 1;
    }

    if state.is_some() {
        // restart of a loaded service
        if unchanged {
            return match launchctl(&["kickstart", "-k", &service_target()]) {
                Ok(_) => {
                    println!("Restarted Keel daemon ({PLIST_LABEL})");
                    0
                }
                Err(err) => {
                    eprintln!("launchctl kickstart -k failed: {err}");
                    1
                }
            };
        }
        if let Err(err) = launchctl(&["bootout", &service_target()]) {
            eprintln!("launchctl bootout failed: {err}");
            return 1;
        }
    }
    bootstrap(plist_path, if restart { "Restarted" } else { "Started" })
}

/// RunAtLoad starts the job, so bootstrap alone is enough. Bootout finishes
/// asynchronously, so a just-unloaded service may need a few tries.
#[cfg(target_os = "macos")]
fn bootstrap(plist_path: &Path, verb: &str) -> i32 {
    let plist = plist_path.to_string_lossy();
    let mut last = String::new();
    for _ in 0..10 {
        match launchctl(&["bootstrap", &user_domain_target(), &plist]) {
            Ok(_) => {
                println!("{verb} Keel daemon ({PLIST_LABEL})");
                return 0;
            }
            Err(err) => last = err,
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    eprintln!("launchctl bootstrap failed: {last}");
    1
}

#[cfg(target_os = "macos")]
fn daemon_stop() -> i32 {
    if service_state().is_none() {
        println!("Keel daemon ({PLIST_LABEL}) is not running");
        return 0;
    }
    match launchctl(&["bootout", &service_target()]) {
        Ok(_) => {
            println!("Stopped Keel daemon ({PLIST_LABEL})");
            0
        }
        Err(err) => {
            eprintln!("launchctl bootout failed: {err}");
            1
        }
    }
}

/// Bootout and delete the plist. Application Support is left alone.
#[cfg(target_os = "macos")]
fn daemon_uninstall(plist_path: &Path) -> i32 {
    if daemon_stop() != 0 {
        return 1;
    }
    match std::fs::remove_file(plist_path) {
        Ok(()) => println!("Removed {}", plist_path.display()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            eprintln!("failed to remove {}: {err}", plist_path.display());
            return 1;
        }
    }
    println!("Data directory was not touched.");
    0
}

#[cfg(target_os = "macos")]
fn daemon_status(data_dir: &Path, port: u16, plist_path: &Path) -> i32 {
    match service_state() {
        Some(Some(pid)) => println!("launchd: loaded (pid = {pid})"),
        Some(None) => println!("launchd: loaded (idle)"),
        None => println!("launchd: not loaded ({})", plist_path.display()),
    }
    let report = keel_rpc::status_report(data_dir, port, Duration::from_millis(300));
    print!("{report}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escape_covers_markup() {
        assert_eq!(
            xml_escape(r#"a&b<c>"d"'e"#),
            "a&amp;b&lt;c&gt;&quot;d&quot;&apos;e"
        );
    }

    #[test]
    fn parse_pid_reads_launchctl_print() {
        assert_eq!(parse_pid("\tstate = running\n\tpid = 4242\n"), Some(4242));
        assert_eq!(parse_pid("\tstate = not running\n"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_render_plist_contains_required_fields() {
        let binary = PathBuf::from("/usr/local/bin/keel");
        let data = PathBuf::from("/Users/test/Library/Application Support/K&eel");
        let path_env = "/usr/local/bin:/usr/bin:/bin";
        let xml = render_plist(&binary, &data, path_env);

        assert!(!xml.contains("{{"), "unfilled placeholder");
        assert!(xml.contains("<string>sh.keel.engine</string>"));
        assert!(xml.contains("<string>/usr/local/bin/keel</string>"));
        assert!(xml.contains("<string>headless</string>"));
        assert!(xml.contains("<key>KEEL_DATA_DIR</key>"));
        assert!(xml.contains("<string>/Users/test/Library/Application Support/K&amp;eel</string>"));
        assert!(xml.contains(
            "<string>/Users/test/Library/Application Support/K&amp;eel/logs/launchd.log</string>"
        ));
        assert!(xml.contains("<string>/usr/local/bin:/usr/bin:/bin</string>"));
        assert!(xml.contains("<key>KEEL_SUPERVISED</key>"));
        assert!(xml.contains("<key>SuccessfulExit</key>\n        <false/>"));
    }
}

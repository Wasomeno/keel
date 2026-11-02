//! Child-process plumbing shared by every CLI driver: PATH composition,
//! executable resolution, version probes, process groups, and the redacted
//! stderr tail. Stdout records split on LF only (an optional CR is stripped);
//! U+2028 and U+2029 stay inside a record.

use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use keel_proto::{AgentEvent, TurnStatus};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::{ControlMsg, HarnessError};

pub(crate) const TERM_WAIT: Duration = Duration::from_secs(2);
pub(crate) const KILL_WAIT: Duration = Duration::from_secs(2);
/// Model-list probes. The engine caps the whole `list_models` call at 10 s.
pub(crate) const PROBE_LIMIT: Duration = Duration::from_secs(9);
const STDERR_TAIL: usize = 6;

// ---------------------------------------------------------------------------
// PATH and resolution
// ---------------------------------------------------------------------------

/// Login-shell PATH, snapshotted once per process (`zsh -lic`, 2 s limit).
pub(crate) fn login_dirs() -> Vec<PathBuf> {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let mut cmd = std::process::Command::new("zsh");
        cmd.args(["-lic", "print -r -- $PATH"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        match output_timeout(cmd, Duration::from_secs(2)) {
            Some(output) => split_login_path(&String::from_utf8_lossy(&output.stdout)),
            None => Vec::new(),
        }
    })
    .clone()
}

pub(crate) fn split_login_path(text: &str) -> Vec<PathBuf> {
    let Some(line) = text.lines().rev().find(|line| {
        let line = line.trim();
        !line.is_empty() && (line.contains(':') || line.starts_with('/'))
    }) else {
        return Vec::new();
    };
    std::env::split_paths(line.trim())
        .filter(|path| !path.as_os_str().is_empty())
        .collect()
}

/// Executable dir, then the engine PATH, then the login PATH. Deduped.
pub(crate) fn compose_path(exe: &Path, engine_path: &str, login: &[PathBuf]) -> String {
    let mut dirs = Vec::new();
    if let Some(parent) = exe.parent()
        && !parent.as_os_str().is_empty()
    {
        dirs.push(parent.to_path_buf());
    }
    dirs.extend(std::env::split_paths(engine_path));
    dirs.extend(login.iter().cloned());
    let mut seen = HashSet::new();
    let unique: Vec<PathBuf> = dirs
        .into_iter()
        .filter(|dir| !dir.as_os_str().is_empty() && seen.insert(dir.clone()))
        .collect();
    std::env::join_paths(&unique)
        .unwrap_or_else(|_| OsString::from(engine_path))
        .to_string_lossy()
        .into_owned()
}

fn engine_path() -> String {
    std::env::var("PATH").unwrap_or_default()
}

/// A command with `KEEL_*` stripped and the composed PATH.
pub(crate) fn command_with(
    exe: &Path,
    engine_path: &str,
    login: &[PathBuf],
) -> std::process::Command {
    let mut cmd = std::process::Command::new(exe);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("KEEL_") {
            cmd.env_remove(key);
        }
    }
    cmd.env("PATH", compose_path(exe, engine_path, login));
    // Keel's own Pi folder, apart from the user's `~/.pi` (see keel.rs).
    if let Some(dir) = crate::pi_agent_dir() {
        cmd.env("PI_CODING_AGENT_DIR", dir);
    }
    cmd
}

pub(crate) fn command(exe: &Path) -> std::process::Command {
    command_with(exe, &engine_path(), &login_dirs())
}

pub(crate) fn is_exec(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub(crate) fn output_timeout(
    mut cmd: std::process::Command,
    limit: Duration,
) -> Option<std::process::Output> {
    let child = cmd.spawn().ok()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(limit) {
        Ok(Ok(output)) => Some(output),
        _ => {
            signal_pid(pid, libc::SIGKILL);
            let _ = rx.recv_timeout(Duration::from_secs(1));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

pub(crate) fn signal_group(pid: u32, sig: i32) {
    if pid == 0 {
        return;
    }
    // `pid` was spawned with `process_group(0)`, so it leads that group.
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), sig);
    }
    #[cfg(not(unix))]
    let _ = sig;
}

pub(crate) fn signal_pid(pid: u32, sig: i32) {
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, sig);
    }
    #[cfg(not(unix))]
    let _ = sig;
}

// ---------------------------------------------------------------------------
// Stdout records and the stderr tail
// ---------------------------------------------------------------------------

pub(crate) fn pipe_lines(
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
) -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        loop {
            let mut buf = Vec::new();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Some(line) = record_line(&mut buf)
                        && tx.send(line).is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    rx
}

pub(crate) fn record_line(buf: &mut Vec<u8>) -> Option<String> {
    if buf.last() == Some(&b'\n') {
        buf.pop();
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    if buf.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(buf).into_owned())
    }
}

pub(crate) fn push_tail(tail: &mut VecDeque<String>, line: String) {
    tail.push_back(redact(&line));
    while tail.len() > STDERR_TAIL {
        tail.pop_front();
    }
}

pub(crate) fn redact(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if let Some(skip) = secret_span(&input[index..], &lower[index..]) {
            out.push_str("[redacted]");
            index += skip;
            continue;
        }
        let ch = input[index..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn secret_span(raw: &str, lower: &str) -> Option<usize> {
    for (needle, spaced) in [("bearer", true), ("basic", true), ("api_key=", false)] {
        if let Some(span) = keyword_span(raw, lower, needle, spaced) {
            return Some(span);
        }
    }
    for needle in ["sk-", "ghp_", "xox"] {
        if lower.starts_with(needle) {
            let token: String = raw[needle.len()..]
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
                .collect();
            if !token.is_empty() {
                return Some(needle.len() + token.len());
            }
        }
    }
    None
}

fn keyword_span(raw: &str, lower: &str, needle: &str, require_space: bool) -> Option<usize> {
    if !lower.starts_with(needle) {
        return None;
    }
    let after = &raw[needle.len()..];
    if require_space {
        let trimmed = after.trim_start();
        if trimmed.len() == after.len() || trimmed.is_empty() {
            return None;
        }
        let spaces = after.len() - trimmed.len();
        let token: String = trimmed
            .chars()
            .take_while(|ch| !ch.is_whitespace())
            .collect();
        if token.is_empty() {
            return None;
        }
        return Some(needle.len() + spaces + token.len());
    }
    let token: String = after.chars().take_while(|ch| !ch.is_whitespace()).collect();
    if token.is_empty() {
        return None;
    }
    Some(needle.len() + token.len())
}

pub(crate) fn tail_message(name: &str, tail: &VecDeque<String>, code: Option<i32>) -> String {
    if tail.is_empty() {
        match code {
            Some(code) => format!("{name} exited {code}"),
            None => format!("{name} closed its output without ending the turn"),
        }
    } else {
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

// ---------------------------------------------------------------------------
// Turn-end events
// ---------------------------------------------------------------------------

pub(crate) fn completed() -> AgentEvent {
    AgentEvent::Done {
        status: TurnStatus::Completed {},
    }
}

pub(crate) fn interrupted() -> AgentEvent {
    AgentEvent::Done {
        status: TurnStatus::Interrupted {},
    }
}

pub(crate) fn errored(message: String) -> AgentEvent {
    AgentEvent::Done {
        status: TurnStatus::Errored { message },
    }
}

pub(crate) fn summary_line(raw: &str) -> String {
    raw.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(160)
        .collect()
}

pub(crate) fn failed(err: io::Error) -> HarnessError {
    HarnessError::Failed(err.to_string())
}

// ---------------------------------------------------------------------------
// A spawned CLI
// ---------------------------------------------------------------------------

/// What the driver loop sees next.
pub(crate) enum Io {
    Line(String),
    /// A control message arrived; the probes that read a `Proc` ignore it.
    Control,
    /// Stdout reached EOF. The stderr tail has been settled.
    Closed,
    /// The interrupt escalation reached SIGKILL.
    Killed,
}

/// One child in its own process group. Stderr is drained continuously into a
/// redacted tail. Dropping it kills the group.
pub(crate) struct Proc {
    name: &'static str,
    pid: u32,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: mpsc::UnboundedReceiver<String>,
    stderr: mpsc::UnboundedReceiver<String>,
    stderr_open: bool,
    controls_open: bool,
    tail: VecDeque<String>,
    /// `(deadline, term_sent)` once an interrupt started the escalation.
    stop: Option<(Instant, bool)>,
}

impl Proc {
    pub(crate) fn spawn(
        cmd: std::process::Command,
        name: &'static str,
    ) -> Result<Self, HarnessError> {
        let mut cmd = tokio::process::Command::from(cmd);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(failed)?;
        let pid = child.id().unwrap_or(0);
        let stdin = child.stdin.take();
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            signal_group(pid, libc::SIGKILL);
            return Err(HarnessError::Failed(format!("{name} pipes")));
        };
        Ok(Self {
            name,
            pid,
            child: Some(child),
            stdin,
            stdout: pipe_lines(stdout),
            stderr: pipe_lines(stderr),
            stderr_open: true,
            controls_open: true,
            tail: VecDeque::new(),
            stop: None,
        })
    }

    pub(crate) async fn write(&mut self, value: &Value) -> io::Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stdin closed"));
        };
        let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
        bytes.push(b'\n');
        stdin.write_all(&bytes).await?;
        stdin.flush().await
    }

    pub(crate) async fn next(&mut self, controls: &mut mpsc::UnboundedReceiver<ControlMsg>) -> Io {
        loop {
            if let Some((deadline, term_sent)) = self.stop
                && Instant::now() >= deadline
            {
                if term_sent {
                    signal_group(self.pid, libc::SIGKILL);
                    return Io::Killed;
                }
                signal_group(self.pid, libc::SIGTERM);
                self.stop = Some((Instant::now() + KILL_WAIT, true));
                continue;
            }
            let deadline = self.stop.map(|(deadline, _)| deadline);
            tokio::select! {
                biased;
                line = self.stdout.recv() => {
                    return match line {
                        Some(line) => Io::Line(line),
                        None => {
                            self.settle().await;
                            Io::Closed
                        }
                    };
                }
                line = self.stderr.recv(), if self.stderr_open => match line {
                    Some(line) => push_tail(&mut self.tail, line),
                    None => self.stderr_open = false,
                },
                msg = controls.recv(), if self.controls_open && self.stop.is_none() => match msg {
                    Some(_) => return Io::Control,
                    // The engine dropped its sender. Keep reading the child.
                    None => self.controls_open = false,
                },
                _ = sleep_until(deadline), if deadline.is_some() => {}
            }
        }
    }

    /// Let stderr finish so the tail carries the final message.
    async fn settle(&mut self) {
        let until = Instant::now() + Duration::from_millis(500);
        while self.stderr_open {
            match tokio::time::timeout_at(until, self.stderr.recv()).await {
                Ok(Some(line)) => push_tail(&mut self.tail, line),
                Ok(None) | Err(_) => break,
            }
        }
    }

    /// Redacted stderr tail, or the exit code when stderr was silent.
    pub(crate) fn crash(&mut self) -> String {
        let code = self
            .child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
            .and_then(|status| status.code());
        tail_message(self.name, &self.tail, code)
    }

    /// Close stdin, give the CLI 3 s to flush its session, then kill the group.
    pub(crate) fn finish(mut self) {
        self.stdin = None;
        let pid = self.pid;
        if let Some(mut child) = self.child.take() {
            tokio::spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                signal_group(pid, libc::SIGKILL);
                let _ = child.wait().await;
            });
        }
        self.pid = 0;
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        signal_group(self.pid, libc::SIGKILL);
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    }
}

/// Steers that reached the channel after the turn ended. The driver runs them
/// as the next turn instead of dropping them.
pub(crate) fn late_steers(controls: &mut mpsc::UnboundedReceiver<ControlMsg>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(msg) = controls.try_recv() {
        if let ControlMsg::Steer { text } = msg {
            out.push(text);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_probe_strips_keel_env_and_composes_path() {
        let cmd = command_with(
            Path::new("/x/bin/tool"),
            "/a:/b",
            &[PathBuf::from("/b"), PathBuf::from("/c")],
        );
        let envs: Vec<_> = cmd.get_envs().collect();
        let path = envs
            .iter()
            .find(|(key, _)| *key == "PATH")
            .and_then(|(_, value)| *value)
            .expect("PATH");
        assert_eq!(path, "/x/bin:/a:/b:/c");
    }

    #[tokio::test]
    async fn closed_controls_do_not_spin() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "sleep 0.3; echo line; echo oops >&2"]);
        let mut proc = Proc::spawn(cmd, "sh").expect("spawn");
        let (tx, mut rx) = mpsc::unbounded_channel::<ControlMsg>();
        drop(tx);
        assert!(matches!(proc.next(&mut rx).await, Io::Line(line) if line == "line"));
        assert!(matches!(proc.next(&mut rx).await, Io::Closed));
        assert!(proc.crash().contains("oops"));
    }
}

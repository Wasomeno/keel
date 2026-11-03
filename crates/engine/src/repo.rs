//! Repository identity, git status/numstat scans, and filesystem watching.
//!
//! Subprocess git with list arguments, never a shell string.
//! 15 s timeout on git subprocesses, no TTY, CI=1, NONINTERACTIVE=1, TERM=dumb, KEEL_* stripped.
//! Repository identity:
//! 1. Root commit of trunk if one exists (origin/HEAD, main, master, trunk, then HEAD).
//! 2. Else normalized `origin` URL.
//! 3. Else sha256 hash of `device_id + ":" + absolute git_common_dir`.
//!    Numstat summary and sha256 on the registry row.
//!    Patch bytes kept under 3 MiB (`DIFF_CAP`) in profile directory and out of Loro.
//!    Watcher via `notify` 7: per-space rescans after events settle (dependency,
//!    build and `.git` internals ignored), plus a 10-minute full rescan.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub const GIT_TIMEOUT: Duration = Duration::from_secs(15);
/// Full rescan of every watched folder, a backstop for missed events.
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(600);
pub const DIFF_CAP: usize = 3 * 1024 * 1024; // 3 MiB

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumstatSummary {
    pub additions: i64,
    pub deletions: i64,
    pub files: i64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRepoStatus {
    pub is_repo: bool,
    pub repository_id: String,
    pub checkout_id: String,
    pub branch: String,
    pub numstat: NumstatSummary,
}

/// `git` with list arguments, no TTY, CI=1, NONINTERACTIVE=1, TERM=dumb, `KEEL_*` stripped.
fn git_command(cwd: &Path, args: &[&str]) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CI", "1")
        .env("NONINTERACTIVE", "1")
        .env("TERM", "dumb")
        .env("GIT_TERMINAL_PROMPT", "0");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("KEEL_") {
            cmd.env_remove(k);
        }
    }
    cmd
}

fn finish(
    status: std::process::ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<String, String> {
    if !status.success() {
        let stderr = String::from_utf8_lossy(stderr);
        return Err(format!("git command failed: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(stdout).trim().to_string())
}

/// Run a git command with timeout. The child is killed when the future is dropped.
pub async fn run_git_cmd(
    cwd: &Path,
    args: &[&str],
    timeout_duration: Duration,
) -> Result<String, String> {
    let mut cmd = tokio::process::Command::from(git_command(cwd, args));
    cmd.kill_on_drop(true);
    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    let output = match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
        Ok(res) => res.map_err(|e| format!("git execution error: {e}"))?,
        Err(_) => {
            return Err(format!(
                "git command timed out after {:?}",
                timeout_duration
            ));
        }
    };
    finish(output.status, &output.stdout, &output.stderr)
}

/// Blocking variant with the same [`GIT_TIMEOUT`]. Never call it while holding
/// the engine state lock.
pub fn run_git_cmd_sync(cwd: &Path, args: &[&str]) -> Result<String, String> {
    use std::io::Read;
    let mut child = git_command(cwd, args)
        .spawn()
        .map_err(|e| format!("failed to run git: {e}"))?;
    // Drain both pipes on threads so a large diff cannot fill a pipe and stall the poll.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = std::time::Instant::now() + GIT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git command timed out after {GIT_TIMEOUT:?}"));
            }
            Err(e) => return Err(format!("git execution error: {e}")),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    finish(status, &stdout, &stderr)
}

/// Normalize an origin URL to a canonical format.
pub fn normalize_origin_url(url: &str) -> String {
    let mut trimmed = url.trim();
    if let Some(stripped) = trimmed.strip_suffix(".git") {
        trimmed = stripped;
    }
    trimmed = trimmed.trim_end_matches('/');

    if let Some(rest) = trimmed.strip_prefix("git@") {
        return rest.replace(':', "/").to_lowercase();
    }
    if let Some(rest) = trimmed.strip_prefix("https://") {
        return rest.to_lowercase();
    }
    if let Some(rest) = trimmed.strip_prefix("http://") {
        return rest.to_lowercase();
    }
    if let Some(rest) = trimmed.strip_prefix("ssh://git@") {
        return rest.replace(':', "/").to_lowercase();
    }
    trimmed.to_lowercase()
}

/// Parse git diff --numstat output into counts and sha256 digest.
pub fn parse_numstat(output: &str) -> NumstatSummary {
    let mut additions = 0i64;
    let mut deletions = 0i64;
    let mut files = 0i64;

    for line in output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            files += 1;
            if let Ok(add) = parts[0].trim().parse::<i64>() {
                additions += add;
            }
            if let Ok(del) = parts[1].trim().parse::<i64>() {
                deletions += del;
            }
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(output.as_bytes());
    let sha256 = format!("{:x}", hasher.finalize());

    NumstatSummary {
        additions,
        deletions,
        files,
        sha256,
    }
}

/// Compute stable checkout id for a checkout path.
pub fn compute_checkout_id(checkout_path: &Path) -> String {
    let canonical = checkout_path
        .canonicalize()
        .unwrap_or_else(|_| checkout_path.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// Probe a directory for git repository status, identity, branch, and diff summary.
pub async fn scan_git_repo(device_id: &str, cwd: &Path) -> GitRepoStatus {
    let device_id = device_id.to_string();
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || scan_git_repo_sync(&device_id, &cwd))
        .await
        .unwrap_or_else(|_| not_a_repo())
}

fn not_a_repo() -> GitRepoStatus {
    GitRepoStatus {
        is_repo: false,
        repository_id: String::new(),
        checkout_id: String::new(),
        branch: String::new(),
        numstat: NumstatSummary {
            additions: 0,
            deletions: 0,
            files: 0,
            sha256: String::new(),
        },
    }
}

/// Spec "Repository identity": the root commit of the trunk if one exists;
/// else the normalized `origin` URL; else a hash of the device id plus the
/// absolute `git rev-parse --git-common-dir`.
pub fn repository_identity(device_id: &str, checkout: &Path) -> String {
    let git = |args: &[&str]| {
        run_git_cmd_sync(checkout, args)
            .ok()
            .filter(|out| !out.is_empty())
    };
    // Trunk: origin's HEAD, then the usual names, then the current HEAD.
    let mut trunks = Vec::new();
    if let Some(head) = git(&["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]) {
        trunks.push(head);
    }
    for name in [
        "refs/heads/main",
        "refs/heads/master",
        "refs/heads/trunk",
        "HEAD",
    ] {
        trunks.push(name.to_string());
    }
    // A shallow clone's boundary commit is not the real root.
    let shallow = git(&["rev-parse", "--is-shallow-repository"]).as_deref() == Some("true");
    for trunk in trunks.iter().filter(|_| !shallow) {
        let verify = format!("{trunk}^{{commit}}");
        if git(&["rev-parse", "--verify", "--quiet", &verify]).is_none() {
            continue;
        }
        // Several roots (merged histories): the smallest id is stable across clones.
        if let Some(root) =
            git(&["rev-list", "--max-parents=0", trunk.as_str()]).and_then(|roots| {
                roots
                    .lines()
                    .map(str::trim)
                    .filter(|r| r.len() >= 40)
                    .min()
                    .map(str::to_string)
            })
        {
            return root;
        }
    }
    if let Some(origin) = git(&["config", "--get", "remote.origin.url"]) {
        return normalize_origin_url(&origin);
    }
    let common_dir = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .unwrap_or_else(|| checkout.join(".git").to_string_lossy().to_string());
    let mut hasher = Sha256::new();
    hasher.update(format!("{}:{}", device_id, common_dir).as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Blocking scan. Never call it while holding the engine state lock.
pub fn scan_git_repo_sync(device_id: &str, cwd: &Path) -> GitRepoStatus {
    let Ok(toplevel) = run_git_cmd_sync(cwd, &["rev-parse", "--show-toplevel"]) else {
        return not_a_repo();
    };

    let checkout_path = PathBuf::from(&toplevel);
    let checkout_id = compute_checkout_id(&checkout_path);

    let branch = run_git_cmd_sync(&checkout_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|_| "HEAD".to_string());

    let repository_id = repository_identity(device_id, &checkout_path);

    let numstat_out = match run_git_cmd_sync(&checkout_path, &["diff", "HEAD", "--numstat"]) {
        Ok(out) => out,
        Err(_) => run_git_cmd_sync(&checkout_path, &["diff", "--numstat"]).unwrap_or_default(),
    };
    let numstat = parse_numstat(&numstat_out);

    GitRepoStatus {
        is_repo: true,
        repository_id,
        checkout_id,
        branch,
        numstat,
    }
}

/// Capture patch bytes capped at DIFF_CAP (3 MiB) and write to profile patches dir.
pub async fn capture_patch(
    cwd: &Path,
    checkout_id: &str,
    profile_dir: &Path,
) -> Result<PathBuf, String> {
    let patch_text = match run_git_cmd(cwd, &["diff", "HEAD"], GIT_TIMEOUT).await {
        Ok(out) => out,
        Err(_) => run_git_cmd(cwd, &["diff"], GIT_TIMEOUT)
            .await
            .unwrap_or_default(),
    };

    let mut bytes = patch_text.into_bytes();
    if bytes.len() > DIFF_CAP {
        bytes.truncate(DIFF_CAP);
    }

    let patches_dir = profile_dir.join("patches");
    tokio::fs::create_dir_all(&patches_dir)
        .await
        .map_err(|e| format!("failed to create patches dir: {e}"))?;

    let file_path = patches_dir.join(format!("{}.patch", checkout_id));
    tokio::fs::write(&file_path, &bytes)
        .await
        .map_err(|e| format!("failed to write patch file: {e}"))?;

    Ok(file_path)
}

pub type RepoStatusSender = mpsc::UnboundedSender<(String, GitRepoStatus, i64)>;

/// What the watcher task is told: a space changed on disk (debounced), or
/// look now (a folder was just added, or a caller asked).
#[derive(Debug)]
enum WatchMsg {
    Changed(String),
    Now(String),
}

/// Folder names whose contents never change a checkout's git status the way
/// we report it: dependencies, build output, caches. Their events are
/// dropped in the notify callback (FSEvents watches a tree; it cannot skip
/// subtrees, so filtering is all we can do).
const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
    ".parcel-cache",
    ".svelte-kit",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".gradle",
    "DerivedData",
    "Pods",
    ".idea",
    ".vscode",
];

/// Whether an event at `path` (under a watched root) can change what a
/// scan reports. Inside `.git`, only HEAD and refs matter (a commit, a
/// checkout); objects, logs, the index and lock files churn on their own,
/// and our own `git` runs touch some of them.
pub fn relevant_change(path: &Path) -> bool {
    let mut parts = path.components().map(|c| c.as_os_str().to_string_lossy());
    let mut in_git = false;
    while let Some(part) = parts.next() {
        if in_git {
            return part == "HEAD" || part == "refs" || part == "packed-refs";
        }
        if part == ".git" {
            in_git = true;
            continue;
        }
        if IGNORED_DIRS.contains(&part.as_ref()) || part == ".DS_Store" {
            return false;
        }
    }
    // `.git` itself (a worktree's gitfile) or a regular file.
    true
}

/// Events for a space gather this long after the last one before its
/// rescan (trailing edge), so a burst of saves is one scan...
pub const SETTLE: Duration = Duration::from_millis(500);
/// ...but a space that never goes quiet (a long build writing into the
/// tree) still gets one at least this often.
pub const MAX_WAIT: Duration = Duration::from_secs(5);

/// Watches space folders with notify 7. Each event is mapped to the spaces
/// it is under; each changed space is rescanned on its own after it settles
/// ([`SETTLE`], at most [`MAX_WAIT`] late), plus a full rescan every
/// [`RESCAN_INTERVAL`].
pub struct RepoWatcher {
    _watcher: Option<RecommendedWatcher>,
    watch_tx: mpsc::UnboundedSender<WatchMsg>,
    watched_spaces: Arc<std::sync::Mutex<HashMap<String, PathBuf>>>,
}

impl RepoWatcher {
    pub fn new(
        device_id: String,
        profile_dir: PathBuf,
        status_tx: RepoStatusSender,
    ) -> (Self, Option<tokio::task::JoinHandle<()>>) {
        let (watch_tx, mut watch_rx) = mpsc::unbounded_channel::<WatchMsg>();
        let watched_spaces: Arc<std::sync::Mutex<HashMap<String, PathBuf>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));
        let spaces_clone = watched_spaces.clone();

        let watcher_task = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            Some(handle.spawn(async move {
                // Space → (first pending event, last pending event).
                let mut pending: HashMap<String, (tokio::time::Instant, tokio::time::Instant)> = HashMap::new();
                let mut rescan_timer = tokio::time::interval(RESCAN_INTERVAL);
                rescan_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                // The first tick is immediate; every folder already gets a `Now`.
                rescan_timer.tick().await;
                let scan = |sid: String, path: PathBuf| {
                    let (device_id, profile_dir, status_tx) = (device_id.clone(), profile_dir.clone(), status_tx.clone());
                    async move {
                        if !path.exists() {
                            return;
                        }
                        let status = scan_git_repo(&device_id, &path).await;
                        if status.is_repo {
                            let _ = capture_patch(&path, &status.checkout_id, &profile_dir).await;
                        }
                        let now_ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as i64;
                        let _ = status_tx.send((sid, status, now_ts));
                    }
                };
                let path_of = |sid: &str| spaces_clone.lock().ok().and_then(|m| m.get(sid).cloned());
                loop {
                    let next_due = pending
                        .values()
                        .map(|(first, last)| (*last + SETTLE).min(*first + MAX_WAIT))
                        .min();
                    tokio::select! {
                        msg = watch_rx.recv() => {
                            let Some(msg) = msg else { break };
                            let now = tokio::time::Instant::now();
                            match msg {
                                WatchMsg::Changed(sid) => {
                                    pending.entry(sid).and_modify(|(_, last)| *last = now).or_insert((now, now));
                                }
                                WatchMsg::Now(sid) => {
                                    pending.remove(&sid);
                                    if let Some(path) = path_of(&sid) {
                                        scan(sid, path).await;
                                    }
                                }
                            }
                        }
                        _ = async {
                            match next_due {
                                Some(at) => tokio::time::sleep_until(at).await,
                                None => std::future::pending::<()>().await,
                            }
                        } => {
                            let now = tokio::time::Instant::now();
                            let due: Vec<String> = pending
                                .iter()
                                .filter(|(_, (first, last))| now >= (*last + SETTLE).min(*first + MAX_WAIT))
                                .map(|(sid, _)| sid.clone())
                                .collect();
                            for sid in due {
                                pending.remove(&sid);
                                if let Some(path) = path_of(&sid) {
                                    scan(sid, path).await;
                                }
                            }
                        }
                        _ = rescan_timer.tick() => {
                            let targets: Vec<(String, PathBuf)> = spaces_clone
                                .lock()
                                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                                .unwrap_or_default();
                            for (sid, path) in targets {
                                scan(sid, path).await;
                            }
                        }
                    }
                }
            }))
        } else {
            warn!("repo watcher: no tokio runtime; folder changes will not be picked up");
            None
        };

        // The callback maps each relevant path to the spaces it is under.
        let watcher_tx_clone = watch_tx.clone();
        let spaces_for_events = watched_spaces.clone();
        let watcher_res = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let Ok(event) = res else { return };
            let Ok(spaces) = spaces_for_events.lock() else {
                return;
            };
            let mut hit: Vec<&String> = Vec::new();
            for path in &event.paths {
                for (sid, root) in spaces.iter() {
                    if hit.contains(&sid) {
                        continue;
                    }
                    if let Ok(rel) = path.strip_prefix(root)
                        && relevant_change(rel)
                    {
                        hit.push(sid);
                    }
                }
            }
            for sid in hit {
                let _ = watcher_tx_clone.send(WatchMsg::Changed(sid.clone()));
            }
        });

        let watcher = match watcher_res {
            Ok(w) => Some(w),
            Err(e) => {
                warn!("failed to initialize notify watcher: {e}");
                None
            }
        };

        (
            Self {
                _watcher: watcher,
                watch_tx,
                watched_spaces,
            },
            watcher_task,
        )
    }

    /// Watch a space folder path and scan it now.
    pub fn watch_folder(&mut self, path: &Path, space_id: &str) {
        // FSEvents reports canonical paths (/private/var/...), so match on those.
        let root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Ok(mut map) = self.watched_spaces.lock() {
            map.insert(space_id.to_string(), root.clone());
        }
        if let Some(ref mut watcher) = self._watcher
            && root.exists()
            && let Err(e) = watcher.watch(&root, RecursiveMode::Recursive)
        {
            debug!("notify watch error for {}: {e}", root.display());
        }
        let _ = self.watch_tx.send(WatchMsg::Now(space_id.to_string()));
    }

    pub fn trigger_rescan(&self, space_id: &str) {
        let _ = self.watch_tx.send(WatchMsg::Now(space_id.to_string()));
    }
}

/// List local branches in repository.
pub async fn list_branches(cwd: &Path) -> Result<Vec<String>, String> {
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || list_branches_sync(&cwd))
        .await
        .map_err(|e| e.to_string())?
}

/// Synchronous listing of local branches in repository.
pub fn list_branches_sync(cwd: &Path) -> Result<Vec<String>, String> {
    let out = run_git_cmd_sync(cwd, &["branch", "--list", "--format=%(refname:short)"])?;
    let branches = out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    Ok(branches)
}

/// Where a checkout's HEAD points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// On a branch (also an unborn one, before the first commit).
    Branch(String),
    /// Detached, at this short commit sha.
    Detached(String),
}

/// The checkout's HEAD: its branch, or the short sha when detached.
pub fn current_head_sync(cwd: &Path) -> Result<Head, String> {
    if let Some(head) = read_head_file(cwd) {
        return Ok(head);
    }
    // `symbolic-ref` also names an unborn branch, where `rev-parse
    // --abbrev-ref HEAD` fails; it fails when HEAD is detached.
    if let Ok(name) = run_git_cmd_sync(cwd, &["symbolic-ref", "--short", "-q", "HEAD"])
        && !name.is_empty()
    {
        return Ok(Head::Branch(name));
    }
    match run_git_cmd_sync(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(name) if !name.is_empty() && name != "HEAD" => Ok(Head::Branch(name)),
        _ => run_git_cmd_sync(cwd, &["rev-parse", "--short", "HEAD"]).map(Head::Detached),
    }
}

/// HEAD read straight from the checkout's git dir (no subprocess): `.git`
/// as a directory, or a worktree's `.git` file pointing at one. `None` when
/// `cwd` isn't a checkout root or HEAD is unusual; git then decides.
pub fn read_head_file(cwd: &Path) -> Option<Head> {
    let dot = cwd.join(".git");
    let git_dir = if dot.is_dir() {
        dot
    } else {
        let text = std::fs::read_to_string(&dot).ok()?;
        let target = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        if target.is_absolute() {
            target
        } else {
            cwd.join(target)
        }
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    // Detached (a bare sha) is left to git, which picks the short sha's length.
    let name = head.strip_prefix("ref: refs/heads/")?;
    (!name.is_empty()).then(|| Head::Branch(name.to_string()))
}

/// Local branches plus where HEAD is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BranchList {
    pub branches: Vec<String>,
    /// The checked-out branch; `None` when detached.
    pub current: Option<String>,
    /// Short sha when HEAD is detached.
    pub detached: Option<String>,
}

/// Local branches and the current one (listed even before its first commit).
pub fn list_branches_with_head_sync(cwd: &Path) -> Result<BranchList, String> {
    let mut branches = list_branches_sync(cwd)?;
    let (current, detached) = match current_head_sync(cwd) {
        Ok(Head::Branch(name)) => (Some(name), None),
        Ok(Head::Detached(sha)) => (None, Some(sha)),
        Err(_) => (None, None),
    };
    if let Some(name) = &current
        && !branches.contains(name)
    {
        branches.insert(0, name.clone());
    }
    Ok(BranchList {
        branches,
        current,
        detached,
    })
}

/// Uncommitted changes to tracked files (staged or not). Untracked files
/// don't count: checkout carries them over and refuses on a real clash.
pub fn has_uncommitted_changes_sync(cwd: &Path) -> Result<bool, String> {
    Ok(!run_git_cmd_sync(cwd, &["status", "--porcelain", "--untracked-files=no"])?.is_empty())
}

/// Check out `branch` in `cwd`'s checkout. A branch that only exists on
/// `origin` is created locally, tracking it. Refuses with a clear message
/// when the tree has uncommitted changes.
pub fn checkout_branch_sync(cwd: &Path, branch: &str) -> Result<(), String> {
    let branch = branch.trim();
    if branch.is_empty() || branch.starts_with('-') {
        return Err(format!("not a branch name: {branch:?}"));
    }
    if has_uncommitted_changes_sync(cwd)? {
        let folder = cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| cwd.display().to_string());
        return Err(format!(
            "Uncommitted changes in {folder} — commit or stash them, or use a worktree, to switch to {branch}"
        ));
    }
    let local = format!("refs/heads/{branch}");
    if run_git_cmd_sync(cwd, &["rev-parse", "--verify", "--quiet", &local]).is_ok() {
        return run_git_cmd_sync(cwd, &["checkout", branch, "--"]).map(|_| ());
    }
    let remote = format!("refs/remotes/origin/{branch}");
    if run_git_cmd_sync(cwd, &["rev-parse", "--verify", "--quiet", &remote]).is_ok() {
        let upstream = format!("origin/{branch}");
        return run_git_cmd_sync(cwd, &["checkout", "-b", branch, "--track", &upstream])
            .map(|_| ());
    }
    Err(format!("branch {branch} not found"))
}

/// Most paths `list_paths_sync` returns.
const PATH_LIMIT: usize = 20_000;

/// Files and folders under `cwd` for `@` references, relative, folders
/// ending in `/`, sorted. A git repo lists tracked and untracked files minus
/// ignored ones; anything else is walked, skipping hidden entries and
/// dependency/build folders.
pub fn list_paths_sync(cwd: &Path) -> Result<Vec<String>, String> {
    let files: Vec<String> = match run_git_cmd_sync(
        cwd,
        &["ls-files", "--cached", "--others", "--exclude-standard"],
    ) {
        Ok(out) => out
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty())
            .take(PATH_LIMIT)
            .collect(),
        Err(_) => walk_files(cwd)?,
    };
    let mut paths = std::collections::BTreeSet::new();
    for file in files {
        let mut dir = file.as_str();
        while let Some((parent, _)) = dir.rsplit_once('/') {
            if !paths.insert(format!("{parent}/")) {
                break;
            }
            dir = parent;
        }
        paths.insert(file);
    }
    Ok(paths.into_iter().collect())
}

fn walk_files(root: &Path) -> Result<Vec<String>, String> {
    const SKIP: &[&str] = &["node_modules", "target", "dist", "build", ".git"];
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || SKIP.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().into_owned());
                if out.len() >= PATH_LIMIT {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

/// Adds a git worktree at `worktree_path` for the repository at `repo_path`.
pub async fn add_worktree(
    repo_path: &Path,
    worktree_path: &Path,
    branch: Option<&str>,
) -> Result<(), String> {
    let (repo, wt, branch) = (
        repo_path.to_path_buf(),
        worktree_path.to_path_buf(),
        branch.map(str::to_string),
    );
    tokio::task::spawn_blocking(move || add_worktree_sync(&repo, &wt, branch.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

/// Synchronous addition of a git worktree at `worktree_path`.
pub fn add_worktree_sync(
    repo_path: &Path,
    worktree_path: &Path,
    branch: Option<&str>,
) -> Result<(), String> {
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create worktree parent dir: {e}"))?;
    }
    let path_str = worktree_path.to_string_lossy().to_string();

    if let Some(b) = branch
        && !b.is_empty()
    {
        if run_git_cmd_sync(repo_path, &["worktree", "add", &path_str, b]).is_ok() {
            return Ok(());
        }
        if run_git_cmd_sync(repo_path, &["worktree", "add", "-b", b, &path_str]).is_ok() {
            return Ok(());
        }
        return run_git_cmd_sync(repo_path, &["worktree", "add", "--detach", &path_str, b])
            .map(|_| ());
    }

    run_git_cmd_sync(
        repo_path,
        &["worktree", "add", "--detach", &path_str, "HEAD"],
    )
    .map(|_| ())
}

/// Removes a git worktree at `worktree_path`.
pub async fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> Result<(), String> {
    let (repo, wt) = (repo_path.to_path_buf(), worktree_path.to_path_buf());
    tokio::task::spawn_blocking(move || remove_worktree_sync(&repo, &wt))
        .await
        .map_err(|e| e.to_string())?
}

/// Spec: "Remove uses `git worktree remove` on that worktree path only." No
/// `--force` (uncommitted work stays) and no recursive delete fallback. The
/// caller checks the path is an engine-created worktree under the data dir.
pub fn remove_worktree_sync(repo_path: &Path, worktree_path: &Path) -> Result<(), String> {
    let path_str = worktree_path.to_string_lossy().to_string();
    run_git_cmd_sync(repo_path, &["worktree", "remove", &path_str])?;
    let _ = run_git_cmd_sync(repo_path, &["worktree", "prune"]);
    Ok(())
}

/// True when `cwd` is strictly inside `{data_dir}/worktrees/`, compared on
/// canonical paths component-wise.
pub fn is_engine_worktree(data_dir: &Path, cwd: &Path) -> bool {
    let (Ok(root), Ok(cwd)) = (
        data_dir.join("worktrees").canonicalize(),
        cwd.canonicalize(),
    ) else {
        return false;
    };
    cwd != root && cwd.starts_with(&root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_paths_walks_a_plain_folder_with_its_folders() {
        let root = std::env::temp_dir().join(format!("keel-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/ui")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        for file in [
            "README.md",
            "src/main.rs",
            "src/ui/view.rs",
            "node_modules/x/i.js",
            ".hidden/h",
        ] {
            std::fs::write(root.join(file), "").unwrap();
        }
        let paths = list_paths_sync(&root).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            paths,
            [
                "README.md",
                "src/",
                "src/main.rs",
                "src/ui/",
                "src/ui/view.rs"
            ]
        );
    }

    #[test]
    fn parse_numstat_parses_counts_and_computes_sha256() {
        let text = "10\t2\tsrc/lib.rs\n5\t0\tCargo.toml\n-\t-\timage.png\n";
        let summary = parse_numstat(text);
        assert_eq!(summary.files, 3);
        assert_eq!(summary.additions, 15);
        assert_eq!(summary.deletions, 2);
        assert!(!summary.sha256.is_empty());
        assert_eq!(summary.sha256.len(), 64);
    }

    #[test]
    fn normalize_origin_url_handles_various_formats() {
        assert_eq!(
            normalize_origin_url("git@github.com:foo/bar.git"),
            "github.com/foo/bar"
        );
        assert_eq!(
            normalize_origin_url("https://github.com/foo/bar.git"),
            "github.com/foo/bar"
        );
        assert_eq!(
            normalize_origin_url("https://github.com/foo/bar/"),
            "github.com/foo/bar"
        );
        assert_eq!(
            normalize_origin_url("ssh://git@gitlab.com/company/project.git"),
            "gitlab.com/company/project"
        );
    }

    #[tokio::test]
    async fn scan_git_repo_on_current_repo_or_temp() {
        let temp = std::env::temp_dir().join(format!("keel-test-repo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();

        // 1. In non-git directory
        let status = scan_git_repo("dev_test", &temp).await;
        assert!(!status.is_repo);

        // 2. Initialize a git repo
        let _ = run_git_cmd(&temp, &["init"], GIT_TIMEOUT).await;
        let _ = run_git_cmd(&temp, &["config", "user.name", "Keel Test"], GIT_TIMEOUT).await;
        let _ = run_git_cmd(
            &temp,
            &["config", "user.email", "test@keel.dev"],
            GIT_TIMEOUT,
        )
        .await;

        let status_empty = scan_git_repo("dev_test", &temp).await;
        assert!(status_empty.is_repo);
        assert!(!status_empty.checkout_id.is_empty());
        assert!(!status_empty.repository_id.is_empty());

        // Create a commit
        std::fs::write(temp.join("hello.txt"), b"hello world\n").unwrap();
        let _ = run_git_cmd(&temp, &["add", "hello.txt"], GIT_TIMEOUT).await;
        let _ = run_git_cmd(&temp, &["commit", "-m", "init commit"], GIT_TIMEOUT).await;

        let status_with_commit = scan_git_repo("dev_test", &temp).await;
        assert!(status_with_commit.is_repo);
        assert_eq!(status_with_commit.repository_id.len(), 40); // 40 char root commit sha

        // Modify file to produce numstat diff
        std::fs::write(temp.join("hello.txt"), b"hello world\nnew line\n").unwrap();
        let status_with_diff = scan_git_repo("dev_test", &temp).await;
        assert_eq!(status_with_diff.numstat.files, 1);
        assert_eq!(status_with_diff.numstat.additions, 1);
        assert_eq!(status_with_diff.numstat.deletions, 0);

        let _ = std::fs::remove_dir_all(temp);
    }

    #[tokio::test]
    async fn worktree_add_and_remove_under_data_dir() {
        let repo_dir = std::env::temp_dir().join(format!("keel-repo-wt-{}", uuid::Uuid::new_v4()));
        let data_dir = std::env::temp_dir().join(format!("keel-data-wt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&repo_dir).unwrap();

        let _ = run_git_cmd(&repo_dir, &["init"], GIT_TIMEOUT).await;
        let _ = run_git_cmd(
            &repo_dir,
            &["config", "user.name", "Keel Test"],
            GIT_TIMEOUT,
        )
        .await;
        let _ = run_git_cmd(
            &repo_dir,
            &["config", "user.email", "test@keel.dev"],
            GIT_TIMEOUT,
        )
        .await;

        std::fs::write(repo_dir.join("main.rs"), b"fn main() {}\n").unwrap();
        let _ = run_git_cmd(&repo_dir, &["add", "main.rs"], GIT_TIMEOUT).await;
        let _ = run_git_cmd(&repo_dir, &["commit", "-m", "first commit"], GIT_TIMEOUT).await;

        // List branches
        let branches = list_branches(&repo_dir).await.expect("list branches");
        assert!(!branches.is_empty());

        let status = scan_git_repo("dev1", &repo_dir).await;
        assert!(status.is_repo);

        let wt_path = data_dir
            .join("worktrees")
            .join(&status.repository_id)
            .join("chat-123");
        // Add worktree
        add_worktree(&repo_dir, &wt_path, Some("feat-test"))
            .await
            .expect("add worktree");
        assert!(wt_path.exists());
        assert!(wt_path.join("main.rs").exists());

        // Scan worktree
        let wt_status = scan_git_repo("dev1", &wt_path).await;
        assert!(wt_status.is_repo);
        assert_eq!(wt_status.repository_id, status.repository_id);

        // Remove worktree
        remove_worktree(&repo_dir, &wt_path)
            .await
            .expect("remove worktree");
        assert!(!wt_path.exists());

        let _ = std::fs::remove_dir_all(repo_dir);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn identity_is_the_trunk_root_and_the_fallback_is_absolute() {
        let dir = std::env::temp_dir().join(format!("keel-id-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| run_git_cmd_sync(&dir, args).expect("git");
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "T"]);
        git(&["config", "user.email", "t@t"]);
        // Empty repo: hash of device id and the absolute common dir.
        let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]);
        assert!(Path::new(&common).is_absolute());
        let mut hasher = Sha256::new();
        hasher.update(format!("dev:{common}").as_bytes());
        assert_eq!(
            repository_identity("dev", &dir),
            format!("{:x}", hasher.finalize())
        );

        git(&["commit", "--allow-empty", "-m", "root"]);
        let main_root = git(&["rev-parse", "HEAD"]);
        // An unrelated history checked out at HEAD does not change the identity.
        git(&["checkout", "--orphan", "other"]);
        git(&["commit", "--allow-empty", "-m", "other root"]);
        assert_ne!(git(&["rev-parse", "HEAD"]), main_root);
        assert_eq!(repository_identity("dev", &dir), main_root);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A repo with one commit on `main` and a second branch `feat`.
    fn repo_with_branches(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("keel-branch-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Keel Test"],
            &["config", "user.email", "test@keel.dev"],
            &["config", "commit.gpgsign", "false"],
        ] {
            run_git_cmd_sync(&dir, args).unwrap();
        }
        std::fs::write(dir.join("a.txt"), b"one\n").unwrap();
        run_git_cmd_sync(&dir, &["add", "a.txt"]).unwrap();
        run_git_cmd_sync(&dir, &["commit", "-m", "first"]).unwrap();
        run_git_cmd_sync(&dir, &["branch", "feat"]).unwrap();
        dir
    }

    #[test]
    fn branch_list_reports_the_current_branch() {
        let dir = repo_with_branches("current");
        let list = list_branches_with_head_sync(&dir).unwrap();
        assert_eq!(list.current.as_deref(), Some("main"));
        assert_eq!(list.detached, None);
        assert!(list.branches.contains(&"feat".to_string()));
        assert!(list.branches.contains(&"main".to_string()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn head_file_matches_git_in_a_checkout_and_a_worktree() {
        let dir = repo_with_branches("headfile");
        let git = run_git_cmd_sync(&dir, &["symbolic-ref", "--short", "HEAD"]).unwrap();
        assert_eq!(read_head_file(&dir), Some(Head::Branch(git)));
        let wt =
            std::env::temp_dir().join(format!("keel-branch-headfile-wt-{}", uuid::Uuid::new_v4()));
        run_git_cmd_sync(
            &dir,
            &["worktree", "add", "-b", "wt/side", wt.to_str().unwrap()],
        )
        .unwrap();
        assert_eq!(read_head_file(&wt), Some(Head::Branch("wt/side".into())));
        assert_eq!(
            current_head_sync(&wt).unwrap(),
            Head::Branch("wt/side".into())
        );
        run_git_cmd_sync(&dir, &["checkout", "--detach"]).unwrap();
        assert_eq!(read_head_file(&dir), None);
        let _ = std::fs::remove_dir_all(wt);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unborn_branch_is_current_and_listed() {
        let dir = std::env::temp_dir().join(format!("keel-branch-unborn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        run_git_cmd_sync(&dir, &["init", "-b", "trunk"]).unwrap();
        let list = list_branches_with_head_sync(&dir).unwrap();
        assert_eq!(list.current.as_deref(), Some("trunk"));
        assert_eq!(list.branches, vec!["trunk".to_string()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn detached_head_reports_a_short_sha_and_no_current() {
        let dir = repo_with_branches("detached");
        let sha = run_git_cmd_sync(&dir, &["rev-parse", "--short", "HEAD"]).unwrap();
        run_git_cmd_sync(&dir, &["checkout", "--detach"]).unwrap();
        let list = list_branches_with_head_sync(&dir).unwrap();
        assert_eq!(list.current, None);
        assert_eq!(list.detached.as_deref(), Some(sha.as_str()));
        assert_eq!(current_head_sync(&dir).unwrap(), Head::Detached(sha));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn checkout_switches_branch_on_a_clean_tree() {
        let dir = repo_with_branches("clean");
        // Untracked files don't block a switch.
        std::fs::write(dir.join("scratch.txt"), b"x").unwrap();
        checkout_branch_sync(&dir, "feat").unwrap();
        assert_eq!(
            current_head_sync(&dir).unwrap(),
            Head::Branch("feat".into())
        );
        assert!(
            checkout_branch_sync(&dir, "nope")
                .unwrap_err()
                .contains("not found")
        );
        assert!(checkout_branch_sync(&dir, "--force").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn checkout_creates_a_tracking_branch_from_origin() {
        let origin = repo_with_branches("origin");
        run_git_cmd_sync(&origin, &["branch", "remote-only"]).unwrap();
        let clone =
            std::env::temp_dir().join(format!("keel-branch-clone-{}", uuid::Uuid::new_v4()));
        run_git_cmd_sync(
            &std::env::temp_dir(),
            &[
                "clone",
                "-q",
                &origin.to_string_lossy(),
                &clone.to_string_lossy(),
            ],
        )
        .unwrap();
        checkout_branch_sync(&clone, "remote-only").unwrap();
        assert_eq!(
            current_head_sync(&clone).unwrap(),
            Head::Branch("remote-only".into())
        );
        let upstream = run_git_cmd_sync(
            &clone,
            &["rev-parse", "--abbrev-ref", "remote-only@{upstream}"],
        )
        .unwrap();
        assert_eq!(upstream, "origin/remote-only");
        let _ = std::fs::remove_dir_all(origin);
        let _ = std::fs::remove_dir_all(clone);
    }

    #[test]
    fn checkout_refuses_on_a_dirty_tree() {
        let dir = repo_with_branches("dirty");
        std::fs::write(dir.join("a.txt"), b"changed\n").unwrap();
        let err = checkout_branch_sync(&dir, "feat").unwrap_err();
        assert!(err.starts_with("Uncommitted changes in "), "{err}");
        assert!(err.contains("worktree"), "{err}");
        assert_eq!(
            current_head_sync(&dir).unwrap(),
            Head::Branch("main".into())
        );
        // Staged changes count too.
        run_git_cmd_sync(&dir, &["add", "a.txt"]).unwrap();
        assert!(checkout_branch_sync(&dir, "feat").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod watch_filter_tests {
    use super::relevant_change;
    use std::path::Path;

    #[test]
    fn dependency_build_and_git_internals_are_ignored() {
        assert!(relevant_change(Path::new("src/main.rs")));
        assert!(relevant_change(Path::new("README.md")));
        assert!(relevant_change(Path::new(".git/HEAD")));
        assert!(relevant_change(Path::new(".git/refs/heads/main")));
        assert!(relevant_change(Path::new(".git/packed-refs")));
        assert!(!relevant_change(Path::new(".git/objects/ab/cdef")));
        assert!(!relevant_change(Path::new(".git/index")));
        assert!(!relevant_change(Path::new(".git/index.lock")));
        assert!(!relevant_change(Path::new("node_modules/react/index.js")));
        assert!(!relevant_change(Path::new("web/node_modules/x/y.js")));
        assert!(!relevant_change(Path::new("target/debug/keel")));
        assert!(!relevant_change(Path::new("dist/app.js")));
        assert!(!relevant_change(Path::new("src/.DS_Store")));
    }
}

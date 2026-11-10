//! Pi RPC driver for the Keel harness. Spawn the bundled `pi --mode rpc`.
//! Do not call `set_steering_mode` (it persists into Pi's settings). Stdout records split on LF only; U+2028 and
//! U+2029 stay inside a record.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio as StdStdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::stream;
use keel_proto::{
    AgentEvent, HarnessId, PermissionMode, QuestionKind, QuestionOption, TokenUsage, TurnStatus,
    UserInputAnswer, UserInputQuestion,
};
use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin};
use tokio::sync::mpsc;

use crate::keel::PiProfile;
use crate::proc::{
    Io, KILL_WAIT, PROBE_LIMIT, Proc, TERM_WAIT, completed, errored, failed, interrupted, is_exec,
    login_dirs, pipe_lines, push_tail, signal_group, summary_line, tail_message,
};
#[cfg(test)]
use crate::proc::{compose_path, redact};
use crate::{
    ControlMsg, Harness, HarnessError, Model, ModelId, ProgressGate, ReasoningLevel, RunControls,
    SessionReport, SessionSpec,
};

const INSTALL_HINT: &str = "Keel's bundled Pi is missing; run scripts/fetch-pi.sh";
const HOMEBREW_PI: &str = "/opt/homebrew/bin/pi";
const SWITCH_WAIT: Duration = Duration::from_secs(15);

struct VersionHit {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
    version: String,
    managed: bool,
}

fn version_cache() -> &'static Mutex<Vec<VersionHit>> {
    static CACHE: Mutex<Vec<VersionHit>> = Mutex::new(Vec::new());
    &CACHE
}

struct ResolveInput {
    executable_env: Option<PathBuf>,
    /// Folders that may hold the bundled `pi` binary, in order.
    bundle_dirs: Vec<PathBuf>,
    engine_path: String,
    login_dirs: Vec<PathBuf>,
}

enum Stop {
    None,
    Term { deadline: Instant },
    Kill { deadline: Instant },
}

struct PiResponse {
    command: String,
    success: bool,
    disposition: Option<String>,
    error: String,
}

enum Incoming {
    Response(PiResponse),
    Settle,
    Events(Vec<AgentEvent>),
    Ignore,
}

/// The bundled Pi. `installed` is a filesystem and version probe.
pub struct PiHarness {
    profile: PiProfile,
    executable: Option<PathBuf>,
    version: String,
    searched: PathBuf,
    engine_path: String,
    login_dirs: Vec<PathBuf>,
    /// Thinking levels per model id, filled by `models()`.
    levels: Mutex<HashMap<String, Vec<String>>>,
    pool: Arc<PiProcessPool>,
}

impl PiHarness {
    /// Keel's own harness (see `keel.rs`), resolved at engine start: the
    /// Pi bundled beside this binary, or `PI_EXECUTABLE`. The login-shell
    /// PATH is snapshotted once, for the tools Pi runs.
    pub fn keel() -> Self {
        let engine_path = std::env::var("PATH").unwrap_or_default();
        let executable_env = std::env::var_os("PI_EXECUTABLE")
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty());
        let harness = Self::assemble_as(
            ResolveInput {
                executable_env,
                bundle_dirs: bundle_dirs(),
                engine_path,
                login_dirs: login_dirs(),
            },
            PiProfile::keel(),
        );
        if let (Some(exe), Some(agent)) = (&harness.executable, crate::pi_agent_dir()) {
            crate::keel::register_bundled_packages(exe, agent);
        }
        harness
    }

    #[cfg(test)]
    fn assemble(input: ResolveInput) -> Self {
        Self::assemble_as(input, PiProfile::keel())
    }

    fn assemble_as(input: ResolveInput, profile: PiProfile) -> Self {
        let candidates = candidate_paths(&input);
        let searched = candidates
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("pi/pi"));
        let mut rejected = String::new();
        let mut chosen = None;
        for path in &candidates {
            if !is_exec(path) {
                continue;
            }
            let (version, managed) = cached_version(path);
            if managed {
                chosen = Some((path.clone(), version));
                break;
            }
            if rejected.is_empty() && !version.is_empty() {
                rejected = version;
            }
        }
        let (executable, version) = match chosen {
            Some((path, version)) => (Some(path), version),
            None => (None, rejected),
        };
        Self {
            profile,
            executable,
            version,
            searched,
            engine_path: input.engine_path,
            login_dirs: input.login_dirs,
            levels: Mutex::new(HashMap::new()),
            pool: Arc::new(PiProcessPool::new()),
        }
    }

    /// A Pi process. Keel's own also gets the Antigravity bridge's env.
    /// `permission` is the run's mode, `None` for probes and sign-in.
    /// `plan_mode` forces that env to ask (`default`, permissions not skipped)
    /// so a mutating Antigravity call still hits the permission deny.
    fn command(
        &self,
        exe: &Path,
        permission: Option<PermissionMode>,
        plan_mode: bool,
    ) -> std::process::Command {
        let mut cmd = crate::proc::command_with(exe, &self.engine_path, &self.login_dirs);
        if self.profile.keel.is_some() {
            cmd.envs(crate::keel::antigravity_env(
                crate::keel::antigravity_acp_bin().as_deref(),
                permission,
                plan_mode,
            ));
        }
        cmd
    }

    fn missing(&self) -> HarnessError {
        HarnessError::NotInstalled {
            path: self.searched.clone(),
            hint: INSTALL_HINT.to_string(),
        }
    }

    /// Short-lived `--no-session --offline` process. Stdin stays open until
    /// every command has its response (Pi exits on EOF without answering).
    async fn probe(&self, model: &str, commands: Vec<Value>) -> Result<Vec<Value>, HarnessError> {
        let exe = self.executable.clone().ok_or_else(|| self.missing())?;
        let mut cmd = self.command(&exe, None, false);
        rpc_args(&mut cmd, model, &std::env::temp_dir(), true);
        let mut proc = Proc::spawn(cmd, "pi")?;
        let mut waiting: HashSet<String> = commands
            .iter()
            .filter_map(|command| {
                command
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        for command in &commands {
            proc.write(command).await.map_err(failed)?;
        }
        let (_keep, mut none) = mpsc::unbounded_channel::<ControlMsg>();
        let read = async {
            let mut lines = Vec::new();
            while !waiting.is_empty() {
                match proc.next(&mut none).await {
                    Io::Line(line) => {
                        let Ok(value) = serde_json::from_str::<Value>(&line) else {
                            continue;
                        };
                        if value.get("type").and_then(Value::as_str) == Some("response")
                            && let Some(id) = value.get("id").and_then(Value::as_str)
                        {
                            waiting.remove(id);
                        }
                        lines.push(value);
                    }
                    Io::Closed | Io::Killed => {
                        return Err(HarnessError::Failed(proc.crash()));
                    }
                    Io::Control => {}
                }
            }
            Ok(lines)
        };
        let out = match tokio::time::timeout(PROBE_LIMIT, read).await {
            Ok(lines) => lines,
            Err(_) => Err(HarnessError::Failed("pi probe timed out".to_string())),
        };
        proc.finish();
        out
    }
}

#[async_trait]
impl Harness for PiHarness {
    fn id(&self) -> HarnessId {
        self.profile.id
    }

    fn display_name(&self) -> &str {
        self.profile.display_name
    }

    fn permission_modes(&self) -> &'static [PermissionMode] {
        &PermissionMode::ALL
    }

    fn installed(&self) -> bool {
        self.executable.is_some()
    }

    fn executable(&self) -> Option<PathBuf> {
        self.executable.clone()
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn deterministic_turn_end(&self) -> bool {
        true
    }

    fn evict_chat(&self, chat_id: &str) {
        self.pool.evict_chat(chat_id);
    }

    fn evict_all(&self) {
        self.pool.evict_all();
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        let lines = self
            .probe(
                "",
                vec![serde_json::json!({"id": "m", "type": "get_available_models"})],
            )
            .await?;
        let Some(value) = lines.into_iter().find(|line| {
            line.get("command").and_then(|cmd| cmd.as_str()) == Some("get_available_models")
        }) else {
            return Err(HarnessError::Failed(
                "pi did not answer get_available_models".to_string(),
            ));
        };
        if value.get("success").and_then(|flag| flag.as_bool()) == Some(false) {
            return Err(HarnessError::Failed(error_text(&value)));
        }
        let Some(models) = value["data"]["models"].as_array() else {
            return Ok(Vec::new());
        };
        let mut levels = self.levels.lock().unwrap_or_else(|err| err.into_inner());
        Ok(models
            .iter()
            .filter_map(|model| {
                let bare = model.get("id").and_then(|id| id.as_str())?;
                let name = model
                    .get("name")
                    .and_then(|name| name.as_str())
                    .unwrap_or(bare);
                // `--model provider/id` is unambiguous when two providers share an id.
                let id = match model.get("provider").and_then(Value::as_str) {
                    Some(provider) if !provider.is_empty() => format!("{provider}/{bare}"),
                    _ => bare.to_string(),
                };
                if let Some(known) = thinking_levels(model) {
                    levels.insert(id.clone(), known);
                }
                Some(Model {
                    id: ModelId(id),
                    name: name.to_string(),
                })
            })
            .collect())
    }

    async fn reasoning_levels(&self, model: &ModelId) -> Result<Vec<ReasoningLevel>, HarnessError> {
        let cached = self
            .levels
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(&model.0)
            .cloned();
        if let Some(levels) = cached {
            return Ok(levels.into_iter().map(ReasoningLevel).collect());
        }
        let lines = self
            .probe(
                &model.0,
                vec![serde_json::json!({"id": "t", "type": "get_available_thinking_levels"})],
            )
            .await?;
        let Some(value) = lines.into_iter().find(|line| {
            line.get("command").and_then(|cmd| cmd.as_str())
                == Some("get_available_thinking_levels")
        }) else {
            return Err(HarnessError::Failed(
                "pi did not answer get_available_thinking_levels".to_string(),
            ));
        };
        if value.get("success").and_then(|flag| flag.as_bool()) == Some(false) {
            return Err(HarnessError::Failed(error_text(&value)));
        }
        let Some(levels) = value["data"]["levels"].as_array() else {
            return Ok(Vec::new());
        };
        Ok(levels
            .iter()
            .filter_map(|level| {
                level
                    .as_str()
                    .map(|level| ReasoningLevel(level.to_string()))
            })
            .collect())
    }

    fn description(&self) -> &'static str {
        self.profile.description
    }

    /// The sign-ins Keel offers, then any other provider in Keel's Pi
    /// `auth.json`. Reads only the keys and credential types, no secrets.
    async fn auth_status(&self) -> Vec<crate::AuthAccount> {
        let stored: serde_json::Map<String, Value> = crate::pi_agent_dir()
            .and_then(|dir| fs::read_to_string(dir.join("auth.json")).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let mut rows = auth_rows(&stored);
        // Antigravity keeps its own sign-in, and is offered only with its server.
        if self.profile.keel.is_some() && crate::keel::antigravity_acp_bin().is_some() {
            if let Some(paths) = crate::antigravity::Paths::of_home() {
                with_antigravity(&mut rows, crate::antigravity::account(&paths));
            }
        }
        rows
    }

    /// `pi --mode rpc` with Keel's login extension, sent `/keel-login`.
    /// The extension's lines arrive on stderr: in RPC mode Pi moves what
    /// extensions write to stdout over there, to keep its protocol clean.
    /// Stdin stays open until the extension reports done or an error (Pi
    /// exits on EOF); dropping this future kills the process.
    async fn sign_in(
        &self,
        provider: &str,
        on: &(dyn Fn(crate::SignInEvent) + Send + Sync),
    ) -> Result<(), HarnessError> {
        if provider.is_empty()
            || !provider
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(HarnessError::Failed(format!(
                "unknown provider {provider:?}"
            )));
        }
        if provider == crate::antigravity::PROVIDER && self.profile.keel.is_some() {
            let bin = crate::keel::antigravity_acp_bin().ok_or_else(|| {
                HarnessError::Failed("Antigravity's server is not installed".into())
            })?;
            let paths = crate::antigravity::Paths::of_home()
                .ok_or_else(|| HarnessError::Failed("no home folder".into()))?;
            let scratch = crate::pi_agent_dir()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .unwrap_or_else(std::env::temp_dir);
            return crate::antigravity::sign_in(&bin, &paths, &scratch, on).await;
        }
        let exe = self.executable.clone().ok_or_else(|| self.missing())?;
        let extension = crate::keel::login_extension().map_err(failed)?;
        let mut std_cmd = self.command(&exe, None, false);
        std_cmd
            .args(["--mode", "rpc", "--no-session", "--extension"])
            .arg(extension)
            .current_dir(std::env::temp_dir());
        let mut cmd = tokio::process::Command::from(std_cmd);
        cmd.stdin(StdStdio::piped())
            .stdout(StdStdio::null())
            .stderr(StdStdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(failed)?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Failed("pi has no stdin".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| HarnessError::Failed("pi has no stderr".into()))?;
        let device = crate::pi_agent_dir()
            .map(crate::keel::device_id)
            .unwrap_or_default();
        let message = format!("/keel-login {provider} {device}");
        write_json(
            &mut stdin,
            &serde_json::json!({"id": "login", "type": "prompt", "message": message.trim_end()}),
        )
        .await
        .map_err(failed)?;
        let read = async {
            let mut lines = tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(stderr));
            while let Some(line) = lines.next_line().await.map_err(failed)? {
                match sign_in_line(&line) {
                    Some(SignInLine::Event(event)) => on(event),
                    Some(SignInLine::Done) => return Ok(()),
                    Some(SignInLine::Failed(why)) => return Err(HarnessError::Failed(why)),
                    None => {}
                }
            }
            Err(HarnessError::Failed(
                "pi exited before the sign-in finished".into(),
            ))
        };
        let out = tokio::time::timeout(SIGN_IN_LIMIT, read)
            .await
            .unwrap_or_else(|_| Err(HarnessError::Failed("sign-in timed out".into())));
        drop(stdin);
        let _ = child.kill().await;
        out
    }

    fn one_shot_args(&self, model: &ModelId, prompt: &str) -> Option<Vec<String>> {
        let mut args: Vec<String> = [
            "-p",
            "--no-session",
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--no-mcp",
            "--no-context-files",
            "--no-prompt-templates",
            "--thinking",
            "off",
        ]
        .map(String::from)
        .to_vec();
        args.extend(crate::model_flag("--model", model));
        args.push(prompt.to_string());
        Some(args)
    }

    async fn run(
        &self,
        spec: SessionSpec,
        controls: RunControls,
    ) -> Result<
        Pin<Box<dyn futures::Stream<Item = Result<AgentEvent, HarnessError>> + Send>>,
        HarnessError,
    > {
        let exe = self.executable.clone().ok_or_else(|| self.missing())?;
        let chat_id = spec
            .chat_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let project_fingerprint = spec
            .project
            .as_ref()
            .map(|p| {
                let mut s = p.name.clone();
                for (a, r) in &p.roots {
                    s.push(':');
                    s.push_str(a);
                    s.push('=');
                    s.push_str(r);
                }
                s
            })
            .unwrap_or_default();
        let target_config = ProcessConfig {
            cwd: spec.cwd.clone(),
            project_fingerprint,
            plan_mode: spec.plan_mode,
            permission: spec.permission,
            longer_cache: spec.longer_cache,
            keep_cache_warm: spec.keep_cache_warm,
            exe_path: exe.clone(),
            tag: self.profile.tag,
        };

        let mut reuse_tx = None;
        {
            let mut entries = self.pool.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = entries.get_mut(&chat_id) {
                if !entry.is_busy && !entry.actor_tx.is_closed() && entry.config == target_config {
                    entry.is_busy = true;
                    reuse_tx = Some(entry.actor_tx.clone());
                } else {
                    let old = entries.remove(&chat_id).unwrap();
                    let _ = old.actor_tx.send(ActorCommand::Evict);
                }
            }
        }

        if reuse_tx.is_none() {
            // Cap concurrent idle processes to 3 (LRU eviction)
            {
                let mut entries = self.pool.entries.lock().unwrap_or_else(|e| e.into_inner());
                let idle_keys: Vec<String> = entries
                    .iter()
                    .filter(|(_, e)| !e.is_busy && !e.actor_tx.is_closed())
                    .map(|(k, _)| k.clone())
                    .collect();
                if idle_keys.len() >= 3 {
                    let oldest_key = idle_keys.into_iter().min_by_key(|k| {
                        entries
                            .get(k)
                            .map(|e| e.last_active)
                            .unwrap_or_else(Instant::now)
                    });
                    if let Some(k) = oldest_key {
                        if let Some(old) = entries.remove(&k) {
                            tracing::info!(target: "keel_harness::pi", chat = %k, "LRU evicting oldest idle warm process");
                            let _ = old.actor_tx.send(ActorCommand::Evict);
                        }
                    }
                }
            }

            let mut std_cmd = self.command(&exe, Some(spec.permission), spec.plan_mode);
            rpc_args(&mut std_cmd, &spec.model.0, &spec.cwd, false);
            if spec.longer_cache {
                std_cmd.env("PI_CACHE_RETENTION", "long");
            }
            let tag = self.profile.tag;
            let data_dir = spec.data_dir.as_deref();
            if let Some(keel) = &self.profile.keel {
                // Before `run_args`, which loads the extensions folder it lands in.
                labels_extension(data_dir);
                memory_extension(data_dir);
                let idle_timeout_secs =
                    spec.idle_eviction_timeout_secs.unwrap_or(15 * 60) as u64;
                cache_warming_extension(data_dir, idle_timeout_secs);
                std_cmd.args(keel.run_args(data_dir));
            }
            if let Some(ext) = ui_extension(data_dir, tag) {
                std_cmd.arg("--extension").arg(ext);
            }
            if let Some(ext) = browser_extension(
                spec.browser_sock.as_deref(),
                spec.browser_chat.as_deref(),
                tag,
            ) {
                std_cmd.arg("--extension").arg(ext);
            }
            if let Some(ext) = project_extension(
                spec.project.as_ref(),
                data_dir,
                spec.browser_chat.as_deref(),
                tag,
            ) {
                std_cmd.arg("--extension").arg(ext);
            }
            // Fail closed: without its gate Pi would run every tool unasked.
            let gate = if spec.plan_mode {
                permission_extension_for(spec.permission, data_dir, tag, true)
            } else {
                permission_extension(spec.permission, data_dir, tag)
            };
            if let Some(gate) = gate.transpose()? {
                std_cmd.arg("--extension").arg(gate);
            }
            if let Some(ext) = plan_prompt_extension(spec.plan_mode, data_dir, tag) {
                std_cmd.arg("--extension").arg(ext);
            }
            if let Some(agent_dir) = crate::keel::pi_agent_dir() {
                crate::keel::update_cache_warming_setting(agent_dir, spec.keep_cache_warm);
            }
            let mut cmd = tokio::process::Command::from(std_cmd);
            cmd.stdin(StdStdio::piped())
                .stdout(StdStdio::piped())
                .stderr(StdStdio::piped())
                .kill_on_drop(true);
            #[cfg(unix)]
            cmd.process_group(0);
            let mut child = cmd.spawn().map_err(failed)?;
            let pid = child.id().unwrap_or(0);
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| HarnessError::Failed("pi stdin missing".to_string()))?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| HarnessError::Failed("pi stdout missing".to_string()))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| HarnessError::Failed("pi stderr missing".to_string()))?;
            let (actor_tx, actor_rx) = mpsc::unbounded_channel();
            let idle_timeout = Duration::from_secs(
                spec.idle_eviction_timeout_secs.unwrap_or(15 * 60) as u64,
            );
            let actor = ProcessActor {
                chat_id: chat_id.clone(),
                child,
                pid,
                _kill: GroupKill { pid },
                stdin,
                stdout: pipe_lines(stdout),
                stderr: pipe_lines(stderr),
                current_model: spec.model.0.clone(),
                current_reasoning: String::new(),
                current_session_path: None,
                config: target_config.clone(),
                last_active: Instant::now(),
                last_report_usage: spec.report_usage.clone(),
                idle_timeout,
                rx: actor_rx,
                seq: 0,
            };
            let pool_clone = Arc::clone(&self.pool);
            tokio::spawn(async move {
                actor.run_loop(pool_clone).await;
            });
            {
                let mut entries = self.pool.entries.lock().unwrap_or_else(|e| e.into_inner());
                entries.insert(
                    chat_id.clone(),
                    PoolEntry {
                        actor_tx: actor_tx.clone(),
                        config: target_config,
                        last_active: Instant::now(),
                        is_busy: true,
                    },
                );
            }
            reuse_tx = Some(actor_tx);
        }

        let actor_tx = reuse_tx.unwrap();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let pool_for_done = Arc::clone(&self.pool);
        let chat_id_for_done = chat_id.clone();
        tokio::spawn(async move {
            let _ = done_rx.await;
            let mut entries = pool_for_done.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = entries.get_mut(&chat_id_for_done) {
                entry.is_busy = false;
                entry.last_active = Instant::now();
            }
        });

        if let Err(_) = actor_tx.send(ActorCommand::StartTurn {
            spec,
            controls,
            event_tx,
            done_tx,
        }) {
            return Err(HarnessError::Failed("warm process actor died".to_string()));
        }

        Ok(Box::pin(stream::unfold(event_rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })))
    }
}

/// Pi has built-in MCP but reads servers only from `~/.pi/agent/mcp.json` and
/// trusted `.pi/mcp.json`; there is no per-run flag. A per-run extension
/// (`--extension <file>`) that calls `pi.registerMcpServer` adds Keel's browser
/// server for this session only, so nothing persists into `~/.pi`. Exposure is
/// `direct` so the tools are declared like built-ins instead of hiding behind
/// codemode. Pi lists a server's description in the prompt only for codemode
/// and deferred servers, so a direct one carries none. Older Pi without
/// `registerMcpServer` loads it as a no-op.
fn mcp_register(name: &str, mcp: &crate::BrowserMcp) -> String {
    let lit = |value: &str| serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string());
    let args = serde_json::to_string(&mcp.args).unwrap_or_else(|_| "[]".to_string());
    let env: serde_json::Map<String, serde_json::Value> = mcp
        .env
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let env = serde_json::to_string(&env).unwrap_or_else(|_| "{}".to_string());
    format!(
        "  pi.registerMcpServer({name}, {{\n    command: {command},\n    args: {args},\n    env: {env},\n    exposure: \"direct\",\n  }});\n",
        name = lit(name),
        command = lit(&mcp.command),
    )
}

fn browser_extension_source(browser: &crate::BrowserMcp) -> String {
    format!(
        "// Generated by Keel for each Pi run. Edits are overwritten.\n\
export default function (pi: any) {{\n\
  if (typeof pi.registerMcpServer !== \"function\") return;\n\
{register}}}\n",
        register = mcp_register(crate::BROWSER_MCP_NAME, browser),
    )
}

/// Writes the chat's browser extension into a private folder beside
/// `browser.sock` and returns its path. It carries the socket token, so it
/// is 0600. `None` when there is no socket or the file cannot be written;
/// the run then goes ahead without browser tools.
fn browser_extension(sock: Option<&Path>, chat: Option<&str>, tag: &str) -> Option<PathBuf> {
    let browser = crate::browser_mcp(sock, chat)?;
    let path = sock?
        .with_file_name("browser-mcp")
        .join(format!("{tag}-{}.ts", crate::chat_file_stem(chat)));
    match crate::write_private(&path, &browser_extension_source(&browser)) {
        Ok(()) => Some(path),
        Err(err) => {
            tracing::warn!(target: "keel_harness::pi", error = %err, path = %path.display(), "browser extension write failed");
            None
        }
    }
}

/// Keel's chat cards: registers the `keel-ui` server and adds the
/// `keel_cards` prompt section on every turn (Pi rebuilds the sections each
/// turn and records an unset one as a removal). Not gated on the tools being
/// selected: Pi declares direct MCP tools after `before_agent_start`, once the
/// server connects, so they are never in `selectedTools` there. No token, so
/// one shared file, not one per chat.
fn ui_extension_source(ui: &crate::BrowserMcp) -> String {
    format!(
        "// Generated by Keel for each Pi run. Edits are overwritten.\n\
const CARDS = {cards};\n\
export default function (pi: any) {{\n\
  if (typeof pi.registerMcpServer !== \"function\") return;\n\
{register}\
  pi.on(\"before_agent_start\", (event: any) => {{\n\
    const sections = event.systemPromptOptions?.sections;\n\
    if (sections) sections.keel_cards = CARDS;\n\
  }});\n\
}}\n",
        cards =
            serde_json::to_string(crate::UI_CARDS_PROMPT).unwrap_or_else(|_| "\"\"".to_string()),
        register = mcp_register(crate::UI_MCP_NAME, ui),
    )
}

/// The cards extension's path in Keel's data dir. `None` without a data dir
/// or when Keel cannot name its own executable.
fn ui_extension(data_dir: Option<&Path>, tag: &str) -> Option<PathBuf> {
    let ui = crate::ui_mcp()?;
    let path = data_dir?.join(format!("keel-ui.{tag}.ts"));
    match write_extension(&path, &ui_extension_source(&ui)) {
        Ok(()) => Some(path),
        Err(err) => {
            tracing::warn!(target: "keel_harness::pi", error = %err, path = %path.display(), "cards extension write failed");
            None
        }
    }
}

/// The chat's project folders as the `keel_project` prompt section, set on
/// every turn like the cards. Pi has no allowed-dirs list (its tools take any
/// absolute path), so naming the roots is all it needs.
fn project_extension_source(project: &crate::ProjectRoots) -> String {
    let mut roots = project.roots.clone();
    if roots.len() > 1 {
        roots[1..].sort_by(|a, b| a.0.cmp(&b.0));
    }
    let text = keel_proto::projects::roots_prompt(&project.name, &roots);
    let trimmed = text.trim_end();
    format!(
        "// Generated by Keel for each Pi run. Edits are overwritten.\n\
const PROJECT = {text};\n\
export default function (pi: any) {{\n\
  pi.on(\"before_agent_start\", (event: any) => {{\n\
    const sections = event.systemPromptOptions?.sections;\n\
    if (sections && sections.keel_project !== PROJECT) sections.keel_project = PROJECT;\n\
  }});\n\
}}\n",
        text = serde_json::to_string(trimmed).unwrap_or_else(|_| "\"\"".to_string()),
    )
}

/// One file per chat under `{data_dir}/projects/`; `None` outside a
/// project or when it cannot be written (the run goes ahead without it).
fn project_extension(
    project: Option<&crate::ProjectRoots>,
    data_dir: Option<&Path>,
    chat: Option<&str>,
    tag: &str,
) -> Option<PathBuf> {
    let project = project.filter(|p| !p.roots.is_empty())?;
    let dir = data_dir?.join("projects");
    let path = dir.join(format!("{tag}-{}.ts", crate::chat_file_stem(chat)));
    let aliases: Vec<&str> = project.roots.iter().map(|(a, _)| a.as_str()).collect();
    match fs::create_dir_all(&dir)
        .and_then(|()| write_extension(&path, &project_extension_source(project)))
    {
        Ok(()) => {
            tracing::info!(target: "keel_harness::pi", project = %project.name, roots = ?aliases, path = %path.display(), "project roots extension");
            Some(path)
        }
        Err(err) => {
            tracing::warn!(target: "keel_harness::pi", error = %err, path = %path.display(), "project extension write failed");
            None
        }
    }
}

/// The fixed top of the `keel_memory` prompt section. The engine's
/// `memory_prompt` builds the rest; the extension falls back to this plus
/// [`MEMORY_UNAVAILABLE`] when the engine cannot be reached.
pub const MEMORY_PREAMBLE: &str =
    "Keel memory is notes you write for a later session. It is not a transcript.

Save a correction, a preference, or a decision that is not in the repo and not in AGENTS.md.
Do not save file layout, architecture, or commands you could read from the code.
Types: user, feedback, project, reference.
User notes apply to every project. Project notes apply to this checkout.
Read a topic with memory_read before you rely on it. Write with memory_write.
If a write reports the index is over the cap, merge or archive lines until it fits.";

pub const MEMORY_UNAVAILABLE: &str = "Memory is unavailable this turn.";

/// Must equal `keel_rpc::PROTO_VERSION` (an engine test checks it; the
/// harness does not depend on keel-rpc).
pub const MEMORY_PROTO: u32 = 1;

/// Keel memory as a Pi extension: injects `sections.keel_memory` on every
/// turn and registers `memory_read` / `memory_write`. Both call the engine
/// over `keel.sock` (4-byte big-endian length, one JSON value; `hello` with
/// the token first). The file only talks to the socket; the engine owns the
/// notes. It carries the engine token, so it is written 0600.
fn memory_extension_source(sock: &Path, token: &str) -> String {
    let lit = |value: &str| serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"// Generated by Keel for each Pi run. Edits are overwritten.
import * as net from "node:net";
import {{ Type }} from "typebox";

const SOCK = {sock};
const TOKEN = {token};
const PROTO = {proto};
const PREAMBLE = {preamble};
const UNAVAILABLE = {unavailable};

function frame(value: unknown): Buffer {{
  const body = Buffer.from(JSON.stringify(value), "utf8");
  const head = Buffer.alloc(4);
  head.writeUInt32BE(body.length, 0);
  return Buffer.concat([head, body]);
}}

// One connection per call: hello, then the method, then close.
function call(method: string, params: Record<string, unknown>): Promise<any> {{
  return new Promise((resolve, reject) => {{
    let settled = false;
    let buf = Buffer.alloc(0);
    const sock = net.createConnection(SOCK);
    const finish = (err: Error | null, value?: any) => {{
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      sock.destroy();
      if (err) reject(err);
      else resolve(value);
    }};
    const timer = setTimeout(() => finish(new Error("Keel did not answer in time")), 10000);
    sock.on("connect", () => sock.write(frame({{ id: "h", method: "hello", params: {{ token: TOKEN, proto: PROTO }} }})));
    sock.on("data", (chunk: Buffer) => {{
      buf = Buffer.concat([buf, chunk]);
      while (buf.length >= 4) {{
        const len = buf.readUInt32BE(0);
        if (buf.length < 4 + len) return;
        let msg: any;
        try {{
          msg = JSON.parse(buf.subarray(4, 4 + len).toString("utf8"));
        }} catch {{
          return finish(new Error("Keel sent a bad reply"));
        }}
        buf = buf.subarray(4 + len);
        if (msg?.id === "h") {{
          if (msg.error) return finish(new Error(msg.error.message ?? "Keel refused the connection"));
          sock.write(frame({{ id: "m", method, params }}));
        }} else if (msg?.id === "m") {{
          if (msg.error) return finish(new Error(msg.error.message ?? "Keel refused the call"));
          return finish(null, msg.result);
        }}
      }}
    }});
    sock.on("error", (err: Error) => finish(err));
    sock.on("close", () => finish(new Error("Keel closed the connection")));
  }});
}}

const text = (value: string) => ({{ content: [{{ type: "text", text: value }}], details: undefined }});
const failed = (err: unknown) => text(`Memory error: ${{err instanceof Error ? err.message : String(err)}}`);
const SCOPE = Type.Union([Type.Literal("user"), Type.Literal("project")], {{ description: "user: every project. project: this checkout." }});

export default function (pi: any) {{
  let cwd: string | undefined;
  let lastMemory: string | undefined;
  pi.on("before_agent_start", async (event: any, ctx: any) => {{
    const sections = event.systemPromptOptions?.sections;
    cwd = event.systemPromptOptions?.cwd ?? ctx?.cwd ?? cwd;
    if (!sections) return;
    try {{
      const result = await call("memory_prompt", {{ cwd }});
      if (result && typeof result.text === "string") {{
        lastMemory = String(result.text).trimEnd();
      }}
    }} catch {{
      // Transient failure: keep last successful memory text if available
    }}
    const current = (lastMemory ?? `${{PREAMBLE}}\n\n${{UNAVAILABLE}}`).trimEnd();
    if (sections.keel_memory !== current) {{
      sections.keel_memory = current;
    }}
  }});

  pi.registerTool({{
    name: "memory_read",
    label: "Read memory",
    description: "Read Keel's notes. Without a slug, returns the whole index and the topic slugs. With a slug, returns that topic's type, description and body.",
    promptSnippet: "Read and update Keel's notes for later sessions.",
    promptGuidelines: [
      "Use memory_write for a correction, preference, or decision that a later session cannot get from the repo or from AGENTS.md.",
      "Use memory_read before relying on a topic. The index line is only a pointer.",
      "Do not use memory_write for task progress in the current chat.",
    ],
    annotations: {{ readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false }},
    parameters: Type.Object({{
      label: Type.String({{ description: "Required. Short progress label: 3-6 words, present participle, sentence case, name the concept. Match the user's language." }}),
      scope: SCOPE,
      slug: Type.Optional(Type.String({{ description: "Topic slug. Omit for the index." }})),
    }}),
    prepareArguments(args: any) {{
      const next = {{ ...(args ?? {{}}) }};
      if (typeof next.label !== "string" || !String(next.label).trim()) next.label = "Working";
      return next;
    }},
    async execute(_id: string, params: any, _signal: any, _onUpdate: any, ctx: any) {{
      try {{
        const {{ label: _label, ...rest }} = params ?? {{}};
        const result = await call("memory_read", {{ scope: rest.scope, slug: rest.slug, cwd: ctx?.cwd ?? cwd }});
        return text(String(result?.text ?? ""));
      }} catch (err) {{
        return failed(err);
      }}
    }},
  }});

  pi.registerTool({{
    name: "memory_write",
    label: "Write memory",
    description: "Save or retire one of Keel's notes. upsert writes the topic and its index line; archive retires the topic. type and description are required for upsert.",
    annotations: {{ readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false }},
    parameters: Type.Object({{
      label: Type.String({{ description: "Required. Short progress label: 3-6 words, present participle, sentence case, name the concept. Match the user's language." }}),
      scope: SCOPE,
      action: Type.Union([Type.Literal("upsert"), Type.Literal("archive")]),
      slug: Type.String({{ description: "Lowercase letters, digits and dashes, up to 64 characters." }}),
      type: Type.Optional(Type.Union([Type.Literal("user"), Type.Literal("feedback"), Type.Literal("project"), Type.Literal("reference")])),
      description: Type.Optional(Type.String({{ description: "One line, at most 200 characters. Shown in the index." }})),
      body: Type.Optional(Type.String({{ description: "Markdown, at most 32 KiB." }})),
    }}),
    prepareArguments(args: any) {{
      const next = {{ ...(args ?? {{}}) }};
      if (typeof next.label !== "string" || !String(next.label).trim()) next.label = "Working";
      return next;
    }},
    async execute(_id: string, params: any, _signal: any, _onUpdate: any, ctx: any) {{
      try {{
        const {{ label: _label, ...rest }} = params ?? {{}};
        const result = await call("memory_write", {{ ...rest, cwd: ctx?.cwd ?? cwd }});
        return text(String(result?.text ?? "Saved."));
      }} catch (err) {{
        return failed(err);
      }}
    }},
  }});
}}
"#,
        sock = lit(&sock.to_string_lossy()),
        token = lit(token),
        proto = MEMORY_PROTO,
        preamble = lit(MEMORY_PREAMBLE),
        unavailable = lit(MEMORY_UNAVAILABLE),
    )
}

/// Writes `{data_dir}/harnesses/keel/extensions/keel-memory.ts` (0600) when
/// the engine's token exists; `folder_extensions` then loads it. Rewritten
/// only when the text changes. Without a token the run goes ahead without
/// memory.
fn memory_extension(data_dir: Option<&Path>) -> Option<PathBuf> {
    let data_dir = data_dir?;
    let token = fs::read_to_string(data_dir.join("ipc.token")).ok()?;
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let path = crate::KeelProfile::extensions_dir(data_dir).join("keel-memory.ts");
    let source = memory_extension_source(&data_dir.join("keel.sock"), token);
    if fs::read_to_string(&path).ok().as_deref() == Some(source.as_str()) {
        return Some(path);
    }
    match crate::write_private(&path, &source) {
        Ok(()) => Some(path),
        Err(err) => {
            tracing::warn!(target: "keel_harness::pi", error = %err, path = %path.display(), "memory extension write failed");
            None
        }
    }
}

/// Writes `{data_dir}/harnesses/keel/extensions/keel-labels.ts`. Wraps the
/// seven built-in tools so each requires a `label`, stripped in `execute`
/// after schema validation. `folder_extensions` loads it with the rest.
fn labels_extension(data_dir: Option<&Path>) -> Option<PathBuf> {
    let data_dir = data_dir?;
    let path = crate::KeelProfile::extensions_dir(data_dir).join("keel-labels.ts");
    let source = LABELS_EXTENSION;
    if fs::read_to_string(&path).ok().as_deref() == Some(source) {
        return Some(path);
    }
    match crate::write_private(&path, source) {
        Ok(()) => Some(path),
        Err(err) => {
            tracing::warn!(target: "keel_harness::pi", error = %err, path = %path.display(), "labels extension write failed");
            None
        }
    }
}

/// Writes `{data_dir}/harnesses/keel/extensions/keel-cache.ts`. Implements
/// `cache_warming_decision` to halt cache warming once the session has been
/// idle longer than the eviction limit.
fn cache_warming_extension(data_dir: Option<&Path>, idle_timeout_secs: u64) -> Option<PathBuf> {
    let data_dir = data_dir?;
    let path = crate::KeelProfile::extensions_dir(data_dir).join("keel-cache.ts");
    let source = format!(
        "// Generated by Keel for prompt cache warming management. Edits are overwritten.\n\
export default function (pi: any) {{\n\
  let lastActive = Date.now();\n\
  const maxIdleMs = {max_idle_ms};\n\
\n\
  pi.on(\"turn_end\", () => {{\n\
    lastActive = Date.now();\n\
  }});\n\
\n\
  pi.on(\"cache_warming_decision\", (_event: any, _ctx: any) => {{\n\
    const idleMs = Date.now() - lastActive;\n\
    if (idleMs > maxIdleMs) {{\n\
      return {{ action: \"stop\" }};\n\
    }}\n\
  }});\n\
}}\n",
        max_idle_ms = idle_timeout_secs * 1000
    );
    if fs::read_to_string(&path).ok().as_deref() == Some(source.as_str()) {
        return Some(path);
    }
    match crate::write_private(&path, &source) {
        Ok(()) => Some(path),
        Err(err) => {
            tracing::warn!(target: "keel_harness::pi", error = %err, path = %path.display(), "cache warming extension write failed");
            None
        }
    }
}

/// Pi extension: require `label` on every built-in and extension tool schema.
/// Keep it through validation (so resumes without a label can synthesize one in
/// `prepareArguments`), then strip in `execute`. Re-register on
/// `before_agent_start` and `before_provider_request` so later package shadows
/// and dynamic tools cannot drop the field.
const LABELS_EXTENSION: &str = r#"// Generated by Keel. Edits are overwritten.
import { Type } from "typebox";
import {
  createBashToolDefinition,
  createEditToolDefinition,
  createFindToolDefinition,
  createGrepToolDefinition,
  createLsToolDefinition,
  createReadToolDefinition,
  createWriteToolDefinition,
} from "@earendil-works/pi-coding-agent";

const LABEL_DESC =
  'Required. Short progress label for the UI: 3-6 words, present participle, sentence case, naming the concept not the file or command. Match the user\'s language. Good: "Reading the on/off cycle logic", "Updating session state transition", "Compiling workspace binaries", "Locating theme token definitions" (Indonesian: "Nge-cek konfigurasi database", "Benerin bug memory leak", "Nge-build binary workspace"). Bad: "Reading file", "Editing session.rs", "Running cargo build", "rg oncycle", raw shell commands.';

const LABEL = Type.String({
  description: LABEL_DESC,
});

const LABEL_PROP = {
  type: "string",
  description: LABEL_DESC,
};

function injectLabelIntoSchema(params: any) {
  if (!params || typeof params !== "object") return;
  const existing = params.properties || {};
  const { label: _existing, ...rest } = existing;
  params.properties = { label: { ...LABEL_PROP }, ...rest };
  if (Array.isArray(params.required)) {
    if (!params.required.includes("label")) {
      params.required.unshift("label");
    }
  } else {
    params.required = ["label"];
  }
}

function processToolSchema(tool: any) {
  if (!tool || typeof tool !== "object") return;
  if (tool.parameters) injectLabelIntoSchema(tool.parameters);
  if (tool.function?.parameters) injectLabelIntoSchema(tool.function.parameters);
  if (tool.input_schema) injectLabelIntoSchema(tool.input_schema);
  if (Array.isArray(tool.functionDeclarations)) {
    for (const decl of tool.functionDeclarations) {
      processToolSchema(decl);
    }
  }
  if (Array.isArray(tool.function_declarations)) {
    for (const decl of tool.function_declarations) {
      processToolSchema(decl);
    }
  }
}

function ensureToolLabels(tools: any) {
  if (!tools) return;
  if (Array.isArray(tools)) {
    for (const tool of tools) {
      processToolSchema(tool);
    }
  } else if (typeof tools === "object") {
    processToolSchema(tools);
  }
}

function wrapToolWithLabel(tool: any) {
  if (!tool || typeof tool !== "object" || tool.__keel_labeled) return tool;
  tool.__keel_labeled = true;
  if (tool.parameters && typeof tool.parameters === "object") {
    injectLabelIntoSchema(tool.parameters);
  }
  const prevPrep = tool.prepareArguments;
  tool.prepareArguments = (args: any) => {
    const next = { ...(args ?? {}) };
    if (typeof next.label !== "string" || !String(next.label).trim()) {
      next.label = "Working";
    }
    return prevPrep ? prevPrep(next) : next;
  };
  const prevExec = tool.execute;
  if (typeof prevExec === "function") {
    tool.execute = async function (toolCallId: any, args: any, signal: any, onUpdate: any, ctx: any) {
      const { label: _label, ...rest } = args ?? {};
      return prevExec.call(this, toolCallId, rest, signal, onUpdate, ctx);
    };
  }
  return tool;
}

function withLabel(def: any) {
  if (!def || typeof def !== "object") return def;
  const props = def.parameters?.properties ?? {};
  const { label: _existing, ...restProps } = props;
  const parameters = Type.Object(
    { label: LABEL, ...restProps },
    { additionalProperties: false },
  );
  const prevPrep = def.prepareArguments;
  const prevExec = def.execute;
  const wrapped = {
    ...def,
    __keel_labeled: true,
    parameters,
    prepareArguments(args: any) {
      const next = { ...(args ?? {}) };
      // Resumed sessions may lack label; keep schema required for new calls.
      if (typeof next.label !== "string" || !String(next.label).trim()) {
        next.label = "Working";
      }
      return prevPrep ? prevPrep(next) : next;
    },
    async execute(toolCallId: any, args: any, signal: any, onUpdate: any, ctx: any) {
      const { label: _label, ...rest } = args ?? {};
      return prevExec.call(this, toolCallId, rest, signal, onUpdate, ctx);
    },
  };
  return wrapped;
}

function registerLabeled(pi: any) {
  const cwd = process.cwd();
  for (const def of [
    createReadToolDefinition(cwd),
    createBashToolDefinition(cwd),
    createEditToolDefinition(cwd),
    createWriteToolDefinition(cwd),
    createFindToolDefinition(cwd),
    createGrepToolDefinition(cwd),
    createLsToolDefinition(cwd),
  ]) {
    pi.registerTool(withLabel(def));
  }
}

export default function (pi: any) {
  registerLabeled(pi);

  const applyToAllTools = () => {
    registerLabeled(pi);
    try {
      const all = pi.getAllTools?.() ?? [];
      for (const tool of all) {
        wrapToolWithLabel(tool);
      }
    } catch {}
  };

  if (typeof pi.registerTool === "function") {
    const origRegisterTool = pi.registerTool.bind(pi);
    pi.registerTool = (tool: any) => {
      wrapToolWithLabel(tool);
      return origRegisterTool(tool);
    };
  }

  pi.on("session_start", async () => {
    applyToAllTools();
  });

  pi.on("before_agent_start", async () => {
    applyToAllTools();
  });

  pi.on("before_provider_request", async (event: any) => {
    if (event?.payload?.tools) {
      ensureToolLabels(event.payload.tools);
    }
    if (event?.payload?.functionDeclarations) {
      ensureToolLabels(event.payload.functionDeclarations);
    }
    if (event?.payload?.function_declarations) {
      ensureToolLabels(event.payload.function_declarations);
    }
  });

  pi.on("tool_call", async (event: any) => {
    if (event?.input && typeof event.input === "object") {
      if (typeof event.input.label !== "string" || !String(event.input.label).trim()) {
        event.input.label = "Working";
      }
    }
  });
}
"#;

/// Writes `source` to `path` unless it is already there.
fn write_extension(path: &Path, source: &str) -> std::io::Result<()> {
    if fs::read_to_string(path).ok().as_deref() == Some(source) {
        return Ok(());
    }
    let tmp = path.with_extension("ts.tmp");
    fs::write(&tmp, source).and_then(|()| fs::rename(&tmp, path))
}

/// Pi runs every tool without asking. Under Ask and Auto-edit a per-run
/// extension gates `tool_call` with `ctx.ui.confirm`, which reaches Keel as a
/// confirm question. Pi's read-only built-ins and `readOnlyHint` tools pass;
/// Auto-edit also passes `edit` and `write`. A call nobody can confirm is
/// blocked.
fn permission_extension_source(permission: PermissionMode) -> String {
    permission_gate_source(permission, false)
}

fn permission_gate_source(permission: PermissionMode, plan_mode: bool) -> String {
    let allow = crate::plan_readonly_tools()
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let ui = keel_proto::UI_TOOLS
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "// Generated by Keel for each Pi run. Edits are overwritten.\n\
const READ_ONLY = new Set([\"read\", \"grep\", \"find\", \"ls\", \"memory_read\"]);\n\
const EDITS = new Set([\"edit\", \"write\"]);\n\
// memory_write takes no repo path: the engine refuses every path outside the\n\
// two memory folders, so it is allowed in every mode. The gate guards the repo.\n\
const ALWAYS = new Set([\"memory_write\"]);\n\
const AUTO_EDIT = {auto_edit};\n\
const PLAN = {plan};\n\
const PLAN_ALLOW = new Set([{allow}]);\n\
const PLAN_UI = new Set([{ui}]);\n\
export default function (pi: any) {{\n\
  pi.on(\"tool_call\", async (event: any, ctx: any) => {{\n\
    const name = event.toolName;\n\
    if (PLAN) {{\n\
      const base = String(name ?? \"\").split(\"__\").pop().toLowerCase().replace(/-/g, \"_\");\n\
      if (!PLAN_ALLOW.has(base) && !PLAN_UI.has(base)) {{\n\
        return {{ block: true, reason: `${{name}} is off in plan mode` }};\n\
      }}\n\
      return undefined;\n\
    }}\n\
    const hints = pi.getAllTools?.().find((tool: any) => tool.name === name)?.annotations;\n\
    if (READ_ONLY.has(name) || ALWAYS.has(name) || hints?.readOnlyHint === true) return undefined;\n\
    if (AUTO_EDIT && EDITS.has(name)) return undefined;\n\
    const input = event.input ?? {{}};\n\
    const detail = String(input.command ?? input.path ?? JSON.stringify(input)).slice(0, 400);\n\
    if (ctx.hasUI && (await ctx.ui.confirm(`Allow ${{name}}?`, detail))) return undefined;\n\
    return {{ block: true, reason: `${{name}} was not approved` }};\n\
  }});\n\
}}\n",
        auto_edit = permission == PermissionMode::AutoEdit,
        plan = if plan_mode { "true" } else { "false" },
        allow = allow,
        ui = ui,
    )
}

/// System-prompt section only while Plan mode is on.
fn plan_prompt_source() -> String {
    let trimmed = crate::PLAN_MODE_PROMPT.trim_end();
    let text = serde_json::to_string(trimmed).unwrap_or_else(|_| "\"\"".into());
    format!(
        "// Generated by Keel for a plan-mode run. Edits are overwritten.\n\
const PLAN = {text};\n\
export default function (pi: any) {{\n\
  pi.on(\"before_agent_start\", (event: any) => {{\n\
    const sections = event.systemPromptOptions?.sections;\n\
    if (sections && sections.keel_plan !== PLAN) sections.keel_plan = PLAN;\n\
  }});\n\
}}\n"
    )
}

fn plan_prompt_extension(plan_mode: bool, data_dir: Option<&Path>, tag: &str) -> Option<PathBuf> {
    if !plan_mode {
        return None;
    }
    let dir = data_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("keel-plan.{tag}.ts"));
    write_extension(&path, &plan_prompt_source()).ok()?;
    Some(path)
}

/// The gate's path, written in Keel's data dir (else in the temp dir).
/// `None` under full access: Pi's own behaviour.
fn permission_extension(
    permission: PermissionMode,
    data_dir: Option<&Path>,
    tag: &str,
) -> Option<Result<PathBuf, HarnessError>> {
    permission_extension_for(permission, data_dir, tag, false)
}

fn permission_extension_for(
    permission: PermissionMode,
    data_dir: Option<&Path>,
    tag: &str,
    plan_mode: bool,
) -> Option<Result<PathBuf, HarnessError>> {
    // Full access skips the ask gate. Plan mode still installs one: it
    // blocks mutating tools instead of offering them.
    if permission == PermissionMode::FullAccess && !plan_mode {
        return None;
    }
    let dir = data_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let path = if plan_mode {
        dir.join(format!(
            "keel-permission-{}.plan.{tag}.ts",
            permission.as_str()
        ))
    } else {
        dir.join(format!("keel-permission-{}.{tag}.ts", permission.as_str()))
    };
    Some(
        write_extension(
            &path,
            &if plan_mode {
                permission_gate_source(permission, true)
            } else {
                permission_extension_source(permission)
            },
        )
            .map(|()| path)
            .map_err(|err| HarnessError::Failed(format!("Pi permission gate: {err}"))),
    )
}

fn rpc_args(cmd: &mut std::process::Command, model: &str, cwd: &Path, probe: bool) {
    cmd.arg("--mode").arg("rpc");
    if probe {
        cmd.arg("--no-session");
        cmd.arg("--offline");
    }
    if !model.is_empty() {
        cmd.arg("--model").arg(model);
    }
    if !cwd.as_os_str().is_empty() && cwd.is_dir() {
        cmd.current_dir(cwd);
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ProcessConfig {
    cwd: PathBuf,
    project_fingerprint: String,
    plan_mode: bool,
    permission: PermissionMode,
    longer_cache: bool,
    keep_cache_warm: bool,
    exe_path: PathBuf,
    tag: &'static str,
}

enum ActorCommand {
    StartTurn {
        spec: SessionSpec,
        controls: RunControls,
        event_tx: mpsc::UnboundedSender<Result<AgentEvent, HarnessError>>,
        done_tx: tokio::sync::oneshot::Sender<()>,
    },
    Evict,
}

struct PoolEntry {
    actor_tx: mpsc::UnboundedSender<ActorCommand>,
    config: ProcessConfig,
    last_active: Instant,
    is_busy: bool,
}

pub struct PiProcessPool {
    entries: Mutex<HashMap<String, PoolEntry>>,
}

impl Default for PiProcessPool {
    fn default() -> Self {
        Self::new()
    }
}

impl PiProcessPool {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn evict_chat(&self, chat_id: &str) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = entries.remove(chat_id) {
            let _ = entry.actor_tx.send(ActorCommand::Evict);
        }
    }

    pub fn evict_all(&self) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        for (_, entry) in entries.drain() {
            let _ = entry.actor_tx.send(ActorCommand::Evict);
        }
    }
}

struct ProcessActor {
    chat_id: String,
    child: Child,
    pid: u32,
    _kill: GroupKill,
    stdin: ChildStdin,
    stdout: mpsc::UnboundedReceiver<String>,
    stderr: mpsc::UnboundedReceiver<String>,
    current_model: String,
    current_reasoning: String,
    current_session_path: Option<String>,
    config: ProcessConfig,
    last_active: Instant,
    last_report_usage: Option<Arc<dyn Fn(TokenUsage) + Send + Sync>>,
    idle_timeout: Duration,
    rx: mpsc::UnboundedReceiver<ActorCommand>,
    seq: u64,
}

impl ProcessActor {
    async fn run_loop(mut self, pool: Arc<PiProcessPool>) {
        loop {
            let deadline = self.last_active + self.idle_timeout;
            let idle_sleep = tokio::time::sleep_until(deadline.into());
            tokio::select! {
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(ActorCommand::StartTurn { spec, controls, event_tx, done_tx }) => {
                            self.last_report_usage = spec.report_usage.clone();
                            let res = self.drive_turn(spec, controls, event_tx).await;
                            self.last_active = Instant::now();
                            let _ = done_tx.send(());
                            if res.is_err() {
                                break;
                            }
                        }
                        Some(ActorCommand::Evict) | None => {
                            break;
                        }
                    }
                }
                line = self.stdout.recv() => {
                    match line {
                        Some(line) => {
                            if let Some(AgentEvent::Usage { usage }) = usage_of(&line) {
                                if let Some(report) = &self.last_report_usage {
                                    report(usage);
                                }
                            }
                        }
                        None => {
                            break;
                        }
                    }
                }
                _ = idle_sleep => {
                    tracing::info!(target: "keel_harness::pi", chat = %self.chat_id, "idle eviction timeout reached");
                    break;
                }
            }
        }
        {
            let mut entries = pool.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries.remove(&self.chat_id);
        }
        signal_group(self.pid, libc::SIGTERM);
        let _ = self.child.kill().await;
    }

    async fn drive_turn(
        &mut self,
        spec: SessionSpec,
        mut controls: RunControls,
        tx: mpsc::UnboundedSender<Result<AgentEvent, HarnessError>>,
    ) -> Result<(), HarnessError> {
        let mut next_id = {
            let seq = &mut self.seq;
            move || {
                *seq += 1;
                format!("k{seq}")
            }
        };

        if let Some(resume) = &spec.resume {
            if self.current_session_path.as_deref() != Some(resume.0.as_str()) {
                let mut success = false;
                let id = next_id();
                let _ = write_json(
                    &mut self.stdin,
                    &serde_json::json!({
                        "id": id,
                        "type": "switch_session",
                        "sessionPath": resume.0,
                    }),
                )
                .await;

                let switched = tokio::time::timeout(SWITCH_WAIT, async {
                    while let Some(line) = self.stdout.recv().await {
                        if let Ok(value) = serde_json::from_str::<Value>(&line)
                            && value["type"] == "response"
                            && value["id"] == id.as_str()
                        {
                            return value["success"] != false;
                        }
                    }
                    false
                })
                .await;

                if matches!(switched, Ok(true)) {
                    success = true;
                } else {
                    tracing::warn!(target: "keel_harness::pi", session = %resume.0, "switch_session failed or timed out; retrying once with longer timeout");
                    let retry_id = next_id();
                    let _ = write_json(
                        &mut self.stdin,
                        &serde_json::json!({
                            "id": retry_id,
                            "type": "switch_session",
                            "sessionPath": resume.0,
                        }),
                    )
                    .await;
                    let retry_switched = tokio::time::timeout(Duration::from_secs(30), async {
                        while let Some(line) = self.stdout.recv().await {
                            if let Ok(value) = serde_json::from_str::<Value>(&line)
                                && value["type"] == "response"
                                && value["id"] == retry_id.as_str()
                            {
                                return value["success"] != false;
                            }
                        }
                        false
                    })
                    .await;
                    if matches!(retry_switched, Ok(true)) {
                        success = true;
                    }
                }

                if !success {
                    if let Ok(Some(status)) = self.child.try_wait() {
                        let mut tail = VecDeque::new();
                        while let Ok(line) = self.stderr.try_recv() {
                            push_tail(&mut tail, line);
                        }
                        let _ = tx.send(Ok(errored(tail_message("pi", &tail, status.code()))));
                        return Err(HarnessError::Failed("process crashed during startup".to_string()));
                    }
                    tracing::error!(target: "keel_harness::pi", session = %resume.0, "switch_session failed on retry; aborting prompt");
                    let _ = tx.send(Ok(AgentEvent::Done {
                        status: keel_proto::TurnStatus::Errored {
                            message: "Couldn't resume this session".to_string(),
                        },
                    }));
                    return Ok(());
                }
                self.current_session_path = Some(resume.0.clone());
            }
        }

        if spec.model.0 != self.current_model {
            let id = next_id();
            let _ = write_json(
                &mut self.stdin,
                &serde_json::json!({"id": id, "type": "set_model", "model": spec.model.0}),
            )
            .await;
            self.current_model = spec.model.0.clone();
        }

        let reasoning_str = spec.reasoning.as_ref().map(|r| r.0.as_str()).unwrap_or("");
        if reasoning_str != self.current_reasoning.as_str() && !reasoning_str.is_empty() {
            let id = next_id();
            let _ = write_json(
                &mut self.stdin,
                &serde_json::json!({"id": id, "type": "set_thinking_level", "level": reasoning_str}),
            )
            .await;
            self.current_reasoning = reasoning_str.to_string();
        }

        let state_id = next_id();
        let _ = write_json(
            &mut self.stdin,
            &serde_json::json!({"id": state_id, "type": "get_state"}),
        )
        .await;

        {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            use std::hash::{Hash, Hasher};
            self.current_model.hash(&mut hasher);
            self.config.cwd.hash(&mut hasher);
            self.config.project_fingerprint.hash(&mut hasher);
            self.config.plan_mode.hash(&mut hasher);
            let fingerprint = format!("{:016x}", hasher.finish());
            tracing::debug!(target: "keel_harness::pi::cache", fingerprint = %fingerprint, "prefix fingerprint");
        }

        let prompt_id = next_id();
        let _ = write_json(
            &mut self.stdin,
            &{
                let (inline, unread) = crate::inline_images(&spec.attachments);
                let mut prompt = serde_json::json!({
                    "id": prompt_id,
                    "type": "prompt",
                    "message": format!("{}{}", spec.initial_text, crate::attachment_note(&unread)),
                });
                if !inline.is_empty() {
                    prompt["images"] = inline
                        .iter()
                        .map(|(file, data)| serde_json::json!({"type": "image", "data": data, "mimeType": file.mime}))
                        .collect();
                }
                prompt
            },
        )
        .await;

        let mut stop = Stop::None;
        let mut tools = HashSet::new();
        let mut labels = LabelStream::default();
        let mut spawns = HashSet::new();
        let mut progress = ProgressGate::default();
        let run_tag = uuid::Uuid::new_v4().simple().to_string();
        let mut assistant_n = 0u32;
        let mut failed: Option<String> = None;
        let mut tail = VecDeque::new();
        let mut stdout_open = true;
        let mut stderr_open = true;
        let mut controls_open = true;

        loop {
            match stop {
                Stop::Term { deadline } if Instant::now() >= deadline => {
                    signal_group(self.pid, libc::SIGTERM);
                    stop = Stop::Kill {
                        deadline: Instant::now() + KILL_WAIT,
                    };
                    continue;
                }
                Stop::Kill { deadline } if Instant::now() >= deadline => {
                    signal_group(self.pid, libc::SIGKILL);
                    let _ = tx.send(Ok(interrupted()));
                    return Err(HarnessError::Failed("process wedged and killed".to_string()));
                }
                _ => {}
            }

            let sleep_for = match stop {
                Stop::Term { deadline } | Stop::Kill { deadline } => {
                    Some(deadline.saturating_duration_since(Instant::now()))
                }
                Stop::None => None,
            };

            if !stdout_open && !stderr_open {
                let exit_code = self.child.try_wait().ok().flatten().and_then(|s| s.code());
                let _ = tx.send(Ok(errored(tail_message("pi", &tail, exit_code))));
                return Err(HarnessError::Failed("process exited".to_string()));
            }

            tokio::select! {
                biased;
                line = self.stdout.recv(), if stdout_open => {
                    match line {
                        Some(line) => {
                            if is_assistant_start(&line) {
                                assistant_n += 1;
                                failed = None;
                            }
                            if let Some(why) = assistant_error(&line) {
                                failed = Some(why);
                            }
                            if spec.plan_mode
                                && let Ok(value) = serde_json::from_str::<Value>(&line)
                                && let Some(reply) = crate::plan_mode_ui_reply(&value)
                            {
                                let _ = write_json(&mut self.stdin, &reply).await;
                                continue;
                            }
                            let events = on_stdout(&line, &mut tools, &mut labels, &mut spawns, &mut progress, &spec.report_session, &stop);
                            let mut ended = false;
                            for mut event in events {
                                if event == completed()
                                    && let Some(why) = failed.take()
                                {
                                    event = errored(why);
                                }
                                if let AgentEvent::Text { message_id, .. } | AgentEvent::Reasoning { message_id, .. } = &mut event {
                                    *message_id = format!("pi-{run_tag}-{assistant_n}");
                                }
                                if let AgentEvent::Usage { usage } = &event {
                                    if let Some(report) = &spec.report_usage {
                                        report(usage.clone());
                                    }
                                }
                                if event == completed() {
                                    let late = crate::proc::late_steers(&mut controls.inbound);
                                    if !late.is_empty() {
                                        let id = next_id();
                                        let _ = write_json(
                                            &mut self.stdin,
                                            &serde_json::json!({"id": id, "type": "prompt", "message": late.join("\n\n")}),
                                        ).await;
                                        continue;
                                    }
                                }
                                ended = matches!(event, AgentEvent::Done { .. });
                                if tx.send(Ok(event)).is_err() {
                                    ended = true;
                                    break;
                                }
                            }
                            if ended {
                                break;
                            }
                        }
                        None => stdout_open = false,
                    }
                }
                line = self.stderr.recv(), if stderr_open => {
                    match line {
                        Some(line) => push_tail(&mut tail, line),
                        None => stderr_open = false,
                    }
                }
                msg = controls.inbound.recv(), if controls_open && matches!(stop, Stop::None) => {
                    match msg {
                        Some(ControlMsg::Interrupt) => {
                            let id = next_id();
                            let _ = write_json(&mut self.stdin, &serde_json::json!({"id": id, "type": "clear_queue"})).await;
                            let id = next_id();
                            let _ = write_json(&mut self.stdin, &serde_json::json!({"id": id, "type": "abort"})).await;
                            stop = Stop::Term { deadline: Instant::now() + TERM_WAIT };
                        }
                        Some(ControlMsg::Steer { text }) => {
                            let id = next_id();
                            let _ = write_json(
                                &mut self.stdin,
                                &serde_json::json!({"id": id, "type": "steer", "message": text}),
                            ).await;
                        }
                        Some(ControlMsg::Answer { answers }) => {
                            for answer in &answers {
                                let _ = write_raw(&mut self.stdin, &extension_line(answer)).await;
                            }
                        }
                        None => controls_open = false,
                    }
                }
                _ = sleep_deadline(sleep_for), if sleep_for.is_some() => {}
            }
        }
        Ok(())
    }
}

fn on_stdout(
    line: &str,
    tools: &mut HashSet<String>,
    labels: &mut LabelStream,
    spawns: &mut HashSet<String>,
    progress: &mut ProgressGate,
    report: &Option<SessionReport>,
    stop: &Stop,
) -> Vec<AgentEvent> {
    note_session(line, report);
    let incoming = parse_line(line, tools, labels, spawns, progress);
    if !matches!(stop, Stop::None) {
        return Vec::new();
    }
    let mut events = match incoming {
        Some(Incoming::Settle) => vec![completed()],
        Some(Incoming::Events(events)) => events,
        Some(Incoming::Response(response)) => on_prompt(response).into_iter().collect(),
        Some(Incoming::Ignore) | None => Vec::new(),
    };
    // Before a settle's Done, so the turn's tokens land with it.
    if let Some(usage) = usage_of(line) {
        events.insert(0, usage);
    }
    events
}

/// An assistant message's tokens (Pi reports them per message, cached input
/// apart), or the model's context window from `get_state`, or idle cache warming.
fn usage_of(line: &str) -> Option<AgentEvent> {
    if !line.contains("\"message_end\"")
        && !line.contains("\"contextWindow\"")
        && !line.contains("\"cache_warm\"")
        && !line.contains("\"usage\"")
    {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    let usage = if value["type"] == "message_end" && value["message"]["role"] == "assistant" {
        let message = &value["message"];
        let u = &message["usage"];
        let usage = TokenUsage {
            model: message["model"].as_str().unwrap_or("").to_string(),
            input: n(&u["input"]),
            output: n(&u["output"]),
            cache_read: n(&u["cacheRead"]),
            cache_write: n(&u["cacheWrite"]),
            cache_warm: 0,
            context_used: None,
            context_window: None,
        };
        let total = usage.total();
        TokenUsage {
            context_used: (total > 0).then_some(total),
            ..usage
        }
    } else if (value["type"] == "entry_appended" && value["entry"]["type"] == "usage")
        || value["type"] == "usage"
    {
        let entry = if value["type"] == "entry_appended" {
            &value["entry"]
        } else {
            &value
        };
        let is_warm = entry["kind"].as_str() == Some("cache_warm");
        let u = &entry["usage"];
        let cache_read = if is_warm { 0 } else { n(&u["cacheRead"]) };
        let cache_write = if is_warm { 0 } else { n(&u["cacheWrite"]) };
        let cache_warm = if is_warm {
            let r = n(&u["cacheRead"]);
            if r > 0 { r } else { n(&u["totalTokens"]) }
        } else {
            0
        };
        let usage = TokenUsage {
            model: entry["model"].as_str().unwrap_or("").to_string(),
            input: if is_warm { 0 } else { n(&u["input"]) },
            output: if is_warm { 0 } else { n(&u["output"]) },
            cache_read,
            cache_write,
            cache_warm,
            context_used: None,
            context_window: None,
        };
        let total = usage.total();
        TokenUsage {
            context_used: (total > 0).then_some(total),
            ..usage
        }
    } else if value["type"] == "response" && value["command"] == "get_state" {
        let model = &value["data"]["model"];
        TokenUsage {
            model: model["id"].as_str().unwrap_or("").to_string(),
            context_window: model["contextWindow"].as_u64(),
            ..TokenUsage::default()
        }
    } else {
        return None;
    };
    Some(AgentEvent::Usage { usage })
}

fn note_session(line: &str, report: &Option<SessionReport>) {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let (id, file) = session_pair(&value);
    if id.is_none() && file.is_none() {
        return;
    }
    if let Some(report) = report {
        report(id.unwrap_or_default(), file);
    }
}

fn on_prompt(response: PiResponse) -> Option<AgentEvent> {
    if response.command != "prompt" {
        return None;
    }
    if !response.success {
        let message = if response.error.is_empty() {
            "pi rejected the prompt".to_string()
        } else {
            response.error
        };
        return Some(errored(message));
    }
    match response.disposition.as_deref() {
        Some("handled") => Some(completed()),
        _ => None,
    }
}

async fn sleep_deadline(wait: Option<Duration>) {
    if let Some(wait) = wait {
        tokio::time::sleep(wait).await;
    }
}

async fn write_json(stdin: &mut ChildStdin, value: &Value) -> io::Result<()> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await
}

async fn write_raw(stdin: &mut ChildStdin, line: &str) -> io::Result<()> {
    stdin.write_all(line.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await
}

/// An assistant `message_end` that stopped on a provider error, as readable
/// text: Pi's `errorMessage` is `"<status> <json body>"`, so the body's
/// `error.message` is preferred when it parses.
fn assistant_error(line: &str) -> Option<String> {
    if !line.contains("\"errorMessage\"") {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    let message = &value["message"];
    if value["type"] != "message_end"
        || message["role"] != "assistant"
        || message["stopReason"] != "error"
    {
        return None;
    }
    let raw = message["errorMessage"]
        .as_str()
        .unwrap_or("the model request failed");
    let body = raw
        .find('{')
        .and_then(|at| serde_json::from_str::<Value>(&raw[at..]).ok());
    Some(
        match body.as_ref().and_then(|b| b["error"]["message"].as_str()) {
            Some(text) => text.to_string(),
            None => raw.to_string(),
        },
    )
}

fn is_assistant_start(line: &str) -> bool {
    line.contains("\"message_start\"")
        && serde_json::from_str::<Value>(line)
            .is_ok_and(|v| v["type"] == "message_start" && v["message"]["role"] == "assistant")
}

fn parse_line(
    line: &str,
    tools: &mut HashSet<String>,
    labels: &mut LabelStream,
    spawns: &mut HashSet<String>,
    progress: &mut ProgressGate,
) -> Option<Incoming> {
    let value: Value = serde_json::from_str(line).ok()?;
    let kind = value
        .get("type")
        .and_then(|kind| kind.as_str())
        .unwrap_or("");
    if kind == "response" {
        return Some(Incoming::Response(PiResponse {
            command: value
                .get("command")
                .and_then(|cmd| cmd.as_str())
                .unwrap_or("")
                .to_string(),
            success: value.get("success").and_then(|flag| flag.as_bool()) != Some(false),
            disposition: value["data"]["disposition"].as_str().map(str::to_string),
            error: error_text(&value),
        }));
    }
    if kind == "extension_ui_request" {
        return Some(match extension_question(&value) {
            Some(event) => Incoming::Events(vec![event]),
            None => Incoming::Ignore,
        });
    }
    if kind == "agent_settled" {
        return Some(Incoming::Settle);
    }
    if kind == "agent_end" {
        tracing::trace!(target: "keel_harness::pi", "agent_end is not turn completion");
        return Some(Incoming::Ignore);
    }
    let events = match kind {
        "message_update" => message_update(&value, tools, labels),
        "tool_execution_start" => tool_start(&value, tools, labels, spawns),
        "tool_execution_update" => tool_update(&value, progress).into_iter().collect(),
        "tool_execution_end" => vec![tool_finished(&value, spawns)],
        _ => {
            if !kind.is_empty()
                && kind != "agent_start"
                && kind != "turn_start"
                && kind != "turn_end"
                && kind != "message_start"
                && kind != "message_end"
            {
                tracing::trace!(target: "keel_harness::pi", event = kind, "drop pi event");
            }
            Vec::new()
        }
    };
    if events.is_empty() {
        Some(Incoming::Ignore)
    } else {
        Some(Incoming::Events(events))
    }
}

fn session_pair(value: &Value) -> (Option<String>, Option<String>) {
    let mut id = None;
    let mut file = None;
    for node in [value, &value["data"]] {
        if let Some(text) = node.get("sessionId").and_then(|text| text.as_str())
            && !text.is_empty()
        {
            id = Some(text.to_string());
        }
        if let Some(text) = node.get("sessionFile").and_then(|text| text.as_str())
            && !text.is_empty()
        {
            file = Some(text.to_string());
        }
    }
    (id, file)
}

fn extension_question(value: &Value) -> Option<AgentEvent> {
    let method = value.get("method").and_then(|method| method.as_str())?;
    let kind = match method {
        "select" => QuestionKind::Select,
        "confirm" => QuestionKind::Confirm,
        "input" => QuestionKind::Input,
        "editor" => QuestionKind::Editor,
        other => {
            tracing::trace!(target: "keel_harness::pi", method = other, "drop extension ui");
            return None;
        }
    };
    let id = value
        .get("id")
        .and_then(|id| id.as_str())
        .unwrap_or("pi-question")
        .to_string();
    let title = value
        .get("title")
        .and_then(|title| title.as_str())
        .unwrap_or("");
    let message = value
        .get("message")
        .and_then(|message| message.as_str())
        .unwrap_or("");
    let prompt = match (title.is_empty(), message.is_empty()) {
        (true, true) => method.to_string(),
        (false, true) => title.to_string(),
        (true, false) => message.to_string(),
        (false, false) => format!("{title}\n{message}"),
    };
    Some(AgentEvent::Question {
        id: id.clone(),
        questions: vec![UserInputQuestion {
            id,
            kind,
            prompt,
            options: question_options(value, kind),
            required: value.get("timeout").is_none(),
            tool: None,
            multi: false,
            other: false,
        }],
        subagent: None,
    })
}

fn question_options(value: &Value, kind: QuestionKind) -> Vec<QuestionOption> {
    match kind {
        QuestionKind::Confirm => vec![
            QuestionOption {
                id: "yes".to_string(),
                label: "Yes".to_string(),
                role: Default::default(),
                detail: None,
            },
            QuestionOption {
                id: "no".to_string(),
                label: "No".to_string(),
                role: Default::default(),
                detail: None,
            },
        ],
        QuestionKind::Input | QuestionKind::Editor => Vec::new(),
        QuestionKind::Select => value
            .get("options")
            .and_then(|options| options.as_array())
            .map(|options| {
                options
                    .iter()
                    .filter_map(|item| {
                        if let Some(text) = item.as_str() {
                            return Some(QuestionOption {
                                id: text.to_string(),
                                label: text.to_string(),
                                role: Default::default(),
                                detail: None,
                            });
                        }
                        let label = item
                            .get("label")
                            .and_then(|label| label.as_str())
                            .unwrap_or("");
                        let id = item.get("id").and_then(|id| id.as_str()).unwrap_or(label);
                        if id.is_empty() {
                            None
                        } else {
                            Some(QuestionOption {
                                id: id.to_string(),
                                label: if label.is_empty() {
                                    id.to_string()
                                } else {
                                    label.to_string()
                                },
                                role: Default::default(),
                                detail: None,
                            })
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn extension_line(answer: &UserInputAnswer) -> String {
    if answer.value.is_empty() {
        return serde_json::to_string(&Cancelled {
            kind: "extension_ui_response",
            id: &answer.question_id,
            cancelled: true,
        })
        .unwrap_or_default();
    }
    match answer.kind {
        QuestionKind::Confirm => {
            let confirmed = matches!(answer.value.as_str(), "yes" | "true" | "confirm");
            serde_json::to_string(&Confirmed {
                kind: "extension_ui_response",
                id: &answer.question_id,
                confirmed,
            })
            .unwrap_or_default()
        }
        QuestionKind::Select | QuestionKind::Input | QuestionKind::Editor => {
            serde_json::to_string(&Valued {
                kind: "extension_ui_response",
                id: &answer.question_id,
                value: &answer.value,
            })
            .unwrap_or_default()
        }
    }
}

#[derive(Serialize)]
struct Valued<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    id: &'a str,
    value: &'a str,
}

#[derive(Serialize)]
struct Confirmed<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    id: &'a str,
    confirmed: bool,
}

#[derive(Serialize)]
struct Cancelled<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    id: &'a str,
    cancelled: bool,
}

fn message_update(
    value: &Value,
    tools: &mut HashSet<String>,
    labels: &mut LabelStream,
) -> Vec<AgentEvent> {
    let Some(event) = value.get("assistantMessageEvent") else {
        return Vec::new();
    };
    let kind = event
        .get("type")
        .and_then(|kind| kind.as_str())
        .unwrap_or("");
    let index = event
        .get("contentIndex")
        .and_then(|index| index.as_u64())
        .unwrap_or(0);
    match kind {
        "text_delta" => delta_event(event, index, false).into_iter().collect(),
        "thinking_delta" => delta_event(event, index, true).into_iter().collect(),
        "toolcall_start" => {
            let id = toolcall_id(event);
            let name = toolcall_name(event);
            labels.begin(index, &id, &name);
            let detail = event
                .get("toolCall")
                .map(|call| crate::tool_detail_for(&name, &call["arguments"]))
                .unwrap_or_default();
            let label = event
                .get("toolCall")
                .map(|call| crate::tool_label(&call["arguments"]))
                .unwrap_or_default();
            let mut out = Vec::new();
            if let Some(ev) = tool_started(tools, &id, &name, 0, detail, label.clone()) {
                out.push(ev);
            }
            if let Some(ev) = labels.emit_if_new(&id, label) {
                out.push(ev);
            }
            out
        }
        "toolcall_delta" => {
            let delta = event
                .get("delta")
                .and_then(|delta| delta.as_str())
                .unwrap_or("");
            labels
                .on_delta(index, delta)
                .into_iter()
                .collect()
        }
        "toolcall_end" => {
            let id = toolcall_id(event);
            let name = toolcall_name(event);
            let args = event
                .get("toolCall")
                .map(|call| &call["arguments"])
                .cloned()
                .unwrap_or(Value::Null);
            let detail = crate::tool_detail_for(&name, &args);
            let label = crate::tool_label(&args);
            labels.finish(index, &id, &label);
            let mut out = Vec::new();
            if let Some(ev) = tool_started(tools, &id, &name, 0, detail, label.clone()) {
                out.push(ev);
            }
            if let Some(ev) = labels.emit_if_new(&id, label) {
                out.push(ev);
            }
            out
        }
        _ => Vec::new(),
    }
}

fn toolcall_id(event: &Value) -> String {
    event
        .get("id")
        .and_then(|id| id.as_str())
        .or_else(|| {
            event
                .get("toolCall")
                .and_then(|call| call.get("id"))
                .and_then(|id| id.as_str())
        })
        .unwrap_or("")
        .to_string()
}

fn toolcall_name(event: &Value) -> String {
    event
        .get("toolName")
        .and_then(|name| name.as_str())
        .or_else(|| {
            event
                .get("toolCall")
                .and_then(|call| call.get("name"))
                .and_then(|name| name.as_str())
        })
        .unwrap_or("tool")
        .to_string()
}

fn delta_event(event: &Value, index: u64, reasoning: bool) -> Option<AgentEvent> {
    let delta = event
        .get("delta")
        .and_then(|delta| delta.as_str())
        .unwrap_or("");
    if delta.is_empty() {
        return None;
    }
    if reasoning {
        Some(AgentEvent::Reasoning {
            message_id: "pi-assistant".to_string(),
            part_id: format!("think-{index}"),
            delta: delta.to_string(),
            subagent: None,
        })
    } else {
        Some(AgentEvent::Text {
            message_id: "pi-assistant".to_string(),
            part_id: format!("text-{index}"),
            delta: delta.to_string(),
            subagent: None,
        })
    }
}

fn tool_start(
    value: &Value,
    tools: &mut HashSet<String>,
    labels: &mut LabelStream,
    spawns: &mut HashSet<String>,
) -> Vec<AgentEvent> {
    let id = value
        .get("toolCallId")
        .and_then(|id| id.as_str())
        .unwrap_or("");
    let name = value
        .get("toolName")
        .and_then(|name| name.as_str())
        .unwrap_or("tool");
    let args = &value["args"];
    let input_bytes = if args.is_null() {
        0
    } else {
        serde_json::to_string(args)
            .map(|text| text.len() as u64)
            .unwrap_or(0)
    };
    // A spawn tool starts a subagent record, not a tool row; the matching
    // `tool_execution_end` carries the same id. pi-subagents' `subagent`
    // also manages runs (`action: "status"`, …); those stay tool rows.
    if crate::is_spawn_tool(name) && args.get("action").is_none() {
        spawns.insert(id.to_string());
        let (sub_name, description) = crate::spawn_meta(args);
        return vec![AgentEvent::SubagentStarted {
            tool_id: id.to_string(),
            subagent_id: id.to_string(),
            name: sub_name.unwrap_or_else(|| "subagent".to_string()),
            description: description.unwrap_or_default(),
        }];
    }
    let label = crate::tool_label(args);
    let mut out = Vec::new();
    if let Some(ev) = labels.emit_if_new(id, label.clone()) {
        out.push(ev);
    }
    if let Some(ev) = tool_started(
        tools,
        id,
        name,
        input_bytes,
        crate::tool_detail_for(name, args),
        label,
    ) {
        out.push(ev);
    }
    out
}

/// A tool row's start. `toolcall_start` comes before the arguments, so the
/// first later start with them goes out again to fill in the detail or label
/// (marked in `tools` as `<id>\0detail`); any other repeat is dropped.
fn tool_started(
    tools: &mut HashSet<String>,
    id: &str,
    name: &str,
    input_bytes: u64,
    detail: String,
    label: String,
) -> Option<AgentEvent> {
    let marker = format!("{id}\0detail");
    let label_marker = format!("{id}\0label");
    if id.is_empty() {
        return None;
    }
    let first = tools.insert(id.to_string());
    let detail_new = !detail.is_empty() && tools.insert(marker);
    let label_new = !label.is_empty() && tools.insert(label_marker);
    if !first && !detail_new && !label_new {
        return None;
    }
    Some(AgentEvent::ToolStarted {
        tool_id: id.to_string(),
        name: name.to_string(),
        input_bytes,
        detail,
        label,
        subagent: None,
        kind: crate::kind_from_name(name).map(str::to_string),
    })
}

/// Accumulates streaming tool-call argument JSON so a `label` can fire as
/// soon as its string is complete.
#[derive(Default)]
struct LabelStream {
    by_index: HashMap<u64, LabelPartial>,
    emitted: HashMap<String, String>,
}

struct LabelPartial {
    id: String,
    buf: String,
}

impl LabelStream {
    fn begin(&mut self, index: u64, id: &str, _name: &str) {
        if id.is_empty() {
            return;
        }
        self.by_index.insert(
            index,
            LabelPartial {
                id: id.to_string(),
                buf: String::new(),
            },
        );
    }

    fn on_delta(&mut self, index: u64, delta: &str) -> Option<AgentEvent> {
        let partial = self.by_index.get_mut(&index)?;
        partial.buf.push_str(delta);
        let label = extract_streaming_label(&partial.buf);
        let id = partial.id.clone();
        self.emit_if_new(&id, label)
    }

    fn finish(&mut self, index: u64, id: &str, label: &str) {
        self.by_index.remove(&index);
        if !id.is_empty() && !label.is_empty() {
            self.emitted.insert(id.to_string(), label.to_string());
        }
    }

    fn emit_if_new(&mut self, id: &str, label: String) -> Option<AgentEvent> {
        if id.is_empty() || label.is_empty() {
            return None;
        }
        if self.emitted.get(id) == Some(&label) {
            return None;
        }
        self.emitted.insert(id.to_string(), label.clone());
        Some(AgentEvent::ToolLabel {
            tool_id: id.to_string(),
            label,
            subagent: None,
        })
    }
}

/// Pull a complete JSON string value for `label` or `toolAction` from a
/// partial args buffer. Returns sanitized text, or empty when incomplete.
fn extract_streaming_label(buf: &str) -> String {
    for key in ["\"label\"", "\"toolAction\""] {
        if let Some(raw) = json_string_after_key(buf, key) {
            let cleaned = crate::sanitize_label(&raw);
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
    }
    String::new()
}

/// After `"key":`, read a complete JSON string (handles `\"` / `\\`).
fn json_string_after_key(buf: &str, key: &str) -> Option<String> {
    let start = buf.find(key)?;
    let after = &buf[start + key.len()..];
    let colon = after.find(':')?;
    let mut rest = after[colon + 1..].trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    rest = &rest[1..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let Some(next) = chars.next() else {
                return None; // incomplete escape
            };
            match next {
                '"' | '\\' | '/' => out.push(next),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() < 4 {
                        return None;
                    }
                    let code = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(code)?);
                }
                _ => out.push(next),
            }
            continue;
        }
        if ch == '"' {
            return Some(out);
        }
        out.push(ch);
    }
    None // string not closed yet
}

/// `partialResult` text while a tool runs. An empty update (Pi sends one
/// before any bytes) is skipped. The partial result is the output so far.
fn tool_update(value: &Value, gate: &mut ProgressGate) -> Option<AgentEvent> {
    let id = value
        .get("toolCallId")
        .and_then(|id| id.as_str())
        .unwrap_or("");
    if id.is_empty() {
        return None;
    }
    let text: String = value["partialResult"]["content"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect()
        })
        .unwrap_or_default();
    if text.is_empty() || !gate.allow(id) {
        return None;
    }
    let name = value
        .get("toolName")
        .and_then(|name| name.as_str())
        .unwrap_or("");
    Some(AgentEvent::ToolProgress {
        tool_id: id.to_string(),
        output_tail: keel_proto::output_tail(&text),
        subagent: None,
        kind: crate::kind_from_name(name).map(str::to_string),
    })
}

fn tool_finished(value: &Value, spawns: &mut HashSet<String>) -> AgentEvent {
    let id = value
        .get("toolCallId")
        .and_then(|id| id.as_str())
        .unwrap_or("tool")
        .to_string();
    let result = &value["result"];
    let content: String = result["content"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let raw = match result {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        _ if !content.is_empty() => content,
        other => other.to_string(),
    };
    let is_error = value["isError"].as_bool().unwrap_or(false);
    // A finished spawn ends that subagent; it never was a tool row.
    if spawns.remove(&id) {
        return AgentEvent::SubagentDone {
            subagent_id: id,
            status: if is_error {
                TurnStatus::Errored {
                    message: summary_line(&raw),
                }
            } else {
                TurnStatus::Completed {}
            },
        };
    }
    AgentEvent::ToolFinished {
        tool_id: id,
        summary: summary_line(&raw),
        output_bytes: raw.len() as u64,
        is_error,
        images: crate::content_images(
            result["content"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default(),
        ),
        output_tail: keel_proto::output_tail(&raw),
        subagent: None,
        diff: None,
        duration_ms: value.get("durationMs").and_then(|n| {
            n.as_u64()
                .or_else(|| n.as_i64().filter(|n| *n >= 0).map(|n| n as u64))
        }),
    }
}

fn error_text(value: &Value) -> String {
    match &value["error"] {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Levels Pi 1.0.4 offers for a model, derived from `get_available_models`
/// the same way `get_available_thinking_levels` reports them: the base set
/// `off..high`, plus `xhigh` / `max` when mapped, minus levels mapped to null.
/// `None` when the record has no `reasoning` flag (older Pi): ask Pi instead.
fn thinking_levels(model: &Value) -> Option<Vec<String>> {
    let reasoning = model.get("reasoning")?.as_bool()?;
    if !reasoning {
        return Some(Vec::new());
    }
    let map = model.get("thinkingLevelMap");
    let level = |name: &str| map.and_then(|map| map.get(name));
    Some(
        ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
            .iter()
            .filter(|name| match level(name) {
                Some(Value::Null) => false,
                Some(_) => true,
                None => !matches!(**name, "xhigh" | "max"),
            })
            .map(|name| name.to_string())
            .collect(),
    )
}

/// A sign-in left open longer than this is given up.
const SIGN_IN_LIMIT: Duration = Duration::from_secs(10 * 60);

enum SignInLine {
    Event(crate::SignInEvent),
    Done,
    Failed(String),
}

/// One `keel_login` line from the login extension; `None` for anything
/// else Pi writes and for notices nobody needs to see.
fn sign_in_line(line: &str) -> Option<SignInLine> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value["type"] != "keel_login" {
        return None;
    }
    if value["done"] == true {
        return Some(SignInLine::Done);
    }
    if let Some(why) = value["error"].as_str() {
        return Some(SignInLine::Failed(why.to_string()));
    }
    let event = &value["event"];
    let text = |k: &str| event[k].as_str().unwrap_or_default().to_string();
    Some(SignInLine::Event(match event["type"].as_str()? {
        "auth_url" => crate::SignInEvent::Browser { url: text("url") },
        "device_code" => crate::SignInEvent::DeviceCode {
            code: text("userCode"),
            url: text("verificationUri"),
        },
        "progress" => crate::SignInEvent::Progress(text("message")),
        _ => return None,
    }))
}

/// The providers Keel's Settings lists, as `(Pi provider id, name, hint)`.
/// They stand in for the agent CLIs Keel used to wrap.
pub(crate) const SIGN_INS: [(&str, &str, &str); 6] = [
    ("anthropic", "Claude", "Pro or Max plan, or an API key"),
    ("openai-codex", "ChatGPT", "Plus or Pro plan, through Codex"),
    ("github-copilot", "GitHub Copilot", "Copilot plan"),
    (
        "commandcode",
        "Command Code",
        "Command Code plan or API key",
    ),
    ("xai", "xAI", "SuperGrok, X Premium, or an API key"),
    ("openai", "OpenAI", "API key, billed per use"),
];

/// One row per [`SIGN_INS`] entry, then one per other provider stored in
/// `auth.json`. `detail` is the stored credential's type.
fn auth_rows(stored: &serde_json::Map<String, Value>) -> Vec<crate::AuthAccount> {
    let row = |id: &str, name: &str, hint: &str| {
        let kind = stored
            .get(id)
            .map(|v| v["type"].as_str().unwrap_or("stored").to_string());
        crate::AuthAccount {
            id: id.to_string(),
            name: name.to_string(),
            hint: hint.to_string(),
            signed_in: kind.is_some(),
            detail: kind.unwrap_or_else(|| "not signed in".to_string()),
        }
    };
    let mut rows: Vec<_> = SIGN_INS
        .iter()
        .map(|(id, name, hint)| row(id, name, hint))
        .collect();
    let mut others: Vec<&String> = stored
        .keys()
        .filter(|k| !SIGN_INS.iter().any(|(id, ..)| id == k))
        .collect();
    others.sort();
    rows.extend(others.into_iter().map(|k| row(k, k, "")));
    rows
}

/// Put the Antigravity row before OpenAI's API key row, the last offered one.
fn with_antigravity(rows: &mut Vec<crate::AuthAccount>, row: crate::AuthAccount) {
    let at = rows
        .iter()
        .position(|r| r.id == "openai")
        .unwrap_or(rows.len());
    rows.insert(at, row);
}

/// Where the app bundle or a dev build keeps Pi: `Contents/Resources/pi`
/// in `Keel.app`, else `pi/` beside the binary (`scripts/fetch-pi.sh`).
fn bundle_dirs() -> Vec<PathBuf> {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    if let Some(contents) = dir.parent() {
        dirs.push(contents.join("Resources/pi"));
    }
    dirs.push(dir.join("pi"));
    dirs
}

fn candidate_paths(input: &ResolveInput) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(path) = &input.executable_env {
        push_unique(&mut out, path.clone());
    }
    for dir in &input.bundle_dirs {
        push_unique(&mut out, dir.join("pi"));
    }
    out
}

fn push_unique(out: &mut Vec<PathBuf>, path: PathBuf) {
    if !out.iter().any(|have| have == &path) {
        out.push(path);
    }
}

fn cached_version(path: &Path) -> (String, bool) {
    let Ok(meta) = fs::metadata(path) else {
        return (String::new(), false);
    };
    let modified = meta.modified().unwrap_or(UNIX_EPOCH);
    let len = meta.len();
    {
        let guard = version_cache()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(hit) = guard
            .iter()
            .find(|hit| hit.path == path && hit.modified == modified && hit.len == len)
        {
            return (hit.version.clone(), hit.managed);
        }
    }
    let (version, managed) = read_version(path);
    // A failed spawn is not cached, so the next probe can retry.
    if version.is_empty() {
        return (version, managed);
    }
    let mut guard = version_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    guard.retain(|hit| hit.path != path);
    guard.push(VersionHit {
        path: path.to_path_buf(),
        modified,
        len,
        version: version.clone(),
        managed,
    });
    if guard.len() > 8 {
        guard.remove(0);
    }
    (version, managed)
}

fn read_version(path: &Path) -> (String, bool) {
    let mut cmd = crate::proc::command(path);
    cmd.arg("--version")
        .stdin(StdStdio::null())
        .stdout(StdStdio::piped())
        .stderr(StdStdio::null());
    let Some(output) = crate::proc::output_timeout(cmd, Duration::from_secs(8)) else {
        return (String::new(), false);
    };
    let text = String::from_utf8_lossy(&output.stdout);
    classify_version(path, &text)
}

fn classify_version(path: &Path, stdout: &str) -> (String, bool) {
    let Some((major, version)) = parse_version(stdout) else {
        return (String::new(), false);
    };
    let homebrew = path == Path::new(HOMEBREW_PI);
    let managed = major == 1 && !(version.starts_with("0.84") || (homebrew && major != 1));
    (version, managed)
}

fn parse_version(text: &str) -> Option<(u32, String)> {
    for line in text.lines() {
        let line = line.trim();
        let tokens = line.split_whitespace().chain(std::iter::once(line));
        for token in tokens {
            if let Some(parsed) = split_semver(token) {
                return Some(parsed);
            }
        }
    }
    None
}

fn split_semver(token: &str) -> Option<(u32, String)> {
    let token = token.trim_matches(|ch: char| !ch.is_ascii_digit() && ch != '.');
    let mut parts = token.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0");
    if patch.is_empty() || patch.chars().any(|ch| !ch.is_ascii_digit()) {
        return None;
    }
    Some((major, format!("{major}.{minor}.{patch}")))
}

#[cfg_attr(not(test), allow(dead_code))]
fn split_records(input: &[u8]) -> (Vec<String>, Vec<u8>) {
    let mut records = Vec::new();
    let mut start = 0;
    for (index, byte) in input.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let mut end = index;
        if end > start && input[end - 1] == b'\r' {
            end -= 1;
        }
        if end > start {
            records.push(String::from_utf8_lossy(&input[start..end]).into_owned());
        }
        start = index + 1;
    }
    (records, input[start..].to_vec())
}

struct GroupKill {
    pid: u32,
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        if self.pid != 0 {
            signal_group(self.pid, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, LazyLock, Mutex};
    use std::time::Duration;

    use futures::StreamExt;
    use keel_proto::{AgentEvent, QuestionKind, TurnStatus, UserInputAnswer};
    use tokio::sync::mpsc;

    use super::{
        HOMEBREW_PI, INSTALL_HINT, PiHarness, ResolveInput, SIGN_INS, SignInLine, Value, auth_rows,
        classify_version, compose_path, extension_line, redact, sign_in_line, split_records,
        with_antigravity,
    };
    use crate::{
        ControlMsg, Harness, HarnessError, ModelId, NativeSessionId, ReasoningLevel, RunControls,
        SessionReport, SessionSpec,
    };

    #[test]
    fn permission_extension_per_mode() {
        use keel_proto::PermissionMode::*;
        assert!(super::permission_extension(FullAccess, None, "pi").is_none());
        let dir = std::env::temp_dir().join(format!("keel-pi-permission-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let ask = super::permission_extension(Ask, Some(&dir), "pi")
            .unwrap()
            .expect("written");
        assert_eq!(ask, dir.join("keel-permission-ask.pi.ts"));
        let source = fs::read_to_string(&ask).unwrap();
        assert!(
            source.contains("const AUTO_EDIT = false;") && source.contains("ctx.ui.confirm("),
            "{source}"
        );
        let edit = super::permission_extension(AutoEdit, Some(&dir), "pi")
            .unwrap()
            .expect("written");
        assert!(
            fs::read_to_string(edit)
                .unwrap()
                .contains("const AUTO_EDIT = true;")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_mode_gate_blocks_mutating_tools_without_a_prompt_only() {
        use keel_proto::PermissionMode::*;
        let off = super::permission_gate_source(Ask, false);
        assert!(off.contains("const PLAN = false;"), "{off}");
        assert!(super::plan_prompt_extension(false, None, "pi").is_none());
        let dir = std::env::temp_dir().join(format!("keel-pi-plan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let gate = super::permission_extension_for(FullAccess, Some(&dir), "pi", true)
            .unwrap()
            .expect("written");
        let source = fs::read_to_string(&gate).unwrap();
        assert!(source.contains("const PLAN = true;"), "{source}");
        let blocked = source.find("is off in plan mode").expect("deny");
        let confirm = source.find("ctx.ui.confirm(").expect("ask path stays");
        assert!(blocked < confirm, "plan mode denies before any confirm");
        assert!(!source.contains("\"edit\"") || source.contains("PLAN_ALLOW"));
        assert!(!crate::plan_mode_allows("edit"));
        assert!(!crate::plan_mode_allows("write"));
        assert!(!crate::plan_mode_allows("bash"));
        assert!(!crate::plan_mode_allows("shell"));
        assert!(!crate::plan_mode_allows("exec"));
        assert!(!crate::plan_mode_allows("memory_write"));
        assert!(!crate::plan_mode_allows("browser_click"));
        assert!(!crate::plan_mode_allows("browser_type"));
        assert!(!crate::plan_mode_allows("mcp__srv__delete_file"));
        assert!(crate::plan_mode_allows("read"));
        assert!(crate::plan_mode_allows("search"));
        assert!(crate::plan_mode_allows("list"));
        assert!(crate::plan_mode_allows("web_fetch"));
        assert!(crate::plan_mode_allows("browser_read"));
        assert!(crate::plan_mode_allows("show_plan"));
        let prompt = super::plan_prompt_extension(true, Some(&dir), "pi").expect("prompt");
        let prompt = fs::read_to_string(prompt).unwrap();
        assert!(prompt.contains("keel_plan"), "{prompt}");
        assert!(prompt.contains(crate::PLAN_MODE_PROMPT));
        assert!(prompt.contains("Research") && prompt.contains("exactly one plan card") && prompt.contains("stop"));
        assert!(!off.contains(crate::PLAN_MODE_PROMPT));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_extension_injects_and_registers_both_tools() {
        let sock = Path::new("/data/keel.sock");
        let source = super::memory_extension_source(sock, "tok-fixture");
        for needle in [
            "\"before_agent_start\"",
            "sections.keel_memory",
            "\"memory_read\"",
            "\"memory_write\"",
            "\"/data/keel.sock\"",
            "readOnlyHint: true",
            "method: \"hello\"",
            "memory_prompt",
        ] {
            assert!(source.contains(needle), "{needle} missing");
        }
        assert!(source.contains("Read and update Keel's notes for later sessions."));
        assert!(
            !source.contains("memory_delete"),
            "the model never gets delete"
        );

        let dir = std::env::temp_dir().join(format!("keel-pi-memory-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            super::memory_extension(Some(&dir)),
            None,
            "no token, no extension"
        );
        fs::write(dir.join("ipc.token"), "tok-fixture\n").unwrap();
        let path = super::memory_extension(Some(&dir)).expect("written");
        assert_eq!(path, dir.join("harnesses/keel/extensions/keel-memory.ts"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            super::memory_extension_source(&dir.join("keel.sock"), "tok-fixture")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        super::memory_extension(Some(&dir)).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            before,
            "same text is not rewritten"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stabilized_injected_sections_and_memory_fallback() {
        let sock = Path::new("/data/keel.sock");
        let mem = super::memory_extension_source(sock, "tok-test");
        // Verify fallback to last successful memory
        assert!(mem.contains("let lastMemory: string | undefined;"));
        assert!(mem.contains("lastMemory = String(result.text).trimEnd();"));
        assert!(mem.contains("const current = (lastMemory ?? `${PREAMBLE}\\n\\n${UNAVAILABLE}`).trimEnd();"));
        assert!(mem.contains("if (sections.keel_memory !== current)"));

        // Verify project extension sorts roots deterministically and trims whitespace
        let roots = crate::ProjectRoots {
            name: "test-proj".to_string(),
            roots: vec![
                ("main".to_string(), "/root/main".to_string()),
                ("b".to_string(), "/root/b".to_string()),
                ("a".to_string(), "/root/a".to_string()),
            ],
        };
        let proj = super::project_extension_source(&roots);
        assert!(proj.contains("keel_project !== PROJECT"));
        assert!(proj.contains("a: /root/a") && proj.contains("b: /root/b"));
        let a_pos = proj.find("a: /root/a").unwrap();
        let b_pos = proj.find("b: /root/b").unwrap();
        assert!(a_pos < b_pos, "secondary roots must be sorted deterministically");

        // Verify plan prompt stabilizes section
        let plan = super::plan_prompt_source();
        assert!(plan.contains("sections.keel_plan !== PLAN"));
        assert!(plan.contains(crate::PLAN_MODE_PROMPT.trim_end()));
    }

    #[test]
    fn permission_gate_passes_memory_tools_without_a_confirm() {
        use keel_proto::PermissionMode::*;
        for mode in [Ask, AutoEdit] {
            let source = super::permission_extension_source(mode);
            assert!(source.contains("\"ls\", \"memory_read\"]"), "{source}");
            assert!(
                source.contains("const ALWAYS = new Set([\"memory_write\"]);"),
                "{source}"
            );
            let pass = source
                .find("ALWAYS.has(name)")
                .expect("memory_write passes");
            assert!(
                pass < source.find("ctx.ui.confirm(").unwrap(),
                "passes before the confirm"
            );
        }
    }

    #[test]
    fn browser_extension_registers_keel_browser_beside_the_socket() {
        assert_eq!(super::browser_extension(None, None, "pi"), None);
        let dir = std::env::temp_dir().join(format!("keel-pi-browser-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("browser.sock");
        let path =
            super::browser_extension(Some(&sock), Some("c1"), "pi").expect("extension written");
        assert_eq!(path, dir.join("browser-mcp").join("pi-c1.ts"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let source = fs::read_to_string(&path).unwrap();
        assert!(
            source.contains("pi.registerMcpServer(\"keel-browser\""),
            "{source}"
        );
        assert!(
            !source.contains("keel-ui"),
            "cards have their own extension: {source}"
        );
        assert!(source.contains("exposure: \"direct\""), "{source}");
        assert!(source.contains("\"KEEL_BROWSER_CHAT\":\"c1\""), "{source}");
        let sock_lit = serde_json::to_string(&sock.to_string_lossy()).unwrap();
        assert!(
            source.contains(&format!("[\"mcp-browser\",\"--socket\",{sock_lit}]")),
            "{source}"
        );
        // Rewriting the same content is a no-op and keeps the path stable.
        assert_eq!(
            super::browser_extension(Some(&sock), Some("c1"), "pi"),
            Some(path)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ui_extension_registers_cards_and_their_prompt_section() {
        assert_eq!(super::ui_extension(None, "pi"), None);
        let dir = std::env::temp_dir().join(format!("keel-pi-ui-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = super::ui_extension(Some(&dir), "pi").expect("extension written");
        assert_eq!(path, dir.join("keel-ui.pi.ts"));
        let source = fs::read_to_string(&path).unwrap();
        assert!(
            source.contains("pi.registerMcpServer(\"keel-ui\""),
            "{source}"
        );
        assert!(source.contains("[\"mcp-ui\"]"), "{source}");
        assert!(
            source.contains("\"before_agent_start\"")
                && source.contains("sections.keel_cards = CARDS"),
            "{source}"
        );
        assert!(
            source.contains("show_diagram"),
            "the section names the cards: {source}"
        );
        assert!(!source.contains("keel-browser"), "{source}");
        let _ = fs::remove_dir_all(&dir);
    }

    fn gate() -> &'static tokio::sync::Mutex<()> {
        static GATE: LazyLock<tokio::sync::Mutex<()>> =
            LazyLock::new(|| tokio::sync::Mutex::new(()));
        &GATE
    }

    struct Tmp {
        path: PathBuf,
    }

    impl Tmp {
        fn new(label: &str) -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("keel-pi-{label}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("temp");
            Self { path }
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    struct EnvGuard {
        saved: Vec<(String, Option<String>)>,
    }

    impl EnvGuard {
        fn set(pairs: &[(&str, &str)]) -> Self {
            let saved = pairs
                .iter()
                .map(|(key, _)| ((*key).to_string(), std::env::var(key).ok()))
                .collect();
            for (key, value) in pairs {
                // Harness tests hold `gate`, so these writes do not race.
                unsafe { std::env::set_var(key, value) };
            }
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    struct Fix {
        tmp: Tmp,
        stub: PathBuf,
        stub_dir: PathBuf,
        work: PathBuf,
        session: PathBuf,
        expected_path: String,
    }

    fn install_stub(tmp: &Tmp) -> PathBuf {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/pi_stub.py");
        let dest = tmp.path.join("bin/pi");
        fs::create_dir_all(dest.parent().expect("parent")).expect("bin");
        fs::copy(&src, &dest).expect("copy stub");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).expect("chmod");
        dest
    }

    fn layout(label: &str) -> Fix {
        let tmp = Tmp::new(label);
        let stub = install_stub(&tmp);
        let stub_dir = tmp.path.join("stub");
        let work = tmp.path.join("work");
        let other = tmp.path.join("other");
        let extra = tmp.path.join("extra");
        fs::create_dir_all(&stub_dir).expect("stub dir");
        fs::create_dir_all(&work).expect("work");
        fs::create_dir_all(&other).expect("other");
        fs::create_dir_all(&extra).expect("extra");
        let session = tmp.path.join("session.jsonl");
        let (_, _, expected_path) = composed_path(&tmp.path, &stub);
        Fix {
            tmp,
            stub,
            stub_dir,
            work,
            session,
            expected_path,
        }
    }

    fn composed_path(root: &Path, stub: &Path) -> (String, Vec<PathBuf>, String) {
        let exe_dir = stub.parent().expect("exe dir").to_path_buf();
        let other = root.join("other");
        let extra = root.join("extra");
        let mut engine_dirs = vec![exe_dir.clone(), other.clone()];
        if let Ok(path) = std::env::var("PATH") {
            engine_dirs.extend(std::env::split_paths(&path));
        }
        let engine_path = std::env::join_paths(&engine_dirs)
            .expect("path")
            .to_string_lossy()
            .into_owned();
        let login = vec![exe_dir, other, extra.clone(), extra];
        let expected = compose_path(stub, &engine_path, &login);
        (engine_path, login, expected)
    }

    fn harness_for(fix: &Fix) -> PiHarness {
        let (engine_path, login_dirs, _) = composed_path(&fix.tmp.path, &fix.stub);
        PiHarness::assemble(ResolveInput {
            executable_env: Some(fix.stub.clone()),
            bundle_dirs: vec![],
            engine_path,
            login_dirs,
        })
    }

    fn bind_stub(fix: &Fix, mode: &str) -> EnvGuard {
        EnvGuard::set(&[
            ("STUB_DIR", fix.stub_dir.to_str().expect("utf8")),
            ("STUB_MODE", mode),
            ("STUB_SESSION", fix.session.to_str().expect("utf8")),
            ("KEEL_DEV_BEARER", "keel-test-bearer-not-for-child"),
            ("KEEL_DATA_DIR", "/tmp/keel-should-not-leak"),
            ("KEEL_IPC_PORT", "9"),
            ("PI_EXECUTABLE", fix.stub.to_str().expect("utf8")),
        ])
    }

    fn spec(fix: &Fix, report: Option<SessionReport>) -> SessionSpec {
        SessionSpec {
            cwd: fix.work.clone(),
            model: ModelId("fixture-model".to_string()),
            reasoning: Some(ReasoningLevel("low".to_string())),
            resume: Some(NativeSessionId(
                fix.work.join("resume.jsonl").to_string_lossy().into_owned(),
            )),
            initial_text: "hello".to_string(),
            attachments: Vec::new(),
            report_session: report,
            data_dir: None,
            browser_sock: None,
            browser_chat: None,
            permission: Default::default(),
            plan_mode: false,
            project: None,
            report_usage: None,
            chat_id: None,
            longer_cache: false,
            keep_cache_warm: true,
            idle_eviction_timeout_secs: None,
        }
    }

    fn write_sh(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("script");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    fn write_version(path: &Path, version: &str) {
        write_sh(path, &format!("printf '%s\\n' '{version}'"));
    }

    async fn until_done(
        mut stream: Pin<Box<dyn futures::Stream<Item = Result<AgentEvent, HarnessError>> + Send>>,
        limit: Duration,
        mut on_event: impl FnMut(&AgentEvent),
    ) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + limit;
        while tokio::time::Instant::now() < deadline {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, stream.next()).await {
                Ok(Some(Ok(event))) => {
                    on_event(&event);
                    let done = matches!(event, AgentEvent::Done { .. });
                    events.push(event);
                    if done {
                        return events;
                    }
                }
                Ok(Some(Err(err))) => panic!("{err}"),
                Ok(None) => return events,
                Err(_) => break,
            }
        }
        events
    }

    fn done_status(events: &[AgentEvent]) -> &TurnStatus {
        match events.last() {
            Some(AgentEvent::Done { status }) => status,
            other => panic!("missing done: {other:?}"),
        }
    }

    fn argv_of(fix: &Fix) -> Vec<String> {
        if let Ok(json_text) = fs::read_to_string(fix.stub_dir.join("argv.json")) {
            if let Ok(args) = serde_json::from_str(&json_text) {
                return args;
            }
        }
        fs::read_to_string(fix.stub_dir.join("argv"))
            .expect("argv")
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn fixture_records_pi_1_0_4() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let version = fs::read_to_string(dir.join("pi-1.0.4.version")).expect("version");
        let (parsed, managed) = classify_version(Path::new("/tmp/pi-fixture"), &version);
        assert_eq!(parsed, "1.0.4");
        assert!(managed);
        let lines: Vec<serde_json::Value> = fs::read_to_string(dir.join("pi-1.0.4.jsonl"))
            .expect("jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).expect("json"))
            .collect();
        let by = |command: &str| {
            lines
                .iter()
                .find(|line| line["command"] == command)
                .unwrap_or_else(|| panic!("{command}"))
        };
        assert_eq!(by("get_state")["data"]["steeringMode"], "one-at-a-time");
        let models = &by("get_available_models")["data"]["models"][0];
        assert_eq!(models["id"], "claude-fable-5");
        // `off` maps to null: Pi does not offer it for this model; xhigh/max are mapped.
        assert_eq!(
            super::thinking_levels(models).expect("levels"),
            ["minimal", "low", "medium", "high", "xhigh", "max"]
        );
        let session = &by("get_state");
        assert!(session["data"]["sessionId"].is_string());
    }

    /// A real Pi 1.0.4 turn (`xai/grok-4.5`, one bash tool call), captured from
    /// the installed binary and fed through the driver's parser.
    #[test]
    fn fixture_turn_parses_into_events() {
        let text = include_str!("../fixtures/pi-1.0.4-turn.jsonl");
        let mut tools = std::collections::HashSet::new();
        let mut labels = super::LabelStream::default();
        let mut spawns = std::collections::HashSet::new();
        let mut progress = super::ProgressGate::default();
        let mut events = Vec::new();
        for line in text.lines() {
            events.extend(super::on_stdout(
                line,
                &mut tools,
                &mut labels,
                &mut spawns,
                &mut progress,
                &None,
                &super::Stop::None,
            ));
        }
        let done = events
            .iter()
            .position(|e| matches!(e, AgentEvent::Done { .. }));
        assert_eq!(done, Some(events.len() - 1), "{events:?}");
        assert!(matches!(
            events.last(),
            Some(AgentEvent::Done {
                status: TurnStatus::Completed {}
            })
        ));
        // Announced before its arguments, then once more with them.
        let starts: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolStarted { name, detail, .. } => {
                    Some((name.as_str(), detail.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(starts, [("bash", ""), ("bash", "echo hi")]);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolFinished { summary, duration_ms: None, .. } if summary == "hi"
        )));
        // The first partial result is empty; the second carries the bytes.
        let tails: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolProgress {
                    output_tail, kind, ..
                } => Some((output_tail.as_str(), kind.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(tails, [("hi\n", Some("execute"))]);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolStarted { kind: Some(kind), .. } if kind == "execute"
        )));
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Text { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "ok");
    }

    /// Pi 1.1 names `durationMs` on `tool_execution_end`. 1.0.4 did not.
    #[test]
    fn tool_end_keeps_duration_ms() {
        let line = r#"{"type":"tool_execution_end","toolCallId":"t","toolName":"bash","durationMs":42,"result":{"content":[{"type":"text","text":"ok"}]},"isError":false}"#;
        let mut tools = std::collections::HashSet::new();
        let mut labels = super::LabelStream::default();
        let mut spawns = std::collections::HashSet::new();
        let mut progress = super::ProgressGate::default();
        let events = super::on_stdout(
            line,
            &mut tools,
            &mut labels,
            &mut spawns,
            &mut progress,
            &None,
            &super::Stop::None,
        );
        assert!(
            matches!(&events[..], [AgentEvent::ToolFinished { duration_ms: Some(42), summary, .. }] if summary == "ok"),
            "{events:?}"
        );
    }

    #[test]
    fn toolcall_delta_emits_label_as_soon_as_it_closes() {
        let mut tools = std::collections::HashSet::new();
        let mut labels = super::LabelStream::default();
        let mut spawns = std::collections::HashSet::new();
        let mut progress = super::ProgressGate::default();
        let start = r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_start","contentIndex":0,"id":"c1","toolName":"read","toolCall":{"id":"c1","name":"read","arguments":{}}}}"#;
        let partial = r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_delta","contentIndex":0,"delta":"{\"label\":\"Reading the on/off cycle"}}"#;
        let closed = r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_delta","contentIndex":0,"delta":" logic\",\"path\":\"a.rs\"}"}}"#;
        let mut events = Vec::new();
        for line in [start, partial, closed] {
            events.extend(super::on_stdout(
                line,
                &mut tools,
                &mut labels,
                &mut spawns,
                &mut progress,
                &None,
                &super::Stop::None,
            ));
        }
        let labels: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolLabel { tool_id, label, .. } if tool_id == "c1" => {
                    Some(label.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["Reading the on/off cycle logic"], "{events:?}");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::ToolStarted { tool_id, name, .. }
                    if tool_id == "c1" && name == "read"
            )),
            "{events:?}"
        );
    }

    #[test]
    fn version_probe_accepts_newer_1_x() {
        // Classify the Homebrew path in memory. Do not create that file.
        let (version, managed) = classify_version(Path::new("/tmp/pi"), "1.9.0\n");
        assert_eq!(version, "1.9.0");
        assert!(managed);
        let (_, managed) = classify_version(Path::new("/tmp/pi"), "v1.2.0\n");
        assert!(managed);
        let (_, managed) = classify_version(Path::new("/tmp/pi"), "0.84.4\n");
        assert!(!managed);
        let (_, managed) = classify_version(Path::new("/tmp/pi"), "2.0.0\n");
        assert!(!managed);
        assert_eq!(HOMEBREW_PI, "/opt/homebrew/bin/pi");
        let (_, managed) = classify_version(Path::new(HOMEBREW_PI), "0.84.4\n");
        assert!(!managed);
        let (version, managed) = classify_version(Path::new(HOMEBREW_PI), "1.2.0\n");
        assert_eq!(version, "1.2.0");
        assert!(managed);
    }

    #[test]
    fn records_split_on_lf_only() {
        let (rows, rest) = split_records(b"one\ntwo\r\n");
        assert_eq!(rows, vec!["one".to_string(), "two".to_string()]);
        assert!(rest.is_empty());
        let (rows, rest) = split_records("a\u{2028}b\u{2029}c\n".as_bytes());
        assert_eq!(rows, vec!["a\u{2028}b\u{2029}c".to_string()]);
        assert!(rest.is_empty());
        let (rows, rest) = split_records(b"partial");
        assert!(rows.is_empty());
        assert_eq!(rest, b"partial");
    }

    #[test]
    fn stderr_secrets_are_redacted() {
        assert_eq!(redact("hello"), "hello");
        assert_eq!(
            redact("Authorization: Bearer abc.def"),
            "Authorization: [redacted]"
        );
        assert_eq!(redact("Basic xyz"), "[redacted]");
        assert_eq!(redact("sk-SECRETVALUE"), "[redacted]");
        assert_eq!(redact("ghp_abcdefghijklmnopqrstuvwxyz"), "[redacted]");
        assert_eq!(redact("xox-dummy-test-token"), "[redacted]");
        assert_eq!(redact("api_key=supersecret"), "[redacted]");
        assert_eq!(redact("prefix sk-SECRET tail"), "prefix [redacted] tail");
    }

    #[test]
    fn extension_responses_keep_field_order() {
        let select = extension_line(&UserInputAnswer {
            question_id: "q-1".to_string(),
            kind: QuestionKind::Select,
            value: "Allow".to_string(),
        });
        assert_eq!(
            select,
            r#"{"type":"extension_ui_response","id":"q-1","value":"Allow"}"#
        );
        let yes = extension_line(&UserInputAnswer {
            question_id: "c1".to_string(),
            kind: QuestionKind::Confirm,
            value: "yes".to_string(),
        });
        assert_eq!(
            yes,
            r#"{"type":"extension_ui_response","id":"c1","confirmed":true}"#
        );
        let no = extension_line(&UserInputAnswer {
            question_id: "c1".to_string(),
            kind: QuestionKind::Confirm,
            value: "no".to_string(),
        });
        assert_eq!(
            no,
            r#"{"type":"extension_ui_response","id":"c1","confirmed":false}"#
        );
        let word = extension_line(&UserInputAnswer {
            question_id: "c1".to_string(),
            kind: QuestionKind::Confirm,
            value: "confirm".to_string(),
        });
        assert_eq!(
            word,
            r#"{"type":"extension_ui_response","id":"c1","confirmed":true}"#
        );
        let empty = extension_line(&UserInputAnswer {
            question_id: "c1".to_string(),
            kind: QuestionKind::Input,
            value: String::new(),
        });
        assert_eq!(
            empty,
            r#"{"type":"extension_ui_response","id":"c1","cancelled":true}"#
        );
    }

    #[tokio::test]
    async fn resolution_prefers_env_then_bundle_and_skips_unmanaged() {
        let _gate = gate().lock().await;
        let tmp = Tmp::new("resolve");
        let env_pi = tmp.path.join("env/pi");
        let app = tmp.path.join("Resources/pi");
        let dev = tmp.path.join("debug/pi");
        write_version(&env_pi, "1.9.0");
        write_version(&app.join("pi"), "1.3.0");
        write_version(&dev.join("pi"), "1.2.0");
        let pick = |env: Option<PathBuf>| {
            PiHarness::assemble(ResolveInput {
                executable_env: env,
                bundle_dirs: vec![app.clone(), dev.clone()],
                engine_path: String::new(),
                login_dirs: vec![],
            })
        };
        let picked = pick(Some(env_pi.clone()));
        assert_eq!(
            (picked.executable(), picked.version()),
            (Some(env_pi), "1.9.0")
        );
        let picked = pick(None);
        assert_eq!(
            (picked.executable(), picked.version()),
            (Some(app.join("pi")), "1.3.0")
        );
        write_version(&app.join("pi"), "0.84.1");
        fs::set_permissions(&dev.join("pi"), fs::Permissions::from_mode(0o644)).expect("0644");
        let skipped = pick(None);
        assert!(!skipped.installed());
        assert_eq!(skipped.version(), "0.84.1");
    }

    #[tokio::test]
    async fn version_cache_reexecs_when_the_file_changes() {
        let _gate = gate().lock().await;
        let tmp = Tmp::new("cache");
        let script = tmp.path.join("pi");
        let count = tmp.path.join("count");
        let count_q = count.to_string_lossy().replace('\'', "'\\''");
        write_sh(
            &script,
            &format!(
                "n=0\nif [ -f '{count_q}' ]; then n=$(cat '{count_q}'); fi\nn=$((n + 1))\nprintf '%s\\n' \"$n\" > '{count_q}'\nprintf '%s\\n' '1.0.4'"
            ),
        );
        let input = |script: &Path| ResolveInput {
            executable_env: Some(script.to_path_buf()),
            bundle_dirs: vec![],
            engine_path: String::new(),
            login_dirs: vec![],
        };
        let first = PiHarness::assemble(input(&script));
        assert_eq!(first.version(), "1.0.4");
        assert_eq!(fs::read_to_string(&count).expect("count").trim(), "1");
        let _second = PiHarness::assemble(input(&script));
        assert_eq!(fs::read_to_string(&count).expect("count").trim(), "1");
        let mut body = fs::read_to_string(&script).expect("script");
        body.push('\n');
        fs::write(&script, body).expect("rewrite");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
        let _third = PiHarness::assemble(input(&script));
        assert_eq!(fs::read_to_string(&count).expect("count").trim(), "2");
    }

    #[tokio::test]
    async fn missing_pi_returns_not_installed() {
        let _gate = gate().lock().await;
        let tmp = Tmp::new("missing");
        let bundle = tmp.path.join("bundle");
        let harness = PiHarness::assemble(ResolveInput {
            executable_env: None,
            bundle_dirs: vec![bundle.clone()],
            engine_path: String::new(),
            login_dirs: vec![],
        });
        assert!(!harness.installed());
        let (tx, rx) = mpsc::unbounded_channel();
        let err = match harness
            .run(
                SessionSpec {
                    cwd: tmp.path.clone(),
                    model: ModelId("fixture-model".to_string()),
                    reasoning: None,
                    resume: None,
                    initial_text: "hello".to_string(),
                    attachments: Vec::new(),
                    report_session: None,
                    data_dir: None,
                    browser_sock: None,
                    browser_chat: None,
                    permission: Default::default(),
                    plan_mode: false,
                    project: None,
                    report_usage: None,
                    chat_id: None,
                    longer_cache: false,
                    keep_cache_warm: true,
                    idle_eviction_timeout_secs: None,
                },
                RunControls { inbound: rx },
            )
            .await
        {
            Err(err) => err,
            Ok(_) => panic!("expected NotInstalled"),
        };
        drop(tx);
        match err {
            HarnessError::NotInstalled { path, hint } => {
                assert_eq!(hint, INSTALL_HINT);
                assert_eq!(path, bundle.join("pi"));
            }
            other => panic!("{other}"),
        }
    }

    #[tokio::test]
    async fn stub_turn_uses_the_session_file_and_composed_path() {
        let _gate = gate().lock().await;
        let fix = layout("turn");
        let _env = bind_stub(&fix, "turn");
        let harness = harness_for(&fix);
        assert!(harness.installed());
        assert_eq!(harness.version(), "1.0.4");
        let models = harness.models().await.expect("models");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id.0, "local/fixture-model");
        assert_eq!(models[0].name, "Fixture");
        let probe_args = argv_of(&fix);
        assert!(
            probe_args
                .windows(2)
                .any(|pair| pair[0] == "--no-session" && pair[1] == "--offline")
        );
        let levels = harness
            .reasoning_levels(&ModelId("local/fixture-model".to_string()))
            .await
            .expect("levels");
        assert_eq!(
            levels,
            vec![
                ReasoningLevel("off".to_string()),
                ReasoningLevel("low".to_string()),
                ReasoningLevel("high".to_string()),
            ]
        );
        let seen = Arc::new(Mutex::new(Vec::<(String, Option<String>)>::new()));
        let report: SessionReport = {
            let seen = Arc::clone(&seen);
            Arc::new(move |id, file| {
                seen.lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .push((id, file));
            })
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let stream = harness
            .run(spec(&fix, Some(report)), RunControls { inbound: rx })
            .await
            .expect("run");
        drop(tx);
        let events = until_done(stream, Duration::from_secs(8), |_| {}).await;
        assert!(matches!(done_status(&events), TurnStatus::Completed {}));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolStarted { tool_id, name, input_bytes, subagent: None, .. }
                if tool_id == "t1" && name == "bash" && *input_bytes == 14
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolFinished { tool_id, summary, output_bytes, is_error, .. }
                if tool_id == "t1" && summary == "ok" && *output_bytes == 7 && !is_error
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Text { message_id, part_id, delta , subagent: None}
                if message_id.starts_with("pi-") && part_id == "text-0" && delta == "fixture-ok"
        )));
        let seen = seen.lock().unwrap_or_else(|err| err.into_inner());
        let session = fix.session.to_string_lossy().into_owned();
        assert!(
            seen.iter()
                .any(|(id, file)| id == "sess-1" && file.as_deref() == Some(session.as_str())),
            "{seen:?}"
        );
        let args = argv_of(&fix);
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--mode" && pair[1] == "rpc")
        );
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--model" && pair[1] == "fixture-model")
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--no-session" || arg == "--offline")
        );
        assert!(!args.iter().any(|arg| arg.starts_with('@')));
        let stdin = fs::read_to_string(fix.stub_dir.join("stdin")).expect("stdin");
        assert!(stdin.contains("\"type\":\"switch_session\""));
        assert!(stdin.contains("resume.jsonl"));
        assert!(
            !fix.stub_dir.join("prompt_before_switch").exists(),
            "prompt sent before switch_session answered"
        );
        assert!(stdin.contains("\"type\":\"set_thinking_level\""));
        assert!(stdin.contains("\"level\":\"low\""));
        assert!(stdin.contains("\"type\":\"get_state\""));
        assert!(stdin.contains("\"type\":\"prompt\""));
        assert!(!stdin.contains("set_steering_mode"));
        assert!(!stdin.contains("streamingBehavior"));
        let meta = fs::read_to_string(fix.stub_dir.join("meta")).expect("meta");
        let pid = meta
            .lines()
            .find_map(|line| line.strip_prefix("pid="))
            .expect("pid");
        let pgid = meta
            .lines()
            .find_map(|line| line.strip_prefix("pgid="))
            .expect("pgid");
        assert_eq!(pid, pgid);
        assert_ne!(pid, "0");
        let env_text = fs::read_to_string(fix.stub_dir.join("env")).expect("env");
        assert!(
            env_text.lines().all(|line| !line.starts_with("KEEL_")),
            "{env_text}"
        );
        assert!(
            !env_text.contains("keel-test-bearer-not-for-child"),
            "{env_text}"
        );
        assert!(
            env_text
                .lines()
                .any(|line| line.starts_with("PI_EXECUTABLE="))
        );
        let path_line = env_text
            .lines()
            .find(|line| line.starts_with("PATH="))
            .expect("PATH");
        assert_eq!(path_line, format!("PATH={}", fix.expected_path));
        let parts: Vec<PathBuf> = std::env::split_paths(&fix.expected_path).collect();
        let exe_dir = fix.stub.parent().expect("exe dir");
        assert_eq!(parts.first().map(PathBuf::as_path), Some(exe_dir));
        assert_eq!(
            parts
                .iter()
                .filter(|part| part.as_path() == exe_dir)
                .count(),
            1
        );
        let extra = fix.tmp.path.join("extra");
        assert_eq!(parts.iter().filter(|part| **part == extra).count(), 1);
    }

    #[tokio::test]
    async fn resume_failure_aborts_prompt_and_emits_error() {
        let _gate = gate().lock().await;
        let fix = layout("switch_fail");
        let harness = harness_for(&fix);
        let _env = bind_stub(&fix, "switch_fail");
        let (tx, rx) = mpsc::unbounded_channel();
        let stream = harness
            .run(spec(&fix, None), RunControls { inbound: rx })
            .await
            .expect("run");
        drop(tx);
        let events = until_done(stream, Duration::from_secs(8), |_| {}).await;
        let status = done_status(&events);
        assert!(
            matches!(&status, TurnStatus::Errored { message } if message == "Couldn't resume this session"),
            "expected Errored with Couldn't resume this session, got {status:?}"
        );
        let stdin = fs::read_to_string(fix.stub_dir.join("stdin")).expect("stdin");
        assert!(stdin.contains("\"type\":\"switch_session\""));
        assert!(
            !stdin.contains("\"type\":\"prompt\""),
            "prompt must not be sent when resume fails"
        );
    }

    #[tokio::test]
    async fn agent_end_does_not_finish_the_turn() {
        let events = run_mode("past-end").await;
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Text { delta, .. } if delta == "after-end"
        )));
        assert!(matches!(done_status(&events), TurnStatus::Completed {}));
    }

    #[tokio::test]
    async fn unicode_separators_stay_inside_the_delta() {
        let events = run_mode("unicode").await;
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Text { delta, .. }
                if delta.contains('\u{2028}') && delta.contains('\u{2029}')
        )));
        assert!(matches!(done_status(&events), TurnStatus::Completed {}));
    }

    #[tokio::test]
    async fn handled_prompt_completes_without_waiting() {
        let events = run_mode("handled").await;
        assert!(
            events
                .iter()
                .all(|event| matches!(event, AgentEvent::Done { .. }))
        );
        assert!(matches!(done_status(&events), TurnStatus::Completed {}));
    }

    #[tokio::test]
    async fn extension_question_drops_widgets() {
        let _gate = gate().lock().await;
        let fix = layout("question");
        let _env = bind_stub(&fix, "question");
        let harness = harness_for(&fix);
        let (tx, rx) = mpsc::unbounded_channel();
        let stream = harness
            .run(spec(&fix, None), RunControls { inbound: rx })
            .await
            .expect("run");
        let events = until_done(stream, Duration::from_secs(8), |event| {
            if matches!(event, AgentEvent::Question { .. }) {
                let _ = tx.send(ControlMsg::Steer {
                    text: "later".to_string(),
                });
                let _ = tx.send(ControlMsg::Answer {
                    answers: vec![UserInputAnswer {
                        question_id: "q-1".to_string(),
                        kind: QuestionKind::Select,
                        value: "Allow".to_string(),
                    }],
                });
            }
        })
        .await;
        let questions: Vec<_> = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::Question { .. }))
            .collect();
        assert_eq!(questions.len(), 1, "{events:?}");
        match &questions[0] {
            AgentEvent::Question {
                id,
                questions,
                subagent: None,
            } => {
                assert_eq!(id, "q-1");
                assert_eq!(questions.len(), 1);
                assert_eq!(questions[0].id, "q-1");
                assert_eq!(questions[0].kind, QuestionKind::Select);
                assert_eq!(questions[0].prompt, "Allow dangerous command?");
                assert!(questions[0].required);
                assert_eq!(questions[0].options[0].id, "Allow");
                assert_eq!(questions[0].options[0].label, "Allow");
                assert_eq!(questions[0].options[1].id, "Block");
            }
            other => panic!("{other:?}"),
        }
        assert!(!format!("{events:?}").contains("hidden"), "{events:?}");
        assert!(matches!(done_status(&events), TurnStatus::Completed {}));
        let stdin = fs::read_to_string(fix.stub_dir.join("stdin")).expect("stdin");
        assert!(stdin.lines().any(|line| {
            line == r#"{"type":"extension_ui_response","id":"q-1","value":"Allow"}"#
        }));
        assert!(stdin.contains("\"type\":\"steer\""));
        assert!(stdin.contains("later"));
        assert!(!stdin.contains("set_steering_mode"));
        assert!(!stdin.contains("streamingBehavior"));
    }

    #[tokio::test]
    async fn crash_attaches_the_redacted_stderr_tail() {
        let events = run_mode("crash").await;
        match done_status(&events) {
            TurnStatus::Errored { message } => {
                assert!(message.contains("[redacted]"), "{message}");
                for secret in ["SECRETVALUE", "abc.def", "ghp_", "xoxb", "supersecret"] {
                    assert!(!message.contains(secret), "{message}");
                }
            }
            other => panic!("{other:?} {events:?}"),
        }
    }

    #[tokio::test]
    async fn interrupt_escalates_and_discards_queue_text() {
        let _gate = gate().lock().await;
        let fix = layout("interrupt");
        let _env = bind_stub(&fix, "interrupt");
        let harness = harness_for(&fix);
        let (tx, rx) = mpsc::unbounded_channel();
        let stream = harness
            .run(spec(&fix, None), RunControls { inbound: rx })
            .await
            .expect("run");
        let started = std::time::Instant::now();
        let events = until_done(stream, Duration::from_secs(12), |event| {
            if matches!(event, AgentEvent::Text { .. }) {
                let _ = tx.send(ControlMsg::Interrupt);
            }
        })
        .await;
        assert!(
            started.elapsed() >= Duration::from_secs(3),
            "elapsed {:?}",
            started.elapsed()
        );
        assert!(matches!(done_status(&events), TurnStatus::Interrupted {}));
        assert!(
            !format!("{events:?}").contains("secret paste"),
            "{events:?}"
        );
        let stdin = fs::read_to_string(fix.stub_dir.join("stdin")).unwrap_or_default();
        assert!(!stdin.contains("secret paste"), "{stdin}");
        assert!(stdin.contains("clear_queue"), "{stdin}");
        assert!(stdin.contains("abort"), "{stdin}");
        let signals = fs::read_to_string(fix.stub_dir.join("signals")).unwrap_or_default();
        assert!(signals.contains("term"), "{signals}");
    }

    async fn run_mode(mode: &str) -> Vec<AgentEvent> {
        let _gate = gate().lock().await;
        let fix = layout(mode);
        let _env = bind_stub(&fix, mode);
        let harness = harness_for(&fix);
        let (tx, rx) = mpsc::unbounded_channel();
        let stream = harness
            .run(spec(&fix, None), RunControls { inbound: rx })
            .await
            .expect("run");
        drop(tx);
        until_done(stream, Duration::from_secs(8), |_| {}).await
    }

    #[tokio::test]
    #[ignore = "spawns the Pi in PI_EXECUTABLE with --no-session --offline"]
    async fn live_pi_get_state_is_offline() {
        let _gate = gate().lock().await;
        let pi = PathBuf::from(std::env::var_os("PI_EXECUTABLE").expect("PI_EXECUTABLE"));
        let tmp = Tmp::new("live");
        let agent = tmp.path.join("agent");
        fs::create_dir_all(&agent).expect("agent dir");
        let _env = EnvGuard::set(&[("PI_CODING_AGENT_DIR", agent.to_str().expect("utf8"))]);
        let harness = PiHarness::assemble(ResolveInput {
            executable_env: Some(pi),
            bundle_dirs: vec![],
            engine_path: std::env::var("PATH").unwrap_or_default(),
            login_dirs: vec![],
        });
        assert!(harness.installed());
        assert!(
            harness.version().starts_with('1') && !harness.version().starts_with("0.84"),
            "{}",
            harness.version()
        );
        let lines = harness
            .probe(
                "",
                vec![serde_json::json!({"id": "s", "type": "get_state"})],
            )
            .await
            .expect("get_state");
        assert!(lines.iter().any(|line| {
            line.get("command").and_then(|cmd| cmd.as_str()) == Some("get_state")
                && line.get("success").and_then(|flag| flag.as_bool()) == Some(true)
        }));
    }

    #[tokio::test]
    #[ignore = "spawns the Pi in PI_EXECUTABLE: model list, levels, one real turn"]
    async fn live_pi_models_and_turn() {
        let _gate = gate().lock().await;
        let harness = PiHarness::keel();
        assert!(harness.installed());
        let models = harness.models().await.expect("models");
        eprintln!("{models:?}");
        let model = models
            .iter()
            .find(|model| model.id.0 == "xai/grok-4.5")
            .unwrap_or(&models[0])
            .id
            .clone();
        let levels = harness.reasoning_levels(&model).await.expect("levels");
        eprintln!("{model:?} {levels:?}");
        let probed = harness
            .probe(
                &model.0,
                vec![serde_json::json!({"id": "t", "type": "get_available_thinking_levels"})],
            )
            .await
            .expect("probe");
        let live: Vec<ReasoningLevel> = probed
            .iter()
            .find(|line| line["command"] == "get_available_thinking_levels")
            .and_then(|line| line["data"]["levels"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|level| level.as_str().map(|l| ReasoningLevel(l.to_string())))
            .collect();
        assert_eq!(levels, live, "derived levels must match Pi's own answer");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let report: SessionReport = {
            let seen = Arc::clone(&seen);
            Arc::new(move |id, file| seen.lock().unwrap().push((id, file)))
        };
        let (_tx, rx) = mpsc::unbounded_channel();
        let stream = harness
            .run(
                SessionSpec {
                    cwd: std::env::temp_dir(),
                    model,
                    reasoning: levels.first().cloned(),
                    resume: None,
                    initial_text: "Reply with the single word ok.".to_string(),
                    attachments: Vec::new(),
                    report_session: Some(report),
                    data_dir: None,
                    browser_sock: None,
                    browser_chat: None,
                    permission: Default::default(),
                    plan_mode: false,
                    project: None,
                    report_usage: None,
                    chat_id: None,
                    longer_cache: false,
                    keep_cache_warm: true,
                    idle_eviction_timeout_secs: None,
                },
                RunControls { inbound: rx },
            )
            .await
            .expect("run");
        let events = until_done(stream, Duration::from_secs(90), |_| {}).await;
        eprintln!("{events:?}");
        assert!(matches!(done_status(&events), TurnStatus::Completed {}));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Text { .. })));
        assert!(seen.lock().unwrap().iter().any(|(_, file)| file.is_some()));
    }

    async fn run_args_for(
        harness: &PiHarness,
        fix: &Fix,
        data: &Path,
        sock: Option<&Path>,
    ) -> Vec<String> {
        let mut spec = spec(fix, None);
        spec.data_dir = Some(data.to_path_buf());
        spec.browser_sock = sock.map(Path::to_path_buf);
        spec.browser_chat = Some("c1".into());
        spec.permission = keel_proto::PermissionMode::Ask;
        let (tx, rx) = mpsc::unbounded_channel();
        let stream = harness
            .run(spec, RunControls { inbound: rx })
            .await
            .expect("run");
        drop(tx);
        let events = until_done(stream, Duration::from_secs(8), |_| {}).await;
        assert!(matches!(done_status(&events), TurnStatus::Completed {}));
        argv_of(fix)
    }

    #[tokio::test]
    async fn keel_run_keeps_its_own_sessions_and_extensions() {
        let _gate = gate().lock().await;
        let fix = layout("keel-run");
        let _env = bind_stub(&fix, "turn");
        let data = fix.tmp.path.join("data");
        fs::create_dir_all(&data).unwrap();
        let sock = data.join("browser.sock");
        let has = |args: &[String], flag: &str, value: &Path| {
            args.windows(2)
                .any(|p| p[0] == flag && p[1] == value.to_string_lossy())
        };
        let keel = run_args_for(&harness_for(&fix), &fix, &data, Some(&sock)).await;
        assert!(
            has(
                &keel,
                "--session-dir",
                &data.join("harnesses/keel/sessions")
            ),
            "{keel:?}"
        );
        assert!(
            has(&keel, "--extension", &data.join("keel-ui.keel.ts")),
            "{keel:?}"
        );
        assert!(
            has(&keel, "--extension", &data.join("browser-mcp/keel-c1.ts")),
            "{keel:?}"
        );
        assert!(
            has(
                &keel,
                "--extension",
                &data.join("keel-permission-ask.keel.ts")
            ),
            "{keel:?}"
        );
        assert!(
            has(
                &keel,
                "--extension",
                &data.join("harnesses/keel/extensions/keel-labels.ts")
            ),
            "{keel:?}"
        );
        assert!(
            has(
                &keel,
                "--extension",
                &data.join("harnesses/keel/extensions/keel-cache.ts")
            ),
            "{keel:?}"
        );
        assert!(
            keel.windows(2).any(
                |p| p[0] == "--append-system-prompt" && p[1] == crate::keel::KEEL_SYSTEM_PROMPT
            ),
            "{keel:?}"
        );
        assert_eq!(
            keel.iter().filter(|a| *a == "--extension").count(),
            5,
            "{keel:?}"
        );

        // Without the browser, sessions, cards and the gate stay in the data dir.
        let bare = run_args_for(&harness_for(&fix), &fix, &data, None).await;
        assert!(
            has(
                &bare,
                "--session-dir",
                &data.join("harnesses/keel/sessions")
            ),
            "{bare:?}"
        );
        assert!(
            has(&bare, "--extension", &data.join("keel-ui.keel.ts")),
            "{bare:?}"
        );
        assert!(
            has(
                &bare,
                "--extension",
                &data.join("keel-permission-ask.keel.ts")
            ),
            "{bare:?}"
        );
        assert!(
            has(
                &bare,
                "--extension",
                &data.join("harnesses/keel/extensions/keel-labels.ts")
            ),
            "{bare:?}"
        );
        assert!(
            has(
                &bare,
                "--extension",
                &data.join("harnesses/keel/extensions/keel-cache.ts")
            ),
            "{bare:?}"
        );
        assert!(
            bare.windows(2).any(
                |p| p[0] == "--append-system-prompt" && p[1] == crate::keel::KEEL_SYSTEM_PROMPT
            ),
            "{bare:?}"
        );
        assert!(!bare.iter().any(|a| a.contains("browser-mcp")), "{bare:?}");
        assert_eq!(
            bare.iter().filter(|a| *a == "--extension").count(),
            4,
            "{bare:?}"
        );
    }

    #[test]
    fn sign_in_lines_map_to_events() {
        use crate::SignInEvent::*;
        let event = |line: &str| match sign_in_line(line) {
            Some(SignInLine::Event(e)) => Some(e),
            _ => None,
        };
        assert_eq!(
            event(
                r#"{"type":"keel_login","event":{"type":"auth_url","url":"https://claude.ai/oauth/authorize?x=1"}}"#
            ),
            Some(Browser {
                url: "https://claude.ai/oauth/authorize?x=1".into()
            })
        );
        assert_eq!(
            event(
                r#"{"type":"keel_login","event":{"type":"device_code","userCode":"0A50-65F9","verificationUri":"https://github.com/login/device"}}"#
            ),
            Some(DeviceCode {
                code: "0A50-65F9".into(),
                url: "https://github.com/login/device".into()
            })
        );
        assert!(matches!(
            sign_in_line(r#"{"type":"keel_login","done":true}"#),
            Some(SignInLine::Done)
        ));
        assert!(
            matches!(sign_in_line(r#"{"type":"keel_login","error":"denied"}"#), Some(SignInLine::Failed(w)) if w == "denied")
        );
        assert!(sign_in_line(r#"{"type":"response","command":"prompt"}"#).is_none());
        assert!(sign_in_line("not json").is_none());
    }

    #[test]
    fn sign_ins_list_the_offered_providers_then_the_rest() {
        let stored: serde_json::Map<String, Value> = serde_json::from_str(
            r#"{"zai":{"type":"api_key","key":"secret"},"anthropic":{"type":"oauth","access":"x"}}"#,
        )
        .unwrap();
        let rows = auth_rows(&stored);
        assert_eq!(rows.len(), SIGN_INS.len() + 1);
        assert_eq!(
            (rows[0].signed_in, rows[0].detail.as_str()),
            (true, "oauth")
        );
        assert!(!rows[1].signed_in);
        let last = rows.last().unwrap();
        assert_eq!(
            (
                last.id.as_str(),
                last.name.as_str(),
                last.signed_in,
                last.detail.as_str()
            ),
            ("zai", "zai", true, "api_key")
        );
        assert_eq!(
            (rows[0].id.as_str(), rows[0].name.as_str()),
            ("anthropic", "Claude")
        );
        assert!(rows.iter().all(|r| !r.detail.contains("secret")));
    }

    #[test]
    fn antigravity_sits_before_the_openai_api_key_row() {
        let mut rows = auth_rows(&serde_json::Map::new());
        let row = crate::AuthAccount {
            id: "antigravity".into(),
            name: "Antigravity".into(),
            hint: String::new(),
            signed_in: false,
            detail: "not signed in".into(),
        };
        with_antigravity(&mut rows, row);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids[ids.len() - 2..], ["antigravity", "openai"]);
    }

    #[test]
    fn project_extension_names_every_root() {
        use crate::ProjectRoots;
        let project = ProjectRoots {
            name: "Resona RIF".into(),
            roots: vec![
                ("web".into(), "/tmp/web".into()),
                ("api".into(), "/tmp/api".into()),
            ],
        };
        let source = super::project_extension_source(&project);
        assert!(
            source.contains("sections.keel_project = PROJECT"),
            "{source}"
        );
        assert!(
            source.contains("Resona RIF")
                && source.contains("web: /tmp/web")
                && source.contains("api: /tmp/api"),
            "{source}"
        );
        let dir = std::env::temp_dir().join(format!("keel-pi-project-{}", std::process::id()));
        let path =
            super::project_extension(Some(&project), Some(&dir), Some("chat-1"), "keel").unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(written, source);
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    fn usage(line: &str) -> TokenUsage {
        match usage_of(line) {
            Some(AgentEvent::Usage { usage }) => usage,
            other => panic!("no usage: {other:?}"),
        }
    }

    #[test]
    fn assistant_messages_and_get_state_report_usage() {
        let turn = include_str!("../fixtures/pi-1.0.4-turn.jsonl");
        // Two assistant messages in the fixture (the tool call, then "ok").
        assert_eq!(turn.lines().filter_map(usage_of).count(), 2);
        let last = usage(
            turn.lines()
                .filter(|l| usage_of(l).is_some())
                .next_back()
                .unwrap(),
        );
        assert_eq!(
            (
                last.model.as_str(),
                last.input,
                last.output,
                last.cache_read
            ),
            ("grok-4.5", 103, 6, 8064)
        );
        assert_eq!(last.context_used, Some(last.total()));
        let state = include_str!("../fixtures/pi-1.0.4.jsonl")
            .lines()
            .next()
            .unwrap();
        let state = usage(state);
        assert_eq!(
            (state.model.as_str(), state.context_window, state.total()),
            ("claude-opus-4-8", Some(1_000_000), 0)
        );
        assert!(usage_of(r#"{"type":"message_end","message":{"role":"user"}}"#).is_none());
    }

    #[test]
    fn cache_warm_entries_and_token_split_parsing() {
        let warm_line = r#"{"type":"entry_appended","entry":{"type":"usage","kind":"cache_warm","model":"claude-3-5-sonnet","usage":{"input":0,"output":0,"cacheRead":50000,"cacheWrite":0,"totalTokens":50000}}}"#;
        let warm = usage(warm_line);
        assert_eq!(warm.model.as_str(), "claude-3-5-sonnet");
        assert_eq!(warm.cache_read, 0);
        assert_eq!(warm.cache_write, 0);
        assert_eq!(warm.cache_warm, 50000);
        assert_eq!(warm.total(), 50000);

        let assistant_line = r#"{"type":"message_end","message":{"role":"assistant","model":"claude-3-5-sonnet","usage":{"input":100,"output":50,"cacheRead":800,"cacheWrite":100}}}"#;
        let u = usage(assistant_line);
        assert_eq!(u.input, 100);
        assert_eq!(u.output, 50);
        assert_eq!(u.cache_read, 800);
        assert_eq!(u.cache_write, 100);
        assert_eq!(u.cache_warm, 0);
        assert_eq!(u.total(), 1050);

        // Hit-rate formula: read / (input + read + write)
        let total_input_side = u.input + u.cache_read + u.cache_write;
        let hit_rate = (u.cache_read as f64) / (total_input_side as f64);
        assert!((hit_rate - (800.0 / 1000.0)).abs() < 1e-6);
        assert_eq!(format!("{:.1}%", hit_rate * 100.0), "80.0%");
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;

    #[test]
    fn provider_errors_read_as_their_message() {
        let line = r#"{"type":"message_end","message":{"role":"assistant","stopReason":"error","errorMessage":"400 {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"Add more at claude.ai\"}}"}}"#;
        assert_eq!(
            assistant_error(line).as_deref(),
            Some("Add more at claude.ai")
        );
        let plain = r#"{"type":"message_end","message":{"role":"assistant","stopReason":"error","errorMessage":"socket hang up"}}"#;
        assert_eq!(assistant_error(plain).as_deref(), Some("socket hang up"));
        let ok = r#"{"type":"message_end","message":{"role":"assistant","stopReason":"stop","errorMessage":null}}"#;
        assert!(assistant_error(ok).is_none());
    }
}

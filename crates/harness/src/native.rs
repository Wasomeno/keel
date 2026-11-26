//! In-process Keel harness driver built directly on `keel-core`, `keel-tools`,
//! `keel-mcp`, `keel-llm-catalog`, and `keel-llm-route`.
//!
//! Replaces the external `pi --mode rpc` subprocess with the native Rust agent
//! loop while preserving the [`Harness`] contract expected by `keel-engine`.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{Stream, stream};
use keel_core::store::{SessionLog, SessionMeta, new_id as new_session_id};
use keel_core::{
    AGENT_TOOL_NAME, Harness as CoreHarness, HookConfig, ProjectInstructions, RunEvent, RunStop,
    Session, SkillCatalog, SubagentCatalog, SubagentConfig, SubagentEvent,
    build_openai_codex_route, build_route,
};
use keel_llm_catalog::{AuthSpec, Catalog, LoadedCatalog, ModelSpec, Provider};
use keel_llm_route::{
    Client, CredentialRef, CredentialStore, EnvResolver, GROK_DEFAULT_CLIENT_ID, GROK_TOKEN_URL,
    OPENAI_DEFAULT_CLIENT_ID, OPENAI_TOKEN_URL, OAuthError, OAuthProviderConfig, OAuthTokenSet,
    SecretResolver, StoreResolver, default_http_client, login_device, login_pkce,
};
use keel_llm_types::{
    CachePolicy, LlmError, LlmEvent, Message, Part, ProtocolId, ReasoningEffort, Role, ToolSpec,
};
use keel_mcp::{McpConfig, ServerConfig};
use keel_proto::{
    AgentEvent, HarnessId, OptionRole, PermissionMode, QuestionKind, QuestionOption, SubagentCaps,
    SubagentRef, SubagentVisibility, TokenUsage, ToolAsk, ToolDiff, TurnStatus, UserInputQuestion,
};
use keel_tools::{
    PermissionDecision, PermissionGate, PermissionRequest, Tool, ToolContext, ToolFuture,
    ToolOutcome, ToolRegistry, Workspace, builtin,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::keel::{KEEL_SYSTEM_PROMPT, KeelProfile, PiProfile};
use crate::{
    AuthAccount, ControlMsg, Harness, HarnessError, Model, ModelId, NativeSessionId,
    ReasoningLevel, RunControls, SessionSpec, SignInEvent,
};

/// Concise one-line description injected into opaque tool schemas. The rule
/// and examples live once in `KEEL_SYSTEM_PROMPT`.
pub const LABEL_DESC: &str =
    "Optional short plain-language progress label naming the action and page or target (e.g. \"Opening the login page\", \"Clicking Save on Settings\"), no trailing dots.";

/// The fixed top of the `<keel_memory>` prompt section.
pub const MEMORY_PREAMBLE: &str = include_str!("../../../prompts/memory.md").trim_ascii_end();

pub const MEMORY_UNAVAILABLE: &str = "Memory is unavailable this turn.";

/// Must equal `keel_rpc::PROTO_VERSION` (checked by engine tests).
pub const MEMORY_PROTO: u32 = 1;

/// Bundled skill for natural Bahasa Indonesia prose, loaded on demand via `skill`.
pub const INDONESIAN_SKILL: &str = include_str!("../../../prompts/indonesian.md");

const DEFAULT_MAX_TURNS: usize = 48;
const MEMORY_IPC_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_IPC_FRAME_BYTES: usize = 4 * 1024 * 1024;
const MAX_DIFF_SNAPSHOT_BYTES: u64 = 256 * 1024;

/// Providers shown in Settings -> Accounts, ordered for display.
const SETTINGS_PROVIDERS: [(&str, &str, &str, &str); 5] = [
    (
        "anthropic",
        "Claude",
        "Anthropic API key (ANTHROPIC_API_KEY)",
        "anthropic",
    ),
    (
        "openai",
        "OpenAI",
        "ChatGPT Plus / Pro subscription, or OPENAI_API_KEY",
        "openai",
    ),
    (
        "xai",
        "xAI",
        "SuperGrok / X Premium OAuth, or XAI_API_KEY",
        "grok",
    ),
    (
        "commandcode",
        "Command Code",
        "Command Code API key (COMMANDCODE_API_KEY)",
        "commandcode",
    ),
    (
        "opencode",
        "OpenCode",
        "OpenCode API key (OPENCODE_API_KEY)",
        "opencode",
    ),
];

/// Default model preference when the session spec omits one.
const DEFAULT_MODEL_CANDIDATES: [(&str, &str); 8] = [
    ("anthropic", "claude-opus-5-5"),
    ("anthropic", "claude-sonnet-5-5"),
    ("openai", "gpt-6.2-astra"),
    ("grok", "grok-4.7"),
    ("commandcode", "claude-opus-5-5"),
    ("opencode", "claude-opus-5-5"),
    ("minimax", "MiniMax-M3.1-Flash"),
    ("neuralwatt", "nw-large"),
];

/// In-process Rust harness implementing [`Harness`] on `keel-core`.
pub struct PiHarness {
    profile: PiProfile,
    max_turns: usize,
}

pub type KeelHarness = PiHarness;

impl PiHarness {
    /// Keel's in-process harness.
    pub fn keel() -> Self {
        let max_turns = std::env::var("KEEL_MAX_TURNS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_TURNS);
        Self {
            profile: PiProfile::keel(),
            max_turns,
        }
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

    fn installed(&self) -> bool {
        true
    }

    fn executable(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn deterministic_turn_end(&self) -> bool {
        true
    }

    fn permission_modes(&self) -> &'static [PermissionMode] {
        &PermissionMode::ALL
    }

    fn description(&self) -> &'static str {
        self.profile.description
    }

    fn has_one_shot(&self, _model: &ModelId) -> bool {
        true
    }

    fn subagent_caps(&self) -> SubagentCaps {
        SubagentCaps {
            visibility: SubagentVisibility::Full,
        }
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        let loaded = load_catalog();
        for warning in &loaded.warnings {
            tracing::warn!(target: "keel_harness::native", warning = %warning, "catalog overlay warning");
        }
        let store = credential_store();
        Ok(list_models_for(&loaded.catalog, store.as_ref()))
    }

    async fn reasoning_levels(&self, model: &ModelId) -> Result<Vec<ReasoningLevel>, HarnessError> {
        let catalog = load_catalog().catalog;
        let store = credential_store();
        let resolver = secret_resolver();
        let (_, spec) = resolve_model(&catalog, resolver.as_ref(), store.as_ref(), &model.0)?;
        if !spec.compat().supports_reasoning {
            return Ok(Vec::new());
        }
        Ok(["minimal", "low", "medium", "high"]
            .into_iter()
            .map(|level| ReasoningLevel(level.to_string()))
            .collect())
    }

    async fn auth_status(&self) -> Vec<AuthAccount> {
        let catalog = load_catalog().catalog;
        let store = credential_store();
        build_auth_rows(&catalog, store.as_ref())
    }

    async fn sign_in(
        &self,
        provider: &str,
        on: &(dyn Fn(SignInEvent) + Send + Sync),
    ) -> Result<(), HarnessError> {
        let normalized = provider.trim().to_ascii_lowercase();
        if normalized.is_empty()
            || !normalized
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(HarnessError::Failed(format!(
                "unknown provider {provider:?}"
            )));
        }
        let store = credential_store().ok_or_else(|| {
            HarnessError::Failed("cannot locate keel credential store".to_string())
        })?;
        if matches!(normalized.as_str(), "xai" | "grok") {
            let mut config = OAuthProviderConfig::grok();
            apply_grok_oauth_env(&mut config)?;
            let http = default_http_client().map_err(failed)?;
            let tokens = match login_pkce(
                &http,
                &config,
                Duration::from_secs(300),
                |url| {
                    on(SignInEvent::Browser {
                        url: url.to_string(),
                    });
                },
            )
            .await
            {
                Ok(tokens) => tokens,
                Err(OAuthError::Bind { .. }) if config.device_authorize_url.is_some() => {
                    login_device(&http, &config, |prompt| {
                        on(SignInEvent::DeviceCode {
                            code: prompt.user_code.clone(),
                            url: prompt
                                .verification_uri_complete
                                .clone()
                                .unwrap_or_else(|| prompt.verification_uri.clone()),
                        });
                    })
                    .await
                    .map_err(failed)?
                }
                Err(err) => return Err(failed(err)),
            };
            store.set_oauth("grok", &tokens).map_err(failed)?;
            let _ = set_env_disabled(&store, "grok", false);
            return Ok(());
        }
        if normalized == "openai" {
            let mut config = OAuthProviderConfig::openai();
            apply_openai_oauth_env(&mut config)?;
            let http = default_http_client().map_err(failed)?;
            let tokens = login_pkce(
                &http,
                &config,
                Duration::from_secs(300),
                |url| {
                    on(SignInEvent::Browser {
                        url: url.to_string(),
                    });
                },
            )
            .await
            .map_err(failed)?;
            store.set_oauth("openai", &tokens).map_err(failed)?;
            let _ = set_env_disabled(&store, "openai", false);
            return Ok(());
        }

        let catalog = load_catalog().catalog;
        let catalog_id = canonical_provider_id(&normalized);
        let Some(entry) = catalog.provider(catalog_id) else {
            return Err(HarnessError::Failed(format!(
                "unknown provider `{provider}`"
            )));
        };
        let disabled_env = load_disabled_env(Some(&store));
        let env_vars = auth_env_vars(&entry.auth);
        if !disabled_env.contains(catalog_id)
            && let Some(secret) = env_vars
                .iter()
                .find_map(|name| std::env::var(name).ok())
                .filter(|val| !val.trim().is_empty())
        {
            store.set(catalog_id, &secret).map_err(failed)?;
            return Ok(());
        }
        if store.get(catalog_id).ok().flatten().is_some()
            || store.get_oauth(catalog_id).ok().flatten().is_some()
        {
            return Ok(());
        }
        let env_hint = if env_vars.is_empty() {
            format!("store key `{catalog_id}`")
        } else {
            format!("set {}", env_vars.join(" or "))
        };
        Err(HarnessError::Failed(format!(
            "{} uses an API key: {env_hint}, or add `{catalog_id}` in {}",
            entry.name,
            store.path().display()
        )))
    }

    async fn set_api_key(&self, provider: &str, api_key: &str) -> Result<(), HarnessError> {
        let store = credential_store().ok_or_else(|| {
            HarnessError::Failed("cannot locate keel credential store".to_string())
        })?;
        set_api_key_with(&store, provider, api_key)
    }

    async fn remove_credential(&self, provider: &str) -> Result<(), HarnessError> {
        let store = credential_store().ok_or_else(|| {
            HarnessError::Failed("cannot locate keel credential store".to_string())
        })?;
        remove_credential_with(&store, crate::pi_agent_dir(), provider)
    }

    async fn one_shot(&self, model: &ModelId, prompt: &str) -> Result<String, HarnessError> {
        let catalog = load_catalog().catalog;
        let store = credential_store();
        let resolver = secret_resolver();
        let (provider, model_spec) =
            resolve_model(&catalog, resolver.as_ref(), store.as_ref(), &model.0)?;
        let route = build_route_for_store(&provider, &model_spec, store.as_ref()).map_err(failed)?;
        let client = Client::with_resolver(resolver).map_err(failed)?;
        let mut session = Session::new(route, client).with_cache(CachePolicy::None);
        let outcome = tokio::time::timeout(crate::ONE_SHOT_LIMIT, session.step(Some(prompt)))
            .await
            .map_err(|_| HarnessError::Failed("Keel one-shot timed out".to_string()))?
            .map_err(failed)?;
        if let Some(err) = outcome.error {
            return Err(failed(err));
        }
        Ok(outcome.text())
    }

    async fn run(
        &self,
        spec: SessionSpec,
        controls: RunControls,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<AgentEvent, HarnessError>> + Send>>, HarnessError>
    {
        let catalog = load_catalog().catalog;
        let store = credential_store();
        let resolver = secret_resolver();
        let (provider, model_spec) =
            resolve_model(&catalog, resolver.as_ref(), store.as_ref(), &spec.model.0)?;
        let route = build_route_for_store(&provider, &model_spec, store.as_ref()).map_err(failed)?;
        let client = Client::with_resolver(resolver).map_err(failed)?;
        let workspace = Workspace::open(&spec.cwd).map_err(failed)?;

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let max_turns = self.max_turns;
        tokio::spawn(async move {
            if let Err(err) = run_session(
                spec,
                controls,
                provider,
                model_spec,
                route,
                client,
                workspace,
                max_turns,
                event_tx.clone(),
            )
            .await
            {
                let _ = event_tx.send(Err(err));
            }
        });

        Ok(Box::pin(stream::unfold(event_rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })))
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    spec: SessionSpec,
    controls: RunControls,
    provider: Provider,
    model_spec: ModelSpec,
    route: keel_llm_route::Route,
    client: Client,
    workspace: Workspace,
    max_turns: usize,
    event_tx: mpsc::UnboundedSender<Result<AgentEvent, HarnessError>>,
) -> Result<(), HarnessError> {
    let data_dir = spec.data_dir.as_deref();
    if let Some(dir) = data_dir {
        ensure_builtin_skills(dir);
    }

    let mut registry = build_builtin_registry();
    let skills = discover_skills(&spec.cwd, data_dir, spec.scratch);
    skills.register(&mut registry);

    let memory_socket = data_dir.filter(|dir| {
        dir.join("ipc.token").is_file() && dir.join("keel.sock").exists()
    });
    if let Some(dir) = memory_socket {
        registry.register(MemoryReadTool::new(dir.to_path_buf(), spec.cwd.clone()));
        registry.register(MemoryWriteTool::new(dir.to_path_buf(), spec.cwd.clone()));
    }

    let has_ui_tools = connect_keel_mcps(&mut registry, &spec).await;

    let mut session = Session::new(route, client).with_cache(cache_policy_from_env());
    if let Some(effort) = spec
        .reasoning
        .as_ref()
        .and_then(|r| parse_reasoning(&r.0))
        .filter(|_| model_spec.compat().supports_reasoning)
    {
        session = session.with_reasoning(effort);
    }

    let sessions_dir = data_dir
        .map(KeelProfile::sessions_dir)
        .unwrap_or_else(|| std::env::temp_dir().join("keel-sessions"));

    let mut resumed_session = try_resume_session(&sessions_dir, spec.resume.as_ref(), &mut session);
    let has_stored_system = resumed_session
        .as_ref()
        .is_some_and(|opened| opened.has_stored_system);
    if !has_stored_system {
        let initial_memory = match memory_socket {
            Some(dir) => Some(fetch_memory_prompt(dir, &spec.cwd).await),
            None => None,
        };
        let project_instructions = (!spec.scratch && !env_flag("KEEL_NO_INSTRUCTIONS"))
            .then(|| ProjectInstructions::discover(&spec.cwd));
        if let Some(instructions) = &project_instructions {
            for warning in instructions.warnings() {
                tracing::warn!(
                    target: "keel_harness::native",
                    warning = %warning,
                    "project instructions warning"
                );
            }
            if !instructions.is_empty() {
                tracing::info!(
                    target: "keel_harness::native",
                    count = instructions.len(),
                    bytes = instructions.total_bytes(),
                    "loaded project instructions"
                );
            }
        }
        configure_fresh_system_prompt(
            &mut session,
            KeelProfile::default().system_prompt.as_deref(),
            has_ui_tools,
            spec.scratch,
            spec.project.as_ref(),
            project_instructions.as_ref(),
            initial_memory.as_deref(),
        );
    }

    let (session_log, resumed_from_log, resumed) = match resumed_session.take() {
        Some(opened) => (opened.log, opened.id, true),
        None => {
            let (log, id) =
                create_session_log(&sessions_dir, &provider, &model_spec, &spec.cwd, &session)?;
            (log, id, false)
        }
    };
    let log_arc = Arc::new(session_log);
    if let Some(report) = &spec.report_session {
        report(
            resumed_from_log.clone(),
            Some(log_arc.path().to_string_lossy().into_owned()),
        );
    }
    session = session.with_sink(Arc::clone(&log_arc) as Arc<dyn keel_core::SessionSink>);

    if resumed {
        if let Some(dir) = memory_socket {
            let current = fetch_memory_prompt(dir, &spec.cwd).await;
            let last_known = last_known_memory(&session);
            if should_emit_memory_update(last_known.as_deref(), current.trim()) {
                session.push_message(Message::user(format!(
                    "<keel_memory_update>\n{}\n</keel_memory_update>",
                    current.trim()
                )));
            }
        }
        if !spec.plan_mode && history_ends_in_plan_mode(&session.history) {
            session.push_message(Message::user(format!(
                "[plan mode]\n{}",
                keel_core::PLAN_MODE_OFF_NOTICE
            )));
        }
    }

    let (inline_imgs, unread_imgs) = crate::inline_images(&spec.attachments);
    if !inline_imgs.is_empty() {
        let mut msg = Message::new(Role::User);
        for (file, data) in inline_imgs {
            msg.parts.push(Part::text(format!("Attached image: {}", file.name)));
            msg.parts.push(Part::image(file.mime.clone(), data));
        }
        session.push_message(msg);
    }

    let mut initial_prompt = normalize_skill_invocation(&spec.initial_text);
    initial_prompt.push_str(&crate::attachment_note(&unread_imgs));
    if initial_prompt.trim().is_empty() && !spec.attachments.is_empty() {
        initial_prompt = "See the attached image(s).".to_string();
    }

    let cancel = CancellationToken::new();
    let steers = Arc::new(Mutex::new(VecDeque::<String>::new()));
    let (gate, perm_rx) = if spec.permission == PermissionMode::FullAccess && !spec.plan_mode {
        (PermissionGate::AllowAll, None)
    } else {
        let (gate, rx) = PermissionGate::ask();
        (gate, Some(rx))
    };

    let scratch_cwd = spec.scratch.then(|| spec.cwd.clone());
    let router_task = tokio::spawn(control_router(
        controls.inbound,
        perm_rx,
        cancel.clone(),
        Arc::clone(&steers),
        event_tx.clone(),
        spec.permission,
        spec.plan_mode,
        scratch_cwd,
    ));

    let hooks = if env_flag("KEEL_HOOKS") {
        HookConfig::load(&spec.cwd)
    } else {
        HookConfig::empty()
    };

    let subagent_catalog = if spec.scratch {
        SubagentCatalog::builtins()
    } else {
        SubagentCatalog::discover(&spec.cwd)
    };
    let mut subagent_config = SubagentConfig::new(subagent_catalog)
        .with_model_catalog(Arc::new(load_catalog().catalog))
        .with_log_dir(log_arc.subagent_dir())
        .with_allow_worktree(!spec.scratch);
    if let Some(dir) = data_dir {
        subagent_config = subagent_config
            .with_worktree_dir(KeelProfile::home(dir).join("worktrees").join(&resumed_from_log));
    }

    let mut harness = CoreHarness::new(session, Arc::new(registry), workspace)
        .with_permissions(gate)
        .with_skills(skills)
        .with_hooks(hooks)
        .with_hook_context(resumed_from_log, if resumed { "resume" } else { "startup" })
        .with_session_state(Arc::clone(&log_arc) as Arc<dyn keel_tools::SessionState>)
        .with_subagents(subagent_config)
        .with_max_turns(max_turns)
        .with_cancellation(cancel.clone())
        .with_sandbox(env_flag("KEEL_SANDBOX"));
    if let Some(plan_prompt) =
        keel_plan_mode_prompt(has_ui_tools, keel_tools::plan_sandbox_supported())
    {
        harness = harness.with_plan_mode_prompt(plan_prompt);
    }
    harness = harness.with_plan_mode(spec.plan_mode);

    let full_model_id = format!("{}/{}", provider.id, model_spec.id);
    let context_window = model_spec.context_window;
    let cwd_for_diffs = spec.cwd.clone();

    let mut next_prompt = Some(initial_prompt);
    let mut final_status: Option<TurnStatus> = None;

    while let Some(prompt) = next_prompt.take() {
        let mut translator = EventTranslator::new(
            event_tx.clone(),
            full_model_id.clone(),
            context_window,
            cwd_for_diffs.clone(),
        );
        let outcome = harness
            .run(&prompt, |event| translator.on_event(event))
            .await;
        match outcome {
            Ok(outcome) => match outcome.stop {
                RunStop::EndTurn => {
                    let queued = steers.lock().unwrap_or_else(|p| p.into_inner()).pop_front();
                    if let Some(steer_text) = queued {
                        next_prompt = Some(normalize_skill_invocation(&steer_text));
                    } else {
                        final_status = Some(TurnStatus::Completed {});
                    }
                }
                RunStop::MaxTurns | RunStop::TaskTimeout | RunStop::Stalled(_) | RunStop::PrematureFinish(_) => {
                    final_status = Some(TurnStatus::Completed {});
                }
                RunStop::Cancelled => {
                    final_status = Some(TurnStatus::Interrupted {});
                }
                RunStop::Failed => {
                    let msg = outcome
                        .last
                        .as_ref()
                        .and_then(|m| m.error.clone())
                        .or(outcome.stop_detail)
                        .unwrap_or_else(|| "turn failed".to_string());
                    final_status = Some(TurnStatus::Errored { message: msg });
                }
            },
            Err(err) => {
                final_status = Some(TurnStatus::Errored {
                    message: err.to_string(),
                });
            }
        }
    }

    router_task.abort();
    if let Some(status) = final_status {
        let _ = event_tx.send(Ok(AgentEvent::Done { status }));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn control_router(
    mut inbound: mpsc::UnboundedReceiver<ControlMsg>,
    mut perm_rx: Option<mpsc::Receiver<PermissionRequest>>,
    cancel: CancellationToken,
    steers: Arc<Mutex<VecDeque<String>>>,
    event_tx: mpsc::UnboundedSender<Result<AgentEvent, HarnessError>>,
    permission: PermissionMode,
    plan_mode: bool,
    scratch_cwd: Option<PathBuf>,
) {
    let mut pending_questions: HashMap<String, oneshot::Sender<PermissionDecision>> = HashMap::new();
    let mut next_q_id: u64 = 0;
    let mut inbound_open = true;

    loop {
        let msg = tokio::select! {
            biased;
            msg = inbound.recv(), if inbound_open => ControlSelect::Inbound(msg),
            req = async {
                match perm_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => ControlSelect::Permission(req),
        };

        match msg {
            ControlSelect::Inbound(Some(ControlMsg::Interrupt)) => {
                cancel.cancel();
                for (_, tx) in pending_questions.drain() {
                    let _ = tx.send(PermissionDecision::deny("cancelled"));
                }
            }
            ControlSelect::Inbound(Some(ControlMsg::Answer { answers })) => {
                for ans in answers {
                    if let Some(tx) = pending_questions.remove(&ans.question_id) {
                        let allow = matches!(
                            ans.value.as_str(),
                            "yes" | "true" | "confirm" | "allow" | "allow_once" | "allow_always"
                        );
                        let decision = if allow {
                            PermissionDecision::Allow
                        } else {
                            PermissionDecision::deny("not approved")
                        };
                        let _ = tx.send(decision);
                    }
                }
            }
            ControlSelect::Inbound(Some(ControlMsg::Steer { text })) => {
                if !text.trim().is_empty() {
                    steers
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push_back(text);
                }
            }
            ControlSelect::Inbound(None) => {
                inbound_open = false;
                if perm_rx.is_none() {
                    break;
                }
            }
            ControlSelect::Permission(Some(req)) => {
                if let Some(decision) =
                    auto_permission(&req, permission, plan_mode, scratch_cwd.as_deref())
                {
                    req.respond(decision);
                } else {
                    next_q_id += 1;
                    let q_id = format!("perm-{next_q_id}");
                    let prompt = match parse_subagent_permission_prefix(&req.summary) {
                        Some((agent_name, _)) => {
                            format!("Allow {}? ({agent_name} subagent)", req.tool)
                        }
                        None => format!("Allow {}?", req.tool),
                    };
                    let tool_ask = ToolAsk::from_input(&req.tool, None, &req.arguments);
                    let question = UserInputQuestion {
                        id: q_id.clone(),
                        kind: QuestionKind::Confirm,
                        prompt,
                        options: vec![
                            QuestionOption::new("yes", "Allow").with_role(OptionRole::AllowOnce),
                            QuestionOption::new("no", "Deny").with_role(OptionRole::RejectOnce),
                        ],
                        required: true,
                        tool: Some(tool_ask),
                        multi: false,
                        other: false,
                    };
                    pending_questions.insert(q_id.clone(), req.responder);
                    let _ = event_tx.send(Ok(AgentEvent::Question {
                        id: q_id,
                        questions: vec![question],
                        subagent: None,
                    }));
                }
            }
            ControlSelect::Permission(None) => {
                perm_rx = None;
                if !inbound_open {
                    break;
                }
            }
        }
    }
}

enum ControlSelect {
    Inbound(Option<ControlMsg>),
    Permission(Option<PermissionRequest>),
}

/// Parse the `[agent:<name>#<id>] ` prefix attached by `keel-core` when a
/// subagent requests tool permission through the parent gate.
fn parse_subagent_permission_prefix(summary: &str) -> Option<(&str, &str)> {
    let rest = summary.strip_prefix("[agent:")?;
    let end = rest.find(']')?;
    let inner = &rest[..end];
    let (name, id) = inner.split_once('#')?;
    (!name.is_empty() && !id.is_empty()).then_some((name, id))
}

fn auto_permission(
    req: &PermissionRequest,
    permission: PermissionMode,
    plan_mode: bool,
    scratch_cwd: Option<&Path>,
) -> Option<PermissionDecision> {
    let base = crate::tool_base(&req.tool);
    if plan_mode {
        if base == "bash" && keel_tools::plan_sandbox_supported() {
            return (permission == PermissionMode::FullAccess)
                .then_some(PermissionDecision::Allow);
        }
        let allow = crate::plan_mode_allows(&req.tool)
            || matches!(
                base.as_str(),
                "view_image" | "skill" | "browser_open_pane" | AGENT_TOOL_NAME
            );
        return Some(if allow {
            PermissionDecision::Allow
        } else {
            PermissionDecision::deny(format!("{} is off in plan mode", req.tool))
        });
    }
    if permission == PermissionMode::FullAccess {
        return Some(PermissionDecision::Allow);
    }
    if matches!(
        base.as_str(),
        "read"
            | "glob"
            | "search"
            | "view_image"
            | "skill"
            | "memory_read"
            | "memory_write"
            | "web_search"
            | "web_fetch"
            | AGENT_TOOL_NAME
    ) || keel_proto::is_ui_tool(&req.tool)
        || (crate::is_browser_tool(&req.tool)
            && (crate::plan_mode_allows(&req.tool) || base == "browser_open_pane"))
    {
        return Some(PermissionDecision::Allow);
    }
    if permission == PermissionMode::AutoEdit
        && matches!(base.as_str(), "write" | "edit" | "apply_patch")
    {
        return Some(PermissionDecision::Allow);
    }
    if let Some(cwd) = scratch_cwd
        && matches!(base.as_str(), "write" | "edit")
        && let Some(raw_path) = req
            .arguments
            .get("path")
            .or_else(|| req.arguments.get("file_path"))
            .and_then(Value::as_str)
        && path_inside_dir(cwd, raw_path)
    {
        return Some(PermissionDecision::Allow);
    }
    None
}

fn path_inside_dir(root: &Path, raw: &str) -> bool {
    let candidate = Path::new(raw);
    let target = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let Ok(canon_root) = fs::canonicalize(root) else {
        return false;
    };
    let mut cur = target.as_path();
    let mut tail = Vec::new();
    let resolved = loop {
        if let Ok(real) = fs::canonicalize(cur) {
            let mut out = real;
            for seg in tail.into_iter().rev() {
                out.push(seg);
            }
            break out;
        }
        let Some(name) = cur.file_name() else {
            return false;
        };
        tail.push(name.to_os_string());
        let Some(parent) = cur.parent() else {
            return false;
        };
        cur = parent;
    };
    resolved == canon_root || resolved.starts_with(&canon_root)
}

struct EventTranslator {
    tx: mpsc::UnboundedSender<Result<AgentEvent, HarnessError>>,
    model_id: String,
    context_window: u32,
    cwd: PathBuf,
    turn_seq: u64,
    turn_usage: Option<keel_llm_types::Usage>,
    labels: LabelStream,
    pending_diffs: HashMap<String, PendingDiff>,
    pending_agent_calls: HashSet<String>,
    spawned_agent_calls: HashSet<String>,
    subagent_turns: HashMap<String, u32>,
}

impl EventTranslator {
    fn new(
        tx: mpsc::UnboundedSender<Result<AgentEvent, HarnessError>>,
        model_id: String,
        context_window: u32,
        cwd: PathBuf,
    ) -> Self {
        Self {
            tx,
            model_id,
            context_window,
            cwd,
            turn_seq: 0,
            turn_usage: None,
            labels: LabelStream::default(),
            pending_diffs: HashMap::new(),
            pending_agent_calls: HashSet::new(),
            spawned_agent_calls: HashSet::new(),
            subagent_turns: HashMap::new(),
        }
    }

    fn emit(&self, event: AgentEvent) {
        let _ = self.tx.send(Ok(event));
    }

    fn current_message_id(&self) -> String {
        format!("keel-msg-{}", self.turn_seq.max(1))
    }

    fn on_event(&mut self, event: RunEvent<'_>) {
        match event {
            RunEvent::Model(llm) => match llm {
                LlmEvent::Start { .. } => {
                    self.turn_seq += 1;
                    self.turn_usage = None;
                    self.labels = LabelStream::default();
                }
                LlmEvent::TextDelta { index, delta } => {
                    if !delta.is_empty() {
                        self.emit(AgentEvent::Text {
                            message_id: self.current_message_id(),
                            part_id: format!("text-{index}"),
                            delta: delta.clone(),
                            subagent: None,
                        });
                    }
                }
                LlmEvent::ReasoningDelta { index, delta } => {
                    if !delta.is_empty() {
                        self.emit(AgentEvent::Reasoning {
                            message_id: self.current_message_id(),
                            part_id: format!("think-{index}"),
                            delta: delta.clone(),
                            subagent: None,
                        });
                    }
                }
                LlmEvent::ToolInputDelta {
                    index,
                    call_id,
                    delta,
                } => {
                    if let Some(ev) = self.labels.on_delta(*index as u64, call_id.as_str(), delta) {
                        self.emit(ev);
                    }
                }
                LlmEvent::Usage(usage) => {
                    self.turn_usage = Some(*usage);
                }
                _ => {}
            },
            RunEvent::TurnFinished { .. } => {
                if let Some(u) = self.turn_usage.take()
                    && u.total() > 0
                {
                    self.emit(AgentEvent::Usage {
                        usage: to_proto_usage(&self.model_id, u, Some(self.context_window)),
                    });
                }
            }
            RunEvent::Compacted { summary_usage, .. } => {
                if summary_usage.total() > 0 {
                    self.emit(AgentEvent::Usage {
                        usage: to_proto_usage(&self.model_id, summary_usage, None),
                    });
                }
            }
            RunEvent::ToolStarted {
                call_id,
                name,
                summary: _,
                arguments,
            } => {
                // `agent` calls render as dedicated `PartView::Subagent` rows via
                // `SubagentEvent::Started`; suppress the redundant parent tool row.
                if name == AGENT_TOOL_NAME {
                    self.pending_agent_calls.insert(call_id.to_owned());
                    return;
                }
                let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                let mut detail = crate::tool_detail_for(name, &args);
                if detail.is_empty() && name == "apply_patch" {
                    detail = patch_first_path(args.get("patch").and_then(Value::as_str));
                }
                let label = crate::tool_label(&args);
                if !label.is_empty()
                    && let Some(ev) = self.labels.emit_if_new(call_id, label.clone())
                {
                    self.emit(ev);
                }
                self.record_pending_diff(call_id, name, &args);
                self.emit(AgentEvent::ToolStarted {
                    tool_id: call_id.to_owned(),
                    name: name.to_owned(),
                    input_bytes: arguments.len() as u64,
                    detail,
                    label,
                    subagent: None,
                    kind: crate::kind_from_name(name).map(str::to_string),
                });
            }
            RunEvent::ToolFinished {
                call_id,
                name,
                is_error,
                preview: _,
                output,
                images,
                duration_ms,
            } => {
                if self.pending_agent_calls.remove(call_id) {
                    let spawned = self.spawned_agent_calls.remove(call_id);
                    // Only surface a raw tool error row if the `agent` tool failed
                    // before spawning a child (e.g. invalid arguments / unknown role).
                    if !is_error || spawned {
                        return;
                    }
                    self.emit(AgentEvent::ToolStarted {
                        tool_id: call_id.to_owned(),
                        name: name.to_owned(),
                        input_bytes: 0,
                        detail: String::new(),
                        label: String::new(),
                        subagent: None,
                        kind: crate::kind_from_name(name).map(str::to_string),
                    });
                }
                let diff = if !is_error {
                    self.finish_pending_diff(call_id)
                } else {
                    self.pending_diffs.remove(call_id);
                    None
                };
                let mut all_images: Vec<String> = images
                    .iter()
                    .map(|img| format!("data:{};base64,{}", img.mime, img.data))
                    .collect();
                all_images.extend(crate::saved_paths(output));
                self.emit(AgentEvent::ToolFinished {
                    tool_id: call_id.to_owned(),
                    summary: summary_line(output, is_error),
                    output_bytes: output.len() as u64,
                    is_error,
                    images: all_images,
                    output_tail: keel_proto::output_tail(output),
                    subagent: None,
                    diff,
                    duration_ms,
                });
            }
            RunEvent::Subagent(ev) => self.on_subagent_event(ev),
            RunEvent::Retrying { .. } | RunEvent::Progress { .. } | RunEvent::VerifyRequested { .. } => {}
        }
    }

    fn on_subagent_event(&mut self, event: &SubagentEvent) {
        match event {
            SubagentEvent::Started {
                id,
                parent_call_id,
                agent,
                description,
                depth: _,
                model,
                read_only,
                resumed: _,
                worktree: _,
                worktree_branch,
            } => {
                let tool_id = if let Some(cid) = parent_call_id {
                    self.spawned_agent_calls.insert(cid.clone());
                    cid.clone()
                } else {
                    id.clone()
                };
                self.subagent_turns.entry(id.clone()).or_insert(0);
                self.emit(AgentEvent::SubagentStarted {
                    tool_id,
                    subagent_id: id.clone(),
                    name: agent.clone(),
                    description: description.clone(),
                });
                self.emit(AgentEvent::SubagentMeta {
                    subagent_id: id.clone(),
                    model: Some(model.clone()),
                    read_only: Some(*read_only),
                    turns: Some(0),
                    input_tokens: None,
                    output_tokens: None,
                    duration_ms: None,
                    worktree_branch: worktree_branch.clone(),
                    active_tool: Some(String::new()),
                });
            }
            SubagentEvent::TextDelta {
                id,
                agent: _,
                index,
                delta,
            } => {
                if !delta.is_empty() {
                    let turn = self.subagent_turns.get(id).copied().unwrap_or(0);
                    self.emit(AgentEvent::Text {
                        message_id: format!("sa-{id}-t{turn}"),
                        part_id: format!("text-{index}"),
                        delta: delta.clone(),
                        subagent: Some(SubagentRef { id: id.clone() }),
                    });
                }
            }
            SubagentEvent::ReasoningDelta {
                id,
                agent: _,
                index,
                delta,
            } => {
                if !delta.is_empty() {
                    let turn = self.subagent_turns.get(id).copied().unwrap_or(0);
                    self.emit(AgentEvent::Reasoning {
                        message_id: format!("sa-{id}-t{turn}"),
                        part_id: format!("think-{index}"),
                        delta: delta.clone(),
                        subagent: Some(SubagentRef { id: id.clone() }),
                    });
                }
            }
            SubagentEvent::ToolStarted {
                id,
                agent: _,
                call_id,
                tool,
                summary,
                arguments,
            } => {
                let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                let mut detail = crate::tool_detail_for(tool, &args);
                if detail.is_empty() && tool == "apply_patch" {
                    detail = patch_first_path(args.get("patch").and_then(Value::as_str));
                }
                if detail.is_empty() && !summary.is_empty() && summary != tool {
                    detail = summary
                        .strip_prefix(tool.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or(summary)
                        .to_string();
                }
                let label = crate::tool_label(&args);
                let kind = crate::kind_from_name(tool).map(str::to_string);
                self.emit(AgentEvent::ToolStarted {
                    tool_id: call_id.clone(),
                    name: tool.clone(),
                    input_bytes: arguments.len() as u64,
                    detail,
                    label,
                    subagent: Some(SubagentRef { id: id.clone() }),
                    kind,
                });
            }
            SubagentEvent::ToolFinished {
                id,
                agent: _,
                call_id,
                tool: _,
                is_error,
                output,
                duration_ms,
            } => {
                self.emit(AgentEvent::ToolFinished {
                    tool_id: call_id.clone(),
                    summary: summary_line(output, *is_error),
                    output_bytes: output.len() as u64,
                    is_error: *is_error,
                    images: Vec::new(),
                    output_tail: keel_proto::output_tail(output),
                    subagent: Some(SubagentRef { id: id.clone() }),
                    diff: None,
                    duration_ms: *duration_ms,
                });
            }
            SubagentEvent::TurnFinished {
                id,
                agent: _,
                turn,
                stop: _,
            } => {
                let turns = *turn as u32;
                self.subagent_turns.insert(id.clone(), turns);
                self.emit(AgentEvent::SubagentMeta {
                    subagent_id: id.clone(),
                    model: None,
                    read_only: None,
                    turns: Some(turns),
                    input_tokens: None,
                    output_tokens: None,
                    duration_ms: None,
                    worktree_branch: None,
                    active_tool: None,
                });
            }
            SubagentEvent::Finished {
                id,
                agent: _,
                model,
                stop,
                turns,
                usage,
                duration_ms,
                worktree: _,
                worktree_branch,
            } => {
                self.subagent_turns.remove(id);
                if usage.total() > 0 {
                    // `context_window: None` rolls subagent spend into chat totals
                    // without overwriting the parent's context meter.
                    self.emit(AgentEvent::Usage {
                        usage: to_proto_usage(model, *usage, None),
                    });
                }
                self.emit(AgentEvent::SubagentMeta {
                    subagent_id: id.clone(),
                    model: Some(model.clone()),
                    read_only: None,
                    turns: Some(*turns as u32),
                    input_tokens: Some(usage.input_tokens),
                    output_tokens: Some(usage.output_tokens),
                    duration_ms: Some(*duration_ms),
                    worktree_branch: worktree_branch.clone(),
                    active_tool: Some(String::new()),
                });
                let status = match stop {
                    RunStop::Cancelled => TurnStatus::Interrupted {},
                    RunStop::Failed => TurnStatus::Errored {
                        message: "subagent failed".to_string(),
                    },
                    _ => TurnStatus::Completed {},
                };
                self.emit(AgentEvent::SubagentDone {
                    subagent_id: id.clone(),
                    status,
                });
            }
        }
    }

    fn record_pending_diff(&mut self, call_id: &str, name: &str, args: &Value) {
        match name {
            "edit" => {
                let Some(path) = args.get("path").and_then(Value::as_str) else {
                    return;
                };
                let old_text = args
                    .get("old_string")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let new_text = args
                    .get("new_string")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.pending_diffs.insert(
                    call_id.to_owned(),
                    PendingDiff::Edit {
                        path: path.to_owned(),
                        old_text,
                        new_text,
                    },
                );
            }
            "write" => {
                let Some(path) = args.get("path").and_then(Value::as_str) else {
                    return;
                };
                let full_path = self.cwd.join(path);
                let old_text = read_bounded_text(&full_path);
                let new_text = args
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.pending_diffs.insert(
                    call_id.to_owned(),
                    PendingDiff::Write {
                        path: path.to_owned(),
                        full_path,
                        old_text,
                        new_text,
                    },
                );
            }
            "apply_patch" => {
                let Some(patch) = args.get("patch").and_then(Value::as_str) else {
                    return;
                };
                let path = patch_first_path(Some(patch));
                self.pending_diffs.insert(
                    call_id.to_owned(),
                    PendingDiff::Patch {
                        path,
                        patch: patch.to_owned(),
                    },
                );
            }
            _ => {}
        }
    }

    fn finish_pending_diff(&mut self, call_id: &str) -> Option<ToolDiff> {
        match self.pending_diffs.remove(call_id)? {
            PendingDiff::Edit {
                path,
                old_text,
                new_text,
            } => Some(ToolDiff {
                path,
                old_text: Some(old_text),
                new_text,
            }),
            PendingDiff::Write {
                path,
                full_path,
                old_text,
                new_text,
            } => {
                let final_new = read_bounded_text(&full_path).unwrap_or(new_text);
                Some(ToolDiff {
                    path,
                    old_text,
                    new_text: final_new,
                })
            }
            PendingDiff::Patch { path, patch } => Some(ToolDiff {
                path,
                old_text: None,
                new_text: patch,
            }),
        }
    }
}

enum PendingDiff {
    Edit {
        path: String,
        old_text: String,
        new_text: String,
    },
    Write {
        path: String,
        full_path: PathBuf,
        old_text: Option<String>,
        new_text: String,
    },
    Patch {
        path: String,
        patch: String,
    },
}

fn read_bounded_text(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_DIFF_SNAPSHOT_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn patch_first_path(patch: Option<&str>) -> String {
    let Some(patch) = patch else {
        return String::new();
    };
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let trimmed = rest.split('\t').next().unwrap_or(rest).trim();
            let cleaned = trimmed.strip_prefix("b/").unwrap_or(trimmed);
            if !cleaned.is_empty() && cleaned != "/dev/null" {
                return cleaned.to_string();
            }
        }
    }
    String::new()
}

fn to_proto_usage(
    model_id: &str,
    u: keel_llm_types::Usage,
    context_window: Option<u32>,
) -> TokenUsage {
    let context_used = context_window.map(|_| {
        u.input_tokens
            .saturating_add(u.cache_read_tokens)
            .saturating_add(u.cache_write_tokens)
            .saturating_add(u.output_tokens)
    });
    TokenUsage {
        model: model_id.to_string(),
        input: u.input_tokens,
        output: u.output_tokens,
        cache_read: u.cache_read_tokens,
        cache_write: u.cache_write_tokens,
        cache_warm: 0,
        context_used,
        context_window: context_window.filter(|w| *w > 0).map(u64::from),
    }
}

fn summary_line(output: &str, is_error: bool) -> String {
    let line = output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(if is_error { "error" } else { "done" });
    if line.chars().count() <= 160 {
        line.to_string()
    } else {
        let head: String = line.chars().take(159).collect();
        format!("{head}…")
    }
}

/// Accumulates streaming tool-call argument JSON so a `label` can fire as
/// soon as its string is complete.
#[derive(Default)]
struct LabelStream {
    by_index: HashMap<u64, (String, String)>,
    emitted: HashSet<String>,
}

impl LabelStream {
    fn on_delta(&mut self, index: u64, call_id: &str, delta: &str) -> Option<AgentEvent> {
        let entry = self
            .by_index
            .entry(index)
            .or_insert_with(|| (call_id.to_string(), String::new()));
        if entry.0.is_empty() && !call_id.is_empty() {
            entry.0 = call_id.to_string();
        }
        entry.1.push_str(delta);
        let label = extract_streaming_label(&entry.1);
        let id = entry.0.clone();
        self.emit_if_new(&id, label)
    }

    fn emit_if_new(&mut self, id: &str, label: String) -> Option<AgentEvent> {
        if id.is_empty() || label.is_empty() || !self.emitted.insert(id.to_string()) {
            return None;
        }
        Some(AgentEvent::ToolLabel {
            tool_id: id.to_string(),
            label,
            subagent: None,
        })
    }
}

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
            let next = chars.next()?;
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
    None
}

// ---------------------------------------------------------------------------
// Tools, Skills, MCP & Memory Helpers
// ---------------------------------------------------------------------------

fn build_builtin_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(LabelledTool(builtin::read::ReadTool));
    registry.register(LabelledTool(builtin::write::WriteTool));
    registry.register(LabelledTool(builtin::edit::EditTool));
    registry.register(LabelledTool(builtin::apply_patch::ApplyPatchTool));
    registry.register(LabelledTool(builtin::bash::BashTool::new()));
    registry.register(LabelledTool(builtin::glob::GlobTool));
    registry.register(LabelledTool(builtin::search::SearchTool));
    registry.register(LabelledTool(builtin::view_image::ViewImageTool));
    let tavily_resolver: builtin::web_search::KeyResolver = Arc::new(|| {
        credential_store().and_then(|store| store.get("tavily").ok().flatten())
    });
    registry.register(LabelledTool(
        builtin::web_search::WebSearchTool::with_key_resolver(tavily_resolver),
    ));
    registry.register(LabelledTool(builtin::web_fetch::WebFetchTool::new()));
    #[cfg(unix)]
    registry.register(LabelledTool(builtin::jobs::JobsTool::new()));
    registry
}

/// Adds an optional `label` field to tools and strips it before execution.
struct LabelledTool<T>(T);

impl<T: Tool> Tool for LabelledTool<T> {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn spec(&self) -> ToolSpec {
        let mut spec = self.0.spec();
        if let Some(props) = spec
            .input_schema
            .get_mut("properties")
            .and_then(Value::as_object_mut)
        {
            props.insert(
                "label".to_string(),
                json!({
                    "type": "string",
                    "description": LABEL_DESC,
                }),
            );
        }
        spec
    }

    fn summarize(&self, arguments: &Value) -> String {
        self.0.summarize(arguments)
    }

    fn parallel_safe(&self) -> bool {
        self.0.parallel_safe()
    }

    fn read_only(&self) -> bool {
        self.0.read_only()
    }

    fn allowed_in_plan_mode(&self) -> bool {
        self.0.allowed_in_plan_mode()
    }

    fn timeout(&self, limits: &keel_tools::ToolLimits) -> Option<Duration> {
        self.0.timeout(limits)
    }

    fn execute<'a>(&'a self, mut arguments: Value, context: &'a ToolContext) -> ToolFuture<'a> {
        if let Some(obj) = arguments.as_object_mut() {
            obj.remove("label");
        }
        self.0.execute(arguments, context)
    }
}

fn ensure_builtin_skills(data_dir: &Path) {
    let skills_root = KeelProfile::home(data_dir).join("builtin-skills");
    let skill_file = skills_root.join("indonesian-natural").join("SKILL.md");
    if fs::read_to_string(&skill_file).ok().as_deref() != Some(INDONESIAN_SKILL) {
        let _ = crate::write_private(&skill_file, INDONESIAN_SKILL);
    }
}

fn discover_skills(cwd: &Path, data_dir: Option<&Path>, scratch: bool) -> SkillCatalog {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
    {
        roots.push(home.join(".agents/skills"));
        roots.push(home.join(".claude/skills"));
    }
    if let Some(agent) = crate::pi_agent_dir() {
        roots.push(agent.join("skills"));
    }
    if let Some(dir) = data_dir {
        roots.push(KeelProfile::home(dir).join("builtin-skills"));
    }
    if !scratch {
        roots.push(cwd.join(".pi/skills"));
        roots.push(cwd.join(".agents/skills"));
        roots.push(cwd.join(".claude/skills"));
    }
    SkillCatalog::discover_roots(roots.iter().map(PathBuf::as_path))
}

fn normalize_skill_invocation(text: &str) -> String {
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix("/skill:") else {
        return text.to_string();
    };
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..end];
    if name.is_empty() {
        return text.to_string();
    }
    let tail = rest[end..].trim();
    if tail.is_empty() {
        format!("Invoke the `skill` tool with `skill: \"{name}\"` and follow its instructions.")
    } else {
        format!(
            "Invoke the `skill` tool with `skill: \"{name}\"` and follow its instructions.\n\n{tail}"
        )
    }
}

async fn connect_keel_mcps(registry: &mut ToolRegistry, spec: &SessionSpec) -> bool {
    let mut servers = BTreeMap::new();
    if let Some(ui) = crate::ui_mcp()
        && is_keel_mcp_exe(&ui.command)
    {
        servers.insert(
            crate::UI_MCP_NAME.to_string(),
            ServerConfig {
                command: ui.command,
                args: ui.args,
                enabled: true,
                env: Vec::new(),
                parallel_safe: true,
                startup_timeout_ms: 10_000,
                url: None,
                headers: BTreeMap::new(),
                env_vars: ui.env.into_iter().collect(),
            },
        );
    }
    if let Some(browser) =
        crate::browser_mcp(spec.browser_sock.as_deref(), spec.browser_chat.as_deref())
        && is_keel_mcp_exe(&browser.command)
    {
        servers.insert(
            crate::BROWSER_MCP_NAME.to_string(),
            ServerConfig {
                command: browser.command,
                args: browser.args,
                enabled: true,
                env: Vec::new(),
                parallel_safe: false,
                startup_timeout_ms: 10_000,
                url: None,
                headers: BTreeMap::new(),
                env_vars: browser.env.into_iter().collect(),
            },
        );
    }
    if servers.is_empty() {
        return false;
    }
    let config = McpConfig {
        servers,
        warnings: Vec::new(),
    };
    let reserved: Vec<String> = registry.names().into_iter().map(str::to_string).collect();
    let setup = config.connect(&reserved, Some(&spec.cwd)).await;
    for warning in &setup.warnings {
        tracing::warn!(target: "keel_harness::native", warning = %warning, "MCP setup warning");
    }
    let mut has_ui = false;
    for tool in setup.tools {
        let is_ui = keel_proto::is_ui_tool(tool.name());
        if is_ui {
            has_ui = true;
        }
        let allowed = crate::plan_mode_allows(tool.name());
        let tool = tool.with_allowed_in_plan_mode(allowed);
        if is_ui {
            registry.register(tool);
        } else {
            registry.register(LabelledTool(tool));
        }
    }
    has_ui
}

fn is_keel_mcp_exe(cmd: &str) -> bool {
    if std::env::var_os("KEEL_BROWSER_MCP_EXE").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    Path::new(cmd)
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("keel"))
}

// ---------------------------------------------------------------------------
// Memory Tools & Engine Unix Socket Client
// ---------------------------------------------------------------------------

struct MemoryReadTool {
    data_dir: PathBuf,
    cwd: PathBuf,
}

impl MemoryReadTool {
    fn new(data_dir: PathBuf, cwd: PathBuf) -> Self {
        Self { data_dir, cwd }
    }
}

impl Tool for MemoryReadTool {
    fn name(&self) -> &str {
        "memory_read"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_read".to_string(),
            description: "Read Keel's notes. Without a slug, returns the whole index and the topic slugs. With a slug, returns that topic's type, description and body.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "label": {"type": "string", "description": LABEL_DESC},
                    "scope": {"type": "string", "enum": ["user", "project"], "description": "user: every project. project: this checkout."},
                    "slug": {"type": "string", "description": "Topic slug. Omit for the index."}
                },
                "required": ["scope"]
            }),
        }
    }

    fn summarize(&self, arguments: &Value) -> String {
        let scope = arguments
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("?");
        match arguments.get("slug").and_then(Value::as_str) {
            Some(slug) if !slug.is_empty() => format!("memory_read {scope}/{slug}"),
            _ => format!("memory_read {scope} index"),
        }
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    fn read_only(&self) -> bool {
        true
    }

    fn execute<'a>(&'a self, arguments: Value, _context: &'a ToolContext) -> ToolFuture<'a> {
        let data_dir = self.data_dir.clone();
        let cwd = self.cwd.to_string_lossy().into_owned();
        Box::pin(async move {
            let mut params = serde_json::Map::new();
            if let Some(scope) = arguments.get("scope").cloned() {
                params.insert("scope".to_string(), scope);
            }
            if let Some(slug) = arguments.get("slug").cloned() {
                params.insert("slug".to_string(), slug);
            }
            params.insert("cwd".to_string(), json!(cwd));
            match engine_ipc_call(&data_dir, "memory_read", Value::Object(params)).await {
                Ok(result) => {
                    let text = result
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    ToolOutcome::ok(text)
                }
                Err(err) => ToolOutcome::error(format!("Memory error: {err}")),
            }
        })
    }
}

struct MemoryWriteTool {
    data_dir: PathBuf,
    cwd: PathBuf,
}

impl MemoryWriteTool {
    fn new(data_dir: PathBuf, cwd: PathBuf) -> Self {
        Self { data_dir, cwd }
    }
}

impl Tool for MemoryWriteTool {
    fn name(&self) -> &str {
        "memory_write"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_write".to_string(),
            description: "Save or retire one of Keel's notes. upsert writes the topic and its index line; archive retires the topic. type and description are required for upsert.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "label": {"type": "string", "description": LABEL_DESC},
                    "scope": {"type": "string", "enum": ["user", "project"], "description": "user: every project. project: this checkout."},
                    "action": {"type": "string", "enum": ["upsert", "archive"]},
                    "slug": {"type": "string", "description": "Lowercase letters, digits and dashes, up to 64 characters."},
                    "type": {"type": "string", "enum": ["user", "feedback", "project", "reference"]},
                    "description": {"type": "string", "description": "One line, at most 200 characters. Shown in the index."},
                    "body": {"type": "string", "description": "Markdown, at most 32 KiB."}
                },
                "required": ["scope", "action", "slug"]
            }),
        }
    }

    fn summarize(&self, arguments: &Value) -> String {
        let scope = arguments
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let action = arguments
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let slug = arguments
            .get("slug")
            .and_then(Value::as_str)
            .unwrap_or("?");
        format!("memory_write {action} {scope}/{slug}")
    }

    fn execute<'a>(&'a self, mut arguments: Value, _context: &'a ToolContext) -> ToolFuture<'a> {
        let data_dir = self.data_dir.clone();
        let cwd = self.cwd.to_string_lossy().into_owned();
        Box::pin(async move {
            if let Some(obj) = arguments.as_object_mut() {
                obj.remove("label");
                obj.insert("cwd".to_string(), json!(cwd));
            }
            match engine_ipc_call(&data_dir, "memory_write", arguments).await {
                Ok(result) => {
                    let text = result
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("Saved.")
                        .to_string();
                    ToolOutcome::ok(text)
                }
                Err(err) => ToolOutcome::error(format!("Memory error: {err}")),
            }
        })
    }
}

async fn fetch_memory_prompt(data_dir: &Path, cwd: &Path) -> String {
    let fallback = || format!("{MEMORY_PREAMBLE}\n\n{MEMORY_UNAVAILABLE}");
    let params = json!({ "cwd": cwd.to_string_lossy() });
    match engine_ipc_call(data_dir, "memory_prompt", params).await {
        Ok(result) => result
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(fallback),
        Err(_) => fallback(),
    }
}

async fn engine_ipc_call(
    data_dir: &Path,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let token = fs::read_to_string(data_dir.join("ipc.token"))
        .map_err(|e| format!("could not read ipc.token: {e}"))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("ipc.token is empty".to_string());
    }
    let sock_path = data_dir.join("keel.sock");
    tokio::time::timeout(MEMORY_IPC_TIMEOUT, async {
        let mut stream = UnixStream::connect(&sock_path)
            .await
            .map_err(|e| format!("could not connect to engine socket: {e}"))?;
        write_frame(
            &mut stream,
            &json!({"id": "h", "method": "hello", "params": {"token": token, "proto": MEMORY_PROTO}}),
        )
        .await?;
        let hello = read_frame(&mut stream).await?;
        if let Some(err) = hello.get("error") {
            return Err(err["message"]
                .as_str()
                .unwrap_or("Keel refused the connection")
                .to_string());
        }
        write_frame(
            &mut stream,
            &json!({"id": "m", "method": method, "params": params}),
        )
        .await?;
        let reply = read_frame(&mut stream).await?;
        if let Some(err) = reply.get("error") {
            return Err(err["message"]
                .as_str()
                .unwrap_or("Keel refused the call")
                .to_string());
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    })
    .await
    .map_err(|_| "Keel did not answer in time".to_string())?
}

async fn write_frame(stream: &mut UnixStream, value: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let len = u32::try_from(body.len()).map_err(|_| "IPC payload too large".to_string())?;
    stream
        .write_all(&len.to_be_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(&body).await.map_err(|e| e.to_string())?;
    stream.flush().await.map_err(|e| e.to_string())
}

async fn read_frame(stream: &mut UnixStream) -> Result<Value, String> {
    let mut head = [0u8; 4];
    stream
        .read_exact(&mut head)
        .await
        .map_err(|e| format!("Keel closed the connection: {e}"))?;
    let len = u32::from_be_bytes(head) as usize;
    if len > MAX_IPC_FRAME_BYTES {
        return Err("Keel sent an oversized reply".to_string());
    }
    let mut buf = vec![0u8; len];
    stream
        .read_exact(&mut buf)
        .await
        .map_err(|e| format!("Keel closed the connection: {e}"))?;
    serde_json::from_slice(&buf).map_err(|_| "Keel sent a bad reply".to_string())
}

// ---------------------------------------------------------------------------
// Session Log, Catalog, Credentials & Model Resolution
// ---------------------------------------------------------------------------

struct ResumedSession {
    log: SessionLog,
    id: String,
    has_stored_system: bool,
}

fn configure_fresh_system_prompt(
    session: &mut Session,
    profile_prompt: Option<&str>,
    has_ui_tools: bool,
    scratch: bool,
    project: Option<&crate::ProjectRoots>,
    project_instructions: Option<&ProjectInstructions>,
    initial_memory: Option<&str>,
) {
    let host_prompt = profile_prompt
        .filter(|p| !p.trim().is_empty())
        .unwrap_or(KEEL_SYSTEM_PROMPT);
    session.system = keel_core::effective_system_prompt(
        Some(host_prompt.trim()),
        Some(keel_core::default_working_rules()),
    );
    if has_ui_tools {
        session.append_system(crate::UI_CARDS_PROMPT);
    }
    if scratch {
        session.append_system(crate::SCRATCH_PROMPT);
    }
    if let Some(project) = project.filter(|p| !p.roots.is_empty()) {
        let mut roots = project.roots.clone();
        if roots.len() > 1 {
            roots[1..].sort_by(|a, b| a.0.cmp(&b.0));
        }
        let text = keel_proto::projects::roots_prompt(&project.name, &roots);
        session.append_system(text.trim_end());
    }
    if let Some(rendered) = project_instructions.and_then(ProjectInstructions::render) {
        session.append_system(&rendered);
    }
    if let Some(mem) = initial_memory {
        session.append_system(&format!("<keel_memory>\n{}\n</keel_memory>", mem.trim_end()));
    }
}

fn keel_plan_mode_prompt(has_ui_tools: bool, bash_sandboxed: bool) -> Option<String> {
    if !has_ui_tools {
        return None;
    }
    if bash_sandboxed {
        Some(format!(
            "{}\n\nYou may also use `bash` for read-only inspection commands (such as `git status`, `git diff`, `git log`, or `rg`); in plan mode, `bash` runs in a read-only, network-disabled OS sandbox.",
            crate::PLAN_MODE_PROMPT
        ))
    } else {
        Some(crate::PLAN_MODE_PROMPT.to_string())
    }
}

fn extract_tagged_block<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.rfind(&open)? + open.len();
    let tail = &text[start..];
    let end = tail.find(&close)?;
    Some(tail[..end].trim())
}

fn last_known_memory(session: &Session) -> Option<String> {
    for msg in session.history.iter().rev() {
        let text = msg.text();
        if let Some(block) = extract_tagged_block(&text, "keel_memory_update") {
            return Some(block.to_string());
        }
    }
    session
        .system
        .as_deref()
        .and_then(|sys| extract_tagged_block(sys, "keel_memory"))
        .map(str::to_string)
}

fn should_emit_memory_update(last_known: Option<&str>, current: &str) -> bool {
    let current = current.trim();
    if current.is_empty() {
        return false;
    }
    let unavailable = format!("{MEMORY_PREAMBLE}\n\n{MEMORY_UNAVAILABLE}");
    if let Some(prev) = last_known {
        if prev == current {
            return false;
        }
        if current == unavailable && prev != unavailable {
            return false;
        }
    }
    true
}

fn history_ends_in_plan_mode(history: &[Message]) -> bool {
    for msg in history.iter().rev() {
        if msg.role != Role::User {
            continue;
        }
        let text = msg.text();
        if let Some(rest) = text.trim_start().strip_prefix("[plan mode]\n") {
            return rest.trim() != keel_core::PLAN_MODE_OFF_NOTICE;
        }
    }
    false
}

fn try_resume_session(
    sessions_dir: &Path,
    resume: Option<&NativeSessionId>,
    session: &mut Session,
) -> Option<ResumedSession> {
    let NativeSessionId(raw) = resume?;
    if raw.trim().is_empty() {
        return None;
    }
    let raw_path = Path::new(raw);
    let (dir, id) = if raw_path.is_file() || raw.ends_with(".jsonl") {
        let dir = raw_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(sessions_dir);
        let stem = raw_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(raw.as_str());
        (dir, stem)
    } else {
        (sessions_dir, raw.as_str())
    };
    match SessionLog::open_with_state(dir, id) {
        Ok((log, meta, messages, working_memory)) => {
            session.history = messages;
            if let Some(wm) = working_memory {
                session.restore_working_memory(wm);
            }
            let has_stored_system = meta.system.is_some();
            if let Some(stored) = meta.system {
                session.system = Some(stored);
            }
            Some(ResumedSession {
                log,
                id: meta.id,
                has_stored_system,
            })
        }
        Err(err) => {
            tracing::warn!(
                target: "keel_harness::native",
                resume = %raw,
                error = %err,
                "could not resume session log; starting a fresh session"
            );
            None
        }
    }
}

fn create_session_log(
    sessions_dir: &Path,
    provider: &Provider,
    model_spec: &ModelSpec,
    cwd: &Path,
    session: &Session,
) -> Result<(SessionLog, String), HarnessError> {
    let id = new_session_id();
    let meta = SessionMeta::new(
        &id,
        &provider.id,
        &model_spec.id,
        model_spec.protocol.as_str(),
    )
    .workspace(cwd.to_string_lossy().into_owned())
    .system(session.system.clone())
    .cache(Some(session.cache.clone()));
    let log = SessionLog::create(sessions_dir, &meta).map_err(failed)?;
    Ok((log, id))
}

fn openai_uses_oauth(store: Option<&CredentialStore>) -> bool {
    store.is_some_and(|s| s.get_oauth("openai").ok().flatten().is_some())
}

fn build_route_for_store(
    provider: &Provider,
    model: &ModelSpec,
    store: Option<&CredentialStore>,
) -> Result<keel_llm_route::Route, LlmError> {
    if provider.id == "openai" && openai_uses_oauth(store) {
        build_openai_codex_route(provider, model)
    } else {
        build_route(provider, model)
    }
}

fn resolve_model(
    catalog: &Catalog,
    resolver: &dyn SecretResolver,
    store: Option<&CredentialStore>,
    requested: &str,
) -> Result<(Provider, ModelSpec), HarnessError> {
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        for (provider_id, model_id) in DEFAULT_MODEL_CANDIDATES {
            if let Some((provider, model)) = catalog.find(provider_id, model_id) {
                let route = build_route_for_store(provider, model, store);
                if route.is_ok_and(|r| r.auth.apply(resolver).is_ok()) {
                    return Ok((provider.clone(), model.clone()));
                }
            }
        }
        for (provider, model) in catalog.models() {
            if provider.id == "custom" && provider.resolved_base_url().is_none() {
                continue;
            }
            if keel_llm_protocol::for_protocol(model.protocol, model.compat()).is_none() {
                continue;
            }
            if let Ok(route) = build_route_for_store(provider, model, store)
                && route.auth.apply(resolver).is_ok()
            {
                return Ok((provider.clone(), model.clone()));
            }
        }
        let (provider_id, model_id) = DEFAULT_MODEL_CANDIDATES[0];
        if let Some((provider, model)) = catalog.find(provider_id, model_id) {
            return Ok((provider.clone(), model.clone()));
        }
        return Err(HarnessError::Failed(
            "no runnable models in catalog".to_string(),
        ));
    }

    if let Some((raw_provider, model_id)) = trimmed.split_once('/') {
        let provider_id = canonical_provider_id(raw_provider);
        if let Some((provider, model)) = catalog.find(provider_id, model_id) {
            return Ok((provider.clone(), model.clone()));
        }
        // Some model IDs themselves contain `/` (e.g. `neuralwatt/Qwen/Qwen3.5...`).
        let matches = catalog.find_model(trimmed);
        if let Some((provider, model)) = matches.into_iter().next() {
            return Ok((provider.clone(), model.clone()));
        }
        return Err(HarnessError::Failed(format!(
            "unknown model `{trimmed}`"
        )));
    }

    let matches = catalog.find_model(trimmed);
    if let Some((provider, model)) = matches.into_iter().next() {
        return Ok((provider.clone(), model.clone()));
    }
    Err(HarnessError::Failed(format!(
        "unknown model `{trimmed}`"
    )))
}

const ANTHROPIC_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_OAUTH_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";

fn canonical_provider_id(id: &str) -> &str {
    match id {
        "xai" => "grok",
        other => other,
    }
}

fn ui_provider_id(id: &str) -> &str {
    match id {
        "grok" => "xai",
        other => other,
    }
}

fn load_catalog() -> LoadedCatalog {
    let embedded = Catalog::builtin();
    let (mut catalog, mut warnings) = match embedded.with_overlay_and_warnings(KEEL_APP_CATALOG_OVERLAY) {
        Ok(res) => res,
        Err(err) => (
            embedded.clone(),
            vec![format!("bundled app catalog overlay failed: {err}")],
        ),
    };
    let runtime = Catalog::load();
    let runtime_applied = if runtime.source.is_some() {
        warnings.extend(runtime.warnings);
        runtime.source
    } else {
        None
    };
    if let Some(path) = runtime_applied.as_deref()
        && let Ok(text) = fs::read_to_string(path)
        && let Ok((merged, extra_warnings)) = catalog.with_overlay_and_warnings(&text)
    {
        catalog = merged;
        warnings.extend(extra_warnings);
    }
    reorder_catalog(&mut catalog);
    LoadedCatalog {
        catalog,
        warnings,
        source: runtime_applied,
    }
}

fn reorder_catalog(catalog: &mut Catalog) {
    catalog
        .providers
        .sort_by_key(|p| provider_display_rank(&p.id));
    for provider in &mut catalog.providers {
        reorder_provider_models(provider);
    }
}

fn provider_display_rank(id: &str) -> usize {
    match id {
        "anthropic" => 0,
        "openai" => 1,
        "grok" | "xai" => 2,
        "opencode" => 3,
        "commandcode" => 4,
        "minimax" => 5,
        "neuralwatt" => 6,
        "custom" => usize::MAX,
        _ => 100,
    }
}

fn reorder_provider_models(provider: &mut Provider) {
    let provider_id = provider.id.clone();
    provider.models.sort_by_key(|m| {
        let lower = m.id.to_ascii_lowercase();
        let fam = match provider_id.as_str() {
            "anthropic" => {
                if lower.contains("opus") {
                    0
                } else if lower.contains("sonnet") {
                    1
                } else if lower.contains("fable") {
                    2
                } else if lower.contains("haiku") {
                    3
                } else {
                    4
                }
            }
            "openai" => {
                if lower.contains("codex") {
                    1
                } else if lower.starts_with('o')
                    && lower.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
                {
                    2
                } else if lower.starts_with("gpt-4") {
                    3
                } else if lower.starts_with("gpt-") {
                    0
                } else {
                    4
                }
            }
            "neuralwatt" => {
                if lower.starts_with("nw-") {
                    0
                } else {
                    1
                }
            }
            _ => {
                if lower.contains("opus") {
                    0
                } else if lower.contains("sonnet") {
                    1
                } else if lower.contains("fable") {
                    2
                } else if lower.contains("haiku") {
                    3
                } else if lower.starts_with("gpt-") && !lower.contains("codex") {
                    4
                } else if lower.contains("codex") {
                    5
                } else if lower.contains("grok") {
                    6
                } else {
                    7
                }
            }
        };
        let dated = is_snapshot_id(&m.id);
        let ver = parse_model_version(&m.id);
        (fam, dated, std::cmp::Reverse(ver))
    });
}

fn is_snapshot_id(id: &str) -> bool {
    let b = id.rsplit('/').next().unwrap_or(id).as_bytes();
    let is_digits = |s: &[u8]| !s.is_empty() && s.iter().all(u8::is_ascii_digit);
    let dated8 = |d: &[u8]| d.len() == 8 && is_digits(d) && (d.starts_with(b"19") || d.starts_with(b"20"));
    if b.len() > 9 && matches!(b[b.len() - 9], b'-' | b'@' | b'_') && dated8(&b[b.len() - 8..]) {
        return true;
    }
    if b.len() > 11 {
        let tail = &b[b.len() - 11..];
        let (y, m, d) = (&tail[1..5], &tail[6..8], &tail[9..11]);
        return tail[0] == b'-'
            && tail[5] == b'-'
            && tail[8] == b'-'
            && is_digits(y)
            && is_digits(m)
            && is_digits(d)
            && (y.starts_with(b"19") || y.starts_with(b"20"));
    }
    false
}

fn parse_model_version(id: &str) -> Vec<u32> {
    let base = id.rsplit('/').next().unwrap_or(id);
    let mut out = Vec::new();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut Vec<u32>| -> bool {
        if run.is_empty() {
            return true;
        }
        let keep = run.len() <= 3;
        if keep {
            out.push(run.parse().unwrap_or(0));
        }
        run.clear();
        keep
    };
    for ch in base.chars() {
        if ch.is_ascii_digit() {
            run.push(ch);
        } else if !flush(&mut run, &mut out) {
            return out;
        }
    }
    flush(&mut run, &mut out);
    out
}

fn provider_is_connected_with_disabled(
    provider: &Provider,
    store: Option<&CredentialStore>,
    disabled_env: &HashSet<String>,
    env_get: &dyn Fn(&str) -> Option<String>,
) -> bool {
    if provider.id == "custom" {
        return provider.resolved_base_url().is_some();
    }
    if matches!(provider.auth, AuthSpec::None) {
        return true;
    }
    let key_present = |key: &str| {
        store.is_some_and(|s| {
            s.get_oauth(key).ok().flatten().is_some() || s.get(key).ok().flatten().is_some()
        })
    };
    if key_present(&provider.id) || (provider.id == "grok" && key_present("xai")) {
        return true;
    }
    !disabled_env.contains(&provider.id)
        && auth_env_vars(&provider.auth)
            .iter()
            .any(|var| env_get(var).is_some_and(|v| !v.trim().is_empty()))
}

fn list_models_for(catalog: &Catalog, store: Option<&CredentialStore>) -> Vec<Model> {
    list_models_for_with(catalog, store, &|var| std::env::var(var).ok())
}

fn list_models_for_with(
    catalog: &Catalog,
    store: Option<&CredentialStore>,
    env_get: &dyn Fn(&str) -> Option<String>,
) -> Vec<Model> {
    let disabled_env = load_disabled_env(store);
    let openai_oauth = openai_uses_oauth(store);
    catalog
        .models()
        .filter(|(provider, model)| {
            if provider.id == "openai"
                && openai_oauth
                && model.protocol != ProtocolId::OpenAiResponses
            {
                return false;
            }
            provider_is_connected_with_disabled(provider, store, &disabled_env, env_get)
                && keel_llm_protocol::for_protocol(model.protocol, model.compat()).is_some()
        })
        .map(|(provider, model)| Model {
            id: ModelId(format!("{}/{}", ui_provider_id(&provider.id), model.id)),
            name: model.name.clone(),
        })
        .collect()
}

fn credential_store() -> Option<CredentialStore> {
    if std::env::var_os(keel_llm_route::CREDENTIALS_ENV).is_some_and(|v| !v.is_empty()) {
        return CredentialStore::discover().ok();
    }
    credential_store_in(crate::pi_agent_dir())
}

fn credential_store_in(agent_dir: Option<&Path>) -> Option<CredentialStore> {
    let store = if let Some(dir) = agent_dir {
        let store = CredentialStore::at(dir.join("credentials.json"));
        let auth_json = dir.join("auth.json");
        if auth_json.is_file() {
            import_legacy_auth_json(&auth_json, &store);
        }
        store
    } else {
        CredentialStore::discover().ok()?
    };
    Some(store)
}

fn default_oauth_metadata(store_key: &str) -> Option<(&'static str, &'static str)> {
    match store_key {
        "grok" => Some((GROK_DEFAULT_CLIENT_ID, GROK_TOKEN_URL)),
        "openai" => Some((OPENAI_DEFAULT_CLIENT_ID, OPENAI_TOKEN_URL)),
        "anthropic" => Some((ANTHROPIC_OAUTH_CLIENT_ID, ANTHROPIC_OAUTH_TOKEN_URL)),
        _ => None,
    }
}

fn import_legacy_auth_json(auth_path: &Path, store: &CredentialStore) {
    let Ok(raw) = fs::read_to_string(auth_path) else {
        return;
    };
    let Ok(Value::Object(entries)) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    let existing: HashSet<String> = store.list().unwrap_or_default().into_iter().collect();
    for (raw_provider, value) in entries {
        let store_key = canonical_provider_id(&raw_provider);
        if existing.contains(store_key) {
            continue;
        }
        if CredentialStore::validate_key(store_key).is_err() {
            continue;
        }
        match value {
            Value::String(secret) => {
                if !secret.trim().is_empty() {
                    let _ = store.set(store_key, &secret);
                }
            }
            Value::Object(map) => {
                let kind = map.get("type").and_then(Value::as_str);
                if kind == Some("oauth") || map.contains_key("access") || map.contains_key("access_token") {
                    let Some(access) = map
                        .get("access")
                        .or_else(|| map.get("access_token"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_string)
                    else {
                        continue;
                    };
                    let refresh = map
                        .get("refresh")
                        .or_else(|| map.get("refresh_token"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_string);
                    let expires_raw = map
                        .get("expires")
                        .or_else(|| map.get("expires_at"))
                        .and_then(Value::as_u64);
                    let expires_at = expires_raw.map(|e| if e > 10_000_000_000 { e / 1000 } else { e });
                    let defaults = default_oauth_metadata(store_key);
                    let client_id = map
                        .get("client_id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| defaults.map(|(cid, _)| cid.to_string()));
                    let token_url = map
                        .get("token_url")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| defaults.map(|(_, url)| url.to_string()));
                    let tokens = OAuthTokenSet {
                        access_token: access,
                        refresh_token: refresh,
                        expires_at,
                        token_type: map
                            .get("token_type")
                            .and_then(Value::as_str)
                            .unwrap_or("Bearer")
                            .to_string(),
                        client_id,
                        token_url,
                    };
                    let _ = store.set_oauth(store_key, &tokens);
                } else if let Some(secret) = map
                    .get("key")
                    .or_else(|| map.get("apiKey"))
                    .or_else(|| map.get("api_key"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                {
                    let _ = store.set(store_key, secret);
                }
            }
            _ => {}
        }
    }
}

/// Resolves stored credentials first (so editing an API key in Keel overrides
/// any inherited shell environment variable), then falls back to process env
/// unless that provider's env fallback was explicitly removed in Keel.
struct KeelSecretResolver {
    store: StoreResolver,
    disabled_env: HashSet<String>,
}

impl KeelSecretResolver {
    fn new(store: CredentialStore) -> Self {
        let disabled_env = load_disabled_env(Some(&store));
        Self {
            store: StoreResolver::new(store),
            disabled_env,
        }
    }
}

impl SecretResolver for KeelSecretResolver {
    fn resolve(&self, reference: &CredentialRef) -> Option<String> {
        self.resolve_result(reference).ok().flatten()
    }

    fn resolve_result(&self, reference: &CredentialRef) -> Result<Option<String>, LlmError> {
        if let Some(key) = reference.store_key.as_deref() {
            if let Some(value) = self
                .store
                .resolve_result(&CredentialRef::from_store(key))?
            {
                return Ok(Some(value));
            }
            if self.disabled_env.contains(key) {
                return Ok(None);
            }
        }
        Ok(EnvResolver.resolve(reference))
    }
}

fn secret_resolver() -> Arc<dyn SecretResolver> {
    match credential_store() {
        Some(store) => Arc::new(KeelSecretResolver::new(store)),
        None => Arc::new(EnvResolver),
    }
}

fn disabled_env_path(store: &CredentialStore) -> PathBuf {
    store.path().with_file_name("env-disabled.json")
}

fn load_disabled_env(store: Option<&CredentialStore>) -> HashSet<String> {
    let Some(store) = store else {
        return HashSet::new();
    };
    let path = disabled_env_path(store);
    let Ok(raw) = fs::read_to_string(path) else {
        return HashSet::new();
    };
    serde_json::from_str::<Vec<String>>(&raw)
        .map(|items| items.into_iter().collect())
        .unwrap_or_default()
}

fn save_disabled_env(store: &CredentialStore, set: &HashSet<String>) -> std::io::Result<()> {
    let path = disabled_env_path(store);
    if set.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    let mut list: Vec<&str> = set.iter().map(String::as_str).collect();
    list.sort_unstable();
    let mut bytes = serde_json::to_vec_pretty(&list)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    bytes.push(b'\n');

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".env-disabled.json.tmp.{}", std::process::id()));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    {
        use std::io::Write as _;
        let mut f = opts.open(&tmp)?;
        if let Err(err) = f.write_all(&bytes).and_then(|_| f.sync_all()) {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
    }
    if let Err(err) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

fn set_env_disabled(
    store: &CredentialStore,
    catalog_id: &str,
    disabled: bool,
) -> std::io::Result<()> {
    let mut set = load_disabled_env(Some(store));
    let changed = if disabled {
        set.insert(catalog_id.to_string())
    } else {
        set.remove(catalog_id)
    };
    if changed {
        save_disabled_env(store, &set)?;
    }
    Ok(())
}

fn provider_env_is_set(catalog: &Catalog, catalog_id: &str) -> bool {
    catalog.provider(catalog_id).is_some_and(|p| {
        auth_env_vars(&p.auth)
            .iter()
            .any(|var| std::env::var(var).ok().is_some_and(|v| !v.trim().is_empty()))
    })
}

fn auth_env_vars(auth: &AuthSpec) -> &[String] {
    match auth {
        AuthSpec::None => &[],
        AuthSpec::Bearer { env }
        | AuthSpec::OptionalBearer { env }
        | AuthSpec::Header { env, .. }
        | AuthSpec::Query { env, .. } => env,
    }
}

fn resolve_settings_catalog_id(provider: &str) -> Result<String, HarnessError> {
    let normalized = provider.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || !normalized
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(HarnessError::Failed(format!(
            "unknown provider {provider:?}"
        )));
    }
    let catalog = load_catalog().catalog;
    let catalog_id = canonical_provider_id(&normalized);
    if catalog.provider(catalog_id).is_none() {
        return Err(HarnessError::Failed(format!(
            "unknown provider `{provider}`"
        )));
    }
    Ok(catalog_id.to_string())
}

fn set_api_key_with(
    store: &CredentialStore,
    provider: &str,
    api_key: &str,
) -> Result<(), HarnessError> {
    let catalog_id = resolve_settings_catalog_id(provider)?;
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        return Err(HarnessError::Failed("API key cannot be empty".to_string()));
    }
    store.set(&catalog_id, trimmed).map_err(failed)?;
    set_env_disabled(store, &catalog_id, false).map_err(failed)
}

fn remove_credential_with(
    store: &CredentialStore,
    agent_dir: Option<&Path>,
    provider: &str,
) -> Result<(), HarnessError> {
    let catalog_id = resolve_settings_catalog_id(provider)?;
    store.remove(&catalog_id).map_err(failed)?;
    if let Some(dir) = agent_dir {
        let auth_json = dir.join("auth.json");
        if auth_json.is_file() {
            remove_legacy_auth_json_entries(&auth_json, &catalog_id).map_err(failed)?;
        }
    }
    let catalog = load_catalog().catalog;
    if provider_env_is_set(&catalog, &catalog_id) {
        set_env_disabled(store, &catalog_id, true).map_err(failed)?;
    }
    Ok(())
}

/// Remove entries matching `catalog_id` from legacy `auth.json` so
/// `import_legacy_auth_json` does not re-import a credential the user removed.
fn remove_legacy_auth_json_entries(auth_path: &Path, catalog_id: &str) -> std::io::Result<()> {
    let meta = match fs::symlink_metadata(auth_path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Ok(());
    }
    let raw = fs::read_to_string(auth_path)?;
    let Ok(Value::Object(mut entries)) = serde_json::from_str::<Value>(&raw) else {
        return Ok(());
    };
    let before = entries.len();
    entries.retain(|raw_provider, _| canonical_provider_id(raw_provider) != catalog_id);
    if entries.len() == before {
        return Ok(());
    }
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(entries))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    bytes.push(b'\n');

    let parent = auth_path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(".auth.json.tmp.{}", std::process::id()));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    {
        use std::io::Write as _;
        let mut f = opts.open(&tmp)?;
        if let Err(err) = f.write_all(&bytes).and_then(|_| f.sync_all()) {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
    }
    if let Err(err) = fs::rename(&tmp, auth_path) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

fn supports_oauth(account_id: &str) -> bool {
    matches!(account_id, "xai" | "grok" | "openai")
}

fn build_auth_rows(catalog: &Catalog, store: Option<&CredentialStore>) -> Vec<AuthAccount> {
    let disabled_env = load_disabled_env(store);
    SETTINGS_PROVIDERS
        .iter()
        .map(|(account_id, name, hint, catalog_id)| {
            let stored = store.and_then(|s| {
                if s.get_oauth(catalog_id).ok().flatten().is_some() {
                    Some("oauth")
                } else if s.get(catalog_id).ok().flatten().is_some() {
                    Some("api_key")
                } else {
                    None
                }
            });
            let env_active =
                stored.is_none() && !disabled_env.contains(*catalog_id) && provider_env_is_set(catalog, catalog_id);
            let detail = stored.or_else(|| env_active.then_some("api_key"));
            AuthAccount {
                id: (*account_id).to_string(),
                name: (*name).to_string(),
                hint: (*hint).to_string(),
                signed_in: detail.is_some(),
                detail: detail.unwrap_or("not signed in").to_string(),
                supports_oauth: supports_oauth(account_id),
                from_env: env_active,
            }
        })
        .collect()
}

fn apply_grok_oauth_env(config: &mut OAuthProviderConfig) -> Result<(), HarnessError> {
    if let Ok(client_id) = std::env::var("KEEL_GROK_CLIENT_ID")
        && !client_id.trim().is_empty()
    {
        config.client_id = client_id;
    }
    if let Ok(url) = std::env::var("KEEL_GROK_OAUTH_AUTHORIZE_URL")
        && !url.trim().is_empty()
    {
        config.authorize_url = url;
    }
    if let Ok(url) = std::env::var("KEEL_GROK_OAUTH_TOKEN_URL")
        && !url.trim().is_empty()
    {
        config.token_url = url;
    }
    if let Ok(url) = std::env::var("KEEL_GROK_OAUTH_DEVICE_URL")
        && !url.trim().is_empty()
    {
        config.device_authorize_url = Some(url);
    }
    if let Ok(port_raw) = std::env::var("KEEL_GROK_OAUTH_PORT")
        && !port_raw.trim().is_empty()
    {
        let port: u16 = port_raw.trim().parse().map_err(|_| {
            HarnessError::Failed(format!(
                "KEEL_GROK_OAUTH_PORT must be a TCP port number, got `{port_raw}`"
            ))
        })?;
        config.callback_port = (port != 0).then_some(port);
    }
    Ok(())
}

fn apply_openai_oauth_env(config: &mut OAuthProviderConfig) -> Result<(), HarnessError> {
    if let Ok(client_id) = std::env::var("KEEL_OPENAI_CLIENT_ID")
        && !client_id.trim().is_empty()
    {
        config.client_id = client_id;
    }
    if let Ok(url) = std::env::var("KEEL_OPENAI_OAUTH_AUTHORIZE_URL")
        && !url.trim().is_empty()
    {
        config.authorize_url = url;
    }
    if let Ok(url) = std::env::var("KEEL_OPENAI_OAUTH_TOKEN_URL")
        && !url.trim().is_empty()
    {
        config.token_url = url;
    }
    if let Ok(port_raw) = std::env::var("KEEL_OPENAI_OAUTH_PORT")
        && !port_raw.trim().is_empty()
    {
        let port: u16 = port_raw.trim().parse().map_err(|_| {
            HarnessError::Failed(format!(
                "KEEL_OPENAI_OAUTH_PORT must be a TCP port number, got `{port_raw}`"
            ))
        })?;
        config.callback_port = (port != 0).then_some(port);
        if port == 0 {
            config.strict_callback_port = false;
        }
    }
    Ok(())
}

fn parse_reasoning(raw: &str) -> Option<ReasoningEffort> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "minimal" => Some(ReasoningEffort::Minimal),
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" | "xhigh" | "max" => Some(ReasoningEffort::High),
        _ => None,
    }
}

fn cache_policy_from_env() -> CachePolicy {
    match std::env::var("KEEL_CACHE")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("none" | "off" | "0" | "false") => CachePolicy::None,
        _ => CachePolicy::Auto,
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}

fn failed(err: impl std::fmt::Display) -> HarnessError {
    HarnessError::Failed(err.to_string())
}

const KEEL_APP_CATALOG_OVERLAY: &str = r#"
version = 1

# ---------------------------------------------------------------------------
# Anthropic (latest models + clean names)
# ---------------------------------------------------------------------------

[[providers]]
id = "anthropic"

[[providers.models]]
id = "claude-opus-5-5"
name = "Claude Opus 5.5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-5"
name = "Claude Opus 5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-4-8"
name = "Claude Opus 4.8"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-4-7"
name = "Claude Opus 4.7"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-4-6"
name = "Claude Opus 4.6"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-4-5"
name = "Claude Opus 4.5"
protocol = "anthropic-messages"
context_window = 200000
max_output_tokens = 64000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-4-5-20251101"
name = "Claude Opus 4.5 (2025-11-01)"
protocol = "anthropic-messages"
context_window = 200000
max_output_tokens = 64000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-sonnet-5-5"
name = "Claude Sonnet 5.5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-sonnet-5"
name = "Claude Sonnet 5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-sonnet-4-6"
name = "Claude Sonnet 4.6"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-sonnet-4-5"
name = "Claude Sonnet 4.5"

[[providers.models]]
id = "claude-sonnet-4-5-20250929"
name = "Claude Sonnet 4.5 (2025-09-29)"
protocol = "anthropic-messages"
context_window = 200000
max_output_tokens = 64000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-fable-5-2"
name = "Claude Fable 5.2"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-fable-5-1"
name = "Claude Fable 5.1"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-fable-5"
name = "Claude Fable 5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-haiku-5-5"
name = "Claude Haiku 5.5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-haiku-4-5"
name = "Claude Haiku 4.5"

[[providers.models]]
id = "claude-haiku-4-5-20251001"
name = "Claude Haiku 4.5 (2025-10-01)"
protocol = "anthropic-messages"
context_window = 200000
max_output_tokens = 64000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

# ---------------------------------------------------------------------------
# OpenAI
# ---------------------------------------------------------------------------

[[providers]]
id = "openai"

[[providers.models]]
id = "gpt-6.2-astra"
name = "GPT-6.2 Astra"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6.2-sol"
name = "GPT-6.2 Sol"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6.1-sol"
name = "GPT-6.1 Sol"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6.1-terra"
name = "GPT-6.1 Terra"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6-astra"
name = "GPT-6 Astra"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6-luna"
name = "GPT-6 Luna"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6-sol"
name = "GPT-6 Sol"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.6"
name = "GPT-5.6"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.6-luna"
name = "GPT-5.6 Luna"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.6-sol"
name = "GPT-5.6 Sol"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.6-terra"
name = "GPT-5.6 Terra"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.5-pro"
name = "GPT-5.5 Pro"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.4-pro"
name = "GPT-5.4 Pro"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.4"
name = "GPT-5.4"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.4-mini"
name = "GPT-5.4 mini"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.4-nano"
name = "GPT-5.4 nano"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.2-pro"
name = "GPT-5.2 Pro"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.2"
name = "GPT-5.2"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.1"
name = "GPT-5.1"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5-pro"
name = "GPT-5 Pro"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 272000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5"
name = "GPT-5"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5-mini"
name = "GPT-5 Mini"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5-nano"
name = "GPT-5 Nano"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.3-codex"
name = "GPT-5.3 Codex"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.3-codex-spark"
name = "GPT-5.3 Codex Spark"
protocol = "openai-responses"
context_window = 128000
max_output_tokens = 32000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "o3-pro"
name = "o3-pro"
protocol = "openai-responses"
context_window = 200000
max_output_tokens = 100000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "o3"
name = "o3"
protocol = "openai-responses"
context_window = 200000
max_output_tokens = 100000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "o4-mini"
name = "o4-mini"
protocol = "openai-responses"
context_window = 200000
max_output_tokens = 100000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "o3-mini"
name = "o3-mini"
protocol = "openai-responses"
context_window = 200000
max_output_tokens = 100000
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = true

[[providers.models]]
id = "gpt-4.1-nano"
name = "GPT-4.1 nano"
protocol = "openai-chat"
context_window = 1047576
max_output_tokens = 32768
[providers.models.capabilities]
tools = true
images = true
reasoning = false
cache = false

[[providers.models]]
id = "gpt-4o-mini"
name = "GPT-4o mini"
protocol = "openai-chat"
context_window = 128000
max_output_tokens = 16384
[providers.models.capabilities]
tools = true
images = true
reasoning = false
cache = false

# ---------------------------------------------------------------------------
# Grok (xAI)
# ---------------------------------------------------------------------------

[[providers]]
id = "grok"

[[providers.models]]
id = "grok-4.7"
name = "Grok 4.7"
protocol = "openai-responses"
context_window = 500000
max_output_tokens = 500000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "grok-4.6"
name = "Grok 4.6"
protocol = "openai-responses"
context_window = 500000
max_output_tokens = 500000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "grok-4.5"
name = "Grok 4.5"
protocol = "openai-responses"
context_window = 500000
max_output_tokens = 500000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "grok-4.3"
name = "Grok 4.3"
protocol = "openai-responses"
context_window = 1000000
max_output_tokens = 30000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

# ---------------------------------------------------------------------------
# OpenCode
# ---------------------------------------------------------------------------

[[providers]]
id = "opencode"

[[providers.models]]
id = "claude-opus-5-5"
name = "Claude Opus 5.5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-5"
name = "Claude Opus 5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-4-8"
name = "Claude Opus 4.8"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-4-7"
name = "Claude Opus 4.7"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-4-6"
name = "Claude Opus 4.6"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-4-5"
name = "Claude Opus 4.5"
protocol = "anthropic-messages"
context_window = 200000
max_output_tokens = 64000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-sonnet-5-5"
name = "Claude Sonnet 5.5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-sonnet-5"
name = "Claude Sonnet 5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-sonnet-4-6"
name = "Claude Sonnet 4.6"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 64000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-sonnet-4-5"
name = "Claude Sonnet 4.5"

[[providers.models]]
id = "claude-fable-5-2"
name = "Claude Fable 5.2"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-fable-5-1"
name = "Claude Fable 5.1"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-fable-5"
name = "Claude Fable 5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-haiku-5-5"
name = "Claude Haiku 5.5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-haiku-4-5"
name = "Claude Haiku 4.5"
protocol = "anthropic-messages"
context_window = 200000
max_output_tokens = 64000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6.2-astra"
name = "GPT-6.2 Astra"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6.2-sol"
name = "GPT-6.2 Sol"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6.1-sol"
name = "GPT-6.1 Sol"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6-astra"
name = "GPT-6 Astra"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6-luna"
name = "GPT-6 Luna"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6-sol"
name = "GPT-6 Sol"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.6-luna"
name = "GPT-5.6 Luna"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.6-sol"
name = "GPT-5.6 Sol"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.6-terra"
name = "GPT-5.6 Terra"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.5-pro"
name = "GPT-5.5 Pro"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.5"
name = "GPT-5.5"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.4-pro"
name = "GPT-5.4 Pro"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.4"
name = "GPT-5.4"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.4-mini"
name = "GPT-5.4 Mini"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.4-nano"
name = "GPT-5.4 Nano"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.3-codex"
name = "GPT-5.3 Codex"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.2-codex"
name = "GPT-5.2 Codex"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.2"
name = "GPT-5.2"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.1-codex"
name = "GPT-5.1 Codex"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.1"
name = "GPT-5.1"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5"
name = "GPT-5"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5-codex"
name = "GPT-5 Codex"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-4.1"
name = "GPT-4.1"

[[providers.models]]
id = "grok-4.7"
name = "Grok 4.7"
protocol = "openai-responses"
context_window = 500000
max_output_tokens = 500000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "grok-4.6"
name = "Grok 4.6"
protocol = "openai-responses"
context_window = 500000
max_output_tokens = 500000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "grok-4.5"
name = "Grok 4.5"
protocol = "openai-responses"
context_window = 500000
max_output_tokens = 500000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "grok-build-0.1"
name = "Grok Build 0.1"
protocol = "openai-responses"
context_window = 256000
max_output_tokens = 256000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "muse-spark-1.3"
name = "Muse Spark 1.3"
protocol = "openai-responses"
context_window = 1048576
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "muse-spark-1.2"
name = "Muse Spark 1.2"
protocol = "openai-responses"
context_window = 1048576
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "muse-wonder-1.0"
name = "Muse Wonder"
protocol = "openai-responses"
context_window = 1048576
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "muse-ember-1.0"
name = "Muse Ember 1.0"
protocol = "openai-responses"
context_window = 1048576
max_output_tokens = 270000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "glm-5.3"
name = "GLM-5.3"
protocol = "openai-chat"
context_window = 1000000
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "glm-5.3-flash"
name = "GLM-5.3 Flash"
protocol = "openai-chat"
context_window = 1000000
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "glm-5.2"
name = "GLM-5.2"
protocol = "openai-chat"
context_window = 1000000
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "glm-5.1"
name = "GLM-5.1"
protocol = "openai-chat"
context_window = 204800
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "glm-5"
name = "GLM-5"
protocol = "openai-chat"
context_window = 204800
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "deepseek-v4-pro"
name = "DeepSeek V4 Pro"
protocol = "openai-chat"
context_window = 1000000
max_output_tokens = 384000
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "deepseek-v4.1-flash"
name = "DeepSeek V4.1 Flash"
protocol = "openai-chat"
context_window = 1000000
max_output_tokens = 384000
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "deepseek-v4.1-flash-free"
name = "DeepSeek V4.1 Flash Free"
protocol = "openai-chat"
context_window = 200000
max_output_tokens = 32000
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "kimi-k3"
name = "Kimi K3"
protocol = "openai-chat"
context_window = 1048576
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "kimi-k2.7-code"
name = "Kimi K2.7 Code"
protocol = "openai-chat"
context_window = 262144
max_output_tokens = 262144
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "kimi-k2.6"
name = "Kimi K2.6"
protocol = "openai-chat"
context_window = 262144
max_output_tokens = 65536
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "kimi-k2.5"
name = "Kimi K2.5"
protocol = "openai-chat"
context_window = 262144
max_output_tokens = 65536
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "qwen3.8-max"
name = "Qwen 3.8 Max"
protocol = "openai-chat"
context_window = 262144
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "qwen3.8-flash"
name = "Qwen 3.8 Flash"
protocol = "openai-chat"
context_window = 1000000
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "qwen3.6-plus"
name = "Qwen 3.6 Plus"
protocol = "openai-chat"
context_window = 262144
max_output_tokens = 65536
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "qwen3.5-plus"
name = "Qwen 3.5 Plus"
protocol = "openai-chat"
context_window = 262144
max_output_tokens = 65536
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "minimax-m3"
name = "MiniMax M3"
protocol = "openai-chat"
context_window = 512000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "minimax-m2.7"
name = "MiniMax M2.7"
protocol = "openai-chat"
context_window = 204800
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "minimax-m2.5"
name = "MiniMax M2.5"
protocol = "openai-chat"
context_window = 204800
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "mistral-large-4"
name = "Mistral Large 4"
protocol = "openai-chat"
context_window = 524288
max_output_tokens = 262144
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "mimo-v2.6-flash-free"
name = "MiMo V2.6 Flash Free"
protocol = "openai-chat"
context_window = 200000
max_output_tokens = 32000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "nemotron-3.5-super-vl-free"
name = "Nemotron 3.5 Super VL Free"
protocol = "openai-chat"
context_window = 1048576
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "nemotron-3.5-lightning-free"
name = "Nemotron 3.5 Lightning Free"
protocol = "openai-chat"
context_window = 262144
max_output_tokens = 262144
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "beam-free"
name = "Beam Free"
protocol = "openai-chat"
context_window = 1048576
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "big-pickle"
name = "Big Pickle"
protocol = "openai-chat"
context_window = 200000
max_output_tokens = 32000
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "flint-40b-free"
name = "Flint 40B Free"
protocol = "openai-chat"
context_window = 262144
max_output_tokens = 65536
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "hy4-flash-free"
name = "Hy4 Flash Free"
protocol = "openai-chat"
context_window = 1048576
max_output_tokens = 65536
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "iceman-free"
name = "Iceman Free"
protocol = "openai-chat"
context_window = 262144
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

[[providers.models]]
id = "tidepool-alpha-free"
name = "Tidepool Alpha Free"
protocol = "openai-chat"
context_window = 1048576
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = false

# ---------------------------------------------------------------------------
# Command Code
# ---------------------------------------------------------------------------

[[providers]]
id = "commandcode"

[[providers.models]]
id = "claude-opus-5-5"
name = "Claude Opus 5.5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-5"
name = "Claude Opus 5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-4-8"
name = "Claude Opus 4.8"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-4-7"
name = "Claude Opus 4.7"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-opus-4-6"
name = "Claude Opus 4.6"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-sonnet-5-5"
name = "Claude Sonnet 5.5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-sonnet-5"
name = "Claude Sonnet 5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-sonnet-4-6"
name = "Claude Sonnet 4.6"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 64000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-fable-5-2"
name = "Claude Fable 5.2"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-fable-5-1"
name = "Claude Fable 5.1"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-fable-5"
name = "Claude Fable 5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-haiku-5-5"
name = "Claude Haiku 5.5"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "claude-haiku-4-5-20251001"
name = "Claude Haiku 4.5 (2025-10-01)"
protocol = "anthropic-messages"
context_window = 200000
max_output_tokens = 64000
[providers.models.capabilities]
tools = true
images = true
reasoning = false
cache = true

[[providers.models]]
id = "gpt-6.2-astra"
name = "GPT-6.2 Astra"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6.2-sol"
name = "GPT-6.2 Sol"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6.1-sol"
name = "GPT-6.1 Sol"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6.1-terra"
name = "GPT-6.1 Terra"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6-astra"
name = "GPT-6 Astra"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6-luna"
name = "GPT-6 Luna"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-6-sol"
name = "GPT-6 Sol"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.6-luna"
name = "GPT-5.6 Luna"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.6-sol"
name = "GPT-5.6 Sol"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.6-terra"
name = "GPT-5.6 Terra"
protocol = "openai-responses"
context_window = 1050000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.5"
name = "GPT-5.5"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.4"
name = "GPT-5.4"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.4-mini"
name = "GPT-5.4 mini"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

[[providers.models]]
id = "gpt-5.3-codex"
name = "GPT-5.3 Codex"
protocol = "openai-responses"
context_window = 400000
max_output_tokens = 128000
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = true

# ---------------------------------------------------------------------------
# MiniMax
# ---------------------------------------------------------------------------

[[providers]]
id = "minimax"
name = "MiniMax"
base_url = "https://api.minimax.io/anthropic/v1"
base_url_env = "KEEL_MINIMAX_BASE_URL"

[providers.auth]
type = "bearer"
env = ["MINIMAX_API_KEY", "MiniMax_API_KEY"]

[[providers.models]]
id = "MiniMax-M3.1-Flash"
name = "MiniMax M3.1 Flash"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 524288
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = true

[[providers.models]]
id = "MiniMax-M3"
name = "MiniMax M3"
protocol = "anthropic-messages"
context_window = 1000000
max_output_tokens = 524288
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = true

[[providers.models]]
id = "MiniMax-M2.7-highspeed"
name = "MiniMax M2.7 Highspeed"
protocol = "anthropic-messages"
context_window = 204800
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = true

[[providers.models]]
id = "MiniMax-M2.7"
name = "MiniMax M2.7"
protocol = "anthropic-messages"
context_window = 204800
max_output_tokens = 131072
[providers.models.capabilities]
tools = true
images = false
reasoning = true
cache = true

# ---------------------------------------------------------------------------
# NeuralWatt
# ---------------------------------------------------------------------------

[[providers]]
id = "neuralwatt"

[[providers.models]]
id = "nw-large"
name = "NeuralWatt Large"
protocol = "openai-chat"
context_window = 1048560
max_output_tokens = 1048560
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "nw-flash"
name = "NeuralWatt Flash"
protocol = "openai-chat"
context_window = 1048560
max_output_tokens = 393216
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "nw-small"
name = "NeuralWatt Small"

[[providers.models]]
id = "glm-5.3"
name = "GLM 5.3"
protocol = "openai-chat"
context_window = 1048560
max_output_tokens = 1048560
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "glm-5.3-flash"
name = "GLM-5.3 Flash"
protocol = "openai-chat"
context_window = 1048560
max_output_tokens = 1048560
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "kimi-k3.1-pro"
name = "Kimi K3.1 Pro"
protocol = "openai-chat"
context_window = 1048560
max_output_tokens = 1048560
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "deepseek-v4.1-flash-speed"
name = "DeepSeek V4.1 Flash (Speed)"
protocol = "openai-chat"
context_window = 1048560
max_output_tokens = 393216
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "mimo-v2.6-pro"
name = "MiMo-V2.6-Pro"
protocol = "openai-chat"
context_window = 1048560
max_output_tokens = 393216
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "qwen3.6-35b"
name = "Qwen 3.6 35B"
protocol = "openai-chat"
context_window = 262128
max_output_tokens = 131056
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false

[[providers.models]]
id = "qwen3.6-35b-fast"
name = "Qwen 3.6 35B Fast"
protocol = "openai-chat"
context_window = 262128
max_output_tokens = 131056
[providers.models.capabilities]
tools = true
images = true
reasoning = false
cache = false

[[providers.models]]
id = "gemma-4-31b"
name = "Gemma 4 31B"
protocol = "openai-chat"
context_window = 262128
max_output_tokens = 16384
[providers.models.capabilities]
tools = true
images = true
reasoning = true
cache = false
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use keel_tools::Tool;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    struct TmpDir(PathBuf);

    impl TmpDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir();
            let base = if base.as_os_str().len() <= 70 {
                base
            } else {
                PathBuf::from("/tmp")
            };
            let short_label: String = label.chars().take(4).collect();
            let path = base.join(format!("kn-{short_label}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn label_stream_emits_once_and_strips_commands() {
        let mut labels = LabelStream::default();
        assert!(labels.on_delta(0, "c1", "{\"label\": \"Reading ").is_none());
        let ev = labels
            .on_delta(0, "c1", "config file\"}")
            .expect("label emits when closed");
        match ev {
            AgentEvent::ToolLabel { tool_id, label, .. } => {
                assert_eq!(tool_id, "c1");
                assert_eq!(label, "Reading config file");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(labels.emit_if_new("c1", "Other".into()).is_none());
    }

    #[test]
    fn labelled_tool_adds_label_to_schema_and_strips_before_execute() {
        struct Echo;
        impl Tool for Echo {
            fn name(&self) -> &str {
                "echo"
            }
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: "echo".into(),
                    description: "echo".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": { "x": { "type": "string" } }
                    }),
                }
            }
            fn execute<'a>(&'a self, args: Value, _ctx: &'a ToolContext) -> ToolFuture<'a> {
                Box::pin(async move { ToolOutcome::ok(args.to_string()) })
            }
        }
        let wrapped = LabelledTool(Echo);
        let spec = wrapped.spec();
        assert!(spec.input_schema["properties"].get("label").is_some());

        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = TmpDir::new("labelled");
        let ctx = ToolContext {
            workspace: Workspace::open(dir.path()).unwrap(),
            cancel: CancellationToken::new(),
            limits: Default::default(),
            session_state: None,
            sandbox: false,
            plan_mode: false,
        };
        let out = rt.block_on(wrapped.execute(json!({"x": "ok", "label": "Testing"}), &ctx));
        assert_eq!(out.text, r#"{"x":"ok"}"#);
    }

    #[tokio::test]
    async fn catalog_models_and_reasoning_levels_resolve() {
        let harness = PiHarness::keel();
        assert!(harness.installed());
        assert_eq!(harness.id(), HarnessId::Keel);
        assert!(harness.has_one_shot(&ModelId("anthropic/claude-opus-5-5".into())));

        let loaded = load_catalog();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let issues = loaded.catalog.validate();
        assert!(issues.is_empty(), "{issues:?}");

        let dir = TmpDir::new("models");
        let store = CredentialStore::at(dir.path().join("credentials.json"));
        store.set("anthropic", "sk-ant-test").unwrap();
        store.set("openai", "sk-openai-test").unwrap();
        store.set("grok", "xai-test").unwrap();

        let models = list_models_for_with(&loaded.catalog, Some(&store), &|_| None);
        assert_eq!(models[0].id.0, "anthropic/claude-opus-5-5");
        assert_eq!(models[0].name, "Claude Opus 5.5");
        assert!(models.iter().any(|m| m.id.0 == "anthropic/claude-sonnet-4-5" && m.name == "Claude Sonnet 4.5"));
        assert!(models.iter().any(|m| m.id.0 == "openai/gpt-6.2-astra" && m.name == "GPT-6.2 Astra"));
        assert!(models.iter().any(|m| m.id.0 == "openai/gpt-4.1" && m.name == "GPT-4.1"));
        assert!(models.iter().any(|m| m.id.0 == "xai/grok-4.7" && m.name == "Grok 4.7"));
        // Unconnected providers stay out of the model list.
        assert!(!models.iter().any(|m| m.id.0.starts_with("opencode/")));
        assert!(!models.iter().any(|m| m.id.0.starts_with("commandcode/")));
        assert!(!models.iter().any(|m| m.id.0.starts_with("minimax/")));

        let claude_levels = harness
            .reasoning_levels(&ModelId("anthropic/claude-opus-5-5".into()))
            .await
            .expect("levels");
        assert!(!claude_levels.is_empty());

        let gpt41_levels = harness
            .reasoning_levels(&ModelId("openai/gpt-4.1".into()))
            .await
            .expect("levels");
        assert!(gpt41_levels.is_empty());

        // Aliased xai/grok-4.7 and xai/grok-4 resolve through grok.
        let xai_levels = harness
            .reasoning_levels(&ModelId("xai/grok-4.7".into()))
            .await
            .expect("xai levels");
        assert!(!xai_levels.is_empty());
        let legacy_xai = harness
            .reasoning_levels(&ModelId("xai/grok-4".into()))
            .await
            .expect("legacy xai levels");
        assert!(!legacy_xai.is_empty());
    }

    #[test]
    fn legacy_auth_json_migrates_into_credential_store_once() {
        let dir = TmpDir::new("authmig");
        fs::write(
            dir.path().join("auth.json"),
            serde_json::to_string(&json!({
                "xai": {
                    "type": "oauth",
                    "access": "xai-access",
                    "refresh": "xai-refresh",
                    "expires": 1_800_000_000_000u64
                },
                "anthropic": {
                    "type": "oauth",
                    "access": "ant-access",
                    "refresh": "ant-refresh",
                    "expires": 1_800_000_000_000u64
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let store = credential_store_in(Some(dir.path())).expect("store");
        let xai = store.get_oauth("grok").unwrap().expect("grok oauth");
        assert_eq!(xai.access_token, "xai-access");
        assert_eq!(xai.refresh_token.as_deref(), Some("xai-refresh"));
        assert_eq!(xai.expires_at, Some(1_800_000_000));
        assert_eq!(xai.client_id.as_deref(), Some(GROK_DEFAULT_CLIENT_ID));
        assert_eq!(xai.token_url.as_deref(), Some(GROK_TOKEN_URL));

        let ant = store.get_oauth("anthropic").unwrap().expect("anthropic oauth");
        assert_eq!(ant.access_token, "ant-access");
        assert_eq!(ant.refresh_token.as_deref(), Some("ant-refresh"));
        assert_eq!(ant.expires_at, Some(1_800_000_000));
        assert_eq!(ant.client_id.as_deref(), Some(ANTHROPIC_OAUTH_CLIENT_ID));

        let catalog = load_catalog().catalog;
        let models = list_models_for_with(&catalog, Some(&store), &|_| None);
        assert!(models.iter().any(|m| m.id.0 == "anthropic/claude-opus-5-5"));
        assert!(models.iter().any(|m| m.id.0 == "xai/grok-4.7"));
        assert!(!models.iter().any(|m| m.id.0.starts_with("openai/")));

        // Subsequent updates in credentials.json are not overwritten by auth.json.
        store.set("anthropic", "sk-ant-explicit").unwrap();
        let reloaded = credential_store_in(Some(dir.path())).expect("store");
        assert_eq!(
            reloaded.get("anthropic").unwrap().as_deref(),
            Some("sk-ant-explicit")
        );
    }

    #[test]
    fn auto_permission_honors_modes_and_scratch() {
        let dir = TmpDir::new("perm");
        let make_req = |tool: &str, args: Value| {
            let (tx, _rx) = oneshot::channel();
            PermissionRequest {
                tool: tool.to_string(),
                summary: tool.to_string(),
                arguments: args,
                responder: tx,
            }
        };

        // Plan mode allows read-only & UI tools, denies edits.
        let req_edit = make_req("edit", json!({"path": "a.rs"}));
        assert!(matches!(
            auto_permission(&req_edit, PermissionMode::FullAccess, true, None),
            Some(PermissionDecision::Deny { .. })
        ));
        let req_read = make_req("read", json!({"path": "a.rs"}));
        assert_eq!(
            auto_permission(&req_read, PermissionMode::Ask, true, None),
            Some(PermissionDecision::Allow)
        );
        let req_ui = make_req("mcp__keel-ui__show_plan", json!({}));
        assert_eq!(
            auto_permission(&req_ui, PermissionMode::Ask, true, None),
            Some(PermissionDecision::Allow)
        );
        let req_view = make_req("view_image", json!({"path": "a.png"}));
        assert_eq!(
            auto_permission(&req_view, PermissionMode::Ask, true, None),
            Some(PermissionDecision::Allow)
        );

        // Ask mode requires prompting for edits and bash, auto-allows reads and memory_write.
        assert!(auto_permission(&req_edit, PermissionMode::Ask, false, None).is_none());
        let req_bash = make_req("bash", json!({"command": "ls"}));
        assert!(auto_permission(&req_bash, PermissionMode::Ask, false, None).is_none());
        let req_mem = make_req("memory_write", json!({}));
        assert_eq!(
            auto_permission(&req_mem, PermissionMode::Ask, false, None),
            Some(PermissionDecision::Allow)
        );
        for web_tool in ["web_search", "web_fetch"] {
            let req_web = make_req(web_tool, json!({}));
            assert_eq!(
                auto_permission(&req_web, PermissionMode::Ask, false, None),
                Some(PermissionDecision::Allow)
            );
            assert_eq!(
                auto_permission(&req_web, PermissionMode::Ask, true, None),
                Some(PermissionDecision::Allow)
            );
        }

        // AutoEdit allows edits, asks for bash.
        assert_eq!(
            auto_permission(&req_edit, PermissionMode::AutoEdit, false, None),
            Some(PermissionDecision::Allow)
        );
        assert!(auto_permission(&req_bash, PermissionMode::AutoEdit, false, None).is_none());

        // Scratch allows writes inside scratch dir, asks outside.
        let req_inside = make_req("write", json!({"path": "note.md"}));
        assert_eq!(
            auto_permission(&req_inside, PermissionMode::Ask, false, Some(dir.path())),
            Some(PermissionDecision::Allow)
        );
        let req_outside = make_req("write", json!({"path": "../escape.md"}));
        assert!(auto_permission(&req_outside, PermissionMode::Ask, false, Some(dir.path())).is_none());
    }

    #[test]
    fn normalize_skill_invocation_rewrites_slash_skill_prefix() {
        assert_eq!(
            normalize_skill_invocation("/skill:indonesian-natural halo"),
            "Invoke the `skill` tool with `skill: \"indonesian-natural\"` and follow its instructions.\n\nhalo"
        );
        assert_eq!(
            normalize_skill_invocation("  /skill:review"),
            "Invoke the `skill` tool with `skill: \"review\"` and follow its instructions."
        );
        assert_eq!(normalize_skill_invocation("hello world"), "hello world");
    }

    #[tokio::test]
    async fn engine_ipc_call_speaks_framed_hello_and_method() {
        let dir = TmpDir::new("ipc");
        let sock_path = dir.path().join("keel.sock");
        fs::write(dir.path().join("ipc.token"), "secret-tok\n").unwrap();

        let listener = UnixListener::bind(&sock_path).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            for _ in 0..2 {
                let mut head = [0u8; 4];
                stream.read_exact(&mut head).await.unwrap();
                let len = u32::from_be_bytes(head) as usize;
                let mut buf = vec![0u8; len];
                stream.read_exact(&mut buf).await.unwrap();
                let msg: Value = serde_json::from_slice(&buf).unwrap();
                let reply = if msg["id"] == "h" {
                    assert_eq!(msg["params"]["token"], "secret-tok");
                    assert_eq!(msg["params"]["proto"], MEMORY_PROTO);
                    json!({"id": "h", "result": {"proto": 1}})
                } else {
                    assert_eq!(msg["method"], "memory_prompt");
                    json!({"id": "m", "result": {"text": "Keel notes"}})
                };
                let body = serde_json::to_vec(&reply).unwrap();
                stream
                    .write_all(&(body.len() as u32).to_be_bytes())
                    .await
                    .unwrap();
                stream.write_all(&body).await.unwrap();
                stream.flush().await.unwrap();
            }
        });

        let text = fetch_memory_prompt(dir.path(), dir.path()).await;
        assert_eq!(text, "Keel notes");
        server.await.unwrap();
    }

    #[test]
    fn auth_status_reads_credential_store_and_reports_providers() {
        let dir = TmpDir::new("auth");
        let store = CredentialStore::at(dir.path().join("credentials.json"));
        store.set("anthropic", "sk-ant-test").unwrap();
        let catalog = Catalog::builtin();
        let rows = build_auth_rows(catalog, Some(&store));
        let anthropic = rows.iter().find(|r| r.id == "anthropic").unwrap();
        assert!(anthropic.signed_in);
        assert_eq!(anthropic.detail, "api_key");
        assert!(!anthropic.supports_oauth);
        let xai = rows.iter().find(|r| r.id == "xai").unwrap();
        assert!(xai.supports_oauth);
        let openai = rows.iter().find(|r| r.id == "openai").unwrap();
        assert!(openai.supports_oauth);
        assert!(!rows.iter().any(|r| matches!(r.id.as_str(), "minimax" | "neuralwatt")));
    }

    #[test]
    fn openai_oauth_routes_to_codex_backend_and_filters_to_responses_models() {
        let dir = TmpDir::new("openai-oauth");
        let store = CredentialStore::at(dir.path().join("credentials.json"));
        let tokens = OAuthTokenSet {
            access_token: "test-oauth-access".to_string(),
            refresh_token: Some("test-oauth-refresh".to_string()),
            expires_at: Some(9_999_999_999),
            token_type: "Bearer".to_string(),
            client_id: Some(OPENAI_DEFAULT_CLIENT_ID.to_string()),
            token_url: Some(OPENAI_TOKEN_URL.to_string()),
        };
        store.set_oauth("openai", &tokens).unwrap();

        let catalog = load_catalog().catalog;
        let rows = build_auth_rows(&catalog, Some(&store));
        let openai = rows.iter().find(|r| r.id == "openai").unwrap();
        assert!(openai.signed_in);
        assert_eq!(openai.detail, "oauth");
        assert!(openai.supports_oauth);

        let models = list_models_for_with(&catalog, Some(&store), &|_| None);
        assert!(
            models.iter().any(|m| m.id.0 == "openai/gpt-5.3-codex"),
            "expected Codex Responses model under OpenAI OAuth"
        );
        assert!(
            !models.iter().any(|m| m.id.0 == "openai/gpt-4.1"),
            "openai-chat models should be hidden under OpenAI subscription OAuth"
        );

        let (provider, codex_model) = catalog.find("openai", "gpt-5.3-codex").unwrap();
        let codex_route = build_route_for_store(provider, codex_model, Some(&store)).unwrap();
        assert_eq!(
            codex_route.endpoint,
            keel_llm_route::OPENAI_CODEX_RESPONSES_URL
        );
        assert!(matches!(
            codex_route.auth,
            keel_llm_route::Auth::OpenAiCodexBearer { .. }
        ));

        // Switching to a plain API key restores the standard Platform API route.
        store.set("openai", "sk-openai-test").unwrap();
        let api_route = build_route_for_store(provider, codex_model, Some(&store)).unwrap();
        assert_eq!(api_route.endpoint, "https://api.openai.com/v1/responses");
        assert!(matches!(api_route.auth, keel_llm_route::Auth::Bearer { .. }));
    }

    #[test]
    fn set_and_remove_credentials_also_clears_legacy_auth_json() {
        let dir = TmpDir::new("auth-rm");
        fs::write(
            dir.path().join("auth.json"),
            serde_json::to_string(&json!({
                "xai": {
                    "type": "oauth",
                    "access": "xai-access",
                    "refresh": "xai-refresh",
                    "expires": 1_800_000_000_000u64
                },
                "opencode": {
                    "type": "api_key",
                    "key": "sk-opencode-old"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let store = credential_store_in(Some(dir.path())).expect("store");
        assert!(store.get_oauth("grok").unwrap().is_some());
        assert_eq!(
            store.get("opencode").unwrap().as_deref(),
            Some("sk-opencode-old")
        );

        // Edit API key via set_api_key_with (including trimming whitespace).
        set_api_key_with(&store, "opencode", "  sk-opencode-new \n").unwrap();
        assert_eq!(
            store.get("opencode").unwrap().as_deref(),
            Some("sk-opencode-new")
        );
        assert!(set_api_key_with(&store, "opencode", "   ").is_err());

        // Disconnect xAI subscription; reloading store must not resurrect it from auth.json.
        remove_credential_with(&store, Some(dir.path()), "xai").unwrap();
        let reloaded = credential_store_in(Some(dir.path())).expect("reloaded");
        assert!(reloaded.get_oauth("grok").unwrap().is_none());
        assert!(reloaded.get("grok").unwrap().is_none());
        assert_eq!(
            reloaded.get("opencode").unwrap().as_deref(),
            Some("sk-opencode-new")
        );

        // Remove OpenCode API key too.
        remove_credential_with(&reloaded, Some(dir.path()), "opencode").unwrap();
        let reloaded2 = credential_store_in(Some(dir.path())).expect("reloaded2");
        assert!(reloaded2.get("opencode").unwrap().is_none());
    }

    #[test]
    fn keel_secret_resolver_prefers_store_and_honors_disabled_env() {
        let dir = TmpDir::new("resolver");
        let store = CredentialStore::at(dir.path().join("credentials.json"));
        // Even if an env var name exists in CredentialRef, a stored key wins.
        store.set("openai", "sk-stored-override").unwrap();
        let resolver = KeelSecretResolver::new(store.clone());
        let cref = CredentialRef::from_env("PATH").with_store_key("openai");
        assert_eq!(resolver.resolve(&cref).as_deref(), Some("sk-stored-override"));

        // Removing the stored key and marking env disabled suppresses env fallback.
        store.remove("openai").unwrap();
        set_env_disabled(&store, "openai", true).unwrap();
        let resolver = KeelSecretResolver::new(store.clone());
        assert_eq!(resolver.resolve(&cref), None);

        // Re-enabling via set_api_key_with clears disabled_env and uses the new key.
        set_api_key_with(&store, "openai", "sk-readded").unwrap();
        let resolver = KeelSecretResolver::new(store);
        assert_eq!(resolver.resolve(&cref).as_deref(), Some("sk-readded"));
    }

    #[test]
    fn fresh_system_prompt_keeps_single_identity_and_harness_working_rules() {
        let catalog = load_catalog().catalog;
        let (provider, model_spec) =
            resolve_model(&catalog, &EnvResolver, None, "openai/gpt-4.1").unwrap();
        let route = build_route(&provider, &model_spec).unwrap();
        let client = Client::with_resolver(Arc::new(EnvResolver)).unwrap();
        let mut session = Session::new(route, client);

        configure_fresh_system_prompt(
            &mut session,
            KeelProfile::default().system_prompt.as_deref(),
            true,
            true,
            None,
            None,
            Some("User prefers short diffs."),
        );

        let sys = session.system.as_deref().expect("system prompt");
        assert!(sys.starts_with(KEEL_SYSTEM_PROMPT));
        assert!(
            !sys.contains("You are keel, a coding agent."),
            "app session should not stack the coding-agent identity before companion identity: {sys}"
        );
        assert!(sys.contains(keel_core::default_working_rules()));
        assert!(sys.contains(crate::UI_CARDS_PROMPT));
        assert!(sys.contains(crate::SCRATCH_PROMPT));
        assert!(sys.contains("<keel_memory>\nUser prefers short diffs.\n</keel_memory>"));
    }

    #[test]
    fn plan_mode_prompt_helper_unifies_ui_and_sandbox_guidance() {
        assert!(keel_plan_mode_prompt(false, true).is_none());
        assert!(keel_plan_mode_prompt(false, false).is_none());

        let without_bash = keel_plan_mode_prompt(true, false).expect("ui plan prompt");
        assert!(without_bash.contains("show_plan"));
        assert!(!without_bash.contains("network-disabled OS sandbox"));

        let with_bash = keel_plan_mode_prompt(true, true).expect("ui plan prompt with bash");
        assert!(with_bash.contains("show_plan"));
        assert!(with_bash.contains("read-only, network-disabled OS sandbox"));
    }

    #[test]
    fn resume_restores_pinned_system_prompt_and_tracks_memory_updates() {
        let dir = TmpDir::new("resume-sys");
        let catalog = load_catalog().catalog;
        let (provider, model_spec) =
            resolve_model(&catalog, &EnvResolver, None, "openai/gpt-4.1").unwrap();
        let route = build_route(&provider, &model_spec).unwrap();
        let client = Client::with_resolver(Arc::new(EnvResolver)).unwrap();

        let mut fresh = Session::new(route.clone(), client.clone());
        let instructions_dir = TmpDir::new("resume-instructions");
        fs::write(
            instructions_dir.path().join("AGENTS.md"),
            "Repo rule: always run cargo test.",
        )
        .unwrap();
        let instructions = ProjectInstructions::discover_with_home(instructions_dir.path(), None);
        configure_fresh_system_prompt(
            &mut fresh,
            Some(KEEL_SYSTEM_PROMPT),
            false,
            false,
            None,
            Some(&instructions),
            Some("v1 memory"),
        );
        let pinned = fresh.system.clone().expect("pinned system");
        let pos_inst = pinned
            .find("Repo rule: always run cargo test.")
            .expect("project instructions included");
        let pos_mem = pinned
            .find("<keel_memory>\nv1 memory\n</keel_memory>")
            .expect("memory included");
        assert!(pos_inst < pos_mem, "instructions should precede memory");
        let (_log, id) =
            create_session_log(dir.path(), &provider, &model_spec, dir.path(), &fresh).unwrap();

        let mut resumed = Session::new(route, client);
        let opened = try_resume_session(dir.path(), Some(&NativeSessionId(id)), &mut resumed)
            .expect("resumed session");
        assert!(opened.has_stored_system);
        assert_eq!(resumed.system.as_deref(), Some(pinned.as_str()));

        assert_eq!(last_known_memory(&resumed).as_deref(), Some("v1 memory"));
        assert!(!should_emit_memory_update(Some("v1 memory"), "v1 memory"));
        assert!(!should_emit_memory_update(
            Some("v1 memory"),
            &format!("{MEMORY_PREAMBLE}\n\n{MEMORY_UNAVAILABLE}")
        ));
        assert!(should_emit_memory_update(Some("v1 memory"), "v2 memory"));

        resumed.push_message(Message::user(
            "<keel_memory_update>\nv2 memory\n</keel_memory_update>",
        ));
        assert_eq!(last_known_memory(&resumed).as_deref(), Some("v2 memory"));
        assert!(!should_emit_memory_update(Some("v2 memory"), "v2 memory"));
    }

    #[test]
    fn history_ends_in_plan_mode_tracks_on_and_off_transitions() {
        let mut history = vec![Message::user("hello")];
        assert!(!history_ends_in_plan_mode(&history));

        history.push(Message::user("[plan mode]\nPlan mode is on."));
        assert!(history_ends_in_plan_mode(&history));

        history.push(Message::user(format!(
            "[plan mode]\n{}",
            keel_core::PLAN_MODE_OFF_NOTICE
        )));
        assert!(!history_ends_in_plan_mode(&history));
    }

    #[test]
    fn subagent_events_translate_without_duplicate_parent_tool_row() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tr = EventTranslator::new(
            tx,
            "grok/grok-4.20".into(),
            200_000,
            PathBuf::from("/tmp"),
        );
        tr.on_event(RunEvent::ToolStarted {
            call_id: "call-1",
            name: AGENT_TOOL_NAME,
            summary: "agent explore",
            arguments: r#"{"agent":"explore","description":"Scan crates","prompt":"Check"}"#,
        });
        tr.on_event(RunEvent::Subagent(&SubagentEvent::Started {
            id: "sa-001".into(),
            parent_call_id: Some("call-1".into()),
            agent: "explore".into(),
            description: "Scan crates".into(),
            depth: 1,
            model: "grok/grok-4.20".into(),
            read_only: true,
            resumed: false,
            worktree: None,
            worktree_branch: None,
        }));
        tr.on_event(RunEvent::Subagent(&SubagentEvent::ToolStarted {
            id: "sa-001".into(),
            agent: "explore".into(),
            call_id: "c-tool-1".into(),
            tool: "search".into(),
            summary: "search Cargo.toml".into(),
            arguments: r#"{"pattern":"keel-core"}"#.into(),
        }));
        tr.on_event(RunEvent::Subagent(&SubagentEvent::TextDelta {
            id: "sa-001".into(),
            agent: "explore".into(),
            index: 0,
            delta: "Found it.".into(),
        }));
        tr.on_event(RunEvent::Subagent(&SubagentEvent::ToolFinished {
            id: "sa-001".into(),
            agent: "explore".into(),
            call_id: "c-tool-1".into(),
            tool: "search".into(),
            is_error: false,
            output: "Cargo.toml:1: keel-core".into(),
            duration_ms: Some(42),
        }));
        tr.on_event(RunEvent::Subagent(&SubagentEvent::Finished {
            id: "sa-001".into(),
            agent: "explore".into(),
            model: "grok/grok-4.20".into(),
            stop: RunStop::EndTurn,
            turns: 2,
            usage: keel_llm_types::Usage {
                input_tokens: 1200,
                output_tokens: 150,
                ..Default::default()
            },
            duration_ms: 820,
            worktree: None,
            worktree_branch: None,
        }));
        tr.on_event(RunEvent::ToolFinished {
            call_id: "call-1",
            name: AGENT_TOOL_NAME,
            is_error: false,
            preview: "ok",
            output: "Found it.",
            images: &[],
            duration_ms: Some(825),
        });

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev.unwrap());
        }
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolStarted { subagent: None, .. })),
            "parent agent tool call must not emit a duplicate top-level tool row"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::SubagentStarted { tool_id, subagent_id, name, description }
            if tool_id == "call-1" && subagent_id == "sa-001" && name == "explore" && description == "Scan crates"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Text { delta, subagent: Some(sub), .. }
            if delta == "Found it." && sub.id == "sa-001"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Usage { usage }
            if usage.input == 1200 && usage.output == 150 && usage.context_window.is_none()
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::SubagentMeta { subagent_id, turns: Some(2), duration_ms: Some(820), .. }
            if subagent_id == "sa-001"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::SubagentDone { subagent_id, status: TurnStatus::Completed {} }
            if subagent_id == "sa-001"
        )));
    }

    #[test]
    fn agent_tool_is_auto_allowed_and_subagent_permission_prefix_parses() {
        let (tx, _rx) = oneshot::channel();
        let req = PermissionRequest {
            tool: AGENT_TOOL_NAME.into(),
            summary: "agent explore".into(),
            arguments: json!({"agent": "explore", "prompt": "check"}),
            responder: tx,
        };
        assert!(matches!(
            auto_permission(&req, PermissionMode::Ask, false, None),
            Some(PermissionDecision::Allow)
        ));
        assert!(matches!(
            auto_permission(&req, PermissionMode::Ask, true, None),
            Some(PermissionDecision::Allow)
        ));
        assert_eq!(
            parse_subagent_permission_prefix("[agent:worker#sa-002] bash cargo test"),
            Some(("worker", "sa-002"))
        );
        assert_eq!(parse_subagent_permission_prefix("bash cargo test"), None);
    }
}

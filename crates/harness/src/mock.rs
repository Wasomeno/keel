//! Scripted harness. It does not spawn a process.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::Stream;
use futures::stream;
use keel_proto::{AgentEvent, HarnessId};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
    ControlMsg, Harness, HarnessError, Model, ModelId, ReasoningLevel, RunControls, SessionSpec,
};

/// One step of a scripted turn.
#[derive(Debug, Clone)]
pub enum Step {
    /// Yield this event.
    Emit(AgentEvent),
    /// Read one [`ControlMsg`] and record it before the next step.
    Wait,
    /// Admit a tool through the plan-mode gate and yield its result.
    Tool(String),
}

#[derive(Clone, Copy)]
enum Mode {
    Script,
    Echo,
    Silent,
}

#[derive(Clone, Debug)]
pub struct MockProcessInfo {
    pub process_id: usize,
    pub session_id: String,
    pub last_active: std::time::Instant,
    pub crashed: bool,
}

/// Mock driver used by the engine tests and by the binary until a CLI driver exists.
pub struct MockHarness {
    mode: Mode,
    steps: Mutex<Vec<Step>>,
    observed: Arc<Mutex<Vec<ControlMsg>>>,
    runs: Arc<AtomicUsize>,
    initial: Arc<Mutex<String>>,
    attached: Arc<Mutex<Vec<crate::AttachmentFile>>>,
    resume: Arc<Mutex<Option<String>>>,
    plan_mode: Arc<std::sync::atomic::AtomicBool>,
    scratch: Arc<std::sync::atomic::AtomicBool>,
    processes: Arc<Mutex<HashMap<String, MockProcessInfo>>>,
    next_process_id: Arc<AtomicUsize>,
    process_spawns: Arc<AtomicUsize>,
}

impl MockHarness {
    pub fn script(steps: Vec<Step>) -> Self {
        Self::new(Mode::Script, steps)
    }

    /// Yields one text delta and `Done { Completed }`.
    pub fn echo() -> Self {
        Self::new(Mode::Echo, Vec::new())
    }

    /// Records controls and yields nothing.
    pub fn silent() -> Self {
        Self::new(Mode::Silent, Vec::new())
    }

    pub fn observed(&self) -> Vec<ControlMsg> {
        self.observed.lock().expect("observed").clone()
    }

    pub fn runs(&self) -> usize {
        self.runs.load(Ordering::Relaxed)
    }

    /// `initial_text` from the latest `run`.
    /// Images the last run was handed.
    pub fn attachments(&self) -> Vec<crate::AttachmentFile> {
        self.attached.lock().expect("attached").clone()
    }

    pub fn initial_text(&self) -> String {
        self.initial.lock().expect("initial").clone()
    }

    /// `resume` from the latest `run`.
    pub fn resumed(&self) -> Option<String> {
        self.resume.lock().expect("resume").clone()
    }

    /// Plan mode from the latest `run`.
    pub fn plan_mode(&self) -> bool {
        self.plan_mode.load(Ordering::Relaxed)
    }

    /// Scratch flag from the latest `run`.
    pub fn scratch(&self) -> bool {
        self.scratch.load(Ordering::Relaxed)
    }

    pub fn process_spawns(&self) -> usize {
        self.process_spawns.load(Ordering::Relaxed)
    }

    pub fn active_process(&self, chat_id: &str) -> Option<MockProcessInfo> {
        self.processes.lock().expect("processes").get(chat_id).cloned()
    }

    pub fn simulate_crash(&self, chat_id: &str) {
        if let Some(proc) = self.processes.lock().expect("processes").get_mut(chat_id) {
            proc.crashed = true;
        }
    }

    pub fn evict_idle(&self, timeout: std::time::Duration) {
        let mut procs = self.processes.lock().expect("processes");
        procs.retain(|_, p| p.last_active.elapsed() < timeout);
    }

    fn new(mode: Mode, steps: Vec<Step>) -> Self {
        Self {
            mode,
            steps: Mutex::new(steps),
            observed: Arc::new(Mutex::new(Vec::new())),
            runs: Arc::new(AtomicUsize::new(0)),
            initial: Arc::new(Mutex::new(String::new())),
            attached: Arc::new(Mutex::new(Vec::new())),
            resume: Arc::new(Mutex::new(None)),
            plan_mode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            scratch: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            processes: Arc::new(Mutex::new(HashMap::new())),
            next_process_id: Arc::new(AtomicUsize::new(1)),
            process_spawns: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl Harness for MockHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }

    fn display_name(&self) -> &str {
        "Mock"
    }

    fn installed(&self) -> bool {
        true
    }

    fn executable(&self) -> Option<PathBuf> {
        None
    }

    fn version(&self) -> &str {
        "0"
    }

    fn deterministic_turn_end(&self) -> bool {
        true
    }

    fn evict_chat(&self, chat_id: &str) {
        self.processes.lock().expect("processes").remove(chat_id);
    }

    fn evict_all(&self) {
        self.processes.lock().expect("processes").clear();
    }

    /// The mock shows subagents, so the engine's routing is testable
    /// without a CLI.
    fn subagent_caps(&self) -> keel_proto::SubagentCaps {
        keel_proto::SubagentCaps {
            visibility: keel_proto::SubagentVisibility::Full,
        }
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![Model {
            id: ModelId("mock".to_string()),
            name: "Mock".to_string(),
        }])
    }

    async fn reasoning_levels(
        &self,
        _model: &ModelId,
    ) -> Result<Vec<ReasoningLevel>, HarnessError> {
        Ok(vec![ReasoningLevel("off".to_string())])
    }

    async fn run(
        &self,
        spec: SessionSpec,
        controls: RunControls,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<AgentEvent, HarnessError>> + Send>>, HarnessError>
    {
        let chat_id = spec
            .chat_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        {
            let mut procs = self.processes.lock().expect("processes");
            let needs_spawn = match procs.get(&chat_id) {
                Some(proc) => proc.crashed,
                None => true,
            };
            if needs_spawn {
                let pid = self.next_process_id.fetch_add(1, Ordering::Relaxed);
                self.process_spawns.fetch_add(1, Ordering::Relaxed);
                let session_id = spec
                    .resume
                    .as_ref()
                    .map(|r| r.0.clone())
                    .unwrap_or_else(|| format!("session-{pid}"));
                procs.insert(
                    chat_id.clone(),
                    MockProcessInfo {
                        process_id: pid,
                        session_id,
                        last_active: std::time::Instant::now(),
                        crashed: false,
                    },
                );
            } else if let Some(proc) = procs.get_mut(&chat_id) {
                proc.last_active = std::time::Instant::now();
            }
        }
        *self.resume.lock().expect("resume") = spec.resume.as_ref().map(|id| id.0.clone());
        *self.attached.lock().expect("attached") = spec.attachments.clone();
        *self.initial.lock().expect("initial") = spec.initial_text;
        self.plan_mode.store(spec.plan_mode, Ordering::Relaxed);
        self.scratch.store(spec.scratch, Ordering::Relaxed);
        self.runs.fetch_add(1, Ordering::Relaxed);
        let observed = Arc::clone(&self.observed);
        let plan_mode = spec.plan_mode;
        let stream: Pin<Box<dyn Stream<Item = Result<AgentEvent, HarnessError>> + Send>> =
            match self.mode {
                Mode::Echo => Box::pin(echo_stream(controls.inbound, observed)),
                Mode::Silent => Box::pin(silent_stream(controls.inbound, observed)),
                Mode::Script => {
                    let steps = self.steps.lock().expect("steps").clone();
                    Box::pin(script_stream(steps, controls.inbound, observed, plan_mode))
                }
            };
        Ok(stream)
    }
}

fn echo_stream(
    mut inbound: UnboundedReceiver<ControlMsg>,
    observed: Arc<Mutex<Vec<ControlMsg>>>,
) -> impl Stream<Item = Result<AgentEvent, HarnessError>> + Send {
    let mut phase = 0u8;
    stream::poll_fn(move |cx| {
        if phase == 0 {
            phase = 1;
            return std::task::Poll::Ready(Some(Ok(AgentEvent::Text {
                message_id: "m".to_string(),
                part_id: "p".to_string(),
                delta: "ok".to_string(),
                subagent: None,
            })));
        }
        if let std::task::Poll::Ready(Some(msg)) = inbound.poll_recv(cx) {
            let interrupt = matches!(msg, ControlMsg::Interrupt);
            observed.lock().expect("observed").push(msg);
            if interrupt {
                phase = 2;
                return std::task::Poll::Ready(Some(Ok(AgentEvent::Done {
                    status: keel_proto::TurnStatus::Interrupted {},
                })));
            }
        }
        if phase == 1 {
            phase = 2;
            return std::task::Poll::Ready(Some(Ok(AgentEvent::Done {
                status: keel_proto::TurnStatus::Completed {},
            })));
        }
        std::task::Poll::Ready(None)
    })
}

fn silent_stream(
    mut inbound: UnboundedReceiver<ControlMsg>,
    observed: Arc<Mutex<Vec<ControlMsg>>>,
) -> impl Stream<Item = Result<AgentEvent, HarnessError>> + Send {
    // Drain until `poll_recv` returns Pending so the waker stays registered.
    stream::poll_fn(move |cx| {
        loop {
            match inbound.poll_recv(cx) {
                std::task::Poll::Ready(Some(msg)) => observed.lock().expect("observed").push(msg),
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    })
}

fn tool_result(plan_mode: bool, name: &str) -> AgentEvent {
    let denied = plan_mode && !crate::plan_mode_allows(name);
    AgentEvent::ToolFinished {
        tool_id: format!("tool-{name}"),
        summary: if denied {
            format!("{name} denied in plan mode")
        } else {
            format!("{name} ok")
        },
        output_bytes: 0,
        is_error: denied,
        images: Vec::new(),
        output_tail: String::new(),
        subagent: None,
        diff: None,
        duration_ms: None,
    }
}

fn script_stream(
    steps: Vec<Step>,
    mut inbound: UnboundedReceiver<ControlMsg>,
    observed: Arc<Mutex<Vec<ControlMsg>>>,
    plan_mode: bool,
) -> impl Stream<Item = Result<AgentEvent, HarnessError>> + Send {
    let mut index = 0usize;
    stream::poll_fn(move |cx| {
        loop {
            let Some(step) = steps.get(index) else {
                return std::task::Poll::Ready(None);
            };
            match step {
                Step::Emit(event) => {
                    let event = event.clone();
                    index += 1;
                    return std::task::Poll::Ready(Some(Ok(event)));
                }
                Step::Tool(name) => {
                    let event = tool_result(plan_mode, name);
                    index += 1;
                    return std::task::Poll::Ready(Some(Ok(event)));
                }
                Step::Wait => match inbound.poll_recv(cx) {
                    std::task::Poll::Ready(Some(msg)) => {
                        observed.lock().expect("observed").push(msg);
                        index += 1;
                    }
                    std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
                    std::task::Poll::Pending => return std::task::Poll::Pending,
                },
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use keel_proto::{QuestionKind, TurnStatus, UserInputQuestion};

    fn spec() -> SessionSpec {
        SessionSpec {
            cwd: PathBuf::from("/tmp"),
            model: ModelId("mock".to_string()),
            reasoning: None,
            resume: None,
            initial_text: "hi".to_string(),
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
            scratch: false,
        }
    }

    #[tokio::test]
    async fn steer_is_observed_before_done() {
        let harness = MockHarness::script(vec![
            Step::Emit(AgentEvent::Text {
                message_id: "m".to_string(),
                part_id: "p".to_string(),
                delta: "a".to_string(),
                subagent: None,
            }),
            Step::Wait,
            Step::Emit(AgentEvent::Done {
                status: TurnStatus::Completed {},
            }),
        ]);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut stream = harness
            .run(spec(), RunControls { inbound: rx })
            .await
            .expect("stream");
        let first = stream.next().await.expect("text").expect("ok");
        assert!(matches!(first, AgentEvent::Text { .. }));
        tx.send(ControlMsg::Steer {
            text: "turn left".to_string(),
        })
        .expect("steer");
        let done = stream.next().await.expect("done").expect("ok");
        assert_eq!(
            done,
            AgentEvent::Done {
                status: TurnStatus::Completed {},
            }
        );
        assert_eq!(
            harness.observed(),
            vec![ControlMsg::Steer {
                text: "turn left".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn mock_emits_a_user_input_question() {
        let question = UserInputQuestion {
            id: "q1".to_string(),
            kind: QuestionKind::Confirm,
            prompt: "ship it?".to_string(),
            options: Vec::new(),
            required: true,
            tool: None,
            multi: false,
            other: false,
        };
        let harness = MockHarness::script(vec![Step::Emit(AgentEvent::Question {
            id: "ask".to_string(),
            questions: vec![question.clone()],
            subagent: None,
        })]);
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut stream = harness
            .run(spec(), RunControls { inbound: rx })
            .await
            .expect("stream");
        let event = stream.next().await.expect("event").expect("ok");
        assert_eq!(
            event,
            AgentEvent::Question {
                id: "ask".to_string(),
                questions: vec![question],
                subagent: None,
            }
        );
    }

    #[tokio::test]
    async fn silent_records_every_control_without_hanging() {
        let harness = MockHarness::silent();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut stream = harness
            .run(spec(), RunControls { inbound: rx })
            .await
            .expect("stream");
        let poll = tokio::spawn(async move { stream.next().await.is_none() });
        tx.send(ControlMsg::Interrupt).expect("one");
        tokio::task::yield_now().await;
        tx.send(ControlMsg::Interrupt).expect("two");
        drop(tx);
        let ended = tokio::time::timeout(std::time::Duration::from_secs(2), poll)
            .await
            .expect("silent stream hung")
            .expect("join");
        assert!(ended);
        assert_eq!(harness.observed().len(), 2);
    }

    #[tokio::test]
    async fn plan_mode_rejects_mutating_tools_and_allows_reads() {
        let names = [
            "edit",
            "write",
            "bash",
            "shell",
            "exec",
            "mcp__notes__memory_write",
            "browser_click",
            "read",
            "search",
            "list",
            "web_search",
            "browser_snapshot",
        ];
        let harness = MockHarness::script(names.iter().map(|n| Step::Tool((*n).to_string())).collect());
        let mut spec = spec();
        spec.plan_mode = true;
        spec.permission = keel_proto::PermissionMode::FullAccess;
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut stream = harness
            .run(spec, RunControls { inbound: rx })
            .await
            .expect("stream");
        let mut denied = Vec::new();
        let mut allowed = Vec::new();
        while let Some(item) = stream.next().await {
            match item.expect("ok") {
                AgentEvent::ToolFinished {
                    summary,
                    is_error,
                    ..
                } => {
                    if is_error {
                        denied.push(summary);
                    } else {
                        allowed.push(summary);
                    }
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(harness.plan_mode());
        assert_eq!(
            denied,
            [
                "edit denied in plan mode",
                "write denied in plan mode",
                "bash denied in plan mode",
                "shell denied in plan mode",
                "exec denied in plan mode",
                "mcp__notes__memory_write denied in plan mode",
                "browser_click denied in plan mode",
            ]
        );
        assert_eq!(
            allowed,
            [
                "read ok",
                "search ok",
                "list ok",
                "web_search ok",
                "browser_snapshot ok",
            ]
        );
    }

    #[tokio::test]
    async fn mock_process_lifecycle_reuses_process_and_session_id() {
        let harness = MockHarness::silent();
        let mut s1 = spec();
        s1.chat_id = Some("chat-test-1".to_string());
        let (_tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
        let _stream1 = harness.run(s1, RunControls { inbound: rx1 }).await.expect("run 1");

        assert_eq!(harness.process_spawns(), 1);
        let proc1 = harness.active_process("chat-test-1").expect("proc 1");
        assert_eq!(proc1.process_id, 1);
        let session_id_1 = proc1.session_id.clone();

        let mut s2 = spec();
        s2.chat_id = Some("chat-test-1".to_string());
        let (_tx2, rx2) = tokio::sync::mpsc::unbounded_channel();
        let _stream2 = harness.run(s2, RunControls { inbound: rx2 }).await.expect("run 2");

        assert_eq!(harness.process_spawns(), 1, "should reuse same process");
        let proc2 = harness.active_process("chat-test-1").expect("proc 2");
        assert_eq!(proc2.process_id, 1, "process id should be unchanged");
        assert_eq!(proc2.session_id, session_id_1, "session id should be unchanged");
    }

    #[tokio::test]
    async fn mock_process_idle_eviction_kills_process() {
        let harness = MockHarness::silent();
        let mut s1 = spec();
        s1.chat_id = Some("chat-test-2".to_string());
        let (_tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
        let _stream1 = harness.run(s1, RunControls { inbound: rx1 }).await.expect("run 1");

        assert_eq!(harness.process_spawns(), 1);
        assert!(harness.active_process("chat-test-2").is_some());

        // Evict idle
        harness.evict_idle(std::time::Duration::from_millis(0));
        assert!(harness.active_process("chat-test-2").is_none());

        let mut s2 = spec();
        s2.chat_id = Some("chat-test-2".to_string());
        let (_tx2, rx2) = tokio::sync::mpsc::unbounded_channel();
        let _stream2 = harness.run(s2, RunControls { inbound: rx2 }).await.expect("run 2");

        assert_eq!(harness.process_spawns(), 2, "should spawn new process after eviction");
        let proc2 = harness.active_process("chat-test-2").expect("proc 2");
        assert_eq!(proc2.process_id, 2);
    }

    #[tokio::test]
    async fn mock_process_respawn_after_crash_resumes_session() {
        let harness = MockHarness::silent();
        let mut s1 = spec();
        s1.chat_id = Some("chat-test-3".to_string());
        let (_tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
        let _stream1 = harness.run(s1, RunControls { inbound: rx1 }).await.expect("run 1");

        assert_eq!(harness.process_spawns(), 1);
        let proc1 = harness.active_process("chat-test-3").expect("proc 1");
        let session_id_1 = proc1.session_id.clone();

        // Simulate crash
        harness.simulate_crash("chat-test-3");
        assert!(harness.active_process("chat-test-3").unwrap().crashed);

        // Run next turn with resume
        let mut s2 = spec();
        s2.chat_id = Some("chat-test-3".to_string());
        s2.resume = Some(crate::NativeSessionId(session_id_1.clone()));
        let (_tx2, rx2) = tokio::sync::mpsc::unbounded_channel();
        let _stream2 = harness.run(s2, RunControls { inbound: rx2 }).await.expect("run 2");

        assert_eq!(harness.process_spawns(), 2, "should respawn after crash");
        let proc2 = harness.active_process("chat-test-3").expect("proc 2");
        assert_eq!(proc2.process_id, 2);
        assert_eq!(proc2.session_id, session_id_1, "should resume same session id");
        assert!(!proc2.crashed);
    }
}

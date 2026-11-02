//! Scripted harness. It does not spawn a process.

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
}

#[derive(Clone, Copy)]
enum Mode {
    Script,
    Echo,
    Silent,
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

    fn new(mode: Mode, steps: Vec<Step>) -> Self {
        Self {
            mode,
            steps: Mutex::new(steps),
            observed: Arc::new(Mutex::new(Vec::new())),
            runs: Arc::new(AtomicUsize::new(0)),
            initial: Arc::new(Mutex::new(String::new())),
            attached: Arc::new(Mutex::new(Vec::new())),
            resume: Arc::new(Mutex::new(None)),
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

    /// The mock shows subagents, so the engine's routing is testable
    /// without a CLI.
    fn subagent_caps(&self) -> keel_proto::SubagentCaps {
        keel_proto::SubagentCaps { visibility: keel_proto::SubagentVisibility::Full }
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
        *self.resume.lock().expect("resume") = spec.resume.as_ref().map(|id| id.0.clone());
        *self.attached.lock().expect("attached") = spec.attachments.clone();
        *self.initial.lock().expect("initial") = spec.initial_text;
        self.runs.fetch_add(1, Ordering::Relaxed);
        let observed = Arc::clone(&self.observed);
        let stream: Pin<Box<dyn Stream<Item = Result<AgentEvent, HarnessError>> + Send>> =
            match self.mode {
                Mode::Echo => Box::pin(echo_stream(controls.inbound, observed)),
                Mode::Silent => Box::pin(silent_stream(controls.inbound, observed)),
                Mode::Script => {
                    let steps = self.steps.lock().expect("steps").clone();
                    Box::pin(script_stream(steps, controls.inbound, observed))
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

fn script_stream(
    steps: Vec<Step>,
    mut inbound: UnboundedReceiver<ControlMsg>,
    observed: Arc<Mutex<Vec<ControlMsg>>>,
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
            project: None,
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
            required: true, tool: None, multi: false, other: false,
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
}

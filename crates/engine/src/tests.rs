//! Supervisor, journal, recovery, watchdog, and duplex checks.
//! Temps stay off the user data directory. Nothing here spawns a CLI.

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use crate::repo;
use keel_doc::{ChatRow, NewSession, Outcome, PartView, SpaceRow, ToolRow};
use keel_harness::{ControlMsg, MockHarness, Step};
use keel_proto::{
    AgentEvent, HarnessId, QuestionKind, RpcRequest, SubagentRef, TurnStatus, UserCommand,
    UserInputAnswer, UserInputQuestion,
};
use keel_rpc::{
    BootConfig, EngineHost, Identity, LockResult, Mode, Outbound, Outcome as BootOutcome, boot,
    try_lock,
};
use keel_store::{DocsStore, Layout, session_doc_id};
use serde_json::{Value, json};
use uuid::Uuid;

use super::journal::{Journal, replay};
use super::{
    COMPACTION_BYTES, Clock, Engine, EngineConfig, EngineError, INTERRUPT_DEADLINE_MS,
    MAX_LIVE_RUNS, ManualClock, QueueStatus, RETAIN_DAYS, RETAIN_MS, SNAPSHOT_FLUSH_MS,
    STALL_WATCHDOG_MS, STREAM_COMMIT_MS, WARM_CAP, WARM_DAYS, WARM_MS,
};

const DEVICE: &str = "22222222-2222-2222-2222-222222222222";
const STATUS_IDLE: &str =
    r#"{"id":"c2","result":{"scope":"local","lock":true,"live_runs":0,"last_watchdog":null}}"#;

struct Tmp {
    path: PathBuf,
}

impl Tmp {
    fn new(label: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("keel-engine-{label}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn open_engine(
    tmp: &Tmp,
    clock: &Arc<ManualClock>,
    harness: &Arc<MockHarness>,
    compaction: usize,
) -> Engine {
    let store = DocsStore::open(tmp.path().join("profile")).expect("store");
    Engine::open(EngineConfig {
        store,
        device_id: DEVICE.to_string(),
        scope: "local".to_string(),
        clock: clock.clone(),
        harnesses: vec![harness.clone()],
        compaction_bytes: compaction,
    })
}

fn chat(engine: &Engine) -> String {
    fs::create_dir_all("/tmp/keel-work").expect("work dir");
    engine
        .create_chat("/tmp/keel-work", "mock", "mock")
        .expect("chat")
}

fn send(text: &str) -> UserCommand {
    UserCommand::Send {
        command_id: Uuid::new_v4(),
        message_id: format!("user-{}", Uuid::new_v4()),
        text: text.to_string(),
        attachments: Vec::new(),
    }
}

fn command_of(command: &UserCommand) -> Uuid {
    match command {
        UserCommand::Send { command_id, .. }
        | UserCommand::Steer { command_id, .. }
        | UserCommand::Interrupt { command_id }
        | UserCommand::Answer { command_id, .. } => *command_id,
    }
}

async fn until(engine: &Engine, mut ready: impl FnMut() -> bool) {
    for _ in 0..40 {
        if ready() {
            return;
        }
        engine.drive().await;
    }
    assert!(ready(), "engine did not reach the expected state");
}

fn put_chat(
    store: &mut DocsStore,
    device: &str,
    chat_id: &str,
    host: &str,
    at: i64,
    archived: bool,
    status: &str,
) {
    if store.registry().view().spaces.is_empty() {
        store
            .registry()
            .put_space(&SpaceRow {
                id: "space".to_string(),
                device_id: device.to_string(),
                path: "/tmp/keel-work".to_string(),
                name: "work".to_string(),
                git_detected: false,
                checkout_id: String::new(),
                repository_id: String::new(),
            })
            .expect("space");
    }
    store
        .registry()
        .put_chat(&ChatRow {
            id: chat_id.to_string(),
            device_id: device.to_string(),
            title: chat_id.to_string(),
            archived,
            cwd: "/tmp/keel-work".to_string(),
            branch: String::new(),
            checkout_id: String::new(),
            space_id: "space".to_string(),
            last_seen_at: at,
            last_message_preview: String::new(),
            last_message_at: at,
            harness: "mock".to_string(),
            model: "mock".to_string(),
            reasoning: String::new(),
            permission: String::new(),
            status: status.to_string(),
            usage: Default::default(),
        })
        .expect("chat row");
    store
        .create_session(NewSession {
            chat_id: chat_id.to_string(),
            host_device: host.to_string(),
            space_id: "space".to_string(),
            harness: HarnessId::Mock,
            cwd: "/tmp/keel-work".to_string(),
            model: "mock".to_string(),
            reasoning: String::new(),
            native_session: String::new(),
            created_at: at,
        })
        .expect("session");
}

fn frame_json(raw: &[u8]) -> String {
    let len = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
    String::from_utf8(raw[4..4 + len].to_vec()).expect("utf8")
}

fn rpc(
    guard: &keel_rpc::ServerGuard,
    id: &str,
    method: &str,
    params: Value,
) -> (String, Vec<String>) {
    let body = json!({ "id": id, "method": method, "params": params }).to_string();
    guard.duplex_write(&body).expect("write");
    let mut extra = Vec::new();
    let reply = loop {
        let raw = guard
            .duplex_read_timeout(Duration::from_secs(2))
            .expect("read");
        let text = frame_json(&raw);
        if text.contains(&format!("\"id\":\"{id}\"")) {
            break text;
        }
        extra.push(text);
    };
    while let Ok(raw) = guard.duplex_read_timeout(Duration::from_millis(40)) {
        extra.push(frame_json(&raw));
    }
    (reply, extra)
}

struct Mem(Arc<Mutex<Vec<u8>>>);

impl Write for Mem {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log").extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_log(run: impl FnOnce()) -> String {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::clone(&buf);
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || Mem(Arc::clone(&writer)))
        .finish();
    tracing::subscriber::with_default(subscriber, run);
    String::from_utf8(buf.lock().expect("log").clone()).expect("log utf8")
}

#[test]
fn constants_match_the_spec() {
    assert_eq!(MAX_LIVE_RUNS, 4);
    assert_eq!(COMPACTION_BYTES, 8 * 1024 * 1024);
    assert_eq!(RETAIN_DAYS, 30);
    assert_eq!(RETAIN_MS, 30 * 24 * 60 * 60 * 1000);
    assert_eq!(WARM_DAYS, 14);
    assert_eq!(WARM_MS, 14 * 24 * 60 * 60 * 1000);
    assert_eq!(WARM_CAP, 30);
    assert_eq!(STALL_WATCHDOG_MS, 600_000);
    assert_eq!(INTERRUPT_DEADLINE_MS, 15_000);
    assert_eq!(STREAM_COMMIT_MS, 120);
    assert_eq!(SNAPSHOT_FLUSH_MS, 5_000);
}

#[test]
fn profile_directory_is_owner_only() {
    let tmp = Tmp::new("mode");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let mode = fs::metadata(engine.profile())
        .expect("meta")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
}

#[test]
fn journal_syncs_every_fifty_events_or_one_second() {
    let tmp = Tmp::new("journal");
    let path = tmp.path().join("run.jsonl");
    let mut journal = Journal::create(&path, "cmd-1", 10_000).expect("create");
    assert_eq!(journal.last_seq(), 1);
    assert_eq!(journal.pending(), 0);
    let event = AgentEvent::Text {
        message_id: "m".to_string(),
        part_id: "p".to_string(),
        delta: "x".to_string(),
        subagent: None,
    };
    for _ in 0..49 {
        journal.append_event(10_000, &event).expect("append");
    }
    assert_eq!(journal.pending(), 49);
    journal.append_event(10_000, &event).expect("fiftieth");
    assert_eq!(journal.pending(), 0);
    journal.append_event(10_000, &event).expect("one more");
    assert_eq!(journal.pending(), 1);
    assert_eq!(journal.last_seq(), 52);
    journal.poll_sync(10_999).expect("early");
    assert_eq!(journal.pending(), 1);
    journal.poll_sync(11_000).expect("second");
    assert_eq!(journal.pending(), 0);
    drop(journal);
    let info = replay(&path).expect("replay");
    assert_eq!(info.last_seq, 52);
    assert!(!info.terminal);
    assert_eq!(info.command_id.as_deref(), Some("cmd-1"));
}

#[tokio::test]
async fn fifth_live_run_is_busy_and_is_not_appended() {
    let tmp = Tmp::new("busy");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let mut ids = Vec::new();
    let mut first_command = None;
    for index in 0..4 {
        let id = chat(&engine);
        let command = send(&format!("run-{index}"));
        if index == 0 {
            first_command = Some(command_of(&command));
        }
        assert_eq!(
            engine.queue_command(&id, command).expect("start"),
            QueueStatus::Processed
        );
        ids.push(id);
    }
    assert_eq!(engine.live_runs(), 4);
    let fifth = chat(&engine);
    let extra = send("fifth");
    let err = engine
        .queue_command(&fifth, extra.clone())
        .expect_err("busy");
    assert!(matches!(err, EngineError::Busy));
    assert_eq!(err.to_string(), "too many live runs");
    assert_eq!(engine.live_runs(), 4);
    assert!(engine.command_outcome(&fifth, command_of(&extra)).is_none());
    until(&engine, || harness.runs() == 4).await;
    assert_eq!(harness.runs(), 4);
    let state = engine.lock();
    let live = state.live.get(&ids[0]).expect("live");
    assert_eq!(live.command_id, first_command.expect("command"));
    assert_eq!(live.last_seq, 1);
}

#[tokio::test]
async fn reopen_after_a_live_run_stamps_aborted_and_does_not_rerun() {
    let tmp = Tmp::new("reopen");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    let command = send("keep going");
    let command_id = command_of(&command);
    engine.queue_command(&id, command).expect("start");
    until(&engine, || harness.runs() == 1).await;
    assert_eq!(engine.chat_status(&id).as_deref(), Some("running"));
    let profile = engine.profile().to_path_buf();
    drop(engine);
    let store = DocsStore::open(&profile).expect("reopen store");
    let again = Arc::new(MockHarness::silent());
    let engine = Engine::open(EngineConfig {
        store,
        device_id: DEVICE.to_string(),
        scope: "local".to_string(),
        clock: clock.clone(),
        harnesses: vec![again.clone()],
        compaction_bytes: COMPACTION_BYTES,
    });
    assert_eq!(engine.chat_status(&id).as_deref(), Some("aborted"));
    assert_eq!(again.runs(), 0);
    assert_eq!(
        engine.command_outcome(&id, command_id).as_deref(),
        Some("processed")
    );
    assert!(
        !engine
            .message_text(&id)
            .iter()
            .any(|text| text == "aborted")
    );
}

#[test]
fn crash_after_the_mark_inserts_one_aborted_turn() {
    let tmp = Tmp::new("crash");
    let clock = Arc::new(ManualClock::new(5_000));
    let command_id = Uuid::new_v4();
    let chat_id = "chat-crash";
    let mut store = DocsStore::open(tmp.path().join("profile")).expect("store");
    put_chat(&mut store, DEVICE, chat_id, DEVICE, 5_000, false, "running");
    let doc = store.session(chat_id).expect("session");
    doc.append_command(
        DEVICE,
        DEVICE,
        &UserCommand::Send {
            command_id,
            message_id: "user-1".to_string(),
            text: "go".to_string(),
            attachments: Vec::new(),
        },
        5_000,
    )
    .expect("command");
    doc.set_outcome(DEVICE, command_id, Outcome::Processed)
        .expect("outcome");
    store
        .record_processed(
            &command_id.to_string(),
            &session_doc_id(chat_id),
            "processed",
            5_000,
        )
        .expect("ledger");
    store.commit(5_000).expect("commit");
    let harness = Arc::new(MockHarness::silent());
    let engine = Engine::open(EngineConfig {
        store,
        device_id: DEVICE.to_string(),
        scope: "local".to_string(),
        clock,
        harnesses: vec![harness.clone()],
        compaction_bytes: COMPACTION_BYTES,
    });
    assert_eq!(harness.runs(), 0);
    assert_eq!(engine.chat_status(chat_id).as_deref(), Some("aborted"));
    let abort_id = format!("abort-{command_id}");
    let state = engine.lock();
    let view = state.store.session(chat_id).expect("session").view();
    let abort = view
        .messages
        .iter()
        .find(|message| message.id == abort_id)
        .expect("abort turn");
    assert_eq!(abort.text, "aborted");
    assert_eq!(
        view.messages
            .iter()
            .filter(|message| message.text == "aborted")
            .count(),
        1
    );
}

#[test]
fn non_terminal_journal_does_not_add_a_second_abort_message() {
    let tmp = Tmp::new("partial");
    let clock = Arc::new(ManualClock::new(5_000));
    let command_id = Uuid::new_v4();
    let chat_id = "chat-partial";
    let mut store = DocsStore::open(tmp.path().join("profile")).expect("store");
    put_chat(&mut store, DEVICE, chat_id, DEVICE, 5_000, false, "running");
    let doc = store.session(chat_id).expect("session");
    doc.append_command(
        DEVICE,
        DEVICE,
        &UserCommand::Send {
            command_id,
            message_id: "user-1".to_string(),
            text: "go".to_string(),
            attachments: Vec::new(),
        },
        5_000,
    )
    .expect("command");
    doc.set_outcome(DEVICE, command_id, Outcome::Processed)
        .expect("outcome");
    doc.append_message("asst", "assistant", 5_000)
        .expect("message");
    doc.append_text("asst", "body", "partial").expect("text");
    store.commit(5_000).expect("commit");
    let path = tmp
        .path()
        .join("profile")
        .join("journals")
        .join(chat_id)
        .join("run.jsonl");
    fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    fs::write(
        &path,
        format!(
            "{{\"seq\":1,\"at\":5000,\"kind\":\"start\",\"command_id\":\"{command_id}\"}}\n{{\"seq\":2,\"at\":5001,\"kind\":\"event\",\"event\":{{\"type\":\"text\",\"message_id\":\"asst\",\"part_id\":\"body\",\"delta\":\"partial\"}}}}\n"
        ),
    )
    .expect("journal");
    let harness = Arc::new(MockHarness::silent());
    let engine = Engine::open(EngineConfig {
        store,
        device_id: DEVICE.to_string(),
        scope: "local".to_string(),
        clock,
        harnesses: vec![harness.clone()],
        compaction_bytes: COMPACTION_BYTES,
    });
    assert_eq!(harness.runs(), 0);
    assert_eq!(engine.chat_status(chat_id).as_deref(), Some("aborted"));
    let texts = engine.message_text(chat_id);
    assert!(texts.iter().any(|text| text == "partial"));
    assert!(!texts.iter().any(|text| text == "aborted"));
    // Recovery seals the journal so the next boot does not stamp it again.
    let info = replay(&path).expect("replay");
    assert_eq!(info.last_seq, 3);
    assert!(info.terminal);
}

#[tokio::test]
async fn watchdog_interrupts_once_and_logs_the_stall() {
    let tmp = Tmp::new("watchdog");
    let clock = Arc::new(ManualClock::new(1_000_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("silent")).expect("start");
    until(&engine, || harness.runs() == 1).await;
    clock.set(1_000_000 + STALL_WATCHDOG_MS - 1);
    engine.tick();
    assert!(engine.last_watchdog().is_none());
    assert_eq!(engine.chat_status(&id).as_deref(), Some("running"));
    let log = capture_log(|| {
        clock.set(1_000_000 + STALL_WATCHDOG_MS);
        engine.tick();
        engine.tick();
    });
    assert!(log.contains("keel_engine::watchdog"), "{log}");
    assert!(log.contains(&id), "{log}");
    assert!(
        log.contains("last_seq=1") || log.contains("last_seq: 1"),
        "{log}"
    );
    assert!(log.contains("stall"), "{log}");
    until(&engine, || {
        harness
            .observed()
            .iter()
            .filter(|msg| matches!(msg, ControlMsg::Interrupt))
            .count()
            == 1
    })
    .await;
    assert_eq!(engine.chat_status(&id).as_deref(), Some("aborted"));
    assert_eq!(engine.last_watchdog().as_deref(), Some(id.as_str()));
    assert_eq!(engine.live_runs(), 1);
    clock.set(1_000_000 + STALL_WATCHDOG_MS + INTERRUPT_DEADLINE_MS - 1);
    engine.tick();
    assert_eq!(engine.live_runs(), 1);
    clock.set(1_000_000 + STALL_WATCHDOG_MS + INTERRUPT_DEADLINE_MS);
    engine.tick();
    assert_eq!(engine.live_runs(), 0);
    assert_eq!(
        harness
            .observed()
            .iter()
            .filter(|msg| matches!(msg, ControlMsg::Interrupt))
            .count(),
        1
    );
    assert_eq!(engine.chat_status(&id).as_deref(), Some("aborted"));
}

#[tokio::test]
async fn interrupt_deadline_drops_the_stream_once() {
    let tmp = Tmp::new("deadline");
    let clock = Arc::new(ManualClock::new(8_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("first")).expect("start");
    until(&engine, || harness.runs() == 1).await;
    let later = send("later");
    let later_id = command_of(&later);
    assert_eq!(
        engine.queue_command(&id, later).expect("queue"),
        QueueStatus::Queued
    );
    assert!(engine.command_outcome(&id, later_id).is_none());
    let interrupt = UserCommand::Interrupt {
        command_id: Uuid::new_v4(),
    };
    assert_eq!(
        engine.queue_command(&id, interrupt).expect("interrupt"),
        QueueStatus::Processed
    );
    until(&engine, || harness.observed().len() == 1).await;
    assert_eq!(harness.observed(), vec![ControlMsg::Interrupt]);
    assert_eq!(engine.live_runs(), 1);
    assert_eq!(engine.chat_status(&id).as_deref(), Some("running"));
    clock.set(8_000 + INTERRUPT_DEADLINE_MS - 1);
    engine.tick();
    assert_eq!(engine.live_runs(), 1);
    clock.set(8_000 + INTERRUPT_DEADLINE_MS);
    engine.tick();
    until(&engine, || harness.runs() == 2).await;
    assert_eq!(harness.runs(), 2);
    assert_eq!(harness.initial_text(), "later");
    assert_eq!(
        harness
            .observed()
            .iter()
            .filter(|msg| matches!(msg, ControlMsg::Interrupt))
            .count(),
        1
    );
    assert_eq!(
        engine.command_outcome(&id, later_id).as_deref(),
        Some("processed")
    );
    assert_eq!(engine.live_runs(), 1);
}

#[tokio::test]
async fn idle_interrupt_and_answer_are_rejected() {
    let tmp = Tmp::new("idle");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    assert_eq!(
        engine
            .queue_command(
                &id,
                UserCommand::Interrupt {
                    command_id: Uuid::new_v4(),
                },
            )
            .expect("interrupt"),
        QueueStatus::Rejected
    );
    assert_eq!(
        engine
            .queue_command(
                &id,
                UserCommand::Answer {
                    command_id: Uuid::new_v4(),
                    question_id: "q".to_string(),
                    answers: Vec::new(),
                },
            )
            .expect("answer"),
        QueueStatus::Rejected
    );
    engine.drive().await;
    assert_eq!(harness.runs(), 0);
    assert_eq!(engine.chat_status(&id).as_deref(), Some("idle"));
}

#[tokio::test]
async fn idle_steer_becomes_the_next_prompt() {
    let tmp = Tmp::new("steer-idle");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    assert_eq!(
        engine
            .queue_command(
                &id,
                UserCommand::Steer {
                    command_id: Uuid::new_v4(),
                    message_id: "steer-1".to_string(),
                    text: "promoted".to_string(),
                },
            )
            .expect("steer"),
        QueueStatus::Processed
    );
    until(&engine, || harness.runs() == 1).await;
    assert_eq!(harness.initial_text(), "promoted");
    assert!(
        engine
            .message_text(&id)
            .iter()
            .any(|text| text == "promoted")
    );
}

/// Two chats stream at once: their output (and heartbeats through a long
/// quiet tool call) must not reorder the sidebar. A prompt lifts its chat.
#[tokio::test]
async fn streaming_never_reorders_and_a_prompt_moves_to_top() {
    let tmp = Tmp::new("attention-order");
    let clock = Arc::new(ManualClock::new(1_000_000));
    let text = |delta: &str| {
        Step::Emit(AgentEvent::Text {
            message_id: "m".to_string(),
            part_id: "p".to_string(),
            delta: delta.to_string(),
            subagent: None,
        })
    };
    let harness = Arc::new(MockHarness::script(vec![
        text("a"),
        Step::Wait,
        text("b"),
        Step::Wait,
    ]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let older = chat(&engine);
    engine
        .queue_command(&older, send("first"))
        .expect("start older");
    until(&engine, || harness.runs() == 1).await;
    clock.set(1_000_010);
    let newer = chat(&engine);
    engine
        .queue_command(&newer, send("second"))
        .expect("start newer");
    until(&engine, || harness.runs() == 2).await;
    let expected = vec![newer.clone(), older.clone()];
    assert_eq!(engine.attention_order(clock.now_ms()), expected);

    // Both streams commit their text, several times over.
    for step in 1..=5 {
        clock.set(1_000_010 + step * STREAM_COMMIT_MS);
        engine.tick();
        engine.drive().await;
        assert_eq!(
            engine.attention_order(clock.now_ms()),
            expected,
            "commit {step}"
        );
    }
    assert!(engine.message_text(&older).iter().any(|t| t.contains('a')));

    // A long quiet stretch: heartbeats keep both running, in place.
    for step in 1..=6 {
        clock.set(1_000_010 + step * 15_000);
        engine.tick();
        assert_eq!(
            engine.attention_order(clock.now_ms()),
            expected,
            "quiet {step}"
        );
    }
    assert_eq!(engine.chat_status(&older).as_deref(), Some("running"));

    // The user prompts the older chat: it moves to the top.
    clock.set(1_200_000);
    assert_eq!(
        engine.queue_command(&older, steer("go on")).expect("steer"),
        QueueStatus::Processed
    );
    let lifted = vec![older.clone(), newer.clone()];
    assert_eq!(engine.attention_order(clock.now_ms()), lifted);
    // ...and its new output still doesn't move anything.
    until(&engine, || harness.observed().len() == 1).await;
    for _ in 0..8 {
        engine.drive().await;
    }
    clock.set(1_200_000 + STREAM_COMMIT_MS);
    engine.tick();
    assert!(
        engine.message_text(&older).iter().any(|t| t.contains('b')),
        "{:?}",
        engine.message_text(&older)
    );
    assert_eq!(engine.attention_order(clock.now_ms()), lifted);
}

#[tokio::test]
async fn steer_is_observed_before_the_turn_ends() {
    let tmp = Tmp::new("steer-live");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::script(vec![
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
    ]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("go")).expect("start");
    until(&engine, || harness.runs() == 1).await;
    assert_eq!(
        engine
            .queue_command(
                &id,
                UserCommand::Steer {
                    command_id: Uuid::new_v4(),
                    message_id: "steer-2".to_string(),
                    text: "turn left".to_string(),
                },
            )
            .expect("steer"),
        QueueStatus::Processed
    );
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("idle")
    })
    .await;
    assert_eq!(harness.runs(), 1);
    assert_eq!(
        harness.observed(),
        vec![ControlMsg::Steer {
            text: "turn left".to_string(),
        }]
    );
}

#[tokio::test]
async fn question_then_answer_reaches_the_mock() {
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
    let tmp = Tmp::new("answer");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::script(vec![
        Step::Emit(AgentEvent::Question {
            id: "ask".to_string(),
            questions: vec![question],
            subagent: None,
        }),
        Step::Wait,
        Step::Emit(AgentEvent::Done {
            status: TurnStatus::Completed {},
        }),
    ]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("go")).expect("start");
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("awaiting_input")
    })
    .await;
    let answers = vec![UserInputAnswer {
        question_id: "q1".to_string(),
        kind: QuestionKind::Confirm,
        value: "yes".to_string(),
    }];
    assert_eq!(
        engine
            .queue_command(
                &id,
                UserCommand::Answer {
                    command_id: Uuid::new_v4(),
                    question_id: "ask".to_string(),
                    answers: answers.clone(),
                },
            )
            .expect("answer"),
        QueueStatus::Processed
    );
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("idle")
    })
    .await;
    assert_eq!(harness.observed(), vec![ControlMsg::Answer { answers }]);
    assert_eq!(
        receipt_of(&engine, &id, "q1").map(|r| r.decision),
        Some(keel_proto::Decision::Answered)
    );
}

/// The receipt a settled question left in the chat's doc.
fn receipt_of(engine: &Engine, chat_id: &str, question_id: &str) -> Option<keel_proto::Receipt> {
    let mut state = engine.lock();
    let view = state.store.session(chat_id)?.view();
    view.messages.iter().find_map(|message| {
        message.parts.iter().find_map(|part| match part {
            PartView::Tool(row)
                if row.tool_id == format!("receipt-{question_id}")
                    && row.name == keel_proto::RECEIPT_TOOL =>
            {
                keel_proto::Receipt::from_summary(&row.summary)
            }
            _ => None,
        })
    })
}

#[tokio::test]
async fn permission_answer_and_stop_leave_receipts() {
    let mut ask = UserInputQuestion::new(
        "p1",
        QuestionKind::Select,
        "Allow navigate?",
        vec![
            keel_proto::QuestionOption::new("always", "Always")
                .with_role(keel_proto::OptionRole::AllowAlways),
            keel_proto::QuestionOption::new("once", "Allow")
                .with_role(keel_proto::OptionRole::AllowOnce),
        ],
        true,
    );
    ask.tool = Some(keel_proto::ToolAsk {
        name: "navigate".into(),
        source: Some("keel-browser".into()),
        description: None,
        args: vec![("url".into(), "https://example.com".into())],
    });
    let mut second = ask.clone();
    second.id = "p2".into();
    let tmp = Tmp::new("receipts");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::script(vec![
        Step::Emit(AgentEvent::Question {
            id: "a".into(),
            questions: vec![ask],
            subagent: None,
        }),
        Step::Wait,
        Step::Emit(AgentEvent::Question {
            id: "b".into(),
            questions: vec![second],
            subagent: None,
        }),
        Step::Wait,
        Step::Emit(AgentEvent::Done {
            status: TurnStatus::Interrupted {},
        }),
    ]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("go")).expect("start");
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("awaiting_input")
    })
    .await;
    let answers = vec![UserInputAnswer {
        question_id: "p1".into(),
        kind: QuestionKind::Select,
        value: "once".into(),
    }];
    engine
        .queue_command(
            &id,
            UserCommand::Answer {
                command_id: Uuid::new_v4(),
                question_id: "a".into(),
                answers,
            },
        )
        .expect("answer");
    let first = receipt_of(&engine, &id, "p1").expect("receipt");
    assert_eq!(first.decision, keel_proto::Decision::Allowed);
    assert_eq!(
        first.tool.as_ref().map(|t| t.name.as_str()),
        Some("navigate")
    );
    until(&engine, || {
        receipt_of(&engine, &id, "p2").is_none()
            && engine.chat_status(&id).as_deref() == Some("awaiting_input")
    })
    .await;
    engine
        .queue_command(
            &id,
            UserCommand::Interrupt {
                command_id: Uuid::new_v4(),
            },
        )
        .expect("stop");
    assert_eq!(
        receipt_of(&engine, &id, "p2").map(|r| r.decision),
        Some(keel_proto::Decision::Stopped)
    );
}

#[tokio::test]
async fn text_folds_after_one_hundred_twenty_milliseconds() {
    let tmp = Tmp::new("fold");
    let clock = Arc::new(ManualClock::new(1_000_000));
    let harness = Arc::new(MockHarness::script(vec![
        Step::Emit(AgentEvent::Text {
            message_id: "assistant-1".to_string(),
            part_id: "p".to_string(),
            delta: "zeta".to_string(),
            subagent: None,
        }),
        Step::Wait,
    ]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("prompt")).expect("start");
    until(&engine, || engine.message_text(&id).len() >= 2).await;
    assert!(
        !engine
            .message_text(&id)
            .iter()
            .any(|text| text.contains("zeta")),
        "{:?}",
        engine.message_text(&id)
    );
    clock.set(1_000_000 + STREAM_COMMIT_MS - 1);
    engine.tick();
    assert!(
        !engine
            .message_text(&id)
            .iter()
            .any(|text| text.contains("zeta"))
    );
    clock.set(1_000_000 + STREAM_COMMIT_MS);
    engine.tick();
    assert!(
        engine
            .message_text(&id)
            .iter()
            .any(|text| text.contains("zeta"))
    );
    let root = engine.profile().join("journals").join(&id);
    let entry = fs::read_dir(&root)
        .expect("journals")
        .next()
        .expect("file")
        .expect("entry");
    let recorded = fs::read_to_string(entry.path()).expect("jsonl");
    assert!(recorded.contains("\"kind\":\"start\""));
    assert!(recorded.contains("zeta"));
}

#[tokio::test]
async fn snapshot_flushes_on_seal_and_on_the_five_second_interval() {
    let tmp = Tmp::new("flush");
    let clock = Arc::new(ManualClock::new(2_000_000));
    let harness = Arc::new(MockHarness::echo());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    let before = engine.flush_count();
    engine.queue_command(&id, send("seal")).expect("start");
    let queued = engine.flush_count();
    assert!(queued > before);
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("idle")
    })
    .await;
    assert!(engine.flush_count() > queued);
    assert!(
        engine
            .message_text(&id)
            .iter()
            .any(|text| text.contains("ok"))
    );

    let clock = Arc::new(ManualClock::new(3_000_000));
    let harness = Arc::new(MockHarness::script(vec![
        Step::Emit(AgentEvent::Text {
            message_id: "assistant-2".to_string(),
            part_id: "p".to_string(),
            delta: "interval".to_string(),
            subagent: None,
        }),
        Step::Wait,
    ]));
    let tmp = Tmp::new("interval");
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("prompt")).expect("start");
    until(&engine, || engine.message_text(&id).len() >= 2).await;
    let mid = engine.flush_count();
    clock.set(3_000_000 + SNAPSHOT_FLUSH_MS - 1);
    engine.tick();
    assert_eq!(engine.flush_count(), mid);
    clock.set(3_000_000 + SNAPSHOT_FLUSH_MS);
    engine.tick();
    assert_eq!(engine.flush_count(), mid + 1);
}

#[tokio::test]
async fn compaction_shrinks_the_snapshot_and_keeps_the_text() {
    let tmp = Tmp::new("compact");
    let clock = Arc::new(ManualClock::new(9_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, 1024);
    let id = chat(&engine);
    let before = {
        let state = engine.lock();
        let doc = state.store.session(&id).expect("session");
        doc.append_message("bulk", "user", 9_000).expect("message");
        for _ in 0..3_000 {
            doc.append_text("bulk", "body", "q").expect("text");
        }
        doc.export_snapshot().expect("snapshot").len()
    };
    assert!(before > 1024, "fixture snapshot is {before} bytes");
    engine.maintenance();
    let after = engine.snapshot_len(&id);
    assert!(after < before, "snapshot {before} -> {after}");
    let text = engine
        .message_text(&id)
        .into_iter()
        .find(|text| text.contains('q'))
        .expect("text");
    assert_eq!(text.len(), 3_000);
}

#[test]
fn retention_collapses_old_turns_and_deletes_old_journals() {
    let tmp = Tmp::new("retain");
    let now = RETAIN_MS + 86_400_000;
    let clock = Arc::new(ManualClock::new(now));
    let mut store = DocsStore::open(tmp.path().join("profile")).expect("store");
    put_chat(&mut store, DEVICE, "kept", DEVICE, now, false, "idle");
    let doc = store.session("kept").expect("session");
    doc.append_message("old-user", "user", 1_000).expect("user");
    doc.append_text("old-user", "body", "ancient user")
        .expect("text");
    doc.append_message("old-assistant", "assistant", 2_000)
        .expect("assistant");
    doc.append_text("old-assistant", "body", "ancient assistant")
        .expect("text");
    doc.append_message("old-tool", "tool", 3_000).expect("tool");
    doc.upsert_tool(
        "old-tool",
        &ToolRow {
            tool_id: "t1".to_string(),
            name: "read".to_string(),
            state: "done".to_string(),
            summary: "secret-summary".to_string(),
            input_bytes: 4,
            output_bytes: 8,
            detail: String::new(),
            images: Vec::new(),
            ..ToolRow::default()
        },
    )
    .expect("tool row");
    doc.append_message("fresh", "user", now).expect("fresh");
    doc.append_text("fresh", "body", "still here")
        .expect("text");
    store.commit(now).expect("commit");
    let journals = tmp.path().join("profile").join("journals").join("kept");
    fs::create_dir_all(&journals).expect("journals");
    let old = journals.join("old.jsonl");
    let fresh_file = journals.join("fresh.jsonl");
    fs::write(&old, "{}\n").expect("old journal");
    File::options()
        .write(true)
        .open(&old)
        .expect("old file")
        .set_modified(UNIX_EPOCH)
        .expect("mtime");
    fs::write(&fresh_file, "{}\n").expect("fresh journal");
    let harness = Arc::new(MockHarness::silent());
    let engine = Engine::open(EngineConfig {
        store,
        device_id: DEVICE.to_string(),
        scope: "local".to_string(),
        clock,
        harnesses: vec![harness],
        compaction_bytes: COMPACTION_BYTES,
    });
    let texts = engine.message_text("kept");
    assert!(
        texts.iter().any(|text| text == "earlier turns omitted"),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|text| text.contains("still here")),
        "{texts:?}"
    );
    assert!(
        !texts.iter().any(|text| text.contains("ancient")),
        "{texts:?}"
    );
    let state = engine.lock();
    let view = state.store.session("kept").expect("session").view();
    let tool = view.messages.iter().find_map(|message| {
        message.parts.iter().find_map(|part| match part {
            PartView::Tool(row) if row.tool_id == "t1" => Some(row.summary.clone()),
            _ => None,
        })
    });
    assert_eq!(tool.as_deref(), Some(""));
    drop(state);
    assert!(!old.exists());
    assert!(fresh_file.exists());
}

#[test]
fn warm_open_caps_at_thirty_and_subscribe_opens_the_cold_chat() {
    let tmp = Tmp::new("warm");
    let now = 2_000_000_000_i64;
    let clock = Arc::new(ManualClock::new(now));
    let mut store = DocsStore::open(tmp.path().join("profile")).expect("store");
    for index in 0..31 {
        put_chat(
            &mut store,
            DEVICE,
            &format!("c{index:02}"),
            DEVICE,
            now - index,
            false,
            "idle",
        );
    }
    put_chat(
        &mut store,
        DEVICE,
        "old",
        DEVICE,
        now - WARM_MS - 1,
        false,
        "idle",
    );
    put_chat(&mut store, DEVICE, "arch", DEVICE, now, true, "idle");
    store.commit(now).expect("commit");
    drop(store);
    // WARM_OPEN: a lazy store imports only the warm sessions at boot.
    let store = DocsStore::open_lazy(tmp.path().join("profile")).expect("lazy store");
    let harness = Arc::new(MockHarness::silent());
    let engine = Engine::open(EngineConfig {
        store,
        device_id: DEVICE.to_string(),
        scope: "local".to_string(),
        clock,
        harnesses: vec![harness],
        compaction_bytes: COMPACTION_BYTES,
    });
    assert_eq!(engine.warm_chat_ids().len(), WARM_CAP);
    assert!(engine.is_warm("c00"));
    assert!(!engine.is_warm("c30"));
    assert!(!engine.is_warm("old"));
    assert!(!engine.is_warm("arch"));
    let loaded = |id: &str| engine.lock().store.session(id).is_some();
    assert!(loaded("c00"));
    assert!(!loaded("c30"));
    assert!(!loaded("old"));
    let (outbound, _rx) = Outbound::pair();
    let body = engine
        .handle(
            &RpcRequest {
                id: "sub".to_string(),
                method: "subscribe_chat".to_string(),
                params: json!({ "chat_id": "c30" }),
            },
            &outbound,
        )
        .expect("subscribe");
    assert!(body.contains("\"stream\":\"s"), "{body}");
    assert!(engine.is_warm("c30"));
    assert!(loaded("c30"));
    assert_eq!(engine.warm_chat_ids().len(), WARM_CAP + 1);
}

#[test]
fn other_device_queues_without_a_run() {
    let tmp = Tmp::new("guest");
    let clock = Arc::new(ManualClock::new(5_000));
    let mut store = DocsStore::open(tmp.path().join("profile")).expect("store");
    put_chat(
        &mut store,
        "other-device",
        "guest",
        "other-device",
        5_000,
        false,
        "idle",
    );
    store.commit(5_000).expect("commit");
    let harness = Arc::new(MockHarness::silent());
    let engine = Engine::open(EngineConfig {
        store,
        device_id: DEVICE.to_string(),
        scope: "local".to_string(),
        clock,
        harnesses: vec![harness.clone()],
        compaction_bytes: COMPACTION_BYTES,
    });
    let command = send("from elsewhere");
    let command_id = command_of(&command);
    assert_eq!(
        engine.queue_command("guest", command).expect("queue"),
        QueueStatus::Queued
    );
    assert_eq!(harness.runs(), 0);
    assert!(engine.command_outcome("guest", command_id).is_none());
}

#[test]
fn lock_loser_does_not_open_sqlite() {
    let tmp = Tmp::new("lock");
    let held = match try_lock(&tmp.path().join("engine.lock")).expect("lock") {
        LockResult::Acquired(lock) => lock,
        LockResult::Held => panic!("lock already held"),
    };
    let opened = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&opened);
    let root = tmp.path().to_path_buf();
    let config = BootConfig {
        data_dir: tmp.path().to_path_buf(),
        port: free_port(),
        mode: Mode::Headless,
        hello_budget: Duration::from_millis(30),
        retry_for: Duration::from_millis(0),
    };
    let outcome = boot(&config, move || {
        flag.store(true, Ordering::SeqCst);
        let layout = Layout::open(&root).map_err(|err| err.to_string())?;
        let profile = layout.profile_dir();
        let _store = DocsStore::open(profile).map_err(|err| err.to_string())?;
        Ok(Identity {
            device_id: layout.device_id().to_string(),
            scope: "local".to_string(),
        })
    })
    .expect("boot");
    assert!(matches!(outcome, BootOutcome::EngineLockHeld));
    assert!(!opened.load(Ordering::SeqCst));
    assert!(!tmp.path().join("profiles/local/store.sqlite").exists());
    drop(held);
}

#[test]
fn subscribe_and_live_events_cross_the_duplex() {
    let tmp = Tmp::new("duplex");
    let store_slot: Arc<Mutex<Option<DocsStore>>> = Arc::new(Mutex::new(None));
    let ident_slot: Arc<Mutex<Option<Identity>>> = Arc::new(Mutex::new(None));
    let store_for_open = Arc::clone(&store_slot);
    let ident_for_open = Arc::clone(&ident_slot);
    let root = tmp.path().to_path_buf();
    let config = BootConfig {
        data_dir: tmp.path().to_path_buf(),
        port: free_port(),
        mode: Mode::Headless,
        hello_budget: Duration::from_millis(30),
        retry_for: Duration::from_millis(0),
    };
    let outcome = boot(&config, move || {
        let layout = Layout::open(&root).map_err(|err| err.to_string())?;
        let profile = layout.profile_dir();
        let store = DocsStore::open(profile).map_err(|err| err.to_string())?;
        let identity = Identity {
            device_id: layout.device_id().to_string(),
            scope: "local".to_string(),
        };
        *store_for_open.lock().expect("store") = Some(store);
        *ident_for_open.lock().expect("identity") = Some(identity.clone());
        Ok(identity)
    })
    .expect("boot");
    let BootOutcome::Engine(guard) = outcome else {
        panic!("expected the engine");
    };
    let store = store_slot.lock().expect("store").take().expect("opened");
    let identity = ident_slot
        .lock()
        .expect("identity")
        .take()
        .expect("identity");
    let harness = Arc::new(MockHarness::script(vec![
        Step::Emit(AgentEvent::Text {
            message_id: "live-1".to_string(),
            part_id: "p".to_string(),
            delta: "streamed".to_string(),
            subagent: None,
        }),
        Step::Wait,
    ]));
    let engine = Engine::open(EngineConfig {
        store,
        device_id: identity.device_id.clone(),
        scope: identity.scope,
        clock: Arc::new(super::SystemClock),
        harnesses: vec![harness],
        compaction_bytes: COMPACTION_BYTES,
    });
    guard.install_host(Arc::new(engine));
    fs::create_dir_all("/tmp/keel-work").expect("work dir");
    guard
        .duplex_write(r#"{"id":"c1","method":"hello","params":{"token":"","proto":1}}"#)
        .expect("hello");
    let hello = frame_json(&guard.duplex_read().expect("hello reply"));
    assert!(hello.contains("\"proto\":1"), "{hello}");
    assert!(hello.contains("\"scope\":\"local\""), "{hello}");
    guard
        .duplex_write(r#"{"id":"c2","method":"engine_status","params":{}}"#)
        .expect("status");
    let status = frame_json(&guard.duplex_read().expect("status reply"));
    assert_eq!(status, STATUS_IDLE);
    let (created, _) = rpc(
        &guard,
        "make",
        "create_chat",
        json!({
            "cwd": "/tmp/keel-work",
            "harness": "mock",
            "model": "mock",
            "title": "Chat"
        }),
    );
    let created: Value = serde_json::from_str(&created).expect("create json");
    let chat_id = created["result"]["chat_id"]
        .as_str()
        .expect("chat")
        .to_string();
    let space_id = created["result"]["space_id"]
        .as_str()
        .expect("space")
        .to_string();
    assert_eq!(chat_id.len(), 36);
    assert_eq!(space_id.len(), 36);
    let (listed, _) = rpc(&guard, "h1", "list_harnesses", json!({}));
    assert_eq!(
        listed,
        r#"{"id":"h1","result":{"harnesses":[{"id":"mock","display_name":"Mock","installed":true,"version":"0","permission_modes":["ask"]}]}}"#
    );
    let (registry, registry_events) = rpc(&guard, "r1", "subscribe_registry", json!({}));
    assert_eq!(registry, r#"{"id":"r1","result":{"stream":"s1"}}"#);
    let registry_blob = registry_events.join("\n");
    assert!(registry_blob.contains("mirror_diff"), "{registry_blob}");
    assert!(
        registry_blob.contains(&format!("chats.{chat_id}")),
        "{registry_blob}"
    );
    let (subscribed, _) = rpc(
        &guard,
        "s1",
        "subscribe_chat",
        json!({ "chat_id": chat_id }),
    );
    assert_eq!(subscribed, r#"{"id":"s1","result":{"stream":"s2"}}"#);
    let command = UserCommand::Send {
        command_id: Uuid::new_v4(),
        message_id: "user-live".to_string(),
        text: "hello".to_string(),
        attachments: Vec::new(),
    };
    let (queued, mut queued_events) = rpc(
        &guard,
        "q1",
        "queue_command",
        json!({
            "chat_id": chat_id,
            "command": serde_json::to_value(&command).expect("command")
        }),
    );
    assert_eq!(queued, r#"{"id":"q1","result":{"status":"processed"}}"#);
    let (running, _) = rpc(&guard, "c9", "engine_status", json!({}));
    assert_eq!(
        running,
        r#"{"id":"c9","result":{"scope":"local","lock":true,"live_runs":1,"last_watchdog":null}}"#
    );
    let mut saw_live = queued_events
        .iter()
        .any(|frame| frame.contains("\"type\":\"live\""));
    if !saw_live {
        for _ in 0..20 {
            if let Ok(raw) = guard.duplex_read_timeout(Duration::from_millis(100)) {
                let text = frame_json(&raw);
                saw_live = text.contains("\"type\":\"live\"") && text.contains("streamed");
                queued_events.push(text);
                if saw_live {
                    break;
                }
            }
        }
    }
    assert!(saw_live, "{queued_events:?}");
    let (unknown, _) = rpc(&guard, "u1", "install", json!({}));
    assert_eq!(
        unknown,
        r#"{"id":"u1","error":{"code":"unknown_method","message":"no such method"}}"#
    );
    let (still, _) = rpc(&guard, "c2", "engine_status", json!({}));
    assert!(still.contains("\"live_runs\":1"), "{still}");

    // Test set_archived
    let (arch, _) = rpc(
        &guard,
        "a1",
        "set_archived",
        json!({ "chat_id": chat_id, "archived": true }),
    );
    assert_eq!(arch, r#"{"id":"a1","result":{}}"#);

    // Test rename_chat
    let (rc, _) = rpc(
        &guard,
        "rc1",
        "rename_chat",
        json!({ "chat_id": chat_id, "title": "Renamed" }),
    );
    assert_eq!(rc, r#"{"id":"rc1","result":{}}"#);

    // Test rename_space
    let (rs, _) = rpc(
        &guard,
        "rs1",
        "rename_space",
        json!({ "space_id": space_id, "name": "Renamed Space" }),
    );
    assert_eq!(rs, r#"{"id":"rs1","result":{}}"#);

    // Test delete_chats: an unknown id is refused, a real one tombstoned.
    let (bad, _) = rpc(
        &guard,
        "dc0",
        "delete_chats",
        json!({ "chat_ids": ["nope"] }),
    );
    assert!(bad.contains("bad_params"), "{bad}");
    let (dc, _) = rpc(
        &guard,
        "dc1",
        "delete_chats",
        json!({ "chat_ids": [chat_id] }),
    );
    assert_eq!(dc, r#"{"id":"dc1","result":{}}"#);

    // Test delete_space
    let (ds, _) = rpc(
        &guard,
        "ds1",
        "delete_space",
        json!({ "space_id": space_id }),
    );
    assert_eq!(ds, r#"{"id":"ds1","result":{}}"#);
}

#[tokio::test]
async fn native_session_file_is_the_next_resume() {
    let tmp = Tmp::new("resume");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::echo());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let chat_id = chat(&engine);
    engine.testing_put_native(&chat_id, "sess-1", "/tmp/pi-session.jsonl");
    assert_eq!(
        engine.testing_native_meta(&chat_id),
        "/tmp/pi-session.jsonl"
    );
    engine.queue_command(&chat_id, send("resume")).unwrap();
    until(&engine, || harness.runs() >= 1).await;
    assert_eq!(harness.resumed().as_deref(), Some("/tmp/pi-session.jsonl"));
}

#[tokio::test]
async fn worktree_toggle_default_off_and_on_with_removal() {
    let tmp = Tmp::new("worktree");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::echo());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);

    let repo_dir = tmp.path().join("my-repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _ = repo::run_git_cmd(&repo_dir, &["init"], repo::GIT_TIMEOUT).await;
    let _ = repo::run_git_cmd(
        &repo_dir,
        &["config", "user.name", "Test"],
        repo::GIT_TIMEOUT,
    )
    .await;
    let _ = repo::run_git_cmd(
        &repo_dir,
        &["config", "user.email", "test@test.com"],
        repo::GIT_TIMEOUT,
    )
    .await;
    std::fs::write(repo_dir.join("code.txt"), b"initial code\n").unwrap();
    let _ = repo::run_git_cmd(&repo_dir, &["add", "code.txt"], repo::GIT_TIMEOUT).await;
    let _ = repo::run_git_cmd(&repo_dir, &["commit", "-m", "init"], repo::GIT_TIMEOUT).await;

    let repo_dir = repo_dir.canonicalize().unwrap();
    let repo_str = repo_dir.to_str().unwrap();

    // 1. Default off: use_worktree = false
    let chat_normal = engine
        .create_chat_full(repo_str, "mock", "mock", false, None)
        .expect("normal chat");
    {
        let state = engine.lock();
        let chat = state
            .store
            .registry()
            .view()
            .chats
            .into_iter()
            .find(|c| c.id == chat_normal)
            .unwrap();
        assert_eq!(chat.cwd, repo_str);
    }

    // 2. Worktree on: use_worktree = true
    let chat_wt = engine
        .create_chat_full(repo_str, "mock", "mock", true, Some("feat-branch"))
        .expect("worktree chat");
    let wt_cwd = {
        let state = engine.lock();
        let chat = state
            .store
            .registry()
            .view()
            .chats
            .into_iter()
            .find(|c| c.id == chat_wt)
            .unwrap();
        assert_ne!(chat.cwd, repo_str);
        assert!(chat.cwd.contains("worktrees"));
        chat.cwd
    };
    let wt_path = std::path::PathBuf::from(&wt_cwd);
    assert!(wt_path.exists());
    assert!(wt_path.join("code.txt").exists());

    // 3. Removal via rpc set_archived
    let rpc_res = engine.rpc_set_archived(
        "a1",
        &serde_json::json!({
            "chat_id": chat_wt,
            "archived": true,
        }),
    );
    assert!(rpc_res.contains("\"result\":{}"));
    assert!(!wt_path.exists());
}

#[tokio::test]
async fn workspace_file_rpc_via_host() {
    let tmp = Tmp::new("keel-workspace-rpc");
    let clock = Arc::new(ManualClock::new(100));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let (outbound, _rx) = Outbound::pair();

    let space_dir = tmp.path().join("workspace");
    fs::create_dir_all(&space_dir).unwrap();
    let space_dir_str = space_dir.to_str().unwrap().to_string();

    let my_space_id = "space-local-1";
    {
        let state = engine.lock();
        state
            .store
            .registry()
            .put_space(&SpaceRow {
                id: my_space_id.to_string(),
                device_id: DEVICE.to_string(),
                path: space_dir_str.clone(),
                name: "test-space".to_string(),
                git_detected: false,
                checkout_id: "".to_string(),
                repository_id: "".to_string(),
            })
            .unwrap();

        state
            .store
            .registry()
            .put_space(&SpaceRow {
                id: "space-remote-1".to_string(),
                device_id: "other-device".to_string(),
                path: space_dir_str,
                name: "remote-space".to_string(),
                git_detected: false,
                checkout_id: "".to_string(),
                repository_id: "".to_string(),
            })
            .unwrap();
    }

    // 1. Write file to space owned by another device -> NotHost
    let req = RpcRequest {
        id: "w0".to_string(),
        method: "write_workspace_file".to_string(),
        params: json!({
            "space_id": "space-remote-1",
            "rel_path": "hello.txt",
            "content": "remote content",
        }),
    };
    let resp: Value = serde_json::from_str(&engine.handle(&req, &outbound).unwrap()).unwrap();
    assert_eq!(resp["error"]["code"], "not_host");

    // 2. Write file to unknown space -> bad_params (space not found)
    let req = RpcRequest {
        id: "w1".to_string(),
        method: "write_workspace_file".to_string(),
        params: json!({
            "space_id": "space-nonexistent",
            "rel_path": "hello.txt",
            "content": "content",
        }),
    };
    let resp: Value = serde_json::from_str(&engine.handle(&req, &outbound).unwrap()).unwrap();
    assert_eq!(resp["error"]["code"], "bad_params");

    // 3. Write file successfully to local space
    fs::create_dir_all(space_dir.join("sub")).unwrap();
    let req = RpcRequest {
        id: "w2".to_string(),
        method: "write_workspace_file".to_string(),
        params: json!({
            "space_id": my_space_id,
            "rel_path": "sub/hello.txt",
            "content": "hello world",
        }),
    };
    let resp: Value = serde_json::from_str(&engine.handle(&req, &outbound).unwrap()).unwrap();
    assert_eq!(resp["result"]["written"], 11);
    assert_eq!(
        fs::read_to_string(space_dir.join("sub/hello.txt")).unwrap(),
        "hello world"
    );

    // 4. Read workspace file successfully
    let req = RpcRequest {
        id: "r1".to_string(),
        method: "read_workspace_file".to_string(),
        params: json!({
            "space_id": my_space_id,
            "rel_path": "sub/hello.txt",
        }),
    };
    let resp: Value = serde_json::from_str(&engine.handle(&req, &outbound).unwrap()).unwrap();
    assert_eq!(resp["result"]["content"], "hello world");

    // 5. Read non-existent file -> path_rejected
    let req = RpcRequest {
        id: "r2".to_string(),
        method: "read_workspace_file".to_string(),
        params: json!({
            "space_id": my_space_id,
            "rel_path": "sub/missing.txt",
        }),
    };
    let resp: Value = serde_json::from_str(&engine.handle(&req, &outbound).unwrap()).unwrap();
    assert_eq!(resp["error"]["code"], "path_rejected");

    // 6. Read from space owned by another device -> not_host
    let req = RpcRequest {
        id: "r3".to_string(),
        method: "read_workspace_file".to_string(),
        params: json!({
            "space_id": "space-remote-1",
            "rel_path": "hello.txt",
        }),
    };
    let resp: Value = serde_json::from_str(&engine.handle(&req, &outbound).unwrap()).unwrap();
    assert_eq!(resp["error"]["code"], "not_host");
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("port");
    listener.local_addr().expect("addr").port()
}

fn steer(text: &str) -> UserCommand {
    UserCommand::Steer {
        command_id: Uuid::new_v4(),
        message_id: format!("steer-{}", Uuid::new_v4()),
        text: text.to_string(),
    }
}

fn wait_then_done() -> Vec<Step> {
    vec![
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
    ]
}

#[tokio::test]
async fn steer_after_the_child_ended_is_promoted_not_lost() {
    let tmp = Tmp::new("steer-late");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::script(vec![Step::Emit(AgentEvent::Done {
        status: TurnStatus::Completed {},
    })]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("go")).expect("start");
    // Let the child run to its end without applying its Done.
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(harness.runs(), 1);
    assert_eq!(engine.live_runs(), 1);
    assert_eq!(
        engine.queue_command(&id, steer("late")).expect("steer"),
        QueueStatus::Queued
    );
    until(&engine, || harness.runs() == 2).await;
    assert_eq!(harness.initial_text(), "late");
    assert!(harness.observed().is_empty());
}

#[tokio::test]
async fn supervisor_folds_and_flushes_without_manual_polls() {
    let tmp = Tmp::new("supervisor");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::script(wait_then_done()));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    let supervisor = tokio::spawn(engine.supervise());
    engine.queue_command(&id, send("go")).expect("start");
    clock.set(clock.now_ms() + STREAM_COMMIT_MS + 1);
    let mut folded = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        clock.set(clock.now_ms() + 10);
        if engine.message_text(&id).iter().any(|text| text == "a") {
            folded = true;
            break;
        }
    }
    assert!(folded, "{:?}", engine.message_text(&id));
    supervisor.abort();
}

#[test]
fn recovery_stamps_once_and_skips_live_steers_and_old_commands() {
    let tmp = Tmp::new("recover-once");
    let clock = Arc::new(ManualClock::new(RETAIN_MS + 10_000));
    let now = RETAIN_MS + 10_000;
    let chat_id = "chat-once";
    let crashed = Uuid::new_v4();
    let live_steer = Uuid::new_v4();
    let ancient = Uuid::new_v4();
    let mut store = DocsStore::open(tmp.path().join("profile")).expect("store");
    put_chat(&mut store, DEVICE, chat_id, DEVICE, now, false, "idle");
    let doc = store.session(chat_id).expect("session");
    for (id, kind, at) in [
        (crashed, "send", now),
        (live_steer, "steer", now),
        (ancient, "send", 1),
    ] {
        let command = if kind == "send" {
            UserCommand::Send {
                command_id: id,
                message_id: format!("u-{id}"),
                text: "x".into(),
                attachments: Vec::new(),
            }
        } else {
            UserCommand::Steer {
                command_id: id,
                message_id: format!("u-{id}"),
                text: "x".into(),
            }
        };
        doc.append_command(DEVICE, DEVICE, &command, at)
            .expect("command");
        doc.set_outcome(DEVICE, id, Outcome::Processed)
            .expect("outcome");
    }
    store.commit(now).expect("commit");
    let reopen = |store: DocsStore| {
        Engine::open(EngineConfig {
            store,
            device_id: DEVICE.to_string(),
            scope: "local".to_string(),
            clock: clock.clone(),
            harnesses: vec![Arc::new(MockHarness::silent())],
            compaction_bytes: COMPACTION_BYTES,
        })
    };
    let engine = reopen(store);
    assert_eq!(engine.chat_status(chat_id).as_deref(), Some("aborted"));
    let aborts = |engine: &Engine| {
        engine
            .message_text(chat_id)
            .iter()
            .filter(|text| *text == "aborted")
            .count()
    };
    assert_eq!(
        aborts(&engine),
        1,
        "only the crashed send, not the live steer or the old send"
    );
    // The user carries on; the chat is idle again.
    engine
        .lock()
        .store
        .registry()
        .set_chat_status(DEVICE, chat_id, keel_proto::ChatStatus::Idle)
        .expect("idle");
    engine.maintenance();
    let profile = engine.profile().to_path_buf();
    drop(engine);
    let engine = reopen(DocsStore::open(&profile).expect("store"));
    assert_eq!(engine.chat_status(chat_id).as_deref(), Some("idle"));
    assert_eq!(aborts(&engine), 1);
}

#[tokio::test]
async fn unknown_chat_is_an_error_and_creates_nothing() {
    let tmp = Tmp::new("unknown");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let (outbound, _rx) = Outbound::pair();
    for (method, params) in [
        ("subscribe_chat", json!({ "chat_id": "nope" })),
        (
            "queue_command",
            json!({ "chat_id": "nope", "command": serde_json::to_value(send("hi")).unwrap() }),
        ),
        (
            "set_chat_options",
            json!({ "chat_id": "nope", "model": "m", "reasoning": "" }),
        ),
    ] {
        let req = RpcRequest {
            id: "x".into(),
            method: method.into(),
            params,
        };
        let resp: Value = serde_json::from_str(&engine.handle(&req, &outbound).unwrap()).unwrap();
        assert_eq!(resp["error"]["code"], "unknown_chat", "{method}");
    }
    assert!(engine.chat_ids().is_empty());
}

#[tokio::test]
async fn ensure_session_maps_every_harness() {
    let tmp = Tmp::new("harness-map");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    for name in ["keel", "mock"] {
        let chat_id = format!("remote-{name}");
        {
            let state = engine.lock();
            if state.store.registry().view().spaces.is_empty() {
                state
                    .store
                    .registry()
                    .put_space(&SpaceRow {
                        id: "space".into(),
                        device_id: "other".into(),
                        path: "/x".into(),
                        name: "x".into(),
                        git_detected: false,
                        checkout_id: String::new(),
                        repository_id: String::new(),
                    })
                    .unwrap();
            }
            state
                .store
                .registry()
                .put_chat(&ChatRow {
                    id: chat_id.clone(),
                    device_id: "other".into(),
                    title: "t".into(),
                    archived: false,
                    cwd: "/x".into(),
                    branch: String::new(),
                    checkout_id: String::new(),
                    space_id: "space".into(),
                    last_seen_at: 1,
                    last_message_preview: String::new(),
                    last_message_at: 1,
                    harness: name.into(),
                    model: "m".into(),
                    reasoning: String::new(),
                    permission: String::new(),
                    status: "idle".into(),
                    usage: Default::default(),
                })
                .unwrap();
        }
        let doc = engine.ensure_session(&chat_id).expect("session");
        assert_eq!(doc.view().meta.harness, name);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn models_options_and_create_chat_validation() {
    let tmp = Tmp::new("models");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let (outbound, _rx) = Outbound::pair();
    let call = |method: &str, params: Value| -> Value {
        let req = RpcRequest {
            id: "x".into(),
            method: method.into(),
            params,
        };
        serde_json::from_str(&engine.handle(&req, &outbound).unwrap()).unwrap()
    };
    let models = call("list_models", json!({ "harness": "mock" }));
    assert_eq!(
        models["result"],
        json!({"models":[{"id":"mock","name":"Mock","reasoning_levels":["off"]}],"default_model":"mock"})
    );
    assert_eq!(
        call("list_models", json!({ "harness": "nope" }))["error"]["code"],
        "bad_params"
    );

    let work = tmp.path().join("work");
    fs::create_dir_all(work.join("sub")).unwrap();
    let space = call("create_space", json!({ "path": work.to_string_lossy() }));
    let space_id = space["result"]["space_id"]
        .as_str()
        .expect("space")
        .to_string();
    let again = call(
        "create_space",
        json!({ "path": work.join("sub/..").to_string_lossy() }),
    );
    assert_eq!(again["result"]["space_id"], space_id.as_str());
    assert_eq!(
        call("create_space", json!({ "path": "relative" }))["error"]["code"],
        "bad_params"
    );

    let bad = |params: Value| call("create_chat", params)["error"]["code"].clone();
    assert_eq!(
        bad(json!({ "space_id": space_id, "harness": "nope", "model": "" })),
        "bad_params"
    );
    assert_eq!(
        bad(json!({ "space_id": space_id, "harness": "pi", "model": "" })),
        "bad_params"
    );
    assert_eq!(
        bad(json!({ "space_id": space_id, "harness": "mock", "model": "bogus" })),
        "bad_params"
    );
    assert_eq!(
        bad(json!({ "space_id": space_id, "harness": "mock", "model": "mock", "cwd": "/tmp" })),
        "bad_params"
    );
    let created = call(
        "create_chat",
        json!({ "space_id": space_id, "harness": "mock", "model": "mock", "reasoning": "off",
                "cwd": work.join("sub").to_string_lossy() }),
    );
    let chat_id = created["result"]["chat_id"]
        .as_str()
        .expect("chat")
        .to_string();
    assert_eq!(
        call(
            "set_chat_options",
            json!({ "chat_id": chat_id, "model": "bogus", "reasoning": "" })
        )["error"]["code"],
        "bad_params"
    );
    assert_eq!(
        call(
            "set_chat_options",
            json!({ "chat_id": chat_id, "model": "mock", "reasoning": "high" })
        ),
        json!({ "id": "x", "result": {} })
    );
    // The registry row the UI restores from carries the same effort.
    assert_eq!(
        engine
            .lock()
            .store
            .registry()
            .view()
            .chats
            .iter()
            .find(|c| c.id == chat_id)
            .map(|c| c.reasoning.as_str()),
        Some("high")
    );
    // The next turn's SessionSpec carries the session meta.
    let mut state = engine.lock();
    let doc = state.store.session(&chat_id).expect("session");
    let command = send("go");
    doc.append_command(DEVICE, DEVICE, &command, 5_000).unwrap();
    let view = doc.view().commands[0].clone();
    let start = engine
        .begin_run(&mut state, &chat_id, &view, 5_000)
        .expect("start");
    assert_eq!(start.spec.model.0, "mock");
    assert_eq!(start.spec.reasoning.map(|r| r.0).as_deref(), Some("high"));
    assert!(start.spec.cwd.ends_with("sub"));
    assert_eq!(start.spec.initial_text, "go");
    drop(state);
    // The retired poteto effort: an old client sending it stores the
    // fallback (here the harness default: this model only has "off"), and a
    // session saved on it runs there too, with no preamble.
    call(
        "set_chat_options",
        json!({ "chat_id": chat_id, "model": "mock", "reasoning": "poteto" }),
    );
    assert_eq!(
        engine
            .lock()
            .store
            .registry()
            .view()
            .chats
            .iter()
            .find(|c| c.id == chat_id)
            .map(|c| c.reasoning.clone())
            .as_deref(),
        Some("")
    );
    engine.lock().live.remove(&chat_id);
    let mut state = engine.lock();
    let doc = state.store.session(&chat_id).expect("session");
    doc.set_options("mock", "poteto").unwrap();
    let command = send("dig");
    doc.append_command(DEVICE, DEVICE, &command, 6_000).unwrap();
    let view = doc.view().commands.last().unwrap().clone();
    let start = engine
        .begin_run(&mut state, &chat_id, &view, 6_000)
        .expect("start");
    assert_eq!(start.spec.reasoning, None);
    assert_eq!(start.spec.initial_text, "dig");
}

#[tokio::test]
async fn a_space_from_another_device_has_no_files_here() {
    let tmp = Tmp::new("foreign-space");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    {
        let state = engine.lock();
        state
            .store
            .registry()
            .put_space(&SpaceRow {
                id: "theirs".into(),
                device_id: "other".into(),
                path: "/x".into(),
                name: "x".into(),
                git_detected: false,
                checkout_id: String::new(),
                repository_id: String::new(),
            })
            .unwrap();
    }
    let (outbound, _rx) = Outbound::pair();
    for (method, params) in [
        (
            "read_workspace_file",
            json!({ "space_id": "theirs", "rel_path": "a" }),
        ),
        (
            "write_workspace_file",
            json!({ "space_id": "theirs", "rel_path": "a", "content": "x" }),
        ),
    ] {
        let req = RpcRequest {
            id: "r".into(),
            method: method.into(),
            params,
        };
        let resp: Value = serde_json::from_str(&engine.handle(&req, &outbound).unwrap()).unwrap();
        assert_eq!(resp["error"]["code"], "not_host", "{method}");
    }
}

#[tokio::test]
async fn archive_removes_only_engine_worktrees() {
    let tmp = Tmp::new("wt-guard");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    // A user folder that merely has `/worktrees/` in its path.
    let user = tmp.path().join("projects").join("worktrees").join("mine");
    fs::create_dir_all(&user).unwrap();
    fs::write(user.join("keep.txt"), "keep").unwrap();
    let chat_id = engine
        .create_chat(user.to_str().unwrap(), "mock", "mock")
        .expect("chat");
    let reply = engine.rpc_set_archived("a", &json!({ "chat_id": chat_id, "archived": true }));
    assert!(reply.contains("\"result\":{}"), "{reply}");
    assert!(user.join("keep.txt").exists());
}

#[tokio::test]
async fn space_rescan_leaves_worktree_chats_alone() {
    let tmp = Tmp::new("wt-status");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let repo_dir = tmp.path().join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    for args in [
        &["init", "-b", "main"][..],
        &["config", "user.name", "T"],
        &["config", "user.email", "t@t"],
        &["commit", "--allow-empty", "-m", "init"],
    ] {
        repo::run_git_cmd_sync(&repo_dir, args).expect("git");
    }
    let main_chat = engine
        .create_chat_full(repo_dir.to_str().unwrap(), "mock", "mock", false, None)
        .unwrap();
    let wt_chat = engine
        .create_chat_full(
            repo_dir.to_str().unwrap(),
            "mock",
            "mock",
            true,
            Some("feature"),
        )
        .unwrap();
    let (space_id, wt_before) = {
        let state = engine.lock();
        let view = state.store.registry().view();
        let wt = view.chats.iter().find(|c| c.id == wt_chat).unwrap().clone();
        (wt.space_id.clone(), wt)
    };
    assert_eq!(wt_before.branch, "feature");
    let mut status = repo::scan_git_repo_sync(DEVICE, &repo_dir);
    status.branch = "elsewhere".into();
    engine.apply(super::RunEvent::RepoStatus {
        space_id,
        status,
        captured_at: 6_000,
    });
    let state = engine.lock();
    let view = state.store.registry().view();
    assert_eq!(
        view.chats
            .iter()
            .find(|c| c.id == main_chat)
            .unwrap()
            .branch,
        "elsewhere"
    );
    let wt_after = view.chats.iter().find(|c| c.id == wt_chat).unwrap();
    assert_eq!(wt_after.branch, "feature");
    assert_eq!(wt_after.checkout_id, wt_before.checkout_id);
}

#[tokio::test]
async fn create_chat_on_another_branch_checks_out_or_refuses_when_dirty() {
    let tmp = Tmp::new("branch-checkout");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let repo_dir = tmp.path().join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    for args in [
        &["init", "-b", "main"][..],
        &["config", "user.name", "T"],
        &["config", "user.email", "t@t"],
        &["config", "commit.gpgsign", "false"],
    ] {
        repo::run_git_cmd_sync(&repo_dir, args).expect("git");
    }
    fs::write(repo_dir.join("a.txt"), "one\n").unwrap();
    repo::run_git_cmd_sync(&repo_dir, &["add", "a.txt"]).unwrap();
    repo::run_git_cmd_sync(&repo_dir, &["commit", "-m", "init"]).unwrap();
    repo::run_git_cmd_sync(&repo_dir, &["branch", "feat"]).unwrap();
    let path = repo_dir.to_str().unwrap();
    let chats = |engine: &Engine| engine.lock().store.registry().view().chats.len();

    // Dirty tree: refused, nothing created, still on main.
    fs::write(repo_dir.join("a.txt"), "changed\n").unwrap();
    let before = chats(&engine);
    let err = engine
        .create_chat_full(path, "mock", "mock", false, Some("feat"))
        .unwrap_err();
    assert!(
        err.to_string().starts_with("Uncommitted changes in repo"),
        "{err}"
    );
    assert_eq!(chats(&engine), before);
    assert_eq!(
        repo::current_head_sync(&repo_dir).unwrap(),
        repo::Head::Branch("main".into())
    );

    // The same branch as checked out needs no switch, dirty or not.
    engine
        .create_chat_full(path, "mock", "mock", false, Some("main"))
        .unwrap();

    // Clean tree: switched, and the chat records the new branch.
    repo::run_git_cmd_sync(&repo_dir, &["checkout", "--", "a.txt"]).unwrap();
    let chat_id = engine
        .create_chat_full(path, "mock", "mock", false, Some("feat"))
        .unwrap();
    assert_eq!(
        repo::current_head_sync(&repo_dir).unwrap(),
        repo::Head::Branch("feat".into())
    );
    let view = engine.lock().store.registry().view();
    assert_eq!(
        view.chats.iter().find(|c| c.id == chat_id).unwrap().branch,
        "feat"
    );
}

#[tokio::test]
async fn chat_checkout_branch_switches_the_chats_checkout_or_refuses_when_dirty() {
    let tmp = Tmp::new("chat-checkout");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let repo_dir = tmp.path().join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    for args in [
        &["init", "-b", "main"][..],
        &["config", "user.name", "T"],
        &["config", "user.email", "t@t"],
        &["config", "commit.gpgsign", "false"],
    ] {
        repo::run_git_cmd_sync(&repo_dir, args).expect("git");
    }
    fs::write(repo_dir.join("a.txt"), "one\n").unwrap();
    repo::run_git_cmd_sync(&repo_dir, &["add", "a.txt"]).unwrap();
    repo::run_git_cmd_sync(&repo_dir, &["commit", "-m", "init"]).unwrap();
    repo::run_git_cmd_sync(&repo_dir, &["branch", "feat"]).unwrap();
    let chat_id = engine
        .create_chat_full(repo_dir.to_str().unwrap(), "mock", "mock", false, None)
        .unwrap();
    let (outbound, mut rx) = Outbound::pair();
    async fn call(
        engine: &Engine,
        outbound: &Outbound,
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
        method: &str,
        params: Value,
    ) -> Value {
        let request = RpcRequest {
            id: method.into(),
            method: method.into(),
            params,
        };
        let first: Value =
            serde_json::from_str(&engine.handle(&request, outbound).unwrap()).unwrap();
        if first.get("id").is_some() {
            return first;
        }
        let pushed = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&pushed).unwrap()
    }

    // The listing is this chat's checkout (the card pins `current` first).
    let list = call(
        &engine,
        &outbound,
        &mut rx,
        "chat_branches",
        json!({ "chat_id": chat_id }),
    )
    .await;
    assert_eq!(list["result"]["current"], json!("main"), "{list}");
    assert_eq!(
        list["result"]["branches"],
        json!(["feat", "main"]),
        "{list}"
    );

    // Dirty tree: refused with the guard's message, still on main.
    fs::write(repo_dir.join("a.txt"), "changed\n").unwrap();
    let refused = call(
        &engine,
        &outbound,
        &mut rx,
        "chat_checkout_branch",
        json!({ "chat_id": chat_id, "branch": "feat" }),
    )
    .await;
    let message = refused["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.starts_with("Uncommitted changes in repo"),
        "{refused}"
    );
    assert_eq!(
        repo::current_head_sync(&repo_dir).unwrap(),
        repo::Head::Branch("main".into())
    );

    // Clean: switched in place.
    repo::run_git_cmd_sync(&repo_dir, &["checkout", "--", "a.txt"]).unwrap();
    let done = call(
        &engine,
        &outbound,
        &mut rx,
        "chat_checkout_branch",
        json!({ "chat_id": chat_id, "branch": "feat" }),
    )
    .await;
    assert_eq!(done["result"]["branch"], json!("feat"), "{done}");
    assert_eq!(
        repo::current_head_sync(&repo_dir).unwrap(),
        repo::Head::Branch("feat".into())
    );

    // Unknown chats and option-looking names are refused.
    let unknown = call(
        &engine,
        &outbound,
        &mut rx,
        "chat_checkout_branch",
        json!({ "chat_id": "nope", "branch": "main" }),
    )
    .await;
    assert!(unknown.get("error").is_some(), "{unknown}");
    let flag = call(
        &engine,
        &outbound,
        &mut rx,
        "chat_checkout_branch",
        json!({ "chat_id": chat_id, "branch": "--force" }),
    )
    .await;
    assert!(flag.get("error").is_some(), "{flag}");
    assert_eq!(
        repo::current_head_sync(&repo_dir).unwrap(),
        repo::Head::Branch("feat".into())
    );
}

/// Each run streams whatever the test pushes on that run's channel, and the
/// stream closes only when the test drops the sender.
struct Gated {
    runs: Mutex<Vec<tokio::sync::mpsc::UnboundedReceiver<AgentEvent>>>,
    started: AtomicU64,
}

#[async_trait::async_trait]
impl keel_harness::Harness for Gated {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "gated"
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
    async fn models(&self) -> Result<Vec<keel_harness::Model>, keel_harness::HarnessError> {
        Ok(Vec::new())
    }
    async fn reasoning_levels(
        &self,
        _model: &keel_harness::ModelId,
    ) -> Result<Vec<keel_harness::ReasoningLevel>, keel_harness::HarnessError> {
        Ok(Vec::new())
    }
    async fn run(
        &self,
        _spec: keel_harness::SessionSpec,
        _controls: keel_harness::RunControls,
    ) -> Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<AgentEvent, keel_harness::HarnessError>> + Send>,
        >,
        keel_harness::HarnessError,
    > {
        let rx = self.runs.lock().expect("runs").remove(0);
        self.started.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (Ok(event), rx))
        })))
    }
}

#[tokio::test]
async fn late_close_of_a_finished_run_does_not_abort_the_next_one() {
    let tmp = Tmp::new("late-close");
    let clock = Arc::new(ManualClock::new(5_000));
    let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
    let (tx2, rx2) = tokio::sync::mpsc::unbounded_channel();
    let harness = Arc::new(Gated {
        runs: Mutex::new(vec![rx1, rx2]),
        started: AtomicU64::new(0),
    });
    let store = DocsStore::open(tmp.path().join("profile")).expect("store");
    let engine = Engine::open(EngineConfig {
        store,
        device_id: DEVICE.to_string(),
        scope: "local".to_string(),
        clock: clock.clone(),
        harnesses: vec![harness.clone()],
        compaction_bytes: COMPACTION_BYTES,
    });
    let id = chat(&engine);
    let done = || AgentEvent::Done {
        status: TurnStatus::Completed {},
    };

    engine.queue_command(&id, send("one")).expect("first");
    until(&engine, || harness.started.load(Ordering::SeqCst) == 1).await;
    tx1.send(done()).expect("done 1");
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("idle")
    })
    .await;

    // Run 1 finished but its stream is still open. Run 2 starts.
    engine.queue_command(&id, send("two")).expect("second");
    until(&engine, || harness.started.load(Ordering::SeqCst) == 2).await;
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("running")
    })
    .await;

    // Run 1's stream closes now. Run 2 must stay live.
    drop(tx1);
    for _ in 0..5 {
        engine.drive().await;
    }
    assert_eq!(engine.chat_status(&id).as_deref(), Some("running"));

    tx2.send(AgentEvent::Text {
        message_id: "m2".to_string(),
        part_id: "p".to_string(),
        delta: "second reply".to_string(),
        subagent: None,
    })
    .expect("text 2");
    tx2.send(done()).expect("done 2");
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("idle")
    })
    .await;
}

#[tokio::test]
async fn each_turn_gets_its_own_assistant_message_when_the_driver_reuses_ids() {
    let tmp = Tmp::new("turn-ids");
    let clock = Arc::new(ManualClock::new(5_000));
    // The echo mock answers "ok" under message id "m" on every run.
    let harness = Arc::new(MockHarness::echo());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    for (n, text) in ["one", "two"].into_iter().enumerate() {
        engine.queue_command(&id, send(text)).expect("send");
        until(&engine, || harness.runs() == n + 1).await;
        until(&engine, || {
            engine.chat_status(&id).as_deref() == Some("idle")
        })
        .await;
        clock.set(5_000 + 1_000 * (n as i64 + 1));
        engine.drive().await;
    }
    let state = engine.lock();
    let view = state.store.session(&id).expect("session").view();
    let replies: Vec<&str> = view
        .messages
        .iter()
        .filter(|message| message.role == "assistant")
        .map(|message| message.text.as_str())
        .collect();
    assert_eq!(replies, vec!["ok", "ok"]);
}

#[tokio::test]
async fn a_failed_tool_is_stored_as_error() {
    let tmp = Tmp::new("tool-error");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::script(vec![
        Step::Emit(AgentEvent::ToolStarted {
            tool_id: "t1".to_string(),
            name: "bash".to_string(),
            input_bytes: 3,
            detail: String::new(),
            subagent: None,
            kind: None,
        }),
        Step::Emit(AgentEvent::ToolFinished {
            tool_id: "t1".to_string(),
            summary: "exit 1".to_string(),
            output_bytes: 6,
            is_error: true,
            images: Vec::new(),
            output_tail: String::new(),
            subagent: None,
            diff: None,
            duration_ms: None,
        }),
        Step::Emit(AgentEvent::Done {
            status: TurnStatus::Completed {},
        }),
    ]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("go")).expect("send");
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("idle")
    })
    .await;
    let state = engine.lock();
    let view = state.store.session(&id).expect("session").view();
    let tool = view
        .messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .find_map(|part| match part {
            PartView::Tool(row) if row.tool_id == "t1" => Some(row.clone()),
            _ => None,
        })
        .expect("tool part");
    assert_eq!(
        (
            tool.name.as_str(),
            tool.state.as_str(),
            tool.summary.as_str()
        ),
        ("bash", "error", "exit 1")
    );
}

/// Kind, a streamed tail, a diff and a harness duration survive a finish.
/// A tool still running when the turn is interrupted is `cancelled`.
/// A generic later kind does not replace a specific one.
#[tokio::test]
async fn trace_fields_survive_finish_and_a_stop_cancels_the_open_tool() {
    let tmp = Tmp::new("tool-trace");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::script(vec![
        Step::Emit(AgentEvent::ToolStarted {
            tool_id: "t-run".to_string(),
            name: "bash".to_string(),
            input_bytes: 4,
            detail: "pnpm test".to_string(),
            subagent: None,
            kind: Some("execute".to_string()),
        }),
        Step::Emit(AgentEvent::ToolProgress {
            tool_id: "t-run".to_string(),
            output_tail: "hi\n".to_string(),
            subagent: None,
            kind: Some("other".to_string()),
        }),
        Step::Emit(AgentEvent::ToolFinished {
            tool_id: "t-run".to_string(),
            summary: "ok".to_string(),
            output_bytes: 3,
            is_error: false,
            images: Vec::new(),
            output_tail: String::new(),
            subagent: None,
            diff: Some(keel_proto::ToolDiff {
                path: "src/lib.rs".to_string(),
                old_text: Some("a".to_string()),
                new_text: "b".to_string(),
            }),
            duration_ms: Some(1_500),
        }),
        Step::Emit(AgentEvent::ToolStarted {
            tool_id: "t-live".to_string(),
            name: "list_dir".to_string(),
            input_bytes: 1,
            detail: "src".to_string(),
            subagent: None,
            kind: None,
        }),
        Step::Emit(AgentEvent::ToolProgress {
            tool_id: "t-live".to_string(),
            output_tail: String::new(),
            subagent: None,
            kind: Some("search".to_string()),
        }),
        Step::Emit(AgentEvent::Plan {
            message_id: "codex-turn".to_string(),
            entries: vec![
                keel_proto::PlanEntry {
                    content: "one".to_string(),
                    status: "completed".to_string(),
                    priority: None,
                },
                keel_proto::PlanEntry {
                    content: "two".to_string(),
                    status: "pending".to_string(),
                    priority: Some("high".to_string()),
                },
            ],
            subagent: None,
        }),
        Step::Emit(AgentEvent::Done {
            status: TurnStatus::Interrupted {},
        }),
    ]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("go")).expect("send");
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("idle")
    })
    .await;
    let state = engine.lock();
    let view = state.store.session(&id).expect("session").view();
    let tool = |tool_id: &str| {
        view.messages
            .iter()
            .flat_map(|message| message.parts.iter())
            .find_map(|part| match part {
                PartView::Tool(row) if row.tool_id == tool_id => Some(row.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {tool_id}"))
    };
    let ran = tool("t-run");
    assert_eq!(ran.kind, "execute");
    assert_eq!(ran.state, "done");
    assert_eq!(ran.detail, "pnpm test");
    assert_eq!(ran.output_tail, "hi\n");
    assert_eq!(ran.started_ms, Some(3_500));
    assert_eq!(ran.ended_ms, Some(5_000));
    let diff: Value = serde_json::from_str(&ran.diff).expect("diff json");
    assert_eq!(diff["path"], "src/lib.rs");
    assert_eq!(diff["old_text"], "a");
    assert_eq!(diff["new_text"], "b");
    let live = tool("t-live");
    assert_eq!(live.state, "cancelled");
    assert_eq!(live.kind, "search");
    assert_eq!(live.started_ms, Some(5_000));
    assert_eq!(live.ended_ms, Some(5_000));
    assert_eq!(live.summary, "");
    let plan = view
        .messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .find_map(|part| match part {
            PartView::Plan { entries } => Some(entries.clone()),
            _ => None,
        })
        .expect("plan");
    assert_eq!(plan.len(), 2);
    assert_eq!(plan[0].content, "one");
    assert_eq!(plan[0].status, "completed");
    assert_eq!(plan[1].priority.as_deref(), Some("high"));
    let message = view
        .messages
        .iter()
        .find(|message| {
            message
                .parts
                .iter()
                .any(|part| matches!(part, PartView::Tool(row) if row.tool_id == "t-run"))
        })
        .expect("assistant");
    let json = super::message_json(message, std::path::Path::new("/nonexistent"));
    let part = json["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["tool_id"] == "t-run")
        .unwrap();
    assert_eq!(part["kind"], "tool");
    assert_eq!(part["tool_kind"], "execute");
    assert_eq!(part["output_tail"], "hi\n");
    assert_eq!(part["diff"]["new_text"], "b");
    assert!(
        json["parts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["kind"] == "plan")
    );
}

/// A screenshot tool's `data:` image is written to disk and the doc keeps
/// only its path, which `message_json` hands the transcript.
#[tokio::test]
async fn a_tool_image_is_saved_and_stored_as_a_path() {
    use base64::Engine as _;
    let tmp = Tmp::new("tool-image");
    let clock = Arc::new(ManualClock::new(5_000));
    let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG shot");
    let harness = Arc::new(MockHarness::script(vec![
        Step::Emit(AgentEvent::ToolStarted {
            tool_id: "s1".to_string(),
            name: "mcp__keel_browser__browser_screenshot".to_string(),
            input_bytes: 2,
            detail: String::new(),
            subagent: None,
            kind: None,
        }),
        Step::Emit(AgentEvent::ToolFinished {
            tool_id: "s1".to_string(),
            summary: "{\"url\": \"https://google.com\"}".to_string(),
            output_bytes: 9,
            is_error: false,
            images: vec![format!("data:image/png;base64,{png}")],
            output_tail: String::new(),
            subagent: None,
            diff: None,
            duration_ms: None,
        }),
        Step::Emit(AgentEvent::Done {
            status: TurnStatus::Completed {},
        }),
    ]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("shot")).expect("send");
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("idle")
    })
    .await;
    let state = engine.lock();
    let view = state.store.session(&id).expect("session").view();
    let message = view
        .messages
        .iter()
        .find(|m| {
            m.parts
                .iter()
                .any(|p| matches!(p, PartView::Tool(row) if row.tool_id == "s1"))
        })
        .expect("tool message");
    let row = message
        .parts
        .iter()
        .find_map(|p| match p {
            PartView::Tool(row) if row.tool_id == "s1" => Some(row.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(row.images.len(), 1, "{row:?}");
    let path = std::path::Path::new(&row.images[0]);
    assert!(
        path.is_absolute() && path.components().any(|c| c.as_os_str() == "screenshots"),
        "{path:?}"
    );
    assert_eq!(std::fs::read(path).unwrap(), b"\x89PNG shot");
    let json = super::message_json(message, std::path::Path::new("/nonexistent"));
    let tool = json["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["kind"] == "tool")
        .unwrap();
    assert_eq!(tool["images"][0], row.images[0].as_str());
    assert!(
        !state
            .store
            .session(&id)
            .unwrap()
            .deep_json()
            .to_string()
            .contains(&png)
    );
}

#[test]
fn message_json_carries_parts_for_every_push() {
    let message = keel_doc::MessageView {
        id: "m".to_string(),
        role: "assistant".to_string(),
        created_at: 1,
        parts: vec![
            PartView::Text {
                id: "p".to_string(),
                body: "hi".to_string(),
            },
            PartView::Tool(ToolRow {
                tool_id: "t".to_string(),
                name: "bash".to_string(),
                state: "error".to_string(),
                summary: "boom".to_string(),
                input_bytes: 0,
                output_bytes: 0,
                detail: String::new(),
                images: vec!["/s/1.png".to_string()],
                ..ToolRow::default()
            }),
        ],
        text: "hi".to_string(),
        attachments: Vec::new(),
    };
    let json = super::message_json(&message, std::path::Path::new("/nonexistent"));
    assert_eq!(json["parts"][0]["kind"], "text");
    assert_eq!(json["parts"][0]["body"], "hi");
    assert_eq!(json["parts"][1]["name"], "bash");
    assert_eq!(json["parts"][1]["state"], "error");
    assert_eq!(json["parts"][1]["images"][0], "/s/1.png");
    // Not repeated at the top level: the parts are the one copy.
    assert!(
        json.get("text").is_none()
            && json.get("tools").is_none()
            && json.get("reasoning").is_none()
    );
}

#[tokio::test]
async fn attached_image_reaches_the_harness_and_the_user_message() {
    let tmp = Tmp::new("attach");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::echo());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    let source = tmp.path().join("shot.png");
    fs::write(&source, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").expect("png");
    let attachment = crate::attachments::attach_file(&tmp.path().join("profile"), &source, None)
        .expect("attach");

    engine
        .queue_command(
            &id,
            UserCommand::Send {
                command_id: Uuid::new_v4(),
                message_id: "u-img".to_string(),
                text: "what is this".to_string(),
                attachments: vec![attachment.clone()],
            },
        )
        .expect("send");
    until(&engine, || harness.runs() == 1).await;
    let files = harness.attachments();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].mime, "image/png");
    assert_eq!(
        fs::read(&files[0].path).expect("stored"),
        fs::read(&source).expect("source")
    );

    let state = engine.lock();
    let view = state.store.session(&id).expect("session").view();
    let message = view
        .messages
        .iter()
        .find(|m| m.id == "u-img")
        .expect("user message");
    assert_eq!(message.attachments, vec![attachment]);
    let json = super::message_json(message, &tmp.path().join("profile"));
    assert!(
        json["attachments"][0]["path"]
            .as_str()
            .is_some_and(|p| p.ends_with(&message.attachments[0].id))
    );
}

#[test]
fn topic_title_names_a_chat_from_its_first_message() {
    assert_eq!(
        super::topic_title(
            "can you search for the latest news on finance and banks in america today"
        )
        .as_deref(),
        Some("Search for the latest news on finance and banks…")
    );
    assert_eq!(
        super::topic_title("  \n## Fix the login bug?\nmore").as_deref(),
        Some("Fix the login bug")
    );
    assert_eq!(
        super::topic_title("Hey, please refactor auth").as_deref(),
        Some("Refactor auth")
    );
    assert_eq!(super::topic_title("Yoooo").as_deref(), Some("Yoooo"));
    assert_eq!(super::topic_title("   ").as_deref(), None);
}

#[test]
fn clean_title_keeps_just_the_title() {
    assert_eq!(
        super::clean_title("Latest US Bank News\n").as_deref(),
        Some("Latest US Bank News")
    );
    assert_eq!(
        super::clean_title("Sure!\n\n**Title: \"US Banking News Today.\"**").as_deref(),
        Some("US Banking News Today")
    );
    assert_eq!(super::clean_title("   \n").as_deref(), None);
    assert_eq!(super::clean_title(&"word ".repeat(30)).as_deref(), None);
}

struct SuggestionHarness(Arc<MockHarness>);

#[async_trait::async_trait]
impl keel_harness::Harness for SuggestionHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Suggestion fixture"
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
    fn one_shot_args(&self, _: &keel_harness::ModelId, _: &str) -> Option<Vec<String>> {
        Some(Vec::new())
    }
    async fn models(&self) -> Result<Vec<keel_harness::Model>, keel_harness::HarnessError> {
        self.0.models().await
    }
    async fn reasoning_levels(
        &self,
        model: &keel_harness::ModelId,
    ) -> Result<Vec<keel_harness::ReasoningLevel>, keel_harness::HarnessError> {
        self.0.reasoning_levels(model).await
    }
    async fn one_shot(
        &self,
        model: &keel_harness::ModelId,
        prompt: &str,
    ) -> Result<String, keel_harness::HarnessError> {
        if model.0 != "mock" {
            return Err(keel_harness::HarnessError::Failed("wrong model".into()));
        }
        let Some((_, recent)) = prompt.split_once("Recent messages, oldest first: ") else {
            return Ok("Cache expiry".into());
        };
        let messages: Value = serde_json::from_str(recent).unwrap();
        // Echo the latest message as the model's answer to exercise the RPC's
        // context selection and output validation through its public interface.
        Ok(messages.as_array().unwrap().last().unwrap()["text"]
            .as_str()
            .unwrap()
            .to_string())
    }
    async fn run(
        &self,
        spec: keel_harness::SessionSpec,
        controls: keel_harness::RunControls,
    ) -> Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<AgentEvent, keel_harness::HarnessError>> + Send>,
        >,
        keel_harness::HarnessError,
    > {
        self.0.run(spec, controls).await
    }
}

#[tokio::test]
async fn prompt_suggestion_uses_current_chat_without_changing_its_session() {
    let tmp = Tmp::new("suggestion");
    let harness = Arc::new(MockHarness::silent());
    let engine = Engine::open(EngineConfig {
        store: DocsStore::open(tmp.path().join("profile")).unwrap(),
        device_id: DEVICE.into(),
        scope: "local".into(),
        clock: Arc::new(ManualClock::new(5000)),
        harnesses: vec![Arc::new(SuggestionHarness(harness.clone()))],
        compaction_bytes: COMPACTION_BYTES,
    });
    let chat_id = chat(&engine);
    let other = chat(&engine);
    let other_doc = engine.ensure_session(&other).unwrap();
    other_doc.append_message("other", "assistant", 1).unwrap();
    other_doc
        .append_text("other", "text", "Wrong chat")
        .unwrap();
    let (outbound, mut rx) = Outbound::pair();
    let request = RpcRequest {
        id: "suggest".into(),
        method: "suggest_prompt".into(),
        params: json!({ "chat_id": chat_id }),
    };
    let empty: Value = serde_json::from_str(&engine.handle(&request, &outbound).unwrap()).unwrap();
    assert_eq!(empty["result"], json!({ "suggestion": null }));
    let doc = engine.ensure_session(&chat_id).unwrap();
    doc.append_message("user", "user", 1).unwrap();
    doc.append_text("user", "text", "Implement cache expiry.")
        .unwrap();
    for (index, (answer, expected)) in [
        (
            "\"Test the cache expiry at 61 seconds.\"\n",
            Some("Test the cache expiry at 61 seconds."),
        ),
        ("NONE", None),
        ("First suggestion\nSecond suggestion", None),
        ("", None),
    ]
    .into_iter()
    .enumerate()
    {
        let message = format!("assistant-{index}");
        doc.append_message(&message, "assistant", 2 + index as i64)
            .unwrap();
        doc.append_text(&message, "text", answer).unwrap();
        let before = doc.view();
        assert_eq!(engine.handle(&request, &outbound).as_deref(), Some("{}"));
        let reply: Value = serde_json::from_str(
            &tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            reply,
            json!({ "id": "suggest", "result": { "suggestion": expected } })
        );
        assert_eq!(doc.view(), before);
        assert_eq!(engine.live_runs(), 0);
    }
    engine
        .queue_command(&chat_id, send("Keep working"))
        .unwrap();
    let busy: Value = serde_json::from_str(&engine.handle(&request, &outbound).unwrap()).unwrap();
    assert_eq!(busy["result"], json!({ "suggestion": null }));
    let unknown = RpcRequest {
        params: json!({ "chat_id": "missing" }),
        ..request
    };
    let error: Value = serde_json::from_str(&engine.handle(&unknown, &outbound).unwrap()).unwrap();
    assert_eq!(error["error"]["code"], "unknown_chat");
}

#[test]
fn screenshot_sweep_removes_only_expired_files_and_empty_dirs() {
    let tmp = Tmp::new("shot-sweep");
    let root = tmp.path().join("screenshots");
    fs::create_dir_all(root.join("chat-a")).unwrap();
    fs::create_dir_all(root.join("chat-b")).unwrap();
    for file in [
        "loose.png",
        "chat-a/one.png",
        "chat-a/two.png",
        "chat-b/three.png",
    ] {
        fs::write(root.join(file), b"png").unwrap();
    }
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    // Fresh files stay.
    assert_eq!(super::delete_older_tree(&root, now, crate::RETAIN_MS), 0);
    assert!(root.join("chat-a/one.png").exists());
    // A month later every file is past retention; the emptied dirs go too.
    let later = now + crate::RETAIN_MS + 60_000;
    assert_eq!(super::delete_older_tree(&root, later, crate::RETAIN_MS), 4);
    assert!(!root.join("chat-a").exists() && !root.join("chat-b").exists());
    assert!(root.exists(), "the screenshots root itself is kept");
    // A missing root is a no-op.
    assert_eq!(
        super::delete_older_tree(&root.join("nope"), later, crate::RETAIN_MS),
        0
    );
}

#[test]
fn block_on_timeout_works_from_every_context() {
    let quick = || async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        7
    };
    assert_eq!(
        super::block_on_timeout(quick(), Duration::from_secs(2)),
        Some(7)
    );
    let slow = async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        1
    };
    assert_eq!(
        super::block_on_timeout(slow, Duration::from_millis(20)),
        None
    );
    let current = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert_eq!(
        current.block_on(async { super::block_on_timeout(quick(), Duration::from_secs(2)) }),
        Some(7)
    );
    let multi = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let got = multi.block_on(async {
        tokio::spawn(async move { super::block_on_timeout(quick(), Duration::from_secs(2)) })
            .await
            .unwrap()
    });
    assert_eq!(got, Some(7));
    // A borrowed future still works (the helper thread is scoped).
    let text = String::from("borrowed");
    assert_eq!(
        super::block_on_timeout(async { text.len() }, Duration::from_secs(1)),
        Some(8)
    );
}

/// A full subagent turn: the spawn, the child's text, thinking and tools, and
/// the child's end all land in the subagent's own record and transcript, and
/// the parent transcript gets only the indicator row.
#[tokio::test]
async fn subagent_traffic_lands_in_its_own_record_and_indicator() {
    let tmp = Tmp::new("subagent");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::script(vec![
        Step::Emit(AgentEvent::SubagentStarted {
            tool_id: "task-1".to_string(),
            subagent_id: "sub-1".to_string(),
            name: "explorer".to_string(),
            description: "look around".to_string(),
        }),
        Step::Emit(AgentEvent::Text {
            message_id: "sm".to_string(),
            part_id: "sp".to_string(),
            delta: "hello ".to_string(),
            subagent: Some(SubagentRef::new("sub-1")),
        }),
        Step::Emit(AgentEvent::Text {
            message_id: "sm".to_string(),
            part_id: "sp".to_string(),
            delta: "world".to_string(),
            subagent: Some(SubagentRef::new("sub-1")),
        }),
        Step::Emit(AgentEvent::ToolStarted {
            tool_id: "t9".to_string(),
            name: "bash".to_string(),
            input_bytes: 3,
            detail: String::new(),
            subagent: Some(SubagentRef::new("sub-1")),
            kind: None,
        }),
        Step::Emit(AgentEvent::ToolFinished {
            tool_id: "t9".to_string(),
            summary: "exit 0".to_string(),
            output_bytes: 12,
            is_error: false,
            images: Vec::new(),
            output_tail: String::new(),
            subagent: Some(SubagentRef::new("sub-1")),
            diff: None,
            duration_ms: None,
        }),
        Step::Emit(AgentEvent::SubagentDone {
            subagent_id: "sub-1".to_string(),
            status: TurnStatus::Completed {},
        }),
        Step::Emit(AgentEvent::Done {
            status: TurnStatus::Completed {},
        }),
    ]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("go")).expect("start");
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("idle")
    })
    .await;

    let state = engine.lock();
    let doc = state.store.session(&id).expect("session");
    let view = doc.view();

    // The subagent record: running through done, with its spawn tool and name.
    assert_eq!(view.subagents.len(), 1);
    let sub = &view.subagents[0];
    assert_eq!(sub.id, "sub-1");
    assert_eq!(sub.name, "explorer");
    assert_eq!(sub.parent_tool_id, "task-1");
    assert_eq!(sub.status, "done");
    assert!(sub.ended_at.is_some());

    // Its own transcript: the folded text and the finished tool.
    let parts = doc.sub_parts("sub-1").expect("sub parts");
    let text = parts
        .iter()
        .filter_map(|part| match part {
            PartView::Text { body, .. } => Some(body.clone()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(text, "hello world");
    assert!(parts.iter().any(|part| matches!(
        part,
        PartView::Tool(row) if row.tool_id == "t9" && row.name == "bash" && row.state == "done"
    )));

    // The parent transcript: one indicator row, no child text.
    let indicator = view
        .messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .find_map(|part| match part {
            PartView::Subagent {
                subagent_id, state, ..
            } => Some((subagent_id.clone(), state.clone())),
            _ => None,
        });
    assert_eq!(indicator, Some(("sub-1".to_string(), "done".to_string())));
    let parent_text = view
        .messages
        .iter()
        .filter(|message| message.role == "assistant")
        .map(|message| message.text.clone())
        .collect::<String>();
    assert!(
        !parent_text.contains("hello world"),
        "child text leaked into the parent"
    );
}

/// A driver that never ends its subagent (an async spawn): the turn's end
/// ends it, in its record and in the parent's indicator row.
#[tokio::test]
async fn turn_end_closes_open_subagents() {
    let tmp = Tmp::new("subagent-sweep");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::script(vec![
        Step::Emit(AgentEvent::SubagentStarted {
            tool_id: "task-1".to_string(),
            subagent_id: "sub-1".to_string(),
            name: "explorer".to_string(),
            description: String::new(),
        }),
        Step::Emit(AgentEvent::Done {
            status: TurnStatus::Completed {},
        }),
    ]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("go")).expect("start");
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("idle")
    })
    .await;

    let state = engine.lock();
    let view = state.store.session(&id).expect("session").view();
    assert_eq!(view.subagents[0].status, "done");
    let row = view
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .find_map(|part| match part {
            PartView::Subagent { state, .. } => Some(state.clone()),
            _ => None,
        });
    assert_eq!(row.as_deref(), Some("done"));
}

/// The suggestion prompt keeps the latest reply's ending (where its question
/// or offer is) and the chat's first ask, however long the chat.
#[test]
fn suggestion_prompt_keeps_the_reply_ending_and_the_goal() {
    let msg = |role: &str, text: String| {
        Arc::new(keel_doc::MessageView {
            id: String::new(),
            role: role.into(),
            created_at: 0,
            parts: Vec::new(),
            text,
            attachments: Vec::new(),
        })
    };
    let mut messages = vec![msg("user", "Build a diff viewer for the files pane".into())];
    for i in 0..10 {
        messages.push(msg("user", format!("step {i}")));
        messages.push(msg("assistant", format!("done {i}")));
    }
    messages.push(msg(
        "assistant",
        format!("{}Want me to add syntax colors next?", "x".repeat(5000)),
    ));
    let prompt = super::suggestion_prompt(&messages, "idle");
    assert!(prompt.contains("Want me to add syntax colors next?"));
    assert!(prompt.contains("Build a diff viewer for the files pane"));
    assert!(
        !prompt.contains("step 0"),
        "old turns beyond the window drop out"
    );
    assert_eq!(super::clip_ends("abcdef", 2, 2), "ab\n[…]\nef");
    assert_eq!(super::clip_ends("abc", 2, 2), "abc");
}

/// A finished show_chart call stores the chart on that message. The reply
/// text alone does not: there is no second model guessing after the turn.
#[tokio::test]
async fn a_ui_tool_call_stores_the_chart_on_its_message() {
    let tmp = Tmp::new("genui-earlier");
    let clock = Arc::new(ManualClock::new(5_000));
    let text = |message_id: &str, delta: &str| {
        Step::Emit(AgentEvent::Text {
            message_id: message_id.into(),
            part_id: "p".into(),
            delta: delta.into(),
            subagent: None,
        })
    };
    let spec = r#"{"component":"chart","kind":"bar","x":"crate","labels":["ui","engine"],"series":[{"name":"before (s)","values":[41.2,33.5]}]}"#;
    let harness = Arc::new(MockHarness::script(vec![
        text("m1", "Build times by crate."),
        Step::Emit(AgentEvent::ToolStarted {
            tool_id: "t1".into(),
            name: "mcp__keel-ui__show_chart".into(),
            input_bytes: 10,
            detail: String::new(),
            subagent: None,
            kind: None,
        }),
        Step::Emit(AgentEvent::ToolFinished {
            tool_id: "t1".into(),
            summary: "chart".into(),
            output_bytes: spec.len() as u64,
            is_error: false,
            images: Vec::new(),
            output_tail: spec.into(),
            subagent: None,
            diff: None,
            duration_ms: None,
        }),
        text("m2", "Done."),
        Step::Emit(AgentEvent::Done {
            status: TurnStatus::Completed {},
        }),
    ]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("go")).expect("start");
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("idle")
    })
    .await;
    let ui_on = |message_id: &str| {
        let mut state = engine.lock();
        let view = state.store.session(&id).expect("session").view();
        view.messages
            .iter()
            .filter(|m| m.id.ends_with(&format!(":{message_id}")))
            .flat_map(|m| m.parts.clone())
            .filter_map(|p| match p {
                PartView::Ui { id, .. } => Some(id),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(ui_on("m1"), ["chart"]);
    assert!(ui_on("m2").is_empty());
}

#[tokio::test]
async fn text_with_the_same_part_id_stays_in_its_own_message() {
    let tmp = Tmp::new("fold-per-message");
    let clock = Arc::new(ManualClock::new(5_000));
    let text = |message_id: &str, delta: &str| {
        Step::Emit(AgentEvent::Text {
            message_id: message_id.into(),
            part_id: "text-0".into(),
            delta: delta.into(),
            subagent: None,
        })
    };
    let harness = Arc::new(MockHarness::script(vec![
        text("m1", "first"),
        text("m2", "second"),
        Step::Emit(AgentEvent::Done {
            status: TurnStatus::Completed {},
        }),
    ]));
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let id = chat(&engine);
    engine.queue_command(&id, send("go")).expect("start");
    until(&engine, || {
        engine.chat_status(&id).as_deref() == Some("idle")
    })
    .await;
    let mut state = engine.lock();
    let view = state.store.session(&id).expect("session").view();
    let texts: Vec<&str> = view
        .messages
        .iter()
        .filter(|m| m.role == "assistant")
        .map(|m| m.text.as_str())
        .collect();
    assert_eq!(texts, ["first", "second"]);
}

#[test]
fn keel_is_the_only_harness() {
    let ids: Vec<&str> = super::cli_harnesses()
        .iter()
        .map(|h| super::harness_name(h.id()))
        .collect();
    assert_eq!(ids, ["keel"]);
    assert_eq!(super::cli_harnesses()[0].display_name(), "Keel");
}

#[test]
fn harness_ids_round_trip_and_old_ones_are_gone() {
    use HarnessId::*;
    for id in [Mock, Keel] {
        let name = super::harness_name(id);
        assert_eq!(super::parse_harness(name), Some(id), "{name}");
        assert_eq!(serde_json::to_value(id).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<HarnessId>(json!(name)).unwrap(),
            id
        );
    }
    for old in [
        "pi",
        "claude_code",
        "codex",
        "grok",
        "command_code",
        "antigravity",
    ] {
        assert_eq!(super::parse_harness(old), None, "{old}");
    }
}

#[test]
fn boot_drops_chats_of_removed_harnesses() {
    let tmp = Tmp::new("drop-old");
    let clock = Arc::new(ManualClock::new(5_000));
    let mut store = DocsStore::open(tmp.path().join("profile")).expect("store");
    put_chat(&mut store, DEVICE, "c-keep", DEVICE, 5_000, false, "idle");
    put_chat(&mut store, DEVICE, "c-old", DEVICE, 5_000, false, "idle");
    let mut row = store
        .registry()
        .view()
        .chats
        .into_iter()
        .find(|c| c.id == "c-old")
        .expect("row");
    row.harness = "codex".to_string();
    store.registry().put_chat(&row).expect("codex row");
    store.commit(5_000).expect("commit");
    drop(store);
    let engine = Engine::open(EngineConfig {
        store: DocsStore::open(tmp.path().join("profile")).expect("reopen"),
        device_id: DEVICE.to_string(),
        scope: "local".to_string(),
        clock,
        harnesses: vec![Arc::new(MockHarness::echo())],
        compaction_bytes: COMPACTION_BYTES,
    });
    let ids: Vec<String> = engine
        .lock()
        .store
        .registry()
        .view()
        .chats
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(ids, ["c-keep"]);
}

#[test]
fn memory_rpcs_write_list_prompt_and_refuse_bad_params() {
    assert_eq!(keel_harness::MEMORY_PROTO, keel_rpc::PROTO_VERSION, "the memory extension's hello");
    let tmp = Tmp::new("memory-rpc");
    let clock = Arc::new(ManualClock::new(5_000));
    let harness = Arc::new(MockHarness::silent());
    let engine = open_engine(&tmp, &clock, &harness, COMPACTION_BYTES);
    let (outbound, _rx) = Outbound::pair();
    let call = |method: &str, params: Value| -> Value {
        let request = RpcRequest { id: method.into(), method: method.into(), params };
        serde_json::from_str(&engine.handle(&request, &outbound).expect("sync reply")).unwrap()
    };
    let home = tmp.path().join("profile/harnesses/keel");

    let empty = call("memory_list", json!({}));
    assert_eq!(empty["result"]["user"]["notes"], json!([]), "{empty}");
    assert_eq!(empty["result"]["project"], Value::Null, "{empty}");
    assert!(!home.join("agent/memory").exists(), "a read creates nothing");

    let wrote = call(
        "memory_write",
        json!({ "scope": "user", "action": "upsert", "slug": "tabs", "type": "feedback",
                "description": "Indent with tabs", "body": "Kevin wants tabs." }),
    );
    assert_eq!(wrote["result"]["over_cap"], json!(false), "{wrote}");
    assert!(home.join("agent/memory/tabs.md").is_file());

    let list = call("memory_list", json!({}));
    assert_eq!(list["result"]["user"]["notes"][0]["slug"], json!("tabs"), "{list}");
    let prompt = call("memory_prompt", json!({}));
    let text = prompt["result"]["text"].as_str().unwrap();
    assert!(text.starts_with(keel_harness::MEMORY_PREAMBLE), "{text}");
    assert!(text.contains("- [tabs](tabs.md) — Indent with tabs"), "{text}");
    assert!(text.contains("This chat has no folder."), "{text}");
    let read = call("memory_read", json!({ "scope": "user", "slug": "tabs" }));
    assert_eq!(read["result"]["body"], json!("Kevin wants tabs."), "{read}");

    for (method, params) in [
        ("memory_write", json!({ "scope": "user", "action": "upsert", "slug": "../x", "type": "user", "description": "d" })),
        ("memory_write", json!({ "scope": "user", "action": "shred", "slug": "tabs" })),
        ("memory_write", json!({ "scope": "project", "action": "archive", "slug": "tabs" })),
        ("memory_read", json!({ "scope": "user", "cwd": "relative/dir" })),
        ("memory_read", json!({ "scope": "galaxy" })),
    ] {
        let reply = call(method, params.clone());
        assert_eq!(reply["error"]["code"], json!("bad_params"), "{params} -> {reply}");
    }

    let deleted = call("memory_delete", json!({ "scope": "user", "slug": "tabs" }));
    assert!(deleted.get("result").is_some(), "{deleted}");
    assert_eq!(call("memory_list", json!({}))["result"]["user"]["notes"], json!([]));
}

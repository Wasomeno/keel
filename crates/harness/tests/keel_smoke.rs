//! A real turn through the Keel harness, with the bundled Pi (or
//! `PI_EXECUTABLE`) and its default model. Needs that Pi signed in to a
//! provider, so it is ignored by default. Run with:
//! `cargo test -p keel-harness --test keel_smoke -- --ignored --nocapture`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use keel_harness::{
    Harness, KeelProfile, ModelId, PiHarness, RunControls, SessionReport, SessionSpec,
};
use keel_proto::{AgentEvent, HarnessId, PermissionMode, TurnStatus};

#[tokio::test]
#[ignore = "real pi turn; needs pi installed and signed in"]
async fn keel_harness_answers_a_real_read_only_prompt() {
    let keel = PiHarness::keel();
    assert_eq!(keel.id(), HarnessId::Keel);
    if !keel.installed() {
        eprintln!("pi not installed; skipping");
        return;
    }
    let root = std::env::temp_dir().join(format!("keel-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (data, work) = (root.join("data"), root.join("work"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let seen = Arc::new(Mutex::new(Vec::<(String, Option<String>)>::new()));
    let report: SessionReport = {
        let seen = Arc::clone(&seen);
        Arc::new(move |id, file| seen.lock().unwrap().push((id, file)))
    };
    let spec = SessionSpec {
        cwd: work.clone(),
        model: ModelId(String::new()), // Pi's default model.
        reasoning: None,
        resume: None,
        initial_text: "Reply with the word ok and nothing else. Do not use any tools.".into(),
        attachments: Vec::new(),
        report_session: Some(report),
        data_dir: Some(data.clone()),
        browser_sock: Some(data.join("browser.sock")),
        browser_chat: Some("smoke".into()),
        permission: PermissionMode::Ask,
        plan_mode: false,
        project: None,
        report_usage: None,
        chat_id: None,
        longer_cache: false,
        keep_cache_warm: true,
        idle_eviction_timeout_secs: None,
    };
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut stream = keel
        .run(spec, RunControls { inbound: rx })
        .await
        .expect("run");
    let (mut text, mut status, mut tools) = (String::new(), None, 0);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    while let Ok(Some(item)) = tokio::time::timeout_at(deadline, stream.next()).await {
        match item.expect("event") {
            AgentEvent::Text { delta, .. } => text.push_str(&delta),
            AgentEvent::ToolStarted { .. } => tools += 1,
            AgentEvent::Done { status: done } => {
                status = Some(done);
                break;
            }
            _ => {}
        }
    }
    eprintln!("keel smoke reply: {text:?}; status: {status:?}; tools: {tools}");
    assert!(
        matches!(status, Some(TurnStatus::Completed { .. })),
        "{status:?}"
    );
    assert!(text.to_lowercase().contains("ok"), "{text:?}");
    assert_eq!(tools, 0);
    // Keel's session file and generated extensions live in its own folders.
    let sessions = KeelProfile::sessions_dir(&data);
    let seen = seen.lock().unwrap().clone();
    eprintln!("reported sessions: {seen:?}");
    assert!(
        seen.iter().any(|(_, file)| file
            .as_deref()
            .is_some_and(|f| f.starts_with(&*sessions.to_string_lossy()))),
        "{seen:?}"
    );
    assert!(data.join("browser-mcp/keel-smoke.ts").is_file());
    assert!(data.join("keel-permission-ask.keel.ts").is_file());
    assert!(!data.join("browser-mcp/pi-smoke.ts").exists());
    let _ = std::fs::remove_dir_all(&root);
}

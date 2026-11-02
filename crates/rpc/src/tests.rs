//! Startup, token, and envelope tests. Temps stay off the user data directory.

use std::fs;
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};

use crate::boot::{BootConfig, Identity, Mode, Outcome};
use crate::codec::encode_frame;
use crate::dispatch::{engine_status_request, hello_request};
use crate::lock::{self, EngineLock, LockResult};
use crate::token::ensure_token;
use crate::{DEFAULT_IPC_PORT, MAX_FRAME_BYTES, boot, status_report};

const DEVICE: &str = "22222222-2222-2222-2222-222222222222";
const SCRIPTED_DEVICE: &str = "11111111-1111-1111-1111-111111111111";
const HELLO_OK: &str = r#"{"id":"c1","result":{"device_id":"22222222-2222-2222-2222-222222222222","scope":"local","proto":1,"build":0}}"#;
const STATUS_OK: &str =
    r#"{"id":"c2","result":{"scope":"local","lock":true,"live_runs":0,"last_watchdog":null}}"#;

struct Tmp {
    path: PathBuf,
}

impl Tmp {
    fn new(label: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("keel-rpc-{label}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
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

fn identity() -> Identity {
    Identity {
        device_id: DEVICE.into(),
        scope: "local".into(),
    }
}

fn config(dir: &Path, port: u16, mode: Mode, hello_ms: u64, retry_ms: u64) -> BootConfig {
    BootConfig {
        data_dir: dir.to_path_buf(),
        port,
        mode,
        hello_budget: Duration::from_millis(hello_ms),
        retry_for: Duration::from_millis(retry_ms),
    }
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn hold_lock(dir: &Path) -> EngineLock {
    match lock::try_lock(&dir.join("engine.lock")).unwrap() {
        LockResult::Acquired(held) => held,
        LockResult::Held => panic!("lock already held"),
    }
}

struct Peer {
    port: u16,
    accepts: Arc<AtomicUsize>,
    shutdown: watch::Sender<bool>,
    _runtime: tokio::runtime::Runtime,
}

impl Peer {
    fn accepts(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

fn scripted_peer(answer_on: usize) -> Peer {
    let accepts = Arc::new(AtomicUsize::new(0));
    let (shutdown, shutdown_rx) = watch::channel(false);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("keel-rpc-test")
        .enable_all()
        .build()
        .unwrap();
    let accepts_task = Arc::clone(&accepts);
    let port = runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut shutdown_rx = shutdown_rx;
            loop {
                let accepted = tokio::select! {
                    _ = shutdown_rx.changed() => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((sock, _)) = accepted else {
                    break;
                };
                let n = accepts_task.fetch_add(1, Ordering::SeqCst) + 1;
                let answer = answer_on != 0 && n == answer_on;
                tokio::spawn(scripted_conn(sock, answer));
            }
        });
        port
    });
    Peer {
        port,
        accepts,
        shutdown,
        _runtime: runtime,
    }
}

async fn scripted_conn(stream: TcpStream, answer: bool) {
    let Ok(mut socket) = accept_async(stream).await else {
        return;
    };
    let Some(Ok(Message::Text(text))) = socket.next().await else {
        return;
    };
    if !answer {
        let _ = socket.next().await;
        return;
    }
    let id = serde_json::from_str::<keel_proto::RpcRequest>(&text)
        .map(|request| request.id)
        .unwrap_or_else(|_| "c1".into());
    let reply = serde_json::to_string(&ScriptedHello {
        id: &id,
        result: ScriptedResult {
            device_id: SCRIPTED_DEVICE,
            scope: "local",
            proto: 1,
        },
    })
    .unwrap();
    let _ = socket.send(Message::Text(reply)).await;
    let _ = socket.next().await;
}

#[derive(serde::Serialize)]
struct ScriptedHello<'a> {
    id: &'a str,
    result: ScriptedResult<'a>,
}

#[derive(serde::Serialize)]
struct ScriptedResult<'a> {
    device_id: &'a str,
    scope: &'a str,
    proto: u32,
}

fn boot_with(
    dir: &Path,
    port: u16,
    mode: Mode,
    hello_ms: u64,
    retry_ms: u64,
) -> (Arc<AtomicBool>, Result<Outcome, crate::BootError>) {
    let opened = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&opened);
    let locked = dir.to_path_buf();
    let outcome = boot(&config(dir, port, mode, hello_ms, retry_ms), move || {
        let held = matches!(
            lock::try_lock(&locked.join("engine.lock")).unwrap(),
            LockResult::Held
        );
        assert!(held, "store callback ran without the lock");
        flag.store(true, Ordering::SeqCst);
        Ok(identity())
    });
    (opened, outcome)
}

fn ws_texts(guard: &crate::ServerGuard, port: u16, requests: &[&str]) -> Vec<String> {
    guard.block_on(async {
        let url = format!("ws://127.0.0.1:{port}");
        let (mut socket, _) = connect_async(&url).await.unwrap();
        let mut bodies = Vec::new();
        for request in requests {
            socket
                .send(Message::Text((*request).to_string()))
                .await
                .unwrap();
            match socket.next().await {
                Some(Ok(Message::Text(body))) => bodies.push(body),
                other => panic!("expected text, got {other:?}"),
            }
        }
        bodies
    })
}

fn frame_body(raw: &[u8]) -> &str {
    let len = u32::from_be_bytes(raw[..4].try_into().unwrap()) as usize;
    assert_eq!(raw.len(), 4 + len);
    std::str::from_utf8(&raw[4..]).unwrap()
}

#[test]
fn default_port_is_the_loopback_port() {
    assert_eq!(DEFAULT_IPC_PORT, 47821);
}

#[test]
fn duplex_length_prefix_rejects_an_oversize_body() {
    let json = br#"{"id":"c1"}"#;
    let frame = encode_frame(json).unwrap();
    assert_eq!(&frame[..4], &(json.len() as u32).to_be_bytes());
    assert_eq!(&frame[4..], json);
    let huge = vec![b'x'; MAX_FRAME_BYTES + 1];
    assert!(encode_frame(&huge).is_err());
}

#[test]
fn token_file_is_mode_0600_and_stable() {
    let dir = Tmp::new("token");
    let first = ensure_token(dir.path()).unwrap();
    assert_eq!(first.len(), 64);
    assert!(
        first
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );
    let mode = fs::metadata(dir.path().join("ipc.token"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let bytes = fs::read(dir.path().join("ipc.token")).unwrap();
    assert_eq!(ensure_token(dir.path()).unwrap(), first);
    assert_eq!(fs::read(dir.path().join("ipc.token")).unwrap(), bytes);
}

#[test]
fn flock_loser_returns_without_waiting() {
    let dir = Tmp::new("flock");
    let _held = hold_lock(dir.path());
    let started = Instant::now();
    let again = lock::try_lock(&dir.path().join("engine.lock")).unwrap();
    assert!(matches!(again, LockResult::Held));
    assert!(started.elapsed() < Duration::from_millis(200));
}

#[test]
fn in_process_frame_matches_websocket_json() {
    let dir = Tmp::new("frame");
    let port = free_port();
    let (opened, outcome) = boot_with(dir.path(), port, Mode::Headed, 40, 80);
    let Outcome::Engine(guard) = outcome.unwrap() else {
        panic!("expected the lock holder");
    };
    assert!(opened.load(Ordering::SeqCst));
    let name = guard.block_on(async {
        tokio::spawn(async { std::thread::current().name().unwrap_or("").to_string() })
            .await
            .unwrap()
    });
    assert!(name.starts_with("keel-engine"), "{name}");
    let token = fs::read_to_string(dir.path().join("ipc.token")).unwrap();
    let ws = ws_texts(&guard, port, &[&hello_request("c1", token.trim())]);
    let raw = guard.duplex_exchange(&hello_request("c1", "")).unwrap();
    assert_eq!(frame_body(&raw), ws[0]);
    assert_eq!(ws[0], HELLO_OK);
    let status = ws_texts(
        &guard,
        port,
        &[
            &hello_request("c1", token.trim()),
            &engine_status_request("c2"),
        ],
    );
    let raw_status = guard.duplex_exchange(&engine_status_request("c2")).unwrap();
    assert_eq!(frame_body(&raw_status), status[1]);
    assert_eq!(status[1], STATUS_OK);
}

#[test]
fn bad_token_closes_and_a_later_hello_still_works() {
    let dir = Tmp::new("token-bad");
    let port = free_port();
    let (_opened, outcome) = boot_with(dir.path(), port, Mode::Headed, 40, 80);
    let Outcome::Engine(guard) = outcome.unwrap() else {
        panic!("expected the lock holder");
    };
    let token = fs::read_to_string(dir.path().join("ipc.token")).unwrap();
    guard.block_on(async {
        let url = format!("ws://127.0.0.1:{port}");
        for presented in ["00", ""] {
            let (mut socket, _) = connect_async(&url).await.unwrap();
            socket
                .send(Message::Text(hello_request("c1", presented)))
                .await
                .unwrap();
            let Some(Ok(Message::Text(body))) = socket.next().await else {
                panic!("missing bad token");
            };
            assert_eq!(
                body,
                r#"{"id":"c1","error":{"code":"bad_token","message":"bad token"}}"#
            );
            match socket.next().await {
                None | Some(Ok(Message::Close(_))) | Some(Err(_)) => {}
                Some(Ok(other)) => panic!("socket stayed open: {other:?}"),
            }
        }
    });
    let ok = ws_texts(&guard, port, &[&hello_request("c1", token.trim())]);
    assert_eq!(ok[0], HELLO_OK);
    let raw = guard.duplex_exchange(&hello_request("c1", "abcd")).unwrap();
    assert_eq!(
        frame_body(&raw),
        r#"{"id":"c1","error":{"code":"bad_token","message":"bad token"}}"#
    );
    assert!(guard.duplex_exchange(&engine_status_request("c2")).is_err());
}

#[test]
fn unknown_method_stays_connected() {
    let dir = Tmp::new("unknown");
    let port = free_port();
    let (_opened, outcome) = boot_with(dir.path(), port, Mode::Headed, 40, 80);
    let Outcome::Engine(guard) = outcome.unwrap() else {
        panic!("expected the lock holder");
    };
    let token = fs::read_to_string(dir.path().join("ipc.token")).unwrap();
    let bodies = ws_texts(
        &guard,
        port,
        &[
            &hello_request("c1", token.trim()),
            r#"{"id":"c2","method":"list_harnesses","params":{}}"#,
            &engine_status_request("c3"),
            &hello_request("c4", token.trim()),
        ],
    );
    assert_eq!(bodies[0], HELLO_OK);
    assert_eq!(
        bodies[1],
        r#"{"id":"c2","error":{"code":"unknown_method","message":"no such method"}}"#
    );
    assert_eq!(
        bodies[2],
        r#"{"id":"c3","result":{"scope":"local","lock":true,"live_runs":0,"last_watchdog":null}}"#
    );
    assert!(bodies[3].contains(r#""proto":1"#));
    let early = ws_texts(&guard, port, &[&engine_status_request("c9")]);
    assert_eq!(
        early[0],
        r#"{"id":"c9","error":{"code":"bad_token","message":"bad token"}}"#
    );
}

#[test]
fn headless_hello_ok_does_not_flock() {
    let dir = Tmp::new("already");
    let peer = scripted_peer(1);
    let (opened, outcome) = boot_with(dir.path(), peer.port, Mode::Headless, 200, 5_000);
    assert!(matches!(outcome, Ok(Outcome::EngineAlreadyRunning)));
    assert!(!opened.load(Ordering::SeqCst));
    assert!(!dir.path().join("engine.lock").exists());
    assert!(!dir.path().join("ipc.token").exists());
    assert_eq!(peer.accepts(), 1);
}

#[test]
fn headless_lock_held_does_not_retry() {
    let dir = Tmp::new("headless-held");
    let peer = scripted_peer(0);
    let _held = hold_lock(dir.path());
    let started = Instant::now();
    let (opened, outcome) = boot_with(dir.path(), peer.port, Mode::Headless, 60, 5_000);
    let elapsed = started.elapsed();
    assert!(matches!(outcome, Ok(Outcome::EngineLockHeld)));
    assert!(!opened.load(Ordering::SeqCst));
    assert_eq!(peer.accepts(), 1);
    assert!(elapsed < Duration::from_millis(1_000), "{elapsed:?}");
    assert!(!dir.path().join("ipc.token").exists());
}

#[test]
fn headed_lock_held_attaches_on_the_second_hello() {
    let dir = Tmp::new("headed-attach");
    let peer = scripted_peer(2);
    let _held = hold_lock(dir.path());
    let (opened, outcome) = boot_with(dir.path(), peer.port, Mode::Headed, 80, 1_500);
    match outcome {
        Ok(Outcome::Attach(hello)) => {
            assert_eq!(hello.device_id, SCRIPTED_DEVICE);
            assert_eq!(hello.scope, "local");
            assert_eq!(hello.proto, 1);
        }
        other => panic!("expected attach, got {}", outcome_name(&other)),
    }
    assert!(!opened.load(Ordering::SeqCst));
    assert_eq!(peer.accepts(), 2);
    assert!(!dir.path().join("ipc.token").exists());
}

#[test]
fn headed_deadline_exits_when_the_peer_stays_silent() {
    let dir = Tmp::new("headed-deadline");
    let peer = scripted_peer(0);
    let _held = hold_lock(dir.path());
    let started = Instant::now();
    let (opened, outcome) = boot_with(dir.path(), peer.port, Mode::Headed, 50, 200);
    let elapsed = started.elapsed();
    assert!(matches!(outcome, Ok(Outcome::EngineLockHeld)));
    assert!(!opened.load(Ordering::SeqCst));
    assert!(peer.accepts() >= 2, "accepts {}", peer.accepts());
    assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    assert!(!dir.path().join("ipc.token").exists());
}

#[test]
fn headed_bind_failure_still_serves_the_duplex() {
    let dir = Tmp::new("headed-bind");
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (opened, outcome) = boot_with(dir.path(), port, Mode::Headed, 40, 80);
    let Outcome::Engine(guard) = outcome.unwrap() else {
        panic!("headed bind failure still returns the engine");
    };
    assert!(opened.load(Ordering::SeqCst));
    let raw = guard.duplex_exchange(&hello_request("c1", "")).unwrap();
    assert_eq!(frame_body(&raw), HELLO_OK);
    drop(listener);
}

#[test]
fn headless_bind_failure_is_fatal_after_the_store_opens() {
    let dir = Tmp::new("headless-bind");
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (opened, outcome) = boot_with(dir.path(), port, Mode::Headless, 40, 80);
    assert!(matches!(outcome, Ok(Outcome::BindFailed)));
    assert!(opened.load(Ordering::SeqCst));
    match lock::try_lock(&dir.path().join("engine.lock")).unwrap() {
        LockResult::Acquired(_) => {}
        LockResult::Held => panic!("headless bind failure kept the lock"),
    }
    drop(listener);
}

#[test]
fn status_prints_local_when_the_engine_is_down() {
    let dir = Tmp::new("status-down");
    let port = free_port();
    let text = status_report(dir.path(), port, Duration::from_millis(40));
    assert_eq!(text, "scope: local\n");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    // A session.json left by an older sync-capable build does not change it.
    fs::write(
        dir.path().join("session.json"),
        r#"{"scope":"synced","owner_id":"00000000-0000-0000-0000-000000000001","device_id":"00000000-0000-0000-0000-000000000002"}"#,
    )
    .unwrap();
    let text = status_report(dir.path(), port, Duration::from_millis(40));
    assert_eq!(text, "scope: local\n");
    assert!(!dir.path().join("ipc.token").exists());
    assert!(!dir.path().join("device-id").exists());
}

#[test]
fn status_prints_the_live_engine() {
    let dir = Tmp::new("status-up");
    let port = free_port();
    let (_opened, outcome) = boot_with(dir.path(), port, Mode::Headed, 40, 80);
    let Outcome::Engine(_guard) = outcome.unwrap() else {
        panic!("expected the lock holder");
    };
    let text = status_report(dir.path(), port, Duration::from_millis(500));
    assert_eq!(
        text,
        format!("scope: local\ndevice_id: {DEVICE}\nlive_runs: 0\n")
    );
}

#[cfg(unix)]
#[test]
fn unix_socket_binds_mode_0600_and_authenticates_with_token() {
    use std::os::unix::fs::PermissionsExt;
    let short_dir = std::path::PathBuf::from(format!("/tmp/keel-sock-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&short_dir);
    fs::create_dir_all(&short_dir).unwrap();
    let port = free_port();
    let (_opened, outcome) = boot_with(&short_dir, port, Mode::Headed, 40, 80);
    let Outcome::Engine(guard) = outcome.unwrap() else {
        panic!("expected engine");
    };

    let sock_path = short_dir.join("keel.sock");
    assert!(sock_path.exists());
    let perms = fs::metadata(&sock_path).unwrap().permissions();
    assert_eq!(perms.mode() & 0o777, 0o600);

    let token = fs::read_to_string(short_dir.join("ipc.token")).unwrap();

    guard.block_on(async {
        // Connect via length-prefixed Unix stream
        let mut stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let hello = hello_request("c1", token.trim());
        crate::codec::write_frame(&mut stream, hello.as_bytes())
            .await
            .unwrap();
        let reply = crate::codec::read_frame(&mut stream).await.unwrap();
        let reply_str = std::str::from_utf8(&reply).unwrap();
        assert_eq!(reply_str, HELLO_OK);

        // Test bad token
        let mut bad_stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let bad_hello = hello_request("c2", "wrong-token");
        crate::codec::write_frame(&mut bad_stream, bad_hello.as_bytes())
            .await
            .unwrap();
        let reply_bad = crate::codec::read_frame(&mut bad_stream).await.unwrap();
        let reply_bad_str = std::str::from_utf8(&reply_bad).unwrap();
        assert!(reply_bad_str.contains("bad_token"));
    });

    drop(guard);
    // After dropping the guard, keel.sock is removed
    assert!(!sock_path.exists());
    let _ = fs::remove_dir_all(&short_dir);
}

fn outcome_name(outcome: &Result<Outcome, crate::BootError>) -> &'static str {
    match outcome {
        Ok(Outcome::Attach(_)) => "attach",
        Ok(Outcome::Engine(_)) => "engine",
        Ok(Outcome::EngineAlreadyRunning) => "already",
        Ok(Outcome::EngineLockHeld) => "held",
        Ok(Outcome::BindFailed) => "bind",
        Err(_) => "error",
    }
}

// ---- Transport robustness: cancel-safe reads, client failure paths. ----

const SPAM_ITEM: &str = r#"{"stream":"s1","event":{"type":"live","agent_event":{"type":"text","message_id":"m","part_id":"p","delta":"hi"}}}"#;
const BIG: usize = 300 * 1024;

/// `spam` starts a stream of pushes; `echo` replies with the length of `params.text`.
struct SpamHost;

impl crate::EngineHost for SpamHost {
    fn live_runs(&self) -> u64 {
        0
    }

    fn last_watchdog(&self) -> Option<String> {
        None
    }

    fn handle(
        &self,
        request: &keel_proto::RpcRequest,
        outbound: &crate::Outbound,
    ) -> Option<String> {
        match request.method.as_str() {
            "spam" => {
                let out = outbound.clone();
                tokio::spawn(async move {
                    for _ in 0..3000 {
                        out.push(SPAM_ITEM);
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                });
                Some(serde_json::json!({"id": request.id, "result": {}}).to_string())
            }
            "echo" => {
                let len = request.params["text"].as_str().map_or(0, str::len);
                Some(serde_json::json!({"id": request.id, "result": {"len": len}}).to_string())
            }
            _ => None,
        }
    }

    fn supervise(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(std::future::pending())
    }
}

fn spam_state() -> Arc<crate::serve::EngineState> {
    let host: Arc<dyn crate::EngineHost> = Arc::new(SpamHost);
    Arc::new(crate::serve::EngineState {
        identity: identity(),
        token: "t".repeat(64),
        host: std::sync::RwLock::new(Some(host)),
    })
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn duplex_large_frame_survives_streaming_pushes() {
    rt().block_on(async {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (_stop, stop_rx) = watch::channel(false);
        tokio::spawn(crate::serve::serve_duplex(server_io, spam_state(), stop_rx));
        let client = crate::RpcClient::from_duplex(client_io).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        client.set_stream_sender(tx);
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        });
        client.call("spam", serde_json::json!({})).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        for _ in 0..5 {
            let text = "x".repeat(BIG);
            let res = tokio::time::timeout(
                Duration::from_secs(5),
                client.call("echo", serde_json::json!({ "text": text })),
            )
            .await
            .expect("echo hung")
            .expect("echo failed");
            assert_eq!(res["len"], BIG);
        }
        assert!(seen.load(Ordering::SeqCst) > 0, "pushes never arrived");
    });
}

#[test]
fn client_rejects_oversize_call_and_keeps_working() {
    rt().block_on(async {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (_stop, stop_rx) = watch::channel(false);
        tokio::spawn(crate::serve::serve_duplex(server_io, spam_state(), stop_rx));
        let client = crate::RpcClient::from_duplex(client_io).await.unwrap();
        let err = client
            .call(
                "echo",
                serde_json::json!({ "text": "x".repeat(MAX_FRAME_BYTES + 1) }),
            )
            .await
            .unwrap_err();
        assert!(err.contains("payload_too_large"), "{err}");
        let res = client
            .call("echo", serde_json::json!({ "text": "abc" }))
            .await
            .unwrap();
        assert_eq!(res["len"], 3);
    });
}

#[test]
fn client_calls_fail_instead_of_hanging_after_disconnect() {
    rt().block_on(async {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (stop, stop_rx) = watch::channel(false);
        tokio::spawn(crate::serve::serve_duplex(server_io, spam_state(), stop_rx));
        let client = crate::RpcClient::from_duplex(client_io).await.unwrap();
        stop.send(true).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let res = tokio::time::timeout(
            Duration::from_secs(2),
            client.call("echo", serde_json::json!({ "text": "abc" })),
        )
        .await
        .expect("call hung after disconnect");
        assert!(res.is_err());
    });
}

/// Write a frame in small chunks so the server sees it across many reads.
#[cfg(unix)]
async fn send(wr: &mut tokio::net::unix::OwnedWriteHalf, body: String) {
    use tokio::io::AsyncWriteExt;
    let frame = encode_frame(body.as_bytes()).unwrap();
    for chunk in frame.chunks(16 * 1024) {
        wr.write_all(chunk).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[cfg(unix)]
#[test]
fn unix_large_frame_survives_streaming_pushes() {
    let dir = PathBuf::from(format!("/tmp/keel-sock-big-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let port = free_port();
    let (_opened, outcome) = boot_with(&dir, port, Mode::Headed, 40, 80);
    let Outcome::Engine(guard) = outcome.unwrap() else {
        panic!("expected engine");
    };
    guard.install_host(Arc::new(SpamHost));
    let token = fs::read_to_string(dir.join("ipc.token")).unwrap();
    guard.block_on(async {
        let stream = tokio::net::UnixStream::connect(dir.join("keel.sock")).await.unwrap();
        let (mut rd, mut wr) = stream.into_split();
        let (ids_tx, mut ids_rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
        tokio::spawn(async move {
            let mut frames = crate::codec::FrameReader::default();
            while let Ok(body) = frames.next(&mut rd).await {
                let val: serde_json::Value = serde_json::from_slice(&body).expect("corrupt frame");
                if val.get("id").is_some() {
                    let _ = ids_tx.send(val);
                }
            }
        });
        send(&mut wr, hello_request("c1", token.trim())).await;
        assert_eq!(ids_rx.recv().await.unwrap()["id"], "c1");
        send(&mut wr, r#"{"id":"c2","method":"spam","params":{}}"#.into()).await;
        assert_eq!(ids_rx.recv().await.unwrap()["id"], "c2");
        for n in 0..3 {
            let id = format!("e{n}");
            let body = serde_json::json!({"id": id, "method": "echo", "params": {"text": "y".repeat(BIG)}});
            send(&mut wr, body.to_string()).await;
            let reply = tokio::time::timeout(Duration::from_secs(5), ids_rx.recv())
                .await
                .expect("echo hung")
                .unwrap();
            assert_eq!(reply["id"], id.as_str());
            assert_eq!(reply["result"]["len"], BIG);
        }
    });
    drop(guard);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn existing_token_with_loose_mode_is_reset_to_0600() {
    let dir = Tmp::new("token-mode");
    let path = dir.path().join("ipc.token");
    fs::write(&path, "abc\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(ensure_token(dir.path()).unwrap(), "abc");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::write(&path, "").unwrap();
    let fresh = ensure_token(dir.path()).unwrap();
    assert_eq!(fresh.len(), 64);
}

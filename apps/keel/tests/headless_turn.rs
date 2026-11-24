//! One headless process, one mock turn, one in-process Keel harness turn
//! backed by a local OpenAI-compatible SSE mock server.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

struct Running {
    child: Child,
    reaped: bool,
    out: Arc<Mutex<String>>,
    err: Arc<Mutex<String>>,
}

impl Running {
    fn logs(&self) -> String {
        format!(
            "stdout:\n{}\nstderr:\n{}",
            self.out.lock().expect("stdout"),
            self.err.lock().expect("stderr")
        )
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn drain(mut pipe: impl Read + Send + 'static) -> (thread::JoinHandle<()>, Arc<Mutex<String>>) {
    let buf = Arc::new(Mutex::new(String::new()));
    let out = Arc::clone(&buf);
    let handle = thread::spawn(move || {
        let mut tmp = [0u8; 1024];
        let mut stored = 0usize;
        loop {
            match pipe.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stored < 16 * 1024 {
                        let take = n.min(16 * 1024 - stored);
                        out.lock()
                            .expect("log")
                            .push_str(&String::from_utf8_lossy(&tmp[..take]));
                        stored += take;
                    }
                }
            }
        }
    });
    (handle, buf)
}

fn free_port() -> u16 {
    loop {
        let listener =
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("port");
        let port = listener.local_addr().expect("addr").port();
        if port != 47821 {
            return port;
        }
    }
}

async fn start_mock_openai_sse_server() -> (u16, Arc<Mutex<Vec<(String, String)>>>) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind mock llm");
    let port = listener.local_addr().expect("mock addr").port();
    let captured = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let captured_ref = Arc::clone(&captured);

    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let captured = Arc::clone(&captured_ref);
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut req_line = String::new();
                if reader.read_line(&mut req_line).await.unwrap_or(0) == 0 {
                    return;
                }
                let mut content_length = 0usize;
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        break;
                    }
                    let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some((name, val)) = trimmed.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        content_length = val.trim().parse::<usize>().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; content_length];
                if content_length > 0 {
                    let _ = reader.read_exact(&mut body).await;
                }
                captured.lock().expect("captured").push((
                    req_line.trim().to_string(),
                    String::from_utf8_lossy(&body).into_owned(),
                ));

                let sse = concat!(
                    "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4.1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"fixture-ok\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4.1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}\n\n",
                    "data: [DONE]\n\n",
                );
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                    sse.len()
                );
                let _ = write_half.write_all(header.as_bytes()).await;
                let _ = write_half.write_all(sse.as_bytes()).await;
                let _ = write_half.flush().await;
            });
        }
    });

    (port, captured)
}

fn hello(token: &str) -> String {
    format!(r#"{{"id":"c1","method":"hello","params":{{"token":"{token}","proto":1}}}}"#)
}

struct Rpc {
    write: futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    frames: Arc<Mutex<Vec<String>>>,
}

impl Rpc {
    async fn request(&mut self, id: &str, payload: String, logs: &str) -> Value {
        self.write.send(Message::Text(payload)).await.expect("send");
        let needle = format!("\"id\":\"{id}\"");
        let start = Instant::now();
        loop {
            let found = self
                .frames
                .lock()
                .expect("frames")
                .iter()
                .any(|frame| frame.contains(&needle));
            if found {
                sleep(Duration::from_millis(40)).await;
                let frames = self.frames.lock().expect("frames");
                let frame = frames
                    .iter()
                    .find(|frame| frame.contains(&needle))
                    .expect("frame")
                    .clone();
                return serde_json::from_str(&frame)
                    .unwrap_or_else(|err| panic!("{err}: {frame}\n{logs}"));
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!(
                    "timeout waiting for {id}\n{logs}\n{:?}",
                    self.frames.lock().expect("frames")
                );
            }
            sleep(Duration::from_millis(20)).await;
        }
    }

    fn blob(&self) -> String {
        self.frames.lock().expect("frames").join("\n")
    }
}

fn occurrences(blob: &str, needle: &str) -> usize {
    blob.matches(needle).count()
}

#[tokio::test]
async fn headless_completes_a_mock_turn_and_a_keel_turn() {
    let root = std::env::temp_dir().join(format!(
        "keel-headless-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("work")).expect("work");
    let data = root.join("data");
    fs::create_dir_all(&data).expect("data");

    let (llm_port, captured_llm) = start_mock_openai_sse_server().await;
    let port = free_port();
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_keel"));
    let mut child = Command::new(&exe)
        .arg("headless")
        .arg("-d")
        .arg(&data)
        .env("KEEL_IPC_PORT", port.to_string())
        .env("KEEL_MOCK_HARNESS", "1")
        .env("KEEL_OPENAI_BASE_URL", format!("http://127.0.0.1:{llm_port}/v1"))
        .env("OPENAI_API_KEY", "sk-test-fixture")
        .env_remove("KEEL_DEV_BEARER")
        .env_remove("KEEL_DATA_DIR")
        .env_remove("KEEL_BROWSER_MCP_EXE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn keel");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");
    let (out_thread, out) = drain(stdout);
    let (err_thread, err) = drain(stderr);
    let mut running = Running {
        child,
        reaped: false,
        out,
        err,
    };

    let started = Instant::now();
    let token = loop {
        if let Some(status) = running.child.try_wait().expect("wait") {
            panic!("keel exited {status}\n{}", running.logs());
        }
        if let Ok(text) = fs::read_to_string(data.join("ipc.token")) {
            let text = text.trim().to_string();
            if text.len() == 64 {
                break text;
            }
        }
        if started.elapsed() > Duration::from_secs(25) {
            panic!("ipc.token missing\n{}", running.logs());
        }
        sleep(Duration::from_millis(50)).await;
    };

    let url = format!("ws://127.0.0.1:{port}");
    let socket = loop {
        match connect_async(&url).await {
            Ok((socket, _)) => break socket,
            Err(err) => {
                if started.elapsed() > Duration::from_secs(25) {
                    panic!("connect {url}: {err}\n{}", running.logs());
                }
                sleep(Duration::from_millis(50)).await;
            }
        }
    };
    let (write, mut read) = socket.split();
    let frames = Arc::new(Mutex::new(Vec::<String>::new()));
    let incoming = Arc::clone(&frames);
    tokio::spawn(async move {
        while let Some(message) = read.next().await {
            match message {
                Ok(Message::Text(text)) => incoming.lock().expect("frames").push(text.to_string()),
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });
    let mut rpc = Rpc { write, frames };

    let hello_body = rpc.request("c1", hello(&token), &running.logs()).await;
    assert_eq!(hello_body["result"]["proto"], 1, "{hello_body}");

    // The server binds before `Engine::local` finishes discovery and installs the host.
    let host_wait = Instant::now();
    let mut attempt = 0u32;
    let listed = loop {
        attempt += 1;
        let id = format!("h{attempt}");
        let listed = rpc
            .request(
                &id,
                json!({"id": id, "method": "list_harnesses", "params": {}}).to_string(),
                &running.logs(),
            )
            .await;
        if listed["result"]["harnesses"].is_array() {
            break listed;
        }
        if host_wait.elapsed() > Duration::from_secs(20) {
            panic!("host not installed: {listed}\n{}", running.logs());
        }
        sleep(Duration::from_millis(50)).await;
    };
    let harnesses = listed["result"]["harnesses"]
        .as_array()
        .unwrap_or_else(|| panic!("{listed}"));
    let keel = harnesses
        .iter()
        .find(|row| row["id"] == "keel")
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(keel["installed"], true, "{listed}");
    assert_eq!(keel["version"], env!("CARGO_PKG_VERSION"), "{listed}");
    assert_eq!(keel["display_name"], "Keel");
    let mock = harnesses
        .iter()
        .find(|row| row["id"] == "mock")
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(mock["installed"], true, "{listed}");
    assert_eq!(mock["version"], "0");

    let mock_chat = rpc
        .request(
            "m1",
            json!({
                "id": "m1",
                "method": "create_chat",
                "params": {"harness": "mock", "model": "mock", "cwd": "/tmp", "title": "Mock"}
            })
            .to_string(),
            &running.logs(),
        )
        .await;
    let mock_id = mock_chat["result"]["chat_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{mock_chat}"))
        .to_string();
    let _subscribed = rpc
        .request(
            "s1",
            json!({"id": "s1", "method": "subscribe_chat", "params": {"chat_id": mock_id}})
                .to_string(),
            &running.logs(),
        )
        .await;
    let queued = rpc
        .request(
            "q1",
            json!({
                "id": "q1",
                "method": "queue_command",
                "params": {
                    "chat_id": mock_id,
                    "command": {
                        "type": "send",
                        "command_id": "11111111-1111-1111-1111-111111111111",
                        "message_id": "u1",
                        "text": "hello"
                    }
                }
            })
            .to_string(),
            &running.logs(),
        )
        .await;
    assert_eq!(queued["result"]["status"], "processed", "{queued}");
    wait_turn(&rpc, "\"delta\":\"ok\"", 0, &running).await;

    let work = root.join("work");
    let keel_chat = rpc
        .request(
            "m2",
            json!({
                "id": "m2",
                "method": "create_chat",
                "params": {
                    "harness": "keel",
                    "model": "openai/gpt-4.1",
                    "cwd": work.to_string_lossy(),
                    "title": "Keel"
                }
            })
            .to_string(),
            &running.logs(),
        )
        .await;
    let keel_id = keel_chat["result"]["chat_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{keel_chat}"))
        .to_string();
    let _subscribed = rpc
        .request(
            "s2",
            json!({"id": "s2", "method": "subscribe_chat", "params": {"chat_id": keel_id}})
                .to_string(),
            &running.logs(),
        )
        .await;
    let before_done = occurrences(&rpc.blob(), "\"type\":\"completed\"");
    let queued = rpc
        .request(
            "q2",
            json!({
                "id": "q2",
                "method": "queue_command",
                "params": {
                    "chat_id": keel_id,
                    "command": {
                        "type": "send",
                        "command_id": "22222222-2222-2222-2222-222222222222",
                        "message_id": "u2",
                        "text": "hello"
                    }
                }
            })
            .to_string(),
            &running.logs(),
        )
        .await;
    assert_eq!(queued["result"]["status"], "processed", "{queued}");
    wait_turn(&rpc, "\"delta\":\"fixture-ok\"", before_done, &running).await;

    let db = data.join("profiles/local/store.sqlite");
    let sessions_prefix = data.join("harnesses/keel/sessions");
    let session_file = sqlite_native(&db, &sessions_prefix);
    assert!(
        Path::new(&session_file).is_file(),
        "expected session file on disk: {session_file}\n{}",
        running.logs()
    );
    let log_text = fs::read_to_string(&session_file).unwrap_or_default();
    assert!(log_text.contains("fixture-ok"), "{log_text}");

    let reqs = captured_llm.lock().expect("reqs");
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    assert!(reqs[0].0.starts_with("POST /v1/chat/completions"), "{reqs:?}");
    assert!(reqs[0].1.contains("\"model\":\"gpt-4.1\""), "{}", reqs[0].1);
    assert!(reqs[0].1.contains("hello"), "{}", reqs[0].1);

    let _ = Command::new("kill")
        .args(["-INT", &running.child.id().to_string()])
        .status();
    let exit_at = Instant::now();
    let status = loop {
        if let Some(status) = running.child.try_wait().expect("wait") {
            break status;
        }
        if exit_at.elapsed() > Duration::from_secs(10) {
            panic!("keel did not exit\n{}", running.logs());
        }
        sleep(Duration::from_millis(50)).await;
    };
    running.reaped = true;
    let _ = out_thread.join();
    let _ = err_thread.join();
    assert!(status.success(), "{status}\n{}", running.logs());
    let _ = fs::remove_dir_all(&root);
}

async fn wait_turn(rpc: &Rpc, delta: &str, done_before: usize, running: &Running) {
    let start = Instant::now();
    loop {
        let blob = rpc.blob();
        if blob.contains(delta) && occurrences(&blob, "\"type\":\"completed\"") > done_before {
            return;
        }
        if start.elapsed() > Duration::from_secs(20) {
            panic!("missing {delta}\n{}\n{blob}", running.logs());
        }
        sleep(Duration::from_millis(20)).await;
    }
}

fn sqlite_native(db: &Path, sessions_dir: &Path) -> String {
    let script = r#"
import sqlite3, sys, time
path, prefix = sys.argv[1:]
last = ""
for _ in range(50):
    try:
        con = sqlite3.connect(path, timeout=1)
        rows = con.execute("select native_id, file_path from native_sessions").fetchall()
    except sqlite3.Error as err:
        last = str(err)
        time.sleep(0.05)
        continue
    for native_id, file_path in rows:
        if native_id and file_path and file_path.startswith(prefix) and file_path.endswith(".jsonl"):
            print(file_path)
            raise SystemExit(0)
    last = repr(rows)
    time.sleep(0.05)
raise SystemExit(f"missing native session under {prefix}: {last}")
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(db)
        .arg(sessions_dir)
        .output()
        .expect("python3");
    if !output.status.success() {
        panic!(
            "sqlite_native failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

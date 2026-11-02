//! One headless process, one mock turn, one Pi turn. The Pi child is the stub.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

struct Running {
    child: Child,
    reaped: bool,
    stub_dir: PathBuf,
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
        kill_stub_group(&self.stub_dir);
    }
}

fn kill_stub_group(stub_dir: &Path) {
    let Ok(meta) = fs::read_to_string(stub_dir.join("meta")) else {
        return;
    };
    let Some(pid) = meta.lines().find_map(|line| line.strip_prefix("pid=")) else {
        return;
    };
    let Some(pgid) = meta.lines().find_map(|line| line.strip_prefix("pgid=")) else {
        return;
    };
    if pid.is_empty() || pid == "0" || pgid.is_empty() || pgid == "0" {
        return;
    }
    let alive = Command::new("kill")
        .args(["-0", pid])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !alive {
        return;
    }
    let _ = Command::new("kill")
        .args(["-KILL", &format!("-{pgid}")])
        .status();
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
async fn headless_completes_a_mock_turn_and_a_pi_turn() {
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
    let stub_src =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/harness/fixtures/pi_stub.py");
    let stub = root.join("bin/pi");
    fs::create_dir_all(stub.parent().expect("bin")).expect("bin");
    fs::copy(&stub_src, &stub).expect("copy stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let stub_dir = root.join("stub");
    fs::create_dir_all(&stub_dir).expect("stub dir");
    let session = root.join("pi-session.jsonl");
    let port = free_port();
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_keel"));
    let mut child = Command::new(&exe)
        .arg("headless")
        .arg("-d")
        .arg(&data)
        .env("KEEL_IPC_PORT", port.to_string())
        .env("KEEL_MOCK_HARNESS", "1")
        .env("PI_EXECUTABLE", &stub)
        .env("STUB_DIR", &stub_dir)
        .env("STUB_SESSION", &session)
        .env("STUB_MODE", "turn")
        .env_remove("KEEL_DEV_BEARER")
        .env_remove("KEEL_DATA_DIR")
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
        stub_dir: stub_dir.clone(),
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
    let pi = harnesses
        .iter()
        .find(|row| row["id"] == "keel")
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(pi["installed"], true, "{listed}");
    assert_eq!(pi["version"], "1.0.4", "{listed}");
    assert_eq!(pi["display_name"], "Keel");
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
    let pi_chat = rpc
        .request(
            "m2",
            json!({
                "id": "m2",
                "method": "create_chat",
                "params": {
                    "harness": "keel",
                    "model": "fixture-model",
                    "cwd": work.to_string_lossy(),
                    "title": "Pi"
                }
            })
            .to_string(),
            &running.logs(),
        )
        .await;
    let pi_id = pi_chat["result"]["chat_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{pi_chat}"))
        .to_string();
    let _subscribed = rpc
        .request(
            "s2",
            json!({"id": "s2", "method": "subscribe_chat", "params": {"chat_id": pi_id}})
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
                    "chat_id": pi_id,
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
    let session_text = session.to_string_lossy().into_owned();
    let listed_rows = sqlite_native(&db, &session_text);
    assert!(
        listed_rows.contains("sess-1") && listed_rows.contains(&session_text),
        "{listed_rows}\n{}",
        running.logs()
    );
    let args = fs::read_to_string(stub_dir.join("argv")).unwrap_or_default();
    assert!(args.contains("fixture-model"), "{args}");
    assert!(!args.contains("--no-session"), "{args}");
    assert!(!args.lines().any(|line| line.starts_with('@')), "{args}");
    let env_text = fs::read_to_string(stub_dir.join("env")).unwrap_or_default();
    assert!(
        env_text.lines().all(|line| !line.starts_with("KEEL_")),
        "{env_text}"
    );
    let stdin = fs::read_to_string(stub_dir.join("stdin")).unwrap_or_default();
    assert!(stdin.contains("get_state"), "{stdin}");
    assert!(stdin.contains("prompt"), "{stdin}");
    assert!(!stdin.contains("set_steering_mode"), "{stdin}");

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

fn sqlite_native(db: &Path, session: &str) -> String {
    let script = r#"
import sqlite3, sys, time
path, native, file = sys.argv[1:]
last = ""
for _ in range(50):
    try:
        con = sqlite3.connect(path, timeout=1)
        rows = con.execute("select native_id, file_path from native_sessions").fetchall()
    except sqlite3.Error as err:
        last = str(err)
        time.sleep(0.05)
        continue
    if any(row[0] == native and row[1] == file for row in rows):
        print(rows)
        raise SystemExit(0)
    last = repr(rows)
    time.sleep(0.05)
print(last)
raise SystemExit(1)
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(db)
        .arg("sess-1")
        .arg(session)
        .output()
        .expect("python3");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    text
}

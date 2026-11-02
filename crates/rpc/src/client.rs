//! One-shot loopback client used by startup and `keel status`.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::PROTO_VERSION;
use crate::boot::HelloOk;
use crate::dispatch::{engine_status_request, hello_request};

#[derive(Debug)]
pub struct EngineSnapshot {
    pub device_id: String,
    pub scope: String,
    pub live_runs: u64,
}

#[derive(Deserialize)]
struct HelloEnvelope {
    id: String,
    result: HelloOk,
}

#[derive(Deserialize)]
struct StatusEnvelope {
    result: StatusBody,
}

#[derive(Deserialize)]
struct StatusBody {
    live_runs: u64,
}

pub async fn probe_hello(port: u16, budget: Duration, token: &str) -> Option<HelloOk> {
    if budget.is_zero() {
        return None;
    }
    timeout(budget, read_hello(port, token))
        .await
        .unwrap_or_default()
}

pub async fn probe_status(port: u16, budget: Duration, token: &str) -> Option<EngineSnapshot> {
    if budget.is_zero() {
        return None;
    }
    timeout(budget, read_status(port, token))
        .await
        .unwrap_or_default()
}

async fn read_hello(port: u16, token: &str) -> Option<HelloOk> {
    let url = format!("ws://127.0.0.1:{port}");
    let (mut socket, _) = connect_async(&url).await.ok()?;
    let request = hello_request("c1", token);
    socket.send(Message::Text(request)).await.ok()?;
    let Message::Text(body) = socket.next().await?.ok()? else {
        return None;
    };
    let envelope: HelloEnvelope = serde_json::from_str(&body).ok()?;
    if envelope.id != "c1" || envelope.result.proto != PROTO_VERSION {
        return None;
    }
    Some(envelope.result)
}

async fn read_status(port: u16, token: &str) -> Option<EngineSnapshot> {
    let url = format!("ws://127.0.0.1:{port}");
    let (mut socket, _) = connect_async(&url).await.ok()?;
    let hello = hello_request("c1", token);
    socket.send(Message::Text(hello)).await.ok()?;
    let Message::Text(hello_body) = socket.next().await?.ok()? else {
        return None;
    };
    let hello: HelloEnvelope = serde_json::from_str(&hello_body).ok()?;
    if hello.id != "c1" || hello.result.proto != PROTO_VERSION {
        return None;
    }
    let status = engine_status_request("c2");
    socket.send(Message::Text(status)).await.ok()?;
    let Message::Text(status_body) = socket.next().await?.ok()? else {
        return None;
    };
    let status: StatusEnvelope = serde_json::from_str(&status_body).ok()?;
    Some(EngineSnapshot {
        device_id: hello.result.device_id,
        scope: hello.result.scope,
        live_runs: status.result.live_runs,
    })
}

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use keel_proto::RpcStreamItem;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

type Reply = oneshot::Sender<Result<Value, String>>;
/// `None` once the connection is gone: new calls fail fast instead of hanging.
type Pending = Arc<Mutex<Option<HashMap<String, Reply>>>>;
type StreamSlot = Arc<Mutex<Option<mpsc::Sender<RpcStreamItem>>>>;

const CLOSED: &str = "connection closed";

/// Bidirectional RPC client supporting duplex and loopback WebSocket.
#[derive(Clone)]
pub struct RpcClient {
    next_id: Arc<AtomicU64>,
    outgoing_tx: mpsc::Sender<String>,
    pending: Pending,
    stream_tx: StreamSlot,
    device_id: Arc<Mutex<String>>,
    scope: Arc<Mutex<String>>,
}

/// What the reader task needs to route one inbound text.
struct Inbound {
    pending: Pending,
    items: mpsc::UnboundedSender<RpcStreamItem>,
}

impl Inbound {
    fn handle(&self, text: &str) {
        if let Ok(item) = serde_json::from_str::<RpcStreamItem>(text) {
            let _ = self.items.send(item);
            return;
        }
        let Ok(val) = serde_json::from_str::<Value>(text) else {
            tracing::debug!(target: "keel_rpc", "client dropped a non-JSON frame");
            return;
        };
        let Some(id) = val.get("id").and_then(|v| v.as_str()) else {
            return;
        };
        let sender = self
            .pending
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|map| map.remove(id));
        if let Some(sender) = sender {
            let _ = sender.send(reply_result(&val));
        }
    }
}

impl Drop for Inbound {
    /// The reader is gone, so no reply can arrive: fail every waiting caller.
    fn drop(&mut self) {
        close_pending(&self.pending);
    }
}

fn close_pending(pending: &Pending) {
    let waiting = pending.lock().unwrap().take();
    for (_, sender) in waiting.into_iter().flatten() {
        let _ = sender.send(Err(CLOSED.to_string()));
    }
}

fn reply_result(val: &Value) -> Result<Value, String> {
    if let Some(err) = val.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("RPC error");
        return Err(msg.to_string());
    }
    Ok(val.get("result").cloned().unwrap_or(Value::Null))
}

impl RpcClient {
    /// Shared setup. Returns the client plus the halves the transport tasks use.
    fn new_parts() -> (Self, mpsc::Receiver<String>, Inbound) {
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<String>(100);
        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
        let stream_tx: StreamSlot = Arc::new(Mutex::new(None));
        // Reader never blocks on the UI: items queue here, and a forwarder
        // awaits the UI's bounded channel. Nothing is dropped for being slow.
        let (items_tx, mut items_rx) = mpsc::unbounded_channel::<RpcStreamItem>();
        let slot = Arc::clone(&stream_tx);
        tokio::spawn(async move {
            while let Some(item) = items_rx.recv().await {
                let tx = slot.lock().unwrap().clone();
                match tx {
                    Some(tx) => {
                        if tx.send(item).await.is_err() {
                            tracing::debug!(target: "keel_rpc", "stream receiver gone; item dropped");
                        }
                    }
                    None => {
                        tracing::debug!(target: "keel_rpc", "stream item before set_stream_sender; dropped")
                    }
                }
            }
        });
        let client = Self {
            next_id: Arc::new(AtomicU64::new(1)),
            outgoing_tx,
            pending: Arc::clone(&pending),
            stream_tx,
            device_id: Arc::new(Mutex::new(String::new())),
            scope: Arc::new(Mutex::new(String::new())),
        };
        let inbound = Inbound {
            pending,
            items: items_tx,
        };
        (client, outgoing_rx, inbound)
    }

    async fn hello(self, token: &str) -> Result<Self, String> {
        let res = self
            .send_request("hello_0".to_string(), hello_request("hello_0", token))
            .await?;
        let hello_ok: HelloOk = serde_json::from_value(res).map_err(|e| e.to_string())?;
        *self.device_id.lock().unwrap() = hello_ok.device_id;
        *self.scope.lock().unwrap() = hello_ok.scope;
        Ok(self)
    }

    pub async fn from_duplex(stream: tokio::io::DuplexStream) -> Result<Self, String> {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let (client, mut outgoing_rx, inbound) = Self::new_parts();

        let pending = Arc::clone(&client.pending);
        tokio::spawn(async move {
            while let Some(msg) = outgoing_rx.recv().await {
                match crate::codec::write_frame(&mut writer, msg.as_bytes()).await {
                    Ok(()) => {}
                    // `send_request` rejects oversize bodies first; never kill the writer for one.
                    Err(crate::codec::FrameError::TooLarge) => continue,
                    Err(crate::codec::FrameError::Io(_)) => break,
                }
            }
            close_pending(&pending);
        });

        tokio::spawn(async move {
            // Dedicated task: this read is never cancelled mid-frame.
            while let Ok(frame) = crate::codec::read_frame(&mut reader).await {
                if let Ok(text) = String::from_utf8(frame) {
                    inbound.handle(&text);
                }
            }
            drop(inbound);
        });

        client.hello("").await
    }

    pub async fn connect_ws(port: u16, token: &str) -> Result<Self, String> {
        let url = format!("ws://127.0.0.1:{port}");
        let (socket, _) = connect_async(&url).await.map_err(|e| e.to_string())?;
        let (mut writer, mut reader) = socket.split();
        let (client, mut outgoing_rx, inbound) = Self::new_parts();

        let pending = Arc::clone(&client.pending);
        tokio::spawn(async move {
            while let Some(msg) = outgoing_rx.recv().await {
                if writer.send(Message::Text(msg)).await.is_err() {
                    break;
                }
            }
            close_pending(&pending);
        });

        tokio::spawn(async move {
            while let Some(msg) = reader.next().await {
                match msg {
                    Ok(Message::Text(text)) => inbound.handle(&text),
                    Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
                    Ok(Message::Binary(_)) => {}
                    Ok(Message::Close(_)) | Err(_) => break,
                }
            }
            drop(inbound);
        });

        client.hello(token).await
    }

    pub fn device_id(&self) -> String {
        self.device_id.lock().unwrap().clone()
    }

    pub fn scope(&self) -> String {
        self.scope.lock().unwrap().clone()
    }

    pub fn set_stream_sender(&self, tx: mpsc::Sender<RpcStreamItem>) {
        *self.stream_tx.lock().unwrap() = Some(tx);
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = format!("req_{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let req = serde_json::json!({
            "id": id,
            "method": method,
            "params": params,
        });
        let body = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        self.send_request(id, body).await
    }

    async fn send_request(&self, id: String, body: String) -> Result<Value, String> {
        if body.len() > crate::MAX_FRAME_BYTES {
            return Err(format!(
                "payload_too_large: request is {} bytes, cap is {}",
                body.len(),
                crate::MAX_FRAME_BYTES
            ));
        }
        let (tx, rx) = oneshot::channel();
        match self.pending.lock().unwrap().as_mut() {
            Some(map) => map.insert(id.clone(), tx),
            None => return Err(CLOSED.to_string()),
        };
        if self.outgoing_tx.send(body).await.is_err() {
            if let Some(map) = self.pending.lock().unwrap().as_mut() {
                map.remove(&id);
            }
            return Err(CLOSED.to_string());
        }
        rx.await.map_err(|_| CLOSED.to_string())?
    }

    pub async fn subscribe_registry(&self) -> Result<String, String> {
        let res = self
            .call("subscribe_registry", serde_json::json!({}))
            .await?;
        let stream = res
            .get("stream")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(stream)
    }

    pub async fn subscribe_chat(&self, chat_id: &str) -> Result<String, String> {
        let res = self
            .call("subscribe_chat", serde_json::json!({ "chat_id": chat_id }))
            .await?;
        let stream = res
            .get("stream")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(stream)
    }

    pub async fn subscribe_browser(&self) -> Result<String, String> {
        let res = self.call("subscribe_browser", serde_json::json!({})).await?;
        let stream = res
            .get("stream")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(stream)
    }

    pub async fn browser_complete(
        &self,
        request_id: &str,
        result: Result<serde_json::Value, String>,
    ) -> Result<(), String> {
        let params = match result {
            Ok(value) => {
                let mut p = value;
                if let Some(obj) = p.as_object_mut() {
                    obj.insert("request_id".into(), serde_json::json!(request_id));
                }
                p
            }
            Err(message) => serde_json::json!({ "request_id": request_id, "error": message }),
        };
        self.call("browser_complete", params).await.map(|_| ())
    }

    pub async fn list_harnesses(&self) -> Result<Vec<Value>, String> {
        let res = self.call("list_harnesses", serde_json::json!({})).await?;
        let harnesses = res
            .get("harnesses")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(harnesses)
    }

    pub async fn queue_command(
        &self,
        chat_id: &str,
        command: keel_proto::UserCommand,
    ) -> Result<String, String> {
        let res = self
            .call(
                "queue_command",
                serde_json::json!({
                    "chat_id": chat_id,
                    "command": command,
                }),
            )
            .await?;
        let status = res
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(status)
    }

    /// The chat harness's subagent capabilities: how much of a subagent's work
    /// is visible and whether a takeover composer makes sense.
    pub async fn subagent_caps(&self, chat_id: &str) -> Result<keel_proto::SubagentCaps, String> {
        let res = self
            .call("subagent_caps", serde_json::json!({ "chat_id": chat_id }))
            .await?;
        serde_json::from_value(res).map_err(|err| err.to_string())
    }

    pub async fn create_chat(
        &self,
        space_id: &str,
        cwd: &str,
        title: &str,
        harness: &str,
        model: &str,
    ) -> Result<(String, String), String> {
        self.create_chat_full(space_id, cwd, title, harness, model, false, None)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_chat_full(
        &self,
        space_id: &str,
        cwd: &str,
        title: &str,
        harness: &str,
        model: &str,
        use_worktree: bool,
        branch: Option<&str>,
    ) -> Result<(String, String), String> {
        let res = self
            .call(
                "create_chat",
                serde_json::json!({
                    "space_id": space_id,
                    "cwd": cwd,
                    "title": title,
                    "harness": harness,
                    "model": model,
                    "reasoning": "",
                    "use_worktree": use_worktree,
                    "branch": branch,
                }),
            )
            .await?;
        let chat_id = res["chat_id"].as_str().unwrap_or_default().to_string();
        let space_id = res["space_id"].as_str().unwrap_or_default().to_string();
        Ok((chat_id, space_id))
    }

    pub async fn list_branches(&self, cwd: &str) -> Result<Vec<String>, String> {
        let res = self
            .call("list_branches", serde_json::json!({ "cwd": cwd }))
            .await?;
        let branches = res["branches"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Ok(branches)
    }

    /// Local branches plus the checked-out one. `current` is `None` when HEAD
    /// is detached (then `detached` has the short sha) or the engine is older.
    pub async fn list_branches_info(&self, cwd: &str) -> Result<BranchInfo, String> {
        self.branches_info(cwd, false).await
    }

    /// Just where HEAD is (read from `.git`, no listing): the fast answer
    /// for a label, ahead of the full `list_branches_info`.
    pub async fn branch_head_info(&self, cwd: &str) -> Result<BranchInfo, String> {
        self.branches_info(cwd, true).await
    }

    async fn branches_info(&self, cwd: &str, head_only: bool) -> Result<BranchInfo, String> {
        let res = self
            .call("list_branches", serde_json::json!({ "cwd": cwd, "head_only": head_only }))
            .await?;
        let text = |key: &str| res[key].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        Ok(BranchInfo {
            branches: res["branches"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            current: text("current"),
            detached: text("detached"),
        })
    }

    pub async fn delete_space(&self, space_id: &str) -> Result<(), String> {
        self.call("delete_space", serde_json::json!({ "space_id": space_id }))
            .await?;
        Ok(())
    }

    pub async fn list_paths(&self, cwd: &str) -> Result<Vec<String>, String> {
        let v = self.call("list_paths", serde_json::json!({ "cwd": cwd })).await?;
        Ok(v.get("paths")
            .and_then(|p| p.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
            .unwrap_or_default())
    }

    pub async fn delete_chats(&self, chat_ids: &[String]) -> Result<(), String> {
        self.call("delete_chats", serde_json::json!({ "chat_ids": chat_ids }))
            .await?;
        Ok(())
    }

    pub async fn rename_space(&self, space_id: &str, name: &str) -> Result<(), String> {
        self.call(
            "rename_space",
            serde_json::json!({ "space_id": space_id, "name": name }),
        )
        .await?;
        Ok(())
    }

    pub async fn rename_chat(&self, chat_id: &str, title: &str) -> Result<(), String> {
        self.call(
            "rename_chat",
            serde_json::json!({ "chat_id": chat_id, "title": title }),
        )
        .await?;
        Ok(())
    }

    pub async fn set_archived(&self, chat_id: &str, archived: bool) -> Result<(), String> {
        self.call(
            "set_archived",
            serde_json::json!({ "chat_id": chat_id, "archived": archived }),
        )
        .await?;
        Ok(())
    }

    pub async fn read_workspace_file(
        &self,
        space_id: &str,
        rel_path: &str,
    ) -> Result<String, String> {
        let res = self
            .call(
                "read_workspace_file",
                serde_json::json!({ "space_id": space_id, "rel_path": rel_path }),
            )
            .await?;
        let content = res["content"].as_str().unwrap_or_default().to_string();
        Ok(content)
    }

    pub async fn write_workspace_file(
        &self,
        space_id: &str,
        rel_path: &str,
        content: &str,
    ) -> Result<usize, String> {
        let res = self
            .call(
                "write_workspace_file",
                serde_json::json!({
                    "space_id": space_id,
                    "rel_path": rel_path,
                    "content": content,
                }),
            )
            .await?;
        let written = res["written"].as_u64().unwrap_or(0) as usize;
        Ok(written)
    }
}

/// A folder's branches, as `list_branches` reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchInfo {
    pub branches: Vec<String>,
    /// The checked-out branch.
    pub current: Option<String>,
    /// Short sha when HEAD is detached.
    pub detached: Option<String>,
}

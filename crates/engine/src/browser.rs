//! Keel-owned browser host: one pane API the agent reaches through MCP.
//!
//! The headed UI subscribes and executes ops against the WKWebView pane.
//! The Pi driver registers `keel mcp-browser`, pointed at
//! `{data_dir}/browser.sock`, through a per-run `--extension`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use keel_proto::{RpcStreamItem, SubscribeEvent};
use keel_rpc::Outbound;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

/// Long enough for a request queued behind its chat's others, a 30 s
/// `wait_for`, and a navigation's settle; the shim waits a little longer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const TEXT_CAP: usize = 50_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserResult {
    pub url: Option<String>,
    pub title: String,
    /// Visible page text for `snapshot`, a short outcome for click/type/scroll/
    /// screenshot; empty otherwise.
    #[serde(default)]
    pub text: String,
    pub open: bool,
    /// PNG of the visible page for `screenshot`; empty otherwise.
    #[serde(default)]
    pub image_base64: String,
    /// `image/png` (default when empty) or `image/jpeg`.
    #[serde(default)]
    pub image_mime: String,
    /// A screenshot the UI already wrote (full-page ones: too big for an
    /// RPC frame); `image_base64` is then a smaller copy for the agent.
    #[serde(default)]
    pub saved_path: Option<String>,
    /// `browser_scroll`: the new position, `{x, y, width, height, viewport_w, viewport_h, target}`.
    #[serde(default)]
    pub scroll: Option<serde_json::Value>,
}

/// One browser op for the UI: its name, a URL (navigate), and only the
/// arguments that op uses.
#[derive(Debug, Clone)]
pub struct BrowserOp {
    name: &'static str,
    url: String,
    params: serde_json::Value,
}

/// `e12`, `[ref=e12]`, `ref=e12` → `e12`.
fn parse_ref(v: &serde_json::Value) -> Result<Option<String>, String> {
    let Some(raw) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let s = raw.trim_start_matches('[').trim_end_matches(']');
    let s = s.strip_prefix("ref=").unwrap_or(s).trim();
    let ok = s.len() > 1 && s.starts_with('e') && s[1..].bytes().all(|b| b.is_ascii_digit());
    if ok { Ok(Some(s.to_string())) } else { Err(format!("ref must look like e12 (from browser_snapshot), got {raw:?}")) }
}

impl BrowserOp {
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Arguments the UI needs; null when the op takes none.
    pub fn params(&self) -> serde_json::Value {
        self.params.clone()
    }

    /// A sock method and its params → an op, with only the fields that op uses.
    pub fn parse(method: &str, params: &serde_json::Value) -> Result<Self, String> {
        let method = method.strip_prefix("browser_").unwrap_or(method);
        let mut args = serde_json::Map::new();
        let selector = params["selector"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let target = parse_ref(&params["ref"])?;
        let has_target = selector.is_some() || target.is_some();
        let put_target = |args: &mut serde_json::Map<String, serde_json::Value>| {
            if let Some(r) = &target {
                args.insert("ref".into(), r.clone().into());
            } else if let Some(sel) = selector {
                args.insert("selector".into(), sel.into());
            }
        };
        // What an action's reply says about the page afterwards.
        let want = match params["snapshot"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            Some(w @ ("diff" | "full" | "none")) => Some(w),
            Some(other) => return Err(format!("snapshot must be diff, full or none (got {other:?})")),
            None => None,
        };
        let mut url = String::new();
        let name: &'static str = match method {
            "open_pane" => "open_pane",
            "navigate" => {
                url = params["url"].as_str().unwrap_or("").trim().to_string();
                if url.is_empty() {
                    return Err("url is required".into());
                }
                "navigate"
            }
            "snapshot" => {
                match params["mode"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    None | Some("interactive") => {}
                    Some("text") => {
                        args.insert("mode".into(), "text".into());
                    }
                    Some(other) => return Err(format!("mode must be interactive or text (got {other:?})")),
                }
                "snapshot"
            }
            "click" => {
                if has_target {
                    put_target(&mut args);
                } else {
                    let (Some(x), Some(y)) = (params["x"].as_f64(), params["y"].as_f64()) else {
                        return Err("ref (from browser_snapshot), selector, or x and y is required".into());
                    };
                    args.insert("x".into(), x.into());
                    args.insert("y".into(), y.into());
                }
                if params["double"].as_bool() == Some(true) {
                    args.insert("double".into(), true.into());
                }
                "click"
            }
            "type" => {
                let Some(text) = params["text"].as_str() else {
                    return Err("text is required".into());
                };
                put_target(&mut args);
                args.insert("text".into(), text.into());
                args.insert("submit".into(), params["submit"].as_bool().unwrap_or(false).into());
                "type"
            }
            "select" | "select_option" => {
                if !has_target {
                    return Err("ref (or selector) of the <select> is required".into());
                }
                let values: Vec<serde_json::Value> = match &params["values"] {
                    serde_json::Value::Array(a) => a.iter().filter(|v| v.is_string()).cloned().collect(),
                    serde_json::Value::String(s) => vec![s.clone().into()],
                    _ => match &params["value"] {
                        serde_json::Value::String(s) => vec![s.clone().into()],
                        _ => Vec::new(),
                    },
                };
                if values.is_empty() {
                    return Err("values is required: the option labels or values to select".into());
                }
                put_target(&mut args);
                args.insert("values".into(), values.into());
                "select"
            }
            "press" | "press_key" => {
                let Some(key) = params["key"].as_str().map(str::trim).filter(|k| !k.is_empty()) else {
                    return Err("key is required, like Enter, Escape, Tab, ArrowDown, or Meta+a".into());
                };
                put_target(&mut args);
                args.insert("key".into(), key.into());
                "press"
            }
            "scroll" => {
                put_target(&mut args);
                args.insert("dx".into(), params["dx"].as_f64().unwrap_or(0.).into());
                args.insert("dy".into(), params["dy"].as_f64().unwrap_or(0.).into());
                match params["to"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    Some(to @ ("top" | "bottom" | "left" | "right")) => {
                        args.insert("to".into(), to.into());
                    }
                    Some(other) => return Err(format!("to must be top, bottom, left or right (got {other:?})")),
                    None => {}
                }
                "scroll"
            }
            "wait_for" | "wait" => {
                put_target(&mut args);
                if let Some(text) = params["text"].as_str().filter(|t| !t.trim().is_empty()) {
                    args.insert("text".into(), text.into());
                }
                if let Some(u) = params["url"].as_str().filter(|t| !t.trim().is_empty()) {
                    args.insert("url".into(), u.trim().into());
                }
                if params["load"].as_bool() == Some(true) {
                    args.insert("load".into(), true.into());
                }
                if let Some(secs) = params["seconds"].as_f64().filter(|s| *s > 0.) {
                    args.insert("seconds".into(), secs.min(30.).into());
                }
                if params["gone"].as_bool() == Some(true) {
                    args.insert("gone".into(), true.into());
                }
                if let Some(ms) = params["timeout_ms"].as_u64() {
                    args.insert("timeout_ms".into(), ms.min(30_000).into());
                }
                if !["ref", "selector", "text", "url", "load", "seconds"].iter().any(|k| args.contains_key(*k)) {
                    return Err("wait_for needs text, ref, selector, url, load, or seconds".into());
                }
                "wait_for"
            }
            "handle_dialog" => {
                args.insert("accept".into(), params["accept"].as_bool().unwrap_or(true).into());
                if let Some(text) = params["text"].as_str() {
                    args.insert("text".into(), text.into());
                }
                "handle_dialog"
            }
            "upload" => {
                if !has_target {
                    return Err("ref (or selector) of the file input is required".into());
                }
                let paths: Vec<serde_json::Value> = match &params["paths"] {
                    serde_json::Value::Array(a) => a.iter().filter(|v| v.as_str().is_some_and(|s| !s.trim().is_empty())).cloned().collect(),
                    serde_json::Value::String(s) if !s.trim().is_empty() => vec![s.clone().into()],
                    _ => Vec::new(),
                };
                if paths.is_empty() {
                    return Err("paths is required: files inside this chat's project folder".into());
                }
                if paths.len() > 20 {
                    return Err("at most 20 files at a time".into());
                }
                put_target(&mut args);
                args.insert("paths".into(), paths.into());
                "upload"
            }
            "console" => {
                match params["level"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    None => {}
                    Some(l @ ("all" | "error" | "warning" | "info")) => {
                        args.insert("level".into(), l.into());
                    }
                    Some(other) => return Err(format!("level must be all, info, warning or error (got {other:?})")),
                }
                if let Some(n) = params["limit"].as_u64() {
                    args.insert("limit".into(), n.clamp(1, 300).into());
                }
                if params["clear"].as_bool() == Some(true) {
                    args.insert("clear".into(), true.into());
                }
                "console"
            }
            "screenshot" => {
                args.insert("full_page".into(), params["full_page"].as_bool().unwrap_or(false).into());
                "screenshot"
            }
            _ => return Err(format!("unknown method: {method}")),
        };
        if let Some(w) = want.filter(|_| !matches!(name, "open_pane" | "snapshot" | "console" | "screenshot")) {
            args.insert("snapshot".into(), w.into());
        }
        let params = if args.is_empty() { serde_json::Value::Null } else { args.into() };
        Ok(Self { name, url, params })
    }
}

/// `paths` for an upload → absolute files inside `root` (the chat's
/// project folder): relative paths are taken from there; anything that
/// resolves outside it (`..`, symlinks, other absolute paths) is refused.
pub fn resolve_upload(root: &Path, paths: &[String]) -> Result<Vec<PathBuf>, String> {
    let root = root.canonicalize().map_err(|e| format!("the project folder {} can't be read: {e}", root.display()))?;
    let mut out = Vec::new();
    for p in paths {
        let raw = Path::new(p.trim());
        let joined = if raw.is_absolute() { raw.to_path_buf() } else { root.join(raw) };
        let real = joined.canonicalize().map_err(|_| format!("{p}: no such file"))?;
        if !real.starts_with(&root) {
            return Err(format!("{p} is outside this chat's project folder ({}); only files inside it can be uploaded", root.display()));
        }
        if !real.is_file() {
            return Err(format!("{p} is not a file"));
        }
        out.push(real);
    }
    Ok(out)
}

/// Same length and bytes, compared without an early exit.
fn token_matches(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    if a.len() != b.len() || b.is_empty() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct Pending {
    tx: oneshot::Sender<Result<BrowserResult, String>>,
}

struct ClientSub {
    stream: String,
    outbound: Outbound,
}

/// Fan-out to the headed UI and wait for `browser_complete`.
#[derive(Clone)]
pub struct BrowserHost {
    inner: Arc<Inner>,
}

impl Default for BrowserHost {
    fn default() -> Self {
        // 244 random bits from the OS, fresh each launch.
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        Self {
            inner: Arc::new(Inner {
                next: AtomicU64::new(1),
                pending: Mutex::new(HashMap::new()),
                client: Mutex::new(None),
                shots: Mutex::new(Default::default()),
                token,
                roots: Mutex::new(HashMap::new()),
            }),
        }
    }
}

struct Inner {
    next: AtomicU64,
    /// Every sock request carries this per-launch secret (the shim gets it
    /// in its environment). Never logged.
    token: String,
    /// Each chat's project folder, for uploads.
    roots: Mutex<HashMap<String, PathBuf>>,
    pending: Mutex<HashMap<String, Pending>>,
    client: Mutex<Option<ClientSub>>,
    /// Screenshots saved and not yet tied to a tool row, newest last.
    shots: Mutex<std::collections::VecDeque<(PathBuf, std::time::Instant)>>,
}

/// A screenshot not claimed by a tool row within this long is forgotten.
const SHOT_CLAIM: Duration = Duration::from_secs(120);

/// `{data_dir}/screenshots`: browser screenshots and tool images.
pub fn screenshots_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("screenshots")
}

/// A file name that sorts by time: `20261009-110312-123-<n>.<ext>`.
pub fn shot_name(ext: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let now = chrono_like_now();
    format!("{now}-{}.{ext}", SEQ.fetch_add(1, Ordering::Relaxed))
}

/// Local time as `YYYYMMDD-HHMMSS-mmm`, without a date crate.
fn chrono_like_now() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    // SAFETY: `localtime_r` writes into the zeroed `tm` we own.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let secs = now.as_secs() as libc::time_t;
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}-{:03}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        now.subsec_millis()
    )
}

/// Write base64 image bytes into `dir`; the absolute path.
pub fn save_image(dir: &Path, base64_data: &str, ext: &str) -> Result<PathBuf, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data.trim())
        .map_err(|e| format!("bad image data: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(shot_name(ext));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

impl BrowserHost {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("browser.sock")
    }

    /// The per-launch sock token, for the harness drivers to hand the shim.
    pub fn token(&self) -> &str {
        &self.inner.token
    }

    /// Chat `chat` runs in `root`: uploads come from there.
    pub fn set_root(&self, chat: &str, root: PathBuf) {
        self.inner.roots.lock().unwrap_or_else(|p| p.into_inner()).insert(chat.to_string(), root);
    }

    fn root(&self, chat: &str) -> Option<PathBuf> {
        self.inner.roots.lock().unwrap_or_else(|p| p.into_inner()).get(chat).cloned()
    }

    /// The chat was stopped: its queued and running browser work ends.
    pub fn cancel_chat(&self, chat: &str) {
        self.push(SubscribeEvent::BrowserCancel { request_id: String::new(), chat: chat.to_string() });
    }

    /// Send an event to the UI; false when none is subscribed.
    fn push(&self, event: SubscribeEvent) -> bool {
        let client = self.inner.client.lock().unwrap_or_else(|p| p.into_inner());
        match client.as_ref() {
            Some(client) if !client.outbound.is_closed() => {
                let item = RpcStreamItem { stream: client.stream.clone(), event };
                match serde_json::to_string(&item) {
                    Ok(json) => {
                        client.outbound.push(&json);
                        true
                    }
                    Err(_) => false,
                }
            }
            _ => false,
        }
    }

    /// UI takes browser requests on this stream.
    pub fn subscribe(&self, stream: String, outbound: Outbound) {
        let mut client = self.inner.client.lock().unwrap_or_else(|p| p.into_inner());
        *client = Some(ClientSub { stream, outbound });
    }

    /// A screenshot was saved at `path`; a tool row may claim it.
    pub fn record_shot(&self, path: PathBuf) {
        let mut shots = self.inner.shots.lock().unwrap_or_else(|p| p.into_inner());
        shots.retain(|(_, at)| at.elapsed() < SHOT_CLAIM);
        shots.push_back((path, std::time::Instant::now()));
    }

    /// Claim `path` (the tool named it), or else the newest unclaimed
    /// screenshot (a harness that dropped the image and the text).
    pub fn claim_shot(&self, path: Option<&Path>) -> Option<PathBuf> {
        let mut shots = self.inner.shots.lock().unwrap_or_else(|p| p.into_inner());
        shots.retain(|(_, at)| at.elapsed() < SHOT_CLAIM);
        match path {
            Some(path) => {
                let ix = shots.iter().position(|(p, _)| p == path)?;
                shots.remove(ix).map(|(p, _)| p)
            }
            None => shots.pop_back().map(|(p, _)| p),
        }
    }

    pub fn complete(&self, request_id: &str, result: Result<BrowserResult, String>) {
        if let Some(pending) = self.inner.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(request_id) {
            let _ = pending.tx.send(result);
        }
    }

    /// Ask the UI to run `op` for chat `chat`. Errors when no pane is
    /// subscribed or it times out. Dropping the future (the shim hung up)
    /// cancels the request in the UI too, so its queue moves on.
    pub async fn request(&self, op: BrowserOp, chat: &str) -> Result<BrowserResult, String> {
        let request_id = format!("br_{}", self.inner.next.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.inner.pending.lock().unwrap_or_else(|p| p.into_inner());
            pending.insert(request_id.clone(), Pending { tx });
        }
        let sent = self.push(SubscribeEvent::BrowserRequest {
            request_id: request_id.clone(),
            op: op.name().to_string(),
            url: op.url().to_string(),
            params: op.params(),
            chat: chat.to_string(),
        });
        if !sent {
            self.inner.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&request_id);
            return Err("browser pane is not available (open Keel headed)".into());
        }
        let mut guard = CancelOnDrop { host: self, request_id: request_id.clone(), armed: true };
        let result = match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(result)) => {
                guard.armed = false;
                result
            }
            Ok(Err(_)) => Err("browser request cancelled".into()),
            Err(_) => Err(format!("browser request timed out after {}s", REQUEST_TIMEOUT.as_secs())),
        };
        drop(guard);
        result
    }
}

/// Cancels an unanswered request in the UI (and forgets it here).
struct CancelOnDrop<'a> {
    host: &'a BrowserHost,
    request_id: String,
    armed: bool,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.host.inner.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.request_id);
        self.host.push(SubscribeEvent::BrowserCancel { request_id: std::mem::take(&mut self.request_id), chat: String::new() });
    }
}

/// Serve newline JSON on `path`: `{id, method, params}` → `{id, result}` / `{id, error}`.
pub async fn serve_sock(path: PathBuf, host: BrowserHost, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let _ = std::fs::remove_file(&path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(err) => {
            tracing::warn!(target: "keel_engine", error = %err, path = %path.display(), "browser.sock bind failed");
            return;
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::info!(target: "keel_engine", path = %path.display(), "browser.sock listening");
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _)) => {
                        let host = host.clone();
                        let shots = path.parent().map(screenshots_dir);
                        tokio::spawn(async move { handle_client(stream, host, shots).await });
                    }
                    Err(err) => {
                        tracing::warn!(target: "keel_engine", error = %err, "browser.sock accept failed");
                        break;
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_file(&path);
}

async fn handle_client(stream: UnixStream, host: BrowserHost, shots: Option<PathBuf>) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<SockReq>(&line) {
            Ok(req) if !token_matches(req.token.as_deref().unwrap_or(""), host.token()) => {
                tracing::warn!(target: "keel_engine", "browser.sock request refused: bad or missing token");
                serde_json::json!({"id": req.id, "error": {"message": "unauthorized: this browser socket needs Keel's per-launch token (restart the agent's session)"}})
            }
            Ok(req) => {
                let chat = req.chat.unwrap_or_default();
                let call = dispatch(&host, &req.method, req.params, shots.as_deref(), &chat);
                tokio::pin!(call);
                // The shim hanging up (its tool call was cancelled) drops
                // the call, which cancels it in the UI.
                let result = tokio::select! {
                    result = &mut call => result,
                    _ = lines.next_line() => return,
                };
                match result {
                    Ok(value) => serde_json::json!({"id": req.id, "result": value}),
                    Err(message) => serde_json::json!({"id": req.id, "error": {"message": message}}),
                }
            }
            Err(err) => serde_json::json!({"id": null, "error": {"message": format!("bad request: {err}")}}),
        };
        let mut body = reply.to_string();
        body.push('\n');
        if writer.write_all(body.as_bytes()).await.is_err() {
            break;
        }
    }
}

#[derive(Deserialize)]
struct SockReq {
    id: serde_json::Value,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
    /// The per-launch token ([`BrowserHost::token`]).
    #[serde(default)]
    token: Option<String>,
    /// The chat whose agent is asking.
    #[serde(default)]
    chat: Option<String>,
}

/// A screenshot is also saved under `shots` (`{data_dir}/screenshots`); its
/// path is `saved_path` in the reply, for the agent and the transcript.
async fn dispatch(
    host: &BrowserHost,
    method: &str,
    params: serde_json::Value,
    shots: Option<&Path>,
    chat: &str,
) -> Result<serde_json::Value, String> {
    let mut op = BrowserOp::parse(method, &params)?;
    if op.name == "upload" {
        // Only files inside the chat's own project folder.
        let root = host.root(chat).ok_or("uploads need the chat's project folder; this request didn't say which chat it is from")?;
        let paths: Vec<String> = op.params["paths"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
        let files = resolve_upload(&root, &paths)?;
        if let Some(obj) = op.params.as_object_mut() {
            obj.remove("paths");
            obj.insert("files".into(), files.iter().map(|f| f.to_string_lossy().into_owned()).collect::<Vec<_>>().into());
        }
    }
    let result = host.request(op, chat).await?;
    let mut text = result.text;
    if text.len() > TEXT_CAP {
        let mut cut = TEXT_CAP;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n…");
    }
    let mut reply = serde_json::json!({
        "url": result.url,
        "title": result.title,
        "text": text,
        "open": result.open,
    });
    if let Some(scroll) = result.scroll {
        reply["scroll"] = scroll;
    }
    let mime = if result.image_mime.is_empty() { "image/png".to_string() } else { result.image_mime };
    // The UI wrote it already (a full page); the image is the agent's copy.
    if let Some(path) = result.saved_path.map(PathBuf::from).filter(|p| p.is_absolute() && p.is_file()) {
        reply["saved_path"] = path.to_string_lossy().into_owned().into();
        host.record_shot(path);
        if !result.image_base64.is_empty() {
            reply["image_mime"] = mime.into();
            reply["image_base64"] = result.image_base64.into();
        }
    } else if !result.image_base64.is_empty() {
        let ext = if mime == "image/jpeg" { "jpg" } else { "png" };
        reply["image_mime"] = mime.into();
        if let Some(dir) = shots {
            match save_image(dir, &result.image_base64, ext) {
                Ok(path) => {
                    reply["saved_path"] = path.to_string_lossy().into_owned().into();
                    host.record_shot(path);
                }
                Err(err) => tracing::warn!(target: "keel_engine", error = %err, "screenshot not saved"),
            }
        }
        reply["image_base64"] = result.image_base64.into();
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_pane_errors_fast() {
        let host = BrowserHost::default();
        let err = host.request(BrowserOp::parse("open_pane", &serde_json::json!({})).unwrap(), "").await.unwrap_err();
        assert!(err.contains("not available"), "{err}");
    }

    #[tokio::test]
    async fn unknown_method_errors_before_the_pane() {
        let host = BrowserHost::default();
        let err = dispatch(&host, "hover", serde_json::json!({}), None, "").await.unwrap_err();
        assert!(err.contains("unknown method"), "{err}");
    }

    /// A screenshot from the pane is written to disk, named in the reply,
    /// and claimable once by a tool row.
    #[tokio::test]
    async fn screenshot_is_saved_and_named() {
        use base64::Engine;
        let host = BrowserHost::default();
        let (outbound, mut pushed) = Outbound::pair();
        host.subscribe("s".into(), outbound);
        let dir = std::env::temp_dir().join(format!("keel-shots-{}", uuid::Uuid::new_v4()));
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG fake");
        let answer = {
            let host = host.clone();
            let png = png.clone();
            tokio::spawn(async move {
                let item: serde_json::Value = serde_json::from_str(&pushed.recv().await.unwrap()).unwrap();
                let id = item["event"]["request_id"].as_str().unwrap().to_string();
                host.complete(&id, Ok(BrowserResult { url: Some("https://google.com".into()), title: "Google".into(), text: String::new(), open: true, image_base64: png, image_mime: String::new(), saved_path: None, scroll: None }));
            })
        };
        let reply = dispatch(&host, "screenshot", serde_json::json!({}), Some(&dir), "").await.unwrap();
        answer.await.unwrap();
        let saved = PathBuf::from(reply["saved_path"].as_str().unwrap());
        assert!(saved.starts_with(&dir) && saved.extension().unwrap() == "png", "{saved:?}");
        assert_eq!(std::fs::read(&saved).unwrap(), b"\x89PNG fake");
        assert_eq!(reply["image_base64"], png);
        assert_eq!(host.claim_shot(Some(&saved)), Some(saved.clone()));
        assert_eq!(host.claim_shot(None), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A full-page shot the UI saved keeps its path; the agent gets the
    /// smaller JPEG copy with its mime, and scroll positions pass through.
    #[tokio::test]
    async fn ui_saved_screenshot_and_scroll_pass_through() {
        let host = BrowserHost::default();
        let (outbound, mut pushed) = Outbound::pair();
        host.subscribe("s".into(), outbound);
        let file = std::env::temp_dir().join(format!("keel-full-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&file, b"png").unwrap();
        let answer = {
            let host = host.clone();
            let file = file.clone();
            tokio::spawn(async move {
                for _ in 0..2 {
                    let item: serde_json::Value = serde_json::from_str(&pushed.recv().await.unwrap()).unwrap();
                    let id = item["event"]["request_id"].as_str().unwrap().to_string();
                    let full = item["event"]["op"] == "screenshot";
                    host.complete(&id, Ok(BrowserResult {
                        url: Some("https://x".into()),
                        title: "X".into(),
                        text: "t".into(),
                        open: false,
                        image_base64: if full { "/9j/".into() } else { String::new() },
                        image_mime: if full { "image/jpeg".into() } else { String::new() },
                        saved_path: full.then(|| file.to_string_lossy().into_owned()),
                        scroll: (!full).then(|| serde_json::json!({"y": 1200})),
                    }));
                }
            })
        };
        let dir = std::env::temp_dir().join("keel-unused-shots");
        let shot = dispatch(&host, "screenshot", serde_json::json!({"full_page": true}), Some(&dir), "").await.unwrap();
        assert_eq!(shot["saved_path"], file.to_string_lossy().as_ref());
        assert_eq!(shot["image_mime"], "image/jpeg");
        assert_eq!(shot["image_base64"], "/9j/");
        assert_eq!(host.claim_shot(Some(&file)), Some(file.clone()));
        let scrolled = dispatch(&host, "scroll", serde_json::json!({"dy": 1200}), Some(&dir), "").await.unwrap();
        assert_eq!(scrolled["scroll"]["y"], 1200);
        assert!(scrolled.get("image_base64").is_none());
        answer.await.unwrap();
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn act_methods_map_and_keep_only_their_fields() {
        use serde_json::json;
        let op = BrowserOp::parse("browser_click", &json!({"selector": " #go ", "x": 1, "junk": 2})).unwrap();
        assert_eq!(op.name(), "click");
        assert_eq!(op.params(), json!({"selector": "#go"}));
        let op = BrowserOp::parse("click", &json!({"x": 10, "y": 20.5})).unwrap();
        assert_eq!(op.params(), json!({"x": 10.0, "y": 20.5}));
        assert!(BrowserOp::parse("click", &json!({"x": 10})).unwrap_err().contains("selector"));

        let op = BrowserOp::parse("type", &json!({"text": "hi", "selector": "input", "submit": true})).unwrap();
        assert_eq!(op.name(), "type");
        assert_eq!(op.params(), json!({"text": "hi", "selector": "input", "submit": true}));
        assert!(BrowserOp::parse("browser_type", &json!({})).unwrap_err().contains("text"));

        let op = BrowserOp::parse("browser_scroll", &json!({"dy": 400})).unwrap();
        assert_eq!(op.name(), "scroll");
        assert_eq!(op.params(), json!({"dx": 0.0, "dy": 400.0}));

        let op = BrowserOp::parse("browser_scroll", &json!({"to": "bottom", "selector": ".list"})).unwrap();
        assert_eq!(op.params(), json!({"dx": 0.0, "dy": 0.0, "to": "bottom", "selector": ".list"}));
        assert!(BrowserOp::parse("browser_scroll", &json!({"to": "middle"})).unwrap_err().contains("top"));

        let op = BrowserOp::parse("browser_screenshot", &json!({})).unwrap();
        assert_eq!(op.name(), "screenshot");
        assert_eq!(op.params(), json!({"full_page": false}));
        let op = BrowserOp::parse("browser_screenshot", &json!({"full_page": true})).unwrap();
        assert_eq!(op.params(), json!({"full_page": true}));
        assert_eq!(BrowserOp::parse("snapshot", &json!({})).unwrap().name(), "snapshot");
        assert!(BrowserOp::parse("navigate", &json!({})).unwrap_err().contains("url"));
    }

    #[test]
    fn request_event_carries_params_and_old_shape_still_parses() {
        let old = r#"{"type":"browser_request","request_id":"br_1","op":"snapshot"}"#;
        let SubscribeEvent::BrowserRequest { params, .. } = serde_json::from_str(old).unwrap() else { panic!() };
        assert!(params.is_null());
        let event = SubscribeEvent::BrowserRequest {
            request_id: "br_2".into(),
            op: "click".into(),
            url: String::new(),
            params: serde_json::json!({"selector": "a"}),
            chat: "c1".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""params":{"selector":"a"}"#), "{json}");
    }

    #[test]
    fn refs_and_the_new_tools_parse() {
        use serde_json::json;
        let op = BrowserOp::parse("browser_click", &json!({"ref": "[ref=e12]", "selector": "#ignored"})).unwrap();
        assert_eq!(op.params(), json!({"ref": "e12"}));
        assert!(BrowserOp::parse("click", &json!({"ref": "button"})).unwrap_err().contains("e12"));
        let op = BrowserOp::parse("browser_type", &json!({"ref": "e3", "text": "hi", "snapshot": "full"})).unwrap();
        assert_eq!(op.params(), json!({"ref": "e3", "text": "hi", "submit": false, "snapshot": "full"}));
        assert!(BrowserOp::parse("type", &json!({"text": "x", "snapshot": "some"})).is_err());
        let op = BrowserOp::parse("browser_select_option", &json!({"ref": "e4", "values": ["Business"]})).unwrap();
        assert_eq!((op.name(), op.params()), ("select", json!({"ref": "e4", "values": ["Business"]})));
        assert!(BrowserOp::parse("browser_select_option", &json!({"values": ["x"]})).is_err());
        let op = BrowserOp::parse("browser_press_key", &json!({"key": "Enter"})).unwrap();
        assert_eq!((op.name(), op.params()), ("press", json!({"key": "Enter"})));
        let op = BrowserOp::parse("browser_wait_for", &json!({"text": "Saved", "timeout_ms": 90000})).unwrap();
        assert_eq!(op.params(), json!({"text": "Saved", "timeout_ms": 30000}));
        assert!(BrowserOp::parse("browser_wait_for", &json!({})).unwrap_err().contains("needs"));
        let op = BrowserOp::parse("browser_handle_dialog", &json!({"accept": false})).unwrap();
        assert_eq!(op.params(), json!({"accept": false}));
        let op = BrowserOp::parse("browser_upload", &json!({"ref": "e9", "paths": ["a.png"]})).unwrap();
        assert_eq!(op.params(), json!({"ref": "e9", "paths": ["a.png"]}));
        let op = BrowserOp::parse("browser_console", &json!({"level": "error", "limit": 5000})).unwrap();
        assert_eq!(op.params(), json!({"level": "error", "limit": 300}));
        let op = BrowserOp::parse("browser_snapshot", &json!({"mode": "text"})).unwrap();
        assert_eq!(op.params(), json!({"mode": "text"}));
        assert!(BrowserOp::parse("browser_snapshot", &json!({})).unwrap().params().is_null());
    }

    #[test]
    fn uploads_stay_inside_the_project_folder() {
        let root = std::env::temp_dir().join(format!("keel-up-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("img")).unwrap();
        std::fs::write(root.join("img/a.png"), b"x").unwrap();
        let outside = std::env::temp_dir().join(format!("keel-out-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&outside, b"secret").unwrap();
        let real_root = root.canonicalize().unwrap();
        assert_eq!(resolve_upload(&root, &["img/a.png".into()]).unwrap(), vec![real_root.join("img/a.png")]);
        assert_eq!(resolve_upload(&root, &[real_root.join("img/a.png").to_string_lossy().into_owned()]).unwrap().len(), 1);
        assert!(resolve_upload(&root, &["../x".into()]).is_err());
        assert!(resolve_upload(&root, &[outside.to_string_lossy().into_owned()]).unwrap_err().contains("outside"));
        assert!(resolve_upload(&root, &["img".into()]).unwrap_err().contains("not a file"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("link.txt")).unwrap();
            assert!(resolve_upload(&root, &["link.txt".into()]).unwrap_err().contains("outside"), "a symlink out is refused");
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn the_token_must_match_exactly() {
        let host = BrowserHost::default();
        let token = host.token().to_string();
        assert_eq!(token.len(), 64);
        assert_ne!(token, BrowserHost::default().token(), "fresh per launch");
        assert!(token_matches(&token, &token));
        assert!(!token_matches("", &token));
        assert!(!token_matches(&token[..63], &token));
        assert!(!token_matches(&format!("{token}x"), &token));
        let mut flipped = token.clone().into_bytes();
        flipped[10] = if flipped[10] == b'a' { b'b' } else { b'a' };
        assert!(!token_matches(std::str::from_utf8(&flipped).unwrap(), &token));
        assert!(!token_matches("", ""), "an empty expected token never matches");
    }

    /// Over the socket: no token, no service; the right token gets through.
    #[tokio::test]
    async fn the_socket_refuses_requests_without_the_token() {
        let host = BrowserHost::default();
        let ask = |line: String| {
            let host = host.clone();
            async move {
                let (client, server) = UnixStream::pair().unwrap();
                tokio::spawn(handle_client(server, host, None));
                let (r, mut w) = client.into_split();
                w.write_all(format!("{line}\n").as_bytes()).await.unwrap();
                let mut lines = BufReader::new(r).lines();
                serde_json::from_str::<serde_json::Value>(&lines.next_line().await.unwrap().unwrap()).unwrap()
            }
        };
        let none = ask(r#"{"id":1,"method":"snapshot"}"#.into()).await;
        assert!(none["error"]["message"].as_str().unwrap().starts_with("unauthorized"), "{none}");
        let wrong = ask(r#"{"id":2,"method":"snapshot","token":"nope"}"#.into()).await;
        assert!(wrong["error"]["message"].as_str().unwrap().starts_with("unauthorized"), "{wrong}");
        assert!(!wrong.to_string().contains(host.token()), "the token is never echoed");
        // Right token: past the check (no pane in a test, so that error).
        let right = ask(format!(r#"{{"id":3,"method":"snapshot","token":"{}"}}"#, host.token())).await;
        assert!(right["error"]["message"].as_str().unwrap().contains("not available"), "{right}");
    }

    /// A shim that hangs up mid-request cancels it in the UI; a stopped
    /// chat cancels all of its work.
    #[tokio::test]
    async fn a_dropped_request_is_cancelled_in_the_ui() {
        let host = BrowserHost::default();
        let (outbound, mut pushed) = Outbound::pair();
        host.subscribe("s".into(), outbound);
        let op = BrowserOp::parse("click", &serde_json::json!({"ref": "e1"})).unwrap();
        let call = {
            let host = host.clone();
            tokio::spawn(async move { host.request(op, "chat-1").await })
        };
        let first: serde_json::Value = serde_json::from_str(&pushed.recv().await.unwrap()).unwrap();
        assert_eq!(first["event"]["type"], "browser_request");
        assert_eq!(first["event"]["chat"], "chat-1");
        let id = first["event"]["request_id"].as_str().unwrap().to_string();
        call.abort();
        let _ = call.await;
        let cancel: serde_json::Value = serde_json::from_str(&pushed.recv().await.unwrap()).unwrap();
        assert_eq!(cancel["event"]["type"], "browser_cancel");
        assert_eq!(cancel["event"]["request_id"], id);
        host.cancel_chat("chat-1");
        let stop: serde_json::Value = serde_json::from_str(&pushed.recv().await.unwrap()).unwrap();
        assert_eq!(stop["event"], serde_json::json!({"type": "browser_cancel", "chat": "chat-1"}));
    }
}

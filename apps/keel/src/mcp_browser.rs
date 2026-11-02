//! Stdio MCP shim for Keel's browser pane.
//!
//! Keel's Pi spawns this (registered by a per-run extension). It speaks MCP
//! on stdio and dials
//! `{data_dir}/browser.sock`. Each call carries the per-launch token from
//! `KEEL_BROWSER_TOKEN` and the chat id from `KEEL_BROWSER_CHAT`, both set
//! in this process's env by the harness that spawned it (never argv).

use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

const SNAP: &str = r#""snapshot":{"type":"string","enum":["diff","full","none"],"description":"Page state in the reply: diff (default; what changed since your last look), full, or none"}"#;
const REF: &str = r#""ref":{"type":"string","description":"Element ref from the snapshot, like e12"}"#;
const SEL: &str = r#""selector":{"type":"string","description":"CSS selector, when there is no ref"}"#;

/// The tool list, kept short: models read every word of it on every turn.
fn tools() -> Value {
    let raw = format!(
        r#"[
  {{"name":"browser_open_pane","description":"Show Keel's browser pane to the user. Every other tool works without it.","inputSchema":{{"type":"object","properties":{{}}}}}},
  {{"name":"browser_navigate","description":"Load a URL in this chat's browser tab and wait for it. Returns the page snapshot.","inputSchema":{{"type":"object","properties":{{"url":{{"type":"string","description":"http(s) URL, or a host like localhost:3000"}},{SNAP}}},"required":["url"]}}}},
  {{"name":"browser_snapshot","description":"The page as a list of elements with refs (e12) for the other tools: roles, names, values, states. mode text gives the visible text instead.","inputSchema":{{"type":"object","properties":{{"mode":{{"type":"string","enum":["interactive","text"],"description":"interactive (default) or text"}}}}}}}},
  {{"name":"browser_click","description":"Click an element by ref (or selector, or x/y viewport pixels). Waits for any navigation it starts, then returns what changed.","inputSchema":{{"type":"object","properties":{{{REF},{SEL},"x":{{"type":"number"}},"y":{{"type":"number"}},"double":{{"type":"boolean"}},{SNAP}}}}}}},
  {{"name":"browser_type","description":"Replace the text of a field (by ref or selector; else the focused one). submit presses Enter after.","inputSchema":{{"type":"object","properties":{{"text":{{"type":"string"}},{REF},{SEL},"submit":{{"type":"boolean"}},{SNAP}}},"required":["text"]}}}},
  {{"name":"browser_select_option","description":"Choose options in a <select> by label or value.","inputSchema":{{"type":"object","properties":{{{REF},{SEL},"values":{{"type":"array","items":{{"type":"string"}}}},{SNAP}}},"required":["values"]}}}},
  {{"name":"browser_press_key","description":"Press a key on an element (by ref or selector) or the focused one: Enter, Escape, Tab, ArrowDown, Meta+a.","inputSchema":{{"type":"object","properties":{{"key":{{"type":"string"}},{REF},{SEL},{SNAP}}},"required":["key"]}}}},
  {{"name":"browser_scroll","description":"Scroll the page or an element by dx/dy pixels, to an edge, or bring a ref into view.","inputSchema":{{"type":"object","properties":{{"dx":{{"type":"number"}},"dy":{{"type":"number","description":"Positive scrolls down"}},"to":{{"type":"string","enum":["top","bottom","left","right"]}},{REF},{SEL},{SNAP}}}}}}},
  {{"name":"browser_wait_for","description":"Wait until text appears, an element (ref or selector) appears, the URL matches, or the page loads; gone waits for it to disappear instead. Or wait a number of seconds.","inputSchema":{{"type":"object","properties":{{"text":{{"type":"string"}},{REF},{SEL},"url":{{"type":"string","description":"URL or substring; * matches anything"}},"load":{{"type":"boolean"}},"gone":{{"type":"boolean"}},"seconds":{{"type":"number"}},"timeout_ms":{{"type":"number","description":"Default 5000, max 30000"}},{SNAP}}}}}}},
  {{"name":"browser_handle_dialog","description":"Answer the page's open confirm or prompt dialog. Unanswered ones are dismissed after 30 s.","inputSchema":{{"type":"object","properties":{{"accept":{{"type":"boolean","description":"Default true"}},"text":{{"type":"string","description":"A prompt's answer"}},{SNAP}}}}}}},
  {{"name":"browser_upload","description":"Put files into a file input (by ref). Paths are relative to the project folder; files outside it are refused.","inputSchema":{{"type":"object","properties":{{{REF},{SEL},"paths":{{"type":"array","items":{{"type":"string"}}}},{SNAP}}},"required":["paths"]}}}},
  {{"name":"browser_console","description":"The tab's recent console messages and page errors.","inputSchema":{{"type":"object","properties":{{"level":{{"type":"string","enum":["all","error","warning","info"]}},"limit":{{"type":"number","description":"Default 50"}},"clear":{{"type":"boolean"}}}}}}}},
  {{"name":"browser_screenshot","description":"Capture the viewport, or the whole page with full_page. The PNG is saved (path in the result) and shown to the user automatically.","inputSchema":{{"type":"object","properties":{{"full_page":{{"type":"boolean"}}}}}}}}
]"#
    );
    serde_json::from_str(&raw).expect("tool list is valid JSON")
}

pub fn run(socket: PathBuf) -> i32 {
    let link = Link {
        socket,
        token: std::env::var("KEEL_BROWSER_TOKEN").unwrap_or_default(),
        chat: std::env::var("KEEL_BROWSER_CHAT").unwrap_or_default(),
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Notifications have no id.
        if msg.get("id").is_none() {
            continue;
        }
        let id = msg["id"].clone();
        let method = msg["method"].as_str().unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let reply = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "keel-browser", "version": env!("CARGO_PKG_VERSION") }
                }
            }),
            "ping" => json!({"jsonrpc":"2.0","id": id, "result": {}}),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tools() }
            }),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match call_sock(&link, name, args) {
                    Ok(result) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "content": content(result), "isError": false }
                    }),
                    Err(message) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": message }],
                            "isError": true
                        }
                    }),
                }
            }
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {other}") }
            }),
        };
        if writeln!(stdout, "{}", reply).is_err() {
            break;
        }
        let _ = stdout.flush();
    }
    0
}

/// Tool output as MCP content. The page text goes as plain text (a JSON
/// string would escape every newline and quote), then one compact JSON line
/// with the url, title and other fields: chat rows keep only the end of a
/// long result, so that line must come last. A screenshot's PNG becomes an
/// image block, led by where it was saved and that the user already sees it.
fn content(mut result: Value) -> Value {
    let image = result.as_object_mut().and_then(|o| o.remove("image_base64"));
    let mime = result
        .as_object_mut()
        .and_then(|o| o.remove("image_mime"))
        .and_then(|m| m.as_str().map(str::to_string))
        .unwrap_or_else(|| "image/png".to_string());
    let saved = result.get("saved_path").and_then(Value::as_str).map(str::to_string);
    let page = result
        .as_object_mut()
        .and_then(|o| o.remove("text"))
        .and_then(|t| t.as_str().map(str::to_string))
        .unwrap_or_default();
    let footer = serde_json::to_string(&result).unwrap_or_default();
    let body = if page.trim().is_empty() { footer } else { format!("{}\n\n{footer}", page.trim_end()) };
    let mut out = Vec::new();
    if let Some(path) = saved {
        out.push(json!({
            "type": "text",
            "text": format!("Saved to {path} — the screenshot is shown to the user in the chat automatically (no need to attach or send it).")
        }));
    }
    if let Some(data) = image.as_ref().and_then(Value::as_str).filter(|s| !s.is_empty()) {
        out.push(json!({ "type": "image", "data": data, "mimeType": mime }));
    }
    out.push(json!({ "type": "text", "text": body }));
    Value::Array(out)
}

struct Link {
    socket: PathBuf,
    token: String,
    chat: String,
}

/// Longer than the host's own 60 s limit, so its timeout message wins.
const READ_TIMEOUT: Duration = Duration::from_secs(65);

const METHODS: &[&str] = &[
    "open_pane", "navigate", "snapshot", "click", "type", "select_option", "press_key", "scroll", "wait_for",
    "handle_dialog", "upload", "console", "screenshot",
];

fn call_sock(link: &Link, tool: &str, args: Value) -> Result<Value, String> {
    let Some(method) = tool.strip_prefix("browser_").filter(|m| METHODS.contains(m)) else {
        return Err(format!("unknown tool: {tool}"));
    };
    let socket = &link.socket;
    let mut stream = UnixStream::connect(socket).map_err(|e| {
        format!("browser.sock unavailable ({e}). Is Keel headed and the engine up?")
    })?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let mut req = json!({"id": 1, "method": method, "params": args});
    if !link.token.is_empty() {
        req["token"] = json!(link.token);
    }
    if !link.chat.is_empty() {
        req["chat"] = json!(link.chat);
    }
    stream
        .write_all(format!("{req}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut reader = std::io::BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => "the browser didn't answer in time".to_string(),
        _ => e.to_string(),
    })?;
    let reply: Value = serde_json::from_str(&line).map_err(|e| format!("bad sock reply: {e}"))?;
    if let Some(err) = reply.get("error") {
        return Err(err["message"].as_str().unwrap_or("browser error").to_string());
    }
    Ok(reply.get("result").cloned().unwrap_or(json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_every_tool_and_each_maps_to_a_method() {
        let tools = tools();
        let names: Vec<&str> = tools.as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names.len(), METHODS.len());
        for (name, method) in names.iter().zip(METHODS) {
            assert_eq!(name.strip_prefix("browser_"), Some(*method));
        }
        for t in tools.as_array().unwrap() {
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
            assert!(t["description"].as_str().unwrap().len() < 220, "keep {} short", t["name"]);
        }
        // Concise: the whole list, as the model sees it.
        assert!(tools.to_string().len() < 6500, "{}", tools.to_string().len());
    }

    #[test]
    fn screenshot_becomes_an_image_block() {
        let out = content(json!({"url": "http://a", "title": "A", "text": "", "open": true, "image_base64": "iVBOR"}));
        assert_eq!(out[0], json!({"type": "image", "data": "iVBOR", "mimeType": "image/png"}));
        let text = out[1]["text"].as_str().unwrap();
        assert!(text.contains("http://a") && !text.contains("iVBOR"), "{text}");
        // Page text is raw, the fields follow on one last line.
        let out = content(json!({"url": "http://a", "title": "A", "text": "- button \"Go\" [ref=e1]\n- link \"Home\" [ref=e2]", "open": false}));
        let text = out[0]["text"].as_str().unwrap();
        assert!(text.starts_with("- button \"Go\" [ref=e1]\n- link"), "{text}");
        assert_eq!(text.lines().last().unwrap(), r#"{"url":"http://a","title":"A","open":false}"#);
        let out = content(json!({"url": "http://a", "title": "A", "text": "", "open": true, "image_base64": "iVBOR", "saved_path": "/d/screenshots/1.png"}));
        let lead = out[0]["text"].as_str().unwrap();
        assert!(lead.starts_with("Saved to /d/screenshots/1.png ") && lead.contains("shown to the user"), "{lead}");
        assert_eq!(out[1]["type"], "image");
        assert!(out[2]["text"].as_str().unwrap().contains("/d/screenshots/1.png"));
        let out = content(json!({"url": "http://a", "title": "A", "text": "", "open": false, "image_base64": "/9j/", "image_mime": "image/jpeg", "saved_path": "/d/full.png"}));
        assert_eq!(out[1], json!({"type": "image", "data": "/9j/", "mimeType": "image/jpeg"}));
        assert!(!out[2]["text"].as_str().unwrap().contains("image_mime"));
        let out = content(json!({"url": null, "title": "", "text": "x", "open": false}));
        assert_eq!(out.as_array().unwrap().len(), 1);
        assert_eq!(out[0]["type"], "text");
    }
}

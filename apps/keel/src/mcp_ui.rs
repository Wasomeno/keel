//! Stdio MCP for Keel's chat cards.
//!
//! The agent calls `show_chart`, `show_choices`, and the other tools when a
//! card is worth showing. This prints the spec as text. The engine reads that
//! text off the finished tool and stores a `ui` part. No second model, and
//! no socket: the call itself is the decision.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

const NAME: &str = "keel-ui";

pub fn run() -> i32 {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
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
                    "serverInfo": { "name": NAME, "version": env!("CARGO_PKG_VERSION") }
                }
            }),
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": keel_engine::ui_tools() }
            }),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match keel_engine::ui_call(name, &args) {
                    Some(Ok(spec)) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": spec }],
                            "isError": false
                        }
                    }),
                    Some(Err(reason)) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": reason }],
                            "isError": true
                        }
                    }),
                    None => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": format!("unknown ui tool: {name}") }],
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
        if writeln!(stdout, "{reply}").is_err() {
            break;
        }
        let _ = stdout.flush();
    }
    0
}

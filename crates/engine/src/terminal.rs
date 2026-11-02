//! Terminals: a PTY running the user's login shell, owned by the engine.
//!
//! A terminal outlives the panel that shows it: closing the panel detaches,
//! and reopening replays the last 1 MiB of output, then tails it. Terminals
//! are loopback-only; they are never served over the device room.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use keel_proto::{RpcStreamItem, SubscribeEvent};
use keel_rpc::Outbound;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Output kept for a panel that attaches later.
pub const REPLAY_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("no such terminal")]
    Unknown,
    #[error("cannot start the shell: {0}")]
    Spawn(String),
    #[error("terminal i/o: {0}")]
    Io(String),
}

struct Sub {
    stream: String,
    outbound: Outbound,
}

struct Term {
    chat_id: Option<String>,
    cwd: PathBuf,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    /// Replay buffer and subscribers share one lock, so a new subscriber gets
    /// the replay and then every later chunk, with nothing lost or doubled.
    io: Mutex<TermIo>,
}

struct TermIo {
    replay: VecDeque<u8>,
    subs: Vec<Sub>,
    exit: Option<Option<i32>>,
}

impl Term {
    #[cfg(unix)]
    fn busy(&self) -> bool {
        let shell = self.child.lock().unwrap_or_else(|p| p.into_inner()).process_id();
        let foreground = self.master.lock().unwrap_or_else(|p| p.into_inner()).process_group_leader();
        matches!((shell, foreground), (Some(shell), Some(fg)) if fg > 0 && fg as u32 != shell)
    }

    #[cfg(not(unix))]
    fn busy(&self) -> bool {
        false
    }
}

/// Every terminal on this engine.
#[derive(Clone, Default)]
pub struct Terminals {
    inner: Arc<Mutex<HashMap<String, Arc<Term>>>>,
}

/// One row of `list_terminals`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TerminalInfo {
    pub terminal_id: String,
    pub chat_id: Option<String>,
    pub cwd: String,
    pub exited: bool,
    /// A command is running in it: the terminal's foreground process group
    /// is not the shell's own (the shell is waiting at its prompt otherwise).
    pub busy: bool,
}

impl Terminals {
    /// Start the login shell in `cwd` at the given size.
    pub fn open(&self, cwd: &Path, chat_id: Option<String>, cols: u16, rows: u16) -> Result<String, TerminalError> {
        let size = PtySize { rows: rows.max(2), cols: cols.max(10), pixel_width: 0, pixel_height: 0 };
        let pair = native_pty_system().openpty(size).map_err(|e| TerminalError::Spawn(e.to_string()))?;
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".to_string());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        cmd.cwd(cwd);
        // The engine's own variables never reach a user shell.
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("KEEL_") {
                cmd.env_remove(&key);
            }
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd).map_err(|e| TerminalError::Spawn(e.to_string()))?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(|e| TerminalError::Spawn(e.to_string()))?;
        let writer = pair.master.take_writer().map_err(|e| TerminalError::Spawn(e.to_string()))?;

        let id = uuid::Uuid::new_v4().to_string();
        let term = Arc::new(Term {
            chat_id,
            cwd: cwd.to_path_buf(),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            io: Mutex::new(TermIo { replay: VecDeque::new(), subs: Vec::new(), exit: None }),
        });
        self.lock().insert(id.clone(), term.clone());
        std::thread::Builder::new()
            .name(format!("keel-pty-{}", &id[..8]))
            .spawn(move || pump(reader, term))
            .map_err(|e| TerminalError::Spawn(e.to_string()))?;
        Ok(id)
    }

    /// Keystrokes and pastes, already encoded as terminal input.
    pub fn input(&self, id: &str, data: &[u8]) -> Result<(), TerminalError> {
        let term = self.get(id)?;
        let mut writer = term.writer.lock().unwrap_or_else(|p| p.into_inner());
        writer.write_all(data).and_then(|_| writer.flush()).map_err(|e| TerminalError::Io(e.to_string()))
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), TerminalError> {
        let term = self.get(id)?;
        let size = PtySize { rows: rows.max(2), cols: cols.max(10), pixel_width: 0, pixel_height: 0 };
        let master = term.master.lock().unwrap_or_else(|p| p.into_inner());
        master.resize(size).map_err(|e| TerminalError::Io(e.to_string()))
    }

    /// End the shell and forget the terminal.
    pub fn close(&self, id: &str) -> Result<(), TerminalError> {
        let term = self.lock().remove(id).ok_or(TerminalError::Unknown)?;
        let _ = term.child.lock().unwrap_or_else(|p| p.into_inner()).kill();
        Ok(())
    }

    /// Replay what is buffered, then stream new output on `stream`.
    pub fn subscribe(&self, id: &str, stream: String, outbound: Outbound) -> Result<(), TerminalError> {
        let term = self.get(id)?;
        let mut io = term.io.lock().unwrap_or_else(|p| p.into_inner());
        io.subs.retain(|sub| !sub.outbound.is_closed());
        if !io.replay.is_empty() {
            let (a, b) = io.replay.as_slices();
            let mut bytes = Vec::with_capacity(a.len() + b.len());
            bytes.extend_from_slice(a);
            bytes.extend_from_slice(b);
            send(&outbound, &stream, output_event(&bytes));
        }
        if let Some(code) = io.exit {
            send(&outbound, &stream, SubscribeEvent::TerminalExit { code });
        }
        io.subs.push(Sub { stream, outbound });
        Ok(())
    }

    pub fn list(&self) -> Vec<TerminalInfo> {
        let mut rows: Vec<TerminalInfo> = self
            .lock()
            .iter()
            .map(|(id, term)| TerminalInfo {
                terminal_id: id.clone(),
                chat_id: term.chat_id.clone(),
                cwd: term.cwd.to_string_lossy().into_owned(),
                exited: term.io.lock().unwrap_or_else(|p| p.into_inner()).exit.is_some(),
                busy: term.busy(),
            })
            .collect();
        rows.sort_by(|a, b| a.terminal_id.cmp(&b.terminal_id));
        rows
    }

    /// Kill every shell; the engine is going away.
    pub fn close_all(&self) {
        for (_, term) in self.lock().drain() {
            let _ = term.child.lock().unwrap_or_else(|p| p.into_inner()).kill();
        }
    }

    fn get(&self, id: &str) -> Result<Arc<Term>, TerminalError> {
        self.lock().get(id).cloned().ok_or(TerminalError::Unknown)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Term>>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Read the PTY until the shell exits, fanning output out and keeping the tail.
fn pump(mut reader: Box<dyn Read + Send>, term: Arc<Term>) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let chunk = &buf[..n];
        let mut io = term.io.lock().unwrap_or_else(|p| p.into_inner());
        io.replay.extend(chunk);
        let over = io.replay.len().saturating_sub(REPLAY_BYTES);
        io.replay.drain(..over);
        io.subs.retain(|sub| !sub.outbound.is_closed());
        let event = output_event(chunk);
        for sub in &io.subs {
            send(&sub.outbound, &sub.stream, event.clone());
        }
    }
    let code = term
        .child
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .wait()
        .ok()
        .map(|status| status.exit_code() as i32);
    let mut io = term.io.lock().unwrap_or_else(|p| p.into_inner());
    io.exit = Some(code);
    for sub in &io.subs {
        send(&sub.outbound, &sub.stream, SubscribeEvent::TerminalExit { code });
    }
}

fn output_event(bytes: &[u8]) -> SubscribeEvent {
    SubscribeEvent::TerminalOutput { data: base64::engine::general_purpose::STANDARD.encode(bytes) }
}

fn send(outbound: &Outbound, stream: &str, event: SubscribeEvent) {
    let item = RpcStreamItem { stream: stream.to_string(), event };
    if let Ok(json) = serde_json::to_string(&item) {
        outbound.push(&json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Collect a stream's decoded output until `needle` shows up.
    fn read_until(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>, needle: &str) -> (String, Option<Option<i32>>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut out = String::new();
        let mut exit = None;
        while Instant::now() < deadline && !out.contains(needle) {
            match rx.try_recv() {
                Ok(json) => {
                    let item: RpcStreamItem = serde_json::from_str(&json).unwrap();
                    match item.event {
                        SubscribeEvent::TerminalOutput { data } => {
                            let bytes = base64::engine::general_purpose::STANDARD.decode(data).unwrap();
                            out.push_str(&String::from_utf8_lossy(&bytes));
                        }
                        SubscribeEvent::TerminalExit { code } => exit = Some(code),
                        _ => {}
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        (out, exit)
    }

    #[test]
    fn shell_runs_input_replays_to_late_subscribers_and_closes() {
        let terminals = Terminals::default();
        let dir = std::env::temp_dir();
        let id = terminals.open(&dir, None, 80, 24).expect("open");
        let (outbound, mut rx) = Outbound::pair();
        terminals.subscribe(&id, "s1".into(), outbound).expect("subscribe");
        terminals.input(&id, b"echo keel-$((40+2))\n").expect("input");
        let (out, _) = read_until(&mut rx, "keel-42");
        assert!(out.contains("keel-42"), "{out}");

        // A panel that attaches later sees the same output from the replay.
        let (late, mut late_rx) = Outbound::pair();
        terminals.subscribe(&id, "s2".into(), late).expect("late subscribe");
        let (replayed, _) = read_until(&mut late_rx, "keel-42");
        assert!(replayed.contains("keel-42"));

        terminals.resize(&id, 120, 40).expect("resize");
        assert_eq!(terminals.list().len(), 1);
        terminals.close(&id).expect("close");
        assert!(terminals.list().is_empty());
        assert!(matches!(terminals.input(&id, b"x"), Err(TerminalError::Unknown)));
    }

    #[test]
    fn busy_while_a_command_runs_not_at_the_prompt() {
        let terminals = Terminals::default();
        let id = terminals.open(&std::env::temp_dir(), None, 80, 24).expect("open");
        let (outbound, mut rx) = Outbound::pair();
        terminals.subscribe(&id, "s".into(), outbound).expect("subscribe");
        terminals.input(&id, b"echo ready-$((1+1))\n").expect("input");
        read_until(&mut rx, "ready-2");
        let busy = |want: bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if terminals.list()[0].busy == want {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        };
        assert!(busy(false), "idle at the prompt");
        terminals.input(&id, b"sleep 30\n").expect("input");
        assert!(busy(true), "sleep in the foreground");
        terminals.input(&id, b"\x03").expect("ctrl-c");
        assert!(busy(false), "back at the prompt");
        terminals.close(&id).expect("close");
    }

    #[test]
    fn exit_is_reported() {
        let terminals = Terminals::default();
        let id = terminals.open(&std::env::temp_dir(), None, 80, 24).expect("open");
        let (outbound, mut rx) = Outbound::pair();
        terminals.subscribe(&id, "s".into(), outbound).expect("subscribe");
        terminals.input(&id, b"exit 3\n").expect("input");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut code = None;
        while Instant::now() < deadline && code.is_none() {
            if let Ok(json) = rx.try_recv() {
                if let SubscribeEvent::TerminalExit { code: c } = serde_json::from_str::<RpcStreamItem>(&json).unwrap().event {
                    code = Some(c);
                }
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert_eq!(code, Some(Some(3)));
    }
}

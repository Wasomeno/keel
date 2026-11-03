//! Debug builds only: drive the window from a script, so changes can be
//! checked without clicks or keyboard focus (a background window gets
//! neither). `{data_dir}/ui-dev.sock` takes one command per line and answers
//! one line; `view::dev` runs each through the same methods a click calls.
//!
//! ```sh
//! echo 'state' | nc -U ~/Library/Application\ Support/Keel/ui-dev.sock
//! ```
//!
//! Commands: `state`, `chats`, `open <chat id>`, `new`, `type <text>` (`\n`
//! for a newline), `send`, `model <id>`, `picker model|branch|folder|repo|none`,
//! `settings`, `skills` (open the skills page; `skills <pack>` folds that pack),
//! `scroll <px>` (the transcript, like the wheel: positive moves
//! up), `redraw` (one fresh frame, for a screenshot).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

/// One command line and where its answer goes.
pub struct DevCommand {
    pub line: String,
    pub reply: std::sync::mpsc::Sender<String>,
}

static COMMANDS: Mutex<Option<tokio::sync::mpsc::UnboundedSender<DevCommand>>> = Mutex::new(None);

/// The window's end. One listener: a new call replaces the last.
pub fn commands() -> tokio::sync::mpsc::UnboundedReceiver<DevCommand> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *COMMANDS.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    rx
}

/// Bind the socket (owner-only) and serve it on a thread. A stale socket
/// from an earlier run is replaced.
pub fn start(data_dir: &Path) {
    let path = data_dir.join("ui-dev.sock");
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(err) => {
            tracing::warn!(target: "keel_ui::dev", error = %err, "ui-dev.sock bind failed");
            return;
        }
    };
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut out = match stream.try_clone() {
                Ok(out) => out,
                Err(_) => continue,
            };
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let answer = ask(line);
                if writeln!(out, "{answer}").is_err() {
                    break;
                }
            }
        }
    });
}

/// Draw one frame now. gpui stops its display link while macOS reports the
/// window hidden (another Space, behind a full-screen app), so a command's
/// change would never reach the screen; a layer marked for display still
/// draws there (`displayLayer:` runs the frame callback). Main thread only.
#[cfg(target_os = "macos")]
pub fn redraw() {
    use objc2::MainThreadMarker;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSApplication, NSView};
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    fn mark(view: &NSView) {
        view.setNeedsDisplay(true);
        let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
        if let Some(layer) = unsafe { layer.as_ref() } {
            let _: () = unsafe { msg_send![layer, setNeedsDisplay] };
        }
    }
    for window in NSApplication::sharedApplication(mtm).windows().iter() {
        window.setSharingType(objc2_app_kit::NSWindowSharingType::ReadOnly);
        let _: () = unsafe { msg_send![&**window, orderWindow: -1isize, relativeTo: 0isize] };
        let Some(content) = window.contentView() else {
            continue;
        };
        mark(&content);
        for view in content.subviews().iter() {
            mark(&view);
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn redraw() {}

fn ask(line: String) -> String {
    let (reply, answer) = std::sync::mpsc::channel();
    let sent = COMMANDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|tx| tx.send(DevCommand { line, reply }).is_ok());
    if !sent {
        return "error: no window".into();
    }
    answer
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| "error: no answer".into())
}

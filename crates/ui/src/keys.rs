//! Keyboard: every action and its key in one table, plus the macOS menu bar
//! that shows them. `secondary` is Cmd on macOS and Ctrl elsewhere.

use gpui::{App, KeyBinding, Menu, MenuItem, actions};

actions!(
    keel,
    [
        Quit,
        Reload,
        OpenSettings,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        NewChat,
        FocusComposer,
        AcceptPromptSuggestion,
        ToggleSidebar,
        ExpandSteps,
        NextChat,
        PrevChat,
        CloseTab,
        ArchiveChat,
        StopRun,
        ToggleTerminal,
        ToggleBrowser,
        BrowserAddress,
        BrowserReload,
        BrowserBack,
        BrowserForward,
        BrowserExpand,
        BrowserEndSession,
        BrowserNewTab,
        ToggleFiles,
        FilesSave,
        BrowserCloseTab,
        TimerStartPause,
        TimerReset,
        ToggleTimer,
        TrackMergeRequest,
        Dismiss,
        JumpTab1,
        JumpTab2,
        JumpTab3,
        JumpTab4,
        JumpTab5,
        JumpTab6,
        JumpTab7,
        JumpTab8,
        JumpTab9,
    ]
);

/// The window's key context; bindings below only fire inside it.
pub const CONTEXT: &str = "KeelWindow";
/// The browser pane's key context, inside the window's.
pub const BROWSER: &str = "Browser";
/// The files pane's.
pub const FILES: &str = "Files";

pub fn bindings() -> Vec<KeyBinding> {
    let ctx = Some(CONTEXT);
    vec![
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-shift-r", Reload, None),
        // Copies selected transcript text when no input has focus.
        KeyBinding::new("secondary-c", gpui_base::input::Copy, ctx),
        KeyBinding::new("secondary-n", NewChat, ctx),
        KeyBinding::new("secondary-,", OpenSettings, None),
        // ⌘= is ⌘+ without Shift; both zoom in, like a browser.
        KeyBinding::new("secondary-=", ZoomIn, ctx),
        KeyBinding::new("secondary-+", ZoomIn, ctx),
        KeyBinding::new("secondary--", ZoomOut, ctx),
        KeyBinding::new("secondary-0", ZoomReset, ctx),
        KeyBinding::new("secondary-k", FocusComposer, ctx),
        KeyBinding::new("secondary-shift-space", AcceptPromptSuggestion, ctx),
        KeyBinding::new("secondary-b", ToggleSidebar, ctx),
        KeyBinding::new("secondary-alt-e", ExpandSteps, ctx),
        KeyBinding::new("ctrl-tab", NextChat, ctx),
        KeyBinding::new("ctrl-shift-tab", PrevChat, ctx),
        KeyBinding::new("secondary-w", CloseTab, ctx),
        KeyBinding::new("secondary-shift-a", ArchiveChat, ctx),
        KeyBinding::new("secondary-.", StopRun, ctx),
        KeyBinding::new("ctrl-`", ToggleTerminal, ctx),
        KeyBinding::new("secondary-shift-b", ToggleBrowser, ctx),
        KeyBinding::new("secondary-shift-e", ToggleFiles, ctx),
        KeyBinding::new("secondary-s", FilesSave, Some(FILES)),
        KeyBinding::new("secondary-shift-f", BrowserExpand, ctx),
        KeyBinding::new("secondary-shift-t", TimerStartPause, ctx),
        KeyBinding::new("secondary-l", BrowserAddress, Some(BROWSER)),
        KeyBinding::new("secondary-r", BrowserReload, Some(BROWSER)),
        KeyBinding::new("secondary-[", BrowserBack, Some(BROWSER)),
        KeyBinding::new("secondary-]", BrowserForward, Some(BROWSER)),
        KeyBinding::new("secondary-shift-w", BrowserEndSession, Some(BROWSER)),
        // In the pane, ⌘T and ⌘W are its tabs, not the chat's.
        KeyBinding::new("secondary-t", BrowserNewTab, Some(BROWSER)),
        KeyBinding::new("secondary-w", BrowserCloseTab, Some(BROWSER)),
        KeyBinding::new("escape", Dismiss, ctx),
        KeyBinding::new("secondary-1", JumpTab1, ctx),
        KeyBinding::new("secondary-2", JumpTab2, ctx),
        KeyBinding::new("secondary-3", JumpTab3, ctx),
        KeyBinding::new("secondary-4", JumpTab4, ctx),
        KeyBinding::new("secondary-5", JumpTab5, ctx),
        KeyBinding::new("secondary-6", JumpTab6, ctx),
        KeyBinding::new("secondary-7", JumpTab7, ctx),
        KeyBinding::new("secondary-8", JumpTab8, ctx),
        KeyBinding::new("secondary-9", JumpTab9, ctx),
    ]
}

/// Bind keys, quit, and install the menu bar.
pub fn init(cx: &mut App) {
    cx.bind_keys(bindings());
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Reload, cx| reload(cx));
    cx.set_menus(vec![
        Menu {
            name: "Keel".into(),
            items: vec![
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Reload Keel", Reload),
                MenuItem::action("Quit Keel", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "Chat".into(),
            items: vec![
                MenuItem::action("New Chat", NewChat),
                MenuItem::action("Focus Composer", FocusComposer),
                MenuItem::action("Use Prompt Suggestion", AcceptPromptSuggestion),
                MenuItem::separator(),
                MenuItem::action("Next Chat", NextChat),
                MenuItem::action("Previous Chat", PrevChat),
                MenuItem::separator(),
                MenuItem::action("Stop Run", StopRun),
                MenuItem::action("Close Tab", CloseTab),
                MenuItem::action("Archive Chat", ArchiveChat),
                MenuItem::separator(),
                MenuItem::action("Track Merge Request…", TrackMergeRequest),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::separator(),
                MenuItem::action("Zoom In", ZoomIn),
                MenuItem::action("Zoom Out", ZoomOut),
                MenuItem::action("Actual Size", ZoomReset),
                MenuItem::separator(),
                MenuItem::action("Toggle Terminal", ToggleTerminal),
                MenuItem::action("Toggle Browser", ToggleBrowser),
                MenuItem::action("Toggle Files", ToggleFiles),
                MenuItem::action("Browser Full Window", BrowserExpand),
                MenuItem::action("New Browser Tab", BrowserNewTab),
                MenuItem::action("End Browser Session", BrowserEndSession),
                MenuItem::separator(),
                MenuItem::action("Start / Pause Timer", TimerStartPause),
                MenuItem::action("Reset Timer", TimerReset),
                MenuItem::action("Show / Hide Timer", ToggleTimer),
            ],
            disabled: false,
        },
    ]);
}

/// Relaunch the UI. The engine is its own daemon, so chats keep running and
/// the new window reattaches. A bundle goes through gpui (`open Keel.app`);
/// a bare dev binary is re-run directly, since `open` on it starts Terminal.
fn reload(cx: &mut App) {
    let Ok(exe) = std::env::current_exe() else {
        return cx.restart();
    };
    if exe.to_string_lossy().contains(".app/Contents/MacOS/") {
        return cx.restart();
    }
    let spawned = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"while kill -0 "$0" 2>/dev/null; do sleep 0.1; done; exec "$@""#)
        .arg(std::process::id().to_string())
        .arg(exe)
        .args(std::env::args_os().skip(1))
        .spawn();
    match spawned {
        Ok(_) => cx.quit(),
        Err(err) => tracing::error!("reload failed: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_binding_parses() {
        assert_eq!(bindings().len(), 41);
    }

    #[test]
    fn open_settings_is_bound_universally() {
        let b = bindings()
            .into_iter()
            .find(|b| b.action().name().ends_with("OpenSettings"))
            .expect("OpenSettings binding found");
        assert!(b.predicate().is_none(), "OpenSettings should have no context predicate to toggle from any sheet/focus");
    }
}

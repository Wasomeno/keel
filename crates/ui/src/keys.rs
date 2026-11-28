//! Keyboard: every action and its key in one table, plus the macOS menu bar
//! that shows them, runtime rebinding from `ui-settings.json`, and
//! conflict-free shortcut validation.

use std::collections::BTreeMap;

use gpui::{
    Action, App, KeyBinding, KeyBindingContextPredicate, Keystroke, Menu, MenuItem, Modifiers,
    OperatingSystem, actions,
};

actions!(
    keel,
    [
        Quit,
        RefreshApp,
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
        ToggleRightPanel,
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
        BrowserPasteAndMatchStyle,
        TimerStartPause,
        TimerReset,
        ToggleTimer,
        ApprovePlan,
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
/// The unified right side panel's key context, inside the window's.
pub const RIGHT_PANEL: &str = "RightPanel";
/// The browser pane's key context, inside `RIGHT_PANEL`.
pub const BROWSER: &str = "Browser";
/// The live web viewport inside `BROWSER` (distinct from the address bar).
pub const BROWSER_VIEWPORT: &str = "BrowserViewport";
/// The files pane's, inside `RIGHT_PANEL`.
pub const FILES: &str = "Files";

#[cfg(target_os = "macos")]
macro_rules! secondary {
    ($key:literal) => {
        concat!("cmd-", $key)
    };
}

#[cfg(not(target_os = "macos"))]
macro_rules! secondary {
    ($key:literal) => {
        concat!("ctrl-", $key)
    };
}

/// Visual grouping for Settings → Shortcuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutCategory {
    App,
    Chat,
    Navigation,
    BrowserAndFiles,
    Timer,
}

impl ShortcutCategory {
    pub const ALL: &[Self] = &[
        Self::App,
        Self::Chat,
        Self::Navigation,
        Self::BrowserAndFiles,
        Self::Timer,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::App => "App & Window",
            Self::Chat => "Chat & Tabs",
            Self::Navigation => "Panels & Navigation",
            Self::BrowserAndFiles => "Browser & Files",
            Self::Timer => "Timer",
        }
    }

    pub fn subtitle(self) -> Option<&'static str> {
        match self {
            Self::App => None,
            Self::Chat => None,
            Self::Navigation => None,
            Self::BrowserAndFiles => Some("Active when the side panel, browser, or file editor has focus."),
            Self::Timer => None,
        }
    }
}

/// Keymap dispatch scope; determines which shortcuts can coexist without collision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutScope {
    Global,
    Window,
    RightPanel,
    Browser,
    BrowserViewport,
    Files,
}

impl ShortcutScope {
    pub fn context(self) -> Option<&'static str> {
        match self {
            Self::Global => None,
            Self::Window => Some(CONTEXT),
            Self::RightPanel => Some(RIGHT_PANEL),
            Self::Browser => Some(BROWSER),
            Self::BrowserViewport => Some(BROWSER_VIEWPORT),
            Self::Files => Some(FILES),
        }
    }

    /// Quiet badge shown on sub-panel shortcuts so scoped defaults like
    /// Browser reload (`⌘R`) or Files save (`⌘S`) read clearly.
    pub fn badge(self) -> Option<&'static str> {
        match self {
            Self::Global | Self::Window => None,
            Self::RightPanel => Some("Side panel"),
            Self::Browser | Self::BrowserViewport => Some("Browser"),
            Self::Files => Some("Files"),
        }
    }

    /// Two shortcuts conflict if they can both be active at the same focus depth,
    /// or if one is `Global` (fires everywhere).
    pub fn conflicts_with(self, other: Self) -> bool {
        match (self, other) {
            (Self::Global, _) | (_, Self::Global) => true,
            (Self::Window, Self::Window) => true,
            (
                Self::RightPanel,
                Self::RightPanel | Self::Browser | Self::BrowserViewport | Self::Files,
            )
            | (
                Self::Browser | Self::BrowserViewport | Self::Files,
                Self::RightPanel,
            ) => true,
            (Self::Browser | Self::BrowserViewport, Self::Browser | Self::BrowserViewport) => {
                true
            }
            (Self::Files, Self::Files) => true,
            _ => false,
        }
    }
}

/// Metadata + action constructor for one editable Keel shortcut.
#[derive(Clone, Copy)]
pub struct ShortcutSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub category: ShortcutCategory,
    pub scope: ShortcutScope,
    pub default_mac: &'static str,
    pub default_other: &'static str,
    pub build_action: fn() -> Box<dyn Action>,
    pub build_static: fn(&'static str, Option<&str>) -> KeyBinding,
}

impl ShortcutSpec {
    pub fn default_for_os(self, os: OperatingSystem) -> &'static str {
        if os == OperatingSystem::Mac {
            self.default_mac
        } else {
            self.default_other
        }
    }
}

macro_rules! spec {
    ($id:literal, $name:literal, $desc:literal, $cat:ident, $scope:ident, $mac:expr, $other:expr, $action:expr) => {
        ShortcutSpec {
            id: $id,
            name: $name,
            description: $desc,
            category: ShortcutCategory::$cat,
            scope: ShortcutScope::$scope,
            default_mac: $mac,
            default_other: $other,
            build_action: || Box::new($action),
            build_static: |key, ctx| KeyBinding::new(key, $action, ctx),
        }
    };
}

const SHORTCUTS: &[ShortcutSpec] = &[
    // App & Window
    spec!(
        "open_settings",
        "Open Settings",
        "Open or close the Settings modal",
        App,
        Global,
        "cmd-,",
        "ctrl-,",
        OpenSettings
    ),
    spec!(
        "refresh_app",
        "Refresh workspace",
        "Reload chat list and workspace state in place",
        App,
        Window,
        "cmd-r",
        "ctrl-r",
        RefreshApp
    ),
    spec!(
        "reload",
        "Reload Keel",
        "Restart the Keel application window",
        App,
        Global,
        "cmd-shift-r",
        "ctrl-shift-r",
        Reload
    ),
    spec!(
        "zoom_in",
        "Zoom in",
        "Increase interface scale",
        App,
        Window,
        "cmd-=",
        "ctrl-=",
        ZoomIn
    ),
    spec!(
        "zoom_out",
        "Zoom out",
        "Decrease interface scale",
        App,
        Window,
        "cmd--",
        "ctrl--",
        ZoomOut
    ),
    spec!(
        "zoom_reset",
        "Actual size",
        "Reset interface scale to 100%",
        App,
        Window,
        "cmd-0",
        "ctrl-0",
        ZoomReset
    ),
    spec!(
        "dismiss",
        "Dismiss",
        "Close open menus, modals, or expanded side panels",
        App,
        Window,
        "escape",
        "escape",
        Dismiss
    ),
    spec!(
        "quit",
        "Quit Keel",
        "Exit the application",
        App,
        Global,
        "cmd-q",
        "ctrl-q",
        Quit
    ),
    // Chat & Tabs
    spec!(
        "new_chat",
        "New chat",
        "Start a fresh conversation",
        Chat,
        Window,
        "cmd-n",
        "ctrl-n",
        NewChat
    ),
    spec!(
        "focus_composer",
        "Focus composer",
        "Move focus to the message input",
        Chat,
        Window,
        "cmd-k",
        "ctrl-k",
        FocusComposer
    ),
    spec!(
        "accept_prompt_suggestion",
        "Use prompt suggestion",
        "Insert the suggested follow-up prompt",
        Chat,
        Window,
        "cmd-shift-space",
        "ctrl-shift-space",
        AcceptPromptSuggestion
    ),
    spec!(
        "approve_plan",
        "Approve plan",
        "Confirm the pending plan in the composer",
        Chat,
        Window,
        "cmd-enter",
        "ctrl-enter",
        ApprovePlan
    ),
    spec!(
        "expand_steps",
        "Toggle turn steps",
        "Expand or collapse reasoning and tool steps",
        Chat,
        Window,
        "cmd-alt-e",
        "ctrl-alt-e",
        ExpandSteps
    ),
    spec!(
        "stop_run",
        "Stop run",
        "Interrupt the active agent turn",
        Chat,
        Window,
        "cmd-.",
        "ctrl-.",
        StopRun
    ),
    spec!(
        "next_chat",
        "Next chat",
        "Switch to the next chat in the sidebar",
        Chat,
        Window,
        "ctrl-tab",
        "ctrl-tab",
        NextChat
    ),
    spec!(
        "prev_chat",
        "Previous chat",
        "Switch to the previous chat in the sidebar",
        Chat,
        Window,
        "ctrl-shift-tab",
        "ctrl-shift-tab",
        PrevChat
    ),
    spec!(
        "close_tab",
        "Close chat tab",
        "Close the active chat tab",
        Chat,
        Window,
        "cmd-w",
        "ctrl-w",
        CloseTab
    ),
    spec!(
        "archive_chat",
        "Archive chat",
        "Archive the active chat",
        Chat,
        Window,
        "cmd-shift-a",
        "ctrl-shift-a",
        ArchiveChat
    ),
    spec!(
        "jump_tab_1",
        "Switch to tab 1",
        "Jump to the 1st open chat tab",
        Chat,
        Window,
        "cmd-1",
        "ctrl-1",
        JumpTab1
    ),
    spec!(
        "jump_tab_2",
        "Switch to tab 2",
        "Jump to the 2nd open chat tab",
        Chat,
        Window,
        "cmd-2",
        "ctrl-2",
        JumpTab2
    ),
    spec!(
        "jump_tab_3",
        "Switch to tab 3",
        "Jump to the 3rd open chat tab",
        Chat,
        Window,
        "cmd-3",
        "ctrl-3",
        JumpTab3
    ),
    spec!(
        "jump_tab_4",
        "Switch to tab 4",
        "Jump to the 4th open chat tab",
        Chat,
        Window,
        "cmd-4",
        "ctrl-4",
        JumpTab4
    ),
    spec!(
        "jump_tab_5",
        "Switch to tab 5",
        "Jump to the 5th open chat tab",
        Chat,
        Window,
        "cmd-5",
        "ctrl-5",
        JumpTab5
    ),
    spec!(
        "jump_tab_6",
        "Switch to tab 6",
        "Jump to the 6th open chat tab",
        Chat,
        Window,
        "cmd-6",
        "ctrl-6",
        JumpTab6
    ),
    spec!(
        "jump_tab_7",
        "Switch to tab 7",
        "Jump to the 7th open chat tab",
        Chat,
        Window,
        "cmd-7",
        "ctrl-7",
        JumpTab7
    ),
    spec!(
        "jump_tab_8",
        "Switch to tab 8",
        "Jump to the 8th open chat tab",
        Chat,
        Window,
        "cmd-8",
        "ctrl-8",
        JumpTab8
    ),
    spec!(
        "jump_tab_9",
        "Switch to last tab",
        "Jump to the last open chat tab",
        Chat,
        Window,
        "cmd-9",
        "ctrl-9",
        JumpTab9
    ),
    // Panels & Navigation
    spec!(
        "toggle_sidebar",
        "Toggle sidebar",
        "Show or hide the left sidebar",
        Navigation,
        Window,
        "cmd-b",
        "ctrl-b",
        ToggleSidebar
    ),
    spec!(
        "toggle_right_panel",
        "Toggle side panel",
        "Show or hide the right side panel",
        Navigation,
        Window,
        "cmd-alt-b",
        "ctrl-alt-b",
        ToggleRightPanel
    ),
    spec!(
        "toggle_terminal",
        "Toggle terminal",
        "Open or focus the terminal tab",
        Navigation,
        Window,
        "ctrl-`",
        "ctrl-`",
        ToggleTerminal
    ),
    spec!(
        "toggle_browser",
        "Toggle browser",
        "Open or focus the browser tab",
        Navigation,
        Window,
        "cmd-shift-b",
        "ctrl-shift-b",
        ToggleBrowser
    ),
    spec!(
        "toggle_files",
        "Toggle files",
        "Open or focus the files tab",
        Navigation,
        Window,
        "cmd-shift-e",
        "ctrl-shift-e",
        ToggleFiles
    ),
    spec!(
        "browser_expand",
        "Expand side panel",
        "Expand or restore the right side panel",
        Navigation,
        Window,
        "cmd-shift-f",
        "ctrl-shift-f",
        BrowserExpand
    ),
    spec!(
        "browser_new_tab",
        "New side panel tab",
        "Open a new tab in the right side panel",
        Navigation,
        RightPanel,
        "cmd-t",
        "ctrl-t",
        BrowserNewTab
    ),
    spec!(
        "browser_close_tab",
        "Close side panel tab",
        "Close the active tab in the right side panel",
        Navigation,
        RightPanel,
        "cmd-w",
        "ctrl-w",
        BrowserCloseTab
    ),
    // Browser & Files
    spec!(
        "files_save",
        "Save file",
        "Save edits in the active file viewer",
        BrowserAndFiles,
        Files,
        "cmd-s",
        "ctrl-s",
        FilesSave
    ),
    spec!(
        "browser_address",
        "Focus address bar",
        "Focus the browser URL field",
        BrowserAndFiles,
        Browser,
        "cmd-l",
        "ctrl-l",
        BrowserAddress
    ),
    spec!(
        "browser_reload",
        "Reload page",
        "Reload or stop loading the active page",
        BrowserAndFiles,
        Browser,
        "cmd-r",
        "ctrl-r",
        BrowserReload
    ),
    spec!(
        "browser_back",
        "Browser back",
        "Go back in browser history",
        BrowserAndFiles,
        Browser,
        "cmd-[",
        "ctrl-[",
        BrowserBack
    ),
    spec!(
        "browser_forward",
        "Browser forward",
        "Go forward in browser history",
        BrowserAndFiles,
        Browser,
        "cmd-]",
        "ctrl-]",
        BrowserForward
    ),
    spec!(
        "browser_end_session",
        "End browser session",
        "Close all browser tabs and clear the session",
        BrowserAndFiles,
        Browser,
        "cmd-shift-w",
        "ctrl-shift-w",
        BrowserEndSession
    ),
    spec!(
        "browser_paste_plain",
        "Paste and match style",
        "Paste plain text into the browser page",
        BrowserAndFiles,
        BrowserViewport,
        "cmd-shift-v",
        "ctrl-shift-v",
        BrowserPasteAndMatchStyle
    ),
    // Timer
    spec!(
        "timer_start_pause",
        "Start / pause timer",
        "Start or pause the Dotify work timer",
        Timer,
        Window,
        "cmd-shift-t",
        "ctrl-shift-t",
        TimerStartPause
    ),
    spec!(
        "timer_reset",
        "Reset timer",
        "Reset the Dotify work timer",
        Timer,
        Window,
        "",
        "",
        TimerReset
    ),
    spec!(
        "toggle_timer",
        "Show / hide timer",
        "Toggle visibility of the timer pill",
        Timer,
        Window,
        "",
        "",
        ToggleTimer
    ),
];

/// Every editable shortcut registered by Keel.
pub fn all_shortcuts() -> &'static [ShortcutSpec] {
    SHORTCUTS
}

pub fn shortcut_by_id(id: &str) -> Option<&'static ShortcutSpec> {
    SHORTCUTS.iter().find(|s| s.id == id)
}

pub fn current_os() -> OperatingSystem {
    if cfg!(target_os = "macos") {
        OperatingSystem::Mac
    } else if cfg!(target_os = "windows") {
        OperatingSystem::Windows
    } else {
        OperatingSystem::Linux
    }
}

/// Effective keystroke string (`""` if unbound) for `id`.
pub fn effective_binding(
    overrides: &BTreeMap<String, String>,
    id: &str,
    os: OperatingSystem,
) -> String {
    if let Some(custom) = overrides.get(id) {
        return custom.trim().to_string();
    }
    shortcut_by_id(id)
        .map(|s| s.default_for_os(os).to_string())
        .unwrap_or_default()
}

pub fn is_customized(
    overrides: &BTreeMap<String, String>,
    id: &str,
    os: OperatingSystem,
) -> bool {
    let Some(spec) = shortcut_by_id(id) else {
        return false;
    };
    let effective = effective_binding(overrides, id, os);
    let default = spec.default_for_os(os);
    match (parse_norm(&effective, os), parse_norm(default, os)) {
        (Some(a), Some(b)) => a != b,
        (None, None) => false,
        _ => effective != default,
    }
}

pub fn customized_count(overrides: &BTreeMap<String, String>, os: OperatingSystem) -> usize {
    SHORTCUTS
        .iter()
        .filter(|s| is_customized(overrides, s.id, os))
        .count()
}

/// Normalized keystroke equality key (ignores `key_char`, normalizes key case).
#[derive(Clone, Debug, PartialEq, Eq)]
struct NormKey {
    control: bool,
    alt: bool,
    shift: bool,
    platform: bool,
    function: bool,
    key: String,
}

impl NormKey {
    fn from_keystroke(ks: &Keystroke) -> Self {
        Self {
            control: ks.modifiers.control,
            alt: ks.modifiers.alt,
            shift: ks.modifiers.shift,
            platform: ks.modifiers.platform,
            function: ks.modifiers.function,
            key: ks.key.to_ascii_lowercase(),
        }
    }

    fn has_primary_modifier(&self) -> bool {
        self.control || self.alt || self.platform
    }
}

fn parse_norm(raw: &str, os: OperatingSystem) -> Option<NormKey> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Keystroke::parse_with_os(trimmed, os)
        .ok()
        .map(|ks| NormKey::from_keystroke(&ks))
}

/// Canonical string serialization of a `Keystroke` for storage in `ui-settings.json`.
pub fn canonicalize_keystroke(ks: &Keystroke, os: OperatingSystem) -> String {
    let clean = Keystroke {
        modifiers: ks.modifiers,
        key: ks.key.to_ascii_lowercase(),
        key_char: None,
    };
    clean.unparse(os)
}

fn is_modifier_only_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "shift" | "control" | "ctrl" | "alt" | "platform" | "cmd" | "super" | "win" | "function"
            | "fn" | "capslock"
    )
}

fn is_function_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=20).contains(&n))
}

fn reserved_system_label(norm: &NormKey, os: OperatingSystem) -> Option<&'static str> {
    let is_mac = os == OperatingSystem::Mac;
    let secondary_only = if is_mac {
        norm.platform && !norm.control && !norm.alt && !norm.function
    } else {
        norm.control && !norm.platform && !norm.alt && !norm.function
    };
    if !secondary_only {
        return None;
    }
    match (norm.shift, norm.key.as_str()) {
        (false, "c") => Some("Copy"),
        (false, "x") => Some("Cut"),
        (false, "v") => Some("Paste"),
        (false, "a") => Some("Select All"),
        (false, "z") => Some("Undo"),
        (true, "z") => Some("Redo"),
        (false, "y") if !is_mac => Some("Redo"),
        (false, "h") if is_mac => Some("Hide Keel"),
        (false, "m") if is_mac => Some("Minimize"),
        _ => None,
    }
}

/// Outcome of inspecting a live keystroke while recording a shortcut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CandidateOutcome {
    /// Bare modifier pressed; keep waiting and update held modifier preview.
    IgnoreModifierOnly,
    /// Unmodified Escape cancels recording.
    Cancel,
    /// Unmodified Backspace / Delete clears the shortcut.
    Clear,
    /// Valid, conflict-free keystroke string ready to save.
    Accept(String),
    /// Validation error (missing modifier, reserved key, or duplicate shortcut).
    Reject(String),
}

/// Validate a recorded keystroke against modifier rules, reserved OS/editing
/// bindings, and all other Keel shortcuts in overlapping scopes.
pub fn validate_candidate(
    overrides: &BTreeMap<String, String>,
    target_id: &str,
    ks: &Keystroke,
    os: OperatingSystem,
) -> CandidateOutcome {
    let key_lower = ks.key.to_ascii_lowercase();
    if is_modifier_only_key(&key_lower) {
        return CandidateOutcome::IgnoreModifierOnly;
    }

    if !ks.modifiers.modified() {
        if key_lower == "escape" {
            return CandidateOutcome::Cancel;
        }
        if matches!(key_lower.as_str(), "backspace" | "delete") {
            return CandidateOutcome::Clear;
        }
    }

    let norm = NormKey::from_keystroke(ks);
    let allow_unmodified = key_lower == "escape" || is_function_key(&key_lower);
    if !norm.has_primary_modifier() && !allow_unmodified {
        let hint = if os == OperatingSystem::Mac {
            "Include ⌘, ⌃, or ⌥ so typing isn't interrupted"
        } else {
            "Include Ctrl or Alt so typing isn't interrupted"
        };
        return CandidateOutcome::Reject(hint.into());
    }

    let canonical = canonicalize_keystroke(ks, os);
    let label = display_compact(&canonical, os);

    if let Some(reserved) = reserved_system_label(&norm, os) {
        return CandidateOutcome::Reject(format!("{label} is reserved for {reserved}"));
    }

    let Some(target_spec) = shortcut_by_id(target_id) else {
        return CandidateOutcome::Reject("Unknown shortcut".into());
    };

    for other in SHORTCUTS {
        if other.id == target_id || !target_spec.scope.conflicts_with(other.scope) {
            continue;
        }
        let other_raw = effective_binding(overrides, other.id, os);
        if let Some(other_norm) = parse_norm(&other_raw, os)
            && other_norm == norm
        {
            return CandidateOutcome::Reject(format!(
                "{label} is already used by {}",
                other.name
            ));
        }
    }

    CandidateOutcome::Accept(canonical)
}

/// True when `ks` matches the currently configured binding for `id`.
pub fn matches_shortcut(
    overrides: &BTreeMap<String, String>,
    id: &str,
    ks: &Keystroke,
    os: OperatingSystem,
) -> bool {
    let raw = effective_binding(overrides, id, os);
    let Some(bound) = parse_norm(&raw, os) else {
        return false;
    };
    bound == NormKey::from_keystroke(ks)
}

fn format_key_name(key: &str, os: OperatingSystem) -> String {
    match key.to_ascii_lowercase().as_str() {
        "escape" => "Esc".into(),
        "space" => "Space".into(),
        "enter" => if os == OperatingSystem::Mac { "↵" } else { "Enter" }.into(),
        "tab" => "Tab".into(),
        "backspace" => if os == OperatingSystem::Mac { "⌫" } else { "Backspace" }.into(),
        "delete" => if os == OperatingSystem::Mac { "⌦" } else { "Del" }.into(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        k if k.len() == 1 => k.to_ascii_uppercase(),
        k => {
            if let Some(rest) = k.strip_prefix('f')
                && rest.chars().all(|c| c.is_ascii_digit())
            {
                return format!("F{rest}");
            }
            let mut chars = k.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        }
    }
}

/// Modifier tokens for live held-modifier preview while recording.
pub fn modifier_tokens(m: Modifiers, os: OperatingSystem) -> Vec<String> {
    let mut out = Vec::new();
    let is_mac = os == OperatingSystem::Mac;
    if is_mac {
        if m.platform {
            out.push("⌘".into());
        }
        if m.control {
            out.push("⌃".into());
        }
        if m.alt {
            out.push("⌥".into());
        }
        if m.shift {
            out.push("⇧".into());
        }
    } else {
        if m.control {
            out.push("Ctrl".into());
        }
        if m.alt {
            out.push("Alt".into());
        }
        if m.shift {
            out.push("Shift".into());
        }
        if m.platform {
            out.push(match os {
                OperatingSystem::Windows => "Win",
                _ => "Super",
            }
            .into());
        }
    }
    if m.function {
        out.push("Fn".into());
    }
    out
}

/// Individual keycap tokens for rendering (e.g. `["⌘", "⇧", "Space"]`).
pub fn display_tokens(keystroke_str: &str, os: OperatingSystem) -> Vec<String> {
    let trimmed = keystroke_str.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(ks) = Keystroke::parse_with_os(trimmed, os) else {
        return vec![trimmed.to_string()];
    };
    let mut tokens = modifier_tokens(ks.modifiers, os);
    tokens.push(format_key_name(&ks.key, os));
    tokens
}

/// Compact single-string representation (e.g. `"⌘⇧Space"` on macOS, `"Ctrl+Shift+Space"` elsewhere).
pub fn display_compact(keystroke_str: &str, os: OperatingSystem) -> String {
    let tokens = display_tokens(keystroke_str, os);
    if tokens.is_empty() {
        return String::new();
    }
    if os == OperatingSystem::Mac {
        let mut out = String::new();
        for (ix, tok) in tokens.iter().enumerate() {
            // Put a thin separator only if the previous token was a word or this is a multi-char key after symbols?
            // Matches Keel's existing `⌘⇧Space`, `⌘,`, `⌃Tab` style directly.
            let _ = ix;
            out.push_str(tok);
        }
        out
    } else {
        tokens.join("+")
    }
}

/// Compact display string for shortcut `id`, or `None` when unbound.
pub fn display_for_id(
    overrides: &BTreeMap<String, String>,
    id: &str,
    os: OperatingSystem,
) -> Option<String> {
    let raw = effective_binding(overrides, id, os);
    let compact = display_compact(&raw, os);
    (!compact.is_empty()).then_some(compact)
}

/// Tooltip-friendly macOS-or-OS shortcut helper (returns `None` on non-macOS only
/// if callers want mac-only, or returns live shortcut everywhere).
pub fn display_for_tooltip(
    overrides: &BTreeMap<String, String>,
    id: &str,
    os: OperatingSystem,
) -> Option<String> {
    cfg!(target_os = "macos")
        .then(|| display_for_id(overrides, id, os))
        .flatten()
}

struct BaseKeyBindings(Vec<KeyBinding>);
impl gpui::Global for BaseKeyBindings {}

fn key_used_in_scope(
    overrides: &BTreeMap<String, String>,
    scope: ShortcutScope,
    raw_key: &str,
    os: OperatingSystem,
) -> bool {
    let Some(target_norm) = parse_norm(raw_key, os) else {
        return false;
    };
    SHORTCUTS.iter().any(|spec| {
        spec.scope.conflicts_with(scope)
            && parse_norm(&effective_binding(overrides, spec.id, os), os)
                .is_some_and(|n| n == target_norm)
    })
}

/// Static default bindings (used by tests and fallback initialization).
pub fn bindings() -> Vec<KeyBinding> {
    let os = current_os();
    let empty = BTreeMap::new();
    let mut out = Vec::new();
    for spec in SHORTCUTS {
        let key = spec.default_for_os(os);
        if key.is_empty() {
            continue;
        }
        if let Ok(ks) = Keystroke::parse_with_os(key, os) {
            let static_key = Box::leak(ks.unparse(os).into_boxed_str());
            out.push((spec.build_static)(static_key, spec.scope.context()));
        }
    }
    out.extend(internal_bindings(&empty, os));
    out
}

fn internal_bindings(overrides: &BTreeMap<String, String>, os: OperatingSystem) -> Vec<KeyBinding> {
    let ctx = Some(CONTEXT);
    let mut out = vec![
        // Copies selected transcript text when no input has focus.
        KeyBinding::new(secondary!("c"), gpui_base::input::Copy, ctx),
        // Inside the live browser viewport, keep Tab and editing shortcuts
        // with the web page instead of Root focus traversal or chat actions.
        KeyBinding::new("tab", gpui::NoAction, Some(BROWSER_VIEWPORT)),
        KeyBinding::new("shift-tab", gpui::NoAction, Some(BROWSER_VIEWPORT)),
        KeyBinding::new(
            secondary!("c"),
            gpui_base::input::Copy,
            Some(BROWSER_VIEWPORT),
        ),
        KeyBinding::new(
            secondary!("x"),
            gpui_base::input::Cut,
            Some(BROWSER_VIEWPORT),
        ),
        KeyBinding::new(
            secondary!("v"),
            gpui_base::input::Paste,
            Some(BROWSER_VIEWPORT),
        ),
        KeyBinding::new(
            secondary!("a"),
            gpui_base::input::SelectAll,
            Some(BROWSER_VIEWPORT),
        ),
        KeyBinding::new(
            secondary!("z"),
            gpui_base::input::Undo,
            Some(BROWSER_VIEWPORT),
        ),
        KeyBinding::new(
            secondary!("shift-z"),
            gpui_base::input::Redo,
            Some(BROWSER_VIEWPORT),
        ),
        KeyBinding::new(
            secondary!("y"),
            gpui_base::input::Redo,
            Some(BROWSER_VIEWPORT),
        ),
    ];

    // Secondary alias for ZoomIn (`⌘+`) when ZoomIn stays on `⌘=` and nothing else uses `⌘+`.
    if !is_customized(overrides, "zoom_in", os)
        && !key_used_in_scope(overrides, ShortcutScope::Window, secondary!("+"), os)
    {
        out.push(KeyBinding::new(secondary!("+"), ZoomIn, ctx));
    }

    // Secondary alias for BrowserPasteAndMatchStyle (`⌘⌥⇧V`) when at default and unused.
    if !is_customized(overrides, "browser_paste_plain", os)
        && !key_used_in_scope(
            overrides,
            ShortcutScope::BrowserViewport,
            secondary!("alt-shift-v"),
            os,
        )
    {
        out.push(KeyBinding::new(
            secondary!("alt-shift-v"),
            BrowserPasteAndMatchStyle,
            Some(BROWSER_VIEWPORT),
        ));
    }

    out
}

fn dynamic_bindings(cx: &App, overrides: &BTreeMap<String, String>) -> Vec<KeyBinding> {
    let os = cx.platform_info().os;
    let mut out = Vec::with_capacity(SHORTCUTS.len() + 14);
    for spec in SHORTCUTS {
        let key = effective_binding(overrides, spec.id, os);
        if key.is_empty() {
            continue;
        }
        let ctx = spec
            .scope
            .context()
            .and_then(|c| KeyBindingContextPredicate::parse(c).ok())
            .map(std::rc::Rc::new);
        match KeyBinding::load(&key, (spec.build_action)(), ctx, false, None, cx) {
            Ok(binding) => out.push(binding),
            Err(err) => tracing::warn!(id = spec.id, key = %key, "ignoring invalid shortcut: {err}"),
        }
    }
    out.extend(internal_bindings(overrides, os));
    out
}

fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Keel".into(),
            items: vec![
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Refresh", RefreshApp),
                MenuItem::action("Reload Keel", Reload),
                MenuItem::action("Quit Keel", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Undo", gpui_base::input::Undo, gpui::OsAction::Undo),
                MenuItem::os_action("Redo", gpui_base::input::Redo, gpui::OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", gpui_base::input::Cut, gpui::OsAction::Cut),
                MenuItem::os_action("Copy", gpui_base::input::Copy, gpui::OsAction::Copy),
                MenuItem::os_action("Paste", gpui_base::input::Paste, gpui::OsAction::Paste),
                MenuItem::action("Paste and Match Style", BrowserPasteAndMatchStyle),
                MenuItem::os_action(
                    "Select All",
                    gpui_base::input::SelectAll,
                    gpui::OsAction::SelectAll,
                ),
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
                MenuItem::action("Toggle Side Panel", ToggleRightPanel),
                MenuItem::action("Toggle Terminal", ToggleTerminal),
                MenuItem::action("Toggle Browser", ToggleBrowser),
                MenuItem::action("Toggle Files", ToggleFiles),
                MenuItem::action("Expand Side Panel", BrowserExpand),
                MenuItem::action("New Side Panel Tab", BrowserNewTab),
                MenuItem::action("End Browser Session", BrowserEndSession),
                MenuItem::separator(),
                MenuItem::action("Start / Pause Timer", TimerStartPause),
                MenuItem::action("Reset Timer", TimerReset),
                MenuItem::action("Show / Hide Timer", ToggleTimer),
            ],
            disabled: false,
        },
    ]
}

/// Bind keys, quit, and install the menu bar.
pub fn init(cx: &mut App, overrides: &BTreeMap<String, String>) {
    if !cx.has_global::<BaseKeyBindings>() {
        let base: Vec<KeyBinding> = cx.key_bindings().borrow().bindings().cloned().collect();
        cx.set_global(BaseKeyBindings(base));
    }
    apply(cx, overrides);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Reload, cx| reload(cx));
}

/// Re-apply all Keel shortcuts from `overrides` and refresh the macOS menu bar.
pub fn apply(cx: &mut App, overrides: &BTreeMap<String, String>) {
    if cx.has_global::<BaseKeyBindings>() {
        let base = cx.global::<BaseKeyBindings>().0.clone();
        cx.clear_key_bindings();
        cx.bind_keys(base);
    }
    let next = dynamic_bindings(cx, overrides);
    cx.bind_keys(next);
    cx.set_menus(menus());
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
        assert_eq!(bindings().len(), 55);
    }

    #[test]
    fn open_settings_is_bound_universally() {
        let b = bindings()
            .into_iter()
            .find(|b| b.action().name().ends_with("OpenSettings"))
            .expect("OpenSettings binding found");
        assert!(
            b.predicate().is_none(),
            "OpenSettings should have no context predicate to toggle from any sheet/focus"
        );
    }

    #[test]
    fn default_shortcuts_have_zero_conflicts_and_unique_ids() {
        let os = OperatingSystem::Mac;
        let empty = BTreeMap::new();
        let mut ids = std::collections::HashSet::new();
        for spec in SHORTCUTS {
            assert!(ids.insert(spec.id), "duplicate id {}", spec.id);
            let raw = spec.default_for_os(os);
            if raw.is_empty() {
                continue;
            }
            let ks = Keystroke::parse_with_os(raw, os).expect("valid default keystroke");
            let expected = if spec.id == "dismiss" && raw == "escape" {
                CandidateOutcome::Cancel
            } else {
                CandidateOutcome::Accept(canonicalize_keystroke(&ks, os))
            };
            assert_eq!(
                validate_candidate(&empty, spec.id, &ks, os),
                expected,
                "default shortcut {} ({raw}) conflicted",
                spec.id
            );
        }
    }

    #[test]
    fn duplicate_validation_blocks_overlapping_scopes() {
        let os = OperatingSystem::Mac;
        let overrides = BTreeMap::new();

        // Same scope (Window vs Window): cmd-b is Toggle sidebar
        let ks_b = Keystroke::parse_with_os("cmd-b", os).unwrap();
        assert_eq!(
            validate_candidate(&overrides, "new_chat", &ks_b, os),
            CandidateOutcome::Reject("⌘B is already used by Toggle sidebar".into())
        );

        // Global vs Window: cmd-, is Open Settings
        let ks_comma = Keystroke::parse_with_os("cmd-,", os).unwrap();
        assert_eq!(
            validate_candidate(&overrides, "new_chat", &ks_comma, os),
            CandidateOutcome::Reject("⌘, is already used by Open Settings".into())
        );

        // Window into Global: cmd-n is New chat
        let ks_n = Keystroke::parse_with_os("cmd-n", os).unwrap();
        assert_eq!(
            validate_candidate(&overrides, "open_settings", &ks_n, os),
            CandidateOutcome::Reject("⌘N is already used by New chat".into())
        );

        // RightPanel vs Browser: cmd-l is Focus address bar
        let ks_l = Keystroke::parse_with_os("cmd-l", os).unwrap();
        assert_eq!(
            validate_candidate(&overrides, "browser_new_tab", &ks_l, os),
            CandidateOutcome::Reject("⌘L is already used by Focus address bar".into())
        );

        // Custom override becomes the active duplicate target
        let mut custom = BTreeMap::new();
        custom.insert("new_chat".into(), "cmd-shift-n".into());
        let ks_shift_n = Keystroke::parse_with_os("cmd-shift-n", os).unwrap();
        assert_eq!(
            validate_candidate(&custom, "toggle_sidebar", &ks_shift_n, os),
            CandidateOutcome::Reject("⌘⇧N is already used by New chat".into())
        );
        // Old default cmd-n is now free!
        assert_eq!(
            validate_candidate(&custom, "toggle_sidebar", &ks_n, os),
            CandidateOutcome::Accept("cmd-n".into())
        );
    }

    #[test]
    fn reserved_and_unmodified_keys_are_rejected() {
        let os = OperatingSystem::Mac;
        let empty = BTreeMap::new();

        // Copy is reserved
        let ks_copy = Keystroke::parse_with_os("cmd-c", os).unwrap();
        assert_eq!(
            validate_candidate(&empty, "new_chat", &ks_copy, os),
            CandidateOutcome::Reject("⌘C is reserved for Copy".into())
        );

        // Bare letter is rejected
        let ks_a = Keystroke::parse_with_os("a", os).unwrap();
        assert!(matches!(
            validate_candidate(&empty, "new_chat", &ks_a, os),
            CandidateOutcome::Reject(_)
        ));

        // Shift-only letter is rejected
        let ks_shift_a = Keystroke::parse_with_os("shift-a", os).unwrap();
        assert!(matches!(
            validate_candidate(&empty, "new_chat", &ks_shift_a, os),
            CandidateOutcome::Reject(_)
        ));

        // Function key without modifier is accepted
        let ks_f5 = Keystroke::parse_with_os("f5", os).unwrap();
        assert_eq!(
            validate_candidate(&empty, "refresh_app", &ks_f5, os),
            CandidateOutcome::Accept("f5".into())
        );
    }

    #[test]
    fn formatting_tokens_render_cleanly() {
        assert_eq!(
            display_tokens("cmd-shift-space", OperatingSystem::Mac),
            vec!["⌘", "⇧", "Space"]
        );
        assert_eq!(
            display_compact("cmd-shift-space", OperatingSystem::Mac),
            "⌘⇧Space"
        );
        assert_eq!(
            display_tokens("ctrl-tab", OperatingSystem::Mac),
            vec!["⌃", "Tab"]
        );
        assert_eq!(
            display_compact("cmd-enter", OperatingSystem::Mac),
            "⌘↵"
        );
    }
}

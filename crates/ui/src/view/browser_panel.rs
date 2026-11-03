//! Browser pane: pages beside the chat, for the app an agent is building.
//! It slides in from the right edge; the live page (a native view on macOS)
//! is moved, clipped and faded with it every frame, so it never lags behind.
//! Drag its left edge to resize; double-click the edge to reset.
//!
//! Tabs: each holds its own page. Each chat's agent works in a tab of its
//! own (see `browser_agent.rs`), so tabs the user opens are never driven;
//! the pane shows the open chat's tab, and other chats' tabs stay out of
//! the strip. File previews open in a tab each. The strip shows once there
//! are two.

#[path = "browser_agent.rs"]
mod agent;

use std::time::{Duration, Instant};

use gpui::{
    AnimationExt as _, AnyElement, ClickEvent, Context, ElementId, Entity, FocusHandle,
    Focusable as _, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ScrollWheelEvent, Window, div, prelude::*, px, relative,
};
use gpui_base::input::InputState;
use keel_theme::Tokens;

use super::{KeelWindow, icon_button};
use crate::browser::agent::ConsoleLog;
use crate::browser::{PageState, normalize_address};
use crate::icons::Icon;
use crate::motion::{BASE, SLOW, SPATIAL, ease_in, ease_out};
use crate::style::{HEADER_HEIGHT, KeelStyled, RADIUS_LG, RADIUS_MD, c, zpx};
use agent::AgentTab;

/// Default share of the window, and the bounds a drag keeps it in.
const DEFAULT_SHARE: f32 = 0.42;
const MIN_WIDTH: f32 = 320.;
/// The chat keeps at least this much beside the pane.
const MIN_CHAT: f32 = 360.;
/// The page card's inset from the pane edges, and its border.
const PAGE_INSET: f32 = 6.;
/// Addresses the empty pane offers: the usual dev-server ports.
const QUICK: [&str; 3] = ["localhost:3000", "localhost:5173", "localhost:8080"];
/// The tab strip's height, and a tab's widest.
const TAB_STRIP_H: f32 = 30.;
const TAB_MAX_W: f32 = 200.;

pub(super) struct BrowserPanel {
    /// The toolbar's hover tooltips.
    tips: super::tooltip::HoverTips,
    pub(super) open: bool,
    /// The last open/close: when, and a generation so its animation replays.
    pub(super) anim: Option<(Instant, u64)>,
    /// Dragged width; `None` follows the window.
    width: Option<f32>,
    pub(super) resizing: bool,
    /// Pointer offset from the pane's left edge at grab, so the edge
    /// doesn't jump onto the cursor.
    resize_grab: f32,
    /// Grown over the whole window.
    pub(super) expanded: bool,
    pub(super) expand_from: f32,
    expand_anim: Option<(Instant, u64)>,
    pub(super) address: Entity<InputState>,
    focus: FocusHandle,
    /// Never empty; `active` is the one shown.
    pub(super) tabs: Vec<BrowserTab>,
    pub(super) active: usize,
    next_tab: u64,
    /// Ended sessions, so the start screen replays its entrance after one.
    sessions: u64,
    /// The window, so host requests run with one even when nothing renders.
    handle: gpui::AnyWindowHandle,
    /// Agent script sequence numbers (a reply is matched to its job).
    seq: u64,
    /// The chat the pane last followed, to show its tab after a switch.
    shown_chat: Option<String>,
    /// Last host request: the header's "browsing in the background" dot.
    pub(super) agent_at: Option<Instant>,
    /// Last agent op or pane close, for [`PARK_AFTER`].
    idle_since: Option<Instant>,
    park_task: Option<gpui::Task<()>>,
}

/// One tab: its page and what the toolbar shows for it.
pub(super) struct BrowserTab {
    pub(super) id: u64,
    pub(super) page: PageState,
    /// Progress bar: from, to, generation (each step replays its glide).
    bar: (f32, f32, u64),
    /// The load finished: the bar fills and fades.
    bar_done: bool,
    pub(super) error: Option<String>,
    /// A local file shown from a transcript chip (the page is its preview,
    /// loaded as HTML, so WebKit reports `about:blank` for it).
    preview: Option<std::path::PathBuf>,
    pub(super) native: Option<std::rc::Rc<crate::browser::NativePage>>,
    _events: Option<gpui::Task<()>>,
    _pump: Option<gpui::Task<()>>,
    /// The last web page (not a file preview).
    web_url: Option<String>,
    /// The page was unloaded after [`PARK_AFTER`] idle with the pane closed;
    /// `web_url` is loaded again when the tab is needed.
    pub(super) parked: bool,
    /// Where an unloaded page was scrolled, put back when it returns.
    restore: Option<(f64, f64)>,
    /// The scroll read that precedes unloading, in flight.
    park_seq: Option<u64>,
    /// The chat whose agent drives this tab.
    agent: Option<AgentTab>,
    /// Console messages and page errors (`browser_console`).
    console: ConsoleLog,
}

impl BrowserTab {
    fn new(id: u64) -> Self {
        Self {
            id,
            page: PageState::default(),
            bar: (0., 0., 0),
            bar_done: false,
            error: None,
            preview: None,
            native: None,
            _events: None,
            _pump: None,
            web_url: None,
            parked: false,
            restore: None,
            park_seq: None,
            agent: None,
            console: ConsoleLog::default(),
        }
    }

    /// A load started: the bar restarts.
    fn loading(&mut self, url: String) {
        self.error = None;
        self.page.url = Some(url);
        self.page.loading = true;
        self.bar = (0., 0.08, self.bar.2 + 1);
        self.bar_done = false;
    }

    /// What its tab says: the page title, else the host, else "New Tab".
    fn label(&self) -> String {
        if !self.page.title.trim().is_empty() {
            return self.page.title.trim().to_string();
        }
        if let Some(path) = &self.preview {
            return path
                .file_name()
                .map_or_else(|| "File".into(), |n| n.to_string_lossy().into_owned());
        }
        match self.page.url.as_deref() {
            Some(url) => {
                let rest = url.split_once("://").map_or(url, |(_, r)| r);
                rest.split(['/', '?', '#'])
                    .next()
                    .unwrap_or(rest)
                    .to_string()
            }
            None => "New Tab".into(),
        }
    }
}

/// One browser request from the engine (an agent's browser_* tool).
pub(super) struct HostRequest {
    request_id: String,
    op: String,
    url: String,
    params: serde_json::Value,
    client: keel_rpc::RpcClient,
    /// The chat whose agent asked (empty from an old shim).
    chat: String,
}

/// A screenshot in flight; `full` once the page size is known.
struct ShotRequest {
    request_id: String,
    client: keel_rpc::RpcClient,
    full_page: bool,
    full: Option<FullPage>,
    /// Full page: frames so far (PNG, CSS y), the y asked for, and the
    /// y the page really scrolled to.
    frames: Vec<(Vec<u8>, f64)>,
    asked_y: f64,
    at_y: f64,
}

/// A full-page capture: CSS width, captured height, document height,
/// viewport height, and the scroll position to put back.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FullPage {
    w: f64,
    h: f64,
    total: f64,
    vh: f64,
    sx: f64,
    sy: f64,
}

/// Pause after each full-page scroll so lazy images can paint.
const FRAME_SETTLE: Duration = Duration::from_millis(160);

/// Tallest full-page capture, in CSS px.
const FULL_PAGE_MAX: f64 = 16_000.;
/// Pages left running with the pane closed and no agent op this long are
/// unloaded (each WebContent process is a few hundred MB); `web_url` brings
/// one back when it is needed again.
const PARK_AFTER: Duration = Duration::from_secs(10 * 60);

impl BrowserPanel {
    pub(super) fn new(window: &mut Window, cx: &mut Context<KeelWindow>) -> Self {
        Self {
            open: false,
            anim: None,
            width: None,
            resizing: false,
            resize_grab: 0.,
            expanded: false,
            expand_from: 0.,
            expand_anim: None,
            address: cx
                .new(|cx| InputState::new(window, cx).placeholder("Search or enter address")),
            focus: cx.focus_handle(),
            tabs: vec![BrowserTab::new(0)],
            active: 0,
            next_tab: 1,
            sessions: 0,
            handle: window.window_handle(),
            seq: 0,
            shown_chat: None,
            agent_at: None,
            idle_since: None,
            park_task: None,
            tips: Default::default(),
        }
    }

    pub(super) fn tab(&self) -> &BrowserTab {
        &self.tabs[self.active]
    }

    pub(super) fn active_native_page(&self) -> Option<std::rc::Rc<crate::browser::NativePage>> {
        self.tabs.get(self.active).and_then(|t| t.native.clone())
    }

    fn tab_mut(&mut self) -> &mut BrowserTab {
        &mut self.tabs[self.active]
    }

    fn tab_ix(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Tabs the strip shows: the user's, and the open chat's agent tab.
    fn visible_in_strip(&self, tab: &BrowserTab, chat: Option<&str>) -> bool {
        tab.agent
            .as_ref()
            .is_none_or(|a| a.chat.is_empty() || Some(a.chat.as_str()) == chat)
    }

    /// A new empty tab after the others; its index.
    fn push_tab(&mut self) -> usize {
        self.tabs.push(BrowserTab::new(self.next_tab));
        self.next_tab += 1;
        self.tabs.len() - 1
    }

    /// Native focus back to gpui, so keys reach Keel's own fields.
    fn release_focus(&self) {
        if let Some(native) = &self.tab().native {
            native.focus_parent();
        }
    }
}

/// How visible the pane is at `k` of its width, respecting entrance vs exit.
fn reveal(k: f32, entering: bool) -> f32 {
    crate::motion::panel_opacity(k, entering)
}

impl KeelWindow {
    /// The pane belongs to an open chat session only: never the new-chat
    /// screen, Settings, or GitLab. `was_hero` is this frame's hero state.
    pub(super) fn browser_allowed(&self) -> bool {
        !self.was_hero
            && !self.settings_open
            && !self.gitlab_open
            && self.confirming_delete_chats.is_none()
            && self.state.confirming_delete_space.is_none()
    }

    /// An agent's page is alive with the pane closed: the header's globe
    /// carries a dot (bright while a request is recent).
    pub(super) fn browser_in_background(&self) -> Option<bool> {
        let b = &self.browser;
        let alive = b.tabs.iter().any(|tab| {
            tab.agent.is_some()
                && tab.native.as_ref().is_some_and(|n| n.background())
                && tab.page.url.is_some()
        });
        (alive && !b.open).then(|| {
            b.agent_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(20))
        })
    }

    /// ⌘⇧B and the header button: slide the pane in or out.
    pub(super) fn toggle_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.browser_allowed() {
            return;
        }
        if !self.browser.open && self.files.open {
            self.toggle_files(window, cx);
        }
        if !self.browser.open {
            self.set_terminal_open(false, window, cx);
        }
        self.update_right_panel_anim();
        let b = &mut self.browser;
        b.tips.clear();
        b.open = !b.open;
        let n = b.anim.map_or(1, |(_, n)| n + 1);
        b.anim = Some((Instant::now(), n));
        b.release_focus();
        if b.open {
            b.expanded = false;
            b.expand_from = 0.;
            b.expand_anim = None;
            self.unpark_tab(self.browser.active, window, cx);
            let b = &mut self.browser;
            if b.tab().page.url.is_none() {
                b.address.update(cx, |s, cx| s.focus(window, cx));
            } else {
                window.focus(&b.focus, cx);
            }
        } else {
            b.expanded = false;
            b.expand_from = 0.;
            b.expand_anim = None;
            for tab in &b.tabs {
                if let Some(page) = &tab.native {
                    page.set_resizing(false);
                }
            }
            self.composer.update(cx, |s, cx| s.focus(window, cx));
            self.schedule_browser_park(cx);
        }
        let settle = self.right_panel_duration();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(settle + Duration::from_millis(20))
                .await;
            let _ = this.update(cx, |_, cx| {
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// ⌘L: the address field, opening the pane if needed.
    pub(super) fn focus_browser_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.browser_allowed() {
            return;
        }
        if !self.browser.open {
            self.toggle_browser(window, cx);
        }
        self.browser.release_focus();
        self.browser.address.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Load what the address field holds.
    pub(super) fn browser_navigate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.browser.address.read(cx).value().to_string();
        let Some(url) = normalize_address(&typed) else {
            return;
        };
        self.browser_load(url, window, cx);
    }

    /// Show tab `ix`: the address follows it, and a parked page comes back.
    fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.browser.tabs.len() {
            return;
        }
        if ix != self.browser.active {
            self.browser.release_focus();
            self.browser.active = ix;
        }
        if self.browser.open {
            self.unpark_tab(ix, window, cx);
        }
        let b = &self.browser;
        let url = b.tab().page.url.clone().unwrap_or_default();
        b.address.update(cx, |s, cx| s.set_value(url, window, cx));
        if b.tab().page.url.is_none() {
            b.address.update(cx, |s, cx| s.focus(window, cx));
        } else {
            window.focus(&b.focus, cx);
        }
        cx.notify();
    }

    /// ⌘T and the toolbar's plus: a new tab on the start screen.
    pub(super) fn browser_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.browser_allowed() {
            return;
        }
        if !self.browser.open {
            self.toggle_browser(window, cx);
        }
        let ix = self.browser.push_tab();
        self.activate_tab(ix, window, cx);
    }

    /// ⌘W in the pane and a tab's ×: close it (its page and process go
    /// with it). The last tab goes back to the start screen instead.
    pub(super) fn browser_close_tab(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ix >= self.browser.tabs.len() {
            return;
        }
        self.drop_agent_tab(ix, "the user closed this chat's browser tab", cx);
        let b = &mut self.browser;
        b.tabs[ix].agent = None;
        if b.tabs.len() == 1 {
            return self.end_browser_session(window, cx);
        }
        b.release_focus();
        b.tabs.remove(ix);
        if ix < b.active || b.active >= b.tabs.len() {
            b.active = b.active.saturating_sub(1);
        }
        let active = b.active;
        self.activate_tab(active, window, cx);
    }

    /// ⌘W with the pane focused.
    pub(super) fn browser_close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.browser.active;
        self.browser_close_tab(ix, window, cx);
    }

    /// Re-arms the idle timer that unloads pages (see [`PARK_AFTER`]).
    fn schedule_browser_park(&mut self, cx: &mut Context<Self>) {
        self.browser.idle_since = Some(Instant::now());
        self.browser.park_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(PARK_AFTER + Duration::from_millis(50))
                .await;
            let _ = this.update(cx, |this, cx| this.park_browser_if_idle(cx));
        }));
    }

    /// Drops every tab's native page (and its WebContent process) when the
    /// pane has sat [`PARK_AFTER`] closed with nothing in flight. URLs,
    /// titles, and the address stay, so the header and `navigated` still
    /// read right; [`Self::unpark_tab`] reloads one.
    /// Tabs left [`PARK_AFTER`] without use and out of sight are unloaded
    /// one by one (their scroll position read first, and put back when the
    /// page returns); a tab whose chat is busy, or the one on screen, stays.
    fn park_browser_if_idle(&mut self, cx: &mut Context<Self>) {
        let b = &self.browser;
        let idle = b.idle_since.is_some_and(|at| at.elapsed() >= PARK_AFTER);
        let mut park = Vec::new();
        for (ix, tab) in b.tabs.iter().enumerate() {
            if tab.native.is_none() || tab.web_url.is_none() || (b.open && ix == b.active) {
                continue;
            }
            let ready = match &tab.agent {
                Some(a) => !a.busy() && a.used.elapsed() >= PARK_AFTER,
                None => idle && !b.open,
            };
            if ready {
                park.push(ix);
            }
        }
        for ix in park {
            self.start_park(ix);
        }
        // Agent tabs idle on their own clocks: look again later.
        if self
            .browser
            .tabs
            .iter()
            .any(|t| t.native.is_some() && t.agent.is_some())
        {
            self.schedule_browser_park(cx);
        }
        cx.notify();
    }

    /// An unloaded tab comes back: a fresh native page on its `web_url`.
    /// True when a load started.
    fn unpark_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(tab) = self.browser.tabs.get_mut(ix).filter(|t| t.parked) else {
            return false;
        };
        let Some(url) = tab.web_url.clone() else {
            tab.parked = false;
            return false;
        };
        if !self.ensure_native_page(ix, window, cx) {
            return false;
        }
        let sites = self.state.ui_settings.browser_allowed_sites.clone();
        let tab = &mut self.browser.tabs[ix];
        let agent = tab.agent.is_some();
        tab.parked = false;
        if let Some(native) = &tab.native {
            native.set_background(agent);
            native.set_agent(agent, &sites);
            native.load(&url);
        }
        tracing::info!(target: "keel_ui::browser", url = ?tab.web_url, "idle page reloaded");
        tab.loading(url);
        self.pump_load(ix, window, cx);
        cx.notify();
        true
    }

    fn no_page_error() -> String {
        "no page is loaded in the browser pane; call browser_navigate first".into()
    }

    /// Show a local file (a transcript chip) in the pane, in its own tab
    /// (the one already showing it, if any): `html` is a self-contained
    /// page for it. Opens the pane if it is closed.
    pub(super) fn browser_preview(
        &mut self,
        path: std::path::PathBuf,
        html: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.browser_allowed() {
            return;
        }
        let b = &mut self.browser;
        let ix = match b
            .tabs
            .iter()
            .position(|t| t.preview.as_ref() == Some(&path))
        {
            Some(ix) => ix,
            // An empty tab that isn't the agent's is taken as is.
            None if b.tab().page.url.is_none() && b.tab().agent.is_none() => b.active,
            None => b.push_tab(),
        };
        if !self.ensure_native_page(ix, window, cx) {
            return;
        }
        let tab = &mut self.browser.tabs[ix];
        if let Some(native) = &tab.native {
            native.load_html(&html);
        }
        tab.preview = Some(path);
        tab.loading(crate::files::file_url(
            tab.preview.as_ref().expect("set above"),
        ));
        self.pump_load(ix, window, cx);
        if !self.browser.open {
            self.toggle_browser(window, cx);
        }
        self.activate_tab(ix, window, cx);
        cx.notify();
    }

    /// Tab `ix`'s native page, created on first use. False (with the tab
    /// showing why) when it can't start.
    pub(super) fn ensure_native_page(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let tab = &mut self.browser.tabs[ix];
        if tab.native.is_some() {
            return true;
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        match crate::browser::NativePage::new(window, tx, f32::from(RADIUS_LG) - 1.) {
            Ok(native) => {
                let id = tab.id;
                tab.native = Some(std::rc::Rc::new(native));
                tab._events = Some(cx.spawn_in(window, async move |this, cx| {
                    while let Some(event) = rx.recv().await {
                        if this
                            .update_in(cx, |this, window, cx| {
                                this.on_browser_event(id, event, window, cx)
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
                true
            }
            Err(e) => {
                tab.error = Some(format!("The browser couldn't start: {e}"));
                cx.notify();
                false
            }
        }
    }

    /// Spin CEF until this tab stops loading. One `DoMessageLoopWork` only
    /// starts the request; without a follow-up the document never commits
    /// and the bar stays up. Agent navigations previously did not do this.
    fn pump_load(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(native) = self.browser.tabs.get(ix).and_then(|t| t.native.clone()) else {
            return;
        };
        self.browser.tabs[ix]._pump = Some(cx.spawn_in(window, async move |this, cx| {
            // About 20s. After that the external timer keeps the loop alive.
            for _ in 0..1200 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                native.spin_soon();
                let keep_going = this
                    .update(cx, |this, _| {
                        crate::dev_control::redraw();
                        this.browser
                            .tabs
                            .get(ix)
                            .map(|t| t.page.loading)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if !keep_going {
                    for _ in 0..30 {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(16))
                            .await;
                        native.spin_soon();
                        let _ = this.update(cx, |_, _| {
                            crate::dev_control::redraw();
                        });
                    }
                    break;
                }
            }
        }));
    }

    /// Load `url` in the shown tab (the user typed it or picked a chip).
    pub(super) fn browser_load(
        &mut self,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browser
            .address
            .update(cx, |s, cx| s.set_value(url.clone(), window, cx));
        self.browser.tab_mut().preview = None;
        let ix = self.browser.active;
        if !self.ensure_native_page(ix, window, cx) {
            return;
        }
        let b = &mut self.browser;
        let tab = b.tab_mut();
        if let Some(native) = &tab.native {
            native.load(&url);
        }
        tab.parked = false;
        tab.loading(url);
        let focus = b.focus.clone();
        self.pump_load(ix, window, cx);
        // Off the field, so the page can take keys once clicked.
        window.focus(&focus, cx);
        cx.notify();
    }

    fn on_browser_event(
        &mut self,
        tab_id: u64,
        event: crate::browser::NativeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::browser::NativeEvent;
        let Some(ix) = self.browser.tab_ix(tab_id) else {
            return;
        };
        let Some(native) = self.browser.tabs[ix].native.clone() else {
            return;
        };
        let is_agent = self.browser.tabs[ix].agent.is_some();
        let shown = ix == self.browser.active;
        // Script results, dialogs, file choosers, console: the agent side.
        let mut event = Some(event);
        self.on_agent_event(ix, &mut event, window, cx);
        let Some(event) = event else {
            return;
        };
        if self.browser.tab_ix(tab_id) != Some(ix) {
            return;
        }
        match event {
            NativeEvent::FrameReady => {
                if native.update_frame() {
                    cx.notify();
                    #[cfg(target_os = "macos")]
                    crate::dev_control::redraw();
                }
            }
            NativeEvent::Wake => {
                // The event handler is inside the window borrow. Pumping
                // here lets Chromium re-enter that borrow and abort.
                native.spin_soon();
            }
            NativeEvent::Changed => {
                let mut state = native.state();
                let b = &mut self.browser;
                let address = b.address.clone();
                let tab = &mut b.tabs[ix];
                // A file preview reads as its file, not `about:blank`; another
                // page, once loaded, ends the preview.
                let blank = state.url.as_deref().is_none_or(|u| u == "about:blank");
                if let Some(path) = &tab.preview {
                    if blank {
                        state.url = Some(crate::files::file_url(path));
                    } else if !state.loading {
                        tab.preview = None;
                    }
                }
                if state.url.is_none() {
                    state.url = tab.page.url.clone();
                }
                let (_, to, n) = tab.bar;
                if state.loading && !tab.page.loading {
                    tab.bar = (0., state.progress.max(0.08), n + 1);
                    tab.bar_done = false;
                } else if state.loading && state.progress > to {
                    tab.bar = (to, state.progress, n + 1);
                } else if !state.loading && tab.page.loading {
                    tab.bar = (to, 1., n + 1);
                    tab.bar_done = true;
                }
                if shown
                    && !address.read(cx).focus_handle(cx).is_focused(window)
                    && let Some(url) = state.url.clone()
                    && address.read(cx).value() != url.as_str()
                {
                    address.update(cx, |s, cx| s.set_value(url, window, cx));
                }
                if tab.preview.is_none()
                    && let Some(url) = state
                        .url
                        .as_deref()
                        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                {
                    tab.web_url = Some(url.to_string());
                }
                let loaded = !state.loading;
                tab.page = state;
                if loaded {
                    self.agent_page_loaded(ix, window, cx);
                }
                cx.notify();
                #[cfg(target_os = "macos")]
                crate::dev_control::redraw();
            }
            // A link that asks for a new window: a new tab, shown.
            NativeEvent::Open(url) => {
                // An agent's page keeps to its own tab: the link opens there.
                if is_agent {
                    native.load(&url);
                    self.browser.tabs[ix].loading(url);
                    self.pump_load(ix, window, cx);
                    return cx.notify();
                }
                let ix = self.browser.push_tab();
                if !self.ensure_native_page(ix, window, cx) {
                    return;
                }
                let tab = &mut self.browser.tabs[ix];
                if let Some(native) = &tab.native {
                    native.load(&url);
                }
                tab.loading(url);
                self.pump_load(ix, window, cx);
                self.activate_tab(ix, window, cx);
            }
            NativeEvent::Key(key) => {
                native.focus_parent();
                window.focus(&self.browser.focus, cx);
                window.defer(cx, move |window, cx| {
                    window.dispatch_keystroke(key, cx);
                });
            }
            NativeEvent::PageSize(raw) => {
                let Some(mut shot) = self.take_shot(ix) else {
                    return;
                };
                match raw.and_then(|body| parse_page_size(&body)) {
                    Ok(full) => {
                        // Frame one at the top, headers and all.
                        shot.full = Some(full);
                        shot.asked_y = 0.;
                        native.request_frame_scroll(0.);
                        self.put_shot(ix, shot);
                    }
                    Err(err) => self.shot_failed(ix, err, window, cx),
                }
            }
            NativeEvent::Scrolled(raw) => {
                let Some(mut shot) = self.take_shot(ix) else {
                    return;
                };
                let y = raw.and_then(|body| {
                    parse_json_object(&body)
                        .and_then(|v| v["y"].as_f64())
                        .ok_or_else(|| format!("the page scroll script returned {body:?}"))
                });
                match y {
                    Ok(y) => {
                        shot.at_y = y;
                        let hide = !shot.frames.is_empty();
                        self.put_shot(ix, shot);
                        let page = std::rc::Rc::downgrade(&native);
                        cx.spawn(async move |_, cx| {
                            cx.background_executor().timer(FRAME_SETTLE).await;
                            cx.update(|_| {
                                if let Some(page) = page.upgrade() {
                                    if hide {
                                        page.hide_fixed();
                                    }
                                    page.request_screenshot(true);
                                }
                            });
                        })
                        .detach();
                    }
                    Err(err) => {
                        // The job restores the page as it fails.
                        self.put_shot(ix, shot);
                        self.shot_failed(ix, err, window, cx);
                    }
                }
            }
            NativeEvent::Screenshot(raw) => {
                let Some(shot) = self.take_shot(ix) else {
                    return;
                };
                if shot.full_page && shot.full.is_none() {
                    // A stray viewport frame before the full page's prep.
                    self.put_shot(ix, shot);
                    return;
                }
                let mut shot = shot;
                let mut raw = Some(raw);
                if let Some(full) = shot.full {
                    match raw.take().expect("raw") {
                        Ok((png, _)) => shot.frames.push((png, shot.at_y)),
                        Err(err) => {
                            self.put_shot(ix, shot);
                            return self.shot_failed(ix, err, window, cx);
                        }
                    }
                    // Next frame a viewport down, until the cap or the bottom
                    // (the page stopped short of the y asked for).
                    let next = shot.at_y + full.vh.max(1.);
                    let bottom = shot.at_y + 0.5 < shot.asked_y;
                    if next < full.h && !bottom && shot.frames.len() < 64 {
                        shot.asked_y = next;
                        native.request_frame_scroll(next);
                        self.put_shot(ix, shot);
                        return;
                    }
                    native.finish_full_page(full.sx, full.sy);
                }
                let page = &self.browser.tabs[ix].page;
                let (url, title, open) = (page.url.clone(), page.title.clone(), self.browser.open);
                let dir = self.state.data_dir.join("screenshots");
                let frames = std::mem::take(&mut shot.frames);
                let full = shot.full;
                cx.spawn(async move |_, cx| {
                    let result = match (raw, full) {
                        (_, Some(full)) => {
                            cx.background_spawn(async move {
                                stitch(&frames, full)
                                    .and_then(|img| shot_image_files(img, &dir, Some(full)))
                            })
                            .await
                        }
                        (Some(Ok((png, width))), None) => {
                            cx.background_spawn(async move { shot_files(&png, width, &dir, None) })
                                .await
                        }
                        (Some(Err(err)), None) => Err(err),
                        (None, None) => Err("no screenshot".into()),
                    };
                    let result = result.map(|files| {
                        serde_json::json!({
                            "url": url,
                            "title": title,
                            "text": files.describe(shot.full),
                            "open": open,
                            "saved_path": files.path.to_string_lossy(),
                            "image_base64": files.agent_base64,
                            "image_mime": files.agent_mime,
                        })
                    });
                    let _ = shot.client.browser_complete(&shot.request_id, result).await;
                })
                .detach();
                self.shot_handed_off(ix, window, cx);
            }
            // Handled on the agent side.
            NativeEvent::Eval(..)
            | NativeEvent::Dialog { .. }
            | NativeEvent::FilePicker(_)
            | NativeEvent::Console(_)
            | NativeEvent::Blocked(_) => {}
        }
    }

    /// ⌘⇧W and the toolbar's power button: tear every tab down (pages,
    /// processes, history, anything playing) and go back to one tab on the
    /// start screen. The pane stays open. Cookies live in WebKit's store
    /// and survive this.
    pub(super) fn end_browser_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.release_focus();
        for ix in 0..self.browser.tabs.len() {
            self.drop_agent_tab(ix, "the user ended the browser session", cx);
        }
        let b = &mut self.browser;
        // Dropping a page removes its native view; the frame callbacks hold
        // it weakly, so nothing outlives this.
        b.tabs.clear();
        b.active = b.push_tab();
        b.sessions += 1;
        b.address.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    /// ⌘R and the toolbar button: reload, or stop a load in progress.
    pub(super) fn browser_reload(&mut self, cx: &mut Context<Self>) {
        if let Some(native) = &self.browser.tab().native {
            if self.browser.tab().page.loading {
                native.stop();
            } else {
                native.reload();
            }
        }
        cx.notify();
    }

    /// ⌘[ / ⌘] and the toolbar arrows.
    pub(super) fn browser_history(&mut self, forward: bool) {
        if let Some(native) = &self.browser.tab().native {
            native.history(forward);
        }
    }

    /// The pane's full width for this window: the dragged width or a share of
    /// the window, always leaving the chat its minimum.
    pub(super) fn browser_width(&self, window: &Window) -> f32 {
        let viewport = f32::from(window.viewport_size().width);
        let sidebar = if self.state.ui_settings.sidebar_collapsed {
            0.
        } else {
            self.state.sidebar_width()
        };
        let max = (viewport - sidebar - MIN_CHAT).max(MIN_WIDTH);
        // Whole pixels. A fractional width snaps back and forth in the
        // compositor and the edge shivers while you drag.
        self.browser
            .width
            .unwrap_or(viewport * DEFAULT_SHARE)
            .clamp(MIN_WIDTH, max)
            .round()
    }

    /// Dev: set the pane width (None: the default share); returns the old one.
    pub(super) fn dev_set_browser_width(&mut self, width: Option<f32>) -> Option<f32> {
        std::mem::replace(&mut self.browser.width, width)
    }

    /// Grab the pane's left edge. The pointer keeps its offset inside the
    /// handle, and the page bitmap scales until the drag ends.
    pub(super) fn begin_browser_resize(&mut self, x: f32, window: &Window) {
        let viewport = f32::from(window.viewport_size().width);
        let edge = viewport - self.browser_width(window);
        self.browser.resize_grab = x - edge;
        self.browser.resizing = true;
        if let Some(page) = self.browser.active_native_page() {
            page.set_resizing(true);
        }
    }

    pub(super) fn end_browser_resize(&mut self) {
        if !self.browser.resizing {
            return;
        }
        self.browser.resizing = false;
        if let Some(page) = self.browser.active_native_page() {
            page.set_resizing(false);
        }
    }

    /// A pane-edge drag: the pane's left edge follows the pointer.
    pub(super) fn drag_browser_edge(&mut self, x: f32, window: &Window) {
        let viewport = f32::from(window.viewport_size().width);
        self.browser.width = Some(viewport - (x - self.browser.resize_grab));
    }

    /// ⌘⇧F and the toolbar button: the pane grows over the whole window
    /// (sidebar and chat stay laid out beneath it), or back to its side.
    pub(super) fn toggle_browser_expand(&mut self, cx: &mut Context<Self>) {
        if !self.browser_allowed() || !self.browser.open || self.right_panel_toggling() {
            return;
        }
        let current = self.browser_expanded_fraction();
        let reduced = self.reduced();
        let b = &mut self.browser;
        b.expanded = !b.expanded;
        b.expand_from = current;
        let n = b.expand_anim.map_or(1, |(_, n)| n + 1);
        b.expand_anim = Some((Instant::now(), n));
        if reduced {
            b.expand_anim = None;
            for tab in &b.tabs {
                if let Some(page) = &tab.native {
                    page.set_resizing(false);
                }
            }
            cx.notify();
            return;
        }
        for tab in &b.tabs {
            if let Some(page) = &tab.native {
                page.set_resizing(true);
            }
        }
        let duration = self.browser_expand_duration();
        let generation = n;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .browser
                    .expand_anim
                    .is_some_and(|(_, n)| n == generation)
                {
                    this.browser.expand_anim = None;
                    for tab in &this.browser.tabs {
                        if let Some(page) = &tab.native {
                            page.set_resizing(false);
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// How expanded the browser pane is right now (0.0 side panel, 1.0 fullscreen).
    pub(super) fn browser_expanded_fraction(&self) -> f32 {
        if self.reduced() {
            return if self.browser.expanded { 1. } else { 0. };
        }
        let Some((at, _)) = self.browser.expand_anim else {
            return if self.browser.expanded { 1. } else { 0. };
        };
        let target = if self.browser.expanded { 1. } else { 0. };
        let dur = expand_duration(self.browser.expand_from, target);
        let elapsed = self.slide_elapsed(at);
        expand_progress(self.browser.expand_from, target, elapsed, dur)
    }

    /// The duration for the browser expand/restore animation, scaling with distance if reversed mid-flight.
    pub(super) fn browser_expand_duration(&self) -> Duration {
        let target = if self.browser.expanded { 1. } else { 0. };
        expand_duration(self.browser.expand_from, target)
    }

    /// Whether the browser pane is currently animating between side panel and fullscreen.
    pub(super) fn browser_expanding(&self) -> bool {
        !self.reduced()
            && self
                .browser
                .expand_anim
                .is_some_and(|(at, _)| self.slide_elapsed(at) < self.browser_expand_duration())
    }

    /// Two pieces: an in-flow spacer that makes room beside the chat, and the
    /// pane itself, absolute over the window's right edge. Splitting them lets
    /// the pane grow over everything without the chat reflowing underneath.
    pub(super) fn render_browser_panel(
        &mut self,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.follow_chat_tab(window, cx);
        // Outside a chat session the pane and its native page stay hidden;
        // `open` is kept, so returning to the chat brings it back.
        let allowed = self.browser_allowed();
        let open = self.browser.open && allowed;
        let toggling = !self.reduced()
            && self
                .browser
                .anim
                .is_some_and(|(at, _)| self.slide_elapsed(at) < self.right_panel_duration());
        for (ix, tab) in self.browser.tabs.iter().enumerate() {
            if let Some(native) = &tab.native {
                let shown = ix == self.browser.active
                    && (open || toggling)
                    && tab.page.url.is_some()
                    && tab.error.is_none();
                native.set_shown(shown);
            }
        }
        if !open && !toggling {
            return Vec::new();
        }
        // A grow runs only on its own; an open or close takes precedence.
        let expanding = self.browser_expanding();
        let fraction = self.browser_expanded_fraction();
        let expanded = self.browser.expanded;
        let viewport = f32::from(window.viewport_size().width);
        let full = self.browser_width(window);
        let current_w = (full + (viewport - full) * fraction).round();

        // Expanded, the toolbar clears the traffic lights it now sits under.
        let (pad_side, pad_full) = (10., f32::from(self.traffic_lights_inset()));
        let pad_left = (pad_side + (pad_full - pad_side) * fraction).round();
        let toolbar = self
            .render_browser_toolbar(t, window, cx)
            .pl(px(pad_left))
            .into_any_element();

        // Sliding, the body keeps its settled width and is clipped (nothing
        // reflows); growing, it fills the pane so the page reflows with it.
        let panel_entering =
            !self.right_panel_toggling() || self.right_panel_target() >= self.right_panel_from;
        let body = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(t.window))
            .border_l_1()
            .border_color(if expanded && !expanding {
                gpui::transparent_black()
            } else {
                c(t.border)
            })
            .child(toolbar)
            .children(self.render_browser_tabs(t, cx))
            .child(self.render_browser_page(t, panel_entering, cx));
        let pane = div()
            .id("browser-panel")
            .track_focus(&self.browser.focus)
            .key_context(crate::keys::BROWSER)
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(current_w))
            .child(body)
            .when(!expanded && !expanding, |d| {
                d.child(self.render_browser_edge(t, cx))
            });

        let anim = self.browser.anim.filter(|_| toggling);
        let pane = if let Some((_, n)) = anim {
            use gpui::AnimationExt as _;
            let (from, to) = (self.right_panel_from, self.right_panel_target());
            let entering = to >= from;
            let curve = gpui::Animation::new(self.right_panel_duration())
                .with_easing(crate::motion::glide_curve);
            pane.with_animation(("browser-slide", n as usize), curve, move |el, delta| {
                let k = from + (to - from) * delta;
                el.right(px(-full * (1.0 - k))).opacity(reveal(k, entering))
            })
            .into_any_element()
        } else if open {
            pane.right_0().into_any_element()
        } else {
            return Vec::new();
        };

        let spacer = if anim.is_some() {
            // Full width at once; the pane sliding over it is the motion.
            div()
                .h_full()
                .flex_none()
                .w(px(full * self.right_panel_target()))
                .into_any_element()
        } else if open {
            div().h_full().flex_none().w(px(full)).into_any_element()
        } else {
            return Vec::new();
        };

        vec![spacer, pane]
    }

    /// The drag handle on the pane's left edge.
    pub(super) fn render_browser_edge(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.browser.resizing;
        div()
            .id("browser-resize-handle")
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(zpx(6.))
            .cursor_col_resize()
            .child(
                div()
                    .w(zpx(2.))
                    .h_full()
                    .when(active, |d| d.bg(c(t.accent)))
                    .when(!active, |d| d.hover(|s| s.bg(c(t.border)))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        this.browser.width = None;
                    } else {
                        this.begin_browser_resize(f32::from(event.position.x), window);
                    }
                    cx.notify();
                }),
            )
    }

    fn render_browser_toolbar(
        &mut self,
        t: Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let page = &self.browser.tab().page;
        let has_page = page.url.is_some();
        let nav = |id: &'static str, icon: Icon, enabled: bool| {
            icon_button(id, icon, t).when(!enabled, |b| b.opacity(0.35).cursor_default())
        };
        let address_focused = self
            .browser
            .address
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let address = div()
            .flex_1()
            .min_w_0()
            .h(zpx(28.))
            .px_2()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(6.))
            .rounded(RADIUS_MD)
            .bg(c(t.input))
            .border_1()
            .border_color(c(if address_focused { t.focus } else { t.border }))
            .t_small()
            .child(
                self.tip_target(
                    div()
                        .id("browser-address-globe")
                        .flex_none()
                        .text_color(c(t.text_faint))
                        .child(Icon::Globe.el(zpx(12.))),
                    "browser-address-globe",
                    cx,
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(self.browser.address.clone()),
            );
        div()
            .relative()
            .h(HEADER_HEIGHT)
            .flex_none()
            .pr_2()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(self.tip_target(
                nav("browser-back", Icon::Back, page.can_back).on_click(
                    cx.listener(|this, _: &ClickEvent, _, _| this.browser_history(false)),
                ),
                "browser-back",
                cx,
            ))
            .child(
                self.tip_target(
                    nav("browser-forward", Icon::Forward, page.can_forward).on_click(
                        cx.listener(|this, _: &ClickEvent, _, _| this.browser_history(true)),
                    ),
                    "browser-forward",
                    cx,
                ),
            )
            .child(
                self.tip_target(
                    nav(
                        "browser-reload",
                        if page.loading {
                            Icon::Close
                        } else {
                            Icon::Reload
                        },
                        has_page,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.browser_reload(cx))),
                    "browser-reload",
                    cx,
                ),
            )
            .child(address)
            .child(self.tip_target(
                icon_button("browser-new-tab", Icon::Plus, t).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.browser_new_tab(window, cx)
                    }),
                ),
                "browser-new-tab",
                cx,
            ))
            .child(self.tip_target(
                nav("browser-external", Icon::External, has_page).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        if let Some(path) = this.browser.tab().preview.clone() {
                            cx.open_with_system(&path);
                        } else if let Some(url) = this.browser.tab().page.url.clone() {
                            cx.open_url(&url);
                        }
                    },
                )),
                "browser-external",
                cx,
            ))
            .child(
                self.tip_target(
                    div()
                        .id("browser-end")
                        .size(zpx(26.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(RADIUS_MD)
                        .text_color(c(t.text_muted))
                        .when(!has_page, |b| b.opacity(0.35).cursor_default())
                        .when(has_page, |b| {
                            b.cursor_pointer()
                                .hover(|s| s.bg(c(t.hover)).text_color(c(t.danger)))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.end_browser_session(window, cx)
                                }))
                        })
                        .child(Icon::Close.el(zpx(15.))),
                    "browser-end",
                    cx,
                ),
            )
            .child(
                div()
                    .h(zpx(14.))
                    .w(zpx(1.))
                    .mx(zpx(2.))
                    .flex_none()
                    .bg(c(t.border)),
            )
            .child(
                self.tip_target(
                    icon_button(
                        "browser-expand",
                        if self.browser.expanded {
                            Icon::Shrink
                        } else {
                            Icon::Expand
                        },
                        t,
                    )
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_browser_expand(cx)),
                    ),
                    "browser-expand",
                    cx,
                ),
            )
            .child(self.tip_target(
                icon_button("browser-close", Icon::SidebarClose, t).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_browser(window, cx)),
                ),
                "browser-close",
                cx,
            ))
            .children(self.render_browser_bar(t))
            .children(self.render_browser_tip(t, window))
    }

    /// What each toolbar button does, as its tooltip says it (state-aware),
    /// with its shortcut where it has one.
    fn browser_tip_label(&self, id: &str) -> Option<(String, Option<&'static str>)> {
        use super::tooltip::keys;
        let tab = self.browser.tab();
        let (text, key) = match id {
            "browser-back" => ("Back", keys("⌘[")),
            "browser-forward" => ("Forward", keys("⌘]")),
            // The same button and ⌘R stop a load in progress.
            "browser-reload" if tab.page.loading => ("Stop loading", keys("⌘R")),
            "browser-reload" => ("Reload", keys("⌘R")),
            "browser-address-globe" => ("Search or enter address", keys("⌘L")),
            "browser-new-tab" => ("New tab", keys("⌘T")),
            // A previewed file opens in its own app; a page in the default browser.
            "browser-external" if tab.preview.is_some() => ("Open in default app", None),
            "browser-external" => ("Open in browser", None),
            // Closes every tab (and the agent's) for a fresh, empty one.
            "browser-end" => ("Close session", keys("⌘⇧W")),
            "browser-expand" if self.browser.expanded => ("Restore", keys("⌘⇧F")),
            "browser-expand" => ("Expand", keys("⌘⇧F")),
            "browser-close" => ("Hide browser", keys("⌘⇧B")),
            _ => return None,
        };
        Some((text.to_string(), key))
    }

    fn render_browser_tip(&mut self, t: Tokens, window: &Window) -> Option<AnyElement> {
        let reduced = self.reduced();
        let mut tips = std::mem::take(&mut self.browser.tips);
        let el = tips.render(|id| self.browser_tip_label(id), t, window, reduced);
        self.browser.tips = tips;
        el
    }

    /// A toolbar button that shows a tooltip on hover: its bounds are
    /// recorded for placing it, and hover and press feed [`HoverTips`].
    /// Disabled buttons too: they still say what they'd do.
    fn tip_target(
        &self,
        button: gpui::Stateful<gpui::Div>,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let bounds = self.browser.tips.bounds.clone();
        button
            .relative()
            .child(
                gpui::canvas(
                    move |b, _, _| {
                        bounds.borrow_mut().insert(id, b);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                let now = Instant::now();
                if let Some(delay) = this.browser.tips.hover(id, *on, now) {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(delay).await;
                        let _ = this.update(cx, |this, cx| {
                            if this.browser.tips.ripen(id, now, Instant::now()) {
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.browser.tips.press(id, Instant::now());
                    cx.notify();
                }),
            )
    }

    /// A thin accent line under the toolbar: glides to each progress step,
    /// then fills and fades when the load finishes.
    fn render_browser_bar(&self, t: Tokens) -> Option<AnyElement> {
        let b = self.browser.tab();
        if !b.page.loading && !b.bar_done {
            return None;
        }
        let (from, to, n) = b.bar;
        let done = b.bar_done;
        let line = div()
            .absolute()
            .left_0()
            .bottom_0()
            .h(px(2.))
            .rounded_r(px(1.))
            .bg(c(t.accent));
        if self.reduced() {
            return (!done).then(|| line.w(relative(to)).into_any_element());
        }
        let duration = if done { BASE * 2 } else { SLOW };
        Some(
            line.with_animation(
                ElementId::Name(format!("browser-bar-{}-{n}", b.id).into()),
                gpui::Animation::new(duration).with_easing(ease_out),
                move |el, d| {
                    el.w(relative(from + (to - from) * d)).opacity(if done {
                        1. - ease_in(d)
                    } else {
                        1.
                    })
                },
            )
            .into_any_element(),
        )
    }

    /// The tab strip, once there are two tabs: titles share the width (up
    /// to [`TAB_MAX_W`] each), the shown tab raised, the agent's marked
    /// with a dot. A tab's × shows on hover (always on the shown one); a
    /// new tab fades in.
    fn render_browser_tabs(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let reduced = self.reduced();
        let b = &self.browser;
        let chat = self.current_chat.as_deref();
        if b.tabs
            .iter()
            .filter(|t| b.visible_in_strip(t, chat))
            .count()
            < 2
        {
            return None;
        }
        let tabs = b
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| b.visible_in_strip(tab, chat))
            .map(|(ix, tab)| {
                let (id, active) = (tab.id, ix == b.active);
                let group = format!("browser-tab-{id}");
                let close = div()
                    .id(("browser-tab-close", id as usize))
                    .flex_none()
                    .size(zpx(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(zpx(4.))
                    .text_color(c(t.text_faint))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                    .when(!active, |d| {
                        d.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
                    })
                    .child(Icon::Close.el(zpx(10.)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if let Some(ix) = this.browser.tab_ix(id) {
                            this.browser_close_tab(ix, window, cx);
                        }
                    }));
                let dot = tab
                    .agent
                    .is_some()
                    .then(|| div().flex_none().size(zpx(6.)).rounded_full().bg(c(t.info)));
                let el = div()
                    .id(("browser-tab", id as usize))
                    .group(group)
                    .flex_1()
                    .min_w_0()
                    .max_w(zpx(TAB_MAX_W))
                    .h(zpx(24.))
                    .pl_2()
                    .pr_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .rounded(RADIUS_MD)
                    .border_1()
                    .t_small()
                    .map(|d| {
                        if active {
                            d.bg(c(t.surface))
                                .border_color(c(t.border))
                                .text_color(c(t.text))
                        } else {
                            d.border_color(gpui::transparent_black())
                                .text_color(c(t.text_muted))
                                .cursor_pointer()
                                .hover(|s| s.bg(c(t.hover)))
                        }
                    })
                    .children(dot)
                    .child(div().flex_1().min_w_0().truncate().child(tab.label()))
                    .child(close)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if let Some(ix) = this.browser.tab_ix(id) {
                            this.activate_tab(ix, window, cx);
                        }
                    }));
                crate::motion::stagger_in(el, ("browser-tab-in", id as usize), 0, reduced)
            });
        Some(
            div()
                .flex_none()
                .h(zpx(TAB_STRIP_H))
                .px(px(PAGE_INSET))
                .pb(zpx(4.))
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .children(tabs)
                .into_any_element(),
        )
    }

    /// The page card: the live page, or a start screen until there's an address.
    fn render_browser_page(
        &mut self,
        t: Tokens,
        _entering: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let card = div()
            .id("browser-page")
            .relative()
            .flex_1()
            .min_h_0()
            .mx(px(PAGE_INSET))
            .mb(px(PAGE_INSET))
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(c(t.border))
            .overflow_hidden()
            .bg(c(t.surface));
        if let Some(error) = self.browser.tab().error.clone() {
            return card.child(self.render_browser_empty(t, Some(error), cx));
        }
        if self.browser.tab().page.url.is_none() {
            return card.child(self.render_browser_empty(t, None, cx));
        }
        let card = match self.browser.tab().native.clone() {
            Some(native) => {
                let page_prep = std::rc::Rc::downgrade(&native);
                let page_paint = std::rc::Rc::downgrade(&native);
                let focus = self.browser.focus.clone();
                card.child(
                    div()
                        .id("browser-viewport")
                        .relative()
                        .size_full()
                        .track_focus(&focus)
                        .key_context("Browser")
                        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                            if let Some(tab) = this.browser.tabs.get(this.browser.active)
                                && let Some(native) = &tab.native
                            {
                                if native.handle_key_down(event) {
                                    cx.stop_propagation();
                                }
                            }
                        }))
                        .capture_key_up(cx.listener(|this, event: &KeyUpEvent, _window, cx| {
                            if let Some(tab) = this.browser.tabs.get(this.browser.active)
                                && let Some(native) = &tab.native
                            {
                                if native.handle_key_up(event) {
                                    cx.stop_propagation();
                                }
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                window.focus(&this.browser.focus, cx);
                                if let Some(tab) = this.browser.tabs.get(this.browser.active)
                                    && let Some(native) = &tab.native
                                {
                                    native.handle_mouse_down(event.position, MouseButton::Left);
                                }
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                window.focus(&this.browser.focus, cx);
                                if let Some(tab) = this.browser.tabs.get(this.browser.active)
                                    && let Some(native) = &tab.native
                                {
                                    native.handle_mouse_down(event.position, MouseButton::Right);
                                }
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Middle,
                            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                window.focus(&this.browser.focus, cx);
                                if let Some(tab) = this.browser.tabs.get(this.browser.active)
                                    && let Some(native) = &tab.native
                                {
                                    native.handle_mouse_down(event.position, MouseButton::Middle);
                                }
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
                                if let Some(tab) = this.browser.tabs.get(this.browser.active)
                                    && let Some(native) = &tab.native
                                {
                                    native.handle_mouse_up(event.position, MouseButton::Left);
                                }
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Right,
                            cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
                                if let Some(tab) = this.browser.tabs.get(this.browser.active)
                                    && let Some(native) = &tab.native
                                {
                                    native.handle_mouse_up(event.position, MouseButton::Right);
                                }
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Middle,
                            cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
                                if let Some(tab) = this.browser.tabs.get(this.browser.active)
                                    && let Some(native) = &tab.native
                                {
                                    native.handle_mouse_up(event.position, MouseButton::Middle);
                                }
                            }),
                        )
                        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, _cx| {
                            if let Some(tab) = this.browser.tabs.get(this.browser.active)
                                && let Some(native) = &tab.native
                            {
                                native.handle_mouse_move(event.position);
                            }
                        }))
                        .on_scroll_wheel(cx.listener(
                            |this, event: &ScrollWheelEvent, _window, _cx| {
                                if let Some(tab) = this.browser.tabs.get(this.browser.active)
                                    && let Some(native) = &tab.native
                                {
                                    native.handle_scroll_wheel(event.position, event.delta);
                                }
                            },
                        ))
                        .child(
                            gpui::canvas(
                                move |bounds, window, _cx| {
                                    if let Some(page) = page_prep.upgrade() {
                                        let scale = window.scale_factor();
                                        page.set_scale_factor(scale);
                                        page.sync(bounds, bounds, 1.0, false);
                                    }
                                },
                                move |bounds, _, window, _cx| {
                                    if let Some(page) = page_paint.upgrade() {
                                        if let Some(old) = page.take_dropped_frame() {
                                            let _ = window.drop_image(old);
                                        }
                                        if let Some(img) = page.render_frame() {
                                            let _ = window.paint_image(
                                                bounds,
                                                Default::default(),
                                                img,
                                                0,
                                                false,
                                            );
                                        }
                                    }
                                },
                            )
                            .absolute()
                            .inset_0(),
                        ),
                )
            }
            None => card,
        };
        card
    }

    fn render_browser_empty(
        &mut self,
        t: Tokens,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chips = QUICK.iter().enumerate().map(|(i, addr)| {
            div()
                .id(("browser-quick", i))
                .h(zpx(26.))
                .px_3()
                .flex()
                .items_center()
                .rounded_full()
                .border_1()
                .border_color(c(t.border))
                .t_small()
                .font_family(crate::style::MONO)
                .text_color(c(t.text_muted))
                .cursor_pointer()
                .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                .child(*addr)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if let Some(url) = normalize_address(addr) {
                        this.browser_load(url, window, cx);
                    }
                }))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .text_color(c(if error.is_some() {
                        t.warning
                    } else {
                        t.text_faint
                    }))
                    .child(
                        if error.is_some() {
                            Icon::Alert
                        } else {
                            Icon::Globe
                        }
                        .el(zpx(26.)),
                    ),
            )
            .child(
                div()
                    .max_w(zpx(280.))
                    .text_center()
                    .t_body()
                    .text_color(c(t.text_muted))
                    .child(error.unwrap_or_else(|| {
                        "Open a page, or the app your agent is building.".into()
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .justify_center()
                    .gap_2()
                    .children(chips),
            )
    }
}

/// Given the start fraction, target, and elapsed vs total duration, compute the interpolated progress along `glide_curve`.
pub(super) fn expand_progress(from: f32, target: f32, elapsed: Duration, dur: Duration) -> f32 {
    if dur.is_zero() || elapsed >= dur {
        return target;
    }
    let t = (elapsed.as_secs_f32() / dur.as_secs_f32()).min(1.);
    from + (target - from) * crate::motion::glide_curve(t)
}

/// Compute the duration for expanding/collapsing based on distance traveled.
pub(super) fn expand_duration(from: f32, target: f32) -> Duration {
    let dist = (target - from).abs();
    SPATIAL.mul_f32(0.4 + 0.6 * dist)
}

/// [`crate::browser::cef`]'s page-size JSON → the capture, its height
/// capped at [`FULL_PAGE_MAX`].
fn parse_page_size(body: &str) -> Result<FullPage, String> {
    let value =
        parse_json_object(body).ok_or_else(|| format!("the page size script returned {body:?}"))?;
    let num = |k: &str| value[k].as_f64().filter(|n| n.is_finite()).unwrap_or(0.);
    let (w, total) = (num("w").round(), num("h").round());
    if w < 1. || total < 1. {
        return Err(format!("the page has no size ({w}x{total})"));
    }
    let vh = num("vh").round().max(1.);
    Ok(FullPage {
        w,
        h: total.min(FULL_PAGE_MAX),
        total,
        vh,
        sx: num("sx"),
        sy: num("sy"),
    })
}

/// A script's result: a JSON object, possibly JSON-encoded as a string.
fn parse_json_object(body: &str) -> Option<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| match v {
            serde_json::Value::String(s) => serde_json::from_str(&s).ok(),
            other => Some(other),
        })
        .filter(serde_json::Value::is_object)
}

/// Full-page frames (PNG at their CSS y) → one image `full.w` wide and
/// `full.h` tall (less when the page is shorter); later frames win overlaps.
fn stitch(frames: &[(Vec<u8>, f64)], full: FullPage) -> Result<image::DynamicImage, String> {
    let width = (full.w.round() as u32).max(1);
    let mut placed: Vec<(image::RgbaImage, u32)> = Vec::new();
    let mut bottom = 0u32;
    for (png, y) in frames {
        let mut img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        if img.width() != width {
            let h = ((u64::from(img.height()) * u64::from(width)) / u64::from(img.width().max(1)))
                .max(1) as u32;
            img = img.resize_exact(width, h, image::imageops::FilterType::Triangle);
        }
        let y = y.max(0.).round() as u32;
        bottom = bottom.max(y + img.height());
        placed.push((img.to_rgba8(), y));
    }
    let height = bottom.min((full.h.round() as u32).max(1));
    if placed.is_empty() || height == 0 {
        return Err("the page gave no frames".into());
    }
    let mut canvas = image::RgbaImage::new(width, height);
    for (img, y) in &placed {
        image::imageops::replace(&mut canvas, img, 0, i64::from(*y));
    }
    Ok(image::DynamicImage::ImageRgba8(canvas))
}

/// A screenshot on disk (full size, PNG) and the agent's copy.
struct ShotFiles {
    path: std::path::PathBuf,
    width: u32,
    height: u32,
    agent_base64: String,
    agent_mime: &'static str,
    agent_size: (u32, u32),
}

impl ShotFiles {
    fn describe(&self, full: Option<FullPage>) -> String {
        let copy = if self.agent_size == (self.width, self.height) {
            String::new()
        } else {
            format!(
                "; you get a {}x{} {} copy",
                self.agent_size.0,
                self.agent_size.1,
                if self.agent_mime == "image/jpeg" {
                    "JPEG"
                } else {
                    "PNG"
                }
            )
        };
        match full {
            Some(f) if f.total > f.h => format!(
                "{}x{} PNG of the full page, cut at {} of its {} CSS px height{copy}",
                self.width, self.height, f.h as u64, f.total as u64
            ),
            Some(_) => format!(
                "{}x{} PNG of the full page (1 px = 1 CSS px){copy}",
                self.width, self.height
            ),
            None => format!(
                "{}x{} PNG of the visible page (1 px = 1 CSS px when width matches the viewport){copy}",
                self.width, self.height
            ),
        }
    }
}

/// Largest agent image, base64 bytes: it rides in a 1 MiB RPC frame.
const AGENT_IMAGE_MAX: usize = 700_000;
/// Agent images stay under this many pixels on a side (model limits).
const AGENT_SIDE_MAX: u32 = 7_900;

/// Retina PNG → a 1 CSS px per pixel PNG under `dir` (cut to the capture's
/// height), plus the agent's copy: the PNG when small enough, else a
/// downscaled JPEG.
fn shot_files(
    png: &[u8],
    width: u32,
    dir: &std::path::Path,
    full: Option<FullPage>,
) -> Result<ShotFiles, String> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    shot_image_files_sized(img, width, dir, full)
}

/// A stitched full page (already 1 px per CSS px) → [`ShotFiles`].
fn shot_image_files(
    img: image::DynamicImage,
    dir: &std::path::Path,
    full: Option<FullPage>,
) -> Result<ShotFiles, String> {
    let width = img.width();
    shot_image_files_sized(img, width, dir, full)
}

fn shot_image_files_sized(
    mut img: image::DynamicImage,
    width: u32,
    dir: &std::path::Path,
    full: Option<FullPage>,
) -> Result<ShotFiles, String> {
    use base64::Engine as _;
    let target = width.clamp(
        1,
        if full.is_some() {
            u32::MAX
        } else {
            MAX_SHOT_WIDTH
        },
    );
    if img.width() > target {
        let height =
            ((u64::from(img.height()) * u64::from(target)) / u64::from(img.width())).max(1) as u32;
        img = img.resize_exact(target, height, image::imageops::FilterType::Triangle);
    }
    if let Some(f) = full {
        let cap = (f.h.round() as u32).max(1);
        if img.height() > cap {
            img = img.crop_imm(0, 0, img.width(), cap);
        }
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    img.write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let bytes = bytes.into_inner();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = dir.join(format!(
        "{stamp}{}.png",
        if full.is_some() { "-full" } else { "" }
    ));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let (width, height) = (img.width(), img.height());
    if width.max(height) <= AGENT_SIDE_MAX && bytes.len() * 4 / 3 + 4 <= AGENT_IMAGE_MAX {
        return Ok(ShotFiles {
            path,
            width,
            height,
            agent_base64: b64.encode(&bytes),
            agent_mime: "image/png",
            agent_size: (width, height),
        });
    }
    let mut scale = (f64::from(AGENT_SIDE_MAX) / f64::from(width.max(height))).min(1.);
    loop {
        let (w, h) = (
            ((f64::from(width) * scale).round() as u32).max(1),
            ((f64::from(height) * scale).round() as u32).max(1),
        );
        let small = if (w, h) == (width, height) {
            img.to_rgb8()
        } else {
            img.resize_exact(w, h, image::imageops::FilterType::Triangle)
                .to_rgb8()
        };
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 72)
            .encode_image(&small)
            .map_err(|e| e.to_string())?;
        let data = b64.encode(&jpeg);
        if data.len() <= AGENT_IMAGE_MAX || w <= 160 {
            return Ok(ShotFiles {
                path,
                width,
                height,
                agent_base64: data,
                agent_mime: "image/jpeg",
                agent_size: (w, h),
            });
        }
        scale *= 0.75;
    }
}

/// Widest screenshot sent to an agent, in pixels.
const MAX_SHOT_WIDTH: u32 = 1600;

#[cfg(test)]
mod host_tests {
    use super::*;

    fn png(w: u32, h: u32, noisy: bool) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            if noisy {
                let v = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503))
                    .wrapping_mul(2_246_822_519);
                image::Rgb([(v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8])
            } else {
                image::Rgb([200, 210, 220])
            }
        });
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn tmp() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "keel-shot-files-{}-{}",
            std::process::id(),
            std::time::Instant::now().elapsed().as_nanos()
        ))
    }

    #[test]
    fn screenshots_shrink_to_the_page_width_and_save() {
        let dir = tmp();
        let files = shot_files(&png(200, 100, false), 100, &dir, None).unwrap();
        assert_eq!((files.width, files.height), (100, 50));
        assert_eq!(files.agent_mime, "image/png");
        assert!(files.path.starts_with(&dir) && files.path.is_file());
        let saved = image::open(&files.path).unwrap();
        assert_eq!((saved.width(), saved.height()), (100, 50));
        assert!(
            files
                .describe(None)
                .starts_with("100x50 PNG of the visible page")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A tall full page: saved at full size (cut to the cap), the agent's
    /// copy a JPEG small enough for an RPC frame and a model.
    #[test]
    fn full_page_saves_png_and_sends_a_small_jpeg() {
        let dir = tmp();
        let full = parse_page_size(r#""{\"w\":400,\"h\":20000,\"vh\":800,\"sx\":0,\"sy\":1200}""#)
            .unwrap();
        assert_eq!(
            full,
            FullPage {
                w: 400.,
                h: FULL_PAGE_MAX,
                total: 20000.,
                vh: 800.,
                sx: 0.,
                sy: 1200.
            }
        );
        // Retina: twice the CSS width, a little taller than the cap.
        let files = shot_files(
            &png(800, 2 * 9000, true),
            400,
            &dir,
            Some(FullPage { h: 8000., ..full }),
        )
        .unwrap();
        assert_eq!((files.width, files.height), (400, 8000));
        assert_eq!(files.agent_mime, "image/jpeg");
        assert!(files.agent_base64.len() <= AGENT_IMAGE_MAX);
        assert!(files.agent_size.1 <= AGENT_SIDE_MAX);
        let saved = image::open(&files.path).unwrap();
        assert_eq!((saved.width(), saved.height()), (400, 8000));
        assert!(
            files
                .describe(Some(FullPage { h: 8000., ..full }))
                .contains("cut at 8000 of its 20000"),
            "{}",
            files.describe(Some(full))
        );
        assert!(parse_page_size("null").is_err());
        assert!(parse_page_size(r#"{"w":0,"h":10}"#).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Frames a viewport apart, the last clamped at the bottom, become one
    /// image: each frame's pixels land at its scroll offset.
    #[test]
    fn frames_stitch_at_their_offsets() {
        let frame = |shade: u8| {
            let img = image::RgbImage::from_pixel(100, 40, image::Rgb([shade, shade, shade]));
            let mut out = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb8(img)
                .write_to(&mut out, image::ImageFormat::Png)
                .unwrap();
            out.into_inner()
        };
        let full = FullPage {
            w: 100.,
            h: 100.,
            total: 100.,
            vh: 40.,
            sx: 0.,
            sy: 0.,
        };
        // 0, 40, then the bottom clamps the third frame to 60.
        let img = stitch(&[(frame(10), 0.), (frame(20), 40.), (frame(30), 60.)], full)
            .unwrap()
            .to_rgba8();
        assert_eq!(img.dimensions(), (100, 100));
        assert_eq!(img.get_pixel(5, 5)[0], 10);
        assert_eq!(img.get_pixel(5, 45)[0], 20);
        assert_eq!(img.get_pixel(5, 65)[0], 30);
        assert_eq!(img.get_pixel(5, 99)[0], 30);
        // Retina frames scale to the CSS width; a cap cuts the height.
        let img = stitch(
            &[(frame(10), 0.)],
            FullPage {
                w: 50.,
                h: 15.,
                ..full
            },
        )
        .unwrap();
        assert_eq!((img.width(), img.height()), (50, 15));
        assert!(stitch(&[], full).is_err());
    }

    #[test]
    fn tabs_are_named_by_title_then_file_then_host() {
        let mut tab = BrowserTab::new(1);
        assert_eq!(tab.label(), "New Tab");
        tab.page.url = Some("http://localhost:3000/a?b".into());
        assert_eq!(tab.label(), "localhost:3000");
        tab.preview = Some("/tmp/report.html".into());
        assert_eq!(tab.label(), "report.html");
        tab.page.title = "  Report  ".into();
        assert_eq!(tab.label(), "Report");
    }

    #[test]
    fn expand_duration_scales_with_travel_distance() {
        assert_eq!(expand_duration(0., 1.), SPATIAL);
        assert_eq!(expand_duration(1., 0.), SPATIAL);
        let partial = expand_duration(0.5, 0.);
        assert!(partial < SPATIAL);
        assert!((partial.as_secs_f32() - SPATIAL.mul_f32(0.7).as_secs_f32()).abs() < 1e-4);
        assert!(
            (expand_duration(0.5, 0.5).as_secs_f32() - SPATIAL.mul_f32(0.4).as_secs_f32()).abs()
                < 1e-4
        );
    }

    #[test]
    fn expand_progress_starts_smoothly_without_jumps() {
        let dur = SPATIAL;
        // Expanding from 0 to 1:
        assert_eq!(expand_progress(0., 1., Duration::ZERO, dur), 0.);
        assert_eq!(expand_progress(0., 1., dur, dur), 1.);
        assert_eq!(
            expand_progress(0., 1., dur + Duration::from_millis(50), dur),
            1.
        );

        // Mid-flight reversal from 0.4 back to 0.0 starts EXACTLY at 0.4 (no sudden jump):
        assert_eq!(expand_progress(0.4, 0., Duration::ZERO, dur), 0.4);
        assert_eq!(expand_progress(0.4, 0., dur, dur), 0.);

        // Monotonic motion during reversal:
        let p1 = expand_progress(0.4, 0., Duration::from_millis(100), dur);
        let p2 = expand_progress(0.4, 0., Duration::from_millis(200), dur);
        assert!(p1 < 0.4 && p1 > p2 && p2 > 0.);
    }
}

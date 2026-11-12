//! Terminal panel: engine-owned shells hosted inside the unified right side panel.
//! Closing the right panel detaches the view; the shells keep running.

use std::cell::Cell;
use std::rc::Rc;

use base64::Engine as _;
use gpui::{
    Bounds, ClickEvent, Context, FocusHandle, IntoElement, KeyDownEvent, Pixels, ScrollWheelEvent,
    Window, div, prelude::*, px,
};
use keel_proto::{RpcStreamItem, SubscribeEvent};
use keel_theme::Tokens;

use super::{KeelWindow, right_panel::RightTabKind};
use crate::style::{MONO, c};
use crate::terminal::{Emulator, Palette, encode_key, encode_paste};

/// Monospace cell metrics for the panel.
const FONT_SIZE: f32 = 12.5;
const LINE_HEIGHT: f32 = 17.;
/// Panel height padding around the grid.
const PAD: f32 = 12.;

/// One shell in the panel.
pub(super) struct TermTab {
    pub(super) id: String,
    /// The chat whose folder it started in.
    pub(super) chat: Option<String>,
    /// The `subscribe_terminal` stream, once known.
    stream: Option<String>,
    pub(super) emu: Emulator,
    pub(super) exited: Option<Option<i32>>,
    pub(super) busy: bool,
}

impl TermTab {
    pub(super) fn label(&self, ix: usize) -> String {
        self.emu
            .title()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("Terminal {}", ix + 1))
    }
}

/// What a new shell does once attached.
enum Then {
    Nothing,
    /// Type it and press Return.
    Run(String),
    /// Type it at the prompt, as a paste, and leave it there.
    Insert(String),
}

/// How long a new shell gets to show its prompt before an insert goes in anyway.
const PROMPT_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Panel state kept on the window.
pub(super) struct TerminalPanel {
    pub(super) tabs: Vec<TermTab>,
    pub(super) active: usize,
    pub(super) focus: FocusHandle,
    /// Output that arrived before its stream id was known, by stream.
    early: std::collections::HashMap<String, Vec<SubscribeEvent>>,
    /// The grid's on-screen bounds from the last paint, to fit cols × rows.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    cell_width: Option<f32>,
    /// A command (not just the shell) is running in one of the tabs: the
    /// header's right-panel trigger carries an accent dot.
    pub(super) busy: bool,
    /// The busy poll is running.
    polling: bool,
}

impl TerminalPanel {
    pub(super) fn new(focus: FocusHandle) -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            focus,
            early: Default::default(),
            bounds: Rc::new(Cell::new(None)),
            cell_width: None,
            busy: false,
            polling: false,
        }
    }

    pub(super) fn tab_ix(&self, id: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }
}

impl KeelWindow {
    /// ⌃`: toggle/focus a terminal tab in the right panel; starts a shell on first use.
    pub(super) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.browser_allowed() {
            return;
        }
        if self.right_panel.open
            && matches!(
                self.right_panel_active_kind(),
                Some(RightTabKind::Terminal(_))
            )
        {
            self.close_right_panel(window, cx);
            return;
        }

        let chat = self.current_chat.clone();
        let existing_ix = pick_tab(&self.terminal.tabs, self.terminal.active, &chat).or_else(|| {
            (!self.terminal.tabs.is_empty())
                .then(|| self.terminal.active.min(self.terminal.tabs.len() - 1))
        });

        if let Some(ix) = existing_ix {
            let tid = self.terminal.tabs[ix].id.clone();
            self.open_right_tab(RightTabKind::Terminal(tid), window, cx);
        } else {
            if !self.right_panel.open {
                self.open_right_panel(window, cx);
            }
            self.arm_pending_terminal_launcher();
            self.new_terminal(cx);
            window.focus(&self.terminal.focus, cx);
        }
        cx.notify();
    }

    fn arm_pending_terminal_launcher(&mut self) {
        if self.right_panel.pending_terminal_launcher.is_some() {
            return;
        }
        let active_launcher = self
            .right_panel
            .tabs
            .get(self.right_panel.active)
            .filter(|t| t.kind == RightTabKind::Launcher)
            .map(|t| t.id);
        let lone_launcher = (self.right_panel.tabs.len() == 1
            && self.right_panel.tabs[0].kind == RightTabKind::Launcher)
            .then(|| self.right_panel.tabs[0].id);
        self.right_panel.pending_terminal_launcher = active_launcher.or(lone_launcher);
    }

    /// Ask the engine every 2 s whether a command is running in one of this
    /// window's tabs (for the header dot and per-tab badge), while any tab is open.
    fn poll_terminals(&mut self, cx: &mut Context<Self>) {
        if self.terminal.polling {
            return;
        }
        self.terminal.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                let client = this.update(cx, |this, _| {
                    (!this.terminal.tabs.is_empty())
                        .then(|| this.client.clone())
                        .flatten()
                });
                let Ok(Some(client)) = client else { break };
                let rows = client
                    .call("list_terminals", serde_json::json!({}))
                    .await
                    .unwrap_or_default();
                let alive = this.update(cx, |this, cx| {
                    let list = rows["terminals"].as_array();
                    let mut changed = false;
                    let mut any_busy = false;
                    for tab in &mut this.terminal.tabs {
                        let tab_busy = list.is_some_and(|rows| {
                            rows.iter().any(|row| {
                                row["terminal_id"] == tab.id.as_str() && row["busy"] == true
                            })
                        });
                        if tab.busy != tab_busy {
                            tab.busy = tab_busy;
                            changed = true;
                        }
                        any_busy |= tab_busy;
                    }
                    if any_busy != this.terminal.busy {
                        this.terminal.busy = any_busy;
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
            }
            let _ = this.update(cx, |this, cx| {
                this.terminal.polling = false;
                let mut changed = this.terminal.busy;
                this.terminal.busy = false;
                for tab in &mut this.terminal.tabs {
                    if tab.busy {
                        tab.busy = false;
                        changed = true;
                    }
                }
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Start a shell in the active chat's folder and attach to it.
    pub(super) fn new_terminal(&mut self, cx: &mut Context<Self>) {
        self.new_terminal_running(Then::Nothing, cx);
    }

    /// Show the panel with a fresh shell that runs `command`. The user
    /// finishes it there.
    pub(super) fn run_in_terminal(
        &mut self,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.browser_allowed() {
            return;
        }
        if !self.right_panel.open {
            self.open_right_panel(window, cx);
        }
        self.arm_pending_terminal_launcher();
        window.focus(&self.terminal.focus, cx);
        self.new_terminal_running(Then::Run(command), cx);
    }

    /// Show the panel and type `text` at the prompt of this chat's shell
    /// (the active tab if it's this chat's, else its latest; a new one if
    /// none), without Return: the user reads it, then runs it.
    pub(super) fn insert_in_terminal(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.browser_allowed() {
            return;
        }
        let chat = self.current_chat.clone();
        match pick_tab(&self.terminal.tabs, self.terminal.active, &chat) {
            Some(ix) => {
                self.terminal.active = ix;
                let tab = &mut self.terminal.tabs[ix];
                let data = encode_paste(&text, bracketed(&tab.emu));
                tab.emu.scroll_to_bottom();
                let id = tab.id.clone();
                self.terminal_send(id.clone(), data, cx);
                self.open_right_tab(RightTabKind::Terminal(id), window, cx);
                window.focus(&self.terminal.focus, cx);
                cx.notify();
            }
            None => {
                if !self.right_panel.open {
                    self.open_right_panel(window, cx);
                }
                self.arm_pending_terminal_launcher();
                window.focus(&self.terminal.focus, cx);
                self.new_terminal_running(Then::Insert(text), cx);
            }
        }
    }

    fn new_terminal_running(&mut self, then: Then, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let (cols, rows) = self.terminal_fit().unwrap_or((100, 15));
        let chat_id = self.current_chat.clone();
        let expected_nav = self.right_panel.nav_seq;
        cx.spawn(async move |this, cx| {
            let opened = client
                .call(
                    "terminal_open",
                    serde_json::json!({ "chat_id": chat_id, "cols": cols, "rows": rows }),
                )
                .await;
            let id = match opened.as_ref().ok().and_then(|v| v["terminal_id"].as_str()) {
                Some(id) => id.to_string(),
                None => {
                    let _ = this.update(cx, |this, cx| {
                        this.right_panel.pending_terminal_launcher = None;
                        this.report("can't open a terminal", opened.err().unwrap_or_default());
                        cx.notify();
                    });
                    return;
                }
            };
            let focus_info = this.update(cx, |this, cx| {
                let panel = &mut this.terminal;
                panel.tabs.push(TermTab {
                    id: id.clone(),
                    chat: chat_id.clone(),
                    stream: None,
                    emu: Emulator::new(cols, rows),
                    exited: None,
                    busy: false,
                });
                let user_stayed = this.right_panel.nav_seq == expected_nav;
                if user_stayed {
                    panel.active = panel.tabs.len() - 1;
                }
                let replace_id = this.right_panel.pending_terminal_launcher.take();
                let kind = RightTabKind::Terminal(id.clone());
                if user_stayed
                    && let Some(lid) = replace_id
                    && let Some(pos) = this
                        .right_panel
                        .tabs
                        .iter()
                        .position(|t| t.id == lid && t.kind == RightTabKind::Launcher)
                {
                    this.right_panel.tabs[pos].kind = kind;
                    this.right_panel.active = pos;
                    this.right_panel.nav_seq = this.right_panel.nav_seq.wrapping_add(1);
                    this.normalize_right_tabs();
                } else {
                    this.right_ensure_kind(kind, user_stayed, user_stayed, None, cx);
                }
                this.poll_terminals(cx);
                cx.notify();
                (
                    this.browser.handle,
                    this.terminal.focus.clone(),
                    this.right_panel.open && user_stayed,
                )
            });
            if let Ok((handle, focus, true)) = focus_info {
                let _ = handle.update(cx, |_, window, cx| {
                    window.focus(&focus, cx);
                });
            }
            let subscribed = client
                .call(
                    "subscribe_terminal",
                    serde_json::json!({ "terminal_id": id }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match subscribed
                    .ok()
                    .and_then(|v| v["stream"].as_str().map(str::to_string))
                {
                    Some(stream) => {
                        let early = this.terminal.early.remove(&stream).unwrap_or_default();
                        if let Some(tab) = this.terminal.tabs.iter_mut().find(|t| t.id == id) {
                            tab.stream = Some(stream);
                        }
                        for event in early {
                            this.apply_terminal_event(&id, event, cx);
                        }
                        match &then {
                            Then::Nothing => {}
                            Then::Run(command) => {
                                this.terminal_send(id.clone(), format!("{command}\r"), cx)
                            }
                            Then::Insert(text) => {
                                this.insert_when_ready(id.clone(), text.clone(), cx)
                            }
                        }
                    }
                    None => this.report("can't attach to the terminal", "subscribe failed"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Paste `text` into a new shell once its prompt is up (it turns on
    /// bracketed paste then), so a multi-line block isn't run line by line.
    fn insert_when_ready(&mut self, id: String, text: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let start = std::time::Instant::now();
            loop {
                let ready = this.update(cx, |this, _| {
                    let tab = this.terminal.tabs.iter().find(|t| t.id == id)?;
                    let on = bracketed(&tab.emu);
                    (on || start.elapsed() >= PROMPT_WAIT || tab.exited.is_some()).then_some(on)
                });
                match ready {
                    Ok(Some(on)) => {
                        let _ = this.update(cx, |this, cx| {
                            this.terminal_send(id.clone(), encode_paste(&text, on), cx)
                        });
                        return;
                    }
                    Ok(None) => {}
                    Err(_) => return,
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
            }
        })
        .detach();
    }

    pub(super) fn teardown_terminal_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.terminal.tabs.len() {
            return;
        }
        let tab = self.terminal.tabs.remove(index);
        let panel = &mut self.terminal;
        panel.active = panel.active.min(panel.tabs.len().saturating_sub(1));
        panel.busy = panel.tabs.iter().any(|t| t.busy);
        if let Some(client) = self.client.clone() {
            cx.spawn(async move |_, _| {
                let _ = client
                    .call(
                        "terminal_close",
                        serde_json::json!({ "terminal_id": tab.id }),
                    )
                    .await;
            })
            .detach();
        }
        cx.notify();
    }

    /// Output and exits from the engine, routed by stream.
    pub(super) fn on_terminal_event(&mut self, item: RpcStreamItem, cx: &mut Context<Self>) {
        let id = self
            .terminal
            .tabs
            .iter()
            .find(|t| t.stream.as_deref() == Some(item.stream.as_str()))
            .map(|t| t.id.clone());
        match id {
            Some(id) => self.apply_terminal_event(&id, item.event, cx),
            None => {
                // Replay can beat the subscribe reply here; keep it for the tab.
                let early = self.terminal.early.entry(item.stream).or_default();
                if early.len() < 256 {
                    early.push(item.event);
                }
            }
        }
    }

    fn apply_terminal_event(&mut self, id: &str, event: SubscribeEvent, cx: &mut Context<Self>) {
        let Some(tab) = self.terminal.tabs.iter_mut().find(|t| t.id == id) else {
            return;
        };
        match event {
            SubscribeEvent::TerminalOutput { data } => {
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
                    return;
                };
                let replies = tab.emu.feed(&bytes);
                for reply in replies {
                    self.terminal_send(id.to_string(), reply, cx);
                }
            }
            SubscribeEvent::TerminalExit { code } => tab.exited = Some(code),
            _ => {}
        }
        cx.notify();
    }

    fn terminal_send(&mut self, id: String, data: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |_, _| {
            let _ = client
                .call(
                    "terminal_input",
                    serde_json::json!({ "terminal_id": id, "data": data }),
                )
                .await;
        })
        .detach();
    }

    /// Keys typed into a focused terminal go to its shell. ⌘ shortcuts and ⌃Tab
    /// stay with the app.
    fn on_terminal_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let panel = &mut self.terminal;
        let Some(tab) = panel.tabs.get_mut(panel.active) else {
            return;
        };
        if tab.exited.is_some() {
            return;
        }
        let ks = &event.keystroke;
        let m = ks.modifiers;
        let id = tab.id.clone();
        if m.platform && ks.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                let bracketed = bracketed(&tab.emu);
                tab.emu.scroll_to_bottom();
                cx.stop_propagation();
                self.terminal_send(id, encode_paste(&text, bracketed), cx);
            }
            return;
        }
        if m.control && ks.key == "tab" {
            return;
        }
        let app_cursor = tab
            .emu
            .mode()
            .contains(alacritty_terminal::term::TermMode::APP_CURSOR);
        if let Some(data) = encode_key(
            &ks.key,
            ks.key_char.as_deref(),
            m.control,
            m.alt,
            m.shift,
            m.platform,
            app_cursor,
        ) {
            tab.emu.scroll_to_bottom();
            cx.stop_propagation();
            self.terminal_send(id, data, cx);
        }
    }

    /// Columns and rows that fit the grid's last painted bounds.
    fn terminal_fit(&self) -> Option<(usize, usize)> {
        let bounds = self.terminal.bounds.get()?;
        let cell = self.terminal.cell_width?;
        let cols = ((f32::from(bounds.size.width) - 2. * PAD) / cell).floor() as usize;
        let rows = ((f32::from(bounds.size.height) - PAD) / LINE_HEIGHT).floor() as usize;
        Some((cols.max(10), rows.max(2)))
    }

    /// Inner terminal surface rendered inside `RightPanel` for `term_id`.
    pub(super) fn render_terminal_content(
        &mut self,
        term_id: &str,
        t: Tokens,
        dark: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if let Some(ix) = self.terminal.tab_ix(term_id) {
            self.terminal.active = ix;
        }
        // Measure the monospace cell once.
        if self.terminal.cell_width.is_none() {
            let text = window.text_system();
            let font_id = text.resolve_font(&gpui::font(MONO));
            self.terminal.cell_width = text
                .advance(font_id, px(FONT_SIZE), 'm')
                .ok()
                .map(|s| f32::from(s.width));
        }
        // Fit the active shell to the space it got last frame.
        if let Some((cols, rows)) = self.terminal_fit() {
            let active = self.terminal.active;
            if let Some(tab) = self.terminal.tabs.get_mut(active)
                && tab.emu.resize(cols, rows)
            {
                let id = tab.id.clone();
                if let Some(client) = self.client.clone() {
                    cx.spawn(async move |_, _| {
                        let _ = client
                            .call("terminal_resize", serde_json::json!({ "terminal_id": id, "cols": cols, "rows": rows }))
                            .await;
                    })
                    .detach();
                }
            }
        }
        let focused = self.terminal.focus.is_focused(window);
        let palette = if dark { Palette::DARK } else { Palette::LIGHT };
        let cell_width = self.terminal.cell_width.unwrap_or(7.5);
        let bounds_slot = self.terminal.bounds.clone();

        let screen = self
            .terminal
            .tab_ix(term_id)
            .and_then(|ix| self.terminal.tabs.get(ix))
            .map(|tab| tab.emu.screen(&palette));
        let grid = div()
            .id("terminal-grid")
            .relative()
            .flex_1()
            .min_h_0()
            .px(px(PAD))
            .pt(px(PAD / 2.))
            .overflow_hidden()
            .font_family(MONO)
            .text_size(px(FONT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .text_color(gpui::rgb(palette.fg))
            .child(
                // Records where the grid landed, to size the shell next frame.
                gpui::canvas(
                    move |bounds, window, _| {
                        if bounds_slot.get() != Some(bounds) {
                            bounds_slot.set(Some(bounds));
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                let dy = f32::from(event.delta.pixel_delta(window.line_height()).y);
                let lines = (dy / LINE_HEIGHT).round() as i32;
                let active = this.terminal.active;
                if lines != 0
                    && let Some(tab) = this.terminal.tabs.get_mut(active)
                {
                    tab.emu.scroll(lines);
                    cx.notify();
                }
            }))
            .when_some(screen, |d, screen| {
                let cursor = screen.cursor;
                d.children(screen.lines.into_iter().map(|line| {
                    let highlights: Vec<_> = line
                        .runs
                        .iter()
                        .map(|run| {
                            (
                                run.range.clone(),
                                gpui::HighlightStyle {
                                    color: Some(gpui::rgb(run.fg).into()),
                                    background_color: run.bg.map(|bg| gpui::rgb(bg).into()),
                                    font_weight: run.bold.then_some(gpui::FontWeight::BOLD),
                                    font_style: run.italic.then_some(gpui::FontStyle::Italic),
                                    underline: run.underline.then(|| gpui::UnderlineStyle {
                                        thickness: px(1.),
                                        ..Default::default()
                                    }),
                                    ..Default::default()
                                },
                            )
                        })
                        .collect();
                    div()
                        .h(px(LINE_HEIGHT))
                        .whitespace_nowrap()
                        .child(gpui::StyledText::new(line.text).with_highlights(highlights))
                }))
                .children(cursor.map(|(col, row)| {
                    div()
                        .absolute()
                        .left(px(PAD + col as f32 * cell_width))
                        .top(px(PAD / 2. + row as f32 * LINE_HEIGHT))
                        .w(px(cell_width))
                        .h(px(LINE_HEIGHT))
                        .when(focused, |d| d.bg(c(t.accent & 0xFFFF_FF00 | 0x99)))
                        .when(!focused, |d| d.border_1().border_color(c(t.accent)))
                }))
            });

        div()
            .id("terminal-panel")
            .track_focus(&self.terminal.focus)
            .key_context("Terminal")
            .capture_key_down(cx.listener(Self::on_terminal_key))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                window.focus(&this.terminal.focus, cx);
                cx.notify();
            }))
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(gpui::rgb(palette.bg))
            .child(grid)
            .into_any_element()
    }
}

/// The shell asked for bracketed paste (its prompt is up).
fn bracketed(emu: &Emulator) -> bool {
    emu.mode()
        .contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE)
}

/// Where an insert for `chat` goes: the active tab if it's that chat's and
/// still running, else that chat's latest running one. `None`: start one.
fn pick_tab(tabs: &[TermTab], active: usize, chat: &Option<String>) -> Option<usize> {
    let fits = |t: &TermTab| t.exited.is_none() && &t.chat == chat;
    if tabs.get(active).is_some_and(fits) {
        return Some(active);
    }
    tabs.iter().rposition(fits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: &str, chat: &str, exited: bool) -> TermTab {
        TermTab {
            id: id.into(),
            chat: Some(chat.into()),
            stream: None,
            emu: Emulator::new(20, 5),
            exited: exited.then_some(Some(0)),
            busy: false,
        }
    }

    #[test]
    fn an_insert_goes_to_this_chats_running_shell() {
        let chat = Some("a".to_string());
        let tabs = vec![
            tab("1", "a", false),
            tab("2", "b", false),
            tab("3", "a", true),
        ];
        // The active tab when it's this chat's.
        assert_eq!(pick_tab(&tabs, 0, &chat), Some(0));
        // Another chat's tab is active: this chat's latest running one.
        assert_eq!(pick_tab(&tabs, 1, &chat), Some(0));
        // Exited shells don't count; none left: start a new one.
        assert_eq!(pick_tab(&tabs[2..], 0, &chat), None);
        assert_eq!(pick_tab(&tabs, 0, &Some("c".into())), None);
        assert_eq!(pick_tab(&[], 0, &chat), None);
    }
}

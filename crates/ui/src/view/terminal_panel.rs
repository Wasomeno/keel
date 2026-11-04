//! Terminal panel: tabs of engine-owned shells, in the right-hand side
//! panel slot (with the browser and files panels; one at a time), sliding
//! in and out like them. Hiding the panel detaches; the shells keep running.

use crate::hover_fade::{HoverFadeExt, reveal_on_hover};
use crate::style::zpx;
use std::cell::Cell;
use std::rc::Rc;

use base64::Engine as _;
use gpui::{
    Bounds, ClickEvent, Context, ElementId, FocusHandle, IntoElement, KeyDownEvent, Pixels,
    ScrollWheelEvent, Window, div, prelude::*, px,
};
use keel_proto::{RpcStreamItem, SubscribeEvent};
use keel_theme::Tokens;

use super::{KeelWindow, icon_button};
use crate::icons::Icon;
use crate::style::{KeelStyled, MONO, RADIUS_MD, RADIUS_SM, c};
use crate::terminal::{Emulator, Palette, encode_key, encode_paste};

/// Monospace cell metrics for the panel.
const FONT_SIZE: f32 = 12.5;
const LINE_HEIGHT: f32 = 17.;
/// Panel height, including its tab row.
const PAD: f32 = 12.;

/// One shell in the panel.
pub(super) struct TermTab {
    id: String,
    /// The `subscribe_terminal` stream, once known.
    stream: Option<String>,
    emu: Emulator,
    exited: Option<Option<i32>>,
}

/// Panel state kept on the window.
pub(super) struct TerminalPanel {
    pub(super) open: bool,
    tabs: Vec<TermTab>,
    active: usize,
    pub(super) focus: FocusHandle,
    /// Output that arrived before its stream id was known, by stream.
    early: std::collections::HashMap<String, Vec<SubscribeEvent>>,
    /// The grid's on-screen bounds from the last paint, to fit cols × rows.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    cell_width: Option<f32>,
    /// The last show or hide: when, and a count that names its animation.
    pub(super) anim: Option<(std::time::Instant, u64)>,
    /// A command (not just the shell) is running in one of the tabs: the
    /// header's terminal button carries a dot.
    pub(super) busy: bool,
    /// The busy poll is running.
    polling: bool,
}

impl TerminalPanel {
    pub(super) fn new(focus: FocusHandle) -> Self {
        Self {
            open: false,
            tabs: Vec::new(),
            active: 0,
            focus,
            early: Default::default(),
            bounds: Rc::new(Cell::new(None)),
            cell_width: None,
            anim: None,
            busy: false,
            polling: false,
        }
    }
}

impl KeelWindow {
    /// ⌃` and the tab-strip button: show or hide; the first show starts a shell.
    pub(super) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = !self.terminal.open;
        self.set_terminal_open(open, window, cx);
        let panel = &mut self.terminal;
        if panel.open {
            if panel.tabs.is_empty() {
                self.new_terminal(cx);
            }
            window.focus(&self.terminal.focus, cx);
        } else {
            self.composer.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }

    /// Show or hide the side panel, sliding like the browser and files
    /// panels; showing it closes whichever of those is open.
    pub(super) fn set_terminal_open(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if open == self.terminal.open || (open && !self.browser_allowed()) {
            return;
        }
        if open && self.browser.open {
            self.toggle_browser(window, cx);
        }
        if open && self.files.open {
            self.toggle_files(window, cx);
        }
        self.update_right_panel_anim();
        let panel = &mut self.terminal;
        panel.open = open;
        let n = panel.anim.map_or(1, |(_, n)| n + 1);
        panel.anim = Some((std::time::Instant::now(), n));
        // One render after the slide ends drops the pane.
        let settle = self.right_panel_duration();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(settle + std::time::Duration::from_millis(20))
                .await;
            let _ = this.update(cx, |_, cx| {
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Ask the engine every 2 s whether a command is running in one of this
    /// window's tabs (for the header dot), while any tab is open.
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
                    let ours = |row: &&serde_json::Value| {
                        this.terminal
                            .tabs
                            .iter()
                            .any(|tab| row["terminal_id"] == tab.id.as_str())
                    };
                    let busy = rows["terminals"].as_array().is_some_and(|rows| {
                        rows.iter().filter(ours).any(|row| row["busy"] == true)
                    });
                    if busy != this.terminal.busy {
                        this.terminal.busy = busy;
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
                if this.terminal.busy {
                    this.terminal.busy = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Start a shell in the active chat's folder and attach to it.
    pub(super) fn new_terminal(&mut self, cx: &mut Context<Self>) {
        self.new_terminal_running(None, cx);
    }

    /// Show the panel with a fresh shell that runs `command`. The user
    /// finishes it there.
    pub(super) fn run_in_terminal(
        &mut self,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_terminal_open(true, window, cx);
        window.focus(&self.terminal.focus, cx);
        self.new_terminal_running(Some(command), cx);
    }

    fn new_terminal_running(&mut self, command: Option<String>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let (cols, rows) = self.terminal_fit().unwrap_or((100, 15));
        let chat_id = self.current_chat.clone();
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
                        this.report("can't open a terminal", opened.err().unwrap_or_default());
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                let panel = &mut this.terminal;
                panel.tabs.push(TermTab {
                    id: id.clone(),
                    stream: None,
                    emu: Emulator::new(cols, rows),
                    exited: None,
                });
                panel.active = panel.tabs.len() - 1;
                this.poll_terminals(cx);
                cx.notify();
            });
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
                        if let (Some(command), Some(client)) =
                            (command.clone(), this.client.clone())
                        {
                            let id = id.clone();
                            cx.spawn(async move |_, _| {
                                let data = format!("{command}\r");
                                let _ = client
                                    .call(
                                        "terminal_input",
                                        serde_json::json!({ "terminal_id": id, "data": data }),
                                    )
                                    .await;
                            })
                            .detach();
                        }
                    }
                    None => this.report("can't attach to the terminal", "subscribe failed"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn close_terminal(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.terminal.tabs.len() {
            return;
        }
        let tab = self.terminal.tabs.remove(index);
        let panel = &mut self.terminal;
        panel.active = panel.active.min(panel.tabs.len().saturating_sub(1));
        if panel.tabs.is_empty() {
            self.set_terminal_open(false, window, cx);
            self.composer.update(cx, |s, cx| s.focus(window, cx));
        }
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
                let bracketed = tab
                    .emu
                    .mode()
                    .contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE);
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

    /// The side panel: a spacer that makes room in the row and the pane over
    /// it, both sliding (as the files panel). Empty when hidden and still.
    pub(super) fn render_terminal_panel(
        &mut self,
        t: Tokens,
        dark: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let allowed = self.browser_allowed();
        let open = self.terminal.open && allowed;
        let toggling = !self.reduced()
            && self
                .terminal
                .anim
                .is_some_and(|(at, _)| at.elapsed() < self.right_panel_duration());
        if !open && !toggling {
            return Vec::new();
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

        let tabs =
            div()
                .h(crate::style::HEADER_HEIGHT)
                .flex_none()
                .pr_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .border_b_1()
                .border_color(c(t.border))
                .children(self.terminal.tabs.iter().enumerate().map(|(ix, tab)| {
                    let active = ix == self.terminal.active;
                    let label = tab
                        .emu
                        .title()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| format!("Terminal {}", ix + 1));
                    let group = format!("term-tab-{ix}");
                    div()
                        .id(ElementId::Name(group.clone().into()))
                        .hover_probe(group.clone())
                        .group(group.clone())
                        .h(zpx(24.))
                        .max_w(zpx(220.))
                        .pl_2()
                        .pr_1()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .rounded(RADIUS_MD)
                        .cursor_pointer()
                        .t_small()
                        .text_color(c(if active { t.text } else { t.text_muted }))
                        .when(active, |s| s.bg(c(t.selected)))
                        .when(!active, |s| s.hover_fade(c(t.hover), None))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.terminal.active = ix;
                            window.focus(&this.terminal.focus, cx);
                            cx.notify();
                        }))
                        .child(Icon::Tool.el(zpx(12.)))
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(label),
                        )
                        .when(tab.exited.is_some(), |d| {
                            d.child(
                                div()
                                    .t_caption()
                                    .text_color(c(t.text_faint))
                                    .child("exited"),
                            )
                        })
                        .child(reveal_on_hover("term-close-reveal", group.clone(), if active { 1. } else { 0. }, 1.,
                            div()
                                .id(ElementId::Name(format!("term-close-{ix}").into()))
                                .size(zpx(16.))
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .rounded(RADIUS_SM)
                                .text_color(c(t.text_faint))
                                .hover(|s| s.text_color(c(t.text)))
                                .hover_fade(c(t.hover), None)
                                .child(Icon::Close.el(zpx(10.)))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.close_terminal(ix, window, cx);
                                }))))
                }))
                .child(
                    icon_button("term-new-btn", Icon::Plus, t)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.new_terminal(cx))),
                )
                .child(div().flex_1())
                .child(
                    icon_button("term-close-btn", Icon::SidebarClose, t).on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.toggle_terminal(window, cx),
                    )),
                );

        let active = self.terminal.active;
        let screen = self
            .terminal
            .tabs
            .get(active)
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

        let full = self.browser_width(window);
        let body = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(gpui::rgb(palette.bg))
            .border_l_1()
            .border_color(c(t.border))
            .child(tabs.pl(px(8.)))
            .child(grid);
        let pane = div()
            .id("terminal-panel")
            .track_focus(&self.terminal.focus)
            .key_context("Terminal")
            .capture_key_down(cx.listener(Self::on_terminal_key))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                window.focus(&this.terminal.focus, cx);
                cx.notify();
            }))
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(full))
            .child(body)
            .child(self.render_browser_edge(t, cx));

        let anim = self.terminal.anim.filter(|_| toggling);
        if let Some((_, n)) = anim {
            use gpui::AnimationExt as _;
            let (from, to) = (self.right_panel_from, self.right_panel_target());
            let entering = to >= from;
            let curve = gpui::Animation::new(self.right_panel_duration())
                .with_easing(crate::motion::glide_curve);
            let pane = pane
                .with_animation(
                    ("terminal-slide", n as usize),
                    curve.clone(),
                    move |el, delta| {
                        let k = from + (to - from) * delta;
                        el.right(px(-full * (1.0 - k)))
                            .opacity(crate::motion::panel_opacity(k, entering))
                    },
                )
                .into_any_element();
            let spacer = div()
                .h_full()
                .flex_none()
                .w(px(full * to))
                .into_any_element();
            vec![spacer, pane]
        } else if open {
            let pane = pane.right_0().into_any_element();
            let spacer = div().h_full().flex_none().w(px(full)).into_any_element();
            vec![spacer, pane]
        } else {
            Vec::new()
        }
    }
}

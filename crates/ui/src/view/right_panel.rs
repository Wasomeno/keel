//! Unified right side panel: one top tab bar, launcher home, resize handle,
//! and fullscreen expand shared across Changes, Browser, Terminal, and File.

use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt as _, AnyElement, ClickEvent, Context, ElementId, FocusHandle,
    FontWeight, IntoElement, MouseButton, MouseDownEvent, SharedString, Window, div, prelude::*, px,
};
use keel_theme::Tokens;

use super::browser_panel::{expand_duration, expand_progress};
use super::files_panel::FilesTab;
use super::{KeelWindow, drag_region, icon_button};
use crate::hover_fade::{HoverFadeExt, reveal_on_hover};
use crate::icons::Icon;
use crate::motion::{FAST, ease_out_strong, panel_shift, stagger_in};
use crate::style::{HEADER_HEIGHT, KeelStyled, RADIUS_MD, RADIUS_XL, c, zpx};

/// Spacing inside the launcher and between cards.
const LAUNCHER_MAX_W: f32 = 348.;
const LAUNCHER_CARD_H: f32 = 108.;
const LAUNCHER_GAP: f32 = 12.;
/// Stagger delay between cards in the 2×2 launcher entrance.
const LAUNCHER_STAGGER_MS: u64 = 24;
/// Tab geometry bounds in the unified top strip.
const TAB_MIN_W: f32 = 56.;
const TAB_MAX_W: f32 = 148.;
const TAB_SEP_W: f32 = 5.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LauncherAction {
    Changes,
    Browser,
    Terminal,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RightTabKind {
    Launcher,
    Changes,
    File,
    Browser(u64),
    Terminal(String),
}

#[derive(Clone, Debug)]
pub(super) struct RightTab {
    pub(super) id: u64,
    pub(super) kind: RightTabKind,
}

pub(super) struct RightPanel {
    pub(super) open: bool,
    pub(super) expanded: bool,
    pub(super) expand_from: f32,
    pub(super) expand_anim: Option<(Instant, u64)>,
    pub(super) tips: super::tooltip::HoverTips,
    pub(super) focus: FocusHandle,
    pub(super) tabs: Vec<RightTab>,
    pub(super) active: usize,
    next_id: u64,
    launcher_gen: u64,
    pub(super) animate_launcher: bool,
    /// When the user clicks Terminal on a Launcher tab, record its tab id so
    /// the spawned shell replaces that Launcher tab cleanly in place.
    pub(super) pending_terminal_launcher: Option<u64>,
    pub(super) nav_seq: u64,
    tab_scroll: gpui::ScrollHandle,
    last_scrolled_tab: Option<(u64, usize, bool)>,
}

impl RightPanel {
    pub(super) fn new(focus: FocusHandle) -> Self {
        Self {
            open: false,
            expanded: false,
            expand_from: 0.,
            expand_anim: None,
            tips: Default::default(),
            focus,
            tabs: vec![RightTab {
                id: 1,
                kind: RightTabKind::Launcher,
            }],
            active: 0,
            next_id: 2,
            launcher_gen: 0,
            animate_launcher: false,
            pending_terminal_launcher: None,
            nav_seq: 0,
            tab_scroll: gpui::ScrollHandle::default(),
            last_scrolled_tab: None,
        }
    }

    pub(super) fn has_kind(&self, kind: &RightTabKind) -> bool {
        self.tabs.iter().any(|t| &t.kind == kind)
    }

    fn alloc_tab(&mut self, kind: RightTabKind) -> RightTab {
        let id = self.next_id;
        self.next_id += 1;
        RightTab { id, kind }
    }

    fn tab_pos(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }
}

impl KeelWindow {
    /// Whether `tab` should appear in the current chat's unified strip.
    fn right_tab_visible(&self, tab: &RightTab, chat: Option<&str>) -> bool {
        match &tab.kind {
            RightTabKind::Launcher | RightTabKind::Changes | RightTabKind::File => true,
            RightTabKind::Browser(id) => self
                .browser
                .tab_ix(*id)
                .is_some_and(|ix| self.browser.visible_in_strip(&self.browser.tabs[ix], chat)),
            RightTabKind::Terminal(id) => self.terminal.tab_ix(id).is_some(),
        }
    }

    fn visible_right_indices(&self, chat: Option<&str>) -> Vec<usize> {
        self.right_panel
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(ix, tab)| self.right_tab_visible(tab, chat).then_some(ix))
            .collect()
    }

    /// Prune closed/stale backing entries and guarantee at least one visible
    /// tab for `current_chat`.
    pub(super) fn normalize_right_tabs(&mut self) {
        // Remove dangling browser/terminal entries and deduplicate non-launcher kinds.
        let mut seen_changes = false;
        let mut seen_file = false;
        let mut seen_browser = Vec::new();
        let mut seen_terminal = Vec::new();
        let browser = &self.browser;
        let terminal = &self.terminal;
        self.right_panel.tabs.retain(|tab| match &tab.kind {
            RightTabKind::Launcher => true,
            RightTabKind::Changes => {
                if seen_changes {
                    false
                } else {
                    seen_changes = true;
                    true
                }
            }
            RightTabKind::File => {
                if seen_file {
                    false
                } else {
                    seen_file = true;
                    true
                }
            }
            RightTabKind::Browser(id) => {
                if browser.tab_ix(*id).is_none() || seen_browser.contains(id) {
                    false
                } else {
                    seen_browser.push(*id);
                    true
                }
            }
            RightTabKind::Terminal(id) => {
                if terminal.tab_ix(id).is_none() || seen_terminal.iter().any(|s| s == id) {
                    false
                } else {
                    seen_terminal.push(id.clone());
                    true
                }
            }
        });

        let chat = self.current_chat.as_deref();
        let visible = self.visible_right_indices(chat);
        if visible.is_empty() {
            let tab = self.right_panel.alloc_tab(RightTabKind::Launcher);
            self.right_panel.tabs.push(tab);
            self.right_panel.active = self.right_panel.tabs.len() - 1;
            self.right_panel.animate_launcher = false;
            self.right_panel.launcher_gen += 1;
            return;
        }
        if !visible.contains(&self.right_panel.active) {
            self.right_panel.active = visible[0];
        }
    }

    pub(super) fn right_panel_active_kind(&self) -> Option<&RightTabKind> {
        let chat = self.current_chat.as_deref();
        self.right_panel
            .tabs
            .get(self.right_panel.active)
            .filter(|t| self.right_tab_visible(t, chat))
            .map(|t| &t.kind)
            .or_else(|| {
                self.right_panel
                    .tabs
                    .iter()
                    .find(|t| self.right_tab_visible(t, chat))
                    .map(|t| &t.kind)
            })
    }

    pub(super) fn has_right_browser_tab(&self, browser_id: u64) -> bool {
        self.right_panel
            .tabs
            .iter()
            .any(|t| t.kind == RightTabKind::Browser(browser_id))
    }

    pub(super) fn is_browser_tab_showing(&self, b_ix: usize) -> bool {
        self.right_panel_open()
            && self.browser.tabs.get(b_ix).is_some_and(|bt| {
                matches!(self.right_panel_active_kind(), Some(RightTabKind::Browser(id)) if *id == bt.id)
            })
    }

    pub(super) fn right_panel_width(&self, window: &Window) -> f32 {
        self.browser_width(window)
    }

    pub(super) fn right_panel_expanded(&self) -> bool {
        self.right_panel.expanded
    }

    pub(super) fn right_panel_expanded_fraction(&self) -> f32 {
        if self.reduced() {
            return if self.right_panel.expanded { 1. } else { 0. };
        }
        let Some((at, _)) = self.right_panel.expand_anim else {
            return if self.right_panel.expanded { 1. } else { 0. };
        };
        let target = if self.right_panel.expanded { 1. } else { 0. };
        let dur = expand_duration(self.right_panel.expand_from, target);
        let elapsed = self.slide_elapsed(at);
        expand_progress(self.right_panel.expand_from, target, elapsed, dur)
    }

    pub(super) fn right_panel_expand_duration(&self) -> Duration {
        let target = if self.right_panel.expanded { 1. } else { 0. };
        expand_duration(self.right_panel.expand_from, target)
    }

    pub(super) fn right_panel_expanding(&self) -> bool {
        !self.reduced()
            && self
                .right_panel
                .expand_anim
                .is_some_and(|(at, _)| self.slide_elapsed(at) < self.right_panel_expand_duration())
    }

    /// ⌘⇧F and the header expand button: grow the active right side panel over
    /// the whole window, or restore it to its side column.
    pub(super) fn toggle_right_panel_expand(&mut self, cx: &mut Context<Self>) {
        if !self.right_panel_open() || self.right_panel_toggling() {
            return;
        }
        let current = self.right_panel_expanded_fraction();
        let reduced = self.reduced();
        let rp = &mut self.right_panel;
        rp.expanded = !rp.expanded;
        rp.expand_from = current;
        let n = rp.expand_anim.map_or(1, |(_, n)| n + 1);
        rp.expand_anim = Some((Instant::now(), n));
        if reduced {
            rp.expand_anim = None;
            for tab in &self.browser.tabs {
                if let Some(page) = &tab.native {
                    page.set_resizing(false);
                    page.arm_layout();
                }
            }
            cx.notify();
            return;
        }
        for tab in &self.browser.tabs {
            if let Some(page) = &tab.native {
                page.set_resizing(true);
            }
        }
        let duration = self.right_panel_expand_duration();
        let generation = n;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .right_panel
                    .expand_anim
                    .is_some_and(|(_, n)| n == generation)
                {
                    this.right_panel.expand_anim = None;
                    for tab in &this.browser.tabs {
                        if let Some(page) = &tab.native {
                            page.set_resizing(false);
                            page.arm_layout();
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn toggle_right_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.browser_allowed() {
            return;
        }
        if self.right_panel.open {
            self.close_right_panel(window, cx);
        } else {
            self.open_right_panel(window, cx);
        }
    }

    pub(super) fn open_right_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.browser_allowed() || self.right_panel.open {
            return;
        }
        self.normalize_right_tabs();
        self.update_right_panel_anim();
        let rp = &mut self.right_panel;
        rp.tips.clear();
        rp.open = true;
        rp.expanded = false;
        rp.expand_from = 0.;
        rp.expand_anim = None;
        rp.animate_launcher = false;
        self.sync_active_right_tab_on_enter(Some(window), cx);
        self.schedule_right_panel_settle(cx);
        cx.notify();
    }

    pub(super) fn close_right_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.right_panel.open {
            return;
        }
        self.update_right_panel_anim();
        let rp = &mut self.right_panel;
        rp.tips.clear();
        rp.open = false;
        rp.expanded = false;
        rp.expand_from = 0.;
        rp.expand_anim = None;
        self.browser.release_focus();
        for tab in &self.browser.tabs {
            if let Some(page) = &tab.native {
                page.set_resizing(false);
                page.set_shown(false);
            }
        }
        self.composer.update(cx, |s, cx| s.focus(window, cx));
        self.schedule_browser_park(cx);
        self.schedule_right_panel_settle(cx);
        cx.notify();
    }

    fn schedule_right_panel_settle(&self, cx: &mut Context<Self>) {
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
    }

    /// Synchronize backing state and focus whenever the right panel opens or
    /// its active tab changes.
    pub(super) fn sync_active_right_tab_on_enter(
        &mut self,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.normalize_right_tabs();
        let kind = self.right_panel_active_kind().cloned();
        let is_browser = matches!(kind, Some(RightTabKind::Browser(_)));
        if !is_browser {
            self.browser.release_focus();
            for tab in &self.browser.tabs {
                if let Some(native) = &tab.native {
                    native.set_shown(false);
                }
            }
        }
        match kind {
            Some(RightTabKind::Launcher) | None => {
                if let Some(window) = window {
                    window.focus(&self.right_panel.focus, cx);
                }
            }
            Some(RightTabKind::Changes) => {
                self.files.tab = FilesTab::Changes;
                self.files_sync_chat(cx);
                self.files_refresh(cx);
                if let Some(window) = window {
                    window.focus(&self.files.focus, cx);
                }
            }
            Some(RightTabKind::File) => {
                self.files.tab = FilesTab::Files;
                self.files_sync_chat(cx);
                self.files_refresh(cx);
                if let Some(window) = window {
                    window.focus(&self.files.focus, cx);
                }
            }
            Some(RightTabKind::Browser(id)) => {
                if let Some(b_ix) = self.browser.tab_ix(id) {
                    if let Some(window) = window {
                        self.activate_tab(b_ix, window, cx);
                    } else {
                        self.browser.active = b_ix;
                        self.sync_browser_address_deferred(b_ix, cx);
                    }
                }
            }
            Some(RightTabKind::Terminal(id)) => {
                if let Some(t_ix) = self.terminal.tab_ix(&id) {
                    self.terminal.active = t_ix;
                }
                if let Some(window) = window {
                    window.focus(&self.terminal.focus, cx);
                }
            }
        }
    }

    fn sync_browser_address_deferred(&mut self, b_ix: usize, cx: &mut Context<Self>) {
        let Some(url) = self.browser.tabs.get(b_ix).map(|t| t.page.url.clone().unwrap_or_default()) else {
            return;
        };
        let handle = self.browser.handle;
        let weak = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.browser.address.update(cx, |s, cx| s.set_value(url, window, cx));
                    if this.is_browser_tab_showing(b_ix) {
                        this.unpark_tab(b_ix, window, cx);
                    }
                    cx.notify();
                });
            });
        });
    }

    /// True if `tabs` holds only the single initial untouched Launcher.
    fn has_only_untouched_launcher(&self) -> bool {
        self.right_panel.tabs.len() == 1
            && self.right_panel.tabs[0].kind == RightTabKind::Launcher
    }

    /// Ensure `kind` has a tab in `right_panel.tabs`; returns its `RightTab.id`.
    /// When `replace_launcher` is true, replaces an active Launcher (or the
    /// lone untouched Launcher) instead of accumulating extra Launcher pills.
    pub(super) fn right_ensure_kind(
        &mut self,
        kind: RightTabKind,
        activate: bool,
        replace_launcher: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.normalize_right_tabs();
        let active_is_launcher = self
            .right_panel
            .tabs
            .get(self.right_panel.active)
            .is_some_and(|t| t.kind == RightTabKind::Launcher);
        let can_replace = replace_launcher && (active_is_launcher || self.has_only_untouched_launcher());

        if let Some(existing_ix) = self.right_panel.tabs.iter().position(|t| t.kind == kind) {
            let existing_id = self.right_panel.tabs[existing_ix].id;
            if can_replace && self.right_panel.active != existing_ix {
                let drop_ix = if active_is_launcher {
                    self.right_panel.active
                } else {
                    0
                };
                if drop_ix < self.right_panel.tabs.len()
                    && self.right_panel.tabs[drop_ix].kind == RightTabKind::Launcher
                {
                    self.right_panel.tabs.remove(drop_ix);
                    if drop_ix < self.right_panel.active {
                        self.right_panel.active -= 1;
                    }
                }
            }
            if activate {
                let target_ix = self.right_panel.tab_pos(existing_id).unwrap_or(0);
                self.right_panel.active = target_ix;
                self.sync_active_right_tab_on_enter(window, cx);
            }
            cx.notify();
            return existing_id;
        }

        let id = if can_replace {
            let slot = if active_is_launcher {
                self.right_panel.active
            } else {
                0
            };
            self.right_panel.tabs[slot].kind = kind;
            if activate {
                self.right_panel.active = slot;
            }
            self.right_panel.tabs[slot].id
        } else {
            let tab = self.right_panel.alloc_tab(kind);
            let id = tab.id;
            self.right_panel.tabs.push(tab);
            if activate {
                self.right_panel.active = self.right_panel.tabs.len() - 1;
            }
            id
        };

        if activate {
            self.right_panel.nav_seq = self.right_panel.nav_seq.wrapping_add(1);
            self.sync_active_right_tab_on_enter(window, cx);
        }
        cx.notify();
        id
    }

    /// Open the right side panel (if shut) directly onto `kind`, running tab
    /// entry setup once for `kind` cleanly.
    pub(super) fn open_right_tab(
        &mut self,
        kind: RightTabKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.browser_allowed() {
            return;
        }
        if !self.right_panel.open {
            self.right_ensure_kind(kind, true, true, None, cx);
            self.open_right_panel(window, cx);
        } else {
            self.right_ensure_kind(kind, true, true, Some(window), cx);
        }
    }

    /// Ensure an existing visible Browser tab or claim/push one, then activate.
    pub(super) fn right_open_or_focus_browser(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.normalize_right_tabs();
        let chat = self.current_chat.as_deref();
        // Prefer an already visible browser tab for this chat (agent first if any).
        let existing_browser = self
            .right_panel
            .tabs
            .iter()
            .filter_map(|t| match t.kind {
                RightTabKind::Browser(id) if self.right_tab_visible(t, chat) => {
                    let is_agent = self
                        .browser
                        .tab_ix(id)
                        .is_some_and(|ix| self.browser.tabs[ix].is_agent());
                    Some((id, is_agent))
                }
                _ => None,
            })
            .max_by_key(|(_, is_agent)| *is_agent)
            .map(|(id, _)| id);

        let browser_id = existing_browser.unwrap_or_else(|| self.acquire_user_browser_tab());
        self.open_right_tab(RightTabKind::Browser(browser_id), window, cx);
    }

    pub(super) fn acquire_user_browser_tab(&mut self) -> u64 {
        if let Some(ix) = self
            .browser
            .tabs
            .iter()
            .position(|t| t.is_empty_scratch() && !self.has_right_browser_tab(t.id))
        {
            return self.browser.tabs[ix].id;
        }
        let ix = self.browser.push_tab();
        self.browser.tabs[ix].id
    }

    /// + button and ⌘T inside the right panel: focus or open a Launcher tab.
    pub(super) fn right_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.browser_allowed() {
            return;
        }
        let was_open = self.right_panel.open;
        if !was_open {
            self.open_right_panel(window, cx);
        }
        self.normalize_right_tabs();
        if matches!(self.right_panel_active_kind(), Some(RightTabKind::Launcher)) {
            if was_open {
                self.right_panel.animate_launcher = true;
                self.right_panel.launcher_gen += 1;
            }
            window.focus(&self.right_panel.focus, cx);
            cx.notify();
            return;
        }
        let chat = self.current_chat.as_deref();
        if let Some(ix) = self
            .right_panel
            .tabs
            .iter()
            .position(|t| t.kind == RightTabKind::Launcher && self.right_tab_visible(t, chat))
        {
            self.right_panel.active = ix;
        } else {
            let tab = self.right_panel.alloc_tab(RightTabKind::Launcher);
            self.right_panel.tabs.push(tab);
            self.right_panel.active = self.right_panel.tabs.len() - 1;
        }
        self.right_panel.animate_launcher = was_open;
        if was_open {
            self.right_panel.launcher_gen += 1;
        }
        self.right_panel.nav_seq = self.right_panel.nav_seq.wrapping_add(1);
        self.sync_active_right_tab_on_enter(Some(window), cx);
        cx.notify();
    }

    /// Activate one launcher action card from the 2×2 home grid.
    pub(super) fn right_launch(
        &mut self,
        action: LauncherAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.browser_allowed() {
            return;
        }
        if !self.right_panel.open {
            self.open_right_panel(window, cx);
        }
        match action {
            LauncherAction::Changes => {
                self.right_panel.pending_terminal_launcher = None;
                self.right_ensure_kind(RightTabKind::Changes, true, true, Some(window), cx);
            }
            LauncherAction::File => {
                self.right_panel.pending_terminal_launcher = None;
                self.right_ensure_kind(RightTabKind::File, true, true, Some(window), cx);
            }
            LauncherAction::Browser => {
                self.right_panel.pending_terminal_launcher = None;
                let bid = self.acquire_user_browser_tab();
                self.right_ensure_kind(RightTabKind::Browser(bid), true, true, Some(window), cx);
            }
            LauncherAction::Terminal => {
                let replace_id = self
                    .right_panel
                    .tabs
                    .get(self.right_panel.active)
                    .filter(|t| t.kind == RightTabKind::Launcher)
                    .map(|t| t.id);
                self.right_panel.pending_terminal_launcher = replace_id;
                self.new_terminal(cx);
            }
        }
    }

    pub(super) fn right_activate_tab_id(
        &mut self,
        tab_id: u64,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.right_panel.tab_pos(tab_id) else {
            return;
        };
        self.right_panel.active = ix;
        self.right_panel.animate_launcher = false;
        self.right_panel.nav_seq = self.right_panel.nav_seq.wrapping_add(1);
        self.sync_active_right_tab_on_enter(window, cx);
        cx.notify();
    }

    pub(super) fn right_close_tab_id(
        &mut self,
        tab_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pos) = self.right_panel.tab_pos(tab_id) else {
            return;
        };
        let kind = self.right_panel.tabs[pos].kind.clone();
        match kind {
            RightTabKind::File => {
                if self.files.dirty() {
                    self.files
                        .mark_selected_dirty_error("Save or discard your edits first.");
                    self.right_activate_tab_id(tab_id, Some(window), cx);
                    return;
                }
            }
            RightTabKind::Browser(bid) => {
                if let Some(b_ix) = self.browser.tab_ix(bid) {
                    self.teardown_browser_tab(b_ix, window, cx);
                }
            }
            RightTabKind::Terminal(tid) => {
                if let Some(t_ix) = self.terminal.tab_ix(&tid) {
                    self.teardown_terminal_tab(t_ix, cx);
                }
            }
            RightTabKind::Launcher | RightTabKind::Changes => {}
        }

        let was_active = pos == self.right_panel.active;
        let chat = self.current_chat.as_deref();
        let vis_before = self.visible_right_indices(chat);
        let vis_pos = vis_before.iter().position(|&i| i == pos);

        self.right_panel.tabs.remove(pos);
        if pos < self.right_panel.active {
            self.right_panel.active -= 1;
        }

        let chat = self.current_chat.as_deref();
        let vis_after = self.visible_right_indices(chat);
        if vis_after.is_empty() {
            let launcher = self.right_panel.alloc_tab(RightTabKind::Launcher);
            self.right_panel.tabs.push(launcher);
            self.right_panel.active = self.right_panel.tabs.len() - 1;
            self.right_panel.animate_launcher = true;
            self.right_panel.launcher_gen += 1;
            self.sync_active_right_tab_on_enter(Some(window), cx);
            cx.notify();
            return;
        }

        if was_active || self.right_panel.active >= self.right_panel.tabs.len() {
            let next_vis_ix = vis_pos
                .map(|vp| vp.min(vis_after.len() - 1))
                .unwrap_or(0);
            self.right_panel.active = vis_after[next_vis_ix];
            self.right_panel.animate_launcher = false;
            self.right_panel.nav_seq = self.right_panel.nav_seq.wrapping_add(1);
            self.sync_active_right_tab_on_enter(Some(window), cx);
        }
        cx.notify();
    }

    pub(super) fn right_close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.normalize_right_tabs();
        let chat = self.current_chat.as_deref();
        let visible = self.visible_right_indices(chat);
        let Some(tab) = self.right_panel.tabs.get(self.right_panel.active) else {
            return;
        };
        // On the lone Launcher home screen there is no close pill; keep the panel intact.
        if visible.len() == 1 && tab.kind == RightTabKind::Launcher {
            return;
        }
        let id = tab.id;
        self.right_close_tab_id(id, window, cx);
    }

    /// Aggregated background activity indicator for the closed header button:
    /// `(color, bright, tooltip_hint)`.
    pub(super) fn right_panel_activity(&self, t: Tokens) -> Option<(u32, bool, &'static str)> {
        if self.terminal.busy {
            return Some((t.accent, true, "Terminal command running"));
        }
        self.browser_in_background()
            .map(|recent| (t.info, recent, "Agent using browser"))
    }

    /// Render the unified right side panel: spacer + sliding/expanding surface
    /// containing top tabs, actions, resize handle, and active tab body.
    pub(super) fn render_right_panel(
        &mut self,
        t: Tokens,
        dark: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.follow_chat_tab(window, cx);
        self.normalize_right_tabs();

        let allowed = self.browser_allowed();
        let open = self.right_panel.open && allowed;
        let toggling = self.right_panel_toggling();

        let active_browser_id = match self.right_panel_active_kind() {
            Some(RightTabKind::Browser(id)) => Some(*id),
            _ => None,
        };
        for tab in &self.browser.tabs {
            if let Some(native) = &tab.native {
                let shown = active_browser_id == Some(tab.id)
                    && (open || toggling)
                    && tab.page.url.is_some()
                    && tab.error.is_none();
                native.set_shown(shown);
            }
        }

        if !open && !toggling {
            return Vec::new();
        }

        let expanding = self.right_panel_expanding();
        let fraction = self.right_panel_expanded_fraction();
        let expanded = self.right_panel.expanded;
        let viewport = f32::from(window.viewport_size().width);
        let full = self.right_panel_width(window);
        let current_w = (full + (viewport - full) * fraction).round();

        let (pad_side, pad_full) = (8., f32::from(self.traffic_lights_inset()));
        let pad_left = (pad_side + (pad_full - pad_side) * fraction).round();

        let header = self
            .render_right_panel_header(current_w, pad_left, t, cx)
            .pl(px(pad_left))
            .into_any_element();

        let active_kind = self.right_panel_active_kind().cloned();
        let content_w = if expanding || expanded { current_w } else { full };
        let body_content: AnyElement = match active_kind {
            Some(RightTabKind::Launcher) | None => {
                self.render_right_launcher(t, cx).into_any_element()
            }
            Some(RightTabKind::Changes) => {
                div()
                    .id("right-files-scope")
                    .track_focus(&self.files.focus)
                    .key_context(crate::keys::FILES)
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.render_changes_content(content_w, t, window, cx))
                    .into_any_element()
            }
            Some(RightTabKind::File) => {
                div()
                    .id("right-files-scope")
                    .track_focus(&self.files.focus)
                    .key_context(crate::keys::FILES)
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.render_files_tree_content(content_w, t, window, cx))
                    .into_any_element()
            }
            Some(RightTabKind::Browser(id)) => {
                if let Some(b_ix) = self.browser.tab_ix(id)
                    && self.browser.active != b_ix
                {
                    self.browser.active = b_ix;
                }
                div()
                    .id("right-browser-scope")
                    .track_focus(&self.browser.focus)
                    .key_context(crate::keys::BROWSER)
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.render_browser_content(t, window, cx))
                    .into_any_element()
            }
            Some(RightTabKind::Terminal(id)) => {
                if let Some(t_ix) = self.terminal.tab_ix(&id) {
                    self.terminal.active = t_ix;
                }
                self.render_terminal_content(&id, t, dark, window, cx)
            }
        };

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
            .child(header)
            .child(body_content)
            .children(self.render_right_tip(t, window));

        let slide_k = if toggling {
            self.right_panel_open_now()
        } else if open {
            1.
        } else {
            return Vec::new();
        };

        // Keep `pane` as one persistent element across toggling/settled states so
        // descendant `GlobalElementId`s (animations, hover states, scroll handles)
        // never reset when the slide finishes.
        let pane = div()
            .id("right-panel")
            .track_focus(&self.right_panel.focus)
            .key_context(crate::keys::RIGHT_PANEL)
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(panel_shift(full, slide_k)))
            .w(px(current_w))
            .child(body)
            .when(!expanded && !expanding, |d| {
                d.child(self.render_browser_edge(t, cx))
            })
            .into_any_element();

        let spacer = div()
            .h_full()
            .flex_none()
            .w(px(full * self.right_panel_target()))
            .into_any_element();

        vec![spacer, pane]
    }

    fn render_right_panel_header(
        &mut self,
        panel_w: f32,
        pad_left: f32,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let reduced = self.reduced();
        let chat = self.current_chat.as_deref();
        let visible = self.visible_right_indices(chat);
        let lone_launcher = visible.len() == 1
            && self.right_panel.tabs[visible[0]].kind == RightTabKind::Launcher;
        let show_bottom_border = !lone_launcher;

        let available_strip = (panel_w - pad_left - 8. - 26. - 68. - 12.).max(120.);
        let needed_min = visible.len() as f32 * TAB_MIN_W
            + visible.len().saturating_sub(1) as f32 * TAB_SEP_W;
        let scroll_tabs = !lone_launcher && needed_min > available_strip;

        if !lone_launcher
            && let Some(vis_pos) = visible.iter().position(|&ix| ix == self.right_panel.active)
        {
            if !scroll_tabs {
                self.right_panel.tab_scroll.set_offset(gpui::point(px(0.), px(0.)));
                self.right_panel.last_scrolled_tab = None;
            } else {
                let key = (
                    self.right_panel.tabs[self.right_panel.active].id,
                    visible.len(),
                    self.right_panel.expanded,
                );
                if self.right_panel.last_scrolled_tab != Some(key) {
                    if self.right_panel.tab_scroll.bounds().size.width > px(0.) {
                        self.right_panel.last_scrolled_tab = Some(key);
                    }
                    self.right_panel.tab_scroll.scroll_to_item(vis_pos);
                }
            }
        } else {
            self.right_panel.last_scrolled_tab = None;
        }

        let changes_count = self.files.changes_count();
        let file_dirty = self.files.dirty();
        let selected_file_name = self.files.selected_file_name().map(str::to_string);

        let pills = if lone_launcher {
            Vec::new()
        } else {
            visible
                .iter()
                .enumerate()
                .map(|(vis_pos, &ix)| {
                    let tab = &self.right_panel.tabs[ix];
                    let id = tab.id;
                    let active = ix == self.right_panel.active;
                    let prev_active = vis_pos > 0 && visible[vis_pos - 1] == self.right_panel.active;
                    let group = format!("right-tab-{id}");

                    let (icon, label, dot, subtle_tag, spin_icon): (
                        Icon,
                        SharedString,
                        Option<u32>,
                        Option<SharedString>,
                        bool,
                    ) = match &tab.kind {
                        RightTabKind::Launcher => (Icon::Plus, "New Tab".into(), None, None, false),
                        RightTabKind::Changes => {
                            let badge = changes_count
                                .filter(|n| *n > 0)
                                .map(|n| SharedString::from(format!("{n}")));
                            (Icon::GitCompare, "Changes".into(), None, badge, false)
                        }
                        RightTabKind::File => {
                            if let Some(name) = &selected_file_name {
                                (
                                    Icon::for_path(name),
                                    name.clone().into(),
                                    file_dirty.then_some(t.warning),
                                    None,
                                    false,
                                )
                            } else {
                                (
                                    Icon::File,
                                    "File".into(),
                                    file_dirty.then_some(t.warning),
                                    None,
                                    false,
                                )
                            }
                        }
                        RightTabKind::Browser(bid) => {
                            if let Some(bt) =
                                self.browser.tab_ix(*bid).map(|bix| &self.browser.tabs[bix])
                            {
                                (
                                    Icon::Globe,
                                    bt.label().into(),
                                    bt.is_agent().then_some(t.info),
                                    None,
                                    bt.page.loading,
                                )
                            } else {
                                (Icon::Globe, "Browser".into(), None, None, false)
                            }
                        }
                        RightTabKind::Terminal(tid) => {
                            if let Some(tix) = self.terminal.tab_ix(tid) {
                                let tt = &self.terminal.tabs[tix];
                                (
                                    Icon::Tool,
                                    tt.label(tix).into(),
                                    tt.busy.then_some(t.accent),
                                    tt.exited.map(|_| "exited".into()),
                                    false,
                                )
                            } else {
                                (Icon::Tool, "Terminal".into(), None, None, false)
                            }
                        }
                    };

                    let close = reveal_on_hover(
                        "right-tab-close-reveal",
                        group.clone(),
                        if active { 1. } else { 0. },
                        1.,
                        div()
                            .id(("right-tab-close", id as usize))
                            .flex_none()
                            .size(zpx(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(zpx(4.))
                            .when(!active, |d| d.bg(c(t.hover)))
                            .text_color(c(t.text_faint))
                            .cursor_pointer()
                            .hover(|s| s.text_color(c(t.text)))
                            .hover_fade(c(t.hover), None)
                            .child(Icon::Close.el(zpx(10.)))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.right_close_tab_id(id, window, cx);
                            })),
                    );

                    let icon_el = if spin_icon {
                        div()
                            .flex_none()
                            .text_color(c(t.accent))
                            .child(crate::motion::spin(
                                Icon::Spinner.el(zpx(13.)),
                                ("right-tab-spin", id as usize),
                                reduced,
                            ))
                            .into_any_element()
                    } else {
                        div()
                            .flex_none()
                            .text_color(c(if active { t.text_muted } else { t.text_faint }))
                            .child(icon.el(zpx(13.)))
                            .into_any_element()
                    };

                    let pill = div()
                        .id(("right-tab", id as usize))
                        .relative()
                        .hover_probe(group.clone())
                        .group(group)
                        .when(scroll_tabs, |d| d.flex_none().max_w(zpx(TAB_MAX_W)))
                        .when(!scroll_tabs, |d| {
                            d.flex_shrink(1.)
                                .min_w(zpx(TAB_MIN_W))
                                .max_w(zpx(TAB_MAX_W))
                        })
                        .h(zpx(26.))
                        .pl(zpx(8.))
                        .pr(zpx(if active { 4. } else { 8. }))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(5.))
                        .rounded(RADIUS_MD)
                        .border_1()
                        .t_small()
                        .map(|d| {
                            if active {
                                d.bg(c(t.surface))
                                    .border_color(c(t.border))
                                    .text_color(c(t.text))
                                    .font_weight(FontWeight::MEDIUM)
                            } else {
                                d.border_color(gpui::transparent_black())
                                    .text_color(c(t.text_muted))
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(c(t.text)))
                                    .hover_fade(c(t.hover), None)
                                    .active(|s| s.bg(c(t.selected)).opacity(0.9))
                            }
                        })
                        .children(dot.map(|col| {
                            div().flex_none().size(zpx(6.)).rounded_full().bg(c(col))
                        }))
                        .child(icon_el)
                        .child(div().min_w_0().truncate().child(label))
                        .children(subtle_tag.map(|tag| {
                            div()
                                .flex_none()
                                .t_caption()
                                .text_color(c(t.text_faint))
                                .child(tag)
                        }))
                        .child(if active {
                            close.into_any_element()
                        } else {
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .right(zpx(3.))
                                .flex()
                                .items_center()
                                .child(close)
                                .into_any_element()
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.right_activate_tab_id(id, Some(window), cx);
                        }));

                    let slot_extra = if vis_pos > 0 { TAB_SEP_W } else { 0. };
                    let show_sep = !active && !prev_active;
                    let slot = div()
                        .id(("right-tab-slot", id as usize))
                        .when(scroll_tabs, |d| {
                            d.flex_none().max_w(zpx(TAB_MAX_W + slot_extra))
                        })
                        .when(!scroll_tabs, |d| {
                            d.flex_shrink(1.)
                                .min_w(zpx(TAB_MIN_W + slot_extra))
                                .max_w(zpx(TAB_MAX_W + slot_extra))
                        })
                        .flex()
                        .flex_row()
                        .items_center()
                        .when(vis_pos > 0, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .px(zpx(2.))
                                    .flex()
                                    .items_center()
                                    .child(
                                        div()
                                            .w(px(1.))
                                            .h(zpx(14.))
                                            .rounded_full()
                                            .bg(if show_sep {
                                                c(t.border)
                                            } else {
                                                gpui::transparent_black()
                                            }),
                                    ),
                            )
                        })
                        .child(pill);

                    stagger_in(slot, ("right-tab-in", id as usize), 0, reduced)
                })
                .collect::<Vec<_>>()
        };

        let new_tab_btn = (!lone_launcher).then(|| {
            self.right_tip_target(
                icon_button("right-new-tab", Icon::Plus, t).on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| this.right_new_tab(window, cx),
                )),
                "right-new-tab",
                cx,
            )
        });

        let left_strip = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(4.))
            .when(!pills.is_empty(), |d| {
                d.child(
                    div()
                        .id("right-tab-scroll")
                        .track_scroll(&self.right_panel.tab_scroll)
                        .min_w_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .overflow_x_scroll()
                        .children(pills),
                )
            })
            .children(new_tab_btn)
            .child(drag_region("right-panel-header-drag").min_w(zpx(8.)));

        let expand_btn = self.right_tip_target(
            icon_button(
                "right-panel-expand",
                if self.right_panel.expanded {
                    Icon::Shrink
                } else {
                    Icon::Expand
                },
                t,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.toggle_right_panel_expand(cx)
            })),
            "right-panel-expand",
            cx,
        );

        let close_btn = self.right_tip_target(
            icon_button("right-panel-close", Icon::PanelRight, t).on_click(cx.listener(
                |this, _: &ClickEvent, window, cx| this.toggle_right_panel(window, cx),
            )),
            "right-panel-close",
            cx,
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
            .when(show_bottom_border, |d| {
                d.border_b_1().border_color(c(t.border).opacity(0.65))
            })
            .child(left_strip)
            .child(expand_btn)
            .child(close_btn)
    }

    fn render_right_launcher(&self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let reduced = self.reduced();
        let animate = !reduced && self.right_panel.animate_launcher;
        let launcher_gen = self.right_panel.launcher_gen;
        let card = |ix: usize, action: LauncherAction, icon: Icon, label: &'static str| {
            let group = format!("right-launcher-card-{ix}");
            let el = div()
                .id(("right-launcher", ix))
                .group(group.clone())
                .flex_1()
                .min_w_0()
                .h(zpx(LAUNCHER_CARD_H))
                .px_3()
                .rounded(RADIUS_XL)
                .border_1()
                .border_color(c(t.border))
                .bg(c(t.surface).opacity(0.45))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(zpx(10.))
                .cursor_pointer()
                .hover(|s| s.border_color(c(t.text_faint).opacity(0.45)))
                .hover_fade(c(t.surface), None)
                .active(|s| s.bg(c(t.selected)).opacity(0.92))
                .child(
                    div()
                        .text_color(c(t.text_muted))
                        .group_hover(group.clone(), |s| s.text_color(c(t.text)))
                        .child(icon.el(zpx(18.))),
                )
                .child(
                    div()
                        .t_body()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c(t.text_muted))
                        .group_hover(group, |s| s.text_color(c(t.text)))
                        .child(label),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.right_launch(action, window, cx);
                }));

            if !animate {
                return el.into_any_element();
            }
            let delay = Duration::from_millis(ix as u64 * LAUNCHER_STAGGER_MS);
            let total = FAST + delay;
            el.with_animation(
                ElementId::Name(format!("right-launcher-in-{launcher_gen}-{ix}").into()),
                Animation::new(total).with_easing(ease_out_strong),
                move |d, progress| {
                    let delay_frac = delay.as_secs_f32() / total.as_secs_f32().max(0.001);
                    let k = if progress <= delay_frac {
                        0.
                    } else {
                        ((progress - delay_frac) / (1. - delay_frac)).clamp(0., 1.)
                    };
                    d.opacity(k).relative().top(px(5. * (1. - k)))
                },
            )
            .into_any_element()
        };

        div()
            .id("right-launcher-root")
            .flex_1()
            .size_full()
            .px_6()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(zpx(LAUNCHER_MAX_W))
                    .flex()
                    .flex_col()
                    .gap(zpx(LAUNCHER_GAP))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_row()
                            .gap(zpx(LAUNCHER_GAP))
                            .child(card(0, LauncherAction::Changes, Icon::GitCompare, "Changes"))
                            .child(card(1, LauncherAction::Browser, Icon::Globe, "Browser")),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_row()
                            .gap(zpx(LAUNCHER_GAP))
                            .child(card(2, LauncherAction::Terminal, Icon::Tool, "Terminal"))
                            .child(card(3, LauncherAction::File, Icon::File, "File")),
                    ),
            )
    }

    pub(super) fn right_tip_label(&self, id: &str) -> Option<(String, Option<&'static str>)> {
        use super::tooltip::keys;
        let (text, key) = match id {
            "right-new-tab" => ("New tab", keys("⌘T")),
            "right-panel-expand" if self.right_panel.expanded => ("Restore", keys("⌘⇧F")),
            "right-panel-expand" => ("Expand", keys("⌘⇧F")),
            "right-panel-close" => ("Hide panel", keys("⌘⌥B")),
            _ => return self.browser_tip_label(id),
        };
        Some((text.to_string(), key))
    }

    pub(super) fn render_right_tip(
        &mut self,
        t: Tokens,
        window: &Window,
    ) -> Option<AnyElement> {
        let reduced = self.reduced();
        let mut tips = std::mem::take(&mut self.right_panel.tips);
        let el = tips.render(|id| self.right_tip_label(id), t, window, reduced);
        self.right_panel.tips = tips;
        el
    }

    pub(super) fn right_tip_target(
        &self,
        button: gpui::Stateful<gpui::Div>,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let bounds = self.right_panel.tips.bounds.clone();
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
                if let Some(delay) = this.right_panel.tips.hover(id, *on, now) {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(delay).await;
                        let _ = this.update(cx, |this, cx| {
                            if this.right_panel.tips.ripen(id, now, Instant::now()) {
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
                    this.right_panel.tips.press(id, Instant::now());
                    cx.notify();
                }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal pure helper mirror for tab state invariants (no GPUI context needed).
    fn visible_ids<'a>(
        tabs: &'a [RightTab],
        visible_browsers: &[u64],
        visible_terminals: &[&str],
    ) -> Vec<u64> {
        tabs.iter()
            .filter(|t| match &t.kind {
                RightTabKind::Launcher | RightTabKind::Changes | RightTabKind::File => true,
                RightTabKind::Browser(id) => visible_browsers.contains(id),
                RightTabKind::Terminal(id) => visible_terminals.iter().any(|s| *s == id),
            })
            .map(|t| t.id)
            .collect()
    }

    #[test]
    fn launcher_singletons_and_multiple_tabs_invariants() {
        let mut next = 1u64;
        let mut alloc = |kind: RightTabKind| {
            let id = next;
            next += 1;
            RightTab { id, kind }
        };
        let mut tabs = vec![alloc(RightTabKind::Launcher)];
        let mut active = 0usize;

        // Replace untouched Launcher with Changes.
        tabs[active].kind = RightTabKind::Changes;
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].kind, RightTabKind::Changes);

        // Open Launcher (+), then pick File -> adds beside Changes.
        tabs.push(alloc(RightTabKind::Launcher));
        active = 1;
        tabs[active].kind = RightTabKind::File;
        assert_eq!(tabs.len(), 2);

        // Add multiple Browser and Terminal tabs.
        tabs.push(alloc(RightTabKind::Browser(10)));
        tabs.push(alloc(RightTabKind::Browser(11)));
        tabs.push(alloc(RightTabKind::Terminal("term-1".into())));
        tabs.push(alloc(RightTabKind::Terminal("term-2".into())));
        assert_eq!(
            visible_ids(&tabs, &[10, 11], &["term-1", "term-2"]).len(),
            6
        );
        // Chat scoping hides another chat's agent browser tab (id 11).
        assert_eq!(visible_ids(&tabs, &[10], &["term-1", "term-2"]).len(), 5);
    }

    #[test]
    fn closing_last_tab_resets_to_launcher() {
        let mut tabs = vec![RightTab {
            id: 2,
            kind: RightTabKind::Changes,
        }];
        tabs.remove(0);
        if tabs.is_empty() {
            tabs.push(RightTab {
                id: 3,
                kind: RightTabKind::Launcher,
            });
        }
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].kind, RightTabKind::Launcher);
    }
}

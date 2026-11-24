//! Settings (⌘,): split-pane master-detail layout with an animated category
//! rail on the left and focused, card-grouped controls on the right.

mod accounts;
mod controls;
mod general;
mod integrations;
mod memory;
mod notifications;
mod skills;

use gpui::{
    AnimationExt, AnyElement, ClickEvent, Context, ElementId, IntoElement,
    UniformListScrollHandle, div, prelude::*,
};
use keel_theme::Tokens;

use super::KeelWindow;
use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::style::{KeelStyled, RADIUS_MD, c, zpx};

pub(super) use controls::{
    section_header, setting_row_info, settings_list, text_button_owned, toggle,
};

const RAIL_WIDTH: f32 = 196.0;
const DETAIL_MAX_WIDTH: f32 = 680.0;
const GROUP_LABEL_H: f32 = 24.0;
const GROUP_GAP: f32 = 16.0;
const ITEM_H: f32 = 32.0;
const ITEM_GAP: f32 = 2.0;

/// Top-level category in the split-pane Settings rail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SettingsTab {
    #[default]
    General,
    Notifications,
    Accounts,
    Skills,
    Memory,
    Integrations,
}

impl SettingsTab {
    pub(super) fn index(self) -> usize {
        match self {
            Self::General => 0,
            Self::Notifications => 1,
            Self::Accounts => 2,
            Self::Skills => 3,
            Self::Memory => 4,
            Self::Integrations => 5,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Notifications => "Notifications",
            Self::Accounts => "Accounts",
            Self::Skills => "Skills",
            Self::Memory => "Memory",
            Self::Integrations => "Integrations",
        }
    }

    pub(super) fn icon(self) -> Icon {
        match self {
            Self::General => Icon::Settings,
            Self::Notifications => Icon::Volume2,
            Self::Accounts => Icon::Agent,
            Self::Skills => Icon::Puzzle,
            Self::Memory => Icon::Brain,
            Self::Integrations => Icon::Globe,
        }
    }

    pub(super) fn info_tip(self) -> Option<&'static str> {
        match self {
            Self::Skills => {
                Some("Reusable instructions Keel can load across chats. Click any skill to inspect its SKILL.md.")
            }
            Self::Memory => {
                Some("Notes the agent saves for future chats. Edit, archive, or delete them here.")
            }
            _ => None,
        }
    }

    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "general" | "appearance" | "updates" | "background" => Some(Self::General),
            "notifications" | "notify" => Some(Self::Notifications),
            "accounts" | "providers" => Some(Self::Accounts),
            "skills" => Some(Self::Skills),
            "memory" => Some(Self::Memory),
            "integrations" | "browser" | "connections" | "gitlab" => Some(Self::Integrations),
            _ => None,
        }
    }

    /// Exact vertical offset (logical px) of this item inside the nav rail list
    /// container, matching the fixed group header and item heights.
    fn rail_y(self) -> f32 {
        match self {
            Self::General => GROUP_LABEL_H,
            Self::Notifications => GROUP_LABEL_H + ITEM_H + ITEM_GAP,
            Self::Accounts => {
                GROUP_LABEL_H + 2.0 * ITEM_H + ITEM_GAP + GROUP_GAP + GROUP_LABEL_H
            }
            Self::Skills => {
                GROUP_LABEL_H + 2.0 * ITEM_H + ITEM_GAP + GROUP_GAP + GROUP_LABEL_H
                    + (ITEM_H + ITEM_GAP)
            }
            Self::Memory => {
                GROUP_LABEL_H + 2.0 * ITEM_H + ITEM_GAP + GROUP_GAP + GROUP_LABEL_H
                    + 2.0 * (ITEM_H + ITEM_GAP)
            }
            Self::Integrations => {
                GROUP_LABEL_H + 2.0 * ITEM_H + ITEM_GAP + GROUP_GAP + GROUP_LABEL_H
                    + 3.0 * (ITEM_H + ITEM_GAP)
            }
        }
    }
}

/// One agent CLI as `harness_status` reports it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct HarnessStatus {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub installed: bool,
    pub version: String,
    pub path: Option<String>,
    /// `None` until a sign-in check is asked for.
    pub accounts: Option<Vec<Account>>,
}

/// A sign-in in progress, as `sign_in_status` last reported it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct SignInUi {
    /// `starting`, `browser`, `device`, `working`, `done`, `failed`, `idle`.
    pub state: String,
    pub url: String,
    pub code: String,
    pub message: String,
}

impl SignInUi {
    pub(crate) fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Self {
            state: s("state"),
            url: s("url"),
            code: s("code"),
            message: s("message"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Account {
    pub id: String,
    pub name: String,
    pub hint: String,
    pub signed_in: bool,
    pub detail: String,
    pub supports_oauth: bool,
    pub from_env: bool,
}

/// Inline API key editor state for one provider row in Settings → Accounts.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ApiKeyEditState {
    pub provider: String,
    pub is_update: bool,
    pub saving: bool,
    pub revealed: bool,
    pub error: Option<String>,
    pub seq: u64,
}

impl Account {
    pub(crate) fn credential(&self) -> String {
        match self.detail.as_str() {
            "oauth" => "Subscription".into(),
            "api_key" => "API key".into(),
            other => other.to_string(),
        }
    }
}

/// One connected service as `list_connections` reports it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ConnectionRow {
    pub kind: String,
    pub host: String,
    pub username: String,
    pub name: String,
}

impl HarnessStatus {
    pub(crate) fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Self {
            id: s("id"),
            display_name: s("display_name"),
            description: s("description"),
            installed: v["installed"].as_bool().unwrap_or(false),
            version: s("version"),
            path: v["path"].as_str().map(str::to_string),
            accounts: v["accounts"].as_array().map(|rows| {
                rows.iter()
                    .map(|a| Account {
                        id: a["id"].as_str().unwrap_or_default().to_string(),
                        name: a["name"].as_str().unwrap_or_default().to_string(),
                        hint: a["hint"].as_str().unwrap_or_default().to_string(),
                        signed_in: a["signed_in"].as_bool().unwrap_or(false),
                        detail: a["detail"].as_str().unwrap_or_default().to_string(),
                        supports_oauth: a["supports_oauth"].as_bool().unwrap_or(false),
                        from_env: a["from_env"].as_bool().unwrap_or(false),
                    })
                    .collect()
            }),
        }
    }
}

impl ConnectionRow {
    pub(super) fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Self {
            kind: s("kind"),
            host: s("host"),
            username: s("username"),
            name: s("name"),
        }
    }
}

/// One skill as `list_skills` reports it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SkillRow {
    pub name: String,
    pub description: String,
    pub insert: String,
    pub scope: String,
    pub path: String,
    pub enabled: bool,
    pub manual_only: bool,
    pub removable: bool,
    pub collection: String,
    pub author: String,
    pub source_url: String,
}

impl SkillRow {
    pub(crate) fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Self {
            name: s("name"),
            description: s("description"),
            insert: s("insert"),
            scope: s("scope"),
            path: s("path"),
            enabled: v["enabled"].as_bool().unwrap_or(false),
            manual_only: v["manual_only"].as_bool().unwrap_or(false),
            removable: v["removable"].as_bool().unwrap_or(false),
            collection: s("collection"),
            author: s("author"),
            source_url: s("source_url"),
        }
    }
}

/// Status filter for skills list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SkillStatusFilter {
    All,
    Enabled,
    Disabled,
}

/// Where an added skill is about to be copied, or a delete/replace still to confirm.
#[derive(Clone)]
pub(super) enum SkillConfirm {
    Replace {
        source: String,
        dest: String,
        name: String,
    },
    Remove {
        path: String,
        name: String,
    },
}

/// Settings → Skills state.
pub(super) struct SkillsPage {
    pub open: bool,
    pub transition: Option<(u64, bool)>,
    pub load_gen: u64,
    pub rows: Option<Result<Vec<SkillRow>, String>>,
    pub added: Option<(String, String)>,
    pub notice: Option<String>,
    pub confirm: Option<SkillConfirm>,
    pub confirm_gen: u64,
    pub confirm_leaving: Option<(u64, SkillConfirm)>,
    pub row_errors: std::collections::HashMap<String, String>,
    pub scroll: gpui::ScrollHandle,
    pub list_scroll: UniformListScrollHandle,
    pub author_filter: Option<String>,
    pub status_filter: SkillStatusFilter,
    pub filter_epoch: u64,
    pub expanded_skill: Option<String>,
    pub expanded_content: std::collections::HashMap<String, String>,
    pub modal_skill: Option<SkillRow>,
    pub modal_leaving: Option<(u64, SkillRow)>,
    pub modal_gen: u64,
    pub modal_scroll: gpui::ScrollHandle,
}

impl Default for SkillsPage {
    fn default() -> Self {
        Self {
            open: false,
            transition: None,
            load_gen: 0,
            rows: None,
            added: None,
            notice: None,
            confirm: None,
            confirm_gen: 0,
            confirm_leaving: None,
            row_errors: std::collections::HashMap::new(),
            scroll: gpui::ScrollHandle::default(),
            list_scroll: UniformListScrollHandle::new(),
            author_filter: None,
            status_filter: SkillStatusFilter::All,
            filter_epoch: 0,
            expanded_skill: None,
            expanded_content: std::collections::HashMap::new(),
            modal_skill: None,
            modal_leaving: None,
            modal_gen: 0,
            modal_scroll: gpui::ScrollHandle::default(),
        }
    }
}

/// One note as `memory_list` reports it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct MemoryNote {
    pub slug: String,
    pub kind: String,
    pub description: String,
    pub unindexed: bool,
}

impl MemoryNote {
    fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Self {
            slug: s("slug"),
            kind: s("type"),
            description: s("description"),
            unindexed: v["unindexed"].as_bool().unwrap_or(false),
        }
    }
}

/// One scope of `memory_list`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct MemoryScopeView {
    pub notes: Vec<MemoryNote>,
    pub truncated: bool,
}

impl MemoryScopeView {
    pub(crate) fn parse(v: &serde_json::Value) -> Option<Self> {
        if !v.is_object() {
            return None;
        }
        let mut notes: Vec<MemoryNote> = v["notes"]
            .as_array()
            .map(|n| n.iter().map(MemoryNote::parse).collect())
            .unwrap_or_default();
        notes.sort_by_key(|n| n.unindexed);
        Some(Self {
            notes,
            truncated: v["truncated"].as_bool().unwrap_or(false),
        })
    }

    pub(crate) fn indexed(&self) -> usize {
        self.notes.iter().filter(|n| !n.unindexed).count()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum MemoryTab {
    #[default]
    You,
    Project,
}

impl MemoryTab {
    pub(crate) fn scope(self) -> &'static str {
        match self {
            Self::You => "user",
            Self::Project => "project",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MemoryConfirm {
    Archive,
    Delete,
}

/// The note in the edit modal.
#[derive(Clone, Debug)]
pub(super) struct MemoryOpen {
    pub tab: MemoryTab,
    pub cwd: Option<String>,
    pub note: MemoryNote,
    pub loaded: Option<Result<(), String>>,
}

/// Settings → Memory state.
#[derive(Default)]
pub(super) struct MemoryPage {
    pub open: bool,
    pub transition: Option<(u64, bool)>,
    pub load_gen: u64,
    pub user: Option<Result<MemoryScopeView, String>>,
    pub project: Option<Result<Option<MemoryScopeView>, String>>,
    pub cwd: Option<String>,
    pub tab: MemoryTab,
    pub list_epoch: u64,
    pub modal: Option<MemoryOpen>,
    pub modal_gen: u64,
    pub modal_leaving: Option<(u64, MemoryOpen)>,
    pub confirm: Option<MemoryConfirm>,
    pub confirm_gen: u64,
    pub busy: bool,
    pub error: Option<String>,
    pub scroll: gpui::ScrollHandle,
    pub modal_scroll: gpui::ScrollHandle,
}

impl KeelWindow {
    pub(super) fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.state.show_overflow_menu = false;
        self.settings_leaving = None;
        self.settings_gen += 1;
        self.settings_open = true;
        self.settings_tab_at = None;
        self.browser_sites_synced = false;
        self.load_agent_status(cx);
        self.load_connections(cx);
        self.load_skills(cx);
        self.load_memory(cx);
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.begin_settings_exit();
        self.finish_settings_exit(cx);
        self.skills.open = false;
        self.skills.transition = None;
        self.skills.confirm = None;
        self.skills.expanded_skill = None;
        self.skills.author_filter = None;
        self.skills.modal_skill = None;
        self.skills.modal_leaving = None;
        self.memory.open = false;
        self.memory.transition = None;
        self.memory.modal = None;
        self.memory.modal_leaving = None;
        self.memory.confirm = None;
        self.show_sound_menu = false;
        self.menus_open.remove("sound-list");
        self.menus_leaving.remove("sound-list");
        self.mark_current_read(cx);
        cx.notify();
    }

    pub(super) fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        if let Some(last) = self.last_settings_toggle {
            if now.duration_since(last) < std::time::Duration::from_millis(300) {
                return;
            }
        }
        self.last_settings_toggle = Some(now);

        if self.settings_open || self.settings_leaving.is_some() {
            self.close_settings(cx);
        } else {
            self.open_settings(cx);
        }
    }

    pub(super) fn select_settings_tab(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
        if self.settings_tab == tab {
            return;
        }
        self.settings_tab_prev = self.settings_tab;
        self.settings_tab = tab;
        self.settings_tab_gen = self.settings_tab_gen.wrapping_add(1);
        self.settings_tab_at = Some(std::time::Instant::now());
        self.show_sound_menu = false;
        self.dotify
            .settings_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        if tab == SettingsTab::Skills {
            self.skills.open = true;
            self.load_skills(cx);
        } else if tab == SettingsTab::Memory {
            self.memory.open = true;
            self.load_memory(cx);
        }
        cx.notify();
    }

    /// True while a tab switch animation is in flight (prevents replaying on
    /// ordinary toggle clicks inside the tab).
    fn recent_tab_switch(&self) -> bool {
        !self.reduced()
            && self
                .settings_tab_at
                .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(220))
    }

    pub(super) fn render_settings(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Close sound dropdown if user scrolls the detail pane
        let current_scroll_y = self.dotify.settings_scroll.offset().y;
        if self.show_sound_menu {
            if let Some(prev) = self.sound_menu_scroll_y {
                if (current_scroll_y - prev).abs() > gpui::px(1.0) {
                    self.show_sound_menu = false;
                    self.sound_menu_scroll_y = None;
                }
            } else {
                self.sound_menu_scroll_y = Some(current_scroll_y);
            }
        } else {
            self.sound_menu_scroll_y = None;
        }

        let rail = self.render_nav_rail(t, cx);
        let detail = self.render_detail_pane(t, cx);
        let skill_modal = self.render_skill_modal(t, cx);
        let memory_modal = self.render_memory_modal(t, cx);

        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_row()
            .bg(c(t.window))
            .child(rail)
            .child(detail)
            .children(skill_modal)
            .children(memory_modal)
    }

    fn rail_badge(&self, tab: SettingsTab) -> Option<String> {
        match tab {
            SettingsTab::Accounts => {
                let count = self
                    .harness_status
                    .as_ref()
                    .and_then(|h| h.first())
                    .and_then(|h| h.accounts.as_ref())
                    .map(|rows| rows.iter().filter(|a| a.signed_in).count())
                    .unwrap_or(0);
                (count > 0).then(|| count.to_string())
            }
            SettingsTab::Skills => match &self.skills.rows {
                Some(Ok(rows)) => {
                    let on = rows.iter().filter(|r| r.enabled).count();
                    (on > 0).then(|| on.to_string())
                }
                _ => None,
            },
            SettingsTab::Memory => match &self.memory.user {
                Some(Ok(scope)) if scope.indexed() > 0 => Some(scope.indexed().to_string()),
                _ => None,
            },
            SettingsTab::Integrations => {
                let n = self.connections.len();
                (n > 0).then(|| n.to_string())
            }
            _ => None,
        }
    }

    fn render_nav_rail(&self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let active = self.settings_tab;
        let prev = self.settings_tab_prev;
        let animating = self.recent_tab_switch();
        let tab_gen = self.settings_tab_gen;
        let update_ready = matches!(self.updates.status, keel_update::Status::Ready { .. })
            && self.updates.ready.is_some();

        let make_pill = move |draw_top: f32, draw_h: f32| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(zpx(draw_top))
                .h(zpx(draw_h))
                .rounded(RADIUS_MD)
                .bg(c(t.selected))
        };

        let sliding_pill: AnyElement = if animating && prev != active {
            let start_y = prev.rail_y();
            let end_y = active.rail_y();
            make_pill(start_y, ITEM_H)
                .with_animation(
                    ElementId::Name(format!("settings-rail-pill-{tab_gen}").into()),
                    gpui::Animation::new(std::time::Duration::from_millis(170)),
                    move |_el, delta| {
                        let (y, h) =
                            controls::compute_pill_geometry(start_y, end_y, ITEM_H, delta);
                        make_pill(y, h)
                    },
                )
                .into_any_element()
        } else {
            make_pill(active.rail_y(), ITEM_H).into_any_element()
        };

        let group_label = |title: &'static str| {
            div()
                .h(zpx(GROUP_LABEL_H))
                .px_2p5()
                .flex()
                .items_center()
                .t_caption()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(c(t.text_faint))
                .child(title)
        };

        let render_item = |tab: SettingsTab| {
            let is_active = tab == active;
            let badge = self.rail_badge(tab);
            let show_dot = tab == SettingsTab::General && update_ready;
            div()
                .id(ElementId::Name(format!("settings-tab-{}", tab.label()).into()))
                .h(zpx(ITEM_H))
                .px_2p5()
                .rounded(RADIUS_MD)
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .cursor_pointer()
                .when(!is_active, |d| d.hover_fade(c(t.hover), None))
                .active(|s| s.opacity(0.8))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.select_settings_tab(tab, cx);
                }))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2p5()
                        .min_w_0()
                        .child(
                            tab.icon()
                                .el(zpx(14.))
                                .text_color(c(if is_active { t.text } else { t.text_muted })),
                        )
                        .child(
                            div()
                                .t_body()
                                .font_weight(if is_active {
                                    gpui::FontWeight::MEDIUM
                                } else {
                                    gpui::FontWeight::NORMAL
                                })
                                .text_color(c(if is_active { t.text } else { t.text_muted }))
                                .child(tab.label()),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1p5()
                        .when(show_dot, |d| {
                            d.child(div().size(zpx(6.)).rounded_full().bg(c(t.accent)))
                        })
                        .when_some(badge, |d, count| {
                            d.child(
                                div()
                                    .t_caption()
                                    .text_color(c(if is_active {
                                        t.text_muted
                                    } else {
                                        t.text_faint
                                    }))
                                    .child(count),
                            )
                        }),
                )
        };

        let nav_list = div()
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .child(sliding_pill)
            .child(group_label("APP"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(ITEM_GAP))
                    .child(render_item(SettingsTab::General))
                    .child(render_item(SettingsTab::Notifications)),
            )
            .child(div().h(zpx(GROUP_GAP)))
            .child(group_label("AGENT"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(ITEM_GAP))
                    .child(render_item(SettingsTab::Accounts))
                    .child(render_item(SettingsTab::Skills))
                    .child(render_item(SettingsTab::Memory))
                    .child(render_item(SettingsTab::Integrations)),
            );

        div()
            .w(zpx(RAIL_WIDTH))
            .flex_none()
            .h_full()
            .bg(c(t.sidebar))
            .border_r_1()
            .border_color(c(t.border))
            .px_3()
            .pt_4()
            .pb_4()
            .flex()
            .flex_col()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .h(zpx(24.))
                            .px_2p5()
                            .flex()
                            .items_center()
                            .text_size(zpx(15.))
                            .line_height(zpx(20.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(t.text))
                            .child("Settings"),
                    )
                    .child(nav_list),
            )
            .child(
                div()
                    .px_2p5()
                    .pt_3()
                    .border_t_1()
                    .border_color(c(t.border))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .t_caption()
                            .text_color(c(t.text_faint))
                            .child(format!("Keel {}", keel_update::CURRENT_VERSION)),
                    )
                    .when(update_ready, |d| {
                        d.child(
                            div()
                                .t_caption()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(c(t.accent))
                                .child("Update ready"),
                        )
                    }),
            )
    }

    fn render_detail_pane(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let tab = self.settings_tab;
        let prev = self.settings_tab_prev;
        let animating = self.recent_tab_switch();
        let tab_gen = self.settings_tab_gen;
        let dir: f32 = if tab.index() >= prev.index() { 1.0 } else { -1.0 };

        let header = div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .border_b_1()
            .border_color(c(t.border))
            .child(
                div()
                    .w_full()
                    .max_w(zpx(DETAIL_MAX_WIDTH))
                    .px_6()
                    .h(zpx(52.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(zpx(15.))
                                    .line_height(zpx(20.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(c(t.text))
                                    .child(tab.label()),
                            )
                            .children(tab.info_tip().map(|tip| {
                                controls::info_badge(format!("tab-info-{}", tab.label()), tip, t)
                            })),
                    )
                    .child(
                        div()
                            .id("settings-done-btn")
                            .size(zpx(26.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(RADIUS_MD)
                            .text_color(c(t.text_muted))
                            .cursor_pointer()
                            .hover(|s| s.text_color(c(t.text)))
                            .hover_fade(c(t.hover), Some(c(t.selected)))
                            .active(|s| s.opacity(0.8))
                            .tooltip(move |_, cx| {
                                cx.new(|_| crate::view::dotify_ui::Tip::new("Close (Esc)", t))
                                    .into()
                            })
                            .tooltip_show_delay(crate::view::tooltip::delay())
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.close_settings(cx);
                            }))
                            .child(Icon::Close.el(zpx(14.))),
                    ),
            );

        // Skills manages its own virtualized list scroll, so it gets a
        // full-height non-scrolling flex container; all other tabs use a
        // single clean vertical scroll area (though General/Notifications/
        // Integrations fit in a single viewport).
        let is_skills = tab == SettingsTab::Skills;
        let tab_body: gpui::Div = match tab {
            SettingsTab::General => self.render_general_tab(t, cx),
            SettingsTab::Notifications => self.render_notifications_tab(t, cx),
            SettingsTab::Accounts => self.render_accounts_tab(t, cx),
            SettingsTab::Skills => self.render_skills_tab(t, cx),
            SettingsTab::Memory => self.render_memory_tab(t, cx),
            SettingsTab::Integrations => self.render_integrations_tab(t, cx),
        };

        let animated_body: AnyElement = if animating {
            tab_body
                .with_animation(
                    ElementId::Name(format!("settings-pane-in-{tab_gen}").into()),
                    gpui::Animation::new(crate::motion::BASE)
                        .with_easing(crate::motion::ease_out_strong),
                    move |el, delta| {
                        el.opacity(delta)
                            .relative()
                            .top(zpx(6.0 * dir * (1.0 - delta)))
                    },
                )
                .into_any_element()
        } else {
            tab_body.into_any_element()
        };

        let body_shell: AnyElement = if is_skills {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .w_full()
                        .max_w(zpx(DETAIL_MAX_WIDTH))
                        .flex_1()
                        .min_h_0()
                        .px_6()
                        .pt_4()
                        .pb_4()
                        .flex()
                        .flex_col()
                        .child(animated_body),
                )
                .into_any_element()
        } else {
            div()
                .id("settings-scroll")
                .track_scroll(&self.dotify.settings_scroll)
                .when(self.show_sound_menu, |d| {
                    d.on_scroll(cx.listener(|this, _, _, cx| {
                        if this.show_sound_menu {
                            this.show_sound_menu = false;
                            this.sound_menu_scroll_y = None;
                            cx.notify();
                        }
                    }))
                })
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .w_full()
                        .max_w(zpx(DETAIL_MAX_WIDTH))
                        .px_6()
                        .pt_4()
                        .pb_6()
                        .flex()
                        .flex_col()
                        .child(animated_body),
                )
                .into_any_element()
        };

        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .child(body_shell)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_blends_each_channel() {
        assert_eq!(controls::mix(0x000000FF, 0xFF8040FF, 0.), 0x000000FF);
        assert_eq!(controls::mix(0x000000FF, 0xFF8040FF, 1.), 0xFF8040FF);
        assert_eq!(controls::mix(0x000000FF, 0xFE8040FF, 0.5), 0x7F4020FF);
    }

    #[test]
    fn settings_tabs_have_monotonic_rail_positions() {
        let tabs = [
            SettingsTab::General,
            SettingsTab::Notifications,
            SettingsTab::Accounts,
            SettingsTab::Skills,
            SettingsTab::Memory,
            SettingsTab::Integrations,
        ];
        for pair in tabs.windows(2) {
            assert!(pair[0].rail_y() < pair[1].rail_y());
            assert!(pair[0].index() < pair[1].index());
        }
        assert_eq!(SettingsTab::from_name("notifications"), Some(SettingsTab::Notifications));
        assert_eq!(SettingsTab::from_name("browser"), Some(SettingsTab::Integrations));
    }

    #[test]
    fn harness_status_rows_parse() {
        let row = HarnessStatus::parse(&serde_json::json!({
            "id": "keel",
            "display_name": "Keel",
            "description": "Keel's agent",
            "installed": true,
            "version": "1.0.5",
            "path": "/Applications/Keel.app/Contents/Resources/pi/pi",
            "accounts": [
                {
                    "id": "anthropic",
                    "name": "Claude",
                    "hint": "Claude Pro or Max, or ANTHROPIC_API_KEY",
                    "signed_in": true,
                    "detail": "oauth",
                    "supports_oauth": true
                },
                {
                    "id": "openai-codex",
                    "name": "ChatGPT",
                    "hint": "ChatGPT Plus or Pro (Codex)",
                    "signed_in": false,
                    "detail": "",
                    "supports_oauth": true
                },
                {
                    "id": "commandcode",
                    "name": "Command Code",
                    "hint": "Command Code API key (COMMANDCODE_API_KEY)",
                    "signed_in": true,
                    "detail": "api_key",
                    "supports_oauth": false,
                    "from_env": false
                },
            ]
        }));
        assert_eq!(row.id, "keel");
        assert!(row.installed);
        let accounts = row.accounts.unwrap();
        assert_eq!(accounts.len(), 3);
        assert!(accounts[0].signed_in);
        assert_eq!(accounts[0].credential(), "Subscription");
        assert!(accounts[0].supports_oauth);
        assert!(!accounts[1].signed_in);
        assert!(accounts[2].signed_in);
        assert_eq!(accounts[2].credential(), "API key");
        assert!(!accounts[2].supports_oauth);
        assert!(!accounts[2].from_env);
    }

    #[test]
    fn skill_row_parses_author_and_source_url() {
        let row = SkillRow::parse(&serde_json::json!({
            "name": "animate",
            "description": "Motion craft",
            "insert": "animate",
            "scope": "user",
            "path": "/Users/test/.agents/skills/animate",
            "enabled": true,
            "manual_only": false,
            "removable": true,
            "collection": "emil",
            "author": "emilkowalski",
            "source_url": "https://github.com/emilkowalski/skills",
        }));
        assert_eq!(row.name, "animate");
        assert_eq!(row.author, "emilkowalski");
        assert_eq!(row.source_url, "https://github.com/emilkowalski/skills");
        assert_eq!(row.collection, "emil");
    }

    #[test]
    fn skill_row_parses_missing_author_gracefully() {
        let row = SkillRow::parse(&serde_json::json!({
            "name": "local-only",
            "description": "Local skill",
            "scope": "user",
            "path": "/tmp/local-only",
            "enabled": false,
        }));
        assert_eq!(row.author, "");
        assert_eq!(row.source_url, "");
        assert_eq!(row.collection, "");
    }
}

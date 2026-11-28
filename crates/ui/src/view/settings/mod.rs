//! Settings (⌘,): split-pane master-detail layout with an animated category
//! rail on the left and focused, card-grouped controls on the right.

mod accounts;
mod controls;
mod general;
mod integrations;
mod memory;
mod notifications;
mod shortcuts;
mod skills;

use gpui::{
    AnimationExt, AnyElement, ClickEvent, Context, ElementId, IntoElement, SpringAnimation,
    UniformListScrollHandle, div, prelude::*,
};
use keel_theme::Tokens;

use super::KeelWindow;
use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::style::{HEADER_H, KeelStyled, NAV_ITEM_H, c, r_md, zpx};

pub(super) use controls::{
    ButtonKind, button, icon_button, section_header, setting_row_info, settings_close_motion,
    settings_list, settings_open_motion, toggle,
};

const RAIL_WIDTH: f32 = 196.0;
const ITEM_GAP: f32 = 2.0;
const GROUP_GAP: f32 = 12.0;
/// First nav item's center lines up with the header title's center.
const RAIL_PAD_TOP: f32 = (HEADER_H - NAV_ITEM_H) / 2.0;

/// Top-level category in the split-pane Settings rail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SettingsTab {
    #[default]
    General,
    Notifications,
    Shortcuts,
    Accounts,
    Memory,
    Integrations,
}

impl SettingsTab {
    #[cfg(test)]
    pub(super) fn index(self) -> usize {
        match self {
            Self::General => 0,
            Self::Notifications => 1,
            Self::Shortcuts => 2,
            Self::Accounts => 3,
            Self::Memory => 4,
            Self::Integrations => 5,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Notifications => "Notifications",
            Self::Shortcuts => "Shortcuts",
            Self::Accounts => "Accounts",
            Self::Memory => "Memory",
            Self::Integrations => "Integrations",
        }
    }

    pub(super) fn icon(self) -> Icon {
        match self {
            Self::General => Icon::Settings,
            Self::Notifications => Icon::Volume2,
            Self::Shortcuts => Icon::Keyboard,
            Self::Accounts => Icon::Agent,
            Self::Memory => Icon::Brain,
            Self::Integrations => Icon::Globe,
        }
    }

    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "general" | "appearance" | "updates" | "background" => Some(Self::General),
            "notifications" | "notify" => Some(Self::Notifications),
            "shortcuts" | "keys" | "keybindings" | "keyboard" => Some(Self::Shortcuts),
            "accounts" | "providers" => Some(Self::Accounts),
            "memory" => Some(Self::Memory),
            "integrations" | "browser" | "connections" => Some(Self::Integrations),
            _ => None,
        }
    }

    /// Vertical offset of this item inside the nav list. Two groups, no labels:
    /// General, Notifications, and Shortcuts, then a 12px gap, then the agent items.
    /// Items are [`NAV_ITEM_H`] tall and 2px apart.
    fn rail_y(self) -> f32 {
        let step = NAV_ITEM_H + ITEM_GAP;
        match self {
            Self::General => 0.,
            Self::Notifications => step,
            Self::Shortcuts => step * 2.,
            Self::Accounts => step * 2. + NAV_ITEM_H + GROUP_GAP,
            Self::Memory => step * 2. + NAV_ITEM_H + GROUP_GAP + step,
            Self::Integrations => step * 2. + NAV_ITEM_H + GROUP_GAP + step * 2.,
        }
    }
}

/// Active inline keystroke capture state in Settings → Shortcuts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ShortcutRecordingState {
    pub id: &'static str,
    pub error: Option<String>,
    pub error_seq: u64,
    pub held_modifiers: gpui::Modifiers,
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

/// Top-level mode inside Settings → Skills.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SkillsViewMode {
    #[default]
    Installed,
    Discover,
}

/// One entry in the curated remote skills catalog.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CatalogSkillRow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub category: String,
    pub trending: bool,
    pub repo: String,
    pub branch: String,
    pub path: String,
    pub source_url: String,
    pub commit: String,
    pub installed: bool,
    pub installed_path: Option<String>,
    pub update_available: bool,
    pub avatar_path: Option<String>,
}

impl CatalogSkillRow {
    pub(crate) fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Self {
            id: s("id"),
            name: s("name"),
            description: s("description"),
            author: s("author"),
            category: s("category"),
            trending: v["trending"].as_bool().unwrap_or(false),
            repo: s("repo"),
            branch: s("branch"),
            path: s("path"),
            source_url: s("source_url"),
            commit: s("commit"),
            installed: v["installed"].as_bool().unwrap_or(false),
            installed_path: v["installed_path"].as_str().map(str::to_string),
            update_available: v["update_available"].as_bool().unwrap_or(false),
            avatar_path: v["avatar_path"].as_str().map(str::to_string),
        }
    }

    pub(crate) fn git_ref(&self) -> &str {
        if !self.commit.is_empty() {
            &self.commit
        } else if !self.branch.is_empty() {
            &self.branch
        } else {
            "main"
        }
    }

    pub(crate) fn to_entry_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "description": self.description,
            "author": self.author,
            "category": self.category,
            "trending": self.trending,
            "repo": self.repo,
            "branch": self.branch,
            "path": self.path,
            "source_url": self.source_url,
            "commit": self.commit,
        })
    }
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
    pub view_mode: SkillsViewMode,
    pub transition: Option<(u64, bool)>,
    pub load_gen: u64,
    pub rows: Option<Result<Vec<SkillRow>, String>>,
    pub added: Option<(String, String)>,
    pub notice: Option<String>,
    pub confirm: Option<SkillConfirm>,
    pub confirm_gen: u64,
    pub confirm_leaving: Option<(u64, SkillConfirm)>,
    pub row_errors: std::collections::HashMap<String, String>,
    pub list_scroll: UniformListScrollHandle,
    pub author_filter: Option<String>,
    pub status_filter: SkillStatusFilter,
    pub filter_epoch: u64,
    pub expanded_skill: Option<String>,
    pub expanded_content: std::collections::HashMap<String, String>,
    pub modal_skill: Option<SkillRow>,
    pub modal_leaving: Option<(u64, SkillRow)>,
    pub modal_scroll: gpui::ScrollHandle,
    /// The creator filter menu next to the search.
    pub creator_menu_open: bool,
    pub creator_btn_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    // Discover catalog state (stale-while-revalidate client refresh).
    pub catalog_gen: u64,
    pub catalog_rows: Option<Result<Vec<CatalogSkillRow>, String>>,
    pub catalog_source: String,
    pub catalog_refreshing: bool,
    pub catalog_warning: Option<String>,
    pub catalog_list_scroll: UniformListScrollHandle,
    pub catalog_scroll: gpui::ScrollHandle,
    pub installed_scroll: gpui::ScrollHandle,
    pub installed_usage_filter: Option<String>,
    pub installed_strip_origin: std::rc::Rc<std::cell::Cell<Option<gpui::Point<gpui::Pixels>>>>,
    pub installed_chip_bounds:
        std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, gpui::Bounds<gpui::Pixels>>>>,
    pub category_filter: Option<String>,
    pub category_menu_open: bool,
    pub catalog_author_filter: Option<String>,
    pub catalog_filter_menu_open: bool,
    pub catalog_filter_btn_bounds:
        std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    pub installing: std::collections::HashSet<String>,
    pub install_errors: std::collections::HashMap<String, String>,
    pub modal_catalog: Option<CatalogSkillRow>,
    pub modal_catalog_leaving: Option<(u64, CatalogSkillRow)>,
    pub catalog_readmes: std::collections::HashMap<String, Result<String, String>>,
    pub catalog_readme_loading: std::collections::HashSet<String>,
    /// Cached GitHub creator avatar PNGs keyed by lowercase repo owner.
    pub catalog_avatars: std::collections::HashMap<String, std::path::PathBuf>,
    pub catalog_avatars_fetching: bool,
    /// Inline detail page navigation transition (forward = into detail, back = to list).
    pub detail_nav_gen: u64,
    pub detail_nav_forward: bool,
    pub detail_nav_at: Option<std::time::Instant>,
    /// Tab switch & filter settle transitions (Emil directional crossfade + subtle rise).
    pub tab_nav_gen: u64,
    pub tab_nav_forward: bool,
    pub tab_nav_at: Option<std::time::Instant>,
    pub filter_at: Option<std::time::Instant>,
    pub filter_forward: bool,
    pub category_strip_origin: std::rc::Rc<std::cell::Cell<Option<gpui::Point<gpui::Pixels>>>>,
    pub category_chip_bounds:
        std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, gpui::Bounds<gpui::Pixels>>>>,
}

impl Default for SkillsPage {
    fn default() -> Self {
        Self {
            open: false,
            view_mode: SkillsViewMode::Installed,
            transition: None,
            load_gen: 0,
            rows: None,
            added: None,
            notice: None,
            confirm: None,
            confirm_gen: 0,
            confirm_leaving: None,
            row_errors: std::collections::HashMap::new(),
            list_scroll: UniformListScrollHandle::new(),
            author_filter: None,
            status_filter: SkillStatusFilter::All,
            filter_epoch: 0,
            expanded_skill: None,
            expanded_content: std::collections::HashMap::new(),
            modal_skill: None,
            modal_leaving: None,
            modal_scroll: gpui::ScrollHandle::default(),
            creator_menu_open: false,
            creator_btn_bounds: Default::default(),
            catalog_gen: 0,
            catalog_rows: None,
            catalog_source: String::new(),
            catalog_refreshing: false,
            catalog_warning: None,
            catalog_list_scroll: UniformListScrollHandle::new(),
            catalog_scroll: gpui::ScrollHandle::default(),
            installed_scroll: gpui::ScrollHandle::default(),
            installed_usage_filter: None,
            installed_strip_origin: Default::default(),
            installed_chip_bounds: Default::default(),
            category_filter: None,
            category_menu_open: false,
            catalog_author_filter: None,
            catalog_filter_menu_open: false,
            catalog_filter_btn_bounds: Default::default(),
            installing: std::collections::HashSet::new(),
            install_errors: std::collections::HashMap::new(),
            modal_catalog: None,
            modal_catalog_leaving: None,
            catalog_readmes: std::collections::HashMap::new(),
            catalog_readme_loading: std::collections::HashSet::new(),
            catalog_avatars: std::collections::HashMap::new(),
            catalog_avatars_fetching: false,
            detail_nav_gen: 0,
            detail_nav_forward: true,
            detail_nav_at: None,
            tab_nav_gen: 0,
            tab_nav_forward: true,
            tab_nav_at: None,
            filter_at: None,
            filter_forward: true,
            category_strip_origin: Default::default(),
            category_chip_bounds: Default::default(),
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
        self.skills.modal_catalog = None;
        self.skills.modal_catalog_leaving = None;
        self.skills.category_menu_open = false;
        self.skills.creator_menu_open = false;
        self.memory.open = false;
        self.memory.transition = None;
        self.memory.modal = None;
        self.memory.modal_leaving = None;
        self.memory.confirm = None;
        self.show_sound_menu = false;
        self.shortcut_recording = None;
        self.shortcut_reset_confirm = None;
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
        self.settings_tab = tab;
        self.settings_tab_gen = self.settings_tab_gen.wrapping_add(1);
        self.settings_tab_at = Some(std::time::Instant::now());
        self.show_sound_menu = false;
        self.shortcut_recording = None;
        self.shortcut_reset_confirm = None;
        self.dotify
            .settings_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        if tab == SettingsTab::Memory {
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
            .children(memory_modal)
    }

    fn render_nav_rail(&self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let active = self.settings_tab;
        let update_ready = matches!(self.updates.status, keel_update::Status::Ready { .. })
            && self.updates.ready.is_some();

        let sliding_pill = div()
            .absolute()
            .left_0()
            .right_0()
            .h(zpx(NAV_ITEM_H))
            .rounded(r_md())
            .bg(c(t.selected))
            .with_spring(
                "settings-rail-pill",
                SpringAnimation::new(controls::PILL_SPRING).to(zpx(active.rail_y())),
                |el, y| el.top(y),
            );

        let render_item = |tab: SettingsTab| {
            let is_active = tab == active;
            let show_dot = tab == SettingsTab::General && update_ready;
            div()
                .id(ElementId::Name(
                    format!("settings-tab-{}", tab.label()).into(),
                ))
                .h(zpx(NAV_ITEM_H))
                .px(zpx(10.))
                .rounded(r_md())
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(zpx(8.))
                .cursor_pointer()
                .hover_fade_when(!is_active, c(t.hover), None)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.select_settings_tab(tab, cx);
                }))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.))
                        .min_w_0()
                        .child(tab.icon().el(zpx(14.)).text_color(c(t.text_muted)))
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
                .when(show_dot, |d| {
                    d.child(
                        div()
                            .size(zpx(6.))
                            .flex_none()
                            .rounded_full()
                            .bg(c(t.accent)),
                    )
                })
        };

        let nav_list = div()
            .id("settings-nav-list")
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .child(sliding_pill)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(ITEM_GAP))
                    .child(render_item(SettingsTab::General))
                    .child(render_item(SettingsTab::Notifications))
                    .child(render_item(SettingsTab::Shortcuts)),
            )
            .child(div().h(zpx(GROUP_GAP)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(ITEM_GAP))
                    .child(render_item(SettingsTab::Accounts))
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
            .px(zpx(8.))
            .pb(zpx(16.))
            .flex()
            .flex_col()
            .justify_between()
            .child(div().pt(zpx(RAIL_PAD_TOP)).child(nav_list))
            .child(
                div()
                    .px(zpx(10.))
                    .t_caption()
                    .text_color(c(t.text_faint))
                    .child(format!("Keel {}", keel_update::CURRENT_VERSION)),
            )
    }

    fn render_detail_pane(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let tab = self.settings_tab;
        let animating = self.recent_tab_switch();
        let tab_gen = self.settings_tab_gen;

        let dismiss_tip = crate::keys::display_for_id(
            &self.state.ui_settings.shortcuts,
            "dismiss",
            crate::keys::current_os(),
        )
        .map(|k| format!("Close ({k})"))
        .unwrap_or_else(|| "Close".to_string());

        let header = div()
            .w_full()
            .h(zpx(HEADER_H))
            .px(zpx(24.))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(c(t.border))
            .child(div().t_title().text_color(c(t.text)).child(tab.label()))
            .child(
                icon_button("settings-done-btn", Icon::Close, t)
                    .tooltip(move |_, cx| {
                        cx.new(|_| crate::view::dotify_ui::Tip::new(dismiss_tip.clone(), t))
                            .into()
                    })
                    .tooltip_show_delay(crate::view::tooltip::delay())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.close_settings(cx);
                    })),
            );

        let tab_body: gpui::Div = match tab {
            SettingsTab::General => self.render_general_tab(t, cx),
            SettingsTab::Notifications => self.render_notifications_tab(t, cx),
            SettingsTab::Shortcuts => self.render_shortcuts_tab(t, cx),
            SettingsTab::Accounts => self.render_accounts_tab(t, cx),
            SettingsTab::Memory => self.render_memory_tab(t, cx),
            SettingsTab::Integrations => self.render_integrations_tab(t, cx),
        };

        // Opacity only. The generation id replays on a tab change and stays
        // put for clicks inside the tab. Reduced motion snaps to opaque.
        let animated_body: AnyElement = tab_body
            .with_animation(
                ElementId::Name(format!("settings-pane-in-{tab_gen}").into()),
                gpui::Animation::new(std::time::Duration::from_millis(120))
                    .with_easing(crate::motion::ease_out),
                move |el, delta| if animating { el.opacity(delta) } else { el },
            )
            .into_any_element();

        let padded = div()
            .w_full()
            .px(zpx(24.))
            .pt(zpx(16.))
            .pb(zpx(24.))
            .flex()
            .flex_col()
            .child(animated_body);

        let body_shell: AnyElement = div()
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
            .child(padded)
            .into_any_element();

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
    fn settings_tabs_have_monotonic_rail_positions() {
        let tabs = [
            SettingsTab::General,
            SettingsTab::Notifications,
            SettingsTab::Shortcuts,
            SettingsTab::Accounts,
            SettingsTab::Memory,
            SettingsTab::Integrations,
        ];
        for pair in tabs.windows(2) {
            assert!(pair[0].rail_y() < pair[1].rail_y());
            assert!(pair[0].index() < pair[1].index());
        }
        // 30px items, 2px apart, 12px between the two groups, no group labels.
        assert_eq!(SettingsTab::General.rail_y(), 0.);
        assert_eq!(SettingsTab::Notifications.rail_y(), 32.);
        assert_eq!(SettingsTab::Shortcuts.rail_y(), 64.);
        assert_eq!(SettingsTab::Accounts.rail_y(), 106.);
        assert_eq!(SettingsTab::Memory.rail_y(), 138.);
        assert_eq!(SettingsTab::Integrations.rail_y(), 170.);
        assert_eq!(
            SettingsTab::from_name("notifications"),
            Some(SettingsTab::Notifications)
        );
        assert_eq!(
            SettingsTab::from_name("shortcuts"),
            Some(SettingsTab::Shortcuts)
        );
        assert_eq!(
            SettingsTab::from_name("browser"),
            Some(SettingsTab::Integrations)
        );
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

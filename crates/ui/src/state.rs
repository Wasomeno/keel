//! UI state, registry views, tabs, space filtering, and settings.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use keel_proto::{
    AttentionChat, ChatStatus, DisplayStatus, ThemeChoice, UiSettings, attention_sort,
    display_status,
};
use keel_theme::{Tokens, resolve_reduced_motion, resolve_theme};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceItem {
    pub id: String,
    pub name: String,
    pub path: String,
    pub device_id: String,
    pub tombstoned: bool,
    pub git_detected: bool,
    pub checkout_id: String,
    pub repository_id: String,
    /// Creation time (ms). Read from the registry view when the engine sends it;
    /// set locally for spaces this UI created.
    pub created_at: i64,
}

impl SpaceItem {
    pub fn test(id: &str, name: &str, path: &str, device_id: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            path: path.to_string(),
            device_id: device_id.to_string(),
            tombstoned: false,
            git_detected: false,
            checkout_id: String::new(),
            repository_id: String::new(),
            created_at: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatItem {
    pub id: String,
    pub space_id: String,
    pub title: String,
    pub status: ChatStatus,
    pub last_message_at: i64,
    pub last_seen_at: i64,
    pub archived: bool,
    pub tombstoned: bool,
    pub branch: String,
    pub checkout_id: String,
    pub cwd: String,
    /// Harness/model/reasoning when known (registry view or set by this UI).
    pub harness: String,
    pub model: String,
    pub reasoning: String,
    /// False until a registry view actually carries `reasoning`. An older row
    /// omits the field; that is not the same as the harness default.
    pub reasoning_known: bool,
    /// `keel_proto::PermissionMode` name; empty is Ask.
    pub permission: String,
    /// Plan mode for this chat. Off when the row has no saved value.
    pub plan_mode: bool,
    /// Tokens spent and the context after the last turn (registry view).
    pub usage: crate::usage::ChatUsage,
}

impl ChatItem {
    pub fn test(
        id: &str,
        space_id: &str,
        title: &str,
        status: ChatStatus,
        last_message_at: i64,
        last_seen_at: i64,
    ) -> Self {
        Self {
            id: id.to_string(),
            space_id: space_id.to_string(),
            title: title.to_string(),
            status,
            last_message_at,
            last_seen_at,
            archived: false,
            tombstoned: false,
            branch: String::new(),
            checkout_id: String::new(),
            cwd: String::new(),
            harness: String::new(),
            model: String::new(),
            reasoning: String::new(),
            reasoning_known: false,
            permission: String::new(),
            plan_mode: false,
            usage: Default::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffItem {
    pub checkout_id: String,
    pub additions: i64,
    pub deletions: i64,
    pub files: i64,
}

pub const SIDEBAR_MIN: f32 = 208.;
pub const SIDEBAR_MAX: f32 = 400.;
pub const SIDEBAR_DEFAULT: f32 = 248.;

fn clamp_sidebar(width: f32) -> f32 {
    if width.is_finite() {
        width.clamp(SIDEBAR_MIN, SIDEBAR_MAX)
    } else {
        SIDEBAR_DEFAULT
    }
}

/// Rows an open space shows before "Show N more".
pub const SPACE_ROW_CAP: usize = 5;

/// One space section of the sidebar.
#[derive(Debug, Clone)]
pub struct SpaceSection {
    pub space_id: String,
    pub name: String,
    /// Every visible chat in the space.
    pub total: usize,
    /// The space's most urgent status (its first chat in attention order).
    pub urgent: DisplayStatus,
    pub collapsed: bool,
    /// Rows drawn, in attention order.
    pub rows: Vec<(AttentionChat, DisplayStatus)>,
    /// Chats held back behind "Show N more".
    pub hidden: usize,
    /// Keel's own "No project" folder: drawn apart from the real folders, last.
    pub loose: bool,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub data_dir: PathBuf,
    pub ui_settings: UiSettings,
    pub active_tab: Option<String>,
    pub spaces: BTreeMap<String, SpaceItem>,
    pub chats: BTreeMap<String, ChatItem>,
    pub diffs: BTreeMap<String, DiffItem>,
    /// `None` represents "All spaces".
    pub space_filter: Option<String>,
    /// Folder picked on the new-chat screen; wins over the filter for the next chat.
    pub new_chat_space: Option<String>,
    /// User explicitly picked "No project" (per-chat ephemeral scratch folder).
    pub new_chat_no_project: bool,
    pub local_device_id: String,
    /// The space whose header menu is open.
    pub space_menu: Option<String>,
    /// Spaces showing every chat instead of the first [`SPACE_ROW_CAP`]. Session-only.
    pub space_show_all: BTreeSet<String>,
    pub show_overflow_menu: bool,
    pub renaming_space: Option<(String, String)>,
    pub confirming_delete_space: Option<String>,
    pub selected_branch: Option<String>,
    pub available_branches: Vec<String>,
    pub show_branch_picker: bool,
    /// The folder the branch fields describe; `None` until fetched.
    pub branch_folder: Option<String>,
    /// That folder's checked-out branch.
    pub current_branch: Option<String>,
    /// Short sha when that folder's HEAD is detached.
    pub detached_head: Option<String>,
    /// The user picked `selected_branch` themselves: a fetch never replaces
    /// it. Cleared when the folder changes.
    pub branch_manual: bool,
    /// The folder changed and its branches are still being fetched: the
    /// chip keeps its previous label until they arrive.
    pub branch_pending: bool,
}

impl AppState {
    pub fn new(data_dir: PathBuf, local_device_id: String) -> Self {
        let settings_path = data_dir.join("ui-settings.json");
        let ui_settings = UiSettings::read_from(&settings_path).unwrap_or_default();
        // Launch on the new-session screen; saved tabs stay open, unselected.
        let active_tab = None;
        Self {
            data_dir,
            ui_settings,
            active_tab,
            spaces: BTreeMap::new(),
            chats: BTreeMap::new(),
            diffs: BTreeMap::new(),
            space_filter: None,
            new_chat_space: None,
            new_chat_no_project: false,
            local_device_id,
            space_menu: None,
            space_show_all: BTreeSet::new(),
            show_overflow_menu: false,
            renaming_space: None,
            confirming_delete_space: None,
            selected_branch: None,
            available_branches: Vec::new(),
            show_branch_picker: false,
            branch_folder: None,
            current_branch: None,
            detached_head: None,
            branch_manual: false,
            branch_pending: false,
        }
    }

    /// The user's own pick from the branch menu.
    pub fn set_branch(&mut self, branch: Option<String>) {
        self.selected_branch = branch;
        self.branch_manual = true;
    }

    /// The new-chat folder changed: forget its branches and any pick.
    pub fn reset_branch_for_folder(&mut self) {
        self.selected_branch = None;
        self.branch_manual = false;
        self.available_branches.clear();
        self.current_branch = None;
        self.detached_head = None;
        self.branch_folder = None;
        self.branch_pending = true;
    }

    /// Leaving a folder for "No project": clear branches and stop pending load.
    pub fn clear_branches_for_no_project(&mut self) {
        self.reset_branch_for_folder();
        self.branch_pending = false;
    }

    /// True when `s` is Keel's synthetic scratch container (`{data_dir}/scratch`
    /// named `"No project"`) or a legacy `{data_dir}/No project` folder.
    pub fn is_scratch_space(&self, s: &SpaceItem) -> bool {
        let p = std::path::Path::new(&s.path);
        if p == self.data_dir.join("scratch") {
            return true;
        }
        p.file_name()
            .is_some_and(|n| n == "No project" || (n == "scratch" && s.name == "No project"))
    }

    /// True when `c` belongs to an ephemeral scratch / no-project chat rather
    /// than a real folder.
    pub fn is_scratch_chat(&self, c: &ChatItem) -> bool {
        if !c.cwd.is_empty() {
            let p = std::path::Path::new(&c.cwd);
            if p.starts_with(self.data_dir.join("scratch"))
                || p.file_name().is_some_and(|n| n == "No project")
            {
                return true;
            }
        }
        self.spaces
            .get(&c.space_id)
            .is_some_and(|s| self.is_scratch_space(s))
    }

    /// The branch chip's label, or `None` while a new folder's branches are
    /// in flight (keep showing the previous one). "Branch" only when the
    /// folder turned out to have none (or isn't a repo).
    pub fn branch_chip_label(&self) -> Option<String> {
        if self.branch_pending {
            return None;
        }
        let branch = self.selected_branch.clone().unwrap_or_default();
        Some(if branch.is_empty() {
            self.detached_head
                .as_ref()
                .map(|sha| format!("detached @{sha}"))
                .unwrap_or_else(|| "Branch".into())
        } else {
            branch
        })
    }

    /// A branch listing for `folder` arrived. Ignored when the folder is no
    /// longer the one the branch fields describe. The current branch is
    /// listed first and becomes the selection unless the user picked one.
    pub fn apply_branches(
        &mut self,
        folder: &str,
        mut branches: Vec<String>,
        current: Option<String>,
        detached: Option<String>,
    ) {
        if self.branch_folder.as_deref() != Some(folder) {
            return;
        }
        if let Some(name) = &current {
            branches.retain(|b| b != name);
            branches.insert(0, name.clone());
        }
        self.branch_pending = false;
        self.available_branches = branches;
        self.current_branch = current.clone();
        self.detached_head = if current.is_some() { None } else { detached };
        if !self.branch_manual {
            self.selected_branch = current;
        }
    }

    pub fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }

    pub fn effective_tokens(&self, system_dark: bool) -> Tokens {
        resolve_theme(self.ui_settings.theme, system_dark)
    }

    pub fn effective_reduced_motion(&self, os_val: bool) -> bool {
        resolve_reduced_motion(self.ui_settings.reduced_motion, os_val)
    }

    pub fn save_settings(&self) {
        let path = self.data_dir.join("ui-settings.json");
        let _ = self.ui_settings.write_to(&path);
    }

    /// Visible chats in attention order.
    ///
    /// Excludes tombstoned and archived chats.
    /// Respects `space_filter` (or All spaces if None).
    /// Orders via `keel_proto::view::attention_sort`.
    pub fn visible_chats(&self, now: i64) -> Vec<(AttentionChat, DisplayStatus)> {
        let mut attention_list: Vec<AttentionChat> = self
            .chats
            .values()
            .filter(|chat| !chat.tombstoned && !chat.archived)
            .filter(|chat| match &self.space_filter {
                Some(space_id) => &chat.space_id == space_id,
                None => true,
            })
            .map(|chat| AttentionChat {
                id: chat.id.clone(),
                space_id: chat.space_id.clone(),
                status: chat.status,
                last_message_at: chat.last_message_at,
                last_seen_at: chat.last_seen_at,
            })
            .collect();

        attention_sort(&mut attention_list, now);

        attention_list
            .into_iter()
            .map(|chat| {
                let disp = display_status(chat.status, chat.last_seen_at, now);
                (chat, disp)
            })
            .collect()
    }

    /// The sidebar's space sections, by name. Each keeps attention order
    /// inside. A folded space still shows chats waiting on you and the
    /// active one, so folding never hides work that needs you. A focused
    /// space (`space_filter`) is the only section and shows everything.
    pub fn sidebar_sections(&self, now: i64) -> Vec<SpaceSection> {
        self.sidebar_sections_skipping(now, &HashSet::new())
    }

    /// Like [`Self::sidebar_sections`], leaving out chats that belong to a
    /// project. Those sessions live under the project, not under a folder.
    pub fn sidebar_sections_skipping(&self, now: i64, skip: &HashSet<String>) -> Vec<SpaceSection> {
        let chats: Vec<_> = self
            .visible_chats(now)
            .into_iter()
            .filter(|(chat, _)| !skip.contains(&chat.id))
            .collect();
        let focused = self.space_filter.is_some();
        let mut spaces: Vec<(String, String, bool)> = self
            .spaces
            .values()
            .filter(|s| !s.tombstoned && self.space_filter.as_ref().is_none_or(|f| f == &s.id))
            .map(|s| (s.id.clone(), s.name.clone(), self.is_scratch_space(s)))
            .collect();
        spaces.sort_by_key(|(_, name, loose)| (*loose, name.to_lowercase()));
        // A chat whose space row hasn't synced yet still needs a section.
        for (chat, _) in &chats {
            if !spaces.iter().any(|(id, ..)| id == &chat.space_id) {
                let at = spaces
                    .iter()
                    .position(|(.., loose)| *loose)
                    .unwrap_or(spaces.len());
                spaces.insert(at, (chat.space_id.clone(), "Space".into(), false));
            }
        }
        let active = self.active_tab.as_deref();
        let build_section = |space_id: String, name: String, loose: bool, ids: &[String]| {
            let all: Vec<_> = chats
                .iter()
                .filter(|(c, _)| ids.iter().any(|id| id == &c.space_id))
                .cloned()
                .collect();
            // "No project" isn't a real folder: nothing to fold.
            let collapsed =
                !focused && !loose && self.ui_settings.collapsed_spaces.contains(&space_id);
            let show_all = focused || ids.iter().any(|id| self.space_show_all.contains(id));
            let pinned = |(c, s): &(AttentionChat, DisplayStatus)| {
                *s == DisplayStatus::AwaitingInput || Some(c.id.as_str()) == active
            };
            let rows: Vec<_> = all
                .iter()
                .enumerate()
                .filter(|(ix, row)| {
                    pinned(row) || (!collapsed && (show_all || *ix < SPACE_ROW_CAP))
                })
                .map(|(_, row)| row.clone())
                .collect();
            SpaceSection {
                urgent: all.first().map_or(DisplayStatus::Idle, |(_, s)| *s),
                total: all.len(),
                hidden: if collapsed { 0 } else { all.len() - rows.len() },
                space_id,
                name,
                collapsed,
                rows,
                loose,
            }
        };
        let mut sections = Vec::new();
        let mut loose_spaces: Vec<(String, String)> = Vec::new();
        for (space_id, name, loose) in spaces {
            if loose {
                loose_spaces.push((space_id, name));
            } else {
                let ids = [space_id.clone()];
                sections.push(build_section(space_id, name, false, &ids));
            }
        }
        if !loose_spaces.is_empty() {
            let first = &loose_spaces[0];
            let space_id = first.0.clone();
            let name = first.1.clone();
            let ids: Vec<String> = loose_spaces.into_iter().map(|(id, _)| id).collect();
            sections.push(build_section(space_id, name, true, &ids));
        }
        sections
    }

    /// Fold or unfold a space section; remembered on this device.
    pub fn toggle_space_collapsed(&mut self, space_id: &str) {
        let folded = &mut self.ui_settings.collapsed_spaces;
        match folded.iter().position(|id| id == space_id) {
            Some(ix) => {
                folded.remove(ix);
            }
            None => folded.push(space_id.to_string()),
        }
        self.save_settings();
    }

    /// Clicking a sidebar row opens or focuses a tab.
    pub fn open_or_focus_tab(&mut self, chat_id: &str) {
        if !self.ui_settings.open_tabs.contains(&chat_id.to_string()) {
            self.ui_settings.open_tabs.push(chat_id.to_string());
        }
        self.active_tab = Some(chat_id.to_string());
        self.save_settings();
    }

    /// Closing a tab removes that chat id from openTabs and does not archive.
    pub fn close_tab(&mut self, chat_id: &str) {
        if let Some(pos) = self
            .ui_settings
            .open_tabs
            .iter()
            .position(|id| id == chat_id)
        {
            self.ui_settings.open_tabs.remove(pos);
            if self.active_tab.as_deref() == Some(chat_id) {
                if self.ui_settings.open_tabs.is_empty() {
                    self.active_tab = None;
                } else {
                    let next_pos = pos.min(self.ui_settings.open_tabs.len() - 1);
                    self.active_tab = Some(self.ui_settings.open_tabs[next_pos].clone());
                }
            }
            self.save_settings();
        }
    }

    /// Derive target space for new session.
    ///
    /// Current space filter, or, when the filter is All spaces, the newest real
    /// folder space this device created (by `created_at`). Never another device's
    /// space or Keel's scratch container as a fallback: `None` means either
    /// `"No project"` (when `new_chat_no_project` is set) or "ask for a folder".
    pub fn target_space_for_new_session(&self) -> Option<SpaceItem> {
        if let Some(space) = self
            .new_chat_space
            .as_ref()
            .and_then(|id| self.spaces.get(id))
            .filter(|s| !s.tombstoned)
        {
            return Some(space.clone());
        }
        if self.new_chat_no_project {
            return None;
        }
        if let Some(ref space_id) = self.space_filter {
            return self.spaces.get(space_id).cloned().filter(|s| !s.tombstoned);
        }
        self.spaces
            .values()
            .filter(|s| {
                !s.tombstoned
                    && s.device_id == self.local_device_id
                    && !self.is_scratch_space(s)
            })
            .max_by_key(|s| s.created_at)
            .cloned()
    }

    /// Record a space this UI just created via `create_space`, so it is the
    /// newest local space even before (or without) a registry `created_at`.
    pub fn record_created_space(&mut self, id: &str, path: &str) {
        let now = Self::now_ms();
        let device = self.local_device_id.clone();
        let scratch_root = self.data_dir.join("scratch");
        let space = self.spaces.entry(id.to_string()).or_insert_with(|| {
            let p = std::path::Path::new(path);
            let name = if p == scratch_root {
                "No project".to_string()
            } else {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string())
            };
            SpaceItem::test(id, &name, path, &device)
        });
        space.created_at = space.created_at.max(now);
        space.tombstoned = false;
    }

    /// Record Keel's synthetic scratch container space by id before its
    /// registry diff lands.
    pub fn record_created_scratch_space(&mut self, id: &str) {
        let path = self.data_dir.join("scratch").to_string_lossy().into_owned();
        self.record_created_space(id, &path);
    }

    pub fn draft(&self, chat_id: &str) -> String {
        self.ui_settings
            .drafts
            .get(chat_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Store (or clear) the device-local draft for a chat and persist it.
    pub fn set_draft(&mut self, chat_id: &str, text: &str) {
        let changed = if text.is_empty() {
            self.ui_settings.drafts.remove(chat_id).is_some()
        } else {
            self.ui_settings
                .drafts
                .insert(chat_id.to_string(), text.to_string())
                .as_deref()
                != Some(text)
        };
        if changed {
            self.save_settings();
        }
    }

    /// Called when a space is deleted. Drops its chat ids from openTabs.
    pub fn on_space_deleted(&mut self, space_id: &str) {
        if let Some(space) = self.spaces.get_mut(space_id) {
            space.tombstoned = true;
        }

        let mut dropped = Vec::new();
        for chat in self.chats.values_mut() {
            if chat.space_id == space_id {
                chat.tombstoned = true;
                dropped.push(chat.id.clone());
            }
        }

        self.ui_settings
            .open_tabs
            .retain(|id| !dropped.contains(id));
        if let Some(ref active) = self.active_tab
            && dropped.contains(active)
        {
            self.active_tab = self.ui_settings.open_tabs.first().cloned();
        }

        if self.space_filter.as_deref() == Some(space_id) {
            self.space_filter = None;
        }

        self.confirming_delete_space = None;
        self.save_settings();
    }

    pub fn set_theme(&mut self, choice: Option<ThemeChoice>) {
        self.ui_settings.theme = choice;
        self.save_settings();
    }

    /// Sidebar width, clamped to the range Zeron uses (208–400 px).
    pub fn sidebar_width(&self) -> f32 {
        clamp_sidebar(self.ui_settings.sidebar_width.unwrap_or(SIDEBAR_DEFAULT))
    }

    /// Live resize; persisted by [`Self::save_settings`] when the drag ends.
    pub fn set_sidebar_width(&mut self, width: f32) {
        self.ui_settings.sidebar_width = Some(clamp_sidebar(width));
    }

    /// The chat `delta` rows away from the active one in sidebar order
    /// (rows a folded space hides are skipped),
    /// wrapping. With nothing active, the first (or last) row.
    pub fn step_chat(&self, now: i64, delta: isize) -> Option<String> {
        let rows: Vec<String> = self
            .sidebar_sections(now)
            .into_iter()
            .flat_map(|s| s.rows)
            .map(|(chat, _)| chat.id)
            .collect();
        if rows.is_empty() {
            return None;
        }
        let len = rows.len() as isize;
        let next = match self
            .active_tab
            .as_ref()
            .and_then(|id| rows.iter().position(|r| r == id))
        {
            Some(ix) => (ix as isize + delta).rem_euclid(len),
            None if delta < 0 => len - 1,
            None => 0,
        };
        rows.get(next as usize).cloned()
    }

    /// The `n`th open tab, 1-based; 9 is the last tab, like browsers.
    pub fn tab_at(&self, n: usize) -> Option<String> {
        let tabs = &self.ui_settings.open_tabs;
        match n {
            9 => tabs.last().cloned(),
            1..=8 => tabs.get(n - 1).cloned(),
            _ => None,
        }
    }

    pub fn toggle_sidebar(&mut self) {
        self.ui_settings.sidebar_collapsed = !self.ui_settings.sidebar_collapsed;
        self.save_settings();
    }

    pub fn set_reduced_motion(&mut self, val: Option<bool>) {
        self.ui_settings.reduced_motion = val;
        self.save_settings();
    }

    /// Merge one registry space view. Only fields present in `view` change,
    /// because incremental diffs carry a subset of the row.
    pub fn update_space_from_view(&mut self, id: &str, view: &serde_json::Value) {
        if view
            .get("tombstoned")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            self.on_space_deleted(id);
            return;
        }
        let space = self
            .spaces
            .entry(id.to_string())
            .or_insert_with(|| SpaceItem::test(id, "", "", ""));
        merge_str(view, "name", &mut space.name);
        merge_str(view, "path", &mut space.path);
        merge_str(view, "device_id", &mut space.device_id);
        merge_bool(view, "git_detected", &mut space.git_detected);
        merge_str(view, "checkout_id", &mut space.checkout_id);
        merge_str(view, "repository_id", &mut space.repository_id);
        if let Some(v) = view.get("created_at").and_then(|v| v.as_i64()) {
            space.created_at = space.created_at.max(v);
        }
    }

    /// Merge one registry chat view (see `update_space_from_view`).
    pub fn update_chat_from_view(&mut self, id: &str, view: &serde_json::Value) {
        if view
            .get("tombstoned")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            if let Some(chat) = self.chats.get_mut(id) {
                chat.tombstoned = true;
            }
            self.close_tab(id);
            return;
        }
        let chat = self
            .chats
            .entry(id.to_string())
            .or_insert_with(|| ChatItem::test(id, "", "", ChatStatus::Idle, 0, 0));
        if let Some(status) = view.get("status").and_then(|v| v.as_str()) {
            chat.status = match status {
                "running" => ChatStatus::Running,
                "awaiting_input" => ChatStatus::AwaitingInput,
                "aborted" => ChatStatus::Aborted,
                _ => ChatStatus::Idle,
            };
        }
        merge_str(view, "space_id", &mut chat.space_id);
        merge_str(view, "title", &mut chat.title);
        merge_str(view, "cwd", &mut chat.cwd);
        merge_i64(view, "last_message_at", &mut chat.last_message_at);
        merge_i64(view, "last_seen_at", &mut chat.last_seen_at);
        merge_bool(view, "archived", &mut chat.archived);
        merge_str(view, "branch", &mut chat.branch);
        merge_str(view, "checkout_id", &mut chat.checkout_id);
        merge_str(view, "harness", &mut chat.harness);
        merge_str(view, "model", &mut chat.model);
        if view.get("reasoning").and_then(|v| v.as_str()).is_some() {
            chat.reasoning_known = true;
        }
        merge_str(view, "reasoning", &mut chat.reasoning);
        merge_str(view, "permission", &mut chat.permission);
        merge_bool(view, "plan_mode", &mut chat.plan_mode);
        if let Some(usage) = view.get("usage") {
            chat.usage = crate::usage::ChatUsage::from_view(usage);
        }
    }

    pub fn update_diff_from_view(&mut self, checkout_id: &str, view: &serde_json::Value) {
        let additions = view.get("additions").and_then(|v| v.as_i64()).unwrap_or(0);
        let deletions = view.get("deletions").and_then(|v| v.as_i64()).unwrap_or(0);
        let files = view.get("files").and_then(|v| v.as_i64()).unwrap_or(0);

        self.diffs.insert(
            checkout_id.to_string(),
            DiffItem {
                checkout_id: checkout_id.to_string(),
                additions,
                deletions,
                files,
            },
        );
    }
}

fn merge_str(view: &serde_json::Value, key: &str, out: &mut String) {
    if let Some(v) = view.get(key).and_then(|v| v.as_str()) {
        *out = v.to_string();
    }
}

fn merge_i64(view: &serde_json::Value, key: &str, out: &mut i64) {
    if let Some(v) = view.get(key).and_then(|v| v.as_i64()) {
        *out = v;
    }
}

fn merge_bool(view: &serde_json::Value, key: &str, out: &mut bool) {
    if let Some(v) = view.get(key).and_then(|v| v.as_bool()) {
        *out = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn test_app(dir: &Path, device_id: &str) -> AppState {
        AppState::new(dir.to_path_buf(), device_id.to_string())
    }

    #[test]
    fn a_row_without_reasoning_does_not_count_as_the_harness_default() {
        let mut state = test_app(Path::new("/tmp"), "dev");
        state.update_chat_from_view("c1", &serde_json::json!({ "model": "m", "harness": "pi" }));
        let chat = state.chats.get("c1").unwrap();
        assert!(!chat.reasoning_known);
        assert!(chat.reasoning.is_empty());

        state.update_chat_from_view("c1", &serde_json::json!({ "reasoning": "high" }));
        let chat = state.chats.get("c1").unwrap();
        assert!(chat.reasoning_known);
        assert_eq!(chat.reasoning, "high");

        // Empty is a real choice: the harness default, picked on purpose.
        state.update_chat_from_view("c1", &serde_json::json!({ "reasoning": "" }));
        assert!(state.chats.get("c1").unwrap().reasoning_known);
        assert!(state.chats.get("c1").unwrap().reasoning.is_empty());
    }

    #[test]
    fn attention_sort_called_and_stale_running_is_unknown() {
        let temp = std::env::temp_dir().join(format!("keel-ui-test-{}", uuid::Uuid::new_v4()));
        let mut app = test_app(&temp, "dev1");

        let now = 100_000;
        // c1: awaiting_input
        app.chats.insert(
            "c1".into(),
            ChatItem::test("c1", "s1", "Chat 1", ChatStatus::AwaitingInput, 10, now),
        );
        // c2: fresh running (seen now)
        app.chats.insert(
            "c2".into(),
            ChatItem::test("c2", "s1", "Chat 2", ChatStatus::Running, 20, now),
        );
        // c3: stale running (seen > 45000 ms ago) -> unknown
        app.chats.insert(
            "c3".into(),
            ChatItem::test("c3", "s1", "Chat 3", ChatStatus::Running, 30, now - 45_001),
        );
        // c4: idle
        app.chats.insert(
            "c4".into(),
            ChatItem::test("c4", "s1", "Chat 4", ChatStatus::Idle, 50, now),
        );

        let visible = app.visible_chats(now);
        assert_eq!(visible.len(), 4);
        // Band 0: c1 (awaiting_input)
        assert_eq!(visible[0].0.id, "c1");
        assert_eq!(visible[0].1, DisplayStatus::AwaitingInput);

        // Band 1: c2 (running)
        assert_eq!(visible[1].0.id, "c2");
        assert_eq!(visible[1].1, DisplayStatus::Running);

        // Band 2: idle & unknown by last_message_at descending: c4 (at 50), then c3 (at 30)
        assert_eq!(visible[2].0.id, "c4");
        assert_eq!(visible[2].1, DisplayStatus::Idle);
        assert_eq!(visible[3].0.id, "c3");
        assert_eq!(visible[3].1, DisplayStatus::Unknown);
    }

    #[test]
    fn space_filter_and_archived_filtering() {
        let temp = std::env::temp_dir().join(format!("keel-ui-test-{}", uuid::Uuid::new_v4()));
        let mut app = test_app(&temp, "dev1");
        let now = 100_000;

        app.chats.insert(
            "c1".into(),
            ChatItem::test("c1", "s1", "Chat 1", ChatStatus::Idle, 10, now),
        );
        app.chats.insert(
            "c2".into(),
            ChatItem::test("c2", "s2", "Chat 2", ChatStatus::Idle, 20, now),
        );
        let mut c3 = ChatItem::test("c3", "s1", "Chat 3", ChatStatus::Idle, 30, now);
        c3.archived = true;
        app.chats.insert("c3".into(), c3);

        // All spaces
        assert_eq!(app.space_filter, None);
        let visible = app.visible_chats(now);
        assert_eq!(visible.len(), 2);
        let ids: Vec<&str> = visible.iter().map(|(c, _)| c.id.as_str()).collect();
        assert_eq!(ids, vec!["c2", "c1"]); // Recency 20 > 10

        // Filter to s1
        app.space_filter = Some("s1".into());
        let visible_s1 = app.visible_chats(now);
        assert_eq!(visible_s1.len(), 1);
        assert_eq!(visible_s1[0].0.id, "c1");
    }

    #[test]
    fn tabs_open_focus_and_closing_does_not_archive() {
        let temp = std::env::temp_dir().join(format!("keel-ui-test-{}", uuid::Uuid::new_v4()));
        let mut app = test_app(&temp, "dev1");

        app.chats.insert(
            "c1".into(),
            ChatItem::test("c1", "s1", "Chat 1", ChatStatus::Idle, 10, 10),
        );

        // Open tab
        app.open_or_focus_tab("c1");
        assert_eq!(app.ui_settings.open_tabs, vec!["c1"]);
        assert_eq!(app.active_tab.as_deref(), Some("c1"));

        // Close tab
        app.close_tab("c1");
        assert!(app.ui_settings.open_tabs.is_empty());
        assert_eq!(app.active_tab, None);
        // Closing tab does NOT archive
        assert!(!app.chats["c1"].archived);

        let settings = UiSettings::read_from(&temp.join("ui-settings.json")).unwrap();
        assert!(settings.open_tabs.is_empty());
    }

    #[test]
    fn launch_starts_on_the_new_session_screen_with_saved_tabs_kept() {
        let temp = std::env::temp_dir().join(format!("keel-ui-test-{}", uuid::Uuid::new_v4()));
        let mut app = test_app(&temp, "dev1");
        app.open_or_focus_tab("c1");
        app.open_or_focus_tab("c2");
        drop(app);

        let mut app = AppState::new(temp, "dev1".into());
        assert_eq!(app.ui_settings.open_tabs, vec!["c1", "c2"]);
        assert_eq!(app.active_tab, None);
        assert_eq!(app.tab_at(1).as_deref(), Some("c1"));
        // Closing a tab with nothing selected leaves the hero up.
        app.close_tab("c1");
        assert_eq!(app.ui_settings.open_tabs, vec!["c2"]);
        assert_eq!(app.active_tab, None);
    }

    #[test]
    fn deleting_space_drops_chats_from_open_tabs() {
        let temp = std::env::temp_dir().join(format!("keel-ui-test-{}", uuid::Uuid::new_v4()));
        let mut app = test_app(&temp, "dev1");

        app.spaces
            .insert("s1".into(), SpaceItem::test("s1", "Space 1", "/p1", "dev1"));
        app.chats.insert(
            "c1".into(),
            ChatItem::test("c1", "s1", "Chat 1", ChatStatus::Idle, 10, 10),
        );
        app.chats.insert(
            "c2".into(),
            ChatItem::test("c2", "s2", "Chat 2", ChatStatus::Idle, 20, 20),
        );

        app.open_or_focus_tab("c1");
        app.open_or_focus_tab("c2");
        assert_eq!(app.ui_settings.open_tabs, vec!["c1", "c2"]);

        // Delete space s1
        app.on_space_deleted("s1");
        assert_eq!(app.ui_settings.open_tabs, vec!["c2"]);
        assert_eq!(app.active_tab.as_deref(), Some("c2"));
        assert!(app.chats["c1"].tombstoned);
    }

    #[test]
    fn new_session_control_space_derivation() {
        let temp = std::env::temp_dir().join(format!("keel-ui-test-{}", uuid::Uuid::new_v4()));
        let mut app = test_app(&temp, "dev1");

        // No spaces exist
        assert_eq!(app.target_space_for_new_session(), None);

        // Only another device's space: no fallback to it.
        app.spaces.insert(
            "s1".into(),
            SpaceItem::test("s1", "Other Space", "/p1", "dev_other"),
        );
        assert_eq!(app.target_space_for_new_session(), None);

        // Newest by created_at wins, not map order ("s9" sorts last but is older).
        let mut newest = SpaceItem::test("s2", "My Newest Space", "/p2", "dev1");
        newest.created_at = 200;
        app.spaces.insert("s2".into(), newest);
        let mut older = SpaceItem::test("s9", "My Old Space", "/p9", "dev1");
        older.created_at = 100;
        app.spaces.insert("s9".into(), older);

        assert_eq!(app.space_filter, None);
        assert_eq!(app.target_space_for_new_session().unwrap().id, "s2");

        // A space this UI just created becomes the target.
        app.record_created_space("s0", "/tmp/proj");
        assert_eq!(app.target_space_for_new_session().unwrap().id, "s0");

        // When space filter is s1: returns s1
        app.space_filter = Some("s1".into());
        let target_s1 = app.target_space_for_new_session().unwrap();
        assert_eq!(target_s1.id, "s1");
    }

    #[test]
    fn partial_chat_view_merges_instead_of_wiping() {
        let temp = std::env::temp_dir().join(format!("keel-ui-test-{}", uuid::Uuid::new_v4()));
        let mut app = test_app(&temp, "dev1");
        app.update_chat_from_view(
            "c1",
            &serde_json::json!({"space_id": "s1", "title": "T", "archived": false, "status": "idle"}),
        );
        app.update_chat_from_view(
            "c1",
            &serde_json::json!({"status": "running", "title": "T2"}),
        );
        let c = &app.chats["c1"];
        assert_eq!(c.space_id, "s1");
        assert_eq!(c.title, "T2");
        assert_eq!(c.status, ChatStatus::Running);

        app.open_or_focus_tab("c1");
        app.update_chat_from_view("c1", &serde_json::json!({"tombstoned": true}));
        assert!(app.ui_settings.open_tabs.is_empty());
        assert_eq!(app.active_tab, None);
    }

    #[test]
    fn drafts_round_trip_per_chat() {
        let temp = std::env::temp_dir().join(format!("keel-ui-test-{}", uuid::Uuid::new_v4()));
        let mut app = test_app(&temp, "dev1");
        app.set_draft("c1", "hello");
        app.set_draft("c2", "world");
        app.set_draft("c2", "");
        let loaded = UiSettings::read_from(&temp.join("ui-settings.json")).unwrap();
        assert_eq!(loaded.drafts.get("c1").map(String::as_str), Some("hello"));
        assert!(!loaded.drafts.contains_key("c2"));
        assert_eq!(app.draft("c1"), "hello");
    }

    #[test]
    fn theme_and_reduced_motion_settings_round_trip() {
        let temp = std::env::temp_dir().join(format!("keel-ui-test-{}", uuid::Uuid::new_v4()));
        let mut app = test_app(&temp, "dev1");

        app.set_theme(Some(ThemeChoice::Dark));
        app.set_reduced_motion(Some(true));

        let loaded = UiSettings::read_from(&temp.join("ui-settings.json")).unwrap();
        assert_eq!(loaded.theme, Some(ThemeChoice::Dark));
        assert_eq!(loaded.reduced_motion, Some(true));

        assert_eq!(app.effective_tokens(false), Tokens::dark());
        assert!(app.effective_reduced_motion(false));
    }

    #[test]
    fn sidebar_sections_sort_by_name_fold_and_cap() {
        let temp = std::env::temp_dir().join(format!("keel-ui-test-{}", uuid::Uuid::new_v4()));
        let mut app = test_app(&temp, "dev1");
        let now = 100_000;
        app.spaces
            .insert("z".into(), SpaceItem::test("z", "zeta", "/z", "dev1"));
        app.spaces
            .insert("a".into(), SpaceItem::test("a", "Alpha", "/a", "dev1"));
        app.spaces
            .insert("e".into(), SpaceItem::test("e", "empty", "/e", "dev1"));
        for i in 0..7 {
            let id = format!("a{i}");
            app.chats.insert(
                id.clone(),
                ChatItem::test(&id, "a", &id, ChatStatus::Idle, 10 + i, now),
            );
        }
        app.chats.insert(
            "z0".into(),
            ChatItem::test("z0", "z", "z0", ChatStatus::AwaitingInput, 1, now),
        );
        app.chats.insert(
            "z1".into(),
            ChatItem::test("z1", "z", "z1", ChatStatus::Running, 2, now),
        );
        app.chats.insert(
            "z2".into(),
            ChatItem::test("z2", "z", "z2", ChatStatus::Idle, 3, now),
        );

        let names = |app: &AppState| {
            app.sidebar_sections(now)
                .into_iter()
                .map(|s| s.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&app), ["Alpha", "empty", "zeta"]);
        // Keel's "No project" folder sits apart, after every real folder.
        app.spaces.insert(
            "n".into(),
            SpaceItem::test("n", "Aaa", "/data/No project", "dev1"),
        );
        assert_eq!(names(&app), ["Alpha", "empty", "zeta", "Aaa"]);
        assert!(app.sidebar_sections(now)[3].loose);
        app.spaces.remove("n");

        // Open: capped at five, newest first; the rest wait behind "Show more".
        let alpha = &app.sidebar_sections(now)[0];
        assert_eq!(
            (alpha.rows.len(), alpha.hidden, alpha.total),
            (SPACE_ROW_CAP, 2, 7)
        );
        assert_eq!(alpha.rows[0].0.id, "a6");
        // ...unless the active chat is further down: it shows anyway.
        app.active_tab = Some("a0".into());
        let alpha = &app.sidebar_sections(now)[0];
        assert_eq!((alpha.rows.len(), alpha.hidden), (6, 1));
        app.space_show_all.insert("a".into());
        assert_eq!(app.sidebar_sections(now)[0].hidden, 0);

        // Folded: only what waits on you (and the active chat) shows through.
        app.toggle_space_collapsed("z");
        let zeta = &app.sidebar_sections(now)[2];
        assert!(zeta.collapsed);
        assert_eq!(zeta.urgent, DisplayStatus::AwaitingInput);
        assert_eq!(
            zeta.rows
                .iter()
                .map(|(c, _)| c.id.as_str())
                .collect::<Vec<_>>(),
            ["z0"]
        );
        assert_eq!((zeta.total, zeta.hidden), (3, 0));
        // Stepping skips folded rows.
        app.active_tab = Some("z0".into());
        assert_eq!(app.step_chat(now, 1).as_deref(), Some("a6"));
        app.toggle_space_collapsed("z");
        assert!(!app.sidebar_sections(now)[2].collapsed);

        // Focused: one section, unfolded and uncapped.
        app.toggle_space_collapsed("a");
        app.space_show_all.clear();
        app.space_filter = Some("a".into());
        let only = app.sidebar_sections(now);
        assert_eq!(only.len(), 1);
        assert_eq!((only[0].collapsed, only[0].rows.len()), (false, 7));
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn sidebar_sections_skip_chats_that_belong_to_a_project() {
        let temp = std::env::temp_dir().join(format!("keel-ui-skip-{}", uuid::Uuid::new_v4()));
        let mut app = test_app(&temp, "dev1");
        let now = 100_000;
        app.spaces
            .insert("a".into(), SpaceItem::test("a", "Alpha", "/a", "dev1"));
        app.spaces.insert(
            "n".into(),
            SpaceItem::test("n", "Loose", "/data/No project", "dev1"),
        );
        app.chats.insert(
            "folder".into(),
            ChatItem::test("folder", "a", "Folder", ChatStatus::Idle, 2, now),
        );
        app.chats.insert(
            "project".into(),
            ChatItem::test("project", "a", "Project", ChatStatus::Idle, 3, now),
        );
        app.chats.insert(
            "loose".into(),
            ChatItem::test("loose", "n", "Loose chat", ChatStatus::Idle, 1, now),
        );
        let skip = HashSet::from(["project".to_string()]);
        let sections = app.sidebar_sections_skipping(now, &skip);
        let ids = |name: &str| {
            sections
                .iter()
                .find(|s| s.name == name)
                .map(|s| {
                    s.rows
                        .iter()
                        .map(|(c, _)| c.id.as_str())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        assert_eq!(ids("Alpha"), ["folder"]);
        assert_eq!(ids("Loose"), ["loose"]);
        assert_eq!(
            sections.iter().find(|s| s.name == "Alpha").map(|s| s.total),
            Some(1)
        );
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn step_chat_wraps_in_sidebar_order_and_tab_at_maps_nine_to_last() {
        let dir = std::env::temp_dir().join(format!("keel-ui-step-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = AppState::new(dir.clone(), "dev".into());
        app.spaces
            .insert("s".into(), SpaceItem::test("s", "Space", "/tmp", "dev"));
        for (id, at) in [("a", 3), ("b", 2), ("c", 1)] {
            app.chats.insert(
                id.into(),
                ChatItem::test(id, "s", id, ChatStatus::Idle, at, at),
            );
        }
        let order: Vec<String> = app
            .visible_chats(10)
            .into_iter()
            .map(|(c, _)| c.id)
            .collect();
        assert_eq!(app.step_chat(10, 1).as_deref(), Some(order[0].as_str()));
        assert_eq!(app.step_chat(10, -1).as_deref(), Some(order[2].as_str()));
        app.active_tab = Some(order[2].clone());
        assert_eq!(app.step_chat(10, 1).as_deref(), Some(order[0].as_str()));
        assert_eq!(app.step_chat(10, -1).as_deref(), Some(order[1].as_str()));

        app.ui_settings.open_tabs = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(app.tab_at(1).as_deref(), Some("a"));
        assert_eq!(app.tab_at(3).as_deref(), Some("c"));
        assert_eq!(app.tab_at(4), None);
        assert_eq!(app.tab_at(9).as_deref(), Some("c"));
        assert_eq!(app.tab_at(0), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidebar_width_clamps_and_collapse_persists() {
        let dir = std::env::temp_dir().join(format!("keel-ui-sidebar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = AppState::new(dir.clone(), "dev".into());
        assert_eq!(app.sidebar_width(), SIDEBAR_DEFAULT);
        app.set_sidebar_width(10.);
        assert_eq!(app.sidebar_width(), SIDEBAR_MIN);
        app.set_sidebar_width(9_000.);
        assert_eq!(app.sidebar_width(), SIDEBAR_MAX);
        app.set_sidebar_width(f32::NAN);
        assert_eq!(app.sidebar_width(), SIDEBAR_DEFAULT);
        app.set_sidebar_width(300.);
        app.save_settings();
        app.toggle_sidebar();
        let reopened = AppState::new(dir.clone(), "dev".into());
        assert_eq!(reopened.sidebar_width(), 300.);
        assert!(reopened.ui_settings.sidebar_collapsed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetched_current_branch_is_selected_until_the_user_picks() {
        let mut app = AppState::new(std::env::temp_dir().join("keel-branch-state"), "dev".into());
        let fetch = |app: &mut AppState, current: Option<&str>, detached: Option<&str>| {
            let branches = vec!["a".to_string(), "main".to_string()];
            app.apply_branches(
                "/repo",
                branches,
                current.map(String::from),
                detached.map(String::from),
            );
        };
        // A listing for a folder that isn't (or is no longer) the target is dropped.
        fetch(&mut app, Some("main"), None);
        assert_eq!(app.selected_branch, None);
        app.branch_folder = Some("/repo".into());
        fetch(&mut app, Some("main"), None);
        assert_eq!(app.selected_branch.as_deref(), Some("main"));
        assert_eq!(
            app.available_branches,
            vec!["main".to_string(), "a".to_string()]
        );
        // A manual pick survives a late fetch.
        app.set_branch(Some("a".into()));
        fetch(&mut app, Some("main"), None);
        assert_eq!(app.selected_branch.as_deref(), Some("a"));
        // A folder change forgets it; a detached HEAD selects nothing.
        app.reset_branch_for_folder();
        app.branch_folder = Some("/repo".into());
        fetch(&mut app, None, Some("abc123"));
        assert_eq!(app.selected_branch, None);
        assert_eq!(app.detached_head.as_deref(), Some("abc123"));
    }

    #[test]
    fn folder_change_then_fetch_is_one_label_transition() {
        use crate::motion::RollingLabel;
        let mut app = AppState::new(std::env::temp_dir().join("keel-branch-roll"), "dev".into());
        let t0 = std::time::Instant::now();
        let mut shown = RollingLabel::default();
        let render = |app: &AppState, shown: &mut RollingLabel, ms: u64| {
            let label = app.branch_chip_label().unwrap_or_else(|| shown.now.clone());
            shown.commit(&label, t0 + std::time::Duration::from_millis(ms), true)
        };
        app.branch_folder = Some("/a".into());
        app.apply_branches("/a", vec!["main".into()], Some("main".into()), None);
        assert!(!render(&app, &mut shown, 0), "first label just appears");
        assert_eq!(shown.now, "main");

        // Folder change: the old label holds (every render) while the fetch runs.
        app.reset_branch_for_folder();
        app.branch_folder = Some("/b".into());
        assert_eq!(app.branch_chip_label(), None);
        for ms in [1000, 1016, 1033] {
            assert!(!render(&app, &mut shown, ms));
            assert_eq!(shown.now, "main");
        }
        // The fetch commits: exactly one roll, straight to the new branch.
        app.apply_branches("/b", vec!["dev".into()], Some("dev".into()), None);
        assert!(render(&app, &mut shown, 1100));
        for ms in [1116, 1133, 1500] {
            assert!(!render(&app, &mut shown, ms), "re-renders don't roll again");
        }
        assert_eq!(
            (shown.prev.as_str(), shown.now.as_str(), shown.roll),
            ("main", "dev", 1)
        );

        // Not a repo: the placeholder, once.
        app.reset_branch_for_folder();
        app.branch_folder = Some("/c".into());
        assert!(!render(&app, &mut shown, 2000));
        app.apply_branches("/c", Vec::new(), None, None);
        assert!(render(&app, &mut shown, 2100));
        assert_eq!(shown.now, "Branch");
        assert_eq!(shown.roll, 2);
    }

    #[test]
    fn scratch_helpers_and_merged_loose_sections() {
        let temp = std::env::temp_dir().join(format!("keel-ui-scratch-{}", uuid::Uuid::new_v4()));
        let mut app = test_app(&temp, "dev1");
        let now = 100_000;
        let scratch_path = temp.join("scratch").to_string_lossy().into_owned();
        let legacy_path = temp.join("No project").to_string_lossy().into_owned();
        app.spaces.insert(
            "real".into(),
            SpaceItem::test("real", "App", "/work/app", "dev1"),
        );
        app.spaces.insert(
            "scratch-user".into(),
            SpaceItem::test("scratch-user", "scratch", "/home/me/scratch", "dev1"),
        );
        app.spaces.insert(
            "s-new".into(),
            SpaceItem::test("s-new", "No project", &scratch_path, "dev1"),
        );
        app.spaces.insert(
            "s-old".into(),
            SpaceItem::test("s-old", "No project", &legacy_path, "dev1"),
        );
        app.spaces.insert(
            "s-remote".into(),
            SpaceItem::test("s-remote", "No project", "/other/host/scratch", "dev2"),
        );

        assert!(!app.is_scratch_space(&app.spaces["real"]));
        assert!(!app.is_scratch_space(&app.spaces["scratch-user"]));
        assert!(app.is_scratch_space(&app.spaces["s-new"]));
        assert!(app.is_scratch_space(&app.spaces["s-old"]));
        assert!(app.is_scratch_space(&app.spaces["s-remote"]));

        let mut c_new = ChatItem::test("c-new", "s-new", "New", ChatStatus::Idle, 20, now);
        c_new.cwd = temp.join("scratch/c-new").to_string_lossy().into_owned();
        let c_old = ChatItem::test("c-old", "s-old", "Old", ChatStatus::Idle, 10, now);
        let c_real = ChatItem::test("c-real", "real", "Real", ChatStatus::Idle, 15, now);
        assert!(app.is_scratch_chat(&c_new));
        assert!(app.is_scratch_chat(&c_old));
        assert!(!app.is_scratch_chat(&c_real));
        app.chats.insert("c-new".into(), c_new);
        app.chats.insert("c-old".into(), c_old);
        app.chats.insert("c-real".into(), c_real);

        let sections = app.sidebar_sections_skipping(now, &HashSet::new());
        let loose: Vec<_> = sections.iter().filter(|s| s.loose).collect();
        assert_eq!(loose.len(), 1, "legacy and scratch loose sections must merge");
        assert!(sections.last().unwrap().loose);
        let loose_ids: Vec<_> = loose[0].rows.iter().map(|(c, _)| c.id.as_str()).collect();
        assert_eq!(loose_ids, ["c-new", "c-old"]);

        // Scratch spaces are excluded from the default fallback, and explicit
        // `new_chat_no_project` overrides fallback.
        app.spaces.get_mut("s-new").unwrap().created_at = 9_999;
        app.spaces.get_mut("real").unwrap().created_at = 1_000;
        assert_eq!(app.target_space_for_new_session().unwrap().id, "real");
        app.new_chat_no_project = true;
        assert!(app.target_space_for_new_session().is_none());
        let _ = std::fs::remove_dir_all(&temp);
    }
}

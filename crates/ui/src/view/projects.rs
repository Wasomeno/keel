//! A project is a name over an ordered list of folders. The first folder is
//! where new chats start; the others are named roots (`@api/…`). The list
//! lives in `{data_dir}/projects.json`. This view is both designs at once:
//! a Projects section and a create sheet in the folder picker, sidebar rows
//! that unfold, a home page, and a composer scope chip.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use gpui::{AnyElement, ClickEvent, Context, ElementId, Entity, IntoElement, Window, anchored, deferred, div, point, prelude::*, px};
use gpui_base::input::InputState;
use keel_proto::projects::{self, Project, ProjectFolder};
use keel_theme::Tokens;

use super::composer_view::Picker;
use super::files_panel::fold;
use super::session_summary::SUMMARY_HOVER_DELAY;
use super::sidebar::{filled_button, text_button};
use super::{KeelWindow, floating_menu_at, menu_item, menu_label, menu_separator, status_dot};
use crate::icons::Icon;
use crate::motion::{ease_out, ease_out_strong};
use crate::state::AppState;
use crate::style::{KeelStyled, MONO, RADIUS_LG, RADIUS_MD, RADIUS_SM, c, zpx};

const SHEET_GROW: f32 = 200.;
const ROW_STAGGER: f32 = 40.;
const ROW_IN: f32 = 180.;
const HOME_ROW_H: f32 = 54.;
/// Chats listed under an unfolded project. The rest stay on the project home.
const SIDE_CHAT_CAP: usize = 3;
const FOLDER_CARD_W: f32 = 300.;
const FOLDER_CARD_LEAVE: Duration = Duration::from_millis(160);

pub(super) struct Projects {
    data_dir: std::path::PathBuf,
    pub(super) store: projects::Projects,
    /// Project whose home page is up.
    home: Option<String>,
    /// Sidebar rows currently unfolded.
    pub(super) unfolded: HashSet<String>,
    home_folded: bool,
    home_fold_gen: u64,
    scope_open: bool,
    sheet: Option<Sheet>,
    sheet_error: Option<String>,
    status: HashMap<String, FolderStatus>,
    status_wait: HashSet<String>,
    /// Project chosen from the folder picker, before a chat exists.
    picked: Option<String>,
    /// Scope for a project that has no chat yet. Missing means every folder.
    draft_scope: HashMap<String, Vec<String>>,
    /// Dev commands and other keyboard actions skip motion.
    pub(super) instant: bool,
    /// Hover card for a project's folders. `left` starts when the pointer
    /// leaves both the header and the card, so it can cross the gap.
    pub(super) folder_card: Option<FolderCard>,
    /// Suppress hover card after click until pointer leaves.
    pub(super) folder_card_suppressed: Option<String>,
}

pub(super) struct FolderCard {
    id: String,
    y: f32,
    since: std::time::Instant,
    over_header: bool,
    over_card: bool,
    left: Option<std::time::Instant>,
}

struct Sheet {
    name: Entity<InputState>,
    rows: Vec<SheetRow>,
    generation: u64,
}

struct SheetRow {
    path: String,
    alias: Entity<InputState>,
    letter: Entity<InputState>,
}

#[derive(Clone, Debug)]
struct FolderStatus {
    branch: String,
    changes: Option<u32>,
    /// A few changed paths, in git's order. The count is `changes`.
    files: Vec<String>,
    file_details: Vec<crate::view::session_card::GitFileChange>,
    total_additions: u64,
    total_deletions: u64,
    repo: bool,
    exists: bool,
}

impl Projects {
    pub(super) fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(projects::FILE);
        let store = std::fs::read_to_string(&path).map(|text| projects::Projects::parse(&text)).unwrap_or_default();
        Self {
            data_dir: data_dir.to_path_buf(),
            store,
            home: None,
            unfolded: HashSet::new(),
            home_folded: false,
            home_fold_gen: 0,
            scope_open: false,
            sheet: None,
            sheet_error: None,
            status: HashMap::new(),
            status_wait: HashSet::new(),
            picked: None,
            draft_scope: HashMap::new(),
            instant: false,
            folder_card: None,
            folder_card_suppressed: None,
        }
    }

    fn save(&self) {
        let path = self.data_dir.join(projects::FILE);
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, self.store.to_json()).and_then(|()| std::fs::rename(&tmp, &path)).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    pub(super) fn home_open(&self) -> bool {
        self.home.is_some()
    }

    pub(super) fn sheet_open(&self) -> bool {
        self.sheet.is_some()
    }

    pub(super) fn close_home(&mut self) {
        self.home = None;
    }

    /// True when a scope menu was open.
    pub(super) fn close_scope(&mut self) -> bool {
        std::mem::take(&mut self.scope_open)
    }

    /// True when the create sheet was open.
    pub(super) fn close_sheet(&mut self) -> bool {
        self.sheet_error = None;
        self.sheet.take().is_some()
    }

    fn find(&self, name: &str) -> Option<&Project> {
        self.store.by_name(name)
    }

    fn project_id_for_path(&self, path: &str) -> Option<String> {
        let path = norm_path(path);
        self.store.projects.iter().find(|p| p.folders.iter().any(|f| paths_eq(&f.path, &path))).map(|p| p.id.clone())
    }

    /// The project a new chat in `cwd` should join, and the scope to store.
    pub(super) fn link_plan(&self, cwd: &str) -> Option<(String, Vec<String>)> {
        let id = self.project_id_for_path(cwd)?;
        let scope = self.draft_scope.get(&id).cloned().unwrap_or_default();
        Some((id, scope))
    }

    pub(super) fn record_chat(&mut self, chat: &str, project: &str, scope: Vec<String>) {
        self.store.chats.insert(chat.to_string(), projects::ChatScope { project: project.to_string(), scope });
        self.draft_scope.remove(project);
        self.save();
    }

    /// Folder chip text when `path` is one of a project's folders.
    pub(super) fn folder_chip(&self, path: Option<&str>, fallback: String) -> String {
        let Some(path) = path else { return fallback };
        let Some(id) = self.project_id_for_path(path) else { return fallback };
        let Some(project) = self.store.project(&id) else { return fallback };
        let Some(folder) = project.folders.iter().find(|f| paths_eq(&f.path, path)) else { return fallback };
        if project.folders.first().is_some_and(|f| f.alias == folder.alias) {
            project.name.clone()
        } else {
            format!("{} · {}", project.name, folder.alias)
        }
    }

    /// Alias and the folder's own name, one per folder, for the files tree.
    pub(super) fn file_roots(&self, chat: Option<&str>) -> Vec<(String, String)> {
        let Some(id) = chat.and_then(|c| self.store.chats.get(c)).map(|l| l.project.clone()) else {
            return Vec::new();
        };
        let Some(project) = self.store.project(&id) else { return Vec::new() };
        project.folders.iter().map(|f| (f.alias.clone(), base_name(&f.path))).collect()
    }

    /// An absolute path inside a project folder, as `@alias/rest`.
    pub(super) fn map_path(&self, chat: Option<&str>, path: &str) -> String {
        if path.is_empty() || path.starts_with('@') {
            return path.to_string();
        }
        let Some(id) = chat.and_then(|c| self.store.chats.get(c)).map(|l| l.project.clone()) else {
            return path.to_string();
        };
        map_alias_path(self.store.project(&id), path).unwrap_or_else(|| path.to_string())
    }

    fn scope_project_id(&self, chat: Option<&str>, cwd: Option<&str>) -> Option<String> {
        if let Some(chat) = chat
            && let Some(link) = self.store.chats.get(chat)
            && self.store.project(&link.project).is_some()
        {
            return Some(link.project.clone());
        }
        if let Some(id) = &self.home
            && self.store.project(id).is_some()
        {
            return Some(id.clone());
        }
        if let Some(id) = cwd.and_then(|c| self.project_id_for_path(c)) {
            return Some(id);
        }
        self.picked.clone().filter(|id| self.store.project(id).is_some())
    }

    /// Absolute folder paths `@` should index, when this chat or the next
    /// folder belongs to a project.
    pub(super) fn mention_dirs(&self, chat: Option<&str>, cwd: Option<&str>) -> Option<Vec<String>> {
        let id = self.scope_project_id(chat, cwd)?;
        let project = self.store.project(&id)?;
        Some(project.folders.iter().map(|f| norm_path(&f.path)).collect())
    }

    pub(super) fn mention_label(&self, chat: Option<&str>, cwd: Option<&str>) -> Option<String> {
        let id = self.scope_project_id(chat, cwd)?;
        self.store.project(&id).map(|p| format!("Files across {}", p.name))
    }

    /// `@` paths across every folder (`api/src/x.ts`), or `None` outside a project.
    pub(super) fn mention_paths(&self, chat: Option<&str>, cwd: Option<&str>, cached: &HashMap<String, Vec<String>>) -> Option<Vec<String>> {
        let id = self.scope_project_id(chat, cwd)?;
        let project = self.store.project(&id)?;
        let mut out = Vec::new();
        for folder in &project.folders {
            let key = norm_path(&folder.path);
            let Some(list) = cached.get(&key).or_else(|| cached.get(&folder.path)) else { continue };
            for path in list {
                out.push(format!("{}/{}", folder.alias, path));
            }
        }
        Some(out)
    }

    pub(super) fn mention_loading(&self, chat: Option<&str>, cwd: Option<&str>, cached: &HashMap<String, Vec<String>>, loading: &HashSet<String>) -> bool {
        let Some(id) = self.scope_project_id(chat, cwd) else { return false };
        let Some(project) = self.store.project(&id) else { return false };
        project.folders.iter().any(|f| {
            let key = norm_path(&f.path);
            let ready = cached.contains_key(&key) || cached.contains_key(&f.path);
            !ready || loading.contains(&key) || loading.contains(&f.path)
        })
    }

    fn scope_of(&self, id: &str, chat: Option<&str>) -> Vec<String> {
        if let Some(chat) = chat
            && let Some(link) = self.store.chats.get(chat).filter(|l| l.project == id)
        {
            return link.scope.clone();
        }
        self.draft_scope.get(id).cloned().unwrap_or_default()
    }

    fn scope_aliases(&self, id: &str, chat: Option<&str>) -> Vec<String> {
        let Some(project) = self.store.project(id) else { return Vec::new() };
        projects::in_scope(project, &self.scope_of(id, chat)).into_iter().map(|f| f.alias.clone()).collect()
    }

    fn write_scope(&mut self, id: &str, chat: Option<&str>, scope: Vec<String>) {
        if let Some(chat) = chat
            && let Some(link) = self.store.chats.get_mut(chat)
            && link.project == id
        {
            link.scope = scope;
            self.save();
            return;
        }
        self.draft_scope.insert(id.to_string(), scope);
    }

    fn toggle_scope_alias(&mut self, id: &str, chat: Option<&str>, alias: &str) {
        let Some(project) = self.store.project(id).cloned() else { return };
        if project.folders.first().is_some_and(|f| f.alias == alias) {
            return;
        }
        let mut scope = self.scope_of(id, chat);
        if scope.is_empty() {
            scope = project.folders.iter().skip(1).map(|f| f.alias.clone()).filter(|a| a != alias).collect();
        } else if let Some(ix) = scope.iter().position(|a| a == alias) {
            scope.remove(ix);
        } else {
            scope.push(alias.to_string());
        }
        let rest: Vec<_> = project.folders.iter().skip(1).map(|f| f.alias.clone()).collect();
        if !rest.is_empty() && rest.iter().all(|a| scope.iter().any(|s| s == a)) {
            scope.clear();
        }
        self.write_scope(id, chat, scope);
    }

    fn add_folder(&mut self, id: &str, path: String) {
        let path = norm_path(&expand_user_path(&path));
        if !Path::new(&path).is_dir() {
            return;
        }
        let Some(project) = self.store.projects.iter_mut().find(|p| p.id == id) else { return };
        if project.folders.iter().any(|f| paths_eq(&f.path, &path)) {
            return;
        }
        let taken: Vec<String> = project.folders.iter().map(|f| f.alias.clone()).collect();
        let alias = projects::default_alias(&path, &taken);
        project.folders.push(ProjectFolder { path, alias, letter: String::new(), note: String::new() });
        self.save();
    }

    pub(super) fn state_json(&self) -> serde_json::Value {
        let projects: Vec<_> = self
            .store
            .projects
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "unfolded": self.unfolded.contains(&p.id),
                    "folders": p.folders.iter().map(|f| serde_json::json!({
                        "alias": f.alias,
                        "path": f.path,
                        "letter": f.letter,
                        "note": f.note,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::json!({
            "count": self.store.projects.len(),
            "chats": self.store.chats.len(),
            "home": self.home.as_ref().and_then(|id| self.store.project(id)).map(|p| p.name.clone()),
            "sheet": self.sheet.is_some(),
            "scope_open": self.scope_open,
            "picked": self.picked.as_ref().and_then(|id| self.store.project(id)).map(|p| p.name.clone()),
            "projects": projects,
        })
    }

    pub(super) fn chat_project_id(&self, chat: &str) -> Option<String> {
        let id = self.store.chats.get(chat)?.project.clone();
        self.store.project(&id).map(|_| id)
    }

    /// Branch label for a folder path, or "…" while its status is still loading.
    pub(super) fn folder_branch_label(&self, path: &str) -> String {
        let key = norm_path(path);
        let Some(status) = self.status.get(&key).or_else(|| self.status.get(path)) else {
            return "…".into();
        };
        if !status.branch.is_empty() {
            status.branch.clone()
        } else if !status.repo {
            "not a repo".into()
        } else {
            "detached".into()
        }
    }

    /// File-change label for one folder. Not a line count, and not a sum.
    pub(super) fn folder_changes_label(&self, path: &str) -> String {
        let key = norm_path(path);
        let Some(status) = self.status.get(&key).or_else(|| self.status.get(path)) else {
            return "…".into();
        };
        folder_changes_text(status.changes, status.repo, status.exists)
    }

    /// Project name and one line per folder this chat works in.
    pub(super) fn card_branches(&self, chat: &str) -> Option<(String, Vec<CardBranch>)> {
        let id = self.chat_project_id(chat)?;
        let project = self.store.project(&id)?;
        let scope = self.store.chats.get(chat).map(|l| l.scope.clone()).unwrap_or_default();
        let name = project.name.clone();
        let lines = projects::in_scope(project, &scope)
            .into_iter()
            .map(|folder| {
                let key = norm_path(&folder.path);
                let status = self.status.get(&key).or_else(|| self.status.get(&folder.path));
                let note = match status {
                    Some(s) if !s.exists => "missing".into(),
                    Some(s) if !s.repo => "not a repo".into(),
                    _ => String::new(),
                };
                CardBranch {
                    alias: folder.alias.clone(),
                    branch: status.map(|s| s.branch.clone()).unwrap_or_default(),
                    files: status.map(|s| s.files.clone()).unwrap_or_default(),
                    file_details: status.map(|s| s.file_details.clone()).unwrap_or_default(),
                    total_additions: status.map(|s| s.total_additions).unwrap_or(0),
                    total_deletions: status.map(|s| s.total_deletions).unwrap_or(0),
                    file_count: status.and_then(|s| s.changes),
                    note,
                }
            })
            .collect();
        Some((name, lines))
    }
}

/// One folder on the session card when the chat belongs to a project.
/// Every folder is the same shape: its name, the branch it is on, and the
/// files that changed there. Nothing on it checks a branch out.
#[derive(Clone)]
pub(super) struct CardBranch {
    pub alias: String,
    pub branch: String,
    /// Changed paths, capped by `folder_status`.
    pub files: Vec<String>,
    pub file_details: Vec<crate::view::session_card::GitFileChange>,
    pub total_additions: u64,
    pub total_deletions: u64,
    /// Uncommitted files in this folder. `None` until the status arrives.
    pub file_count: Option<u32>,
    /// "missing" or "not a repo". Empty when the folder is a checkout.
    pub note: String,
}

fn folder_changes_text(changes: Option<u32>, repo: bool, exists: bool) -> String {
    match (changes, repo, exists) {
        (_, _, false) => "missing".into(),
        (_, false, _) => "not a repo".into(),
        (Some(0) | None, true, true) => "clean".into(),
        (Some(n), true, true) => format!("{n} changed"),
    }
}

fn base_name(path: &str) -> String {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_string()
}

fn expand_user_path(path: &str) -> String {
    let path = path.trim();
    if path == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| path.to_string());
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return Path::new(&home).join(rest).to_string_lossy().into_owned();
    }
    path.to_string()
}

fn norm_path(path: &str) -> String {
    expand_user_path(path).trim_end_matches('/').to_string()
}

fn paths_eq(a: &str, b: &str) -> bool {
    norm_path(a) == norm_path(b)
}

/// `name alias=path ...`, with the name allowed to contain spaces.
fn parse_spec(arg: &str) -> Result<(String, Vec<(String, String)>), String> {
    let mut name = Vec::new();
    let mut folders = Vec::new();
    for word in arg.split_whitespace() {
        if let Some((alias, path)) = word.split_once('=') {
            if alias.is_empty() || path.is_empty() {
                return Err(format!("bad folder {word}"));
            }
            folders.push((alias.to_string(), expand_user_path(path)));
        } else if folders.is_empty() {
            name.push(word);
        } else {
            return Err(format!("unexpected {word}"));
        }
    }
    let name = name.join(" ");
    if name.is_empty() {
        return Err("need a project name".into());
    }
    Ok((name, folders))
}

fn map_alias_path(project: Option<&Project>, path: &str) -> Option<String> {
    let project = project?;
    let path = norm_path(path);
    let mut best: Option<(usize, String)> = None;
    for folder in &project.folders {
        let root = norm_path(&folder.path);
        if path == root {
            return Some(format!("@{}", folder.alias));
        }
        let prefix = format!("{root}/");
        if path.starts_with(&prefix) && best.as_ref().is_none_or(|(n, _)| root.len() > *n) {
            best = Some((root.len(), format!("@{}/{}", folder.alias, &path[prefix.len()..])));
        }
    }
    best.map(|(_, mapped)| mapped)
}

fn hue(ix: usize, t: Tokens) -> u32 {
    match ix % 3 {
        0 => t.accent,
        1 => t.success,
        _ => t.warning,
    }
}

fn mark_letter(folder: &ProjectFolder) -> String {
    folder.mark()
}

pub(super) fn mark(letter: impl Into<gpui::SharedString>, ix: usize, t: Tokens) -> gpui::Div {
    div()
        .w(zpx(14.))
        .flex_none()
        .flex()
        .justify_center()
        .t_caption()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(c(hue(ix, t)))
        .child(letter.into())
}

fn faint(text: impl Into<gpui::SharedString>, t: Tokens) -> gpui::Div {
    div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().t_small().text_color(c(t.text_faint)).child(text.into())
}

fn ago(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let secs = ((AppState::now_ms() - ms) / 1000).max(0);
    match secs {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 86_400 * 7 => format!("{}d ago", s / 86_400),
        _ => String::new(),
    }
}

#[derive(Clone, Copy)]
enum Clock {
    Rest,
    Run,
}

fn timed<E: IntoElement + 'static>(el: E, id: String, total: f32, clock: Clock, f: impl Fn(E, f32) -> E + 'static) -> AnyElement {
    use gpui::AnimationExt as _;
    match clock {
        Clock::Rest => f(el, f32::MAX).into_any_element(),
        Clock::Run => el
            .with_animation(ElementId::Name(id.into()), gpui::Animation::new(Duration::from_secs_f32(total / 1000.)), move |el, d| f(el, d * total))
            .into_any_element(),
    }
}

fn step(ms: f32, delay: f32, dur: f32) -> f32 {
    ((ms - delay) / dur).clamp(0., 1.)
}

fn fold_progress(ms: f32, opening: bool, open_ms: f32, close_ms: f32) -> f32 {
    if opening { ease_out_strong(step(ms, 0., open_ms)) } else { 1. - ease_out(step(ms, 0., close_ms)) }
}

fn input_text(input: &Entity<InputState>, cx: &Context<KeelWindow>) -> String {
    input.read(cx).value().trim().to_string()
}

pub(super) fn render_projects_label(t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id("projects-group-label")
        .h(zpx(32.))
        .flex_none()
        .px_2()
        .flex()
        .flex_row()
        .items_center()
        .child(
            div()
                .px_2()
                .t_caption()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(c(t.text_faint))
                .child("Projects"),
        )
}

/// A project collapsible chevron's turn: pointing down (0 rad) when open,
/// right (-π/2 rad) when folded.
pub(super) fn chevron_turn_radians(folded: bool) -> f32 {
    if folded { -std::f32::consts::FRAC_PI_2 } else { 0. }
}

impl KeelWindow {
    fn project_clock(&self) -> Clock {
        if self.reduced() || self.projects.instant { Clock::Rest } else { Clock::Run }
    }

    fn sheet_row(&self, path: String, alias: String, letter: String, window: &mut Window, cx: &mut Context<Self>) -> SheetRow {
        let alias_in = cx.new(|cx| InputState::new(window, cx).placeholder("alias"));
        alias_in.update(cx, |s, cx| s.set_value(alias, window, cx));
        let letter_in = cx.new(|cx| InputState::new(window, cx).placeholder(""));
        if !letter.is_empty() {
            letter_in.update(cx, |s, cx| s.set_value(letter, window, cx));
        }
        SheetRow { path, alias: alias_in, letter: letter_in }
    }

    fn begin_sheet(&mut self, name: &str, folders: Vec<(String, String)>, window: &mut Window, cx: &mut Context<Self>) {
        self.set_picker(Some(Picker::Folder), cx);
        let name_in = cx.new(|cx| InputState::new(window, cx).placeholder("Project name"));
        if !name.is_empty() {
            name_in.update(cx, |s, cx| s.set_value(name.to_string(), window, cx));
        }
        let mut rows = Vec::new();
        for (alias, path) in folders {
            rows.push(self.sheet_row(path, alias, String::new(), window, cx));
        }
        let generation = self.projects.sheet.as_ref().map_or(0, |s| s.generation) + 1;
        self.projects.sheet = Some(Sheet { name: name_in, rows, generation });
        self.projects.sheet_error = None;
    }

    fn push_sheet_folder(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let path = norm_path(&path);
        if !Path::new(&path).is_dir() {
            self.projects.sheet_error = Some("That isn't a folder.".into());
            return;
        }
        let taken = {
            let Some(sheet) = &self.projects.sheet else { return };
            if sheet.rows.iter().any(|r| paths_eq(&r.path, &path)) {
                return;
            }
            sheet.rows.iter().map(|r| input_text(&r.alias, cx)).filter(|s| !s.is_empty()).collect::<Vec<_>>()
        };
        let alias = projects::default_alias(&path, &taken);
        let row = self.sheet_row(path, alias, String::new(), window, cx);
        self.projects.sheet_error = None;
        if let Some(sheet) = &mut self.projects.sheet {
            sheet.rows.push(row);
        }
    }

    fn commit_sheet(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let draft = {
            let Some(sheet) = &self.projects.sheet else { return None };
            let name = input_text(&sheet.name, cx);
            let rows: Vec<(String, String, String)> = sheet
                .rows
                .iter()
                .map(|row| (norm_path(&row.path), input_text(&row.alias, cx), input_text(&row.letter, cx)))
                .collect();
            (name, rows)
        };
        let (name, rows) = draft;
        if name.is_empty() {
            self.projects.sheet_error = Some("Name the project.".into());
            return None;
        }
        let mut folders = Vec::new();
        let mut taken = Vec::new();
        for (path, alias, letter) in rows {
            let letter = letter.chars().next().map(|c| c.to_string()).unwrap_or_default();
            if !projects::valid_alias(&alias) {
                self.projects.sheet_error = Some(format!("\"{alias}\" can't be an alias. Use letters, digits, - and _."));
                return None;
            }
            if taken.iter().any(|t| t == &alias) {
                self.projects.sheet_error = Some(format!("{alias} is used twice."));
                return None;
            }
            taken.push(alias.clone());
            folders.push(ProjectFolder { path, alias, letter, note: String::new() });
        }
        if folders.is_empty() {
            self.projects.sheet_error = Some("Add a folder.".into());
            return None;
        }
        let primary = folders[0].path.clone();
        let id = uuid::Uuid::new_v4().to_string();
        self.projects.store.projects.push(Project {
            id: id.clone(),
            name,
            folders,
            created_at: AppState::now_ms().max(0) as u64,
        });
        self.projects.picked = Some(id);
        self.projects.save();
        self.projects.sheet = None;
        self.projects.sheet_error = None;
        Some(primary)
    }

    pub(super) fn project_chats(&self, id: &str) -> Vec<(String, String, i64)> {
        let mut rows: Vec<_> = self
            .projects
            .store
            .chats_of(id)
            .into_iter()
            .filter_map(|chat| {
                let item = self.state.chats.get(chat)?;
                if item.archived || item.tombstoned {
                    return None;
                }
                Some((item.id.clone(), item.title.clone(), item.last_message_at))
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.2));
        rows
    }

    /// `ui-dev.sock` commands that drive the real project list.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub(super) fn project_dev(&mut self, cmd: &str, arg: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        self.projects.instant = true;
        let result = match cmd {
            "project_create" => self.dev_project_create(arg),
            "project_note" => self.dev_project_note(arg),
            "project_open" => self.dev_project_open(arg, cx),
            "project_home" => self.dev_project_home(arg, cx),
            "project_new" => {
                self.begin_sheet("", Vec::new(), window, cx);
                "ok".into()
            }
            "project_commit" => match self.commit_sheet(cx) {
                Some(path) => {
                    self.set_picker(None, cx);
                    self.use_folder_for_new_chat(path, cx);
                    "ok".into()
                }
                None => self.projects.sheet_error.clone().unwrap_or_else(|| "error: could not create".into()),
            },
            "project_sheet" => {
                if arg.is_empty() || arg == "off" {
                    self.projects.close_sheet();
                    if self.open_picker() == Some(Picker::Folder) {
                        self.set_picker(None, cx);
                    }
                    "ok".into()
                } else {
                    match parse_spec(arg) {
                        Ok((name, folders)) => {
                            self.begin_sheet(&name, folders, window, cx);
                            "ok".into()
                        }
                        Err(err) => format!("error: {err}"),
                    }
                }
            }
            "project_unfold" => {
                self.projects.instant = false;
                self.dev_project_unfold(arg, true)
            }
            "project_fold" => {
                self.projects.instant = false;
                if arg == "home" {
                    self.projects.home_folded = !self.projects.home_folded;
                    self.projects.home_fold_gen += 1;
                    "ok".into()
                } else {
                    self.dev_project_unfold(arg, false)
                }
            }
            "project_scope" => {
                if arg == "off" {
                    self.projects.close_scope();
                    "ok".into()
                } else {
                    self.dev_project_scope(cx)
                }
            }
            "project_files" => {
                self.projects.close_scope();
                self.projects.close_home();
                self.set_picker(None, cx);
                // An empty chat sits on the hero, which hides the files
                // panel. Nudge the local row so the tree can be shown.
                if let Some(id) = self.current_chat.clone()
                    && let Some(chat) = self.state.chats.get_mut(&id)
                    && chat.last_message_at == 0
                {
                    chat.last_message_at = 1;
                }
                self.files.tab = super::files_panel::FilesTab::Files;
                if !self.files.open {
                    self.toggle_files(window, cx);
                }
                "ok".into()
            }
            _ => format!("error: unknown project command {cmd}"),
        };
        result
    }

    fn dev_project_create(&mut self, arg: &str) -> String {
        let (name, folders) = match parse_spec(arg) {
            Ok(v) => v,
            Err(err) => return format!("error: {err}"),
        };
        if folders.is_empty() {
            return "error: need alias=path".into();
        }
        let mut taken = Vec::new();
        let mut rows = Vec::new();
        for (alias, path) in folders {
            if !projects::valid_alias(&alias) {
                return format!("error: bad alias {alias}");
            }
            if taken.iter().any(|t| t == &alias) {
                return format!("error: {alias} is used twice");
            }
            taken.push(alias.clone());
            rows.push(ProjectFolder { path: norm_path(&path), alias, letter: String::new(), note: String::new() });
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.projects.store.projects.push(Project {
            id: id.clone(),
            name,
            folders: rows,
            created_at: AppState::now_ms().max(0) as u64,
        });
        self.projects.picked = Some(id);
        self.projects.save();
        "ok".into()
    }

    fn dev_project_note(&mut self, arg: &str) -> String {
        let Some((id, rest)) = match_named(&self.projects.store, arg).map(|(project, rest)| (project.id.clone(), rest.to_string())) else {
            return "error: no such project".into();
        };
        let mut words = rest.splitn(2, char::is_whitespace);
        let Some(alias) = words.next().filter(|s| !s.is_empty()) else {
            return "error: project_note <project> <alias> <note>".into();
        };
        let alias = alias.to_string();
        let note = words.next().unwrap_or("").trim().to_string();
        let Some(project) = self.projects.store.projects.iter_mut().find(|p| p.id == id) else {
            return "error: no such project".into();
        };
        let Some(folder) = project.folders.iter_mut().find(|f| f.alias == alias) else {
            return format!("error: no folder {alias}");
        };
        folder.note = note;
        self.projects.save();
        "ok".into()
    }

    fn dev_project_open(&mut self, arg: &str, cx: &mut Context<Self>) -> String {
        let Some(project) = match_named(&self.projects.store, arg).map(|(p, _)| p.clone()) else {
            return "error: no such project".into();
        };
        let id = project.id.clone();
        self.projects.picked = Some(id.clone());
        self.projects.unfolded.insert(id.clone());
        if let Some(chat) = self.project_chats(&id).into_iter().next() {
            self.projects.home = None;
            self.close_pages();
            self.state.open_or_focus_tab(&chat.0);
            return "ok".into();
        }
        let Some(path) = project.primary().map(|f| f.path.clone()) else {
            return "error: project has no folder".into();
        };
        let Some(client) = self.client.clone() else { return "error: no engine".into() };
        let harness = self.selected_harness.clone();
        let model = self.selected_model.clone();
        let effort = self.selected_effort.clone();
        let permission = self.effective_permission().as_str().to_string();
        let title = project.name.clone();
        cx.spawn(async move |this, cx| {
            let space = client.call("create_space", serde_json::json!({ "path": path })).await;
            let Some(space_id) = space.ok().and_then(|v| v.get("space_id").and_then(|s| s.as_str()).map(str::to_string)) else {
                return;
            };
            let created = client
                .call(
                    "create_chat",
                    serde_json::json!({
                        "space_id": space_id,
                        "cwd": path,
                        "title": title,
                        "harness": harness,
                        "model": model,
                        "reasoning": effort,
                        "permission": permission,
                    }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.state.record_created_space(&space_id, &path);
                this.projects.picked = Some(id.clone());
                if let Some(chat_id) = created.ok().and_then(|v| v.get("chat_id").and_then(|s| s.as_str()).map(str::to_string)) {
                    this.projects.record_chat(&chat_id, &id, Vec::new());
                    let now = AppState::now_ms();
                    let chat = this.state.chats.entry(chat_id.clone()).or_insert_with(|| {
                        crate::state::ChatItem::test(&chat_id, &space_id, &title, keel_proto::ChatStatus::Idle, now, now)
                    });
                    chat.harness = harness;
                    chat.model = model;
                    chat.reasoning = effort;
                    chat.reasoning_known = true;
                    chat.permission = permission;
                    this.projects.home = None;
                    this.state.open_or_focus_tab(&chat_id);
                }
                cx.notify();
            });
        })
        .detach();
        "ok".into()
    }

    fn dev_project_home(&mut self, arg: &str, cx: &mut Context<Self>) -> String {
        if arg == "off" {
            self.projects.home = None;
            return "ok".into();
        }
        let Some(project) = (if arg.is_empty() { self.projects.store.projects.first() } else { self.projects.find(arg) }).cloned() else {
            return "error: no such project".into();
        };
        self.close_pages();
        self.projects.home = Some(project.id);
        self.projects.scope_open = false;
        self.refresh_folder_status(cx);
        "ok".into()
    }

    fn dev_project_unfold(&mut self, arg: &str, open: bool) -> String {
        let id = if arg.is_empty() { self.projects.store.projects.first().map(|p| p.id.clone()) } else { self.projects.find(arg).map(|p| p.id.clone()) };
        let Some(id) = id else { return "error: no such project".into() };
        if open {
            self.projects.unfolded.insert(id.clone());
        } else {
            self.projects.unfolded.remove(&id);
        }
        self.glide_gen += 1;
        self.space_fold_gen.insert(format!("p:{id}"), self.glide_gen);
        "ok".into()
    }

    fn dev_project_scope(&mut self, cx: &mut Context<Self>) -> String {
        let chat = self.current_chat.as_deref();
        let cwd = self.mention_root();
        if self.projects.scope_project_id(chat, cwd.as_deref()).is_none() {
            return "error: no project in scope".into();
        }
        self.set_picker(None, cx);
        self.show_permission_menu = false;
        self.projects.scope_open = true;
        "ok".into()
    }

    fn refresh_folder_status(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.projects.home.clone() else { return };
        self.refresh_project_folders(&id, cx);
    }

    /// Branch and change count for each folder of `id`, once.
    pub(super) fn refresh_project_folders(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(paths) = self.projects.store.project(id).map(|p| p.folders.iter().map(|f| norm_path(&f.path)).collect::<Vec<_>>()) else {
            return;
        };
        let Some(client) = self.client.clone() else { return };
        for path in paths {
            if self.projects.status.contains_key(&path) || !self.projects.status_wait.insert(path.clone()) {
                continue;
            }
            let client = client.clone();
            cx.spawn(async move |this, cx| {
                let res = client.call("folder_status", serde_json::json!({ "cwd": path })).await;
                let _ = this.update(cx, |this, cx| {
                    this.projects.status_wait.remove(&path);
                    this.projects.status.insert(path, status_from(res));
                    cx.notify();
                });
            })
            .detach();
        }
    }

    pub(super) fn project_picker_rows(&self, t: Tokens, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let query = self.picker_query.to_lowercase();
        let mut rows: Vec<AnyElement> = vec![menu_label("Projects", t).into_any_element()];
        for project in &self.projects.store.projects {
            if !query.is_empty() && !project.name.to_lowercase().contains(&query) && !project.folders.iter().any(|f| f.alias.contains(&query)) {
                continue;
            }
            let id = project.id.clone();
            let path = project.primary().map(|f| f.path.clone()).unwrap_or_default();
            let aliases = project.folders.iter().map(|f| f.alias.as_str()).collect::<Vec<_>>().join(", ");
            let n = project.folders.len();
            let row = menu_item(format!("project-{id}"), t, true, self.projects.picked.as_deref() == Some(id.as_str()))
                .child(div().flex_none().text_color(c(t.text)).child(project.name.clone()))
                .child(faint(format!("{n} folders · {aliases}"), t))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.projects.instant = false;
                    this.projects.picked = Some(id.clone());
                    this.projects.close_sheet();
                    this.set_picker(None, cx);
                    if !path.is_empty() {
                        this.use_folder_for_new_chat(path.clone(), cx);
                    }
                }));
            rows.push(row.into_any_element());
        }
        rows.push(
            menu_item("project-new", t, true, false)
                .child(div().text_color(c(t.text_muted)).child("New project…"))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.projects.instant = false;
                    this.begin_sheet("", Vec::new(), window, cx);
                    cx.notify();
                }))
                .into_any_element(),
        );
        rows.push(menu_separator(t).into_any_element());
        rows
    }

    pub(super) fn render_project_sheet(&self, t: Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(sheet) = &self.projects.sheet else { return div().into_any_element() };
        let clock = self.project_clock();
        let generation = sheet.generation;
        let rows_n = sheet.rows.len() + 1;
        let total = SHEET_GROW.max(60. + ROW_STAGGER * rows_n as f32 + ROW_IN);
        let stagger = move |el: gpui::Stateful<gpui::Div>, ix: usize| {
            timed(el, format!("project-sheet-row-{generation}-{ix}"), total, clock, move |el, ms| {
                let k = ease_out(step(ms, 60. + ROW_STAGGER * ix as f32, ROW_IN));
                el.opacity(k).relative().top(zpx(4. * (1. - k)))
            })
        };
        let back = cx.listener(|this, _: &ClickEvent, _, cx| {
            this.projects.instant = true;
            this.projects.close_sheet();
            cx.notify();
        });
        let header = div()
            .px_2()
            .pt_1()
            .pb_1p5()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id("project-sheet-back")
                    .flex_none()
                    .text_color(c(t.text_faint))
                    .cursor_pointer()
                    .hover(|s| s.text_color(c(t.text)))
                    .child(Icon::Back.el(zpx(13.)))
                    .on_click(back),
            )
            .child(div().t_body().font_weight(gpui::FontWeight::MEDIUM).text_color(c(t.text)).child("New project"));
        let name = div()
            .mx_1()
            .h(zpx(30.))
            .px_2()
            .flex()
            .flex_row()
            .items_center()
            .rounded(RADIUS_MD)
            .border_1()
            .border_color(c(t.border))
            .bg(c(t.window))
            .w_full()
            .t_body()
            .text_color(c(t.text))
            .child(div().flex_1().min_w_0().child(sheet.name.clone()));
        let mut col: Vec<AnyElement> = vec![header.into_any_element(), name.into_any_element(), menu_label("Folders", t).mt_1().into_any_element()];
        for (ix, row) in sheet.rows.iter().enumerate() {
            let alias = input_text(&row.alias, cx);
            let letter = input_text(&row.letter, cx);
            let shown = letter.chars().next().or_else(|| alias.chars().next()).map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into());
            let name = base_name(&row.path);
            let line = div()
                .id(ElementId::Name(format!("project-sheet-folder-{ix}").into()))
                .h(zpx(34.))
                .px_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .rounded(RADIUS_SM)
                .hover(|s| s.bg(c(t.hover)))
                .child(mark(shown, ix, t))
                .child(
                    div()
                        .flex_none()
                        .h(zpx(20.))
                        .w(zpx(64.))
                        .px_1p5()
                        .flex()
                        .items_center()
                        .rounded(RADIUS_SM)
                        .border_1()
                        .border_color(c(t.border))
                        .font_family(MONO)
                        .t_small()
                        .text_color(c(t.text))
                        .overflow_hidden()
                        .child(div().size_full().min_w_0().child(row.alias.clone())),
                )
                .child(
                    div()
                        .flex_none()
                        .w(zpx(28.))
                        .min_w_0()
                        .overflow_hidden()
                        .font_family(MONO)
                        .t_caption()
                        .text_color(c(hue(ix, t)))
                        .child(div().size_full().min_w_0().child(row.letter.clone())),
                )
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(c(t.text_muted)).child(name))
                .when(ix == 0, |d| d.child(div().flex_none().px_1p5().rounded(RADIUS_SM).bg(c(t.hover)).t_caption().text_color(c(t.text_faint)).child("starts here")));
            col.push(stagger(line, ix));
        }
        let add = menu_item("project-sheet-add", t, true, false)
            .child(div().text_color(c(t.text_muted)).child("Add folder…"))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.pick_sheet_folder(window, cx);
            }));
        col.push(stagger(add, sheet.rows.len()));
        col.push(menu_separator(t).into_any_element());
        if let Some(err) = &self.projects.sheet_error {
            col.push(div().px_2().t_small().text_color(c(t.warning)).child(err.clone()).into_any_element());
        }
        col.push(
            div()
                .px_2()
                .pt_0p5()
                .pb_1()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().t_small().text_color(c(t.text_faint)).child("Chats start in the first folder. Point at the others by name, like @api/src."))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_1()
                        .child(text_button("project-sheet-cancel", "Cancel", t).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.projects.instant = true;
                            this.projects.close_sheet();
                            cx.notify();
                        })))
                        .child(filled_button("project-sheet-create", "Create project", t.accent, t).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if let Some(path) = this.commit_sheet(cx) {
                                this.projects.instant = true;
                                this.set_picker(None, cx);
                                this.use_folder_for_new_chat(path, cx);
                            }
                            cx.notify();
                        }))),
                )
                .into_any_element(),
        );
        timed(div().flex().flex_col().children(col), format!("project-sheet-{generation}"), total, clock, |el, ms| {
            let k = ease_out(step(ms, 0., SHEET_GROW));
            if k >= 1. {
                return el;
            }
            el.max_h(zpx(140. + 260. * k)).overflow_hidden().opacity(0.5 + 0.5 * k)
        })
    }

    fn pick_sheet_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: true, prompt: Some("Add folders".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    for path in paths {
                        this.push_sheet_folder(path.to_string_lossy().into_owned(), window, cx);
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    pub(super) fn render_project_header(
        &mut self,
        project: Project,
        open: bool,
        first: bool,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let id = project.id.clone();
        let reduced = self.reduced() || self.projects.instant;
        let group = format!("project-h-{id}");
        let home_open = self.projects.home.as_deref() == Some(id.as_str());
        let toggle_id = id.clone();
        let total = self.project_chats(&id).len();
        let urgent = self
            .project_chats(&id)
            .into_iter()
            .filter_map(|(cid, _, _)| self.state.chats.get(&cid))
            .map(|item| keel_proto::display_status(item.status, item.last_seen_at, AppState::now_ms()))
            .find(|s| *s == keel_proto::DisplayStatus::AwaitingInput || *s == keel_proto::DisplayStatus::Running)
            .unwrap_or(keel_proto::DisplayStatus::Idle);
        let collapsed = !open;
        let turn = chevron_turn_radians;
        let chevron = Icon::ChevronDown.el(zpx(12.));
        let chevron: AnyElement = match self.space_fold_gen.get(&format!("p:{id}")) {
            Some(&generation) if !reduced => {
                use gpui::AnimationExt as _;
                let (from, to) = (turn(!collapsed), turn(collapsed));
                chevron
                    .with_animation(
                        ElementId::Name(format!("project-side-chevron-{id}-{generation}").into()),
                        gpui::Animation::new(crate::motion::FAST).with_easing(crate::motion::ease_out),
                        move |icon, delta| {
                            icon.with_transformation(gpui::Transformation::rotate(gpui::radians(from + (to - from) * delta)))
                        },
                    )
                    .into_any_element()
            }
            _ => chevron.with_transformation(gpui::Transformation::rotate(gpui::radians(turn(collapsed)))).into_any_element(),
        };
        div()
            .id(ElementId::Name(format!("project-header-{id}").into()))
            .flex_none()
            .when(!first, |d| d.pt(zpx(10.)))
            .child(
                div()
                    .id(ElementId::Name(group.clone().into()))
                    .group(group.clone())
                    .h(zpx(28.))
                    .pl_1()
                    .pr_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .rounded(RADIUS_MD)
                    .cursor_pointer()
                    .hover(|s| s.bg(c(t.hover)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_project_fold(&toggle_id, cx);
                    }))
                    .child(
                        div()
                            .id(ElementId::Name(format!("project-side-fold-{id}").into()))
                            .size(zpx(16.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(c(t.text_faint))
                            .child(chevron),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .t_small()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(t.text_muted))
                            .group_hover(group.clone(), |s| s.text_color(c(t.text)))
                            .child(project.name),
                    )
                    .when(collapsed, |d| {
                        d.children(match urgent {
                            keel_proto::DisplayStatus::AwaitingInput | keel_proto::DisplayStatus::Running => {
                                status_dot(urgent, &format!("project-{id}"), t, reduced)
                            }
                            _ => None,
                        })
                        .when(total > 0, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .px_1()
                                    .t_caption()
                                    .font_family(crate::style::MONO)
                                    .text_color(c(t.text_faint))
                                    .child(total.to_string()),
                            )
                        })
                    })
                    .child(
                        div()
                            .flex_none()
                            .opacity(if home_open { 1. } else { 0. })
                            .group_hover(group, |s| s.opacity(1.))
                            .child(
                                super::sidebar::header_button(format!("project-home-{id}"), Icon::AppWindow, t)
                                    .tooltip(move |_, cx| cx.new(|_| super::dotify_ui::Tip::new("Project home", t)).into())
                                    .tooltip_show_delay(super::tooltip::delay())
                                    .on_click(cx.listener({
                                        let id = id.clone();
                                        move |this, _: &ClickEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.projects.instant = false;
                                            this.projects.folder_card = None;
                                            this.projects.folder_card_suppressed = Some(id.clone());
                                            this.close_pages();
                                            this.projects.home = Some(id.clone());
                                            this.refresh_folder_status(cx);
                                            cx.notify();
                                        }
                                    })),
                            ),
                    )
                    .on_hover(cx.listener({
                        let id = id.clone();
                        move |this, on: &bool, window, cx| {
                            let y = f32::from(window.mouse_position().y);
                            this.hover_project_folders(&id, y, *on, cx);
                        }
                    })),
            )
    }

    /// Folder detail for the project under the pointer. Same surface as the
    /// session summary card, held just long enough to cross onto it.
    pub(super) fn render_project_folder_card(&mut self, t: Tokens, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.state.ui_settings.sidebar_collapsed {
            self.projects.folder_card = None;
            return None;
        }
        let (id, anchor_y, since, left) = {
            let card = self.projects.folder_card.as_ref()?;
            (card.id.clone(), card.y, card.since, card.left)
        };
        // Don't show redundant floating card if the project's own Home page is already active.
        if self.projects.home.as_deref() == Some(id.as_str()) {
            self.projects.folder_card = None;
            return None;
        }
        let elapsed = since.elapsed();
        if let Some(left) = left {
            // If the user already hovered out before the card ever became ripe, dismiss immediately.
            if elapsed < SUMMARY_HOVER_DELAY || left.elapsed() >= FOLDER_CARD_LEAVE {
                self.projects.folder_card = None;
                return None;
            }
            window.request_animation_frame();
        }
        let reduced = self.reduced();
        if !reduced && elapsed < SUMMARY_HOVER_DELAY {
            window.request_animation_frame();
            return None;
        }
        let project = self.projects.store.project(&id)?.clone();
        let appear = if reduced {
            1.
        } else {
            let enter_x = (elapsed.saturating_sub(SUMMARY_HOVER_DELAY).as_secs_f32() / 0.18).min(1.);
            let mut a = ease_out(enter_x);
            if enter_x < 1. {
                window.request_animation_frame();
            }
            if let Some(left) = left {
                let exit_x = (left.elapsed().as_secs_f32() / FOLDER_CARD_LEAVE.as_secs_f32()).clamp(0., 1.);
                a *= 1. - ease_out(exit_x);
            }
            a
        };
        if appear <= 0.005 {
            if left.is_some() {
                self.projects.folder_card = None;
            }
            return None;
        }
        let primary = project.primary().map(|f| f.alias.clone()).unwrap_or_else(|| "the first folder".into());
        let mut rows: Vec<AnyElement> = Vec::new();
        for (ix, folder) in project.folders.iter().enumerate() {
            let branch = self.projects.folder_branch_label(&folder.path);
            let changes = self.projects.folder_changes_label(&folder.path);
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(1.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(6.))
                            .min_w_0()
                            .child(mark(mark_letter(folder), ix, t))
                            .child(div().flex_none().t_small().font_weight(gpui::FontWeight::MEDIUM).text_color(c(t.text)).child(folder.alias.clone()))
                            .child(div().flex_1().min_w_0().truncate().t_small().text_color(c(t.text_muted)).child(base_name(&folder.path))),
                    )
                    .child(
                        div()
                            .pl(zpx(22.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(6.))
                            .min_w_0()
                            .child(div().flex_none().text_color(c(t.text_faint)).child(Icon::Branch.el(zpx(11.))))
                            .child(div().min_w_0().truncate().t_small().text_color(c(t.text_muted)).child(branch))
                            .child(div().flex_1())
                            .child(div().flex_none().t_small().text_color(c(t.text_faint)).child(changes)),
                    )
                    .into_any_element(),
            );
        }
        let sidebar_width = self.state.sidebar_width();
        Some(
            deferred(
                anchored()
                    .position(point(px(sidebar_width + 8.), px((anchor_y - 16.).max(8.))))
                    .snap_to_window_with_margin(zpx(8.))
                    .child(
                        div()
                            .id(ElementId::Name(format!("project-folder-card-{id}").into()))
                            .w(zpx(FOLDER_CARD_W))
                            .bg(c(t.floating))
                            .border_1()
                            .border_color(c(t.border))
                            .rounded(RADIUS_LG)
                            .shadow(vec![gpui::BoxShadow {
                                color: c(t.shadow),
                                offset: point(px(0.), zpx(8.)),
                                blur_radius: zpx(24.),
                                spread_radius: zpx(-4.),
                                inset: false,
                            }])
                            .p(zpx(12.))
                            .flex()
                            .flex_col()
                            .gap(zpx(8.))
                            .opacity(appear)
                            .on_hover(cx.listener({
                                let id = id.clone();
                                move |this, on: &bool, window, cx| {
                                    let y = f32::from(window.mouse_position().y);
                                    this.set_folder_card(&id, y, None, Some(*on));
                                    cx.notify();
                                }
                            }))
                            .child(div().t_body().font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(t.text)).child(project.name.clone()))
                            .child(div().t_small().text_color(c(t.text_muted)).child({
                                let n = project.folders.len();
                                let folders = if n == 1 { "1 folder".into() } else { format!("{n} folders") };
                                format!("{folders}. Chats start in {primary}.")
                            }))
                            .children(rows),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    /// Chat ids linked to any project. They stay out of the folder and loose lists.
    pub(super) fn linked_chat_ids(&self) -> std::collections::HashSet<String> {
        self.projects.store.chats.keys().cloned().collect()
    }

    /// Sidebar order: project sessions, then folder sessions, then chats with neither.
    pub(super) fn sidebar_row_ids(&self) -> Vec<String> {
        let mut rows = Vec::new();
        if self.state.space_filter.is_none() {
            let ids: Vec<String> = self.projects.store.projects.iter().map(|p| p.id.clone()).collect();
            for id in ids {
                rows.extend(self.visible_project_chats(&id).into_iter().map(|(chat, _)| chat.id));
            }
        }
        let skip = self.linked_chat_ids();
        rows.extend(
            self.state
                .sidebar_sections_skipping(AppState::now_ms(), &skip)
                .into_iter()
                .flat_map(|section| section.rows.into_iter().map(|(chat, _)| chat.id)),
        );
        rows
    }

    /// Sessions of one project. Folded, only the active chat and anything waiting
    /// on you. Unfolded, the three most recent, so the list stays a list.
    pub(super) fn visible_project_chats(&self, id: &str) -> Vec<(keel_proto::AttentionChat, keel_proto::DisplayStatus)> {
        let now = AppState::now_ms();
        let active = self.state.active_tab.as_deref();
        let open = self.projects.unfolded.contains(id);
        let mut out = Vec::new();
        for (chat_id, _, _) in self.project_chats(id) {
            let Some(item) = self.state.chats.get(&chat_id) else { continue };
            let status = keel_proto::display_status(item.status, item.last_seen_at, now);
            let pinned = status == keel_proto::DisplayStatus::AwaitingInput || active == Some(chat_id.as_str());
            if !open && !pinned {
                continue;
            }
            out.push((
                keel_proto::AttentionChat {
                    id: item.id.clone(),
                    space_id: item.space_id.clone(),
                    status: item.status,
                    last_message_at: item.last_message_at,
                    last_seen_at: item.last_seen_at,
                },
                status,
            ));
            if open && out.len() == SIDE_CHAT_CAP {
                break;
            }
        }
        out
    }

    fn hover_project_folders(&mut self, id: &str, y: f32, over_header: bool, cx: &mut Context<Self>) {
        if !over_header {
            if self.projects.folder_card_suppressed.as_deref() == Some(id) {
                self.projects.folder_card_suppressed = None;
            }
        } else if self.projects.folder_card_suppressed.as_deref() == Some(id) {
            return;
        }
        self.set_folder_card(id, y, Some(over_header), None);
        if over_header {
            self.refresh_project_folders(id, cx);
        }
        cx.notify();
    }
}

impl Projects {
    pub(super) fn set_folder_card(
        &mut self,
        id: &str,
        y: f32,
        over_header: Option<bool>,
        over_card: Option<bool>,
        now: std::time::Instant,
    ) {
        let leaving = over_header == Some(false) || over_card == Some(false);
        if leaving && self.folder_card.as_ref().is_none_or(|card| card.id != id) {
            return;
        }
        // If the pointer leaves the header before the card has even become visible (still within hover delay),
        // cancel the pending card immediately. No leave timer or ghost card.
        if over_header == Some(false) {
            if let Some(card) = &self.folder_card {
                if !card.over_card && now.saturating_duration_since(card.since) < SUMMARY_HOVER_DELAY {
                    self.folder_card = None;
                    return;
                }
            }
        }
        let card = self.folder_card.get_or_insert_with(|| FolderCard {
            id: id.to_string(),
            y,
            since: now,
            over_header: false,
            over_card: false,
            left: None,
        });
        if card.id != id {
            *card = FolderCard { id: id.to_string(), y, since: now, over_header: false, over_card: false, left: None };
        }
        card.y = y;
        if let Some(on) = over_header {
            card.over_header = on;
        }
        if let Some(on) = over_card {
            card.over_card = on;
        }
        if card.over_header || card.over_card {
            card.left = None;
        } else if card.left.is_none() {
            card.left = Some(now);
        }
    }
}

impl KeelWindow {
    fn set_folder_card(&mut self, id: &str, y: f32, over_header: Option<bool>, over_card: Option<bool>) {
        self.projects.set_folder_card(id, y, over_header, over_card, std::time::Instant::now());
    }

    pub(super) fn toggle_project_fold(&mut self, id: &str, cx: &mut Context<Self>) {
        self.projects.instant = false;
        self.projects.folder_card = None;
        self.projects.folder_card_suppressed = Some(id.to_string());
        let open = self.projects.unfolded.contains(id);
        if open {
            self.projects.unfolded.remove(id);
        } else {
            self.projects.unfolded.insert(id.to_string());
        }
        self.glide_gen += 1;
        self.space_fold_gen.insert(format!("p:{id}"), self.glide_gen);
        cx.notify();
    }

    pub(super) fn render_project_home(&mut self, t: Tokens, cx: &mut Context<Self>) -> AnyElement {
        self.refresh_folder_status(cx);
        let Some(id) = self.projects.home.clone() else { return div().into_any_element() };
        let Some(project) = self.projects.store.project(&id).cloned() else { return div().into_any_element() };
        let (folded, fold_gen) = (self.projects.home_folded, self.projects.home_fold_gen);
        let clock = if fold_gen == 0 || self.projects.instant || self.reduced() { Clock::Rest } else { self.project_clock() };
        let (open_ms, close_ms) = (fold::OPEN.as_secs_f32() * 1000., fold::CLOSE.as_secs_f32() * 1000.);
        let total = if folded { close_ms } else { open_ms };
        let opening = !folded;
        let section = |label: &'static str, count: usize| {
            div()
                .h(zpx(28.))
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .t_small()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(c(t.text_muted))
                .child(label)
                .child(div().t_caption().font_family(MONO).font_weight(gpui::FontWeight::NORMAL).text_color(c(t.text_faint)).child(count.to_string()))
        };
        let chevron = timed(Icon::ChevronDown.el(zpx(12.)), format!("project-home-chevron-{fold_gen}"), total, clock, move |el, ms| {
            let open = if clock_is_rest(clock) { if opening { 1. } else { 0. } } else { fold_progress(ms, opening, open_ms, close_ms) };
            el.with_transformation(gpui::Transformation::rotate(gpui::percentage(fold::chevron_turn(open))))
        });
        let folders_header = div()
            .id("project-home-folders-toggle")
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .ml(zpx(-20.))
            .cursor_pointer()
            .child(div().size(zpx(16.)).flex().items_center().justify_center().text_color(c(t.text_faint)).child(chevron))
            .child(section("Folders", project.folders.len()))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.projects.instant = false;
                this.projects.home_folded = !this.projects.home_folded;
                this.projects.home_fold_gen += 1;
                cx.notify();
            }));
        let primary = project.primary().map(|f| f.alias.clone()).unwrap_or_default();
        let n = project.folders.len();
        let rows = project.folders.iter().enumerate().map(|(ix, f)| {
            let key = norm_path(&f.path);
            let status = self.projects.status.get(&key);
            let branch = status.map(|s| if !s.branch.is_empty() { s.branch.clone() } else if !s.repo { "not a repo".into() } else { "detached".into() }).unwrap_or_else(|| "…".into());
            let changes = status
                .map(|s| match s.changes {
                    Some(0) | None if s.repo => "clean".into(),
                    Some(n) => format!("{n} changed"),
                    None if !s.exists => "missing".into(),
                    None => "not a repo".into(),
                })
                .unwrap_or_else(|| "…".into());
            let changed = status.and_then(|s| s.changes).unwrap_or(0) > 0;
            div()
                .id(ElementId::Name(format!("project-home-folder-{ix}").into()))
                .h(zpx(HOME_ROW_H))
                .flex_none()
                .px_2()
                .ml(zpx(-8.))
                .mr(zpx(-8.))
                .flex()
                .flex_col()
                .justify_center()
                .gap_0p5()
                .rounded(RADIUS_MD)
                .hover(|s| s.bg(c(t.hover)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(mark(mark_letter(f), ix, t))
                        .child(div().flex_none().t_body().font_weight(gpui::FontWeight::MEDIUM).text_color(c(t.text)).child(f.alias.clone()))
                        .child(div().flex_1())
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .t_small()
                                .text_color(c(t.text_faint))
                                .child(Icon::Branch.el(zpx(12.)))
                                .child(div().max_w(zpx(170.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(branch)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(zpx(88.))
                                .flex()
                                .justify_end()
                                .t_small()
                                .text_color(c(if changed { t.text_muted } else { t.text_faint }))
                                .child(changes),
                        ),
                )
                .child(
                    div()
                        .pl(zpx(22.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .t_small()
                        .when(!f.note.is_empty(), |d| d.child(div().flex_none().text_color(c(t.text_muted)).child(f.note.clone())))
                        .child(faint(display_path(&f.path), t).font_family(MONO).t_caption()),
                )
        });
        let list_h = HOME_ROW_H * n as f32;
        let list = timed(div().flex().flex_col().children(rows), format!("project-home-fold-{fold_gen}"), total, clock, move |el, ms| {
            let p = if clock_is_rest(clock) { if opening { 1. } else { 0. } } else { fold_progress(ms, opening, open_ms, close_ms) };
            if p >= 1. {
                return el;
            }
            el.h(zpx(list_h * p)).overflow_hidden().opacity(p)
        });
        let add = div()
            .id("project-home-add")
            .h(zpx(30.))
            .px_2()
            .ml(zpx(-8.))
            .mr(zpx(-8.))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .rounded(RADIUS_MD)
            .t_small()
            .text_color(c(t.text_faint))
            .cursor_pointer()
            .hover(|s| s.bg(c(t.hover)).text_color(c(t.text_muted)))
            .child(div().w(zpx(14.)).flex().justify_center().child(Icon::Plus.el(zpx(12.))))
            .child("Add folder")
            .child(div().flex_1())
            .child("or drop one here")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.pick_home_folder(window, cx)))
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                let Some(id) = this.projects.home.clone() else { return };
                for path in paths.paths() {
                    if path.is_dir() {
                        this.projects.add_folder(&id, path.to_string_lossy().into_owned());
                    }
                }
                this.projects.status.clear();
                this.refresh_folder_status(cx);
                cx.notify();
            }));
        let chats = self.project_chats(&id);
        let chat_n = chats.len();
        let chat_rows = chats.into_iter().enumerate().map(|(n, (chat_id, title, when))| {
            let scope = self.projects.store.chats.get(&chat_id).map(|l| l.scope.clone()).unwrap_or_default();
            let letters = projects::in_scope(&project, &scope);
            div()
                .id(ElementId::Name(format!("project-home-chat-{n}").into()))
                .h(zpx(34.))
                .px_2()
                .ml(zpx(-8.))
                .mr(zpx(-8.))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .rounded(RADIUS_MD)
                .cursor_pointer()
                .hover(|s| s.bg(c(t.hover)))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().t_body().text_color(c(t.text_muted)).child(title))
                .child(div().flex_none().flex().flex_row().gap_0p5().children(letters.iter().map(|f| {
                    let ix = project.folders.iter().position(|g| g.alias == f.alias).unwrap_or(0);
                    mark(mark_letter(f), ix, t)
                })))
                .child(div().flex_none().w(zpx(72.)).flex().justify_end().t_small().text_color(c(t.text_faint)).child(ago(when)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.projects.home = None;
                    this.state.open_or_focus_tab(&chat_id);
                    cx.notify();
                }))
        });
        let blurb = format!("{n} folders the agent works across. New chats start in {primary}.");
        div()
            .id("project-home")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .mx_auto()
                    .w_full()
                    .max_w(zpx(680.))
                    .px(zpx(32.))
                    .pt(zpx(28.))
                    .pb(zpx(32.))
                    .flex()
                    .flex_col()
                    .child(div().t_title().text_color(c(t.text)).child(project.name.clone()))
                    .child(div().mt_1().t_small().text_color(c(t.text_faint)).child(blurb))
                    .child(div().mt(zpx(28.)).child(folders_header))
                    .child(list)
                    .child(add)
                    .child(div().mt(zpx(24.)).child(section("Chats", chat_n)))
                    .children(chat_rows),
            )
            .into_any_element()
    }

    fn pick_home_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.projects.home.clone() else { return };
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: true, prompt: Some("Add folders".into()) });
        let _ = window;
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
                for path in paths {
                    this.projects.add_folder(&id, path.to_string_lossy().into_owned());
                }
                this.projects.status.clear();
                this.refresh_folder_status(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn project_scope_chip(&mut self, t: Tokens, compact: u8, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat = self.current_chat.clone();
        let cwd = self.mention_root();
        let id = self.projects.scope_project_id(chat.as_deref(), cwd.as_deref())?;
        let project = self.projects.store.project(&id)?.clone();
        let open = self.projects.scope_open;
        let aliases = self.projects.scope_aliases(&id, chat.as_deref());
        let n = if aliases.is_empty() { project.folders.len() } else { aliases.len() };
        let label = match compact {
            0 => if aliases.is_empty() { "Folders".into() } else { aliases.join(", ") },
            1 => if n == 1 { aliases.first().cloned().unwrap_or_else(|| "1 folder".into()) } else { format!("{n} folders") },
            _ => String::new(),
        };
        let phase = self.menu_phase("project-scope-menu", open, cx);
        let menu = phase.map(|leaving| {
            let scope = self.projects.scope_of(&id, chat.as_deref());
            let mut items: Vec<AnyElement> = vec![
                menu_label("Folders this chat works in", t).into_any_element(),
                menu_item("project-scope-all", t, true, scope.is_empty())
                    .child(div().flex_1().child("All folders"))
                    .child(div().flex_none().t_small().text_color(c(t.text_faint)).child(project.folders.len().to_string()))
                    .on_click(cx.listener({
                        let id = id.clone();
                        let chat = chat.clone();
                        move |this, _: &ClickEvent, _, cx| {
                            this.projects.write_scope(&id, chat.as_deref(), Vec::new());
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
                menu_separator(t).into_any_element(),
            ];
            for (ix, folder) in project.folders.iter().enumerate() {
                let on = aliases.iter().any(|a| a == &folder.alias);
                let alias = folder.alias.clone();
                items.push(
                    menu_item(format!("project-scope-{ix}"), t, true, on)
                        .child(mark(mark_letter(folder), ix, t))
                        .child(div().flex_none().child(folder.alias.clone()))
                        .child(faint(base_name(&folder.path), t))
                        .on_click(cx.listener({
                            let id = id.clone();
                            let chat = chat.clone();
                            move |this, _: &ClickEvent, _, cx| {
                                this.projects.toggle_scope_alias(&id, chat.as_deref(), &alias);
                                cx.notify();
                            }
                        }))
                        .into_any_element(),
                );
            }
            items.push(menu_separator(t).into_any_element());
            items.push(div().px_2().pt_0p5().pb_1().t_small().text_color(c(t.text_faint)).child("The others stay reachable with @.").into_any_element());
            let dismiss = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                this.note_menu_dismissed("project-scope-menu");
                this.projects.scope_open = false;
                cx.notify();
            });
            floating_menu_at("project-scope-menu", t, 280., true, leaving, self.reduced(), dismiss, items)
        });
        let chip = div()
            .id("project-scope-chip")
            .h(zpx(26.))
            .px_2()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .rounded(RADIUS_MD)
            .t_small()
            .text_color(c(if open { t.text } else { t.text_muted }))
            .cursor_pointer()
            .when(open, |s| s.bg(c(t.selected)))
            .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
            .min_w_0()
            .overflow_hidden()
            .child(Icon::Folder.el(zpx(12.)).flex_none())
            .when(!label.is_empty(), |d| {
                d.child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
            })
            .child(div().flex_none().text_color(c(t.text_faint)).child(Icon::ChevronDown.el(zpx(10.))))
            .tooltip(move |_, cx| cx.new(|_| super::dotify_ui::Tip::new("Folders the agent works in", t)).into())
            .tooltip_show_delay(super::tooltip::delay())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if this.menu_just_dismissed("project-scope-menu") {
                    return;
                }
                let next = !this.projects.scope_open;
                this.set_picker(None, cx);
                this.show_permission_menu = false;
                this.projects.scope_open = next;
                cx.notify();
            }));
        Some(div().relative().min_w_0().flex_shrink(1.).children(menu).child(chip).into_any_element())
    }
}

fn clock_is_rest(clock: Clock) -> bool {
    matches!(clock, Clock::Rest)
}

fn display_path(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if let Some(rest) = norm_path(path).strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

fn status_from(res: Result<serde_json::Value, String>) -> FolderStatus {
    let Ok(v) = res else {
        return FolderStatus {
            branch: String::new(),
            changes: None,
            files: Vec::new(),
            file_details: Vec::new(),
            total_additions: 0,
            total_deletions: 0,
            repo: false,
            exists: false,
        };
    };
    let exists = v.get("exists").and_then(|s| s.as_bool()).unwrap_or(true);
    let branch = v.get("branch").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let detached = v.get("detached").and_then(|s| s.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
    let changes = v.get("changes").and_then(|s| s.as_u64()).map(|n| n as u32);
    let files = v
        .get("files")
        .and_then(|s| s.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let file_details = v
        .get("file_details")
        .and_then(|d| serde_json::from_value::<Vec<crate::view::session_card::GitFileChange>>(d.clone()).ok())
        .unwrap_or_default();
    let total_additions = v.get("total_additions").and_then(|n| n.as_u64()).unwrap_or_else(|| {
        file_details.iter().filter_map(|f| f.added).map(u64::from).sum()
    });
    let total_deletions = v.get("total_deletions").and_then(|n| n.as_u64()).unwrap_or_else(|| {
        file_details.iter().filter_map(|f| f.removed).map(u64::from).sum()
    });
    let repo = changes.is_some() || !branch.is_empty() || detached.is_some();
    let branch = if branch.is_empty() { detached.unwrap_or_default() } else { branch };
    FolderStatus {
        branch,
        changes,
        files,
        file_details,
        total_additions,
        total_deletions,
        repo,
        exists,
    }
}

/// Longest project name or id at the start of `arg`, and the text after it.
fn match_named<'a>(store: &'a projects::Projects, arg: &'a str) -> Option<(&'a Project, &'a str)> {
    let arg = arg.trim();
    let mut best: Option<(&Project, usize)> = None;
    for project in &store.projects {
        for key in [project.name.as_str(), project.id.as_str()] {
            if key.is_empty() {
                continue;
            }
            let hit = if arg.eq_ignore_ascii_case(key) {
                Some(key.len())
            } else if arg.len() > key.len() && arg.as_bytes().get(key.len()) == Some(&b' ') && arg[..key.len()].eq_ignore_ascii_case(key) {
                Some(key.len())
            } else {
                None
            };
            if let Some(n) = hit
                && best.as_ref().is_none_or(|(_, was)| n > *was)
            {
                best = Some((project, n));
            }
        }
    }
    best.map(|(project, n)| (project, arg[n..].trim_start()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_change_labels_stay_per_folder() {
        assert_eq!(folder_changes_text(Some(0), true, true), "clean");
        assert_eq!(folder_changes_text(Some(37), true, true), "37 changed");
        assert_eq!(folder_changes_text(None, false, true), "not a repo");
        assert_eq!(folder_changes_text(None, true, false), "missing");
    }

    #[test]
    fn spec_parses_a_spaced_name() {
        let (name, folders) = parse_spec("Resona RIF web=/tmp/web api=/tmp/api").unwrap();
        assert_eq!(name, "Resona RIF");
        assert_eq!(folders, vec![("web".into(), "/tmp/web".into()), ("api".into(), "/tmp/api".into())]);
    }

    #[test]
    fn paths_map_onto_aliases() {
        let project = Project {
            id: "p".into(),
            name: "P".into(),
            folders: vec![
                ProjectFolder { path: "/r/web".into(), alias: "web".into(), ..Default::default() },
                ProjectFolder { path: "/r/api".into(), alias: "api".into(), ..Default::default() },
            ],
            created_at: 0,
        };
        assert_eq!(map_alias_path(Some(&project), "/r/api/src/x.ts").as_deref(), Some("@api/src/x.ts"));
        assert_eq!(map_alias_path(Some(&project), "/r/web").as_deref(), Some("@web"));
        assert_eq!(map_alias_path(Some(&project), "/other/x"), None);
    }

    #[test]
    fn project_collapsible_chevron_turn() {
        assert_eq!(chevron_turn_radians(true), -std::f32::consts::FRAC_PI_2);
        assert_eq!(chevron_turn_radians(false), 0.0);

        // When open is true (unfolding), the animation starts at folded (-FRAC_PI_2) and lands at open (0.0)
        let open = true;
        let (from, to) = (chevron_turn_radians(open), chevron_turn_radians(!open));
        assert_eq!(from, -std::f32::consts::FRAC_PI_2);
        assert_eq!(to, 0.0);

        // When open is false (folding), the animation starts at open (0.0) and lands at folded (-FRAC_PI_2)
        let open = false;
        let (from, to) = (chevron_turn_radians(open), chevron_turn_radians(!open));
        assert_eq!(from, 0.0);
        assert_eq!(to, -std::f32::consts::FRAC_PI_2);
    }

    #[test]
    fn project_folder_card_hover_out_cancels_before_delay_and_click_suppresses() {
        let temp_dir = std::env::temp_dir().join(format!("keel-test-projects-{}", std::process::id()));
        let mut projects = Projects::load(&temp_dir);
        let t0 = std::time::Instant::now();

        // 1. Pointer hovers on project header
        projects.set_folder_card("resona", 120.0, Some(true), None, t0);
        assert!(projects.folder_card.is_some());
        assert_eq!(projects.folder_card.as_ref().unwrap().id, "resona");
        assert!(projects.folder_card.as_ref().unwrap().over_header);
        assert_eq!(projects.folder_card.as_ref().unwrap().left, None);

        // 2. Pointer hovers out after only 100ms (< SUMMARY_HOVER_DELAY = 350ms)
        let t_leave = t0 + std::time::Duration::from_millis(100);
        projects.set_folder_card("resona", 120.0, Some(false), None, t_leave);

        // The card is cancelled immediately so it never pops up later!
        assert!(projects.folder_card.is_none());

        // 3. Pointer hovers and stays past SUMMARY_HOVER_DELAY (e.g. 400ms)
        projects.set_folder_card("resona", 120.0, Some(true), None, t0);
        let t_leave_late = t0 + std::time::Duration::from_millis(400);
        projects.set_folder_card("resona", 120.0, Some(false), None, t_leave_late);

        // Card was already ripe, so leaving gives grace period to cross into card
        assert!(projects.folder_card.is_some());
        assert_eq!(projects.folder_card.as_ref().unwrap().left, Some(t_leave_late));

        // 4. Moving pointer over card clears left
        let t_on_card = t_leave_late + std::time::Duration::from_millis(50);
        projects.set_folder_card("resona", 120.0, None, Some(true), t_on_card);
        assert_eq!(projects.folder_card.as_ref().unwrap().left, None);
        assert!(projects.folder_card.as_ref().unwrap().over_card);
    }
}

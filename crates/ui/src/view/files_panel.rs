//! Files pane: what the chat's agent changed (every diff in one scrolling
//! stack, beside the changed files as a list or a tree) and the chat's folder
//! as a tree, with a viewer for any file in it: an editor for text, rendered
//! Markdown, and images. It slides in from the right like the browser pane,
//! in the same slot (one or the other).
//!
//! Kept light: nothing is read until the pane is open, folders load one level
//! at a time, long lists and diffs draw only their visible rows, and nothing
//! watches the disk. The list refreshes when the engine reports the checkout
//! changed, when a turn ends, and on demand. Saving refuses to overwrite a
//! file that changed since it was opened (the agent may have written it).

use crate::hover_fade::HoverFadeExt;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, ClickEvent, Context, Entity, FocusHandle, IntoElement, SharedString,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_base::input::{EditorState, InputEvent, RopeExt as _};
use keel_theme::Tokens;

use super::{KeelWindow, icon_button};
use crate::icons::Icon;
use crate::motion::{BASE, appear};
use crate::style::{HEADER_HEIGHT, KeelStyled, MONO, RADIUS_MD, c, zpx};
use crate::syntax::SyntaxKind;

pub(super) mod fold;
mod stack;
mod viewer;

/// The list column beside the content; under [`STACK_BELOW`] the two stack
/// (the list, or the open file with a way back).
const LIST_W: f32 = 256.;
const STACK_BELOW: f32 = 620.;
/// A list row and a diff line.
const ROW_H: f32 = 28.;
const LINE_H: f32 = 22.;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum FilesTab {
    #[default]
    Changes,
    Files,
}

/// Something fetched: in flight, there, or why not.
enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct Change {
    path: String,
    status: char,
    added: Option<u32>,
    removed: Option<u32>,
}

#[derive(Clone, serde::Deserialize)]
struct Entry {
    name: String,
    dir: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LineKind {
    Hunk,
    Added,
    Removed,
    Context,
    /// `\ No newline at end of file` and the like.
    Note,
}

#[derive(Clone, Debug)]
struct DiffLine {
    kind: LineKind,
    old: Option<u32>,
    new: Option<u32>,
    text: SharedString,
    /// Syntax runs, by byte range into `text`.
    spans: Vec<(std::ops::Range<usize>, SyntaxKind)>,
    /// The part of a changed line that actually differs from its partner.
    emph: Option<std::ops::Range<usize>>,
    /// On a hunk: unchanged lines hidden above it (a click shows them).
    gap: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(super) enum ChangeDetailTab {
    #[default]
    Diff,
    File,
}

/// A parsed patch.
struct DiffDoc {
    lines: Vec<DiffLine>,
    truncated: bool,
}

/// The file open in the editor.
struct OpenFile {
    path: String,
    editor: Entity<EditorState>,
    /// Hash of the disk content the edits started from.
    base: String,
    /// The text last loaded or saved, to tell whether there are edits.
    saved: String,
    dirty: bool,
    saving: bool,
    /// The file changed on disk since it was opened; the save was refused.
    conflict: bool,
    error: Option<String>,
    /// When the last save landed, for the brief "Saved" in the header.
    saved_at: Option<Instant>,
    _changes: gpui::Subscription,
}

pub(super) struct FilesPanel {
    pub(super) open: bool,
    pub(super) anim: Option<(Instant, u64)>,
    pub(super) tab: FilesTab,
    pub(super) change_detail_tab: ChangeDetailTab,
    /// The chat everything below belongs to; another chat starts over.
    chat: Option<String>,
    changes: Option<Load<Rc<Vec<Change>>>>,
    /// A refresh in flight, and another asked for meanwhile.
    refreshing: bool,
    refresh_again: bool,
    dirs: HashMap<String, Load<Vec<Entry>>>,
    expanded: HashSet<String>,
    /// Open folders flattened to rows, built once per change (not per frame).
    tree_cache: Option<Rc<Vec<TreeRow>>>,
    /// The folder opening or closing in the tree.
    tree_fold: Option<fold::TreeFold<TreeRow>>,
    /// Dev: a tooltip shown on demand, for a look at it.
    pub(super) dev_tip: Option<Entity<super::dotify_ui::Tip>>,
    selected: Option<String>,
    /// The Changes tab: every diff, stacked.
    stack: stack::Stack,
    /// The Files tab's viewer: Markdown, images, the reveal glow.
    viewer: viewer::Viewer,
    file: Option<OpenFile>,
    /// A file being read for the editor.
    opening: Option<String>,
    focus: FocusHandle,
    /// The window, for replies that land with no frame running.
    handle: gpui::AnyWindowHandle,
}

impl FilesPanel {
    pub(super) fn new(window: &mut Window, cx: &mut Context<KeelWindow>) -> Self {
        Self {
            open: false,
            anim: None,
            tab: FilesTab::Changes,
            change_detail_tab: ChangeDetailTab::Diff,
            chat: None,
            changes: None,
            refreshing: false,
            refresh_again: false,
            dirs: HashMap::new(),
            expanded: HashSet::new(),
            tree_cache: None,
            tree_fold: None,
            dev_tip: None,
            selected: None,
            stack: stack::Stack::new(),
            viewer: viewer::Viewer::new(),
            file: None,
            opening: None,
            focus: cx.focus_handle(),
            handle: window.window_handle(),
        }
    }

    /// The open file's editor (restyled when the theme changes).
    pub(super) fn editor(&self) -> Option<Entity<EditorState>> {
        self.file.as_ref().map(|f| f.editor.clone())
    }

    /// Edits not saved yet.
    pub(super) fn dirty(&self) -> bool {
        self.file.as_ref().is_some_and(|f| f.dirty)
    }

    fn change(&self, path: &str) -> Option<&Change> {
        match &self.changes {
            Some(Load::Ready(list)) => list.iter().find(|c| c.path == path),
            _ => None,
        }
    }
}

/// A tree row: path, name, depth, is a folder, open, git status.
#[derive(Clone)]
struct TreeRow {
    path: String,
    name: String,
    depth: usize,
    dir: bool,
    open: bool,
    status: Option<char>,
    /// "Empty", under an open folder with nothing in it.
    placeholder: bool,
    /// Set on a project folder's own row: its index, and the folder's real name.
    root_ix: Option<usize>,
    root_name: Option<String>,
}

/// A unified diff → lines with their old and new line numbers. File headers
/// (`diff --git`, `index`, `---`, `+++`) are dropped; hunks keep their header.
fn parse_patch(patch: &str) -> (Vec<DiffLine>, usize) {
    let (mut lines, mut widest, mut widest_len) = (Vec::new(), 0, 0);
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    // The new-side line after the last hunk: what lies between is hidden.
    let mut shown_to = 1u32;
    for raw in patch.lines() {
        let (kind, text) = if let Some(rest) = raw.strip_prefix("@@") {
            // `@@ -a,b +c,d @@ context`
            let mut nums = rest.split_whitespace();
            let start = |s: Option<&str>| {
                s.and_then(|s| s[1..].split(',').next()?.parse::<u32>().ok())
                    .unwrap_or(1)
            };
            if in_hunk {
                shown_to = new;
            }
            old = start(nums.next());
            new = start(nums.next());
            in_hunk = true;
            (LineKind::Hunk, raw)
        } else if !in_hunk {
            continue;
        } else if let Some(text) = raw.strip_prefix('+') {
            (LineKind::Added, text)
        } else if let Some(text) = raw.strip_prefix('-') {
            (LineKind::Removed, text)
        } else if let Some(text) = raw.strip_prefix(' ') {
            (LineKind::Context, text)
        } else if raw.starts_with('\\') {
            (LineKind::Note, raw)
        } else if raw.starts_with("diff --git") {
            in_hunk = false;
            shown_to = 1;
            continue;
        } else {
            (LineKind::Context, raw)
        };
        let (o, n) = match kind {
            LineKind::Added => (None, Some(new)),
            LineKind::Removed => (Some(old), None),
            LineKind::Context => (Some(old), Some(new)),
            // A hunk carries where it starts on each side.
            LineKind::Hunk => (Some(old), Some(new)),
            _ => (None, None),
        };
        let gap = if kind == LineKind::Hunk {
            new.saturating_sub(shown_to)
        } else {
            0
        };
        match kind {
            LineKind::Added => new += 1,
            LineKind::Removed => old += 1,
            LineKind::Context => {
                old += 1;
                new += 1;
            }
            _ => {}
        }
        // Tabs as four spaces, so the monospace columns line up.
        let text: SharedString = text.replace('\t', "    ").into();
        if text.len() > widest_len {
            (widest, widest_len) = (lines.len(), text.len());
        }
        lines.push(DiffLine {
            kind,
            old: o,
            new: n,
            text,
            spans: Vec::new(),
            emph: None,
            gap,
        });
    }
    mark_inline(&mut lines);
    (lines, widest)
}

/// Pair each run of removed lines with the added run after it, line by
/// line, and mark the stretch that differs, so a one-word edit reads as one
/// word, not two whole lines.
fn mark_inline(lines: &mut [DiffLine]) {
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != LineKind::Removed {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && lines[i].kind == LineKind::Removed {
            i += 1;
        }
        let mid = i;
        while i < lines.len() && lines[i].kind == LineKind::Added {
            i += 1;
        }
        for k in 0..(mid - start).min(i - mid) {
            if let Some((o, n)) = inline_change(&lines[start + k].text, &lines[mid + k].text) {
                lines[start + k].emph = Some(o);
                lines[mid + k].emph = Some(n);
            }
        }
    }
}

/// What differs between two versions of a line, as a range in each: the
/// middle left once the shared start and end are taken off, widened to whole
/// words. None when under half the line survived (a rewrite, not an edit).
fn inline_change(old: &str, new: &str) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let (o, n) = (old.as_bytes(), new.as_bytes());
    let mut pre = o.iter().zip(n).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(pre) {
        pre -= 1;
    }
    let room = o.len().min(n.len()) - pre;
    let mut suf = o
        .iter()
        .rev()
        .zip(n.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(o.len() - suf) {
        suf -= 1;
    }
    if pre + suf == o.len().max(n.len()) || (pre + suf) * 2 < o.len().max(n.len()) {
        return None;
    }
    // Shared bytes, so widening by them keeps both sides on char boundaries.
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while pre > 0 && word(o[pre - 1]) {
        pre -= 1;
    }
    while suf > 0 && word(o[o.len() - suf]) {
        suf -= 1;
    }
    Some((pre..o.len() - suf, pre..n.len() - suf))
}

/// Show the unchanged lines hidden above the hunk at `ix`, from the file as
/// it is now: they become context rows, and the hunk band goes.
fn expand_gap(lines: &[DiffLine], ix: usize, file: &str) -> Vec<DiffLine> {
    let hunk = &lines[ix];
    let (Some(old_start), Some(new_start)) = (hunk.old, hunk.new) else {
        return lines.to_vec();
    };
    let from = new_start - hunk.gap;
    let shift = old_start as i64 - new_start as i64;
    let mut out = lines[..ix].to_vec();
    for (n, text) in (from..new_start).zip(file.lines().skip(from.saturating_sub(1) as usize)) {
        let text: SharedString = text.replace('\t', "    ").into();
        out.push(DiffLine {
            kind: LineKind::Context,
            old: Some((n as i64 + shift) as u32),
            new: Some(n),
            text,
            spans: Vec::new(),
            emph: None,
            gap: 0,
        });
    }
    out.extend_from_slice(&lines[ix + 1..]);
    out
}

/// Syntax colours and the in-line change wash as one run list (gpui wants
/// runs that don't overlap): cut at every edge, then style each piece.
fn paint_runs(
    line: &DiffLine,
    syntax: &keel_theme::SyntaxPaintMap,
    emph_bg: gpui::Hsla,
) -> Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> {
    let emph = line.emph.clone().filter(|r| !r.is_empty());
    let mut cuts: Vec<usize> = line
        .spans
        .iter()
        .flat_map(|(r, _)| [r.start, r.end])
        .chain(emph.iter().flat_map(|r| [r.start, r.end]))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .filter_map(|w| {
            let piece = w[0]..w[1];
            let kind = line
                .spans
                .iter()
                .find(|(r, _)| r.start <= piece.start && piece.end <= r.end)
                .map(|(_, k)| *k);
            let hot = emph
                .as_ref()
                .is_some_and(|r| r.start <= piece.start && piece.end <= r.end);
            if kind.is_none() && !hot {
                return None;
            }
            let color = kind.map(|k| match k {
                SyntaxKind::Keyword => syntax.keyword,
                SyntaxKind::String => syntax.string,
                SyntaxKind::Comment => syntax.comment,
                SyntaxKind::Number => syntax.number,
                SyntaxKind::Function => syntax.function,
                SyntaxKind::Type => syntax.type_name,
            });
            Some((
                piece,
                gpui::HighlightStyle {
                    color: color.map(|v| c(v << 8 | 0xFF)),
                    font_style: (kind == Some(SyntaxKind::Comment))
                        .then_some(gpui::FontStyle::Italic),
                    background_color: hot.then_some(emph_bg),
                    ..Default::default()
                },
            ))
        })
        .collect()
}

/// Color the code by the file's extension. Each side highlights as the file
/// it was (context + removed) or is (context + added), so a string opened on
/// one side doesn't bleed into the other; context lines keep the new side.
fn highlight_diff(lines: &mut [DiffLine], path: &str) {
    let ext = path.rsplit_once('.').map(|(_, e)| e);
    for side in [LineKind::Removed, LineKind::Added] {
        let rows: Vec<usize> = (0..lines.len())
            .filter(|&i| matches!(lines[i].kind, LineKind::Context) || lines[i].kind == side)
            .collect();
        let (mut code, mut starts) = (String::new(), Vec::with_capacity(rows.len()));
        for &i in &rows {
            starts.push(code.len());
            code.push_str(&lines[i].text);
            code.push('\n');
        }
        let spans = crate::syntax::highlight(&code, ext);
        let mut first = 0;
        for (k, &i) in rows.iter().enumerate() {
            let (a, b) = (starts[k], starts[k] + lines[i].text.len());
            while first < spans.len() && spans[first].0.end <= a {
                first += 1;
            }
            // A run can span lines (block comments): cut it to this one.
            lines[i].spans = spans[first..]
                .iter()
                .take_while(|(r, _)| r.start < b)
                .map(|(r, k)| (r.start.max(a) - a..r.end.min(b) - a, *k))
                .filter(|(r, _)| !r.is_empty())
                .collect();
        }
    }
}

impl KeelWindow {
    /// ⌘⇧E and the header button: slide the pane in or out. It shares the
    /// browser's slot, so opening one closes the other.
    pub(super) fn toggle_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.browser_allowed() {
            return;
        }
        if !self.files.open && self.browser.open {
            self.toggle_browser(window, cx);
        }
        if !self.files.open {
            self.set_terminal_open(false, window, cx);
        }
        self.update_right_panel_anim();
        let f = &mut self.files;
        f.open = !f.open;
        let n = f.anim.map_or(1, |(_, n)| n + 1);
        f.anim = Some((Instant::now(), n));
        if f.open {
            window.focus(&f.focus, cx);
            self.files_sync_chat(cx);
            self.files_refresh(cx);
        } else {
            self.composer.update(cx, |s, cx| s.focus(window, cx));
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

    /// Open a changed file's diff in the right-side files panel: its section
    /// in the stack, opened and brought to the top.
    pub(super) fn show_file_diff(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self.projects.map_path(
            self.current_chat.as_deref().or(self.files.chat.as_deref()),
            &path,
        );
        let rel = if path.starts_with('@') {
            path
        } else if let Some(root) = self.mention_root() {
            std::path::Path::new(&path)
                .strip_prefix(&root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(path)
        } else {
            path
        };

        if !self.files.open {
            if self.browser.open {
                self.toggle_browser(window, cx);
            }
            self.set_terminal_open(false, window, cx);
            self.update_right_panel_anim();
            let f = &mut self.files;
            f.open = true;
            let n = f.anim.map_or(1, |(_, n)| n + 1);
            f.anim = Some((Instant::now(), n));
            window.focus(&f.focus, cx);
            self.files_sync_chat(cx);
            self.files_refresh(cx);
            let settle = self.right_panel_duration();
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(settle + Duration::from_millis(20))
                    .await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        } else {
            self.files_sync_chat(cx);
            self.files_refresh(cx);
        }

        self.files_goto_change(rel, cx);
    }

    /// Open a file in the right-side files panel editor.
    pub(super) fn show_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.projects.map_path(
            self.current_chat.as_deref().or(self.files.chat.as_deref()),
            &path,
        );
        let rel = if path.starts_with('@') {
            path
        } else if let Some(root) = self.mention_root() {
            std::path::Path::new(&path)
                .strip_prefix(&root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(path)
        } else {
            path
        };
        let rel = rel
            .trim_start_matches("./")
            .trim_start_matches('/')
            .to_string();

        if self.files.dirty() && self.files.file.as_ref().is_some_and(|f| f.path != rel) {
            if let Some(file) = &mut self.files.file {
                file.error = Some("Save or discard your edits first.".into());
            }
            cx.notify();
            return;
        }

        if !self.files.open {
            if self.browser.open {
                self.toggle_browser(window, cx);
            }
            self.set_terminal_open(false, window, cx);
            self.update_right_panel_anim();
            let f = &mut self.files;
            f.open = true;
            let n = f.anim.map_or(1, |(_, n)| n + 1);
            f.anim = Some((Instant::now(), n));
            window.focus(&f.focus, cx);
            self.files_sync_chat(cx);
            self.files_refresh(cx);
            let settle = self.right_panel_duration();
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(settle + Duration::from_millis(20))
                    .await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        } else {
            self.files_sync_chat(cx);
            self.files_refresh(cx);
        }

        let mut dir_accum = String::new();
        let parts: Vec<&str> = rel.split('/').collect();
        if parts.len() > 1 {
            for comp in &parts[..parts.len() - 1] {
                if !dir_accum.is_empty() {
                    dir_accum.push('/');
                }
                dir_accum.push_str(comp);
                self.files.expanded.insert(dir_accum.clone());
            }
        }

        self.files.tab = FilesTab::Files;
        self.files_select(rel, window, cx);
        cx.notify();
    }

    /// The open chat's id, when its files can be shown.
    fn files_chat(&self) -> Option<String> {
        self.current_chat.clone()
    }

    /// Another chat is open: drop what belonged to the last one. Unsaved
    /// edits stay until saved or discarded (the pane says so).
    fn files_sync_chat(&mut self, cx: &mut Context<Self>) {
        let chat = self.files_chat();
        let f = &mut self.files;
        if f.chat == chat || f.dirty() {
            return;
        }
        f.chat = chat;
        f.changes = None;
        f.dirs.clear();
        f.expanded.clear();
        f.tree_cache = None;
        f.selected = None;
        f.stack.clear();
        f.viewer.clear();
        f.file = None;
        f.opening = None;
        cx.notify();
    }

    /// Fetch the changed files again (one fetch at a time; a request during
    /// one runs once it lands), and with them the open diff, and the open
    /// file when it has no edits.
    pub(super) fn files_refresh(&mut self, cx: &mut Context<Self>) {
        if !self.files.open {
            return;
        }
        self.files_sync_chat(cx);
        let (Some(chat), Some(client)) = (self.files.chat.clone(), self.client.clone()) else {
            return;
        };
        if self.files.refreshing {
            self.files.refresh_again = true;
            return;
        }
        self.files.refreshing = true;
        if self.files.changes.is_none() {
            self.files.changes = Some(Load::Loading);
        }
        let roots = self.projects.file_roots(Some(chat.as_str()));
        let queries: Vec<String> = if roots.is_empty() {
            vec![String::new()]
        } else {
            roots.iter().map(|(alias, _)| format!("@{alias}")).collect()
        };
        if self.files.tab == FilesTab::Files {
            if roots.is_empty() {
                if !self.files.dirs.contains_key("") {
                    self.files_load_dir(String::new(), cx);
                }
            } else {
                for root in &queries {
                    if !self.files.dirs.contains_key(root) {
                        // Open each folder once, so the tree arrives divided.
                        // A folder the user has closed stays closed.
                        self.files.expanded.insert(root.clone());
                        self.files_load_dir(root.clone(), cx);
                    }
                }
            }
        }
        cx.spawn(async move |this, cx| {
            let mut merged: Vec<Change> = Vec::new();
            let mut failed: Option<String> = None;
            let mut any_ok = false;
            for root in &queries {
                let res = client
                    .call(
                        "chat_changes",
                        serde_json::json!({ "chat_id": chat, "path": root }),
                    )
                    .await;
                match res.and_then(|v| {
                    serde_json::from_value::<Vec<Change>>(v["changes"].clone())
                        .map_err(|e| e.to_string())
                }) {
                    Ok(list) => {
                        any_ok = true;
                        for mut change in list {
                            if !root.is_empty() {
                                change.path = format!("{root}/{}", change.path);
                            }
                            merged.push(change);
                        }
                    }
                    Err(err) => failed = Some(err),
                }
            }
            let _ = this.update(cx, |this, cx| {
                let (old, show_files, chat_now) = {
                    let f = &mut this.files;
                    f.refreshing = false;
                    if f.chat.as_deref() != Some(chat.as_str()) {
                        return;
                    }
                    let old = match &f.changes {
                        Some(Load::Ready(list)) => Some(list.clone()),
                        _ => None,
                    };
                    f.changes = Some(if any_ok {
                        Load::Ready(Rc::new(merged))
                    } else {
                        Load::Failed(failed.unwrap_or_else(|| "Couldn't read changes".into()))
                    });
                    f.tree_cache = None;
                    // A folder that isn't a repo has no changes to show.
                    let show_files =
                        matches!(f.changes, Some(Load::Failed(_))) && f.tab == FilesTab::Changes;
                    if show_files {
                        f.tab = FilesTab::Files;
                    }
                    (old, show_files, f.chat.clone())
                };
                if show_files {
                    let roots = this.projects.file_roots(chat_now.as_deref());
                    if roots.is_empty() {
                        if !this.files.dirs.contains_key("") {
                            this.files_load_dir(String::new(), cx);
                        }
                    } else {
                        for (alias, _) in roots {
                            let key = format!("@{alias}");
                            if !this.files.dirs.contains_key(&key) {
                                this.files.expanded.insert(key.clone());
                                this.files_load_dir(key, cx);
                            }
                        }
                    }
                }
                this.files_reconcile_docs(old, cx);
                if let Some(file) = &this.files.file
                    && !file.dirty
                    && !file.saving
                {
                    let path = file.path.clone();
                    this.files_reload_file(path, cx);
                }
                if std::mem::take(&mut this.files.refresh_again) {
                    this.files_refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The engine saw the checkout change, or a turn ended: refresh if the
    /// pane shows that chat.
    pub(super) fn files_on_checkout_changed(&mut self, checkout_id: &str, cx: &mut Context<Self>) {
        let ours = self
            .files
            .chat
            .as_ref()
            .and_then(|id| self.state.chats.get(id))
            .is_some_and(|c| c.checkout_id == checkout_id);
        if self.files.open && ours {
            self.files_refresh(cx);
        }
    }

    pub(super) fn files_on_turn_end(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        if self.files.open && self.files.chat.as_deref() == Some(chat_id) {
            self.files_refresh(cx);
        }
    }

    fn files_load_dir(&mut self, rel: String, cx: &mut Context<Self>) {
        let (Some(chat), Some(client)) = (self.files.chat.clone(), self.client.clone()) else {
            return;
        };
        self.files.dirs.insert(rel.clone(), Load::Loading);
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "chat_list_dir",
                    serde_json::json!({ "chat_id": chat, "path": rel }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.files.chat.as_deref() != Some(chat.as_str()) {
                    return;
                }
                let load = match res.and_then(|v| {
                    serde_json::from_value::<Vec<Entry>>(v["entries"].clone())
                        .map_err(|e| e.to_string())
                }) {
                    Ok(list) => Load::Ready(list),
                    Err(err) => Load::Failed(err),
                };
                let opening = this.files.tree_fold.as_ref().is_some_and(|m| m.key == rel);
                this.files.dirs.insert(rel, load);
                this.files.tree_cache = None;
                // The folder was opening before its rows came: they slide in now.
                if opening {
                    let rows = this.tree_rows();
                    if let Some(m) = &mut this.files.tree_fold {
                        m.arrived(rows);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn files_toggle_dir(&mut self, rel: String, cx: &mut Context<Self>) {
        let reduced = self.reduced();
        let now = self.frame_now.get().unwrap_or_else(Instant::now);
        let old = self.tree_rows();
        self.files.tree_cache = None;
        if !self.files.expanded.remove(&rel) {
            self.files.expanded.insert(rel.clone());
            if !matches!(
                self.files.dirs.get(&rel),
                Some(Load::Ready(_) | Load::Loading)
            ) {
                self.files_load_dir(rel.clone(), cx);
            }
        }
        let new = self.tree_rows();
        let at = old.iter().position(|r| r.dir && r.path == rel);
        self.files.tree_fold = fold::TreeFold::toggled(
            self.files.tree_fold.take(),
            &rel,
            at,
            old,
            new,
            now,
            reduced,
        );
        cx.notify();
    }

    /// Show a file in the viewer: an image as itself, anything else in the
    /// editor (Markdown can switch to its rendered preview).
    fn files_select(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.files.dirty() && self.files.file.as_ref().is_some_and(|f| f.path != path) {
            // Unsaved edits elsewhere: stay on them (the header offers save/discard).
            if let Some(file) = &mut self.files.file {
                file.error = Some("Save or discard your edits first.".into());
            }
            cx.notify();
            return;
        }
        self.files.selected = Some(path.clone());
        self.files.viewer.actual = false;
        // Picking a file ends a narrow-panel reveal: show the file.
        self.files.viewer.reveal = None;
        if viewer::is_image(&path) {
            self.files_locate(path, cx);
        } else {
            self.files_open_file(path, window, cx);
        }
        cx.notify();
    }

    /// Read `path` into the editor (a fresh one, so undo starts here).
    fn files_open_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.files.file.as_ref().is_some_and(|f| f.path == path) {
            return;
        }
        let (Some(chat), Some(client)) = (self.files.chat.clone(), self.client.clone()) else {
            return;
        };
        self.files.opening = Some(path.clone());
        self.files.viewer.open_error = None;
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "chat_read_file",
                    serde_json::json!({ "chat_id": chat, "path": path }),
                )
                .await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.files.opening.as_deref() != Some(path.as_str()) {
                        return;
                    }
                    this.files.opening = None;
                    match res {
                        Ok(v) => {
                            let content = v["content"].as_str().unwrap_or_default().to_string();
                            let base = v["hash"].as_str().unwrap_or_default().to_string();
                            this.files_install_editor(path, content, base, window, cx);
                        }
                        Err(err) => {
                            this.files.file = None;
                            this.files.viewer.open_error = Some((path, err));
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn files_install_editor(
        &mut self,
        path: String,
        content: String,
        base: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dark = matches!(
            window.appearance(),
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
        );
        let style = code_editor_style(
            self.state.effective_tokens(dark),
            keel_theme::resolve_syntax_theme(self.state.ui_settings.theme, dark),
        );
        let text = content.clone();
        // The extension names the grammar (`rs`, `json`, `tsx`…); none, plain.
        let lang = path.rsplit_once('.').map_or("", |(_, e)| e).to_string();
        let editor = cx.new(|cx| {
            let mut s = EditorState::new(window, cx)
                .line_number(true)
                .soft_wrap(false)
                .folding(false)
                .language(lang);
            crate::syntax::EditorHighlighter::attach(&mut s, cx);
            s.set_editor_style(style);
            s.set_value(text, window, cx);
            s
        });
        let sub = cx.subscribe(&editor, |this, editor, event: &InputEvent, cx| {
            if let InputEvent::Change = event
                && let Some(file) = &mut this.files.file
                && file.editor == editor
            {
                // One compare per edit, not per frame.
                file.dirty = editor.read(cx).value().as_ref() != file.saved.as_str();
                file.error = None;
                cx.notify();
            }
        });
        self.files.file = Some(OpenFile {
            path,
            editor,
            base,
            saved: content,
            dirty: false,
            saving: false,
            conflict: false,
            error: None,
            saved_at: None,
            _changes: sub,
        });
    }

    /// The open file changed on disk and has no edits: load it again in place.
    fn files_reload_file(&mut self, path: String, cx: &mut Context<Self>) {
        let (Some(chat), Some(client)) = (self.files.chat.clone(), self.client.clone()) else {
            return;
        };
        let handle = self.files.handle;
        cx.spawn(async move |this, cx| {
            let Ok(v) = client
                .call(
                    "chat_read_file",
                    serde_json::json!({ "chat_id": chat, "path": path }),
                )
                .await
            else {
                return;
            };
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    let Some(file) = this
                        .files
                        .file
                        .as_mut()
                        .filter(|f| f.path == path && (!f.dirty || f.conflict))
                    else {
                        return;
                    };
                    let content = v["content"].as_str().unwrap_or_default().to_string();
                    file.base = v["hash"].as_str().unwrap_or_default().to_string();
                    if content != file.saved || file.conflict {
                        file.saved = content.clone();
                        file.editor
                            .update(cx, |s, cx| s.set_value(content, window, cx));
                    }
                    file.dirty = false;
                    file.conflict = false;
                    file.error = None;
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// ⌘S in the pane and the Save button. `force` writes over a file that
    /// changed on disk (after the user said so).
    pub(super) fn files_save(&mut self, force: bool, cx: &mut Context<Self>) {
        let (Some(chat), Some(client)) = (self.files.chat.clone(), self.client.clone()) else {
            return;
        };
        let Some(file) = self
            .files
            .file
            .as_mut()
            .filter(|f| (f.dirty || force) && !f.saving)
        else {
            return;
        };
        file.saving = true;
        file.error = None;
        let content = file.editor.read(cx).value().to_string();
        let (path, base) = (file.path.clone(), file.base.clone());
        cx.spawn(async move |this, cx| {
            // Overwriting: write over whatever is there now.
            let base = if force {
                match client.call("chat_read_file", serde_json::json!({ "chat_id": chat, "path": path })).await {
                    Ok(v) => v["hash"].as_str().unwrap_or_default().to_string(),
                    Err(_) => base,
                }
            } else {
                base
            };
            let res = client
                .call("chat_write_file", serde_json::json!({ "chat_id": chat, "path": path, "content": content, "base": base }))
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(file) = this.files.file.as_mut().filter(|f| f.path == path) else {
                    return;
                };
                file.saving = false;
                match res {
                    Ok(v) => {
                        file.base = v["hash"].as_str().unwrap_or_default().to_string();
                        file.dirty = file.editor.read(cx).value().as_ref() != content.as_str();
                        file.saved = content;
                        file.conflict = false;
                        file.saved_at = Some(Instant::now());
                        // Take "Saved" down once it has been seen.
                        cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(SAVED_FOR).await;
                            let _ = this.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                    }
                    Err(err) if err == keel_proto::FILE_CHANGED => file.conflict = true,
                    Err(err) => file.error = Some(err),
                }
                this.files_refresh(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Throw the edits away: the file as it is on disk now.
    fn files_discard(&mut self, cx: &mut Context<Self>) {
        if let Some(file) = &mut self.files.file {
            file.dirty = false;
            file.conflict = true; // reload even though nothing differs from `saved`
            let path = file.path.clone();
            self.files_reload_file(path, cx);
        }
    }

    /// Spacer beside the chat plus the pane over the window's right edge,
    /// like the browser pane (and the same width and drag edge).
    pub(super) fn render_files_panel(
        &mut self,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let allowed = self.browser_allowed();
        let open = self.files.open && allowed;
        let toggling = !self.reduced()
            && self
                .files
                .anim
                .is_some_and(|(at, _)| at.elapsed() < self.right_panel_duration());
        if !open && !toggling {
            return Vec::new();
        }
        if open {
            self.files_sync_chat(cx);
        }
        let full = self.browser_width(window);
        let body = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(t.window))
            .border_l_1()
            .border_color(c(t.border))
            .child(self.render_files_toolbar(t, cx))
            .child(self.render_files_body(full, t, window, cx));
        let pane = div()
            .id("files-panel")
            .track_focus(&self.files.focus)
            .key_context(crate::keys::FILES)
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(full))
            .child(body)
            .child(self.render_browser_edge(t, cx))
            // Dev only (`files_tip`): a tooltip shown without a hover.
            .children(
                self.files
                    .dev_tip
                    .clone()
                    .map(|tip| div().absolute().top(zpx(52.)).left(zpx(16.)).child(tip)),
            );

        let anim = self.files.anim.filter(|_| toggling);
        if let Some((_, n)) = anim {
            use gpui::AnimationExt as _;
            let (from, to) = (self.right_panel_from, self.right_panel_target());
            let entering = to >= from;
            let curve = gpui::Animation::new(self.right_panel_duration())
                .with_easing(crate::motion::glide_curve);
            let pane = pane
                .with_animation(
                    ("files-slide", n as usize),
                    curve.clone(),
                    move |el, delta| {
                        let k = from + (to - from) * delta;
                        el.right(px(-full * (1.0 - k)))
                            .opacity(crate::motion::panel_opacity(k, entering))
                    },
                )
                .into_any_element();
            // Full width at once. Animating it resizes the chat every frame,
            // which is the jump; the pane sliding over it is the motion.
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

    fn render_files_toolbar(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let reduced = self.reduced();
        let f = &self.files;
        let badge = match &f.changes {
            Some(Load::Ready(list)) if !list.is_empty() => Some(list.len()),
            _ => None,
        };
        let refresh = if f.refreshing {
            // Spinning while it fetches: the click registered and work is on.
            div()
                .size(zpx(28.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(c(t.text_muted))
                .child(crate::motion::spin(
                    Icon::Reload.el(zpx(14.)),
                    "files-refresh-spin",
                    reduced,
                ))
                .into_any_element()
        } else {
            icon_button("files-refresh", Icon::Reload, t)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    // A manual refresh re-reads open folders too.
                    let open: Vec<String> = this.files.dirs.keys().cloned().collect();
                    for rel in open {
                        this.files_load_dir(rel, cx);
                    }
                    // And every diff already loaded.
                    this.files.stack.reload_all = true;
                    this.files_refresh(cx);
                }))
                .into_any_element()
        };
        div()
            .h(HEADER_HEIGHT)
            .flex_none()
            .pl_2()
            .pr_1p5()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(
                seg_track(t)
                    .child(
                        seg_item(
                            "files-tab-changes",
                            "Changes",
                            badge,
                            f.tab == FilesTab::Changes,
                            t,
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.files.tab = FilesTab::Changes;
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        seg_item(
                            "files-tab-files",
                            "Files",
                            None,
                            f.tab == FilesTab::Files,
                            t,
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.files.tab = FilesTab::Files;
                                this.files_refresh(cx);
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(div().flex_1())
            .child(refresh)
            .child(icon_button("files-close", Icon::SidebarClose, t).on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_files(window, cx)),
            ))
    }

    fn render_files_body(
        &mut self,
        width: f32,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.files.chat.is_none() {
            return div()
                .flex_1()
                .border_t_1()
                .border_color(c(t.border))
                .child(empty_state(
                    Icon::Folder,
                    "No chat open",
                    "Open a chat to browse its folder and review what the agent changed.",
                    t,
                ))
                .into_any_element();
        }
        let inner = match self.files.tab {
            FilesTab::Changes => self.render_changes(width, t, window, cx),
            FilesTab::Files => self.render_files_tab(width, t, window, cx),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .border_t_1()
            .border_color(c(t.border))
            .child(inner)
            .into_any_element()
    }

    /// The folder tree beside the viewer; when narrow, one or the other.
    fn render_files_tab(
        &mut self,
        width: f32,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stacked = width < STACK_BELOW;
        self.files.viewer.narrow = stacked;
        let showing =
            self.files.selected.is_some() && !(stacked && self.files.viewer.reveal.is_some());
        let list = (!stacked || !showing).then(|| {
            div()
                .flex_none()
                .when(stacked, |d| d.flex_1())
                .when(!stacked, |d| {
                    d.w(zpx(LIST_W)).border_r_1().border_color(c(t.border))
                })
                .h_full()
                .flex()
                .flex_col()
                .child(self.render_tree(t, window, cx))
        });
        let content_w = if stacked {
            width
        } else {
            width - f32::from(zpx(LIST_W))
        };
        let content = (!stacked || showing)
            .then(|| self.render_files_content(content_w, stacked, t, window, cx));
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_row()
            .children(list)
            .children(content)
            .into_any_element()
    }

    /// Placeholder rows while a list loads, shaped like the rows to come.
    fn render_list_skeleton(&self, prefix: &'static str, t: Tokens) -> AnyElement {
        let reduced = self.reduced();
        div()
            .py_1p5()
            .flex()
            .flex_col()
            .children((0..7usize).map(|i| {
                let phase = i as f32 * 0.07;
                div()
                    .h(zpx(ROW_H))
                    .mx_1p5()
                    .px_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(crate::motion::shimmer(
                        div()
                            .size(zpx(13.))
                            .rounded(px(3.))
                            .bg(c(t.text).opacity(0.06)),
                        (prefix, i * 3),
                        phase,
                        c(t.text).opacity(0.05),
                        reduced,
                    ))
                    .child(crate::motion::shimmer(
                        div()
                            .h(zpx(8.))
                            .w(zpx(64. + 22. * (i % 3) as f32))
                            .rounded(px(3.))
                            .bg(c(t.text).opacity(0.06)),
                        (prefix, i * 3 + 1),
                        phase,
                        c(t.text).opacity(0.05),
                        reduced,
                    ))
                    .child(div().flex_1())
                    .child(crate::motion::shimmer(
                        div()
                            .h(zpx(8.))
                            .w(zpx(22.))
                            .rounded(px(3.))
                            .bg(c(t.text).opacity(0.05)),
                        (prefix, i * 3 + 2),
                        phase,
                        c(t.text).opacity(0.05),
                        reduced,
                    ))
            }))
            .into_any_element()
    }

    /// Open folders flattened to rows, depth first, cached until a folder
    /// loads or opens or the changes land.
    fn tree_rows(&mut self) -> Rc<Vec<TreeRow>> {
        if let Some(rows) = &self.files.tree_cache {
            return rows.clone();
        }
        let roots = self.projects.file_roots(self.files.chat.as_deref());
        let f = &self.files;
        // Project changes already use `@alias/…`, the same paths the tree uses.
        let changed: HashMap<String, char> = match &f.changes {
            Some(Load::Ready(list)) => list.iter().map(|c| (c.path.clone(), c.status)).collect(),
            _ => HashMap::new(),
        };
        fn walk(
            f: &FilesPanel,
            changed: &HashMap<String, char>,
            rel: &str,
            depth: usize,
            rows: &mut Vec<TreeRow>,
        ) {
            let Some(Load::Ready(entries)) = f.dirs.get(rel) else {
                return;
            };
            for e in entries {
                let path = if rel.is_empty() {
                    e.name.clone()
                } else {
                    format!("{rel}/{}", e.name)
                };
                let open = e.dir && f.expanded.contains(&path);
                // A folder holding changes says so, so they can be found.
                let status = if e.dir {
                    let prefix = format!("{path}/");
                    changed
                        .keys()
                        .any(|p| p.starts_with(&prefix))
                        .then_some('•')
                } else {
                    changed.get(&path).copied()
                };
                rows.push(TreeRow {
                    path: path.clone(),
                    name: e.name.clone(),
                    depth,
                    dir: e.dir,
                    open,
                    status,
                    placeholder: false,
                    root_ix: None,
                    root_name: None,
                });
                if open {
                    // Listed and empty: a placeholder, so opening it shows something
                    // (and it folds like any child). Still loading: nothing yet.
                    if matches!(f.dirs.get(&path), Some(Load::Ready(inner)) if inner.is_empty()) {
                        let path = format!("{path}/\0empty");
                        rows.push(TreeRow {
                            path,
                            name: "Empty".into(),
                            depth: depth + 1,
                            dir: false,
                            open: false,
                            status: None,
                            placeholder: true,
                            root_ix: None,
                            root_name: None,
                        });
                    } else {
                        walk(f, changed, &path, depth + 1, rows);
                    }
                }
            }
        }
        let mut rows = Vec::new();
        if roots.is_empty() {
            walk(f, &changed, "", 0, &mut rows);
        } else {
            for (ix, (alias, folder)) in roots.iter().enumerate() {
                let key = format!("@{alias}");
                let open = f.expanded.contains(&key);
                let prefix = format!("{key}/");
                let status = changed
                    .keys()
                    .any(|p| p.starts_with(&prefix))
                    .then_some('•');
                rows.push(TreeRow {
                    path: key.clone(),
                    name: alias.clone(),
                    depth: 0,
                    dir: true,
                    open,
                    status,
                    placeholder: false,
                    root_ix: Some(ix),
                    root_name: Some(folder.clone()),
                });
                if open {
                    walk(f, &changed, &key, 1, &mut rows);
                }
            }
        }
        let rows = Rc::new(rows);
        self.files.tree_cache = Some(rows.clone());
        rows
    }

    /// The folder tree: no guide lines, a small indent per level, a dot on
    /// folders holding changes. A reveal scrolls its row to the middle and
    /// lights it briefly.
    fn render_tree(
        &mut self,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rooted = !self
            .projects
            .file_roots(self.files.chat.as_deref())
            .is_empty();
        if !rooted {
            match self.files.dirs.get("") {
                None | Some(Load::Loading) => {
                    return self.render_list_skeleton("files-tree-skel", t);
                }
                Some(Load::Failed(err)) => {
                    return empty_state(Icon::Alert, "Couldn't list files", err.clone(), t);
                }
                Some(Load::Ready(entries)) if entries.is_empty() => {
                    return empty_state(
                        Icon::Folder,
                        "Empty folder",
                        "There's nothing in this chat's folder yet.",
                        t,
                    );
                }
                Some(Load::Ready(_)) => {}
            }
        }
        let live = self.tree_rows();
        self.files_reveal_scroll(&live, cx);
        let now = self.frame_now.get().unwrap_or_else(Instant::now);
        if let Some(m) = &mut self.files.tree_fold {
            if m.motion.done(now) {
                self.files.tree_fold = None;
            } else {
                m.motion.start(now);
                window.request_animation_frame();
            }
        }
        // While a folder moves, the rows with it open, laid out slot by slot.
        let moving = self.files.tree_fold.as_ref().map(|m| {
            (
                m.key.clone(),
                m.motion.content(now),
                m.motion.chevron(now),
                m.start,
                m.children,
                m.rows.clone(),
                m.reveal(now, ROW_H),
            )
        });
        let rows = moving.as_ref().and_then(|m| m.5.clone()).unwrap_or(live);
        let glow = self.files.viewer.glow();
        let reduced = self.reduced();
        let selected = self.files.selected.clone();
        let weak = cx.entity().downgrade();
        uniform_list("files-tree", rows.len(), move |range, _, _| {
            let row_el = |ix: usize| -> AnyElement {
                let TreeRow {
                    path,
                    name,
                    depth,
                    dir,
                    open,
                    status,
                    placeholder,
                    root_ix,
                    root_name,
                } = rows[ix].clone();
                if let Some(root_ix) = root_ix {
                    let weak = weak.clone();
                    let folder = root_name.unwrap_or_default();
                    let turn = fold::chevron_turn(match &moving {
                        Some(m) if m.0 == path => m.2,
                        _ if open => 1.,
                        _ => 0.,
                    });
                    return div()
                        .id(("files-tree-row", ix))
                        .h(zpx(ROW_H))
                        .w_full()
                        .pr_2()
                        .pl(zpx(6.))
                        .mt(if ix > 0 { zpx(6.) } else { zpx(0.) })
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1p5()
                        .rounded(RADIUS_MD)
                        .t_small()
                        .cursor_pointer()
                        .hover_fade(c(t.hover), None)
                        .child(
                            div()
                                .flex_none()
                                .w(zpx(10.))
                                .text_color(c(t.text_faint))
                                .child(Icon::ChevronDown.el(zpx(10.)).with_transformation(
                                    gpui::Transformation::rotate(gpui::percentage(turn)),
                                )),
                        )
                        .child(
                            div()
                                .w(zpx(14.))
                                .flex_none()
                                .flex()
                                .justify_center()
                                .t_caption()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(c(match root_ix % 3 {
                                    0 => t.accent,
                                    1 => t.success,
                                    _ => t.warning,
                                }))
                                .child(
                                    name.chars()
                                        .next()
                                        .map(|c| c.to_uppercase().to_string())
                                        .unwrap_or_else(|| "?".into()),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(c(t.text))
                                .child(name.clone()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .t_caption()
                                .text_color(c(t.text_faint))
                                .child(folder),
                        )
                        .on_click(move |_, _, cx| {
                            let path = path.clone();
                            let _ = weak.update(cx, |this, cx| this.files_toggle_dir(path, cx));
                        })
                        .into_any_element();
                }
                if placeholder {
                    // The sidebar's empty-group look: small, faint, inert;
                    // the label sits where a child's name would.
                    return div()
                        .id(("files-tree-row", ix))
                        .h(zpx(ROW_H))
                        .w_full()
                        .pr_2()
                        .pl(zpx(6. + depth as f32 * 12. + 10. + 6. + 13. + 6.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .t_small()
                        .text_color(c(t.text_faint))
                        .child(div().min_w_0().truncate().child(name))
                        .into_any_element();
                }
                let on = selected.as_deref() == Some(path.as_str());
                let lit = glow.as_ref().filter(|(p, _)| *p == path).map(|(_, n)| *n);
                let weak = weak.clone();
                let icon = if dir {
                    Icon::Folder
                } else {
                    Icon::for_path(&name)
                };
                let dot = status.map(|s| match s {
                    'A' | '?' => t.success,
                    'D' => t.danger,
                    '•' => t.text_faint,
                    _ => t.warning,
                });
                div()
                    .id(("files-tree-row", ix))
                    .relative()
                    .h(zpx(ROW_H))
                    .w_full()
                    .pr_2()
                    .pl(zpx(6. + depth as f32 * 12.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .rounded(RADIUS_MD)
                    .t_small()
                    .cursor_pointer()
                    .map(|d| {
                        if on {
                            d.bg(c(t.selected))
                        } else {
                            d.hover_fade(c(t.hover), None)
                        }
                    })
                    .active(|s| s.bg(c(t.selected)))
                    .children(lit.map(|n| viewer::reveal_glow(n, t, reduced)))
                    .child(
                        div()
                            .flex_none()
                            .w(zpx(10.))
                            .text_color(c(t.text_faint))
                            .when(dir, |d| {
                                let turn = fold::chevron_turn(match &moving {
                                    Some(m) if m.0 == path => m.2,
                                    _ if open => 1.,
                                    _ => 0.,
                                });
                                d.child(Icon::ChevronDown.el(zpx(10.)).with_transformation(
                                    gpui::Transformation::rotate(gpui::percentage(turn)),
                                ))
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(c(if dir { t.text_muted } else { t.text_faint }))
                            .child(icon.el(zpx(13.))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(c(if dir || on || lit.is_some() {
                                t.text
                            } else {
                                t.text_muted
                            }))
                            .when(on, |d| d.font_weight(gpui::FontWeight::MEDIUM))
                            .child(name),
                    )
                    .children(
                        dot.map(|col| div().flex_none().size(zpx(6.)).rounded_full().bg(c(col))),
                    )
                    .on_click(move |_, window, cx| {
                        let path = path.clone();
                        let _ = weak.update(cx, |this, cx| {
                            if dir {
                                this.files_toggle_dir(path, cx);
                            } else {
                                this.files.viewer.reveal = None;
                                this.files_select(path, window, cx);
                            }
                        });
                    })
                    .into_any_element()
            };
            range
                .map(|ix| match &moving {
                    Some((_, open, _, start, children, Some(_), reveal)) => fold::slot(
                        ix,
                        fold::slot_rows(ix, rows.len(), *start, *children, *reveal, ROW_H),
                        *start..*start + *children,
                        *open,
                        ROW_H,
                        &row_el,
                    ),
                    _ => row_el(ix),
                })
                .collect()
        })
        .track_scroll(&self.files.viewer.tree_scroll)
        .p_1p5()
        .flex_1()
        .into_any_element()
    }

    /// The selected file: a header (name, folder, a way to its diff when it
    /// changed, Preview / Source for Markdown, save state) over the viewer.
    fn render_files_content(
        &mut self,
        width: f32,
        stacked: bool,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(path) = self.files.selected.clone() else {
            return div()
                .flex_1()
                .flex()
                .child(empty_state(Icon::FileText, "Nothing selected", "Pick a file to read or edit it. Markdown renders and images show as themselves.", t))
                .into_any_element();
        };
        let reduced = self.reduced();
        let change = self.files.change(&path).cloned();
        let kind = crate::files::FileKind::of(std::path::Path::new(&path));
        let markdown = kind == crate::files::FileKind::Markdown;
        let file = self.files.file.as_ref().filter(|f| f.path == path);
        let loaded = file.is_some();
        let (dirty, saving, conflict, error) = file.map_or((false, false, false, None), |f| {
            (f.dirty, f.saving, f.conflict, f.error.clone())
        });
        let just_saved = file
            .and_then(|f| f.saved_at)
            .filter(|at| at.elapsed() < SAVED_FOR)
            .is_some();
        let preview = markdown && loaded && !self.files.viewer.md_source;
        let (dir, name) = path
            .rsplit_once('/')
            .map_or(("", path.as_str()), |(d, n)| (d, n));
        let header =
            div()
                .h(HEADER_HEIGHT)
                .flex_none()
                .pl(zpx(if stacked { 6. } else { 12. }))
                .pr_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(c(t.border))
                .when(stacked, |d| {
                    d.child(
                        icon_button("files-back", Icon::Back, t).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.files.selected = None;
                                cx.notify();
                            },
                        )),
                    )
                })
                .child(
                    div()
                        .flex_none()
                        .text_color(c(t.text_faint))
                        .child(Icon::for_path(name).el(zpx(14.))),
                )
                // Name first and strong, its folder after it and faint: the eye
                // lands on the file, the path is there when wanted.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_row()
                        .items_baseline()
                        .gap_1p5()
                        .overflow_hidden()
                        .child(
                            div()
                                .flex_none()
                                .t_small()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(c(t.text))
                                .child(name.to_string()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .t_caption()
                                .text_color(c(t.text_faint))
                                .child(dir.to_string()),
                        ),
                )
                // A quiet confirmation after a save, gone on its own.
                .when(just_saved && !dirty, |d| {
                    d.child(appear(
                        div()
                            .flex_none()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .t_caption()
                            .text_color(c(t.success))
                            .child(Icon::Check.el(zpx(12.)))
                            .child("Saved"),
                        "files-saved",
                        crate::motion::FAST,
                        2.,
                        reduced,
                    ))
                })
                .when(dirty && !conflict, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .t_caption()
                            .text_color(c(t.text_muted))
                            .child(div().size(zpx(6.)).rounded_full().bg(c(t.warning)))
                            .child("Unsaved"),
                    )
                    .child(text_button("files-discard", "Discard", t).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.files_discard(cx)),
                    ))
                    .child(
                        solid_button("files-save", if saving { "Saving…" } else { "Save" }, t)
                            .child(kbd("⌘S", true, t))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.files_save(false, cx)
                            })),
                    )
                })
                // The file changed in this chat: its diff, one click away.
                .when(change.is_some() && !dirty, |d| {
                    let p = path.clone();
                    d.child(diffstat(
                        change.as_ref().and_then(|c| c.added),
                        change.as_ref().and_then(|c| c.removed),
                        t,
                    ))
                    .child(
                        icon_button("files-view-diff", Icon::FileDiff, t)
                            .tooltip(move |_, cx| {
                                cx.new(|_| super::dotify_ui::Tip::new("View changes", t))
                                    .into()
                            })
                            .tooltip_show_delay(crate::view::tooltip::delay())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.files_goto_change(p.clone(), cx)
                            })),
                    )
                })
                .when(markdown && loaded, |d| {
                    d.child(
                        seg_track(t)
                            .child(
                                seg_item("files-md-preview", "Preview", None, preview, t).on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.files.viewer.md_source = false;
                                        cx.notify();
                                    }),
                                ),
                            )
                            .child(
                                seg_item("files-md-source", "Source", None, !preview, t).on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.files.viewer.md_source = true;
                                        cx.notify();
                                    }),
                                ),
                            ),
                    )
                });
        // The agent (or anyone) wrote the file since it was opened.
        let banner =
            conflict.then(|| {
                div()
                    .flex_none()
                    .mx_2()
                    .mt_2()
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .rounded(RADIUS_MD)
                    .bg(c(t.warning).opacity(0.1))
                    .border_1()
                    .border_color(c(t.warning).opacity(0.25))
                    .t_small()
                    .text_color(c(t.text))
                    .child(
                        div()
                            .flex_none()
                            .text_color(c(t.warning))
                            .child(Icon::Alert.el(zpx(13.))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child("Changed on disk"),
                            )
                            .child(
                                div()
                                    .t_caption()
                                    .text_color(c(t.text_muted))
                                    .child("Something wrote this file after you opened it."),
                            ),
                    )
                    .child(
                        text_button("files-take-disk", "Use disk version", t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.files_discard(cx)),
                        ),
                    )
                    .child(solid_button("files-overwrite", "Keep mine", t).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.files_save(true, cx)),
                    ))
            });
        let error = error.map(|e| {
            div()
                .flex_none()
                .px_3()
                .py_1p5()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .border_b_1()
                .border_color(c(t.border))
                .t_small()
                .text_color(c(t.danger))
                .child(Icon::Alert.el(zpx(12.)))
                .child(e)
        });
        let body = self.render_file_view_body(&path, width, t, window, cx);
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .children(banner)
            .children(error)
            // A short settle when another file (or view) comes up, so the
            // swap reads as a change rather than a flicker. Keyed by file and
            // view: a refresh of the same file doesn't replay it.
            .child(appear(
                div().flex_1().min_h_0().flex().flex_col().child(body),
                ("files-content-in", hash_key(&path, preview)),
                BASE,
                4.,
                reduced,
            ))
            .into_any_element()
    }

    pub(super) fn render_file_view_body(
        &mut self,
        path: &str,
        width: f32,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let kind = crate::files::FileKind::of(std::path::Path::new(path));
        let image = viewer::is_image(path);
        let markdown = kind == crate::files::FileKind::Markdown;
        let file = self.files.file.as_ref().filter(|f| f.path == path);
        let loaded = file.is_some();
        let preview = markdown && loaded && !self.files.viewer.md_source;
        if image {
            self.render_image_view(path, width, t, window, cx)
        } else if preview {
            self.render_md_preview(path, width, t, window, cx)
        } else {
            match self.files.file.as_ref().filter(|f| f.path == path) {
                Some(file) => {
                    let lines = file.editor.read(cx).text().lines_len();
                    let lang = path
                        .rsplit_once('.')
                        .and_then(|(_, e)| crate::syntax::language_name(e))
                        .unwrap_or("Plain text");
                    let eol = if file.saved.contains("\r\n") {
                        "CRLF"
                    } else {
                        "LF"
                    };
                    div()
                        .flex_1()
                        .min_h_0()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .size_full()
                                .pt_2()
                                .pl_1()
                                .t_code()
                                .child(file.editor.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .h(zpx(26.))
                                .px_3()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_3()
                                .border_t_1()
                                .border_color(c(t.border).opacity(0.7))
                                .t_caption()
                                .text_color(c(t.text_faint))
                                .child(div().child(if lines == 1 {
                                    "1 line".to_string()
                                } else {
                                    format!("{} lines", grouped(lines as u32))
                                }))
                                .child(div().flex_1())
                                .child(div().child(lang))
                                .child(div().child(eol))
                                .child(div().child("UTF-8")),
                        )
                        .into_any_element()
                }
                None => {
                    let failed = self
                        .files
                        .viewer
                        .open_error
                        .as_ref()
                        .filter(|(p, _)| *p == path)
                        .map(|(_, e)| e.clone());
                    if failed.is_none()
                        && self.files.opening.as_deref() != Some(path)
                        && self.files.file.as_ref().is_none_or(|f| !f.dirty)
                    {
                        self.files_open_file(path.to_string(), window, cx);
                    }
                    match failed {
                        Some(err) => empty_state(Icon::Alert, "Can't show this file", err, t),
                        None => self.render_code_skeleton("files-open-skel", t),
                    }
                }
            }
        }
    }

    /// Placeholder lines while code loads: a gutter and ragged lines.
    fn render_code_skeleton(&self, prefix: &'static str, t: Tokens) -> AnyElement {
        let reduced = self.reduced();
        const WIDTHS: [f32; 12] = [
            0.42, 0.6, 0.35, 0.72, 0.5, 0.28, 0.64, 0.55, 0.38, 0.7, 0.46, 0.3,
        ];
        div()
            .flex_1()
            .pt_2()
            .flex()
            .flex_col()
            .children(WIDTHS.iter().enumerate().map(|(i, w)| {
                div()
                    .h(zpx(LINE_H))
                    .px_3()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .w(zpx(18.))
                            .h(zpx(7.))
                            .rounded(px(2.))
                            .bg(c(t.text).opacity(0.04)),
                    )
                    .child(crate::motion::shimmer(
                        div()
                            .h(zpx(8.))
                            .w(gpui::relative(*w))
                            .rounded(px(3.))
                            .bg(c(t.text).opacity(0.06)),
                        (prefix, i),
                        i as f32 * 0.05,
                        c(t.text).opacity(0.05),
                        reduced,
                    ))
            }))
            .into_any_element()
    }
}

/// How long "Saved" stays up after a save.
const SAVED_FOR: Duration = Duration::from_millis(1600);

/// A segmented control's track: a quiet well the segments sit in.
fn seg_track(t: Tokens) -> gpui::Div {
    div()
        .flex_none()
        .p(px(2.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(2.))
        .rounded(px(7.))
        .bg(c(t.text).opacity(0.05))
}

/// One segment: raised on the track when on (a fill and a hairline, no
/// shadow), plain text that brightens on hover when off. `badge` is a count.
fn seg_item(
    id: &'static str,
    label: &'static str,
    badge: Option<usize>,
    on: bool,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(zpx(22.))
        .px_2p5()
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(px(5.))
        .t_small()
        .font_weight(gpui::FontWeight::MEDIUM)
        .map(|d| {
            if on {
                d.bg(c(t.window))
                    .border_1()
                    .border_color(c(t.border))
                    .text_color(c(t.text))
            } else {
                d.border_1()
                    .border_color(gpui::transparent_black())
                    .text_color(c(t.text_muted))
                    .cursor_pointer()
                    .hover(|s| s.text_color(c(t.text)))
            }
        })
        .child(label)
        .children(badge.map(|n| {
            div()
                .t_caption()
                .font_family(MONO)
                .text_color(c(if on { t.text_muted } else { t.text_faint }))
                .child(grouped(n as u32))
        }))
}

/// A centred empty, error or nothing-yet view: a quiet outline icon, a
/// short title, one plain sentence.
fn empty_state(
    icon: Icon,
    title: &'static str,
    body: impl Into<SharedString>,
    t: Tokens,
) -> AnyElement {
    div()
        .flex_1()
        .size_full()
        .p_6()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .mb_1()
                .text_color(c(t.text_faint))
                .child(icon.el(zpx(18.))),
        )
        .child(
            div()
                .t_small()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(c(t.text))
                .child(title),
        )
        .child(
            div()
                .max_w(zpx(240.))
                .t_caption()
                .text_center()
                .text_color(c(t.text_faint))
                .child(body.into()),
        )
        .into_any_element()
}

/// `+12 −3` in tabular figures; zero sides drop out.
fn diffstat(added: Option<u32>, removed: Option<u32>, t: Tokens) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .flex_row()
        .gap_1()
        .t_caption()
        .font_family(MONO)
        .children(added.filter(|n| *n > 0).map(|n| {
            div()
                .text_color(c(t.success))
                .child(format!("+{}", grouped(n)))
        }))
        .children(removed.filter(|n| *n > 0).map(|n| {
            div()
                .text_color(c(t.danger))
                .child(format!("−{}", grouped(n)))
        }))
}

/// A file's git status as a small tinted letter: readable without
/// shouting, and the letter keeps it colour-blind safe.
fn status_chip(status: char, t: Tokens) -> gpui::Div {
    let color = match status {
        'A' | '?' => t.success,
        'D' => t.danger,
        'U' => t.warning,
        _ => t.text_muted,
    };
    div()
        .flex_none()
        .size(zpx(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(c(color).opacity(0.14))
        .text_color(c(color))
        .t_caption()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(if status == '?' {
            "A".to_string()
        } else {
            status.to_string()
        })
}

/// The primary action: solid ink on the page colour, no shadow.
fn solid_button(id: &'static str, label: &'static str, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .h(zpx(24.))
        .pl_2p5()
        .pr_1p5()
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(RADIUS_MD)
        .bg(c(t.text))
        .text_color(c(t.window))
        .t_caption()
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|s| s.opacity(0.88))
        .active(|s| s.opacity(0.75))
        .child(label)
}

/// A secondary action: text only, a wash on hover.
fn text_button(id: &'static str, label: &'static str, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .h(zpx(24.))
        .px_2()
        .flex()
        .items_center()
        .rounded(RADIUS_MD)
        .text_color(c(t.text_muted))
        .t_caption()
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|s| s.text_color(c(t.text)))
        .hover_fade(c(t.hover), Some(c(t.selected)))
        .child(label)
}

/// A shortcut as a small key cap. `on_solid`: sits on a solid button.
fn kbd(keys: &'static str, on_solid: bool, t: Tokens) -> gpui::Div {
    let ink = if on_solid { t.window } else { t.text };
    div()
        .flex_none()
        .h(zpx(16.))
        .px_1()
        .flex()
        .items_center()
        .rounded(px(4.))
        .bg(c(ink).opacity(0.12))
        .text_color(c(ink).opacity(0.75))
        .t_caption()
        .font_family(MONO)
        .child(keys)
}

/// 1894 → "1,894".
fn grouped(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// The file editor's look: the app's input colours plus syntax colours, a
/// faint band on the caret's line, and line numbers that stay out of the way.
pub(super) fn code_editor_style(
    t: Tokens,
    syntax: keel_theme::SyntaxPaintMap,
) -> gpui_base::input::InputEditorStyle {
    gpui_base::input::InputEditorStyle {
        foreground: c(syntax.plain << 8 | 0xFF),
        muted_foreground: c(t.text_faint).opacity(0.7),
        highlight_styles: std::sync::Arc::new(crate::syntax::EditorPaint(syntax)),
        editor_active_line: Some(c(t.text).opacity(0.045)),
        ..super::editor_style(t)
    }
}

/// An animation key for a file shown one way.
fn hash_key(path: &str, diff: bool) -> usize {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (path, diff).hash(&mut h);
    h.finish() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_number_their_lines() {
        let patch = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -3,3 +3,4 @@ fn main\n keep\n-old\n+new\n+\tmore\n tail\n\\ No newline at end of file\n";
        let (lines, widest) = parse_patch(patch);
        let view: Vec<_> = lines
            .iter()
            .map(|l| (l.kind, l.old, l.new, l.text.to_string()))
            .collect();
        assert_eq!(
            view,
            vec![
                (
                    LineKind::Hunk,
                    Some(3),
                    Some(3),
                    "@@ -3,3 +3,4 @@ fn main".into()
                ),
                (LineKind::Context, Some(3), Some(3), "keep".into()),
                (LineKind::Removed, Some(4), None, "old".into()),
                (LineKind::Added, None, Some(4), "new".into()),
                (LineKind::Added, None, Some(5), "    more".into()),
                (LineKind::Context, Some(5), Some(6), "tail".into()),
                (
                    LineKind::Note,
                    None,
                    None,
                    "\\ No newline at end of file".into()
                ),
            ]
        );
        assert_eq!(widest, 6);
        // A new file starts at line 1; a second file's headers are skipped.
        let (lines, _) = parse_patch(
            "--- /dev/null\n+++ b/n\n@@ -0,0 +1,2 @@\n+a\n+b\ndiff --git a/y b/y\n--- a/y\n+++ b/y\n@@ -1 +1 @@\n-c\n+d\n",
        );
        let nums: Vec<_> = lines.iter().map(|l| (l.old, l.new)).collect();
        assert_eq!(
            nums,
            vec![
                (Some(0), Some(1)),
                (None, Some(1)),
                (None, Some(2)),
                (Some(1), Some(1)),
                (Some(1), None),
                (None, Some(1))
            ]
        );
        // Each side colours as its own file; runs stay inside their line.
        let (mut lines, _) =
            parse_patch("@@ -1,2 +1,2 @@\n-let a = \"x\";\n+let b = 42;\n fn f() {}\n");
        highlight_diff(&mut lines, "src/x.rs");
        for l in &lines {
            assert!(l.spans.iter().all(|(r, _)| r.end <= l.text.len()), "{l:?}");
        }
        assert!(
            lines[1].spans.iter().any(|(_, k)| *k == SyntaxKind::String),
            "{:?}",
            lines[1]
        );
        assert!(
            lines[2]
                .spans
                .iter()
                .any(|(r, k)| &lines[2].text[r.clone()] == "42" && *k == SyntaxKind::Number),
            "{:?}",
            lines[2]
        );
        assert_eq!(
            (grouped(7), grouped(1894), grouped(1_000_000)),
            ("7".into(), "1,894".into(), "1,000,000".into())
        );
        // A one-word edit marks that word (whole), not the line.
        let (o, n) = inline_change("let count = total + 1;", "let count = total + 2;").unwrap();
        assert_eq!(
            (&"let count = total + 1;"[o], &"let count = total + 2;"[n]),
            ("1", "2")
        );
        let (o, n) = inline_change("fn old_name(x)", "fn new_name(x)").unwrap();
        assert_eq!(
            (&"fn old_name(x)"[o], &"fn new_name(x)"[n]),
            ("old_name", "new_name")
        );
        assert!(
            inline_change("abc", "xyz").is_none(),
            "a rewrite stays a plain wash"
        );
        assert!(inline_change("é ✓ a", "é ✓ b").is_some());
        // Hidden lines between hunks are counted, and expand from the file.
        let (lines, _) =
            parse_patch("@@ -2,2 +2,2 @@\n a2\n-b3\n+B3\n@@ -10,1 +10,1 @@\n-j10\n+J10\n");
        let hunk = lines
            .iter()
            .position(|l| l.kind == LineKind::Hunk && l.new == Some(10))
            .unwrap();
        assert_eq!((lines[0].gap, lines[hunk].gap), (1, 6));
        assert_eq!(
            (lines[2].emph.clone(), lines[3].emph.clone()),
            (Some(0..2), Some(0..2))
        );
        let file: String = (1..=12).map(|i| format!("l{i}\n")).collect();
        let open = expand_gap(&lines, hunk, &file);
        let shown: Vec<_> = open
            .iter()
            .filter(|l| l.kind == LineKind::Context)
            .map(|l| (l.old, l.new, l.text.to_string()))
            .collect();
        assert_eq!(shown.first(), Some(&(Some(2), Some(2), "a2".into())));
        assert_eq!(shown.last(), Some(&(Some(9), Some(9), "l9".into())));
        assert_eq!(shown.len(), 7);
        assert!(open.iter().filter(|l| l.kind == LineKind::Hunk).count() == 1);
    }
}

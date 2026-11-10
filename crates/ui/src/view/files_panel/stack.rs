//! The Changes tab: the changed files (flat, or as a folder tree with
//! summed counts) beside every diff in one scrolling stack. Each file is a
//! section with a header that pins to the top while its lines scroll by.
//! Only what's on screen is drawn and only what's near it is fetched, so a
//! hundred-file change set stays light. Lockfiles, generated, deleted and
//! huge files start folded; long diffs show their first lines and a way to
//! the rest. Wide panes pair old and new side by side.

use super::*;
use crate::hover_fade::HoverFadeExt;
use gpui::{AnimationExt, ListAlignment, ListState, Pixels, WeakEntity};
use std::collections::BTreeMap;

use super::fold::{TreeFold, chevron_turn, slot, slot_rows};

/// The diff column (logical px) from which old and new sit side by side.
const SPLIT_AT: f32 = 600.;
/// Lines a section shows before "Show more", and how many more each click.
const FIRST_ROWS: usize = 400;
const MORE_ROWS: usize = 2000;
/// A diff this many changed lines long starts folded.
#[cfg(test)]
const BIG_DIFF: u32 = 1500;
/// Diffs fetched at once.
const MAX_LOADS: usize = 4;
/// Code rows: 18px lines in a row padded to LINE_H.
const CODE_LH: f32 = 18.;

#[cfg(test)]
const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "Gemfile.lock",
    "poetry.lock",
    "composer.lock",
    "go.sum",
    "uv.lock",
    "Podfile.lock",
    "flake.lock",
];

/// Something in a section that isn't a line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Note {
    Loading,
    Failed,
    Empty,
    Image,
    Truncated,
}

/// One row of the stack. Files by index into the change list, lines by
/// index into that file's parsed diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Item {
    Line(usize, usize),
    /// Side by side: the old line and the new one (either may be missing).
    Pair(usize, Option<usize>, Option<usize>),
    Note(usize, Note),
    /// "Show N more lines".
    More(usize, usize),
}

/// A row of the changes tree: a folder (chains of single folders joined,
/// `src/view`) with its files' summed counts, or a file.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ChangeRow {
    pub(super) key: String,
    pub(super) name: String,
    pub(super) depth: usize,
    pub(super) file: Option<usize>,
    pub(super) open: bool,
    pub(super) added: u32,
    pub(super) removed: u32,
    pub(super) count: usize,
}

/// Why a file starts folded, said on its header.
#[cfg(test)]
pub(super) fn closed_why(ch: &Change) -> Option<&'static str> {
    let name = ch.path.rsplit('/').next().unwrap_or(&ch.path);
    if ch.status == 'D' {
        return Some("Deleted");
    }
    if LOCKFILES.contains(&name) {
        return Some("Lockfile");
    }
    let generated = [".min.js", ".min.css", ".map", ".snap"]
        .iter()
        .any(|e| name.ends_with(e))
        || ch.path.starts_with("dist/")
        || ch.path.contains("/dist/");
    if generated {
        return Some("Generated");
    }
    if ch.added.unwrap_or(0) + ch.removed.unwrap_or(0) > BIG_DIFF {
        return Some("Large diff");
    }
    None
}

#[cfg(test)]
pub(super) fn is_open(ch: &Change, open: &HashMap<String, bool>) -> bool {
    open.get(&ch.path)
        .copied()
        .unwrap_or_else(|| closed_why(ch).is_none())
}

/// `@web/src` reads as `web/src`. The `@` is how a path names its folder.
fn shown_dir(dir: &str) -> String {
    if dir.is_empty() {
        return String::new();
    }
    dir.split('/')
        .map(|part| part.strip_prefix('@').unwrap_or(part))
        .collect::<Vec<_>>()
        .join("/")
}

/// The alias of `@alias/rest`, or "" when the path is not in a named folder.
fn change_alias(path: &str) -> &str {
    path.strip_prefix('@')
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("")
}

/// One row of the flat changes list: a folder, or a file in it.
#[derive(Clone)]
enum ChangeFlat {
    Head {
        alias: String,
        branch: String,
        n: usize,
    },
    File(usize),
}

/// Where a file sits in the flat changes list once folder headers are in it.
#[cfg(test)]
pub(super) fn change_list_row(list: &[Change], file_ix: usize) -> usize {
    if !list.iter().any(|c| c.path.starts_with('@')) {
        return file_ix;
    }
    let mut headers = 0usize;
    let mut prev = "";
    for (i, change) in list.iter().enumerate() {
        let alias = change_alias(&change.path);
        if alias != prev {
            headers += 1;
            prev = alias;
        }
        if i == file_ix {
            return i + headers;
        }
    }
    file_ix
}

/// The changed files as folders and files: folders first, single-folder
/// chains joined into one row, each folder with its files' summed counts.
pub(super) fn change_tree(list: &[Change], folded: &HashSet<String>) -> Vec<ChangeRow> {
    #[derive(Default)]
    struct Node {
        dirs: BTreeMap<String, Node>,
        files: Vec<usize>,
        added: u32,
        removed: u32,
        count: usize,
    }
    fn emit(
        node: &Node,
        prefix: &str,
        depth: usize,
        list: &[Change],
        folded: &HashSet<String>,
        out: &mut Vec<ChangeRow>,
    ) {
        for (name, child) in &node.dirs {
            let mut label = name.clone();
            let mut key = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let mut n = child;
            while n.files.is_empty() && n.dirs.len() == 1 {
                let (k, c) = n.dirs.iter().next().expect("one child");
                label = format!("{label}/{k}");
                key = format!("{key}/{k}");
                n = c;
            }
            let open = !folded.contains(&key);
            out.push(ChangeRow {
                key: key.clone(),
                name: label,
                depth,
                file: None,
                open,
                added: n.added,
                removed: n.removed,
                count: n.count,
            });
            if open {
                emit(n, &key, depth + 1, list, folded, out);
            }
        }
        for &ix in &node.files {
            let ch = &list[ix];
            let name = ch.path.rsplit('/').next().unwrap_or(&ch.path).to_string();
            out.push(ChangeRow {
                key: ch.path.clone(),
                name,
                depth,
                file: Some(ix),
                open: false,
                added: ch.added.unwrap_or(0),
                removed: ch.removed.unwrap_or(0),
                count: 1,
            });
        }
    }
    let mut root = Node::default();
    for (ix, ch) in list.iter().enumerate() {
        let (a, r) = (ch.added.unwrap_or(0), ch.removed.unwrap_or(0));
        let parts: Vec<&str> = ch.path.split('/').collect();
        let mut node = &mut root;
        for part in &parts[..parts.len() - 1] {
            node = node.dirs.entry((*part).to_string()).or_default();
            node.added += a;
            node.removed += r;
            node.count += 1;
        }
        node.files.push(ix);
    }
    let mut out = Vec::new();
    emit(&root, "", 0, list, folded, &mut out);
    out
}

/// Rows for a single file's diff detail view (no outer accordion header/foot).
pub(super) fn build_detail_items(
    f: usize,
    ch: &Change,
    doc_load: Option<&Load<Rc<DiffDoc>>>,
    shown_limit: Option<usize>,
    split: bool,
) -> Vec<Item> {
    let mut out = Vec::new();
    if viewer::is_image(&ch.path) {
        out.push(Item::Note(f, Note::Image));
        return out;
    }
    match doc_load {
        None | Some(Load::Loading) => out.push(Item::Note(f, Note::Loading)),
        Some(Load::Failed(_)) => out.push(Item::Note(f, Note::Failed)),
        Some(Load::Ready(doc)) if doc.lines.is_empty() => out.push(Item::Note(f, Note::Empty)),
        Some(Load::Ready(doc)) => {
            let limit = shown_limit.unwrap_or(FIRST_ROWS).min(doc.lines.len());
            if split {
                push_split(f, &doc.lines[..limit], &mut out);
            } else {
                for (ix, _) in doc.lines[..limit].iter().enumerate() {
                    out.push(Item::Line(f, ix));
                }
            }
            if limit < doc.lines.len() {
                out.push(Item::More(f, doc.lines.len() - limit));
            }
            if doc.truncated {
                out.push(Item::Note(f, Note::Truncated));
            }
        }
    }
    out
}

/// Side by side: a run of removed lines pairs with the added run after it,
/// row for row; context sits on both sides; hunks span the row.
fn push_split(f: usize, lines: &[DiffLine], out: &mut Vec<Item>) {
    let mut ix = 0;
    while ix < lines.len() {
        match lines[ix].kind {
            LineKind::Removed | LineKind::Added => {
                let start = ix;
                while ix < lines.len() && lines[ix].kind == LineKind::Removed {
                    ix += 1;
                }
                let mid = ix;
                while ix < lines.len() && lines[ix].kind == LineKind::Added {
                    ix += 1;
                }
                let (removed, added) = (mid - start, ix - mid);
                for k in 0..removed.max(added) {
                    let a = (k < removed).then_some(start + k);
                    let b = (k < added).then_some(mid + k);
                    out.push(Item::Pair(f, a, b));
                }
            }
            LineKind::Context => {
                out.push(Item::Pair(f, Some(ix), Some(ix)));
                ix += 1;
            }
            LineKind::Hunk | LineKind::Note => {
                out.push(Item::Line(f, ix));
                ix += 1;
            }
        }
    }
}

/// What the rows read while they draw, taken once per change.
#[derive(Default)]
pub(super) struct Snap {
    docs: Vec<Option<Rc<DiffDoc>>>,
    /// Width of the line numbers, in digits.
    digits: Vec<usize>,
}

pub(super) struct Stack {
    list: ListState,
    items: Rc<Vec<Item>>,
    files: Rc<Vec<Change>>,
    pub(super) dirty: bool,
    split: bool,
    docs: HashMap<String, Load<Rc<DiffDoc>>>,
    loading: usize,
    queue: Vec<String>,
    /// Bumped when the chat changes, so late diffs land nowhere.
    generation: u64,
    shown: HashMap<String, usize>,
    /// The file at the top of the stack, lit in the list.
    pub(super) active: Option<String>,
    pub(super) tree: bool,
    pub(super) mode_generation: usize,
    folded: HashSet<String>,
    rows: Option<Rc<Vec<ChangeRow>>>,
    list_scroll: UniformListScrollHandle,
    tree_scroll: UniformListScrollHandle,
    /// Narrow pane: the list instead of the stack.
    pub(super) narrow_list: bool,
    pub(super) reload_all: bool,
    remeasure: HashSet<String>,
    /// Dev: the split layout regardless of width (None follows the width).
    pub(super) force_split: Option<bool>,
    /// The tree folder opening or closing.
    tree_fold: Option<TreeFold<ChangeRow>>,
    /// Dev: a record of each frame of a fold, while asked for.
    /// Dev (`files_fold`): since when, which file, and one entry per frame.
    pub(super) trace: Option<(Instant, String, Vec<serde_json::Value>)>,
}

impl Stack {
    pub(super) fn new() -> Self {
        Self {
            list: ListState::new(0, ListAlignment::Top, px(600.)),
            items: Rc::new(Vec::new()),
            files: Rc::new(Vec::new()),
            dirty: false,
            split: false,
            docs: HashMap::new(),
            loading: 0,
            queue: Vec::new(),
            generation: 0,
            shown: HashMap::new(),
            active: None,
            tree: false,
            mode_generation: 0,
            folded: HashSet::new(),
            rows: None,
            list_scroll: UniformListScrollHandle::new(),
            tree_scroll: UniformListScrollHandle::new(),
            narrow_list: false,
            reload_all: false,
            remeasure: HashSet::new(),
            force_split: None,
            tree_fold: None,
            trace: None,
        }
    }

    /// Another chat: nothing carries over but the list or tree choice.
    pub(super) fn clear(&mut self) {
        let (tree, generation) = (self.tree, self.generation + 1);
        *self = Self::new();
        self.tree = tree;
        self.generation = generation;
    }

    /// Dev: a summary of the stack, for checks.
    pub(super) fn state_json(&self) -> serde_json::Value {
        let top = self.list.logical_scroll_top();
        serde_json::json!({
            "files": self.files.len(),
            "items": self.items.len(),
            "split": self.split,
            "tree": self.tree,
            "top": top.item_ix,
            "active": self.active,
            "loaded": self.docs.values().filter(|d| matches!(d, Load::Ready(_))).count(),
            "loading": self.loading,
            "queued": self.queue.len(),
            "tree_folding": self.tree_fold.as_ref().map(|f| f.key.clone()),
        })
    }

    fn rows(&mut self, list: &[Change]) -> Rc<Vec<ChangeRow>> {
        if let Some(rows) = &self.rows {
            return rows.clone();
        }
        let rows = Rc::new(change_tree(list, &self.folded));
        self.rows = Some(rows.clone());
        rows
    }
}

/// What a row needs to draw itself.
struct Ctx {
    t: Tokens,
    syntax: keel_theme::SyntaxPaintMap,
    items: Rc<Vec<Item>>,
    files: Rc<Vec<Change>>,
    snap: Rc<Snap>,
    weak: WeakEntity<KeelWindow>,
    reduced: bool,
}

impl KeelWindow {
    /// Bring a changed file's section or detail up: open it and show diff.
    pub(super) fn files_goto_change(&mut self, path: String, cx: &mut Context<Self>) {
        if path.starts_with('@') && !path.contains('/') {
            self.files.selected = None;
            self.files.tab = FilesTab::Changes;
            cx.notify();
        } else {
            self.files_open_change(path, cx);
        }
    }

    pub(super) fn files_open_change(&mut self, path: String, cx: &mut Context<Self>) {
        if let Some(Load::Ready(list)) = &self.files.changes {
            let s = &mut self.files.stack;
            if !Rc::ptr_eq(&s.files, list) && s.files.as_slice() != list.as_slice() {
                s.files = list.clone();
                s.rows = None;
            }
        }
        {
            let s = &mut self.files.stack;
            let before = s.folded.len();
            s.folded.retain(|k| !path.starts_with(&format!("{k}/")));
            if s.folded.len() != before {
                s.rows = None;
            }
            s.active = Some(path.clone());
            s.dirty = true;
        }
        self.files.selected = Some(path.clone());
        self.files.change_detail_tab = ChangeDetailTab::Diff;
        self.files.tab = FilesTab::Changes;
        if !viewer::is_image(&path) {
            self.files_want_diff(path.clone(), cx);
        }
        cx.notify();
    }

    fn branch_of_alias(&self, alias: &str) -> String {
        let Some(chat) = self.files.chat.as_deref() else {
            return String::new();
        };
        let Some(link) = self.projects.store.chats.get(chat) else {
            return String::new();
        };
        let Some(folder) = self
            .projects
            .store
            .project(&link.project)
            .and_then(|p| p.folder(alias))
        else {
            return String::new();
        };
        let branch = self.projects.folder_branch_label(&folder.path);
        if branch == "…" {
            String::new()
        } else {
            branch
        }
    }

    /// A folder in the changes tree folds or opens; its rows slide.
    pub(super) fn files_toggle_change_dir(&mut self, key: String, cx: &mut Context<Self>) {
        let list = match &self.files.changes {
            Some(Load::Ready(list)) => list.clone(),
            _ => return,
        };
        let reduced = self.reduced();
        let now = self.frame_now.get().unwrap_or_else(Instant::now);
        let s = &mut self.files.stack;
        if !Rc::ptr_eq(&s.files, &list) && s.files.as_slice() != list.as_slice() {
            s.files = list.clone();
            s.rows = None;
        }
        let old = s.rows(&list);
        if !s.folded.remove(&key) {
            s.folded.insert(key.clone());
        }
        s.rows = None;
        let new = s.rows(&list);
        let at = old.iter().position(|r| r.file.is_none() && r.key == key);
        s.tree_fold = TreeFold::toggled(s.tree_fold.take(), &key, at, old, new, now, reduced);
        cx.notify();
    }

    pub(super) fn files_toggle_section(&mut self, path: String, cx: &mut Context<Self>) {
        if self.files.selected.as_deref() == Some(&path) {
            self.files.selected = None;
        } else {
            self.files_open_change(path, cx);
        }
        cx.notify();
    }

    /// The diff is wanted (on screen, or asked for): fetch it, or queue it
    /// behind the ones in flight.
    fn files_want_diff(&mut self, path: String, cx: &mut Context<Self>) {
        let s = &mut self.files.stack;
        if s.docs.contains_key(&path) {
            return;
        }
        s.docs.insert(path.clone(), Load::Loading);
        if s.loading < MAX_LOADS {
            self.files_load_diff(path, cx);
        } else if !s.queue.contains(&path) {
            s.queue.push(path);
        }
    }

    fn files_pump(&mut self, cx: &mut Context<Self>) {
        while self.files.stack.loading < MAX_LOADS {
            let Some(path) = self.files.stack.queue.pop() else {
                return;
            };
            if self.files.stack.files.iter().any(|c| c.path == path) {
                self.files_load_diff(path, cx);
            } else {
                self.files.stack.docs.remove(&path);
            }
        }
    }

    /// Fetch one file's diff. A reload keeps the old lines up until the new
    /// ones land.
    fn files_load_diff(&mut self, path: String, cx: &mut Context<Self>) {
        let (Some(chat), Some(client)) = (self.files.chat.clone(), self.client.clone()) else {
            return;
        };
        let s = &mut self.files.stack;
        s.loading += 1;
        s.docs.entry(path.clone()).or_insert(Load::Loading);
        let generation = s.generation;
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "chat_diff",
                    serde_json::json!({ "chat_id": chat, "path": path }),
                )
                .await;
            // Parsing a big patch stays off the UI thread.
            let doc = match res {
                Ok(v) => {
                    let patch = v["patch"].as_str().unwrap_or_default().to_string();
                    let truncated = v["truncated"].as_bool().unwrap_or(false);
                    let file = path.clone();
                    let lines = cx
                        .background_spawn(async move {
                            let (mut lines, _) = parse_patch(&patch);
                            highlight_diff(&mut lines, &file);
                            lines
                        })
                        .await;
                    Load::Ready(Rc::new(DiffDoc { lines, truncated }))
                }
                Err(err) => Load::Failed(err),
            };
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.files.stack;
                if s.generation != generation {
                    return;
                }
                s.loading = s.loading.saturating_sub(1);
                if matches!(s.docs.get(&path), Some(Load::Ready(_))) {
                    s.remeasure.insert(path.clone());
                }
                s.docs.insert(path, doc);
                s.dirty = true;
                this.files_pump(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// After a refresh: drop diffs of files no longer changed, and reload
    /// the ones whose counts or status moved (all of them on Refresh).
    pub(super) fn files_reconcile_docs(
        &mut self,
        old: Option<Rc<Vec<Change>>>,
        cx: &mut Context<Self>,
    ) {
        let list = match &self.files.changes {
            Some(Load::Ready(list)) => list.clone(),
            _ => return,
        };
        let s = &mut self.files.stack;
        s.files = list.clone();
        s.rows = None;
        let all = std::mem::take(&mut s.reload_all);
        let now: HashSet<&str> = list.iter().map(|c| c.path.as_str()).collect();
        s.docs.retain(|p, _| now.contains(p.as_str()));
        s.queue.retain(|p| now.contains(p.as_str()));
        let was: HashMap<&str, &Change> = old
            .as_ref()
            .map(|o| o.iter().map(|c| (c.path.as_str(), c)).collect())
            .unwrap_or_default();
        let stale: Vec<String> = list
            .iter()
            .filter(|ch| matches!(s.docs.get(&ch.path), Some(Load::Ready(_) | Load::Failed(_))))
            .filter(|ch| {
                all || was.get(ch.path.as_str()).is_none_or(|o| {
                    o.status != ch.status || o.added != ch.added || o.removed != ch.removed
                })
            })
            .map(|ch| ch.path.clone())
            .collect();
        s.dirty = true;
        for path in stale {
            if self.files.stack.loading < MAX_LOADS {
                self.files_load_diff(path, cx);
            } else if !self.files.stack.queue.contains(&path) {
                self.files.stack.queue.push(path);
            }
        }
    }

    fn files_show_more(&mut self, path: String, cx: &mut Context<Self>) {
        let s = &mut self.files.stack;
        let n = s.shown.get(&path).copied().unwrap_or(FIRST_ROWS);
        s.shown.insert(path, n + MORE_ROWS);
        s.dirty = true;
        cx.notify();
    }

    /// A click on a hunk band: read the file and show the lines above it.
    fn files_expand_gap(&mut self, path: String, ix: usize, cx: &mut Context<Self>) {
        let (Some(chat), Some(client)) = (self.files.chat.clone(), self.client.clone()) else {
            return;
        };
        let Some(Load::Ready(doc)) = self.files.stack.docs.get(&path) else {
            return;
        };
        let doc = doc.clone();
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
            let content = v["content"].as_str().unwrap_or_default().to_string();
            let file = path.clone();
            let base = doc.lines.clone();
            let lines = cx
                .background_spawn(async move {
                    let mut lines = expand_gap(&base, ix, &content);
                    highlight_diff(&mut lines, &file);
                    lines
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // Only onto the diff it was asked from (not one a refresh replaced).
                let s = &mut this.files.stack;
                if let Some(Load::Ready(now)) = s.docs.get(&path)
                    && Rc::ptr_eq(now, &doc)
                {
                    s.docs.insert(
                        path.clone(),
                        Load::Ready(Rc::new(DiffDoc {
                            lines,
                            truncated: doc.truncated,
                        })),
                    );
                    s.remeasure.insert(path);
                    s.dirty = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The Changes tab: the list (or tree) beside the stack; a narrow pane
    /// shows one at a time.
    pub(super) fn render_changes(
        &mut self,
        width: f32,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let list = match &self.files.changes {
            None | Some(Load::Loading) => {
                return div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(self.render_list_skeleton("files-changes-skel", t))
                    .into_any_element();
            }
            Some(Load::Failed(err)) => {
                return empty_state(Icon::Alert, "Couldn't read changes", err.clone(), t);
            }
            Some(Load::Ready(list)) if list.is_empty() => {
                let rooted = !self
                    .projects
                    .file_roots(self.files.chat.as_deref())
                    .is_empty();
                let body = if rooted {
                    "Nothing is changed in these folders."
                } else {
                    "Files the agent edits in this folder show up here, ready to review."
                };
                return empty_state(Icon::FileDiff, "No changes yet", body, t);
            }
            Some(Load::Ready(list)) => list.clone(),
        };

        if let Some(selected) = self
            .files
            .selected
            .clone()
            .filter(|p| !p.starts_with('@') || p.contains('/'))
        {
            if list.iter().any(|c| c.path == selected) {
                return self.render_change_detail(&selected, &list, width, t, window, cx);
            }
        }

        self.render_change_list(&list, t, window, cx)
    }

    /// The changed files: a summary bar (how many, how many lines, list or
    /// tree), then the rows. The file at the top of the stack is lit.
    fn render_change_list(
        &mut self,
        list: &Rc<Vec<Change>>,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !Rc::ptr_eq(&self.files.stack.files, list)
            && self.files.stack.files.as_slice() != list.as_slice()
        {
            self.files.stack.files = list.clone();
            self.files.stack.rows = None;
        }
        let (added, removed) = list.iter().fold((0, 0), |(a, r), ch| {
            (a + ch.added.unwrap_or(0), r + ch.removed.unwrap_or(0))
        });
        let tree = self.files.stack.tree;
        let mode_seg = move |id: &'static str, icon: Icon, on: bool| {
            div()
                .id(id)
                .size(zpx(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .border_1()
                .map(|d| {
                    if on {
                        d.bg(c(t.window))
                            .border_color(c(t.border))
                            .text_color(c(t.text))
                    } else {
                        d.border_color(gpui::transparent_black())
                            .text_color(c(t.text_faint))
                            .cursor_pointer()
                            .hover(|s| s.text_color(c(t.text)))
                            .active(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                    }
                })
                .child(icon.el(zpx(13.)))
        };
        let summary =
            div()
                .flex_none()
                .h(zpx(34.))
                .pl_3()
                .pr_1p5()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(c(t.border).opacity(0.7))
                .t_caption()
                .child(
                    div()
                        .flex_none()
                        .text_color(c(t.text_muted))
                        .child(if list.len() == 1 {
                            "1 file".to_string()
                        } else {
                            format!("{} files", grouped(list.len() as u32))
                        }),
                )
                .child(diffstat(Some(added), Some(removed), t))
                .child(div().flex_1())
                .child(
                    seg_track(t)
                        .child(mode_seg("files-list-flat", Icon::List, !tree).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                if this.files.stack.tree {
                                    this.files.stack.tree = false;
                                    this.files.stack.mode_generation += 1;
                                    cx.notify();
                                }
                            }),
                        ))
                        .child(mode_seg("files-list-tree", Icon::ListTree, tree).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                if !this.files.stack.tree {
                                    this.files.stack.tree = true;
                                    this.files.stack.mode_generation += 1;
                                    cx.notify();
                                }
                            }),
                        )),
                );
        let active = self.files.stack.active.clone();
        let weak = cx.entity().downgrade();
        let file_row = Rc::new(
            move |id: usize,
                  ch: &Change,
                  name: String,
                  dir: Option<String>,
                  depth: usize,
                  tree: bool|
                  -> AnyElement {
                let on = active.as_deref() == Some(ch.path.as_str());
                let path = ch.path.clone();
                let weak = weak.clone();
                let deleted = ch.status == 'D';
                div()
                    .id(("files-change", id))
                    .h(zpx(ROW_H))
                    .w_full()
                    .pl(zpx(8. + depth as f32 * 12.))
                    .pr_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
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
                    .when(tree, |d| d.child(div().flex_none().w(zpx(10.))))
                    .child(
                        div()
                            .flex_none()
                            .text_color(c(if on { t.text_muted } else { t.text_faint }))
                            .child(Icon::for_path(&name).el(zpx(13.))),
                    )
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
                                    .text_color(c(if deleted { t.text_muted } else { t.text }))
                                    .when(deleted, |d| d.line_through())
                                    .child(name),
                            )
                            .children(dir.map(|dir| {
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .t_caption()
                                    .text_color(c(t.text_faint))
                                    .child(dir)
                            })),
                    )
                    .child(diffstat(ch.added, ch.removed, t))
                    .child(status_chip(ch.status, t))
                    .on_click(move |_, _, cx| {
                        let path = path.clone();
                        let _ = weak.update(cx, |this, cx| this.files_open_change(path, cx));
                    })
                    .into_any_element()
            },
        );
        let rows: AnyElement = if tree {
            let now = self.frame_now.get().unwrap_or_else(Instant::now);
            let s = &mut self.files.stack;
            if let Some(fold) = &mut s.tree_fold {
                if fold.motion.done(now) {
                    s.tree_fold = None;
                } else {
                    fold.motion.start(now);
                    window.request_animation_frame();
                }
            }
            let live = s.rows(list);
            // While a folder moves, the rows with it open, laid out slot by slot.
            let moving = s.tree_fold.as_ref().map(|m| {
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
            let list = list.clone();
            let weak = cx.entity().downgrade();
            uniform_list("files-changes-tree", rows.len(), move |range, _, _| {
                let row_el = |ix: usize| -> AnyElement {
                    let row = &rows[ix];
                    if let Some(fi) = row.file {
                        return file_row(ix, &list[fi], row.name.clone(), None, row.depth, true);
                    }
                    let key = row.key.clone();
                    let weak = weak.clone();
                    div()
                        .id(("files-change-dir", ix))
                        .h(zpx(ROW_H))
                        .w_full()
                        .pl(zpx(8. + row.depth as f32 * 12.))
                        .pr_2()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .rounded(RADIUS_MD)
                        .t_small()
                        .cursor_pointer()
                        .hover_fade(c(t.hover), Some(c(t.selected)))
                        .child(
                            div()
                                .flex_none()
                                .w(zpx(10.))
                                .text_color(c(t.text_faint))
                                .child(Icon::ChevronDown.el(zpx(10.)).with_transformation(
                                    gpui::Transformation::rotate(gpui::percentage(chevron_turn(
                                        match &moving {
                                            Some(m) if m.0 == row.key => m.2,
                                            _ if row.open => 1.,
                                            _ => 0.,
                                        },
                                    ))),
                                )),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(c(t.text_faint))
                                .child(Icon::Folder.el(zpx(13.))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(c(t.text_muted))
                                .child(shown_dir(&row.name)),
                        )
                        // Summed counts, quiet: the folder's weight, not a call to act.
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_row()
                                .gap_1()
                                .t_caption()
                                .font_family(MONO)
                                .text_color(c(t.text_faint))
                                .when(row.added > 0, |d| {
                                    d.child(format!("+{}", grouped(row.added)))
                                })
                                .when(row.removed > 0, |d| {
                                    d.child(format!("\u{2212}{}", grouped(row.removed)))
                                }),
                        )
                        .on_click(move |_, _, cx| {
                            let key = key.clone();
                            let _ =
                                weak.update(cx, |this, cx| this.files_toggle_change_dir(key, cx));
                        })
                        .into_any_element()
                };
                range
                    .map(|ix| match &moving {
                        Some((_, open, _, start, children, Some(_), reveal)) => slot(
                            ix,
                            slot_rows(ix, rows.len(), *start, *children, *reveal, ROW_H),
                            *start..*start + *children,
                            *open,
                            ROW_H,
                            &row_el,
                        ),
                        _ => row_el(ix),
                    })
                    .collect()
            })
            .track_scroll(&self.files.stack.tree_scroll)
            .p_1p5()
            .flex_1()
            .into_any_element()
        } else {
            let grouped = list.iter().any(|c| c.path.starts_with('@'));
            let mut flat: Vec<ChangeFlat> = Vec::new();
            if grouped {
                let mut i = 0;
                while i < list.len() {
                    let alias = change_alias(&list[i].path).to_string();
                    let start = i;
                    i += 1;
                    while i < list.len() && change_alias(&list[i].path) == alias {
                        i += 1;
                    }
                    flat.push(ChangeFlat::Head {
                        alias: alias.clone(),
                        branch: self.branch_of_alias(&alias),
                        n: i - start,
                    });
                    flat.extend((start..i).map(ChangeFlat::File));
                }
            } else {
                flat.extend((0..list.len()).map(ChangeFlat::File));
            }
            let list = list.clone();
            uniform_list("files-changes", flat.len(), move |range, _, _| {
                range
                    .map(|ix| match &flat[ix] {
                        ChangeFlat::Head { alias, branch, n } => div()
                            .id(("files-change-root", ix))
                            .h(zpx(ROW_H))
                            .w_full()
                            .mt(if ix == 0 { zpx(0.) } else { zpx(8.) })
                            .px_2()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .t_small()
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(c(t.text))
                                    .child(alias.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .t_caption()
                                    .text_color(c(t.text_faint))
                                    .child(if branch == "…" {
                                        String::new()
                                    } else {
                                        branch.clone()
                                    }),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .t_caption()
                                    .text_color(c(t.text_faint))
                                    .child(n.to_string()),
                            )
                            .into_any_element(),
                        ChangeFlat::File(fi) => {
                            let ch = &list[*fi];
                            let (dir, name) = ch
                                .path
                                .rsplit_once('/')
                                .map_or((None, ch.path.as_str()), |(d, n)| (Some(shown_dir(d)), n));
                            let dir = dir.and_then(|d| {
                                d.split_once('/')
                                    .map(|(_, rest)| rest.to_string())
                                    .filter(|rest| !rest.is_empty())
                            });
                            file_row(
                                ix,
                                ch,
                                name.to_string(),
                                dir,
                                if grouped { 1 } else { 0 },
                                false,
                            )
                        }
                    })
                    .collect()
            })
            .track_scroll(&self.files.stack.list_scroll)
            .p_1p5()
            .flex_1()
            .into_any_element()
        };
        let mode_gen = self.files.stack.mode_generation;
        let reduced = self.reduced();
        let rows_wrapper = if mode_gen > 0 {
            let travel = if reduced { 0. } else { 1. };
            let duration = if reduced {
                std::time::Duration::from_millis(1)
            } else {
                std::time::Duration::from_millis(160)
            };
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(rows)
                .with_animation(
                    gpui::ElementId::Name(format!("changes-mode-swap-{mode_gen}").into()),
                    gpui::Animation::new(duration).with_easing(crate::motion::ease_out_strong),
                    move |el, d| {
                        let dy = 5. * (1. - d) * travel;
                        el.opacity(d).relative().top(zpx(dy))
                    },
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(rows)
                .into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(summary)
            .child(rows_wrapper)
            .into_any_element()
    }

    fn render_change_detail(
        &mut self,
        path: &str,
        list: &Rc<Vec<Change>>,
        width: f32,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let change = list.iter().find(|c| c.path == path).cloned();
        let (dir, name) = path.rsplit_once('/').map_or(("", path), |(d, n)| (d, n));
        let clean_dir = shown_dir(dir);
        let tab = self.files.change_detail_tab;
        let file = self.files.file.as_ref().filter(|f| f.path == path);
        let loaded = file.is_some();
        let (dirty, saving, conflict, error) = file.map_or((false, false, false, None), |f| {
            (f.dirty, f.saving, f.conflict, f.error.clone())
        });
        let just_saved = file
            .and_then(|f| f.saved_at)
            .filter(|at| at.elapsed() < SAVED_FOR)
            .is_some();
        let markdown = crate::files::FileKind::of(std::path::Path::new(path))
            == crate::files::FileKind::Markdown;
        let preview = markdown && loaded && !self.files.viewer.md_source;
        let reduced = self.reduced();

        let back_btn = icon_button("changes-detail-back", Icon::Back, t)
            .tooltip(move |_, cx| {
                cx.new(|_| crate::view::dotify_ui::Tip::new("Back to changed files", t))
                    .into()
            })
            .tooltip_show_delay(crate::view::tooltip::delay())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.files.selected = None;
                cx.notify();
            }));

        let tab_toggle = seg_track(t)
            .child(
                seg_item(
                    "change-tab-diff",
                    "Diff",
                    None,
                    tab == ChangeDetailTab::Diff,
                    t,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.files.change_detail_tab = ChangeDetailTab::Diff;
                    cx.notify();
                })),
            )
            .child(
                seg_item(
                    "change-tab-file",
                    "File",
                    None,
                    tab == ChangeDetailTab::File,
                    t,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.files.change_detail_tab = ChangeDetailTab::File;
                    if let Some(selected) = &this.files.selected {
                        let path = selected.clone();
                        if viewer::is_image(&path) {
                            this.files_locate(path, cx);
                        } else {
                            this.files_open_file(path, window, cx);
                        }
                    }
                    cx.notify();
                })),
            );

        let header =
            div()
                .h(HEADER_HEIGHT)
                .flex_none()
                .pl_2()
                .pr_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(c(t.border).opacity(0.7))
                .child(back_btn)
                .child(
                    div()
                        .flex_none()
                        .text_color(c(t.text_faint))
                        .child(Icon::for_path(name).el(zpx(14.))),
                )
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
                                .child(clean_dir.to_string()),
                        ),
                )
                .child(tab_toggle)
                .when(tab == ChangeDetailTab::File && just_saved && !dirty, |d| {
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
                .when(tab == ChangeDetailTab::File && dirty && !conflict, |d| {
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
                .when(tab == ChangeDetailTab::File && markdown && loaded, |d| {
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
                })
                .child(diffstat(
                    change.as_ref().and_then(|c| c.added),
                    change.as_ref().and_then(|c| c.removed),
                    t,
                ))
                .child(status_chip(change.as_ref().map_or('M', |c| c.status), t));

        let banner =
            (tab == ChangeDetailTab::File && conflict).then(|| {
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

        let error_banner = (tab == ChangeDetailTab::File)
            .then(|| error)
            .flatten()
            .map(|e| {
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

        let body: AnyElement = match tab {
            ChangeDetailTab::Diff => {
                self.render_single_file_diff(path, change.as_ref(), width, t, window, cx)
            }
            ChangeDetailTab::File => self.render_file_view_body(path, width, t, window, cx),
        };

        div()
            .flex_1()
            .min_w_0()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .children(banner)
            .children(error_banner)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(body),
            )
            .into_any_element()
    }

    fn render_single_file_diff(
        &mut self,
        path: &str,
        change: Option<&Change>,
        width: f32,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if viewer::is_image(path) {
            let deleted = change.map_or(false, |c| c.status == 'D');
            return div()
                .px_4()
                .py_6()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .text_color(c(t.text_faint))
                        .child(Icon::Image.el(zpx(16.))),
                )
                .child(
                    div()
                        .flex_1()
                        .t_small()
                        .text_color(c(t.text_muted))
                        .child(if deleted {
                            "Image deleted"
                        } else {
                            "Image changed"
                        }),
                )
                .when(!deleted, |d| {
                    d.child(
                        text_button("diff-open-image", "Open in viewer", t).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.files.change_detail_tab = ChangeDetailTab::File;
                                cx.notify();
                            },
                        )),
                    )
                })
                .into_any_element();
        }

        match self.files.stack.docs.get(path) {
            None | Some(Load::Loading) => {
                self.files_want_diff(path.to_string(), cx);
                self.render_code_skeleton("diff-single-skel", t)
            }
            Some(Load::Failed(err)) => {
                empty_state(Icon::Alert, "Couldn't load diff", err.clone(), t)
            }
            Some(Load::Ready(doc)) if doc.lines.is_empty() => empty_state(
                Icon::FileDiff,
                "No line changes",
                "It's binary, or only its mode or name changed.",
                t,
            ),
            Some(Load::Ready(doc)) => {
                let zoom = crate::style::zoom();
                let split = self
                    .files
                    .stack
                    .force_split
                    .unwrap_or(width >= SPLIT_AT * zoom);
                let doc = doc.clone();
                let reduced = self.reduced();
                let ch = change.cloned().unwrap_or_else(|| Change {
                    path: path.to_string(),
                    status: 'M',
                    added: None,
                    removed: None,
                });
                let (list_state, items_rc) = {
                    let s = &mut self.files.stack;
                    let limit = s.shown.get(path).copied().unwrap_or(FIRST_ROWS);
                    let items = build_detail_items(
                        0,
                        &ch,
                        Some(&Load::Ready(doc.clone())),
                        Some(limit),
                        split,
                    );
                    let reset = s.split != split || s.items.len() != items.len();
                    s.split = split;
                    s.items = Rc::new(items);
                    if reset {
                        s.list.reset(s.items.len());
                    }
                    (s.list.clone(), s.items.clone())
                };

                let dark = matches!(
                    window.appearance(),
                    gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
                );
                let syntax = keel_theme::resolve_syntax_theme(self.state.ui_settings.theme, dark);
                let digits = doc
                    .lines
                    .iter()
                    .filter_map(|l| l.new.max(l.old))
                    .max()
                    .unwrap_or(1)
                    .to_string()
                    .len()
                    .max(2);
                let snap = Rc::new(Snap {
                    docs: vec![Some(doc)],
                    digits: vec![digits],
                });
                let ctx = Rc::new(Ctx {
                    t,
                    syntax,
                    items: items_rc,
                    files: Rc::new(vec![ch]),
                    snap,
                    weak: cx.entity().downgrade(),
                    reduced,
                });
                let rows_ctx = ctx.clone();
                div()
                    .id("files-single-diff")
                    .size_full()
                    .child(
                        gpui::list(list_state, move |ix, _, _| {
                            render_detail_item(&rows_ctx, ix)
                        })
                        .size_full(),
                    )
                    .into_any_element()
            }
        }
    }
}

fn render_detail_item(cx: &Ctx, ix: usize) -> AnyElement {
    let Some(&it) = cx.items.get(ix) else {
        return div().into_any_element();
    };
    render_body(cx, it, ix == 0)
}

fn render_body(cx: &Ctx, it: Item, first: bool) -> AnyElement {
    let doc = |f: usize| cx.snap.docs.get(f).and_then(|d| d.clone());
    match it {
        Item::Line(f, i) => match doc(f) {
            Some(doc) if doc.lines[i].kind == LineKind::Hunk => hunk_row(cx, f, i, &doc, first),
            Some(doc) => unified_row(cx, f, i, &doc),
            None => div().into_any_element(),
        },
        Item::Pair(f, a, b) => match doc(f) {
            Some(doc) => pair_row(cx, f, a, b, &doc),
            None => div().into_any_element(),
        },
        Item::Note(f, note) => note_row(cx, f, note),
        Item::More(f, hidden) => more_row(cx, f, hidden),
    }
}

/// The sides of a section: rows sit between its borders.
fn shell(t: Tokens) -> gpui::Div {
    div().bg(c(t.window))
}

fn row_id(f: usize, i: usize) -> usize {
    (f << 32) | i
}

fn num_w(cx: &Ctx, f: usize) -> Pixels {
    zpx(12. + cx.snap.digits.get(f).copied().unwrap_or(2) as f32 * 7.)
}

fn num_cell(n: Option<u32>, w: Pixels, strong: bool, t: Tokens) -> gpui::Div {
    div()
        .flex_none()
        .w(w)
        .pr_2()
        .py(zpx(2.))
        .text_right()
        .t_caption()
        .line_height(zpx(CODE_LH))
        .font_family(MONO)
        .text_color(c(if strong { t.text_muted } else { t.text_faint }))
        .when(!strong, |d| d.opacity(0.7))
        .child(n.map(|n| n.to_string()).unwrap_or_default())
}

fn mark_cell(mark: &'static str, tone: Option<u32>, t: Tokens) -> gpui::Div {
    div()
        .flex_none()
        .w(zpx(18.))
        .py(zpx(2.))
        .t_code()
        .line_height(zpx(CODE_LH))
        .flex()
        .justify_center()
        .text_color(tone.map_or(c(t.text_faint), |tone| c(tone)))
        .child(mark)
}

fn code_cell(line: &DiffLine, tone: Option<u32>, cx: &Ctx) -> gpui::Div {
    let t = cx.t;
    let code: AnyElement = if line.kind == LineKind::Note {
        div()
            .text_color(c(t.text_faint))
            .italic()
            .child(line.text.clone())
            .into_any_element()
    } else {
        let emph_bg = c(tone.unwrap_or(t.accent)).opacity(0.28);
        gpui::StyledText::new(line.text.clone())
            .with_highlights(paint_runs(line, &cx.syntax, emph_bg))
            .into_any_element()
    };
    div()
        .flex_1()
        .min_w_0()
        .pr_3()
        .py(zpx(2.))
        .t_code()
        .line_height(zpx(CODE_LH))
        .text_color(c(cx.syntax.plain << 8 | 0xFF))
        .child(code)
}

fn tone_of(kind: LineKind, t: Tokens) -> (Option<u32>, &'static str) {
    match kind {
        LineKind::Added => (Some(t.success), "+"),
        LineKind::Removed => (Some(t.danger), "\u{2212}"),
        _ => (None, ""),
    }
}

/// The gutter: a 2px edge in the line's colour, then a slightly deeper
/// wash under the numbers.
fn gutter(tone: Option<u32>, _t: Tokens) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .flex_row()
        .border_l_2()
        .border_color(match tone {
            Some(tone) => c(tone).opacity(0.85),
            _ => gpui::transparent_black(),
        })
        .when_some(tone, |d, tone| d.bg(c(tone).opacity(0.1)))
}

/// One line, unified: both numbers on a tinted gutter, the mark, then the
/// code, wrapped (never scrolled sideways).
fn unified_row(cx: &Ctx, f: usize, ix: usize, doc: &DiffDoc) -> AnyElement {
    let t = cx.t;
    let line = &doc.lines[ix];
    let (tone, mark) = tone_of(line.kind, t);
    let w = num_w(cx, f);
    shell(t)
        .child(
            div()
                .id(("diff-line", row_id(f, ix)))
                .group("diff-row")
                .relative()
                .min_h(zpx(LINE_H))
                .flex()
                .flex_row()
                .when_some(tone, |d, tone| d.bg(c(tone).opacity(0.09)))
                .child(
                    gutter(tone, t)
                        .child(num_cell(line.old, w, tone.is_some(), t))
                        .child(num_cell(line.new, w, tone.is_some(), t)),
                )
                .child(mark_cell(mark, tone, t))
                .child(code_cell(line, tone, cx)),
        )
        .into_any_element()
}

/// Old and new side by side. A missing side is a quiet blank.
fn pair_row(cx: &Ctx, f: usize, a: Option<usize>, b: Option<usize>, doc: &DiffDoc) -> AnyElement {
    let t = cx.t;
    let w = num_w(cx, f);
    let half = |ix: Option<usize>, old_side: bool| -> AnyElement {
        let Some(ix) = ix else {
            return div()
                .flex_1()
                .min_w_0()
                .bg(c(t.text).opacity(0.03))
                .into_any_element();
        };
        let line = &doc.lines[ix];
        let (tone, mark) = tone_of(line.kind, t);
        let n = if old_side { line.old } else { line.new };
        div()
            .id(("diff-half", row_id(f, ix) * 2 + old_side as usize))
            .group("diff-row")
            .relative()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_row()
            .when_some(tone, |d, tone| d.bg(c(tone).opacity(0.09)))
            .child(gutter(tone, t).child(num_cell(n, w, tone.is_some(), t)))
            .child(mark_cell(mark, tone, t))
            .child(code_cell(line, tone, cx))
            .into_any_element()
    };
    shell(t)
        .child(
            div()
                .id(("diff-pair", row_id(f, a.or(b).unwrap_or(0))))
                .min_h(zpx(LINE_H))
                .flex()
                .flex_row()
                .child(half(a, true))
                .child(div().flex_none().w(px(1.)).bg(c(t.border)))
                .child(half(b, false)),
        )
        .into_any_element()
}

/// `@@ -a,b +c,d @@ scope`: a thin band with the scope git found, and when
/// lines are hidden above it, a way to show them.
fn hunk_row(cx: &Ctx, f: usize, ix: usize, doc: &DiffDoc, first: bool) -> AnyElement {
    let t = cx.t;
    let line = &doc.lines[ix];
    let text = line.text.as_ref();
    let scope = text
        .get(2..)
        .and_then(|s| s.find("@@"))
        .map_or("", |i| text[i + 4..].trim());
    let gap = line.gap;
    let label = match gap {
        0 if scope.is_empty() => text.to_string(),
        0 => String::new(),
        1 => "Show 1 unchanged line".into(),
        n => format!("Show {} unchanged lines", grouped(n)),
    };
    let (weak, path) = (cx.weak.clone(), cx.files[f].path.clone());
    shell(t)
        .child(
            div()
                .id(("diff-hunk", row_id(f, ix)))
                .group("diff-hunk")
                .h(zpx(LINE_H + 4.))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .bg(c(t.text).opacity(0.035))
                .when(!first, |d| d.border_t_1())
                .border_b_1()
                .border_color(c(t.border).opacity(0.6))
                .t_caption()
                .whitespace_nowrap()
                .overflow_hidden()
                .when(gap > 0, |d| {
                    d.cursor_pointer()
                        .hover_fade(c(t.accent).opacity(0.08), None)
                        .on_click(move |_, _, cx| {
                            let path = path.clone();
                            let _ = weak.update(cx, |this, cx| this.files_expand_gap(path, ix, cx));
                        })
                })
                .child(
                    div()
                        .flex_none()
                        .w(num_w(cx, f) * 2. + zpx(2.))
                        .flex()
                        .justify_center()
                        .text_color(c(t.text_faint))
                        .when(gap > 0, |d| {
                            d.group_hover("diff-hunk", |s| s.text_color(c(t.accent)))
                                .child(Icon::ArrowUpDown.el(zpx(12.)))
                        }),
                )
                .when(!label.is_empty(), |d| {
                    d.child(
                        div()
                            .flex_none()
                            .text_color(c(t.text_muted))
                            .when(gap > 0, |d| {
                                d.group_hover("diff-hunk", |s| s.text_color(c(t.accent)))
                            })
                            .child(label),
                    )
                })
                .when(!scope.is_empty(), |d| {
                    d.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .pr_3()
                            .font_family(MONO)
                            .text_color(c(t.text_faint))
                            .child(scope.to_string()),
                    )
                }),
        )
        .into_any_element()
}

fn note_text(t: Tokens, text: &'static str) -> gpui::Div {
    div()
        .px_4()
        .py_3()
        .t_small()
        .text_color(c(t.text_muted))
        .child(text)
}

/// What a section says when it has no lines to show.
fn note_row(cx: &Ctx, f: usize, note: Note) -> AnyElement {
    let t = cx.t;
    let body: AnyElement = match note {
        Note::Loading => div()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap(zpx(7.))
            .children([0.46, 0.7, 0.34].into_iter().enumerate().map(|(i, w)| {
                crate::motion::shimmer(
                    div()
                        .h(zpx(8.))
                        .w(gpui::relative(w))
                        .rounded(px(3.))
                        .bg(c(t.text).opacity(0.06)),
                    ("diff-skel", row_id(f, i)),
                    i as f32 * 0.05,
                    c(t.text).opacity(0.05),
                    cx.reduced,
                )
            }))
            .into_any_element(),
        Note::Failed => {
            note_text(t, "Couldn't load this diff. Refresh to try again.").into_any_element()
        }
        Note::Empty => note_text(
            t,
            "No line changes. It's binary, or only its mode or name changed.",
        )
        .into_any_element(),
        Note::Truncated => note_text(t, "The rest of this diff is too long to show.")
            .border_t_1()
            .border_color(c(t.border).opacity(0.6))
            .into_any_element(),
        Note::Image => {
            let ch = &cx.files[f];
            let (weak, path) = (cx.weak.clone(), ch.path.clone());
            let deleted = ch.status == 'D';
            div()
                .px_3()
                .py_3()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .text_color(c(t.text_faint))
                        .child(Icon::Image.el(zpx(14.))),
                )
                .child(
                    div()
                        .flex_1()
                        .t_small()
                        .text_color(c(t.text_muted))
                        .child(if deleted {
                            "Image deleted"
                        } else {
                            "Image changed"
                        }),
                )
                .when(!deleted, |d| {
                    d.child(
                        text_button("diff-open-image", "Open in viewer", t).on_click(
                            move |_, window, cx| {
                                let path = path.clone();
                                let _ = weak.update(cx, |this, cx| {
                                    this.files_reveal(path, true, window, cx)
                                });
                            },
                        ),
                    )
                })
                .into_any_element()
        }
    };
    shell(t).child(body).into_any_element()
}

/// A long diff's first lines are shown; this shows the next batch.
fn more_row(cx: &Ctx, f: usize, hidden: usize) -> AnyElement {
    let t = cx.t;
    let (weak, path) = (cx.weak.clone(), cx.files[f].path.clone());
    let next = hidden.min(MORE_ROWS);
    shell(t)
        .child(
            div()
                .id(("diff-more", f))
                .h(zpx(LINE_H + 8.))
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap_1p5()
                .border_t_1()
                .border_color(c(t.border).opacity(0.6))
                .t_caption()
                .text_color(c(t.text_muted))
                .cursor_pointer()
                .hover(|s| s.text_color(c(t.text)))
                .hover_fade(c(t.hover), None)
                .child(format!("Show {} more lines", grouped(next as u32)))
                .when(hidden > next, |d| {
                    d.child(
                        div()
                            .text_color(c(t.text_faint))
                            .child(format!("of {}", grouped(hidden as u32))),
                    )
                })
                .on_click(move |_, _, cx| {
                    let path = path.clone();
                    let _ = weak.update(cx, |this, cx| this.files_show_more(path, cx));
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(path: &str, status: char, a: u32, r: u32) -> Change {
        Change {
            path: path.into(),
            status,
            added: Some(a),
            removed: Some(r),
        }
    }

    #[test]
    fn tree_joins_single_folders_and_sums_counts() {
        let list = vec![
            ch("src/view/a.rs", 'M', 3, 1),
            ch("src/view/b.rs", 'A', 2, 0),
            ch("README.md", 'M', 1, 1),
        ];
        let rows = change_tree(&list, &HashSet::new());
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["src/view", "a.rs", "b.rs", "README.md"]);
        assert_eq!((rows[0].added, rows[0].removed, rows[0].count), (5, 1, 2));
        assert_eq!(rows[1].depth, 1);
        let folded: HashSet<String> = ["src/view".to_string()].into();
        assert_eq!(change_tree(&list, &folded).len(), 2);
    }

    #[test]
    fn folds_lockfiles_generated_deleted_and_huge() {
        assert_eq!(closed_why(&ch("Cargo.lock", 'M', 1, 1)), Some("Lockfile"));
        assert_eq!(
            closed_why(&ch("web/app.min.js", 'M', 1, 1)),
            Some("Generated")
        );
        assert_eq!(closed_why(&ch("old.rs", 'D', 0, 9)), Some("Deleted"));
        assert_eq!(
            closed_why(&ch("big.rs", 'M', 1400, 200)),
            Some("Large diff")
        );
        assert_eq!(closed_why(&ch("src/main.rs", 'M', 10, 2)), None);
        let mut open = HashMap::new();
        open.insert("Cargo.lock".to_string(), true);
        assert!(is_open(&ch("Cargo.lock", 'M', 1, 1), &open));
    }

    #[test]
    fn split_pairs_runs_and_spans_hunks() {
        let (lines, _) = parse_patch("@@ -1,4 +1,5 @@\n a\n-b\n-c\n+B\n+C\n+D\n e\n");
        let mut out = Vec::new();
        push_split(0, &lines, &mut out);
        assert_eq!(
            out,
            [
                Item::Line(0, 0),
                Item::Pair(0, Some(1), Some(1)),
                Item::Pair(0, Some(2), Some(4)),
                Item::Pair(0, Some(3), Some(5)),
                Item::Pair(0, None, Some(6)),
                Item::Pair(0, Some(7), Some(7)),
            ]
        );
    }

    #[test]
    fn detail_items_builds_lines_without_outer_accordion_chrome() {
        let (lines, _) = parse_patch("@@ -1,2 +1,2 @@\n-old\n+new\n");
        let doc = Rc::new(DiffDoc {
            lines,
            truncated: false,
        });
        let items = build_detail_items(
            0,
            &ch("a.rs", 'M', 1, 1),
            Some(&Load::Ready(doc)),
            None,
            false,
        );
        assert_eq!(
            items,
            [Item::Line(0, 0), Item::Line(0, 1), Item::Line(0, 2)]
        );
    }

    #[test]
    fn list_row_counts_a_header_per_folder() {
        let list = vec![
            ch("@web/a.ts", 'M', 1, 0),
            ch("@web/b.ts", 'M', 1, 0),
            ch("@api/c.ts", 'A', 2, 0),
        ];
        assert_eq!(change_list_row(&list, 0), 1);
        assert_eq!(change_list_row(&list, 1), 2);
        assert_eq!(change_list_row(&list, 2), 4);
        assert_eq!(change_list_row(&[ch("a.ts", 'M', 1, 0)], 0), 0);
        assert_eq!(shown_dir("@web/src"), "web/src");
    }
}

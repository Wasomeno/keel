//! The Files tab's viewer beyond the editor: images shown as themselves
//! (fit to the pane on a checkerboard, or at actual size), Markdown rendered
//! the way the transcript renders it, and the reveal that walks the folder
//! tree to a file and lights its row for a moment.

use super::*;
use gpui::{AnimationExt, ListAlignment, ListState, list};
use std::path::PathBuf;

/// How long a revealed row stays lit (it fades out over the back half).
const GLOW: Duration = Duration::from_millis(1400);
/// Image formats the viewer draws.
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg"];

pub(super) fn is_image(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, e)| IMAGE_EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// An image on disk: where, how big, and its pixel size when it could be read.
#[derive(Clone)]
pub(super) struct ImageInfo {
    abs: PathBuf,
    bytes: u64,
    dims: Option<(u32, u32)>,
}

/// Rendered Markdown for one file, re-parsed only when its text changes.
struct MdDoc {
    path: String,
    key: u64,
    items: Rc<Vec<crate::transcript::TranscriptRowItem>>,
    list: ListState,
}

pub(super) struct Viewer {
    /// Markdown: the source in the editor instead of the rendered page.
    pub(super) md_source: bool,
    /// Images: actual size instead of fit to the pane.
    pub(super) actual: bool,
    /// The Files tab is one column (the tree or the viewer).
    pub(super) narrow: bool,
    /// The file couldn't be opened, and why.
    pub(super) open_error: Option<(String, String)>,
    /// Narrow pane, a reveal without opening: show the tree, not the viewer.
    pub(super) reveal: Option<String>,
    /// A reveal waiting for its row (folders still loading).
    pending: Option<String>,
    /// The lit row: path, which reveal, since when.
    glow_at: Option<(String, u64, Instant)>,
    reveals: u64,
    pub(super) tree_scroll: UniformListScrollHandle,
    images: HashMap<String, Load<ImageInfo>>,
    md: Option<MdDoc>,
    /// The image area's size, measured as it paints (fit needs it).
    area: Rc<std::cell::Cell<(f32, f32)>>,
}

impl Viewer {
    pub(super) fn new() -> Self {
        Self {
            md_source: false,
            actual: false,
            narrow: false,
            open_error: None,
            reveal: None,
            pending: None,
            glow_at: None,
            reveals: 0,
            tree_scroll: UniformListScrollHandle::new(),
            images: HashMap::new(),
            md: None,
            area: Rc::new(std::cell::Cell::new((0., 0.))),
        }
    }

    /// Another chat: keep only the Preview / Source choice.
    pub(super) fn clear(&mut self) {
        let md_source = self.md_source;
        *self = Self::new();
        self.md_source = md_source;
    }

    /// The row lit by the last reveal, while it's lit.
    pub(super) fn glow(&self) -> Option<(String, u64)> {
        self.glow_at
            .as_ref()
            .filter(|(_, _, at)| at.elapsed() < GLOW)
            .map(|(p, n, _)| (p.clone(), *n))
    }

    /// Dev: what the viewer shows.
    pub(super) fn state_json(&self) -> serde_json::Value {
        serde_json::json!({
            "md_source": self.md_source,
            "actual": self.actual,
            "narrow": self.narrow,
            "reveal": self.reveal,
            "pending": self.pending,
            "glow": self.glow().map(|(p, _)| p),
            "images": self.images.len(),
            "md_rows": self.md.as_ref().map(|m| m.items.len()),
        })
    }
}

/// The wash on a revealed row: in at once, held, then out. Opacity only, so
/// it costs a few frames and a click elsewhere cuts it short (the row redraws
/// without it).
pub(super) fn reveal_glow(n: u64, t: Tokens, reduced: bool) -> AnyElement {
    let wash = div()
        .absolute()
        .inset_0()
        .rounded(RADIUS_MD)
        .bg(c(t.accent).opacity(0.16))
        .border_1()
        .border_color(c(t.accent).opacity(0.45));
    if reduced {
        return wash.into_any_element();
    }
    wash.with_animation(
        ("files-reveal-glow", n as usize),
        gpui::Animation::new(GLOW),
        |el, d| {
            // Hold for the first half, then ease out.
            let fade = ((d - 0.5) * 2.).clamp(0., 1.);
            el.opacity(1. - crate::motion::ease_out(fade))
        },
    )
    .into_any_element()
}

/// The pane's checkerboard behind an image, so transparency reads as such.
fn checker(t: Tokens) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let s = zpx(8.);
            let tone = c(t.text).opacity(0.05);
            let cols = (f32::from(bounds.size.width) / f32::from(s)).ceil() as usize;
            let rows = (f32::from(bounds.size.height) / f32::from(s)).ceil() as usize;
            for r in 0..rows {
                for k in (r % 2..cols).step_by(2) {
                    let cell = gpui::Bounds::new(
                        gpui::point(
                            bounds.origin.x + s * k as f32,
                            bounds.origin.y + s * r as f32,
                        ),
                        gpui::size(s, s),
                    )
                    .intersect(&bounds);
                    window.paint_quad(gpui::fill(cell, tone));
                }
            }
        },
    )
    .absolute()
    .size_full()
}

fn human_bytes(n: u64) -> String {
    match n {
        0..1_000 => format!("{n} B"),
        1_000..1_000_000 => format!("{:.1} KB", n as f64 / 1_000.).replace(".0 ", " "),
        _ => format!("{:.1} MB", n as f64 / 1_000_000.).replace(".0 ", " "),
    }
}

/// An image's pixel size, read from its header (or an SVG's own size).
fn image_dims(path: &std::path::Path) -> Option<(u32, u32)> {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
    {
        let data = std::fs::read(path).ok()?;
        let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
        let size = tree.size();
        return Some((size.width().round() as u32, size.height().round() as u32));
    }
    image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

fn text_key(path: &str, text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    text.hash(&mut h);
    h.finish()
}

/// Folders from the top down to `path`'s own: `a`, `a/b` for `a/b/c.rs`.
fn ancestors(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut acc = String::new();
    let parts: Vec<&str> = path.split('/').collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(part);
        out.push(acc.clone());
    }
    out
}

impl KeelWindow {
    /// Where a file sits in the codebase: the Files tab, its folders opened
    /// down to it, its row scrolled to the middle and lit for a moment. It
    /// opens in the viewer too when asked, or when there's room for both
    /// (not a deleted file: there's nothing to open).
    pub(super) fn files_reveal(
        &mut self,
        path: String,
        select: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.files.tab = FilesTab::Files;
        if !self.files.dirs.contains_key("") {
            self.files_load_dir(String::new(), cx);
        }
        for dir in ancestors(&path) {
            self.files.expanded.insert(dir.clone());
            if !matches!(
                self.files.dirs.get(&dir),
                Some(Load::Ready(_) | Load::Loading)
            ) {
                self.files_load_dir(dir, cx);
            }
        }
        self.files.tree_cache = None;
        let deleted = self.files.change(&path).is_some_and(|c| c.status == 'D');
        let open = !deleted && (select || !self.files.viewer.narrow);
        let v = &mut self.files.viewer;
        v.pending = Some(path.clone());
        v.reveal = (!open).then(|| path.clone());
        if open && self.files.selected.as_deref() != Some(path.as_str()) {
            self.files_select(path, window, cx);
        }
        cx.notify();
    }

    /// A reveal waiting on the tree: once the row is there (or its folders
    /// have loaded and it isn't, for a deleted file: its nearest folder),
    /// scroll to it and light it.
    pub(super) fn files_reveal_scroll(&mut self, rows: &Rc<Vec<TreeRow>>, _cx: &mut Context<Self>) {
        let Some(path) = self.files.viewer.pending.clone() else {
            return;
        };
        let dirs = ancestors(&path);
        let found = rows
            .iter()
            .position(|r| r.path == path)
            .map(|ix| (ix, path.clone()))
            .or_else(|| {
                let loading = std::iter::once("")
                    .chain(dirs.iter().map(String::as_str))
                    .any(|d| {
                        !matches!(
                            self.files.dirs.get(d),
                            Some(Load::Ready(_) | Load::Failed(_))
                        )
                    });
                if loading {
                    return None;
                }
                dirs.iter().rev().find_map(|d| {
                    rows.iter()
                        .position(|r| r.path == *d)
                        .map(|ix| (ix, d.clone()))
                })
            });
        let Some((ix, lit)) = found else {
            if dirs.iter().all(|d| {
                matches!(
                    self.files.dirs.get(d.as_str()),
                    Some(Load::Ready(_) | Load::Failed(_))
                )
            }) && self.files.dirs.contains_key("")
            {
                self.files.viewer.pending = None;
            }
            return;
        };
        let v = &mut self.files.viewer;
        v.pending = None;
        v.reveals += 1;
        v.glow_at = Some((lit, v.reveals, Instant::now()));
        v.tree_scroll
            .scroll_to_item(ix, gpui::ScrollStrategy::Center);
    }

    /// Where an image is on disk and how big, read once per file.
    pub(super) fn files_locate(&mut self, path: String, cx: &mut Context<Self>) {
        if matches!(
            self.files.viewer.images.get(&path),
            Some(Load::Ready(_) | Load::Loading)
        ) {
            return;
        }
        let (Some(chat), Some(client)) = (self.files.chat.clone(), self.client.clone()) else {
            return;
        };
        self.files.viewer.images.insert(path.clone(), Load::Loading);
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "chat_file_info",
                    serde_json::json!({ "chat_id": chat, "path": path }),
                )
                .await;
            let info = match res {
                Ok(v) => {
                    let abs = PathBuf::from(v["path"].as_str().unwrap_or_default());
                    let bytes = v["bytes"].as_u64().unwrap_or(0);
                    let p = abs.clone();
                    let dims = cx.background_spawn(async move { image_dims(&p) }).await;
                    Load::Ready(ImageInfo { abs, bytes, dims })
                }
                Err(err) => Load::Failed(err),
            };
            let _ = this.update(cx, |this, cx| {
                if this.files.chat.as_deref() == Some(chat.as_str()) {
                    this.files.viewer.images.insert(path, info);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// An image as itself: centred on a quiet surface over a checkerboard,
    /// fit to the pane (never blown up past its pixels, but an SVG may grow a
    /// little), or at actual size and scrollable. A status line gives its
    /// size, weight and format.
    pub(super) fn render_image_view(
        &mut self,
        path: &str,
        width: f32,
        t: Tokens,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let info = match self.files.viewer.images.get(path) {
            Some(Load::Ready(info)) => info.clone(),
            Some(Load::Failed(err)) => {
                return empty_state(Icon::Image, "Can't show this image", err.clone(), t);
            }
            Some(Load::Loading) => return self.render_code_skeleton("files-image-skel", t),
            None => {
                self.files_locate(path.to_string(), cx);
                return self.render_code_skeleton("files-image-skel", t);
            }
        };
        let zoom = crate::style::zoom();
        let pad = 24. * zoom;
        let (aw, ah) = match self.files.viewer.area.get() {
            (w, h) if w > 0. && h > 0. => (w, h),
            _ => (width, 420. * zoom),
        };
        let (aw, ah) = ((aw - 2. * pad).max(40.), (ah - 2. * pad).max(40.));
        let svg = path
            .rsplit_once('.')
            .is_some_and(|(_, e)| e.eq_ignore_ascii_case("svg"));
        let fit = info.dims.map(|(w, h)| {
            let (w, h) = (w.max(1) as f32, h.max(1) as f32);
            let grow = if svg {
                (160. * zoom / w.max(h)).max(1.)
            } else {
                1.
            };
            (aw / w).min(ah / h).min(grow)
        });
        let actual = self.files.viewer.actual;
        let scale = if actual { Some(1.) } else { fit };
        let shrunk = fit.is_some_and(|f| f < 0.999);
        let picture = match (info.dims, scale) {
            (Some((w, h)), Some(s)) => {
                let (w, h) = (
                    px((w as f32 * s).round().max(1.)),
                    px((h as f32 * s).round().max(1.)),
                );
                div()
                    .flex_none()
                    .relative()
                    .w(w)
                    .h(h)
                    .rounded(px(4.))
                    .overflow_hidden()
                    .border_1()
                    .border_color(c(t.border))
                    .bg(c(t.window))
                    .child(checker(t))
                    .child(
                        gpui::img(info.abs.clone())
                            .absolute()
                            .inset_0()
                            .size_full()
                            .object_fit(gpui::ObjectFit::Contain),
                    )
                    .into_any_element()
            }
            // Its size couldn't be read: let the image size itself.
            _ => div()
                .flex_none()
                .max_w(px(aw))
                .max_h(px(ah))
                .child(
                    gpui::img(info.abs.clone())
                        .max_w(px(aw))
                        .max_h(px(ah))
                        .object_fit(gpui::ObjectFit::Contain),
                )
                .into_any_element(),
        };
        let area = self.files.viewer.area.clone();
        // Measure the area as it paints; a new size draws once more to fit it.
        let measure = gpui::canvas(
            move |bounds, window, _| {
                let now = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                if (now.0 - area.get().0).abs() > 0.5 || (now.1 - area.get().1).abs() > 0.5 {
                    area.set(now);
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let stage = div()
            .id("files-image-stage")
            .flex_1()
            .min_h_0()
            .relative()
            .bg(c(t.surface))
            .child(measure)
            .map(|d| {
                if actual {
                    d.overflow_scroll().child(
                        div()
                            .p(px(pad))
                            .flex()
                            .min_w_full()
                            .min_h_full()
                            .items_center()
                            .justify_center()
                            .child(picture),
                    )
                } else {
                    d.overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(px(pad))
                        .child(picture)
                }
            });
        let ext = path
            .rsplit_once('.')
            .map_or(String::new(), |(_, e)| e.to_ascii_uppercase());
        let facts = [
            info.dims
                .map(|(w, h)| format!("{} \u{00D7} {}", grouped(w), grouped(h))),
            Some(human_bytes(info.bytes)),
            Some(if ext == "JPEG" { "JPG".into() } else { ext }),
        ];
        let status = div()
            .flex_none()
            .h(zpx(30.))
            .pl_3()
            .pr_1p5()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .border_t_1()
            .border_color(c(t.border).opacity(0.7))
            .t_caption()
            .text_color(c(t.text_faint))
            .children(
                facts
                    .into_iter()
                    .flatten()
                    .map(|f| div().flex_none().child(f)),
            )
            .child(div().flex_1())
            .when_some(fit.filter(|_| shrunk && !actual), |d, f| {
                d.child(
                    div()
                        .flex_none()
                        .child(format!("{}%", (f * 100.).round() as u32)),
                )
            })
            .when(shrunk, |d| {
                d.child(
                    seg_track(t)
                        .child(
                            seg_item("files-image-fit", "Fit", None, !actual, t).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.files.viewer.actual = false;
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(
                            seg_item("files-image-actual", "Actual size", None, actual, t)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.files.viewer.actual = true;
                                    cx.notify();
                                })),
                        ),
                )
            });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(stage)
            .child(status)
            .into_any_element()
    }

    /// Markdown as the transcript draws it: the same headings, lists, code
    /// blocks (with copy and folding) and images. It follows unsaved edits.
    pub(super) fn render_md_preview(
        &mut self,
        path: &str,
        width: f32,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(file) = self.files.file.as_ref().filter(|f| f.path == path) else {
            return self.render_code_skeleton("files-md-skel", t);
        };
        let text = if file.dirty {
            file.editor.read(cx).text().to_string()
        } else {
            file.saved.clone()
        };
        if text.trim().is_empty() {
            return empty_state(
                Icon::FileText,
                "Empty file",
                "Switch to Source to write it.",
                t,
            );
        }
        let key = text_key(path, &text);
        if self.files.viewer.md.as_ref().is_none_or(|m| m.key != key) {
            let born = Instant::now();
            let items: Vec<_> = crate::markdown::parse_markdown_blocks("files-md", &text)
                .into_iter()
                .map(|block| crate::transcript::TranscriptRowItem {
                    message_id: "files-md".into(),
                    role: "assistant".into(),
                    block,
                    born,
                    enter: None,
                    leaving: None,
                })
                .collect();
            match &mut self.files.viewer.md {
                Some(m) if m.path == path => {
                    m.list.splice(0..m.items.len(), items.len());
                    m.items = Rc::new(items);
                    m.key = key;
                }
                slot => {
                    let list = ListState::new(items.len(), ListAlignment::Top, px(1200.));
                    *slot = Some(MdDoc {
                        path: path.to_string(),
                        key,
                        items: Rc::new(items),
                        list,
                    });
                }
            }
        }
        let Some(md) = self.files.viewer.md.as_ref() else {
            return div().into_any_element();
        };
        let dark = matches!(
            window.appearance(),
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
        );
        let syntax = keel_theme::resolve_syntax_theme(self.state.ui_settings.theme, dark);
        let weak = cx.entity().downgrade();
        let zoom = crate::style::zoom();
        let rc = Rc::new(crate::transcript::RowContext {
            tokens: t,
            syntax,
            reduced: self.reduced(),
            diagram_menu: None,
            diagram_menu_ix: 0,
            diagram_menu_keyboard: false,
            toggles: Rc::new(self.fold_toggles.clone()),
            copied: self.copied.as_ref().map(|(id, _)| id.clone()),
            copied_at: self.copied.as_ref().map(|(_, at)| *at),
            on_action: Rc::new(move |action, cx| {
                let _ = weak.update(cx, |view, cx| match action {
                    crate::transcript::RowAction::Toggle { row_id } => view.toggle_row(&row_id, cx),
                    crate::transcript::RowAction::Copy { row_id, content } => {
                        view.copy_code(row_id, content, cx)
                    }
                    crate::transcript::RowAction::OpenImage { path } => {
                        view.open_lightbox(crate::view::Lightbox::Image(path.into()), cx)
                    }
                    crate::transcript::RowAction::OpenUrl { url } => cx.open_url(&url),
                    _ => {}
                });
            }),
            order: 0,
            live_choices: None,
            merges: Rc::new(HashMap::new()),
            tracked_mr: None,
            armed_runs: Rc::new(HashSet::new()),
            run_at: None,
            step_from: None,
            tool_from: Default::default(),
            muted: false,
            now: Instant::now(),
            now_ms: crate::trace_view::now_ms(),
            clock_epoch: self.clock_epoch,
            viewport_h: f32::from(window.viewport_size().height),
            open_lines: Rc::new(HashMap::new()),
            row_heights: Rc::new(std::cell::RefCell::new(HashMap::new())),
            prose_max_w: crate::transcript::PROSE_MAX_W.min((width / zoom - 48.).max(200.)),
            bubble_max_w: None,
            plan_running: false,
            plan_cards: std::rc::Rc::new(std::collections::HashMap::new()),
            probe: None,
        });
        let items = md.items.clone();
        let n = items.len();
        list(md.list.clone(), move |ix, _, _| {
            let prev = ix.checked_sub(1).map(|p| &items[p]);
            div()
                .w_full()
                .px(zpx(24.))
                .when(ix == 0, |d| d.pt(zpx(20.)))
                .when(ix + 1 == n, |d| d.pb(zpx(48.)))
                .child(crate::transcript::render_row(ix, &items[ix], prev, &rc))
                .into_any_element()
        })
        .flex_1()
        .min_h_0()
        .size_full()
        .into_any_element()
    }
}

impl KeelWindow {
    /// Dev socket commands for the Files pane (`files_*`). None: not one.
    pub(in crate::view) fn files_dev(
        &mut self,
        cmd: &str,
        arg: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let ok = |s: &str| Some(s.to_string());
        let out = match cmd {
            "files_toggle" => {
                self.toggle_files(window, cx);
                ok("ok")
            }
            "files_tab" => {
                self.files.tab = if arg == "files" { FilesTab::Files } else { FilesTab::Changes };
                ok("ok")
            }
            "files_tree" => {
                let next = arg != "off";
                if self.files.stack.tree != next {
                    self.files.stack.tree = next;
                    self.files.stack.mode_generation += 1;
                }
                ok("ok")
            }
            "files_list" => {
                self.files.stack.narrow_list = arg != "off";
                ok("ok")
            }
            "files_scroll" => {
                self.files_goto_change(arg.to_string(), cx);
                ok("ok")
            }
            "files_reveal" => {
                self.files_reveal(arg.to_string(), false, window, cx);
                ok("ok")
            }
            "files_select" => {
                self.files_select(arg.to_string(), window, cx);
                ok("ok")
            }
            "files_md_source" => {
                self.files.viewer.md_source = arg != "off";
                ok("ok")
            }
            "files_actual" => {
                self.files.viewer.actual = arg != "off";
                ok("ok")
            }
            "files_split" => {
                self.files.stack.force_split = match arg {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                };
                self.files.stack.dirty = true;
                ok("ok")
            }
            "files_width" => {
                let was = self.dev_set_browser_width(arg.parse::<f32>().ok());
                Some(serde_json::json!({ "was": was }).to_string())
            }
            // Fold a diff section and record each frame of it (`files_trace`).
            "files_fold" => {
                self.files.stack.trace = Some((Instant::now(), arg.to_string(), Vec::new()));
                self.files_toggle_section(arg.to_string(), cx);
                ok("ok")
            }
            "files_trace" => {
                let frames = self.files.stack.trace.as_ref().map(|(_, _, f)| f.clone()).unwrap_or_default();
                Some(serde_json::Value::Array(frames).to_string())
            }
            "files_tree_fold" => {
                self.files_toggle_change_dir(arg.to_string(), cx);
                ok("ok")
            }
            "files_dir" => {
                self.files_toggle_dir(arg.to_string(), cx);
                ok("ok")
            }
            "files_tip" => {
                let t = self.state.effective_tokens(matches!(window.appearance(), gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark));
                self.files.dev_tip = (arg != "off").then(|| cx.new(|_| super::super::dotify_ui::Tip::new(arg.to_string(), t)));
                ok("ok")
            }
            "files_back" => {
                self.files.selected = None;
                ok("ok")
            }
            "files_detail_tab" => {
                self.files.change_detail_tab = match arg {
                    "file" => {
                        if let Some(selected) = &self.files.selected {
                            let path = selected.clone();
                            if is_image(&path) {
                                self.files_locate(path, cx);
                            } else {
                                self.files_open_file(path, window, cx);
                            }
                        }
                        super::ChangeDetailTab::File
                    }
                    _ => super::ChangeDetailTab::Diff,
                };
                ok("ok")
            }
            "files_state" => Some(
                serde_json::json!({
                    "tab": if matches!(self.files.tab, FilesTab::Files) { "files" } else { "changes" },
                    "selected": self.files.selected,
                    "detail_tab": match self.files.change_detail_tab {
                        super::ChangeDetailTab::Diff => "diff",
                        super::ChangeDetailTab::File => "file",
                    },
                    "width": self.browser_width(window),
                    "stack": self.files.stack.state_json(),
                    "viewer": self.files.viewer.state_json(),
                })
                .to_string(),
            ),
            _ => return None,
        };
        cx.notify();
        out
    }
}

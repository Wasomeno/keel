//! `mermaid` fences, laid out and drawn by merman (the Mermaid port Zed uses)
//! as an SVG, rasterized with resvg. Subgraphs, shapes, and edge routing come
//! from Mermaid's own layout, not ours.
//!
//! Layout costs ~10ms, so results are cached per source: the size (for the
//! row-height memo, theme-free), the parsed SVG (per theme), and one raster at
//! the size it is drawn.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, LazyLock, OnceLock};

use gpui::RenderImage;

use keel_theme::Tokens;
use merman::svg::{
    HostTheme, HostThemeAppearance, Presentation, PresentationProfile, SvgPipeline, ThemeRole,
};
use merman::{Engine, OperationControl, RenderOutput, RenderRequest, Renderer, SvgRequest};

/// Inset around the drawing inside the fence.
pub const PAD: f32 = 12.;
/// Tallest a diagram draws in the chat; Expand shows it bigger.
pub const MAX_INLINE_H: f32 = 480.;
/// Scale floor where 13px node labels stay >= 12px.
pub const MIN_TEXT_SCALE: f32 = 12.0 / 13.0;
/// Logical padding around exported PNG.
pub const EXPORT_PAD: f32 = 24.;

/// A part of the diagram, in its own units (the SVG viewBox), drawn into
/// `pw`×`ph` device pixels. Coordinates are rounded to 1/4 unit so a request
/// that has not really moved does not redraw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub pw: u32,
    pub ph: u32,
}

impl Region {
    fn snapped(self) -> Self {
        let q = |v: f32| (v * 4.).round() / 4.;
        Self {
            x: q(self.x),
            y: q(self.y),
            w: q(self.w).max(0.25),
            h: q(self.h).max(0.25),
            ..self
        }
    }
}

/// One diagram's pixels in one view: the raster on screen and the one being
/// drawn on a background thread (~250ms at retina width, too slow for a frame).
#[derive(Default)]
struct Slot {
    shown: Option<(Region, Arc<RenderImage>)>,
    job: Option<(Region, Arc<OnceLock<Option<Arc<RenderImage>>>>)>,
}

/// What to paint now.
pub struct Raster {
    /// Best available pixels and the region they show: exact, or a stale one
    /// to place by its region until the new one lands.
    pub image: Option<(Region, Arc<RenderImage>)>,
    /// A replaced raster to free from the atlas.
    pub stale: Option<Arc<RenderImage>>,
    /// A raster is still being drawn: paint again next frame.
    pub pending: bool,
}

/// A parsed diagram in one theme, ready to rasterize at any size.
struct Doc {
    tree: resvg::usvg::Tree,
    width: f32,
    height: f32,
}

// ponytail: cleared wholesale at the cap; an LRU if long sessions full of
// diagrams ever thrash it.
const CACHE_CAP: usize = 128;

thread_local! {
    static SIZES: RefCell<HashMap<u64, Option<(f32, f32)>>> = RefCell::default();
    static DOCS: RefCell<HashMap<u64, Option<Arc<Doc>>>> = RefCell::default();
    static RASTERS: RefCell<HashMap<u64, Slot>> = RefCell::default();
}

pub(crate) static FONTS: LazyLock<Arc<resvg::usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut db = resvg::usvg::fontdb::Database::new();
    db.load_system_fonts();
    Arc::new(db)
});

fn key(parts: &[&str]) -> u64 {
    let mut h = DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

fn cached<T: Clone>(
    cache: &'static std::thread::LocalKey<RefCell<HashMap<u64, Option<T>>>>,
    k: u64,
    make: impl FnOnce() -> Option<T>,
) -> Option<T> {
    if let Some(hit) = cache.with(|c| c.borrow().get(&k).cloned()) {
        return hit;
    }
    let made = make();
    cache.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= CACHE_CAP {
            c.clear();
        }
        c.insert(k, made.clone());
    });
    made
}

/// Natural size of the diagram, or `None` when merman can't draw it (the fence
/// then stays a code block).
pub fn size(source: &str) -> Option<(f32, f32)> {
    cached(&SIZES, key(&[source]), || {
        render_svg(source, None).as_deref().and_then(view_box)
    })
}

/// Scale for rendering inline: fit to width, never grown, and never shrunk
/// below `MIN_TEXT_SCALE` so 13px node labels stay >= 12px.
pub fn inline_scale((w, _): (f32, f32), col: f32) -> f32 {
    let avail = (col - PAD * 2.).max(1.);
    (avail / w).min(1.).max(MIN_TEXT_SCALE)
}

/// Drawn size inline in a column `col` px wide: fits to width, height capped at `MAX_INLINE_H`.
pub fn inline_fit((w, h): (f32, f32), col: f32) -> (f32, f32) {
    let s = inline_scale((w, h), col);
    (w * s, (h * s).min(MAX_INLINE_H))
}

/// Whether the diagram is taller than `MAX_INLINE_H` when rendered inline.
pub fn is_cropped((w, h): (f32, f32), col: f32) -> bool {
    let s = inline_scale((w, h), col);
    h * s > MAX_INLINE_H
}

/// Kept for backward compatibility with callers of `fit`.
pub fn fit((w, h): (f32, f32), col: f32) -> (f32, f32) {
    inline_fit((w, h), col)
}

/// Check if the source starts with a top-to-bottom direction that can safely be swapped to LR.
pub fn can_swap_direction(source: &str) -> bool {
    let clean = strip_fences_and_inits(source);
    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("%%") {
            continue;
        }
        if (trimmed.starts_with("flowchart ") || trimmed.starts_with("graph "))
            && (trimmed.ends_with(" TD")
                || trimmed.ends_with(" TB")
                || trimmed.contains(" TD ")
                || trimmed.contains(" TB "))
        {
            return true;
        }
        break;
    }
    false
}

/// Swaps flowchart/graph TD/TB to LR.
pub fn swap_to_lr(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut swapped = false;
    for line in source.lines() {
        if !swapped {
            let trimmed = line.trim();
            if (trimmed.starts_with("flowchart ") || trimmed.starts_with("graph "))
                && (trimmed.contains("TD") || trimmed.contains("TB"))
            {
                let replaced = line
                    .replacen("flowchart TD", "flowchart LR", 1)
                    .replacen("flowchart TB", "flowchart LR", 1)
                    .replacen("graph TD", "graph LR", 1)
                    .replacen("graph TB", "graph LR", 1);
                out.push_str(&replaced);
                out.push('\n');
                swapped = true;
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// If top-to-bottom graph would be > 2.5x taller than the card width, swap direction to LR.
pub fn maybe_swap_direction<'a>(source: &'a str, card_width: f32) -> std::borrow::Cow<'a, str> {
    if can_swap_direction(source) {
        if let Some((_, h)) = size(source) {
            if h > 2.5 * card_width {
                let swapped = swap_to_lr(source);
                if size(&swapped).is_some() {
                    return std::borrow::Cow::Owned(swapped);
                }
            }
        }
    }
    std::borrow::Cow::Borrowed(source)
}

fn strip_fences_and_inits(source: &str) -> &str {
    let mut s = source.trim();
    if s.starts_with("```") {
        if let Some(idx) = s.find('\n') {
            s = s[idx + 1..].trim_start();
        }
    }
    if s.ends_with("```") {
        if let Some(idx) = s.rfind("```") {
            s = s[..idx].trim_end();
        }
    }
    s
}

/// Sentence-case a string (first character uppercase, remainder lowercase if all caps, else preserved).
pub fn to_sentence_case(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let is_all_caps = trimmed.len() > 3 && trimmed.chars().all(|c| !c.is_alphabetic() || c.is_uppercase());
    let rest: String = if is_all_caps {
        trimmed.chars().skip(1).flat_map(|c| c.to_lowercase()).collect()
    } else {
        trimmed.chars().skip(1).collect()
    };
    let first = trimmed.chars().next().unwrap().to_uppercase().to_string();
    format!("{first}{rest}")
}

/// Extract title from diagram source: top-level title directive/frontmatter, or first subgraph, or root node label, else "Diagram".
pub fn diagram_title(source: &str) -> String {
    let clean = strip_fences_and_inits(source);
    let mut in_frontmatter = false;
    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed == "---" {
            in_frontmatter = !in_frontmatter;
            continue;
        }
        if in_frontmatter || trimmed.starts_with("title ") || trimmed.starts_with("title:") {
            if let Some(rest) = trimmed.strip_prefix("title:") {
                let t = rest.trim().trim_matches('"').trim_matches('\'');
                if !t.is_empty() {
                    return to_sentence_case(t);
                }
            } else if let Some(rest) = trimmed.strip_prefix("title ") {
                let t = rest.trim().trim_matches('"').trim_matches('\'');
                if !t.is_empty() {
                    return to_sentence_case(t);
                }
            }
        }
    }
    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("subgraph ") {
            let rest = trimmed.strip_prefix("subgraph ").unwrap().trim();
            if let Some(idx) = rest.find('[') {
                let inner = &rest[idx + 1..];
                if let Some(end) = inner.rfind(']') {
                    let label = inner[..end].trim().trim_matches('"').trim_matches('\'');
                    if !label.is_empty() {
                        return to_sentence_case(label);
                    }
                }
            }
            let name = rest.split_whitespace().next().unwrap_or("").trim_matches('"');
            if !name.is_empty() {
                return to_sentence_case(name);
            }
        }
    }
    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("%%")
            || trimmed.starts_with("flowchart")
            || trimmed.starts_with("graph")
            || trimmed.starts_with("classDef")
            || trimmed.starts_with("direction")
            || trimmed.starts_with("subgraph")
            || trimmed.starts_with("end")
            || trimmed.starts_with("style")
        {
            continue;
        }
        if let Some(label) = extract_first_node_label(trimmed) {
            return to_sentence_case(&label);
        }
    }
    "Diagram".to_string()
}

fn extract_first_node_label(line: &str) -> Option<String> {
    let pairs = [
        ("([", "])"),
        ("((", "))"),
        ("[", "]"),
        ("(", ")"),
        ("{", "}"),
    ];
    let mut earliest: Option<(usize, usize, &str, &str)> = None;
    for &(open, close) in &pairs {
        if let Some(pos) = line.find(open) {
            if earliest.map_or(true, |(p, _, _, _)| pos < p) {
                if let Some(end) = line[pos + open.len()..].find(close) {
                    earliest = Some((pos, pos + open.len() + end, open, close));
                }
            }
        }
    }
    if let Some((start, end, open, _)) = earliest {
        let raw = &line[start + open.len()..end];
        let label = raw.trim().trim_matches('"').trim_matches('\'').trim();
        if !label.is_empty() {
            return Some(label.to_string());
        }
    }
    None
}

/// Diagnostic for diagram parsing errors: 0-based error line index and message.
pub fn parse_error(source: &str) -> Option<(usize, String)> {
    let source_prep = prepare_source(source);
    let renderer = Renderer::new();
    let request = SvgRequest {
        pipeline: Some(SvgPipeline::resvg_safe()),
        ..SvgRequest::default()
    };
    match renderer.render(RenderRequest::svg(&source_prep, OperationControl::new(), request)) {
        Ok(_) => None,
        Err(err) => {
            let msg = err.to_string();
            let mut line_ix = 0;
            for (idx, line) in source.lines().enumerate() {
                if !line.trim().is_empty() && !line.trim().starts_with("%%") {
                    line_ix = idx;
                }
            }
            Some((line_ix, msg))
        }
    }
}

/// Whether merman can draw `source` in this theme. Parses once per theme.
pub fn renders(source: &str, t: &Tokens) -> bool {
    doc(source, t).is_some()
}

fn doc(source: &str, t: &Tokens) -> Option<Arc<Doc>> {
    let k = key(&[
        source,
        &t.code.to_string(),
        &t.surface.to_string(),
        &t.text.to_string(),
    ]);
    cached(&DOCS, k, || {
        let svg = render_svg(source, Some(t))?;
        let (width, height) = view_box(&svg)?;
        let options = resvg::usvg::Options {
            fontdb: FONTS.clone(),
            ..Default::default()
        };
        let tree = resvg::usvg::Tree::from_str(&svg, &options).ok()?;
        Some(Arc::new(Doc {
            tree,
            width,
            height,
        }))
    })
}

/// SVG groups that are a diagram's nodes, by what their id contains
/// (merman prefixes them: `merman-flowchart-A-0`): flowchart, state, class
/// and ER diagrams.
const NODE_IDS: [&str; 4] = ["flowchart-", "state-", "classId-", "entity-"];

/// The diagram's nodes, for clicks: each node's label and box, in the
/// diagram's own units (as [`size`]).
pub fn nodes(source: &str, t: &Tokens) -> Vec<(String, f32, f32, f32, f32)> {
    type Rect = resvg::tiny_skia::Rect;
    fn shape(group: &resvg::usvg::Group) -> Option<Rect> {
        group
            .children()
            .iter()
            .fold(None, |acc: Option<Rect>, node| {
                let next = match node {
                    resvg::usvg::Node::Path(_) => Some(node.abs_bounding_box()),
                    resvg::usvg::Node::Group(inner) => shape(inner),
                    _ => None,
                };
                match (acc, next) {
                    (Some(a), Some(b)) => Rect::from_ltrb(
                        a.left().min(b.left()),
                        a.top().min(b.top()),
                        a.right().max(b.right()),
                        a.bottom().max(b.bottom()),
                    ),
                    (a, b) => a.or(b),
                }
            })
    }
    fn walk(group: &resvg::usvg::Group, boxes: &mut Vec<Rect>, texts: &mut Vec<(String, Rect)>) {
        for node in group.children() {
            match node {
                resvg::usvg::Node::Group(inner) => {
                    if NODE_IDS.iter().any(|kind| inner.id().contains(kind)) {
                        boxes.extend(shape(inner));
                    }
                    walk(inner, boxes, texts);
                }
                resvg::usvg::Node::Text(text) => {
                    let words: Vec<&str> = text
                        .chunks()
                        .iter()
                        .map(|chunk| chunk.text().trim())
                        .filter(|w| !w.is_empty())
                        .collect();
                    if !words.is_empty() {
                        texts.push((words.join(" "), node.abs_bounding_box()));
                    }
                }
                _ => {}
            }
        }
    }
    let Some(doc) = doc(source, t) else {
        return Vec::new();
    };
    let (mut boxes, mut texts) = (Vec::new(), Vec::new());
    walk(doc.tree.root(), &mut boxes, &mut texts);
    boxes
        .into_iter()
        .filter_map(|b| {
            let inside = |r: &Rect| {
                let (cx, cy) = (r.x() + r.width() / 2., r.y() + r.height() / 2.);
                cx >= b.left() && cx <= b.right() && cy >= b.top() && cy <= b.bottom()
            };
            let label = texts
                .iter()
                .filter(|(_, r)| inside(r))
                .map(|(text, _)| text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            (!label.is_empty()).then(|| (label, b.x(), b.y(), b.width(), b.height()))
        })
        .collect()
}

/// The whole diagram `device_w` pixels wide. Drawing happens off the UI
/// thread; until it lands, the previous size (if any) stands in. `view` names
/// who draws it (inline, lightbox): each keeps its own raster, or they would
/// trade sizes every frame.
pub fn raster(source: &str, t: &Tokens, device_w: f32, view: &str) -> Raster {
    let Some((w, h)) = doc(source, t).map(|d| (d.width, d.height)) else {
        return Raster::NONE;
    };
    // Buckets of 64px: a window drag re-rasterizes a few times, not every frame.
    let pw = ((device_w / 64.).ceil() * 64.).clamp(64., 8192.) as u32;
    let ph = ((h * pw as f32 / w).ceil() as u32).clamp(1, 8192);
    raster_region(
        source,
        t,
        Region {
            x: 0.,
            y: 0.,
            w,
            h,
            pw,
            ph,
        },
        view,
    )
}

/// `region` of the diagram, for a view that shows only part of it (zoomed in).
pub fn raster_region(source: &str, t: &Tokens, region: Region, view: &str) -> Raster {
    let Some(doc) = doc(source, t) else {
        return Raster::NONE;
    };
    let want = region.snapped();
    let k = key(&[source, &t.code.to_string(), view]);
    RASTERS.with(|r| {
        let mut r = r.borrow_mut();
        let slot = r.entry(k).or_default();
        let mut stale = None;
        if let Some((done_for, done)) = slot.job.take_if(|(_, job)| job.get().is_some())
            && let Some(Some(image)) = done.get().cloned()
        {
            stale = slot.shown.replace((done_for, image)).map(|(_, old)| old);
        }
        let exact = slot.shown.as_ref().is_some_and(|(at, _)| *at == want);
        if !exact && slot.job.as_ref().is_none_or(|(at, _)| *at != want) {
            let job = Arc::new(OnceLock::new());
            let out = job.clone();
            std::thread::spawn(move || {
                let _ = out.set(
                    draw(&doc, want)
                        .map(|px| Arc::new(RenderImage::new(vec![image::Frame::new(px)]))),
                );
            });
            slot.job = Some((want, job));
        }
        Raster {
            image: slot.shown.clone(),
            stale,
            pending: slot.job.is_some(),
        }
    })
}

/// What `view` last drew, without asking for anything new (mid-gesture).
pub fn shown(source: &str, t: &Tokens, view: &str) -> Option<(Region, Arc<RenderImage>)> {
    let k = key(&[source, &t.code.to_string(), view]);
    RASTERS.with(|r| r.borrow().get(&k).and_then(|slot| slot.shown.clone()))
}

impl Raster {
    pub const NONE: Self = Self {
        image: None,
        stale: None,
        pending: false,
    };
}

/// `region` as BGRA pixels (in an RGBA buffer: the order gpui uploads).
fn draw(doc: &Doc, region: Region) -> Option<image::RgbaImage> {
    let pixmap = render_pixmap(doc, region, None)?;
    let mut bgra = Vec::with_capacity(pixmap.data().len());
    for p in pixmap.pixels() {
        let c = p.demultiply();
        bgra.extend_from_slice(&[c.blue(), c.green(), c.red(), c.alpha()]);
    }
    image::ImageBuffer::from_raw(region.pw, region.ph, bgra)
}

fn render_pixmap(doc: &Doc, region: Region, fill: Option<u32>) -> Option<resvg::tiny_skia::Pixmap> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(region.pw, region.ph)?;
    if let Some(v) = fill {
        let ch = |shift: u32| ((v >> shift) & 0xFF) as u8;
        pixmap.fill(resvg::tiny_skia::Color::from_rgba8(
            ch(24),
            ch(16),
            ch(8),
            255,
        ));
    }
    let (sx, sy) = (region.pw as f32 / region.w, region.ph as f32 / region.h);
    let transform =
        resvg::tiny_skia::Transform::from_scale(sx, sy).pre_translate(-region.x, -region.y);
    resvg::render(&doc.tree, transform, &mut pixmap.as_mut());
    Some(pixmap)
}

/// The whole diagram as a PNG at 2x, with padding and the card background
/// so it reads cleanly wherever it is pasted. Slow (~1s for a big chart):
/// call off the UI thread with what [`png_job`] returns.
pub fn png_job(
    source: &str,
    t: &Tokens,
) -> Option<impl FnOnce() -> Option<Vec<u8>> + Send + 'static> {
    let doc = doc(source, t)?;
    let bg = t.code;
    Some(move || {
        let pad = EXPORT_PAD;
        let total_w = doc.width + pad * 2.;
        let total_h = doc.height + pad * 2.;
        let scale = (2. as f32).min(8192. / total_w).min(8192. / total_h);
        let (pw, ph) = (
            (total_w * scale).ceil() as u32,
            (total_h * scale).ceil() as u32,
        );
        let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph)?;
        let ch = |shift: u32| ((bg >> shift) & 0xFF) as u8;
        pixmap.fill(resvg::tiny_skia::Color::from_rgba8(
            ch(24),
            ch(16),
            ch(8),
            255,
        ));
        let transform = resvg::tiny_skia::Transform::from_scale(scale, scale)
            .pre_translate(pad, pad);
        resvg::render(&doc.tree, transform, &mut pixmap.as_mut());
        let rgba = image::RgbaImage::from_raw(pw, ph, pixmap.take())?;
        let mut png = std::io::Cursor::new(Vec::new());
        rgba.write_to(&mut png, image::ImageFormat::Png).ok()?;
        Some(png.into_inner())
    })
}

fn render_svg(source: &str, t: Option<&Tokens>) -> Option<String> {
    let source = prepare_source(source);
    let mut renderer = Renderer::new();
    let mut request = SvgRequest {
        pipeline: Some(SvgPipeline::resvg_safe()),
        ..SvgRequest::default()
    };

    if let Some(tokens) = t {
        let theme = host_theme(tokens);
        let presentation = Presentation::new()
            .with_profile(PresentationProfile::MermanModern)
            .with_theme(theme);
        let resolved = presentation.resolve();
        let engine = resolved.materialize_engine(Engine::new());
        request.presentation = resolved.render_policy();
        renderer = renderer.with_engine(engine);
    }

    match renderer.render(RenderRequest::svg(
        &source,
        OperationControl::new(),
        request,
    )) {
        Ok(RenderOutput::Svg(Some(svg))) => Some(post_process_svg(svg.into_parts().0, t, &source)),
        _ => None,
    }
}

/// Pre-processes diagram source for resilience:
/// - Strips markdown fences if wrapped in ```` ```mermaid ... ````
/// - Strips existing `%%{init:...}%%` directives to avoid fatal double-init conflicts
fn prepare_source(source: &str) -> String {
    let mut s = source.trim();

    if s.starts_with("```") {
        if let Some(idx) = s.find('\n') {
            s = s[idx + 1..].trim_start();
        }
    }
    if s.ends_with("```") {
        if let Some(idx) = s.rfind("```") {
            s = s[..idx].trim_end();
        }
    }

    let stripped = strip_init_directives(s);
    let converted = convert_decision_nodes(&stripped);
    format!(
        "%%{{init: {{\"flowchart\": {{\"curve\": \"basis\", \"wrappingWidth\": 220, \"padding\": 8}}, \"themeVariables\": {{\"fontSize\": \"13px\"}}}}}}%%\n{converted}"
    )
}

/// Transforms decision nodes from large stock diamonds `id{Valid}` to sleek stadium pills with `?` glyph `id([Valid ?])`.
fn convert_decision_nodes(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 64);
    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("%%") {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let mut line_out = String::with_capacity(line.len() + 16);
        let mut rest = line;
        while let Some(open) = rest.find('{') {
            if open > 0 && rest.as_bytes()[open - 1] == b'%' {
                line_out.push_str(&rest[..open + 1]);
                rest = &rest[open + 1..];
                continue;
            }
            if let Some(close) = rest[open + 1..].find('}') {
                let close_idx = open + 1 + close;
                let inner = &rest[open + 1..close_idx];
                if !inner.contains('\n') && !inner.trim().is_empty() {
                    let trimmed_inner = inner.trim();
                    let (quoted, inner_clean) = if (trimmed_inner.starts_with('"') && trimmed_inner.ends_with('"'))
                        || (trimmed_inner.starts_with('\'') && trimmed_inner.ends_with('\''))
                    {
                        (true, &trimmed_inner[1..trimmed_inner.len() - 1])
                    } else {
                        (false, trimmed_inner)
                    };
                    let label = if inner_clean.trim_end().ends_with('?') {
                        inner_clean.to_string()
                    } else {
                        format!("{} ?", inner_clean.trim_end())
                    };
                    line_out.push_str(&rest[..open]);
                    if quoted {
                        line_out.push_str(&format!("([\"{label}\"])"));
                    } else {
                        line_out.push_str(&format!("([{label}])"));
                    }
                    rest = &rest[close_idx + 1..];
                } else {
                    line_out.push_str(&rest[..=open]);
                    rest = &rest[open + 1..];
                }
            } else {
                line_out.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
        line_out.push_str(rest);
        out.push_str(&line_out);
        out.push('\n');
    }
    out
}

fn strip_init_directives(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("%%{") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("}%%") {
            rest = &rest[start + end + 3..];
            if rest.starts_with("\r\n") {
                rest = &rest[2..];
            } else if rest.starts_with('\n') {
                rest = &rest[1..];
            }
        } else {
            out.push_str(&rest[start..start + 3]);
            rest = &rest[start + 3..];
        }
    }
    out.push_str(rest);
    out
}

/// Maps Keel's design tokens into Merman's native HostTheme presentation roles.
fn host_theme(t: &Tokens) -> HostTheme {
    let dark = luminance(t.text) > luminance(t.code);
    let app = if dark {
        HostThemeAppearance::Dark
    } else {
        HostThemeAppearance::Light
    };
    let theme = HostTheme::new().with_appearance(app);

    let theme = theme
        .try_with_font_family("-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif")
        .unwrap_or_else(|_| HostTheme::new().with_appearance(app));
    let mut theme = theme
        .try_with_font_size("13px")
        .unwrap_or_else(|_| HostTheme::new().with_appearance(app));

    let hex = |v: u32| blend(v, t.code);
    let roles = [
        (ThemeRole::Canvas, hex(t.code)),
        (ThemeRole::Surface, hex(t.floating)),
        (ThemeRole::SurfaceAlt, hex(t.surface)),
        (ThemeRole::SurfaceMuted, hex(t.code)),
        (ThemeRole::Text, hex(t.text)),
        (ThemeRole::SubtleText, hex(t.text_muted)),
        (ThemeRole::Border, hex(t.border)),
        (ThemeRole::Line, hex(t.text_muted)),
        (ThemeRole::EdgeLabelBackground, hex(t.code)),
        (ThemeRole::ClusterBackground, hex(t.surface)),
        (ThemeRole::ClusterBorder, hex(t.border)),
        (ThemeRole::NoteBackground, hex(t.surface)),
        (ThemeRole::NoteBorder, hex(t.border)),
        (ThemeRole::NoteText, hex(t.text)),
        (ThemeRole::ActorBackground, hex(t.floating)),
        (ThemeRole::ActorBorder, hex(t.border)),
        (ThemeRole::ActorText, hex(t.text)),
    ];

    for (role, color) in roles {
        if let Ok(th) = theme.clone().try_with_role(role, color) {
            theme = th;
        }
    }

    theme
}

/// Finds the ID of a terminal/sink node in a flowchart to highlight with the single accent color.
fn find_terminal_node(source: &str) -> Option<String> {
    let clean = strip_fences_and_inits(source);
    let mut sources = std::collections::HashSet::new();
    let mut targets = Vec::new();

    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("%%") || trimmed.starts_with("subgraph") || trimmed.is_empty() {
            continue;
        }
        if let Some(arrow_pos) = trimmed
            .find("-->")
            .or_else(|| trimmed.find("-.->"))
            .or_else(|| trimmed.find("==>"))
        {
            let left = trimmed[..arrow_pos].trim();
            let right_part = &trimmed[arrow_pos..];
            let right_start = if right_part.starts_with("-->") || right_part.starts_with("==>") {
                3
            } else {
                4
            };
            let mut right = right_part[right_start..].trim();
            if right.starts_with('|') {
                if let Some(pipe_end) = right[1..].find('|') {
                    right = right[pipe_end + 2..].trim();
                }
            }
            let extract_id = |s: &str| -> String {
                s.chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                    .collect()
            };
            let left_id = extract_id(left);
            let right_id = extract_id(right);
            if !left_id.is_empty() {
                sources.insert(left_id);
            }
            if !right_id.is_empty() {
                targets.push(right_id);
            }
        }
    }
    targets.into_iter().rev().find(|t| !sources.contains(t))
}

fn mix(fg: u32, bg: u32, amount: f32) -> u32 {
    let ch = |shift: u32| {
        let f = ((fg >> shift) & 0xFF) as f32;
        let b = ((bg >> shift) & 0xFF) as f32;
        ((f * amount + b * (1. - amount)).round() as u32) << shift
    };
    ch(24) | ch(16) | ch(8) | 0xFF
}

/// Applies modern Keel aesthetic polish to Mermaid's raw SVG:
/// - Transparent canvas background
/// - UI font (-apple-system, BlinkMacSystemFont, Segoe UI, Roboto)
/// - Tone steps from Keel tokens with zero 1px outlines on nodes or groups
/// - One radius set: 6px nodes, 10px groups
/// - Group titles 12px sentence case, left-aligned at top-left
/// - Edges 1.25px, ink at ~35% opacity, small filled arrowheads
/// - Single accent color on the terminal/sink node, ink for everything else
fn post_process_svg(svg: String, t: Option<&Tokens>, source: &str) -> String {
    let mut svg = svg
        .replace("background-color:white", "background-color:transparent")
        .replace("background-color: white", "background-color: transparent")
        .replace(
            "\"trebuchet ms\",verdana,arial,sans-serif",
            "-apple-system, BlinkMacSystemFont, \"Segoe UI\", Roboto, sans-serif",
        )
        .replace(
            "&quot;trebuchet ms&quot;,verdana,arial,sans-serif",
            "-apple-system, BlinkMacSystemFont, &quot;Segoe UI&quot;, Roboto, sans-serif",
        )
        .replace(
            "trebuchet ms, verdana, arial, sans-serif",
            "-apple-system, BlinkMacSystemFont, sans-serif",
        )
        .replace("data-look=\"neo\"", "data-look=\"classic\"");

    if let Some(tokens) = t {
        let dark = luminance(tokens.text) > luminance(tokens.code);
        let group_bg = if dark {
            blend(0xFFFFFF0F, tokens.code)
        } else {
            blend(tokens.surface, tokens.code)
        };
        let node_bg = if dark {
            blend(0xFFFFFF20, tokens.code)
        } else {
            blend(tokens.floating, tokens.code)
        };
        let accent_bg = blend(tokens.accent, tokens.code);
        let accent_text = blend(tokens.text_on_accent, tokens.accent);
        let edge_ink = blend(mix(tokens.text, tokens.code, 0.35), tokens.code);
        let card_bg = blend(tokens.code, tokens.code);
        let text = blend(tokens.text, tokens.code);
        let text_muted = blend(tokens.text_muted, tokens.code);

        // Mark terminal node with node-accent class if found
        if let Some(terminal) = find_terminal_node(source) {
            let needle = format!("id=\"merman-flowchart-{terminal}-");
            if let Some(pos) = svg.find(&needle) {
                if let Some(class_pos) = svg[..pos].rfind("<g class=\"") {
                    let end_quote = class_pos + 10 + svg[class_pos + 10..].find('"').unwrap_or(0);
                    svg.insert_str(end_quote, " node-accent");
                }
            }
        }

        // Align cluster labels to top-left and sentence case
        align_cluster_labels(&mut svg);

        let precision_css = format!(
            "#merman .node rect,#merman .node path,#merman .node circle,#merman .node ellipse{{stroke:none!important;stroke-width:0!important;fill:{node_bg}!important;}}\
             #merman .node polygon{{fill:{node_bg}!important;stroke:{node_bg}!important;stroke-width:6px!important;stroke-linejoin:round!important;stroke-linecap:round!important;}}\
             #merman .node text,#merman .node span{{fill:{text}!important;color:{text}!important;font-size:13px!important;font-weight:500!important;letter-spacing:-0.01em!important;}}\
             #merman .node.node-accent rect,#merman .node.node-accent path{{fill:{accent_bg}!important;stroke:none!important;stroke-width:0!important;}}\
             #merman .node.node-accent polygon{{fill:{accent_bg}!important;stroke:{accent_bg}!important;stroke-width:6px!important;stroke-linejoin:round!important;stroke-linecap:round!important;}}\
             #merman .node.node-accent text,#merman .node.node-accent span{{fill:{accent_text}!important;color:{accent_text}!important;}}\
             #merman .cluster rect{{fill:{group_bg}!important;stroke:none!important;stroke-width:0!important;rx:10px;ry:10px;}}\
             #merman .cluster-label text,#merman .cluster text{{fill:{text_muted}!important;font-size:12px!important;font-weight:500!important;text-anchor:start!important;}}\
             #merman .cluster-label span,#merman .cluster span{{color:{text_muted}!important;font-size:12px!important;font-weight:500!important;text-align:left!important;}}\
             #merman .edgePaths .path,#merman .flowchart-link{{stroke-width:1.25px!important;stroke-linecap:round;stroke-linejoin:round;stroke:{edge_ink}!important;fill:none!important;}}\
             #merman .marker,#merman .arrowheadPath,#merman marker path{{fill:{edge_ink}!important;stroke:none!important;}}\
             #merman .edgeLabel rect{{rx:5px;ry:5px;fill:{card_bg}!important;stroke:none!important;opacity:1;}}\
             #merman .edgeLabel text,#merman .edgeLabel span{{fill:{text_muted}!important;color:{text_muted}!important;font-size:11px!important;font-weight:500!important;}}"
        );
        svg = svg.replace("</style>", &format!("{precision_css}</style>"));
        svg = svg.replace("fill=\"#1f1f22\"", &format!("fill=\"{card_bg}\""));
        svg = svg.replace("fill=\"#e4ddd2\"", &format!("fill=\"{card_bg}\""));
        svg = svg.replace("markerWidth=\"10.5\" markerHeight=\"14\"", "markerWidth=\"7.5\" markerHeight=\"10\"");
        svg = svg.replace("markerWidth=\"8\" markerHeight=\"8\"", "markerWidth=\"6\" markerHeight=\"6\"");
    }

    round_rects(&svg)
}

fn align_cluster_labels(svg: &mut String) {
    let mut search_from = 0;
    while let Some(start) = svg[search_from..].find("<g id=\"merman-foreignobject-label-") {
        let abs_start = search_from + start;
        let Some(end) = svg[abs_start..].find("</g>") else {
            break;
        };
        let abs_end = abs_start + end + 4;
        let block = &svg[abs_start..abs_end];
        if block.contains("cluster-label") {
            let mut modified = block.to_string();
            modified = modified.replace("text-anchor=\"middle\"", "text-anchor=\"start\"");
            modified = modified.replace("font-size:13px", "font-size:12px");
            if let Some(x_pos) = modified.find("<text x=\"") {
                if let Some(quote_end) = modified[x_pos + 9..].find('"') {
                    modified.replace_range(x_pos + 9..x_pos + 9 + quote_end, "0");
                }
            }
            if let Some(text_open) = modified.find("<text ") {
                if let Some(tag_close) = modified[text_open..].find('>') {
                    let text_start = text_open + tag_close + 1;
                    if let Some(text_end) = modified[text_start..].find("</text>") {
                        let text_val = &modified[text_start..text_start + text_end];
                        let sc = to_sentence_case(text_val);
                        modified.replace_range(text_start..text_start + text_end, &sc);
                    }
                }
            }
            svg.replace_range(abs_start..abs_end, &modified);
            search_from = abs_start + modified.len();
        } else {
            search_from = abs_end;
        }
    }

    let mut search_cluster = 0;
    while let Some(c_pos) = svg[search_cluster..].find("<g class=\"cluster\"") {
        let abs_c_pos = search_cluster + c_pos;
        let Some(c_end) = svg[abs_c_pos..].find("</g>") else {
            break;
        };
        let abs_c_end = abs_c_pos + c_end + 4;
        let cluster_block = &svg[abs_c_pos..abs_c_end];

        let rect_x = cluster_block.find(" x=\"").and_then(|p| {
            let rest = &cluster_block[p + 4..];
            let q = rest.find('"')?;
            rest[..q].parse::<f32>().ok()
        });
        let rect_y = cluster_block.find(" y=\"").and_then(|p| {
            let rest = &cluster_block[p + 4..];
            let q = rest.find('"')?;
            rest[..q].parse::<f32>().ok()
        });

        if let (Some(rx), Some(ry)) = (rect_x, rect_y) {
            if let Some(label_pos) = cluster_block.find("<g class=\"cluster-label\" transform=\"translate(") {
                let trans_start = abs_c_pos + label_pos + "<g class=\"cluster-label\" transform=\"translate(".len();
                if let Some(trans_end) = svg[trans_start..].find(')') {
                    let new_trans = format!("{:.1},{:.1}", rx + 12.0, ry + 10.0);
                    svg.replace_range(trans_start..trans_start + trans_end, &new_trans);
                }
            }
        }
        search_cluster = abs_c_pos + 18;
    }
}

fn strip_rx_ry(tag: &str) -> String {
    let mut s = tag.to_string();
    while let Some(pos) = s.find("rx=\"") {
        if let Some(end) = s[pos + 4..].find('"') {
            let full_end = (pos + 4 + end + 1).min(s.len());
            let trim_end = if s[full_end..].starts_with(' ') { full_end + 1 } else { full_end };
            s.replace_range(pos..trim_end, "");
        } else {
            break;
        }
    }
    while let Some(pos) = s.find("ry=\"") {
        if let Some(end) = s[pos + 4..].find('"') {
            let full_end = (pos + 4 + end + 1).min(s.len());
            let trim_end = if s[full_end..].starts_with(' ') { full_end + 1 } else { full_end };
            s.replace_range(pos..trim_end, "");
        } else {
            break;
        }
    }
    s
}

fn round_rects(svg: &str) -> String {
    let mut out = String::with_capacity(svg.len() + 256);
    let mut cluster_depth = 0usize;
    let mut edge_label_depth = 0usize;
    let mut rest = svg;

    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..lt]);
        let tag_rest = &rest[lt..];
        let Some(gt) = tag_rest.find('>') else {
            out.push_str(tag_rest);
            break;
        };
        let tag = &tag_rest[..=gt];
        rest = &tag_rest[gt + 1..];

        if tag.starts_with("<g ") && tag.contains("cluster") {
            cluster_depth += 1;
            out.push_str(tag);
        } else if tag.starts_with("<g ") && tag.contains("edgeLabel") {
            edge_label_depth += 1;
            out.push_str(tag);
        } else if tag.starts_with("</g>") {
            cluster_depth = cluster_depth.saturating_sub(1);
            edge_label_depth = edge_label_depth.saturating_sub(1);
            out.push_str(tag);
        } else if tag.starts_with("<rect ") {
            if edge_label_depth > 0 {
                let stripped = strip_rx_ry(tag);
                out.push_str("<rect rx=\"5\" ry=\"5\" stroke=\"none\" ");
                out.push_str(&stripped[6..]);
            } else if cluster_depth > 0 && !tag.contains("fill=\"none\"") {
                let stripped = strip_rx_ry(tag);
                out.push_str("<rect rx=\"10\" ry=\"10\" ");
                out.push_str(&stripped[6..]);
            } else if tag.contains("label-container") || (!tag.contains("fill=\"none\"") && !tag.contains("class=\"text\"")) {
                let is_pill = tag.contains("rx=\"") && {
                    if let Some(pos) = tag.find("rx=\"") {
                        let rest = &tag[pos + 4..];
                        if let Some(end) = rest.find('"') {
                            rest[..end].parse::<f32>().ok().is_some_and(|r| r > 15.)
                        } else { false }
                    } else { false }
                };
                if is_pill {
                    out.push_str(tag);
                } else {
                    let stripped = strip_rx_ry(tag);
                    out.push_str("<rect rx=\"6\" ry=\"6\" ");
                    out.push_str(&stripped[6..]);
                }
            } else {
                out.push_str(tag);
            }
        } else if tag.starts_with("<polygon ") && !tag.contains("stroke-linejoin=") {
            out.push_str("<polygon stroke-linejoin=\"round\" stroke-linecap=\"round\" ");
            out.push_str(&tag[9..]);
        } else {
            out.push_str(tag);
        }
    }
    out
}

/// `viewBox="0 0 w h"` on the root element.
fn view_box(svg: &str) -> Option<(f32, f32)> {
    let start = svg.find("viewBox=\"")? + "viewBox=\"".len();
    let end = start + svg[start..].find('"')?;
    let n: Vec<f32> = svg[start..end]
        .split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    match n[..] {
        [_, _, w, h] if w > 0. && h > 0. => Some((w, h)),
        _ => None,
    }
}

/// `0xRRGGBBAA` over an opaque background, as `#rrggbb`.
fn blend(fg: u32, bg: u32) -> String {
    let a = (fg & 0xFF) as f32 / 255.;
    let ch = |shift: u32| {
        let f = ((fg >> shift) & 0xFF) as f32;
        let b = ((bg >> shift) & 0xFF) as f32;
        (f * a + b * (1. - a)).round() as u8
    };
    format!("#{:02x}{:02x}{:02x}", ch(24), ch(16), ch(8))
}

fn luminance(v: u32) -> u32 {
    ((v >> 24) & 0xFF) + ((v >> 16) & 0xFF) + ((v >> 8) & 0xFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHART: &str = r#"flowchart TD
    subgraph Client["Frontend (React 19 + TanStack Start)"]
        Router["TanStack Router\n(file-based routes)"] --> UI["UI Layer"]
        subgraph Pages["Pages & Routes"]
            Landing[Landing] --> Login[Login]
        end
    end
    subgraph Data["Data Models"]
        Sess[(Sessions)]
    end
    Client <--> Data
"#;

    #[test]
    fn subgraph_chart_renders_with_text_labels() {
        let svg = render_svg(CHART, Some(&Tokens::dark())).expect("renders");
        assert!(
            !svg.contains("foreignObject"),
            "HTML labels would draw blank"
        );
        for label in [
            "UI Layer",
            "Sessions",
        ] {
            assert!(svg.contains(label), "missing {label}");
        }
        let (w, h) = view_box(&svg).unwrap();
        assert!(w > 100. && h > 100.);
        assert_eq!(
            size(CHART).map(|s| s.0.round()),
            Some(w.round()),
            "theme must not change layout"
        );
    }

    #[test]
    fn modern_styling_and_rounded_corners_applied() {
        let t = Tokens::dark();
        let svg = render_svg(CHART, Some(&t)).expect("renders");
        assert!(
            !svg.contains("trebuchet ms"),
            "trebuchet ms font replaced by system sans-serif"
        );
        assert!(
            svg.contains("rx=\"6\" ry=\"6\""),
            "node rectangles get 6px rounded corners"
        );
        assert!(
            svg.contains("rx=\"10\" ry=\"10\""),
            "cluster rectangles get 10px rounded corners"
        );
        assert!(
            !svg.contains("background-color:white"),
            "white background replaced with transparent"
        );
    }

    #[test]
    fn tall_diagram_swaps_to_lr() {
        let tall = "flowchart TD\n  A --> B\n  B --> C\n  C --> D\n  D --> E\n  E --> F\n  F --> G\n  G --> H\n  H --> I\n  I --> J\n  J --> K\n  K --> L";
        let swapped = maybe_swap_direction(tall, 200.0);
        assert!(swapped.contains("flowchart LR"));
    }

    #[test]
    fn test_actual_failed_diagram() {
        let Ok(source) = std::fs::read_to_string("/tmp/actual_failed_diagram.mmd") else {
            return;
        };
        let t = Tokens::dark();
        assert!(renders(&source, &t), "actual sequence diagram must render");
        let job = png_job(&source, &t).expect("generates png job");
        let _png_bytes = job().expect("renders png");
    }

    #[test]
    fn resilience_handles_fences_double_inits_and_br_tags() {
        let t = Tokens::dark();
        let raw = "```mermaid\n%%{init: {'theme': 'dark'}}%%\nflowchart TD\n  A[Line 1<br>Line 2] --> B[Line 3<br/>Line 4]\n```";
        let svg =
            render_svg(raw, Some(&t)).expect("renders despite fences, inner init, and br tags");
        assert!(svg.contains("Line 1"), "contains Line 1");
        assert!(svg.contains("Line 2"), "contains Line 2");
        assert!(svg.contains("Line 3"), "contains Line 3");
        assert!(svg.contains("Line 4"), "contains Line 4");
    }

    #[test]
    fn flowchart_nodes_have_labels_and_boxes_inside_the_drawing() {
        let source = "graph TD\n  A[Build app] --> B[Run tests]\n  B --> C{Green?}";
        let found = nodes(source, &Tokens::dark());
        let labels: Vec<&str> = found.iter().map(|n| n.0.as_str()).collect();
        assert_eq!(labels.len(), 3, "{labels:?}");
        for label in ["Build app", "Run tests", "Green?"] {
            assert!(labels.contains(&label), "missing {label} in {labels:?}");
        }
        let (w, h) = size(source).unwrap();
        for (label, x, y, bw, bh) in &found {
            assert!(
                *bw > 10.
                    && *bh > 10.
                    && *x >= -1.
                    && *y >= -1.
                    && x + bw <= w + 1.
                    && y + bh <= h + 1.,
                "{label} at {x},{y} {bw}x{bh} in {w}x{h}"
            );
        }
    }

    /// Poll like the painter does, one "frame" per loop.
    fn settle(source: &str, t: &Tokens, w: f32) -> Raster {
        for _ in 0..500 {
            let r = raster(source, t, w, "test");
            if !r.pending {
                return r;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("raster never landed");
    }

    #[test]
    fn raster_lands_in_the_background_and_swaps_sizes() {
        let t = Tokens::dark();
        let first = raster(CHART, &t, 800., "test");
        assert!(
            first.pending && first.image.is_none(),
            "first paint never blocks"
        );
        let (_, image) = settle(CHART, &t, 800.).image.expect("drawn");
        assert_eq!(image.size(0).width.0, 832, "64px bucket");
        let same = raster(CHART, &t, 810., "test");
        assert!(
            !same.pending && same.image.unwrap().1.id == image.id,
            "same bucket, same raster"
        );
        let resized = raster(CHART, &t, 1200., "test");
        assert!(resized.pending);
        assert_eq!(
            resized.image.map(|i| i.1.id),
            Some(image.id),
            "old size stands in"
        );
        let mut stale = None;
        for _ in 0..500 {
            let r = raster(CHART, &t, 1200., "test");
            stale = stale.or(r.stale);
            if !r.pending {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            stale.map(|o| o.id),
            Some(image.id),
            "replaced raster handed back to free"
        );
    }

    #[test]
    fn bare_canvas_is_transparent_not_white() {
        let doc = doc(CHART, &Tokens::dark()).unwrap();
        let whole = Region {
            x: 0.,
            y: 0.,
            w: doc.width,
            h: doc.height,
            pw: 400,
            ph: 300,
        };
        let px = draw(&doc, whole).unwrap();
        assert_eq!(
            &px.as_raw()[..4],
            [0, 0, 0, 0],
            "corner shows the fence, not white"
        );
    }

    #[test]
    fn region_draws_only_that_part_at_full_resolution() {
        let doc = doc(CHART, &Tokens::dark()).unwrap();
        // The top-left quarter into as many pixels as the whole would get.
        let quarter = Region {
            x: 0.,
            y: 0.,
            w: doc.width / 2.,
            h: doc.height / 2.,
            pw: 400,
            ph: 300,
        };
        let whole = Region {
            w: doc.width,
            h: doc.height,
            ..quarter
        };
        assert_ne!(
            draw(&doc, quarter).unwrap().as_raw(),
            draw(&doc, whole).unwrap().as_raw()
        );
        let nudged = Region { x: 0.1, ..quarter }.snapped();
        assert_eq!(
            nudged,
            quarter.snapped(),
            "sub-quarter-unit moves do not redraw"
        );
    }

    #[test]
    fn png_is_opaque_at_twice_the_size() {
        let t = Tokens::dark();
        let png = png_job(CHART, &t).unwrap()().expect("encodes");
        let image = image::load_from_memory(&png).unwrap().into_rgba8();
        let (w, _) = size(CHART).unwrap();
        assert_eq!(image.width(), ((w + EXPORT_PAD * 2.) * 2.).ceil() as u32);
        assert_eq!(image.get_pixel(0, 0).0[3], 255, "pastes on any background");
    }

    #[test]
    fn inline_fit_caps_height_and_never_drops_below_min_scale() {
        let col = 800. + PAD * 2.;
        assert_eq!(
            fit((2560., 2180.), col).1,
            MAX_INLINE_H,
            "tall chart capped at MAX_INLINE_H"
        );
        let wide = fit((1600., 200.), col);
        assert!(
            wide.0 >= 1600. * MIN_TEXT_SCALE,
            "wide chart never scales below MIN_TEXT_SCALE"
        );
        assert_eq!(
            fit((300., 100.), col),
            (300., 100.),
            "small chart keeps its size"
        );
    }

    #[test]
    fn junk_is_not_a_diagram() {
        assert!(size("this is not mermaid at all").is_none());
    }

    #[test]
    fn blend_flattens_alpha() {
        assert_eq!(blend(0xFFFFFF14, 0x000000FF), "#141414");
        assert_eq!(blend(0x1B1B1EFF, 0x000000FF), "#1b1b1e");
    }

    #[test]
    fn direction_swap_and_title() {
        let td = "flowchart TD\n  A --> B\n  B --> C";
        assert!(can_swap_direction(td));
        assert_eq!(swap_to_lr(td), "flowchart LR\n  A --> B\n  B --> C\n");

        let tall_small = "flowchart TD\n  A\n  B\n  C\n  D\n  E\n  A-->B-->C-->D-->E";
        let swapped = maybe_swap_direction(tall_small, 100.0);
        assert!(swapped.contains("flowchart LR"));

        assert_eq!(diagram_title(CHART), "Frontend (React 19 + TanStack Start)");
        assert_eq!(diagram_title("--- \ntitle: My Architecture\n---\nflowchart LR\nA --> B"), "My Architecture");
        assert_eq!(diagram_title("flowchart TD\nA --> B"), "Diagram");
        assert_eq!(diagram_title("flowchart TD\n  A[User request] --> B{Valid}"), "User request");
        assert_eq!(diagram_title("flowchart TD\n  req[USER REQUEST] --> B{Valid}"), "User request");

        let prep = prepare_source("flowchart TD\n  A[User request] --> B{Valid}");
        assert!(prep.contains("B([Valid ?])"));
    }

    #[test]
    fn terminal_node_gets_accent() {
        let chart = "flowchart TD\n  Start --> Done";
        let t = Tokens::dark();
        let svg = render_svg(chart, Some(&t)).expect("renders");
        assert!(svg.contains("node-accent"), "terminal node gets node-accent class");
    }
}

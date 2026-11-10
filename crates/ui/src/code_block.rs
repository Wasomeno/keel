//! Code block, full (a markdown fence) and compact (inside a trace row).
//!
//! Diffing, unified-diff parsing and the height cap are pure. The show-more
//! cap reads [`CodeBlockProps::toggled_at`] and eases with [`glide_curve`]
//! over 360 ms. Nothing here starts a gpui animation, so a frame is requested
//! only while that window is open: no nested animation id, no repeat.

use crate::hover_fade::HoverFadeExt;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, Hitbox, Hsla, IntoElement, SharedString, div,
    prelude::*, px,
};
use keel_theme::{SyntaxPaintMap, Tokens};

use crate::icons::Icon;
use crate::motion::glide_curve;
use crate::style::{CODE, KeelStyled, MONO, RADIUS_MD, TypeStep, c, zoom, zpx};
use crate::syntax::{Spans, SyntaxKind};

/// Collapsed compact body, logical px. `max-h-64`.
pub const COMPACT_CAP: f32 = 256.;
/// Collapsed full body, logical px.
pub const FULL_CAP: f32 = 480.;
/// Bottom edge fade while the body is clipped.
pub const FADE_H: f32 = 16.;
/// Show-more glide. Same length as [`crate::motion::ROW_OPEN`].
pub const SHOW_MORE_MS: u64 = 200;
/// Show-less glide: closing is quicker than opening.
pub const SHOW_LESS_MS: u64 = 160;
/// Expanded full block: at most this fraction of the viewport, and it scrolls.
pub const EXPANDED_VIEWPORT: f32 = 0.7;
/// Compact output past this many lines is cut. The rest is "Show full output".
pub const MAX_RENDERED_LINES: usize = 400;

const COMPACT_LINE_H: f32 = 16.;
const FULL_LINE_H: f32 = 18.;
const COMPACT_PAD_Y: f32 = 8.;
const FULL_PAD_Y: f32 = 10.;
const COMPACT_PAD_X: f32 = 10.;
const FULL_PAD_X: f32 = 12.;
const HEADER_H: f32 = 28.;
const MORE_H: f32 = 26.;
const BUTTON: f32 = 22.;
const FLOAT_INSET: f32 = 6.;
const MARKER_W: f32 = 14.;
const NUMBER_GAP: f32 = 12.;
/// Line diff above this many cells uses a prefix/suffix cut instead of LCS.
const DIFF_CELL_CAP: usize = 200_000;

/// `Full` is the markdown fence. `Compact` sits inside a trace row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeSize {
    Full,
    Compact,
}

/// `Destructive` is a failed command: danger border, a 6% danger wash, full text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeVariant {
    Default,
    Destructive,
}

/// One source line. `Hunk` is a `@@` header. Markers carry added/removed,
/// so the tint is not the only signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineVariant {
    Default,
    Added,
    Removed,
    Highlighted,
    Hunk,
}

/// What a control in the block asked for. The parent owns the state
/// (`expanded`, `copied`, `toggled_at`) and passes it back next frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeAction {
    /// `text` is the visible source. Gutters are not included.
    /// Not sent while [`CodeBlockProps::closed`] is false.
    Copy {
        text: String,
    },
    /// Toggle the show-more cap. Set `toggled_at` to `Instant::now()`.
    ShowMore,
    /// Compact output was cut at [`keel_proto::OUTPUT_TAIL`] or [`MAX_RENDERED_LINES`].
    ShowFull {
        content: String,
        title: Option<String>,
        error: bool,
    },
    Run,
    /// Type `text` at this chat's terminal prompt without pressing Return.
    /// Only shell fences (see [`is_shell`]) that aren't [`CodeBlockProps::runnable`].
    Insert {
        text: String,
    },
    OpenViewer {
        content: String,
        title: Option<String>,
        error: bool,
    },
}

/// Parent callback. Same shape as the transcript's row action.
pub type OnCodeAction = Rc<dyn Fn(CodeAction, &mut App) + 'static>;

/// Inputs for [`render`]. Built with [`CodeBlockProps::new`]; every field is public.
#[derive(Debug, Clone)]
pub struct CodeBlockProps {
    pub id: String,
    pub title: Option<String>,
    pub language: Option<String>,
    pub content: String,
    /// Precomputed highlight. `None` paints plain. Ranges index `content`.
    pub spans: Option<Spans>,
    /// Parallel to `content`'s lines. Missing entries paint as [`LineVariant::Default`].
    pub line_variants: Option<Vec<LineVariant>>,
    pub size: CodeSize,
    pub variant: CodeVariant,
    /// `None` is automatic: full blocks number lines when there are more than 8,
    /// or when [`Self::start_line`] is set. Compact stays unnumbered until `Some(true)`.
    pub line_numbers: Option<bool>,
    /// 1-based first line. `None` starts at 1 when numbers are on.
    pub start_line: Option<u32>,
    /// Fence closed. Copy stays disabled while this is false.
    pub closed: bool,
    /// Show-more is open. Compact ignores this and keeps the 256 cap.
    pub expanded: bool,
    pub copied: bool,
    /// When Copy was clicked: the check's crossfade and the "Copied" tip run
    /// from here (see [`copy_look`]). `None` with `copied` shows the check, still.
    pub copied_at: Option<Instant>,
    /// When the show-more toggle happened. `None` snaps to the resting cap.
    pub toggled_at: Option<Instant>,
    pub reduced: bool,
    /// Header slot: Run. Ignored on compact.
    pub runnable: bool,
    /// Header slot: Open in viewer. Ignored on compact.
    pub open_viewer: bool,
}

impl CodeBlockProps {
    /// Fixed, no toggle: compact output wraps, full blocks keep each line
    /// whole and scroll sideways.
    pub fn wraps(&self) -> bool {
        self.size == CodeSize::Compact
    }

    /// Copy starts enabled.
    pub fn new(id: impl Into<String>, content: impl Into<String>, size: CodeSize) -> Self {
        Self {
            id: id.into(),
            title: None,
            language: None,
            content: content.into(),
            spans: None,
            line_variants: None,
            size,
            variant: CodeVariant::Default,
            line_numbers: None,
            start_line: None,
            closed: true,
            expanded: false,
            copied: false,
            copied_at: None,
            toggled_at: None,
            reduced: false,
            runnable: false,
            open_viewer: false,
        }
    }
}

/// Copy feedback, in ms from the click: the check eases in over
/// [`COPIED_IN_MS`] while the "Copied" tip shows at once (a
/// [`COPIED_TIP_IN_MS`] fade), holds until [`COPIED_HOLD_MS`], then fades in
/// [`COPIED_OUT_MS`] and the copy icon eases back over the same.
pub const COPIED_IN_MS: f32 = 150.;
pub const COPIED_TIP_IN_MS: f32 = 120.;
pub const COPIED_HOLD_MS: f32 = 1200.;
pub const COPIED_OUT_MS: f32 = 100.;

/// The copy button this frame: how far the check has replaced the copy
/// icon, the "Copied" tip's opacity and y offset, and whether either moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CopyLook {
    pub check: f32,
    pub tip: f32,
    pub tip_dy: f32,
    pub animating: bool,
}

/// [`CopyLook`] at `now`. Ease-out throughout; reduced motion swaps without fades.
pub fn copy_look(
    copied: bool,
    copied_at: Option<Instant>,
    now: Instant,
    reduced: bool,
) -> CopyLook {
    let rest = CopyLook {
        check: 0.,
        tip: 0.,
        tip_dy: 0.,
        animating: false,
    };
    if !copied {
        return rest;
    }
    let Some(at) = copied_at else {
        return CopyLook { check: 1., ..rest };
    };
    let ms = now.saturating_duration_since(at).as_secs_f32() * 1000.;
    let tip_gone = COPIED_HOLD_MS + COPIED_OUT_MS;
    let icon_back = tip_gone + COPIED_OUT_MS;
    if reduced {
        return CopyLook {
            check: if ms < icon_back { 1. } else { 0. },
            tip: if ms < COPIED_HOLD_MS { 1. } else { 0. },
            ..rest
        };
    }
    let ease = crate::motion::ease_out;
    let check = if ms < COPIED_IN_MS {
        ease(ms / COPIED_IN_MS)
    } else if ms < tip_gone {
        1.
    } else if ms < icon_back {
        1. - ease((ms - tip_gone) / COPIED_OUT_MS)
    } else {
        0.
    };
    let rise = crate::view::tooltip::TIP_RISE;
    let (tip, tip_dy) = if ms < COPIED_TIP_IN_MS {
        let p = ease(ms / COPIED_TIP_IN_MS);
        (p, -rise * (1. - p))
    } else if ms < COPIED_HOLD_MS {
        (1., 0.)
    } else if ms < tip_gone {
        (1. - ease((ms - COPIED_HOLD_MS) / COPIED_OUT_MS), 0.)
    } else {
        (0., 0.)
    };
    CopyLook {
        check,
        tip,
        tip_dy,
        // Nothing moves during the hold: the owner wakes the block at
        // [`COPIED_HOLD_MS`] for the exit.
        animating: ms < COPIED_IN_MS.max(COPIED_TIP_IN_MS)
            || (ms >= COPIED_HOLD_MS && ms < icon_back),
    }
}

/// Fences that hold shell commands: they get "Insert in terminal".
pub fn is_shell(language: Option<&str>) -> bool {
    matches!(
        language.map(|l| l.trim().to_ascii_lowercase()).as_deref(),
        Some("bash" | "sh" | "zsh" | "shell" | "console")
    )
}

/// What "Insert in terminal" types: the block without its trailing newline.
/// A session with `$ ` prompts gives just its commands, prompts dropped.
pub fn terminal_text(content: &str) -> Option<String> {
    let prompted: Vec<&str> = content
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("$ "))
        .collect();
    let text = if prompted.is_empty() {
        content
            .trim_end()
            .trim_start_matches(['\n', '\r'])
            .to_string()
    } else {
        prompted.join("\n").trim_end().to_string()
    };
    (!text.trim().is_empty()).then_some(text)
}

/// One unified-diff row. `text` has the leading `+`/`-`/` ` marker removed.
/// Hunk rows keep the raw `@@` line. File headers are dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDiffLine {
    pub variant: LineVariant,
    pub text: String,
    pub old: Option<u32>,
    pub new: Option<u32>,
}

/// The bytes of `content` a compact block actually paints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayWindow {
    pub start: usize,
    pub end: usize,
    /// Cut for length. Full blocks are never cut.
    pub truncated: bool,
}

/// Body geometry for one frame, in logical px (zoom 1). [`render`] scales with [`zpx`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockMetrics {
    /// Padding plus lines, before the cap.
    pub content_h: f32,
    /// Scroller max height this frame.
    pub cap: f32,
    /// Paint the [`FADE_H`] fade. The body is taller than `cap`.
    pub fade: bool,
    /// Full size, and the cap can still grow: the "Show N more lines" row.
    pub show_more: bool,
    /// Lines hidden by the collapsed cap. The footer's N. Stable during the glide.
    pub more_lines: usize,
    /// Lines below `cap` this frame.
    pub hidden_lines: usize,
    /// Inside the 360 ms glide.
    pub animating: bool,
    /// 0 = chevron down, 1 = chevron up.
    pub chevron: f32,
}

/// Line-level edit script. Identical lines stay [`LineVariant::Default`].
/// A trailing newline does not invent an empty row (`str::lines` rules).
pub fn diff_lines(old: &str, new: &str) -> Vec<(LineVariant, String)> {
    let a = split_lines(old);
    let b = split_lines(new);
    if a.len().saturating_mul(b.len()) > DIFF_CELL_CAP {
        coarse_diff(&a, &b)
    } else {
        lcs_diff(&a, &b)
    }
}

/// Parse a unified diff into rows. Headers (`diff --git`, `index`, `---`, `+++`)
/// are dropped. Each `@@` line is [`LineVariant::Hunk`] and resets the counters.
pub fn parse_unified(diff: &str) -> Vec<ParsedDiffLine> {
    let mut out = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    for raw in diff.lines() {
        if raw.starts_with("diff --git") {
            in_hunk = false;
            continue;
        }
        if let Some((o, n)) = hunk_header(raw) {
            old = o;
            new = n;
            in_hunk = true;
            out.push(ParsedDiffLine {
                variant: LineVariant::Hunk,
                text: raw.to_string(),
                old: Some(old),
                new: Some(new),
            });
            continue;
        }
        if !in_hunk || raw.starts_with('\\') {
            continue;
        }
        if let Some(text) = raw.strip_prefix('+') {
            out.push(ParsedDiffLine {
                variant: LineVariant::Added,
                text: text.to_string(),
                old: None,
                new: Some(new),
            });
            new += 1;
        } else if let Some(text) = raw.strip_prefix('-') {
            out.push(ParsedDiffLine {
                variant: LineVariant::Removed,
                text: text.to_string(),
                old: Some(old),
                new: None,
            });
            old += 1;
        } else {
            let text = raw.strip_prefix(' ').unwrap_or(raw);
            out.push(ParsedDiffLine {
                variant: LineVariant::Default,
                text: text.to_string(),
                old: Some(old),
                new: Some(new),
            });
            old += 1;
            new += 1;
        }
    }
    out
}

/// Join a diff script into the `content` and `line_variants` a block paints.
/// Markers stay out of `content`, so copy never picks them up.
pub fn join_diff(lines: &[(LineVariant, String)]) -> (String, Vec<LineVariant>) {
    let text = lines
        .iter()
        .map(|(_, line)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let variants = lines.iter().map(|(variant, _)| *variant).collect();
    (text, variants)
}

/// Compact blocks keep the tail: [`keel_proto::OUTPUT_TAIL`] bytes, then the
/// last [`MAX_RENDERED_LINES`] lines, snapped to a line boundary.
/// Full blocks return the whole string.
pub fn display_window(size: CodeSize, content: &str) -> DisplayWindow {
    if size != CodeSize::Compact {
        return DisplayWindow {
            start: 0,
            end: content.len(),
            truncated: false,
        };
    }
    let mut start = 0usize;
    if content.len() > keel_proto::OUTPUT_TAIL {
        start = content.len().saturating_sub(keel_proto::OUTPUT_TAIL);
        while start < content.len() && !content.is_char_boundary(start) {
            start += 1;
        }
        if let Some(i) = content[start..].find('\n') {
            let next = start + i + 1;
            if next < content.len() {
                start = next;
            }
        } else if let Some(i) = content[..start].rfind('\n') {
            start = i + 1;
        }
    }
    let tail = &content[start..];
    let total = split_lines(tail).len();
    if total > MAX_RENDERED_LINES {
        let skip = total - MAX_RENDERED_LINES;
        let mut seen = 0usize;
        for (i, byte) in tail.bytes().enumerate() {
            if byte == b'\n' {
                seen += 1;
                if seen == skip {
                    start += i + 1;
                    break;
                }
            }
        }
    }
    while start < content.len() && !content.is_char_boundary(start) {
        start += 1;
    }
    DisplayWindow {
        start,
        end: content.len(),
        truncated: start > 0,
    }
}

/// Spans that are safe to paint. While the fence is open, runs stop at the
/// last completed line so a half-arrived line stays plain.
pub fn visible_spans(content: &str, spans: &Spans, closed: bool) -> Spans {
    let limit = if closed {
        content.len()
    } else {
        content.rfind('\n').map_or(0, |i| i + 1)
    };
    spans
        .iter()
        .filter_map(|(range, kind)| {
            let start = range.start.min(limit);
            let end = range.end.min(limit);
            (start < end).then(|| (start..end, *kind))
        })
        .collect()
}

/// `None` follows the full-block rule. Compact numbers only when asked.
pub fn line_numbers_on(
    size: CodeSize,
    choice: Option<bool>,
    lines: usize,
    start_line: Option<u32>,
) -> bool {
    match choice {
        Some(on) => on,
        None => size == CodeSize::Full && (lines > 8 || start_line.is_some()),
    }
}

pub fn show_more_label(hidden: usize) -> String {
    if hidden == 1 {
        "Show 1 more line".to_string()
    } else {
        format!("Show {hidden} more lines")
    }
}

/// Capped body height. `viewport_h` is logical px; pass 0 when the viewport
/// is unknown and the expanded block grows to its content.
/// Reduced motion snaps. Otherwise the cap glides for [`SHOW_MORE_MS`].
pub fn block_layout(
    size: CodeSize,
    expanded: bool,
    line_count: usize,
    viewport_h: f32,
    toggled_at: Option<Instant>,
    now: Instant,
    reduced: bool,
) -> BlockMetrics {
    let (line_h, pad) = line_box(size);
    let content_h = pad * 2. + line_h * line_count as f32;
    let (collapsed_h, expanded_h) = resting_caps(size, content_h, viewport_h);
    let can_grow = expanded_h > collapsed_h + 0.5;
    let (from, to) = if expanded {
        (collapsed_h, expanded_h)
    } else {
        (expanded_h, collapsed_h)
    };
    let (cap, animating, chevron) = glide_cap(from, to, expanded, toggled_at, now, reduced);
    let more_lines = if content_h > collapsed_cap_of(size) + 0.5 {
        lines_hidden(line_h, pad, line_count, collapsed_cap_of(size))
    } else {
        0
    };
    BlockMetrics {
        content_h,
        cap,
        fade: content_h > cap + 0.5,
        show_more: size == CodeSize::Full && more_lines > 0 && can_grow && (!expanded || animating),
        more_lines,
        hidden_lines: lines_hidden(line_h, pad, line_count, cap),
        animating,
        chevron,
    }
}

/// The block's outer height this frame (header, body cap, show-more row,
/// border), for the list's height estimate.
pub fn outer_height(props: &CodeBlockProps, viewport_h: f32, now: Instant) -> f32 {
    let logical = if zoom() > 0. {
        viewport_h / zoom()
    } else {
        viewport_h
    };
    let window = display_window(props.size, &props.content);
    let lines = split_line_ranges(&props.content[window.start..window.end])
        .len()
        .max(1);
    let m = block_layout(
        props.size,
        props.expanded,
        lines,
        logical,
        props.toggled_at,
        now,
        props.reduced,
    );
    let header = if props.size == CodeSize::Full {
        HEADER_H
    } else {
        0.
    };
    header + m.cap + if m.show_more { MORE_H } else { 0. } + 2.
}

/// The block. `viewport_h` is `window.viewport_size().height` in gpui px;
/// it is converted to logical px before the 70% cap is applied.
pub fn render(
    props: &CodeBlockProps,
    tokens: Tokens,
    syntax: &SyntaxPaintMap,
    viewport_h: f32,
    on_action: OnCodeAction,
) -> AnyElement {
    let logical = if zoom() > 0. {
        viewport_h / zoom()
    } else {
        viewport_h
    };
    let window = display_window(props.size, &props.content);
    let shown = &props.content[window.start..window.end];
    let rows = split_line_ranges(shown);
    let metrics = block_layout(
        props.size,
        props.expanded,
        rows.len(),
        logical,
        props.toggled_at,
        Instant::now(),
        props.reduced,
    );
    let skipped = props.content[..window.start]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    let variants = aligned_variants(props.line_variants.as_deref(), skipped, rows.len());
    let spans = spans_for(&props.content, window, props.spans.as_ref(), props.closed);
    let numbers = line_numbers_on(props.size, props.line_numbers, rows.len(), props.start_line);
    let first = props.start_line.filter(|n| *n > 0).unwrap_or(1);
    let bg = surface(tokens, props.variant);
    let border = border_of(tokens, props.variant);
    let plain = if props.variant == CodeVariant::Destructive {
        c(tokens.text)
    } else {
        c(syntax.plain << 8 | 0xFF)
    };
    let group: SharedString = format!("code-{}", props.id).into();
    let copy_body = shown.to_string();
    let copy_moving =
        copy_look(props.copied, props.copied_at, Instant::now(), props.reduced).animating;

    div()
        .group(group.clone())
        .relative()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .rounded(RADIUS_MD)
        .bg(c(bg))
        .border_1()
        .border_color(border)
        .overflow_hidden()
        .when(props.size == CodeSize::Full, |d| {
            d.child(header(
                props,
                tokens,
                border,
                copy_body.clone(),
                on_action.clone(),
            ))
        })
        .child(body(
            props,
            tokens,
            syntax,
            &group,
            &rows,
            &variants,
            &spans,
            numbers,
            first,
            plain,
            c(tokens.selection),
            bg,
            metrics,
            copy_body,
            on_action.clone(),
        ))
        .when(
            props.size == CodeSize::Compact && (window.truncated || metrics.fade),
            |d| {
                let on_action = on_action.clone();
                let content = props.content.clone();
                let title = props.title.clone().or_else(|| props.language.clone());
                let error = props.variant == CodeVariant::Destructive;
                d.child(floating_show_full(
                    &props.id,
                    tokens,
                    props.variant,
                    move |_, _, cx| {
                        on_action(
                            CodeAction::ShowFull {
                                content: content.clone(),
                                title: title.clone(),
                                error,
                            },
                            cx,
                        )
                    },
                ))
            },
        )
        .when(metrics.show_more, |d| {
            let label = show_more_label(metrics.more_lines);
            let turn = metrics.chevron;
            d.child(more_row(&props.id, "more", &label, turn, tokens, true, {
                let on_action = on_action.clone();
                move |_, _, cx| on_action(CodeAction::ShowMore, cx)
            }))
        })
        .when(metrics.animating || copy_moving, |d| {
            d.child(
                gpui::canvas(
                    |_, _, _| (),
                    |_, _, window, _| window.request_animation_frame(),
                )
                .absolute()
                .top_0()
                .left_0()
                .size(px(0.)),
            )
        })
        .into_any_element()
}

fn header(
    props: &CodeBlockProps,
    t: Tokens,
    border: Hsla,
    copy_text: String,
    on_action: OnCodeAction,
) -> AnyElement {
    let label = props
        .title
        .clone()
        .or_else(|| props.language.clone())
        .unwrap_or_else(|| "text".to_string());
    let file = props.title.is_some();
    div()
        .h(zpx(HEADER_H))
        .pl(zpx(FULL_PAD_X))
        .pr(zpx(4.))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(zpx(8.))
        .border_b_1()
        .border_color(border)
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(6.))
                .t_caption()
                .text_color(c(t.text_faint))
                .when(file, |d| d.child(Icon::File.el(zpx(12.))))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(label),
                ),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(2.))
                .when(props.open_viewer, |d| {
                    let on_action = on_action.clone();
                    let content = props.content.clone();
                    let title = props.title.clone().or_else(|| props.language.clone());
                    let error = props.variant == CodeVariant::Destructive;
                    d.child(icon_button(
                        format!("code-view-{}", props.id),
                        Icon::External.el(zpx(12.)).into_any_element(),
                        c(t.text_faint),
                        c(t.hover),
                        c(t.text),
                        true,
                        Some(("Open in viewer".into(), t)),
                        move |_, _, cx| {
                            on_action(
                                CodeAction::OpenViewer {
                                    content: content.clone(),
                                    title: title.clone(),
                                    error,
                                },
                                cx,
                            )
                        },
                    ))
                })
                .when(props.runnable, |d| {
                    let on_action = on_action.clone();
                    d.child(icon_button(
                        format!("code-run-{}", props.id),
                        Icon::Tool.el(zpx(12.)).into_any_element(),
                        c(t.text_faint),
                        c(t.hover),
                        c(t.text),
                        true,
                        Some(("Run in terminal".into(), t)),
                        move |_, _, cx| on_action(CodeAction::Run, cx),
                    ))
                })
                .when_some(insert_text(props), |d, text| {
                    let on_action = on_action.clone();
                    d.child(icon_button(
                        format!("code-insert-{}", props.id),
                        Icon::Terminal.el(zpx(12.)).into_any_element(),
                        c(t.text_faint),
                        c(t.hover),
                        c(t.text),
                        props.closed,
                        Some(("Insert in terminal".into(), t)),
                        move |_, _, cx| on_action(CodeAction::Insert { text: text.clone() }, cx),
                    ))
                })
                .child(copy_button(props, t, copy_text, on_action)),
        )
        .into_any_element()
}

fn body(
    props: &CodeBlockProps,
    t: Tokens,
    syntax: &SyntaxPaintMap,
    group: &SharedString,
    rows: &[(Range<usize>, String)],
    variants: &[LineVariant],
    spans: &Spans,
    numbers: bool,
    first: u32,
    plain: Hsla,
    selection: Hsla,
    bg: u32,
    metrics: BlockMetrics,
    copy_body: String,
    on_action: OnCodeAction,
) -> AnyElement {
    let (line_h, _) = line_box(props.size);
    let pad_x = if props.size == CodeSize::Compact {
        COMPACT_PAD_X
    } else {
        FULL_PAD_X
    };
    let step = code_type(props.size);
    let markers = variants.iter().any(|v| *v != LineVariant::Default);
    if props.size == CodeSize::Compact && !numbers && !markers {
        let mut highlights = Vec::new();
        if copy_body.starts_with("$ ") || copy_body == "$" {
            highlights.push((
                0..1,
                gpui::HighlightStyle {
                    color: Some(c(t.text_faint)),
                    ..Default::default()
                },
            ));
        }
        for (range, kind) in spans {
            let rgb = match kind {
                SyntaxKind::Comment => syntax.comment,
                SyntaxKind::String => syntax.string,
                SyntaxKind::Keyword => syntax.keyword,
                SyntaxKind::Number => syntax.number,
                SyntaxKind::Function => syntax.function,
                SyntaxKind::Type => syntax.type_name,
            };
            highlights.push((
                range.clone(),
                gpui::HighlightStyle {
                    color: Some(c(rgb << 8 | 0xFF)),
                    font_style: (*kind == SyntaxKind::Comment).then_some(gpui::FontStyle::Italic),
                    ..Default::default()
                },
            ));
        }
        let copy_for_float = copy_body.clone();
        let styled = if highlights.is_empty() {
            gpui::StyledText::new(copy_body)
        } else {
            gpui::StyledText::new(copy_body).with_highlights(highlights)
        };
        return div()
            .relative()
            .min_w_0()
            .overflow_hidden()
            .child(
                div()
                    .id(ElementId::Name(format!("code-body-{}", props.id).into()))
                    .w_full()
                    .min_w_0()
                    .max_h(zpx(metrics.cap))
                    .overflow_hidden()
                    .px(zpx(pad_x))
                    .py(zpx(line_box(props.size).1))
                    .type_step(step)
                    .font_family(MONO)
                    .text_color(plain)
                    .when(props.wraps(), |d| d.whitespace_normal())
                    .when(!props.wraps(), |d| d.whitespace_nowrap())
                    .child(styled),
            )
            .when(metrics.fade && props.size == CodeSize::Full, |d| {
                let solid = c(bg);
                let clear = solid.opacity(0.);
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(zpx(FADE_H))
                        .bg(gpui::linear_gradient(
                            180.,
                            gpui::linear_color_stop(clear, 0.),
                            gpui::linear_color_stop(solid, 1.),
                        )),
                )
            })
            .when(props.size == CodeSize::Compact, {
                let group = group.clone();
                let on_action = on_action.clone();
                move |d| d.child(floating_copy(props, t, group, copy_for_float, on_action))
            })
            .into_any_element();
    }
    let digits = if numbers {
        let last = first.saturating_add(rows.len().saturating_sub(1) as u32);
        decimal_digits(last).max(decimal_digits(first))
    } else {
        0
    };
    let num_w = if numbers {
        digits as f32 * step.size * 0.7 + NUMBER_GAP
    } else {
        0.
    };
    let order = order_base(&props.id);
    let lines = rows.iter().enumerate().map(|(ix, (range, text))| {
        let variant = variants.get(ix).copied().unwrap_or(LineVariant::Default);
        line_row(
            props,
            t,
            syntax,
            spans,
            variant,
            text,
            range.start,
            ix,
            numbers,
            first.saturating_add(ix as u32),
            num_w,
            markers,
            plain,
            selection,
            line_h,
            order + ix as u64,
        )
    });
    div()
        .relative()
        .min_w_0()
        .overflow_hidden()
        .child(
            div()
                .id(ElementId::Name(format!("code-body-{}", props.id).into()))
                .w_full()
                .min_w_0()
                .max_h(zpx(metrics.cap))
                .px(zpx(pad_x))
                .py(zpx(line_box(props.size).1))
                .when(props.size == CodeSize::Full, |d| d.overflow_y_scroll())
                .when(props.size == CodeSize::Compact, |d| d.overflow_hidden())
                .when(!props.wraps(), |d| d.overflow_x_scroll())
                .type_step(step)
                .font_family(MONO)
                .text_color(plain)
                .child(
                    div()
                        .min_w_full()
                        .when(props.wraps(), |d| d.w_full())
                        .flex()
                        .flex_col()
                        .children(lines),
                ),
        )
        .when(metrics.fade && props.size == CodeSize::Full, |d| {
            let solid = c(bg);
            let clear = solid.opacity(0.);
            d.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(zpx(FADE_H))
                    .bg(gpui::linear_gradient(
                        180.,
                        gpui::linear_color_stop(clear, 0.),
                        gpui::linear_color_stop(solid, 1.),
                    )),
            )
        })
        .when(props.size == CodeSize::Compact, {
            let group = group.clone();
            let on_action = on_action.clone();
            move |d| d.child(floating_copy(props, t, group, copy_body, on_action))
        })
        .into_any_element()
}

fn line_row(
    props: &CodeBlockProps,
    t: Tokens,
    syntax: &SyntaxPaintMap,
    spans: &Spans,
    variant: LineVariant,
    text: &str,
    base: usize,
    index: usize,
    numbers: bool,
    number: u32,
    num_w: f32,
    markers: bool,
    plain: Hsla,
    selection: Hsla,
    line_h: f32,
    order: u64,
) -> AnyElement {
    let (wash, mark, mark_color) = match variant {
        LineVariant::Added => (Some(c(t.success).opacity(0.12)), "+", c(t.success)),
        LineVariant::Removed => (Some(c(t.danger).opacity(0.12)), "−", c(t.danger)),
        LineVariant::Highlighted => (Some(c(t.text).opacity(0.08)), ">", c(t.text_muted)),
        LineVariant::Hunk => (None, "", c(t.text_faint)),
        LineVariant::Default => (None, "", plain),
    };
    let hunk = variant == LineVariant::Hunk;
    let dollar = props.size == CodeSize::Compact
        && index == 0
        && variant == LineVariant::Default
        && (text == "$" || text.starts_with("$ "));
    let highlights = if hunk {
        Vec::new()
    } else {
        line_highlights(text.len(), base, spans, syntax, dollar, c(t.text_faint))
    };
    let color = if hunk { c(t.text_faint) } else { plain };
    div()
        .min_w_full()
        .when(props.wraps(), |d| d.w_full().min_w_0())
        .min_h(zpx(line_h))
        .flex()
        .flex_row()
        .items_start()
        .when_some(wash, |d, wash| d.bg(wash))
        .when(numbers, |d| {
            d.child(
                div()
                    .flex_none()
                    .w(zpx(num_w))
                    .pr(zpx(NUMBER_GAP))
                    .min_h(zpx(line_h))
                    .text_right()
                    .text_color(c(t.text_faint))
                    .opacity(0.7)
                    .child(if hunk {
                        String::new()
                    } else {
                        number.to_string()
                    }),
            )
        })
        .when(markers, |d| {
            d.child(
                div()
                    .flex_none()
                    .w(zpx(MARKER_W))
                    .min_h(zpx(line_h))
                    .flex()
                    .justify_center()
                    .text_color(mark_color)
                    .child(mark),
            )
        })
        .child(line_text(
            props, text, index, order, color, selection, highlights,
        ))
        .into_any_element()
}

fn line_text(
    props: &CodeBlockProps,
    text: &str,
    index: usize,
    order: u64,
    color: Hsla,
    selection: Hsla,
    highlights: Vec<(Range<usize>, gpui::HighlightStyle)>,
) -> AnyElement {
    let painted = text.to_string();
    let styled = if highlights.is_empty() {
        gpui::StyledText::new(painted.clone())
    } else {
        gpui::StyledText::new(painted.clone()).with_highlights(highlights)
    };
    if props.size == CodeSize::Compact {
        return div()
            .min_w_0()
            .min_h(zpx(line_box(props.size).0))
            .text_color(color)
            .when(props.wraps(), |d| d.flex_1().whitespace_normal())
            .when(!props.wraps(), |d| d.flex_none().whitespace_nowrap())
            .child(styled)
            .into_any_element();
    }
    let layout = styled.layout().clone();
    LineText {
        key: format!("sel-code-{}-{index}", props.id).into(),
        text: painted.into(),
        layout,
        order,
        selection,
        child: div()
            .min_w_0()
            .min_h(zpx(line_box(props.size).0))
            .text_color(color)
            .when(props.wraps(), |d| d.flex_1().whitespace_normal())
            .when(!props.wraps(), |d| d.flex_none().whitespace_nowrap())
            .child(styled)
            .into_any_element(),
    }
    .into_any_element()
}

fn line_highlights(
    len: usize,
    base: usize,
    spans: &Spans,
    syntax: &SyntaxPaintMap,
    dollar: bool,
    faint: Hsla,
) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
    let mut out = Vec::new();
    let skip = usize::from(dollar);
    if dollar && len > 0 {
        out.push((
            0..1,
            gpui::HighlightStyle {
                color: Some(faint),
                ..Default::default()
            },
        ));
    }
    for (range, kind) in spans {
        if range.end <= base || range.start >= base + len {
            continue;
        }
        let mut start = range.start.max(base) - base;
        let end = range.end.min(base + len) - base;
        if start < skip {
            start = skip;
        }
        if start >= end {
            continue;
        }
        let rgb = match kind {
            SyntaxKind::Keyword => syntax.keyword,
            SyntaxKind::String => syntax.string,
            SyntaxKind::Comment => syntax.comment,
            SyntaxKind::Number => syntax.number,
            SyntaxKind::Function => syntax.function,
            SyntaxKind::Type => syntax.type_name,
        };
        out.push((
            start..end,
            gpui::HighlightStyle {
                color: Some(c(rgb << 8 | 0xFF)),
                font_style: (*kind == SyntaxKind::Comment).then_some(gpui::FontStyle::Italic),
                ..Default::default()
            },
        ));
    }
    out
}

fn floating_copy(
    props: &CodeBlockProps,
    t: Tokens,
    group: SharedString,
    text: String,
    on_action: OnCodeAction,
) -> AnyElement {
    let shown = if props.closed { 1. } else { 0.45 };
    let hover_opacity = shown;
    let focus_opacity = shown;
    let color = c(t.text_faint);
    let hover_fg = c(t.text);
    let hover_bg = c(t.hover);
    let look = copy_look(props.copied, props.copied_at, Instant::now(), props.reduced);
    let closed = props.closed;
    let id = props.id.clone();
    let mut button = div()
        .id(ElementId::Name(format!("code-float-{id}").into()))
        .absolute()
        .top(zpx(FLOAT_INSET))
        .right(zpx(FLOAT_INSET))
        .size(zpx(BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded(RADIUS_MD)
        .bg(c(surface(t, props.variant)))
        .border_1()
        .border_color(border_of(t, props.variant))
        .text_color(color)
        // Stays up while "Copied" shows, even if the pointer left.
        .opacity(if props.copied { shown } else { 0. })
        .group_hover(group, move |s| s.opacity(hover_opacity))
        .focus(move |s| s.opacity(focus_opacity))
        .focusable();
    if closed {
        button = button
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
            .on_click(move |_, _, cx| {
                on_action(CodeAction::Copy { text: text.clone() }, cx);
            });
    }
    button
        .child(copy_icon(look, t))
        .children(copied_tip(look, t))
        .into_any_element()
}

fn copy_button(
    props: &CodeBlockProps,
    t: Tokens,
    text: String,
    on_action: OnCodeAction,
) -> AnyElement {
    let look = copy_look(props.copied, props.copied_at, Instant::now(), props.reduced);
    // While "Copied" shows, the hover tooltip stays away (no "Copy" under it).
    let tip = (!props.copied).then(|| ("Copy".into(), t));
    let button = icon_button(
        format!("code-copy-{}", props.id),
        copy_icon(look, t),
        c(t.text_faint),
        c(t.hover),
        c(t.text),
        props.closed,
        tip,
        move |_, _, cx| on_action(CodeAction::Copy { text: text.clone() }, cx),
    );
    match copied_tip(look, t) {
        Some(tip) => div()
            .relative()
            .flex_none()
            .child(button)
            .child(tip)
            .into_any_element(),
        None => button,
    }
}

/// The shell text "Insert in terminal" would type, when the block offers it.
fn insert_text(props: &CodeBlockProps) -> Option<String> {
    if props.runnable || !is_shell(props.language.as_deref()) {
        return None;
    }
    terminal_text(&props.content)
}

/// Copy and check stacked in one 12 px box, crossfading: no layout shift.
fn copy_icon(look: CopyLook, t: Tokens) -> AnyElement {
    let layer = |icon: Icon, opacity: f32| {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .opacity(opacity)
            .child(icon.el(zpx(12.)))
    };
    let mut stack = div().relative().flex_none().size(zpx(12.));
    if look.check < 1. {
        stack = stack.child(layer(Icon::Copy, 1. - look.check));
    }
    if look.check > 0. {
        stack = stack.child(layer(Icon::Check, look.check).text_color(c(t.success)));
    }
    stack.into_any_element()
}

/// Width of the slot the "Copied" tip centers in, logical px.
const COPIED_SLOT: f32 = 96.;

/// "Copied" under the button, on the tooltip capsule. Deferred, so the
/// block's clip and the code below don't cover it; absolute, so nothing moves.
fn copied_tip(look: CopyLook, t: Tokens) -> Option<AnyElement> {
    if look.tip <= 0. {
        return None;
    }
    let gap = crate::view::tooltip::TIP_GAP;
    Some(
        div()
            .absolute()
            .top(zpx(BUTTON + gap))
            .left(zpx(BUTTON / 2. - COPIED_SLOT / 2.))
            .w(zpx(COPIED_SLOT))
            .flex()
            .justify_center()
            .child(
                gpui::deferred(
                    crate::view::tooltip::capsule(t, "Copied", None)
                        .top(zpx(look.tip_dy))
                        .opacity(look.tip),
                )
                .with_priority(2),
            )
            .into_any_element(),
    )
}

fn icon_button(
    id: String,
    icon: AnyElement,
    color: Hsla,
    hover_bg: Hsla,
    hover_fg: Hsla,
    enabled: bool,
    tip: Option<(SharedString, Tokens)>,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut App) + 'static,
) -> AnyElement {
    let button = div()
        .id(ElementId::Name(id.into()))
        .flex_none()
        .size(zpx(BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded(RADIUS_MD)
        .text_color(color)
        .focusable();
    let button = if enabled {
        button
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
            .active(move |s| s.bg(hover_bg).opacity(0.75))
            .on_click(on_click)
    } else {
        drop(on_click);
        button
    };
    let button = match tip.filter(|_| enabled) {
        Some((text, t)) => button
            .tooltip(move |_, cx| cx.new(|_| crate::view::Tip::new(text.clone(), t)).into())
            .tooltip_show_delay(crate::view::tooltip::delay()),
        None => button,
    };
    button.child(icon).into_any_element()
}

fn floating_show_full(
    id: &str,
    t: Tokens,
    variant: CodeVariant,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut App) + 'static,
) -> AnyElement {
    let error = variant == CodeVariant::Destructive;
    let solid = c(surface(t, variant));
    let clear = solid.opacity(0.);
    let (pill_bg, pill_hover_bg, pill_border, pill_hover_border, pill_text, pill_hover_text) =
        if error {
            (
                c(mix(t.danger, t.code, 0.20)),
                c(mix(t.danger, t.code, 0.32)),
                c(t.danger).opacity(0.40),
                c(t.danger).opacity(0.75),
                c(t.danger),
                c(t.text),
            )
        } else {
            (
                c(mix(t.hover, t.surface, 0.50)),
                c(t.hover),
                c(t.border).opacity(0.55),
                c(t.border),
                c(t.text_muted),
                c(t.text),
            )
        };

    let on_click_bg = std::rc::Rc::new(on_click);
    let on_click_pill = on_click_bg.clone();

    div()
        .id(ElementId::Name(format!("code-fade-{}", id).into()))
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(zpx(68.))
        .bg(gpui::linear_gradient(
            180.,
            gpui::linear_color_stop(clear, 0.),
            gpui::linear_color_stop(solid, 1.),
        ))
        .flex()
        .items_end()
        .justify_center()
        .pb(zpx(8.))
        .cursor_pointer()
        .on_click(move |ev, window, cx| on_click_bg(ev, window, cx))
        .child(
            div()
                .id(ElementId::Name(format!("code-pill-{}", id).into()))
                .h(zpx(24.))
                .px(zpx(12.))
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(zpx(5.))
                .rounded_full()
                .bg(pill_bg)
                .border_1()
                .border_color(pill_border)
                .shadow(vec![gpui::BoxShadow {
                    color: gpui::black().opacity(0.45),
                    offset: gpui::point(px(0.), px(3.)),
                    blur_radius: px(8.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .t_caption()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(pill_text)
                .hover(move |s| {
                    s.bg(pill_hover_bg)
                        .border_color(pill_hover_border)
                        .text_color(pill_hover_text)
                })
                .on_click(move |ev, window, cx| on_click_pill(ev, window, cx))
                .child(Icon::Expand.el(zpx(11.)))
                .child("Show full output"),
        )
        .into_any_element()
}

fn more_row(
    id: &str,
    kind: &str,
    label: &str,
    turn: f32,
    t: Tokens,
    chevron: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(ElementId::Name(format!("code-{kind}-{id}").into()))
        .h(zpx(MORE_H))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(zpx(4.))
        .bg(c(t.code))
        .border_t_1()
        .border_color(c(t.border).opacity(0.40))
        .t_caption()
        .text_color(c(t.text_muted))
        .cursor_pointer()
        .hover(|s| s.text_color(c(t.text)))
        .hover_fade(c(t.hover), Some(c(t.selected)))
        .active(|s| s.opacity(0.85))
        .on_click(on_click)
        .child(label.to_string())
        .when(chevron, |d| {
            d.child(Icon::ChevronDown.el(zpx(12.)).with_transformation(
                gpui::Transformation::rotate(gpui::radians(turn * std::f32::consts::PI)),
            ))
        })
        .into_any_element()
}

/// Code column only. Registered with the window selection so a drag copies
/// the source line and not the gutter. One handle per line; `order` is the
/// document order, so a drag across lines joins with newlines.
struct LineText {
    key: SharedString,
    text: SharedString,
    layout: gpui::TextLayout,
    order: u64,
    /// Text-selection wash. The gutter is not part of this element, so it is not copied.
    selection: Hsla,
    child: AnyElement,
}

impl IntoElement for LineText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for LineText {
    type RequestLayoutState = ();
    type PrepaintState = (gpui_base::TextSelectionHandle, Hitbox);

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut gpui::Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<gpui::Pixels>,
        _: &mut (),
        window: &mut gpui::Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.child.prepaint(window, cx);
        let handle = window
            .use_keyed_state(ElementId::Name(self.key.clone()), cx, |_, cx| {
                gpui_base::TextSelectionHandle::new("", cx)
            })
            .read(cx)
            .clone();
        let hitbox = window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal);
        handle.register(
            gpui_base::TextSelectionRegistration::new(hitbox.clone(), bounds)
                .with_document_order(self.order)
                .with_text_bounds(vec![bounds]),
            window,
            cx,
        );
        (handle, hitbox)
    }

    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<gpui::Pixels>,
        _: &mut (),
        (handle, hitbox): &mut Self::PrepaintState,
        window: &mut gpui::Window,
        cx: &mut App,
    ) {
        window.set_cursor_style(gpui::CursorStyle::IBeam, hitbox);
        self.child.paint(window, cx);
        let run = gpui_base::TextSelectionRun::new(self.text.clone(), self.layout.clone(), bounds)
            .with_document_order(self.order);
        let range = handle
            .update_runs(&[run], cx)
            .ranges()
            .first()
            .cloned()
            .flatten();
        let Some(range) = range else { return };
        let (Some(start), Some(end)) = (
            self.layout.position_for_index(range.start),
            self.layout.position_for_index(range.end),
        ) else {
            return;
        };
        let lh = self.layout.line_height();
        let quad = |a: gpui::Point<gpui::Pixels>, b: gpui::Point<gpui::Pixels>| {
            gpui::fill(Bounds::from_corners(a, b), self.selection)
        };
        if start.y == end.y {
            window.paint_quad(quad(start, gpui::point(end.x, end.y + lh)));
        } else {
            window.paint_quad(quad(start, gpui::point(bounds.right(), start.y + lh)));
            if end.y > start.y + lh {
                window.paint_quad(quad(
                    gpui::point(bounds.left(), start.y + lh),
                    gpui::point(bounds.right(), end.y),
                ));
            }
            window.paint_quad(quad(
                gpui::point(bounds.left(), end.y),
                gpui::point(end.x, end.y + lh),
            ));
        }
    }
}

pub fn surface(t: Tokens, variant: CodeVariant) -> u32 {
    match variant {
        CodeVariant::Default => t.code,
        // danger @ 6% over the code fill.
        CodeVariant::Destructive => mix(t.danger, t.code, 0.06),
    }
}

pub fn border_of(t: Tokens, variant: CodeVariant) -> Hsla {
    match variant {
        CodeVariant::Default => c(t.border),
        CodeVariant::Destructive => c(t.danger).opacity(0.35),
    }
}

pub fn mix(fg: u32, bg: u32, alpha: f32) -> u32 {
    let channel = |shift: u32| {
        let top = ((fg >> shift) & 0xFF) as f32;
        let under = ((bg >> shift) & 0xFF) as f32;
        (top * alpha + under * (1. - alpha)).round().clamp(0., 255.) as u32
    };
    (channel(24) << 24) | (channel(16) << 16) | (channel(8) << 8) | 0xFF
}

fn code_type(size: CodeSize) -> TypeStep {
    match size {
        CodeSize::Compact => TypeStep {
            size: 11.5,
            line: COMPACT_LINE_H,
            weight: gpui::FontWeight::NORMAL,
        },
        CodeSize::Full => CODE,
    }
}

fn line_box(size: CodeSize) -> (f32, f32) {
    match size {
        CodeSize::Compact => (COMPACT_LINE_H, COMPACT_PAD_Y),
        CodeSize::Full => (FULL_LINE_H, FULL_PAD_Y),
    }
}

fn collapsed_cap_of(size: CodeSize) -> f32 {
    match size {
        CodeSize::Compact => COMPACT_CAP,
        CodeSize::Full => FULL_CAP,
    }
}

/// `(collapsed height, expanded height)`, each already clamped to the content.
fn resting_caps(size: CodeSize, content_h: f32, viewport_h: f32) -> (f32, f32) {
    let collapsed = collapsed_cap_of(size);
    let collapsed_h = content_h.min(collapsed);
    let expanded_h = match size {
        CodeSize::Compact => collapsed_h,
        CodeSize::Full => {
            let limit = if viewport_h > 0. {
                viewport_h * EXPANDED_VIEWPORT
            } else {
                content_h
            };
            // Show-more grows. A short window does not pull the cap under 480.
            content_h.min(limit.max(collapsed_h))
        }
    };
    (collapsed_h, expanded_h)
}

fn glide_cap(
    from: f32,
    to: f32,
    expanded: bool,
    toggled_at: Option<Instant>,
    now: Instant,
    reduced: bool,
) -> (f32, bool, f32) {
    let resting = if expanded { 1. } else { 0. };
    if (to - from).abs() < 0.5 {
        return (to, false, resting);
    }
    let Some(at) = toggled_at else {
        return (to, false, resting);
    };
    if reduced {
        return (to, false, resting);
    }
    let elapsed = now.saturating_duration_since(at);
    let window = Duration::from_millis(if expanded { SHOW_MORE_MS } else { SHOW_LESS_MS });
    let t = (elapsed.as_secs_f32() / window.as_secs_f32()).clamp(0., 1.);
    let eased = glide_curve(t);
    let cap = from + (to - from) * eased;
    let chevron = if expanded { eased } else { 1. - eased };
    (cap, t < 1., chevron)
}

fn lines_hidden(line_h: f32, pad: f32, line_count: usize, cap: f32) -> usize {
    if line_h <= 0. {
        return 0;
    }
    let inner = (cap - pad * 2.).max(0.);
    // The epsilon keeps `n * line_h / line_h` from flooring to n - 1.
    let visible = ((inner + 0.01) / line_h).floor().max(0.) as usize;
    line_count.saturating_sub(visible)
}

fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.lines().collect()
    }
}

fn split_line_ranges(text: &str) -> Vec<(Range<usize>, String)> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    for line in text.split_inclusive('\n') {
        let end = start + line.len();
        let bare = line.strip_suffix('\n').unwrap_or(line);
        out.push((start..start + bare.len(), bare.to_string()));
        start = end;
    }
    out
}

fn aligned_variants(all: Option<&[LineVariant]>, skipped: usize, shown: usize) -> Vec<LineVariant> {
    let mut out = Vec::with_capacity(shown);
    for i in 0..shown {
        out.push(
            all.and_then(|v| v.get(skipped + i))
                .copied()
                .unwrap_or(LineVariant::Default),
        );
    }
    out
}

fn spans_for(content: &str, window: DisplayWindow, spans: Option<&Spans>, closed: bool) -> Spans {
    let Some(spans) = spans else {
        return Vec::new();
    };
    let shifted = spans
        .iter()
        .filter_map(|(range, kind)| {
            let start = range.start.max(window.start);
            let end = range.end.min(window.end);
            (start < end).then(|| (start - window.start..end - window.start, *kind))
        })
        .collect::<Spans>();
    visible_spans(&content[window.start..window.end], &shifted, closed)
}

fn hunk_header(raw: &str) -> Option<(u32, u32)> {
    let rest = raw.strip_prefix("@@ ")?;
    let mut parts = rest.split_whitespace();
    Some((hunk_start(parts.next()?)?, hunk_start(parts.next()?)?))
}

fn hunk_start(token: &str) -> Option<u32> {
    let token = token.strip_prefix(['+', '-'])?;
    token.split(',').next()?.parse().ok()
}

fn lcs_diff(a: &[&str], b: &[&str]) -> Vec<(LineVariant, String)> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((LineVariant::Default, a[i].to_string()));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push((LineVariant::Removed, a[i].to_string()));
            i += 1;
        } else {
            out.push((LineVariant::Added, b[j].to_string()));
            j += 1;
        }
    }
    while i < n {
        out.push((LineVariant::Removed, a[i].to_string()));
        i += 1;
    }
    while j < m {
        out.push((LineVariant::Added, b[j].to_string()));
        j += 1;
    }
    out
}

/// Shared prefix and suffix stay, and the middle is a delete plus an insert.
/// Used when a full LCS table would be large; the edit script is correct, not minimal.
fn coarse_diff(a: &[&str], b: &[&str]) -> Vec<(LineVariant, String)> {
    let mut pre = 0usize;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0usize;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }
    let mut out = Vec::new();
    for line in &a[..pre] {
        out.push((LineVariant::Default, (*line).to_string()));
    }
    for line in &a[pre..a.len() - suf] {
        out.push((LineVariant::Removed, (*line).to_string()));
    }
    for line in &b[pre..b.len() - suf] {
        out.push((LineVariant::Added, (*line).to_string()));
    }
    for line in &a[a.len() - suf..] {
        out.push((LineVariant::Default, (*line).to_string()));
    }
    out
}

fn decimal_digits(n: u32) -> usize {
    let mut digits = 1usize;
    let mut x = n;
    while x >= 10 {
        x /= 10;
        digits += 1;
    }
    digits
}

fn order_base(id: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash & 0xFFFF_FFFF) << 16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(got: f32, want: f32) {
        assert!((got - want).abs() < 0.05, "{got} != {want}");
    }

    fn at() -> Instant {
        Instant::now()
    }

    #[test]
    fn diff_lines_keeps_shared_lines_and_marks_the_edit() {
        let got = diff_lines("a\nb\nc", "a\nx\nc");
        assert_eq!(
            got,
            vec![
                (LineVariant::Default, "a".into()),
                (LineVariant::Removed, "b".into()),
                (LineVariant::Added, "x".into()),
                (LineVariant::Default, "c".into()),
            ]
        );
    }

    #[test]
    fn diff_lines_insertion_deletion_and_no_phantom_newline() {
        assert_eq!(
            diff_lines("b", "a\nb"),
            vec![
                (LineVariant::Added, "a".into()),
                (LineVariant::Default, "b".into())
            ]
        );
        assert_eq!(
            diff_lines("a\nb", "b"),
            vec![
                (LineVariant::Removed, "a".into()),
                (LineVariant::Default, "b".into())
            ]
        );
        assert_eq!(diff_lines("", ""), Vec::<(LineVariant, String)>::new());
        assert_eq!(
            diff_lines("a\n", "a"),
            vec![(LineVariant::Default, "a".into())]
        );
        assert_eq!(
            diff_lines("", "a\nb"),
            vec![
                (LineVariant::Added, "a".into()),
                (LineVariant::Added, "b".into())
            ]
        );
    }

    #[test]
    fn coarse_diff_matches_a_single_middle_edit() {
        let a = ["a", "b", "c"];
        let b = ["a", "x", "c"];
        assert_eq!(
            coarse_diff(&a, &b),
            vec![
                (LineVariant::Default, "a".into()),
                (LineVariant::Removed, "b".into()),
                (LineVariant::Added, "x".into()),
                (LineVariant::Default, "c".into()),
            ]
        );
    }

    #[test]
    fn join_diff_drops_markers_from_the_text() {
        let lines = diff_lines("a", "b");
        let (text, variants) = join_diff(&lines);
        assert_eq!(text, "a\nb");
        assert_eq!(variants, vec![LineVariant::Removed, LineVariant::Added]);
    }

    #[test]
    fn parse_unified_drops_headers_and_numbers_the_hunk() {
        let diff = "\
diff --git a/a.rs b/a.rs
index 111..222 100644
--- a/a.rs
+++ b/a.rs
@@ -1,3 +10,3 @@ fn main
 context
-old
+new
 stay
\\ No newline at end of file
";
        let got = parse_unified(diff);
        assert_eq!(got[0].variant, LineVariant::Hunk);
        assert_eq!(got[0].old, Some(1));
        assert_eq!(got[0].new, Some(10));
        assert!(got[0].text.starts_with("@@"));
        assert_eq!(
            got[1],
            ParsedDiffLine {
                variant: LineVariant::Default,
                text: "context".into(),
                old: Some(1),
                new: Some(10)
            }
        );
        assert_eq!(
            got[2],
            ParsedDiffLine {
                variant: LineVariant::Removed,
                text: "old".into(),
                old: Some(2),
                new: None
            }
        );
        assert_eq!(
            got[3],
            ParsedDiffLine {
                variant: LineVariant::Added,
                text: "new".into(),
                old: None,
                new: Some(11)
            }
        );
        assert_eq!(
            got[4],
            ParsedDiffLine {
                variant: LineVariant::Default,
                text: "stay".into(),
                old: Some(3),
                new: Some(12)
            }
        );
        assert_eq!(got.len(), 5);
    }

    #[test]
    fn parse_unified_second_hunk_resets_and_single_line_headers_work() {
        let diff = "\
@@ -10 +20 @@
 a
-b
+c
diff --git a/b.rs b/b.rs
@@ -30,1 +40,1 @@
 d
";
        let got = parse_unified(diff);
        assert_eq!(got[0].variant, LineVariant::Hunk);
        assert_eq!((got[0].old, got[0].new), (Some(10), Some(20)));
        assert_eq!(got[1].variant, LineVariant::Default);
        assert_eq!((got[1].old, got[1].new), (Some(10), Some(20)));
        assert_eq!(got[2].text, "b");
        assert_eq!(got[2].old, Some(11));
        assert_eq!(got[3].text, "c");
        assert_eq!(got[3].new, Some(21));
        assert_eq!(got[4].variant, LineVariant::Hunk);
        assert_eq!((got[4].old, got[4].new), (Some(30), Some(40)));
        assert_eq!(
            got[5],
            ParsedDiffLine {
                variant: LineVariant::Default,
                text: "d".into(),
                old: Some(30),
                new: Some(40)
            }
        );
        assert!(parse_unified("diff --git a/a b/a\n--- a\n+++ b\n").is_empty());
    }

    #[test]
    fn layout_short_full_block_is_uncapped() {
        let m = block_layout(CodeSize::Full, false, 3, 1000., None, at(), false);
        near(m.content_h, FULL_PAD_Y * 2. + FULL_LINE_H * 3.);
        near(m.cap, m.content_h);
        assert!(!m.fade);
        assert!(!m.show_more);
        assert_eq!(m.more_lines, 0);
        assert!(!m.animating);
        assert_eq!(FADE_H, 16.);
    }

    #[test]
    fn layout_full_collapsed_caps_at_480_and_counts_hidden_lines() {
        let lines = 40;
        let m = block_layout(CodeSize::Full, false, lines, 1000., None, at(), false);
        near(m.cap, FULL_CAP);
        assert!(m.fade);
        assert!(m.show_more);
        let visible = ((FULL_CAP - FULL_PAD_Y * 2. + 0.01) / FULL_LINE_H).floor() as usize;
        assert_eq!(m.more_lines, lines - visible);
        assert_eq!(m.hidden_lines, m.more_lines);
        assert_eq!(
            show_more_label(m.more_lines),
            format!("Show {} more lines", m.more_lines)
        );
        assert_eq!(show_more_label(1), "Show 1 more line");
    }

    #[test]
    fn layout_show_more_glides_from_480_toward_the_viewport() {
        let now = at();
        let lines = 100;
        let viewport = 1000.;
        let content = FULL_PAD_Y * 2. + FULL_LINE_H * lines as f32;
        let target = (viewport * EXPANDED_VIEWPORT).min(content);
        assert!(target > FULL_CAP);

        let start = block_layout(CodeSize::Full, true, lines, viewport, Some(now), now, false);
        near(start.cap, FULL_CAP);
        assert!(start.animating);
        assert!(start.show_more);
        near(start.chevron, 0.);

        let half = Duration::from_millis(SHOW_MORE_MS / 2);
        let mid = block_layout(
            CodeSize::Full,
            true,
            lines,
            viewport,
            Some(now),
            now + half,
            false,
        );
        let t = half.as_secs_f32() / Duration::from_millis(SHOW_MORE_MS).as_secs_f32();
        let eased = glide_curve(t);
        near(mid.cap, FULL_CAP + (target - FULL_CAP) * eased);
        assert!(
            mid.cap > FULL_CAP + (target - FULL_CAP) * 0.5,
            "glide starts fast"
        );
        assert!(mid.animating);

        let done = block_layout(
            CodeSize::Full,
            true,
            lines,
            viewport,
            Some(now),
            now + Duration::from_millis(SHOW_MORE_MS),
            false,
        );
        near(done.cap, target);
        assert!(!done.animating);
        assert!(!done.show_more);
        assert!(done.fade);
        near(done.chevron, 1.);

        let snap = block_layout(CodeSize::Full, true, lines, viewport, None, now, false);
        near(snap.cap, target);
        assert!(!snap.animating);
    }

    #[test]
    fn layout_collapse_glides_back_and_reduced_motion_snaps() {
        let now = at();
        let viewport = 1000.;
        let lines = 100;
        let target = viewport * EXPANDED_VIEWPORT;
        let closing = block_layout(
            CodeSize::Full,
            false,
            lines,
            viewport,
            Some(now),
            now,
            false,
        );
        near(closing.cap, target);
        assert!(closing.animating);
        near(closing.chevron, 1.);

        let shut = block_layout(
            CodeSize::Full,
            false,
            lines,
            viewport,
            Some(now),
            now + Duration::from_millis(SHOW_MORE_MS),
            false,
        );
        near(shut.cap, FULL_CAP);
        assert!(!shut.animating);
        assert!(shut.show_more);

        let reduced = block_layout(CodeSize::Full, true, lines, viewport, Some(now), now, true);
        near(reduced.cap, target);
        assert!(!reduced.animating);
    }

    #[test]
    fn layout_compact_caps_at_256_without_show_more() {
        let short = block_layout(CodeSize::Compact, true, 4, 1000., None, at(), false);
        near(short.cap, COMPACT_PAD_Y * 2. + COMPACT_LINE_H * 4.);
        assert!(!short.fade);
        assert!(!short.show_more);

        let long = block_layout(CodeSize::Compact, true, 40, 1000., Some(at()), at(), false);
        near(long.cap, COMPACT_CAP);
        assert!(long.fade);
        assert!(!long.show_more);
        assert!(!long.animating);
    }

    #[test]
    fn layout_short_viewport_does_not_shrink_or_offer_show_more() {
        let m = block_layout(CodeSize::Full, true, 200, 400., None, at(), false);
        near(m.cap, FULL_CAP);
        assert!(!m.show_more);
        assert!(m.fade);
        let open = block_layout(CodeSize::Full, true, 200, 0., None, at(), false);
        near(open.cap, open.content_h);
        assert!(!open.fade);
    }

    #[test]
    fn line_numbers_follow_the_full_block_rule() {
        assert!(!line_numbers_on(CodeSize::Full, None, 8, None));
        assert!(line_numbers_on(CodeSize::Full, None, 9, None));
        assert!(line_numbers_on(CodeSize::Full, None, 1, Some(40)));
        assert!(!line_numbers_on(CodeSize::Compact, None, 40, Some(3)));
        assert!(line_numbers_on(CodeSize::Compact, Some(true), 1, None));
        assert!(!line_numbers_on(CodeSize::Full, Some(false), 40, Some(1)));
    }

    #[test]
    fn visible_spans_stop_at_the_last_complete_line() {
        let content = "fn a() {}\nlet x =";
        let spans: Spans = vec![(0..2, SyntaxKind::Keyword), (12..15, SyntaxKind::Keyword)];
        let open = visible_spans(content, &spans, false);
        assert_eq!(open, vec![(0..2, SyntaxKind::Keyword)]);
        let closed = visible_spans(content, &spans, true);
        assert_eq!(closed.len(), 2);
        assert!(visible_spans("no newline yet", &spans, false).is_empty());
    }

    #[test]
    fn display_window_caps_compact_lines_and_bytes() {
        let lines = "x\n".repeat(MAX_RENDERED_LINES + 1);
        let window = display_window(CodeSize::Compact, &lines);
        assert!(window.truncated);
        assert!(lines[window.start..].lines().count() <= MAX_RENDERED_LINES);
        assert_eq!(lines.as_bytes()[window.start - 1], b'\n');

        let full = display_window(CodeSize::Full, &lines);
        assert!(!full.truncated);
        assert_eq!(full.start, 0);

        let wide = format!(
            "{}{}",
            "abcdefghij\n".repeat(20),
            "z".repeat(keel_proto::OUTPUT_TAIL)
        );
        let cut = display_window(CodeSize::Compact, &wide);
        assert!(cut.truncated);
        assert!(wide.is_char_boundary(cut.start));
        assert!(cut.start > 0);
        assert!(wide[cut.start..].lines().count() <= MAX_RENDERED_LINES);
    }

    fn look_at(ms: u64, reduced: bool) -> CopyLook {
        let t0 = Instant::now();
        copy_look(true, Some(t0), t0 + Duration::from_millis(ms), reduced)
    }

    #[test]
    fn copy_swaps_to_a_check_and_shows_copied_at_once() {
        let start = look_at(0, false);
        assert!(start.animating);
        near(start.check, 0.);
        // Eased out: well past halfway at a third of the way in.
        assert!(look_at(50, false).check > 0.5);
        let settled = look_at(COPIED_IN_MS as u64, false);
        near(settled.check, 1.);
        near(settled.tip, 1.);
        near(settled.tip_dy, 0.);
        // The tip is already visible on the first frames, not after a hover delay.
        assert!(look_at(40, false).tip > 0.5);
    }

    #[test]
    fn copied_holds_without_frames_then_fades_quicker_than_it_came() {
        let hold = look_at(800, false);
        assert!(!hold.animating, "no frames while nothing moves");
        assert_eq!((hold.check, hold.tip), (1., 1.));
        let leaving = look_at(COPIED_HOLD_MS as u64 + 50, false);
        assert!(leaving.animating && leaving.tip > 0. && leaving.tip < 1.);
        assert_eq!(leaving.check, 1., "the check waits for the tip to go");
        let gone = look_at((COPIED_HOLD_MS + COPIED_OUT_MS) as u64, false);
        near(gone.tip, 0.);
        let back = look_at((COPIED_HOLD_MS + 2. * COPIED_OUT_MS) as u64, false);
        assert_eq!(
            back,
            CopyLook {
                check: 0.,
                tip: 0.,
                tip_dy: 0.,
                animating: false
            }
        );
        assert!(COPIED_OUT_MS < COPIED_IN_MS && COPIED_IN_MS < 300.);
    }

    #[test]
    fn reduced_motion_swaps_copy_feedback_without_fades() {
        assert_eq!(
            look_at(0, true),
            CopyLook {
                check: 1.,
                tip: 1.,
                tip_dy: 0.,
                animating: false
            }
        );
        let late = look_at(COPIED_HOLD_MS as u64, true);
        assert_eq!((late.check, late.tip), (1., 0.));
        // Not copied: the copy icon at rest, whatever the time.
        assert_eq!(
            copy_look(false, Some(Instant::now()), Instant::now(), false).check,
            0.
        );
        // Copied with no time: a still check, no tip.
        let still = copy_look(true, None, Instant::now(), false);
        assert_eq!((still.check, still.tip, still.animating), (1., 0., false));
    }

    #[test]
    fn only_shell_fences_offer_insert_in_terminal() {
        for lang in ["bash", "sh", "zsh", "shell", "console", " Bash "] {
            assert!(is_shell(Some(lang)), "{lang}");
        }
        for lang in ["rust", "python", "text", "fish-ish", ""] {
            assert!(!is_shell(Some(lang)), "{lang}");
        }
        assert!(!is_shell(None));
        let mut props = CodeBlockProps::new("r", "ls -la\n", CodeSize::Full);
        assert_eq!(insert_text(&props), None);
        props.language = Some("bash".into());
        assert_eq!(insert_text(&props).as_deref(), Some("ls -la"));
        // A Run button already covers it.
        props.runnable = true;
        assert_eq!(insert_text(&props), None);
    }

    #[test]
    fn insert_types_the_commands_without_a_trailing_return() {
        let block = "export CEF_DEBUG_LOG=1\n/Applications/Keel.app/Contents/MacOS/keel --v=1\n\n";
        assert_eq!(
            terminal_text(block).as_deref(),
            Some("export CEF_DEBUG_LOG=1\n/Applications/Keel.app/Contents/MacOS/keel --v=1")
        );
        // A console session: just the commands, prompts and output dropped.
        let session = "$ cargo --version\ncargo 1.90.0\n  $ echo hi\nhi\n";
        assert_eq!(
            terminal_text(session).as_deref(),
            Some("cargo --version\necho hi")
        );
        assert_eq!(terminal_text("  \n\n"), None);
    }

    #[test]
    fn full_blocks_scroll_sideways_and_compact_output_wraps() {
        assert!(!CodeBlockProps::new("f", "x", CodeSize::Full).wraps());
        assert!(CodeBlockProps::new("c", "x", CodeSize::Compact).wraps());
    }
}

//! Trace lines in the chat: tool calls, thinking, plans, subagents and past
//! turns, each one line with an icon, a title and an optional description.
//!
//! Motion is timestamp-driven: every progress value here is computed from
//! `rc.now` against a stored instant (a toggle, a tool's start or end), and
//! the view's motion ticker repaints while any of those windows is open. No
//! `with_animation`, so nothing nests and nothing repeats on its own.

use crate::hover_fade::{HoverFadeExt, reveal_on_hover};
use gpui::{
    AnyElement, ElementId, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder,
};

use crate::code_block::{self, CodeAction, CodeBlockProps, CodeSize, CodeVariant, LineVariant};
use crate::icons::Icon;
use crate::markdown::ToolEntry;
use crate::motion::{ease_in_out, ease_out, glide_curve};
use crate::style::{KeelStyled, MONO, RADIUS_MD, c, zpx};
use crate::trace::{PlanItem, StepBody, StepKind, StepStatus, TraceNode, TraceStep};
use crate::transcript::{RowAction, RowContext, TranscriptRowItem};

/// Line height (spec D4).
pub const LINE_H: f32 = 26.;
/// The hover pill reaches this far into the gutter; content stays on the
/// prose edge.
pub const INSET: f32 = 8.;
/// Expand / collapse / line entry, ms.
pub const OPEN_MS: f32 = 220.;
/// Closing is quicker than opening.
pub const CLOSE_MS: f32 = 160.;
/// A live line arriving: fade and a 5 px rise on (0.2, 0.8, 0.2, 1), while
/// its height glides open so what's below slides rather than jumps.
pub const ENTER_MS: f32 = 220.;
const ENTER_RISE: f32 = 5.;
const ENTER_HEIGHT_MS: f32 = 200.;
/// Lines arriving together (within ~100 ms) step 30 ms apart, three at most.
const STAGGER_MS: f32 = 30.;
const STAGGER_CAP: usize = 3;
/// A finished line that streamed its output stays open this long, then folds.
pub const AUTO_CLOSE_DELAY_MS: f32 = 600.;
/// An error opens after this beat, so the status change reads first.
const ERROR_OPEN_DELAY_MS: f32 = 80.;
/// Running status crossfade.
const STATUS_FADE_MS: f32 = 160.;
/// Shimmer: a sweep of `SWEEP_MS`, then a rest, every `PERIOD_MS`.
const SHIMMER_SWEEP_MS: f32 = 1400.;
const SHIMMER_PERIOD_MS: f32 = 1800.;
/// At most this many titles shimmer in one group; the rest show the dot only.
const MAX_SHIMMERS: usize = 3;
/// Everything here that moves finishes within this long of its stamp; the
/// motion ticker keeps painting at 60 fps until then.
pub const TRANSITION_WINDOW_MS: i64 = (AUTO_CLOSE_DELAY_MS + CLOSE_MS + 120.) as i64;

/// `t` in 0..1 of a `ms`-long window that started `elapsed_ms` ago.
fn progress(elapsed_ms: f32, ms: f32) -> f32 {
    if ms <= 0. {
        1.
    } else {
        (elapsed_ms / ms).clamp(0., 1.)
    }
}

/// How open a fold is: 0 closed, 1 open, between while it moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fold {
    pub open: bool,
    /// Eased height factor this frame.
    pub height: f32,
    /// Eased opacity this frame.
    pub opacity: f32,
    /// Chevron turn, 0 right, 1 down.
    pub chevron: f32,
}

impl Fold {
    fn rest(open: bool) -> Self {
        let v = if open { 1. } else { 0. };
        Self {
            open,
            height: v,
            opacity: v,
            chevron: v,
        }
    }

    /// `open` reached `elapsed_ms` ago. Opening glides (0.32,0.72,0,1) over
    /// 220 ms with the fade on its first 60%; closing eases in-out over 220 ms
    /// with the fade on its first 160 ms.
    pub fn at(open: bool, elapsed_ms: Option<f32>, reduced: bool) -> Self {
        let Some(ms) = elapsed_ms.filter(|ms| *ms >= 0.) else {
            return Self::rest(open);
        };
        if reduced {
            // Opacity only, 120 ms.
            let p = progress(ms, 120.);
            let o = if open { p } else { 1. - p };
            return Self {
                open,
                height: if open || p < 1. { 1. } else { 0. },
                opacity: o,
                chevron: if open { 1. } else { 0. },
            };
        }
        if open {
            let h = glide_curve(progress(ms, OPEN_MS));
            Self {
                open,
                height: h,
                opacity: ease_out(progress(ms, OPEN_MS * 0.6)),
                chevron: h,
            }
        } else {
            let h = 1. - ease_out(progress(ms, CLOSE_MS));
            Self {
                open,
                height: h,
                opacity: (1. - ease_out(progress(ms, CLOSE_MS * 0.8))).powf(1.2),
                chevron: h,
            }
        }
    }

    /// Body painted this frame (fully closed paints nothing).
    pub fn visible(&self) -> bool {
        self.height > 0.001 || self.open
    }

    pub fn settled(&self) -> bool {
        (self.height - if self.open { 1. } else { 0. }).abs() < 0.001
    }
}

/// When a line opens on its own: errors.
/// Running terminal commands stay minimized on default to eliminate layout shifts.
/// The user's choice (`over`) wins and animates from its toggle instant.
pub fn line_fold(
    step: &TraceStep,
    over: Option<bool>,
    toggled_ms: Option<f32>,
    now_ms: i64,
    reduced: bool,
) -> Fold {
    if let Some(open) = over {
        return Fold::at(open, toggled_ms, reduced);
    }
    let since_end = step.ended_ms.map(|end| (now_ms - end) as f32);
    match step.status {
        StepStatus::Error if has_body(step) => {
            Fold::at(true, since_end.map(|ms| ms - ERROR_OPEN_DELAY_MS), reduced)
        }
        _ => Fold::rest(false),
    }
}

fn has_body(step: &TraceStep) -> bool {
    match &step.body {
        StepBody::None | StepBody::Shots(_) | StepBody::Terminal { .. } => {
            !step.children.is_empty()
        }
        StepBody::Output { tail, .. } => !tail.trim().is_empty(),
        StepBody::Text(text) => !text.trim().is_empty(),
        StepBody::Plan(entries) => !entries.is_empty(),
        StepBody::Diff { .. } | StepBody::Links(_) => true,
        StepBody::Browser(b) => !b.rows.is_empty() || !b.raw.trim().is_empty(),
    }
}

/// Shimmer band position this frame: `Some(left)` as a fraction of the
/// title's width (the band is 0.6 wide), `None` during the rest.
pub fn shimmer_left(clock_ms: f32, phase: f32) -> Option<f32> {
    let t = ((clock_ms / SHIMMER_PERIOD_MS) + phase).fract() * SHIMMER_PERIOD_MS;
    (t < SHIMMER_SWEEP_MS).then(|| -0.6 + 1.6 * (t / SHIMMER_SWEEP_MS))
}

/// Text shimmer: the highlight eases through the title in `GLYPH_SWEEP_MS`,
/// then rests, every `GLYPH_PERIOD_MS`.
const GLYPH_SWEEP_MS: f32 = 1500.;
const GLYPH_PERIOD_MS: f32 = 1900.;
/// Peak: the muted title moves this far toward the full text color.
const GLYPH_PEAK: f32 = 0.85;

/// Where the highlight's center is this frame, as a fraction of the
/// title's length, or `None` during the rest. It starts and ends one
/// half-width outside the text, so it fades in and out at the edges.
pub fn glyph_center(clock_ms: f32, phase: f32, half: f32) -> Option<f32> {
    let t = ((clock_ms / GLYPH_PERIOD_MS) + phase).fract() * GLYPH_PERIOD_MS;
    (t < GLYPH_SWEEP_MS).then(|| -half + (1. + 2. * half) * ease_in_out(t / GLYPH_SWEEP_MS))
}

/// The highlight's half-width for a title of `chars` characters: about six
/// characters, never thinner than an eighth of the text or wider than half.
pub fn glyph_half_width(chars: usize) -> f32 {
    (6. / chars.max(1) as f32).clamp(0.12, 0.45)
}

/// How much of the highlight a glyph at `x` (0..1 along the title) gets: a
/// raised-cosine falloff, 1 at the center and 0 one half-width away.
pub fn glyph_weight(x: f32, center: f32, half: f32) -> f32 {
    let d = (x - center) / half;
    if d.abs() >= 1. {
        0.
    } else {
        0.5 + 0.5 * (std::f32::consts::PI * d).cos()
    }
}

fn lerp_color(a: gpui::Hsla, b: gpui::Hsla, t: f32) -> gpui::Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    let m = |x: f32, y: f32| x + (y - x) * t;
    gpui::Rgba {
        r: m(a.r, b.r),
        g: m(a.g, b.g),
        b: m(a.b, b.b),
        a: m(a.a, b.a),
    }
    .into()
}

/// `text` as styled text whose glyphs brighten toward `peak` where the
/// highlight passes. `offset` and `total` place it within the whole title
/// (the verb and the subject share one sweep).
fn shimmer_text(
    text: String,
    offset: usize,
    total: usize,
    center: Option<f32>,
    half: f32,
    base: gpui::Hsla,
    peak: gpui::Hsla,
) -> gpui::StyledText {
    let Some(center) = center else {
        return gpui::StyledText::new(text);
    };
    let n = total.max(1) as f32;
    let mut runs: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> = Vec::new();
    for (k, (at, ch)) in text.char_indices().enumerate() {
        let w = glyph_weight((offset + k) as f32 / n + 0.5 / n, center, half);
        if w < 0.02 || ch.is_whitespace() {
            continue;
        }
        let color = lerp_color(base, peak, w * GLYPH_PEAK);
        runs.push((
            at..at + ch.len_utf8(),
            gpui::HighlightStyle {
                color: Some(color),
                ..Default::default()
            },
        ));
    }
    gpui::StyledText::new(text).with_highlights(runs)
}

/// Opacity of the running dot: breathes 0.35..1 on the shared clock.
pub fn dot_opacity(clock_ms: f32, phase: f32) -> f32 {
    let x = ((clock_ms / 1600.) + phase).fract() * std::f32::consts::TAU;
    0.35 + 0.65 * (0.5 - 0.5 * x.cos())
}

/// Opacity and rise of a line that arrived `age_ms` ago, `k`-th of a burst.
pub fn entry(age_ms: f32, k: usize, reduced: bool) -> (f32, f32) {
    if reduced {
        return (progress(age_ms, 120.), 0.);
    }
    let t = progress(age_ms - STAGGER_MS * k.min(STAGGER_CAP) as f32, ENTER_MS);
    if t >= 1. {
        // Land exactly: no sub-pixel offset left behind.
        return (1., 0.);
    }
    let p = crate::motion::ease_pill_in(t);
    (p, ENTER_RISE * (1. - p))
}

/// How open (0..1) an arriving line's height is: it glides over 200 ms on
/// the same stagger, so the rows below slide down instead of jumping.
pub fn entry_height(age_ms: f32, k: usize, reduced: bool) -> f32 {
    if reduced {
        return 1.;
    }
    ease_out(progress(
        age_ms - STAGGER_MS * k.min(STAGGER_CAP) as f32,
        ENTER_HEIGHT_MS,
    ))
}

fn icon_for(step: &TraceStep, name: &str) -> Icon {
    if step.status == StepStatus::Pending && step.kind == StepKind::Plan {
        return Icon::Circle;
    }
    if let Some(op) = step.browser {
        return crate::browser_trace::icon(op);
    }
    match step.kind {
        StepKind::Read => Icon::FileText,
        StepKind::Edit => Icon::FilePen,
        StepKind::Delete => Icon::Trash,
        StepKind::Move => Icon::MoveRight,
        StepKind::Search => Icon::Search,
        StepKind::Execute => Icon::Terminal,
        StepKind::Think => Icon::Brain,
        StepKind::Fetch | StepKind::Sources => Icon::Globe,
        StepKind::SwitchMode => Icon::Reload,
        StepKind::Browser => crate::transcript::browser_step(name).map_or(Icon::Globe, |s| s.icon),
        StepKind::Subagent => Icon::Agent,
        StepKind::Plan => Icon::ListChecks,
        StepKind::Other => Icon::Tool,
    }
}

/// The title split into the verb ("Ran", "Failed to run") and the subject,
/// which paints in mono when it is a path or a command.
fn split_title(step: &TraceStep) -> (String, String) {
    let title = step.title();
    let subject = step.subject.trim();
    if !subject.is_empty() && title.ends_with(subject) && title.len() > subject.len() {
        let head = title[..title.len() - subject.len()].trim_end().to_string();
        return (head, subject.to_string());
    }
    (title, String::new())
}

fn mono_subject(kind: StepKind) -> bool {
    matches!(
        kind,
        StepKind::Read
            | StepKind::Edit
            | StepKind::Delete
            | StepKind::Move
            | StepKind::Execute
            | StepKind::Search
    )
}

/// Paths keep their file name: the middle goes.
fn middle_ellipsis(path: &str, max: usize) -> String {
    let n = path.chars().count();
    if n <= max || !path.contains('/') {
        return path.to_string();
    }
    let keep_tail = path
        .rsplit('/')
        .next()
        .map_or(0, |f| f.chars().count() + 1)
        .min(max.saturating_sub(4));
    let head: String = path
        .chars()
        .take(max.saturating_sub(keep_tail + 1))
        .collect();
    let tail: String = path.chars().skip(n - keep_tail).collect();
    format!("{head}…{tail}")
}

/// Per-line inputs that do not live on the step.
pub struct LineCtx<'a> {
    pub line_id: String,
    pub name: &'a str,
    pub over: Option<bool>,
    /// The line shimmers (running, under the cap, motion on).
    pub shimmer: bool,
    /// Burst index for the entry stagger.
    pub burst: usize,
    /// The command the output belongs to (execute lines prefix `$ cmd`).
    pub command: Option<String>,
    /// A title not derived from the step ("Explored 4 files").
    pub title: Option<String>,
    /// The row's own `expanded` is the open state: a click sends the plain
    /// row id (thinking, plans) instead of a `{line}=0/1` choice.
    pub row_toggle: bool,
}

impl<'a> LineCtx<'a> {
    pub fn new(line_id: String, name: &'a str) -> Self {
        Self {
            line_id,
            name,
            over: None,
            shimmer: false,
            burst: 0,
            command: None,
            title: None,
            row_toggle: false,
        }
    }
}

/// One trace line, and its body when open.
pub fn render_line(step: &TraceStep, lc: LineCtx, depth: usize, rc: &RowContext) -> AnyElement {
    let t = rc.tokens;
    let now_ms = rc.now_ms;
    let toggled_ms = rc
        .toggles
        .get(&lc.line_id)
        .map(|(_, at)| rc.now.saturating_duration_since(*at).as_secs_f32() * 1000.);
    let expandable = has_body(step);
    let fold = if expandable {
        line_fold(step, lc.over, toggled_ms, now_ms, rc.reduced)
    } else {
        Fold::rest(false)
    };
    let clock = rc
        .now
        .saturating_duration_since(rc.clock_epoch)
        .as_secs_f32()
        * 1000.;
    let phase = (lc.burst as f32 * 0.17).fract();

    let (head, subject) = match &lc.title {
        Some(title) => (title.clone(), String::new()),
        None => split_title(step),
    };
    let (head_color, subject_color, icon_color) = match step.status {
        StepStatus::Error => (t.danger, t.text, t.danger),
        StepStatus::Running => (t.text_muted, t.text_muted, t.text_muted),
        StepStatus::Pending => (t.text_faint, t.text_faint, t.text_faint),
        StepStatus::Cancelled => (
            t.text_faint,
            t.text_faint,
            t.text_faint & 0xFFFF_FF00 | 0x99,
        ),
        StepStatus::Done => (t.text_muted, t.text_muted, t.text_faint),
    };
    // A just-finished line crossfades its icon tint from the running one.
    let since_end = step
        .ended_ms
        .map(|e| (now_ms - e) as f32)
        .unwrap_or(f32::MAX);
    let settle = if rc.reduced {
        1.
    } else {
        ease_out(progress(since_end, STATUS_FADE_MS))
    };
    let description = match (&step.description, step.status, step.started_ms) {
        (Some(d), _, _) => Some(d.clone()),
        (None, StepStatus::Running, Some(start)) if now_ms - start >= 3_000 => Some(
            crate::trace::format_duration(((now_ms - start) / 1_000 * 1_000) as u64),
        ),
        _ => None,
    };

    // Running to done: the old title (still shimmering) and icon fade out
    // over the new ones in STATUS_FADE_MS, instead of a swap.
    let was = (!rc.reduced
        && lc.title.is_none()
        && step.status != StepStatus::Running
        && since_end < STATUS_FADE_MS)
        .then(|| {
            let mut was = step.clone();
            was.status = StepStatus::Running;
            was
        });
    let was_title = was
        .as_ref()
        .map(split_title)
        .filter(|old| *old != (head.clone(), subject.clone()));
    let was_icon = was
        .as_ref()
        .map(|w| icon_for(w, lc.name))
        .filter(|old| *old != icon_for(step, lc.name));

    // Running: the icon itself breathes (no badge dot over it).
    let breathe = if step.status == StepStatus::Running && !rc.reduced {
        0.5 + 0.5 * (dot_opacity(clock, phase) - 0.35) / 0.65
    } else {
        1.
    };
    let icon = div()
        .relative()
        .flex_none()
        .size(zpx(16.))
        .flex()
        .items_center()
        .justify_center()
        .text_color(c(icon_color))
        .opacity((0.6 + 0.4 * settle) * breathe)
        .child(
            div()
                .when(was_icon.is_some(), |d| d.opacity(settle))
                .child(icon_for(step, lc.name).el(zpx(14.))),
        )
        .children(was_icon.map(|old| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .opacity(1. - settle)
                .child(old.el(zpx(14.)))
        }));

    let mono = mono_subject(step.kind);
    let shown_subject = |subject: &str| {
        if mono
            && matches!(
                step.kind,
                StepKind::Read | StepKind::Edit | StepKind::Delete | StepKind::Move
            )
        {
            middle_ellipsis(subject, 64)
        } else {
            subject.to_string()
        }
    };
    // The shimmer lights the glyphs only: per-character colors from a soft
    // moving highlight, no band painted over the line.
    let peak = c(t.text);
    let title_row =
        |head: String, subject_text: String, shimmer: bool, head_color: u32, subject_color: u32| {
            let total = head.chars().count()
                + usize::from(!subject_text.is_empty())
                + subject_text.chars().count();
            let half = glyph_half_width(total);
            let center = (shimmer && !rc.reduced)
                .then(|| glyph_center(clock, phase, half))
                .flatten();
            let head_len = head.chars().count();
            let head_el = shimmer_text(head, 0, total, center, half, c(head_color), peak);
            let subject_el = (!subject_text.is_empty()).then(|| {
                shimmer_text(
                    subject_text,
                    head_len + 1,
                    total,
                    center,
                    half,
                    c(subject_color),
                    peak,
                )
            });
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .gap(zpx(4.))
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(c(head_color))
                        .child(head_el),
                )
                .when_some(subject_el, |d, subject_el| {
                    d.child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(c(subject_color))
                            .when(mono, |d| d.font_family(MONO).text_size(zpx(11.5)))
                            .child(subject_el),
                    )
                })
        };
    let subject_text = shown_subject(&subject);
    let fading = was_title.is_some();
    let title = div()
        .relative()
        .min_w_0()
        .flex_shrink_1()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .t_small()
        .child(
            title_row(head, subject_text, lc.shimmer, head_color, subject_color)
                .when(fading, |d| d.opacity(settle)),
        )
        .children(was_title.map(|(old_head, old_subject)| {
            // The running look it had: muted, and its shimmer fading with it.
            div()
                .absolute()
                .top_0()
                .left_0()
                .opacity(1. - settle)
                .child(title_row(
                    old_head,
                    shown_subject(&old_subject),
                    true,
                    t.text_muted,
                    t.text_muted,
                ))
        }));

    let desc = description.map(|text| {
        let diffstat = matches!(step.body, StepBody::Diff { .. }) && text.starts_with('+');
        let el = div()
            .flex_none()
            .t_caption()
            .text_color(c(t.text_faint))
            .whitespace_nowrap();
        if diffstat {
            let (add, del) = text.split_once(' ').unwrap_or((&text, ""));
            el.font_family(MONO)
                .flex()
                .gap(zpx(4.))
                .child(
                    div()
                        .text_color(c(t.success).opacity(0.85))
                        .child(add.to_string()),
                )
                .child(
                    div()
                        .text_color(c(t.danger).opacity(0.85))
                        .child(del.to_string()),
                )
        } else {
            el.child(text)
        }
    });

    let chevron = expandable.then(|| {
        let turned =
            Icon::ChevronDown
                .el(zpx(12.))
                .with_transformation(gpui::Transformation::rotate(gpui::radians(
                    -std::f32::consts::FRAC_PI_2 * (1. - fold.chevron),
                )));
        div().flex_none().text_color(c(t.text_faint)).child(turned)
    });

    let line_id = lc.line_id.clone();
    let on_action = rc.on_action.clone();
    let open_now = fold.open;
    let row_toggle = lc.row_toggle;
    let line = div()
        .id(ElementId::Name(format!("trace-{}", lc.line_id).into()))
        .h(zpx(LINE_H))
        .px(zpx(INSET))
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(8.))
        .rounded(RADIUS_MD)
        .when(expandable, |d| d.hover_fade(c(t.hover), Some(c(t.selected))))
        .child(icon)
        .child(title)
        .children(desc)
        .children(chevron)
        .when(expandable, |d| {
            d.cursor_pointer()
                .on_click(move |_, _, cx| {
                    let row_id = if row_toggle {
                        line_id.clone()
                    } else {
                        format!("{line_id}={}", if open_now { 0 } else { 1 })
                    };
                    on_action(RowAction::Toggle { row_id }, cx);
                })
        });

    let body = (expandable && fold.visible()).then(|| {
        let content = render_body(step, &lc, depth, rc);
        let el = div()
            .ml(zpx(15.))
            .pl(zpx(12.))
            .py(zpx(4.))
            .border_l_1()
            .border_color(c(t.border))
            .flex()
            .flex_col()
            .gap(zpx(4.))
            .child(content);
        fold_clip(el, &lc.line_id, body_height(step), fold, rc)
    });

    let age = step
        .started_ms
        .map(|s| (now_ms - s) as f32)
        .unwrap_or(f32::MAX);
    let window = ENTER_MS + STAGGER_MS * STAGGER_CAP as f32 + 20.;
    let (opacity, rise) = if age < window {
        entry(age, lc.burst, rc.reduced)
    } else {
        (1., 0.)
    };
    let opening = if age < window && !fold.visible() {
        entry_height(age, lc.burst, rc.reduced)
    } else {
        1.
    };
    div()
        .w_full()
        .flex()
        .flex_col()
        .when(opacity < 1., |d| {
            d.opacity(opacity).relative().top(zpx(rise))
        })
        .when(opening < 1., |d| {
            d.max_h(zpx(LINE_H * opening)).overflow_hidden()
        })
        .child(line)
        .children(body)
        .into_any_element()
}

/// A fold's body: at rest as is; moving, clipped to `fold.height` of its
/// natural height, measured as it draws (so the glide tracks the real body
/// instead of an estimate that hangs or snaps). `est` covers the one frame
/// before the first measure.
fn fold_clip(el: gpui::Div, line_id: &str, est: f32, fold: Fold, rc: &RowContext) -> AnyElement {
    let key = format!("fold:{line_id}");
    let heights = rc.row_heights.clone();
    let already_measured = heights.borrow().contains_key(&key);
    let natural = heights
        .borrow()
        .get(&key)
        .copied()
        .map_or(zpx(est), gpui::px);
    if fold.settled() && fold.open {
        if already_measured {
            return div().relative().child(el).into_any_element();
        }
        let measure = gpui::canvas(
            move |bounds, _, _| {
                heights
                    .borrow_mut()
                    .insert(key, f32::from(bounds.size.height));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        return div().relative().child(el).child(measure).into_any_element();
    }
    let measure = gpui::canvas(
        move |bounds, _, _| {
            heights
                .borrow_mut()
                .insert(key, f32::from(bounds.size.height));
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    let nudge = if fold.open {
        4. * (1. - fold.height)
    } else {
        0.
    };
    div()
        .overflow_hidden()
        .h(natural * fold.height)
        .opacity(fold.opacity)
        .child(
            div()
                .relative()
                .flex_none()
                .top(zpx(nudge))
                .child(el)
                .child(measure),
        )
        .into_any_element()
}

/// The fold's height before its body has been measured.
fn body_height(step: &TraceStep) -> f32 {
    let pad = 8. + 2.;
    match &step.body {
        StepBody::Output { tail, .. } => {
            (tail.lines().count().max(1) as f32 * 16. + 16. + 2. + 16.)
                .min(code_block::COMPACT_CAP + 2.)
                + pad
        }
        StepBody::Diff { new, old, .. } => {
            let n = new.lines().count() + old.as_deref().map_or(0, |o| o.lines().count());
            (n.max(1) as f32 * 16. + 18.).min(code_block::COMPACT_CAP + 2.) + pad
        }
        StepBody::Text(text) => {
            text.lines()
                .map(|l| l.chars().count() / 90 + 1)
                .sum::<usize>() as f32
                * 16.
                + pad
        }
        StepBody::Plan(entries) => entries.len() as f32 * LINE_H + pad,
        StepBody::Links(links) => links.len().max(1) as f32 * LINE_H + pad,
        StepBody::Browser(b) => b.rows.len() as f32 * BROWSER_ROW_H + BROWSER_ROW_H + 4. + pad,
        _ => step.children.len().max(1) as f32 * LINE_H + pad,
    }
}

fn copy_action(rc: &RowContext, row_id: String) -> code_block::OnCodeAction {
    let on_action = rc.on_action.clone();
    std::rc::Rc::new(move |action, cx| match action {
        CodeAction::Copy { text } => {
            on_action(
                RowAction::Copy {
                    row_id: row_id.clone(),
                    content: text,
                },
                cx,
            );
        }
        CodeAction::ShowFull {
            content,
            title,
            error,
        }
        | CodeAction::OpenViewer {
            content,
            title,
            error,
        } => {
            on_action(
                RowAction::OpenFullOutput {
                    title,
                    content,
                    error,
                },
                cx,
            );
        }
        _ => {}
    })
}

fn compact(
    rc: &RowContext,
    id: &str,
    content: String,
    variants: Option<Vec<LineVariant>>,
    error: bool,
    title: Option<String>,
) -> AnyElement {
    let mut props = CodeBlockProps::new(id.to_string(), content, CodeSize::Compact);
    props.title = title;
    props.line_variants = variants;
    props.variant = if error {
        CodeVariant::Destructive
    } else {
        CodeVariant::Default
    };
    props.copied = rc.copied.as_deref() == Some(id);
    props.copied_at = rc.copied_at.filter(|_| props.copied);
    props.reduced = rc.reduced;
    code_block::render(
        &props,
        rc.tokens,
        &rc.syntax,
        rc.viewport_h,
        copy_action(rc, id.to_string()),
    )
}

fn render_body(step: &TraceStep, lc: &LineCtx, depth: usize, rc: &RowContext) -> AnyElement {
    let t = rc.tokens;
    let id = format!("{}-body", lc.line_id);
    match &step.body {
        StepBody::Output { tail, error } => {
            let tail = tail.trim_end_matches('\n');
            let (title, content) = match &lc.command {
                Some(cmd) if !cmd.is_empty() => {
                    (Some(format!("$ {cmd}")), format!("$ {cmd}\n{tail}"))
                }
                _ => (
                    Some(lc.name.to_string()).filter(|s| !s.is_empty()),
                    tail.to_string(),
                ),
            };
            compact(rc, &id, content, None, *error, title)
        }
        StepBody::Diff { old, new, .. } => {
            let lines = match old {
                Some(old) => code_block::diff_lines(old, new),
                None if new.lines().any(|l| l.starts_with("@@")) => code_block::parse_unified(new)
                    .into_iter()
                    .map(|l| (l.variant, l.text))
                    .collect(),
                None => new
                    .lines()
                    .map(|l| (LineVariant::Added, l.to_string()))
                    .collect(),
            };
            let (content, variants) = code_block::join_diff(&lines);
            compact(rc, &id, content, Some(variants), false, None)
        }
        StepBody::Text(text) => div()
            .t_small()
            .text_color(c(t.text_muted))
            .whitespace_normal()
            .child(text.clone())
            .into_any_element(),
        StepBody::Plan(entries) => plan_entries(entries, &lc.line_id, rc),
        StepBody::Browser(b) => browser_body(b, &lc.line_id, rc),
        // Sources a web search or read used: the site, then the address.
        StepBody::Links(links) => div()
            .flex()
            .flex_col()
            .children(links.iter().map(|link| {
                div()
                    .h(zpx(LINE_H))
                    .flex()
                    .items_center()
                    .gap(zpx(8.))
                    .t_small()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_none()
                            .text_color(c(t.text_muted))
                            .child(link.title.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(c(t.text_muted).opacity(0.6))
                            .child(link.url.clone()),
                    )
            }))
            .into_any_element(),
        _ => div()
            .flex()
            .flex_col()
            .children(step.children.iter().enumerate().map(|(k, child)| {
                let line_id = format!("{}/{}", lc.line_id, child.id);
                let over = rc.open_lines.get(&line_id).copied();
                let mut lc = LineCtx::new(line_id, "");
                lc.over = over;
                lc.shimmer = child.status == StepStatus::Running && k < MAX_SHIMMERS;
                lc.burst = k;
                render_line(child, lc, depth + 1, rc)
            }))
            .into_any_element(),
    }
}

/// One label/value row of a browser body.
const BROWSER_ROW_H: f32 = 18.;

/// A browser action's body: URL, title and result as short rows, and a
/// "Raw" toggle that shows the tool's own output in a compact code block.
fn browser_body(
    b: &crate::browser_trace::BrowserBody,
    line_id: &str,
    rc: &RowContext,
) -> AnyElement {
    let t = rc.tokens;
    let raw_id = format!("{line_id}~raw");
    let raw_open = rc.open_lines.get(&raw_id).copied().unwrap_or(false);
    let toggled_ms = rc
        .toggles
        .get(&raw_id)
        .map(|(_, at)| rc.now.saturating_duration_since(*at).as_secs_f32() * 1000.);
    let fold = Fold::at(raw_open, toggled_ms, rc.reduced);
    let rows = b.rows.iter().map(|(label, value)| {
        let error = *label == "Error";
        div()
            .min_h(zpx(BROWSER_ROW_H))
            .flex()
            .flex_row()
            .items_baseline()
            .gap(zpx(8.))
            .child(
                div()
                    .flex_none()
                    .w(zpx(40.))
                    .t_caption()
                    .text_color(c(t.text_faint))
                    .child(*label),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .t_small()
                    .text_color(c(if error { t.danger } else { t.text_muted }))
                    .when(matches!(*label, "URL" | "Saved"), |d| {
                        d.font_family(MONO).text_size(zpx(11.5))
                    })
                    .child(value.clone()),
            )
    });
    let on_action = rc.on_action.clone();
    let toggle_id = raw_id.clone();
    let toggle = (!b.raw.trim().is_empty()).then(|| {
        div()
            .id(ElementId::Name(format!("trace-{raw_id}").into()))
            .mt(zpx(2.))
            .px(zpx(4.))
            .h(zpx(BROWSER_ROW_H))
            .flex()
            .items_center()
            .gap(zpx(4.))
            .rounded(RADIUS_MD)
            .cursor_pointer()
            .t_caption()
            .text_color(c(t.text_faint))
            .hover(|s| s.text_color(c(t.text_muted)))
            .hover_fade(c(t.hover), None)
            .child(Icon::ChevronDown.el(zpx(10.)).with_transformation(
                gpui::Transformation::rotate(gpui::radians(
                    -std::f32::consts::FRAC_PI_2 * (1. - fold.chevron),
                )),
            ))
            .child("Raw")
            .on_click(move |_, _, cx| {
                on_action(
                    RowAction::Toggle {
                        row_id: format!("{toggle_id}={}", if raw_open { 0 } else { 1 }),
                    },
                    cx,
                );
            })
    });
    let raw = (fold.visible() && !b.raw.trim().is_empty()).then(|| {
        let est = (b.raw.lines().count().max(1) as f32 * 16. + 18.)
            .min(code_block::COMPACT_CAP + 2.)
            + 2.;
        let el = div().w_full().pt(zpx(2.)).child(compact(
            rc,
            &format!("{line_id}-raw"),
            b.raw.clone(),
            None,
            b.error,
            None,
        ));
        div().w_full().child(fold_clip(el, &raw_id, est, fold, rc))
    });
    div()
        .flex()
        .flex_col()
        .items_start()
        .children(rows.map(|r| r.w_full()))
        .children(toggle)
        .children(raw)
        .into_any_element()
}

fn plan_step(entry: &PlanItem, ix: usize) -> TraceStep {
    let status = match entry.status.as_str() {
        "completed" | "done" | "complete" => StepStatus::Done,
        "in_progress" | "inProgress" | "running" => StepStatus::Running,
        "failed" | "error" => StepStatus::Error,
        "cancelled" | "canceled" => StepStatus::Cancelled,
        _ => StepStatus::Pending,
    };
    let mut step = TraceStep::plan(format!("entry{ix}"), Vec::new(), status);
    step.kind = StepKind::Other;
    step.subject = entry.content.clone();
    step.body = StepBody::None;
    step
}

fn plan_entries(entries: &[PlanItem], line_id: &str, rc: &RowContext) -> AnyElement {
    let t = rc.tokens;
    div()
        .flex()
        .flex_col()
        .children(entries.iter().enumerate().map(|(ix, entry)| {
            let step = plan_step(entry, ix);
            let (icon, color) = match step.status {
                StepStatus::Done => (Icon::Check, t.success),
                StepStatus::Running => (Icon::Circle, t.text),
                StepStatus::Error => (Icon::Alert, t.danger),
                _ => (Icon::Circle, t.text_faint),
            };
            let text_color = match step.status {
                StepStatus::Done => t.text_faint,
                StepStatus::Running => t.text,
                _ => t.text_muted,
            };
            let clock = rc
                .now
                .saturating_duration_since(rc.clock_epoch)
                .as_secs_f32()
                * 1000.;
            div()
                .id(ElementId::Name(format!("{line_id}-plan{ix}").into()))
                .h(zpx(LINE_H))
                .px(zpx(INSET))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(8.))
                .child(
                    div()
                        .flex_none()
                        .size(zpx(16.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(c(color))
                        .when(step.status == StepStatus::Running && !rc.reduced, |d| {
                            d.opacity(dot_opacity(clock, 0.))
                        })
                        .child(icon.el(zpx(12.))),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .t_small()
                        .text_color(c(text_color))
                        .when(step.status == StepStatus::Done, |d| d.line_through())
                        .child(entry.content.clone()),
                )
        }))
        .into_any_element()
}

/// Browser calls in a row with the same step read as one line, "×2".
fn fold_repeats(steps: Vec<(TraceStep, String)>) -> Vec<(TraceStep, String, usize)> {
    let mut out: Vec<(TraceStep, String, usize)> = Vec::new();
    for (step, name) in steps {
        if let Some((last, last_name, n)) = out.last_mut()
            && step.kind == StepKind::Browser
            && last.kind == StepKind::Browser
            && last.verb == step.verb
            && last.subject == step.subject
            && !matches!(step.status, StepStatus::Error)
            && !matches!(last.status, StepStatus::Error)
        {
            *n += 1;
            *last_name = name;
            last.status = step.status;
            last.ended_ms = step.ended_ms;
            continue;
        }
        out.push((step, name, 1));
    }
    out
}

/// A run of tool calls: one line each, reads and searches folded into an
/// "Explored" line once there are three in a row.
pub fn render_tool_group(
    item: &TranscriptRowItem,
    tools: &[ToolEntry],
    overrides: &[(String, bool)],
    rc: &RowContext,
) -> gpui::Div {
    let group_id = item.row_id().to_string();
    let over = |line: &str| {
        overrides
            .iter()
            .find(|(id, _)| id == line)
            .map(|(_, open)| *open)
    };
    let names: std::collections::HashMap<&str, &ToolEntry> =
        tools.iter().map(|t| (t.tool_id.as_str(), t)).collect();
    let steps: Vec<(TraceStep, String)> = tools
        .iter()
        .filter(|t| !t.is_receipt())
        .map(|t| (TraceStep::from(t), t.name.clone()))
        .collect();
    let folded = fold_repeats(steps);
    let mut repeat: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut plain: Vec<TraceStep> = Vec::new();
    for (mut step, _, n) in folded {
        if n > 1 {
            step.description = Some(format!("×{n}"));
            repeat.insert(step.id.clone(), n);
        }
        plain.push(step);
    }
    let nodes = crate::trace::group_steps(plain);
    let mut shimmers = 0usize;
    // Lines that started together cascade in.
    let mut last_start: Option<i64> = None;
    let mut burst = 0usize;
    let mut next_burst = |start: Option<i64>| {
        match (start, last_start) {
            (Some(s), Some(l)) if (s - l).abs() < 100 => burst += 1,
            _ => burst = 0,
        }
        last_start = start;
        burst
    };
    let mut lines: Vec<AnyElement> = Vec::new();
    for node in nodes {
        match node {
            TraceNode::Step(step) => {
                let tool = names.get(step.id.as_str());
                let line_id = crate::turn::line_id(&group_id, &step.id);
                let running = step.status == StepStatus::Running;
                let shimmer = running && shimmers < MAX_SHIMMERS;
                shimmers += usize::from(shimmer);
                let b = next_burst(step.started_ms);
                let command = (step.kind == StepKind::Execute)
                    .then(|| tool.map(|t| t.detail.clone()).unwrap_or_default());
                let mut lc = LineCtx::new(line_id.clone(), tool.map_or("", |t| t.name.as_str()));
                lc.over = over(&line_id);
                lc.shimmer = shimmer;
                lc.burst = b;
                lc.command = command;
                lines.push(render_line(&step, lc, 0, rc));
            }
            TraceNode::Browsed(group) => {
                let first = group
                    .steps
                    .first()
                    .map(|s| s.id.clone())
                    .unwrap_or_default();
                let line_id = crate::turn::line_id(&group_id, &format!("browsed-{first}"));
                let mut parent =
                    TraceStep::plan(format!("browsed-{first}"), Vec::new(), group.status);
                parent.kind = StepKind::Browser;
                parent.browser = Some(crate::browser_trace::BrowserOp::Navigate);
                parent.subject = String::new();
                parent.body = StepBody::None;
                parent.started_ms = group.steps.iter().filter_map(|s| s.started_ms).min();
                parent.ended_ms = group.steps.iter().filter_map(|s| s.ended_ms).max();
                parent.children = group.steps.clone();
                let running = group.status == StepStatus::Running;
                let shimmer = running && shimmers < MAX_SHIMMERS;
                shimmers += usize::from(shimmer);
                let b = next_burst(parent.started_ms);
                let mut lc = LineCtx::new(line_id.clone(), "");
                lc.over = over(&line_id);
                lc.shimmer = shimmer;
                lc.burst = b;
                lc.title = Some(group.title.clone());
                lines.push(render_line(&parent, lc, 0, rc));
            }
            TraceNode::Explored(group) => {
                let first = group
                    .steps
                    .first()
                    .map(|s| s.id.clone())
                    .unwrap_or_default();
                let line_id = crate::turn::line_id(&group_id, &format!("explored-{first}"));
                let mut parent =
                    TraceStep::plan(format!("explored-{first}"), Vec::new(), group.status);
                parent.kind = StepKind::Search;
                parent.subject = String::new();
                parent.body = StepBody::None;
                parent.started_ms = group.steps.iter().filter_map(|s| s.started_ms).min();
                parent.ended_ms = group.steps.iter().filter_map(|s| s.ended_ms).max();
                parent.children = group.steps.clone();
                let running = group.status == StepStatus::Running;
                let shimmer = running && shimmers < MAX_SHIMMERS;
                shimmers += usize::from(shimmer);
                let b = next_burst(parent.started_ms);
                let mut lc = LineCtx::new(line_id.clone(), "");
                lc.over = over(&line_id);
                lc.shimmer = shimmer;
                lc.burst = b;
                lc.title = Some(group.title.clone());
                lines.push(render_line(&parent, lc, 0, rc));
            }
        }
    }
    div()
        .w_full()
        .ml(zpx(-INSET))
        .py(zpx(2.))
        .flex()
        .flex_col()
        .children(lines)
}

/// Thinking: "Thought" with its text behind a rail.
pub fn render_reasoning(
    item: &TranscriptRowItem,
    content: &str,
    expanded: bool,
    rc: &RowContext,
) -> gpui::Div {
    let mut step = TraceStep::plan(item.row_id().to_string(), Vec::new(), StepStatus::Done);
    step.kind = StepKind::Think;
    step.verb = crate::trace::verb_for(StepKind::Think, "think");
    step.subject = String::new();
    step.body = StepBody::Text(content.to_string());
    let mut lc = LineCtx::new(item.row_id().to_string(), "think");
    lc.over = Some(expanded);
    lc.row_toggle = true;
    div()
        .w_full()
        .ml(zpx(-INSET))
        .child(render_line(&step, lc, 0, rc))
}

/// The agent's plan. Open while any entry is in progress.
pub fn render_plan(
    item: &TranscriptRowItem,
    entries: &[PlanItem],
    live: bool,
    expanded: Option<bool>,
    rc: &RowContext,
) -> gpui::Div {
    let status = if live {
        StepStatus::Running
    } else {
        StepStatus::Done
    };
    let step = TraceStep::plan(item.row_id().to_string(), entries.to_vec(), status);
    let mut lc = LineCtx::new(item.row_id().to_string(), "plan");
    lc.over = expanded.or(Some(live));
    lc.shimmer = live;
    div()
        .w_full()
        .ml(zpx(-INSET))
        .py(zpx(2.))
        .child(render_line(&step, lc, 0, rc))
}

/// A turn's steps, folded into an editorial byline.
pub fn render_turn_summary(
    item: &TranscriptRowItem,
    label: &str,
    failed: bool,
    expanded: bool,
    rc: &RowContext,
) -> gpui::Div {
    let t = rc.tokens;
    let toggled_ms = rc
        .toggles
        .get(item.row_id())
        .map(|(_, at)| rc.now.saturating_duration_since(*at).as_secs_f32() * 1000.);
    let fold = Fold::at(expanded, toggled_ms, rc.reduced);
    let row_id = item.row_id().to_string();
    let on_action = rc.on_action.clone();
    let icon = if failed { Icon::Alert } else { Icon::Check };
    let group: gpui::SharedString = format!("worked-{row_id}").into();
    div()
        .w_full()
        .h(zpx(LINE_H))
        .flex()
        .flex_row()
        .items_center()
        .child(
            div()
                .id(ElementId::Name(group.clone()))
                .group(group.clone())
                .h(zpx(22.))
                .px(zpx(4.))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(6.))
                .rounded(crate::style::RADIUS_SM)
                .cursor_pointer()
                .hover_fade(c(t.hover), None)
                .active(|s| s.opacity(0.75))
                .on_click(move |_, _, cx| {
                    on_action(
                        RowAction::Toggle {
                            row_id: row_id.clone(),
                        },
                        cx,
                    )
                })
                .child(
                    div()
                        .flex_none()
                        .size(zpx(12.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(c(if failed { t.danger } else { t.success }))
                        .child(icon.el(zpx(10.5))),
                )
                .child(
                    div()
                        .t_small()
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(c(if expanded { t.text } else { t.text_muted }))
                        .group_hover(group.clone(), |s| s.text_color(c(t.text)))
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(c(t.text_faint))
                        .group_hover(group, |s| s.text_color(c(t.text_muted)))
                        .child(Icon::ChevronDown.el(zpx(9.5)).with_transformation(
                            gpui::Transformation::rotate(gpui::radians(
                                -std::f32::consts::FRAC_PI_2 * (1. - fold.chevron),
                            )),
                        )),
                ),
        )
}

/// A spawned subagent: "Running the research agent", ↗ opens its session.
pub fn render_subagent(
    item: &TranscriptRowItem,
    subagent_id: &str,
    name: &str,
    state: &str,
    summary: &str,
    rc: &RowContext,
) -> gpui::Div {
    let t = rc.tokens;
    let status = match state {
        "running" | "" => StepStatus::Running,
        "error" | "failed" | "errored" => StepStatus::Error,
        "cancelled" | "interrupted" => StepStatus::Cancelled,
        _ => StepStatus::Done,
    };
    let mut step = TraceStep::plan(item.row_id().to_string(), Vec::new(), status);
    step.kind = StepKind::Subagent;
    step.verb = crate::trace::verb_for(StepKind::Subagent, "agent");
    let label = name.trim();
    step.subject = if label.is_empty() || label == "subagent" {
        "subagent".into()
    } else if label.ends_with("agent") {
        label.to_string()
    } else {
        format!("{label} agent")
    };
    step.body = if status == StepStatus::Error && !summary.is_empty() {
        StepBody::Output {
            tail: summary.to_string(),
            error: true,
        }
    } else {
        StepBody::None
    };
    let id = subagent_id.to_string();
    let on_action = rc.on_action.clone();
    let mut lc = LineCtx::new(item.row_id().to_string(), "agent");
    lc.shimmer = status == StepStatus::Running;
    let line = render_line(&step, lc, 0, rc);
    let group: gpui::SharedString = format!("sub-{id}").into();
    div().w_full().ml(zpx(-INSET)).child(
        div()
            .id(ElementId::Name(format!("subagent-{id}").into()))
            .hover_probe(group.clone())
            .group(group.clone())
            .relative()
            .w_full()
            .rounded(RADIUS_MD)
            .cursor_pointer()
            .hover_fade(c(t.hover), None)
            .on_click(move |_, _, cx| {
                on_action(
                    RowAction::OpenSubagent {
                        subagent_id: id.clone(),
                    },
                    cx,
                )
            })
            .child(line)
            .child(reveal_on_hover("external-reveal", group, 0., 1.,
                div()
                    .absolute()
                    .top_0()
                    .right(zpx(INSET))
                    .h(zpx(LINE_H))
                    .flex()
                    .items_center()
                    .text_color(c(t.text_faint))
                    .child(Icon::External.el(zpx(12.))))),
    )
}

/// Does anything in this group move right now (a running line, a fold or an
/// entry still inside its window)? The view's ticker asks this.
/// The row has a running step or a transition still settling.
pub fn row_animating(item: &TranscriptRowItem) -> bool {
    use crate::markdown::TranscriptBlock;
    match &item.block {
        TranscriptBlock::ToolGroup { tools, .. } => tools_animating(tools, now_ms()),
        TranscriptBlock::Plan { live, .. } => *live,
        TranscriptBlock::Subagent { state, .. } => state == "running",
        _ => false,
    }
}

/// Wall-clock ms, the timebase tool timings use.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

pub fn tools_animating(tools: &[ToolEntry], now_ms: i64) -> bool {
    tools.iter().any(|t| {
        t.is_live()
            || t.ended_ms
                .is_some_and(|e| now_ms - e < TRANSITION_WINDOW_MS)
            || t.started_ms.is_some_and(|s| {
                now_ms - s < (ENTER_MS + STAGGER_MS * STAGGER_CAP as f32) as i64 + 40
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_glides_open_and_eases_closed() {
        let start = Fold::at(true, Some(0.), false);
        assert!(start.height < 1e-3);
        let mid = Fold::at(true, Some(160.), false);
        assert!(
            mid.height > 0.5 && mid.height < 1.,
            "glide leads: {}",
            mid.height
        );
        assert!(Fold::at(true, Some(OPEN_MS), false).settled());
        let closing = Fold::at(false, Some(CLOSE_MS / 2.), false);
        assert!(
            closing.height > 0.05 && closing.height < 0.3,
            "ease-out is mostly closed at half time: {}",
            closing.height
        );
        assert!(Fold::at(false, Some(CLOSE_MS), false).settled());
        assert!(!Fold::at(false, Some(CLOSE_MS), false).visible());
        // No stamp: rest.
        assert!(Fold::at(true, None, false).settled());
    }

    #[test]
    fn reduced_motion_is_a_short_fade() {
        let f = Fold::at(true, Some(60.), true);
        assert_eq!(f.height, 1.);
        assert!(f.opacity > 0.4 && f.opacity < 0.6);
    }

    #[test]
    fn shimmer_sweeps_then_rests() {
        assert_eq!(shimmer_left(0., 0.), Some(-0.6));
        let end = shimmer_left(1399., 0.).unwrap();
        assert!(end > 0.99);
        assert_eq!(shimmer_left(1500., 0.), None);
        assert_eq!(shimmer_left(1800., 0.), Some(-0.6));
    }

    #[test]
    fn glyph_shimmer_eases_through_and_rests() {
        let half = glyph_half_width(20);
        assert!((half - 0.3).abs() < 1e-6);
        assert_eq!(glyph_half_width(200), 0.12);
        assert_eq!(glyph_half_width(3), 0.45);
        // Starts and ends one half-width outside the text: no hard entry.
        assert!((glyph_center(0., 0., half).unwrap() + half).abs() < 1e-6);
        assert!((glyph_center(1499.9, 0., half).unwrap() - (1. + half)).abs() < 1e-3);
        // Eased: slow at the ends, quick in the middle.
        let early = glyph_center(150., 0., half).unwrap() - glyph_center(0., 0., half).unwrap();
        let mid = glyph_center(825., 0., half).unwrap() - glyph_center(675., 0., half).unwrap();
        assert!(mid > early * 2., "{early} {mid}");
        // A short rest, then again (period 1.9 s).
        assert_eq!(glyph_center(1600., 0., half), None);
        assert!(glyph_center(1900., 0., half).is_some());
        // Soft falloff: full at the center, smooth to nothing at the edge.
        assert_eq!(glyph_weight(0.5, 0.5, 0.2), 1.);
        assert!((glyph_weight(0.6, 0.5, 0.2) - 0.5).abs() < 1e-6);
        assert_eq!(glyph_weight(0.7, 0.5, 0.2), 0.);
        assert!(glyph_weight(0.69, 0.5, 0.2) < 0.01);
        // Peak color stays below the full text color.
        let base = gpui::hsla(0., 0., 0.5, 1.);
        let peak = gpui::hsla(0., 0., 1., 1.);
        let top = lerp_color(base, peak, GLYPH_PEAK);
        assert!(top.l > 0.9 && top.l < 0.95, "{top:?}");
    }

    #[test]
    fn dot_breathes_in_range() {
        for ms in (0..3200).step_by(100) {
            let o = dot_opacity(ms as f32, 0.);
            assert!((0.35..=1.0).contains(&o));
        }
    }

    #[test]
    fn entry_staggers_and_lands() {
        assert!(entry(0., 0, false).0 < 1e-3);
        assert_eq!(entry(ENTER_MS, 0, false), (1., 0.));
        assert!(entry(ENTER_MS, 2, false).0 < 1.);
        assert_eq!(entry(ENTER_MS + 120., 9, false), (1., 0.));
        // Rises 5 px and eases out: most of the way early.
        assert!((entry(0., 0, false).1 - 5.).abs() < 1e-3);
        assert!(entry(ENTER_MS / 2., 0, false).0 > 0.7);
        // The stagger caps at three lines.
        assert_eq!(entry(ENTER_MS + 90., 7, false), (1., 0.));
        // Height opens over 200 ms; reduced motion doesn't glide it.
        assert!(entry_height(0., 0, false) < 1e-3);
        assert_eq!(entry_height(200., 0, false), 1.);
        assert_eq!(entry_height(0., 0, true), 1.);
    }

    fn step(status: StepStatus, ended: Option<i64>) -> TraceStep {
        let mut s = TraceStep::plan("x", Vec::new(), status);
        s.kind = StepKind::Execute;
        s.body = StepBody::Output {
            tail: "out".into(),
            error: status == StepStatus::Error,
        };
        s.ended_ms = ended;
        s
    }

    #[test]
    fn streamed_output_defaults_to_closed_and_opens_on_user_or_error() {
        // Minimized on default when running and when done
        assert!(!line_fold(&step(StepStatus::Running, None), None, None, 0, false).open);
        assert!(
            !line_fold(
                &step(StepStatus::Done, Some(1_000)),
                None,
                None,
                1_300,
                false
            )
            .open
        );
        assert!(
            !line_fold(
                &step(StepStatus::Done, Some(1_000)),
                None,
                None,
                3_000,
                false
            )
            .visible()
        );
        // The user's choice wins (can be opened manually while running or done)
        assert!(line_fold(&step(StepStatus::Running, None), Some(true), None, 0, false).open);
        assert!(
            line_fold(
                &step(StepStatus::Done, Some(1_000)),
                Some(true),
                None,
                3_000,
                false
            )
            .open
        );
        // Error auto-opens
        assert!(
            line_fold(
                &step(StepStatus::Error, Some(1_000)),
                None,
                None,
                3_000,
                false
            )
            .open
        );
    }

    #[test]
    fn titles_split_verb_and_subject() {
        let mut s = TraceStep::plan("x", Vec::new(), StepStatus::Done);
        s.kind = StepKind::Execute;
        s.verb = crate::trace::verb_for(StepKind::Execute, "bash");
        s.subject = "pnpm test".into();
        assert_eq!(split_title(&s), ("Ran".into(), "pnpm test".into()));
        s.status = StepStatus::Error;
        assert_eq!(
            split_title(&s),
            ("Failed to run".into(), "pnpm test".into())
        );
    }

    #[test]
    fn long_paths_keep_the_file_name() {
        let p = "crates/ui/src/view/very/deep/nested/folder/structure/that/goes/on/transcript.rs";
        let short = middle_ellipsis(p, 40);
        assert!(
            short.ends_with("/transcript.rs") && short.contains('…') && short.chars().count() <= 40,
            "{short}"
        );
    }
}

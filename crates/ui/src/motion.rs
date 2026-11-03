//! Motion: one place for durations and curves. Nothing runs longer than
//! 200 ms except loops (spinner, pulse), springs and the two big spatial moves
//! (sidebar width, new-chat composer drop), and reduced motion turns every
//! transition into an instant cut.

use crate::style::zpx;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, ElementId, IntoElement, ParentElement, Styled, Transformation,
    percentage,
};

/// Menus, chips, small state changes.
pub const FAST: Duration = Duration::from_millis(120);
/// Rows appearing.
pub const BASE: Duration = Duration::from_millis(160);
/// Sidebar re-sort glide. The ceiling for one-shot motion.
pub const SLOW: Duration = Duration::from_millis(200);
/// Sidebar open/close and the composer dropping from the new-chat hero:
/// long travel needs a little longer to read as motion, not a jump.
pub const SPATIAL: Duration = Duration::from_millis(420);
/// The docked sidebar opening and closing, both on [`glide_curve`]: the same
/// feel each way, the close a touch quicker (<250ms for responsive navigation).
pub const SHEET_OPEN: Duration = Duration::from_millis(240);
pub const SHEET_CLOSE: Duration = Duration::from_millis(180);
/// Right side panels (browser, files, terminal) opening and closing on [`glide_curve`]:
/// under 300 ms for responsive UI, with closing faster than opening.
pub const PANEL_OPEN: Duration = Duration::from_millis(260);
pub const PANEL_CLOSE: Duration = Duration::from_millis(200);
/// A spring settling: long enough for its overshoot to read.
pub const SPRING: Duration = Duration::from_millis(380);
/// Loops.
pub const SPIN: Duration = Duration::from_millis(900);
pub const PULSE: Duration = Duration::from_millis(1600);
/// The session timer's running dot: slower and shallower than [`PULSE`],
/// since it may breathe for hours.
pub const BREATHE: Duration = Duration::from_millis(2000);
/// One skeleton sweep.
pub const SHIMMER: Duration = Duration::from_millis(1400);

/// Fast start, soft landing: the default for anything entering.
pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// `cubic-bezier(0.23, 1, 0.32, 1)`: the entrance curve. Stronger than
/// [`ease_out`], so the first frames move at once.
pub fn ease_out_strong(t: f32) -> f32 {
    if t <= 0. {
        return 0.;
    }
    if t >= 1. {
        return 1.;
    }
    let (x1, y1, x2, y2) = (0.23, 1.0, 0.32, 1.0);
    let bez = |a: f32, b: f32, s: f32| {
        3. * a * s * (1. - s).powi(2) + 3. * b * s * s * (1. - s) + s.powi(3)
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..12 {
        let mid = (lo + hi) / 2.;
        if bez(x1, x2, mid) < t {
            lo = mid
        } else {
            hi = mid
        }
    }
    bez(y1, y2, (lo + hi) / 2.)
}

/// `cubic-bezier(0.32, 0.72, 0, 1)`, the iOS sheet curve: moves at once
/// (no hesitation) without a first-frame lurch, then settles long and soft.
/// For a long move from one resting spot to another, like the composer's drop.
pub fn glide_curve(t: f32) -> f32 {
    if t <= 0. {
        return 0.;
    }
    if t >= 1. {
        return 1.;
    }
    let (x1, y1, x2, y2) = (0.32, 0.72, 0.0, 1.0);
    let bez = |a: f32, b: f32, s: f32| {
        3. * a * s * (1. - s).powi(2) + 3. * b * s * s * (1. - s) + s.powi(3)
    };
    // x(s) is increasing on 0..1: bisect for the s where x(s) = t.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.;
        if bez(x1, x2, mid) < t {
            lo = mid
        } else {
            hi = mid
        }
    }
    bez(y1, y2, (lo + hi) / 2.)
}

/// A damped spring: shoots toward the target, overshoots it by about 6%
/// (under a third of the way through), and settles. Exactly 0 and 1 at the ends.
/// Not an easing: gpui panics on eased values outside 0..1, so run the
/// animation linear and apply this inside its animator.
pub fn spring(t: f32) -> f32 {
    if t >= 1.0 {
        return 1.0;
    }
    1.0 - (-8.5 * t).exp() * (8.0 * t).cos()
}

/// `cubic-bezier(0.65, 0, 0.35, 1)`: symmetric in-out, for folds closing.
pub fn ease_in_out(t: f32) -> f32 {
    if t <= 0. {
        return 0.;
    }
    if t >= 1. {
        return 1.;
    }
    crate::approval::bezier(0.65, 0., 0.35, 1., t)
}

/// Slow start, fast finish: the curve for anything leaving.
pub fn ease_in(t: f32) -> f32 {
    t * t * t
}

/// A one-shot that becomes an instant cut under reduced motion.
fn once(duration: Duration, reduced: bool) -> Animation {
    let duration = if reduced {
        Duration::from_millis(1)
    } else {
        duration
    };
    Animation::new(duration).with_easing(ease_out)
}

/// Fade in while rising `rise` px into place. `id` must be new to replay.
pub fn appear<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    duration: Duration,
    rise: f32,
    reduced: bool,
) -> impl IntoElement {
    el.with_animation(id, once(duration, reduced), move |el, delta| {
        el.opacity(delta).relative().top(zpx(rise * (1.0 - delta)))
    })
}

/// How a floating control is drawn mid-presence: opacity, a downward
/// offset in px (positive is lower), and a scale about its centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub opacity: f32,
    pub dy: f32,
    pub scale: f32,
}

impl Look {
    pub const REST: Look = Look {
        opacity: 1.,
        dy: 0.,
        scale: 1.,
    };
    fn lerp(self, to: Look, e: f32) -> Look {
        Look {
            opacity: self.opacity + (to.opacity - self.opacity) * e,
            dy: self.dy + (to.dy - self.dy) * e,
            scale: self.scale + (to.scale - self.scale) * e,
        }
    }
}

/// The "Latest" pill arriving: rises 8 px and grows from 96%.
pub const PILL_IN: Duration = Duration::from_millis(240);
/// Leaving: quicker, to 6 px lower at 97%.
pub const PILL_OUT: Duration = Duration::from_millis(180);
const PILL_ENTER_FROM: Look = Look {
    opacity: 0.,
    dy: 8.,
    scale: 0.96,
};
const PILL_EXIT_TO: Look = Look {
    opacity: 0.,
    dy: 6.,
    scale: 0.97,
};

/// `cubic-bezier(0.2, 0.8, 0.2, 1)`: the pill's entrance.
pub fn ease_pill_in(t: f32) -> f32 {
    crate::approval::bezier(0.2, 0.8, 0.2, 1., t)
}

/// Timestamp-driven enter/exit for a floating control. It stays mounted
/// through its exit; a flip mid-way heads back from where it is drawn (the
/// time left scales with the distance left), never restarting from an end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Presence {
    shown: bool,
    from: Look,
    at: Option<std::time::Instant>,
    duration: Duration,
}

impl Default for Presence {
    fn default() -> Self {
        Self::hidden()
    }
}

impl Presence {
    pub fn hidden() -> Self {
        Self {
            shown: false,
            from: PILL_EXIT_TO,
            at: None,
            duration: PILL_OUT,
        }
    }

    fn target(&self) -> Look {
        if self.shown { Look::REST } else { PILL_EXIT_TO }
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    pub fn animating(&self, now: std::time::Instant) -> bool {
        self.at
            .is_some_and(|at| now.saturating_duration_since(at) < self.duration)
    }

    /// Drawn at all: shown, or still playing its exit.
    pub fn mounted(&self, now: std::time::Instant) -> bool {
        self.shown || self.animating(now)
    }

    /// On its way out: draw it, but take no clicks or hover.
    pub fn leaving(&self, now: std::time::Instant) -> bool {
        !self.shown && self.animating(now)
    }

    pub fn look_at(&self, now: std::time::Instant) -> Look {
        let Some(at) = self.at else {
            return self.target();
        };
        let t = (now.saturating_duration_since(at).as_secs_f32() / self.duration.as_secs_f32())
            .clamp(0., 1.);
        let e = if self.shown {
            ease_pill_in(t)
        } else {
            ease_in_out(t)
        };
        self.from.lerp(self.target(), e)
    }

    /// Show or hide. Reduced motion cuts.
    pub fn set(&mut self, shown: bool, now: std::time::Instant, reduced: bool) {
        if shown == self.shown {
            return;
        }
        let from = if self.mounted(now) {
            self.look_at(now)
        } else {
            PILL_ENTER_FROM
        };
        self.shown = shown;
        if reduced {
            self.at = None;
            return;
        }
        let full = if shown { PILL_IN } else { PILL_OUT };
        let left = (self.target().opacity - from.opacity).abs().clamp(0., 1.);
        self.from = from;
        self.duration = full.mul_f32(left).max(Duration::from_millis(1));
        self.at = Some(now);
    }
}

/// A transcript row arriving live: fade and a 5 px rise over `ROW_ENTER`
/// on `ease_out_strong` (0.23, 1, 0.32, 1), its height opening on `glide_curve` (0.32, 0.72, 0, 1).
pub const ROW_ENTER: Duration = Duration::from_millis(220);
pub const ROW_OPEN: Duration = Duration::from_millis(200);
/// Removed or replaced: fade out over the first 60%, while the height
/// closes over all of it on `ease_in_out`, so content is gone before the space folds.
pub const ROW_EXIT: Duration = Duration::from_millis(180);
/// Rows this close together are one burst, each `ROW_STAGGER` after the last.
pub const ROW_BURST: Duration = Duration::from_millis(100);
pub const ROW_STAGGER: Duration = Duration::from_millis(30);

/// A row mid entrance or exit: opacity, downward offset (px) and how much
/// of its natural height it takes (0..1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowLook {
    pub opacity: f32,
    pub dy: f32,
    pub height: f32,
}

/// The entrance of a row whose motion starts `at` (may be ahead, for the
/// stagger), at `now`. `None` once it rests, or under reduced motion.
pub fn row_enter(
    at: std::time::Instant,
    now: std::time::Instant,
    reduced: bool,
) -> Option<RowLook> {
    if reduced {
        return None;
    }
    let Some(elapsed) = now.checked_duration_since(at) else {
        return Some(RowLook {
            opacity: 0.,
            dy: 5.,
            height: 0.,
        });
    };
    if elapsed >= ROW_ENTER.max(ROW_OPEN) {
        return None;
    }
    let p = ease_out_strong((elapsed.as_secs_f32() / ROW_ENTER.as_secs_f32()).min(1.));
    let h = glide_curve((elapsed.as_secs_f32() / ROW_OPEN.as_secs_f32()).min(1.));
    Some(RowLook {
        opacity: p,
        dy: 5. * (1. - p),
        height: h,
    })
}

/// The exit of a row that started leaving `elapsed` ago. Reduced motion
/// keeps a plain fade, with no collapse.
pub fn row_exit(elapsed: Duration, reduced: bool) -> Option<RowLook> {
    if elapsed >= ROW_EXIT {
        return Some(RowLook {
            opacity: 0.,
            dy: 0.,
            height: 0.,
        });
    }
    let t = elapsed.as_secs_f32() / ROW_EXIT.as_secs_f32();
    let fade = ease_out((t / 0.6).min(1.));
    Some(RowLook {
        opacity: 1. - fade,
        dy: 0.,
        height: if reduced { 1. } else { 1. - ease_in_out(t) },
    })
}

/// The "Latest" click's scroll: long enough to read as travel to the end.
pub const SCROLL_TO_END: Duration = Duration::from_millis(350);

/// Where a smooth scroll that began at `from` px (scroll top) stands
/// `elapsed` in, heading for `to` (re-read every frame, so content arriving
/// meanwhile moves the goal), and whether it has arrived. Ease-out.
pub fn scroll_glide(from: f32, to: f32, elapsed: Duration) -> (f32, bool) {
    let t = (elapsed.as_secs_f32() / SCROLL_TO_END.as_secs_f32()).clamp(0., 1.);
    (from + (to - from) * ease_out(t), t >= 1.)
}

/// A sidebar row moving to its new place in the order: long enough to read
/// as travel, short enough not to lag the click that caused it.
pub const REORDER: Duration = Duration::from_millis(300);

/// How far (px) a row that moved is still drawn from its new slot, `elapsed`
/// after the move began from `from` px away: ease-out to 0 over [`REORDER`].
/// Timestamp-driven, so the row element keeps its identity, and a second move
/// mid-flight starts from where the row is drawn, not where it was laid out.
pub fn reorder_offset(from: f32, elapsed: Duration, reduced: bool) -> f32 {
    if reduced || elapsed >= REORDER {
        return 0.;
    }
    from * (1. - ease_out(elapsed.as_secs_f32() / REORDER.as_secs_f32()))
}

/// Rise `lift` px into place over [`SPATIAL`] (negative: drop from above).
pub fn travel<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    lift: f32,
    reduced: bool,
) -> impl IntoElement {
    let duration = if reduced {
        Duration::from_millis(1)
    } else {
        SPATIAL
    };
    el.with_animation(
        id,
        Animation::new(duration).with_easing(ease_out),
        move |el, delta| el.relative().top(zpx(lift * (1.0 - delta))),
    )
}

/// Fade in over [`SPATIAL`], without moving.
pub fn fade_in<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    reduced: bool,
) -> gpui::AnyElement {
    let duration = if reduced {
        Duration::from_millis(1)
    } else {
        SPATIAL
    };
    el.with_animation(
        id,
        Animation::new(duration).with_easing(ease_out),
        |el, delta| el.opacity(delta),
    )
    .into_any_element()
}

/// One frame of a settings drill-in. `t` is already eased (0 at the click,
/// 1 at rest). The child travels a full width, in from the right and back
/// out the same way. The parent only shifts [`DRILL_PARALLAX`] px, so it
/// reads as the page underneath. Reduced motion cuts: no travel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrillFrame {
    /// Parent offset in px. Negative is to the left.
    pub parent_x: f32,
    /// Child offset as a fraction of its own width. 1 is fully off to the right.
    pub child_shift: f32,
}

/// How far the page you came from slides while the new one covers it.
pub const DRILL_PARALLAX: f32 = 28.;

pub fn drill_frame(t: f32, leaving: bool, reduced: bool) -> DrillFrame {
    let t = t.clamp(0., 1.);
    if reduced {
        return DrillFrame {
            parent_x: 0.,
            child_shift: 0.,
        };
    }
    if leaving {
        DrillFrame {
            parent_x: -DRILL_PARALLAX * (1. - t),
            child_shift: t,
        }
    } else {
        DrillFrame {
            parent_x: -DRILL_PARALLAX * t,
            child_shift: 1. - t,
        }
    }
}

/// Settings arriving or leaving, at eased progress `t` (the animation
/// applies [`ease_out`] both ways). 0 is the start of the move, 1 is rest
/// or gone. Opacity and an 8 px rise share that progress; leaving drops
/// the same 8 px, shorter. Reduced motion keeps the fade and drops the rise.
pub fn sheet_look(t: f32, leaving: bool, reduced: bool) -> (f32, f32) {
    let t = t.clamp(0., 1.);
    let opacity = if leaving { 1. - t } else { t };
    let rise = if reduced { 0. } else { 8. };
    let dy = if leaving { rise * t } else { rise * (1. - t) };
    (opacity, dy)
}

/// How visible a side panel is mid-slide: `k` (0..=1) is how open it is.
/// Entering: fully visible by 60% of travel so it reads as arriving, not a late fade.
/// Leaving: fades smoothly from the start so content does not linger across
/// the screen while moving out.
pub fn panel_opacity(k: f32, entering: bool) -> f32 {
    let k = k.clamp(0., 1.);
    if entering {
        ease_out((k / 0.6).min(1.))
    } else {
        k.powf(1.1)
    }
}

/// Fade out over [`SPATIAL`].
pub fn vanish<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    reduced: bool,
) -> impl IntoElement {
    let duration = if reduced {
        Duration::from_millis(1)
    } else {
        SPATIAL
    };
    el.with_animation(
        id,
        Animation::new(duration).with_easing(ease_out),
        |el, delta| el.opacity(1.0 - delta),
    )
}

/// Quick exit: fade and drop a few px over [`FAST`]. For dismissals that should
/// leave faster than they arrived (selection bar, checkmarks).
pub fn dismiss<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    reduced: bool,
) -> gpui::AnyElement {
    if reduced {
        return el.into_any_element();
    }
    el.with_animation(
        id,
        Animation::new(FAST).with_easing(ease_in),
        |el, delta| el.opacity(1.0 - delta).relative().top(zpx(6. * delta)),
    )
    .into_any_element()
}

/// A skeleton block: `el` (sized and filled by the caller) with a soft
/// light band sweeping across it every 1.4 s. `phase` (0..1) offsets the
/// sweep so stacked blocks ripple instead of flashing in unison. Plain under
/// reduced motion.
pub fn shimmer(
    el: gpui::Div,
    id: impl Into<ElementId>,
    phase: f32,
    highlight: gpui::Hsla,
    reduced: bool,
) -> gpui::AnyElement {
    use gpui::{ParentElement, Styled};
    let el = el.relative().overflow_hidden();
    if reduced {
        return el.into_any_element();
    }
    let clear = gpui::Hsla { a: 0., ..highlight };
    let half = |from: gpui::Hsla, to: gpui::Hsla| {
        gpui::div().flex_1().h_full().bg(gpui::linear_gradient(
            90.,
            gpui::linear_color_stop(from, 0.),
            gpui::linear_color_stop(to, 1.),
        ))
    };
    let band = gpui::div()
        .absolute()
        .top_0()
        .bottom_0()
        .w(gpui::relative(0.6))
        .flex()
        .flex_row()
        .child(half(clear, highlight))
        .child(half(highlight, clear))
        .with_animation(
            id,
            Animation::new(SHIMMER)
                .repeat()
                .with_easing(move |t| (t + phase).fract()),
            |el, t| {
                // From fully off the left edge to fully off the right.
                el.left(gpui::relative(-0.6 + 1.8 * t))
            },
        );
    el.child(band).into_any_element()
}

/// Fade in while sliding `dx` px from the left (negative: from the right).
pub fn slide_x<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    dx: f32,
    duration: Duration,
    reduced: bool,
) -> gpui::AnyElement {
    if reduced {
        return el.into_any_element();
    }
    el.with_animation(id, once(duration, reduced), move |el, delta| {
        el.opacity(delta).relative().left(zpx(dx * (1.0 - delta)))
    })
    .into_any_element()
}

/// A 4 px rise on [`ease_out_strong`]. For the one element the eye should
/// land on. gpui's scale is SVG-only, so this stays a rise. Reduced motion skips it.
pub fn settle<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    reduced: bool,
) -> gpui::AnyElement {
    if reduced {
        return el.into_any_element();
    }
    el.with_animation(
        id,
        Animation::new(BASE).with_easing(ease_out_strong),
        |el, delta| el.relative().top(zpx(4. * (1. - delta))),
    )
    .into_any_element()
}

/// Fade and rise in, the `index`-th of a list: each waits 35 ms more than
/// the one before (capped at 10), so a list cascades in. `id` must be new
/// for each load so it plays once per load, not on every render.
pub fn stagger_in<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    index: usize,
    reduced: bool,
) -> gpui::AnyElement {
    if reduced {
        return el.into_any_element();
    }
    let delay = index.min(10) as f32 * 35.;
    let length = BASE.as_millis() as f32;
    let total = Duration::from_millis((delay + length) as u64);
    el.with_animation(
        id,
        Animation::new(total).with_easing(move |t| {
            ease_out_strong(((t * (delay + length) - delay) / length).clamp(0., 1.))
        }),
        |el, delta| el.opacity(delta).relative().top(zpx(4. * (1. - delta))),
    )
    .into_any_element()
}

/// [`stagger_in`] for rows riding in with the sidebar: after 20 ms, 20 ms
/// apart (capped at 6) over 220 ms, so the cascade lands with [`SHEET_OPEN`].
pub fn stagger_in_light<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    index: usize,
    reduced: bool,
) -> gpui::AnyElement {
    if reduced {
        return el.into_any_element();
    }
    let delay = 20. + index.min(6) as f32 * 20.;
    let length = 220.;
    let total = Duration::from_millis((delay + length) as u64);
    el.with_animation(
        id,
        Animation::new(total).with_easing(move |t| {
            ease_out(((t * (delay + length) - delay) / length).clamp(0., 1.))
        }),
        |el, delta| el.opacity(delta).relative().top(zpx(4. * (1. - delta))),
    )
    .into_any_element()
}

/// Breathing opacity for "running" indicators. Static under reduced motion.
pub fn pulse<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    reduced: bool,
) -> gpui::AnyElement {
    if reduced {
        return el.into_any_element();
    }
    el.with_animation(
        id,
        Animation::new(PULSE)
            .repeat()
            .with_easing(gpui::pulsating_between(0.35, 1.0)),
        |el, delta| el.opacity(delta),
    )
    .into_any_element()
}

/// The session timer's dot: opacity 0.5 ↔ 1 over [`BREATHE`]. Solid under
/// reduced motion.
pub fn breathe<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    reduced: bool,
) -> gpui::AnyElement {
    if reduced {
        return el.into_any_element();
    }
    el.with_animation(
        id,
        Animation::new(BREATHE)
            .repeat()
            .with_easing(gpui::pulsating_between(0.5, 1.0)),
        |el, delta| el.opacity(delta),
    )
    .into_any_element()
}

/// A spinning icon. Static under reduced motion.
pub fn spin(
    icon: crate::icons::IconSvg,
    id: impl Into<ElementId>,
    reduced: bool,
) -> gpui::AnyElement {
    if reduced {
        return icon.into_any_element();
    }
    icon.with_animation(id, Animation::new(SPIN).repeat(), |icon, delta| {
        icon.with_transformation(Transformation::rotate(percentage(delta)))
    })
    .into_any_element()
}

/// A 3×3 grid of dots with a brightness wave running corner to corner:
/// the turn's loader. Static under reduced motion.
pub fn dot_grid(color: gpui::Hsla, id: &'static str, reduced: bool) -> gpui::AnyElement {
    let dot = || gpui::div().size(zpx(3.)).rounded_full().bg(color);
    let grid = |cell: &dyn Fn(usize) -> gpui::AnyElement| {
        gpui::div()
            .flex()
            .flex_col()
            .gap(zpx(2.))
            .children((0..3).map(|row| {
                gpui::div()
                    .flex()
                    .flex_row()
                    .gap(zpx(2.))
                    .children((0..3).map(|col| cell(row * 3 + col)))
            }))
    };
    if reduced {
        return grid(&|_| dot().opacity(0.6).into_any_element()).into_any_element();
    }
    grid(&|i| {
        // Diagonal index 0..=4: the wave reaches each diagonal a fifth of a cycle apart.
        let diagonal = (i / 3 + i % 3) as f32;
        dot()
            .with_animation(
                (id, i),
                Animation::new(SPIN + SPIN / 3).repeat(),
                move |el, delta| {
                    let phase = (delta - diagonal / 5.0).rem_euclid(1.0);
                    let bump = (phase * std::f32::consts::TAU).cos() * 0.5 + 0.5;
                    el.opacity(0.2 + 0.8 * bump)
                },
            )
            .into_any_element()
    })
    .into_any_element()
}

/// "1s", "59s", "1m 5s".
pub fn elapsed_label(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// A 0..1 fill that eases to a new target over [`FAST`], on [`ease_out_strong`]
/// so the first frames move at once. Retargeting mid-move starts from where
/// it is. Reduced motion snaps, since the caller passes `animate: false`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FillTween {
    from: f32,
    to: f32,
    start: Option<std::time::Instant>,
}

impl FillTween {
    pub fn settled(v: f32) -> Self {
        Self {
            from: v,
            to: v,
            start: None,
        }
    }

    pub fn value_at(&self, now: std::time::Instant) -> f32 {
        match self.start {
            Some(start) => {
                let t = (now.saturating_duration_since(start).as_secs_f32() / FAST.as_secs_f32())
                    .clamp(0., 1.);
                self.from + (self.to - self.from) * ease_out_strong(t)
            }
            None => self.to,
        }
    }

    pub fn animating(&self, now: std::time::Instant) -> bool {
        self.start
            .is_some_and(|start| now.saturating_duration_since(start) < FAST)
    }

    /// Head for `to`. No move when already there. `animate: false` snaps.
    pub fn retarget(&mut self, to: f32, now: std::time::Instant, animate: bool) {
        if (to - self.to).abs() < 1e-3 {
            return;
        }
        if !animate {
            *self = Self::settled(to);
            return;
        }
        self.from = self.value_at(now);
        self.to = to;
        self.start = Some(now);
    }
}

/// A width that eases to a new target over [`SLOW`] (ease-out). Retargeting
/// mid-move starts from where it is now, so rapid changes never jump. The
/// frames themselves are drawn by a gpui animation keyed on [`Self::generation`]
/// running the same curve from [`Self::from`] to [`Self::to`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WidthTween {
    pub from: f32,
    pub to: f32,
    start: Option<std::time::Instant>,
    /// Bumped on every animated retarget; names the gpui animation.
    pub generation: u64,
}

impl WidthTween {
    /// Resting at `width`: the first render does not grow from 0.
    pub fn settled(width: f32) -> Self {
        Self {
            from: width,
            to: width,
            start: None,
            generation: 0,
        }
    }

    /// Where it is at `now`.
    pub fn value_at(&self, now: std::time::Instant) -> f32 {
        match self.start {
            Some(start) => {
                let t = (now.saturating_duration_since(start).as_secs_f32() / SLOW.as_secs_f32())
                    .clamp(0., 1.);
                self.from + (self.to - self.from) * ease_out(t)
            }
            None => self.to,
        }
    }

    /// Still moving at `now`.
    pub fn animating(&self, now: std::time::Instant) -> bool {
        self.start
            .is_some_and(|start| now.saturating_duration_since(start) < SLOW)
    }

    /// Head for `to`, from the current position. `animate: false` (reduced
    /// motion, or a re-measure of the same text) snaps. True when a new
    /// animation started.
    pub fn retarget(&mut self, to: f32, now: std::time::Instant, animate: bool) -> bool {
        if (to - self.to).abs() < 0.5 {
            return false;
        }
        if !animate {
            *self = Self {
                generation: self.generation,
                ..Self::settled(to)
            };
            return false;
        }
        self.from = self.value_at(now);
        self.to = to;
        self.start = Some(now);
        self.generation += 1;
        true
    }
}

/// A label that rolls (old out, new in, over [`SLOW`]) when its text
/// changes. Identical values never roll; a change while a roll is still
/// running swaps the incoming text and lets that same roll finish, rather
/// than restarting it with a half-faded label.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RollingLabel {
    pub now: String,
    pub prev: String,
    /// Rolls so far; names the roll animation (stable across re-renders).
    pub roll: u64,
    started: Option<std::time::Instant>,
}

impl RollingLabel {
    /// Show `label`. True when a new roll started. The first label, and any
    /// change with `animate` off (reduced motion), just appear.
    pub fn commit(&mut self, label: &str, at: std::time::Instant, animate: bool) -> bool {
        if label == self.now {
            return false;
        }
        let mid_roll = self
            .started
            .is_some_and(|s| at.saturating_duration_since(s) < SLOW);
        if self.now.is_empty() || !animate || mid_roll {
            self.now = label.to_string();
            return false;
        }
        self.prev = std::mem::replace(&mut self.now, label.to_string());
        self.roll += 1;
        self.started = Some(at);
        true
    }
}

/// A fraction for gpui's `relative()` / `percentage()`, which assert 0..=1:
/// clamped, and NaN or infinity (a 0/0 share, say) is 0.
pub fn unit(x: f32) -> f32 {
    if x.is_finite() { x.clamp(0., 1.) } else { 0. }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unit_is_always_a_fraction() {
        for (x, want) in [
            (0.5, 0.5),
            (-0.25, 0.),
            (1.7, 1.),
            (f32::NAN, 0.),
            (f32::INFINITY, 0.),
            (f32::NEG_INFINITY, 0.),
        ] {
            assert_eq!(super::unit(x), want, "{x}");
        }
        // The strong ease-out never leaves 0..=1, even fed out-of-range input.
        for x in [-1., 0., 0.3, 1., 2., f32::NAN] {
            let y = super::unit(super::ease_out_strong(super::unit(x)));
            assert!((0. ..=1.).contains(&y));
        }
    }
    use super::*;

    #[test]
    fn rows_enter_with_a_glide_and_leave_collapsing() {
        let t0 = std::time::Instant::now();
        let ms = |n| Duration::from_millis(n);
        let start = row_enter(t0, t0, false).unwrap();
        assert_eq!((start.opacity, start.height), (0., 0.));
        assert!((start.dy - 5.).abs() < 1e-4);
        // A staggered row waits closed.
        assert_eq!(row_enter(t0 + ms(30), t0, false).unwrap().opacity, 0.);
        let mid = row_enter(t0, t0 + ms(110), false).unwrap();
        assert!(
            mid.opacity > 0.6 && mid.height > 0.6 && mid.dy < 2.,
            "{mid:?}"
        );
        assert_eq!(row_enter(t0, t0 + ROW_ENTER, false), None);
        assert_eq!(row_enter(t0, t0, true), None);
        // Content fades ahead of the space closing.
        let out = row_exit(ms(75), false).unwrap();
        assert!(out.opacity < 0.3 && out.height > 0.5, "{out:?}");
        assert_eq!(row_exit(ms(110), false).unwrap().opacity, 0.);
        assert_eq!(row_exit(ROW_EXIT, false).unwrap().height, 0.);
        assert_eq!(row_exit(ms(75), true).unwrap().height, 1.);
    }

    #[test]
    fn pill_presence_enters_exits_and_reverses_from_where_it_is() {
        let t0 = std::time::Instant::now();
        let ms = |n| Duration::from_millis(n);
        let near = |a: Look, b: Look| {
            (a.opacity - b.opacity).abs() < 1e-4
                && (a.dy - b.dy).abs() < 1e-3
                && (a.scale - b.scale).abs() < 1e-4
        };
        let mut p = Presence::hidden();
        assert!(!p.mounted(t0));
        p.set(true, t0, false);
        assert_eq!(p.look_at(t0), PILL_ENTER_FROM);
        let mid = p.look_at(t0 + ms(60));
        assert!(
            mid.opacity > 0.3 && mid.opacity < 1. && mid.dy > 0. && mid.scale < 1.,
            "{mid:?}"
        );
        assert!(near(p.look_at(t0 + PILL_IN), Look::REST));
        assert!(!p.animating(t0 + PILL_IN));

        // Hide halfway in: start from the drawn look, stay mounted, not interactive.
        let flip = t0 + ms(60);
        p.set(false, flip, false);
        assert!(near(p.look_at(flip), mid));
        assert!(p.leaving(flip + ms(10)) && p.mounted(flip + ms(10)));
        // Less to undo, so less time than a full exit.
        let gone = flip + PILL_OUT.mul_f32(mid.opacity) + ms(1);
        assert!(!p.mounted(gone));
        assert!(near(p.look_at(gone), PILL_EXIT_TO));

        // Show again mid-exit: reverses from the current look.
        let mut q = Presence::hidden();
        q.set(true, t0, false);
        q.set(false, t0 + PILL_IN, false);
        let half = t0 + PILL_IN + ms(90);
        let drawn = q.look_at(half);
        q.set(true, half, false);
        assert!(near(q.look_at(half), drawn));
        assert!(q.shown() && !q.leaving(half));

        let mut r = Presence::hidden();
        r.set(true, t0, true);
        assert_eq!(r.look_at(t0), Look::REST);
        r.set(false, t0, true);
        assert!(!r.mounted(t0));
    }

    #[test]
    fn scroll_glide_eases_out_and_tracks_a_moving_end() {
        let ms = |n| Duration::from_millis(n);
        assert_eq!(scroll_glide(100., 900., ms(0)), (100., false));
        let (mid, done) = scroll_glide(100., 900., ms(175));
        assert!(!done && mid > 500. && mid < 900., "{mid}");
        // The end grew meanwhile: it lands on the new end.
        assert_eq!(scroll_glide(100., 1200., SCROLL_TO_END), (1200., true));
    }

    #[test]
    fn reorder_glide_eases_out_to_rest_and_cuts_when_reduced() {
        let ms = |n| Duration::from_millis(n);
        assert_eq!(reorder_offset(-40., ms(0), false), -40.);
        let mid = reorder_offset(-40., ms(150), false);
        assert!(
            mid > -10. && mid < 0.,
            "ease-out covers most of the way early: {mid}"
        );
        assert_eq!(reorder_offset(-40., REORDER, false), 0.);
        assert_eq!(reorder_offset(-40., ms(0), true), 0.);
        assert!(REORDER >= ms(250) && REORDER <= ms(350));
    }

    #[test]
    fn fill_tween_eases_both_ways_and_turns_around_mid_move() {
        let now = std::time::Instant::now();
        let mut fill = FillTween::settled(0.);
        fill.retarget(1., now, true);
        assert!(fill.value_at(now) < 0.01);
        let early = now + FAST / 5;
        let there = fill.value_at(early);
        // Front-loaded: a fifth of the time has covered more than a fifth of the way.
        assert!(there > 0.2 && there < 0.95, "{there}");
        // A clear mid-move heads back from where it is, not from 1.
        fill.retarget(0., early, true);
        assert!((fill.value_at(early) - there).abs() < 1e-3);
        assert!(fill.value_at(early + FAST) < 0.01);
        // Reduced motion snaps.
        fill.retarget(1., early, false);
        assert!((fill.value_at(early) - 1.).abs() < 1e-6);
        assert!(!fill.animating(early));
    }

    #[test]
    fn glide_curve_starts_moving_and_lands() {
        assert!(glide_curve(0.0).abs() < 1e-4);
        assert!((glide_curve(1.0) - 1.0).abs() < 1e-4);
        // Moving from the first frame, but gentler than a cubic ease-out.
        let first = glide_curve(0.04);
        assert!(
            first > 0.02 && first < ease_out(0.04),
            "first frame {first}"
        );
        // Monotonic.
        assert!(
            (1..=100).all(|i| glide_curve(i as f32 / 100.) >= glide_curve((i - 1) as f32 / 100.))
        );
    }

    #[test]
    fn elapsed_label_rolls_into_minutes() {
        assert_eq!(elapsed_label(Duration::from_millis(1900)), "1s");
        assert_eq!(elapsed_label(Duration::from_secs(65)), "1m 5s");
    }

    #[test]
    fn spring_overshoots_a_little_then_settles() {
        assert_eq!(spring(0.0), 0.0);
        assert_eq!(spring(1.0), 1.0);
        let peak = (0..=100)
            .map(|i| spring(i as f32 / 100.0))
            .fold(0.0, f32::max);
        assert!(peak > 1.03 && peak < 1.09, "overshoot {peak}");
        // Nearly home well before the end: the tail doesn't wobble visibly.
        assert!((spring(0.85) - 1.0).abs() < 0.01);
    }

    #[test]
    fn one_shots_stay_under_the_ceiling_and_ease_out_lands() {
        for d in [FAST, BASE, SLOW] {
            assert!(d <= Duration::from_millis(200));
        }
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        // Front-loaded: more than half the travel in the first third.
        assert!(ease_out(1.0 / 3.0) > 0.6);
    }

    #[test]
    fn panel_opacity_enters_early_and_exits_smoothly() {
        assert_eq!(panel_opacity(0., true), 0.);
        assert_eq!(panel_opacity(1., true), 1.);
        assert_eq!(panel_opacity(0.6, true), 1.);
        assert!(panel_opacity(0.3, true) > 0.8);

        assert_eq!(panel_opacity(0., false), 0.);
        assert_eq!(panel_opacity(1., false), 1.);
        let mid = panel_opacity(0.5, false);
        assert!(mid < 0.55 && mid > 0.4);
        assert!(PANEL_OPEN < Duration::from_millis(300));
        assert!(PANEL_CLOSE < PANEL_OPEN);
    }

    #[test]
    fn drill_enters_from_the_right_and_leaves_the_same_way() {
        let start = drill_frame(0., false, false);
        assert_eq!(start.child_shift, 1.);
        assert_eq!(start.parent_x, 0.);
        let rest = drill_frame(1., false, false);
        assert_eq!(rest.child_shift, 0.);
        assert_eq!(rest.parent_x, -DRILL_PARALLAX);
        // Back starts where forward landed, then reverses out to the right.
        let leave = drill_frame(0., true, false);
        assert_eq!(leave, rest);
        let gone = drill_frame(1., true, false);
        assert_eq!(gone.child_shift, 1.);
        assert_eq!(gone.parent_x, 0.);
        assert_eq!(drill_frame(0.4, false, true).child_shift, 0.);
        assert_eq!(drill_frame(0.4, true, true).parent_x, 0.);
    }

    #[test]
    fn sheet_enters_from_below_and_leaves_the_same_way() {
        assert_eq!(sheet_look(0., false, false), (0., 8.));
        assert_eq!(sheet_look(1., false, false), (1., 0.));
        assert_eq!(sheet_look(0., true, false), (1., 0.));
        assert_eq!(sheet_look(1., true, false), (0., 8.));
        // Reduced motion fades without moving. Midway is halfway: the curve
        // lives in the animation, not here, so it isn't applied twice.
        assert_eq!(sheet_look(0., false, true).1, 0.);
        assert_eq!(sheet_look(1., true, true), (0., 0.));
        assert_eq!(sheet_look(0.5, false, false), (0.5, 4.));
    }

    #[test]
    fn width_tween_settles_retargets_from_current_and_snaps_when_asked() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut w = WidthTween::settled(60.);
        assert_eq!(w.value_at(t0), 60.);
        assert!(!w.animating(t0));
        assert!(!w.retarget(60.2, t0, true), "sub-pixel change is ignored");

        assert!(w.retarget(160., t0, true));
        assert_eq!(w.value_at(t0), 60.);
        let mid = t0 + Duration::from_millis(100);
        let at_mid = w.value_at(mid);
        assert!(at_mid > 60. && at_mid < 160.);
        assert!(
            at_mid > 110.,
            "ease-out is past halfway at half time: {at_mid}"
        );
        assert_eq!(w.value_at(t0 + SLOW), 160.);

        // Retarget mid-move: continues from where it is, no jump.
        assert!(w.retarget(100., mid, true));
        assert_eq!(w.from, at_mid);
        assert_eq!(w.value_at(mid), at_mid);
        assert_eq!(w.generation, 2);
        assert!(w.animating(mid));
        assert_eq!(w.value_at(mid + SLOW), 100.);

        // Reduced motion snaps.
        assert!(!w.retarget(40., mid, false));
        assert_eq!(w.value_at(mid), 40.);
        assert!(!w.animating(mid));
    }

    #[test]
    fn rolling_label_dedupes_and_retargets_mid_roll() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut l = RollingLabel::default();
        assert!(!l.commit("a", t0, true));
        assert!(!l.commit("a", t0, true));
        assert!(l.commit("b", t0, true));
        // Mid-roll: same roll continues toward the newest text.
        assert!(!l.commit("c", t0 + Duration::from_millis(50), true));
        assert_eq!((l.prev.as_str(), l.now.as_str(), l.roll), ("a", "c", 1));
        assert!(l.commit("d", t0 + SLOW + Duration::from_millis(1), true));
        assert_eq!((l.prev.as_str(), l.roll), ("c", 2));
        assert!(
            !l.commit("e", t0 + SLOW * 3, false),
            "reduced motion just swaps"
        );
        assert_eq!(l.now, "e");
    }
}

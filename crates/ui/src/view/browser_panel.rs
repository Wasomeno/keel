//! Browser pane: pages beside the chat, for the app an agent is building.
//! It slides in from the right edge; the live page (a native view on macOS)
//! is moved, clipped and faded with it every frame, so it never lags behind.
//! Drag its left edge to resize; double-click the edge to reset.
//!
//! Tabs: each holds its own page. Each chat's agent works in a tab of its
//! own (see `browser_agent.rs`), so tabs the user opens are never driven;
//! the pane shows the open chat's tab, and other chats' tabs stay out of
//! the strip. File previews open in a tab each. The strip shows once there
//! are two.

#[path = "browser_agent.rs"]
mod agent;

use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt as _, AnyElement, ClickEvent, Context, ElementId, Entity, FocusHandle,
    Focusable as _, FontWeight, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ScrollEvent, Window, div, prelude::*, px, relative,
};
use gpui_base::input::InputState;
use keel_theme::Tokens;

use super::{KeelWindow, icon_button};
use crate::browser::agent::ConsoleLog;
use crate::browser::{PageState, normalize_address};
use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::motion::{
    BASE, FAST, RollingLabel, SLOW, SPIN, WidthTween, ease_in, ease_out, ease_out_strong,
    glide_curve,
};
use crate::style::{HEADER_HEIGHT, KeelStyled, RADIUS_LG, RADIUS_MD, SMALL, c, zpx};
use agent::{AgentTab, BrowserControlState};

/// Default share of the window, and the bounds a drag keeps it in.
const DEFAULT_SHARE: f32 = 0.34;
const MIN_WIDTH: f32 = 320.;
/// The chat keeps at least this much beside the pane.
const MIN_CHAT: f32 = 460.;
/// The page card's inset from the pane edges, and its border.
const PAGE_INSET: f32 = 6.;
/// Addresses the empty pane offers: the usual dev-server ports.
const QUICK: [&str; 3] = ["localhost:3000", "localhost:5173", "localhost:8080"];

pub(super) struct BrowserPanel {
    /// Dragged width; `None` follows the window.
    width: Option<f32>,
    pub(super) resizing: bool,
    /// Pointer offset from the pane's left edge at grab, so the edge
    /// doesn't jump onto the cursor.
    resize_grab: f32,
    /// `width` when the edge was grabbed. A drag that ends there does not
    /// flash the settle line.
    resize_from: Option<f32>,
    pub(super) address: Entity<InputState>,
    pub(super) focus: FocusHandle,
    /// Never empty; `active` is the one shown (and `tabs[0]` stays dormant
    /// until the user or agent claims a Browser tab).
    pub(super) tabs: Vec<BrowserTab>,
    pub(super) active: usize,
    next_tab: u64,
    /// Ended sessions, so the start screen replays its entrance after one.
    sessions: u64,
    /// The window, so host requests run with one even when nothing renders.
    pub(super) handle: gpui::AnyWindowHandle,
    /// Agent script sequence numbers (a reply is matched to its job).
    seq: u64,
    /// The chat the pane last followed, to show its tab after a switch.
    shown_chat: Option<String>,
    /// Last host request: the header's "browsing in the background" dot.
    pub(super) agent_at: Option<Instant>,
    /// Last agent op or pane close, for [`PARK_AFTER`].
    idle_since: Option<Instant>,
    park_task: Option<gpui::Task<()>>,
    /// Isolated viewport entity so CEF frames and cursor updates don't rebuild `KeelWindow`.
    viewport: Entity<BrowserViewportView>,
    /// Isolated progress & settle-sweep entity so loading animations don't rebuild `KeelWindow`.
    load_bar: Entity<BrowserLoadBarView>,
    /// Isolated floating capsule HUD showing agent control and the Take over / Hand back trigger.
    control_hud: Entity<BrowserControlHudView>,
    /// Cached per-tab spinner entities so rotating tab icons don't rebuild `KeelWindow`.
    pub(super) spinners: std::collections::HashMap<u64, Entity<SpinIconView>>,
    /// Last state synced to `viewport` (`(page_ptr, cursor, guarded)`) to skip no-op updates.
    last_viewport: Option<(usize, gpui::CursorStyle, bool)>,
    /// Last state synced to `load_bar` to skip no-op updates.
    last_load_bar: Option<LoadBarSnapshot>,
    /// Last state synced to `control_hud` to skip no-op updates.
    last_hud: Option<ControlHudSnapshot>,
}

/// Snapshot driving [`BrowserLoadBarView`], compared before updating the entity.
#[derive(Clone, PartialEq)]
struct LoadBarSnapshot {
    tokens: Tokens,
    reduced: bool,
    tab_id: u64,
    loading: bool,
    bar: (f32, f32, u64),
    bar_done: bool,
    layout_line: Option<(u64, Option<Instant>)>,
}

/// Isolated entity rendering the thin 2px loading bar and layout-settle sweep under the toolbar.
#[derive(Default)]
pub(super) struct BrowserLoadBarView {
    state: Option<LoadBarSnapshot>,
}

impl BrowserLoadBarView {
    fn sync(&mut self, next: LoadBarSnapshot, cx: &mut Context<Self>) {
        if self.state.as_ref() != Some(&next) {
            self.state = Some(next);
            cx.notify();
        }
    }
}

impl gpui::Render for BrowserLoadBarView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let host = div().id("browser-load-bar").relative().size_full();
        let Some(state) = &self.state else {
            return host;
        };
        let t = state.tokens;
        let bar_el = if state.loading || state.bar_done {
            let (from, to, n) = state.bar;
            let done = state.bar_done;
            let line = div()
                .absolute()
                .left_0()
                .bottom_0()
                .h(px(2.))
                .rounded_r(px(1.))
                .bg(c(t.accent));
            if state.reduced {
                (!done).then(|| line.w(relative(to)).into_any_element())
            } else {
                let duration = if done { BASE * 2 } else { SLOW };
                Some(
                    line.with_animation(
                        ElementId::Name(format!("browser-bar-{}-{n}", state.tab_id).into()),
                        gpui::Animation::new(duration).with_easing(ease_out),
                        move |el, d| {
                            el.w(relative(from + (to - from) * d)).opacity(if done {
                                1. - ease_in(d)
                            } else {
                                1.
                            })
                        },
                    )
                    .into_any_element(),
                )
            }
        } else {
            None
        };

        let layout_el = state.layout_line.map(|(n, leaving)| {
            let segment = div()
                .absolute()
                .top_0()
                .h_full()
                .w(relative(0.28))
                .rounded_r(px(1.))
                .bg(c(t.accent));
            let segment = if state.reduced {
                segment.left_0().into_any_element()
            } else {
                segment
                    .with_animation(
                        ElementId::Name(format!("browser-settle-sweep-{n}").into()),
                        Animation::new(SPIN).repeat(),
                        |el, d| el.left(relative(0.72 * d)),
                    )
                    .into_any_element()
            };
            let track = div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(2.))
                .overflow_hidden()
                .child(segment);
            if state.reduced || leaving.is_none() {
                track.into_any_element()
            } else {
                track
                    .with_animation(
                        ElementId::Name(format!("browser-settle-out-{n}").into()),
                        Animation::new(FAST).with_easing(ease_out_strong),
                        |el, d| el.opacity(1. - d),
                    )
                    .into_any_element()
            }
        });

        host.children(bar_el).children(layout_el)
    }
}

/// Isolated spinning tab indicator so tab loading animation doesn't rebuild `KeelWindow`.
pub(super) struct SpinIconView {
    id: u64,
    reduced: bool,
}

impl SpinIconView {
    fn new(id: u64, reduced: bool) -> Self {
        Self { id, reduced }
    }
}

impl gpui::Render for SpinIconView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(("right-tab-spin-host", self.id as usize))
            .flex_none()
            .child(crate::motion::spin(
                Icon::Spinner.el(zpx(13.)),
                ("right-tab-spin", self.id as usize),
                self.reduced,
            ))
    }
}

/// Duration of the viewport-guard nudge when clicking while the agent is in control.
const HUD_NUDGE_DUR: Duration = Duration::from_millis(320);
/// Capsule HUD entry/exit timing: asymmetric (exit ~64% of entry), and under
/// reduced motion movement is removed while a gentle opacity fade survives.
const HUD_ENTER: Duration = Duration::from_millis(220);
const HUD_EXIT: Duration = Duration::from_millis(140);
const HUD_REDUCED_ENTER: Duration = Duration::from_millis(140);
const HUD_REDUCED_EXIT: Duration = Duration::from_millis(100);
const HUD_ENTER_DY: f32 = 14.;
const HUD_EXIT_DY: f32 = 10.;
const HUD_MODE_SWAP: Duration = Duration::from_millis(180);
const HUD_ROLL_TRAVEL: f32 = 5.;

/// Timestamp-driven, interruptible entry/exit state for the browser capsule HUD.
/// Entry rises smoothly on [`glide_curve`]; exit drops back along the same path
/// on [`ease_out`]. Reversing mid-flight continues from the live `(opacity, dy)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct HudPresence {
    shown: bool,
    from_opacity: f32,
    from_dy: f32,
    at: Option<Instant>,
    duration: Duration,
    reduced: bool,
}

impl Default for HudPresence {
    fn default() -> Self {
        Self::hidden()
    }
}

impl HudPresence {
    pub(super) fn hidden() -> Self {
        Self {
            shown: false,
            from_opacity: 0.,
            from_dy: HUD_EXIT_DY,
            at: None,
            duration: HUD_EXIT,
            reduced: false,
        }
    }

    pub(super) fn cut(&mut self) {
        self.shown = false;
        self.from_opacity = 0.;
        self.from_dy = if self.reduced { 0. } else { HUD_EXIT_DY };
        self.at = None;
    }

    pub(super) fn animating(&self, now: Instant) -> bool {
        self.at
            .is_some_and(|at| now.saturating_duration_since(at) < self.duration)
    }

    pub(super) fn mounted(&self, now: Instant) -> bool {
        self.shown || self.animating(now)
    }

    pub(super) fn leaving(&self, now: Instant) -> bool {
        !self.shown && self.animating(now)
    }

    /// Returns `(opacity, dy_px)` at `now`. Positive `dy_px` shifts downward
    /// toward the bottom edge of the page card.
    pub(super) fn look_at(&self, now: Instant) -> (f32, f32) {
        let (target_opacity, target_dy) = if self.shown {
            (1., 0.)
        } else if self.reduced {
            (0., 0.)
        } else {
            (0., HUD_EXIT_DY)
        };
        let Some(at) = self.at else {
            return (target_opacity, target_dy);
        };
        let raw = (now.saturating_duration_since(at).as_secs_f32() / self.duration.as_secs_f32())
            .clamp(0., 1.);
        if raw >= 1. {
            return (target_opacity, target_dy);
        }
        if self.reduced {
            let e = ease_out(raw);
            return (self.from_opacity + (target_opacity - self.from_opacity) * e, 0.);
        }
        if self.shown {
            let move_e = glide_curve(raw);
            // Opacity settles over the first ~72% so the capsule reads crisply
            // before its deceleration tail finishes.
            let fade_e = ease_out_strong((raw / 0.72).min(1.));
            (
                self.from_opacity + (1. - self.from_opacity) * fade_e,
                self.from_dy * (1. - move_e),
            )
        } else {
            let e = ease_out(raw);
            (
                self.from_opacity * (1. - e),
                self.from_dy + (HUD_EXIT_DY - self.from_dy) * e,
            )
        }
    }

    pub(super) fn set(&mut self, shown: bool, now: Instant, reduced: bool) {
        self.reduced = reduced;
        if shown == self.shown {
            return;
        }
        let was_mounted = self.mounted(now);
        let (cur_opacity, cur_dy) = if was_mounted {
            self.look_at(now)
        } else if reduced {
            (0., 0.)
        } else {
            (0., HUD_ENTER_DY)
        };
        self.shown = shown;
        self.from_opacity = cur_opacity;
        self.from_dy = if reduced { 0. } else { cur_dy };
        let full = match (shown, reduced) {
            (true, false) => HUD_ENTER,
            (false, false) => HUD_EXIT,
            (true, true) => HUD_REDUCED_ENTER,
            (false, true) => HUD_REDUCED_EXIT,
        };
        let target_opacity = if shown { 1. } else { 0. };
        let dist = (target_opacity - cur_opacity).abs().clamp(0.05, 1.);
        self.duration = full.mul_f32(dist).max(Duration::from_millis(1));
        self.at = Some(now);
    }
}

/// Compute `(dy_px, highlight_0_to_1)` for a viewport-guard nudge at `elapsed`.
/// Reduced motion keeps the highlight and drops the vertical lift.
pub(super) fn hud_nudge_frame(elapsed: Duration, reduced: bool) -> (f32, f32) {
    if elapsed >= HUD_NUDGE_DUR {
        return (0., 0.);
    }
    let t = (elapsed.as_secs_f32() / HUD_NUDGE_DUR.as_secs_f32()).clamp(0., 1.);
    let highlight = (1. - ease_out(t)).clamp(0., 1.);
    let dy = if reduced {
        0.
    } else {
        -3. * (t * std::f32::consts::PI).sin() * (1. - t)
    };
    (dy, highlight)
}

/// Snapshot driving [`BrowserControlHudView`], compared before updating the entity.
#[derive(Clone, PartialEq)]
struct ControlHudSnapshot {
    tokens: Tokens,
    reduced: bool,
    tab_id: u64,
    mode: BrowserControlState,
    op_label: Option<&'static str>,
    show_page: bool,
}

/// Isolated floating capsule HUD inside `browser-page`: shows when the agent
/// is driving the page and gives a 1-click "Take over" / "Hand back" trigger.
pub(super) struct BrowserControlHudView {
    state: Option<ControlHudSnapshot>,
    presence: HudPresence,
    nudge_at: Option<Instant>,
    label_roll: RollingLabel,
    label_w: Option<WidthTween>,
    label_cur_w: f32,
    label_prev_w: f32,
    mode_is_agent: Option<bool>,
    mode_swap_at: Option<Instant>,
    mode_gen: u64,
    parent: gpui::WeakEntity<KeelWindow>,
}

impl BrowserControlHudView {
    fn new(parent: gpui::WeakEntity<KeelWindow>) -> Self {
        Self {
            state: None,
            presence: HudPresence::hidden(),
            nudge_at: None,
            label_roll: RollingLabel::default(),
            label_w: None,
            label_cur_w: 0.,
            label_prev_w: 0.,
            mode_is_agent: None,
            mode_swap_at: None,
            mode_gen: 0,
            parent,
        }
    }

    fn sync(&mut self, next: ControlHudSnapshot, cx: &mut Context<Self>) {
        if self.state.as_ref() != Some(&next) {
            let switched_tab = self.state.as_ref().is_some_and(|s| s.tab_id != next.tab_id);
            if switched_tab {
                self.nudge_at = None;
                self.label_roll = RollingLabel::default();
                self.label_w = None;
                self.label_cur_w = 0.;
                self.label_prev_w = 0.;
                self.mode_is_agent = None;
                self.mode_swap_at = None;
            } else if !matches!(next.mode, BrowserControlState::AgentInControl { .. }) {
                self.nudge_at = None;
            }
            self.state = Some(next);
            cx.notify();
        }
    }

    pub(super) fn trigger_nudge(&mut self, cx: &mut Context<Self>) {
        self.nudge_at = Some(Instant::now());
        cx.notify();
    }
}

impl gpui::Render for BrowserControlHudView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let Some(state) = self.state.clone() else {
            self.presence.cut();
            return div().id("browser-control-hud").into_any_element();
        };

        let want = state.show_page && state.mode != BrowserControlState::None;
        let was_mounted = self.presence.mounted(now);
        self.presence.set(want, now, state.reduced);

        let (nudge_dy, nudge_hi) = match self.nudge_at {
            Some(at) => {
                let elapsed = now.saturating_duration_since(at);
                if elapsed >= HUD_NUDGE_DUR {
                    self.nudge_at = None;
                    (0., 0.)
                } else {
                    hud_nudge_frame(elapsed, state.reduced)
                }
            }
            None => (0., 0.),
        };

        if !self.presence.mounted(now) {
            self.label_roll = RollingLabel::default();
            self.label_w = None;
            self.label_cur_w = 0.;
            self.label_prev_w = 0.;
            self.mode_is_agent = None;
            self.mode_swap_at = None;
            return div().id("browser-control-hud").into_any_element();
        }

        let t = state.tokens;
        let (look_opacity, look_dy) = self.presence.look_at(now);
        let leaving = self.presence.leaving(now);
        let is_agent = matches!(state.mode, BrowserControlState::AgentInControl { .. });
        let busy = matches!(state.mode, BrowserControlState::AgentInControl { busy: true });

        if want {
            match self.mode_is_agent {
                Some(prev) if prev != is_agent => {
                    self.mode_is_agent = Some(is_agent);
                    self.mode_gen += 1;
                    self.mode_swap_at = if was_mounted && !state.reduced {
                        Some(now)
                    } else {
                        None
                    };
                }
                None => {
                    self.mode_is_agent = Some(is_agent);
                    self.mode_swap_at = None;
                }
                _ => {}
            }
        }

        let swap_progress = self.mode_swap_at.and_then(|at| {
            let elapsed = now.saturating_duration_since(at);
            if elapsed >= HUD_MODE_SWAP {
                self.mode_swap_at = None;
                None
            } else {
                Some(ease_out_strong(
                    (elapsed.as_secs_f32() / HUD_MODE_SWAP.as_secs_f32()).clamp(0., 1.),
                ))
            }
        });

        let status_label: String = if is_agent {
            if busy {
                match state.op_label {
                    Some(op) => format!("Agent · {op}"),
                    None => "Agent in control".into(),
                }
            } else {
                "Agent in control".into()
            }
        } else {
            "You're in control".into()
        };

        let target_w = super::composer_view::measure_label(window, &status_label, FontWeight::MEDIUM);
        let label_changed = !self.label_roll.now.is_empty() && status_label != self.label_roll.now;
        let animate_inner = was_mounted && !state.reduced && !leaving;
        match &mut self.label_w {
            None => self.label_w = Some(WidthTween::settled(target_w)),
            Some(tween) => {
                tween.retarget(target_w, now, label_changed && animate_inner);
            }
        }
        if self.label_roll.commit(&status_label, now, animate_inner) {
            self.label_prev_w = self.label_cur_w;
        }
        self.label_cur_w = target_w;
        let width = self.label_w.expect("set above");
        let width_moving = width.animating(now) && !state.reduced;

        if self.presence.animating(now)
            || self.nudge_at.is_some()
            || swap_progress.is_some()
            || width_moving
        {
            window.request_animation_frame();
        }

        let dot_color = match swap_progress {
            Some(p) => {
                let from = c(if is_agent { t.success } else { t.info });
                let to = c(if is_agent { t.info } else { t.success });
                from.blend(to.opacity(p))
            }
            None => c(if is_agent { t.info } else { t.success }),
        };

        let dot_base = div()
            .flex_none()
            .size(zpx(7.))
            .rounded_full()
            .bg(dot_color);
        let dot: AnyElement = if busy {
            crate::motion::pulse(
                dot_base,
                ("browser-hud-pulse", state.tab_id as usize),
                state.reduced,
            )
        } else {
            dot_base
                .opacity(if is_agent { 0.85 } else { 0.9 })
                .into_any_element()
        };

        let roll_h = SMALL.line;
        let roll_anim = || Animation::new(SLOW).with_easing(ease_out);
        let spacer = div().h(zpx(roll_h));
        let spacer = if width_moving {
            let (from, to) = (width.from, width.to);
            spacer
                .with_animation(
                    ("browser-hud-label-w", width.generation as usize),
                    roll_anim(),
                    move |el, d| el.w(px(from + (to - from) * d)),
                )
                .into_any_element()
        } else {
            spacer.w(px(target_w)).into_any_element()
        };
        let label_line = |text: String, w: f32| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .h(zpx(roll_h))
                .w(px(w))
                .flex()
                .items_center()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .t_small()
                .font_weight(FontWeight::MEDIUM)
                .text_color(c(t.text))
                .child(text)
        };
        let label_frame = div()
            .relative()
            .flex_shrink(1.)
            .min_w_0()
            .max_w_full()
            .h(zpx(roll_h))
            .overflow_hidden()
            .child(spacer);
        let roll_n = self.label_roll.roll as usize;
        let label_el: AnyElement = if roll_n == 0 || state.reduced {
            label_frame
                .child(label_line(status_label, target_w).when(!width_moving, |d| d.max_w_full()))
                .into_any_element()
        } else {
            label_frame
                .child(
                    label_line(status_label, target_w)
                        .when(!width_moving, |d| d.max_w_full())
                        .with_animation(("browser-hud-roll", roll_n * 2 + 1), roll_anim(), |el, d| {
                            el.top(zpx(HUD_ROLL_TRAVEL * (1. - d))).opacity(d)
                        }),
                )
                .child(
                    label_line(self.label_roll.prev.clone(), self.label_prev_w).with_animation(
                        ("browser-hud-roll", roll_n * 2),
                        roll_anim(),
                        |el, d| el.top(zpx(-HUD_ROLL_TRAVEL * d)).opacity((1. - d * 1.5).max(0.)),
                    ),
                )
                .into_any_element()
        };

        let tab_id = state.tab_id;
        let swap_y = swap_progress.map_or(0., |p| 2. * (1. - p));
        let swap_op = swap_progress.unwrap_or(1.);
        let action_btn: AnyElement = if is_agent {
            let tip_label = "Stop agent and take manual control";
            div()
                .id(("browser-takeover-btn", self.mode_gen as usize))
                .flex_none()
                .relative()
                .top(px(nudge_dy + swap_y))
                .opacity(swap_op)
                .h(zpx(24.))
                .px(zpx(10.))
                .rounded_full()
                .bg(c(t.accent))
                .border_1()
                .border_color(if nudge_hi > 0.01 {
                    c(t.info).opacity(0.9 * nudge_hi)
                } else {
                    gpui::transparent_black()
                })
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(5.))
                .t_small()
                .font_weight(FontWeight::MEDIUM)
                .text_color(c(t.text_on_accent))
                .when(!leaving, |b| {
                    b.cursor_pointer()
                        .hover_fade(c(t.accent_hover), Some(c(t.accent).opacity(0.84)))
                        .active(|s| s.relative().top(px(0.5)))
                        .tooltip(move |_, cx| {
                            cx.new(|_| super::dotify_ui::Tip::new(tip_label, t)).into()
                        })
                        .tooltip_show_delay(crate::view::tooltip::delay())
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            let _ = this.parent.update(cx, |kw, cx| {
                                if let Some(ix) = kw.browser.tab_ix(tab_id) {
                                    kw.take_over_browser_tab(ix, window, cx);
                                }
                            });
                        }))
                })
                .child(Icon::PointerClick.el(zpx(11.)))
                .child("Take over")
                .into_any_element()
        } else {
            let tip_label = "Return browser control to the agent";
            div()
                .id(("browser-handback-btn", self.mode_gen as usize))
                .flex_none()
                .relative()
                .top(px(swap_y))
                .opacity(swap_op)
                .h(zpx(24.))
                .px(zpx(10.))
                .rounded_full()
                .bg(c(t.surface))
                .border_1()
                .border_color(c(t.border))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(5.))
                .t_small()
                .font_weight(FontWeight::MEDIUM)
                .text_color(c(t.text))
                .when(!leaving, |b| {
                    b.cursor_pointer()
                        .hover_fade(c(t.hover), Some(c(t.selected)))
                        .active(|s| s.relative().top(px(0.5)))
                        .tooltip(move |_, cx| {
                            cx.new(|_| super::dotify_ui::Tip::new(tip_label, t)).into()
                        })
                        .tooltip_show_delay(crate::view::tooltip::delay())
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            let _ = this.parent.update(cx, |kw, cx| {
                                if let Some(ix) = kw.browser.tab_ix(tab_id) {
                                    kw.hand_back_browser_tab(ix, window, cx);
                                }
                            });
                        }))
                })
                .child(Icon::Agent.el(zpx(11.)))
                .child("Hand back")
                .into_any_element()
        };

        let capsule_border = if nudge_hi > 0.01 {
            c(t.border).blend(c(t.info).opacity(0.65 * nudge_hi))
        } else {
            c(t.border)
        };

        let capsule = div()
            .id("browser-control-hud")
            .max_w_full()
            .h(zpx(34.))
            .pl(zpx(12.))
            .pr(zpx(5.))
            .floating_surface(t)
            .rounded_full()
            .border_color(capsule_border)
            .opacity(look_opacity)
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(10.))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(7.))
                    .child(dot)
                    .child(label_el),
            )
            .child(action_btn);

        div()
            .id("browser-control-hud-anchor")
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(12. - look_dy))
            .px_3()
            .flex()
            .flex_row()
            .justify_center()
            .child(capsule)
            .into_any_element()
    }
}

/// Isolated CEF viewport entity so 60fps browser paints and cursor changes don't rebuild `KeelWindow`.
pub(super) struct BrowserViewportView {
    page: Option<std::rc::Rc<crate::browser::NativePage>>,
    cursor: gpui::CursorStyle,
    guarded: bool,
    focus: FocusHandle,
    hud: Entity<BrowserControlHudView>,
    parent: gpui::WeakEntity<KeelWindow>,
}

impl BrowserViewportView {
    fn new(
        focus: FocusHandle,
        hud: Entity<BrowserControlHudView>,
        parent: gpui::WeakEntity<KeelWindow>,
    ) -> Self {
        Self {
            page: None,
            cursor: gpui::CursorStyle::Arrow,
            guarded: false,
            focus,
            hud,
            parent,
        }
    }

    fn set_state(
        &mut self,
        page: Option<std::rc::Rc<crate::browser::NativePage>>,
        cursor: gpui::CursorStyle,
        guarded: bool,
        cx: &mut Context<Self>,
    ) {
        self.page = page;
        self.cursor = cursor;
        self.guarded = guarded;
        cx.notify();
    }

    fn notify_frame(
        &mut self,
        page: std::rc::Rc<crate::browser::NativePage>,
        cursor: gpui::CursorStyle,
        guarded: bool,
        cx: &mut Context<Self>,
    ) {
        self.page = Some(page);
        self.cursor = cursor;
        self.guarded = guarded;
        cx.notify();
    }
}

impl gpui::Render for BrowserViewportView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(native) = self.page.clone() else {
            return div().id("browser-viewport").size_full().into_any_element();
        };
        let page_prep = std::rc::Rc::downgrade(&native);
        let page_paint = std::rc::Rc::downgrade(&native);
        let focus = self.focus.clone();
        let focus_prep = focus.clone();
        let guarded = self.guarded;
        div()
            .id("browser-viewport")
            .relative()
            .size_full()
            .cursor(if guarded {
                gpui::CursorStyle::Arrow
            } else {
                self.cursor
            })
            .track_focus(&focus)
            .key_context(crate::keys::BROWSER_VIEWPORT)
            .on_action(cx.listener(
                |this, _: &gpui_base::input::Copy, _window, cx| {
                    if !this.guarded
                        && let Some(native) = &this.page
                    {
                        native.copy();
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(cx.listener(
                |this, _: &gpui_base::input::Cut, _window, cx| {
                    if !this.guarded
                        && let Some(native) = &this.page
                    {
                        native.cut();
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(cx.listener(
                |this, _: &gpui_base::input::Paste, _window, cx| {
                    if !this.guarded
                        && let Some(native) = &this.page
                    {
                        native.paste();
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(cx.listener(
                |this, _: &crate::keys::BrowserPasteAndMatchStyle, _window, cx| {
                    if !this.guarded
                        && let Some(native) = &this.page
                    {
                        native.paste_and_match_style();
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(cx.listener(
                |this, _: &gpui_base::input::SelectAll, _window, cx| {
                    if !this.guarded
                        && let Some(native) = &this.page
                    {
                        native.select_all();
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(cx.listener(
                |this, _: &gpui_base::input::Undo, _window, cx| {
                    if !this.guarded
                        && let Some(native) = &this.page
                    {
                        native.undo();
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(cx.listener(
                |this, _: &gpui_base::input::Redo, _window, cx| {
                    if !this.guarded
                        && let Some(native) = &this.page
                    {
                        native.redo();
                        cx.stop_propagation();
                    }
                },
            ))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if !this.guarded
                    && let Some(native) = &this.page
                    && native.handle_key_down(event)
                {
                    cx.stop_propagation();
                }
            }))
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, _window, cx| {
                if !this.guarded
                    && let Some(native) = &this.page
                    && native.handle_key_up(event)
                {
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if this.guarded {
                        this.hud.update(cx, |hud, cx| hud.trigger_nudge(cx));
                        cx.stop_propagation();
                        return;
                    }
                    let was_focused = this.focus.is_focused(window);
                    window.focus(&this.focus, cx);
                    if !was_focused {
                        cx.notify();
                        let _ = this.parent.update(cx, |_, cx| cx.notify());
                    }
                    if let Some(native) = &this.page {
                        native.handle_mouse_down(
                            event.position,
                            MouseButton::Left,
                            event.modifiers,
                            event.click_count,
                        );
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if this.guarded {
                        this.hud.update(cx, |hud, cx| hud.trigger_nudge(cx));
                        cx.stop_propagation();
                        return;
                    }
                    let was_focused = this.focus.is_focused(window);
                    window.focus(&this.focus, cx);
                    if !was_focused {
                        cx.notify();
                        let _ = this.parent.update(cx, |_, cx| cx.notify());
                    }
                    if let Some(native) = &this.page {
                        native.handle_mouse_down(
                            event.position,
                            MouseButton::Right,
                            event.modifiers,
                            event.click_count,
                        );
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if this.guarded {
                        this.hud.update(cx, |hud, cx| hud.trigger_nudge(cx));
                        cx.stop_propagation();
                        return;
                    }
                    let was_focused = this.focus.is_focused(window);
                    window.focus(&this.focus, cx);
                    if !was_focused {
                        cx.notify();
                        let _ = this.parent.update(cx, |_, cx| cx.notify());
                    }
                    if let Some(native) = &this.page {
                        native.handle_mouse_down(
                            event.position,
                            MouseButton::Middle,
                            event.modifiers,
                            event.click_count,
                        );
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Navigate(gpui::NavigationDirection::Back),
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    if this.guarded {
                        cx.stop_propagation();
                        return;
                    }
                    window.focus(&this.focus, cx);
                    if let Some(native) = &this.page {
                        native.history(false);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Navigate(gpui::NavigationDirection::Forward),
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    if this.guarded {
                        cx.stop_propagation();
                        return;
                    }
                    window.focus(&this.focus, cx);
                    if let Some(native) = &this.page {
                        native.history(true);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
                    if !this.guarded
                        && let Some(native) = &this.page
                    {
                        native.handle_mouse_up(
                            event.position,
                            MouseButton::Left,
                            event.modifiers,
                            event.click_count,
                        );
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
                    if !this.guarded
                        && let Some(native) = &this.page
                    {
                        native.handle_mouse_up_outside(
                            event.position,
                            MouseButton::Left,
                            event.modifiers,
                            event.click_count,
                        );
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
                    if !this.guarded
                        && let Some(native) = &this.page
                    {
                        native.handle_mouse_up(
                            event.position,
                            MouseButton::Right,
                            event.modifiers,
                            event.click_count,
                        );
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
                    if !this.guarded
                        && let Some(native) = &this.page
                    {
                        native.handle_mouse_up(
                            event.position,
                            MouseButton::Middle,
                            event.modifiers,
                            event.click_count,
                        );
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, _cx| {
                if !this.guarded
                    && let Some(native) = &this.page
                {
                    native.handle_mouse_move(
                        event.position,
                        event.modifiers,
                        event.pressed_button,
                    );
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, window, _cx| {
                if !this.guarded
                    && !*hovered
                    && let Some(native) = &this.page
                {
                    native.handle_mouse_leave(window.mouse_position(), window.modifiers());
                }
            }))
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _window, _cx| {
                if this.guarded {
                    return;
                }
                if let Some(native) = &this.page {
                    native.handle_scroll_wheel(event.position, event.delta, event.modifiers);
                }
            }))
            .child(
                gpui::canvas(
                    move |bounds, window, _cx| {
                        if let Some(page) = page_prep.upgrade() {
                            let scale = window.scale_factor();
                            page.set_scale_factor(scale);
                            page.set_focus(!guarded && focus_prep.is_focused(window));
                            page.sync(bounds, bounds, 1.0, false);
                        }
                    },
                    move |bounds, _, window, _cx| {
                        if let Some(page) = page_paint.upgrade() {
                            for old in page.take_dropped_frames() {
                                let _ = window.drop_image(old);
                            }
                            if let Some(img) = page.render_frame() {
                                let _ = window.paint_image(
                                    bounds,
                                    bounds,
                                    Default::default(),
                                    img,
                                    0,
                                    false,
                                );
                            }
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element()
    }
}

/// One tab: its page and what the toolbar shows for it.
pub(super) struct BrowserTab {
    pub(super) id: u64,
    pub(super) page: PageState,
    /// Progress bar: from, to, generation (each step replays its glide).
    bar: (f32, f32, u64),
    /// The load finished: the bar fills and fades.
    bar_done: bool,
    pub(super) error: Option<String>,
    /// A local file shown from a transcript chip (the page is its preview,
    /// loaded as HTML, so WebKit reports `about:blank` for it).
    preview: Option<std::path::PathBuf>,
    pub(super) native: Option<std::rc::Rc<crate::browser::NativePage>>,
    _events: Option<gpui::Task<()>>,
    _pump: Option<gpui::Task<()>>,
    /// The last web page (not a file preview).
    web_url: Option<String>,
    /// The page was unloaded after [`PARK_AFTER`] idle with the pane closed;
    /// `web_url` is loaded again when the tab is needed.
    pub(super) parked: bool,
    /// Where an unloaded page was scrolled, put back when it returns.
    restore: Option<(f64, f64)>,
    /// The scroll read that precedes unloading, in flight.
    park_seq: Option<u64>,
    /// The chat whose agent drives this tab.
    agent: Option<AgentTab>,
    /// Console messages and page errors (`browser_console`).
    console: ConsoleLog,
    /// Settle line while the page bitmap catches the pane. Generation, and
    /// when the fade-out started (`None` while the paint is still due).
    layout_line: Option<(u64, Option<Instant>)>,
}

impl BrowserTab {
    fn new(id: u64) -> Self {
        Self {
            id,
            page: PageState::default(),
            bar: (0., 0., 0),
            bar_done: false,
            error: None,
            preview: None,
            native: None,
            _events: None,
            _pump: None,
            web_url: None,
            parked: false,
            restore: None,
            park_seq: None,
            agent: None,
            console: ConsoleLog::default(),
            layout_line: None,
        }
    }

    /// A load started: the bar restarts.
    fn loading(&mut self, url: String) {
        self.error = None;
        self.page.url = Some(url);
        self.page.loading = true;
        self.bar = (0., 0.08, self.bar.2 + 1);
        self.bar_done = false;
    }

    /// True while this tab is still an unused empty placeholder.
    pub(super) fn is_empty_scratch(&self) -> bool {
        self.agent.is_none()
            && self.page.url.is_none()
            && self.preview.is_none()
            && self.error.is_none()
    }

    pub(super) fn is_agent(&self) -> bool {
        self.agent.is_some()
    }

    fn control_state(&self, now: Instant) -> BrowserControlState {
        self.agent
            .as_ref()
            .map_or(BrowserControlState::None, |a| a.control_state(now))
    }

    pub(super) fn agent_in_control(&self, now: Instant) -> bool {
        matches!(
            self.control_state(now),
            BrowserControlState::AgentInControl { .. }
        )
    }

    /// What its tab says: the page title, else the host, else "New Tab".
    pub(super) fn label(&self) -> String {
        if !self.page.title.trim().is_empty() {
            return self.page.title.trim().to_string();
        }
        if let Some(path) = &self.preview {
            return path
                .file_name()
                .map_or_else(|| "File".into(), |n| n.to_string_lossy().into_owned());
        }
        match self.page.url.as_deref() {
            Some(url) => {
                let rest = url.split_once("://").map_or(url, |(_, r)| r);
                rest.split(['/', '?', '#'])
                    .next()
                    .unwrap_or(rest)
                    .to_string()
            }
            None => "New Tab".into(),
        }
    }
}

/// One browser request from the engine (an agent's browser_* tool).
pub(super) struct HostRequest {
    request_id: String,
    op: String,
    url: String,
    params: serde_json::Value,
    client: keel_rpc::RpcClient,
    /// The chat whose agent asked (empty from an old shim).
    chat: String,
}

/// A screenshot in flight; `full` once the page size is known.
struct ShotRequest {
    request_id: String,
    client: keel_rpc::RpcClient,
    full_page: bool,
    full: Option<FullPage>,
    /// Full page: frames so far (PNG, CSS y), the y asked for, and the
    /// y the page really scrolled to.
    frames: Vec<(Vec<u8>, f64)>,
    asked_y: f64,
    at_y: f64,
}

/// A full-page capture: CSS width, captured height, document height,
/// viewport height, and the scroll position to put back.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FullPage {
    w: f64,
    h: f64,
    total: f64,
    vh: f64,
    sx: f64,
    sy: f64,
}

/// Pause after each full-page scroll so lazy images can paint.
const FRAME_SETTLE: Duration = Duration::from_millis(160);

/// Tallest full-page capture, in CSS px.
const FULL_PAGE_MAX: f64 = 16_000.;
/// Pages left running with the pane closed and no agent op this long are
/// unloaded (each WebContent process is a few hundred MB); `web_url` brings
/// one back when it is needed again.
const PARK_AFTER: Duration = Duration::from_secs(10 * 60);

impl BrowserPanel {
    pub(super) fn new(window: &mut Window, cx: &mut Context<KeelWindow>) -> Self {
        let focus = cx.focus_handle();
        let parent = cx.entity().downgrade();
        let control_hud = cx.new(|_| BrowserControlHudView::new(parent.clone()));
        Self {
            width: None,
            resizing: false,
            resize_grab: 0.,
            resize_from: None,
            address: cx
                .new(|cx| InputState::new(window, cx).placeholder("Search or enter address")),
            viewport: cx.new(|_| BrowserViewportView::new(focus.clone(), control_hud.clone(), parent)),
            load_bar: cx.new(|_| BrowserLoadBarView::default()),
            control_hud,
            spinners: std::collections::HashMap::new(),
            last_viewport: None,
            last_load_bar: None,
            last_hud: None,
            focus,
            tabs: vec![BrowserTab::new(0)],
            active: 0,
            next_tab: 1,
            sessions: 0,
            handle: window.window_handle(),
            seq: 0,
            shown_chat: None,
            agent_at: None,
            idle_since: None,
            park_task: None,
        }
    }

    pub(super) fn tab_spinner(
        &mut self,
        id: u64,
        reduced: bool,
        cx: &mut Context<KeelWindow>,
    ) -> Entity<SpinIconView> {
        let spinner = self
            .spinners
            .entry(id)
            .or_insert_with(|| cx.new(|_| SpinIconView::new(id, reduced)))
            .clone();
        if spinner.read(cx).reduced != reduced {
            spinner.update(cx, |s, cx| {
                s.reduced = reduced;
                cx.notify();
            });
        }
        spinner
    }

    pub(super) fn tab(&self) -> &BrowserTab {
        &self.tabs[self.active]
    }

    pub(super) fn active_native_page(&self) -> Option<std::rc::Rc<crate::browser::NativePage>> {
        self.tabs.get(self.active).and_then(|t| t.native.clone())
    }

    fn tab_mut(&mut self) -> &mut BrowserTab {
        &mut self.tabs[self.active]
    }

    pub(super) fn tab_ix(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Tabs the strip shows: the user's, and the open chat's agent tab.
    pub(super) fn visible_in_strip(&self, tab: &BrowserTab, chat: Option<&str>) -> bool {
        tab.agent
            .as_ref()
            .is_none_or(|a| a.chat.is_empty() || Some(a.chat.as_str()) == chat)
    }

    /// A new empty tab after the others; its index.
    pub(super) fn push_tab(&mut self) -> usize {
        self.tabs.push(BrowserTab::new(self.next_tab));
        self.next_tab += 1;
        self.tabs.len() - 1
    }

    /// Native focus back to gpui, so keys reach Keel's own fields.
    pub(super) fn release_focus(&self) {
        if let Some(native) = &self.tab().native {
            native.focus_parent();
        }
    }
}

impl KeelWindow {
    /// The pane belongs to an open chat session only: never the new-chat
    /// screen or Settings. `was_hero` is this frame's hero state.
    pub(super) fn browser_allowed(&self) -> bool {
        !self.was_hero
            && !self.settings_open
            && self.active_bot().is_none()
            && self.confirming_delete_chats.is_none()
            && self.state.confirming_delete_space.is_none()
    }

    /// True when the live web viewport (not the address bar or chat composer) has focus.
    pub(super) fn browser_viewport_focused(&self, window: &Window) -> bool {
        self.browser.focus.is_focused(window)
            && self.right_panel_open()
            && matches!(
                self.right_panel_active_kind(),
                Some(super::right_panel::RightTabKind::Browser(_))
            )
            && self.browser.tab().native.is_some()
            && self.browser.tab().page.url.is_some()
            && self.browser.tab().error.is_none()
    }

    /// An agent's page is alive with the right side panel closed: the
    /// header's panel button carries a dot (bright while a request is recent).
    pub(super) fn browser_in_background(&self) -> Option<bool> {
        let b = &self.browser;
        let alive = b.tabs.iter().any(|tab| {
            tab.agent.as_ref().is_some_and(|a| !a.user_takeover)
                && tab.native.as_ref().is_some_and(|n| n.background())
                && tab.page.url.is_some()
        });
        (alive && !self.right_panel_open()).then(|| {
            b.agent_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(20))
        })
    }

    /// ⌘⇧B: open/focus Browser tab in the right side panel, or close the panel
    /// if Browser is already active.
    pub(super) fn toggle_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.browser_allowed() {
            return;
        }
        if self.right_panel.open
            && matches!(
                self.right_panel_active_kind(),
                Some(super::right_panel::RightTabKind::Browser(_))
            )
        {
            self.close_right_panel(window, cx);
        } else {
            self.right_open_or_focus_browser(window, cx);
        }
    }

    /// ⌘L: the address field, opening or focusing a Browser tab if needed.
    pub(super) fn focus_browser_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.browser_allowed() {
            return;
        }
        if !self.right_panel.open
            || !matches!(
                self.right_panel_active_kind(),
                Some(super::right_panel::RightTabKind::Browser(_))
            )
        {
            self.right_open_or_focus_browser(window, cx);
        }
        self.browser.release_focus();
        self.browser.address.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Load what the address field holds.
    pub(super) fn browser_navigate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.browser.address.read(cx).value().to_string();
        let Some(url) = normalize_address(&typed) else {
            return;
        };
        self.browser_load(url, window, cx);
    }

    /// Show tab `ix`: the address follows it, and a parked page comes back.
    pub(super) fn activate_tab(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ix >= self.browser.tabs.len() {
            return;
        }
        if ix != self.browser.active {
            self.browser.release_focus();
            self.browser.active = ix;
        }
        if self.is_browser_tab_showing(ix) {
            self.unpark_tab(ix, window, cx);
        }
        let b = &self.browser;
        let url = b.tab().page.url.clone().unwrap_or_default();
        b.address.update(cx, |s, cx| s.set_value(url, window, cx));
        if b.tab().page.url.is_none() {
            b.address.update(cx, |s, cx| s.focus(window, cx));
        } else {
            window.focus(&b.focus, cx);
        }
        cx.notify();
    }

    /// Release native view & agent state for browser tab index `ix`.
    pub(super) fn teardown_browser_tab(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ix >= self.browser.tabs.len() {
            return;
        }
        self.drop_agent_tab(ix, "the user closed this chat's browser tab", cx);
        let b = &mut self.browser;
        b.tabs[ix].agent = None;
        b.release_focus();
        if b.tabs.len() == 1 {
            let next_id = b.next_tab;
            b.next_tab += 1;
            b.tabs[0] = BrowserTab::new(next_id);
            b.active = 0;
            b.sessions += 1;
            b.address.update(cx, |s, cx| s.set_value("", window, cx));
        } else {
            b.tabs.remove(ix);
            if ix < b.active || b.active >= b.tabs.len() {
                b.active = b.active.saturating_sub(1);
            }
        }
    }

    /// ⌘W with the pane focused.
    pub(super) fn browser_close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.right_close_active_tab(window, cx);
    }

    /// Re-arms the idle timer that unloads pages (see [`PARK_AFTER`]).
    pub(super) fn schedule_browser_park(&mut self, cx: &mut Context<Self>) {
        self.browser.idle_since = Some(Instant::now());
        self.browser.park_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(PARK_AFTER + Duration::from_millis(50))
                .await;
            let _ = this.update(cx, |this, cx| this.park_browser_if_idle(cx));
        }));
    }

    /// Drops every tab's native page (and its WebContent process) when the
    /// pane has sat [`PARK_AFTER`] closed with nothing in flight. URLs,
    /// titles, and the address stay, so the header and `navigated` still
    /// read right; [`Self::unpark_tab`] reloads one.
    /// Tabs left [`PARK_AFTER`] without use and out of sight are unloaded
    /// one by one (their scroll position read first, and put back when the
    /// page returns); a tab whose chat is busy, or the one on screen, stays.
    fn park_browser_if_idle(&mut self, cx: &mut Context<Self>) {
        let idle = self
            .browser
            .idle_since
            .is_some_and(|at| at.elapsed() >= PARK_AFTER);
        let mut park = Vec::new();
        for (ix, tab) in self.browser.tabs.iter().enumerate() {
            let showing = self.is_browser_tab_showing(ix);
            if tab.native.is_none() || tab.web_url.is_none() || showing {
                continue;
            }
            let ready = match &tab.agent {
                Some(a) => !a.busy() && a.used.elapsed() >= PARK_AFTER,
                None => idle && !showing,
            };
            if ready {
                park.push(ix);
            }
        }
        for ix in park {
            self.start_park(ix);
        }
        // Agent tabs idle on their own clocks: look again later.
        if self
            .browser
            .tabs
            .iter()
            .any(|t| t.native.is_some() && t.agent.is_some())
        {
            self.schedule_browser_park(cx);
        }
        cx.notify();
    }

    /// An unloaded tab comes back: a fresh native page on its `web_url`.
    /// True when a load started.
    pub(super) fn unpark_tab(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.browser.tabs.get_mut(ix).filter(|t| t.parked) else {
            return false;
        };
        let Some(url) = tab.web_url.clone() else {
            tab.parked = false;
            return false;
        };
        if !self.ensure_native_page(ix, window, cx) {
            return false;
        }
        let sites = self.state.ui_settings.browser_allowed_sites.clone();
        let tab = &mut self.browser.tabs[ix];
        let agent = tab.agent.as_ref().is_some_and(|a| !a.user_takeover);
        tab.parked = false;
        if let Some(native) = &tab.native {
            native.set_background(agent);
            native.set_agent(agent, &sites);
            native.load(&url);
        }
        tracing::info!(target: "keel_ui::browser", url = ?tab.web_url, "idle page reloaded");
        tab.loading(url);
        self.pump_load(ix, window, cx);
        cx.notify();
        true
    }

    fn no_page_error() -> String {
        "no page is loaded in the browser pane; call browser_navigate first".into()
    }

    /// Show a local file (a transcript chip) in the pane, in its own tab
    /// (the one already showing it, if any): `html` is a self-contained
    /// page for it. Opens the pane if it is closed.
    pub(super) fn browser_preview(
        &mut self,
        path: std::path::PathBuf,
        html: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.browser_allowed() {
            return;
        }
        let b = &mut self.browser;
        let active_is_empty = b.tabs.get(b.active).is_some_and(|t| t.is_empty_scratch());
        let ix = match b
            .tabs
            .iter()
            .position(|t| t.preview.as_ref() == Some(&path))
        {
            Some(ix) => ix,
            None if active_is_empty => b.active,
            None => {
                if let Some(ix) = b.tabs.iter().position(|t| t.is_empty_scratch()) {
                    ix
                } else {
                    b.push_tab()
                }
            }
        };
        if !self.ensure_native_page(ix, window, cx) {
            return;
        }
        let bid = self.browser.tabs[ix].id;
        let tab = &mut self.browser.tabs[ix];
        if let Some(native) = &tab.native {
            native.load_html(&html);
        }
        tab.preview = Some(path);
        tab.loading(crate::files::file_url(
            tab.preview.as_ref().expect("set above"),
        ));
        self.pump_load(ix, window, cx);
        self.right_ensure_kind(
            super::right_panel::RightTabKind::Browser(bid),
            true,
            true,
            Some(window),
            cx,
        );
        if !self.right_panel.open {
            self.open_right_panel(window, cx);
        }
        cx.notify();
    }

    /// Tab `ix`'s native page, created on first use. False (with the tab
    /// showing why) when it can't start.
    pub(super) fn ensure_native_page(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let tab = &mut self.browser.tabs[ix];
        if tab.native.is_some() {
            return true;
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        match crate::browser::NativePage::new(window, tx, f32::from(RADIUS_LG) - 1.) {
            Ok(native) => {
                let id = tab.id;
                tab.native = Some(std::rc::Rc::new(native));
                tab._events = Some(cx.spawn_in(window, async move |this, cx| {
                    while let Some(event) = rx.recv().await {
                        if this
                            .update_in(cx, |this, window, cx| {
                                this.on_browser_event(id, event, window, cx)
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
                true
            }
            Err(e) => {
                tab.error = Some(format!("The browser couldn't start: {e}"));
                cx.notify();
                false
            }
        }
    }

    fn browser_tokens(&self, window: &Window) -> Tokens {
        let system_dark = matches!(
            window.appearance(),
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
        );
        self.state.effective_tokens(system_dark)
    }

    /// Push the active tab's `NativePage`, cursor, and guard flag to `viewport` only when they change.
    pub(super) fn sync_browser_viewport(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let tab = self.browser.tab();
        let native = tab.native.clone();
        let guarded = tab.agent_in_control(now);
        let ptr = native
            .as_ref()
            .map_or(0usize, |p| std::rc::Rc::as_ptr(p) as usize);
        let cursor = native
            .as_ref()
            .map_or(gpui::CursorStyle::Arrow, |p| p.cursor_style());
        let key = (ptr, cursor, guarded);
        if self.browser.last_viewport == Some(key) {
            return;
        }
        self.browser.last_viewport = Some(key);
        if let Some(page) = &native {
            let _ = page.update_frame();
        }
        self.browser
            .viewport
            .update(cx, |v, cx| v.set_state(native, cursor, guarded, cx));
    }

    /// Push the active tab's agent control / takeover state to `control_hud` only when it changes.
    fn sync_browser_hud(&mut self, tokens: Tokens, cx: &mut Context<Self>) {
        let now = Instant::now();
        let reduced = self.reduced();
        let tab = self.browser.tab();
        let snapshot = ControlHudSnapshot {
            tokens,
            reduced,
            tab_id: tab.id,
            mode: tab.control_state(now),
            op_label: tab.agent.as_ref().and_then(|a| a.active_op),
            show_page: tab.error.is_none(),
        };
        if self.browser.last_hud.as_ref() == Some(&snapshot) {
            return;
        }
        self.browser.last_hud = Some(snapshot.clone());
        self.browser
            .control_hud
            .update(cx, |v, cx| v.sync(snapshot, cx));
    }

    /// Push the active tab's progress/settle state to `load_bar` only when it changes.
    fn sync_browser_load_bar(&mut self, tokens: Tokens, cx: &mut Context<Self>) {
        let reduced = self.reduced();
        let tab = self.browser.tab();
        let snapshot = LoadBarSnapshot {
            tokens,
            reduced,
            tab_id: tab.id,
            loading: tab.page.loading,
            bar: tab.bar,
            bar_done: tab.bar_done,
            layout_line: tab.layout_line,
        };
        if self.browser.last_load_bar.as_ref() == Some(&snapshot) {
            return;
        }
        self.browser.last_load_bar = Some(snapshot.clone());
        self.browser
            .load_bar
            .update(cx, |v, cx| v.sync(snapshot, cx));
    }

    /// Spin CEF until this tab stops loading. One `DoMessageLoopWork` only
    /// starts the request; without a follow-up the document never commits
    /// and the bar stays up. Agent navigations previously did not do this.
    fn pump_load(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(native) = self.browser.tabs.get(ix).and_then(|t| t.native.clone()) else {
            return;
        };
        self.browser.tabs[ix]._pump = Some(cx.spawn_in(window, async move |this, cx| {
            // About 20s. After that the external timer keeps the loop alive.
            for _ in 0..1200 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                native.spin_soon();
                let keep_going = this
                    .read_with(cx, |this, _| {
                        this.browser
                            .tabs
                            .get(ix)
                            .is_some_and(|t| t.page.loading)
                    })
                    .unwrap_or(false);
                if !keep_going {
                    for _ in 0..30 {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(16))
                            .await;
                        native.spin_soon();
                    }
                    break;
                }
            }
        }));
    }

    /// Load `url` in the shown tab (the user typed it or picked a chip).
    pub(super) fn browser_load(
        &mut self,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browser
            .address
            .update(cx, |s, cx| s.set_value(url.clone(), window, cx));
        self.browser.tab_mut().preview = None;
        let ix = self.browser.active;
        let bid = self.browser.tabs[ix].id;
        if !self.has_right_browser_tab(bid) {
            self.right_ensure_kind(
                super::right_panel::RightTabKind::Browser(bid),
                true,
                true,
                Some(window),
                cx,
            );
        }
        if !self.ensure_native_page(ix, window, cx) {
            return;
        }
        let b = &mut self.browser;
        let tab = b.tab_mut();
        if let Some(native) = &tab.native {
            native.load(&url);
        }
        tab.parked = false;
        tab.loading(url);
        let focus = b.focus.clone();
        self.pump_load(ix, window, cx);
        // Off the field, so the page can take keys once clicked.
        window.focus(&focus, cx);
        cx.notify();
    }

    fn on_browser_event(
        &mut self,
        tab_id: u64,
        event: crate::browser::NativeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::browser::NativeEvent;
        let Some(ix) = self.browser.tab_ix(tab_id) else {
            return;
        };
        let Some(native) = self.browser.tabs[ix].native.clone() else {
            return;
        };
        let is_agent = self.browser.tabs[ix]
            .agent
            .as_ref()
            .is_some_and(|a| !a.user_takeover);
        let shown = ix == self.browser.active;
        // Script results, dialogs, file choosers, console: the agent side.
        let mut event = Some(event);
        self.on_agent_event(ix, &mut event, window, cx);
        let Some(event) = event else {
            return;
        };
        if self.browser.tab_ix(tab_id) != Some(ix) {
            return;
        }
        match event {
            NativeEvent::FrameReady => {
                if shown && native.update_frame() {
                    let ptr = std::rc::Rc::as_ptr(&native) as usize;
                    let cursor = native.cursor_style();
                    let guarded = self.browser.tabs[ix].agent_in_control(Instant::now());
                    self.browser.last_viewport = Some((ptr, cursor, guarded));
                    self.browser
                        .viewport
                        .update(cx, |v, cx| v.notify_frame(native, cursor, guarded, cx));
                }
            }
            NativeEvent::Wake => {
                // The event handler is inside the window borrow. Pumping
                // here lets Chromium re-enter that borrow and abort.
                native.spin_soon();
            }
            NativeEvent::Changed => {
                let mut state = native.state();
                let cursor = native.cursor_style();
                let b = &mut self.browser;
                let address = b.address.clone();
                let tab = &mut b.tabs[ix];
                let prev_header = (
                    tab.page.loading,
                    tab.page.can_back,
                    tab.page.can_forward,
                    tab.page.url.is_some(),
                    tab.label(),
                    tab.preview.is_some(),
                );
                // A file preview reads as its file, not `about:blank`; another
                // page, once loaded, ends the preview.
                let blank = state.url.as_deref().is_none_or(|u| u == "about:blank");
                if let Some(path) = &tab.preview {
                    if blank {
                        state.url = Some(crate::files::file_url(path));
                    } else if !state.loading {
                        tab.preview = None;
                    }
                }
                if state.url.is_none() {
                    state.url = tab.page.url.clone();
                }
                let (_, to, n) = tab.bar;
                if state.loading && !tab.page.loading {
                    tab.bar = (0., state.progress.max(0.08), n + 1);
                    tab.bar_done = false;
                } else if state.loading && state.progress > to {
                    tab.bar = (to, state.progress, n + 1);
                } else if !state.loading && tab.page.loading {
                    tab.bar = (to, 1., n + 1);
                    tab.bar_done = true;
                }
                if shown
                    && !address.read(cx).focus_handle(cx).is_focused(window)
                    && let Some(url) = state.url.clone()
                    && address.read(cx).value() != url.as_str()
                {
                    address.update(cx, |s, cx| s.set_value(url, window, cx));
                }
                if tab.preview.is_none()
                    && let Some(url) = state
                        .url
                        .as_deref()
                        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                {
                    tab.web_url = Some(url.to_string());
                }
                let loaded = !state.loading;
                tab.page = state;
                let next_header = (
                    tab.page.loading,
                    tab.page.can_back,
                    tab.page.can_forward,
                    tab.page.url.is_some(),
                    tab.label(),
                    tab.preview.is_some(),
                );
                let header_changed = prev_header != next_header;
                if loaded {
                    self.agent_page_loaded(ix, window, cx);
                }
                if shown {
                    let ptr = std::rc::Rc::as_ptr(&native) as usize;
                    let guarded = self.browser.tabs[ix].agent_in_control(Instant::now());
                    if self.browser.last_viewport != Some((ptr, cursor, guarded)) {
                        self.browser.last_viewport = Some((ptr, cursor, guarded));
                        self.browser
                            .viewport
                            .update(cx, |v, cx| v.set_state(Some(native), cursor, guarded, cx));
                    }
                    let t = self.browser_tokens(window);
                    self.sync_browser_load_bar(t, cx);
                }
                if header_changed || (loaded && is_agent) {
                    cx.notify();
                }
            }
            // A link that asks for a new window: a new tab, shown.
            NativeEvent::Open(url) => {
                // An agent's page keeps to its own tab: the link opens there.
                if is_agent {
                    native.load(&url);
                    self.browser.tabs[ix].loading(url);
                    self.pump_load(ix, window, cx);
                    return cx.notify();
                }
                let ix = self.browser.push_tab();
                if !self.ensure_native_page(ix, window, cx) {
                    return;
                }
                let bid = self.browser.tabs[ix].id;
                let tab = &mut self.browser.tabs[ix];
                if let Some(native) = &tab.native {
                    native.load(&url);
                }
                tab.loading(url);
                self.pump_load(ix, window, cx);
                self.right_ensure_kind(
                    super::right_panel::RightTabKind::Browser(bid),
                    true,
                    false,
                    Some(window),
                    cx,
                );
            }
            NativeEvent::Key(key) => {
                native.focus_parent();
                window.focus(&self.browser.focus, cx);
                window.defer(cx, move |window, cx| {
                    window.dispatch_keystroke(key, cx);
                });
            }
            NativeEvent::PageSize(raw) => {
                let Some(mut shot) = self.take_shot(ix) else {
                    return;
                };
                match raw.and_then(|body| parse_page_size(&body)) {
                    Ok(full) => {
                        // Frame one at the top, headers and all.
                        shot.full = Some(full);
                        shot.asked_y = 0.;
                        native.request_frame_scroll(0.);
                        self.put_shot(ix, shot);
                    }
                    Err(err) => self.shot_failed(ix, err, window, cx),
                }
            }
            NativeEvent::Scrolled(raw) => {
                let Some(mut shot) = self.take_shot(ix) else {
                    return;
                };
                let y = raw.and_then(|body| {
                    parse_json_object(&body)
                        .and_then(|v| v["y"].as_f64())
                        .ok_or_else(|| format!("the page scroll script returned {body:?}"))
                });
                match y {
                    Ok(y) => {
                        shot.at_y = y;
                        let hide = !shot.frames.is_empty();
                        self.put_shot(ix, shot);
                        let page = std::rc::Rc::downgrade(&native);
                        cx.spawn(async move |_, cx| {
                            cx.background_executor().timer(FRAME_SETTLE).await;
                            cx.update(|_| {
                                if let Some(page) = page.upgrade() {
                                    if hide {
                                        page.hide_fixed();
                                    }
                                    page.request_screenshot(true);
                                }
                            });
                        })
                        .detach();
                    }
                    Err(err) => {
                        // The job restores the page as it fails.
                        self.put_shot(ix, shot);
                        self.shot_failed(ix, err, window, cx);
                    }
                }
            }
            NativeEvent::Screenshot(raw) => {
                let Some(shot) = self.take_shot(ix) else {
                    return;
                };
                if shot.full_page && shot.full.is_none() {
                    // A stray viewport frame before the full page's prep.
                    self.put_shot(ix, shot);
                    return;
                }
                let mut shot = shot;
                let mut raw = Some(raw);
                if let Some(full) = shot.full {
                    match raw.take().expect("raw") {
                        Ok((png, _)) => shot.frames.push((png, shot.at_y)),
                        Err(err) => {
                            self.put_shot(ix, shot);
                            return self.shot_failed(ix, err, window, cx);
                        }
                    }
                    // Next frame a viewport down, until the cap or the bottom
                    // (the page stopped short of the y asked for).
                    let next = shot.at_y + full.vh.max(1.);
                    let bottom = shot.at_y + 0.5 < shot.asked_y;
                    if next < full.h && !bottom && shot.frames.len() < 64 {
                        shot.asked_y = next;
                        native.request_frame_scroll(next);
                        self.put_shot(ix, shot);
                        return;
                    }
                    native.finish_full_page(full.sx, full.sy);
                }
                let page = &self.browser.tabs[ix].page;
                let (url, title, open) = (
                    page.url.clone(),
                    page.title.clone(),
                    self.is_browser_tab_showing(ix),
                );
                let dir = self.state.data_dir.join("screenshots");
                let frames = std::mem::take(&mut shot.frames);
                let full = shot.full;
                cx.spawn(async move |_, cx| {
                    let result = match (raw, full) {
                        (_, Some(full)) => {
                            cx.background_spawn(async move {
                                stitch(&frames, full)
                                    .and_then(|img| shot_image_files(img, &dir, Some(full)))
                            })
                            .await
                        }
                        (Some(Ok((png, width))), None) => {
                            cx.background_spawn(async move { shot_files(&png, width, &dir, None) })
                                .await
                        }
                        (Some(Err(err)), None) => Err(err),
                        (None, None) => Err("no screenshot".into()),
                    };
                    let result = result.map(|files| {
                        serde_json::json!({
                            "url": url,
                            "title": title,
                            "text": files.describe(shot.full),
                            "open": open,
                            "saved_path": files.path.to_string_lossy(),
                            "image_base64": files.agent_base64,
                            "image_mime": files.agent_mime,
                        })
                    });
                    let _ = shot.client.browser_complete(&shot.request_id, result).await;
                })
                .detach();
                self.shot_handed_off(ix, window, cx);
            }
            // Handled on the agent side.
            NativeEvent::Eval(..)
            | NativeEvent::Dialog { .. }
            | NativeEvent::FilePicker(_)
            | NativeEvent::Console(_)
            | NativeEvent::Blocked(_) => {}
        }
    }

    /// ⌘⇧W and the toolbar's power button: tear every tab down (pages,
    /// processes, history, anything playing) and reset to one fresh Browser tab.
    pub(super) fn end_browser_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.release_focus();
        for ix in 0..self.browser.tabs.len() {
            self.drop_agent_tab(ix, "the user ended the browser session", cx);
        }
        self.right_panel
            .tabs
            .retain(|t| !matches!(t.kind, super::right_panel::RightTabKind::Browser(_)));
        let b = &mut self.browser;
        // Dropping a page removes its native view; the frame callbacks hold
        // it weakly, so nothing outlives this.
        b.tabs.clear();
        b.spinners.clear();
        b.active = b.push_tab();
        b.sessions += 1;
        let fresh_id = b.tabs[b.active].id;
        b.address.update(cx, |s, cx| {
            s.set_value("", window, cx);
        });
        if self.right_panel.open {
            self.right_ensure_kind(
                super::right_panel::RightTabKind::Browser(fresh_id),
                true,
                true,
                Some(window),
                cx,
            );
        } else {
            self.normalize_right_tabs();
        }
        cx.notify();
    }

    /// ⌘R and the toolbar button: reload, or stop a load in progress.
    pub(super) fn browser_reload(&mut self, cx: &mut Context<Self>) {
        if let Some(native) = &self.browser.tab().native {
            if self.browser.tab().page.loading {
                native.stop();
            } else {
                native.reload();
            }
        }
        cx.notify();
    }

    /// ⌘[ / ⌘] and the toolbar arrows.
    pub(super) fn browser_history(&mut self, forward: bool) {
        if let Some(native) = &self.browser.tab().native {
            native.history(forward);
        }
    }

    /// The pane's full width for this window: the dragged width or a share of
    /// the window, independent of the left sidebar so toggling the sidebar
    /// never shifts the right panel.
    pub(super) fn browser_width(&self, window: &Window) -> f32 {
        let viewport = f32::from(window.viewport_size().width);
        let max = (viewport - MIN_CHAT).max(MIN_WIDTH);
        // Whole pixels. A fractional width snaps back and forth in the
        // compositor and the edge shivers while you drag.
        self.browser
            .width
            .unwrap_or(viewport * DEFAULT_SHARE)
            .clamp(MIN_WIDTH, max)
            .round()
    }

    /// Dev: set the pane width (None: the default share); returns the old one.
    pub(super) fn dev_set_browser_width(&mut self, width: Option<f32>) -> Option<f32> {
        std::mem::replace(&mut self.browser.width, width)
    }

    /// Grab the pane's left edge. The pointer keeps its offset inside the
    /// handle, and the page bitmap scales until the drag ends.
    pub(super) fn begin_browser_resize(&mut self, x: f32, window: &Window) {
        let viewport = f32::from(window.viewport_size().width);
        let edge = viewport - self.browser_width(window);
        self.browser.resize_grab = x - edge;
        self.browser.resize_from = self.browser.width;
        self.browser.resizing = true;
        if let Some(page) = self.browser.active_native_page() {
            page.set_resizing(true);
        }
    }

    pub(super) fn end_browser_resize(&mut self) {
        if !self.browser.resizing {
            return;
        }
        let changed = self.browser.width != self.browser.resize_from;
        self.browser.resizing = false;
        if let Some(page) = self.browser.active_native_page() {
            page.set_resizing(false);
            // The bitmap was scaled for the drag. Ask for one reflow now,
            // and keep the settle line up until that paint (not during the drag).
            if changed {
                page.arm_layout();
            }
        }
    }

    /// A pane-edge drag: the pane's left edge follows the pointer.
    pub(super) fn drag_browser_edge(&mut self, x: f32, window: &Window) {
        let viewport = f32::from(window.viewport_size().width);
        self.browser.width = Some(viewport - (x - self.browser.resize_grab));
    }

    /// ⌘⇧F and the header expand button: grows/restores the active right panel.
    pub(super) fn toggle_browser_expand(&mut self, cx: &mut Context<Self>) {
        self.toggle_right_panel_expand(cx);
    }

    pub(super) fn browser_expanding(&self) -> bool {
        self.right_panel_expanding()
    }

    /// Browser sub-toolbar (back/forward/reload/address/external/close-session)
    /// and page viewport rendered inside the unified right side panel.
    pub(super) fn render_browser_content(
        &mut self,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_browser_toolbar(t, window, cx))
            .child(self.render_browser_page(t, cx))
            .into_any_element()
    }

    /// The drag handle on the pane's left edge.
    pub(super) fn render_browser_edge(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.browser.resizing;
        div()
            .id("browser-resize-handle")
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(zpx(6.))
            .cursor_col_resize()
            .child(
                div()
                    .w(zpx(2.))
                    .h_full()
                    .when(active, |d| d.bg(c(t.accent)))
                    .when(!active, |d| d.hover(|s| s.bg(c(t.border)))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        let changed = this.browser.width.is_some();
                        this.browser.width = None;
                        if changed && let Some(page) = this.browser.active_native_page() {
                            page.arm_layout();
                        }
                    } else {
                        this.begin_browser_resize(f32::from(event.position.x), window);
                    }
                    cx.notify();
                }),
            )
    }

    fn render_browser_toolbar(
        &mut self,
        t: Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        self.note_layout_line(cx);
        self.sync_browser_load_bar(t, cx);
        let page = &self.browser.tab().page;
        let has_page = page.url.is_some();
        let nav = |id: &'static str, icon: Icon, enabled: bool| {
            icon_button(id, icon, t).when(!enabled, |b| b.opacity(0.35).cursor_default())
        };
        let address_focused = self
            .browser
            .address
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let address = div()
            .flex_1()
            .min_w_0()
            .h(zpx(28.))
            .px_2()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(6.))
            .rounded(RADIUS_MD)
            .bg(c(t.input))
            .border_1()
            .border_color(c(if address_focused { t.focus } else { t.border }))
            .t_small()
            .child(
                self.right_tip_target(
                    div()
                        .id("browser-address-globe")
                        .flex_none()
                        .text_color(c(t.text_faint))
                        .child(Icon::Globe.el(zpx(12.))),
                    "browser-address-globe",
                    cx,
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(self.browser.address.clone()),
            );
        div()
            .relative()
            .h(HEADER_HEIGHT)
            .flex_none()
            .px(px(PAGE_INSET))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(self.right_tip_target(
                nav("browser-back", Icon::Back, page.can_back).on_click(
                    cx.listener(|this, _: &ClickEvent, _, _| this.browser_history(false)),
                ),
                "browser-back",
                cx,
            ))
            .child(
                self.right_tip_target(
                    nav("browser-forward", Icon::Forward, page.can_forward).on_click(
                        cx.listener(|this, _: &ClickEvent, _, _| this.browser_history(true)),
                    ),
                    "browser-forward",
                    cx,
                ),
            )
            .child(
                self.right_tip_target(
                    nav(
                        "browser-reload",
                        if page.loading {
                            Icon::Close
                        } else {
                            Icon::Reload
                        },
                        has_page,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.browser_reload(cx))),
                    "browser-reload",
                    cx,
                ),
            )
            .child(address)
            .child(self.right_tip_target(
                nav("browser-external", Icon::External, has_page).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        if let Some(path) = this.browser.tab().preview.clone() {
                            cx.open_with_system(&path);
                        } else if let Some(url) = this.browser.tab().page.url.clone() {
                            cx.open_url(&url);
                        }
                    },
                )),
                "browser-external",
                cx,
            ))
            .child(
                self.right_tip_target(
                    div()
                        .id("browser-end")
                        .size(zpx(26.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(RADIUS_MD)
                        .text_color(c(t.text_muted))
                        .when(!has_page, |b| b.opacity(0.35).cursor_default())
                        .when(has_page, |b| {
                            b.cursor_pointer()
                                .hover(|s| s.text_color(c(t.danger)))
                                .hover_fade(c(t.hover), None)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.end_browser_session(window, cx)
                                }))
                        })
                        .child(Icon::Close.el(zpx(15.))),
                    "browser-end",
                    cx,
                ),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(2.))
                    .child(self.browser.load_bar.clone()),
            )
    }

    fn page_geometry_pending(&self) -> bool {
        let tab = self.browser.tab();
        let has_page = tab.page.url.is_some() && tab.error.is_none() && tab.native.is_some();
        let loading = tab.page.loading || tab.bar_done;
        let paint_due = tab
            .native
            .as_ref()
            .is_some_and(|page| page.layout_pending());
        geometry_line(has_page, loading, paint_due)
    }

    /// Arm or retire the settle line. The fade-out is one [`FAST`] pass;
    /// a new wait during it starts a fresh line.
    fn note_layout_line(&mut self, cx: &mut Context<Self>) {
        let pending = self.page_geometry_pending();
        let reduced = self.reduced();
        let tab_id = self.browser.tab().id;
        let line = &mut self.browser.tab_mut().layout_line;
        if pending {
            let restart = line.as_ref().is_none_or(|(_, leave)| leave.is_some());
            if restart {
                let n = line.map(|(n, _)| n.saturating_add(1)).unwrap_or(1);
                *line = Some((n, None));
            }
            return;
        }
        let Some((n, None)) = *line else {
            return;
        };
        // Reduced motion keeps the mark and drops the fade: the line goes
        // when the paint does.
        if reduced {
            *line = None;
            return;
        }
        *line = Some((n, Some(Instant::now())));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FAST).await;
            let _ = this.update(cx, |this, cx| {
                let Some(tab) = this.browser.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
                    return;
                };
                if tab
                    .layout_line
                    .is_some_and(|(generation, leave)| generation == n && leave.is_some())
                {
                    tab.layout_line = None;
                    if this.browser.tab().id == tab_id {
                        let t = this
                            .browser
                            .last_load_bar
                            .as_ref()
                            .map(|b| b.tokens)
                            .unwrap_or_else(|| this.state.effective_tokens(true));
                        this.sync_browser_load_bar(t, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// What each browser toolbar button does, as its tooltip says it (state-aware),
    /// with its shortcut where it has one.
    pub(super) fn browser_tip_label(
        &self,
        id: &str,
    ) -> Option<(String, Option<gpui::SharedString>)> {
        let key_for = |shortcut_id: &str| {
            crate::keys::display_for_tooltip(
                &self.state.ui_settings.shortcuts,
                shortcut_id,
                crate::keys::current_os(),
            )
            .map(gpui::SharedString::from)
        };
        let tab = self.browser.tab();
        let (text, key) = match id {
            "browser-back" => ("Back", key_for("browser_back")),
            "browser-forward" => ("Forward", key_for("browser_forward")),
            // The same button and Reload shortcut stop a load in progress.
            "browser-reload" if tab.page.loading => ("Stop loading", key_for("browser_reload")),
            "browser-reload" => ("Reload", key_for("browser_reload")),
            "browser-address-globe" => ("Search or enter address", key_for("browser_address")),
            // A previewed file opens in its own app; a page in the default browser.
            "browser-external" if tab.preview.is_some() => ("Open in default app", None),
            "browser-external" => ("Open in browser", None),
            // Closes every browser tab (and the agent's) for a fresh, empty one.
            "browser-end" => ("Close session", key_for("browser_end_session")),
            _ => return None,
        };
        Some((text.to_string(), key))
    }

    /// The page card: the live page, or a start screen until there's an address.
    fn render_browser_page(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let agent_controlled = self.browser.tab().agent_in_control(now);
        self.sync_browser_hud(t, cx);
        let card = div()
            .id("browser-page")
            .relative()
            .flex_1()
            .min_h_0()
            .mx(px(PAGE_INSET))
            .mb(px(PAGE_INSET))
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(if agent_controlled {
                c(t.info).opacity(0.45)
            } else {
                c(t.border)
            })
            .overflow_hidden()
            .bg(c(t.floating));
        if let Some(error) = self.browser.tab().error.clone() {
            return card
                .child(self.render_browser_empty(t, Some(error), cx))
                .child(self.browser.control_hud.clone());
        }
        if self.browser.tab().page.url.is_none() {
            return card
                .child(self.render_browser_empty(t, None, cx))
                .child(self.browser.control_hud.clone());
        }
        if self.browser.tab().native.is_some() {
            self.sync_browser_viewport(cx);
            card.child(self.browser.viewport.clone())
                .child(self.browser.control_hud.clone())
        } else {
            card.child(self.browser.control_hud.clone())
        }
    }

    fn render_browser_empty(
        &mut self,
        t: Tokens,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chips = QUICK.iter().enumerate().map(|(i, addr)| {
            div()
                .id(("browser-quick", i))
                .h(zpx(26.))
                .px_3()
                .flex()
                .items_center()
                .rounded_full()
                .border_1()
                .border_color(c(t.border).opacity(0.65))
                .bg(c(t.surface).opacity(0.75))
                .t_small()
                .font_family(crate::style::MONO)
                .text_color(c(t.text_muted))
                .cursor_pointer()
                .hover(|s| s.text_color(c(t.text)).border_color(c(t.border)))
                .hover_fade(c(t.surface), None)
                .active(|s| s.bg(c(t.selected)).opacity(0.9))
                .child(*addr)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if let Some(url) = normalize_address(addr) {
                        this.browser_load(url, window, cx);
                    }
                }))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .text_color(c(if error.is_some() {
                        t.warning
                    } else {
                        t.text_faint
                    }))
                    .child(
                        if error.is_some() {
                            Icon::Alert
                        } else {
                            Icon::Globe
                        }
                        .el(zpx(26.)),
                    ),
            )
            .child(
                div()
                    .max_w(zpx(280.))
                    .text_center()
                    .t_body()
                    .text_color(c(t.text_muted))
                    .child(error.unwrap_or_else(|| {
                        "Open a page, or the app your agent is building.".into()
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .justify_center()
                    .gap_2()
                    .children(chips),
            )
    }
}

/// Given the start fraction, target, and elapsed vs total duration, compute the interpolated progress along `glide_curve`.
pub(super) fn expand_progress(from: f32, target: f32, elapsed: Duration, dur: Duration) -> f32 {
    if dur.is_zero() || elapsed >= dur {
        return target;
    }
    let t = (elapsed.as_secs_f32() / dur.as_secs_f32()).min(1.);
    from + (target - from) * crate::motion::glide_curve(t)
}

/// Compute the duration for expanding/collapsing based on distance traveled.
pub(super) fn expand_duration(from: f32, target: f32) -> Duration {
    let dist = (target - from).abs();
    crate::motion::SPATIAL.mul_f32(0.4 + 0.6 * dist)
}

/// [`crate::browser::cef`]'s page-size JSON → the capture, its height
/// capped at [`FULL_PAGE_MAX`].
fn parse_page_size(body: &str) -> Result<FullPage, String> {
    let value =
        parse_json_object(body).ok_or_else(|| format!("the page size script returned {body:?}"))?;
    let num = |k: &str| value[k].as_f64().filter(|n| n.is_finite()).unwrap_or(0.);
    let (w, total) = (num("w").round(), num("h").round());
    if w < 1. || total < 1. {
        return Err(format!("the page has no size ({w}x{total})"));
    }
    let vh = num("vh").round().max(1.);
    Ok(FullPage {
        w,
        h: total.min(FULL_PAGE_MAX),
        total,
        vh,
        sx: num("sx"),
        sy: num("sy"),
    })
}

/// A script's result: a JSON object, possibly JSON-encoded as a string.
fn parse_json_object(body: &str) -> Option<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| match v {
            serde_json::Value::String(s) => serde_json::from_str(&s).ok(),
            other => Some(other),
        })
        .filter(serde_json::Value::is_object)
}

/// Full-page frames (PNG at their CSS y) → one image `full.w` wide and
/// `full.h` tall (less when the page is shorter); later frames win overlaps.
fn stitch(frames: &[(Vec<u8>, f64)], full: FullPage) -> Result<image::DynamicImage, String> {
    let width = (full.w.round() as u32).max(1);
    let mut placed: Vec<(image::RgbaImage, u32)> = Vec::new();
    let mut bottom = 0u32;
    for (png, y) in frames {
        let mut img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        if img.width() != width {
            let h = ((u64::from(img.height()) * u64::from(width)) / u64::from(img.width().max(1)))
                .max(1) as u32;
            img = img.resize_exact(width, h, image::imageops::FilterType::Triangle);
        }
        let y = y.max(0.).round() as u32;
        bottom = bottom.max(y + img.height());
        placed.push((img.to_rgba8(), y));
    }
    let height = bottom.min((full.h.round() as u32).max(1));
    if placed.is_empty() || height == 0 {
        return Err("the page gave no frames".into());
    }
    let mut canvas = image::RgbaImage::new(width, height);
    for (img, y) in &placed {
        image::imageops::replace(&mut canvas, img, 0, i64::from(*y));
    }
    Ok(image::DynamicImage::ImageRgba8(canvas))
}

/// A screenshot on disk (full size, PNG) and the agent's copy.
struct ShotFiles {
    path: std::path::PathBuf,
    width: u32,
    height: u32,
    agent_base64: String,
    agent_mime: &'static str,
    agent_size: (u32, u32),
}

impl ShotFiles {
    fn describe(&self, full: Option<FullPage>) -> String {
        let copy = if self.agent_size == (self.width, self.height) {
            String::new()
        } else {
            format!(
                "; you get a {}x{} {} copy",
                self.agent_size.0,
                self.agent_size.1,
                if self.agent_mime == "image/jpeg" {
                    "JPEG"
                } else {
                    "PNG"
                }
            )
        };
        match full {
            Some(f) if f.total > f.h => format!(
                "{}x{} PNG of the full page, cut at {} of its {} CSS px height{copy}",
                self.width, self.height, f.h as u64, f.total as u64
            ),
            Some(_) => format!(
                "{}x{} PNG of the full page (1 px = 1 CSS px){copy}",
                self.width, self.height
            ),
            None => format!(
                "{}x{} PNG of the visible page (1 px = 1 CSS px when width matches the viewport){copy}",
                self.width, self.height
            ),
        }
    }
}

/// Largest agent image, base64 bytes: it rides in a 1 MiB RPC frame.
const AGENT_IMAGE_MAX: usize = 700_000;
/// Agent images stay under this many pixels on a side (model limits).
const AGENT_SIDE_MAX: u32 = 7_900;

/// Retina PNG → a 1 CSS px per pixel PNG under `dir` (cut to the capture's
/// height), plus the agent's copy: the PNG when small enough, else a
/// downscaled JPEG.
fn shot_files(
    png: &[u8],
    width: u32,
    dir: &std::path::Path,
    full: Option<FullPage>,
) -> Result<ShotFiles, String> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    shot_image_files_sized(img, width, dir, full)
}

/// A stitched full page (already 1 px per CSS px) → [`ShotFiles`].
fn shot_image_files(
    img: image::DynamicImage,
    dir: &std::path::Path,
    full: Option<FullPage>,
) -> Result<ShotFiles, String> {
    let width = img.width();
    shot_image_files_sized(img, width, dir, full)
}

fn shot_image_files_sized(
    mut img: image::DynamicImage,
    width: u32,
    dir: &std::path::Path,
    full: Option<FullPage>,
) -> Result<ShotFiles, String> {
    use base64::Engine as _;
    let target = width.clamp(
        1,
        if full.is_some() {
            u32::MAX
        } else {
            MAX_SHOT_WIDTH
        },
    );
    if img.width() > target {
        let height =
            ((u64::from(img.height()) * u64::from(target)) / u64::from(img.width())).max(1) as u32;
        img = img.resize_exact(target, height, image::imageops::FilterType::Triangle);
    }
    if let Some(f) = full {
        let cap = (f.h.round() as u32).max(1);
        if img.height() > cap {
            img = img.crop_imm(0, 0, img.width(), cap);
        }
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    img.write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let bytes = bytes.into_inner();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = dir.join(format!(
        "{stamp}{}.png",
        if full.is_some() { "-full" } else { "" }
    ));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let (width, height) = (img.width(), img.height());
    if width.max(height) <= AGENT_SIDE_MAX && bytes.len() * 4 / 3 + 4 <= AGENT_IMAGE_MAX {
        return Ok(ShotFiles {
            path,
            width,
            height,
            agent_base64: b64.encode(&bytes),
            agent_mime: "image/png",
            agent_size: (width, height),
        });
    }
    let mut scale = (f64::from(AGENT_SIDE_MAX) / f64::from(width.max(height))).min(1.);
    loop {
        let (w, h) = (
            ((f64::from(width) * scale).round() as u32).max(1),
            ((f64::from(height) * scale).round() as u32).max(1),
        );
        let small = if (w, h) == (width, height) {
            img.to_rgb8()
        } else {
            img.resize_exact(w, h, image::imageops::FilterType::Triangle)
                .to_rgb8()
        };
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 72)
            .encode_image(&small)
            .map_err(|e| e.to_string())?;
        let data = b64.encode(&jpeg);
        if data.len() <= AGENT_IMAGE_MAX || w <= 160 {
            return Ok(ShotFiles {
                path,
                width,
                height,
                agent_base64: data,
                agent_mime: "image/jpeg",
                agent_size: (w, h),
            });
        }
        scale *= 0.75;
    }
}

/// The settle line is up while a loaded page's bitmap is still the previous
/// size. A real load already has the toolbar's progress bar.
pub(super) fn geometry_line(has_page: bool, loading: bool, paint_due: bool) -> bool {
    has_page && !loading && paint_due
}

/// Widest screenshot sent to an agent, in pixels.
const MAX_SHOT_WIDTH: u32 = 1600;

#[cfg(test)]
mod host_tests {
    use super::*;
    use crate::motion::SPATIAL;

    fn png(w: u32, h: u32, noisy: bool) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            if noisy {
                let v = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503))
                    .wrapping_mul(2_246_822_519);
                image::Rgb([(v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8])
            } else {
                image::Rgb([200, 210, 220])
            }
        });
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn tmp() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "keel-shot-files-{}-{}",
            std::process::id(),
            std::time::Instant::now().elapsed().as_nanos()
        ))
    }

    #[test]
    fn screenshots_shrink_to_the_page_width_and_save() {
        let dir = tmp();
        let files = shot_files(&png(200, 100, false), 100, &dir, None).unwrap();
        assert_eq!((files.width, files.height), (100, 50));
        assert_eq!(files.agent_mime, "image/png");
        assert!(files.path.starts_with(&dir) && files.path.is_file());
        let saved = image::open(&files.path).unwrap();
        assert_eq!((saved.width(), saved.height()), (100, 50));
        assert!(
            files
                .describe(None)
                .starts_with("100x50 PNG of the visible page")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A tall full page: saved at full size (cut to the cap), the agent's
    /// copy a JPEG small enough for an RPC frame and a model.
    #[test]
    fn full_page_saves_png_and_sends_a_small_jpeg() {
        let dir = tmp();
        let full = parse_page_size(r#""{\"w\":400,\"h\":20000,\"vh\":800,\"sx\":0,\"sy\":1200}""#)
            .unwrap();
        assert_eq!(
            full,
            FullPage {
                w: 400.,
                h: FULL_PAGE_MAX,
                total: 20000.,
                vh: 800.,
                sx: 0.,
                sy: 1200.
            }
        );
        // Retina: twice the CSS width, a little taller than the cap.
        let files = shot_files(
            &png(800, 2 * 9000, true),
            400,
            &dir,
            Some(FullPage { h: 8000., ..full }),
        )
        .unwrap();
        assert_eq!((files.width, files.height), (400, 8000));
        assert_eq!(files.agent_mime, "image/jpeg");
        assert!(files.agent_base64.len() <= AGENT_IMAGE_MAX);
        assert!(files.agent_size.1 <= AGENT_SIDE_MAX);
        let saved = image::open(&files.path).unwrap();
        assert_eq!((saved.width(), saved.height()), (400, 8000));
        assert!(
            files
                .describe(Some(FullPage { h: 8000., ..full }))
                .contains("cut at 8000 of its 20000"),
            "{}",
            files.describe(Some(full))
        );
        assert!(parse_page_size("null").is_err());
        assert!(parse_page_size(r#"{"w":0,"h":10}"#).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Frames a viewport apart, the last clamped at the bottom, become one
    /// image: each frame's pixels land at its scroll offset.
    #[test]
    fn frames_stitch_at_their_offsets() {
        let frame = |shade: u8| {
            let img = image::RgbImage::from_pixel(100, 40, image::Rgb([shade, shade, shade]));
            let mut out = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb8(img)
                .write_to(&mut out, image::ImageFormat::Png)
                .unwrap();
            out.into_inner()
        };
        let full = FullPage {
            w: 100.,
            h: 100.,
            total: 100.,
            vh: 40.,
            sx: 0.,
            sy: 0.,
        };
        // 0, 40, then the bottom clamps the third frame to 60.
        let img = stitch(&[(frame(10), 0.), (frame(20), 40.), (frame(30), 60.)], full)
            .unwrap()
            .to_rgba8();
        assert_eq!(img.dimensions(), (100, 100));
        assert_eq!(img.get_pixel(5, 5)[0], 10);
        assert_eq!(img.get_pixel(5, 45)[0], 20);
        assert_eq!(img.get_pixel(5, 65)[0], 30);
        assert_eq!(img.get_pixel(5, 99)[0], 30);
        // Retina frames scale to the CSS width; a cap cuts the height.
        let img = stitch(
            &[(frame(10), 0.)],
            FullPage {
                w: 50.,
                h: 15.,
                ..full
            },
        )
        .unwrap();
        assert_eq!((img.width(), img.height()), (50, 15));
        assert!(stitch(&[], full).is_err());
    }

    #[test]
    fn tabs_are_named_by_title_then_file_then_host() {
        let mut tab = BrowserTab::new(1);
        assert_eq!(tab.label(), "New Tab");
        tab.page.url = Some("http://localhost:3000/a?b".into());
        assert_eq!(tab.label(), "localhost:3000");
        tab.preview = Some("/tmp/report.html".into());
        assert_eq!(tab.label(), "report.html");
        tab.page.title = "  Report  ".into();
        assert_eq!(tab.label(), "Report");
    }

    #[test]
    fn settle_line_waits_on_a_loaded_page_only() {
        assert!(!geometry_line(false, false, true));
        assert!(!geometry_line(true, true, true));
        assert!(!geometry_line(true, false, false));
        assert!(geometry_line(true, false, true));
    }

    #[test]
    fn expand_duration_scales_with_travel_distance() {
        assert_eq!(expand_duration(0., 1.), SPATIAL);
        assert_eq!(expand_duration(1., 0.), SPATIAL);
        let partial = expand_duration(0.5, 0.);
        assert!(partial < SPATIAL);
        assert!((partial.as_secs_f32() - SPATIAL.mul_f32(0.7).as_secs_f32()).abs() < 1e-4);
        assert!(
            (expand_duration(0.5, 0.5).as_secs_f32() - SPATIAL.mul_f32(0.4).as_secs_f32()).abs()
                < 1e-4
        );
    }

    #[test]
    fn expand_progress_starts_smoothly_without_jumps() {
        let dur = SPATIAL;
        // Expanding from 0 to 1:
        assert_eq!(expand_progress(0., 1., Duration::ZERO, dur), 0.);
        assert_eq!(expand_progress(0., 1., dur, dur), 1.);
        assert_eq!(
            expand_progress(0., 1., dur + Duration::from_millis(50), dur),
            1.
        );

        // Mid-flight reversal from 0.4 back to 0.0 starts EXACTLY at 0.4 (no sudden jump):
        assert_eq!(expand_progress(0.4, 0., Duration::ZERO, dur), 0.4);
        assert_eq!(expand_progress(0.4, 0., dur, dur), 0.);

        // Monotonic motion during reversal:
        let p1 = expand_progress(0.4, 0., Duration::from_millis(100), dur);
        let p2 = expand_progress(0.4, 0., Duration::from_millis(200), dur);
        assert!(p1 < 0.4 && p1 > p2 && p2 > 0.);
    }

    #[test]
    fn hud_nudge_lifts_smoothly_and_respects_reduced_motion() {
        assert_eq!(hud_nudge_frame(HUD_NUDGE_DUR, false), (0., 0.));

        let (dy, hi) = hud_nudge_frame(Duration::from_millis(80), false);
        assert!(dy < 0., "expected upward lift, got {dy}");
        assert!(hi > 0. && hi < 1., "expected decaying highlight, got {hi}");

        let (reduced_dy, reduced_hi) = hud_nudge_frame(Duration::from_millis(80), true);
        assert_eq!(reduced_dy, 0.);
        assert!((reduced_hi - hi).abs() < 1e-5);
    }

    #[test]
    fn hud_presence_enters_exits_reverses_and_respects_reduced_motion() {
        let t0 = Instant::now();
        let mut p = HudPresence::hidden();
        assert!(!p.mounted(t0));

        // Entry starts from HUD_ENTER_DY with front-loaded glide and settles to (1, 0).
        p.set(true, t0, false);
        assert!(p.mounted(t0));
        assert_eq!(p.look_at(t0), (0., HUD_ENTER_DY));
        let mid_in = t0 + Duration::from_millis(90);
        let (op_mid, dy_mid) = p.look_at(mid_in);
        assert!(op_mid > 0.6 && op_mid < 1., "expected front-loaded fade, got {op_mid}");
        assert!(dy_mid > 0. && dy_mid < HUD_ENTER_DY * 0.5, "expected fast settle, got {dy_mid}");
        assert_eq!(p.look_at(t0 + HUD_ENTER), (1., 0.));
        assert!(!p.animating(t0 + HUD_ENTER));

        // Interrupting entry mid-flight reverses smoothly from current live (opacity, dy).
        let mut rev = HudPresence::hidden();
        rev.set(true, t0, false);
        let at_rev = t0 + Duration::from_millis(80);
        let live = rev.look_at(at_rev);
        rev.set(false, at_rev, false);
        assert!(rev.leaving(at_rev));
        assert_eq!(rev.look_at(at_rev), live);
        assert_eq!(rev.look_at(at_rev + HUD_EXIT), (0., HUD_EXIT_DY));

        // Reduced motion removes all movement (dy == 0) while preserving opacity fades.
        let mut r = HudPresence::hidden();
        r.set(true, t0, true);
        assert_eq!(r.look_at(t0), (0., 0.));
        let (r_op, r_dy) = r.look_at(t0 + Duration::from_millis(60));
        assert!(r_op > 0. && r_op < 1.);
        assert_eq!(r_dy, 0.);
        assert_eq!(r.look_at(t0 + HUD_REDUCED_ENTER), (1., 0.));
    }
}

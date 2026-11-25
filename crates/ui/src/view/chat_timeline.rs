//! Right-edge chat timeline rail: hairline turn ticks and a bottom "Latest"
//! anchor with smooth enter/exit and scroll-tracking motion.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Element, ElementId, GlobalElementId, Hitbox,
    HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Render, Window, div, point, prelude::*, px, size,
};
use keel_proto::ChatStatus;
use keel_theme::Tokens;

use super::KeelWindow;
use crate::hover_fade::Fade;
use crate::motion::{ease_out_strong, elapsed_label};
use crate::style::{HEADER_HEIGHT, KeelStyled, RADIUS_LG, c, zpx};

const RAIL_WIDTH: f32 = 34.0;
const RAIL_RIGHT_INSET: f32 = 12.0;
const RAIL_SLIDE_PX: f32 = 8.0;
const RAIL_PAD_Y: f32 = 6.0;
const PREVIEW_WIDTH: f32 = 248.0;
const PREVIEW_GAP: f32 = 10.0;
const PREVIEW_GLIDE_MS: u64 = 85;
const TICK_MIN_PITCH: f32 = 7.0;
const TICK_MAX_PITCH: f32 = 11.0;

#[derive(Clone)]
struct TimelinePoint {
    row_ix: usize,
    preview: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct TimelineSyncKey {
    active_chat: bool,
    points_ptr: usize,
    items_gen: u64,
    transcript_empty: bool,
    running_since: Option<Instant>,
    t: Tokens,
    reduced: bool,
}

/// Independent overlay view for the right-edge chat timeline rail so its
/// scroll-wake and hover animations never trigger `owner_animating` on the
/// transcript's `gpui::list` layer or rebuild `KeelWindow`.
pub(super) struct ChatTimelineView {
    active_chat: bool,
    points: Rc<Vec<(usize, String)>>,
    timeline_points: Rc<Vec<TimelinePoint>>,
    transcript_empty: bool,
    running_since: Option<Instant>,
    t: Tokens,
    reduced: bool,
    presence: crate::motion::Presence,
    list_state: gpui::ListState,
    stick_flag: Rc<Cell<bool>>,
    dock_h: Rc<Cell<f32>>,
    scroll_until: Rc<Cell<Option<Instant>>>,
    on_jump_row: Rc<dyn Fn(usize, &mut Window, &mut gpui::App)>,
    on_jump_latest: Rc<dyn Fn(&mut Window, &mut gpui::App)>,
}

impl ChatTimelineView {
    pub(super) fn new(
        t: Tokens,
        reduced: bool,
        list_state: gpui::ListState,
        stick_flag: Rc<Cell<bool>>,
        dock_h: Rc<Cell<f32>>,
        scroll_until: Rc<Cell<Option<Instant>>>,
        on_jump_row: Rc<dyn Fn(usize, &mut Window, &mut gpui::App)>,
        on_jump_latest: Rc<dyn Fn(&mut Window, &mut gpui::App)>,
    ) -> Self {
        Self {
            active_chat: false,
            points: Rc::default(),
            timeline_points: Rc::default(),
            transcript_empty: true,
            running_since: None,
            t,
            reduced,
            presence: crate::motion::Presence::hidden().with_durations(
                Duration::from_millis(180),
                Duration::from_millis(140),
            ),
            list_state,
            stick_flag,
            dock_h,
            scroll_until,
            on_jump_row,
            on_jump_latest,
        }
    }

    pub(super) fn cut(&mut self) {
        self.presence.cut();
    }

    pub(super) fn shown(&self) -> bool {
        self.presence.shown()
    }

    fn sync(
        &mut self,
        active_chat: bool,
        points: Rc<Vec<(usize, String)>>,
        transcript_empty: bool,
        running_since: Option<Instant>,
        t: Tokens,
        reduced: bool,
    ) {
        if !Rc::ptr_eq(&self.points, &points) {
            self.timeline_points = Rc::new(
                points
                    .iter()
                    .map(|(row_ix, preview)| TimelinePoint {
                        row_ix: *row_ix,
                        preview: preview.clone(),
                    })
                    .collect(),
            );
            self.points = points;
        }
        self.active_chat = active_chat;
        self.transcript_empty = transcript_empty;
        self.running_since = running_since;
        self.t = t;
        self.reduced = reduced;
    }

    fn is_at_bottom(&self) -> bool {
        if self.transcript_empty || self.points.is_empty() {
            return true;
        }
        let max = self.list_state.max_offset_for_scrollbar().y;
        if max <= zpx(0.) {
            return true;
        }
        let current = -self.list_state.scroll_px_offset_for_scrollbar().y;
        self.stick_flag.get()
            || (max - current) <= crate::transcript::TranscriptState::RE_ENGAGE_THRESHOLD
    }
}

impl Render for ChatTimelineView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let host = div().absolute().top_0().right_0().bottom_0().w(px(0.));
        let now = Instant::now();
        let at_bottom = self.is_at_bottom();
        if !self.active_chat || self.points.is_empty() {
            self.presence.cut();
            return host;
        }

        let want_rail = self.points.len() >= 2 || !at_bottom;
        self.presence.set(want_rail, now, self.reduced);
        if self.presence.animating(now) {
            window.request_animation_frame();
        }
        if !self.presence.mounted(now) {
            return host;
        }
        let look = self.presence.look_at(now);
        let leaving = self.presence.leaving(now);

        let top_ix = self.list_state.logical_scroll_top().item_ix;
        let active_ix = active_timeline_index(&self.timeline_points, top_ix, at_bottom);

        let viewport_h = f32::from(window.viewport_size().height);
        let safe_top = f32::from(HEADER_HEIGHT) + 16.0;
        let safe_bottom = self.dock_h.get().max(88.0) + 16.0;
        let avail_h = (viewport_h - safe_top - safe_bottom).max(60.0);
        let rail_pad_y = f32::from(zpx(RAIL_PAD_Y)).round();
        let total_items = self.timeline_points.len() + 1;
        let ticks_avail = (avail_h - 2.0 * rail_pad_y).max(20.0);
        let pitch = (compute_pitch(ticks_avail, total_items) * crate::style::zoom())
            .min(ticks_avail / (total_items as f32))
            .max(4.0)
            .floor();
        let rail_h = 2.0 * rail_pad_y + pitch * total_items as f32;
        let rail_top = (safe_top + ((avail_h - rail_h) * 0.5).max(0.0)).round();

        host.child(ChatTimelineRail {
            t: self.t,
            points: self.timeline_points.clone(),
            active_ix,
            at_bottom,
            running_since: self.running_since,
            rail_top,
            rail_h,
            rail_pad_y,
            pitch,
            presence_opacity: look.opacity,
            leaving,
            scroll_until: self.scroll_until.get(),
            reduced: self.reduced,
            on_jump_row: self.on_jump_row.clone(),
            on_jump_latest: self.on_jump_latest.clone(),
        })
    }
}

impl KeelWindow {
    /// Syncs non-scroll timeline inputs into [`ChatTimelineView`] only when they change.
    pub(super) fn sync_chat_timeline(&mut self, t: Tokens, cx: &mut Context<Self>) {
        let active_chat = self.current_chat.is_some()
            && !(self.right_panel_expanded() && self.right_panel_open())
            && self.subagent_open.is_none()
            && self.chat_reveal.is_none();
        let points = self.transcript.user_points_rc();
        let transcript_empty = self.transcript.items.is_empty();
        let running_since =
            (self.active_status() == Some(ChatStatus::Running)).then_some(self.loader_seed.0);
        let reduced = self.reduced();
        let key = TimelineSyncKey {
            active_chat,
            points_ptr: Rc::as_ptr(&points) as usize,
            items_gen: self.transcript.items_gen(),
            transcript_empty,
            running_since,
            t,
            reduced,
        };
        if self.timeline_sync_key == Some(key) {
            return;
        }
        self.timeline_sync_key = Some(key);
        self.timeline_view.update(cx, |view, _| {
            view.sync(
                active_chat,
                points,
                transcript_empty,
                running_since,
                t,
                reduced,
            );
        });
    }
}

fn compute_pitch(avail_h: f32, count: usize) -> f32 {
    if count == 0 {
        return TICK_MAX_PITCH;
    }
    let raw = avail_h / count as f32;
    if raw >= TICK_MAX_PITCH {
        TICK_MAX_PITCH
    } else if raw >= TICK_MIN_PITCH {
        raw
    } else {
        raw.max(4.0)
    }
}

fn active_timeline_index(points: &[TimelinePoint], top_ix: usize, at_bottom: bool) -> usize {
    if at_bottom {
        return points.len();
    }
    points
        .iter()
        .rposition(|p| p.row_ix <= top_ix + 1)
        .unwrap_or(0)
}

fn latest_tick_style(
    t: Tokens,
    active: f32,
    hover: f32,
    neighbor: f32,
    emphasis: f32,
    running: bool,
    pressed: bool,
) -> (f32, gpui::Hsla) {
    // Reaching the bottom only changes color. Using the current fade value
    // for both colors keeps the first frame continuous when it retargets.
    let width = 14.0 + 6.0 * hover.max(neighbor) - if pressed { 1.5 } else { 0.0 };
    let base_alpha = (0.55 + 0.23 * active + emphasis).min(1.0);
    let alpha = base_alpha + (0.95_f32.max(base_alpha) - base_alpha) * hover;
    let color = if pressed {
        c(t.accent)
    } else if running {
        c(t.text)
            .blend(c(t.accent).opacity(1.0 - active))
            .opacity(1.0 + (alpha - 1.0) * active)
    } else {
        c(t.text).opacity(alpha)
    };
    (width, color)
}

struct TimelineState {
    rail_fade: Fade,
    scroll_fade: Fade,
    tick_fades: Vec<Fade>,
    active_fades: Vec<Fade>,
    hovered_ix: Option<usize>,
    shown_ix: Option<usize>,
    pressed_ix: Option<usize>,
    /// Frozen snapshot while `leaving` is true so ticks never snap/resize mid-exit.
    exit_snapshot: Option<(usize, bool)>,
    preview_y_from: f32,
    preview_y_to: f32,
    preview_y_at: Option<Instant>,
}

impl Default for TimelineState {
    fn default() -> Self {
        Self {
            rail_fade: Fade::new_with(
                false,
                Duration::from_millis(130),
                Duration::from_millis(90),
                true,
            ),
            scroll_fade: Fade::new_with(
                false,
                Duration::from_millis(110),
                Duration::from_millis(420),
                false,
            ),
            tick_fades: Vec::new(),
            active_fades: Vec::new(),
            hovered_ix: None,
            shown_ix: None,
            pressed_ix: None,
            exit_snapshot: None,
            preview_y_from: 0.0,
            preview_y_to: 0.0,
            preview_y_at: None,
        }
    }
}

impl TimelineState {
    fn ensure_ticks(&mut self, count: usize, active_ix: usize) {
        if self.tick_fades.len() != count {
            self.tick_fades.resize(
                count,
                Fade::new_with(
                    false,
                    Duration::from_millis(95),
                    Duration::from_millis(80),
                    true,
                ),
            );
        }
        if self.active_fades.len() != count {
            self.active_fades = (0..count)
                .map(|ix| {
                    Fade::new_with(
                        ix == active_ix,
                        Duration::from_millis(150),
                        Duration::from_millis(130),
                        true,
                    )
                })
                .collect();
        }
    }

    fn preview_y(&self, now: Instant, reduced: bool) -> (f32, bool) {
        if reduced {
            return (self.preview_y_to, false);
        }
        let Some(at) = self.preview_y_at else {
            return (self.preview_y_to, false);
        };
        let dur = Duration::from_millis(PREVIEW_GLIDE_MS).as_secs_f32();
        let t = (now.saturating_duration_since(at).as_secs_f32() / dur).min(1.0);
        if t >= 1.0 {
            return (self.preview_y_to, false);
        }
        let e = ease_out_strong(t);
        (
            self.preview_y_from + (self.preview_y_to - self.preview_y_from) * e,
            true,
        )
    }
}

struct ChatTimelineRail {
    t: Tokens,
    points: Rc<Vec<TimelinePoint>>,
    active_ix: usize,
    at_bottom: bool,
    running_since: Option<Instant>,
    rail_top: f32,
    rail_h: f32,
    rail_pad_y: f32,
    pitch: f32,
    presence_opacity: f32,
    leaving: bool,
    scroll_until: Option<Instant>,
    reduced: bool,
    on_jump_row: Rc<dyn Fn(usize, &mut Window, &mut gpui::App)>,
    on_jump_latest: Rc<dyn Fn(&mut Window, &mut gpui::App)>,
}

impl ChatTimelineRail {
    fn slot_center_y(&self, ix: usize) -> f32 {
        self.rail_pad_y + (ix as f32 + 0.5) * self.pitch
    }
}

impl IntoElement for ChatTimelineRail {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ChatTimelineRail {
    type RequestLayoutState = (AnyElement, Rc<RefCell<TimelineState>>);
    type PrepaintState = Vec<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        Some("chat-timeline-rail".into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let total_items = self.points.len() + 1;
        let state = window.with_element_state(
            id.unwrap(),
            |s: Option<Rc<RefCell<TimelineState>>>, _| {
                let s = s.unwrap_or_default();
                (s.clone(), s)
            },
        );
        let now = Instant::now();
        let leaving = self.leaving;
        let scroll_active =
            !leaving && !self.reduced && self.scroll_until.is_some_and(|until| now < until);
        let (
            eff_active_ix,
            eff_at_bottom,
            wake,
            rail_t,
            preview_y,
            hovered_ix,
            shown_ix,
            pressed_ix,
            tick_ts,
            active_ts,
            mut moving,
        ) = {
            let mut st = state.borrow_mut();
            st.ensure_ticks(total_items, self.active_ix);
            let (eff_active_ix, eff_at_bottom) = if leaving {
                *st.exit_snapshot
                    .get_or_insert((self.active_ix, self.at_bottom))
            } else {
                st.exit_snapshot = Some((self.active_ix, self.at_bottom));
                (self.active_ix, self.at_bottom)
            };
            let mut wake_moving = if leaving {
                false
            } else {
                st.scroll_fade.set(scroll_active, now, self.reduced)
            };
            let (wake, wm) = st.scroll_fade.value(now);
            wake_moving |= wm;
            let mut active_moving = false;
            if !leaving {
                for (ix, fade) in st.active_fades.iter_mut().enumerate() {
                    active_moving |= fade.set(ix == eff_active_ix, now, self.reduced);
                }
            }
            let (rail_t, rail_moving) = st.rail_fade.value(now);
            let (preview_y, py_moving) = st.preview_y(now, self.reduced);
            let mut tick_moving = false;
            let tick_ts: Vec<f32> = st
                .tick_fades
                .iter()
                .map(|f| {
                    let (v, m) = f.value(now);
                    tick_moving |= m;
                    v
                })
                .collect();
            let active_ts: Vec<f32> = st
                .active_fades
                .iter()
                .map(|f| {
                    let (v, m) = f.value(now);
                    active_moving |= m;
                    v
                })
                .collect();
            (
                eff_active_ix,
                eff_at_bottom,
                wake,
                rail_t,
                preview_y,
                st.hovered_ix,
                st.shown_ix,
                st.pressed_ix,
                tick_ts,
                active_ts,
                scroll_active
                    || wake_moving
                    || rail_moving
                    || py_moving
                    || tick_moving
                    || active_moving,
            )
        };
        if self.running_since.is_some() && !eff_at_bottom {
            moving = true;
        }
        if moving {
            window.request_animation_frame();
        }

        let t = self.t;
        let pitch = self.pitch;
        let mut tick_rows: Vec<AnyElement> = Vec::with_capacity(total_items);

        for (ix, point) in self.points.iter().enumerate() {
            let tick_t = tick_ts.get(ix).copied().unwrap_or(0.0);
            let active_t = active_ts.get(ix).copied().unwrap_or(0.0);
            let neighbor_t = hovered_ix
                .filter(|&h| h != ix && h.abs_diff(ix) == 1)
                .map(|h| tick_ts.get(h).copied().unwrap_or(0.0) * 0.45)
                .unwrap_or(0.0);
            let focus_t = tick_t.max(neighbor_t);

            let is_pressed = !leaving && pressed_ix == Some(ix);
            let rest_w = 9.0 + 5.0 * active_t;
            let hover_w = 19.0 + active_t;
            let press_nudge = if is_pressed { 1.5 } else { 0.0 };
            let line_w = (rest_w + (hover_w - rest_w) * focus_t - press_nudge).max(7.0);
            let line_h = if active_t > 0.5 || tick_t > 0.5 { 2.0 } else { 1.5 };

            let base_alpha = 0.40 + 0.24 * wake + 0.24 * rail_t + 0.14 * neighbor_t;
            let active_alpha = 0.78 + 0.18 * wake;
            let alpha = base_alpha + (active_alpha - base_alpha) * active_t;
            let color = if is_pressed {
                c(t.accent)
            } else if tick_t > 0.25 {
                c(t.text).opacity((0.60 + 0.35 * tick_t).max(alpha))
            } else if active_t > 0.15 {
                c(t.text).opacity(alpha)
            } else {
                c(t.text_faint).opacity(alpha.min(0.92))
            };

            let row_ix = point.row_ix;
            let on_jump = self.on_jump_row.clone();
            tick_rows.push(
                div()
                    .id(("timeline-tick", ix))
                    .w_full()
                    .h(px(pitch))
                    .px(zpx(6.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .cursor_pointer()
                    .on_click(move |_: &ClickEvent, window, cx| on_jump(row_ix, window, cx))
                    .child(div().w(zpx(line_w)).h(zpx(line_h)).rounded_full().bg(color))
                    .into_any_element(),
            );
        }

        // Bottom "Latest" line: matches the ruler with no extra arrow icon.
        let latest_ix = self.points.len();
        let latest_t = tick_ts.get(latest_ix).copied().unwrap_or(0.0);
        let latest_active_t = active_ts.get(latest_ix).copied().unwrap_or(0.0);
        let latest_neighbor_t = hovered_ix
            .filter(|&h| h != latest_ix && h.abs_diff(latest_ix) == 1)
            .map(|h| tick_ts.get(h).copied().unwrap_or(0.0) * 0.45)
            .unwrap_or(0.0);
        let latest_pressed = !leaving && pressed_ix == Some(latest_ix);
        let (line_w, color) = latest_tick_style(
            t,
            latest_active_t,
            latest_t,
            latest_neighbor_t,
            0.16 * wake + 0.12 * rail_t,
            self.running_since.is_some(),
            latest_pressed,
        );
        let on_latest = self.on_jump_latest.clone();
        tick_rows.push(
            div()
                .id(("timeline-tick", latest_ix))
                .w_full()
                .h(px(pitch))
                .px(zpx(6.0))
                .flex()
                .flex_row()
                .items_center()
                .justify_end()
                .cursor_pointer()
                .on_click(move |_: &ClickEvent, window, cx| on_latest(window, cx))
                .child(div().w(zpx(line_w)).h(zpx(2.0)).rounded_full().bg(color))
                .into_any_element(),
        );

        let preview_card = if !leaving && rail_t > 0.005 {
            shown_ix.map(|ix| self.render_preview_card(ix, preview_y, rail_t, eff_active_ix, t))
        } else {
            None
        };

        let right_inset = RAIL_RIGHT_INSET - RAIL_SLIDE_PX * (1.0 - self.presence_opacity);

        let mut rail_el = div()
            .id("chat-timeline-box")
            .absolute()
            .top(px(self.rail_top))
            .right(zpx(right_inset))
            .w(zpx(RAIL_WIDTH))
            .h(px(self.rail_h))
            .py(px(self.rail_pad_y))
            .opacity(self.presence_opacity)
            .flex()
            .flex_col()
            .children(tick_rows)
            .children(preview_card)
            .into_any_element();

        (rail_el.request_layout(window, cx), (rail_el, state))
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (el, _): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> Self::PrepaintState {
        el.prepaint(window, cx);
        let total_items = self.points.len() + 1;
        let mut hitboxes = Vec::with_capacity(total_items);
        let mut y = bounds.origin.y + px(self.rail_pad_y);
        for _ in 0..total_items {
            let slot = Bounds {
                origin: point(bounds.origin.x, y),
                size: size(bounds.size.width, px(self.pitch)),
            };
            hitboxes.push(window.insert_hitbox(slot, HitboxBehavior::Normal));
            y += px(self.pitch);
        }
        hitboxes
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        (el, state): &mut Self::RequestLayoutState,
        hitboxes: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        let total_items = self.points.len() + 1;
        let new_hovered = if self.leaving || cx.has_active_drag() {
            None
        } else {
            hitboxes.iter().position(|hb| hb.is_hovered(window))
        };
        let now = Instant::now();
        let (needs_frame, pressed_active) = {
            let mut st = state.borrow_mut();
            st.ensure_ticks(total_items, self.active_ix);
            let was_hovered = st.hovered_ix.is_some();
            let is_hovered = new_hovered.is_some();
            let mut changed = st.rail_fade.set(is_hovered, now, self.reduced);
            for (ix, fade) in st.tick_fades.iter_mut().enumerate() {
                changed |= fade.set(new_hovered == Some(ix), now, self.reduced);
            }
            if new_hovered != st.hovered_ix {
                if let Some(new_i) = new_hovered {
                    let target_y = self.slot_center_y(new_i);
                    if was_hovered {
                        let (cur_y, _) = st.preview_y(now, self.reduced);
                        st.preview_y_from = cur_y;
                        st.preview_y_to = target_y;
                        st.preview_y_at = (!self.reduced).then_some(now);
                    } else {
                        st.preview_y_from = target_y;
                        st.preview_y_to = target_y;
                        st.preview_y_at = None;
                    }
                    st.shown_ix = Some(new_i);
                    changed = true;
                }
                st.hovered_ix = new_hovered;
            }
            if new_hovered.is_none() {
                st.pressed_ix = None;
            }
            (changed, st.pressed_ix.is_some())
        };
        if needs_frame {
            window.request_animation_frame();
        }
        el.paint(window, cx);

        if self.leaving {
            return;
        }
        let view = window.current_view();
        let hbs = hitboxes.clone();
        window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
            if phase == gpui::DispatchPhase::Bubble {
                let now_hovered = hbs.iter().position(|hb| hb.is_hovered(window));
                if now_hovered != new_hovered {
                    cx.notify(view);
                }
            }
        });
        let hbs = hitboxes.clone();
        let st = state.clone();
        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
            if phase == gpui::DispatchPhase::Bubble
                && e.button == MouseButton::Left
                && let Some(ix) = hbs.iter().position(|hb| hb.is_hovered(window))
            {
                st.borrow_mut().pressed_ix = Some(ix);
                cx.notify(view);
            }
        });
        if pressed_active {
            let st = state.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == gpui::DispatchPhase::Capture {
                    st.borrow_mut().pressed_ix = None;
                    cx.notify(view);
                }
            });
        }
    }
}

impl ChatTimelineRail {
    fn render_preview_card(
        &self,
        ix: usize,
        preview_y: f32,
        rail_t: f32,
        active_ix: usize,
        t: Tokens,
    ) -> AnyElement {
        let total = self.points.len();
        let is_latest = ix >= total;
        let is_active = ix == active_ix;
        let right_offset = RAIL_WIDTH + PREVIEW_GAP - 6.0 * (1.0 - rail_t);
        let top_offset = (preview_y - 22.0).clamp(-16.0, (self.rail_h - 30.0).max(-16.0));

        let card = div()
            .absolute()
            .top(px(top_offset))
            .right(zpx(right_offset))
            .w(zpx(PREVIEW_WIDTH))
            .px_3()
            .py_2()
            .rounded(RADIUS_LG)
            .floating_surface(t)
            .opacity(rail_t)
            .flex()
            .flex_col()
            .gap(zpx(3.0));

        if is_latest {
            let status_text = match self.running_since {
                Some(since) => format!("Running · {}", elapsed_label(since.elapsed())),
                None if self.at_bottom => "At latest".to_string(),
                None => "Jump to latest".to_string(),
            };
            card.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .when(self.running_since.is_some(), |d| {
                                d.child(crate::motion::dot_grid(
                                    c(t.text),
                                    "timeline-running-dots",
                                    self.reduced,
                                ))
                            })
                            .child(
                                div()
                                    .t_small()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(c(t.text))
                                    .child("Latest"),
                            ),
                    )
                    .child(
                        div()
                            .t_caption()
                            .text_color(c(if self.at_bottom {
                                t.text_faint
                            } else {
                                t.text_muted
                            }))
                            .child(status_text),
                    ),
            )
            .into_any_element()
        } else {
            let point = &self.points[ix];
            card.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .t_caption()
                            .text_color(c(t.text_faint))
                            .child(format!("{} / {}", ix + 1, total)),
                    )
                    .when(is_active && !self.at_bottom, |d| {
                        d.child(
                            div()
                                .t_caption()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(c(t.accent))
                                .child("Viewing"),
                        )
                    }),
            )
            .child(
                div()
                    .t_small()
                    .text_color(c(t.text))
                    .line_clamp(2)
                    .child(point.preview.clone()),
            )
            .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_index_tracks_bottom_and_scrolled_turn() {
        let pts = vec![
            TimelinePoint {
                row_ix: 0,
                preview: "First".into(),
            },
            TimelinePoint {
                row_ix: 5,
                preview: "Second".into(),
            },
            TimelinePoint {
                row_ix: 12,
                preview: "Third".into(),
            },
        ];
        assert_eq!(active_timeline_index(&pts, 0, true), 3);
        assert_eq!(active_timeline_index(&pts, 0, false), 0);
        assert_eq!(active_timeline_index(&pts, 4, false), 1);
        assert_eq!(active_timeline_index(&pts, 9, false), 1);
        assert_eq!(active_timeline_index(&pts, 12, false), 2);
    }

    #[test]
    fn latest_tick_keeps_its_width_as_active_color_fades() {
        for dark in [false, true] {
            let t = keel_theme::resolve_theme(None, dark);
            let mut state = TimelineState::default();
            state.ensure_ticks(4, 2);
            let fade = &mut state.active_fades[3];
            let now = Instant::now();
            let style =
                |active, running| latest_tick_style(t, active, 0.0, 0.0, 0.16, running, false);
            let away = fade.value(now).0;
            fade.set(true, now, false);
            let first = fade.value(now).0;
            let mid = fade.value(now + Duration::from_millis(50)).0;
            let end = fade.value(now + Duration::from_millis(150)).0;
            for running in [false, true] {
                assert_eq!(style(away, running), style(first, running));
                for active in [first, mid, end] {
                    assert_eq!(style(active, running).0, 14.0);
                }
                assert_ne!(style(first, running).1, style(mid, running).1);
                assert_ne!(style(mid, running).1, style(end, running).1);
            }
            assert!(style(first, false).1.a < style(mid, false).1.a);
            assert!(style(mid, false).1.a < style(end, false).1.a);
            assert_eq!(style(first, true).1, c(t.accent));
            assert_eq!(style(end, true), style(end, false));
            fade.set(false, now + Duration::from_millis(150), true);
            assert_eq!(
                style(fade.value(now + Duration::from_millis(150)).0, false),
                style(away, false)
            );
        }
    }

    #[test]
    fn pitch_clamps_smoothly_for_short_and_long_chats() {
        assert_eq!(compute_pitch(400.0, 5), TICK_MAX_PITCH);
        let dense = compute_pitch(180.0, 20);
        assert!(dense >= TICK_MIN_PITCH && dense < TICK_MAX_PITCH);
    }
}

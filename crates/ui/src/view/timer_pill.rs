//! The header pill: Dotify's (see `dotify_ui.rs` for what it shows: Connect,
//! Clock in, Reconnect, or the running entry). Docked beside the header's
//! navigation pill by default; drag it out and it snaps to the nearest
//! corner of the chat pane (never free-floating), drag it onto the header
//! and it docks again. Click (or ⌘⇧T) opens Dotify, or Settings to connect
//! it; right-click opens its menu.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnimationExt as _, AnyElement, Bounds, ClickEvent, Context, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Task, div, prelude::*, px,
};
use keel_theme::Tokens;

use super::{KeelWindow, menu_item, menu_motion, menu_panel, menu_separator};
use crate::motion;
use crate::style::{KeelStyled, c, zpx};
use crate::timer::{TimerDock, now_ms};

/// Pointer travel before a press on the pill becomes a drag.
const DRAG_SLOP: f32 = 4.;
/// Gap from the chat pane's edges for a corner-snapped pill.
const EDGE: f32 = 16.;

type BoundsCell = Rc<Cell<Option<Bounds<Pixels>>>>;

/// A press on the pill: where, and where in the pill it was grabbed.
#[derive(Clone, Copy)]
struct Press {
    at: Point<Pixels>,
    grab: Point<Pixels>,
}

/// View-only timer state (the timer itself is [`SessionTimer`]).
#[derive(Default)]
pub(super) struct TimerUi {
    hover: bool,
    menu: bool,
    tick: Option<Task<()>>,
    press: Option<Press>,
    /// The pointer, once a press has turned into a drag.
    drag: Option<Point<Pixels>>,
    /// Where the pill was painted last (header, corner): grabs and menu-made
    /// moves start from here.
    painted: BoundsCell,
    /// Its resting slot this frame, for the glide into it.
    rest: BoundsCell,
    /// The chat pane below the header: the corners belong to it.
    area: BoundsCell,
    /// A move gliding into its slot: (number, the origin it starts from).
    glide: Option<(u64, Point<Pixels>)>,
    glide_gen: u64,
}

impl KeelWindow {
    // ── Behavior ────────────────────────────────────────────────────────

    fn timer_changed(&mut self, cx: &mut Context<Self>) {
        self.timer.save(&self.state.data_dir);
        self.ensure_timer_tick(cx);
        cx.notify();
    }

    /// ⌘⇧T and a click on the pill, which is always Dotify's: connected, its
    /// popover; not connected, Settings (where Dotify connects). A hidden
    /// pill comes back first.
    pub(super) fn timer_start_pause(&mut self, cx: &mut Context<Self>) {
        if !self.dotify_visible() {
            return;
        }
        self.timer.hidden = false;
        self.timer.save(&self.state.data_dir);
        if self.dotify_mode() {
            self.dotify_toggle_popover(cx);
        } else {
            self.open_settings(cx);
        }
    }

    pub(super) fn timer_reset(&mut self, cx: &mut Context<Self>) {
        self.timer.reset();
        self.timer_changed(cx);
    }

    pub(super) fn timer_set_hidden(&mut self, hidden: bool, cx: &mut Context<Self>) {
        self.timer.hidden = hidden;
        self.timer_ui.menu = false;
        self.timer_ui.hover = false;
        self.timer_changed(cx);
    }

    /// Rest the pill at `dock`, gliding from `from` (a window-space origin;
    /// default: where it was last painted).
    fn timer_place(
        &mut self,
        dock: TimerDock,
        from: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let from = from.or_else(|| self.timer_ui.painted.get().map(|b| b.origin));
        if dock != self.timer.dock {
            // The old slot's bounds would aim the glide at the wrong place.
            self.timer_ui.rest.set(None);
        }
        if let Some(from) = from {
            self.timer_ui.glide_gen += 1;
            self.timer_ui.glide = Some((self.timer_ui.glide_gen, from));
        }
        self.timer.dock = dock;
        self.timer_ui.hover = false;
        self.timer_ui.menu = false;
        self.timer_changed(cx);
    }

    /// One repaint per second while it runs and shows, on the second.
    pub(super) fn ensure_timer_tick(&mut self, cx: &mut Context<Self>) {
        if !self.timer.running() || self.timer.hidden || self.dotify_mode() {
            self.timer_ui.tick = None;
            return;
        }
        if self.timer_ui.tick.is_some() {
            return;
        }
        self.timer_ui.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                let wait = this.update(cx, |this, _| {
                    (this.timer.running() && !this.timer.hidden && !this.dotify_mode())
                        .then(|| this.timer.ms_to_next_second(now_ms()))
                });
                let Ok(Some(wait)) = wait else {
                    break;
                };
                cx.background_executor()
                    .timer(Duration::from_millis(wait + 5))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    fn timer_press(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let origin = self
            .timer_ui
            .painted
            .get()
            .map_or(event.position, |b| b.origin);
        self.timer_ui.press = Some(Press {
            at: event.position,
            grab: event.position - origin,
        });
        self.timer_ui.menu = false;
        cx.notify();
    }

    fn timer_pointer_move(
        &mut self,
        position: Point<Pixels>,
        pressed: Option<MouseButton>,
        cx: &mut Context<Self>,
    ) {
        let Some(press) = self.timer_ui.press else {
            return;
        };
        // Released somewhere that swallowed the mouse-up.
        if pressed != Some(MouseButton::Left) {
            self.timer_pointer_up(position, cx);
            return;
        }
        if self.timer_ui.drag.is_none() {
            let d = position - press.at;
            let (dx, dy) = (f32::from(d.x), f32::from(d.y));
            if dx * dx + dy * dy < DRAG_SLOP * DRAG_SLOP {
                return;
            }
            self.timer_ui.hover = false;
        }
        self.timer_ui.drag = Some(position);
        cx.notify();
    }

    fn timer_pointer_up(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(press) = self.timer_ui.press.take() else {
            return;
        };
        match self.timer_ui.drag.take() {
            Some(_) => {
                let dock = self.timer_drop_target(position);
                self.timer_place(dock, Some(position - press.grab), cx);
            }
            None => self.timer_start_pause(cx),
        }
    }

    /// Where a drop at `position` lands: the header above the chat pane,
    /// else the pane's nearest corner.
    fn timer_drop_target(&self, position: Point<Pixels>) -> TimerDock {
        let area = self.timer_ui.area.get().map(|b| {
            (
                f32::from(b.left()),
                f32::from(b.top()),
                f32::from(b.right()),
                f32::from(b.bottom()),
            )
        });
        TimerDock::for_drop(f32::from(position.x), f32::from(position.y), area)
    }

    fn timer_dragging(&self) -> bool {
        self.timer_ui.drag.is_some()
    }

    // ── Rendering ───────────────────────────────────────────────────────

    /// The header slot beside the navigation pill: the pill when docked
    /// there, a drop hint while a drag would dock it.
    pub(super) fn render_timer_header(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.timer.hidden || !self.dotify_visible() {
            return None;
        }
        if let Some(pointer) = self.timer_ui.drag {
            return (self.timer_drop_target(pointer) == TimerDock::Header).then(|| {
                div()
                    .flex_none()
                    .ml(zpx(6.))
                    .child(self.timer_drop_hint(t))
                    .into_any_element()
            });
        }
        if self.timer.dock != TimerDock::Header {
            return None;
        }
        let pill = self.timer_pill(t, cx);
        Some(
            div()
                .flex_none()
                .ml(zpx(6.))
                .child(self.timer_rest_slot(pill))
                .into_any_element(),
        )
    }

    /// The corner layer over the chat pane (below the header, above the
    /// composer: the caller places it). Always there while the timer shows,
    /// so a drag knows the corners even when the pill is in the header.
    pub(super) fn render_timer_corners(
        &mut self,
        t: Tokens,
        top: Pixels,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.timer.hidden || !self.dotify_visible() {
            return None;
        }
        let record = self.timer_ui.area.clone();
        let target = self.timer_ui.drag.map(|p| self.timer_drop_target(p));
        let corner = match target {
            Some(TimerDock::Header) => None,
            Some(dock) => Some((dock, self.timer_drop_hint(t))),
            None if self.timer.dock != TimerDock::Header => {
                let pill = self.timer_pill(t, cx);
                Some((self.timer.dock, self.timer_rest_slot(pill)))
            }
            None => None,
        };
        let corner = corner.map(|(dock, el)| self.timer_corner(dock, el));
        Some(
            div()
                .absolute()
                .top(top)
                .left_0()
                .right_0()
                .bottom_0()
                .child(
                    gpui::canvas(
                        move |bounds, _, _| record.set(Some(bounds)),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .children(corner)
                .into_any_element(),
        )
    }

    /// `el` pinned `EDGE` in from a corner of the layer. On the sidebar's
    /// side it steps clear of the peeking sidebar, in step with its slide.
    fn timer_corner(&self, dock: TimerDock, el: AnyElement) -> AnyElement {
        let edge = zpx(EDGE);
        let base = div()
            .absolute()
            .when(dock.is_bottom(), |d| d.bottom(edge))
            .when(!dock.is_bottom(), |d| d.top(edge))
            .when(!dock.is_left(), |d| d.right(edge))
            .child(el);
        if !dock.is_left() {
            return base.into_any_element();
        }
        let collapsed = self.state.ui_settings.sidebar_collapsed;
        let (peek, leaving) = (
            collapsed && self.sidebar_peek,
            collapsed && self.peek_closing,
        );
        if !peek && !leaving {
            return base.left(edge).into_any_element();
        }
        // The peek floats 8px in from the window's left edge.
        let (rest, out) = (
            f32::from(edge),
            f32::from(edge) + 8. + self.state.sidebar_width(),
        );
        if self.reduced() {
            return base
                .left(px(if peek { out } else { rest }))
                .into_any_element();
        }
        let (duration, easing): (Duration, fn(f32) -> f32) = if leaving {
            (motion::FAST, motion::ease_in)
        } else {
            (motion::BASE, motion::ease_out)
        };
        base.with_animation(
            ("timer-nudge", self.peek_gen as usize),
            gpui::Animation::new(duration).with_easing(easing),
            move |el, d| {
                let shown = if leaving { 1. - d } else { d };
                el.left(px(rest + (out - rest) * shown))
            },
        )
        .into_any_element()
    }

    /// Wraps the resting pill: records the slot, and plays the glide from
    /// wherever the pill was (drop point, old slot) into it.
    fn timer_rest_slot(&self, pill: AnyElement) -> AnyElement {
        let rest = self.timer_ui.rest.clone();
        let record = rest.clone();
        let slot = div().relative().flex_none().child(
            gpui::canvas(
                move |bounds, _, _| record.set(Some(bounds)),
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        );
        match self.timer_ui.glide {
            Some((n, from)) if !self.reduced() => slot
                .child(div().child(pill).with_animation(
                    ("timer-glide", n as usize),
                    gpui::Animation::new(motion::SLOW).with_easing(motion::ease_out),
                    move |el, d| match rest.get() {
                        // The new slot isn't measured yet (one frame).
                        None => el.opacity(0.),
                        Some(r) => {
                            let k = 1. - d;
                            el.relative()
                                .left(px(f32::from(from.x - r.origin.x) * k))
                                .top(px(f32::from(from.y - r.origin.y) * k))
                        }
                    },
                ))
                .into_any_element(),
            _ => slot.child(pill).into_any_element(),
        }
    }

    /// Where a drag would land: a soft outline the size of the pill.
    fn timer_drop_hint(&self, t: Tokens) -> AnyElement {
        let size = self.timer_ui.painted.get().map(|b| b.size);
        let (w, h) = size.map_or((zpx(34.), zpx(34.)), |s| (s.width, s.height));
        div()
            .w(w)
            .h(h)
            .rounded(zpx(10.))
            .border_1()
            .border_color(c(t.border))
            .bg(c(t.hover))
            .opacity(0.8)
            .into_any_element()
    }

    /// The pill under the pointer while dragging, plus the window-wide
    /// listeners a press needs (the release can land anywhere). Painted last.
    pub(super) fn render_timer_drag(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.dotify_visible() || self.timer_ui.press.is_none() {
            return None;
        }
        let weak = cx.entity().downgrade();
        let listeners = gpui::canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let on_move = weak.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                    if phase == gpui::DispatchPhase::Bubble {
                        let _ = on_move.update(cx, |this, cx| {
                            this.timer_pointer_move(e.position, e.pressed_button, cx)
                        });
                    }
                });
                let on_up = weak;
                window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                    if phase == gpui::DispatchPhase::Bubble && e.button == MouseButton::Left {
                        let _ = on_up.update(cx, |this, cx| this.timer_pointer_up(e.position, cx));
                    }
                });
            },
        )
        .absolute()
        .size_0();
        let ghost = match (self.timer_ui.drag, self.timer_ui.press) {
            (Some(pointer), Some(press)) => {
                let origin = pointer - press.grab;
                let content = self.dotify_pill_content(t, false, false, false, cx);
                Some(
                    div()
                        .absolute()
                        .left(origin.x)
                        .top(origin.y)
                        .p(zpx(3.))
                        .floating_surface(t)
                        .opacity(0.94)
                        .cursor(gpui::CursorStyle::ClosedHand)
                        .shadow(vec![gpui::BoxShadow {
                            color: c(t.shadow),
                            offset: gpui::point(zpx(0.), zpx(12.)),
                            blur_radius: zpx(28.),
                            spread_radius: zpx(-4.),
                            inset: false,
                        }])
                        .child(content),
                )
            }
            _ => None,
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .child(listeners)
                .children(ghost)
                .into_any_element(),
        )
    }

    /// The right-click menu, anchored under the pill (over it, in a bottom
    /// corner). At the root so it never nests inside the header's lift.
    pub(super) fn render_timer_menu(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.dotify_visible() {
            return None;
        }
        let leaving =
            self.menu_phase("timer-menu", self.timer_ui.menu && !self.timer.hidden, cx)?;
        let pill = self.timer_ui.painted.get()?;
        let up = self.timer.dock.is_bottom();
        let docked = self.timer.dock == TimerDock::Header;
        let head: Vec<AnyElement> = if self.dotify_mode() {
            self.dotify_menu_items(t, cx)
        } else {
            vec![
                menu_item("timer-menu-connect", t, true, false)
                    .child("Connect Dotify…")
                    .child(div().flex_1())
                    .child(div().t_caption().text_color(c(t.text_faint)).child("⌘⇧T"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.timer_ui.menu = false;
                        this.timer_start_pause(cx);
                    }))
                    .into_any_element(),
            ]
        };
        let mut items = head;
        items.extend([
            menu_separator(t).into_any_element(),
            menu_item("timer-menu-dock", t, true, false)
                .child(if docked { "Pop out" } else { "Dock in header" })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let dock = if docked {
                        TimerDock::TopRight
                    } else {
                        TimerDock::Header
                    };
                    this.timer_place(dock, None, cx);
                }))
                .into_any_element(),
            menu_item("timer-menu-hide", t, true, false)
                .child("Hide")
                .on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.timer_set_hidden(true, cx)),
                )
                .into_any_element(),
        ]);
        let dismiss = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.timer_ui.menu = false;
            cx.notify();
        });
        let (anchor, at) = if up {
            (gpui::Anchor::BottomLeft, pill.origin)
        } else {
            (
                gpui::Anchor::TopLeft,
                gpui::point(pill.left(), pill.bottom()),
            )
        };
        let panel = menu_panel(
            "timer-menu",
            t,
            if self.dotify_mode() { 200. } else { 184. },
            up,
            items,
        )
        .when(leaving.is_none(), |d| d.on_mouse_down_out(dismiss));
        let menu = menu_motion(panel, "timer-menu", up, leaving, self.reduced());
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(at)
                    .anchor(anchor)
                    .snap_to_window_with_margin(zpx(8.))
                    .child(menu),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// The pill itself: header chrome when docked, a floating surface in a
    /// corner. Press to start/pause or drag; right-click for the menu.
    fn timer_pill(&mut self, t: Tokens, cx: &mut Context<Self>) -> AnyElement {
        let painted = self.timer_ui.painted.clone();
        let hovered = self.timer_ui.hover && !self.timer_dragging();
        let pressed = self.timer_ui.press.is_some() && self.timer_ui.drag.is_none();
        let content = self.dotify_pill_content(t, hovered, pressed, true, cx);
        div()
            .id("session-timer")
            .relative()
            .flex_none()
            .cursor_pointer()
            .child(
                gpui::canvas(
                    move |bounds, _, _| painted.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(content)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.timer_ui.hover != *hovered {
                    this.timer_ui.hover = *hovered;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| this.timer_press(e, cx)),
            )
            // Also here, in case the release beats the window listeners' first paint.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, _, cx| this.timer_pointer_up(e.position, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.timer_ui.menu = true;
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// Escape and outside clicks: close the right-click menu. True when open.
    pub(super) fn timer_close_menu(&mut self) -> bool {
        std::mem::take(&mut self.timer_ui.menu)
    }

    pub(super) fn timer_ui_close_menu_only(&mut self) {
        self.timer_ui.menu = false;
    }

    /// When the pill rests in the transcript's top-right corner: how far
    /// down it reaches (its inset plus its height), so the session card can
    /// sit under it instead of on it.
    pub(super) fn timer_top_right_reach(&self) -> Option<f32> {
        let rests = self.dotify_visible()
            && !self.timer.hidden
            && self.timer.dock == TimerDock::TopRight
            && self.timer_ui.drag.is_none();
        rests.then(|| {
            let h = self
                .timer_ui
                .rest
                .get()
                .map_or(f32::from(zpx(28.)), |b| f32::from(b.size.height));
            f32::from(zpx(EDGE)) + h
        })
    }

    pub(super) fn timer_ui_painted(&self) -> Option<Bounds<Pixels>> {
        self.timer_ui.painted.get()
    }
}

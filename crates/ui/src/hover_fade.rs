//! Hover fades. gpui's `.hover()` swaps styles in one frame; these elements
//! ease the change instead: in over [`HOVER_IN`], out over [`HOVER_OUT`], both
//! on [`ease_out`]. Frames are requested only while a fade is playing, so an
//! idle window stays idle. Presses stay instant, and reduced motion cuts.
//!
//! - [`HoverFadeExt::hover_fade`] paints a hover tint under the element's
//!   children (over its own background), clipped to its corner radii.
//! - [`reveal_on_hover`] fades a child's opacity while a named group is hovered,
//!   the eased twin of `.opacity(0.).group_hover(g, |s| s.opacity(1.))`,
//!   with the group parent marked by [`HoverFadeExt::hover_probe`].

use crate::motion::ease_out;
use crate::style::zpx;
use gpui::{
    AbsoluteLength, AnyElement, App, Bounds, Corners, Element, ElementId, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, SharedString, Stateful, Style, Styled, Window,
    prelude::*,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

thread_local! {
    /// The latest hitbox prepainted for each probe group. A probe is its
    /// group parent's first child, so it prepaints before any reveal inside
    /// that parent reads it.
    static PROBES: RefCell<HashMap<SharedString, Hitbox>> = RefCell::new(HashMap::new());
    /// True while building rows inside a virtualized list scroll layer: hovers
    /// resolve immediately without per-frame `request_animation_frame()` ticks
    /// or `cx.notify(list_view)` calls that would bypass scroll-layer caching.
    static LIST_SCOPE: Cell<bool> = const { Cell::new(false) };
}

/// Marks hover fades constructed inside `f` as list-scoped (instant hover, no
/// animation frames or list-view notifications).
pub fn in_list_scope<R>(f: impl FnOnce() -> R) -> R {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            LIST_SCOPE.with(|c| c.set(self.0));
        }
    }
    let prev = LIST_SCOPE.with(|c| c.replace(true));
    let _reset = Reset(prev);
    f()
}

fn in_list() -> bool {
    LIST_SCOPE.with(Cell::get)
}
use std::time::{Duration, Instant};

/// Hover tint fading in.
pub const HOVER_IN: Duration = Duration::from_millis(120);
/// Hover tint fading out: a touch quicker than in.
pub const HOVER_OUT: Duration = Duration::from_millis(100);

/// An eased 0..1 value chasing a hover flag.
#[derive(Clone, Copy)]
pub struct Fade {
    on: bool,
    from: f32,
    since: Option<Instant>,
    in_dur: Duration,
    out_dur: Duration,
    strong: bool,
}

impl Fade {
    pub fn new(on: bool) -> Self {
        Self::new_with(on, HOVER_IN, HOVER_OUT, false)
    }

    pub fn new_with(on: bool, in_dur: Duration, out_dur: Duration, strong: bool) -> Self {
        Self {
            on,
            from: if on { 1. } else { 0. },
            since: None,
            in_dur,
            out_dur,
            strong,
        }
    }

    /// Current value and whether it is still moving.
    pub fn value(&self, now: Instant) -> (f32, bool) {
        let target = if self.on { 1. } else { 0. };
        let Some(since) = self.since else {
            return (target, false);
        };
        let dur = if self.on { self.in_dur } else { self.out_dur };
        let t = (now.saturating_duration_since(since).as_secs_f32() / dur.as_secs_f32()).min(1.);
        if t >= 1. {
            return (target, false);
        }
        let e = if self.strong {
            crate::motion::ease_out_strong(t)
        } else {
            ease_out(t)
        };
        (self.from + (target - self.from) * e, true)
    }

    /// Retarget from wherever the value is now. Returns true on a change.
    pub fn set(&mut self, on: bool, now: Instant, reduced: bool) -> bool {
        if on == self.on {
            return false;
        }
        let (v, _) = self.value(now);
        self.on = on;
        self.from = v;
        self.since = (!reduced).then_some(now);
        true
    }
}

#[derive(Default)]
struct TintState {
    fade: Option<Fade>,
    pressed: bool,
}

/// The tint layer behind [`HoverFadeExt::hover_fade`] (and, untinted, the
/// probe behind [`HoverFadeExt::hover_probe`]).
pub struct HoverTint {
    probe: Option<SharedString>,
    enabled: bool,
    instant: bool,
    color: Hsla,
    press: Option<Hsla>,
    radii: Corners<Option<AbsoluteLength>>,
    style: Style,
}

impl IntoElement for HoverTint {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for HoverTint {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(
            if self.probe.is_some() {
                "hover-probe"
            } else {
                "hover-tint"
            }
            .into(),
        )
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (window.request_layout(self.style.clone(), [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) -> Hitbox {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        if let Some(group) = &self.probe {
            PROBES.with(|p| p.borrow_mut().insert(group.clone(), hitbox.clone()));
        }
        hitbox
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(id) = id else { return };
        if self.probe.is_some() {
            return;
        }
        let reduced = self.instant || cx.reduce_motion();
        let hovered = self.enabled && hitbox.is_hovered(window) && !cx.has_active_drag();
        let state = window.with_element_state(id, |s: Option<Rc<RefCell<TintState>>>, _| {
            let s = s.unwrap_or_default();
            (s.clone(), s)
        });
        let now = Instant::now();
        let (value, moving, pressed) = {
            let mut st = state.borrow_mut();
            let fade = st.fade.get_or_insert_with(|| Fade::new(hovered));
            fade.set(hovered, now, reduced);
            let (v, moving) = fade.value(now);
            if !hovered {
                st.pressed = false;
            }
            (v, moving, st.pressed)
        };
        let rem = window.rem_size();
        let px = |l: Option<AbsoluteLength>| l.map_or(Pixels::ZERO, |l| l.to_pixels(rem));
        let radii = Corners {
            top_left: px(self.radii.top_left),
            top_right: px(self.radii.top_right),
            bottom_right: px(self.radii.bottom_right),
            bottom_left: px(self.radii.bottom_left),
        };
        let paint = match (pressed, self.press) {
            (true, Some(press)) => Some(press),
            _ if value > 0.001 => Some(self.color.opacity(value)),
            _ => None,
        };
        if let Some(color) = paint {
            window.paint_quad(gpui::fill(bounds, color).corner_radii(radii));
        }
        if moving && !self.instant {
            window.request_animation_frame();
        }

        // Redraw when the hover or press flips; nothing runs in between.
        let view = window.current_view();
        if self.enabled {
            let hb = hitbox.clone();
            let instant = self.instant;
            window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                if phase == gpui::DispatchPhase::Bubble && hb.is_hovered(window) != hovered {
                    if instant {
                        window.refresh();
                    } else {
                        cx.notify(view);
                    }
                }
            });
        }
        if self.enabled && self.press.is_some() {
            let hb = hitbox.clone();
            let st = state.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase == gpui::DispatchPhase::Bubble
                    && e.button == gpui::MouseButton::Left
                    && hb.is_hovered(window)
                {
                    st.borrow_mut().pressed = true;
                    cx.notify(view);
                }
            });
            if pressed {
                let st = state.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        st.borrow_mut().pressed = false;
                        cx.notify(view);
                    }
                });
            }
        }
    }
}

/// Eased hover tints for stateful divs.
pub trait HoverFadeExt: Sized {
    /// Fade `color` in under the children on hover (call after `.rounded()`,
    /// the tint copies the corner radii). `press`, when set, shows at once
    /// while the mouse is held, so press feedback stays instant.
    fn hover_fade(self, color: impl Into<Hsla>, press: Option<Hsla>) -> Self;
    /// Like [`Self::hover_fade`], but keeps the tint layer mounted when `enabled`
    /// is `false` so child indices never shift and an active hover eases out
    /// cleanly instead of snapping off.
    fn hover_fade_when(self, enabled: bool, color: impl Into<Hsla>, press: Option<Hsla>) -> Self;
    /// [`Self::hover_fade`] with the tint (and its hover area) grown past the
    /// element by `x` and `y`, for bare rows whose text sits flush with
    /// their neighbours.
    fn hover_fade_outset(
        self,
        color: impl Into<Hsla>,
        press: Option<Hsla>,
        x: Pixels,
        y: Pixels,
    ) -> Self;
    /// Mark this element as the hover group `group` for [`reveal_on_hover`]
    /// children. Call before adding children.
    fn hover_probe(self, group: impl Into<SharedString>) -> Self;
}

impl HoverFadeExt for Stateful<gpui::Div> {
    fn hover_fade(self, color: impl Into<Hsla>, press: Option<Hsla>) -> Self {
        self.hover_fade_outset(color, press, Pixels::ZERO, Pixels::ZERO)
    }

    fn hover_fade_when(
        mut self,
        enabled: bool,
        color: impl Into<Hsla>,
        press: Option<Hsla>,
    ) -> Self {
        let color = color.into();
        let r = self.style().corner_radii.clone();
        let radii = Corners {
            top_left: r.top_left,
            top_right: r.top_right,
            bottom_right: r.bottom_right,
            bottom_left: r.bottom_left,
        };
        self.child(HoverTint {
            probe: None,
            enabled,
            instant: in_list(),
            color,
            press,
            radii,
            style: layer_style(Pixels::ZERO, Pixels::ZERO),
        })
    }

    fn hover_fade_outset(
        mut self,
        color: impl Into<Hsla>,
        press: Option<Hsla>,
        x: Pixels,
        y: Pixels,
    ) -> Self {
        let color = color.into();
        let r = self.style().corner_radii.clone();
        let radii = Corners {
            top_left: r.top_left,
            top_right: r.top_right,
            bottom_right: r.bottom_right,
            bottom_left: r.bottom_left,
        };
        self.child(HoverTint {
            probe: None,
            enabled: true,
            instant: in_list(),
            color,
            press,
            radii,
            style: layer_style(x, y),
        })
    }

    fn hover_probe(self, group: impl Into<SharedString>) -> Self {
        self.child(HoverTint {
            probe: Some(group.into()),
            enabled: true,
            instant: in_list(),
            color: gpui::transparent_black(),
            press: None,
            radii: Corners::default(),
            style: layer_style(Pixels::ZERO, Pixels::ZERO),
        })
    }
}

/// Absolute, inset `-x`/`-y`: covers the parent (plus any outset) without
/// joining its layout.
fn layer_style(x: Pixels, y: Pixels) -> Style {
    let mut style = Style::default();
    style.position = gpui::Position::Absolute;
    let at = |v: Pixels| gpui::Length::Definite((-v).into());
    style.inset.top = at(y);
    style.inset.left = at(x);
    style.inset.bottom = at(y);
    style.inset.right = at(x);
    style
}

/// A child whose opacity (and optional positional offset) eases toward `on`
/// while `group` is hovered and toward `off` otherwise.
pub struct RevealOnHover<E> {
    id: ElementId,
    group: SharedString,
    off: f32,
    on: f32,
    in_dur: Duration,
    out_dur: Duration,
    strong: bool,
    instant: bool,
    offset_x: f32,
    offset_y: f32,
    el: Option<E>,
}

/// Fade `el` from `off` to `on` opacity while `group` is hovered. The eased
/// twin of `.opacity(off).group_hover(group, |s| s.opacity(on))`; the group
/// parent marks itself with [`HoverFadeExt::hover_probe`].
pub fn reveal_on_hover<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    group: impl Into<SharedString>,
    off: f32,
    on: f32,
    el: E,
) -> RevealOnHover<E> {
    RevealOnHover {
        id: id.into(),
        group: group.into(),
        off,
        on,
        in_dur: HOVER_IN,
        out_dur: HOVER_OUT,
        strong: false,
        instant: in_list(),
        offset_x: 0.,
        offset_y: 0.,
        el: Some(el),
    }
}

/// Reveal with custom in/out durations and strong ease-out curve (`cubic-bezier(0.23, 1, 0.32, 1)`).
pub fn reveal_on_hover_curved<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    group: impl Into<SharedString>,
    off: f32,
    on: f32,
    in_dur: Duration,
    out_dur: Duration,
    el: E,
) -> RevealOnHover<E> {
    RevealOnHover {
        id: id.into(),
        group: group.into(),
        off,
        on,
        in_dur,
        out_dur,
        strong: true,
        instant: in_list(),
        offset_x: 0.,
        offset_y: 0.,
        el: Some(el),
    }
}

impl<E> RevealOnHover<E> {
    /// Slide from `(x, y)` px at rest (`v = 0`) to `(0, 0)` when hovered (`v = 1`).
    /// Skipped under reduced motion so the reveal stays a pure opacity fade.
    pub fn with_offset(mut self, x: f32, y: f32) -> Self {
        self.offset_x = x;
        self.offset_y = y;
        self
    }
}

impl<E: IntoElement + Styled + 'static> IntoElement for RevealOnHover<E> {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl<E: IntoElement + Styled + 'static> Element for RevealOnHover<E> {
    type RequestLayoutState = (AnyElement, Rc<RefCell<Option<Fade>>>);
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let in_dur = self.in_dur;
        let out_dur = self.out_dur;
        let strong = self.strong;
        let state =
            window.with_element_state(id.unwrap(), |s: Option<Rc<RefCell<Option<Fade>>>>, _| {
                let s = s.unwrap_or_else(|| Rc::new(RefCell::new(Some(Fade::new_with(false, in_dur, out_dur, strong)))));
                (s.clone(), s)
            });
        let (v, moving) = state
            .borrow()
            .map_or((0., false), |f| f.value(Instant::now()));
        if moving && !self.instant {
            window.request_animation_frame();
        }
        let opacity = self.off + (self.on - self.off) * v;
        let mut styled = self.el.take().expect("laid out once").opacity(opacity);
        if !self.instant
            && !cx.reduce_motion()
            && self.off < self.on
            && (self.offset_x != 0. || self.offset_y != 0.)
        {
            let t = 1. - v;
            styled = styled
                .relative()
                .left(zpx(self.offset_x * t))
                .top(zpx(self.offset_y * t));
        }
        let mut el = styled.into_any_element();
        (el.request_layout(window, cx), (el, state))
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        (el, _): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Hitbox> {
        let group = PROBES.with(|p| p.borrow().get(&self.group).cloned());
        el.prepaint(window, cx);
        group
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        (el, state): &mut Self::RequestLayoutState,
        group: &mut Option<Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let group = group.clone();
        let hovered = group.as_ref().is_some_and(|g| g.is_hovered(window)) && !cx.has_active_drag();
        let now = Instant::now();
        let in_dur = self.in_dur;
        let out_dur = self.out_dur;
        let strong = self.strong;
        let reduced = self.instant || cx.reduce_motion();
        let changed = {
            let mut st = state.borrow_mut();
            match st.as_mut() {
                None => {
                    *st = Some(Fade::new_with(hovered, in_dur, out_dur, strong));
                    hovered
                }
                Some(f) => f.set(hovered, now, reduced),
            }
        };
        if changed && !self.instant {
            window.request_animation_frame();
        }
        el.paint(window, cx);
        if let Some(g) = group {
            let view = window.current_view();
            let instant = self.instant;
            let state = state.clone();
            window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                if phase == gpui::DispatchPhase::Bubble {
                    let next = g.is_hovered(window) && !cx.has_active_drag();
                    if next != hovered {
                        if instant {
                            if let Some(f) = state.borrow_mut().as_mut() {
                                f.set(next, Instant::now(), true);
                            }
                            window.refresh();
                        } else {
                            cx.notify(view);
                        }
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_eases_and_settles() {
        let t0 = Instant::now();
        let mut f = Fade::new(false);
        assert_eq!(f.value(t0), (0., false));
        assert!(f.set(true, t0, false));
        let (mid, moving) = f.value(t0 + HOVER_IN / 2);
        assert!(moving && mid > 0.5 && mid < 1.);
        assert_eq!(f.value(t0 + HOVER_IN), (1., false));
        // Retarget mid-way: leaves from the current value, not from 1.
        let t1 = t0 + HOVER_IN / 2;
        let mut g = Fade::new(false);
        g.set(true, t0, false);
        g.set(false, t1, false);
        assert!((g.from - mid).abs() < 1e-4);
        assert_eq!(g.value(t1 + HOVER_OUT), (0., false));
    }

    #[test]
    fn reduced_motion_cuts() {
        let t0 = Instant::now();
        let mut f = Fade::new(false);
        f.set(true, t0, true);
        assert_eq!(f.value(t0), (1., false));
    }
}

//! Hover tooltips: a small capsule on the theme's popover surface.
//!
//! [`HoverTips`] keeps one toolbar's hover state: a tooltip appears after
//! [`TIP_DELAY`] on a button; once one has shown, a neighbour's switches in
//! at once for [`TIP_WARM`] (the warm-up). Entrance and exit are
//! timestamp-driven, with frames asked for only while they play. The
//! capsule takes no mouse events, so it never steals a click.
//! [`capsule`] is the look (a small rounded rectangle, despite the name), shared with gpui's own tooltips (see `Tip`).
//! Those keep the same rules: [`delay`] says how long gpui waits, none
//! while warm, and [`Entrance`] plays the fade unless it's a warm switch.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{AnyElement, Bounds, Pixels, div, point, prelude::*, px};

use keel_theme::Tokens;

use crate::style::{KeelStyled, RADIUS_SM, c, zpx};

/// Hover this long before a tooltip shows.
pub(super) const TIP_DELAY: Duration = Duration::from_millis(400);
/// After a tooltip, a neighbour's shows with no delay for this long.
pub(super) const TIP_WARM: Duration = Duration::from_millis(500);
const TIP_IN: f32 = 120.;
const TIP_OUT: f32 = 100.;
/// The entrance slides down this far onto its place.
const TIP_RISE: f32 = 3.;
/// Below the button.
const TIP_GAP: f32 = 6.;
const TEXT: f32 = 12.;
const KEY_TEXT: f32 = 11.;

#[derive(Default)]
pub(super) struct HoverTips {
    /// The button under the pointer, and since when.
    hovered: Option<(&'static str, Instant)>,
    /// Showing: which, since when, and whether it plays the entrance (a
    /// warm switch doesn't).
    shown: Option<(&'static str, Instant, bool)>,
    /// Fading out.
    leaving: Option<(&'static str, Instant)>,
    /// When the last tooltip went away (the warm-up runs from here).
    hidden_at: Option<Instant>,
    /// Clicked: no tooltip until the pointer leaves it.
    suppressed: Option<&'static str>,
    /// Each button's bounds, as last painted.
    pub(super) bounds: Rc<RefCell<HashMap<&'static str, Bounds<Pixels>>>>,
}

impl HoverTips {
    /// The pointer entered (`on`) or left `id`. `Some(delay)`: call
    /// [`Self::ripen`] with `now` after it.
    pub(super) fn hover(&mut self, id: &'static str, on: bool, now: Instant) -> Option<Duration> {
        if !on {
            if self.suppressed == Some(id) {
                self.suppressed = None;
            }
            if self.hovered.is_some_and(|(h, _)| h == id) {
                self.hovered = None;
            }
            if self.shown.is_some_and(|(s, _, _)| s == id) {
                self.shown = None;
                self.leaving = Some((id, now));
                self.hidden_at = Some(now);
            }
            return None;
        }
        self.hovered = Some((id, now));
        if self.suppressed == Some(id) || self.shown.is_some_and(|(s, _, _)| s == id) {
            return None;
        }
        if self.warm(now) {
            // Straight across from a neighbour: no delay, no entrance, and
            // the old one simply gives way (two never overlap).
            self.leaving = None;
            self.shown = Some((id, now, false));
            return None;
        }
        Some(TIP_DELAY)
    }

    fn warm(&self, now: Instant) -> bool {
        self.shown.is_some() || self.hidden_at.is_some_and(|at| now.saturating_duration_since(at) < TIP_WARM)
    }

    /// The hover delay that began at `since` ran out: show its tooltip if
    /// the pointer is still on that button. True when one appeared.
    pub(super) fn ripen(&mut self, id: &'static str, since: Instant, now: Instant) -> bool {
        if self.hovered != Some((id, since)) || self.shown.is_some() || self.suppressed == Some(id) {
            return false;
        }
        if self.leaving.is_some_and(|(l, _)| l == id) {
            self.leaving = None;
        }
        self.shown = Some((id, now, true));
        true
    }

    /// A click on `id`: its tooltip fades and stays away until the pointer leaves.
    pub(super) fn press(&mut self, id: &'static str, now: Instant) {
        self.suppressed = Some(id);
        if self.shown.is_some_and(|(s, _, _)| s == id) {
            self.shown = None;
            self.leaving = Some((id, now));
            self.hidden_at = None;
        }
    }

    /// Forget everything (the toolbar went away).
    pub(super) fn clear(&mut self) {
        let bounds = self.bounds.clone();
        *self = Self { bounds, ..Self::default() };
    }

    /// Which tooltip to draw now and its look: (id, opacity, y offset).
    /// `None` when nothing shows. Also says whether it's still moving.
    fn phase(&mut self, now: Instant, reduced: bool) -> Option<(&'static str, f32, f32, bool)> {
        if let Some((id, at, animate)) = self.shown {
            let x = now.saturating_duration_since(at).as_secs_f32() * 1000. / TIP_IN;
            if !animate || reduced || x >= 1. {
                return Some((id, 1., 0., false));
            }
            let p = crate::motion::ease_out(x);
            return Some((id, p, -TIP_RISE * (1. - p), true));
        }
        let (id, at) = self.leaving?;
        let x = now.saturating_duration_since(at).as_secs_f32() * 1000. / TIP_OUT;
        if reduced || x >= 1. {
            self.leaving = None;
            return None;
        }
        Some((id, 1. - crate::motion::ease_out(x), 0., true))
    }

    /// The tooltip, if one shows: `label(id)` names a button (and its
    /// shortcut), read fresh each frame so a toggle's label follows its state.
    pub(super) fn render(
        &mut self,
        label: impl Fn(&str) -> Option<(String, Option<&'static str>)>,
        t: Tokens,
        window: &gpui::Window,
        reduced: bool,
    ) -> Option<AnyElement> {
        let (id, opacity, dy, moving) = self.phase(Instant::now(), reduced)?;
        if moving {
            window.request_animation_frame();
        }
        let anchor = *self.bounds.borrow().get(id)?;
        let (text, keys) = label(id)?;
        let w = capsule_width(window, &text, keys);
        let viewport = f32::from(window.viewport_size().width);
        let margin = f32::from(zpx(8.));
        let center = f32::from(anchor.origin.x) + f32::from(anchor.size.width) / 2.;
        let x = (center - w / 2.).min(viewport - w - margin).max(margin);
        let y = f32::from(anchor.origin.y + anchor.size.height) + f32::from(zpx(TIP_GAP)) + f32::from(zpx(dy));
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(point(px(x), px(y)))
                    .snap_to_window_with_margin(zpx(8.))
                    .child(capsule(t, text, keys).opacity(opacity)),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}

/// The tooltip's look: 12 px theme text on the popover surface (solid,
/// the same rim and radius as menus) with a smaller, softer shadow, as
/// it's a small thing close to its button; the shortcut, if any, as a
/// muted keycap.
pub(super) fn capsule(t: Tokens, text: impl Into<gpui::SharedString>, keys: Option<&'static str>) -> gpui::Div {
    div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(6.))
        .px(zpx(9.))
        .py(zpx(6.))
        .floating_surface(t)
        .shadow(vec![gpui::BoxShadow {
            color: c(t.shadow),
            offset: point(px(0.), zpx(4.)),
            blur_radius: zpx(14.),
            spread_radius: zpx(-2.),
            inset: false,
        }])
        .whitespace_nowrap()
        .text_size(zpx(TEXT))
        .line_height(zpx(16.))
        .text_color(c(t.text))
        .child(div().relative().child(text.into()))
        .children(keys.map(|k| {
            div()
                .relative()
                .px(zpx(4.))
                .rounded(RADIUS_SM)
                .border_1()
                .border_color(c(t.border))
                .bg(c(t.hover))
                .text_size(zpx(KEY_TEXT))
                .line_height(zpx(14.))
                .text_color(c(t.text_muted))
                .child(k)
        }))
}

thread_local! {
    /// gpui tooltips showing now.
    static ALIVE: Cell<usize> = const { Cell::new(0) };
    /// When the last one went away (the warm-up runs from here).
    static GONE_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The window's reduced-motion setting, as of its last frame.
    static REDUCED: Cell<bool> = const { Cell::new(false) };
}

/// Each frame: whether motion is reduced (gpui's tooltips can't ask).
pub(super) fn set_reduced(on: bool) {
    REDUCED.with(|r| r.set(on));
}

fn tips_warm(now: Instant) -> bool {
    ALIVE.with(Cell::get) > 0 || GONE_AT.with(Cell::get).is_some_and(|at| now.saturating_duration_since(at) < TIP_WARM)
}

/// How long gpui waits before a tooltip: [`TIP_DELAY`] cold, none while
/// one shows or just went (moving along a row of buttons).
pub(super) fn delay() -> Duration {
    if tips_warm(Instant::now()) { Duration::ZERO } else { TIP_DELAY }
}

/// A gpui tooltip's entrance: a short fade as it drops [`TIP_RISE`] onto
/// its place, skipped for a warm switch or with reduced motion. Counts
/// itself while it lives, for [`delay`].
pub(super) struct Entrance {
    born: Instant,
    animate: bool,
}

impl Entrance {
    pub(super) fn new() -> Self {
        let now = Instant::now();
        let animate = !tips_warm(now) && !REDUCED.with(Cell::get);
        ALIVE.with(|a| a.set(a.get() + 1));
        Self { born: now, animate }
    }

    /// (opacity, y offset) now, and whether it's still moving.
    pub(super) fn look(&self, now: Instant) -> (f32, f32, bool) {
        let x = now.saturating_duration_since(self.born).as_secs_f32() * 1000. / TIP_IN;
        if !self.animate || x >= 1. {
            return (1., 0., false);
        }
        let p = crate::motion::ease_out(x);
        (p, -TIP_RISE * (1. - p), true)
    }
}

impl Drop for Entrance {
    fn drop(&mut self) {
        ALIVE.with(|a| a.set(a.get().saturating_sub(1)));
        GONE_AT.with(|g| g.set(Some(Instant::now())));
    }
}

/// The capsule's width, to center it under its button before it's laid out.
fn capsule_width(window: &gpui::Window, text: &str, keys: Option<&str>) -> f32 {
    let measure = |s: &str, size: f32| {
        let system = window.text_system();
        let font = system.resolve_font(&gpui::font(crate::style::SANS));
        let size = zpx(size);
        s.chars().map(|ch| system.advance(font, size, ch).map(|a| f32::from(a.width)).unwrap_or(f32::from(size) * 0.6)).sum::<f32>()
    };
    let z = |v: f32| f32::from(zpx(v));
    let key = keys.map_or(0., |k| z(6.) + measure(k, KEY_TEXT) + z(8.) + 2.);
    measure(text, TEXT) + key + z(18.) + 2.
}

/// The shortcut as shown on this platform (only macOS shows keycaps).
pub(super) fn keys(mac: &'static str) -> Option<&'static str> {
    cfg!(target_os = "macos").then_some(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gpui_tooltip_waits_cold_and_switches_at_once_warm() {
        // Thread-local, so this test's tooltips are its own.
        GONE_AT.with(|g| g.set(None));
        assert_eq!(delay(), TIP_DELAY);
        let first = Entrance::new();
        assert!(first.animate);
        let (o, dy, moving) = first.look(first.born);
        assert!(o == 0. && dy == -TIP_RISE && moving);
        assert_eq!(first.look(first.born + Duration::from_millis(TIP_IN as u64)), (1., 0., false));
        assert_eq!(delay(), Duration::ZERO);
        drop(first);
        // Straight onto a neighbour: no wait and no entrance.
        assert_eq!(delay(), Duration::ZERO);
        let next = Entrance::new();
        assert!(!next.animate);
        drop(next);
        GONE_AT.with(|g| g.set(Some(Instant::now() - TIP_WARM)));
        assert_eq!(delay(), TIP_DELAY);
    }

    #[test]
    fn reduced_motion_skips_the_gpui_entrance() {
        GONE_AT.with(|g| g.set(None));
        set_reduced(true);
        let tip = Entrance::new();
        set_reduced(false);
        assert_eq!(tip.look(tip.born).0, 1.);
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn a_tooltip_waits_for_the_hover_delay() {
        let t0 = Instant::now();
        let mut tips = HoverTips::default();
        assert_eq!(tips.hover("back", true, t0), Some(TIP_DELAY));
        assert!(tips.phase(at(t0, 100), false).is_none());
        // A delay from an earlier hover doesn't show it.
        assert!(!tips.ripen("back", at(t0, 1), at(t0, 400)));
        assert!(tips.ripen("back", t0, at(t0, 400)));
        let (id, o, dy, moving) = tips.phase(at(t0, 400), false).unwrap();
        assert_eq!(id, "back");
        assert!(o < 0.05 && dy < -2.9 && moving, "the entrance starts faded, 3 px up");
        let (_, o, dy, moving) = tips.phase(at(t0, 560), false).unwrap();
        assert!(o == 1. && dy == 0. && !moving);
        // Left before the delay: never shows.
        let mut quick = HoverTips::default();
        quick.hover("back", true, t0);
        quick.hover("back", false, at(t0, 200));
        assert!(!quick.ripen("back", t0, at(t0, 400)));
    }

    #[test]
    fn neighbours_switch_at_once_while_warm() {
        let t0 = Instant::now();
        let mut tips = HoverTips::default();
        tips.hover("back", true, t0);
        tips.ripen("back", t0, at(t0, 400));
        // Leave one, enter the next (either order): straight across, no fade overlap.
        tips.hover("back", false, at(t0, 900));
        assert_eq!(tips.hover("forward", true, at(t0, 900)), None);
        let (id, o, _, moving) = tips.phase(at(t0, 900), false).unwrap();
        assert_eq!((id, o, moving), ("forward", 1., false));
        assert_eq!(tips.hover("reload", true, at(t0, 950)), None);
        tips.hover("forward", false, at(t0, 950));
        assert_eq!(tips.phase(at(t0, 950), false).unwrap().0, "reload");
        // Off the toolbar: it fades over 100 ms...
        tips.hover("reload", false, at(t0, 1000));
        let (id, o, _, _) = tips.phase(at(t0, 1050), false).unwrap();
        assert!(id == "reload" && o > 0. && o < 1.);
        assert!(tips.phase(at(t0, 1101), false).is_none());
        // ...back within the warm-up: instant; after it: the delay again.
        assert_eq!(tips.hover("back", true, at(t0, 1400)), None);
        tips.hover("back", false, at(t0, 1450));
        assert_eq!(tips.hover("forward", true, at(t0, 2000)), Some(TIP_DELAY));
    }

    #[test]
    fn a_click_hides_the_tooltip_until_the_pointer_leaves() {
        let t0 = Instant::now();
        let mut tips = HoverTips::default();
        tips.hover("reload", true, t0);
        tips.ripen("reload", t0, at(t0, 400));
        tips.press("reload", at(t0, 500));
        assert!(tips.phase(at(t0, 700), false).is_none());
        // Still on it: nothing comes back; a click doesn't warm the next one.
        assert_eq!(tips.hover("reload", true, at(t0, 800)), None);
        assert!(tips.phase(at(t0, 800), false).is_none());
        tips.hover("reload", false, at(t0, 900));
        assert_eq!(tips.hover("reload", true, at(t0, 1000)), Some(TIP_DELAY));
    }

    #[test]
    fn reduced_motion_shows_and_hides_without_fades() {
        let t0 = Instant::now();
        let mut tips = HoverTips::default();
        tips.hover("back", true, t0);
        tips.ripen("back", t0, at(t0, 400));
        assert_eq!(tips.phase(at(t0, 401), true), Some(("back", 1., 0., false)));
        tips.hover("back", false, at(t0, 500));
        assert!(tips.phase(at(t0, 501), true).is_none());
    }
}

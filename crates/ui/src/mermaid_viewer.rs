//! The expanded mermaid diagram: pan and zoom. Pinch or ⌘-scroll zooms around
//! the pointer, scroll or drag pans, `+` `-` `0` from the keyboard.
//!
//! Two rasters paint it. A whole-diagram one (fit size, 2x headroom) is there
//! at once and follows every frame of a gesture; once the view rests, a crisp
//! one of just the visible part is drawn at screen resolution and laid on top.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Bounds, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Point, ScrollDelta, Styled, div,
    point, px, size,
};
use keel_theme::Tokens;

use crate::mermaid::{self, Region};

const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 24.;
/// A gesture this recent is still moving: hold off the crisp redraw.
const SETTLE: Duration = Duration::from_millis(150);

/// Where the diagram sits. `zoom` is relative to fit-to-window; `pan` moves
/// its center off the window's center, in px.
pub struct Viewport {
    pub zoom: f32,
    pub pan: Point<Pixels>,
    /// The panel, as last painted: the frame for pointer math.
    bounds: Bounds<Pixels>,
    drag: Option<Point<Pixels>>,
    moved: Instant,
}

impl Default for Viewport {
    fn default() -> Self {
        Self { zoom: 1., pan: Point::default(), bounds: Bounds::default(), drag: None, moved: Instant::now() }
    }
}

pub type SharedViewport = Rc<RefCell<Viewport>>;

impl Viewport {
    /// Diagram px → screen px.
    pub fn scale(&self, (w, h): (f32, f32)) -> f32 {
        let fit = (f32::from(self.bounds.size.width) / w).min(f32::from(self.bounds.size.height) / h);
        fit * self.zoom
    }

    /// The diagram's rectangle on screen.
    fn rect(&self, natural: (f32, f32)) -> Bounds<Pixels> {
        let s = self.scale(natural);
        let size = size(px(natural.0 * s), px(natural.1 * s));
        let center = self.bounds.center() + self.pan;
        Bounds::new(center - point(size.width / 2., size.height / 2.), size)
    }

    /// Zoom by `factor`, keeping the diagram point under `at` where it is.
    pub fn zoom_at(&mut self, factor: f32, at: Point<Pixels>, natural: (f32, f32)) {
        let before = self.rect(natural);
        let s0 = self.scale(natural);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let s1 = self.scale(natural);
        let k = s1 / s0;
        // Same diagram point under `at`: the origin moves toward `at` by (1 - k).
        let origin = at - (at - before.origin) * k;
        let size = size(before.size.width * k, before.size.height * k);
        let center = origin + point(size.width / 2., size.height / 2.);
        self.pan = center - self.bounds.center();
        self.moved = Instant::now();
    }

    pub fn zoom_center(&mut self, factor: f32, natural: (f32, f32)) {
        self.zoom_at(factor, self.bounds.center(), natural);
    }

    pub fn pan_by(&mut self, delta: Point<Pixels>) {
        self.pan = self.pan + delta;
        self.moved = Instant::now();
    }

    pub fn reset(&mut self) {
        self.zoom = 1.;
        self.pan = Point::default();
        self.moved = Instant::now();
    }
}

/// The interactive canvas. Fills its parent.
pub fn viewer(source: String, t: Tokens, vp: SharedViewport) -> impl IntoElement {
    let natural = mermaid::size(&source).unwrap_or((1., 1.));
    let paint_vp = vp.clone();
    let (wheel, pinch, down, moving, up) = (vp.clone(), vp.clone(), vp.clone(), vp.clone(), vp);
    div()
        .id("mermaid-viewer")
        .size_full()
        .overflow_hidden()
        .cursor_grab()
        .on_scroll_wheel(move |e, window, _| {
            let mut vp = wheel.borrow_mut();
            let delta = match e.delta {
                ScrollDelta::Pixels(p) => p,
                ScrollDelta::Lines(l) => point(px(l.x * 20.), px(l.y * 20.)),
            };
            if e.modifiers.platform || e.modifiers.control {
                vp.zoom_at((-f32::from(delta.y) * 0.01).exp(), e.position, natural);
            } else {
                vp.pan_by(delta);
            }
            window.refresh();
        })
        .on_pinch(move |e, window, _| {
            pinch.borrow_mut().zoom_at(1. + e.delta, e.position, natural);
            window.refresh();
        })
        .on_mouse_down(MouseButton::Left, move |e, _, _| down.borrow_mut().drag = Some(e.position))
        .on_mouse_move(move |e, window, _| {
            let mut vp = moving.borrow_mut();
            if let Some(last) = vp.drag {
                if e.pressed_button != Some(MouseButton::Left) {
                    vp.drag = None;
                    return;
                }
                vp.pan_by(e.position - last);
                vp.drag = Some(e.position);
                window.refresh();
            }
        })
        .on_mouse_up(MouseButton::Left, move |_, _, _| up.borrow_mut().drag = None)
        .child(
            gpui::canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    paint_vp.borrow_mut().bounds = bounds;
                    let vp = paint_vp.borrow();
                    window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                        paint(&source, &t, &vp, natural, window)
                    });
                },
            )
            .size_full(),
        )
}

fn paint(source: &str, t: &Tokens, vp: &Viewport, natural: (f32, f32), window: &mut gpui::Window) {
    let sf = window.scale_factor();
    let rect = vp.rect(natural);
    let s = vp.scale(natural);
    let mut pending = false;
    let mut draw = |raster: mermaid::Raster, window: &mut gpui::Window| -> Option<Region> {
        if let Some(stale) = raster.stale {
            let _ = window.drop_image(stale);
        }
        pending |= raster.pending;
        let (region, image) = raster.image?;
        // Place by the region it shows, so a stale raster still lines up.
        let at = Bounds::new(
            rect.origin + point(px(region.x * s), px(region.y * s)),
            size(px(region.w * s), px(region.h * s)),
        );
        let _ = window.paint_image(at, Default::default(), image, 0, false);
        Some(region)
    };

    // Whole diagram, 2x the fit size: sharp enough for the first zoom steps.
    let fit_w = f32::from(vp.bounds.size.width).min(natural.0 * f32::from(vp.bounds.size.height) / natural.1);
    let base_w = (fit_w * sf * 2.).min(4096.);
    let base = draw(mermaid::raster(source, t, base_w, "viewer"), window);

    // Zoomed past the base: the visible part at screen resolution, once still.
    let needs_crisp = rect.size.width * sf > px(base.map_or(0, |r| r.pw) as f32 * 1.05);
    if needs_crisp {
        if vp.moved.elapsed() < SETTLE {
            if let Some(shown) = mermaid::shown(source, t, "viewer-crisp") {
                draw(mermaid::Raster { image: Some(shown), stale: None, pending: false }, window);
            }
            pending = true;
        } else if let Some(visible) = rect.intersect(&vp.bounds).nonempty() {
            let region = Region {
                x: f32::from(visible.origin.x - rect.origin.x) / s,
                y: f32::from(visible.origin.y - rect.origin.y) / s,
                w: f32::from(visible.size.width) / s,
                h: f32::from(visible.size.height) / s,
                pw: (f32::from(visible.size.width) * sf).ceil() as u32,
                ph: (f32::from(visible.size.height) * sf).ceil() as u32,
            };
            draw(mermaid::raster_region(source, t, region, "viewer-crisp"), window);
        }
    }
    if pending {
        window.request_animation_frame();
    }
}

trait NonEmpty: Sized {
    fn nonempty(self) -> Option<Self>;
}

impl NonEmpty for Bounds<Pixels> {
    fn nonempty(self) -> Option<Self> {
        (self.size.width > px(1.) && self.size.height > px(1.)).then_some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> Viewport {
        Viewport { bounds: Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(800.))), ..Default::default() }
    }

    #[test]
    fn starts_fit_and_centered() {
        let v = vp();
        let r = v.rect((2000., 800.));
        assert_eq!(r.size.width, px(1000.));
        assert_eq!(r.center(), v.bounds.center());
    }

    #[test]
    fn zoom_keeps_the_point_under_the_pointer() {
        let mut v = vp();
        let natural = (2000., 800.);
        let at = point(px(250.), px(300.));
        let under = |v: &Viewport| {
            let r = v.rect(natural);
            ((at.x - r.origin.x) / v.scale(natural), (at.y - r.origin.y) / v.scale(natural))
        };
        let before = under(&v);
        v.zoom_at(3., at, natural);
        let after = under(&v);
        assert!((before.0 - after.0).abs() < px(0.01) && (before.1 - after.1).abs() < px(0.01), "{before:?} {after:?}");
        assert_eq!(v.zoom, 3.);
    }

    #[test]
    fn zoom_is_clamped_and_reset_recenters() {
        let mut v = vp();
        v.zoom_center(1000., (100., 100.));
        assert_eq!(v.zoom, MAX_ZOOM);
        v.pan_by(point(px(40.), px(-10.)));
        v.reset();
        assert_eq!((v.zoom, v.pan), (1., Point::default()));
    }
}

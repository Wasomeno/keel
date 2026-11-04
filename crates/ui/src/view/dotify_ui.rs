//! Dotify's look: the pill's content, the
//! popover (header, banners, idle / clocked-in / result / edit views,
//! stats), the receipt pill, the right-click items and the Settings card.
//! All motion is timestamp-driven: each frame computes its look from the
//! time since an event (curves in `crate::dotify`), and asks for another
//! frame while something is still moving. No scale; reduced motion lands
//! at once with short fades.

use crate::hover_fade::HoverFadeExt;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, BoxShadow, ClickEvent, Context, FontWeight, HighlightStyle, Hsla, MouseDownEvent,
    Render, SharedString, Window, canvas, div, point, prelude::*, px,
};
use keel_proto::dotify::{
    DotifyError, Entry, Project, Tracker, hours_label, local_hm, parse_hex_color,
};
use keel_theme::Tokens;

use super::KeelWindow;
use super::dotify::{AnimW, Primary, PrimaryPhase, SPIN_MIN_MS, clock, ms_since};
use super::menu_item;
use crate::dotify::{
    self as dm, Badge, Dot, Look, PillLook, Section, Tween, View, fade, in_out, in_work_hours,
    local_offset_s, out_quint, span,
};
use crate::icons::Icon;
use crate::style::{MONO, c, zpx};
use crate::timer::{clock_label, now_ms};

// ── Ink ────────────────────────────────────────────────────────────────────

/// "White x%" in the dark theme; the text color at x% in either.
fn ink(t: Tokens, a: f32) -> Hsla {
    c(t.text).opacity(a)
}

fn is_dark(t: Tokens) -> bool {
    let w = t.window;
    let (r, g, b) = (
        ((w >> 24) & 0xFF) as f32,
        ((w >> 16) & 0xFF) as f32,
        ((w >> 8) & 0xFF) as f32,
    );
    (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255. < 0.5
}

/// "Black x%" (wells, the stats strip) in the dark theme; a faint ink well in light.
fn shade(t: Tokens, a: f32) -> Hsla {
    if is_dark(t) {
        gpui::black().opacity(a)
    } else {
        ink(t, a * 0.12)
    }
}

fn mix(a: Hsla, b: Hsla, x: f32) -> Hsla {
    let (a, b) = (gpui::Rgba::from(a), gpui::Rgba::from(b));
    let x = x.clamp(0., 1.);
    let l = |p: f32, q: f32| p + (q - p) * x;
    Hsla::from(gpui::Rgba {
        r: l(a.r, b.r),
        g: l(a.g, b.g),
        b: l(a.b, b.b),
        a: l(a.a, b.a),
    })
}

fn place(el: impl IntoElement, look: Look) -> gpui::Div {
    div()
        .relative()
        .top(zpx(look.dy))
        .opacity(look.opacity)
        .child(el)
}

fn spinner(size: f32, color: Hsla, ms: f32, period: f32, reduced: bool) -> AnyElement {
    if reduced {
        return div()
            .opacity(0.4)
            .text_color(color)
            .child(Icon::Spinner.el(zpx(size)))
            .into_any_element();
    }
    div()
        .flex_none()
        .text_color(color)
        .child(
            Icon::Spinner
                .el(zpx(size))
                .with_transformation(gpui::Transformation::rotate(gpui::percentage(
                    dm::spinner_turns(ms, period),
                ))),
        )
        .into_any_element()
}

fn key_chip(label: &'static str, bg: Hsla, fg: Hsla) -> gpui::Div {
    div()
        .flex_none()
        .min_w(zpx(18.))
        .h(zpx(18.))
        .px(zpx(4.))
        .ml(zpx(6.))
        .rounded(zpx(4.))
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .font_family(MONO)
        .text_size(zpx(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(fg)
        .child(label)
}

fn square(t: Tokens, p: Option<&Project>, size: f32) -> gpui::Div {
    let color = p
        .and_then(|p| p.color.as_deref())
        .and_then(parse_hex_color)
        .map(c)
        .unwrap_or(ink(t, 0.2));
    div().flex_none().size(zpx(size)).rounded(zpx(2.)).bg(color)
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

fn range_label(e: &Entry, offset: i64) -> String {
    match e.end_ms {
        Some(end) => format!("{}–{}", local_hm(e.start_ms, offset), local_hm(end, offset)),
        None => format!("{}–", local_hm(e.start_ms, offset)),
    }
}

/// A plain text tooltip. Give the element `.tooltip_show_delay(
/// tooltip::delay())` too, so a neighbour's shows at once.
pub(crate) struct Tip {
    text: SharedString,
    t: Tokens,
    entrance: super::tooltip::Entrance,
}

impl Tip {
    pub(crate) fn new(text: impl Into<SharedString>, t: Tokens) -> Self {
        Self {
            text: text.into(),
            t,
            entrance: super::tooltip::Entrance::new(),
        }
    }
}

impl Render for Tip {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // The same capsule as the toolbars' hover tooltips, and the same entrance.
        let (opacity, dy, moving) = self.entrance.look(std::time::Instant::now());
        if moving {
            window.request_animation_frame();
        }
        super::tooltip::capsule(self.t, self.text.clone(), None)
            .top(crate::style::zpx(dy))
            .opacity(opacity)
    }
}

/// Ease a measured width (`vertical`: height) toward the natural one.
fn anim_size(
    aw: &mut AnimW,
    inner: impl IntoElement,
    ms: f32,
    curve: fn(f32) -> f32,
    animate: bool,
    vertical: bool,
    want: &mut bool,
) -> gpui::Div {
    anim_size_keyed(aw, None, inner, ms, curve, animate, vertical, want)
}

/// [`anim_size`] for content that says what it is (`key`): the size only
/// eases when the content changes, never for the pixel-snap wobble while
/// the box itself moves (the header pill during the sidebar's glide).
#[allow(clippy::too_many_arguments)]
fn anim_size_keyed(
    aw: &mut AnimW,
    key: Option<u64>,
    inner: impl IntoElement,
    ms: f32,
    curve: fn(f32) -> f32,
    animate: bool,
    vertical: bool,
    want: &mut bool,
) -> gpui::Div {
    let measured = aw.measured.get();
    let now = clock();
    if key.is_some() && key != aw.content_key {
        aw.content_key = key;
        aw.key_at = now;
    }
    let follows =
        |to: f32| crate::view::dotify::size_follows(measured - to, key.is_some(), now - aw.key_at);
    if measured > 0. {
        match &mut aw.tween {
            None => aw.tween = Some(Tween::settled(measured)),
            Some(tw) => {
                if follows(tw.to) {
                    tw.retarget(measured, now, ms, curve, animate);
                }
            }
        }
    }
    let rec = aw.measured.clone();
    let probe = canvas(
        move |b, _, _| {
            rec.set(f32::from(if vertical {
                b.size.height
            } else {
                b.size.width
            }))
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0();
    let inner = if vertical {
        div()
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .child(inner)
            .child(probe)
    } else {
        div()
            .relative()
            .flex()
            .flex_none()
            .whitespace_nowrap()
            .child(inner)
            .child(probe)
    };
    // Always held at the tween, even settled: new content is measured a
    // frame late, and drawing it free for that frame pops it to full size.
    match aw.tween {
        Some(tw) => {
            if tw.moving(now) || (measured > 0. && follows(tw.to)) {
                *want = true;
            }
            let v = px(tw.value(now));
            // Flex, so `inner` keeps its own width under the clip: as a block
            // child it would stretch to `v`, and measuring that feeds the tween
            // its own half-pixel-snapped value (132 ↔ 131.5 forever).
            if vertical {
                div().w_full().overflow_hidden().h(v).child(inner)
            } else {
                div().flex().flex_none().overflow_hidden().w(v).child(inner)
            }
        }
        None => {
            if vertical {
                div().w_full().child(inner)
            } else {
                div().flex_none().child(inner)
            }
        }
    }
}

/// A primary button: label (+ key chip); loading spinner, success check,
/// error border flash; width held while busy; colors blend on enable.
#[allow(clippy::too_many_arguments)]
fn primary_button(
    id: &'static str,
    label: &'static str,
    key: Option<&'static str>,
    enabled: bool,
    st: &mut Primary,
    t: Tokens,
    reduced: bool,
    want: &mut bool,
) -> gpui::Stateful<gpui::Div> {
    let now = Instant::now();
    match st.enabled {
        Some((was, _)) if was == enabled => {}
        Some(_) => st.enabled = Some((enabled, now)),
        None => st.enabled = Some((enabled, now - Duration::from_secs(1))),
    }
    let flip = ms_since(st.enabled.map(|e| e.1));
    let x = if reduced {
        span(flip, 0., 120.)
    } else {
        out_quint(span(flip, 0., 160.))
    };
    if flip < 200. {
        *want = true;
    }
    let (on_bg, off_bg) = (ink(t, 0.92), ink(t, 0.10));
    let (on_fg, off_fg) = (c(t.window), ink(t, 0.35));
    let bg = if enabled {
        mix(off_bg, on_bg, x)
    } else {
        mix(on_bg, off_bg, x)
    };
    let fg = if enabled {
        mix(off_fg, on_fg, x)
    } else {
        mix(on_fg, off_fg, x)
    };
    let (label_o, spin_o, check_o, flash) = match st.phase {
        PrimaryPhase::Idle => (1., 0., 0., 0.),
        PrimaryPhase::Loading(at) => {
            let ms = ms_since(Some(at));
            *want = true;
            (
                1. - fade(ms, 0., 120., reduced),
                fade(ms, 0., 120., reduced),
                0.,
                0.,
            )
        }
        PrimaryPhase::Success(at) => {
            let ms = ms_since(Some(at));
            if ms < 200. {
                *want = true;
            }
            let x = fade(ms, 0., 140., reduced);
            (0., 1. - x, x, 0.)
        }
        PrimaryPhase::Error(at) => {
            let ms = ms_since(Some(at));
            if ms >= 600. {
                st.phase = PrimaryPhase::Idle;
                (1., 0., 0., 0.)
            } else {
                *want = true;
                (fade(ms, 0., 120., reduced), 0., 0., dm::error_flash(ms))
            }
        }
    };
    let busy = st.busy();
    let rec = st.width.clone();
    let held = st.width.get();
    let chip_bg = c(t.window).opacity(0.12);
    let chip_fg = c(t.window).opacity(0.6);
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .opacity(label_o)
        .child(label)
        .when_some(key.filter(|_| enabled), |d, k| {
            d.child(key_chip(k, chip_bg, chip_fg))
        });
    let ms = match st.phase {
        PrimaryPhase::Loading(at) => ms_since(Some(at)),
        _ => 0.,
    };
    div()
        .id(id)
        .relative()
        .flex_none()
        .h(zpx(32.))
        .px(zpx(14.))
        .rounded(zpx(8.))
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .border_1()
        .border_color(if flash > 0. {
            c(t.danger).opacity(flash)
        } else {
            gpui::transparent_black()
        })
        .text_size(zpx(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(fg)
        .when(busy && held > 0., |d| d.w(px(held)))
        .when(enabled && !busy, |d| {
            d.cursor_pointer()
                .hover(|s| s.opacity(0.92))
                .active(|s| s.opacity(0.84))
        })
        .when(!busy, |d| {
            d.child(
                canvas(
                    move |b, _, _| rec.set(f32::from(b.size.width)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
        })
        .child(row)
        .when(spin_o > 0. || check_o > 0., |d| {
            d.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .opacity(spin_o)
                            .child(spinner(14., fg, ms, 800., reduced)),
                    )
                    .child(
                        div()
                            .absolute()
                            .opacity(check_o)
                            .text_color(fg)
                            .child(Icon::Check.el(zpx(14.))),
                    ),
            )
        })
}

/// Secondary / two-click confirm button (Clock out, Disconnect).
#[allow(clippy::too_many_arguments)]
fn confirm_button(
    id: &'static str,
    idle: &'static str,
    armed_label: &'static str,
    ghost: bool,
    confirm: &dm::Confirm,
    busy: Option<Instant>,
    aw: &mut AnimW,
    t: Tokens,
    reduced: bool,
    want: &mut bool,
) -> gpui::Stateful<gpui::Div> {
    let now = now_ms();
    let armed = confirm.armed(now);
    let since = confirm.armed_at().map_or(1e9, |at| (now - at) as f32);
    let x = if armed {
        *want = true;
        fade(since, 0., 140., reduced)
    } else if since >= dm::CONFIRM_MS as f32 && since < dm::CONFIRM_MS as f32 + 160. {
        *want = true;
        1. - fade(since - dm::CONFIRM_MS as f32, 0., 160., reduced)
    } else {
        0.
    };
    let red = c(t.danger);
    let (bg0, bd0, fg0) = if ghost {
        (
            gpui::transparent_black(),
            gpui::transparent_black(),
            ink(t, 0.65),
        )
    } else {
        (ink(t, 0.07), ink(t, 0.10), ink(t, 0.88))
    };
    let bg = mix(bg0, red.opacity(0.16), x);
    let bd = mix(bd0, red.opacity(0.35), x);
    let fg = mix(fg0, red.opacity(0.95), x);
    let label: &'static str = if armed {
        if reduced {
            "Click again to confirm"
        } else {
            armed_label
        }
    } else {
        idle
    };
    let label_o = if armed {
        fade(since, 0., 120., reduced)
    } else {
        1.
    };
    let remaining = confirm.remaining(now, reduced);
    let label_el = div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(6.))
        .opacity(if busy.is_some() { 0. } else { label_o })
        .child(label)
        .when(armed, |d| {
            d.child(
                div()
                    .absolute()
                    .left_0()
                    .bottom(zpx(-4.))
                    .h(zpx(2.))
                    .rounded(zpx(1.))
                    .w(gpui::relative(remaining))
                    .bg(red.opacity(0.6)),
            )
        });
    let inner = anim_size(aw, label_el, 160., out_quint, !reduced, false, want);
    div()
        .id(id)
        .relative()
        .flex_none()
        .h(zpx(32.))
        .px(zpx(12.))
        .rounded(zpx(8.))
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .border_1()
        .border_color(bd)
        .text_size(zpx(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(fg)
        .cursor_pointer()
        .when(!armed && ghost, |d| {
            d.hover(move |s| s.bg(red.opacity(0.12)).text_color(red.opacity(0.95)))
        })
        .when(!armed && !ghost, |d| d.hover(move |s| s.bg(ink(t, 0.11))))
        .child(inner)
        .when_some(busy, |d, at| {
            *want = true;
            d.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(spinner(14., fg, ms_since(Some(at)), 800., reduced)),
            )
        })
}

fn secondary_button(
    id: &'static str,
    label: &'static str,
    busy: Option<Instant>,
    t: Tokens,
    reduced: bool,
    want: &mut bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .relative()
        .flex_none()
        .h(zpx(32.))
        .px(zpx(12.))
        .rounded(zpx(8.))
        .flex()
        .items_center()
        .justify_center()
        .bg(ink(t, 0.07))
        .border_1()
        .border_color(ink(t, 0.10))
        .text_size(zpx(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink(t, 0.88))
        .cursor_pointer()
        .hover_fade(ink(t, 0.11), None)
        .child(
            div()
                .opacity(if busy.is_some() { 0. } else { 1. })
                .child(label),
        )
        .when_some(busy, |d, at| {
            *want = true;
            d.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(spinner(14., ink(t, 0.7), ms_since(Some(at)), 800., reduced)),
            )
        })
}

fn ghost_button(id: &'static str, label: &'static str, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .h(zpx(32.))
        .px(zpx(12.))
        .rounded(zpx(8.))
        .flex()
        .items_center()
        .text_size(zpx(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink(t, 0.65))
        .cursor_pointer()
        .hover(|s| s.text_color(ink(t, 0.85)))
        .hover_fade(ink(t, 0.07), None)
        .child(label)
}

fn link(id: &'static str, label: &'static str, color: Hsla) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .text_size(zpx(12.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .cursor_pointer()
        .hover(|s| s.opacity(0.8))
        .child(label)
}

impl KeelWindow {
    // ── Pill ─────────────────────────────────────────────────────────────

    /// The pill's content while Dotify is connected (`live`: the real pill,
    /// not the drag ghost).
    pub(super) fn dotify_pill_content(
        &mut self,
        t: Tokens,
        hovered: bool,
        pressed: bool,
        live: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if live {
            self.dotify_ensure_tick(cx);
        }
        let reduced = self.reduced();
        let offset = local_offset_s();
        let remind = self.state.ui_settings.dotify_nudge
            && in_work_hours(now_ms(), offset)
            && self.dotify.tracker == Tracker::Idle;
        let look = if self.dotify_mode() {
            dm::pill_look(
                &self.dotify.tracker,
                self.dotify.pending.as_ref(),
                self.dotify.sync_error.as_ref(),
                self.dotify.overtime,
                remind,
            )
        } else {
            PillLook::Connect
        };
        let key = match look {
            PillLook::Connect => 5,
            PillLook::Idle { .. } => 1,
            PillLook::Reconnect => 2,
            PillLook::Running { .. } => 3,
            PillLook::Pending { .. } => 4,
        };
        if live && key != self.dotify.pill_key {
            let first = self.dotify.pill_key == 0;
            self.dotify.pill_key_from = self.dotify.pill_key;
            self.dotify.pill_key = key;
            self.dotify.pill_look_at = Some(if first {
                Instant::now() - Duration::from_secs(10)
            } else {
                Instant::now()
            });
        }
        let ms_look = ms_since(self.dotify.pill_look_at);
        let mut want = false;

        // Manual sync: a spinner in place of the dot / icon (at least 400 ms).
        let spin_ms = self.dotify.manual_spin.map(|s| ms_since(Some(s)));
        let spinning = spin_ms.is_some_and(|ms| self.dotify.syncing || ms < SPIN_MIN_MS);
        if spinning {
            want = true;
        }
        let spin_el = |ms: f32| spinner(10., ink(t, 0.6), ms, 900., reduced);

        let project = match (&look, &self.dotify.tracker) {
            (PillLook::Running { .. }, Tracker::Running(e)) => e
                .project
                .as_ref()
                .map(|p| p.name.clone())
                .filter(|n| !n.is_empty()),
            _ => None,
        };
        let pname = project.clone().unwrap_or_default();
        if live && pname != self.dotify.pill_project {
            self.dotify.pill_project = pname;
            self.dotify.pill_project_at = Some(Instant::now());
        }

        let row = div()
            .h(zpx(26.))
            .px(zpx(10.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(6.));
        let label_text = |s: &'static str, color: Hsla| {
            div()
                .text_size(zpx(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(color)
                .child(s)
        };
        let content = match look {
            PillLook::Idle { badge } => {
                let icon = div()
                    .relative()
                    .flex_none()
                    .text_color(ink(t, 0.55))
                    .child(if spinning {
                        spin_el(spin_ms.unwrap_or(0.))
                    } else {
                        Icon::Timer.el(zpx(14.)).into_any_element()
                    })
                    .when_some(badge, |d, b| {
                        let color = match b {
                            Badge::Amber => c(t.warning),
                            Badge::Grey => ink(t, 0.35),
                        };
                        d.child(
                            div()
                                .absolute()
                                .top(zpx(-1.))
                                .right(zpx(-1.))
                                .size(zpx(5.))
                                .rounded_full()
                                .bg(color),
                        )
                    });
                row.child(icon).child(label_text("Clock in", ink(t, 0.70)))
            }
            PillLook::Connect => row
                .child(
                    div()
                        .flex_none()
                        .text_color(ink(t, 0.55))
                        .child(Icon::Timer.el(zpx(14.))),
                )
                .child(label_text("Connect Dotify", ink(t, 0.70))),
            PillLook::Reconnect => row
                .child(
                    div()
                        .flex_none()
                        .text_color(c(t.danger).opacity(0.9))
                        .child(Icon::Timer.el(zpx(14.))),
                )
                .child(label_text("Reconnect Dotify", c(t.danger).opacity(0.9))),
            PillLook::Running { dot, dim, overtime } => {
                let (elapsed, since_run) = match &self.dotify.tracker {
                    Tracker::Running(e) => (
                        e.elapsed_ms(now_ms()),
                        (now_ms() - e.start_ms).max(0) as f32,
                    ),
                    _ => (0, 0.),
                };
                let color = match dot {
                    Dot::Green => c(t.success),
                    Dot::Amber => c(t.warning),
                    Dot::Grey => ink(t, 0.35),
                    Dot::Red => c(t.danger),
                };
                // Breathing starts 600 ms into this look; still while unfocused.
                let breathes = look.breathes() && self.window_active;
                let breath = if breathes {
                    dm::breathe(ms_look.min(since_run), reduced)
                } else {
                    1.
                };
                let entering = live && self.dotify.pill_key_from == 1 && ms_look < 400.;
                let dot_in = if entering {
                    fade(ms_look, 200., 160., reduced)
                } else {
                    1.
                };
                if entering {
                    want = true;
                }
                let dot_el = if spinning {
                    spin_el(spin_ms.unwrap_or(0.))
                } else {
                    div()
                        .flex_none()
                        .size(zpx(6.))
                        .rounded_full()
                        .bg(color)
                        .opacity(breath * dot_in)
                        .into_any_element()
                };
                let time_color = if overtime {
                    ink(t, 1.0)
                } else if dim {
                    ink(t, 0.70)
                } else {
                    ink(t, 0.92)
                };
                let p_fade = fade(ms_since(self.dotify.pill_project_at), 0., 140., reduced);
                if p_fade < 1. {
                    want = true;
                }
                row.child(div().w(zpx(10.)).flex().justify_center().child(dot_el))
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(zpx(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(time_color)
                            .child(clock_label(elapsed)),
                    )
                    .when_some(project, |d, p| {
                        d.child(
                            div()
                                .text_size(zpx(12.5))
                                .text_color(ink(t, 0.25))
                                .child("·"),
                        )
                        .child(
                            div()
                                .max_w(zpx(120.))
                                .truncate()
                                .text_size(zpx(12.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(ink(t, 0.55))
                                .opacity(p_fade)
                                .child(p),
                        )
                    })
            }
            PillLook::Pending { label } => {
                let wait = ms_since(self.dotify.pending_at);
                want = true;
                let lead = if dm::spinner_visible(wait) {
                    spinner(10., ink(t, 0.6), wait, 900., reduced)
                } else {
                    div().size(zpx(10.)).into_any_element()
                };
                row.child(lead).child(
                    div()
                        .opacity(fade(wait, 0., 120., reduced))
                        .child(label_text(label, ink(t, 0.60))),
                )
            }
        };

        // New content fades in, rising 3 px from 120 ms, after a change.
        let changed = live && self.dotify.pill_key_from != 0 && ms_look < 320.;
        let content = if changed {
            want = true;
            if reduced {
                place(
                    content,
                    Look {
                        opacity: fade(ms_look, 0., 120., true),
                        dy: 0.,
                    },
                )
            } else {
                let e = out_quint(span(ms_look, 120., 160.));
                place(
                    content,
                    Look {
                        opacity: e,
                        dy: 3. * (1. - e),
                    },
                )
            }
        } else {
            div().child(content)
        };

        // Width: idle ↔ running 240 / 200 INOUT; anything else 160 OUT.
        let (dur, curve): (f32, fn(f32) -> f32) = match (self.dotify.pill_key_from, key) {
            (1, 3) => (240., in_out),
            (3, 1) => (200., in_out),
            _ => (160., out_quint),
        };
        // Keyed by what the pill shows, so only a content change eases its
        // width; the header gliding with the sidebar never does.
        let content_key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut h);
            self.dotify.pill_project.hash(&mut h);
            if let PillLook::Pending { label } = &look {
                label.hash(&mut h);
            }
            h.finish()
        };
        let content = if live {
            anim_size_keyed(
                &mut self.dotify.pill_w,
                Some(content_key),
                content,
                dur,
                curve,
                !reduced,
                false,
                &mut want,
            )
        } else {
            content.flex_none()
        };

        // Chrome: overlay alphas by state; the open state blends over 100 ms.
        let open = self.dotify.open && self.dotify.close_at.is_none();
        let ox = if reduced {
            1.
        } else {
            span(ms_since(self.dotify.open_toggled_at), 0., 100.)
        };
        if ox < 1. {
            want = true;
        }
        let open_x = if open { ox } else { 1. - ox };
        let base_bg = if pressed {
            0.10
        } else if hovered {
            0.08
        } else {
            0.05
        };
        let mut bg = mix(ink(t, base_bg), ink(t, 0.10), open_x);
        let mut border = mix(ink(t, 0.08), ink(t, 0.14), open_x);
        let mut glow = 0.;
        if let PillLook::Running { overtime: true, .. } = look {
            let (tint, g) = dm::overtime_in(ms_since(self.dotify.overtime_at), reduced);
            if tint < 1. || g > 0. {
                want = true;
            }
            bg = mix(bg, c(t.warning).opacity(0.08), tint);
            border = mix(border, c(t.warning).opacity(0.22), tint);
            glow = g;
        }
        let tip: Option<String> = match (&look, &self.dotify.sync_error) {
            (PillLook::Running { overtime: true, .. }, _) => {
                Some(match self.dotify.overtime_wall_ms {
                    Some(at) => format!("Over by {}", short_clock(now_ms() - at)),
                    None => "Overtime".to_string(),
                })
            }
            (_, Some(DotifyError::Offline(_) | DotifyError::Server(_))) => {
                Some("Offline · will sync".into())
            }
            (_, Some(DotifyError::Auth(_))) => Some("Dotify token expired".into()),
            _ => None,
        };
        if want && live {
            self.dotify.want_frame = true;
        }
        div()
            .id(if live {
                "dotify-pill"
            } else {
                "dotify-pill-ghost"
            })
            .flex_none()
            .rounded(zpx(13.))
            .bg(c(t.floating & 0xFFFF_FF00 | 0xB3))
            .child(
                div()
                    .rounded(zpx(13.))
                    .bg(bg)
                    .border_1()
                    .border_color(border)
                    .when(glow > 0., |d| {
                        d.shadow(vec![BoxShadow {
                            color: c(t.warning).opacity(glow),
                            offset: point(px(0.), px(0.)),
                            blur_radius: px(0.),
                            spread_radius: zpx(4.),
                            inset: false,
                        }])
                    })
                    .child(content),
            )
            .when_some(tip.filter(|_| live), |d, tip| {
                let text: SharedString = tip.into();
                d.tooltip(move |_, cx| cx.new(|_| Tip::new(text.clone(), t)).into())
                    .tooltip_show_delay(crate::view::tooltip::delay())
            })
            .into_any_element()
    }

    // ── Right-click menu ─────────────────────────────────────────────────

    pub(super) fn dotify_menu_items(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let running = matches!(self.dotify.tracker, Tracker::Running(_));
        let armed = self.dotify.confirm_menu.armed(now_ms());
        if armed {
            self.dotify.want_frame = true;
        }
        let red = c(t.danger);
        let mut items = Vec::new();
        if running {
            items.push(
                menu_item(
                    "timer-menu-clock-out",
                    t,
                    self.dotify.pending.is_none(),
                    false,
                )
                .when(armed, |d| d.text_color(red.opacity(0.95)))
                .child(if armed {
                    "Confirm clock out"
                } else {
                    "Clock out"
                })
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if this.dotify_menu_clock_out(cx) {
                        this.timer_ui_close_menu_only();
                    }
                    cx.notify();
                }))
                .into_any_element(),
            );
        }
        items.push(
            menu_item("timer-menu-dotify", t, true, false)
                .child(if running {
                    "Show Dotify"
                } else {
                    "Clock in…"
                })
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(zpx(11.))
                        .text_color(c(t.text_faint))
                        .child("⌘⇧T"),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.timer_ui_close_menu_only();
                    this.dotify_open_popover(cx);
                }))
                .into_any_element(),
        );
        items.push(
            menu_item("timer-menu-sync", t, true, false)
                .child("Sync with Dotify")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.timer_ui_close_menu_only();
                    this.dotify_manual_sync(cx);
                }))
                .into_any_element(),
        );
        items
    }

    // ── Overlay: popover + receipt ───────────────────────────────────────

    /// The popover and the receipt; asks for the next frame while anything
    /// Dotify draws (pill included) is still moving.
    pub(super) fn render_dotify_overlay(
        &mut self,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let build_start = Instant::now();
        if let Some(p) = self.render_dotify_popover(t, window, cx) {
            out.push(p);
        }
        if let Some(r) = self.render_dotify_receipt(t, cx) {
            out.push(r);
        }
        if let Some(sw) = self.render_dotify_switch(t, cx) {
            out.push(sw);
        }
        if self.dotify.bench.on && self.dotify.open && !self.dotify.bench.reported {
            let open_ms = ms_since(self.dotify.open_at);
            let now = Instant::now();
            let gap = self
                .dotify
                .bench
                .last
                .map_or(0., |l| now.duration_since(l).as_secs_f32() * 1000.);
            let build = now.duration_since(build_start).as_secs_f32() * 1000.;
            self.dotify.bench.last = Some(now);
            if open_ms < 800. {
                tracing::debug!(target: "keel_ui::dotify", open_ms, gap, build, "framelog frame");
                self.dotify.bench.frames.push((open_ms, gap, build));
            } else {
                self.dotify.bench.reported = true;
                let b = &self.dotify.bench;
                let entry: Vec<&(f32, f32, f32)> = b
                    .frames
                    .iter()
                    .filter(|f| f.0 <= dm::ENTRY_DONE_MS + 20.)
                    .collect();
                let mut gaps: Vec<f32> = entry.iter().skip(1).map(|f| f.1).collect();
                gaps.sort_by(|a, b| a.total_cmp(b));
                let span_ms = entry.last().map_or(0., |l| l.0) - entry.first().map_or(0., |f| f.0);
                let fps = if span_ms > 0. {
                    (entry.len().saturating_sub(1)) as f32 * 1000. / span_ms
                } else {
                    0.
                };
                let p95 = gaps
                    .get(((gaps.len() as f32) * 0.95) as usize)
                    .copied()
                    .unwrap_or(0.);
                let max = gaps.last().copied().unwrap_or(0.);
                let avg_build = entry.iter().map(|f| f.2).sum::<f32>() / entry.len().max(1) as f32;
                let max_build = entry.iter().map(|f| f.2).fold(0., f32::max);
                // Label crossfades after entry (should be 0 unless the minute changed).
                tracing::info!(target: "keel_ui::dotify", frames = entry.len(), fps = format!("{fps:.1}"), p95_gap_ms = format!("{p95:.1}"), max_gap_ms = format!("{max:.1}"), avg_build_ms = format!("{avg_build:.2}"), max_build_ms = format!("{max_build:.2}"), label_crossfades = b.label_fades, "framelog popover entry");
            }
        }
        if std::mem::take(&mut self.dotify.want_frame) {
            window.request_animation_frame();
        }
        out
    }

    fn render_dotify_receipt(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let r = self.dotify.receipt.clone()?;
        if self.timer.hidden || !self.dotify_mode() {
            return None;
        }
        let Some(look) = dm::receipt_look(r.ms(), self.reduced()) else {
            self.dotify.receipt = None;
            return None;
        };
        // Paused while hovered: a timer wakes it when the hover ends.
        if r.hover_since.is_none() {
            self.dotify.want_frame = true;
        }
        let pill = self.timer_ui_painted()?;
        let editable = !r.entry.locked && r.entry.end_ms.is_some();
        let el = div()
            .id("dotify-receipt")
            .mt(zpx(4.))
            .h(zpx(28.))
            .px(zpx(12.))
            .rounded(zpx(14.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(6.))
            .bg(c(t.floating))
            .border_1()
            .border_color(ink(t, 0.09))
            .shadow(vec![BoxShadow {
                color: gpui::black().opacity(0.35),
                offset: point(px(0.), zpx(6.)),
                blur_radius: zpx(16.),
                spread_radius: px(0.),
                inset: false,
            }])
            .occlude()
            .text_size(zpx(12.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(ink(t, 0.85))
            .child(
                div()
                    .text_color(c(t.success))
                    .child(Icon::Check.el(zpx(12.))),
            )
            .child(r.text.clone())
            .when(editable, |d| {
                d.child(div().text_color(ink(t, 0.3)).child("·")).child(
                    link("dotify-receipt-edit", "Edit", c(t.accent)).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.dotify_edit_result(cx)),
                    ),
                )
            })
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if let Some(r) = this.dotify.receipt.as_mut() {
                    match (*hovered, r.hover_since) {
                        (true, None) => r.hover_since = Some(Instant::now()),
                        (false, Some(h)) => {
                            r.paused += h.elapsed();
                            r.hover_since = None;
                        }
                        _ => {}
                    }
                }
                cx.notify();
            }));
        let el = place(el, look);
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(pill.left(), pill.bottom()))
                    .snap_to_window_with_margin(zpx(8.))
                    .child(el),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// The switch offer under the header pill (not over the popover or a
    /// receipt): stays until answered.
    fn render_dotify_switch(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let visible = self.dotify.switch.is_some()
            && !self.timer.hidden
            && self.dotify_mode()
            && !self.dotify.open
            && self.dotify.receipt.is_none();
        let leaving = self.menu_phase("dotify-switch", visible, cx)?;
        if visible {
            self.dotify.switch_shown = self.dotify.switch.clone();
        }
        let o = self.dotify.switch_shown.clone()?;
        let reduced = self.reduced();
        if o.busy.is_some() {
            self.dotify.want_frame = true;
        }
        let pill = self.timer_ui_painted()?;
        let busy = o.busy.map(|b| b.elapsed().as_secs_f32() * 1000.);
        let el = div()
            .id("dotify-switch")
            .mt(zpx(4.))
            .min_h(zpx(28.))
            .py(zpx(5.))
            .px(zpx(12.))
            .rounded(zpx(14.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(6.))
            .bg(c(t.floating))
            .border_1()
            .border_color(ink(t, 0.09))
            .shadow(vec![BoxShadow {
                color: gpui::black().opacity(0.35),
                offset: point(px(0.), zpx(6.)),
                blur_radius: zpx(16.),
                spread_radius: px(0.),
                inset: false,
            }])
            .occlude()
            .text_size(zpx(12.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(ink(t, 0.85))
            .child(square(t, Some(&o.project), 8.))
            .child(
                div()
                    .text_color(ink(t, 0.55))
                    .child(format!("In {} ·", crate::dotify::repo_label(&o.repo))),
            )
            .child(format!("Switch to {}?", o.project.name))
            .child(div().text_color(ink(t, 0.3)).child("·"))
            .map(|d| match busy {
                Some(ms) => d
                    .child(spinner(12., c(t.accent), ms, 900., reduced))
                    .child(div().text_color(ink(t, 0.55)).child("Switching…")),
                None => d
                    .child(link("dotify-switch-yes", "Switch", c(t.accent)).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.dotify_switch_accept(cx)),
                    ))
                    .child(
                        link("dotify-switch-later", "Not now", ink(t, 0.6)).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| this.dotify_switch_dismiss(false, cx),
                        )),
                    )
                    .child(
                        link("dotify-switch-never", "Don't ask", ink(t, 0.4)).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.dotify_switch_dismiss(true, cx)
                            }),
                        ),
                    ),
            });
        let el = super::menu_motion(el, "dotify-switch", false, leaving, reduced);
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(pill.left(), pill.bottom()))
                    .snap_to_window_with_margin(zpx(8.))
                    .child(el),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    fn render_dotify_popover(
        &mut self,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.dotify.open || !self.dotify_mode() || self.timer.hidden {
            return None;
        }
        let reduced = self.reduced();
        let mut want = false;
        if std::mem::take(&mut self.dotify.open_fresh) && self.dotify.close_at.is_none() {
            let now = Instant::now();
            self.dotify.open_at = Some(now);
            self.dotify.view_at = Some(now);
        }
        let up = self.timer.dock.is_bottom();
        let frame_look = match self.dotify.close_at {
            Some(at) => {
                let ms = ms_since(Some(at));
                if reduced || ms >= dm::POPOVER_EXIT_MS {
                    self.dotify_finish_close();
                    cx.notify();
                    return None;
                }
                want = true;
                dm::popover_exit(ms, up)
            }
            None => {
                if ms_since(self.dotify.open_at) < 900. {
                    want = true;
                }
                dm::popover_enter(ms_since(self.dotify.open_at), up, reduced)
            }
        };
        let pill = self.timer_ui_painted()?;
        self.dotify_apply_prefill(window, cx);
        let open_ms = ms_since(self.dotify.open_at);
        let view_ms = ms_since(self.dotify.view_at);
        let first_view = view_ms + 5. >= open_ms;
        let view = self.dotify.flow.view;
        let sec = |i: usize| {
            if first_view {
                dm::section_enter(open_ms, i, reduced)
            } else {
                Look {
                    opacity: 1.,
                    dy: 0.,
                }
            }
        };
        let body_ms = if first_view { open_ms } else { view_ms };
        let body_look = move |i: usize| dm::section_enter(body_ms, i, reduced);
        let switch_look = if first_view {
            Look {
                opacity: 1.,
                dy: 0.,
            }
        } else {
            dm::view_enter(view_ms, reduced)
        };
        if !first_view && view_ms < 400. {
            want = true;
        }

        let header = self.dotify_header(t, reduced, &mut want, cx);
        let banners = self.dotify_banners(t, reduced, &mut want, cx);
        let body: Vec<AnyElement> = match view {
            View::Idle | View::Edit => {
                self.dotify_pick_view(t, window, reduced, &body_look, &mut want, cx)
            }
            View::ClockedIn => vec![self.dotify_clocked_in_view(t, reduced, &mut want, cx)],
            View::Result => vec![self.dotify_result_view(
                t,
                reduced,
                if first_view { open_ms } else { view_ms },
                &mut want,
                cx,
            )],
        };
        let stats = self.dotify_stats(t, reduced, &mut want);

        let middle = div()
            .flex()
            .flex_col()
            .children(banners)
            .child(place(div().flex().flex_col().children(body), switch_look));
        // Height: a view switch 220 INOUT; list and banner changes 160 OUT.
        let (dur, curve): (f32, fn(f32) -> f32) = if view_ms < 260. && !first_view {
            (220., in_out)
        } else {
            (160., out_quint)
        };
        let animate = !reduced;
        let middle = anim_size(
            &mut self.dotify.height,
            middle,
            dur,
            curve,
            animate,
            true,
            &mut want,
        );

        let dismiss = cx.listener(|this, _: &MouseDownEvent, _, cx| this.dotify_close_outside(cx));
        let panel = div()
            .id("dotify-popover")
            .w(zpx(320.))
            .rounded(zpx(12.))
            .overflow_hidden()
            .bg(c(t.floating))
            .border_1()
            .border_color(ink(t, 0.09))
            .shadow(vec![
                BoxShadow {
                    color: gpui::black().opacity(if is_dark(t) { 0.45 } else { 0.16 }),
                    offset: point(px(0.), zpx(12.)),
                    blur_radius: zpx(32.),
                    spread_radius: px(0.),
                    inset: false,
                },
                BoxShadow {
                    color: gpui::black().opacity(if is_dark(t) { 0.30 } else { 0.08 }),
                    offset: point(px(0.), zpx(2.)),
                    blur_radius: zpx(6.),
                    spread_radius: px(0.),
                    inset: false,
                },
            ])
            .occlude()
            .on_mouse_down_out(dismiss)
            .flex()
            .flex_col()
            .child(place(header, sec(0)))
            .child(middle)
            .children(stats.map(|s| place(s, sec(5))));
        let panel = place(panel, frame_look);
        let (anchor, at) = if up {
            (
                gpui::Anchor::BottomLeft,
                gpui::point(pill.left(), pill.top() - zpx(6.)),
            )
        } else {
            (
                gpui::Anchor::TopLeft,
                gpui::point(pill.left(), pill.bottom() + zpx(6.)),
            )
        };
        if want {
            self.dotify.want_frame = true;
        }
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(at)
                    .anchor(anchor)
                    .snap_to_window_with_margin(zpx(8.))
                    .child(panel),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    fn dotify_header(
        &mut self,
        t: Tokens,
        reduced: bool,
        want: &mut bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        // The label changes only when its text does, and never while the
        // entry plays (an open-triggered sync waits, then one crossfade).
        let want_label = self.dotify_sync_label_text();
        let now = clock();
        let entering = ms_since(self.dotify.open_at) < dm::ENTRY_DONE_MS;
        if self.dotify.sync_label.update(&want_label, now, entering) {
            if self.dotify.bench.on {
                self.dotify.bench.label_fades += 1;
                tracing::info!(target: "keel_ui::dotify", open_ms = ms_since(self.dotify.open_at), from = %self.dotify.sync_label.prev, to = %self.dotify.sync_label.shown, "framelog label crossfade");
            }
        }
        if entering && want_label != self.dotify.sync_label.shown {
            // Deferred: come back once the entry is done.
            *want = true;
        }
        let (old_o, new_o) = self.dotify.sync_label.opacities(now, reduced);
        if self.dotify.sync_label.fading(now) {
            *want = true;
        }
        let label_el = div()
            .relative()
            .text_size(zpx(11.5))
            .text_color(ink(t, 0.4))
            .when(old_o > 0. && !self.dotify.sync_label.prev.is_empty(), |d| {
                d.child(
                    div()
                        .absolute()
                        .right_0()
                        .top_0()
                        .whitespace_nowrap()
                        .opacity(old_o)
                        .child(self.dotify.sync_label.prev.clone()),
                )
            })
            .child(
                div()
                    .whitespace_nowrap()
                    .opacity(new_o)
                    .child(self.dotify.sync_label.shown.clone()),
            );
        // Refresh: 800 ms a turn, at least 400 ms; then it finishes its turn, easing out.
        let turns = self.dotify.manual_spin.and_then(|start| {
            let ms = ms_since(Some(start));
            match self.dotify.manual_done {
                None => Some(ms / 800.),
                Some(done) => {
                    let done_ms =
                        (done.duration_since(start).as_secs_f32() * 1000.).max(SPIN_MIN_MS);
                    if ms < done_ms {
                        return Some(ms / 800.);
                    }
                    let base = done_ms / 800.;
                    let target = (base + 0.25).ceil();
                    let x = span(ms, done_ms, 400.);
                    (x < 1.).then(|| base + (target - base) * out_quint(x))
                }
            }
        });
        if turns.is_none() && self.dotify.manual_done.is_some() {
            self.dotify.manual_spin = None;
            self.dotify.manual_done = None;
        }
        if turns.is_some() {
            *want = true;
        }
        let icon = Icon::Reload.el(zpx(14.));
        let icon: AnyElement = match turns {
            Some(turns) if !reduced => icon
                .with_transformation(gpui::Transformation::rotate(gpui::percentage(
                    turns.rem_euclid(1.),
                )))
                .into_any_element(),
            Some(_) => div().opacity(0.4).child(icon).into_any_element(),
            None => icon.into_any_element(),
        };
        div()
            .h(zpx(40.))
            .pl(zpx(14.))
            .pr(zpx(12.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(6.))
            .child(
                div()
                    .text_size(zpx(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ink(t, 0.9))
                    .child("Dotify"),
            )
            .child(div().flex_1())
            .child(label_el)
            .child(
                div()
                    .id("dotify-refresh")
                    .size(zpx(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(zpx(6.))
                    .text_color(ink(t, 0.5))
                    .cursor_pointer()
                    .hover_fade(ink(t, 0.08), None)
                    .child(icon)
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.dotify_manual_sync(cx)),
                    ),
            )
    }

    fn dotify_banners(
        &mut self,
        t: Tokens,
        reduced: bool,
        want: &mut bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let red = c(t.danger);
        let banner = |bg: Hsla,
                      border: Hsla,
                      icon: AnyElement,
                      text: AnyElement,
                      action: AnyElement,
                      opacity: f32| {
            div()
                .mx(zpx(12.))
                .mb(zpx(8.))
                .px(zpx(12.))
                .py(zpx(10.))
                .rounded(zpx(8.))
                .bg(bg)
                .border_1()
                .border_color(border)
                .flex()
                .flex_row()
                .items_start()
                .gap(zpx(8.))
                .opacity(opacity)
                .child(div().pt(zpx(1.)).flex_none().child(icon))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(zpx(12.5))
                        .line_height(zpx(17.))
                        .text_color(ink(t, 0.85))
                        .child(text),
                )
                .child(action)
                .into_any_element()
        };
        let open_o = |at: Option<Instant>, want: &mut bool| {
            let (_, o) = dm::banner_open(ms_since(at), reduced);
            if o < 1. {
                *want = true;
            }
            o
        };
        match &self.dotify.sync_error {
            Some(DotifyError::Auth(_)) => {
                let o = open_o(self.dotify.error_at, want);
                out.push(banner(
                    red.opacity(0.10),
                    red.opacity(0.25),
                    div()
                        .text_color(red)
                        .child(Icon::Alert.el(zpx(14.)))
                        .into_any_element(),
                    div()
                        .child("Dotify token expired. Reconnect to keep syncing.")
                        .into_any_element(),
                    link("dotify-open-settings", "Open Settings", ink(t, 0.9))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.dotify_close_popover(cx);
                            this.open_settings(cx);
                        }))
                        .into_any_element(),
                    o,
                ));
            }
            Some(DotifyError::Offline(_) | DotifyError::Server(_)) => {
                let o = open_o(self.dotify.error_at, want);
                out.push(banner(
                    ink(t, 0.05),
                    ink(t, 0.10),
                    div()
                        .text_color(ink(t, 0.5))
                        .child(Icon::CloudOff.el(zpx(14.)))
                        .into_any_element(),
                    div()
                        .child("You're offline. Entries will sync when you're back.")
                        .into_any_element(),
                    link("dotify-retry", "Retry", ink(t, 0.9))
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.dotify_manual_sync(cx)),
                        )
                        .into_any_element(),
                    o,
                ));
            }
            _ => {}
        }
        if let Some(msg) = self.dotify.rejected.clone() {
            let o = open_o(self.dotify.rejected_at, want);
            let copy = msg.clone();
            out.push(banner(
                red.opacity(0.10),
                red.opacity(0.25),
                div()
                    .text_color(red)
                    .child(Icon::Alert.el(zpx(14.)))
                    .into_any_element(),
                div()
                    .line_clamp(3)
                    .child(format!("“{msg}”"))
                    .into_any_element(),
                link("dotify-copy-error", "Copy", ink(t, 0.9))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()))
                    }))
                    .into_any_element(),
                o,
            ));
        }
        out
    }

    // ── Idle / edit: search, list, description, footer ───────────────────

    fn dotify_pick_view(
        &mut self,
        t: Tokens,
        window: &mut Window,
        reduced: bool,
        look: &dyn Fn(usize) -> Look,
        want: &mut bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let edit = self.dotify.flow.view == View::Edit;
        let search_focused =
            gpui::Focusable::focus_handle(self.dotify_search.read(cx), cx).is_focused(window);
        let desc_focused =
            gpui::Focusable::focus_handle(self.dotify_desc.read(cx), cx).is_focused(window);
        let well = |focused: bool| {
            div()
                .rounded(zpx(8.))
                .bg(shade(t, 0.25))
                .border_1()
                .border_color(if focused {
                    c(t.accent).opacity(0.55)
                } else {
                    ink(t, 0.08)
                })
                .when(focused, |d| {
                    d.shadow(vec![BoxShadow {
                        color: c(t.accent).opacity(0.18),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: zpx(3.),
                        inset: false,
                    }])
                })
        };
        let searching = self.dotify.searching;
        let has_query = !self.dotify.query.is_empty();
        let search = well(search_focused)
            .mx(zpx(12.))
            .h(zpx(32.))
            .px(zpx(10.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .child(
                div()
                    .flex_none()
                    .text_color(ink(t, 0.4))
                    .child(if searching {
                        *want = true;
                        spinner(
                            13.,
                            ink(t, 0.5),
                            ms_since(self.dotify.results_at.or(self.dotify.open_at)),
                            800.,
                            reduced,
                        )
                    } else {
                        Icon::Search.el(zpx(13.)).into_any_element()
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(zpx(13.))
                    .child(self.dotify_search.clone()),
            )
            .when(has_query, |d| {
                d.child(
                    div()
                        .id("dotify-search-clear")
                        .flex_none()
                        .size(zpx(18.))
                        .rounded(zpx(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(ink(t, 0.4))
                        .cursor_pointer()
                        .hover_fade(ink(t, 0.08), None)
                        .child(Icon::Close.el(zpx(11.)))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.dotify_clear_search(window, cx)
                        })),
                )
            })
            .when(!has_query && !search_focused, |d| {
                d.child(key_chip("/", ink(t, 0.07), ink(t, 0.4)))
            });

        let list = self.dotify_list(t, reduced, want, cx);

        let from_branch = self.dotify.prefill_text.from_branch
            && !self.dotify.prefill_text.text.is_empty()
            && self.dotify_desc.read(cx).value() == self.dotify.prefill_text.text;
        let desc = div()
            .mx(zpx(12.))
            .flex()
            .flex_col()
            .gap(zpx(6.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .child(
                        div()
                            .text_size(zpx(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ink(t, 0.4))
                            .child("DESCRIPTION"),
                    )
                    .when(from_branch, |d| {
                        d.child(
                            div()
                                .px(zpx(5.))
                                .h(zpx(16.))
                                .rounded(zpx(4.))
                                .flex()
                                .items_center()
                                .gap(zpx(3.))
                                .bg(ink(t, 0.06))
                                .text_size(zpx(10.5))
                                .text_color(ink(t, 0.5))
                                .child(Icon::Branch.el(zpx(10.)))
                                .child("from branch"),
                        )
                    }),
            )
            .child(
                well(desc_focused)
                    .px(zpx(10.))
                    .py(zpx(6.))
                    .text_size(zpx(13.))
                    .child(self.dotify_desc.clone()),
            );

        let picked = self.dotify.picked.is_some();
        let footer = div()
            .mx(zpx(12.))
            .mt(zpx(12.))
            .mb(zpx(12.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(zpx(12.))
                    .text_color(ink(t, 0.4))
                    .child(if edit {
                        "Editing the entry you just stopped".to_string()
                    } else if picked {
                        String::new()
                    } else {
                        "Pick a project".to_string()
                    }),
            );
        let footer = if edit {
            let st = &mut self.dotify.save_btn;
            footer
                .child(
                    ghost_button("dotify-edit-cancel", "Cancel", t).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            this.dotify.flow.cancel_edit();
                            this.dotify.view_at = Some(Instant::now());
                            cx.notify();
                        },
                    )),
                )
                .child(
                    primary_button(
                        "dotify-save",
                        "Save",
                        Some("⏎"),
                        picked,
                        st,
                        t,
                        reduced,
                        want,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dotify_save_edit(cx))),
                )
        } else {
            let st = &mut self.dotify.clock_in_btn;
            footer.child(
                primary_button(
                    "dotify-clock-in",
                    "Clock in",
                    Some("⏎"),
                    picked,
                    st,
                    t,
                    reduced,
                    want,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dotify_clock_in(cx))),
            )
        };
        vec![
            place(search, look(1)).into_any_element(),
            // The list's rows carry their own slots on the same timeline.
            list.into_any_element(),
            place(desc, look(3)).into_any_element(),
            place(footer, look(4)).into_any_element(),
        ]
    }

    fn dotify_list(
        &mut self,
        t: Tokens,
        reduced: bool,
        want: &mut bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        const MAX_H: f32 = 296.;
        const ROW_H: f32 = 32.;
        let (rows, ranges) = self.dotify_rows_and_ranges();
        let query = self.dotify.query.trim().to_string();
        let open_ms = ms_since(self.dotify.open_at);
        let list_slot = dm::section_delay(dm::LIST_SECTION);
        // One timeline: from open; late rows (and search results) from when
        // they arrived, already at the list's slot.
        let rows_ms = if !query.is_empty()
            && self
                .dotify
                .results_at
                .is_some_and(|r| self.dotify.open_at.is_none_or(|o| r > o))
        {
            ms_since(self.dotify.results_at) + list_slot
        } else if let Some(at) = self.dotify.rows_at {
            ms_since(Some(at)) + list_slot
        } else {
            open_ms
        };
        let picked = self.dotify.picked.as_ref().map(|p| p.id);
        let picked_ms = ms_since(self.dotify.picked_at);
        let highlight = self.dotify.highlight;
        let loading = !self.dotify.projects_loaded && rows.is_empty();
        let skeleton = |t: Tokens, opacity: f32| {
            let widths = [0.62, 0.48, 0.74, 0.55, 0.68, 0.42, 0.6, 0.5];
            div()
                .flex()
                .flex_col()
                .pt(zpx(4.))
                .opacity(opacity)
                .children(widths.iter().map(|w| {
                    div()
                        .mx(zpx(6.))
                        .h(zpx(ROW_H))
                        .px(zpx(8.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.))
                        .child(
                            div()
                                .flex_none()
                                .size(zpx(8.))
                                .rounded(zpx(2.))
                                .bg(ink(t, 0.10)),
                        )
                        .child(
                            div()
                                .h(zpx(8.))
                                .rounded(zpx(4.))
                                .w(gpui::relative(*w))
                                .bg(ink(t, 0.07)),
                        )
                }))
        };
        let shell = || div().relative().mt(zpx(8.)).mb(zpx(10.));
        if loading {
            // Reserve the list's height so nothing jumps when projects land.
            let look = dm::section_enter(open_ms, dm::LIST_SECTION, reduced);
            return shell()
                .h(zpx(MAX_H))
                .overflow_hidden()
                .child(place(skeleton(t, 1.), look));
        }
        if rows.is_empty() {
            let text = if query.is_empty() {
                "No projects".to_string()
            } else {
                format!("No projects match “{query}”")
            };
            return shell()
                .h(zpx(64.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(zpx(12.5))
                .text_color(ink(t, 0.4))
                .child(text);
        }
        if rows_ms < dm::ENTRY_DONE_MS + 40. {
            *want = true;
        }
        let mut last: Option<Section> = None;
        let mut items: Vec<AnyElement> = Vec::new();
        let mut slot = 0usize;
        let mut hi_child: Option<usize> = None;
        for (i, row) in rows.iter().enumerate() {
            if last != Some(row.section) && row.section != Section::Results {
                let look = dm::row_enter(rows_ms, slot, reduced);
                slot += 1;
                items.push(
                    place(
                        div()
                            .h(zpx(24.))
                            .px(zpx(14.))
                            .pt(zpx(if last.is_some() { 8. } else { 4. }))
                            .flex()
                            .items_center()
                            .text_size(zpx(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ink(t, 0.4))
                            .child(row.section.label()),
                        look,
                    )
                    .into_any_element(),
                );
            }
            last = Some(row.section);
            let p = &row.project;
            let is_picked = picked == Some(p.id);
            let is_hi = highlight == Some(i);
            if is_hi {
                hi_child = Some(items.len());
            }
            let fav = self.dotify.fav_ids.contains(&p.id);
            let flip = self
                .dotify
                .star_flip
                .get(&p.id)
                .map(|at| ms_since(Some(*at)))
                .unwrap_or(1e9);
            if flip < 300. {
                *want = true;
            }
            let star_o = if flip < 300. {
                fade(flip, 0., 160., reduced)
            } else {
                1.
            };
            let enter = dm::row_enter(rows_ms, slot, reduced);
            slot += 1;
            let check_o = if is_picked {
                fade(picked_ms, 0., 140., reduced)
            } else {
                0.
            };
            if is_picked && check_o < 1. {
                *want = true;
            }
            let name_el: AnyElement = match ranges.get(i).filter(|r| !r.is_empty()) {
                None => div().truncate().child(p.name.clone()).into_any_element(),
                Some(r) => {
                    let hl = HighlightStyle {
                        color: Some(ink(t, 1.0)),
                        font_weight: Some(FontWeight::SEMIBOLD),
                        ..Default::default()
                    };
                    let highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> =
                        r.iter().map(|(s, e)| (*s..*e, hl)).collect();
                    div()
                        .truncate()
                        .child(gpui::StyledText::new(p.name.clone()).with_highlights(highlights))
                        .into_any_element()
                }
            };
            let group: SharedString = format!("dotify-row-{i}").into();
            let pick_p = p.clone();
            let star_p = p.clone();
            let el = div()
                .id(("dotify-row", i))
                .group(group.clone())
                .mx(zpx(6.))
                .h(zpx(ROW_H))
                .px(zpx(8.))
                .rounded(zpx(6.))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(8.))
                .when(is_picked, |d| d.bg(ink(t, 0.08)))
                .when(is_hi && !is_picked, |d| d.bg(ink(t, 0.06)))
                .hover_fade(ink(t, 0.06), None)
                .cursor_pointer()
                .text_size(zpx(13.))
                .text_color(if is_picked {
                    ink(t, 0.94)
                } else {
                    ink(t, 0.80)
                })
                .child(square(t, Some(p), 8.))
                .child(div().flex_1().min_w_0().child(name_el))
                .child(
                    div()
                        .id(("dotify-star", i))
                        .flex_none()
                        .size(zpx(20.))
                        .rounded(zpx(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(star_o)
                        .text_color(if fav { c(t.warning) } else { ink(t, 0.35) })
                        .when(!fav, |d| {
                            d.invisible().group_hover(group.clone(), |s| s.visible())
                        })
                        .when(is_hi && !fav, |d| d.visible())
                        .hover_fade(ink(t, 0.08), None)
                        .child(if fav {
                            Icon::StarFilled.el(zpx(12.))
                        } else {
                            Icon::Star.el(zpx(12.))
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.dotify_toggle_favorite(star_p.clone(), cx);
                        })),
                )
                .child(
                    div()
                        .flex_none()
                        .w(zpx(14.))
                        .opacity(check_o)
                        .text_color(ink(t, 0.9))
                        .child(Icon::Check.el(zpx(12.))),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.dotify_pick(pick_p.clone(), Some(i), cx)
                }));
            items.push(place(el, enter).into_any_element());
        }
        // Keep the keyboard highlight in view.
        if highlight != self.dotify.scrolled_highlight {
            self.dotify.scrolled_highlight = highlight;
            if let Some(ix) = hi_child {
                self.dotify.list_scroll.scroll_to_item(ix);
            }
        }
        // Skeleton crossfades out over the arriving rows.
        let skel_o = self
            .dotify
            .rows_at
            .map_or(0., |at| 1. - fade(ms_since(Some(at)), 0., 160., reduced));
        if skel_o > 0. {
            *want = true;
        }
        shell()
            .child(
                div()
                    .id("dotify-list")
                    .max_h(zpx(MAX_H))
                    .overflow_y_scroll()
                    .track_scroll(&self.dotify.list_scroll)
                    .flex()
                    .flex_col()
                    .pt(zpx(0.))
                    .children(items),
            )
            .when(skel_o > 0., |d| {
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .child(skeleton(t, skel_o)),
                )
            })
    }

    // ── Clocked in ───────────────────────────────────────────────────────

    fn dotify_clocked_in_view(
        &mut self,
        t: Tokens,
        reduced: bool,
        want: &mut bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Tracker::Running(e) = self.dotify.tracker.clone() else {
            return div().h(zpx(80.)).into_any_element();
        };
        let offset = local_offset_s();
        let elapsed = e.elapsed_ms(now_ms());
        let overtime = self.dotify.overtime;
        let pname = e
            .project
            .as_ref()
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "No project".into());
        let breath = if self.window_active && !overtime {
            dm::breathe(
                ms_since(self.dotify.view_at.or(self.dotify.open_at)),
                reduced,
            )
        } else {
            1.
        };
        let dot = if overtime { c(t.warning) } else { c(t.success) };
        let card = div()
            .mx(zpx(12.))
            .px(zpx(12.))
            .py(zpx(12.))
            .rounded(zpx(10.))
            .bg(shade(t, 0.25))
            .border_1()
            .border_color(ink(t, 0.08))
            .flex()
            .flex_col()
            .gap(zpx(6.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .child(div().size(zpx(6.)).rounded_full().bg(dot).opacity(breath))
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(zpx(22.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ink(t, 0.94))
                            .child(clock_label(elapsed)),
                    )
                    .when(overtime, |d| {
                        let label = match self.dotify.overtime_wall_ms {
                            Some(at) => format!("Overtime +{}", short_clock(now_ms() - at)),
                            None => "Overtime".to_string(),
                        };
                        d.child(
                            div()
                                .px(zpx(6.))
                                .h(zpx(18.))
                                .rounded(zpx(4.))
                                .flex()
                                .items_center()
                                .bg(c(t.warning).opacity(0.12))
                                .text_size(zpx(11.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(c(t.warning))
                                .child(label),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .child(square(t, e.project.as_ref(), 8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(zpx(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(ink(t, 0.85))
                            .child(pname),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(zpx(12.))
                            .text_color(ink(t, 0.4))
                            .child(format!("since {}", local_hm(e.start_ms, offset))),
                    ),
            )
            .when(!e.description.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(zpx(12.5))
                        .line_clamp(2)
                        .text_color(ink(t, 0.6))
                        .child(e.description.clone()),
                )
            });
        let busy = self.dotify.out_busy;
        let footer = div()
            .mx(zpx(12.))
            .mt(zpx(12.))
            .mb(zpx(12.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(zpx(12.))
                    .text_color(ink(t, 0.4))
                    .child("Edit after clocking out"),
            )
            .child({
                let confirm = self.dotify.confirm_out;
                confirm_button(
                    "dotify-clock-out",
                    "Clock out",
                    "Confirm clock out",
                    false,
                    &confirm,
                    busy,
                    &mut self.dotify.confirm_out_w,
                    t,
                    reduced,
                    want,
                )
                .on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.dotify_clock_out_click(cx)),
                )
            });
        div()
            .pt(zpx(4.))
            .flex()
            .flex_col()
            .child(card)
            .child(footer)
            .into_any_element()
    }

    // ── Result ───────────────────────────────────────────────────────────

    fn dotify_result_view(
        &mut self,
        t: Tokens,
        reduced: bool,
        ms: f32,
        want: &mut bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(e) = self.dotify.flow.result.clone() else {
            return div().h(zpx(80.)).into_any_element();
        };
        let offset = local_offset_s();
        let check = fade(ms, 80., 200., reduced);
        if check < 1. {
            *want = true;
        }
        let total = hours_label(e.elapsed_ms(now_ms()) as f64 / 3_600_000.);
        let pname = e
            .project
            .as_ref()
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "No project".into());
        let editable = !e.locked && e.end_ms.is_some();
        let head = div()
            .mx(zpx(12.))
            .pt(zpx(4.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(10.))
            .child(
                div()
                    .flex_none()
                    .size(zpx(28.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(c(t.success).opacity(0.14 * check))
                    .text_color(c(t.success))
                    .child(div().opacity(check).child(Icon::Check.el(zpx(15.)))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(zpx(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ink(t, 0.94))
                            .child(format!("Clocked out · {total}")),
                    )
                    .child(
                        div()
                            .text_size(zpx(12.))
                            .text_color(ink(t, 0.5))
                            .child(range_label(&e, offset)),
                    ),
            );
        let card = div()
            .mx(zpx(12.))
            .mt(zpx(10.))
            .px(zpx(12.))
            .py(zpx(10.))
            .rounded(zpx(10.))
            .bg(shade(t, 0.25))
            .border_1()
            .border_color(ink(t, 0.08))
            .flex()
            .flex_col()
            .gap(zpx(4.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .child(square(t, e.project.as_ref(), 8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(zpx(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(ink(t, 0.85))
                            .child(pname),
                    ),
            )
            .child(
                div()
                    .text_size(zpx(12.5))
                    .line_clamp(2)
                    .text_color(if e.description.is_empty() {
                        ink(t, 0.35)
                    } else {
                        ink(t, 0.6)
                    })
                    .child(if e.description.is_empty() {
                        "No description".to_string()
                    } else {
                        e.description.clone()
                    }),
            )
            .when(e.locked, |d| {
                d.child(
                    div()
                        .text_size(zpx(11.5))
                        .text_color(ink(t, 0.4))
                        .child("Period confirmed · can't edit"),
                )
            });
        let footer = div()
            .mx(zpx(12.))
            .mt(zpx(12.))
            .mb(zpx(12.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .child(div().flex_1())
            .when(editable, |d| {
                d.child(ghost_button("dotify-result-edit", "Edit", t).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.dotify_edit_result(cx)),
                ))
            })
            .child(
                secondary_button("dotify-result-done", "Done", None, t, reduced, want).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.dotify_close_popover(cx)),
                ),
            );
        div()
            .flex()
            .flex_col()
            .child(head)
            .child(card)
            .child(footer)
            .into_any_element()
    }

    // ── Stats ────────────────────────────────────────────────────────────

    fn dotify_stats(&mut self, t: Tokens, reduced: bool, want: &mut bool) -> Option<gpui::Div> {
        let today = self.dotify_today_h();
        let stats = dm::stats(today, self.dotify.summary.as_ref());
        if stats.iter().all(Option::is_none) {
            return None;
        }
        // Animate the fill on the day's first open or when a value moved.
        if self.dotify.stats_anim.is_none()
            && let Some(seen) = self.dotify_stats_seen()
        {
            let animate = dm::stats_animate(self.dotify.memory.stats_seen.as_ref(), &seen);
            self.dotify.stats_anim = Some((Instant::now(), animate));
            if self.dotify.memory.stats_seen != Some(seen) && !self.dotify.demo {
                self.dotify.memory.stats_seen = Some(seen);
                self.dotify.memory.save(&self.state.data_dir);
            }
        }
        let (anim_at, animate) = self.dotify.stats_anim.unwrap_or((Instant::now(), false));
        let ms = ms_since(Some(anim_at));
        let now = clock();
        let labels = ["Today", "Week", "Period"];
        let mut cols: Vec<AnyElement> = Vec::new();
        for (i, stat) in stats.iter().enumerate() {
            let Some(stat) = stat else {
                continue;
            };
            let over = stat.over_h();
            let bar = stat.fill().map(|to| {
                let opening = dm::bar_fill(ms, i, to, animate, reduced);
                let fill = if animate && !reduced && ms < 200. + 60. * i as f32 + 500. {
                    *want = true;
                    self.dotify.bar_tweens[i] = Some(Tween::settled(to));
                    opening
                } else {
                    let tw = self.dotify.bar_tweens[i].get_or_insert(Tween::settled(to));
                    tw.retarget(to, now, 400., out_quint, !reduced);
                    if tw.moving(now) {
                        *want = true;
                    }
                    tw.value(now)
                };
                let color = if over.is_some() {
                    c(t.warning)
                } else {
                    ink(t, 0.7)
                };
                div()
                    .mt(zpx(6.))
                    .h(zpx(3.))
                    .w_full()
                    .rounded(zpx(2.))
                    .bg(ink(t, 0.10))
                    .child(
                        div()
                            .h_full()
                            .rounded(zpx(2.))
                            .w(gpui::relative(fill.clamp(0., 1.)))
                            .bg(color),
                    )
            });
            let value = match stat.target_h {
                Some(target) => format!("{} / {}", hours_label(stat.value_h), hours_label(target)),
                None => hours_label(stat.value_h),
            };
            cols.push(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(zpx(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ink(t, 0.4))
                            .child(labels[i].to_uppercase()),
                    )
                    .child(
                        div()
                            .mt(zpx(2.))
                            .truncate()
                            .text_size(zpx(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if over.is_some() {
                                c(t.warning)
                            } else {
                                ink(t, 0.85)
                            })
                            .child(value),
                    )
                    .children(bar)
                    .into_any_element(),
            );
        }
        Some(
            div()
                .px(zpx(14.))
                .py(zpx(10.))
                .bg(shade(t, 0.18))
                .border_t_1()
                .border_color(ink(t, 0.07))
                .flex()
                .flex_row()
                .gap(zpx(14.))
                .children(cols),
        )
    }

    // ── Settings card ────────────────────────────────────────────────────

    pub(super) fn render_dotify_settings_card(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reduced = self.reduced();
        let mut want = false;
        let conn = if self.dotify.demo_settings_disconnected {
            None
        } else {
            self.dotify.conn.clone()
        };
        let since_change = ms_since(self.dotify.connected_at);
        let body_o = fade(since_change, 0., 160., reduced);
        if body_o < 1. {
            want = true;
        }
        let (chip_text, chip_color) = match (&conn, &self.dotify.sync_error) {
            (Some(_), Some(DotifyError::Auth(_))) => ("Expired", c(t.danger)),
            (Some(_), Some(DotifyError::Offline(_) | DotifyError::Server(_))) => {
                ("Offline", ink(t, 0.5))
            }
            (Some(_), _) => ("Connected", c(t.success)),
            (None, _) => ("Not connected", ink(t, 0.45)),
        };
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(10.))
            .child(
                div()
                    .flex_none()
                    .size(zpx(32.))
                    .rounded(zpx(8.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(ink(t, 0.07))
                    .border_1()
                    .border_color(ink(t, 0.08))
                    .text_color(ink(t, 0.8))
                    .child(Icon::Timer.el(zpx(16.))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(zpx(13.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ink(t, 0.94))
                            .child("Dotify"),
                    )
                    .child(
                        div()
                            .text_size(zpx(12.))
                            .text_color(ink(t, 0.5))
                            .child("Clock in and out from the timer pill."),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .h(zpx(20.))
                    .px(zpx(8.))
                    .rounded(zpx(10.))
                    .flex()
                    .items_center()
                    .gap(zpx(5.))
                    .bg(chip_color.opacity(0.12))
                    .text_size(zpx(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(chip_color)
                    .child(div().size(zpx(5.)).rounded_full().bg(chip_color))
                    .child(chip_text),
            );
        let body = match conn {
            Some(conn) => self.dotify_settings_connected(t, &conn, reduced, &mut want, cx),
            None => self.dotify_settings_connect(t, reduced, &mut want, cx),
        };
        if want {
            self.dotify.want_frame = true;
        }
        div()
            .p(zpx(14.))
            .rounded(zpx(12.))
            .bg(ink(t, 0.03))
            .border_1()
            .border_color(ink(t, 0.08))
            .flex()
            .flex_col()
            .gap(zpx(12.))
            .child(header)
            .child(div().opacity(body_o).child(body))
            .into_any_element()
    }

    fn dotify_settings_connected(
        &mut self,
        t: Tokens,
        conn: &super::dotify::DotifyConn,
        reduced: bool,
        want: &mut bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let display = if conn.name.is_empty() {
            conn.email.clone()
        } else {
            conn.name.clone()
        };
        let account = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(10.))
            .child(
                div()
                    .flex_none()
                    .size(zpx(28.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(ink(t, 0.10))
                    .text_size(zpx(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ink(t, 0.8))
                    .child(initials(&display)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(zpx(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(ink(t, 0.9))
                            .child(display),
                    )
                    .when(!conn.email.is_empty() && !conn.name.is_empty(), |d| {
                        d.child(
                            div()
                                .truncate()
                                .text_size(zpx(12.))
                                .text_color(ink(t, 0.5))
                                .child(conn.email.clone()),
                        )
                    }),
            );
        let test_busy = self.dotify.test_busy;
        let result = self.dotify.test_result.clone();
        let result_el = result.map(|(r, at)| {
            let o = fade(ms_since(Some(at)), 0., 140., reduced);
            if o < 1. {
                *want = true;
            }
            let (icon, color, text) = match r {
                Ok(s) => (Icon::Check, c(t.success), s),
                Err(e) => (Icon::Alert, c(t.danger), e),
            };
            div()
                .opacity(o)
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(5.))
                .text_size(zpx(12.))
                .text_color(color)
                .child(icon.el(zpx(12.)))
                .child(text)
        });
        let confirm = self.dotify.disconnect_confirm;
        let actions = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .child(
                secondary_button(
                    "dotify-test",
                    "Test connection",
                    test_busy,
                    t,
                    reduced,
                    want,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dotify_test(cx))),
            )
            .children(result_el)
            .child(div().flex_1())
            .child(
                confirm_button(
                    "dotify-disconnect",
                    "Disconnect",
                    "Confirm disconnect",
                    true,
                    &confirm,
                    None,
                    &mut AnimW::default(),
                    t,
                    reduced,
                    want,
                )
                .on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.dotify_disconnect_click(cx)),
                ),
            );
        let on = self.state.ui_settings.dotify_nudge;
        let flip = ms_since(self.dotify.nudge_flip_at);
        let x = if reduced {
            span(flip, 0., 120.)
        } else {
            out_quint(span(flip, 0., 180.))
        };
        if x < 1. {
            *want = true;
        }
        let pos = if on { x } else { 1. - x };
        let track = mix(ink(t, 0.14), c(t.accent), pos);
        let switch = div()
            .id("dotify-nudge-switch")
            .flex_none()
            .relative()
            .w(zpx(32.))
            .h(zpx(18.))
            .rounded(zpx(9.))
            .bg(track)
            .cursor_pointer()
            .hover_fade(ink(t, 0.06), Some(ink(t, 0.1)))
            .child(
                div()
                    .absolute()
                    .top(zpx(2.))
                    .left(zpx(2. + 14. * pos))
                    .size(zpx(14.))
                    .rounded_full()
                    .bg(gpui::white()),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dotify_toggle_nudge(cx)));
        let nudge = div()
            .pt(zpx(12.))
            .border_t_1()
            .border_color(ink(t, 0.07))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(10.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(zpx(13.))
                            .text_color(ink(t, 0.88))
                            .child("Remind me to clock in"),
                    )
                    .child(div().text_size(zpx(12.)).text_color(ink(t, 0.5)).child(
                        "A nudge when you start working without a running entry (Mon–Fri, 9–18).",
                    )),
            )
            .child(switch);
        div()
            .flex()
            .flex_col()
            .gap(zpx(12.))
            .child(account)
            .child(actions)
            .child(nudge)
            .into_any_element()
    }

    fn dotify_settings_connect(
        &mut self,
        t: Tokens,
        reduced: bool,
        want: &mut bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = self.dotify.token_shown;
        let err = self.dotify.connect_error.clone();
        let icon_btn = |id: &'static str, icon: Icon| {
            div()
                .id(id)
                .flex_none()
                .size(zpx(24.))
                .rounded(zpx(6.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(ink(t, 0.5))
                .cursor_pointer()
                .hover(|s| s.text_color(ink(t, 0.8)))
                .hover_fade(ink(t, 0.08), None)
                .child(icon.el(zpx(13.)))
        };
        let field = div()
            .h(zpx(34.))
            .pl(zpx(10.))
            .pr(zpx(4.))
            .rounded(zpx(8.))
            .bg(shade(t, 0.25))
            .border_1()
            .border_color(if err.is_some() {
                c(t.danger).opacity(0.5)
            } else {
                ink(t, 0.10)
            })
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(4.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family(MONO)
                    .text_size(zpx(12.))
                    .child(self.dotify_token.clone()),
            )
            .child(
                icon_btn(
                    "dotify-token-eye",
                    if shown { Icon::EyeOff } else { Icon::Eye },
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.dotify_toggle_token_shown(window, cx)
                })),
            )
            .child(icon_btn("dotify-token-paste", Icon::Clipboard).on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.dotify_paste_token(window, cx)),
            ));
        let err_o = fade(ms_since(self.dotify.connect_error_at), 0., 140., reduced);
        if err.is_some() && err_o < 1. {
            *want = true;
        }
        let has_token = !self.dotify_token.read(cx).value().trim().is_empty();
        let st = &mut self.dotify.connect_btn;
        let button = primary_button(
            "dotify-connect",
            "Test & connect",
            None,
            has_token,
            st,
            t,
            reduced,
            want,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.dotify_connect(window, cx)));
        div()
            .flex()
            .flex_col()
            .gap(zpx(10.))
            .child(field)
            .when_some(err, |d, e| d.child(div().opacity(err_o).flex().flex_row().items_center().gap(zpx(5.)).text_size(zpx(12.)).text_color(c(t.danger)).child(Icon::Alert.el(zpx(12.))).child(e)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(10.))
                    .child(div().flex_1().min_w_0().text_size(zpx(12.)).text_color(ink(t, 0.45)).child("Paste your Dotify API token. It stays on this Mac, readable only by you."))
                    .child(button),
            )
            .into_any_element()
    }
}

/// `h:mm` for a duration in ms.
fn short_clock(ms: i64) -> String {
    let mins = (ms.max(0) / 60_000) as i64;
    format!("{}:{:02}", mins / 60, mins % 60)
}

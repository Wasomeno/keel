//! Settings primitives: one button, cards, rows, toggles, and segmented controls.
//!
//! Motion that is specific to Settings lives here. Shared helpers in
//! `crate::motion` (`appear`, `dismiss`, `scrim`) stay as they are.

use gpui::{
    AnimationExt, AnyElement, ClickEvent, Context, ElementId, IntoElement, SharedString,
    SpringAnimation, SpringConfig, div, prelude::*,
};
use keel_theme::Tokens;

use super::super::KeelWindow;
use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::style::{CONTROL_H, INPUT_H, KeelStyled, ROW_MIN_H, c, r_lg, r_md, r_sm, zpx};

/// Rail and segmented pills. Critically damped (ζ = 1): stiffness 900,
/// damping 60, mass 1 settles to 0.1% in about 0.3s.
pub(crate) const PILL_SPRING: SpringConfig = SpringConfig::new(900., 60., 1.);
/// Toggle knob. Critically damped, about 0.2s: ω ≈ sqrt(2100), damping = 2ω.
pub(crate) const KNOB_SPRING: SpringConfig = SpringConfig::new(2100., 91.6, 1.);

const PRESS: f32 = 0.8;

/// The four button treatments Settings uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ButtonKind {
    /// Accent fill, text on accent.
    Primary,
    /// Hairline outline, no fill.
    Secondary,
    /// No chrome until hover.
    Ghost,
    /// Danger text and a danger outline. For a destructive confirm.
    Danger,
}

/// One settings button: [`CONTROL_H`] tall, medium small type, 10px of
/// horizontal padding, press opacity 0.8. `enabled` drops the fill and the
/// pointer instead of washing the control with opacity.
pub(crate) fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
    div()
        .id(ElementId::Name(id.into()))
        .h(zpx(CONTROL_H))
        .px(zpx(10.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(r_md())
        .border_1()
        .t_small()
        .font_weight(gpui::FontWeight::MEDIUM)
        .map(|d| {
            if !enabled {
                // A disabled ghost stays chromeless; the rest keep their outline.
                let edge = if kind == ButtonKind::Ghost {
                    gpui::transparent_black()
                } else {
                    c(t.border)
                };
                return d
                    .border_color(edge)
                    .text_color(c(t.text_faint))
                    .cursor_default();
            }
            match kind {
                ButtonKind::Primary => d
                    .bg(c(t.accent))
                    .border_color(c(t.accent))
                    .text_color(c(t.text_on_accent))
                    .cursor_pointer()
                    .active(|s| s.opacity(PRESS)),
                ButtonKind::Secondary => d
                    .border_color(c(t.border))
                    .text_color(c(t.text))
                    .cursor_pointer()
                    .hover_fade(c(t.hover), None)
                    .active(|s| s.opacity(PRESS)),
                ButtonKind::Ghost => d
                    .border_color(gpui::transparent_black())
                    .text_color(c(t.text))
                    .cursor_pointer()
                    .hover_fade(c(t.hover), None)
                    .active(|s| s.opacity(PRESS)),
                ButtonKind::Danger => d
                    .border_color(c(t.danger))
                    .text_color(c(t.danger))
                    .cursor_pointer()
                    .hover_fade(c(t.hover), None)
                    .active(|s| s.opacity(PRESS)),
            }
        })
        .child(label)
}

/// A 26px square ghost icon button. The settings close control.
pub(crate) fn icon_button(
    id: impl Into<SharedString>,
    icon: Icon,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .size(zpx(CONTROL_H))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(r_md())
        .text_color(c(t.text_muted))
        .cursor_pointer()
        .hover(|s| s.text_color(c(t.text)))
        .hover_fade(c(t.hover), None)
        .active(|s| s.opacity(PRESS))
        .child(icon.el(zpx(14.)))
}

/// Sentence-case group label, 8px above its card. `info` is a one-line
/// caption under the label, for context the rows don't already give.
pub(crate) fn card_group_header(
    title: &'static str,
    info: Option<&'static str>,
    right: Option<AnyElement>,
    t: Tokens,
) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(zpx(12.))
        .pb(zpx(8.))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(zpx(2.))
                .child(
                    div()
                        .t_small()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(c(t.text_muted))
                        .child(title),
                )
                .children(
                    info.map(|text| div().t_caption().text_color(c(t.text_faint)).child(text)),
                ),
        )
        .children(right)
}

/// Bordered card, no fill. Rows inside pick up a hairline between them.
pub(crate) fn settings_card(
    rows: impl IntoIterator<Item = impl IntoElement>,
    t: Tokens,
) -> gpui::Div {
    div()
        .w_full()
        .rounded(r_lg())
        .border_1()
        .border_color(c(t.border))
        .px(zpx(16.))
        .flex()
        .flex_col()
        .children(rows.into_iter().enumerate().map(|(ix, row)| {
            div()
                .w_full()
                .when(ix > 0, |d| d.border_t_1().border_color(c(t.border)))
                .child(row)
        }))
}

/// Legacy alias kept for `updates.rs`.
pub(in crate::view) fn settings_list(rows: Vec<gpui::Div>, t: Tokens) -> gpui::Div {
    settings_card(rows, t)
}

/// Legacy section header kept for `updates.rs`. The subtitle is a caption.
pub(in crate::view) fn section_header(
    title: &'static str,
    subtitle: &'static str,
    right: Option<AnyElement>,
    t: Tokens,
) -> gpui::Div {
    let tip = (!subtitle.is_empty()).then_some(subtitle);
    card_group_header(title, tip, right, t)
}

/// Label on the left, control on the right. `hint` sits under the label in
/// muted small type; leave it out when the label says enough.
pub(crate) fn setting_row(
    t: Tokens,
    label: &'static str,
    hint: Option<&'static str>,
    control: AnyElement,
) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .min_h(zpx(ROW_MIN_H))
        .py(zpx(10.))
        .gap(zpx(16.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .justify_center()
                .gap(zpx(2.))
                .child(div().t_body().text_color(c(t.text)).child(label))
                .when_some(hint, |d, hint| {
                    d.child(div().t_small().text_color(c(t.text_muted)).child(hint))
                }),
        )
        .child(control)
}

/// [`setting_row`] under its older name; `hint` is the line under the label.
pub(crate) fn setting_row_info(
    t: Tokens,
    label: &'static str,
    hint: Option<&'static str>,
    control: AnyElement,
) -> gpui::Div {
    setting_row(t, label, hint, control)
}

fn focus_ring(t: Tokens) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color: c(t.focus),
        offset: gpui::point(zpx(0.), zpx(0.)),
        blur_radius: zpx(0.),
        spread_radius: zpx(1.),
        inset: false,
    }]
}

/// A labelled text field. The shell is [`INPUT_H`], hairline-bordered, and
/// draws the focus ring when the shell itself is focused. The inner editor
/// uses the same `focus` role through the base theme.
pub(crate) fn field(t: Tokens, label: &'static str, input: AnyElement) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(12.))
        .child(
            div()
                .w(zpx(64.))
                .flex_none()
                .t_small()
                .text_color(c(t.text_muted))
                .child(label),
        )
        .child(input_shell(t).child(input))
}

pub(crate) fn input_shell(t: Tokens) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .h(zpx(INPUT_H))
        .px(zpx(8.))
        .flex()
        .items_center()
        .rounded(r_md())
        .border_1()
        .border_color(c(t.border))
        .focus(|s| s.border_color(c(t.focus)).shadow(focus_ring(t)))
        .t_body()
}

/// 1px drop under a raised pill or knob, painted with the shadow role.
fn raised_shadow(t: Tokens) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color: c(t.shadow),
        offset: gpui::point(zpx(0.), zpx(1.)),
        blur_radius: zpx(1.),
        spread_radius: zpx(0.),
        inset: false,
    }]
}

/// White would blow out on `window` (contrast above 7): a dark theme.
fn is_dark(t: Tokens) -> bool {
    keel_theme::contrast(0xFFFF_FFFF, t.window) > 7.0
}

/// Toggle knob colors, off then on. Light: white both ways. Dark: a light
/// gray on the dark off track, `text_on_accent` on the light accent track.
fn knob_fills(t: Tokens) -> (gpui::Hsla, gpui::Hsla) {
    if is_dark(t) {
        (c(t.text_muted), c(t.text_on_accent))
    } else {
        let white = gpui::hsla(0., 0., 1., 1.);
        (white, white)
    }
}

/// Soft halo a toggle gains on hover.
fn hover_halo(t: Tokens) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color: c(t.hover),
        offset: gpui::point(zpx(0.), zpx(0.)),
        blur_radius: zpx(0.),
        spread_radius: zpx(3.),
        inset: false,
    }]
}

impl KeelWindow {
    /// Segmented control. The track is the input fill at [`CONTROL_H`]; the
    /// selected pill sits 1px inside its border on every side. The pill is a
    /// raised, hairline-bordered surface (window in light, a lifted overlay
    /// in dark), not an accent fill. Its left edge springs per group.
    pub(crate) fn render_segmented_control<T: Copy + PartialEq + 'static, F>(
        &self,
        group_id: &'static str,
        options: &[(&'static str, T)],
        current: T,
        item_w: f32,
        t: Tokens,
        cx: &mut Context<Self>,
        on_select: F,
    ) -> gpui::Div
    where
        F: Fn(&mut Self, T, &mut Context<Self>) + Copy + 'static,
    {
        let current_ix = options
            .iter()
            .position(|(_, val)| *val == current)
            .unwrap_or(0);
        let count = options.len();
        // Absolute children are placed inside the track's 1px border.
        const BORDER: f32 = 1.;
        const PAD: f32 = 1.;
        let pill_h = CONTROL_H - BORDER * 2. - PAD * 2.;
        let total_w = BORDER * 2. + PAD * 2. + item_w * count as f32;
        let left = PAD + current_ix as f32 * item_w;
        let pill_fill = if is_dark(t) { c(t.selected) } else { c(t.window) };

        let pill = div()
            .absolute()
            .top(zpx(PAD))
            .w(zpx(item_w))
            .h(zpx(pill_h))
            .rounded(r_sm())
            .bg(pill_fill)
            .border_1()
            .border_color(c(t.border))
            .shadow(raised_shadow(t))
            .with_spring(
                ElementId::Name(format!("settings-seg-{group_id}").into()),
                SpringAnimation::new(PILL_SPRING).to(zpx(left)),
                |el, x| el.left(x),
            );

        let hit_targets = div()
            .absolute()
            .top(zpx(PAD))
            .left(zpx(PAD))
            .h(zpx(pill_h))
            .flex()
            .flex_row()
            .items_center()
            .children(options.iter().enumerate().map(|(ix, (label, val))| {
                let selected = ix == current_ix;
                let val = *val;
                div()
                    .id(ElementId::Name(format!("{group_id}-{label}").into()))
                    .w(zpx(item_w))
                    .h(zpx(pill_h))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(r_sm())
                    .cursor_pointer()
                    .t_small()
                    .font_weight(if selected {
                        gpui::FontWeight::MEDIUM
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .text_color(c(if selected { t.text } else { t.text_muted }))
                    .when(!selected, |d| d.hover_fade(c(t.hover), None))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        on_select(this, val, cx);
                        cx.notify();
                    }))
                    .child(*label)
            }));

        div()
            .relative()
            .flex_none()
            .w(zpx(total_w))
            .h(zpx(CONTROL_H))
            .rounded(r_md())
            .bg(c(t.input))
            .border_1()
            .border_color(c(t.border))
            .child(pill)
            .child(hit_targets)
    }
}

/// A 30×18 switch. The knob springs on a bool; the track color follows the
/// same phase as an interpolated [`gpui::Hsla`].
pub(in crate::view) fn toggle(id: String, on: bool, t: Tokens) -> gpui::Stateful<gpui::Div> {
    const TRACK_W: f32 = 30.;
    const TRACK_H: f32 = 18.;
    const BORDER: f32 = 1.;
    const KNOB: f32 = 14.;
    // Knob gap inside the border: 16px inner height - 14px knob = 1px a side.
    const INSET: f32 = 1.;
    let spring_id = id.clone();
    let (knob_off, knob_on) = knob_fills(t);
    let off_fill = c(t.input);
    let on_fill = c(t.accent);
    let off_edge = c(t.border);
    let on_edge = c(t.accent);
    let shadow = raised_shadow(t);

    let track = div()
        .w(zpx(TRACK_W))
        .h(zpx(TRACK_H))
        .relative()
        .flex_none()
        .rounded_full()
        .border_1()
        .with_spring(
            ElementId::Name(format!("settings-toggle-{spring_id}").into()),
            SpringAnimation::new(KNOB_SPRING).to(on),
            move |el, phase| {
                let left = phase.interpolate(
                    zpx(INSET),
                    zpx(TRACK_W - BORDER * 2. - KNOB - INSET),
                );
                el.bg(phase.interpolate_clamped(off_fill, on_fill))
                    .border_color(phase.interpolate_clamped(off_edge, on_edge))
                    .child(
                        div()
                            .absolute()
                            .top(zpx(INSET))
                            .left(left)
                            .size(zpx(KNOB))
                            .rounded_full()
                            .bg(phase.interpolate_clamped(knob_off, knob_on))
                            .shadow(shadow.clone()),
                    )
            },
        );

    div()
        .id(ElementId::Name(id.into()))
        .flex_none()
        .rounded_full()
        .cursor_pointer()
        .hover(|s| s.shadow(hover_halo(t)))
        .child(track)
}

/// Preview control. A bool spring crossfades idle and playing; reduced motion
/// snaps because gpui springs do.
pub(crate) fn render_sound_preview_btn(is_playing: bool, t: Tokens) -> gpui::Stateful<gpui::Div> {
    let idle_bg = c(t.input);
    let play_bg = c(t.window);
    let idle_edge = c(t.border);
    let play_edge = c(t.accent);
    let idle_text = c(t.text_muted);
    let play_text = c(t.accent);

    let body = div()
        .w(zpx(88.))
        .h(zpx(CONTROL_H))
        .relative()
        .rounded(r_md())
        .overflow_hidden()
        .border_1()
        .with_spring(
            "settings-sound-preview",
            SpringAnimation::new(KNOB_SPRING).to(is_playing),
            move |el, phase| {
                let play = phase.clamp(0.0..=1.0);
                let text = play.interpolate(idle_text, play_text);
                el.bg(play.interpolate_clamped(idle_bg, play_bg))
                    .border_color(play.interpolate_clamped(idle_edge, play_edge))
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .gap(zpx(6.))
                            .opacity(play.interpolate(1., 0.))
                            .child(Icon::Play.el(zpx(10.)).text_color(text))
                            .child(div().t_small().text_color(text).child("Preview")),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .gap(zpx(6.))
                            .opacity(play.0)
                            .child(Icon::Volume2.el(zpx(12.)).text_color(text))
                            .child(div().t_small().text_color(text).child("Playing…")),
                    )
            },
        );

    div()
        .id("sound-preview-btn")
        .flex_none()
        .cursor_pointer()
        .active(|s| s.opacity(PRESS))
        .child(body)
}

/// Settings modal entrance: 180ms, opacity and an 8px rise, `ease_out_strong`.
/// Reduced motion snaps to the resting state inside gpui.
pub(in crate::view) fn settings_open_motion<E>(el: E, id: impl Into<ElementId>) -> impl IntoElement
where
    E: IntoElement + gpui::Styled + 'static,
{
    el.with_animation(
        id,
        gpui::Animation::new(std::time::Duration::from_millis(180))
            .with_easing(crate::motion::ease_out_strong),
        |el, delta| el.opacity(delta).relative().top(zpx(8. * (1. - delta))),
    )
}

/// Settings modal exit: 120ms, opacity and a 4px drop, `ease_out`.
pub(in crate::view) fn settings_close_motion<E>(el: E, id: impl Into<ElementId>) -> gpui::AnyElement
where
    E: IntoElement + gpui::Styled + 'static,
{
    el.with_animation(
        id,
        gpui::Animation::new(std::time::Duration::from_millis(120))
            .with_easing(crate::motion::ease_out),
        |el, delta| el.opacity(1. - delta).relative().top(zpx(4. * delta)),
    )
    .into_any_element()
}

pub(crate) fn reveal_in_finder(path: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .args(["-R", path])
        .spawn();
    #[cfg(not(target_os = "macos"))]
    {
        let folder = std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new(path));
        let _ = std::process::Command::new("xdg-open").arg(folder).spawn();
    }
}

pub(crate) fn open_browser_url(url: &str) {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(not(target_os = "macos"))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

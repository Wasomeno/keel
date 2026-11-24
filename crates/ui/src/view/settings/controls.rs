//! Reusable Settings primitives: grouped cards, compact rows, hover info
//! badges (`InfoTip`), status pills, animated switches, and segmented controls.

use gpui::{AnyElement, ClickEvent, Context, ElementId, IntoElement, div, prelude::*};
use keel_theme::Tokens;

use super::super::KeelWindow;
use super::super::tooltip::InfoTip;
use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::style::{KeelStyled, RADIUS_LG, RADIUS_MD, RADIUS_SM, c, zpx};

/// Compact uppercase/medium group label above a [`settings_card`].
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
        .gap_3()
        .pb_2()
        .px_0p5()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .child(
                    div()
                        .t_caption()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(t.text_muted))
                        .child(title),
                )
                .children(info.map(|tip| info_badge(format!("card-info-{title}"), tip, t))),
        )
        .children(right)
}

/// A rounded, hairline-bordered settings card holding rows separated by 1 px hairlines.
pub(crate) fn settings_card(rows: Vec<gpui::Div>, t: Tokens) -> gpui::Div {
    div()
        .w_full()
        .rounded(RADIUS_LG)
        .bg(c(t.floating))
        .border_1()
        .border_color(c(t.border))
        .px_4()
        .flex()
        .flex_col()
        .children(
            rows.into_iter()
                .enumerate()
                .map(|(ix, row)| row.when(ix > 0, |d| d.border_t_1().border_color(c(t.border)))),
        )
}

/// Legacy alias kept for `updates.rs` compatibility: now renders a clean card.
pub(in crate::view) fn settings_list(rows: Vec<gpui::Div>, t: Tokens) -> gpui::Div {
    settings_card(rows, t)
}

/// Legacy section header kept for `updates.rs` compatibility: compact group
/// header with optional hover tooltip instead of a verbose inline subtitle.
pub(in crate::view) fn section_header(
    title: &'static str,
    subtitle: &'static str,
    right: Option<AnyElement>,
    t: Tokens,
) -> gpui::Div {
    let tip = (!subtitle.is_empty()).then_some(subtitle);
    card_group_header(title, tip, right, t)
}

/// A subtle `?` badge that reveals a wrapped [`InfoTip`] on hover.
pub(crate) fn info_badge(id: impl Into<String>, tip: &'static str, t: Tokens) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    div()
        .id(ElementId::Name(id.into()))
        .flex_none()
        .size(zpx(15.))
        .rounded_full()
        .border_1()
        .border_color(c(t.border))
        .bg(c(t.hover))
        .flex()
        .items_center()
        .justify_center()
        .text_size(zpx(10.))
        .line_height(zpx(10.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(c(t.text_faint))
        .cursor_default()
        .hover(|s| s.text_color(c(t.text)).border_color(c(t.text_faint)))
        .child("?")
        .tooltip(move |_, cx| cx.new(|_| InfoTip::new(tip, t)).into())
}

/// Compact setting row: label + optional hover info badge on the left, control
/// on the right. No verbose inline paragraphs.
pub(crate) fn setting_row_info(
    t: Tokens,
    label: &'static str,
    info: Option<&'static str>,
    control: AnyElement,
) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .min_h(zpx(44.))
        .py_2p5()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .child(
                    div()
                        .t_body()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(c(t.text))
                        .child(label),
                )
                .children(info.map(|tip| info_badge(format!("info-{label}"), tip, t))),
        )
        .child(control)
}

/// Compact status pill (neutral, success, or warning tint).
pub(crate) fn status_pill(label: impl Into<gpui::SharedString>, fg: u32, t: Tokens) -> gpui::Div {
    div()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(c(t.hover))
        .border_1()
        .border_color(c(t.border))
        .t_caption()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(c(fg))
        .child(label.into())
}

/// A labelled text field inside a card.
pub(crate) fn field(t: Tokens, label: &'static str, input: AnyElement) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(zpx(64.))
                .flex_none()
                .t_small()
                .text_color(c(t.text_muted))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h(zpx(32.))
                .px_2p5()
                .flex()
                .items_center()
                .rounded(RADIUS_MD)
                .bg(c(t.input))
                .border_1()
                .border_color(c(t.border))
                .t_body()
                .child(input),
        )
}

pub(crate) fn row_text(
    id: impl Into<String>,
    label: &'static str,
    color: u32,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into().into()))
        .px_2p5()
        .py_1()
        .flex_none()
        .rounded(RADIUS_MD)
        .t_small()
        .text_color(c(color))
        .cursor_pointer()
        .hover_fade(c(t.hover), None)
        .active(|s| s.opacity(0.78))
        .child(label)
}

/// `filled_button` (accent) with a runtime id.
pub(crate) fn filled_button_owned(
    id: String,
    label: &'static str,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .flex_none()
        .px_2p5()
        .py_1()
        .rounded(RADIUS_MD)
        .bg(c(t.accent))
        .t_small()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(c(t.text_on_accent))
        .cursor_pointer()
        .hover(|s| s.opacity(0.9))
        .active(|s| s.opacity(0.75))
        .child(label)
}

/// `text_button` with a runtime id.
pub(in crate::view) fn text_button_owned(
    id: String,
    label: &'static str,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .px_2p5()
        .py_1()
        .flex_none()
        .rounded(RADIUS_MD)
        .t_small()
        .text_color(c(t.accent))
        .cursor_pointer()
        .hover_fade(c(t.hover), None)
        .active(|s| s.opacity(0.75))
        .child(label)
}

/// Filter pill for creators.
pub(crate) fn creator_pill(
    id: String,
    label: String,
    selected: bool,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .px_2p5()
        .h(zpx(23.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(RADIUS_SM)
        .t_small()
        .cursor_pointer()
        .border_1()
        .map(|d| {
            if selected {
                d.bg(c(t.accent).opacity(0.14))
                    .border_color(c(t.accent).opacity(0.8))
                    .text_color(c(t.accent))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .active(|s| s.opacity(0.85))
            } else {
                d.bg(c(t.hover).opacity(0.35))
                    .border_color(c(t.border).opacity(0.45))
                    .text_color(c(t.text_muted))
                    .hover(|s| s.border_color(c(t.border)).text_color(c(t.text)))
                    .hover_fade(c(t.hover), None)
                    .active(|s| s.opacity(0.75))
            }
        })
        .child(label)
}

/// Computes `(draw_pos, draw_len)` with asymmetric leading/trailing edge morphology.
/// Used both for horizontal segmented control pills and the vertical rail indicator.
pub(crate) fn compute_pill_geometry(
    start_pos: f32,
    end_pos: f32,
    item_len: f32,
    delta: f32,
) -> (f32, f32) {
    let dist = end_pos - start_pos;
    if dist.abs() < 0.1 {
        return (end_pos, item_len);
    }
    let lead_t = crate::motion::ease_out_strong(delta);
    let lag_curve = crate::motion::glide_curve(delta);
    let trail_t = 0.55 * lead_t + 0.45 * lag_curve;

    if dist > 0. {
        let curr_end = (start_pos + item_len) + dist * lead_t;
        let curr_start = start_pos + dist * trail_t;
        let curr_len = (curr_end - curr_start).max(item_len);
        (curr_start, curr_len)
    } else {
        let curr_start = start_pos + dist * lead_t;
        let curr_end = (start_pos + item_len) + dist * trail_t;
        let curr_len = (curr_end - curr_start).max(item_len);
        (curr_start, curr_len)
    }
}

/// Inner text renderer for the clipped active mask layer.
fn render_active_text_row(
    labels: &[&'static str],
    item_w: f32,
    text_color: u32,
    draw_left: f32,
) -> gpui::Div {
    const PAD: f32 = 2.0;
    const GAP: f32 = 2.0;
    const H: f32 = 24.0;
    div()
        .absolute()
        .top(zpx(0.))
        .left(zpx(PAD - draw_left))
        .h(zpx(H))
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(GAP))
        .children(labels.iter().map(|label| {
            div()
                .w(zpx(item_w))
                .h(zpx(H))
                .flex()
                .items_center()
                .justify_center()
                .t_small()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(c(text_color))
                .child(*label)
        }))
}

impl KeelWindow {
    /// Remember that switch `id` just flipped, so it slides instead of snapping.
    pub(in crate::view) fn note_switch_flip(&mut self, id: &str) {
        self.switch_gen += 1;
        self.switch_flips
            .retain(|_, (_, at)| at.elapsed() < std::time::Duration::from_secs(1));
        self.switch_flips
            .insert(id.to_string(), (self.switch_gen, std::time::Instant::now()));
    }

    /// The flip number when switch `id` changed just now (and motion is on).
    pub(in crate::view) fn recent_flip(&self, id: &str) -> Option<u64> {
        self.switch_flips
            .get(id)
            .filter(|(_, at)| {
                !self.reduced() && at.elapsed() < std::time::Duration::from_millis(150)
            })
            .map(|(n, _)| *n)
    }

    /// Remember that segmented control `id` just changed selection from `from_ix` to `to_ix`.
    pub(crate) fn note_segmented_flip(&mut self, id: &str, from_ix: usize, to_ix: usize) {
        self.segmented_gen += 1;
        self.segmented_flips
            .retain(|_, (_, _, _, at)| at.elapsed() < std::time::Duration::from_secs(1));
        self.segmented_flips.insert(
            id.to_string(),
            (
                from_ix,
                to_ix,
                self.segmented_gen,
                std::time::Instant::now(),
            ),
        );
    }

    /// The recent flip info `(from_ix, to_ix, generation)` when segmented control `id` changed just now.
    pub(crate) fn recent_segmented_flip(&self, id: &str) -> Option<(usize, usize, u64)> {
        self.segmented_flips
            .get(id)
            .filter(|(_, _, _, at)| {
                !self.reduced() && at.elapsed() < std::time::Duration::from_millis(150)
            })
            .map(|(from, to, generation, _)| (*from, *to, *generation))
    }

    /// Sleek segmented control with optical clip-mask architecture and elastic morphology.
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
        use gpui::AnimationExt as _;

        let current_ix = options
            .iter()
            .position(|(_, val)| *val == current)
            .unwrap_or(0);
        let flip = self.recent_segmented_flip(group_id);
        let count = options.len();
        const PAD: f32 = 2.0;
        const GAP: f32 = 2.0;
        const H: f32 = 24.0;
        const PILL_RADIUS: f32 = 5.0;
        const TRACK_RADIUS: f32 = 8.0;

        let inner_w =
            PAD * 2.0 + (item_w * count as f32) + (GAP * (count.saturating_sub(1)) as f32);
        let total_w = inner_w + 2.0;
        let total_h = H + PAD * 2.0 + 2.0;
        let left_of = |ix: usize| PAD + ix as f32 * (item_w + GAP);

        let labels_list: Vec<&'static str> = options.iter().map(|(l, _)| *l).collect();
        let text_on_accent = t.text_on_accent;

        let animated_pill: gpui::AnyElement = match flip {
            Some((from_ix, to_ix, generation)) => {
                let start_left = left_of(from_ix);
                let end_left = left_of(to_ix);
                let labels_copy = labels_list.clone();

                let make_pill = move |draw_left: f32, draw_w: f32| {
                    div()
                        .absolute()
                        .top(zpx(PAD))
                        .left(zpx(draw_left))
                        .w(zpx(draw_w))
                        .h(zpx(H))
                        .rounded(zpx(PILL_RADIUS))
                        .bg(c(t.accent))
                        .overflow_hidden()
                        .shadow(vec![gpui::BoxShadow {
                            color: c(t.shadow).opacity(0.2),
                            offset: gpui::point(zpx(0.), zpx(1.)),
                            blur_radius: zpx(1.5),
                            spread_radius: zpx(0.),
                            inset: false,
                        }])
                        .child(render_active_text_row(
                            &labels_copy,
                            item_w,
                            text_on_accent,
                            draw_left,
                        ))
                };

                let initial_pill = make_pill(start_left, item_w);
                initial_pill
                    .with_animation(
                        ElementId::Name(format!("{group_id}-seg-flip-{generation}").into()),
                        gpui::Animation::new(std::time::Duration::from_millis(150)),
                        move |_el, delta| {
                            let (draw_left, draw_w) =
                                compute_pill_geometry(start_left, end_left, item_w, delta);
                            make_pill(draw_left, draw_w)
                        },
                    )
                    .into_any_element()
            }
            None => {
                let draw_left = left_of(current_ix);
                div()
                    .absolute()
                    .top(zpx(PAD))
                    .left(zpx(draw_left))
                    .w(zpx(item_w))
                    .h(zpx(H))
                    .rounded(zpx(PILL_RADIUS))
                    .bg(c(t.accent))
                    .overflow_hidden()
                    .shadow(vec![gpui::BoxShadow {
                        color: c(t.shadow).opacity(0.2),
                        offset: gpui::point(zpx(0.), zpx(1.)),
                        blur_radius: zpx(1.5),
                        spread_radius: zpx(0.),
                        inset: false,
                    }])
                    .child(render_active_text_row(
                        &labels_list,
                        item_w,
                        text_on_accent,
                        draw_left,
                    ))
                    .into_any_element()
            }
        };

        let inactive_labels = div()
            .absolute()
            .top(zpx(PAD))
            .left(zpx(PAD))
            .h(zpx(H))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(GAP))
            .children(options.iter().map(|(label, _)| {
                div()
                    .w(zpx(item_w))
                    .h(zpx(H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .t_small()
                    .font_weight(gpui::FontWeight::NORMAL)
                    .text_color(c(t.text_muted))
                    .child(*label)
            }));

        let hit_targets = div()
            .absolute()
            .top(zpx(PAD))
            .left(zpx(PAD))
            .h(zpx(H))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(GAP))
            .children(options.iter().enumerate().map(|(ix, (label, val))| {
                let is_selected = ix == current_ix;
                let val = *val;
                div()
                    .id(ElementId::Name(format!("{group_id}-{label}").into()))
                    .w(zpx(item_w))
                    .h(zpx(H))
                    .rounded(zpx(PILL_RADIUS))
                    .cursor_pointer()
                    .when(!is_selected, |d| {
                        d.hover_fade(c(t.hover).opacity(0.35), None)
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if ix != current_ix {
                            this.note_segmented_flip(group_id, current_ix, ix);
                        }
                        on_select(this, val, cx);
                        cx.notify();
                    }))
            }));

        div()
            .relative()
            .flex_none()
            .w(zpx(total_w))
            .h(zpx(total_h))
            .rounded(zpx(TRACK_RADIUS))
            .bg(c(t.input))
            .border_1()
            .border_color(c(t.border))
            .child(inactive_labels)
            .child(animated_pill)
            .child(hit_targets)
    }
}

/// A switch: a pill with a knob that stretches mid-travel with asymmetric
/// leading/trailing edge morphology.
pub(in crate::view) fn toggle(
    id: String,
    on: bool,
    flip: Option<u64>,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    use gpui::AnimationExt as _;
    const TRAVEL: f32 = 12.;
    const KNOB: f32 = 14.;

    let paint = move |is_on: bool, delta: f32| {
        let lead_t = crate::motion::ease_out_strong(delta);
        let lag_curve = crate::motion::glide_curve(delta);
        let trail_t = 0.55 * lead_t + 0.45 * lag_curve;

        let (left, width, track_progress) = if is_on {
            let right = KNOB + TRAVEL * lead_t;
            let l = TRAVEL * trail_t;
            let w = (right - l).max(KNOB);
            (l, w, lead_t)
        } else {
            let l = TRAVEL * (1.0 - lead_t);
            let right = KNOB + TRAVEL * (1.0 - trail_t);
            let w = (right - l).max(KNOB);
            (l, w, 1.0 - lead_t)
        };

        let track_bg = mix(t.input, t.accent, track_progress);
        let track_border = mix(t.border, t.accent, track_progress);

        let knob_bg = if t == keel_theme::Tokens::dark() {
            mix(t.text_faint, t.text_on_accent, track_progress)
        } else {
            0xFFFFFFFF
        };

        div()
            .w(zpx(30.))
            .h(zpx(18.))
            .p(zpx(1.))
            .flex_none()
            .rounded_full()
            .flex()
            .items_center()
            .bg(c(track_bg))
            .border_1()
            .border_color(c(track_border))
            .child(
                div()
                    .h(zpx(KNOB))
                    .rounded_full()
                    .w(zpx(width))
                    .ml(zpx(left))
                    .bg(c(knob_bg))
                    .border_1()
                    .border_color(c(0x00000018))
                    .shadow(vec![gpui::BoxShadow {
                        color: c(0x00000028),
                        offset: gpui::point(zpx(0.), zpx(1.)),
                        blur_radius: zpx(1.5),
                        spread_radius: zpx(0.),
                        inset: false,
                    }]),
            )
    };

    let body: gpui::AnyElement = match flip {
        Some(n) => {
            let initial_track = paint(on, 0.0);
            initial_track
                .with_animation(
                    ElementId::Name(format!("{id}-flip-{n}").into()),
                    gpui::Animation::new(std::time::Duration::from_millis(150)),
                    move |_el, delta| paint(on, delta),
                )
                .into_any_element()
        }
        None => paint(on, 1.0).into_any_element(),
    };

    div()
        .id(ElementId::Name(id.into()))
        .flex_none()
        .rounded_full()
        .cursor_pointer()
        .child(body)
        .hover_fade(c(t.hover), Some(c(t.selected)))
}

/// Blend two `0xRRGGBBAA` colors, `at` 0 → `from`, 1 → `to`.
pub(crate) fn mix(from: u32, to: u32, at: f32) -> u32 {
    let at = at.clamp(0., 1.);
    (0..4).fold(0, |acc, i| {
        let shift = 24 - 8 * i;
        let (a, b) = (
            ((from >> shift) & 0xFF) as f32,
            ((to >> shift) & 0xFF) as f32,
        );
        acc | (((a + (b - a) * at).round() as u32) << shift)
    })
}

/// Morphing audio preview button: smoothly transitions between idle (Play + "Preview")
/// and active (Volume2 + "Playing…").
pub(crate) fn render_sound_preview_btn(
    is_playing: bool,
    flip: Option<u64>,
    reduced: bool,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    use gpui::AnimationExt as _;

    let paint = move |is_playing: bool, progress: f32| {
        let curve = if is_playing {
            crate::motion::ease_out_strong(progress)
        } else {
            crate::motion::ease_out(progress)
        };
        let play_ratio = if is_playing { curve } else { 1.0 - curve };

        let accent_fill = (t.accent & 0xFFFFFF00) | 0x22;
        let accent_border = (t.accent & 0xFFFFFF00) | 0x66;
        let bg_color = mix(t.input, accent_fill, play_ratio);
        let border_color = mix(t.border, accent_border, play_ratio);
        let text_color = mix(t.text_muted, t.accent, play_ratio);

        let idle_opacity = (1.0 - play_ratio).clamp(0.0, 1.0);
        let playing_opacity = play_ratio.clamp(0.0, 1.0);
        let idle_top = -3.5 * play_ratio;
        let playing_top = 3.5 * (1.0 - play_ratio);

        let idle_layer = div()
            .absolute()
            .inset_0()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap_1p5()
            .opacity(idle_opacity)
            .top(zpx(idle_top))
            .child(Icon::Play.el(zpx(10.)).text_color(c(text_color)))
            .child(div().t_small().text_color(c(text_color)).child("Preview"));

        let playing_layer = div()
            .absolute()
            .inset_0()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap_1p5()
            .opacity(playing_opacity)
            .top(zpx(playing_top))
            .child(Icon::Volume2.el(zpx(12.)).text_color(c(text_color)))
            .child(div().t_small().text_color(c(text_color)).child("Playing…"));

        div()
            .w(zpx(88.))
            .h(zpx(26.))
            .relative()
            .rounded(RADIUS_MD)
            .overflow_hidden()
            .bg(c(bg_color))
            .border_1()
            .border_color(c(border_color))
            .child(idle_layer)
            .child(playing_layer)
    };

    let body: gpui::AnyElement = match flip {
        Some(n) if !reduced => {
            let initial = paint(is_playing, 0.0);
            initial
                .with_animation(
                    ElementId::Name(format!("sound-preview-flip-{n}").into()),
                    gpui::Animation::new(std::time::Duration::from_millis(200)),
                    move |_el, delta| paint(is_playing, delta),
                )
                .into_any_element()
        }
        _ => paint(is_playing, 1.0).into_any_element(),
    };

    div()
        .id(ElementId::Name("sound-preview-btn".into()))
        .flex_none()
        .cursor_pointer()
        .hover(|s| s.opacity(0.92))
        .active(|s| s.opacity(0.75))
        .child(body)
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

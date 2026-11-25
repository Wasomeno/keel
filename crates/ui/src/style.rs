//! Design scale: color conversion, type ramp, radii, spacing, and the
//! projection of Keel tokens onto `gpui-base`'s theme. Views use these
//! instead of literal sizes and colors.

use gpui::{App, FontWeight, Hsla, Pixels, Styled, px, rgba};
use gpui_base::Theme;
use keel_theme::Tokens;

/// `0xRRGGBBAA` token to a paint color.
pub fn c(rgba_value: u32) -> Hsla {
    rgba(rgba_value).into()
}

pub const SANS: &str = ".SystemUIFont";
/// SF Mono: registered at startup from Terminal.app (see `load_mono`), so
/// code reads in the system's own monospace rather than Menlo.
pub const MONO: &str = "SF Mono";

/// Register SF Mono from Terminal.app, where macOS ships it.
// ponytail: macOS-only path; bundle a mono font if Keel ever runs elsewhere.
pub fn load_mono(cx: &gpui::App) {
    const DIR: &str = "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts";
    let faces: Vec<std::borrow::Cow<'static, [u8]>> = [
        "Regular",
        "RegularItalic",
        "Medium",
        "MediumItalic",
        "Semibold",
        "SemiboldItalic",
        "Bold",
        "BoldItalic",
    ]
    .iter()
    .filter_map(|w| std::fs::read(format!("{DIR}/SF-Mono-{w}.otf")).ok())
    .map(std::borrow::Cow::Owned)
    .collect();
    if faces.is_empty() || cx.text_system().add_fonts(faces).is_err() {
        eprintln!("keel: SF Mono not found in Terminal.app; code falls back to the system font");
    }
}

/// Radii: badges `SM`, buttons and inputs `MD`, cards `LG`, modals `XL`.
/// These constants are unscaled `px`. Settings paints the same steps through
/// [`r_sm`], [`r_md`], [`r_lg`], and [`r_xl`] so they follow zoom.
pub const RADIUS_SM: Pixels = px(4.);
pub const RADIUS_MD: Pixels = px(6.);
pub const RADIUS_LG: Pixels = px(10.);
pub const RADIUS_XL: Pixels = px(14.);

/// Spacing scale in logical pixels. Paint with [`zpx`].
pub const SP: [f32; 9] = [2., 4., 6., 8., 12., 16., 20., 24., 32.];

/// Fixed control and settings-chrome heights, logical pixels. Paint with [`zpx`].
pub const CONTROL_H: f32 = 26.;
pub const INPUT_H: f32 = 28.;
pub const NAV_ITEM_H: f32 = 30.;
pub const ROW_MIN_H: f32 = 44.;
pub const HEADER_H: f32 = 52.;

/// Zoomed radii. Settings uses these; the `RADIUS_*` constants stay unscaled.
pub fn r_sm() -> Pixels {
    zpx(4.)
}
pub fn r_md() -> Pixels {
    zpx(6.)
}
pub fn r_lg() -> Pixels {
    zpx(10.)
}
pub fn r_xl() -> Pixels {
    zpx(14.)
}

/// Fixed chrome heights.
pub const HEADER_HEIGHT: Pixels = px(40.);
/// Space the macOS traffic lights take at the left of the title bar.
pub const TRAFFIC_LIGHTS_WIDTH: Pixels = px(78.);
pub const TRANSCRIPT_MAX_WIDTH: Pixels = px(760.);
/// Horizontal padding on each side of the reading column (`px_4`).
pub const COLUMN_PAD: f32 = 16.;

/// UI zoom (⌘+ / ⌘- / ⌘0): a factor every fixed size goes through via
/// [`zpx`]. Process-wide, set from the view and `ui-settings.json`.
static ZOOM_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3F80_0000); // 1.0

/// Zoom levels ⌘+ / ⌘- step through.
pub const ZOOM_STEPS: [f32; 10] = [0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];

pub fn zoom() -> f32 {
    f32::from_bits(ZOOM_BITS.load(std::sync::atomic::Ordering::Relaxed))
}

pub fn set_zoom(factor: f32) {
    let factor = factor.clamp(ZOOM_STEPS[0], ZOOM_STEPS[ZOOM_STEPS.len() - 1]);
    ZOOM_BITS.store(factor.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// The zoom step after (`up`) or before `current`, staying at the ends.
pub fn zoom_step(current: f32, up: bool) -> f32 {
    let next = if up {
        ZOOM_STEPS
            .iter()
            .copied()
            .find(|step| *step > current + 0.001)
    } else {
        ZOOM_STEPS
            .iter()
            .rev()
            .copied()
            .find(|step| *step < current - 0.001)
    };
    next.unwrap_or(current)
}

/// `n` logical pixels at the current zoom. Use for fixed sizes in the UI;
/// measured positions and sizes stay in plain `px`.
pub fn zpx(n: f32) -> Pixels {
    px(n * zoom())
}

/// Both sides of [`COLUMN_PAD`], in logical pixels at the current zoom.
pub fn column_gutter() -> Pixels {
    zpx(COLUMN_PAD * 2.)
}

/// The docked composer's max width, and the transcript prose measure, in
/// logical pixels: the reading column less `px_4` on each side.
pub fn column_content_max() -> Pixels {
    TRANSCRIPT_MAX_WIDTH - column_gutter()
}

/// One step of the type ramp: size, line height, weight.
#[derive(Clone, Copy)]
pub struct TypeStep {
    pub size: f32,
    pub line: f32,
    pub weight: FontWeight,
}

/// Desktop-dense ramp. Body is 13 px like native macOS lists.
pub const CAPTION: TypeStep = TypeStep {
    size: 11.,
    line: 14.,
    weight: FontWeight::NORMAL,
};
pub const SMALL: TypeStep = TypeStep {
    size: 12.,
    line: 16.,
    weight: FontWeight::NORMAL,
};
pub const BODY: TypeStep = TypeStep {
    size: 13.,
    line: 18.,
    weight: FontWeight::NORMAL,
};
pub const READING: TypeStep = TypeStep {
    size: 14.,
    line: 22.,
    weight: FontWeight::NORMAL,
};
pub const TITLE: TypeStep = TypeStep {
    size: 15.,
    line: 20.,
    weight: FontWeight::SEMIBOLD,
};
pub const CODE: TypeStep = TypeStep {
    size: 12.5,
    line: 18.,
    weight: FontWeight::NORMAL,
};

/// Type and surface shorthands on any styled element.
pub trait KeelStyled: Styled + Sized {
    fn type_step(self, step: TypeStep) -> Self {
        self.text_size(zpx(step.size))
            .line_height(zpx(step.line))
            .font_weight(step.weight)
    }
    fn t_caption(self) -> Self {
        self.type_step(CAPTION)
    }
    fn t_small(self) -> Self {
        self.type_step(SMALL)
    }
    fn t_body(self) -> Self {
        self.type_step(BODY)
    }
    fn t_reading(self) -> Self {
        self.type_step(READING)
    }
    fn t_title(self) -> Self {
        self.type_step(TITLE)
    }
    fn t_code(self) -> Self {
        self.type_step(CODE).font_family(MONO)
    }
    /// Popover / menu surface: floating fill, hairline, large radius, shadow.
    fn floating_surface(self, t: Tokens) -> Self {
        self.bg(c(t.floating))
            .border_1()
            .border_color(c(t.border))
            .rounded(RADIUS_LG)
            .shadow(vec![gpui::BoxShadow {
                color: c(t.shadow),
                offset: gpui::point(zpx(0.), zpx(8.)),
                blur_radius: zpx(24.),
                spread_radius: zpx(-4.),
                inset: false,
            }])
    }
}

impl<E: Styled> KeelStyled for E {}

/// Make `gpui-base` components (inputs, popovers, scrollbars) paint with Keel's
/// tokens. Its defaults are fully transparent, so this runs before first paint
/// and again whenever the appearance changes.
pub fn apply_base_theme(t: Tokens, cx: &mut App) {
    let theme = Theme::global_mut(cx);
    let colors = &mut theme.tokens.colors;
    colors.background = c(t.window);
    colors.foreground = c(t.text);
    colors.surface = c(t.floating);
    colors.surface_foreground = c(t.text);
    colors.primary = c(t.accent);
    colors.primary_foreground = c(t.text_on_accent);
    colors.secondary = c(t.surface);
    colors.secondary_foreground = c(t.text);
    colors.muted = c(t.surface);
    colors.muted_foreground = c(t.text_muted);
    colors.accent = c(t.hover);
    colors.accent_foreground = c(t.text);
    colors.destructive = c(t.danger);
    colors.destructive_foreground = c(t.text_on_accent);
    colors.border = c(t.border);
    colors.input = c(t.input);
    colors.ring = c(t.focus);
    theme.tokens.typography.sans = SANS.into();
    theme.tokens.typography.mono = MONO.into();
    theme.tokens.shadow = gpui_base::theme_tokens::ShadowTokens::elevations(c(t.shadow));
    theme.resizable.handle = Some(c(t.border));
    theme.resizable.active_handle = Some(c(t.accent));
}

#[cfg(test)]
mod zoom_tests {
    use super::*;

    #[test]
    fn zoom_steps_and_clamps() {
        assert_eq!(zoom_step(1.0, true), 1.1);
        assert_eq!(zoom_step(1.0, false), 0.9);
        assert_eq!(zoom_step(2.0, true), 2.0);
        assert_eq!(zoom_step(0.67, false), 0.67);
        // Off-step values snap to the next step in that direction.
        assert_eq!(zoom_step(1.05, true), 1.1);
    }
}

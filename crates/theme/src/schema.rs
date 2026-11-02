//! Semantic theme schema.
//!
//! Components read roles, never raw colors. Every value is `0xRRGGBBAA`
//! (sRGB plus alpha) so this crate stays free of the UI toolkit; translucent
//! roles (`hover`, `selected`, `border`) are overlays on whatever is under them.

/// Semantic color roles for one appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tokens {
    // Surfaces, back to front.
    /// Main canvas: transcript and tab strip.
    pub window: u32,
    /// Sidebar canvas; a step away from `window` so the split reads without a border.
    pub sidebar: u32,
    /// Raised content: user bubble, cards, question panel.
    pub surface: u32,
    /// Popovers and menus.
    pub floating: u32,
    /// Text fields.
    pub input: u32,
    /// Code blocks and inline code.
    pub code: u32,
    /// Overlay for a hovered row or control.
    pub hover: u32,
    /// Overlay for the selected row or tab.
    pub selected: u32,
    /// Hairline dividers and outlines.
    pub border: u32,
    /// Drop shadow under floating surfaces.
    pub shadow: u32,

    // Text.
    pub text: u32,
    /// Secondary text: metadata, labels.
    pub text_muted: u32,
    /// Tertiary text: placeholders, disabled.
    pub text_faint: u32,
    /// Text on an `accent` or `danger` fill.
    pub text_on_accent: u32,

    // Interaction.
    pub accent: u32,
    pub accent_hover: u32,
    /// Focus ring.
    pub focus: u32,
    /// Text selection fill.
    pub selection: u32,

    // Status.
    pub danger: u32,
    pub warning: u32,
    pub success: u32,
    /// Information / active indicators (blue).
    pub info: u32,
}

impl Tokens {
    /// Built-in light scheme.
    pub const fn light() -> Self {
        Self {
            window: 0xF7F4EEFF,
            sidebar: 0xECE6DCFF,
            surface: 0xE4DDD2FF,
            floating: 0xEEEAE4FF,
            input: 0xE8E3DAFF,
            code: 0xEBE5DBFF,
            hover: 0x1E1D1B0D,
            selected: 0x1E1D1B17,
            border: 0x1E1D1B1A,
            shadow: 0x1E1D1B24,

            text: 0x1E1D1BFF,
            text_muted: 0x615C54FF,
            text_faint: 0x8C857AFF,
            text_on_accent: 0xEEEAE4FF,

            // Neutral: warm near-black ink.
            accent: 0x1E1D1BFF,
            accent_hover: 0x35322EFF,
            focus: 0x1E1D1B4D,
            selection: 0x1E1D1B26,

            danger: 0xBA3333FF,
            warning: 0xA66408FF,
            success: 0x1F7A3CFF,
            info: 0x1D68BDFF,
        }
    }

    /// Built-in dark scheme.
    pub const fn dark() -> Self {
        Self {
            window: 0x161618FF,
            sidebar: 0x111113FF,
            surface: 0x1F1F22FF,
            floating: 0x1C1C1EFF,
            input: 0x1C1C1FFF,
            code: 0x1B1B1EFF,
            hover: 0xFFFFFF0D,
            selected: 0xFFFFFF17,
            border: 0xFFFFFF14,
            shadow: 0x00000073,

            text: 0xEDEDEFFF,
            text_muted: 0xA1A1AAFF,
            text_faint: 0x71717AFF,
            // Dark ink: the accent is a light neutral, white wouldn't read on it.
            text_on_accent: 0x18181BFF,

            // Neutral: light grey, no hue.
            accent: 0xD4D4D8FF,
            accent_hover: 0xE4E4E7FF,
            focus: 0xD4D4D880,
            selection: 0xFFFFFF33,

            danger: 0xF2555AFF,
            warning: 0xF0A63AFF,
            success: 0x3FB950FF,
            info: 0x388BFDFF,
        }
    }
}

/// Syntax color paint map for code syntax highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxPaintMap {
    pub keyword: u32,
    pub string: u32,
    pub comment: u32,
    pub number: u32,
    pub function: u32,
    pub type_name: u32,
    pub plain: u32,
}

impl SyntaxPaintMap {
    /// Built-in light scheme for syntax highlighting.
    pub const fn light() -> Self {
        Self {
            keyword: 0x7E22CE,
            string: 0x166534,
            comment: 0x6E685E,
            number: 0x9A3412,
            function: 0x1D4ED8,
            type_name: 0x0F766E,
            plain: 0x1E1D1B,
        }
    }

    /// Built-in dark scheme for syntax highlighting.
    pub const fn dark() -> Self {
        Self {
            keyword: 0xC084FC,
            string: 0x4ADE80,
            comment: 0x9CA3AF,
            number: 0xFBBF24,
            function: 0x60A5FA,
            type_name: 0x2DD4BF,
            plain: 0xEDEDEF,
        }
    }
}

/// WCAG relative-luminance contrast of `fg` drawn over `bg` (alpha composited).
pub fn contrast(fg: u32, bg: u32) -> f32 {
    let (bg_r, bg_g, bg_b, _) = split(bg);
    let (r, g, b, a) = split(fg);
    let mix = |c: f32, under: f32| c * a + under * (1.0 - a);
    let top = luminance(mix(r, bg_r), mix(g, bg_g), mix(b, bg_b));
    let base = luminance(bg_r, bg_g, bg_b);
    let (hi, lo) = if top > base { (top, base) } else { (base, top) };
    (hi + 0.05) / (lo + 0.05)
}

fn split(rgba: u32) -> (f32, f32, f32, f32) {
    let c = |shift: u32| ((rgba >> shift) & 0xFF) as f32 / 255.0;
    (c(24), c(16), c(8), c(0))
}

fn luminance(r: f32, g: f32, b: f32) -> f32 {
    let lin = |c: f32| {
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Primary text 4.5:1 and muted 3:1 on every surface it sits on, in both schemes.
    #[test]
    fn text_roles_meet_contrast_on_every_surface() {
        for (name, t) in [("light", Tokens::light()), ("dark", Tokens::dark())] {
            for (surface_name, surface) in [
                ("window", t.window),
                ("sidebar", t.sidebar),
                ("surface", t.surface),
                ("floating", t.floating),
                ("input", t.input),
                ("code", t.code),
            ] {
                let primary = contrast(t.text, surface);
                assert!(primary >= 4.5, "{name} text on {surface_name}: {primary:.2}");
                let muted = contrast(t.text_muted, surface);
                assert!(muted >= 3.0, "{name} muted on {surface_name}: {muted:.2}");
            }
            for (fill_name, fill) in [("accent", t.accent), ("danger", t.danger)] {
                let on = contrast(t.text_on_accent, fill);
                assert!(on >= 3.0, "{name} text_on_accent on {fill_name}: {on:.2}");
            }
            let accent = contrast(t.accent, t.window);
            assert!(accent >= 3.0, "{name} accent on window: {accent:.2}");
        }
    }

    #[test]
    fn contrast_of_black_on_white_is_21() {
        assert!((contrast(0x000000FF, 0xFFFFFFFF) - 21.0).abs() < 0.01);
    }
}

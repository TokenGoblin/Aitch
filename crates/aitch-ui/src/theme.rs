//! Colors. Phase 0 needs exactly one of them.
//!
//! Phase 5 replaces this with a loadable theme plus `aitchrc` overrides.

/// A linear-space RGBA color, the form wgpu wants for a clear value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    /// Build a color from 8-bit sRGB, converting to linear space.
    ///
    /// The surface is configured with an sRGB format, so a clear value has to
    /// be linear or the color comes out washed out.
    pub fn srgb(r: u8, g: u8, b: u8) -> Color {
        Color::srgba(r, g, b, 1.0)
    }

    /// The same, with an alpha in 0..=1. Alpha is linear in sRGB, so it is
    /// taken as given rather than run through the transfer function.
    pub fn srgba(r: u8, g: u8, b: u8, a: f64) -> Color {
        Color {
            r: srgb_to_linear(r),
            g: srgb_to_linear(g),
            b: srgb_to_linear(b),
            a,
        }
    }
}

impl From<Color> for wgpu::Color {
    fn from(c: Color) -> wgpu::Color {
        wgpu::Color {
            r: c.r,
            g: c.g,
            b: c.b,
            a: c.a,
        }
    }
}

/// The color set the editor draws with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    pub cursor: Color,
    /// Drawn behind selected text, so it has to stay readable through it.
    pub selection: Color,
}

impl Theme {
    pub fn dark() -> Theme {
        Theme {
            background: Color::srgb(0x14, 0x16, 0x1a),
            foreground: Color::srgb(0xd4, 0xd7, 0xdd),
            cursor: Color::srgb(0x7a, 0xa2, 0xf7),
            selection: Color::srgba(0x3d, 0x59, 0xa1, 0.55),
        }
    }
}

impl Default for Theme {
    fn default() -> Theme {
        Theme::dark()
    }
}

/// The sRGB transfer function, in reverse.
fn srgb_to_linear(channel: u8) -> f64 {
    let c = channel as f64 / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_endpoints_map_to_the_linear_endpoints() {
        assert!(srgb_to_linear(0).abs() < 1e-12);
        assert!((srgb_to_linear(255) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn srgb_is_monotonic() {
        let mut previous = -1.0;
        for channel in 0..=255u8 {
            let value = srgb_to_linear(channel);
            assert!(value > previous, "channel {channel} went backwards");
            previous = value;
        }
    }

    #[test]
    fn the_dark_background_is_dark_and_opaque() {
        let bg = Theme::dark().background;
        assert!(bg.r < 0.02 && bg.g < 0.02 && bg.b < 0.02);
        assert_eq!(bg.a, 1.0);
    }

    #[test]
    fn the_selection_is_translucent_so_text_reads_through_it() {
        let selection = Theme::dark().selection;
        assert!(selection.a > 0.0 && selection.a < 1.0);
    }

    #[test]
    fn text_is_legible_against_the_background() {
        let theme = Theme::dark();
        // Not a contrast-ratio calculation, just a guard against a theme that
        // paints text the same color as the page it sits on.
        assert!(theme.foreground.g > theme.background.g * 10.0);
        assert!(theme.cursor.b > theme.background.b);
    }
}

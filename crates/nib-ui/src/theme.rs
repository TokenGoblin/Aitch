//! Colors. Phase 0 needs exactly one of them.
//!
//! Phase 5 replaces this with a loadable theme plus `nibrc` overrides.

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
        Color {
            r: srgb_to_linear(r),
            g: srgb_to_linear(g),
            b: srgb_to_linear(b),
            a: 1.0,
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
}

impl Theme {
    pub fn dark() -> Theme {
        Theme {
            background: Color::srgb(0x14, 0x16, 0x1a),
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
}

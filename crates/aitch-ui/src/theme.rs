//! Colors: the editor's chrome and the syntax palette.
//!
//! Two themes, one light and one dark, as PLAN.md Phase 5 asks. Loading one
//! from `aitchrc` is Phase 7; what matters here is that both are complete, so
//! that choosing the light one does not leave half the screen unreadable.

use aitch_core::Token;

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
    /// The status line and footer sit on this.
    pub chrome_background: Color,
    pub chrome_foreground: Color,
    /// A footer chord is drawn reversed, the way nano draws it.
    pub key_background: Color,
    pub key_foreground: Color,

    /// Behind the line the cursor is on. Barely there on purpose: it should
    /// answer "where am I" without competing with the text.
    pub current_line: Color,
    /// The gutter, when line numbers are on.
    pub line_number: Color,
    /// Behind a bracket and its partner.
    pub bracket_match: Color,
    /// Tabs and trailing spaces, when whitespace rendering is on.
    pub whitespace: Color,

    /// One colour per [`Token`]. Kept as a struct rather than a map so that
    /// adding a token is a compile error here rather than a silently
    /// uncoloured language.
    pub syntax: Syntax,
}

/// The syntax palette.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Syntax {
    pub keyword: Color,
    pub string: Color,
    pub comment: Color,
    pub function: Color,
    pub type_name: Color,
    pub number: Color,
    pub constant: Color,
    pub variable: Color,
    pub property: Color,
    pub operator: Color,
    pub punctuation: Color,
    pub attribute: Color,
    pub namespace: Color,
    pub heading: Color,
}

impl Theme {
    pub fn dark() -> Theme {
        Theme {
            background: Color::srgb(0x14, 0x16, 0x1a),
            foreground: Color::srgb(0xd4, 0xd7, 0xdd),
            cursor: Color::srgb(0x7a, 0xa2, 0xf7),
            selection: Color::srgba(0x3d, 0x59, 0xa1, 0.55),
            chrome_background: Color::srgb(0x1e, 0x21, 0x28),
            chrome_foreground: Color::srgb(0xc0, 0xc5, 0xce),
            key_background: Color::srgb(0xc0, 0xc5, 0xce),
            key_foreground: Color::srgb(0x14, 0x16, 0x1a),

            current_line: Color::srgba(0x2a, 0x2e, 0x3a, 0.5),
            line_number: Color::srgb(0x50, 0x57, 0x6b),
            bracket_match: Color::srgba(0x7a, 0xa2, 0xf7, 0.35),
            whitespace: Color::srgb(0x35, 0x3a, 0x48),

            syntax: Syntax {
                keyword: Color::srgb(0xbb, 0x9a, 0xf7),
                string: Color::srgb(0x9e, 0xce, 0x6a),
                comment: Color::srgb(0x63, 0x6d, 0x93),
                function: Color::srgb(0x7a, 0xa2, 0xf7),
                type_name: Color::srgb(0x2a, 0xc3, 0xde),
                number: Color::srgb(0xff, 0x9e, 0x64),
                constant: Color::srgb(0xff, 0x9e, 0x64),
                variable: Color::srgb(0xc0, 0xca, 0xf5),
                property: Color::srgb(0x73, 0xda, 0xca),
                operator: Color::srgb(0x89, 0xdd, 0xff),
                punctuation: Color::srgb(0x9a, 0xa5, 0xce),
                attribute: Color::srgb(0xe0, 0xaf, 0x68),
                namespace: Color::srgb(0x2a, 0xc3, 0xde),
                heading: Color::srgb(0x7a, 0xa2, 0xf7),
            },
        }
    }

    /// The light theme. PLAN.md Phase 5 asks for two to start, one of each,
    /// because a dark-only editor is unusable in a bright room.
    pub fn light() -> Theme {
        Theme {
            background: Color::srgb(0xfa, 0xfa, 0xfa),
            foreground: Color::srgb(0x38, 0x3a, 0x42),
            cursor: Color::srgb(0x40, 0x78, 0xf2),
            selection: Color::srgba(0x40, 0x78, 0xf2, 0.22),
            chrome_background: Color::srgb(0xe8, 0xe9, 0xeb),
            chrome_foreground: Color::srgb(0x38, 0x3a, 0x42),
            key_background: Color::srgb(0x38, 0x3a, 0x42),
            key_foreground: Color::srgb(0xfa, 0xfa, 0xfa),

            current_line: Color::srgba(0x38, 0x3a, 0x42, 0.06),
            line_number: Color::srgb(0x9d, 0xa0, 0xa8),
            bracket_match: Color::srgba(0x40, 0x78, 0xf2, 0.25),
            whitespace: Color::srgb(0xd0, 0xd2, 0xd6),

            syntax: Syntax {
                keyword: Color::srgb(0xa6, 0x26, 0xa4),
                string: Color::srgb(0x50, 0xa1, 0x4f),
                // 3.5:1 against the background. Lighter greys look right in
                // a mockup and vanish on a real screen; a test holds the line.
                comment: Color::srgb(0x82, 0x85, 0x8f),
                function: Color::srgb(0x40, 0x78, 0xf2),
                type_name: Color::srgb(0xc1, 0x84, 0x01),
                number: Color::srgb(0x98, 0x68, 0x01),
                constant: Color::srgb(0x98, 0x68, 0x01),
                variable: Color::srgb(0x38, 0x3a, 0x42),
                property: Color::srgb(0x01, 0x84, 0xbc),
                operator: Color::srgb(0x01, 0x84, 0xbc),
                punctuation: Color::srgb(0x53, 0x55, 0x5d),
                attribute: Color::srgb(0xc1, 0x84, 0x01),
                namespace: Color::srgb(0xc1, 0x84, 0x01),
                heading: Color::srgb(0x40, 0x78, 0xf2),
            },
        }
    }

    /// What colour a highlighted run should be.
    pub fn color_for(&self, token: Token) -> Color {
        match token {
            Token::Keyword => self.syntax.keyword,
            Token::String => self.syntax.string,
            Token::Comment => self.syntax.comment,
            Token::Function => self.syntax.function,
            Token::Type => self.syntax.type_name,
            Token::Number => self.syntax.number,
            Token::Constant => self.syntax.constant,
            Token::Variable => self.syntax.variable,
            Token::Property => self.syntax.property,
            Token::Operator => self.syntax.operator,
            Token::Punctuation => self.syntax.punctuation,
            Token::Attribute => self.syntax.attribute,
            Token::Namespace => self.syntax.namespace,
            Token::Heading => self.syntax.heading,
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

    /// Rough relative luminance, for asserting that text can be read.
    fn luminance(color: Color) -> f64 {
        0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b
    }

    /// How far apart two colours are, as a contrast ratio.
    fn contrast(a: Color, b: Color) -> f64 {
        let (light, dark) = if luminance(a) > luminance(b) {
            (luminance(a), luminance(b))
        } else {
            (luminance(b), luminance(a))
        };
        (light + 0.05) / (dark + 0.05)
    }

    #[test]
    fn every_token_is_legible_in_both_themes() {
        // A palette entry that blends into the background is worse than no
        // highlighting at all: the text disappears.
        let tokens = [
            Token::Keyword,
            Token::String,
            Token::Comment,
            Token::Function,
            Token::Type,
            Token::Number,
            Token::Constant,
            Token::Variable,
            Token::Property,
            Token::Operator,
            Token::Punctuation,
            Token::Attribute,
            Token::Namespace,
            Token::Heading,
        ];

        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for token in tokens {
                let ratio = contrast(theme.color_for(token), theme.background);
                assert!(
                    ratio >= 3.0,
                    "{name}: {token:?} has only {ratio:.1}:1 against the background"
                );
            }
        }
    }

    #[test]
    fn comments_are_quieter_than_code_but_still_readable() {
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            let comment = contrast(theme.syntax.comment, theme.background);
            let keyword = contrast(theme.syntax.keyword, theme.background);
            assert!(
                comment < keyword,
                "{name}: comments should recede behind keywords"
            );
            assert!(comment >= 3.0, "{name}: but still be readable");
        }
    }

    #[test]
    fn the_light_theme_is_actually_light() {
        assert!(luminance(Theme::light().background) > 0.7);
        assert!(luminance(Theme::dark().background) < 0.05);
    }

    #[test]
    fn the_quiet_backgrounds_stay_quiet() {
        // Current-line and bracket highlights sit behind text that must stay
        // readable through them, so they are translucent and faint.
        for theme in [Theme::dark(), Theme::light()] {
            assert!(theme.current_line.a < 0.6);
            assert!(theme.bracket_match.a < 0.6);
        }
    }

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
    fn a_footer_key_is_drawn_reversed() {
        // nano shows the chord in reverse video; that is how the eye finds it.
        let theme = Theme::dark();
        assert!(theme.key_background.r > theme.key_foreground.r);
        assert!(theme.chrome_foreground.r > theme.chrome_background.r);
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

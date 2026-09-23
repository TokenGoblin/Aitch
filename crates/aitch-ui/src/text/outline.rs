//! The shared glyph-outline contract between the font parser and the
//! rasterizer.
//!
//! Defined once, up front, rather than left for two parallel tracks to
//! converge on independently — Phase 0's Track B and Track C happened to
//! both pick `isize` for a raw window handle with no prior agreement, and
//! that agent's own retro flagged relying on that twice as unwise for a
//! richer shared type. This one is richer, so it is settled here instead.

/// A single point in a glyph contour, in font units — the em square defined
/// by the font's `unitsPerEm` (from `head`).
///
/// TrueType's `glyf` table encodes a contour as a mix of on-curve points
/// (the outline passes through them) and off-curve points (quadratic Bezier
/// control points, per the TrueType spec's "midpoint of two consecutive
/// off-curve points is an implied on-curve point" rule). This mirrors that
/// directly rather than pre-flattening to line segments, so the rasterizer
/// chooses its own curve-flattening tolerance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutlinePoint {
    pub x: i16,
    pub y: i16,
    pub on_curve: bool,
}

/// One closed contour: a loop of points with an implied edge from the last
/// point back to the first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contour {
    pub points: Vec<OutlinePoint>,
}

/// A parsed glyph, ready to rasterize: its outline, and how far the pen
/// advances afterward.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlyphOutline {
    /// Zero or more closed contours, in font units. A glyph with no ink
    /// (space) has none.
    pub contours: Vec<Contour>,
    /// Horizontal advance, in font units, from `hmtx`.
    pub advance_width: u16,
}

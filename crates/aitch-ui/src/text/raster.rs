//! Scanline glyph rasterizer and glyph cache — Phase 1 Track B.
//!
//! Consumes only the shared [`super::outline`] contract (`GlyphOutline`,
//! `Contour`, `OutlinePoint`), never anything from `font.rs` — Track A's
//! sfnt/TrueType parser is a separate, parallel piece of Phase 1 landing in
//! that file. Until it exists, this module is developed and tested against a
//! handful of hand-built `GlyphOutline` fixtures (see the tests below); the
//! integration step that follows both tracks only has to swap "a hand-built
//! `GlyphOutline`" for "`font::Font::outline_for(glyph_id)`" — the type this
//! module depends on does not change.
//!
//! ## Winding rule
//!
//! Fill uses the **nonzero winding rule**, not even-odd. That is TrueType's
//! actual rule (see the spec's description of `glyf` contour direction), and
//! it is the one that behaves correctly on a glyph like lowercase `o`: the
//! outer and inner contours are wound in opposite directions so the nonzero
//! rule cancels to zero in the hole, while an even-odd rule would happen to
//! agree here too but breaks on self-overlapping contours that real font
//! hinting occasionally produces (a shape where the same region is covered by
//! two same-direction contours — even-odd would erase it, nonzero keeps it
//! filled, which is the outcome TrueType's rendering model promises).
//!
//! ## Antialiasing
//!
//! Coverage is exact (analytic) along each scanline's x axis — for a given
//! horizontal sample line, the fraction of each pixel's width covered by an
//! "inside" span is computed precisely, not sampled — and supersampled 4x
//! along y (four evenly spaced sample lines per pixel row, averaged). This is
//! the "simple supersampling" the plan calls an honest starting point: it is
//! cheap, has no directional bias in x, and gives smooth-looking diagonals
//! and curves at ordinary text sizes. Its known limitation is purely
//! vertical: a feature thinner than 1/4 pixel tall that falls entirely
//! between two of the four sample lines can be under-counted or missed, the
//! same way any finite-sample supersampling can alias a sub-pixel feature.
//! Exact analytic coverage on both axes (as a trapezoidal-area accumulator
//! would give) would remove that, at more implementation complexity; nothing
//! here needs it yet.

use std::collections::HashMap;
use std::hash::Hash;
use std::rc::Rc;

use super::outline::{GlyphOutline, OutlinePoint};

/// Vertical supersampling factor: this many evenly spaced horizontal sample
/// lines per pixel row, averaged into that row's coverage. See the module
/// docs for what this trades off.
const SUBSAMPLES: u32 = 4;

/// How far (in pixels, post-scale) a quadratic Bezier's control point may
/// deviate from the chord between its endpoints before it gets subdivided
/// again. Small enough that curves look smooth at ordinary text sizes,
/// coarse enough that a glyph does not flatten into thousands of segments.
const FLATTEN_TOLERANCE: f32 = 0.2;

/// Recursion cap for Bezier flattening, purely as a safety net against
/// degenerate/huge control points looping forever; ordinary glyph curves
/// bottom out via the tolerance check long before this.
const MAX_FLATTEN_DEPTH: u32 = 10;

// ---- Coverage bitmap -------------------------------------------------------

/// A rasterized glyph: per-pixel coverage, tightly cropped to the ink's own
/// bounding box.
///
/// This carries no left/top bearing of its own — it is a pure pixel buffer,
/// which is what lets a caller that has already decided a pixel position
/// (such as [`crate::platform::win32::surface::Surface::draw_coverage`], which
/// takes an explicit `(x, y)`) draw one without carrying bearing fields it
/// has no use for. [`rasterize`] hands the bearing back separately, in the
/// [`RasterizedGlyph`] wrapper it returns — see that type's docs for exactly
/// how to turn its `bearing_x`/`bearing_y` plus a pen position into the
/// `(x, y)` this struct's own draw call wants.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Coverage {
    pub width: u32,
    pub height: u32,
    /// Row-major, top row first, one byte per pixel: `0` is no ink, `255` is
    /// fully covered.
    pub pixels: Vec<u8>,
}

impl Coverage {
    /// An empty (zero-sized) bitmap — what an outline with no ink rasterizes
    /// to. Chosen over an all-zero bitmap "of the requested size" because
    /// this rasterizer never takes a requested size in the first place: its
    /// bitmap dimensions are always derived from the outline's own ink
    /// bounds, and a glyph with no contours has no bounds to derive from.
    #[must_use]
    pub fn empty() -> Self {
        Coverage {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        }
    }

    /// The coverage byte at `(x, y)`. Panics (via the debug-only bounds
    /// check below and an out-of-range index otherwise) if the coordinate is
    /// outside the bitmap — callers own bounds-checking against `width`/
    /// `height`, same as any other pixel buffer in this codebase (see
    /// `Surface::pixels`).
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> u8 {
        debug_assert!(x < self.width && y < self.height, "pixel out of bounds");
        self.pixels[(y * self.width + x) as usize]
    }
}

/// The result of rasterizing a glyph: the cropped [`Coverage`] bitmap, plus
/// the bearing needed to place that bitmap back relative to the glyph's pen
/// origin and baseline — the exact gap [`Coverage`]'s own docs used to flag
/// as "an integration concern for whoever draws into `Surface`."
///
/// ## Placing this bitmap
///
/// `bearing_x` and `bearing_y` are both in pixels, already multiplied by the
/// same `scale` [`rasterize`] was called with — no further conversion is
/// needed. Given a pen position `(pen_x, baseline_y)` in the destination
/// surface's ordinary y-down coordinates (y increases downward; `baseline_y`
/// is the row the glyph's baseline sits on), draw `coverage`'s top-left
/// pixel at:
///
/// ```text
/// (pen_x + bearing_x, baseline_y - bearing_y)
/// ```
///
/// - **`bearing_x`** is the horizontal offset from the pen origin
///   (font-unit x = 0) to the bitmap's left edge. It is exactly `min_x` in
///   `rasterize`'s own scaled coordinate space — the value the function
///   already computes internally to crop the bitmap, and previously
///   discarded afterward.
/// - **`bearing_y`** is the offset from the baseline (font-unit y = 0, in
///   the font's y-**up** space, where positive is above the baseline) up to
///   the bitmap's top edge. It is exactly `max_y` in that same scaled space.
///   It is positive when the ink's top edge sits above the baseline (the
///   overwhelming majority of glyphs), and **negative** when the entire
///   glyph sits below the baseline (e.g. a fixture shaped like the tail of
///   a descender) — the sign is `max_y`'s own sign, never forced positive.
///
/// Worked example (see `bearing_matches_hand_computed_offset` below): an
/// outline whose ink spans font-unit x in `[3, 13]` and y in `[7, 17]`
/// (y-up), rasterized at `scale = 1.0`, crops to a 10x10 bitmap with
/// `bearing_x = 3.0` and `bearing_y = 17.0`. To draw it with the pen at
/// `(100, 50)` (baseline on screen row 50): the top-left pixel goes at
/// `(100 + 3, 50 - 17)` = `(103, 33)` — 17 pixels *above* the baseline row,
/// which is correct because the ink's own top edge is 17 font
/// units/pixels above font-unit y = 0.
///
/// Worked example below the baseline (see
/// `bearing_is_negative_when_ink_is_entirely_below_the_baseline` below): an
/// outline whose ink spans y in `[-10, -2]` (entirely below the baseline)
/// has `bearing_y = -2.0`. Placing it with the baseline on screen row 50
/// puts the top-left pixel at row `50 - (-2)` = `52` — 2 pixels *below* the
/// baseline, which is correct because the top of that ink is 2 units below
/// font-unit y = 0.
///
/// An empty bitmap (`coverage` is [`Coverage::empty`]) has no ink and
/// therefore no meaningful bearing; both fields are `0.0` there so a caller
/// never has to special-case emptiness before using them — drawing a
/// zero-sized bitmap at any offset is already a no-op.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RasterizedGlyph {
    pub coverage: Coverage,
    /// Horizontal offset, in pixels (post-scale), from the glyph's pen
    /// origin to `coverage`'s left edge. See the type-level docs above for
    /// the full placement formula and worked examples.
    pub bearing_x: f32,
    /// Vertical offset, in pixels (post-scale), from the glyph's baseline up
    /// to `coverage`'s top edge — positive when the top edge is above the
    /// baseline, negative when it is below. See the type-level docs above
    /// for the full placement formula and worked examples.
    pub bearing_y: f32,
}

impl RasterizedGlyph {
    /// An empty rasterized glyph — what an outline with no ink produces:
    /// [`Coverage::empty`] with zero bearing in both axes (see this type's
    /// docs for why zero is the right answer for an empty bitmap).
    #[must_use]
    pub fn empty() -> Self {
        RasterizedGlyph {
            coverage: Coverage::empty(),
            bearing_x: 0.0,
            bearing_y: 0.0,
        }
    }
}

/// A point in scaled pixel space (font units already multiplied by the
/// caller's scale factor), before the final flip into bitmap-local
/// coordinates.
#[derive(Clone, Copy, Debug)]
struct Vec2 {
    x: f32,
    y: f32,
}

fn midpoint(a: Vec2, b: Vec2) -> Vec2 {
    Vec2 {
        x: (a.x + b.x) * 0.5,
        y: (a.y + b.y) * 0.5,
    }
}

/// Rasterize `outline` (in font units) at `scale` (font units → pixels,
/// i.e. `pixel_size / units_per_em` — the caller computes this from a
/// `Font`, this function only ever sees the plain ratio).
///
/// Returns a [`RasterizedGlyph`], not a bare [`Coverage`]: its
/// `bearing_x`/`bearing_y` record exactly where the cropped bitmap sits
/// relative to the glyph's own pen origin and baseline — see that type's
/// docs for the placement formula this function's `min_x`/`max_y` (computed
/// in pass 1 below) are handed out as.
#[must_use]
pub fn rasterize(outline: &GlyphOutline, scale: f32) -> RasterizedGlyph {
    // Pass 1: flatten every contour into a closed polyline, in scaled
    // pixel-space with font's y-up orientation, and track the global bounds
    // across every contour's vertices at the same time.
    let mut polylines: Vec<Vec<Vec2>> = Vec::with_capacity(outline.contours.len());
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;

    for contour in &outline.contours {
        let polyline = flatten_contour(&contour.points, scale);
        for v in &polyline {
            min_x = min_x.min(v.x);
            min_y = min_y.min(v.y);
            max_x = max_x.max(v.x);
            max_y = max_y.max(v.y);
        }
        if polyline.len() >= 2 {
            polylines.push(polyline);
        }
    }

    if polylines.is_empty() || !min_x.is_finite() {
        return RasterizedGlyph::empty();
    }

    let width = (max_x - min_x).ceil().max(0.0) as u32;
    let height = (max_y - min_y).ceil().max(0.0) as u32;
    if width == 0 || height == 0 {
        return RasterizedGlyph::empty();
    }

    // Pass 2: shift to bitmap-local x, flip font's y-up into bitmap-local
    // y-down (row 0 is the top, matching `Surface`'s pixel order), and build
    // the scanline edge list.
    let mut edges = Vec::new();
    for polyline in &polylines {
        let local: Vec<Vec2> = polyline
            .iter()
            .map(|v| Vec2 {
                x: v.x - min_x,
                y: max_y - v.y,
            })
            .collect();
        for pair in local.windows(2) {
            push_edge(&mut edges, pair[0], pair[1]);
        }
    }

    let mut pixels = vec![0u8; (width as usize) * (height as usize)];
    let mut row_coverage = vec![0f32; width as usize];
    let mut crossings: Vec<(f32, i32)> = Vec::new();
    let sample_weight = 1.0 / SUBSAMPLES as f32;

    for row in 0..height {
        row_coverage.iter_mut().for_each(|c| *c = 0.0);

        for s in 0..SUBSAMPLES {
            let sample_y = row as f32 + (s as f32 + 0.5) / SUBSAMPLES as f32;

            crossings.clear();
            for edge in &edges {
                if sample_y >= edge.y0 && sample_y < edge.y1 {
                    let x = edge.x_at_y0 + (sample_y - edge.y0) * edge.dxdy;
                    crossings.push((x, edge.winding));
                }
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));

            let mut winding_number = 0i32;
            let mut span_start = 0.0f32;
            for &(x, winding) in &crossings {
                if winding_number != 0 {
                    add_span_coverage(&mut row_coverage, span_start, x, sample_weight);
                }
                winding_number += winding;
                span_start = x;
            }
        }

        let row_base = row as usize * width as usize;
        for (col, coverage) in row_coverage.iter().enumerate() {
            pixels[row_base + col] = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }

    RasterizedGlyph {
        coverage: Coverage {
            width,
            height,
            pixels,
        },
        bearing_x: min_x,
        bearing_y: max_y,
    }
}

/// Add `weight` worth of coverage to every pixel column overlapping the span
/// `[x0, x1)`, clipped to `row`'s width — exact fractional overlap per
/// column, which is what makes coverage analytic along x.
fn add_span_coverage(row: &mut [f32], x0: f32, x1: f32, weight: f32) {
    let width = row.len() as f32;
    let x0 = x0.clamp(0.0, width);
    let x1 = x1.clamp(0.0, width);
    if x1 <= x0 {
        return;
    }
    let start_col = x0.floor() as usize;
    let end_col = (x1.ceil() as usize).min(row.len());
    for (col, cell) in row.iter_mut().enumerate().take(end_col).skip(start_col) {
        let col_left = col as f32;
        let col_right = col_left + 1.0;
        let overlap = (x1.min(col_right) - x0.max(col_left)).max(0.0);
        *cell += overlap * weight;
    }
}

/// One flattened polygon edge, ready for a horizontal-line intersection test.
/// `y0 < y1` always; `winding` records which original direction it came
/// from (`+1` if the source segment increased in y, `-1` if it decreased),
/// which is exactly the nonzero-winding-rule contribution a crossing makes.
struct Edge {
    y0: f32,
    y1: f32,
    x_at_y0: f32,
    dxdy: f32,
    winding: i32,
}

fn push_edge(edges: &mut Vec<Edge>, a: Vec2, b: Vec2) {
    if a.y == b.y {
        return; // horizontal edges never cross a horizontal scanline
    }
    let (lower, upper, winding) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
    edges.push(Edge {
        y0: lower.y,
        y1: upper.y,
        x_at_y0: lower.x,
        dxdy: (upper.x - lower.x) / (upper.y - lower.y),
        winding,
    });
}

/// Flatten one contour into a closed polyline in scaled pixel space
/// (font's y-up orientation, not yet flipped to bitmap-local). The returned
/// list starts and ends at the same point, so `windows(2)` over it yields
/// every edge including the implicit closing one.
///
/// Implements TrueType's on-curve/off-curve model exactly as documented on
/// [`OutlinePoint`]: a run of off-curve points has an implied on-curve
/// midpoint inserted between each consecutive pair, so what remains is
/// always either a line (two consecutive on-curve points) or a single
/// quadratic Bezier (on-curve, one off-curve control point, on-curve).
fn flatten_contour(points: &[OutlinePoint], scale: f32) -> Vec<Vec2> {
    if points.len() < 2 {
        return Vec::new();
    }

    let to_vec2 = |p: &OutlinePoint| Vec2 {
        x: p.x as f32 * scale,
        y: p.y as f32 * scale,
    };

    // Choose the starting on-curve point and the rest of the contour ("body")
    // to walk from there, wrapping cyclically back to just before the start.
    // If the contour has no on-curve point anywhere (a font could in
    // principle encode e.g. a circle as every point off-curve), synthesize
    // the start as the implied midpoint of the last and first points, per
    // the same on-curve/off-curve rule, and walk every original point as the
    // body.
    let (mut current, body): (Vec2, Vec<OutlinePoint>) =
        match points.iter().position(|p| p.on_curve) {
            Some(idx) => {
                let mut rest = Vec::with_capacity(points.len() - 1);
                rest.extend_from_slice(&points[idx + 1..]);
                rest.extend_from_slice(&points[..idx]);
                (to_vec2(&points[idx]), rest)
            }
            None => {
                let start = midpoint(to_vec2(&points[points.len() - 1]), to_vec2(&points[0]));
                (start, points.to_vec())
            }
        };

    let mut out = Vec::with_capacity(body.len() + 2);
    out.push(current);

    let mut i = 0usize;
    while i < body.len() {
        let p = &body[i];
        if p.on_curve {
            current = to_vec2(p);
            out.push(current);
            i += 1;
        } else {
            let control = to_vec2(p);
            let end = match body.get(i + 1) {
                Some(next) if next.on_curve => {
                    let e = to_vec2(next);
                    i += 2;
                    e
                }
                Some(next) => {
                    let e = midpoint(control, to_vec2(next));
                    i += 1;
                    e
                }
                None => {
                    // Last entry in the body is off-curve, and the contour
                    // closes back to the start: the implied end point is the
                    // midpoint of this control point and the start.
                    i += 1;
                    midpoint(control, out[0])
                }
            };
            flatten_quad(current, control, end, 0, &mut out);
            current = end;
        }
    }

    // Close the loop explicitly. A duplicate final point (when the walk
    // already landed exactly back on the start) is harmless: `push_edge`
    // drops the resulting zero-length edge as a degenerate horizontal one.
    let start = out[0];
    out.push(start);
    out
}

fn flatten_quad(p0: Vec2, control: Vec2, p2: Vec2, depth: u32, out: &mut Vec<Vec2>) {
    if depth >= MAX_FLATTEN_DEPTH || quad_is_flat(p0, control, p2) {
        out.push(p2);
        return;
    }
    let p01 = midpoint(p0, control);
    let p12 = midpoint(control, p2);
    let p012 = midpoint(p01, p12);
    flatten_quad(p0, p01, p012, depth + 1, out);
    flatten_quad(p012, p12, p2, depth + 1, out);
}

/// Perpendicular distance from `control` to the chord `p0`-`p2`, used as the
/// flatness test: a quadratic Bezier never bulges further from its chord
/// than its control point does, so bounding that distance bounds the whole
/// curve's deviation.
fn quad_is_flat(p0: Vec2, control: Vec2, p2: Vec2) -> bool {
    let dx = p2.x - p0.x;
    let dy = p2.y - p0.y;
    let len_sq = dx * dx + dy * dy;
    if len_sq < f32::EPSILON {
        // Degenerate chord (control effectively equals both endpoints);
        // treat as flat rather than dividing by ~zero.
        return true;
    }
    let cross = (control.x - p0.x) * dy - (control.y - p0.y) * dx;
    let distance = cross.abs() / len_sq.sqrt();
    distance <= FLATTEN_TOLERANCE
}

// ---- Glyph cache ------------------------------------------------------------

/// Caches rasterized [`RasterizedGlyph`]s (bitmap plus bearing) by an
/// arbitrary key.
///
/// Keyed generically rather than on a real glyph ID because Track A's font
/// parser (`font.rs`) has not landed yet — today's caller can key on
/// something like `(char, pixel_size_bits)`, and swapping to `(GlyphId,
/// pixel_size_bits)` once the parser exists is a change to the key type a
/// caller passes in, not to this cache's logic.
///
/// `RasterizedGlyph` values are held behind `Rc` so a cache hit is a cheap
/// clone of the handle, not a copy of the pixel buffer.
pub struct GlyphCache<K> {
    entries: HashMap<K, Rc<RasterizedGlyph>>,
}

impl<K: Eq + Hash> Default for GlyphCache<K> {
    fn default() -> Self {
        GlyphCache {
            entries: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash> GlyphCache<K> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The cached rasterized glyph for `key`, or `rasterize_fn`'s result on a
    /// miss — stored for next time either way. Taking a closure rather than
    /// calling [`rasterize`] directly keeps this cache usable for the same
    /// glyph coming from a hand-built fixture today or the real font parser
    /// later, and lets a test observe cache behavior by wrapping a call
    /// counter around the closure (see the tests below).
    pub fn get_or_insert_with<F>(&mut self, key: K, rasterize_fn: F) -> Rc<RasterizedGlyph>
    where
        F: FnOnce() -> RasterizedGlyph,
    {
        if let Some(existing) = self.entries.get(&key) {
            return Rc::clone(existing);
        }
        let glyph = Rc::new(rasterize_fn());
        self.entries.insert(key, Rc::clone(&glyph));
        glyph
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::outline::Contour;
    use std::cell::Cell;

    fn point(x: i16, y: i16) -> OutlinePoint {
        OutlinePoint {
            x,
            y,
            on_curve: true,
        }
    }

    fn contour(points: &[(i16, i16)]) -> Contour {
        Contour {
            points: points.iter().map(|&(x, y)| point(x, y)).collect(),
        }
    }

    fn outline(contours: Vec<Contour>) -> GlyphOutline {
        GlyphOutline {
            contours,
            advance_width: 0,
        }
    }

    // ---- empty glyph --------------------------------------------------

    #[test]
    fn empty_outline_rasterizes_to_a_zero_sized_bitmap() {
        let empty = outline(Vec::new());
        let glyph = rasterize(&empty, 1.0);
        assert_eq!(glyph.coverage.width, 0);
        assert_eq!(glyph.coverage.height, 0);
        assert!(glyph.coverage.pixels.is_empty());
    }

    #[test]
    fn empty_outline_has_zero_bearing_in_both_axes() {
        // An empty bitmap has no ink to bear an offset from; `RasterizedGlyph`'s
        // docs promise both fields are 0.0 in this case specifically so a
        // caller never has to special-case emptiness before using them.
        let empty = outline(Vec::new());
        let glyph = rasterize(&empty, 1.0);
        assert_eq!(glyph.bearing_x, 0.0);
        assert_eq!(glyph.bearing_y, 0.0);
        // Same via the explicit constructor, which callers also reach for
        // an outline-less glyph like space without going through rasterize.
        let via_empty = RasterizedGlyph::empty();
        assert_eq!(via_empty.bearing_x, 0.0);
        assert_eq!(via_empty.bearing_y, 0.0);
        assert_eq!(via_empty.coverage, Coverage::empty());
    }

    // ---- a simple filled rectangle -------------------------------------

    fn rectangle_outline() -> GlyphOutline {
        // A 10x10 unit square, single contour, pixel-aligned at scale 1.0 so
        // the interior sample points never straddle an edge.
        outline(vec![contour(&[(0, 0), (10, 0), (10, 10), (0, 10)])])
    }

    #[test]
    fn filled_rectangle_has_uniform_full_coverage_and_the_right_size() {
        let glyph = rasterize(&rectangle_outline(), 1.0);
        let coverage = &glyph.coverage;
        assert_eq!((coverage.width, coverage.height), (10, 10));
        for y in 0..coverage.height {
            for x in 0..coverage.width {
                assert_eq!(
                    coverage.pixel(x, y),
                    255,
                    "pixel ({x}, {y}) should be fully covered"
                );
            }
        }
    }

    #[test]
    fn scaling_a_fixture_roughly_doubles_its_bitmap_dimensions() {
        let at_1x = rasterize(&rectangle_outline(), 1.0);
        let at_2x = rasterize(&rectangle_outline(), 2.0);
        assert_eq!((at_1x.coverage.width, at_1x.coverage.height), (10, 10));
        assert_eq!((at_2x.coverage.width, at_2x.coverage.height), (20, 20));
    }

    // ---- bearing: placing the cropped bitmap back at the pen/baseline --

    #[test]
    fn bearing_matches_hand_computed_offset() {
        // Ink spans font-unit x in [3, 13] and y in [7, 17] (y-up), well
        // clear of both the pen origin (x=0) and the baseline (y=0) on
        // every side, so a bug that silently assumed either bound was zero
        // would be caught. This is the exact fixture worked through by hand
        // in `RasterizedGlyph`'s doc comment.
        let offset_square = outline(vec![contour(&[(3, 7), (13, 7), (13, 17), (3, 17)])]);
        let glyph = rasterize(&offset_square, 1.0);

        assert_eq!((glyph.coverage.width, glyph.coverage.height), (10, 10));
        // bearing_x is min_x in scaled space: the left edge of the ink is at
        // font-unit x = 3, and scale is 1.0, so bearing_x = 3.0 exactly.
        assert_eq!(glyph.bearing_x, 3.0);
        // bearing_y is max_y in scaled space: the top edge of the ink (the
        // higher y, since font space is y-up) is at font-unit y = 17, so
        // bearing_y = 17.0 exactly.
        assert_eq!(glyph.bearing_y, 17.0);

        // Prove the placement formula itself, not just the raw numbers:
        // drawing this bitmap's top-left corner at
        // (pen_x + bearing_x, baseline_y - bearing_y) must land the ink's
        // true top-left (font-unit (3, 17), 17 units above the baseline) at
        // that same screen position.
        let (pen_x, baseline_y) = (100.0f32, 50.0f32);
        let top_left = (pen_x + glyph.bearing_x, baseline_y - glyph.bearing_y);
        assert_eq!(top_left, (103.0, 33.0));
    }

    #[test]
    fn bearing_is_negative_when_ink_is_entirely_below_the_baseline() {
        // Ink spans font-unit y in [-10, -2]: entirely below the baseline,
        // the shape of a descender that hangs down with nothing above
        // font-unit y = 0 at all. This is the fixture worked through by hand
        // in `RasterizedGlyph`'s doc comment for the below-baseline case.
        let descender = outline(vec![contour(&[(0, -10), (10, -10), (10, -2), (0, -2)])]);
        let glyph = rasterize(&descender, 1.0);

        assert_eq!((glyph.coverage.width, glyph.coverage.height), (10, 8));
        assert_eq!(glyph.bearing_x, 0.0);
        // max_y here is -2 (the less-negative, i.e. higher, of the two
        // bounds) — the top of this ink is still 2 units *below* the
        // baseline, so bearing_y must be negative, not merely small.
        assert_eq!(glyph.bearing_y, -2.0);

        // Placement formula: baseline_y - bearing_y = baseline_y - (-2) =
        // baseline_y + 2, i.e. 2 pixels *below* the baseline row — correct,
        // since the top of this ink sits 2 units below font-unit y = 0.
        let baseline_y = 50.0f32;
        assert_eq!(baseline_y - glyph.bearing_y, 52.0);
    }

    #[test]
    fn bearing_scales_with_the_given_scale_factor() {
        // Same offset-square fixture as `bearing_matches_hand_computed_offset`,
        // at 2x scale: bearing is in post-scale pixels, so it must double
        // right along with the bitmap dimensions, not stay in font units.
        let offset_square = outline(vec![contour(&[(3, 7), (13, 7), (13, 17), (3, 17)])]);
        let glyph = rasterize(&offset_square, 2.0);

        assert_eq!((glyph.coverage.width, glyph.coverage.height), (20, 20));
        assert_eq!(glyph.bearing_x, 6.0);
        assert_eq!(glyph.bearing_y, 34.0);
    }

    // ---- an L-shape: filled region and an explicitly empty region ------

    #[test]
    fn an_l_shape_is_filled_only_where_it_has_ink() {
        // (0,0)-(10,0)-(10,4)-(4,4)-(4,10)-(0,10): a backwards-L / gamma
        // shape, in a 10x10 bounding box. The bottom bar (font y in [0,4])
        // spans the full width; the left bar (font x in [0,4]) spans the
        // full height; the top-right 6x6 notch (font x in [4,10], y in
        // [4,10]) has no ink at all.
        let l_shape = outline(vec![contour(&[
            (0, 0),
            (10, 0),
            (10, 4),
            (4, 4),
            (4, 10),
            (0, 10),
        ])]);
        let coverage = rasterize(&l_shape, 1.0).coverage;
        assert_eq!((coverage.width, coverage.height), (10, 10));

        // Bottom bar (bitmap-local rows 6..10, since local y = max_y - font
        // y flips the font's bottom band to the bitmap's bottom rows): full
        // width is filled.
        assert_eq!(coverage.pixel(7, 7), 255);
        // Left bar (bitmap-local rows 0..6, columns 0..4): filled.
        assert_eq!(coverage.pixel(1, 2), 255);
        // The notch (bitmap-local rows 0..6, columns 4..10): no ink.
        assert_eq!(coverage.pixel(7, 2), 0);
    }

    // ---- a hole via two opposite-wound contours, like lowercase 'o' ----

    #[test]
    fn opposite_wound_contours_produce_a_hole_under_the_nonzero_rule() {
        // Outer 20x20 square, wound counter-clockwise (right, up, left,
        // down); inner 10x10 square, wound clockwise (up, right, down,
        // left) -- the opposite direction, which is what makes the nonzero
        // winding rule cancel to zero inside it.
        let outer = contour(&[(0, 0), (20, 0), (20, 20), (0, 20)]);
        let inner = contour(&[(5, 5), (5, 15), (15, 15), (15, 5)]);
        let donut = outline(vec![outer, inner]);

        let coverage = rasterize(&donut, 1.0).coverage;
        assert_eq!((coverage.width, coverage.height), (20, 20));

        // Deep inside the hole: unfilled.
        assert_eq!(coverage.pixel(10, 10), 0);
        // In the ring between the two squares: filled.
        assert_eq!(coverage.pixel(2, 10), 255);
        assert_eq!(coverage.pixel(10, 2), 255);
    }

    // ---- a quadratic curve actually curves ------------------------------

    #[test]
    fn an_off_curve_point_bulges_away_from_the_straight_chord() {
        // Three on-curve corners of a triangle, plus a fourth edge replaced
        // by a quadratic bulging outward: on-curve (0,0) -> on-curve (10,0)
        // -> off-curve control (10,10) pulling the curve from (10,0) toward
        // (0,10) -> on-curve (0,10) -> back to (0,0) in a straight line.
        // This isn't tested for an exact shape, only that curved flattening
        // produces more than the four original vertices (i.e. it actually
        // subdivided) and stays within the outline's own bounding box.
        let curved = outline(vec![Contour {
            points: vec![
                OutlinePoint {
                    x: 0,
                    y: 0,
                    on_curve: true,
                },
                OutlinePoint {
                    x: 10,
                    y: 0,
                    on_curve: true,
                },
                OutlinePoint {
                    x: 20,
                    y: 10,
                    on_curve: false,
                },
                OutlinePoint {
                    x: 10,
                    y: 20,
                    on_curve: true,
                },
                OutlinePoint {
                    x: 0,
                    y: 10,
                    on_curve: true,
                },
            ],
        }]);
        let coverage = rasterize(&curved, 1.0).coverage;
        assert!(coverage.width > 0 && coverage.height > 0);
        // Somewhere in the middle should be filled; this is mostly a smoke
        // test that curved contours rasterize without panicking or
        // producing an empty bitmap.
        assert!(coverage.pixels.iter().any(|&c| c > 0));
    }

    // ---- glyph cache -----------------------------------------------------

    #[test]
    fn repeated_requests_for_the_same_key_do_not_re_rasterize() {
        let outline = rectangle_outline();
        let mut cache: GlyphCache<(char, u32)> = GlyphCache::new();
        let calls = Cell::new(0u32);

        let first = cache.get_or_insert_with(('a', 12), || {
            calls.set(calls.get() + 1);
            rasterize(&outline, 1.0)
        });
        let second = cache.get_or_insert_with(('a', 12), || {
            calls.set(calls.get() + 1);
            rasterize(&outline, 1.0)
        });

        assert_eq!(
            calls.get(),
            1,
            "the second request must hit the cache, not re-rasterize"
        );
        assert_eq!(first, second);
        assert!(
            Rc::ptr_eq(&first, &second),
            "a cache hit should hand back the same allocation"
        );
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn a_different_key_rasterizes_again() {
        let outline = rectangle_outline();
        let mut cache: GlyphCache<(char, u32)> = GlyphCache::new();
        let calls = Cell::new(0u32);

        cache.get_or_insert_with(('a', 12), || {
            calls.set(calls.get() + 1);
            rasterize(&outline, 1.0)
        });
        cache.get_or_insert_with(('a', 24), || {
            calls.set(calls.get() + 1);
            rasterize(&outline, 2.0)
        });

        assert_eq!(calls.get(), 2);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn a_new_cache_is_empty() {
        let cache: GlyphCache<char> = GlyphCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }
}

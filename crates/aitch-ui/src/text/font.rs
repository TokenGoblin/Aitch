//! Hand-written sfnt/TrueType parser. Phase 1 Track A, per
//! [`PLAN-ZERO-DEP.md`](../../../../../PLAN-ZERO-DEP.md) §2's `cosmic-text`
//! row and §4 Phase 1.
//!
//! Reads exactly what the rest of the text stack needs out of a `glyf`-based
//! TrueType font (not CFF/OpenType — sfnt tag `00 01 00 00`, which is what
//! the bundled `assets/fonts/DejaVuSansMono.ttf` is): `head` for the em
//! square, `maxp` for the glyph count, `cmap` to go from a `char` to a glyph
//! ID, `loca`+`glyf` for outlines, and `hmtx` for advance widths. Every
//! multi-byte field in an sfnt file is big-endian, unlike most FFI ABIs this
//! codebase otherwise deals with.
//!
//! Known gaps, both acceptable for Phase 1 and both documented rather than
//! silently wrong:
//! - **Composite glyphs** (a `glyf` entry built from other glyphs, signalled
//!   by a negative `numberOfContours`) are not composed. DejaVu Sans Mono's
//!   ASCII range is simple glyphs throughout, which is all Phase 1 needs;
//!   [`Font::outline_for_glyph_id`] returns `None` for one rather than
//!   guessing at it.
//! - **`cmap` format 4** (segment mapping to delta values) is the only
//!   subtable format understood. It is the format every practical TrueType
//!   font ships for Basic Multilingual Plane coverage, DejaVu included, and
//!   a codepoint outside the BMP (above `U+FFFF`) can't be represented by it
//!   at all — [`Font::glyph_id_for_char`] returns `None` rather than
//!   panicking, exactly as it does for a BMP codepoint with no mapping.

use super::outline::{Contour, GlyphOutline, OutlinePoint};

/// A parsed font, borrowing the bytes it was built from.
///
/// Parsing happens once, in [`Font::parse`]; everything it finds — table
/// offsets, the `cmap` subtable's layout, `head`/`hhea`/`maxp`'s scalar
/// fields — is resolved up front, so [`Font::glyph_for_char`] only ever does
/// bounds-checked reads against the original slice. Callers hand in bytes
/// they keep alive for as long as the `Font` is used; the bundled font is a
/// `'static` `include_bytes!` literal, so this is a non-issue in practice and
/// avoids copying the font's ~330 KiB.
#[derive(Debug)]
pub struct Font<'a> {
    data: &'a [u8],
    units_per_em: u16,
    index_to_loc_format: i16,
    num_glyphs: u16,
    num_h_metrics: u16,
    ascender: i16,
    descender: i16,
    hmtx_offset: usize,
    loca_offset: usize,
    glyf_offset: usize,
    // The `cmap` format 4 subtable's fixed-size arrays, as absolute byte
    // offsets into `data` -- computed once here so a lookup is pure
    // arithmetic plus a handful of bounds-checked reads.
    cmap_seg_count: usize,
    cmap_end_codes: usize,
    cmap_start_codes: usize,
    cmap_id_deltas: usize,
    cmap_id_range_offsets: usize,
}

/// Why [`Font::parse`] refused a byte slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontError {
    /// Shorter than an sfnt table directory could possibly be.
    TooShort,
    /// The sfnt version tag was not one of the TrueType (`glyf`-outline)
    /// values this parser understands -- most likely `OTTO`, a CFF-flavored
    /// OpenType font, which has no `glyf`/`loca` to read.
    NotTrueType(u32),
    /// A table this parser depends on is not in the font's table directory.
    MissingTable(&'static str),
    /// A table was found but its contents don't fit its own declared size,
    /// or a field this parser relies on is out of the range it allows.
    Malformed(&'static str),
}

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FontError::TooShort => {
                write!(f, "font data is too short to hold an sfnt table directory")
            }
            FontError::NotTrueType(tag) => write!(
                f,
                "not a glyf-based TrueType font (sfnt version {tag:#010x}, expected 0x00010000)"
            ),
            FontError::MissingTable(name) => {
                write!(f, "font is missing the required `{name}` table")
            }
            FontError::Malformed(what) => write!(f, "font's `{what}` is malformed"),
        }
    }
}

impl std::error::Error for FontError {}

impl<'a> Font<'a> {
    /// Parse an sfnt/TrueType font's bytes.
    ///
    /// Everything the rest of this type needs is resolved here: the table
    /// directory is walked once for the seven tables this parser reads, and
    /// `head`/`hhea`/`maxp`'s scalar fields and the `cmap` format 4
    /// subtable's array layout are read up front so later calls are pure
    /// lookups.
    pub fn parse(data: &'a [u8]) -> Result<Font<'a>, FontError> {
        if data.len() < 12 {
            return Err(FontError::TooShort);
        }
        let sfnt_version = u32_at(data, 0).ok_or(FontError::TooShort)?;
        // 0x00010000 is the ordinary TrueType tag; 0x74727565 ('true') is an
        // old Mac spelling of the same glyf-outline format. 'OTTO' (CFF
        // outlines) is deliberately not accepted: there is no `glyf`/`loca`
        // in one for this parser to read.
        if sfnt_version != 0x0001_0000 && sfnt_version != 0x7472_7565 {
            return Err(FontError::NotTrueType(sfnt_version));
        }
        let num_tables = u16_at(data, 4).ok_or(FontError::TooShort)?;

        let head = find_table(data, num_tables, b"head").ok_or(FontError::MissingTable("head"))?;
        let maxp = find_table(data, num_tables, b"maxp").ok_or(FontError::MissingTable("maxp"))?;
        let hhea = find_table(data, num_tables, b"hhea").ok_or(FontError::MissingTable("hhea"))?;
        let hmtx = find_table(data, num_tables, b"hmtx").ok_or(FontError::MissingTable("hmtx"))?;
        let loca = find_table(data, num_tables, b"loca").ok_or(FontError::MissingTable("loca"))?;
        let glyf = find_table(data, num_tables, b"glyf").ok_or(FontError::MissingTable("glyf"))?;
        let cmap = find_table(data, num_tables, b"cmap").ok_or(FontError::MissingTable("cmap"))?;

        let head_data = table_slice(data, head)?;
        // unitsPerEm: offset 18. indexToLocFormat: offset 50. Getting the
        // latter wrong silently misreads every glyph after the first, so it
        // is validated here rather than trusted.
        let units_per_em = u16_at(head_data, 18).ok_or(FontError::Malformed("head"))?;
        let index_to_loc_format = i16_at(head_data, 50).ok_or(FontError::Malformed("head"))?;
        if index_to_loc_format != 0 && index_to_loc_format != 1 {
            return Err(FontError::Malformed("head.indexToLocFormat"));
        }

        let maxp_data = table_slice(data, maxp)?;
        let num_glyphs = u16_at(maxp_data, 4).ok_or(FontError::Malformed("maxp"))?;

        let hhea_data = table_slice(data, hhea)?;
        let num_h_metrics = u16_at(hhea_data, 34).ok_or(FontError::Malformed("hhea"))?;
        if num_h_metrics == 0 {
            return Err(FontError::Malformed("hhea.numberOfHMetrics"));
        }
        // Ascender/descender: offsets 4 and 6, right after hhea's own version
        // field. Descender is negative by convention (it points below the
        // baseline); a real line height is `ascender - descender`, not a
        // guessed multiple of the font size.
        let ascender = i16_at(hhea_data, 4).ok_or(FontError::Malformed("hhea"))?;
        let descender = i16_at(hhea_data, 6).ok_or(FontError::Malformed("hhea"))?;

        let cmap_data = table_slice(data, cmap)?;
        let subtable_offset = find_cmap_format4_subtable(cmap_data)
            .ok_or(FontError::MissingTable("cmap format 4 subtable"))?;
        let seg_count_x2 =
            u16_at(cmap_data, subtable_offset + 6).ok_or(FontError::Malformed("cmap"))?;
        let seg_count = (seg_count_x2 / 2) as usize;
        // Format 4's layout, relative to the subtable's own start: a 14-byte
        // header, then four parallel arrays of `seg_count` u16s each --
        // endCode, a reserved pad word, startCode, idDelta, idRangeOffset.
        let cmap_base = table_offset(cmap) + subtable_offset;
        let end_codes = cmap_base + 14;
        let start_codes = end_codes + seg_count * 2 + 2;
        let id_deltas = start_codes + seg_count * 2;
        let id_range_offsets = id_deltas + seg_count * 2;

        Ok(Font {
            data,
            units_per_em,
            index_to_loc_format,
            num_glyphs,
            num_h_metrics,
            ascender,
            descender,
            hmtx_offset: table_offset(hmtx),
            loca_offset: table_offset(loca),
            glyf_offset: table_offset(glyf),
            cmap_seg_count: seg_count,
            cmap_end_codes: end_codes,
            cmap_start_codes: start_codes,
            cmap_id_deltas: id_deltas,
            cmap_id_range_offsets: id_range_offsets,
        })
    }

    /// The size of the em square every glyph coordinate (and every advance
    /// width) is relative to. A rasterizer scales font units to pixels by
    /// `pixels_per_em / units_per_em`.
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// How many glyphs this font has. Glyph IDs from
    /// [`Font::glyph_id_for_char`] are always below this.
    pub fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    /// The typographic ascender, in font units above the baseline
    /// (positive). From `hhea`, not a guess.
    pub fn ascender(&self) -> i16 {
        self.ascender
    }

    /// The typographic descender, in font units below the baseline
    /// (negative, by the `hhea` table's own convention). A real line height
    /// is `ascender - descender`, not a multiple of the font size.
    pub fn descender(&self) -> i16 {
        self.descender
    }

    /// Map a Unicode codepoint to a glyph ID via `cmap` format 4.
    ///
    /// `None` covers three distinct cases the caller doesn't need to tell
    /// apart: the codepoint is above the Basic Multilingual Plane (format 4
    /// cannot represent it at all), it falls in a gap between the format's
    /// segments, or it resolves to glyph 0 (`.notdef`) -- which format 4's
    /// own terminator segment does deliberately for anything unmapped.
    pub fn glyph_id_for_char(&self, c: char) -> Option<u16> {
        let code = u16::try_from(c as u32).ok()?;

        for i in 0..self.cmap_seg_count {
            let end_code = u16_at(self.data, self.cmap_end_codes + i * 2)?;
            if code > end_code {
                continue;
            }
            let start_code = u16_at(self.data, self.cmap_start_codes + i * 2)?;
            if code < start_code {
                // Between this segment and the previous one: unmapped.
                return None;
            }
            let id_delta = i16_at(self.data, self.cmap_id_deltas + i * 2)?;
            let id_range_offset = u16_at(self.data, self.cmap_id_range_offsets + i * 2)?;

            let glyph_id = if id_range_offset == 0 {
                (code as i32 + id_delta as i32) as u16
            } else {
                // Per the sfnt spec, the byte offset is measured from the
                // idRangeOffset array *element's own address* -- not from
                // the start of the subtable or the array.
                let element_address = self.cmap_id_range_offsets + i * 2;
                let glyph_index_address =
                    element_address + id_range_offset as usize + 2 * (code - start_code) as usize;
                let raw = u16_at(self.data, glyph_index_address)?;
                if raw == 0 {
                    0
                } else {
                    (raw as i32 + id_delta as i32) as u16
                }
            };

            return if glyph_id == 0 { None } else { Some(glyph_id) };
        }

        None
    }

    /// Look up a glyph's outline and advance width by ID.
    ///
    /// `loca`'s two bracketing offsets for `glyph_id` bound its data in
    /// `glyf`; equal offsets mean an empty glyph (space, most often), which
    /// is a valid zero-contour outline rather than an error. A negative
    /// `numberOfContours` marks a composite glyph, which this parser does
    /// not compose (see the module docs) -- `None` there, not a guess.
    pub fn outline_for_glyph_id(&self, glyph_id: u16) -> Option<GlyphOutline> {
        if glyph_id >= self.num_glyphs {
            return None;
        }
        let advance_width = self.advance_width(glyph_id);
        let start = self.loca_entry(glyph_id)?;
        let end = self.loca_entry(glyph_id + 1)?;
        if end < start {
            return None;
        }
        if start == end {
            return Some(GlyphOutline {
                contours: Vec::new(),
                advance_width,
            });
        }

        let glyph_data = self
            .data
            .get(self.glyf_offset + start as usize..self.glyf_offset + end as usize)?;
        let num_contours = i16_at(glyph_data, 0)?;
        if num_contours < 0 {
            return None; // Composite glyph: not composed in Phase 1.
        }
        let contours = parse_simple_glyph(glyph_data, num_contours as usize)?;
        Some(GlyphOutline {
            contours,
            advance_width,
        })
    }

    /// [`Font::glyph_id_for_char`] followed by [`Font::outline_for_glyph_id`]
    /// -- the one call the rasterizer actually wants.
    pub fn glyph_for_char(&self, c: char) -> Option<GlyphOutline> {
        self.outline_for_glyph_id(self.glyph_id_for_char(c)?)
    }

    /// One `loca` entry, as a byte offset into `glyf`.
    ///
    /// Format 0 (`index_to_loc_format == 0`) stores each entry as a `u16`
    /// half the real offset, so it must be doubled; format 1 stores the real
    /// `u32` offset directly. Getting this branch backwards would silently
    /// misread every glyph after the first, which is why both formats have a
    /// dedicated test against a hand-built fixture -- the one real font
    /// bundled today happens to use format 1.
    fn loca_entry(&self, index: u16) -> Option<u32> {
        let index = index as usize;
        match self.index_to_loc_format {
            0 => {
                let raw = u16_at(self.data, self.loca_offset + index * 2)?;
                Some(raw as u32 * 2)
            }
            1 => u32_at(self.data, self.loca_offset + index * 4),
            // Rejected in `parse`; no other value reaches here.
            _ => None,
        }
    }

    /// A glyph's horizontal advance, in font units, from `hmtx`.
    ///
    /// `hmtx` stores one `(advanceWidth, lsb)` pair per glyph up to
    /// `numberOfHMetrics`, then only `lsb` for the rest -- the last stored
    /// advance width repeats for every glyph after it, which is the space
    /// saving the table format is for.
    fn advance_width(&self, glyph_id: u16) -> u16 {
        let index = if glyph_id < self.num_h_metrics {
            glyph_id
        } else {
            self.num_h_metrics - 1
        };
        u16_at(self.data, self.hmtx_offset + index as usize * 4).unwrap_or(0)
    }
}

/// A big-endian `u16` at `offset`, or `None` if it doesn't fit in `data`.
fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// A big-endian `i16` at `offset`. Same bytes as [`u16_at`]; TrueType has no
/// separate signed-integer wire format.
fn i16_at(data: &[u8], offset: usize) -> Option<i16> {
    u16_at(data, offset).map(|v| v as i16)
}

/// A big-endian `u32` at `offset`, or `None` if it doesn't fit in `data`.
fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// A table's `(offset, length)` from the sfnt table directory, both as
/// absolute byte offsets/lengths into the whole font.
type TableRecord = (u32, u32);

fn table_offset(table: TableRecord) -> usize {
    table.0 as usize
}

/// Find `tag` in the sfnt table directory (immediately after the 12-byte
/// header, one 16-byte record per table: 4-byte tag, `u32` checksum -- never
/// verified, since a mismatch there is a corrupt-file problem this parser
/// doesn't try to diagnose -- then `u32` offset and `u32` length).
fn find_table(data: &[u8], num_tables: u16, tag: &[u8; 4]) -> Option<TableRecord> {
    for i in 0..num_tables {
        let record_offset = 12 + i as usize * 16;
        let record = data.get(record_offset..record_offset + 16)?;
        if &record[0..4] == tag {
            let offset = u32::from_be_bytes(record[8..12].try_into().unwrap());
            let length = u32::from_be_bytes(record[12..16].try_into().unwrap());
            return Some((offset, length));
        }
    }
    None
}

/// Slice out a table's bytes, checked against the whole font's length.
fn table_slice(data: &[u8], table: TableRecord) -> Result<&[u8], FontError> {
    let start = table.0 as usize;
    let end = start
        .checked_add(table.1 as usize)
        .ok_or(FontError::TooShort)?;
    data.get(start..end).ok_or(FontError::TooShort)
}

/// Find a `cmap` format 4 subtable, preferring the Windows/Unicode-BMP
/// encoding (platform 3, encoding 1) over any other that happens to be
/// format 4. Returns the subtable's offset relative to the start of the
/// `cmap` table.
fn find_cmap_format4_subtable(cmap: &[u8]) -> Option<usize> {
    let num_tables = u16_at(cmap, 2)?;
    let mut fallback = None;
    for i in 0..num_tables {
        let record_offset = 4 + i as usize * 8;
        let platform_id = u16_at(cmap, record_offset)?;
        let encoding_id = u16_at(cmap, record_offset + 2)?;
        let offset = u32_at(cmap, record_offset + 4)? as usize;
        if u16_at(cmap, offset) != Some(4) {
            continue;
        }
        if platform_id == 3 && encoding_id == 1 {
            return Some(offset);
        }
        fallback.get_or_insert(offset);
    }
    fallback
}

/// Bounds-checked sequential reader over a glyph's bytes, sparing
/// [`parse_simple_glyph`] from tracking an offset by hand.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn u8(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(byte)
    }

    fn u16(&mut self) -> Option<u16> {
        let value = u16_at(self.data, self.pos)?;
        self.pos += 2;
        Some(value)
    }

    fn i16(&mut self) -> Option<i16> {
        Some(self.u16()? as i16)
    }

    fn skip(&mut self, n: usize) {
        self.pos += n;
    }
}

/// Parse a simple glyph's contours out of its `glyf` bytes.
///
/// `glyph_data` starts at `numberOfContours` (already read by the caller to
/// decide simple vs. composite, but re-read here for a self-contained
/// parse). Layout: a 10-byte header (`numberOfContours` + bounding box),
/// `numberOfContours` end-point indices, hinting instructions, a run-length
/// encoded flags array, then delta-encoded x coordinates and y coordinates
/// (x array complete before y array starts -- not interleaved).
fn parse_simple_glyph(glyph_data: &[u8], num_contours: usize) -> Option<Vec<Contour>> {
    const ON_CURVE_POINT: u8 = 0x01;
    const X_SHORT_VECTOR: u8 = 0x02;
    const Y_SHORT_VECTOR: u8 = 0x04;
    const REPEAT_FLAG: u8 = 0x08;
    const X_IS_SAME_OR_POSITIVE: u8 = 0x10;
    const Y_IS_SAME_OR_POSITIVE: u8 = 0x20;

    let mut r = Reader::new(glyph_data);
    r.skip(2); // numberOfContours, already known.
    r.skip(8); // xMin, yMin, xMax, yMax: the bounding box, unused here.

    let mut end_pts = Vec::with_capacity(num_contours);
    for _ in 0..num_contours {
        end_pts.push(r.u16()?);
    }
    let num_points = match end_pts.last() {
        Some(&last) => last as usize + 1,
        None => 0,
    };

    let instruction_length = r.u16()?;
    r.skip(instruction_length as usize);

    let mut flags = Vec::with_capacity(num_points);
    while flags.len() < num_points {
        let flag = r.u8()?;
        flags.push(flag);
        if flag & REPEAT_FLAG != 0 {
            let repeat = r.u8()?;
            for _ in 0..repeat {
                if flags.len() >= num_points {
                    break;
                }
                flags.push(flag);
            }
        }
    }

    let mut xs = Vec::with_capacity(num_points);
    let mut x = 0i32;
    for &flag in &flags {
        let dx = if flag & X_SHORT_VECTOR != 0 {
            let magnitude = r.u8()? as i32;
            if flag & X_IS_SAME_OR_POSITIVE != 0 {
                magnitude
            } else {
                -magnitude
            }
        } else if flag & X_IS_SAME_OR_POSITIVE != 0 {
            0
        } else {
            r.i16()? as i32
        };
        x += dx;
        xs.push(x);
    }

    let mut ys = Vec::with_capacity(num_points);
    let mut y = 0i32;
    for &flag in &flags {
        let dy = if flag & Y_SHORT_VECTOR != 0 {
            let magnitude = r.u8()? as i32;
            if flag & Y_IS_SAME_OR_POSITIVE != 0 {
                magnitude
            } else {
                -magnitude
            }
        } else if flag & Y_IS_SAME_OR_POSITIVE != 0 {
            0
        } else {
            r.i16()? as i32
        };
        y += dy;
        ys.push(y);
    }

    let mut contours = Vec::with_capacity(num_contours);
    let mut start = 0usize;
    for &end in &end_pts {
        let end = end as usize;
        if end < start || end >= num_points {
            return None;
        }
        let mut points = Vec::with_capacity(end - start + 1);
        for i in start..=end {
            points.push(OutlinePoint {
                x: i16::try_from(xs[i]).ok()?,
                y: i16::try_from(ys[i]).ok()?,
                on_curve: flags[i] & ON_CURVE_POINT != 0,
            });
        }
        contours.push(Contour { points });
        start = end + 1;
    }

    Some(contours)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEJAVU: &[u8] = include_bytes!("../../assets/fonts/DejaVuSansMono.ttf");

    fn dejavu() -> Font<'static> {
        Font::parse(DEJAVU).expect("the bundled font should parse")
    }

    #[test]
    fn the_bundled_font_parses_without_error() {
        dejavu();
    }

    #[test]
    fn units_per_em_matches_the_font_file() {
        // DejaVu fonts are commonly built at a 2048-unit em square -- but
        // taken from this file's own `head` table, not assumed.
        assert_eq!(dejavu().units_per_em(), 2048);
    }

    #[test]
    fn printable_ascii_all_share_one_advance_width() {
        let font = dejavu();
        let widths: Vec<(char, u16)> = (0x20u32..=0x7Eu32)
            .map(|c| {
                let ch = char::from_u32(c).unwrap();
                let glyph = font
                    .glyph_for_char(ch)
                    .unwrap_or_else(|| panic!("{ch:?} should be mapped in DejaVu Sans Mono"));
                (ch, glyph.advance_width)
            })
            .collect();
        let first = widths[0].1;
        assert_ne!(first, 0);
        for (ch, width) in &widths {
            assert_eq!(
                *width, first,
                "a monospace font should give every printable ASCII glyph the same \
                 advance width, but {ch:?} is {width} against {first}"
            );
        }
    }

    #[test]
    fn letters_digits_and_punctuation_have_at_least_one_contour() {
        let font = dejavu();
        for ch in ['A', 'a', 'Z', 'z', '0', '9', '@', '#', '.', '_'] {
            let glyph = font
                .glyph_for_char(ch)
                .unwrap_or_else(|| panic!("{ch:?} should be mapped"));
            assert!(
                !glyph.contours.is_empty(),
                "{ch:?} should have at least one contour"
            );
            assert!(
                glyph.contours.iter().any(|c| !c.points.is_empty()),
                "{ch:?}'s contour(s) should have points"
            );
        }
    }

    #[test]
    fn ascender_and_descender_are_plausible_for_the_real_font() {
        let font = dejavu();
        // Ascender above the baseline, descender below it (negative, by
        // hhea's own convention) -- both read from the file, not assumed,
        // but sanity-checked against the em square they're defined in terms
        // of so a byte-offset mistake reading garbage would be caught.
        assert!(font.ascender() > 0);
        assert!(font.descender() < 0);
        let em = i32::from(font.units_per_em());
        assert!((i32::from(font.ascender())).abs() < em * 2);
        assert!((i32::from(font.descender())).abs() < em * 2);
    }

    #[test]
    fn space_has_no_ink_but_a_real_advance() {
        let font = dejavu();
        let space = font.glyph_for_char(' ').expect("space should be mapped");
        assert!(space.contours.is_empty());
        assert_ne!(space.advance_width, 0);
    }

    #[test]
    fn an_unmapped_codepoint_returns_none_rather_than_panicking() {
        let font = dejavu();
        // U+FFFF is a noncharacter. Format 4's mandatory terminator segment
        // maps it, and everything else it covers, to glyph 0 -- treated as
        // "no mapping" exactly like a genuine gap between segments.
        assert!(font.glyph_id_for_char('\u{FFFF}').is_none());
        // Above the Basic Multilingual Plane, format 4 cannot represent the
        // codepoint at all, regardless of what the font contains.
        assert!(font.glyph_for_char('\u{10FFFF}').is_none());
    }

    #[test]
    fn garbage_bytes_are_refused_not_panicked_on() {
        assert!(Font::parse(&[]).is_err());
        assert!(Font::parse(&[0u8; 4]).is_err());
        assert!(Font::parse(&[0u8; 11]).is_err());
    }

    #[test]
    fn cff_flavored_opentype_is_rejected_by_name() {
        let mut data = vec![0u8; 12];
        data[0..4].copy_from_slice(b"OTTO");
        let err = Font::parse(&data).unwrap_err();
        assert!(matches!(err, FontError::NotTrueType(_)), "{err:?}");
    }

    #[test]
    fn a_font_missing_a_required_table_names_it() {
        // A well-formed header claiming zero tables is missing all seven.
        let mut data = vec![0u8; 12];
        data[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        let err = Font::parse(&data).unwrap_err();
        assert!(matches!(err, FontError::MissingTable(_)), "{err:?}");
    }

    /// A minimal, hand-built two-glyph font using the *short* `loca` format
    /// (`indexToLocFormat == 0`), which nothing in the bundled DejaVu font
    /// exercises -- it uses the long format, given its size. Built
    /// programmatically (offsets computed, not hand-counted) so the fixture
    /// itself can't drift into being wrong the same way the code under test
    /// might be.
    ///
    /// Glyph 0 is an empty `.notdef`; glyph 1 is a three-point triangle
    /// mapped from `'A'`. Both share one `hmtx` advance width, so this also
    /// stands in for "a font with more than one glyph" more generally.
    fn build_short_loca_test_font() -> Vec<u8> {
        let mut head = Vec::new();
        head.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // version
        head.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // fontRevision
        head.extend_from_slice(&0u32.to_be_bytes()); // checkSumAdjustment
        head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes()); // magicNumber
        head.extend_from_slice(&0u16.to_be_bytes()); // flags
        head.extend_from_slice(&1000u16.to_be_bytes()); // unitsPerEm
        head.extend_from_slice(&0i64.to_be_bytes()); // created
        head.extend_from_slice(&0i64.to_be_bytes()); // modified
        head.extend_from_slice(&0i16.to_be_bytes()); // xMin
        head.extend_from_slice(&0i16.to_be_bytes()); // yMin
        head.extend_from_slice(&500i16.to_be_bytes()); // xMax
        head.extend_from_slice(&700i16.to_be_bytes()); // yMax
        head.extend_from_slice(&0u16.to_be_bytes()); // macStyle
        head.extend_from_slice(&8u16.to_be_bytes()); // lowestRecPPEM
        head.extend_from_slice(&2i16.to_be_bytes()); // fontDirectionHint
        head.extend_from_slice(&0i16.to_be_bytes()); // indexToLocFormat: short
        head.extend_from_slice(&0i16.to_be_bytes()); // glyphDataFormat
        assert_eq!(head.len(), 54);

        // Version 0.5 (no glyph-level metrics beyond numGlyphs) -- this
        // parser only ever reads that one field, at a fixed offset shared by
        // both maxp versions.
        let mut maxp = Vec::new();
        maxp.extend_from_slice(&0x0000_5000u32.to_be_bytes());
        maxp.extend_from_slice(&2u16.to_be_bytes()); // numGlyphs
        assert_eq!(maxp.len(), 6);

        let mut hhea = Vec::new();
        hhea.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // version
        hhea.extend_from_slice(&800i16.to_be_bytes()); // ascent
        hhea.extend_from_slice(&(-200i16).to_be_bytes()); // descent
        hhea.extend_from_slice(&0i16.to_be_bytes()); // lineGap
        hhea.extend_from_slice(&600u16.to_be_bytes()); // advanceWidthMax
        hhea.extend_from_slice(&0i16.to_be_bytes()); // minLeftSideBearing
        hhea.extend_from_slice(&0i16.to_be_bytes()); // minRightSideBearing
        hhea.extend_from_slice(&700i16.to_be_bytes()); // xMaxExtent
        hhea.extend_from_slice(&1i16.to_be_bytes()); // caretSlopeRise
        hhea.extend_from_slice(&0i16.to_be_bytes()); // caretSlopeRun
        hhea.extend_from_slice(&0i16.to_be_bytes()); // caretOffset
        hhea.extend_from_slice(&[0u8; 8]); // reserved x4
        hhea.extend_from_slice(&0i16.to_be_bytes()); // metricDataFormat
        hhea.extend_from_slice(&2u16.to_be_bytes()); // numberOfHMetrics
        assert_eq!(hhea.len(), 36);

        // Both glyphs share one advance width, same as a monospace font.
        let mut hmtx = Vec::new();
        hmtx.extend_from_slice(&600u16.to_be_bytes());
        hmtx.extend_from_slice(&0i16.to_be_bytes());
        hmtx.extend_from_slice(&600u16.to_be_bytes());
        hmtx.extend_from_slice(&50i16.to_be_bytes());
        assert_eq!(hmtx.len(), 8);

        // cmap format 4: 'A' (0x0041) maps to glyph 1; every other BMP
        // codepoint falls through the mandatory terminator segment to glyph
        // 0, i.e. "unmapped".
        let seg_count: u16 = 2;
        let mut sub = Vec::new();
        sub.extend_from_slice(&4u16.to_be_bytes()); // format
        let length_pos = sub.len();
        sub.extend_from_slice(&0u16.to_be_bytes()); // length, patched below
        sub.extend_from_slice(&0u16.to_be_bytes()); // language
        sub.extend_from_slice(&(seg_count * 2).to_be_bytes()); // segCountX2
        sub.extend_from_slice(&4u16.to_be_bytes()); // searchRange (unused)
        sub.extend_from_slice(&1u16.to_be_bytes()); // entrySelector (unused)
        sub.extend_from_slice(&0u16.to_be_bytes()); // rangeShift (unused)
        sub.extend_from_slice(&0x0041u16.to_be_bytes()); // endCode[0]
        sub.extend_from_slice(&0xFFFFu16.to_be_bytes()); // endCode[1]
        sub.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
        sub.extend_from_slice(&0x0041u16.to_be_bytes()); // startCode[0]
        sub.extend_from_slice(&0xFFFFu16.to_be_bytes()); // startCode[1]
        sub.extend_from_slice(&(1i16.wrapping_sub(0x0041)).to_be_bytes()); // idDelta[0]: 0x41 + delta = 1
        sub.extend_from_slice(&1i16.to_be_bytes()); // idDelta[1]: 0xFFFF + 1 = 0
        sub.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset[0]
        sub.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset[1]
        let sub_len = sub.len() as u16;
        sub[length_pos..length_pos + 2].copy_from_slice(&sub_len.to_be_bytes());

        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes()); // version
        cmap.extend_from_slice(&1u16.to_be_bytes()); // numTables
        cmap.extend_from_slice(&3u16.to_be_bytes()); // platformID: Windows
        cmap.extend_from_slice(&1u16.to_be_bytes()); // encodingID: Unicode BMP
        let subtable_offset = cmap.len() as u32 + 4; // + the offset field itself
        cmap.extend_from_slice(&subtable_offset.to_be_bytes());
        cmap.extend_from_slice(&sub);

        // glyf: glyph 0 is empty; glyph 1 is a triangle, all points on-curve
        // with long (non-short-vector) deltas, padded to an even length so
        // the short loca format's "value * 2" byte offsets land exactly.
        let glyph0: Vec<u8> = Vec::new();
        let mut glyph1 = Vec::new();
        glyph1.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours
        glyph1.extend_from_slice(&0i16.to_be_bytes()); // xMin
        glyph1.extend_from_slice(&0i16.to_be_bytes()); // yMin
        glyph1.extend_from_slice(&500i16.to_be_bytes()); // xMax
        glyph1.extend_from_slice(&700i16.to_be_bytes()); // yMax
        glyph1.extend_from_slice(&2u16.to_be_bytes()); // endPtsOfContours[0]
        glyph1.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
        glyph1.extend_from_slice(&[0x01, 0x01, 0x01]); // flags: on-curve, long deltas
        let deltas: [(i16, i16); 3] = [(0, 0), (500, 0), (-250, 700)];
        for &(dx, _) in &deltas {
            glyph1.extend_from_slice(&dx.to_be_bytes());
        }
        for &(_, dy) in &deltas {
            glyph1.extend_from_slice(&dy.to_be_bytes());
        }
        if glyph1.len() % 2 != 0 {
            glyph1.push(0);
        }

        let mut glyf = Vec::new();
        glyf.extend_from_slice(&glyph0);
        let glyph1_start = glyf.len() as u32;
        glyf.extend_from_slice(&glyph1);
        let glyph1_end = glyf.len() as u32;

        let mut loca = Vec::new();
        for offset in [0u32, glyph1_start, glyph1_end] {
            assert_eq!(offset % 2, 0, "short loca format needs even byte offsets");
            loca.extend_from_slice(&((offset / 2) as u16).to_be_bytes());
        }

        let tables: [(&[u8; 4], Vec<u8>); 7] = [
            (b"cmap", cmap),
            (b"glyf", glyf),
            (b"head", head),
            (b"hhea", hhea),
            (b"hmtx", hmtx),
            (b"loca", loca),
            (b"maxp", maxp),
        ];

        let header_len = 12;
        let directory_len = tables.len() * 16;
        let mut offset = header_len + directory_len;
        let mut directory = Vec::new();
        let mut body = Vec::new();
        for (tag, data) in &tables {
            directory.extend_from_slice(*tag);
            directory.extend_from_slice(&0u32.to_be_bytes()); // checksum: unused
            directory.extend_from_slice(&(offset as u32).to_be_bytes());
            directory.extend_from_slice(&(data.len() as u32).to_be_bytes());
            body.extend_from_slice(data);
            offset += data.len();
        }

        let mut font = Vec::new();
        font.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // sfnt version
        font.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        font.extend_from_slice(&0u16.to_be_bytes()); // searchRange (unused)
        font.extend_from_slice(&0u16.to_be_bytes()); // entrySelector (unused)
        font.extend_from_slice(&0u16.to_be_bytes()); // rangeShift (unused)
        font.extend_from_slice(&directory);
        font.extend_from_slice(&body);
        font
    }

    #[test]
    fn short_loca_format_is_read_correctly() {
        let bytes = build_short_loca_test_font();
        let font = Font::parse(&bytes).expect("the fixture font should parse");
        assert_eq!(font.units_per_em(), 1000);
        assert_eq!(font.num_glyphs(), 2);
        assert_eq!(font.ascender(), 800);
        assert_eq!(font.descender(), -200);

        let notdef = font
            .outline_for_glyph_id(0)
            .expect("glyph 0 exists in this fixture");
        assert!(
            notdef.contours.is_empty(),
            "the fixture's .notdef is deliberately empty"
        );
        assert_eq!(notdef.advance_width, 600);

        let a = font
            .glyph_for_char('A')
            .expect("'A' is mapped in the fixture's cmap");
        assert_eq!(a.advance_width, 600);
        assert_eq!(a.contours.len(), 1);
        assert_eq!(
            a.contours[0].points,
            vec![
                OutlinePoint {
                    x: 0,
                    y: 0,
                    on_curve: true
                },
                OutlinePoint {
                    x: 500,
                    y: 0,
                    on_curve: true
                },
                OutlinePoint {
                    x: 250,
                    y: 700,
                    on_curve: true
                },
            ]
        );

        // The fixture's cmap only ever names 'A'; everything else falls
        // through its terminator segment to glyph 0, i.e. unmapped.
        assert!(font.glyph_for_char('B').is_none());
    }
}

//! Shaping and drawing the visible text.
//!
//! Only the lines on screen are ever laid out. The cosmic-text buffer holds
//! the visible window and nothing else, so the cost of a frame is set by the
//! window size rather than the file size — which is what lets a 50 MB log
//! scroll at the same speed as a 50 line one.

use aitch_core::{Buffer, Position};
use cosmic_text::{Attrs, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap};

use crate::render::atlas::Atlas;
use crate::render::quads::{Instances, Kind};
use crate::theme::Theme;

/// Line height as a multiple of the font size.
const LINE_HEIGHT_RATIO: f32 = 1.4;

/// Width of the cursor in logical pixels.
const CURSOR_WIDTH: f32 = 2.0;

/// What was last shaped, so an unchanged frame does not reshape.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShapeKey {
    first_line: usize,
    line_count: usize,
    width: u32,
    /// Bumped when the buffer's content could have changed under us.
    generation: u64,
}

pub struct TextRenderer {
    font_system: FontSystem,
    swash: SwashCache,
    layout: cosmic_text::Buffer,
    /// Logical font size; physical metrics are derived from it and the scale.
    font_size: f32,
    scale_factor: f32,
    cell_width: f32,
    /// The lines currently in `layout`, so byte offsets can be mapped back.
    visible: Vec<String>,
    first_line: usize,
    shaped: Option<ShapeKey>,
}

impl TextRenderer {
    pub fn new(font_size: f32, scale_factor: f32) -> TextRenderer {
        let mut font_system = FontSystem::new();
        let metrics = metrics_for(font_size, scale_factor);
        let mut layout = cosmic_text::Buffer::new(&mut font_system, metrics);
        // Long lines scroll horizontally rather than wrapping. Soft wrap is a
        // Phase 5 toggle, and reflow would break the line-per-row assumption
        // the viewport is built on.
        layout.set_wrap(&mut font_system, Wrap::None);

        let cell_width = measure_cell_width(&mut font_system, metrics);

        TextRenderer {
            font_system,
            swash: SwashCache::new(),
            layout,
            font_size,
            scale_factor,
            cell_width,
            visible: Vec::new(),
            first_line: 0,
            shaped: None,
        }
    }

    /// Physical height of one line of text.
    pub fn line_height(&self) -> f32 {
        metrics_for(self.font_size, self.scale_factor).line_height
    }

    /// Physical advance width of one monospace cell.
    pub fn cell_width(&self) -> f32 {
        self.cell_width
    }

    /// How many whole lines fit in a height of physical pixels.
    pub fn lines_for_height(&self, height: f32) -> usize {
        ((height / self.line_height()).floor() as usize).max(1)
    }

    /// React to a DPI change. Returns true if anything actually changed, in
    /// which case the caller must reset the glyph atlas: every cached glyph
    /// was rasterized for the old scale and is now the wrong size.
    pub fn set_scale_factor(&mut self, scale_factor: f32) -> bool {
        if (self.scale_factor - scale_factor).abs() < f32::EPSILON {
            return false;
        }
        self.scale_factor = scale_factor;
        let metrics = metrics_for(self.font_size, scale_factor);
        self.layout.set_metrics(&mut self.font_system, metrics);
        self.cell_width = measure_cell_width(&mut self.font_system, metrics);
        self.shaped = None;
        true
    }

    /// Shape the visible window, if it is not already shaped.
    pub fn prepare(
        &mut self,
        buffer: &Buffer,
        first_line: usize,
        size: (f32, f32),
        generation: u64,
    ) {
        // One extra line so a partly scrolled row at the bottom still draws.
        let line_count = self.lines_for_height(size.1) + 2;
        let key = ShapeKey {
            first_line,
            line_count,
            width: size.0 as u32,
            generation,
        };
        if self.shaped.as_ref() == Some(&key) {
            return;
        }

        self.visible.clear();
        let last = (first_line + line_count).min(buffer.len_lines());
        for line in first_line..last {
            self.visible.push(buffer.line_text(line).to_string());
        }
        self.first_line = first_line;

        let text = self.visible.join("\n");
        self.layout
            .set_size(&mut self.font_system, Some(size.0), Some(size.1));
        self.layout.set_text(
            &mut self.font_system,
            &text,
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        self.layout.shape_until_scroll(&mut self.font_system, false);

        self.shaped = Some(key);
    }

    /// Add the visible glyphs and the cursor to this frame's instances.
    ///
    /// `y_offset` is the sub-line scroll offset in physical pixels: negative,
    /// so a partly scrolled first row is clipped at the top of the window.
    pub fn push_instances(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut Atlas,
        instances: &mut Instances,
        buffer: &Buffer,
        y_offset: f32,
        theme: &Theme,
    ) {
        // Split the borrow so the atlas can rasterize while the layout is read.
        let TextRenderer {
            font_system,
            swash,
            layout,
            ..
        } = self;

        for run in layout.layout_runs() {
            let baseline = run.line_y + y_offset;
            for glyph in run.glyphs {
                let physical = glyph.physical((0.0, 0.0), 1.0);
                let Some(entry) = atlas.glyph(queue, font_system, swash, physical.cache_key) else {
                    continue;
                };

                let position = [
                    physical.x as f32 + entry.offset[0],
                    baseline + physical.y as f32 + entry.offset[1],
                ];
                let kind = if entry.color { Kind::Color } else { Kind::Mask };
                instances.push(position, entry, theme.foreground, kind);
            }
        }

        self.push_cursor(instances, atlas.white(), buffer, y_offset, theme);
    }

    fn push_cursor(
        &self,
        instances: &mut Instances,
        white: crate::render::atlas::Entry,
        buffer: &Buffer,
        y_offset: f32,
        theme: &Theme,
    ) {
        let cursor = buffer.cursor();
        let Some(layout_line) = cursor.line.checked_sub(self.first_line) else {
            return;
        };
        let Some(text) = self.visible.get(layout_line) else {
            return;
        };

        let byte_offset = char_to_byte(text, cursor.column);
        let Some((x, top)) = self.caret_position(layout_line, byte_offset) else {
            return;
        };

        instances.push_rect(
            [x, top + y_offset],
            [CURSOR_WIDTH * self.scale_factor, self.line_height()],
            white,
            theme.cursor,
        );
    }

    /// Where the caret sits for a byte offset within a laid-out line.
    fn caret_position(&self, layout_line: usize, byte_offset: usize) -> Option<(f32, f32)> {
        for run in self.layout.layout_runs() {
            if run.line_i != layout_line {
                continue;
            }
            for glyph in run.glyphs {
                if byte_offset >= glyph.start && byte_offset < glyph.end {
                    return Some((glyph.x, run.line_top));
                }
            }
            // At or past the end of the line.
            return Some((run.line_w, run.line_top));
        }
        None
    }

    /// Map a click in physical pixels back to a position in the buffer.
    ///
    /// `y_offset` is the same sub-line offset passed to [`Self::push_instances`].
    pub fn hit(&self, x: f32, y: f32, y_offset: f32) -> Option<Position> {
        let hit = self.layout.hit(x, y - y_offset)?;
        let text = self.visible.get(hit.line)?;
        Some(Position::new(
            self.first_line + hit.line,
            byte_to_char(text, hit.index),
        ))
    }
}

fn metrics_for(font_size: f32, scale_factor: f32) -> Metrics {
    let physical = font_size * scale_factor;
    Metrics::new(physical, (physical * LINE_HEIGHT_RATIO).round())
}

/// Shape a single `M` to find the monospace advance width.
fn measure_cell_width(font_system: &mut FontSystem, metrics: Metrics) -> f32 {
    let mut probe = cosmic_text::Buffer::new(font_system, metrics);
    probe.set_wrap(font_system, Wrap::None);
    probe.set_text(
        font_system,
        "M",
        &Attrs::new().family(Family::Monospace),
        Shaping::Advanced,
        None,
    );
    probe.shape_until_scroll(font_system, false);

    probe
        .layout_runs()
        .next()
        .and_then(|run| run.glyphs.first().map(|glyph| glyph.w))
        .unwrap_or(metrics.font_size * 0.6)
}

/// Byte offset of the `n`th char in `text`, or the end of it.
fn char_to_byte(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

/// How many chars precede a byte offset in `text`.
fn byte_to_char(text: &str, byte: usize) -> usize {
    text.char_indices().take_while(|(i, _)| *i < byte).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_and_byte_offsets_round_trip_through_ascii() {
        let text = "hello world";
        for chars in 0..=text.chars().count() {
            let byte = char_to_byte(text, chars);
            assert_eq!(byte_to_char(text, byte), chars);
        }
    }

    #[test]
    fn char_and_byte_offsets_round_trip_through_multibyte_text() {
        // 'é' is two bytes, '→' three, '𝄞' four: every UTF-8 length.
        let text = "aé→𝄞b";
        assert_eq!(char_to_byte(text, 0), 0);
        assert_eq!(char_to_byte(text, 1), 1);
        assert_eq!(char_to_byte(text, 2), 3);
        assert_eq!(char_to_byte(text, 3), 6);
        assert_eq!(char_to_byte(text, 4), 10);

        for chars in 0..=text.chars().count() {
            let byte = char_to_byte(text, chars);
            assert_eq!(byte_to_char(text, byte), chars, "at char {chars}");
        }
    }

    #[test]
    fn offsets_past_the_end_clamp_to_the_end() {
        let text = "abc";
        assert_eq!(char_to_byte(text, 99), 3);
        assert_eq!(byte_to_char(text, 99), 3);
    }

    #[test]
    fn line_height_scales_with_dpi() {
        let one = metrics_for(14.0, 1.0);
        let two = metrics_for(14.0, 2.0);
        assert_eq!(two.font_size, one.font_size * 2.0);
        assert!(two.line_height >= one.line_height * 2.0 - 1.0);
    }
}

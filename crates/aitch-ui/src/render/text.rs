//! Shaping and drawing the visible text.
//!
//! Only the lines on screen are ever laid out. The cosmic-text buffer holds
//! the visible window and nothing else, so the cost of a frame is set by the
//! window size rather than the file size — which is what lets a 50 MB log
//! scroll at the same speed as a 50 line one.

use aitch_core::{Buffer, Highlights, Position, ViewOptions};
use cosmic_text::{Attrs, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap};

use crate::render::atlas::Atlas;
use crate::render::quads::{Instances, Kind};
use crate::theme::Theme;

/// The attributes to shape with: the configured family, or whatever the
/// system calls monospace.
///
/// A family that is not installed falls through to the fallback chain rather
/// than failing, which is why an unknown name is not an error.
fn attrs(family: Option<&str>) -> Attrs<'_> {
    match family {
        Some(name) => Attrs::new().family(Family::Name(name)),
        None => Attrs::new().family(Family::Monospace),
    }
}

/// One visible line's whitespace marks: which line, where its top is, and the
/// byte offset and x position of each glyph on it.
type LineMarks = (usize, f32, Vec<(usize, f32)>);

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
    /// A scratch buffer for one-off lines — status, prompt, footer, help.
    /// Kept separate so shaping chrome never disturbs the document's layout.
    chrome: cosmic_text::Buffer,
    /// Logical font size; physical metrics are derived from it and the scale.
    font_size: f32,
    scale_factor: f32,
    cell_width: f32,
    /// The lines currently in `layout`, so byte offsets can be mapped back.
    visible: Vec<String>,
    first_line: usize,
    /// Width of the text area in physical pixels, for full-width bands.
    width: f32,
    /// The configured font family, if one was asked for.
    family: Option<String>,
    /// How wide a tab is drawn, in cells. Kept so a live config change can
    /// move it; both layout buffers are told separately.
    tab_width: u16,
    shaped: Option<ShapeKey>,
}

impl TextRenderer {
    pub fn new(font_size: f32, scale_factor: f32) -> TextRenderer {
        TextRenderer::with_family(font_size, scale_factor, None, DEFAULT_TAB_WIDTH)
    }

    /// The same, with the font family and tab width from the config.
    ///
    /// A family that is not installed falls back to whatever the system calls
    /// monospace, which is better than refusing to draw. `tab_width` is how
    /// wide a literal tab is drawn, which is a separate question from what
    /// the Tab key inserts — a file full of tabs still has to line up.
    pub fn with_family(
        font_size: f32,
        scale_factor: f32,
        family: Option<String>,
        tab_width: usize,
    ) -> TextRenderer {
        let mut font_system = FontSystem::new();
        let metrics = metrics_for(font_size, scale_factor);
        let tab_width = tab_width.clamp(1, 16) as u16;
        let mut layout = cosmic_text::Buffer::new(&mut font_system, metrics);
        layout.set_tab_width(&mut font_system, tab_width);
        // Long lines scroll horizontally rather than wrapping. Soft wrap is a
        // Phase 5 toggle, and reflow would break the line-per-row assumption
        // the viewport is built on.
        layout.set_wrap(&mut font_system, Wrap::None);

        let mut chrome = cosmic_text::Buffer::new(&mut font_system, metrics);
        chrome.set_wrap(&mut font_system, Wrap::None);
        chrome.set_tab_width(&mut font_system, tab_width);

        let cell_width = measure_cell_width(&mut font_system, metrics, family.as_deref());

        TextRenderer {
            font_system,
            swash: SwashCache::new(),
            layout,
            chrome,
            font_size,
            scale_factor,
            cell_width,
            visible: Vec::new(),
            first_line: 0,
            width: 0.0,
            family,
            tab_width,
            shaped: None,
        }
    }

    /// Change how wide a tab is drawn.
    ///
    /// The gutter and the Tab key read the width from the config directly, so
    /// without this a live change moved everything except the tabs already on
    /// screen: the status line said "settings reloaded" and a tab-indented
    /// file quietly stopped lining up.
    ///
    /// True when it actually changed, so the caller can invalidate its layout.
    pub fn set_tab_width(&mut self, tab_width: usize) -> bool {
        let tab_width = tab_width.clamp(1, 16) as u16;
        if tab_width == self.tab_width {
            return false;
        }
        self.tab_width = tab_width;
        self.layout.set_tab_width(&mut self.font_system, tab_width);
        self.chrome.set_tab_width(&mut self.font_system, tab_width);
        true
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
        self.chrome.set_metrics(&mut self.font_system, metrics);
        let family = self.family.clone();
        self.cell_width = measure_cell_width(&mut self.font_system, metrics, family.as_deref());
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

        self.width = size.0;
        self.visible.clear();
        let last = (first_line + line_count).min(buffer.len_lines());
        for line in first_line..last {
            self.visible.push(buffer.line_text(line).to_string());
        }
        self.first_line = first_line;

        let text = self.visible.join("\n");
        self.layout
            .set_size(&mut self.font_system, Some(size.0), Some(size.1));
        let family = self.family.clone();
        self.layout.set_text(
            &mut self.font_system,
            &text,
            &attrs(family.as_deref()),
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
        self.push_instances_at(queue, atlas, instances, buffer, (0.0, y_offset), theme);
    }

    /// The same, offset from the left as well — the sidebar takes a column.
    pub fn push_instances_at(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut Atlas,
        instances: &mut Instances,
        buffer: &Buffer,
        offset: (f32, f32),
        theme: &Theme,
    ) {
        self.push_document(
            queue,
            atlas,
            instances,
            buffer,
            offset,
            theme,
            None,
            aitch_core::ViewOptions::default(),
            None,
        );
    }

    /// Draw the document, colouring each run by its syntax token.
    ///
    /// `highlights` may describe a slightly older version of the text — it is
    /// produced off this thread on purpose. Byte offsets from a stale parse
    /// can land anywhere, so a token is looked up per glyph and a miss simply
    /// means the default colour rather than a wrong one.
    #[allow(clippy::too_many_arguments)]
    pub fn push_document(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut Atlas,
        instances: &mut Instances,
        buffer: &Buffer,
        offset: (f32, f32),
        theme: &Theme,
        highlights: Option<&Highlights>,
        view: ViewOptions,
        brackets: Option<(usize, usize)>,
    ) {
        let (x_offset, y_offset) = offset;
        self.push_current_line(instances, atlas.white(), buffer, offset, theme);
        self.push_selection(instances, atlas.white(), buffer, offset, theme);
        self.push_brackets(instances, atlas.white(), buffer, offset, theme, brackets);

        // Split the borrow so the atlas can rasterize while the layout is read.
        let first_line = self.first_line;
        // Whitespace markers are gathered here and drawn after, because
        // drawing them needs the shaping buffer that this loop is holding.
        let mut marks: Vec<LineMarks> = Vec::new();
        let TextRenderer {
            font_system,
            swash,
            layout,
            ..
        } = self;

        for run in layout.layout_runs() {
            let baseline = run.line_y + y_offset;
            // Where this visible line starts in the document, so a glyph's
            // offset within the line can be turned into a document offset.
            let line_start = first_line
                .checked_add(run.line_i)
                .filter(|line| *line < buffer.len_lines())
                .map(|line| buffer.text().line_to_byte(line));

            for glyph in run.glyphs {
                let physical = glyph.physical((0.0, 0.0), 1.0);
                let Some(entry) = atlas.glyph(queue, font_system, swash, physical.cache_key) else {
                    continue;
                };

                let color = match (highlights, line_start) {
                    (Some(highlights), Some(start)) => highlights
                        .token_at(start + glyph.start)
                        .map(|token| theme.color_for(token))
                        .unwrap_or(theme.foreground),
                    _ => theme.foreground,
                };

                let position = [
                    x_offset + physical.x as f32 + entry.offset[0],
                    baseline + physical.y as f32 + entry.offset[1],
                ];
                let kind = if entry.color { Kind::Color } else { Kind::Mask };
                instances.push(position, entry, color, kind);
            }

            if view.whitespace {
                marks.push((
                    run.line_i,
                    run.line_top,
                    run.glyphs.iter().map(|g| (g.start, g.x)).collect(),
                ));
            }
        }

        if view.whitespace {
            self.push_whitespace(queue, atlas, instances, &marks, offset, theme);
        }

        self.push_cursor(instances, atlas.white(), buffer, offset, theme);
    }

    /// Dim markers where tabs and trailing spaces are.
    ///
    /// Only trailing spaces: marking every space between words would make
    /// prose unreadable, and the ones that matter are the ones you cannot see.
    fn push_whitespace(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut Atlas,
        instances: &mut Instances,
        marks: &[LineMarks],
        offset: (f32, f32),
        theme: &Theme,
    ) {
        for (line_index, line_top, glyphs) in marks {
            let Some(line) = self.visible.get(*line_index).cloned() else {
                continue;
            };
            let trailing_from = line.trim_end().len();

            for (byte, x) in glyphs {
                let Some(c) = line[*byte..].chars().next() else {
                    continue;
                };
                let marker = match c {
                    '\t' => "\u{2192}",
                    ' ' if *byte >= trailing_from => "\u{00b7}",
                    _ => continue,
                };
                self.push_line(
                    queue,
                    atlas,
                    instances,
                    marker,
                    (offset.0 + x, line_top + offset.1),
                    theme.whitespace,
                );
            }
        }
    }

    /// A box behind a bracket and its partner.
    fn push_brackets(
        &self,
        instances: &mut Instances,
        white: crate::render::atlas::Entry,
        buffer: &Buffer,
        offset: (f32, f32),
        theme: &Theme,
        brackets: Option<(usize, usize)>,
    ) {
        let Some((first, second)) = brackets else {
            return;
        };
        for at in [first, second] {
            let position = buffer.char_to_position(at);
            let Some(row) = position.line.checked_sub(self.first_line) else {
                continue;
            };
            let Some(text) = self.visible.get(row) else {
                continue;
            };
            let Some(run) = self.layout.layout_runs().find(|run| run.line_i == row) else {
                continue;
            };

            let byte = char_to_byte(text, position.column);
            let x = run
                .glyphs
                .iter()
                .find(|glyph| byte >= glyph.start && byte < glyph.end)
                .map(|glyph| glyph.x)
                .unwrap_or(run.line_w);

            instances.push_rect(
                [offset.0 + x, run.line_top + offset.1],
                [self.cell_width, self.line_height()],
                white,
                theme.bracket_match,
            );
        }
    }

    /// A faint band behind the line the cursor is on.
    ///
    /// Drawn under everything else, including the selection, so that a
    /// selected current line still reads as selected.
    fn push_current_line(
        &self,
        instances: &mut Instances,
        white: crate::render::atlas::Entry,
        buffer: &Buffer,
        offset: (f32, f32),
        theme: &Theme,
    ) {
        // A selection makes the current line obvious already, and two washes
        // stacked on one line just muddies both.
        if buffer.has_selection() {
            return;
        }
        let Some(row) = buffer.cursor().line.checked_sub(self.first_line) else {
            return;
        };
        let Some(run) = self.layout.layout_runs().find(|run| run.line_i == row) else {
            return;
        };

        instances.push_rect(
            [0.0, run.line_top + offset.1],
            [self.width, self.line_height()],
            white,
            theme.current_line,
        );
    }

    /// Draw the selection behind the text, one band per visible line.
    ///
    /// cosmic-text works out the span within a line, which is the part that is
    /// awkward: a run may be bidirectional, and the selected range does not
    /// have to line up with glyph boundaries.
    fn push_selection(
        &self,
        instances: &mut Instances,
        white: crate::render::atlas::Entry,
        buffer: &Buffer,
        offset: (f32, f32),
        theme: &Theme,
    ) {
        let (x_offset, y_offset) = offset;
        let Some(range) = buffer.selection() else {
            return;
        };
        let start = buffer.char_to_position(range.start);
        let end = buffer.char_to_position(range.end);

        for run in self.layout.layout_runs() {
            let line = self.first_line + run.line_i;
            if line < start.line || line > end.line {
                continue;
            }
            let Some(text) = self.visible.get(run.line_i) else {
                continue;
            };

            let from = if line == start.line {
                char_to_byte(text, start.column)
            } else {
                0
            };
            let to = if line == end.line {
                char_to_byte(text, end.column)
            } else {
                text.len()
            };

            let span = run.highlight(
                cosmic_text::Cursor::new(run.line_i, from),
                cosmic_text::Cursor::new(run.line_i, to),
            );
            let Some((x, width)) = span else {
                continue;
            };

            // A selection that runs past the end of a line shows a sliver, so
            // that selecting a line break is visible rather than invisible.
            let width = if line < end.line {
                width.max(self.cell_width * 0.5)
            } else {
                width
            };
            if width <= 0.0 {
                continue;
            }

            instances.push_rect(
                [x_offset + x, run.line_top + y_offset],
                [width, self.line_height()],
                white,
                theme.selection,
            );
        }
    }

    /// Draw one line of text at a physical-pixel position.
    ///
    /// Used for everything that is not the document: the status line, the
    /// prompt, the footer and the help pane. Returns the width drawn, so a
    /// caller laying out cells left to right knows where the next one goes.
    pub fn push_line(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut Atlas,
        instances: &mut Instances,
        text: &str,
        at: (f32, f32),
        color: crate::theme::Color,
    ) -> f32 {
        if text.is_empty() {
            return 0.0;
        }

        let TextRenderer {
            font_system,
            swash,
            chrome,
            family,
            ..
        } = self;

        chrome.set_text(
            font_system,
            text,
            &attrs(family.as_deref()),
            Shaping::Advanced,
            None,
        );
        chrome.shape_until_scroll(font_system, false);

        let mut width: f32 = 0.0;
        for run in chrome.layout_runs() {
            for glyph in run.glyphs {
                let physical = glyph.physical((0.0, 0.0), 1.0);
                let Some(entry) = atlas.glyph(queue, font_system, swash, physical.cache_key) else {
                    continue;
                };
                let position = [
                    at.0 + physical.x as f32 + entry.offset[0],
                    at.1 + run.line_y + physical.y as f32 + entry.offset[1],
                ];
                let kind = if entry.color { Kind::Color } else { Kind::Mask };
                instances.push(position, entry, color, kind);
            }
            width = width.max(run.line_w);
        }
        width
    }

    fn push_cursor(
        &self,
        instances: &mut Instances,
        white: crate::render::atlas::Entry,
        buffer: &Buffer,
        offset: (f32, f32),
        theme: &Theme,
    ) {
        let (x_offset, y_offset) = offset;
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
            [x_offset + x, top + y_offset],
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

/// How wide a tab is drawn when nothing says otherwise.
const DEFAULT_TAB_WIDTH: usize = 4;

fn metrics_for(font_size: f32, scale_factor: f32) -> Metrics {
    let physical = font_size * scale_factor;
    // Both floors at 1: a zero line height is an assertion failure inside the
    // shaper, and a window that will not open is the worst way to find out
    // about a bad number in a config file.
    Metrics::new(
        physical.max(1.0),
        (physical * LINE_HEIGHT_RATIO).round().max(1.0),
    )
}

/// Shape a single `M` to find the monospace advance width.
fn measure_cell_width(font_system: &mut FontSystem, metrics: Metrics, family: Option<&str>) -> f32 {
    let mut probe = cosmic_text::Buffer::new(font_system, metrics);
    probe.set_wrap(font_system, Wrap::None);
    probe.set_text(font_system, "M", &attrs(family), Shaping::Advanced, None);
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

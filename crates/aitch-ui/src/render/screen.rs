//! Laying out one frame of the editor: the text area and the chrome under it.
//!
//! Kept out of `surface.rs` so that a frame can be drawn without a window —
//! the offscreen tests and the `dump_frame` example use exactly this code,
//! rather than a second copy of the layout that could drift from it.
//!
//! The screen is the nano screen: the document fills the top, and the bottom
//! three rows are one status-or-prompt line and the footer's two. Never three
//! footer rows; that limit is what the whole design hangs on.

use aitch_core::Editor;

use crate::render::atlas::Atlas;
use crate::render::quads::Instances;
use crate::render::text::TextRenderer;
use crate::theme::Theme;

/// Rows at the bottom that belong to the editor rather than the document.
pub const CHROME_ROWS: usize = 3;

/// Where and at what scale a frame is being drawn.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    /// Physical size of the drawable area.
    pub size: (f32, f32),
    pub scale_factor: f32,
    /// How far into the top visible line the window has scrolled, in pixels.
    pub sub_line_offset: f32,
    /// Bumped when the visible text could have changed, so the shaper knows
    /// when its cached layout is stale.
    pub generation: u64,
}

/// How many rows of document text fit in a window this tall.
pub fn text_rows(text: &TextRenderer, height: f32) -> usize {
    text.lines_for_height(height)
        .saturating_sub(CHROME_ROWS)
        .max(1)
}

/// How many character cells fit across a window this wide.
pub fn columns(text: &TextRenderer, width: f32) -> usize {
    let columns = width / text.cell_width().max(1.0);
    (columns.floor() as usize).max(1)
}

/// Fill `instances` with a whole frame.
pub fn draw(
    queue: &wgpu::Queue,
    atlas: &mut Atlas,
    text: &mut TextRenderer,
    instances: &mut Instances,
    editor: &Editor,
    theme: &Theme,
    layout: Layout,
) {
    let (width, height) = layout.size;
    let line_height = text.line_height();
    let rows = text_rows(text, height);
    let text_height = rows as f32 * line_height;
    let _ = height;

    match editor.help() {
        // The help pane takes over the text area. Still not a dialog: the
        // footer stays put underneath and says how to leave.
        Some(help) => {
            let lines = editor.help_text();
            for (row, line) in lines.iter().skip(help.scroll).take(rows).enumerate() {
                let y = row as f32 * line_height;
                text.push_line(queue, atlas, instances, line, (0.0, y), theme.foreground);
            }
        }
        None => {
            let buffer = editor.buffer();
            let first_line = editor.viewport().first_line();
            text.prepare(buffer, first_line, (width, text_height), layout.generation);
            text.push_instances(
                queue,
                atlas,
                instances,
                buffer,
                -layout.sub_line_offset,
                theme,
            );
        }
    }

    draw_chrome(
        queue,
        atlas,
        text,
        instances,
        editor,
        theme,
        layout,
        line_height,
        rows,
    );
}

/// The status or prompt line, and the two footer rows.
#[allow(clippy::too_many_arguments)]
fn draw_chrome(
    queue: &wgpu::Queue,
    atlas: &mut Atlas,
    text: &mut TextRenderer,
    instances: &mut Instances,
    editor: &Editor,
    theme: &Theme,
    layout: Layout,
    line_height: f32,
    rows: usize,
) {
    let width = layout.size.0;
    let white = atlas.white();
    let top = rows as f32 * line_height;

    // A bar behind the whole chrome, so it reads as a separate region.
    instances.push_rect(
        [0.0, top],
        [width, line_height * CHROME_ROWS as f32],
        white,
        theme.chrome_background,
    );

    // One line above the footer: the prompt if there is one, else the
    // status. A prompt replaces the status rather than stacking on it —
    // three rows of chrome is the limit and the footer owns two of them.
    let (line, cursor_column) = match editor.prompt() {
        Some(prompt) => {
            let label = prompt.label();
            // The caret sits after "label: " plus however far into the
            // input it has been moved.
            (
                prompt.line(),
                Some(label.chars().count() + 2 + prompt.cursor()),
            )
        }
        None => (editor.status_line(), None),
    };

    text.push_line(
        queue,
        atlas,
        instances,
        &line,
        (0.0, top),
        theme.chrome_foreground,
    );

    if let Some(column) = cursor_column {
        let cell = text.cell_width();
        instances.push_rect(
            [column as f32 * cell, top],
            [(2.0 * layout.scale_factor).max(1.0), line_height],
            white,
            theme.cursor,
        );
    }

    // The footer: chords reversed, labels plain, exactly two rows.
    let footer = editor.footer(columns(text, width));
    let cell = text.cell_width();
    for (row, cells) in footer.rows.iter().enumerate() {
        let y = top + (row + 1) as f32 * line_height;
        for (column, entry) in cells.iter().enumerate() {
            let x = (column * footer.column_width) as f32 * cell;

            let chord_width = entry.chord.chars().count() as f32 * cell;
            instances.push_rect(
                [x, y],
                [chord_width, line_height],
                white,
                theme.key_background,
            );
            text.push_line(
                queue,
                atlas,
                instances,
                &entry.chord,
                (x, y),
                theme.key_foreground,
            );
            text.push_line(
                queue,
                atlas,
                instances,
                &entry.label,
                (x + chord_width + cell, y),
                theme.chrome_foreground,
            );
        }
    }
}

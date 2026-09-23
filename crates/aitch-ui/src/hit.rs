//! Mouse -> position/selection -- Phase 3 Track B
//! ([`PLAN-ZERO-DEP.md`](../../../../PLAN-ZERO-DEP.md) §4 Phase 3).
//!
//! Recovers what Phase 0 stripped out of `app.rs` (`position_at_pointer`,
//! click-to-place, drag-to-select, wheel-to-scroll — see
//! `git show adf6c14^:crates/aitch-ui/src/app.rs`), rebuilt against
//! [`shape::layout_line`] instead of the deleted `render::Surface::hit`.
//! Everything here is a pure function or a small piece of state with no
//! window/GPU dependency: fabricate an `aitch_core::Buffer` and some plain
//! numbers, no real click or window required. Wiring this against
//! `Window`'s `Event::MouseMove`/`MouseButton`/`MouseWheel` and `app.rs`'s
//! `Metrics` is a later integration step, once all three Phase 3 tracks land.
//!
//! # The `Metrics` visibility question
//!
//! `app.rs`'s `Metrics` struct (line height, advance width, tab columns,
//! font scale) is exactly the layout information [`position_at`] needs, but
//! it is a private struct local to `app.rs` and this track does not touch
//! that file. So [`position_at`] takes `line_height`/`advance_width`/
//! `tab_columns` as plain parameters instead of a `&Metrics` — whoever does
//! the integration step can either keep passing these three fields through,
//! or make `Metrics` (or just the fields this needs) `pub(crate)` first.
//!
//! # The text-area height question
//!
//! [`position_at`] also takes `text_area_height` (pixels) as a plain
//! parameter rather than computing it: a click at or below this height is
//! chrome (footer/status/prompt rows), never the document, but this track
//! has no visibility into how tall that chrome is — that is the
//! footer-rendering track's output, in a file this track does not touch
//! either. The integration step computes `text_area_height` (surface
//! height minus the chrome rows' height) and passes it in.

use aitch_core::{Buffer, Position};

use crate::text::shape;

/// Map a pointer position, in client-area pixels, to a place in the
/// document — or `None` if the click landed on chrome rather than text.
///
/// `first_line` is the buffer line currently at the top of the viewport
/// (`Viewport::first_line()`); only that one field of `Viewport` is needed
/// here, so callers pass it directly rather than the whole type.
/// `line_height`, `advance_width`, and `tab_columns` are the same values
/// `render_frame` lays text out with (see the module docs above).
///
/// A click at or past `text_area_height` is rejected outright: that is the
/// footer/status/prompt rows, never the document. Everything else resolves
/// to a line (clamped to the buffer's last line, so clicking in the blank
/// area below a short document lands on its last line rather than nowhere)
/// and a character column within that line (clamped to the line's own
/// length, so clicking past a short line's end lands at end-of-line). `x`
/// and `y` are `i32` rather than `u32` because a drag can carry the pointer
/// to a negative coordinate relative to the window (see `window.rs`'s
/// module docs) — a negative `y` clamps to the viewport's first line, and a
/// negative `x` clamps to column 0, rather than panicking or wrapping.
#[allow(clippy::too_many_arguments)]
pub fn position_at(
    x: i32,
    y: i32,
    text_area_height: i32,
    buffer: &Buffer,
    first_line: usize,
    line_height: f32,
    advance_width: f32,
    tab_columns: usize,
) -> Option<Position> {
    if line_height <= 0.0 || y >= text_area_height {
        return None;
    }

    let total_lines = buffer.len_lines();
    if total_lines == 0 {
        return None;
    }

    let row_offset = if y < 0 {
        0
    } else {
        (y as f32 / line_height).floor() as usize
    };
    let line = (first_line + row_offset).min(total_lines - 1);

    let text = buffer.line_text(line);
    let column = column_at_x(text.as_str(), x as f32, advance_width, tab_columns);

    Some(Position::new(line, column))
}

/// Which character column in `text` a pixel x-coordinate is closest to, in
/// the same monospace layout [`shape::layout_line`] produces for rendering.
///
/// A tab produces no placement of its own (see that function's docs), so
/// this walks `text`'s characters alongside the placements it *does*
/// produce to recover which original character (tabs included) each one
/// represents — counting characters, not reimplementing the tab-stop column
/// arithmetic `layout_line` already got right. A click inside the blank
/// space a tab occupies resolves to whichever neighboring character's cell
/// it falls in, exactly as it would for any other character.
///
/// Returns `text`'s full character count (end-of-line) once `x` reaches or
/// passes the last placement's own cell.
fn column_at_x(text: &str, x: f32, advance_width: f32, tab_columns: usize) -> usize {
    let placements = shape::layout_line(text, advance_width, tab_columns, 0.0);

    let mut placement_index = 0usize;
    for (char_index, ch) in text.chars().enumerate() {
        if ch == '\t' {
            continue;
        }
        let placement = &placements[placement_index];
        placement_index += 1;
        let cell_end = placement.x + advance_width;
        if x < cell_end {
            return char_index;
        }
    }

    text.chars().count()
}

/// A left-button press, drag, and release, modeled as the small bit of state
/// they need: whether a drag is currently in progress. Owns nothing about
/// the window or the buffer — every method takes the `Buffer` it acts on,
/// so this stays exactly as pure and testable as the rest of this module.
///
/// Mirrors the pre-rewrite `App`'s `dragging: bool` field and its
/// `WindowEvent::MouseInput`/`CursorMoved` handling (see the module docs):
/// a press places the cursor and drops a mark for a drag to extend from: a
/// move while dragging extends the selection to the new position; a release
/// stops extending and drops a mark that never grew into a selection (a
/// plain click, no drag).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Drag {
    active: bool,
}

impl Drag {
    pub fn new() -> Drag {
        Drag { active: false }
    }

    /// Whether a drag is currently in progress (the button is down and has
    /// not yet been released).
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The left button went down at `position`.
    pub fn press(&mut self, buffer: &mut Buffer, position: Position) {
        buffer.set_cursor(position);
        // A mark here is what a subsequent drag has to extend from.
        buffer.set_mark();
        self.active = true;
    }

    /// The pointer moved to `position` while the button is held. Does
    /// nothing if no press is in progress. Returns whether the cursor
    /// actually moved, the way `Buffer`'s own movement methods do, so a
    /// caller knows whether a redraw is warranted.
    pub fn drag_to(&mut self, buffer: &mut Buffer, position: Position) -> bool {
        if !self.active {
            return false;
        }
        buffer.set_cursor_extending(position)
    }

    /// The left button went up. Stops extending, and clears a lone mark
    /// that never grew into a selection — a click with no drag would
    /// otherwise leave a mark behind to surprise the next movement key.
    pub fn release(&mut self, buffer: &mut Buffer) {
        self.active = false;
        if !buffer.has_selection() {
            buffer.clear_selection();
        }
    }
}

/// One wheel notch scrolls this many lines — matches the pre-rewrite
/// `App::scroll_by`'s `WHEEL_LINES` constant.
const WHEEL_LINES: f32 = 3.0;

/// Turn a wheel event's `delta_lines` into a new scroll position, in pixels,
/// clamped between `0` (the top of the document) and the furthest the
/// document can scroll (its last line at the top of the viewport).
///
/// `Event::MouseWheel`'s `delta_lines` is positive "away from the user" —
/// the usual scroll-up, content-moves-down direction — which is why it
/// subtracts from `scroll` here: scrolling up means showing earlier lines,
/// which is a smaller `scroll` (pixel offset of the viewport's first line).
pub fn scroll_by_wheel(scroll: f32, delta_lines: f32, line_height: f32, total_lines: usize) -> f32 {
    let max_scroll = (total_lines.saturating_sub(1) as f32 * line_height).max(0.0);
    let pixels = -delta_lines * line_height * WHEEL_LINES;
    (scroll + pixels).clamp(0.0, max_scroll)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADVANCE: f32 = 10.0;
    const LINE_HEIGHT: f32 = 20.0;
    const TAB_COLUMNS: usize = 4;

    /// line 0: "ab"          -- short line, end-of-line test
    /// line 1: ""            -- blank line
    /// line 2: "a\tb"        -- a tab
    /// line 3: "a\u{1F600}b" -- a multi-byte, single-char emoji
    /// line 4: ""            -- phantom trailing empty line
    fn rich_buffer() -> Buffer {
        Buffer::from_str("ab\n\na\tb\na\u{1F600}b\n")
    }

    #[test]
    fn a_click_well_within_a_characters_cell_lands_on_that_character() {
        let buffer = rich_buffer();
        // Row 0, x = 15 falls inside 'b''s cell ([10, 20)).
        let position = position_at(15, 10, 200, &buffer, 0, LINE_HEIGHT, ADVANCE, TAB_COLUMNS);
        assert_eq!(position, Some(Position::new(0, 1)));
    }

    #[test]
    fn a_click_past_the_end_of_a_short_line_lands_at_end_of_line() {
        let buffer = rich_buffer();
        let position = position_at(1000, 10, 200, &buffer, 0, LINE_HEIGHT, ADVANCE, TAB_COLUMNS);
        assert_eq!(position, Some(Position::new(0, 2)));
    }

    #[test]
    fn a_click_on_a_blank_line_lands_at_column_zero() {
        let buffer = rich_buffer();
        // Row 1 (y in [20, 40)) is the empty line.
        let position = position_at(50, 25, 200, &buffer, 0, LINE_HEIGHT, ADVANCE, TAB_COLUMNS);
        assert_eq!(position, Some(Position::new(1, 0)));
    }

    #[test]
    fn a_click_below_the_last_line_of_a_short_document_clamps_to_it() {
        let buffer = Buffer::from_str("hi");
        assert_eq!(buffer.len_lines(), 1);
        // Well below any real line, but still within the text area. x = 15
        // lands in 'i''s cell ([10, 20)).
        let position = position_at(15, 900, 1000, &buffer, 0, LINE_HEIGHT, ADVANCE, TAB_COLUMNS);
        assert_eq!(position, Some(Position::new(0, 1)));
    }

    #[test]
    fn a_click_in_chrome_below_the_text_area_is_rejected() {
        let buffer = rich_buffer();
        let position = position_at(5, 100, 100, &buffer, 0, LINE_HEIGHT, ADVANCE, TAB_COLUMNS);
        assert_eq!(position, None);
    }

    #[test]
    fn a_click_before_the_tab_lands_before_it_and_after_lands_on_the_far_character() {
        let buffer = rich_buffer();
        // Row 2 (y in [40, 60)) is "a\tb": 'a' at column 0 (x in [0, 10)),
        // then a tab to column 4 (x = 40), then 'b' (x in [40, 50)).
        let before = position_at(5, 45, 200, &buffer, 0, LINE_HEIGHT, ADVANCE, TAB_COLUMNS);
        assert_eq!(before, Some(Position::new(2, 0)));

        // Anywhere in the tab's own gap, or on 'b' itself, lands on 'b' --
        // character index 2 ('a', '\t', 'b'), not the visual column 4.
        let in_the_gap = position_at(20, 45, 200, &buffer, 0, LINE_HEIGHT, ADVANCE, TAB_COLUMNS);
        assert_eq!(in_the_gap, Some(Position::new(2, 2)));

        let on_b = position_at(45, 45, 200, &buffer, 0, LINE_HEIGHT, ADVANCE, TAB_COLUMNS);
        assert_eq!(on_b, Some(Position::new(2, 2)));
    }

    #[test]
    fn multi_byte_characters_resolve_by_character_index_not_byte_offset() {
        let buffer = rich_buffer();
        // Row 3 (y in [60, 80)) is "a\u{1F600}b": 'a' at column 0, the emoji
        // (4 bytes, 1 char) at column 1, 'b' at column 2. If this resolved
        // by byte offset instead of char index, 'b' would land at column 4
        // or 5, not 2.
        let on_emoji = position_at(15, 65, 200, &buffer, 0, LINE_HEIGHT, ADVANCE, TAB_COLUMNS);
        assert_eq!(on_emoji, Some(Position::new(3, 1)));

        let on_b = position_at(25, 65, 200, &buffer, 0, LINE_HEIGHT, ADVANCE, TAB_COLUMNS);
        assert_eq!(on_b, Some(Position::new(3, 2)));
    }

    #[test]
    fn a_negative_pointer_position_clamps_instead_of_panicking() {
        let buffer = rich_buffer();
        // A drag that carried the pointer above and left of the window.
        let position = position_at(-50, -50, 200, &buffer, 0, LINE_HEIGHT, ADVANCE, TAB_COLUMNS);
        assert_eq!(position, Some(Position::new(0, 0)));
    }

    #[test]
    fn the_viewports_first_line_offsets_which_line_a_click_hits() {
        let buffer = rich_buffer();
        let position = position_at(5, 10, 200, &buffer, 2, LINE_HEIGHT, ADVANCE, TAB_COLUMNS);
        // Row 0 on screen, but the viewport's first line is 2.
        assert_eq!(position, Some(Position::new(2, 0)));
    }

    // -- click / drag / release --------------------------------------------

    #[test]
    fn a_press_then_drag_extends_the_selection_from_the_press_position() {
        let mut buffer = Buffer::from_str("hello world\nsecond line\n");
        let mut drag = Drag::new();

        drag.press(&mut buffer, Position::new(0, 2));
        assert_eq!(buffer.cursor(), Position::new(0, 2));
        assert!(!buffer.has_selection(), "a press alone selects nothing");
        assert!(drag.is_active());

        assert!(drag.drag_to(&mut buffer, Position::new(0, 5)));
        assert_eq!(buffer.cursor(), Position::new(0, 5));
        assert_eq!(
            buffer.selected_text().as_deref(),
            Some("llo"),
            "selection runs from the press position to here"
        );

        assert!(drag.drag_to(&mut buffer, Position::new(1, 3)));
        assert_eq!(buffer.cursor(), Position::new(1, 3));
        // The mark stayed put at the original press position the whole time.
        let anchor_char = buffer.position_to_char(Position::new(0, 2));
        let cursor_char = buffer.position_to_char(Position::new(1, 3));
        assert_eq!(
            buffer.selection(),
            Some(anchor_char..cursor_char),
            "the mark never moved off the press position"
        );

        drag.release(&mut buffer);
        assert!(!drag.is_active());
        assert!(
            buffer.has_selection(),
            "a real drag's selection survives release"
        );
    }

    #[test]
    fn a_plain_click_with_no_drag_leaves_no_selection() {
        let mut buffer = Buffer::from_str("hello world\n");
        let mut drag = Drag::new();

        drag.press(&mut buffer, Position::new(0, 3));
        drag.release(&mut buffer);

        assert!(!drag.is_active());
        assert!(!buffer.has_selection());

        // The mark itself must be gone too, not just coincidentally equal to
        // the cursor -- otherwise the very next movement key would produce a
        // surprise selection.
        buffer.move_right();
        assert!(
            !buffer.has_selection(),
            "a leftover mark would select against the next movement"
        );
    }

    #[test]
    fn dragging_without_a_prior_press_does_nothing() {
        let mut buffer = Buffer::from_str("hello\n");
        let mut drag = Drag::new();
        assert!(!drag.drag_to(&mut buffer, Position::new(0, 3)));
        assert_eq!(buffer.cursor(), Position::new(0, 0));
        assert!(!buffer.has_selection());
    }

    // -- wheel / scroll clamping --------------------------------------------

    #[test]
    fn scrolling_up_from_the_top_stays_at_zero() {
        // Positive delta_lines is "away from the user" -- scroll up.
        let scroll = scroll_by_wheel(0.0, 1.0, LINE_HEIGHT, 100);
        assert_eq!(scroll, 0.0);
    }

    #[test]
    fn scrolling_down_past_the_end_clamps_to_max_scroll() {
        let total_lines = 10;
        let max_scroll = (total_lines - 1) as f32 * LINE_HEIGHT;
        // A large negative delta_lines is a big scroll down.
        let scroll = scroll_by_wheel(0.0, -1000.0, LINE_HEIGHT, total_lines);
        assert_eq!(scroll, max_scroll);
    }

    #[test]
    fn one_wheel_notch_scrolls_three_lines() {
        let scroll = scroll_by_wheel(500.0, -1.0, LINE_HEIGHT, 1000);
        assert_eq!(scroll, 500.0 + LINE_HEIGHT * WHEEL_LINES);
    }

    #[test]
    fn an_empty_document_has_no_scroll_room() {
        let scroll = scroll_by_wheel(0.0, -5.0, LINE_HEIGHT, 1);
        assert_eq!(scroll, 0.0, "one line means nowhere to scroll to");
    }
}

//! The text buffer: a rope, a cursor, and the movement over both.
//!
//! Phase 1 reads and navigates. Nothing here mutates the text — that arrives in
//! Phase 2 through `edit.rs`, and every mutation will go through it.
//!
//! # Positions
//!
//! Two coordinate systems, and they are not interchangeable:
//!
//! - A **char index** is an offset into the whole rope, which is what ropey
//!   indexes by and what the cursor stores.
//! - A [`Position`] is a line and a column, both zero-based, where the column
//!   counts `char`s from the start of the line and excludes the line break.
//!
//! Columns count `char`s, not grapheme clusters, so the cursor steps through
//! the halves of a family emoji. Fixing that needs cluster boundaries from the
//! shaper, so it waits for cosmic-text's layout to be wired for editing rather
//! than pulling in a second segmentation crate to disagree with it.

use std::io;

use ropey::{Rope, RopeSlice};

use crate::command::Command;

/// A line and column, both zero-based. The column excludes the line break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

impl Position {
    pub fn new(line: usize, column: usize) -> Position {
        Position { line, column }
    }
}

/// What a [`Buffer::apply`] call did, so the caller knows whether to redraw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The command ran and the buffer or cursor changed.
    Changed,
    /// The command ran and nothing changed (cursor already at the end, say).
    Unchanged,
    /// This command is not a Phase 1 movement. The caller decides what next.
    Unhandled,
}

/// A window onto the buffer, measured in whole lines.
///
/// Sub-line scroll offsets are the renderer's business; this is the part the
/// editor reasons about and the harness can assert on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    first_line: usize,
    height_lines: usize,
}

impl Viewport {
    pub fn new(height_lines: usize) -> Viewport {
        Viewport {
            first_line: 0,
            height_lines: height_lines.max(1),
        }
    }

    pub fn first_line(&self) -> usize {
        self.first_line
    }

    pub fn height_lines(&self) -> usize {
        self.height_lines
    }

    /// The last line that is at least partly visible.
    pub fn last_line(&self) -> usize {
        self.first_line + self.height_lines.saturating_sub(1)
    }

    pub fn set_height_lines(&mut self, height_lines: usize) {
        self.height_lines = height_lines.max(1);
    }

    pub fn contains(&self, line: usize) -> bool {
        line >= self.first_line && line <= self.last_line()
    }

    /// Scroll so `first_line` is `line`, clamped so at least one line shows.
    ///
    /// A buffer can always be scrolled until its last line sits at the top;
    /// scrolling further would show nothing but background.
    pub fn scroll_to(&mut self, line: usize, total_lines: usize) -> bool {
        let max_first = total_lines.saturating_sub(1);
        let clamped = line.min(max_first);
        let changed = clamped != self.first_line;
        self.first_line = clamped;
        changed
    }

    /// Scroll by a signed number of lines.
    pub fn scroll_by(&mut self, delta: isize, total_lines: usize) -> bool {
        let target = if delta >= 0 {
            self.first_line.saturating_add(delta as usize)
        } else {
            self.first_line.saturating_sub(delta.unsigned_abs())
        };
        self.scroll_to(target, total_lines)
    }

    /// Scroll the least amount that brings `line` into view.
    pub fn scroll_to_show(&mut self, line: usize, total_lines: usize) -> bool {
        if line < self.first_line {
            self.scroll_to(line, total_lines)
        } else if line > self.last_line() {
            let first = line.saturating_sub(self.height_lines.saturating_sub(1));
            self.scroll_to(first, total_lines)
        } else {
            false
        }
    }
}

/// A cursor: where it is, and where it would like to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Cursor {
    char_index: usize,
    /// The column a run of vertical movement is aiming for.
    ///
    /// Walking down past a short line and back out the other side should
    /// return to the original column, so vertical movement remembers it and
    /// horizontal movement clears it.
    goal_column: Option<usize>,
}

/// A rope, a cursor, and the movement over both.
#[derive(Debug, Clone)]
pub struct Buffer {
    text: Rope,
    cursor: Cursor,
}

impl Buffer {
    pub fn new() -> Buffer {
        Buffer::from_rope(Rope::new())
    }

    /// Named to match `Rope::from_str`, which is what it wraps. `FromStr`
    /// would force a `Result` on an operation that cannot fail.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Buffer {
        Buffer::from_rope(Rope::from_str(text))
    }

    fn from_rope(text: Rope) -> Buffer {
        Buffer {
            text,
            cursor: Cursor::default(),
        }
    }

    /// Stream a buffer in from a reader.
    ///
    /// This is the Phase 1 loader and it is deliberately naive: it requires
    /// valid UTF-8 and says nothing about line endings. Encoding detection,
    /// BOM handling, Latin-1 fallback and line-ending preservation are Phase 2
    /// and belong in `fileio.rs`, not here.
    ///
    /// It streams rather than reading the file into a `String` first, which is
    /// what keeps a 50 MB file inside the memory budget in PLAN.md §6.
    pub fn from_reader<R: io::Read>(reader: R) -> io::Result<Buffer> {
        Ok(Buffer::from_rope(Rope::from_reader(reader)?))
    }

    pub fn text(&self) -> &Rope {
        &self.text
    }

    pub fn len_chars(&self) -> usize {
        self.text.len_chars()
    }

    /// Number of lines, counting a trailing empty line after a final newline.
    pub fn len_lines(&self) -> usize {
        self.text.len_lines()
    }

    /// One line including its line break, or an empty slice past the end.
    pub fn line(&self, line: usize) -> RopeSlice<'_> {
        if line < self.text.len_lines() {
            self.text.line(line)
        } else {
            self.text.slice(self.text.len_chars()..)
        }
    }

    /// The visible text of one line, without its line break.
    pub fn line_text(&self, line: usize) -> RopeSlice<'_> {
        let slice = self.line(line);
        let end = slice.len_chars() - line_break_len(slice);
        slice.slice(..end)
    }

    /// Length of a line in `char`s, excluding its line break.
    pub fn line_len(&self, line: usize) -> usize {
        let slice = self.line(line);
        slice.len_chars() - line_break_len(slice)
    }

    pub fn cursor_char(&self) -> usize {
        self.cursor.char_index
    }

    pub fn cursor(&self) -> Position {
        self.char_to_position(self.cursor.char_index)
    }

    pub fn char_to_position(&self, char_index: usize) -> Position {
        let char_index = char_index.min(self.text.len_chars());
        let line = self.text.char_to_line(char_index);
        let column = char_index - self.text.line_to_char(line);
        Position { line, column }
    }

    pub fn position_to_char(&self, position: Position) -> usize {
        let line = position.line.min(self.text.len_lines().saturating_sub(1));
        let column = position.column.min(self.line_len(line));
        self.text.line_to_char(line) + column
    }

    /// Move the cursor to a position, clamping it into the buffer.
    pub fn set_cursor(&mut self, position: Position) -> bool {
        let char_index = self.position_to_char(position);
        self.set_cursor_char(char_index)
    }

    fn set_cursor_char(&mut self, char_index: usize) -> bool {
        let clamped = char_index.min(self.text.len_chars());
        let moved = clamped != self.cursor.char_index;
        self.cursor.char_index = clamped;
        self.cursor.goal_column = None;
        moved
    }

    // -- horizontal movement ----------------------------------------------

    pub fn move_left(&mut self) -> bool {
        let target = self.cursor.char_index.saturating_sub(1);
        self.set_cursor_char(target)
    }

    pub fn move_right(&mut self) -> bool {
        let target = self.cursor.char_index.saturating_add(1);
        self.set_cursor_char(target)
    }

    pub fn move_line_start(&mut self) -> bool {
        let line = self.cursor().line;
        self.set_cursor_char(self.text.line_to_char(line))
    }

    pub fn move_line_end(&mut self) -> bool {
        let line = self.cursor().line;
        let target = self.text.line_to_char(line) + self.line_len(line);
        self.set_cursor_char(target)
    }

    pub fn move_buffer_start(&mut self) -> bool {
        self.set_cursor_char(0)
    }

    pub fn move_buffer_end(&mut self) -> bool {
        self.set_cursor_char(self.text.len_chars())
    }

    /// To the start of the previous word, the way nano's `M-Space` goes.
    pub fn move_word_left(&mut self) -> bool {
        let mut index = self.cursor.char_index;
        while index > 0 && !is_word_char(self.char_at(index - 1)) {
            index -= 1;
        }
        while index > 0 && is_word_char(self.char_at(index - 1)) {
            index -= 1;
        }
        self.set_cursor_char(index)
    }

    /// To the start of the next word, the way nano's `^Space` goes.
    pub fn move_word_right(&mut self) -> bool {
        let end = self.text.len_chars();
        let mut index = self.cursor.char_index;
        while index < end && is_word_char(self.char_at(index)) {
            index += 1;
        }
        while index < end && !is_word_char(self.char_at(index)) {
            index += 1;
        }
        self.set_cursor_char(index)
    }

    // -- vertical movement -------------------------------------------------

    pub fn move_up(&mut self) -> bool {
        self.move_vertically(-1)
    }

    pub fn move_down(&mut self) -> bool {
        self.move_vertically(1)
    }

    /// Move by whole screens, keeping the cursor's offset within the screen.
    pub fn move_page_up(&mut self, viewport: &Viewport) -> bool {
        self.move_vertically(-(viewport.height_lines() as isize))
    }

    pub fn move_page_down(&mut self, viewport: &Viewport) -> bool {
        self.move_vertically(viewport.height_lines() as isize)
    }

    fn move_vertically(&mut self, lines: isize) -> bool {
        let current = self.cursor();
        let goal = self.cursor.goal_column.unwrap_or(current.column);

        let last_line = self.text.len_lines().saturating_sub(1);
        let target_line = if lines >= 0 {
            current.line.saturating_add(lines as usize).min(last_line)
        } else {
            current.line.saturating_sub(lines.unsigned_abs())
        };

        let column = goal.min(self.line_len(target_line));
        let char_index = self.text.line_to_char(target_line) + column;

        let moved = char_index != self.cursor.char_index;
        self.cursor.char_index = char_index;
        // Kept across the whole run of vertical movement, not just one step.
        self.cursor.goal_column = Some(goal);
        moved
    }

    // -- command dispatch --------------------------------------------------

    /// Apply a movement command.
    ///
    /// Phase 1 handles movement and nothing else; everything else reports
    /// [`Applied::Unhandled`] rather than pretending to have done something.
    /// The viewport is not scrolled here — call [`Buffer::follow_cursor`].
    pub fn apply(&mut self, command: &Command, viewport: &Viewport) -> Applied {
        let moved = match command {
            Command::MoveLeft => self.move_left(),
            Command::MoveRight => self.move_right(),
            Command::MoveUp => self.move_up(),
            Command::MoveDown => self.move_down(),
            Command::MoveWordLeft => self.move_word_left(),
            Command::MoveWordRight => self.move_word_right(),
            Command::MoveLineStart => self.move_line_start(),
            Command::MoveLineEnd => self.move_line_end(),
            Command::MovePageUp => self.move_page_up(viewport),
            Command::MovePageDown => self.move_page_down(viewport),
            Command::MoveBufferStart => self.move_buffer_start(),
            Command::MoveBufferEnd => self.move_buffer_end(),
            _ => return Applied::Unhandled,
        };

        if moved {
            Applied::Changed
        } else {
            Applied::Unchanged
        }
    }

    /// Scroll `viewport` the least amount that keeps the cursor visible.
    pub fn follow_cursor(&self, viewport: &mut Viewport) -> bool {
        viewport.scroll_to_show(self.cursor().line, self.len_lines())
    }

    fn char_at(&self, index: usize) -> char {
        self.text.char(index)
    }
}

impl Default for Buffer {
    fn default() -> Buffer {
        Buffer::new()
    }
}

/// Characters that make up a word for word-wise movement.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Length in `char`s of the line break ending a line slice: 2 for CRLF, 1 for
/// a lone LF or CR, 0 for the last line of a file with no trailing newline.
fn line_break_len(line: RopeSlice<'_>) -> usize {
    let len = line.len_chars();
    if len == 0 {
        return 0;
    }
    match line.char(len - 1) {
        '\n' => {
            if len >= 2 && line.char(len - 2) == '\r' {
                2
            } else {
                1
            }
        }
        // A lone CR is a line break to ropey, and to Classic Mac OS files.
        '\r' => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "first line\nsecond\n\nlonger fourth line\n";

    fn sample() -> Buffer {
        Buffer::from_str(SAMPLE)
    }

    #[test]
    fn lines_exclude_their_line_break() {
        let b = sample();
        assert_eq!(b.line_text(0), "first line");
        assert_eq!(b.line_len(0), 10);
        assert_eq!(b.line_text(2), "");
        assert_eq!(b.line_len(2), 0);
    }

    #[test]
    fn crlf_counts_as_one_break_not_two_columns() {
        let b = Buffer::from_str("alpha\r\nbeta\r\n");
        assert_eq!(b.line_text(0), "alpha");
        assert_eq!(b.line_len(0), 5);
        assert_eq!(b.line_text(1), "beta");
    }

    #[test]
    fn a_file_with_no_trailing_newline_has_no_phantom_line() {
        let b = Buffer::from_str("one\ntwo");
        assert_eq!(b.len_lines(), 2);
        assert_eq!(b.line_text(1), "two");
    }

    #[test]
    fn a_trailing_newline_leaves_an_empty_last_line() {
        let b = sample();
        assert_eq!(b.len_lines(), 5);
        assert_eq!(b.line_len(4), 0);
    }

    #[test]
    fn positions_and_char_indices_round_trip() {
        let b = sample();
        for index in 0..=b.len_chars() {
            let position = b.char_to_position(index);
            assert_eq!(b.position_to_char(position), index, "at {index}");
        }
    }

    #[test]
    fn horizontal_movement_stops_at_the_ends() {
        let mut b = sample();
        assert!(!b.move_left());
        assert_eq!(b.cursor(), Position::new(0, 0));

        b.move_buffer_end();
        assert!(!b.move_right());
        assert_eq!(b.cursor_char(), b.len_chars());
    }

    #[test]
    fn moving_right_off_a_line_lands_at_the_start_of_the_next() {
        let mut b = sample();
        b.move_line_end();
        assert_eq!(b.cursor(), Position::new(0, 10));
        b.move_right();
        assert_eq!(b.cursor(), Position::new(1, 0));
    }

    #[test]
    fn home_and_end_stay_on_their_line() {
        let mut b = sample();
        b.set_cursor(Position::new(1, 3));
        b.move_line_end();
        assert_eq!(b.cursor(), Position::new(1, 6));
        b.move_line_start();
        assert_eq!(b.cursor(), Position::new(1, 0));
    }

    #[test]
    fn vertical_movement_remembers_the_column_across_short_lines() {
        let mut b = sample();
        b.set_cursor(Position::new(0, 9));

        b.move_down();
        assert_eq!(b.cursor(), Position::new(1, 6), "clamped to a short line");
        b.move_down();
        assert_eq!(b.cursor(), Position::new(2, 0), "clamped to an empty line");
        b.move_down();
        // The goal column survived two clamps, which is the whole point.
        assert_eq!(b.cursor(), Position::new(3, 9));
    }

    #[test]
    fn horizontal_movement_resets_the_goal_column() {
        let mut b = sample();
        b.set_cursor(Position::new(0, 9));

        b.move_down();
        assert_eq!(
            b.cursor(),
            Position::new(1, 6),
            "clamped, still aiming at 9"
        );

        // Moving sideways makes column 5 the new intent; the old goal of 9 is
        // gone, so coming back out of the empty line lands on 5, not 9.
        b.move_left();
        assert_eq!(b.cursor(), Position::new(1, 5));

        b.move_down();
        assert_eq!(b.cursor(), Position::new(2, 0));
        b.move_down();
        assert_eq!(b.cursor(), Position::new(3, 5));
    }

    #[test]
    fn vertical_movement_clamps_at_the_first_and_last_line() {
        let mut b = sample();
        b.move_up();
        assert_eq!(b.cursor(), Position::new(0, 0));

        for _ in 0..20 {
            b.move_down();
        }
        assert_eq!(b.cursor().line, b.len_lines() - 1);
    }

    #[test]
    fn word_movement_lands_on_word_starts() {
        let mut b = Buffer::from_str("alpha  beta_two, gamma");
        b.move_word_right();
        assert_eq!(b.cursor(), Position::new(0, 7));
        b.move_word_right();
        assert_eq!(b.cursor(), Position::new(0, 17));

        b.move_word_left();
        assert_eq!(b.cursor(), Position::new(0, 7));
        b.move_word_left();
        assert_eq!(b.cursor(), Position::new(0, 0));
        assert!(!b.move_word_left());
    }

    // -- viewport ----------------------------------------------------------

    #[test]
    fn the_viewport_follows_the_cursor_by_the_least_it_can() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let mut b = Buffer::from_str(&text);
        let mut v = Viewport::new(10);

        b.set_cursor(Position::new(9, 0));
        assert!(!b.follow_cursor(&mut v), "line 9 is already visible");

        b.set_cursor(Position::new(10, 0));
        assert!(b.follow_cursor(&mut v));
        assert_eq!(v.first_line(), 1, "scrolled exactly one line");

        b.set_cursor(Position::new(0, 0));
        b.follow_cursor(&mut v);
        assert_eq!(v.first_line(), 0);
    }

    #[test]
    fn the_viewport_cannot_scroll_past_the_last_line() {
        let mut v = Viewport::new(10);
        v.scroll_by(1000, 50);
        assert_eq!(v.first_line(), 49);
        assert!(!v.scroll_by(1, 50), "already at the end");
    }

    #[test]
    fn the_viewport_cannot_scroll_above_the_first_line() {
        let mut v = Viewport::new(10);
        assert!(!v.scroll_by(-5, 50));
        assert_eq!(v.first_line(), 0);
    }

    #[test]
    fn paging_moves_by_a_screen() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let mut b = Buffer::from_str(&text);
        let v = Viewport::new(25);

        b.move_page_down(&v);
        assert_eq!(b.cursor().line, 25);
        b.move_page_down(&v);
        assert_eq!(b.cursor().line, 50);
        b.move_page_up(&v);
        assert_eq!(b.cursor().line, 25);
    }

    // -- dispatch ----------------------------------------------------------

    #[test]
    fn movement_commands_dispatch() {
        let mut b = sample();
        let v = Viewport::new(10);

        assert_eq!(b.apply(&Command::MoveRight, &v), Applied::Changed);
        assert_eq!(b.cursor(), Position::new(0, 1));
        assert_eq!(b.apply(&Command::MoveBufferEnd, &v), Applied::Changed);
        assert_eq!(b.apply(&Command::MoveBufferEnd, &v), Applied::Unchanged);
    }

    #[test]
    fn phase_one_admits_what_it_cannot_do() {
        let mut b = sample();
        let v = Viewport::new(10);
        // Editing is Phase 2. Reporting Unhandled is the honest answer.
        assert_eq!(b.apply(&Command::Cut, &v), Applied::Unhandled);
        assert_eq!(b.apply(&Command::WriteOut, &v), Applied::Unhandled);
    }

    #[test]
    fn a_buffer_streams_in_from_a_reader() {
        let b = Buffer::from_reader(SAMPLE.as_bytes()).unwrap();
        assert_eq!(b.len_lines(), 5);
        assert_eq!(b.line_text(0), "first line");
    }

    #[test]
    fn invalid_utf8_is_an_error_for_now() {
        // Phase 2 replaces this with detection and a Latin-1 fallback.
        let bytes: &[u8] = &[0xff, 0xfe, 0x00];
        assert!(Buffer::from_reader(bytes).is_err());
    }
}

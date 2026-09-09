//! The text buffer: a rope, a cursor, a selection, and everything done to them.
//!
//! Every mutation here goes through `edit.rs` and is recorded in `history.rs`.
//! Nothing else in the crate touches the rope's mutating methods.
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
use std::ops::Range;

use ropey::{Rope, RopeSlice};

use crate::command::Command;
use crate::edit::{self, Edit};
use crate::history::{History, Kind};
use crate::line_ending::LineEnding;

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
    /// Not the buffer's to run — saving, quitting, the system clipboard.
    /// The caller decides what happens next.
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

/// A byte offset paired with its row and column, which is how a parser wants
/// a position. Named because the pair of them reads as noise inline.
type BytePoint = (usize, (usize, usize));

/// A text change described in the units a parser wants: bytes and
/// row/column points, before and after.
///
/// The editor's own [`Edit`] is in characters, because that is what a cursor
/// and a rope work in. tree-sitter works in bytes and points, and needs both
/// sides of the change to reuse a tree instead of reparsing the file. Working
/// them out has to happen while both versions of the text are still to hand,
/// which is here rather than anywhere later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    /// Row and byte column, as a parser counts them.
    pub start_point: (usize, usize),
    pub old_end_point: (usize, usize),
    pub new_end_point: (usize, usize),
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

/// A rope, a cursor, a selection, and everything done to them.
#[derive(Debug, Clone)]
pub struct Buffer {
    text: Rope,
    cursor: Cursor,
    /// The fixed end of the selection. `None` means nothing is selected.
    anchor: Option<usize>,
    history: History,
    /// What a newly typed newline inserts. Existing line breaks are stored
    /// verbatim and never rewritten, so a mixed file stays mixed.
    line_ending: LineEnding,
    /// The cut buffer behind nano's `^K` and `^U`. Not the system clipboard:
    /// that belongs to the UI, since only the UI can talk to a window server.
    cut_buffer: String,
    /// Whether the last command was also a cut, so consecutive cuts pile up
    /// into one cut buffer the way nano's do.
    cutting: bool,
    /// Changes since the syntax parser last caught up, oldest first.
    pending_edits: Vec<TextEdit>,
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
        let line_ending = LineEnding::dominant(&text);
        Buffer {
            text,
            cursor: Cursor::default(),
            anchor: None,
            history: History::new(),
            line_ending,
            cut_buffer: String::new(),
            cutting: false,
            pending_edits: Vec::new(),
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
    ///
    /// An explicit placement — a click, a goto — drops the selection. Movement
    /// commands do not, because in nano the mark stays set while you move and
    /// that is what makes the selection grow.
    pub fn set_cursor(&mut self, position: Position) -> bool {
        let char_index = self.position_to_char(position);
        self.anchor = None;
        self.set_cursor_char(char_index)
    }

    /// Move the cursor without disturbing the mark, so a mouse drag extends
    /// the selection from wherever the press landed.
    pub fn set_cursor_extending(&mut self, position: Position) -> bool {
        let char_index = self.position_to_char(position);
        self.set_cursor_char(char_index)
    }

    /// True if `index` falls between the CR and the LF of a CRLF pair.
    ///
    /// The cursor must never rest there: ropey counts it as one line break, so
    /// a cursor inside it has a column past the end of its own line.
    fn inside_crlf(&self, index: usize) -> bool {
        index > 0
            && index < self.text.len_chars()
            && self.text.char(index - 1) == '\r'
            && self.text.char(index) == '\n'
    }

    fn set_cursor_char(&mut self, char_index: usize) -> bool {
        let mut clamped = char_index.min(self.text.len_chars());
        if self.inside_crlf(clamped) {
            clamped -= 1;
        }
        let moved = clamped != self.cursor.char_index;
        self.cursor.char_index = clamped;
        self.cursor.goal_column = None;
        moved
    }

    // -- horizontal movement ----------------------------------------------

    pub fn move_left(&mut self) -> bool {
        let mut target = self.cursor.char_index.saturating_sub(1);
        if self.inside_crlf(target) {
            target -= 1;
        }
        self.set_cursor_char(target)
    }

    pub fn move_right(&mut self) -> bool {
        let mut target = self.cursor.char_index.saturating_add(1);
        if self.inside_crlf(target) {
            target += 1;
        }
        self.set_cursor_char(target.min(self.text.len_chars()))
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

    // -- selection ---------------------------------------------------------

    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    /// The selected character range, low end first, or `None`.
    ///
    /// An anchor sitting exactly on the cursor is a mark with nothing selected
    /// yet, which is a real state in nano: `^6` and then no movement.
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        let cursor = self.cursor.char_index;
        if anchor == cursor {
            return None;
        }
        Some(anchor.min(cursor)..anchor.max(cursor))
    }

    pub fn selected_text(&self) -> Option<String> {
        let range = self.selection()?;
        Some(self.text.slice(range).to_string())
    }

    /// nano's `M-A` / `^6`: set the mark here, or drop it if already set.
    pub fn set_mark(&mut self) -> bool {
        self.anchor = match self.anchor {
            Some(_) => None,
            None => Some(self.cursor.char_index),
        };
        true
    }

    /// Select an explicit character range, leaving the cursor at its end.
    ///
    /// Search uses this to highlight a match: the match becomes the selection,
    /// so it is drawn the same way and a replace can act on it directly.
    pub fn select_range(&mut self, start: usize, end: usize) -> bool {
        let length = self.text.len_chars();
        let start = start.min(length);
        let end = end.min(length);
        self.anchor = Some(start);
        self.cursor.char_index = end;
        self.cursor.goal_column = None;
        start != end
    }

    pub fn clear_selection(&mut self) -> bool {
        let had = self.anchor.is_some();
        self.anchor = None;
        had
    }

    pub fn select_all(&mut self) -> bool {
        if self.text.len_chars() == 0 {
            return false;
        }
        self.set_cursor_char(self.text.len_chars());
        self.anchor = Some(0);
        true
    }

    /// Run a movement, dragging the selection with it.
    fn select_with(&mut self, movement: &Command, viewport: &Viewport) -> Applied {
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor.char_index);
        }
        let anchor = self.anchor;
        let applied = self.apply(movement, viewport);
        // The movement must not drop the mark it is extending from.
        self.anchor = anchor;
        applied
    }

    // -- editing -----------------------------------------------------------

    /// What a newly typed newline inserts. Existing breaks keep their own.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn set_line_ending(&mut self, line_ending: LineEnding) {
        self.line_ending = line_ending;
    }

    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty()
    }

    /// Note that the buffer has been written to disk.
    pub fn mark_saved(&mut self) {
        self.history.mark_saved();
    }

    /// Insert text at the cursor, replacing the selection if there is one.
    pub fn insert(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let cursor_before = self.cursor.char_index;
        let (at, removed) = self.take_selection();
        // Typing over a selection is one discrete step rather than part of a
        // run: undo should hand the replaced text back in one go.
        let kind = if removed.is_empty() {
            Kind::Insert
        } else {
            Kind::Discrete
        };
        self.apply_edit(Edit::replace(at, removed, text), kind, cursor_before)
    }

    /// Insert the file's line ending at the cursor.
    pub fn insert_newline(&mut self) -> bool {
        let text = self.line_ending.as_str();
        self.insert(text)
    }

    /// Backspace. Deletes a whole CRLF rather than splitting it in half.
    pub fn delete_backward(&mut self) -> bool {
        if self.has_selection() {
            return self.delete_selection();
        }
        let end = self.cursor.char_index;
        if end == 0 {
            return false;
        }
        let start =
            if end >= 2 && self.text.char(end - 1) == '\n' && self.text.char(end - 2) == '\r' {
                end - 2
            } else {
                end - 1
            };
        let removed = self.text.slice(start..end).to_string();
        self.apply_edit(Edit::delete(start, removed), Kind::DeleteBackward, end)
    }

    /// Delete forward, treating a CRLF as one thing in the same way.
    pub fn delete_forward(&mut self) -> bool {
        if self.has_selection() {
            return self.delete_selection();
        }
        let at = self.cursor.char_index;
        if at >= self.text.len_chars() {
            return false;
        }
        let end = if at + 1 < self.text.len_chars()
            && self.text.char(at) == '\r'
            && self.text.char(at + 1) == '\n'
        {
            at + 2
        } else {
            at + 1
        };
        let removed = self.text.slice(at..end).to_string();
        self.apply_edit(Edit::delete(at, removed), Kind::DeleteForward, at)
    }

    /// Remove the selection, if there is one.
    pub fn delete_selection(&mut self) -> bool {
        let cursor_before = self.cursor.char_index;
        let (at, removed) = self.take_selection();
        self.apply_edit(Edit::delete(at, removed), Kind::Discrete, cursor_before)
    }

    /// nano's `^K`: cut the selection, or the whole line if there is none.
    ///
    /// Consecutive cuts pile into one cut buffer, so three cuts and an uncut
    /// move three lines. Anything else in between starts the buffer over.
    pub fn cut(&mut self) -> bool {
        let cursor_before = self.cursor.char_index;
        let (at, removed) = if self.has_selection() {
            self.take_selection()
        } else {
            let line = self.cursor().line;
            let start = self.text.line_to_char(line);
            let end = start + self.line(line).len_chars();
            (start, self.text.slice(start..end).to_string())
        };

        if removed.is_empty() {
            return false;
        }
        if !self.cutting {
            self.cut_buffer.clear();
        }
        self.cut_buffer.push_str(&removed);
        let cut = self.apply_edit(Edit::delete(at, removed), Kind::Discrete, cursor_before);
        self.cutting = true;
        cut
    }

    /// nano's `^U`: paste the cut buffer at the cursor, keeping it for again.
    pub fn uncut(&mut self) -> bool {
        if self.cut_buffer.is_empty() {
            return false;
        }
        let cursor_before = self.cursor.char_index;
        let text = std::mem::take(&mut self.cut_buffer);
        let (at, removed) = self.take_selection();
        let pasted = self.apply_edit(
            Edit::replace(at, removed, text.clone()),
            Kind::Discrete,
            cursor_before,
        );
        self.cut_buffer = text;
        pasted
    }

    pub fn cut_buffer(&self) -> &str {
        &self.cut_buffer
    }

    pub fn undo(&mut self) -> bool {
        let Some(transaction) = self.history.undo() else {
            return false;
        };
        let inverted = transaction.edit.inverted();
        let before = (
            self.point_at(inverted.at),
            self.point_at(inverted.removed_end()),
        );

        edit::apply(&mut self.text, &inverted);
        self.anchor = None;
        self.cursor.char_index = transaction.cursor_before.min(self.text.len_chars());
        self.cursor.goal_column = None;
        // The parser is told about an undo exactly as it is told about a
        // typed character; to it they are the same kind of change.
        self.note_edit(before);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(transaction) = self.history.redo() else {
            return false;
        };
        let before = (
            self.point_at(transaction.edit.at),
            self.point_at(transaction.edit.removed_end()),
        );

        edit::apply(&mut self.text, &transaction.edit);
        self.anchor = None;
        self.cursor.char_index = transaction.cursor_after.min(self.text.len_chars());
        self.cursor.goal_column = None;
        self.note_edit(before);
        true
    }

    /// What an edit should replace: the selection if there is one, otherwise
    /// an empty run at the cursor. Drops the mark either way.
    fn take_selection(&mut self) -> (usize, String) {
        match self.selection() {
            Some(range) => {
                let text = self.text.slice(range.clone()).to_string();
                self.anchor = None;
                (range.start, text)
            }
            None => {
                self.anchor = None;
                (self.cursor.char_index, String::new())
            }
        }
    }

    /// Take the byte-level changes a syntax parser has not seen yet.
    pub fn take_text_edits(&mut self) -> Vec<TextEdit> {
        std::mem::take(&mut self.pending_edits)
    }

    /// Whether anything has changed since the parser last caught up.
    pub fn has_pending_edits(&self) -> bool {
        !self.pending_edits.is_empty()
    }

    /// Byte offset and row/column point of a character index.
    fn point_at(&self, char_index: usize) -> BytePoint {
        let byte = self
            .text
            .char_to_byte(char_index.min(self.text.len_chars()));
        let row = self.text.byte_to_line(byte);
        let column = byte - self.text.line_to_byte(row);
        (byte, (row, column))
    }

    /// Note a change for the parser. Called with the old text still in place
    /// for the first two points, and the new text for the third.
    fn note_edit(&mut self, before: (BytePoint, BytePoint)) {
        let ((start_byte, start_point), (old_end_byte, old_end_point)) = before;
        // The cursor sits at the end of what was just inserted.
        let (new_end_byte, new_end_point) = self.point_at(self.cursor.char_index);

        self.pending_edits.push(TextEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_point,
            old_end_point,
            new_end_point,
        });
    }

    /// The one path from an [`Edit`] to the buffer: apply it, record it, and
    /// leave the cursor where the edit ends.
    fn apply_edit(&mut self, edit: Edit, kind: Kind, cursor_before: usize) -> bool {
        if edit.is_empty() {
            return false;
        }
        // Both taken before the text moves, while these indices still mean
        // what they say.
        let before = (self.point_at(edit.at), self.point_at(edit.removed_end()));

        let cursor_after = edit.end();
        edit::apply(&mut self.text, &edit);
        self.history.record(edit, kind, cursor_before, cursor_after);
        self.cursor.char_index = cursor_after;
        self.cursor.goal_column = None;
        self.anchor = None;
        self.note_edit(before);
        true
    }

    // -- command dispatch --------------------------------------------------

    /// Run a command against the buffer.
    ///
    /// Commands the buffer has no business handling — quitting, saving, the
    /// system clipboard — report [`Applied::Unhandled`] so the caller deals
    /// with them, rather than being swallowed silently here.
    /// The viewport is not scrolled: call [`Buffer::follow_cursor`].
    pub fn apply(&mut self, command: &Command, viewport: &Viewport) -> Applied {
        // A cut only counts as consecutive if nothing happened in between.
        if !matches!(command, Command::Cut) {
            self.cutting = false;
        }

        // Moving the cursor ends the undo run in progress, so the next
        // character typed starts a new step instead of joining the last one.
        if command.is_movement() {
            self.history.break_run();
        }

        let changed = match command {
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

            Command::Select(movement) => return self.select_with(movement, viewport),
            Command::SetMark => self.set_mark(),
            Command::SelectAll => self.select_all(),

            Command::InsertText(text) => self.insert(text),
            Command::InsertNewline => self.insert_newline(),
            // Tab width and expand-tabs are `aitchrc` settings, in Phase 7.
            Command::InsertTab => self.insert("\t"),
            Command::DeleteBackward => self.delete_backward(),
            Command::DeleteForward => self.delete_forward(),
            Command::Cut => self.cut(),
            Command::Uncut => self.uncut(),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),

            _ => return Applied::Unhandled,
        };

        if changed {
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
    fn the_buffer_admits_what_is_not_its_business() {
        let mut b = sample();
        let v = Viewport::new(10);
        // Saving, quitting and the system clipboard belong to the caller.
        // Reporting Unhandled is the honest answer, and lets the UI act.
        for command in [
            Command::WriteOut,
            Command::Quit,
            Command::Copy,
            Command::Paste,
            Command::WhereIs,
        ] {
            assert_eq!(
                b.apply(&command, &v),
                Applied::Unhandled,
                "{command} is not the buffer's to handle"
            );
        }
    }

    // -- editing -----------------------------------------------------------

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut b = Buffer::from_str("hello world");
        b.set_cursor(Position::new(0, 5));
        assert!(b.insert(","));
        assert_eq!(b.text().to_string(), "hello, world");
        assert_eq!(b.cursor(), Position::new(0, 6));
    }

    #[test]
    fn backspace_and_delete_take_one_character_each() {
        let mut b = Buffer::from_str("abcd");
        b.set_cursor(Position::new(0, 2));

        assert!(b.delete_backward());
        assert_eq!(b.text().to_string(), "acd");
        assert_eq!(b.cursor(), Position::new(0, 1));

        assert!(b.delete_forward());
        assert_eq!(b.text().to_string(), "ad");
        assert_eq!(b.cursor(), Position::new(0, 1));
    }

    #[test]
    fn deleting_at_the_edges_does_nothing() {
        let mut b = Buffer::from_str("abc");
        b.move_buffer_start();
        assert!(!b.delete_backward());
        b.move_buffer_end();
        assert!(!b.delete_forward());
        assert_eq!(b.text().to_string(), "abc");
    }

    #[test]
    fn a_typed_newline_uses_the_file_line_ending() {
        let mut b = Buffer::from_str("alpha\r\nbeta\r\n");
        assert_eq!(b.line_ending(), LineEnding::CrLf);
        b.set_cursor(Position::new(0, 5));
        b.insert_newline();
        assert_eq!(b.text().to_string(), "alpha\r\n\r\nbeta\r\n");
    }

    #[test]
    fn backspace_over_a_crlf_takes_both_halves() {
        let mut b = Buffer::from_str("one\r\ntwo\r\n");
        b.set_cursor(Position::new(1, 0));
        assert!(b.delete_backward());
        assert_eq!(b.text().to_string(), "onetwo\r\n");
    }

    #[test]
    fn delete_forward_over_a_crlf_takes_both_halves() {
        let mut b = Buffer::from_str("one\r\ntwo\r\n");
        b.set_cursor(Position::new(0, 3));
        assert!(b.delete_forward());
        assert_eq!(b.text().to_string(), "onetwo\r\n");
    }

    // -- selection ---------------------------------------------------------

    #[test]
    fn the_mark_and_the_cursor_bound_the_selection() {
        let mut b = Buffer::from_str("hello world");
        b.set_cursor(Position::new(0, 0));
        assert!(!b.has_selection(), "a mark on its own selects nothing");

        b.set_mark();
        assert!(!b.has_selection(), "still nothing until the cursor moves");

        for _ in 0..5 {
            b.move_right();
        }
        assert_eq!(b.selected_text().as_deref(), Some("hello"));
    }

    #[test]
    fn the_mark_survives_movement_the_way_nano_does() {
        let mut b = Buffer::from_str("hello world");
        b.set_mark();
        let v = Viewport::new(10);
        b.apply(&Command::MoveWordRight, &v);
        assert!(b.has_selection(), "movement must not drop the mark");
    }

    #[test]
    fn set_mark_toggles_off_again() {
        let mut b = Buffer::from_str("hello");
        b.set_mark();
        b.move_right();
        assert!(b.has_selection());
        b.set_mark();
        assert!(!b.has_selection());
    }

    #[test]
    fn select_wraps_a_movement_and_starts_its_own_mark() {
        let mut b = Buffer::from_str("hello world");
        let v = Viewport::new(10);
        let select_right = Command::Select(Box::new(Command::MoveWordRight));

        b.apply(&select_right, &v);
        assert_eq!(b.selected_text().as_deref(), Some("hello "));

        // A second one extends the same selection rather than restarting it.
        b.apply(&select_right, &v);
        assert_eq!(b.selected_text().as_deref(), Some("hello world"));
    }

    #[test]
    fn a_click_drops_the_selection() {
        let mut b = Buffer::from_str("hello world");
        b.set_mark();
        b.move_word_right();
        assert!(b.has_selection());

        b.set_cursor(Position::new(0, 3));
        assert!(!b.has_selection());
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut b = Buffer::from_str("hello world");
        b.set_mark();
        for _ in 0..5 {
            b.move_right();
        }
        assert!(b.insert("goodbye"));
        assert_eq!(b.text().to_string(), "goodbye world");
        assert!(!b.has_selection());
    }

    #[test]
    fn backspace_over_a_selection_removes_all_of_it() {
        let mut b = Buffer::from_str("hello world");
        b.select_all();
        assert!(b.delete_backward());
        assert_eq!(b.text().to_string(), "");
    }

    #[test]
    fn select_all_covers_the_whole_buffer() {
        let mut b = Buffer::from_str("one\ntwo\n");
        assert!(b.select_all());
        assert_eq!(b.selected_text().as_deref(), Some("one\ntwo\n"));

        let mut empty = Buffer::new();
        assert!(!empty.select_all(), "nothing to select");
    }

    // -- cut buffer --------------------------------------------------------

    #[test]
    fn cut_takes_the_whole_line_including_its_break() {
        let mut b = Buffer::from_str("one\ntwo\nthree\n");
        b.set_cursor(Position::new(1, 1));
        assert!(b.cut());
        assert_eq!(b.text().to_string(), "one\nthree\n");
        assert_eq!(b.cut_buffer(), "two\n");
    }

    #[test]
    fn consecutive_cuts_pile_up_the_way_nano_does() {
        let mut b = Buffer::from_str("one\ntwo\nthree\nfour\n");
        let v = Viewport::new(10);
        b.set_cursor(Position::new(0, 0));

        b.apply(&Command::Cut, &v);
        b.apply(&Command::Cut, &v);
        assert_eq!(b.cut_buffer(), "one\ntwo\n");
        assert_eq!(b.text().to_string(), "three\nfour\n");

        // Anything else in between starts the cut buffer over.
        b.apply(&Command::MoveDown, &v);
        b.apply(&Command::Cut, &v);
        assert_eq!(b.cut_buffer(), "four\n");
    }

    #[test]
    fn uncut_pastes_the_cut_buffer_and_keeps_it() {
        let mut b = Buffer::from_str("one\ntwo\n");
        b.set_cursor(Position::new(0, 0));
        b.cut();

        b.move_buffer_end();
        assert!(b.uncut());
        assert_eq!(b.text().to_string(), "two\none\n");
        assert_eq!(b.cut_buffer(), "one\n", "the cut buffer survives a paste");

        assert!(b.uncut(), "and can be pasted again");
        assert_eq!(b.text().to_string(), "two\none\none\n");
    }

    #[test]
    fn cutting_a_selection_takes_only_the_selection() {
        let mut b = Buffer::from_str("hello world");
        b.set_mark();
        for _ in 0..5 {
            b.move_right();
        }
        assert!(b.cut());
        assert_eq!(b.cut_buffer(), "hello");
        assert_eq!(b.text().to_string(), " world");
    }

    // -- undo --------------------------------------------------------------

    #[test]
    fn a_run_of_typing_undoes_in_one_step() {
        let mut b = Buffer::new();
        for c in "hello".chars() {
            b.insert(&c.to_string());
        }
        assert_eq!(b.text().to_string(), "hello");

        assert!(b.undo());
        assert_eq!(b.text().to_string(), "");
        assert_eq!(b.cursor(), Position::new(0, 0));

        assert!(b.redo());
        assert_eq!(b.text().to_string(), "hello");
    }

    #[test]
    fn moving_the_cursor_splits_the_undo_run() {
        let mut b = Buffer::new();
        let v = Viewport::new(10);
        for c in "abc".chars() {
            b.apply(&Command::InsertText(c.to_string()), &v);
        }
        b.apply(&Command::MoveLeft, &v);
        for c in "XY".chars() {
            b.apply(&Command::InsertText(c.to_string()), &v);
        }
        assert_eq!(b.text().to_string(), "abXYc");

        b.undo();
        assert_eq!(b.text().to_string(), "abc", "only the second run went");
        b.undo();
        assert_eq!(b.text().to_string(), "");
    }

    #[test]
    fn undo_restores_a_replaced_selection_in_one_go() {
        let mut b = Buffer::from_str("hello world");
        b.select_all();
        b.insert("gone");
        assert_eq!(b.text().to_string(), "gone");

        assert!(b.undo());
        assert_eq!(b.text().to_string(), "hello world");
    }

    #[test]
    fn undo_at_the_bottom_of_the_stack_does_nothing() {
        let mut b = Buffer::from_str("untouched");
        assert!(!b.undo());
        assert!(!b.redo());
        assert_eq!(b.text().to_string(), "untouched");
    }

    #[test]
    fn undoing_every_edit_gets_the_original_text_back() {
        let original = "one\ntwo\nthree\n";
        let mut b = Buffer::from_str(original);
        let v = Viewport::new(10);

        b.set_cursor(Position::new(1, 0));
        b.apply(&Command::InsertText("x".to_string()), &v);
        b.apply(&Command::Cut, &v);
        b.apply(&Command::MoveDown, &v);
        b.apply(&Command::Uncut, &v);
        b.apply(&Command::DeleteForward, &v);
        b.apply(&Command::InsertNewline, &v);
        assert_ne!(b.text().to_string(), original);

        while b.undo() {}
        assert_eq!(b.text().to_string(), original);
    }

    // -- dirtiness ---------------------------------------------------------

    #[test]
    fn a_freshly_loaded_buffer_is_clean() {
        let b = Buffer::from_str("hello");
        assert!(!b.is_dirty());
    }

    #[test]
    fn editing_dirties_and_saving_cleans() {
        let mut b = Buffer::from_str("hello");
        b.insert("!");
        assert!(b.is_dirty());

        b.mark_saved();
        assert!(!b.is_dirty());

        b.insert("?");
        assert!(b.is_dirty());
        b.undo();
        assert!(!b.is_dirty(), "back at what is on disk");
    }

    #[test]
    fn movement_alone_does_not_dirty_a_buffer() {
        let mut b = Buffer::from_str("one\ntwo\n");
        let v = Viewport::new(10);
        for command in [
            Command::MoveDown,
            Command::MoveLineEnd,
            Command::MoveBufferStart,
        ] {
            b.apply(&command, &v);
        }
        assert!(!b.is_dirty());
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

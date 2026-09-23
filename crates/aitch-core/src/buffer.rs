//! The text buffer: a piece table, a cursor, a selection, and everything done
//! to them.
//!
//! Every mutation here goes through `edit.rs` and is recorded in `history.rs`.
//! Nothing else in the crate touches the piece table's mutating methods.
//!
//! # Storage
//!
//! The text itself lives in a hand-written [`PieceTable`] rather than a rope:
//! an immutable `original` buffer (the file as loaded) plus a growable `add`
//! buffer (everything typed since), stitched together by a sequence of
//! [`Piece`] descriptors. An edit splits the piece or pieces covering the
//! affected range, appends any new text to `add`, and splices a piece for it
//! into the sequence — no copy of the whole document is needed per edit. See
//! [`PLAN-ZERO-DEP.md`](../../../../PLAN-ZERO-DEP.md) §3 for why a piece table
//! rather than a hand-rolled balanced-tree rope: matching a rope's UTF-8-safe
//! rebalancing by hand is a real correctness hazard, and a piece table gets
//! comparable practical performance for an editor's access pattern with far
//! less code.
//!
//! [`Buffer::snapshot`] hands back an owned `String` for whatever caller
//! wants the document as plain text rather than through the piece table
//! directly — deliberate, called-out seams rather than an oversight:
//! everything that actually mutates or queries the buffer's own text goes
//! through the piece table, never through a snapshot. This crate has no
//! `ropey` dependency at all as of `PLAN-ZERO-DEP.md` Phase 6 — `Buffer`
//! handed back a `ropey::Rope` from here until then, when `search.rs`'s
//! in-buffer search (the last caller that wanted more from it than
//! `.to_string()`) was rewritten against [`Buffer::snapshot_chars`] instead.
//!
//! # Positions
//!
//! Two coordinate systems, and they are not interchangeable:
//!
//! - A **char index** is an offset into the whole document, which is what the
//!   piece table indexes by and what the cursor stores.
//! - A [`Position`] is a line and a column, both zero-based, where the column
//!   counts `char`s from the start of the line and excludes the line break.
//!
//! Columns count `char`s, not grapheme clusters, so the cursor steps through
//! the halves of a family emoji. Fixing that needs cluster boundaries from the
//! shaper, so it waits for cosmic-text's layout to be wired for editing rather
//! than pulling in a second segmentation crate to disagree with it.

use std::fmt;
use std::io;
use std::ops::Range;

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

/// A text change described in the units `syntax.rs`'s lexer wants: bytes and
/// row/column points, before and after.
///
/// The editor's own [`Edit`] is in characters, because that is what a cursor
/// and a rope work in. [`crate::syntax::Highlighter::parse`] only actually
/// reads `start_point`'s row — see its own docs — but the rest are kept
/// alongside it because working any of them out needs both versions of the
/// text still to hand, which is here rather than anywhere later.
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

/// A piece table, a cursor, a selection, and everything done to them.
#[derive(Debug, Clone)]
pub struct Buffer {
    text: PieceTable,
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
        Buffer::from_piece_table(PieceTable::new())
    }

    /// Named to match `String::from_str`/`str::from`. `FromStr` would force
    /// a `Result` on an operation that cannot fail.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Buffer {
        Buffer::from_piece_table(PieceTable::from_str(text))
    }

    fn from_piece_table(text: PieceTable) -> Buffer {
        let line_ending = LineEnding::dominant(&text.to_string());
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
        Ok(Buffer::from_piece_table(PieceTable::from_reader(reader)?))
    }

    /// A snapshot of the whole document as one owned `String`.
    ///
    /// The buffer's own storage is the piece table below, not a
    /// contiguous string: this materializes a fresh one on every call, for
    /// whatever caller needs the document as plain text rather than through
    /// the piece table directly — saving to disk, `search.rs`'s in-buffer
    /// search (see [`Buffer::snapshot_chars`]), and tests that just want to
    /// assert on the buffer's contents. Not called from any per-keystroke
    /// editing path in this crate; see the module docs for why this seam
    /// exists. (This used to hand back a `ropey::Rope` instead — Phase 6,
    /// per `PLAN-ZERO-DEP.md`, is when the last of that left: `search.rs`'s
    /// in-buffer search and `editor.rs`'s bracket matching were the two
    /// remaining callers that wanted more than `.to_string()` ever gave
    /// them, and both are rewritten against this and [`Buffer::char_at`]/
    /// [`Buffer::char_to_byte`] now instead.)
    pub fn snapshot(&self) -> String {
        self.text.to_string()
    }

    /// The whole document as owned `char`s, in order. `search.rs`'s
    /// in-buffer search works in character indices (matching the cursor's
    /// own coordinate system — see the module docs above), so this is its
    /// snapshot of choice rather than [`Buffer::snapshot`]'s `String`, which
    /// would make every match position a byte offset a search then has to
    /// convert back.
    pub fn snapshot_chars(&self) -> Vec<char> {
        self.snapshot().chars().collect()
    }

    /// Every line's text, each including its own line break — the same
    /// convention as [`Buffer::line`]. Built for `syntax.rs`'s line-state
    /// lexer (`PLAN-ZERO-DEP.md` Phase 5), which walks the document as owned,
    /// `Send` lines to hand to a worker thread rather than through the piece
    /// table directly. Materializing this costs the same as
    /// [`Buffer::snapshot`] above, just shaped differently.
    pub fn snapshot_lines(&self) -> Vec<String> {
        (0..self.len_lines())
            .map(|line| self.line(line).0)
            .collect()
    }

    pub fn len_chars(&self) -> usize {
        self.text.len_chars()
    }

    /// Number of lines, counting a trailing empty line after a final newline.
    pub fn len_lines(&self) -> usize {
        self.text.len_lines()
    }

    /// Byte offset, in the whole document, where `line` starts.
    ///
    /// For `syntax.rs`'s highlight-range requests, which still think in
    /// bytes (matching [`crate::syntax::Span`]'s coordinates) but no longer
    /// have a `Rope` to ask, per Phase 5.
    pub fn line_to_byte(&self, line: usize) -> usize {
        self.text.line_to_byte(line)
    }

    /// Byte length of the whole document. See [`Buffer::line_to_byte`].
    pub fn len_bytes(&self) -> usize {
        self.text.len_bytes()
    }

    /// One line including its line break, or an empty slice past the end.
    pub fn line(&self, line: usize) -> LineText {
        if line < self.text.len_lines() {
            let start = self.text.line_to_char(line);
            let end = if line + 1 < self.text.len_lines() {
                self.text.line_to_char(line + 1)
            } else {
                self.text.len_chars()
            };
            LineText(self.text.slice_to_string(start..end))
        } else {
            LineText(String::new())
        }
    }

    /// The visible text of one line, without its line break.
    pub fn line_text(&self, line: usize) -> LineText {
        let slice = self.line(line);
        let end = slice.len_chars() - line_break_len(slice.as_str());
        slice.truncate_chars(end)
    }

    /// Length of a line in `char`s, excluding its line break.
    pub fn line_len(&self, line: usize) -> usize {
        let slice = self.line(line);
        slice.len_chars() - line_break_len(slice.as_str())
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
        Some(self.text.slice_to_string(range))
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
        let removed = self.text.slice_to_string(start..end);
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
        let removed = self.text.slice_to_string(at..end);
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
            (start, self.text.slice_to_string(start..end))
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
                let text = self.text.slice_to_string(range.clone());
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

    pub fn char_at(&self, index: usize) -> char {
        self.text.char(index)
    }

    /// Byte offset, in the whole document, of character `index`.
    ///
    /// For `editor.rs`'s bracket matching, which still thinks in bytes to
    /// match `Highlights::token_at`'s coordinates but no longer has a `Rope`
    /// to ask, per Phase 6.
    pub fn char_to_byte(&self, index: usize) -> usize {
        self.text.char_to_byte(index)
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
fn line_break_len(line: &str) -> usize {
    let mut chars = line.chars().rev();
    match chars.next() {
        Some('\n') => {
            if chars.next() == Some('\r') {
                2
            } else {
                1
            }
        }
        // A lone CR is a line break, same as it was to ropey, and to Classic
        // Mac OS files.
        Some('\r') => 1,
        _ => 0,
    }
}

// -- the piece table -----------------------------------------------------
//
// See the module docs at the top of this file for the design in prose. In
// short: `original` is the file as loaded, `add` is everything typed since,
// and `pieces` stitches runs of one or the other into the document in order.
// Every position this type is handed or hands back is a **character** index;
// converting a character offset to a byte offset *within one piece's source
// buffer* always goes through `char_indices`, never raw arithmetic, which is
// what keeps a split from ever landing inside a multi-byte UTF-8 sequence.

/// Which buffer a [`Piece`] draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Original,
    Add,
}

/// A run of text: a byte range in one of the piece table's two buffers, with
/// its `char` length cached so walking the piece list to answer a `char`
/// query never has to re-decode UTF-8 it has already counted once.
#[derive(Debug, Clone, Copy)]
struct Piece {
    source: Source,
    byte_start: usize,
    byte_len: usize,
    char_len: usize,
}

/// A resume point built from a known line boundary — see
/// [`PieceTable::checkpoint_at_line`], which is the only place one is made.
#[derive(Debug, Clone, Copy)]
struct Checkpoint {
    /// Index into `pieces` of the piece containing `line_chars`.
    piece: usize,
    /// Chars/bytes accumulated by every piece strictly before `piece`.
    cum_chars: usize,
    cum_bytes: usize,
    /// The line boundary's own document-wide char/byte position.
    line_chars: usize,
    line_bytes: usize,
}

/// The byte offset within `text` of its `n`th `char`, or `text.len()` if
/// `text` has `n` or fewer characters.
///
/// Every piece split and every piece-table query funnels through this: it is
/// the one place a character count becomes a byte offset, and it can only
/// ever produce one that `char_indices` itself produced, so it can only ever
/// land on a real `char` boundary.
fn nth_char_byte(text: &str, n: usize) -> usize {
    text.char_indices()
        .nth(n)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

/// Like [`nth_char_byte`], but resumes the scan from `hint` — a
/// `(char_offset, byte_offset)` pair into `text` already known to be
/// correct and no later than `n` — instead of always starting at byte 0.
///
/// This is what keeps a piece-table lookup cheap however far `n` is into a
/// large file: without it, reading a line near the end of a 50 MB document
/// re-scans tens of millions of characters from the start of the piece on
/// every call, since a piece is not chunked the way a rope's leaves are.
/// [`PieceTable::checkpoint_before`] supplies the hint — the nearest known
/// line boundary at or before the target — so the scan a caller actually
/// pays for is bounded by *that line's* length, not the document's.
///
/// Falls back to scanning from the start if `n` is before the hint, which
/// should never happen given how callers build one, but staying correct
/// costs one comparison.
fn nth_char_byte_from(text: &str, n: usize, hint: (usize, usize)) -> usize {
    let (hint_chars, hint_bytes) = hint;
    if n < hint_chars || hint_bytes > text.len() {
        return nth_char_byte(text, n);
    }
    text[hint_bytes..]
        .char_indices()
        .nth(n - hint_chars)
        .map(|(byte, _)| hint_bytes + byte)
        .unwrap_or(text.len())
}

/// One line's text, returned by [`Buffer::line`] and [`Buffer::line_text`].
///
/// An owned wrapper rather than a borrowed slice: a line's text can be
/// stitched together from more than one piece, so there is usually no single
/// contiguous slice of either buffer to borrow.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineText(String);

impl LineText {
    /// Length in `char`s.
    pub fn len_chars(&self) -> usize {
        self.0.chars().count()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first `n` characters. Used to trim a line break off the end.
    fn truncate_chars(&self, n: usize) -> LineText {
        LineText(self.0[..nth_char_byte(&self.0, n)].to_string())
    }
}

impl PartialEq<&str> for LineText {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<str> for LineText {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl fmt::Display for LineText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Character offsets of every line's start, including a trailing entry for
/// the phantom empty line after a final line break — the same convention
/// `ropey::Rope::len_lines` used. Always has at least one entry, `0`. The
/// parallel `Vec` gives each entry's *byte* offset too — the checkpoint
/// [`PieceTable::checkpoint_before`] resumes a char→byte lookup from, so
/// one is never had without the other; see that function's own docs for
/// why this exists.
///
/// A lone `\r`, a lone `\n`, and a `\r\n` pair each count as one line break,
/// matching ropey and Classic Mac OS / Unix / Windows files respectively.
fn scan_line_starts(text: &str) -> (Vec<usize>, Vec<usize>) {
    let mut starts = vec![0usize];
    let mut byte_starts = vec![0usize];
    let mut index = 0usize;
    let mut byte_index = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        index += 1;
        byte_index += c.len_utf8();
        match c {
            '\n' => {
                starts.push(index);
                byte_starts.push(byte_index);
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                    index += 1;
                    byte_index += 1; // '\n' is always one byte
                }
                starts.push(index);
                byte_starts.push(byte_index);
            }
            _ => {}
        }
    }
    (starts, byte_starts)
}

/// Same as [`scan_line_starts`], for a slice of the document that is being
/// rescanned after an edit rather than the whole thing.
///
/// `at_doc_end` says whether this slice reaches the true end of the document.
/// When it does not, a trailing line break at the end of `text` starts a line
/// whose entry is already present — the first preserved entry just past this
/// slice — so scanning it again here would duplicate it.
fn scan_line_starts_region(text: &str, at_doc_end: bool) -> (Vec<usize>, Vec<usize>) {
    let (mut starts, mut byte_starts) = scan_line_starts(text);
    if !at_doc_end && starts.len() > 1 && *starts.last().unwrap() == text.chars().count() {
        starts.pop();
        byte_starts.pop();
    }
    (starts, byte_starts)
}

/// An immutable `original` buffer, a growable `add` buffer, and a sequence of
/// [`Piece`]s stitching runs of one or the other into the document, in order.
///
/// A separate `line_starts` index gives `Buffer` the cheap line lookups it
/// needs without walking the piece list, and is patched incrementally: an
/// edit rescans only the affected lines (plus one line of margin either side,
/// so a `\r` and `\n` merging or splitting across the edit's boundary is
/// never missed) rather than the whole document. See the module docs.
///
/// `line_bytes` is `line_starts`' parallel byte-offset twin, kept in
/// lockstep everywhere `line_starts` changes — see
/// [`PieceTable::checkpoint_before`] for what it buys: without it, reading
/// or editing near the end of a large file re-decodes UTF-8 from the start
/// of the document on every call, since a piece has no internal chunking
/// the way a rope's leaves do to bound that cost. Found and fixed after
/// `PLAN-ZERO-DEP.md` Phase 8's own hand-written benchmark harness measured
/// it directly: `visible_window`'s `at_end` case cost 1.72s over 10
/// iterations against `at_start`'s 64µs, for the same 50-line read.
#[derive(Debug, Clone)]
pub(crate) struct PieceTable {
    original: String,
    add: String,
    pieces: Vec<Piece>,
    len_chars: usize,
    line_starts: Vec<usize>,
    line_bytes: Vec<usize>,
}

impl PieceTable {
    pub(crate) fn new() -> PieceTable {
        PieceTable {
            original: String::new(),
            add: String::new(),
            pieces: Vec::new(),
            len_chars: 0,
            line_starts: vec![0],
            line_bytes: vec![0],
        }
    }

    pub(crate) fn from_str(text: &str) -> PieceTable {
        let mut pieces = Vec::new();
        if !text.is_empty() {
            pieces.push(Piece {
                source: Source::Original,
                byte_start: 0,
                byte_len: text.len(),
                char_len: text.chars().count(),
            });
        }
        let (line_starts, line_bytes) = scan_line_starts(text);
        PieceTable {
            len_chars: pieces.first().map_or(0, |p| p.char_len),
            line_starts,
            line_bytes,
            original: text.to_owned(),
            add: String::new(),
            pieces,
        }
    }

    pub(crate) fn from_reader<R: io::Read>(mut reader: R) -> io::Result<PieceTable> {
        let mut text = String::new();
        reader.read_to_string(&mut text)?;
        Ok(PieceTable::from_str(&text))
    }

    pub(crate) fn len_chars(&self) -> usize {
        self.len_chars
    }

    pub(crate) fn len_lines(&self) -> usize {
        self.line_starts.len()
    }

    pub(crate) fn line_to_char(&self, line: usize) -> usize {
        self.line_starts
            .get(line)
            .copied()
            .unwrap_or(self.len_chars)
    }

    pub(crate) fn char_to_line(&self, index: usize) -> usize {
        let index = index.min(self.len_chars);
        match self.line_starts.binary_search(&index) {
            Ok(i) => i,
            // `i` is where `index` would be inserted to keep the vec sorted,
            // so the line containing it is the one just before that — and
            // `i` can never be `0`, since `line_starts[0]` is always `0` and
            // `index` is never negative.
            Err(i) => i - 1,
        }
    }

    fn piece_text<'a>(&'a self, piece: &Piece) -> &'a str {
        let source = match piece.source {
            Source::Original => self.original.as_str(),
            Source::Add => self.add.as_str(),
        };
        &source[piece.byte_start..piece.byte_start + piece.byte_len]
    }

    /// A resume point for the piece-walking loops below: the piece
    /// containing a known line boundary, the `(chars, bytes)` totals of
    /// everything strictly before that piece, and the boundary's own
    /// `(chars, bytes)` position — used to seed [`nth_char_byte_from`] once
    /// a loop reaches that piece.
    ///
    /// [`PieceTable::checkpoint_before`]/[`PieceTable::checkpoint_before_byte`]
    /// build one from the nearest line start at or before a target
    /// position, found via [`PieceTable::char_to_line`]'s or `line_bytes`'
    /// own binary search — so every char↔byte lookup below only ever scans
    /// from the start of the target's own *line*, never from the start of
    /// the document.
    fn checkpoint_at_line(&self, line: usize) -> Checkpoint {
        let line_chars = self.line_starts.get(line).copied().unwrap_or(0);
        let line_bytes = self.line_bytes.get(line).copied().unwrap_or(0);

        let mut cum_chars = 0usize;
        let mut cum_bytes = 0usize;
        for (i, piece) in self.pieces.iter().enumerate() {
            let piece_end = cum_chars + piece.char_len;
            if line_chars < piece_end || i + 1 == self.pieces.len() {
                return Checkpoint {
                    piece: i,
                    cum_chars,
                    cum_bytes,
                    line_chars,
                    line_bytes,
                };
            }
            cum_chars = piece_end;
            cum_bytes += piece.byte_len;
        }
        Checkpoint {
            piece: 0,
            cum_chars: 0,
            cum_bytes: 0,
            line_chars: 0,
            line_bytes: 0,
        }
    }

    fn checkpoint_before(&self, index: usize) -> Checkpoint {
        self.checkpoint_at_line(self.char_to_line(index))
    }

    fn checkpoint_before_byte(&self, byte: usize) -> Checkpoint {
        let line = match self.line_bytes.binary_search(&byte) {
            Ok(i) => i,
            // Same reasoning as `char_to_line`'s own binary search: `i` can
            // never be `0` here, since `line_bytes[0]` is always `0`.
            Err(i) => i - 1,
        };
        self.checkpoint_at_line(line)
    }

    /// The checkpoint's hint for the first piece a loop visits after
    /// calling [`PieceTable::checkpoint_before`]/`checkpoint_before_byte` —
    /// `(0, 0)` for every piece after that, since the hint only ever
    /// describes a position inside the checkpoint's own piece.
    fn hint_for(cp: &Checkpoint, at_checkpoint_piece: bool) -> (usize, usize) {
        if at_checkpoint_piece {
            (cp.line_chars - cp.cum_chars, cp.line_bytes - cp.cum_bytes)
        } else {
            (0, 0)
        }
    }

    pub(crate) fn char(&self, index: usize) -> char {
        let cp = self.checkpoint_before(index);
        let mut cum_chars = cp.cum_chars;
        for (offset, piece) in self.pieces[cp.piece..].iter().enumerate() {
            let piece_end = cum_chars + piece.char_len;
            if index < piece_end {
                let local = index - cum_chars;
                let text = self.piece_text(piece);
                let hint = Self::hint_for(&cp, offset == 0);
                return text[nth_char_byte_from(text, local, hint)..]
                    .chars()
                    .next()
                    .expect("local offset landed on a char boundary");
            }
            cum_chars = piece_end;
        }
        panic!("char index {index} out of bounds (len {})", self.len_chars);
    }

    pub(crate) fn slice_to_string(&self, range: Range<usize>) -> String {
        let start = range.start.min(self.len_chars);
        let end = range.end.min(self.len_chars);
        if start >= end {
            return String::new();
        }
        let mut out = String::new();
        let cp = self.checkpoint_before(start);
        let mut cum_chars = cp.cum_chars;
        for (offset, piece) in self.pieces[cp.piece..].iter().enumerate() {
            let piece_start = cum_chars;
            let piece_end = cum_chars + piece.char_len;
            cum_chars = piece_end;
            if piece_end <= start {
                continue;
            }
            if piece_start >= end {
                break;
            }
            let text = self.piece_text(piece);
            let local_start = start.saturating_sub(piece_start);
            let local_end = (end - piece_start).min(piece.char_len);
            let hint = Self::hint_for(&cp, offset == 0);
            let byte_start = nth_char_byte_from(text, local_start, hint);
            // `byte_start` is itself now a valid, later hint for finding
            // `local_end` within the same piece, so this never re-scans
            // the part of the line already walked to find `local_start`.
            let byte_end = nth_char_byte_from(text, local_end, (local_start, byte_start));
            out.push_str(&text[byte_start..byte_end]);
        }
        out
    }

    /// Byte offset, in the whole document laid out as one UTF-8 string, of
    /// character `index`. Used only for the row/column points a future
    /// syntax parser wants; never a piece's own byte offset in `original` or
    /// `add`, which is a different number.
    pub(crate) fn char_to_byte(&self, index: usize) -> usize {
        let index = index.min(self.len_chars);
        let cp = self.checkpoint_before(index);
        let mut cum_chars = cp.cum_chars;
        let mut cum_bytes = cp.cum_bytes;
        for (offset, piece) in self.pieces[cp.piece..].iter().enumerate() {
            let piece_end = cum_chars + piece.char_len;
            if index >= piece_end {
                cum_bytes += piece.byte_len;
                cum_chars = piece_end;
                continue;
            }
            let local = index - cum_chars;
            let text = self.piece_text(piece);
            let hint = Self::hint_for(&cp, offset == 0);
            return cum_bytes + nth_char_byte_from(text, local, hint);
        }
        cum_bytes
    }

    /// The inverse of [`PieceTable::char_to_byte`]. Only ever called with a
    /// byte offset this type produced itself, so it is always on a boundary.
    fn byte_to_char(&self, byte: usize) -> usize {
        let cp = self.checkpoint_before_byte(byte);
        let mut cum_chars = cp.cum_chars;
        let mut cum_bytes = cp.cum_bytes;
        for (offset, piece) in self.pieces[cp.piece..].iter().enumerate() {
            let piece_end_bytes = cum_bytes + piece.byte_len;
            if byte >= piece_end_bytes {
                cum_bytes = piece_end_bytes;
                cum_chars += piece.char_len;
                continue;
            }
            let local_byte = byte - cum_bytes;
            let text = self.piece_text(piece);
            let (hint_chars, hint_bytes) = Self::hint_for(&cp, offset == 0);
            if local_byte >= hint_bytes {
                return cum_chars + hint_chars + text[hint_bytes..local_byte].chars().count();
            }
            // Defensive: should never happen given how the checkpoint was
            // chosen (its byte position is always <= any byte this piece
            // is later asked about), but stay correct rather than assume.
            return cum_chars + text[..local_byte].chars().count();
        }
        cum_chars
    }

    pub(crate) fn byte_to_line(&self, byte: usize) -> usize {
        self.char_to_line(self.byte_to_char(byte))
    }

    pub(crate) fn line_to_byte(&self, line: usize) -> usize {
        self.char_to_byte(self.line_to_char(line))
    }

    pub(crate) fn len_bytes(&self) -> usize {
        self.char_to_byte(self.len_chars)
    }

    /// Ensure a piece boundary exists at character offset `at`, splitting the
    /// piece that currently covers it if necessary.
    ///
    /// The split point is computed with [`nth_char_byte`] over that piece's
    /// own text, so it always lands on a `char` boundary — this is the one
    /// property this whole type exists to guarantee.
    fn split_at_char(&mut self, at: usize) {
        if at == 0 {
            return;
        }
        // Splitting never changes any char offset (it only ever turns one
        // piece into two covering the same range), so `self.line_starts`/
        // `line_bytes` — still describing the document from before this
        // edit's pieces started changing — are exactly what a checkpoint
        // for `at` needs, however many splits this same edit has already
        // made.
        let cp = self.checkpoint_before(at);
        let mut cum_chars = cp.cum_chars;
        for i in cp.piece..self.pieces.len() {
            let piece = self.pieces[i];
            let piece_end = cum_chars + piece.char_len;
            if at == cum_chars {
                return;
            }
            if at > cum_chars && at < piece_end {
                let local = at - cum_chars;
                let hint = Self::hint_for(&cp, i == cp.piece);
                let byte_offset = nth_char_byte_from(self.piece_text(&piece), local, hint);
                let left = Piece {
                    source: piece.source,
                    byte_start: piece.byte_start,
                    byte_len: byte_offset,
                    char_len: local,
                };
                let right = Piece {
                    source: piece.source,
                    byte_start: piece.byte_start + byte_offset,
                    byte_len: piece.byte_len - byte_offset,
                    char_len: piece.char_len - local,
                };
                self.pieces.splice(i..=i, [left, right]);
                return;
            }
            cum_chars = piece_end;
        }
        // `at == len_chars`: the end of the document is always a boundary.
    }

    fn pieces_char_len(&self) -> usize {
        self.pieces.iter().map(|p| p.char_len).sum()
    }

    /// Remove the pieces (after splitting at both ends) that fall entirely
    /// within `[start, end)`.
    fn remove_pieces(&mut self, start: usize, end: usize) {
        self.split_at_char(start);
        self.split_at_char(end);
        let mut cum_chars = 0usize;
        let mut first = None;
        let mut last = None;
        for (i, piece) in self.pieces.iter().enumerate() {
            let piece_start = cum_chars;
            let piece_end = cum_chars + piece.char_len;
            cum_chars = piece_end;
            if piece_start >= start && piece_end <= end {
                first.get_or_insert(i);
                last = Some(i);
            }
        }
        if let (Some(first), Some(last)) = (first, last) {
            self.pieces.drain(first..=last);
        }
    }

    /// Splice a new piece for `text` in at character offset `at`.
    ///
    /// Typing forward — the overwhelmingly common case — extends the piece
    /// most recently appended to `add` in place instead of growing the piece
    /// list by one piece per keystroke: this is the whole reason a run of
    /// ordinary typing does not turn the piece table into a piece per
    /// character.
    fn insert_pieces(&mut self, at: usize, text: &str) {
        let inserted_chars = text.chars().count();
        if at == self.pieces_char_len() {
            if let Some(last) = self.pieces.last_mut() {
                if last.source == Source::Add && last.byte_start + last.byte_len == self.add.len() {
                    last.byte_len += text.len();
                    last.char_len += inserted_chars;
                    self.add.push_str(text);
                    return;
                }
            }
        }

        let byte_start = self.add.len();
        self.add.push_str(text);
        let new_piece = Piece {
            source: Source::Add,
            byte_start,
            byte_len: text.len(),
            char_len: inserted_chars,
        };

        self.split_at_char(at);
        let mut cum_chars = 0usize;
        let mut insert_at = self.pieces.len();
        for (i, piece) in self.pieces.iter().enumerate() {
            if cum_chars == at {
                insert_at = i;
                break;
            }
            cum_chars += piece.char_len;
        }
        self.pieces.insert(insert_at, new_piece);
    }

    pub(crate) fn insert(&mut self, at: usize, text: &str) {
        self.edit(at, 0, text);
    }

    pub(crate) fn remove(&mut self, range: Range<usize>) {
        self.edit(range.start, range.end - range.start, "");
    }

    /// Replace `removed_chars` characters at `at` with `inserted`, updating
    /// the piece list and the line-start index together.
    fn edit(&mut self, at: usize, removed_chars: usize, inserted: &str) {
        if removed_chars == 0 && inserted.is_empty() {
            return;
        }
        let old_total = self.len_chars;
        let old_end = (at + removed_chars).min(old_total);

        // The old line range to rescan, with one extra line of margin either
        // side: enough to catch a `\r`/`\n` pair merging or splitting across
        // the edit's boundary, since a line always contains the character
        // immediately after its start. See `scan_line_starts_region`.
        let line_lo = self.char_to_line(at).saturating_sub(1);
        let line_hi = (self.char_to_line(old_end) + 1).min(self.line_starts.len() - 1);
        let region_start = self.line_starts[line_lo];
        let region_start_bytes = self.line_bytes[line_lo];
        let at_doc_end = line_hi + 1 >= self.line_starts.len();
        let region_end_old = if at_doc_end {
            old_total
        } else {
            self.line_starts[line_hi + 1]
        };

        // Bytes removed, worked out from the still-untouched piece list —
        // needed to keep `line_bytes` in step with `line_starts` below, the
        // same way `removed_chars` already keeps `line_starts` itself in
        // step.
        let removed_bytes = if removed_chars > 0 {
            self.char_to_byte(old_end) - self.char_to_byte(at)
        } else {
            0
        };

        if removed_chars > 0 {
            self.remove_pieces(at, old_end);
        }
        if !inserted.is_empty() {
            self.insert_pieces(at, inserted);
        }

        let inserted_chars = inserted.chars().count();
        let delta = inserted_chars as isize - removed_chars as isize;
        let byte_delta = inserted.len() as isize - removed_bytes as isize;
        self.len_chars = (old_total as isize + delta) as usize;

        let region_end_new = (region_end_old as isize + delta) as usize;
        let region_text = self.slice_to_string(region_start..region_end_new);
        let (region_starts, region_byte_starts) = scan_line_starts_region(&region_text, at_doc_end);

        let tail_from = line_hi + 1;
        let mut line_starts = Vec::with_capacity(
            line_lo + region_starts.len() + self.line_starts.len().saturating_sub(tail_from),
        );
        let mut line_bytes = Vec::with_capacity(line_starts.capacity());
        line_starts.extend_from_slice(&self.line_starts[..line_lo]);
        line_bytes.extend_from_slice(&self.line_bytes[..line_lo]);
        line_starts.extend(region_starts.into_iter().map(|s| region_start + s));
        line_bytes.extend(
            region_byte_starts
                .into_iter()
                .map(|s| region_start_bytes + s),
        );
        line_starts.extend(
            self.line_starts[tail_from..]
                .iter()
                .map(|&s| (s as isize + delta) as usize),
        );
        line_bytes.extend(
            self.line_bytes[tail_from..]
                .iter()
                .map(|&s| (s as isize + byte_delta) as usize),
        );
        self.line_starts = line_starts;
        self.line_bytes = line_bytes;
    }
}

impl fmt::Display for PieceTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for piece in &self.pieces {
            f.write_str(self.piece_text(piece))?;
        }
        Ok(())
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
        assert_eq!(b.snapshot(), "hello, world");
        assert_eq!(b.cursor(), Position::new(0, 6));
    }

    #[test]
    fn backspace_and_delete_take_one_character_each() {
        let mut b = Buffer::from_str("abcd");
        b.set_cursor(Position::new(0, 2));

        assert!(b.delete_backward());
        assert_eq!(b.snapshot(), "acd");
        assert_eq!(b.cursor(), Position::new(0, 1));

        assert!(b.delete_forward());
        assert_eq!(b.snapshot(), "ad");
        assert_eq!(b.cursor(), Position::new(0, 1));
    }

    #[test]
    fn deleting_at_the_edges_does_nothing() {
        let mut b = Buffer::from_str("abc");
        b.move_buffer_start();
        assert!(!b.delete_backward());
        b.move_buffer_end();
        assert!(!b.delete_forward());
        assert_eq!(b.snapshot(), "abc");
    }

    #[test]
    fn a_typed_newline_uses_the_file_line_ending() {
        let mut b = Buffer::from_str("alpha\r\nbeta\r\n");
        assert_eq!(b.line_ending(), LineEnding::CrLf);
        b.set_cursor(Position::new(0, 5));
        b.insert_newline();
        assert_eq!(b.snapshot(), "alpha\r\n\r\nbeta\r\n");
    }

    #[test]
    fn backspace_over_a_crlf_takes_both_halves() {
        let mut b = Buffer::from_str("one\r\ntwo\r\n");
        b.set_cursor(Position::new(1, 0));
        assert!(b.delete_backward());
        assert_eq!(b.snapshot(), "onetwo\r\n");
    }

    #[test]
    fn delete_forward_over_a_crlf_takes_both_halves() {
        let mut b = Buffer::from_str("one\r\ntwo\r\n");
        b.set_cursor(Position::new(0, 3));
        assert!(b.delete_forward());
        assert_eq!(b.snapshot(), "onetwo\r\n");
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
        assert_eq!(b.snapshot(), "goodbye world");
        assert!(!b.has_selection());
    }

    #[test]
    fn backspace_over_a_selection_removes_all_of_it() {
        let mut b = Buffer::from_str("hello world");
        b.select_all();
        assert!(b.delete_backward());
        assert_eq!(b.snapshot(), "");
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
        assert_eq!(b.snapshot(), "one\nthree\n");
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
        assert_eq!(b.snapshot(), "three\nfour\n");

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
        assert_eq!(b.snapshot(), "two\none\n");
        assert_eq!(b.cut_buffer(), "one\n", "the cut buffer survives a paste");

        assert!(b.uncut(), "and can be pasted again");
        assert_eq!(b.snapshot(), "two\none\none\n");
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
        assert_eq!(b.snapshot(), " world");
    }

    // -- undo --------------------------------------------------------------

    #[test]
    fn a_run_of_typing_undoes_in_one_step() {
        let mut b = Buffer::new();
        for c in "hello".chars() {
            b.insert(&c.to_string());
        }
        assert_eq!(b.snapshot(), "hello");

        assert!(b.undo());
        assert_eq!(b.snapshot(), "");
        assert_eq!(b.cursor(), Position::new(0, 0));

        assert!(b.redo());
        assert_eq!(b.snapshot(), "hello");
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
        assert_eq!(b.snapshot(), "abXYc");

        b.undo();
        assert_eq!(b.snapshot(), "abc", "only the second run went");
        b.undo();
        assert_eq!(b.snapshot(), "");
    }

    #[test]
    fn undo_restores_a_replaced_selection_in_one_go() {
        let mut b = Buffer::from_str("hello world");
        b.select_all();
        b.insert("gone");
        assert_eq!(b.snapshot(), "gone");

        assert!(b.undo());
        assert_eq!(b.snapshot(), "hello world");
    }

    #[test]
    fn undo_at_the_bottom_of_the_stack_does_nothing() {
        let mut b = Buffer::from_str("untouched");
        assert!(!b.undo());
        assert!(!b.redo());
        assert_eq!(b.snapshot(), "untouched");
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
        assert_ne!(b.snapshot(), original);

        while b.undo() {}
        assert_eq!(b.snapshot(), original);
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

    // -- piece table ---------------------------------------------------
    //
    // The rest of the buffer's tests above exercise the piece table only
    // incidentally, through `Buffer`'s cursor and CRLF policy. These test it
    // directly, at the boundary the plan called out as the real risk: a
    // split landing inside a multi-byte character, and the incrementally
    // patched line index drifting from the truth after an edit.

    #[test]
    fn piece_table_splits_never_land_inside_a_multibyte_character() {
        // é is 2 bytes, 日 is 3, and 𝄞 is 4 (a surrogate pair in UTF-16, one
        // `char` in Rust) — inserting or deleting at every character
        // boundary in between covers "immediately before", "immediately
        // after", and every position a naive byte-offset split would get
        // wrong for each of them.
        let text = "a é 日 𝄞 b";
        let total = text.chars().count();

        for at in 0..=total {
            let mut table = PieceTable::from_str(text);
            table.insert(at, "|");
            let expected: String = text
                .chars()
                .take(at)
                .chain(std::iter::once('|'))
                .chain(text.chars().skip(at))
                .collect();
            assert_eq!(table.to_string(), expected, "insert at char {at}");
            assert_eq!(table.len_chars(), total + 1, "insert at char {at}");
        }

        for at in 0..total {
            let mut table = PieceTable::from_str(text);
            table.remove(at..at + 1);
            let expected: String = text
                .chars()
                .enumerate()
                .filter(|(i, _)| *i != at)
                .map(|(_, c)| c)
                .collect();
            assert_eq!(table.to_string(), expected, "delete at char {at}");
            assert_eq!(table.len_chars(), total - 1, "delete at char {at}");
        }
    }

    #[test]
    fn editing_around_multibyte_characters_through_the_buffer_never_panics() {
        // The same boundaries as above, but through `Buffer`'s own
        // `Position`-based API, so a bad char/byte conversion in `buffer.rs`
        // itself — not just in the piece table underneath — would show up.
        let text = "café 日本語 𝄞clef";
        let total_chars = text.chars().count();

        for at in 0..=total_chars {
            let mut b = Buffer::from_str(text);
            b.set_cursor(b.char_to_position(at));
            assert!(b.insert("_"), "insert at {at}");
            assert_eq!(b.len_chars(), total_chars + 1, "insert at {at}");

            let mut b = Buffer::from_str(text);
            b.set_cursor(b.char_to_position(at));
            // Deleting forward at the very end is a no-op, which is fine —
            // the point is that neither direction ever panics or corrupts.
            b.delete_forward();
            b.set_cursor(b.char_to_position(at.min(b.len_chars())));
            b.delete_backward();
        }
    }

    #[test]
    fn a_crlf_pair_forming_across_an_edit_keeps_line_starts_correct() {
        // A lone CR, then inserting the LF that turns it into one CRLF
        // break right at the boundary between them — the case the extra
        // line of margin in `PieceTable::edit` exists to catch.
        let mut table = PieceTable::from_str("a\rb");
        assert_eq!(table.len_lines(), 2);
        assert_eq!(table.line_to_char(1), 2);

        table.insert(2, "\n");
        assert_eq!(table.to_string(), "a\r\nb");
        assert_eq!(
            table.len_lines(),
            2,
            "a CR and a freshly inserted LF must merge into one break"
        );
        assert_eq!(table.line_to_char(1), 3);

        // And the reverse: removing that same `\n` must split the CRLF back
        // into a lone CR rather than leaving a stray line-start entry.
        table.remove(2..3);
        assert_eq!(table.to_string(), "a\rb");
        assert_eq!(table.len_lines(), 2);
        assert_eq!(table.line_to_char(1), 2);
    }

    #[test]
    fn a_cr_inserted_right_before_an_existing_lf_merges_into_one_break() {
        let mut table = PieceTable::from_str("a\nb");
        assert_eq!(table.len_lines(), 2);

        table.insert(1, "\r");
        assert_eq!(table.to_string(), "a\r\nb");
        assert_eq!(
            table.len_lines(),
            2,
            "the new CR and the existing LF must merge into one break"
        );
        assert_eq!(table.line_to_char(1), 3);
    }

    #[test]
    fn undo_redo_survive_multibyte_edits_without_corrupting_boundaries() {
        let mut b = Buffer::from_str("café 日本語");
        b.set_cursor(Position::new(0, 4)); // right after "café"
        b.insert("🎉");
        assert_eq!(b.snapshot(), "café🎉 日本語");

        assert!(b.undo());
        assert_eq!(b.snapshot(), "café 日本語");
        assert!(b.redo());
        assert_eq!(b.snapshot(), "café🎉 日本語");
    }

    #[test]
    fn undo_lands_cleanly_on_a_piece_boundary_an_earlier_edit_left() {
        let mut b = Buffer::from_str("hello world");
        // Creates a piece boundary at char 5, after "hello".
        b.set_cursor(Position::new(0, 5));
        b.insert(",");
        // A second, discrete edit elsewhere so it does not coalesce with
        // the first, giving undo two real steps to walk back through.
        b.move_buffer_end();
        b.insert("!");
        assert_eq!(b.snapshot(), "hello, world!");

        assert!(b.undo());
        assert_eq!(b.snapshot(), "hello, world");
        assert!(b.undo());
        assert_eq!(b.snapshot(), "hello world");
        assert!(!b.undo());

        assert!(b.redo());
        assert!(b.redo());
        assert_eq!(b.snapshot(), "hello, world!");
    }

    #[test]
    fn line_queries_stay_correct_at_the_edges_and_on_an_empty_document() {
        let mut b = Buffer::new();
        assert_eq!(b.len_lines(), 1);
        assert_eq!(b.line_text(0), "");

        // A line break inserted into an empty document.
        b.insert("\n");
        assert_eq!(b.len_lines(), 2);
        assert_eq!(b.line_text(0), "");
        assert_eq!(b.line_text(1), "");

        // A line break inserted at the very start of a non-empty, non-ASCII
        // document, and another at the very end.
        let mut b = Buffer::from_str("café\r\n日本語\r\nend");
        assert_eq!(b.len_lines(), 3);

        b.set_cursor(Position::new(0, 0));
        b.insert("\n");
        assert_eq!(b.len_lines(), 4);
        assert_eq!(b.line_text(0), "");
        assert_eq!(b.line_text(1), "café");

        b.move_buffer_end();
        b.insert("\n");
        assert_eq!(b.len_lines(), 5);
        assert_eq!(b.line_text(4), "");
        assert_eq!(b.line_text(3), "end");
    }

    #[test]
    fn reading_a_line_deep_into_a_large_document_is_still_correct() {
        // Regression test for a real bug: every char<->byte lookup in the
        // piece table used to scan a piece's text from byte 0 regardless of
        // how far in the target was, so a line read near the end of a large
        // freshly-loaded file re-decoded tens of millions of characters on
        // every call. `PLAN-ZERO-DEP.md` Phase 8's own benchmark rewrite
        // measured it directly (`visible_window`'s `at_end` case: 1.72s over
        // 10 iterations against `at_start`'s 64µs, for the same 50-line
        // read) before `PieceTable::checkpoint_before` fixed it. This is the
        // correctness half of that fix — the benchmark covers the timing.
        let mut text = String::new();
        for i in 0..50_000 {
            text.push_str(&format!("line {i}: café → 日本語\n"));
        }
        let buffer = Buffer::from_str(&text);
        assert_eq!(buffer.len_lines(), 50_001, "a trailing phantom line too");

        for line in [0, 1, 25_000, 49_999] {
            assert_eq!(
                buffer.line_text(line).as_str(),
                format!("line {line}: café → 日本語"),
                "line {line} read incorrectly"
            );
        }
    }

    #[test]
    fn typing_a_long_multibyte_document_matches_building_it_as_a_string() {
        let words = [
            "The quick ",
            "brown fox ",
            "jumps over ",
            "the lazy dog. ",
            "café ",
            "日本語 ",
            "🎉 ",
            "naïve résumé\n",
        ];
        let mut b = Buffer::new();
        let v = Viewport::new(10);
        let mut expected = String::new();
        for _ in 0..20 {
            for word in words {
                b.apply(&Command::InsertText(word.to_string()), &v);
                expected.push_str(word);
            }
        }
        assert_eq!(b.snapshot(), expected);
        assert_eq!(b.len_chars(), expected.chars().count());

        // Every position must still round trip, including inside the CJK
        // and emoji runs the fast-append path folded into a handful of
        // pieces.
        for index in (0..=b.len_chars()).step_by(7) {
            let position = b.char_to_position(index);
            assert_eq!(b.position_to_char(position), index, "at {index}");
        }
    }

    /// A tiny xorshift generator: deterministic (so a failure is
    /// reproducible) and dependency-free.
    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[test]
    fn a_long_sequence_of_edits_matches_a_plain_string_reference() {
        // Mixed ASCII, accented, CJK and emoji text, so a boundary bug in
        // either the piece splitting or the line-start patching has plenty
        // of chances to show up.
        let seed_words = [
            "café",
            "naïve",
            "日本語",
            "🎉party",
            "hello world",
            "\n",
            "résumé\n",
            "𝄞",
            "  ",
            "x",
            "\r\n",
        ];
        let mut table = PieceTable::from_str("");
        let mut reference = String::new();
        let mut state = 0x2545_F491_4F6C_DD1Du64;

        for step in 0..500 {
            let total_chars = reference.chars().count();
            match xorshift(&mut state) % 3 {
                // Insert a random seed word at a random position.
                0 => {
                    let at = if total_chars == 0 {
                        0
                    } else {
                        (xorshift(&mut state) as usize) % (total_chars + 1)
                    };
                    let word = seed_words[(xorshift(&mut state) as usize) % seed_words.len()];
                    table.insert(at, word);
                    let byte_at = nth_char_byte(&reference, at);
                    reference.insert_str(byte_at, word);
                }
                // Delete a short run at a random position.
                1 if total_chars > 0 => {
                    let at = (xorshift(&mut state) as usize) % total_chars;
                    let len = ((xorshift(&mut state) as usize) % 4 + 1).min(total_chars - at);
                    table.remove(at..at + len);
                    let byte_start = nth_char_byte(&reference, at);
                    let byte_end = nth_char_byte(&reference, at + len);
                    reference.replace_range(byte_start..byte_end, "");
                }
                // Replace a short run: delete then insert at the same spot.
                _ if total_chars > 0 => {
                    let at = (xorshift(&mut state) as usize) % total_chars;
                    let len = ((xorshift(&mut state) as usize) % 3).min(total_chars - at);
                    table.remove(at..at + len);
                    let byte_start = nth_char_byte(&reference, at);
                    let byte_end = nth_char_byte(&reference, at + len);
                    reference.replace_range(byte_start..byte_end, "");
                    let word = seed_words[(xorshift(&mut state) as usize) % seed_words.len()];
                    table.insert(at, word);
                    let byte_at = nth_char_byte(&reference, at);
                    reference.insert_str(byte_at, word);
                }
                _ => {}
            }

            assert_eq!(
                table.to_string(),
                reference,
                "text diverged after step {step}"
            );
            assert_eq!(
                table.len_chars(),
                reference.chars().count(),
                "char count diverged after step {step}"
            );
            let (expected_starts, expected_bytes) = scan_line_starts(&reference);
            assert_eq!(
                table.line_starts, expected_starts,
                "line starts diverged after step {step}"
            );
            assert_eq!(
                table.line_bytes, expected_bytes,
                "line byte offsets diverged after step {step}"
            );
        }
    }
}

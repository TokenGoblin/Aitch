//! Undo, redo, and the coalescing that makes them feel right.
//!
//! A run of typed characters is one undo step. A run of backspaces is another.
//! Anything that is not a continuation of the run in progress — a cursor jump,
//! a newline, a change of direction, a save — starts a new one.
//!
//! Dirtiness is tracked here too, because it is the same question: is the undo
//! stack at the depth it was when the file was last written?

use crate::edit::Edit;

/// What kind of run a transaction belongs to, for coalescing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Typed text, growing rightwards.
    Insert,
    /// Backspace, eating leftwards.
    DeleteBackward,
    /// Delete, eating rightwards.
    DeleteForward,
    /// Everything else: paste, cut, a replace. Never coalesces.
    Discrete,
}

/// One undoable step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub edit: Edit,
    /// Where the cursor was before, so undo can put it back.
    pub cursor_before: usize,
    /// Where the cursor ended up, so redo can put it back.
    pub cursor_after: usize,
    kind: Kind,
}

impl Transaction {
    pub fn kind(&self) -> Kind {
        self.kind
    }
}

/// The undo and redo stacks, plus where the file on disk sits between them.
#[derive(Debug, Clone, Default)]
pub struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    /// Set when the next edit must start a new run rather than joining one.
    barrier: bool,
    /// Undo depth at the last save. `None` means no save point is reachable.
    saved_depth: Option<usize>,
}

impl History {
    pub fn new() -> History {
        History {
            saved_depth: Some(0),
            ..History::default()
        }
    }

    /// Record an edit that has already been applied.
    ///
    /// Returns true if it joined the run in progress rather than starting one.
    pub fn record(
        &mut self,
        edit: Edit,
        kind: Kind,
        cursor_before: usize,
        cursor_after: usize,
    ) -> bool {
        // A new edit makes the redo stack unreachable. If the save point was
        // in there, the file can no longer be returned to its saved state by
        // undoing, so it stops being a save point at all.
        if !self.redo.is_empty() {
            self.redo.clear();
            if self
                .saved_depth
                .is_some_and(|depth| depth > self.undo.len())
            {
                self.saved_depth = None;
            }
        }

        if !self.barrier {
            if let Some(last) = self.undo.last_mut() {
                if merge(last, &edit, kind) {
                    last.cursor_after = cursor_after;
                    return true;
                }
            }
        }

        self.barrier = false;
        self.undo.push(Transaction {
            edit,
            cursor_before,
            cursor_after,
            kind,
        });
        false
    }

    /// End the run in progress, so the next edit starts a new undo step.
    ///
    /// Called when the cursor moves on its own, when the file is saved, and
    /// after any edit that should stand alone.
    pub fn break_run(&mut self) {
        self.barrier = true;
    }

    /// Take the next transaction to undo. The caller applies its inverse.
    pub fn undo(&mut self) -> Option<Transaction> {
        let transaction = self.undo.pop()?;
        self.redo.push(transaction.clone());
        self.barrier = true;
        Some(transaction)
    }

    /// Take the next transaction to redo. The caller applies it as-is.
    pub fn redo(&mut self) -> Option<Transaction> {
        let transaction = self.redo.pop()?;
        self.undo.push(transaction.clone());
        self.barrier = true;
        Some(transaction)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Note that the buffer has just been written to disk.
    pub fn mark_saved(&mut self) {
        self.saved_depth = Some(self.undo.len());
        self.barrier = true;
    }

    /// Whether the buffer differs from what is on disk.
    ///
    /// Undoing back to the save point makes a buffer clean again, which is
    /// what a depth comparison gets you and a boolean flag does not.
    pub fn is_dirty(&self) -> bool {
        self.saved_depth != Some(self.undo.len())
    }

    /// How many undo steps are stacked up. Diagnostics and tests.
    pub fn depth(&self) -> usize {
        self.undo.len()
    }
}

/// Try to fold `edit` into the transaction in progress.
fn merge(last: &mut Transaction, edit: &Edit, kind: Kind) -> bool {
    if last.kind != kind || kind == Kind::Discrete {
        return false;
    }

    match kind {
        // Typing continues where the last character landed. A newline ends
        // the run: undoing a paragraph in one step is not what anyone means.
        // It has to end the run from both sides — a newline neither joins the
        // run before it nor lets the next character join the newline.
        Kind::Insert => {
            if last.edit.end() != edit.at
                || edit.inserted.contains('\n')
                || last.edit.inserted.ends_with('\n')
            {
                return false;
            }
            last.edit.inserted.push_str(&edit.inserted);
            true
        }

        // Backspace eats leftwards, so each edit ends where the last began.
        Kind::DeleteBackward => {
            if edit.removed_end() != last.edit.at {
                return false;
            }
            last.edit.at = edit.at;
            last.edit.removed.insert_str(0, &edit.removed);
            true
        }

        // Delete eats rightwards from a fixed point.
        Kind::DeleteForward => {
            if edit.at != last.edit.at {
                return false;
            }
            last.edit.removed.push_str(&edit.removed);
            true
        }

        Kind::Discrete => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type `text` one character at a time, as a keyboard would.
    fn type_text(history: &mut History, at: usize, text: &str) {
        let mut cursor = at;
        for c in text.chars() {
            let edit = Edit::insert(cursor, c.to_string());
            let after = cursor + 1;
            history.record(edit, Kind::Insert, cursor, after);
            cursor = after;
        }
    }

    #[test]
    fn a_run_of_typing_is_one_undo_step() {
        let mut h = History::new();
        type_text(&mut h, 0, "hello");
        assert_eq!(h.depth(), 1);

        let transaction = h.undo().unwrap();
        assert_eq!(transaction.edit.inserted, "hello");
        assert_eq!(transaction.cursor_before, 0);
        assert_eq!(transaction.cursor_after, 5);
    }

    #[test]
    fn a_newline_ends_the_typing_run() {
        let mut h = History::new();
        type_text(&mut h, 0, "one");
        h.record(Edit::insert(3, "\n"), Kind::Insert, 3, 4);
        type_text(&mut h, 4, "two");

        // "one", the newline, then "two".
        assert_eq!(h.depth(), 3);
    }

    #[test]
    fn typing_somewhere_else_ends_the_run() {
        let mut h = History::new();
        type_text(&mut h, 0, "abc");
        // Same kind, but not contiguous: the cursor was moved in between.
        type_text(&mut h, 50, "xyz");
        assert_eq!(h.depth(), 2);
    }

    #[test]
    fn a_cursor_move_ends_the_run_even_when_contiguous() {
        let mut h = History::new();
        type_text(&mut h, 0, "abc");
        h.break_run();
        type_text(&mut h, 3, "def");
        assert_eq!(h.depth(), 2);
    }

    #[test]
    fn a_run_of_backspaces_is_one_undo_step() {
        let mut h = History::new();
        // Backspacing "cba" out of "abc", right to left.
        h.record(Edit::delete(2, "c"), Kind::DeleteBackward, 3, 2);
        h.record(Edit::delete(1, "b"), Kind::DeleteBackward, 2, 1);
        h.record(Edit::delete(0, "a"), Kind::DeleteBackward, 1, 0);

        assert_eq!(h.depth(), 1);
        let transaction = h.undo().unwrap();
        // Reassembled in document order, not in the order they were typed.
        assert_eq!(transaction.edit.removed, "abc");
        assert_eq!(transaction.edit.at, 0);
    }

    #[test]
    fn a_run_of_deletes_is_one_undo_step() {
        let mut h = History::new();
        for c in ["a", "b", "c"] {
            h.record(Edit::delete(5, c), Kind::DeleteForward, 5, 5);
        }
        assert_eq!(h.depth(), 1);
        assert_eq!(h.undo().unwrap().edit.removed, "abc");
    }

    #[test]
    fn typing_and_deleting_are_separate_steps() {
        let mut h = History::new();
        type_text(&mut h, 0, "abc");
        h.record(Edit::delete(2, "c"), Kind::DeleteBackward, 3, 2);
        assert_eq!(h.depth(), 2);
    }

    #[test]
    fn discrete_edits_never_join_anything() {
        let mut h = History::new();
        for _ in 0..3 {
            h.record(Edit::insert(0, "pasted"), Kind::Discrete, 0, 6);
        }
        assert_eq!(h.depth(), 3);
    }

    #[test]
    fn redo_returns_what_undo_took() {
        let mut h = History::new();
        type_text(&mut h, 0, "hello");

        let undone = h.undo().unwrap();
        assert!(!h.can_undo());
        assert!(h.can_redo());

        let redone = h.redo().unwrap();
        assert_eq!(redone.edit, undone.edit);
        assert!(h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn editing_after_an_undo_drops_the_redo_stack() {
        let mut h = History::new();
        type_text(&mut h, 0, "hello");
        h.undo();
        assert!(h.can_redo());

        type_text(&mut h, 0, "x");
        assert!(!h.can_redo(), "the future was rewritten");
    }

    // -- dirtiness ---------------------------------------------------------

    #[test]
    fn a_fresh_buffer_is_clean() {
        assert!(!History::new().is_dirty());
    }

    #[test]
    fn an_edit_makes_it_dirty_and_a_save_makes_it_clean() {
        let mut h = History::new();
        type_text(&mut h, 0, "hello");
        assert!(h.is_dirty());

        h.mark_saved();
        assert!(!h.is_dirty());
    }

    #[test]
    fn undoing_back_to_the_saved_state_is_clean_again() {
        let mut h = History::new();
        type_text(&mut h, 0, "hello");
        h.mark_saved();

        type_text(&mut h, 5, " world");
        assert!(h.is_dirty());

        h.undo();
        assert!(!h.is_dirty(), "back at the state that was written");

        h.redo();
        assert!(h.is_dirty());
    }

    #[test]
    fn undoing_past_the_saved_state_is_dirty() {
        let mut h = History::new();
        type_text(&mut h, 0, "hello");
        h.mark_saved();
        h.undo();
        assert!(
            h.is_dirty(),
            "the file on disk has text this buffer does not"
        );
    }

    #[test]
    fn a_save_point_stranded_in_the_redo_stack_stops_counting() {
        let mut h = History::new();
        type_text(&mut h, 0, "one");
        h.break_run();
        type_text(&mut h, 3, "two");
        h.mark_saved();

        // Undo past the save point, then edit: the saved state is now
        // unreachable, and coming back to this depth must not read as clean.
        h.undo();
        h.break_run();
        type_text(&mut h, 3, "three");
        assert_eq!(h.depth(), 2, "same depth as when it was saved");
        assert!(h.is_dirty(), "but a different document");
    }
}

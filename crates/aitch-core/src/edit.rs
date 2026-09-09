//! Every mutation of a buffer's text goes through here.
//!
//! One primitive does all of it: an [`Edit`] replaces a run of text at a
//! character index with another run. Insert, delete and replace are all the
//! same operation with one side or the other empty, which is what makes undo
//! trivial — the inverse of an edit is the edit with its two sides swapped.
//!
//! This is the only file that calls a mutating ropey method. If you are
//! reaching for `Rope::insert` anywhere else, stop (see `CLAUDE.md`).

use std::fmt;

use ropey::Rope;

/// A replacement of `removed` with `inserted`, starting at character `at`.
///
/// Both sides are stored as text rather than as a range, so an edit carries
/// everything undo needs without holding a reference to the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Character index where the change starts.
    pub at: usize,
    /// The text that was there. Empty for a pure insertion.
    pub removed: String,
    /// The text that replaces it. Empty for a pure deletion.
    pub inserted: String,
}

impl Edit {
    pub fn insert(at: usize, text: impl Into<String>) -> Edit {
        Edit {
            at,
            removed: String::new(),
            inserted: text.into(),
        }
    }

    pub fn delete(at: usize, removed: impl Into<String>) -> Edit {
        Edit {
            at,
            removed: removed.into(),
            inserted: String::new(),
        }
    }

    pub fn replace(at: usize, removed: impl Into<String>, inserted: impl Into<String>) -> Edit {
        Edit {
            at,
            removed: removed.into(),
            inserted: inserted.into(),
        }
    }

    /// The edit that undoes this one.
    pub fn inverted(&self) -> Edit {
        Edit {
            at: self.at,
            removed: self.inserted.clone(),
            inserted: self.removed.clone(),
        }
    }

    /// Nothing to do — neither side has any text.
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.inserted.is_empty()
    }

    /// Characters removed.
    pub fn removed_chars(&self) -> usize {
        self.removed.chars().count()
    }

    /// Characters inserted.
    pub fn inserted_chars(&self) -> usize {
        self.inserted.chars().count()
    }

    /// Character index just past the inserted text, once applied.
    pub fn end(&self) -> usize {
        self.at + self.inserted_chars()
    }

    /// Character index just past the removed text, before applying.
    pub fn removed_end(&self) -> usize {
        self.at + self.removed_chars()
    }
}

impl fmt::Display for Edit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.removed.is_empty(), self.inserted.is_empty()) {
            (true, _) => write!(f, "insert {:?} at {}", self.inserted, self.at),
            (_, true) => write!(f, "delete {:?} at {}", self.removed, self.at),
            _ => write!(
                f,
                "replace {:?} with {:?} at {}",
                self.removed, self.inserted, self.at
            ),
        }
    }
}

/// Apply an edit to a rope.
///
/// In debug builds this checks that `removed` is really what is there. A
/// mismatch means the caller built the edit against a different version of the
/// text, and applying it anyway would corrupt the buffer silently — which is
/// exactly the class of bug undo makes hard to trace back.
pub fn apply(rope: &mut Rope, edit: &Edit) {
    debug_assert!(
        edit.removed_end() <= rope.len_chars(),
        "edit {edit} runs past the end of a {} char buffer",
        rope.len_chars()
    );
    debug_assert_eq!(
        rope.slice(edit.at..edit.removed_end()).to_string(),
        edit.removed,
        "edit {edit} does not match the text it claims to remove"
    );

    if !edit.removed.is_empty() {
        rope.remove(edit.at..edit.removed_end());
    }
    if !edit.inserted.is_empty() {
        rope.insert(edit.at, &edit.inserted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(text: &str) -> Rope {
        Rope::from_str(text)
    }

    #[test]
    fn insert_puts_text_where_it_says() {
        let mut r = rope("hello world");
        apply(&mut r, &Edit::insert(5, ","));
        assert_eq!(r.to_string(), "hello, world");
    }

    #[test]
    fn delete_takes_out_what_it_says() {
        let mut r = rope("hello, world");
        apply(&mut r, &Edit::delete(5, ","));
        assert_eq!(r.to_string(), "hello world");
    }

    #[test]
    fn replace_does_both_in_one_step() {
        let mut r = rope("hello world");
        apply(&mut r, &Edit::replace(6, "world", "there"));
        assert_eq!(r.to_string(), "hello there");
    }

    #[test]
    fn every_edit_is_its_own_undo() {
        for edit in [
            Edit::insert(5, ", dear"),
            Edit::delete(0, "hello"),
            Edit::replace(6, "world", "everyone"),
            Edit::insert(11, "!"),
        ] {
            let before = "hello world";
            let mut r = rope(before);
            apply(&mut r, &edit);
            let after = r.to_string();
            assert_ne!(after, before, "{edit} changed nothing");

            apply(&mut r, &edit.inverted());
            assert_eq!(r.to_string(), before, "{edit} did not undo cleanly");
        }
    }

    #[test]
    fn inverting_twice_is_the_original_edit() {
        let edit = Edit::replace(3, "abc", "xyz");
        assert_eq!(edit.inverted().inverted(), edit);
    }

    #[test]
    fn multibyte_text_counts_in_chars_not_bytes() {
        // 'é' is two bytes and one char; the rope indexes by char.
        let mut r = rope("naïve café");
        apply(&mut r, &Edit::delete(6, "café"));
        assert_eq!(r.to_string(), "naïve ");

        let edit = Edit::insert(0, "→𝄞");
        assert_eq!(edit.inserted_chars(), 2);
        assert_eq!(edit.end(), 2);
        apply(&mut r, &edit);
        assert_eq!(r.to_string(), "→𝄞naïve ");
    }

    #[test]
    fn an_edit_at_the_very_end_is_fine() {
        let mut r = rope("abc");
        apply(&mut r, &Edit::insert(3, "d"));
        assert_eq!(r.to_string(), "abcd");
    }

    #[test]
    fn an_empty_edit_changes_nothing() {
        let mut r = rope("abc");
        let nothing = Edit::insert(1, "");
        assert!(nothing.is_empty());
        apply(&mut r, &nothing);
        assert_eq!(r.to_string(), "abc");
    }

    #[test]
    #[should_panic(expected = "does not match the text it claims to remove")]
    fn removing_the_wrong_text_is_caught() {
        let mut r = rope("hello world");
        apply(&mut r, &Edit::delete(0, "goodbye"));
    }
}

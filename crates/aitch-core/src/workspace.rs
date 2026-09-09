//! The set of open buffers, and the folder they came from.
//!
//! nano has one buffer. VS Code has a project. This is the smaller half of
//! that: several documents, one of them active, cycled with `M-,` and `M-.`
//! and listed on the prompt line with `M-B`.
//!
//! **No tab bar**, deliberately, and PLAN.md §5 settles it: a tab strip is a
//! second piece of chrome competing with the footer for the same attention.
//! The buffer list goes on the prompt line, where every other list already is.

use std::path::{Path, PathBuf};

use crate::document::Document;
use crate::fileio::FileError;

/// The open documents, and the root folder if one was opened.
#[derive(Debug)]
pub struct Workspace {
    documents: Vec<Document>,
    active: usize,
    root: Option<PathBuf>,
    /// Whether the folder was asked for, or worked out from a file inside it.
    ///
    /// `aitch src/main.rs` sets a root so that the tree and quick open have
    /// somewhere to look, but the person opened a file, not a project. The
    /// difference matters because watching a folder means watching it
    /// recursively: inferring a root from `~/notes.txt` and then watching it
    /// puts a recursive watch on the whole home directory.
    root_opened: bool,
}

impl Workspace {
    /// A workspace holding one document and no folder.
    pub fn new(document: Document) -> Workspace {
        Workspace {
            documents: vec![document],
            active: 0,
            root: None,
            root_opened: false,
        }
    }

    /// A workspace rooted at a folder, starting with an empty buffer.
    pub fn with_root(root: PathBuf) -> Workspace {
        Workspace {
            documents: vec![Document::blank()],
            active: 0,
            root: Some(root),
            root_opened: true,
        }
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Give the workspace a folder that was worked out rather than asked for.
    ///
    /// See [`Workspace::root_was_opened`]: this is the weaker of the two, and
    /// nothing that costs anything per-file should be started on the strength
    /// of it.
    pub fn set_root(&mut self, root: PathBuf) {
        self.root = Some(root);
        self.root_opened = false;
    }

    /// Whether the folder was opened deliberately, as `aitch .` does.
    ///
    /// False when it was inferred from a file's parent, which is most of the
    /// time: `aitch notes.txt` in a home directory would otherwise start a
    /// recursive watch over everything in it.
    pub fn root_was_opened(&self) -> bool {
        self.root_opened
    }

    pub fn active(&self) -> &Document {
        &self.documents[self.active]
    }

    pub fn active_mut(&mut self) -> &mut Document {
        &mut self.documents[self.active]
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn documents(&self) -> &[Document] {
        &self.documents
    }

    pub fn len(&self) -> usize {
        self.documents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }

    /// Move to the next buffer, wrapping. False if there is only one.
    ///
    /// Not `next`: this is not an iterator, and reading it as one would
    /// suggest it hands back a document rather than moving the cursor.
    pub fn next_buffer(&mut self) -> bool {
        if self.documents.len() < 2 {
            return false;
        }
        self.active = (self.active + 1) % self.documents.len();
        true
    }

    /// Move to the previous buffer, wrapping.
    pub fn previous_buffer(&mut self) -> bool {
        if self.documents.len() < 2 {
            return false;
        }
        self.active = (self.active + self.documents.len() - 1) % self.documents.len();
        true
    }

    pub fn activate(&mut self, index: usize) -> bool {
        if index >= self.documents.len() || index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    /// Whether a path is already open, and where.
    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.documents
            .iter()
            .position(|document| document.path() == Some(path))
    }

    /// Open a file, or switch to it if it is already open.
    ///
    /// Opening the same file twice would give two buffers over one file, and
    /// then a way to lose an edit by saving the stale one.
    pub fn open(&mut self, path: &Path) -> Result<(), FileError> {
        if let Some(index) = self.index_of(path) {
            self.active = index;
            return Ok(());
        }

        let document = if path.exists() {
            Document::open(path)?
        } else {
            Document::new_at(path)
        };

        // A single untouched, unnamed buffer is the one you get at startup;
        // opening a file should fill it rather than leave it behind.
        if self.documents.len() == 1 && is_scratch(&self.documents[0]) {
            self.documents[0] = document;
            self.active = 0;
        } else {
            self.documents.push(document);
            self.active = self.documents.len() - 1;
        }
        Ok(())
    }

    /// Add an already-loaded document.
    pub fn push(&mut self, document: Document) {
        if self.documents.len() == 1 && is_scratch(&self.documents[0]) {
            self.documents[0] = document;
            self.active = 0;
        } else {
            self.documents.push(document);
            self.active = self.documents.len() - 1;
        }
    }

    /// Close the active buffer. False when it was the last one, in which case
    /// nothing is closed and the caller should quit instead.
    pub fn close_active(&mut self) -> bool {
        if self.documents.len() < 2 {
            return false;
        }
        self.documents.remove(self.active);
        if self.active >= self.documents.len() {
            self.active = self.documents.len() - 1;
        }
        true
    }

    pub fn any_dirty(&self) -> bool {
        self.documents.iter().any(Document::is_dirty)
    }

    /// The first buffer with unsaved changes, if there is one.
    ///
    /// Quitting asks about them one at a time, starting here, so the question
    /// is always about a buffer that can be brought to the front and looked at.
    pub fn first_dirty(&self) -> Option<usize> {
        self.documents.iter().position(Document::is_dirty)
    }

    /// How many buffers have unsaved changes.
    pub fn dirty_count(&self) -> usize {
        self.documents.iter().filter(|d| d.is_dirty()).count()
    }

    /// The buffer list as it reads on the prompt line: `1 notes.txt*`.
    pub fn listing(&self) -> Vec<String> {
        self.documents
            .iter()
            .enumerate()
            .map(|(index, document)| {
                let marker = if document.is_dirty() { "*" } else { "" };
                let here = if index == self.active { ">" } else { " " };
                format!("{here}{} {}{marker}", index + 1, document.display_name())
            })
            .collect()
    }
}

/// An untouched, unnamed buffer — the one a bare `aitch` starts with.
fn is_scratch(document: &Document) -> bool {
    document.path().is_none() && !document.is_dirty() && document.buffer.len_chars() == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_that_was_opened_is_told_apart_from_one_that_was_inferred() {
        // Watching a folder means watching it recursively, so `aitch
        // ~/notes.txt` -- which sets a root of the home directory purely so
        // the tree has somewhere to look -- must not be mistaken for `aitch ~`.
        let opened = Workspace::with_root(PathBuf::from("/projects/thing"));
        assert!(opened.root_was_opened());

        let mut inferred = Workspace::new(named("notes.txt"));
        inferred.set_root(PathBuf::from("/home/someone"));
        assert!(inferred.root().is_some(), "the tree still has a root");
        assert!(
            !inferred.root_was_opened(),
            "but nothing expensive should start on the strength of it"
        );
    }

    #[test]
    fn a_workspace_with_no_folder_has_nothing_to_watch() {
        let alone = Workspace::new(named("notes.txt"));
        assert!(alone.root().is_none());
        assert!(!alone.root_was_opened());
    }
    use crate::Buffer;

    fn named(name: &str) -> Document {
        Document::new_at(Path::new(name))
    }

    #[test]
    fn a_new_workspace_holds_one_document() {
        let workspace = Workspace::new(Document::blank());
        assert_eq!(workspace.len(), 1);
        assert_eq!(workspace.active_index(), 0);
        assert!(workspace.root().is_none());
    }

    #[test]
    fn cycling_wraps_in_both_directions() {
        let mut workspace = Workspace::new(named("one.txt"));
        workspace.push(named("two.txt"));
        workspace.push(named("three.txt"));
        assert_eq!(workspace.active_index(), 2);

        assert!(workspace.next_buffer());
        assert_eq!(workspace.active_index(), 0, "wrapped forward");
        assert!(workspace.previous_buffer());
        assert_eq!(workspace.active_index(), 2, "wrapped back");
    }

    #[test]
    fn cycling_one_buffer_does_nothing() {
        let mut workspace = Workspace::new(Document::blank());
        assert!(!workspace.next_buffer());
        assert!(!workspace.previous_buffer());
    }

    #[test]
    fn the_startup_scratch_buffer_is_filled_rather_than_left_behind() {
        let mut workspace = Workspace::new(Document::blank());
        workspace.push(named("first.txt"));

        assert_eq!(workspace.len(), 1, "the empty buffer was replaced");
        assert_eq!(workspace.active().display_name(), "first.txt");
    }

    #[test]
    fn a_scratch_buffer_that_has_been_typed_into_is_kept() {
        let mut document = Document::blank();
        document.buffer.insert("unsaved work");

        let mut workspace = Workspace::new(document);
        workspace.push(named("other.txt"));

        assert_eq!(workspace.len(), 2, "the work was not thrown away");
    }

    #[test]
    fn opening_a_file_twice_switches_rather_than_duplicating() {
        let mut workspace = Workspace::new(named("one.txt"));
        workspace.push(named("two.txt"));
        assert_eq!(workspace.active_index(), 1);

        // Same path again: go back to it, do not open a second view.
        assert!(workspace.open(Path::new("one.txt")).is_ok());
        assert_eq!(workspace.len(), 2);
        assert_eq!(workspace.active_index(), 0);
    }

    #[test]
    fn closing_moves_to_a_neighbour() {
        let mut workspace = Workspace::new(named("one.txt"));
        workspace.push(named("two.txt"));
        workspace.push(named("three.txt"));

        workspace.activate(1);
        assert!(workspace.close_active());
        assert_eq!(workspace.len(), 2);
        assert_eq!(workspace.active().display_name(), "three.txt");
    }

    #[test]
    fn closing_the_last_buffer_is_refused() {
        let mut workspace = Workspace::new(named("only.txt"));
        assert!(!workspace.close_active(), "the caller should quit instead");
        assert_eq!(workspace.len(), 1);
    }

    #[test]
    fn closing_the_final_entry_steps_back_rather_than_off_the_end() {
        let mut workspace = Workspace::new(named("one.txt"));
        workspace.push(named("two.txt"));
        assert_eq!(workspace.active_index(), 1);

        assert!(workspace.close_active());
        assert_eq!(workspace.active_index(), 0);
    }

    #[test]
    fn the_listing_marks_the_active_buffer_and_the_dirty_ones() {
        let mut workspace = Workspace::new(named("one.txt"));
        workspace.push(named("two.txt"));
        workspace.active_mut().buffer.insert("x");
        workspace.activate(0);

        assert_eq!(workspace.listing(), [">1 one.txt", " 2 two.txt*"]);
    }

    #[test]
    fn any_dirty_sees_a_change_in_a_buffer_that_is_not_active() {
        let mut workspace = Workspace::new(named("one.txt"));
        workspace.push(named("two.txt"));
        workspace.active_mut().buffer.insert("x");
        workspace.activate(0);

        assert!(!workspace.active().is_dirty());
        assert!(workspace.any_dirty(), "the other one still needs saving");
    }

    #[test]
    fn documents_keep_their_own_buffers() {
        let mut workspace = Workspace::new(named("one.txt"));
        workspace.active_mut().buffer = Buffer::from_str("first");
        workspace.push(named("two.txt"));
        workspace.active_mut().buffer = Buffer::from_str("second");

        workspace.activate(0);
        assert_eq!(workspace.active().buffer.text().to_string(), "first");
        workspace.activate(1);
        assert_eq!(workspace.active().buffer.text().to_string(), "second");
    }
}
